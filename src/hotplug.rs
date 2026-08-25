//! Minimal uevent listener ("hotplug-lite").
//!
//! The kernel's devtmpfs (mounted in `mount::mount_early_vfs`) already
//! creates `/dev` nodes for hotplugged devices on its own - this module
//! doesn't duplicate that. Its job is the part devtmpfs *doesn't* do:
//! fixing permissions on device classes that need non-root access (ttys,
//! input devices, ...) as they show up. This is a small, hardcoded
//! ruleset, not a rules-file-driven udev replacement, and it only chmods -
//! proper group ownership would need `/etc/group` lookups that aren't
//! wired up yet.

use crate::logging;
use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::mem;
use std::os::unix::io::RawFd;
use std::thread;

const NETLINK_KOBJECT_UEVENT: i32 = 15;
const KOBJECT_UEVENT_MULTICAST_GROUP: u32 = 1;

/// (subsystem, mode) applied to DEVNAME when a matching ADD event arrives.
/// Extend this table as MITOS needs more device classes handled.
const RULES: &[(&str, u32)] = &[("tty", 0o620), ("input", 0o660)];

/// Spawns the listener on its own thread and returns immediately. Failures
/// (e.g. no permission to open a netlink socket in a sandboxed test
/// environment) are logged, not fatal - hotplug permission-fixing is a
/// nice-to-have, not something boot should ever block on.
pub fn spawn_listener() {
    thread::spawn(|| {
        if let Err(e) = run() {
            logging::warn(&format!("hotplug listener stopped: {e}"));
        }
    });
}

fn run() -> io::Result<()> {
    let fd = open_socket()?;
    logging::debug("hotplug listener up");
    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        handle_event(&buf[..n as usize]);
    }
}

fn open_socket() -> io::Result<RawFd> {
    unsafe {
        let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, NETLINK_KOBJECT_UEVENT);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut addr: libc::sockaddr_nl = mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as u16;
        addr.nl_pid = 0; // let the kernel assign our netlink port id
        addr.nl_groups = KOBJECT_UEVENT_MULTICAST_GROUP;

        let ret = libc::bind(
            fd,
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_nl>() as u32,
        );
        if ret < 0 {
            let e = io::Error::last_os_error();
            libc::close(fd);
            return Err(e);
        }
        Ok(fd)
    }
}

fn handle_event(raw: &[u8]) {
    let fields = parse_event(raw);
    if fields.get("ACTION").map(String::as_str) != Some("add") {
        return; // only fixing up permissions on arrival for now
    }
    let Some(subsystem) = fields.get("SUBSYSTEM") else { return };
    let Some(devname) = fields.get("DEVNAME") else { return };

    for (rule_subsystem, mode) in RULES {
        if subsystem.as_str() == *rule_subsystem {
            apply_permissions(devname, *mode);
            break;
        }
    }
}

/// Kernel uevent payloads are NUL-separated ASCII strings: a summary
/// header (`add@/devices/...`) followed by `KEY=VALUE` entries.
fn parse_event(raw: &[u8]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for chunk in raw.split(|&b| b == 0) {
        let Ok(s) = std::str::from_utf8(chunk) else { continue };
        if let Some((k, v)) = s.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn apply_permissions(devname: &str, mode: u32) {
    let path = format!("/dev/{devname}");
    let Ok(c_path) = CString::new(path.clone()) else { return };

    let ok = unsafe { libc::chmod(c_path.as_ptr(), mode) == 0 };
    if ok {
        logging::debug(&format!("hotplug: set {path} to mode {mode:o}"));
    } else {
        logging::debug(&format!("hotplug: chmod {path} failed: {}", io::Error::last_os_error()));
    }
}
