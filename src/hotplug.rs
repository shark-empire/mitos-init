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
//!
//! Security note (relevant now that this runs on a real, multi-process
//! system rather than just this project's own kernel): binding to the
//! kobject-uevent multicast group does *not* by itself guarantee a
//! received message actually came from the kernel - a local process with
//! network-admin-ish privilege could otherwise craft a lookalike netlink
//! message. `run` verifies each message via `SCM_CREDENTIALS` and checks
//! the sender's pid is 0 (kernel), the same check udev/systemd-udevd use
//! for exactly this reason, before acting on it.

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
    // CMSG_SPACE rounds up to whatever alignment recvmsg expects for the
    // control buffer - using the macro rather than hand-computing it
    // avoids getting that padding subtly wrong.
    let cmsg_cap = unsafe { libc::CMSG_SPACE(mem::size_of::<libc::ucred>() as u32) as usize };
    let mut cmsg_buf = vec![0u8; cmsg_cap];

    loop {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_buf.len();

        let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }

        if !sender_is_kernel(&msg) {
            logging::warn("hotplug: dropped a uevent-looking message not sent by the kernel");
            continue;
        }

        handle_event(&buf[..n as usize]);
    }
}

/// Walks the received control messages looking for `SCM_CREDENTIALS` and
/// checks the sender's pid is 0 - see the module doc comment for why.
fn sender_is_kernel(msg: &libc::msghdr) -> bool {
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(msg as *const libc::msghdr);
        while !cmsg.is_null() {
            let hdr = &*cmsg;
            if hdr.cmsg_level == libc::SOL_SOCKET && hdr.cmsg_type == libc::SCM_CREDENTIALS {
                let data = libc::CMSG_DATA(cmsg) as *const libc::ucred;
                return (*data).pid == 0;
            }
            cmsg = libc::CMSG_NXTHDR(msg as *const libc::msghdr, cmsg);
        }
    }
    false // no SCM_CREDENTIALS present at all - treat as untrusted
}

fn open_socket() -> io::Result<RawFd> {
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            NETLINK_KOBJECT_UEVENT,
        );
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

        // Ask the kernel to attach SCM_CREDENTIALS to every message we
        // receive, so `sender_is_kernel` has something to check.
        let enable: libc::c_int = 1;
        let opt_ret = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            &enable as *const libc::c_int as *const libc::c_void,
            mem::size_of::<libc::c_int>() as u32,
        );
        if opt_ret < 0 {
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
    let Some(subsystem) = fields.get("SUBSYSTEM") else {
        return;
    };
    let Some(devname) = fields.get("DEVNAME") else {
        return;
    };
    // Defense in depth even though the sender is now verified as the
    // kernel: never let a DEVNAME value walk us outside /dev.
    if devname.contains("..") || devname.starts_with('/') {
        logging::warn(&format!("hotplug: rejecting suspicious DEVNAME '{devname}'"));
        return;
    }

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
        let Ok(s) = std::str::from_utf8(chunk) else {
            continue;
        };
        if let Some((k, v)) = s.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn apply_permissions(devname: &str, mode: u32) {
    let path = format!("/dev/{devname}");
    let Ok(c_path) = CString::new(path.clone()) else {
        return;
    };

    let ok = unsafe { libc::chmod(c_path.as_ptr(), mode) == 0 };
    if ok {
        logging::debug(&format!("hotplug: set {path} to mode {mode:o}"));
    } else {
        logging::debug(&format!(
            "hotplug: chmod {path} failed: {}",
            io::Error::last_os_error()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_pairs_from_nul_separated_payload() {
        let raw = b"add@/devices/virtual/tty/tty1\0ACTION=add\0SUBSYSTEM=tty\0DEVNAME=tty1\0";
        let fields = parse_event(raw);
        assert_eq!(fields.get("ACTION").map(String::as_str), Some("add"));
        assert_eq!(fields.get("SUBSYSTEM").map(String::as_str), Some("tty"));
        assert_eq!(fields.get("DEVNAME").map(String::as_str), Some("tty1"));
    }

    #[test]
    fn ignores_the_leading_summary_line() {
        // "add@/devices/foo" has no '=' so split_once finds nothing to insert.
        let raw = b"add@/devices/foo\0ACTION=add\0";
        let fields = parse_event(raw);
        assert_eq!(fields.len(), 1);
    }
}
