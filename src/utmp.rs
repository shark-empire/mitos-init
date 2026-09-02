//! Boot-time utmp/wtmp logging, so standard tools (`who -b`, `last reboot`)
//! see the boot the way they would under sysvinit/systemd. Login programs
//! (getty, login) own the USER_PROCESS/DEAD_PROCESS session records for
//! individual logins - this only logs the BOOT_TIME record that init
//! itself is responsible for.
//!
//! Uses glibc's own utmpx API (`pututxline`) for the utmp database itself,
//! since it already knows the real default file path and locking. wtmp is
//! simpler than it looks though - it's just a flat file of back-to-back
//! `utmpx` records - and the libc crate doesn't bind glibc's `updwtmpx()`
//! helper for appending one, so `append_to_wtmp` does exactly what that
//! function does internally: open for append, write the raw record bytes.

use crate::logging;
use libc::{c_char, utmpx, BOOT_TIME};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

const WTMP_PATH: &str = "/var/log/wtmp";

fn copy_into<const N: usize>(dst: &mut [c_char; N], src: &str) {
    for (slot, byte) in dst.iter_mut().zip(src.bytes().take(N)) {
        *slot = byte as c_char;
    }
}

/// Best-effort: a missing or unwritable utmp/wtmp database shouldn't
/// affect boot, so failures here are logged at debug level, not
/// propagated.
pub fn log_boot() {
    let mut entry: utmpx = unsafe { std::mem::zeroed() };
    entry.ut_type = BOOT_TIME;
    copy_into(&mut entry.ut_line, "~");
    copy_into(&mut entry.ut_id, "~~");
    copy_into(&mut entry.ut_user, "reboot");

    let now = unsafe { libc::time(std::ptr::null_mut()) };
    // utmpx's timestamp field is glibc's internal `__timeval`, not the
    // usual public `timeval` - a distinct type despite matching fields.
    entry.ut_tv = libc::__timeval {
        tv_sec: now as _,
        tv_usec: 0,
    };

    unsafe {
        libc::setutxent();
        if libc::pututxline(&entry).is_null() {
            logging::debug("couldn't write utmp boot record");
        }
        libc::endutxent();
    }

    append_to_wtmp(&entry);
}

fn append_to_wtmp(entry: &utmpx) {
    let bytes = unsafe {
        std::slice::from_raw_parts(
            entry as *const utmpx as *const u8,
            std::mem::size_of::<utmpx>(),
        )
    };

    match OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o664)
        .open(WTMP_PATH)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(bytes) {
                logging::debug(&format!("couldn't append to {WTMP_PATH}: {e}"));
            }
        }
        Err(e) => logging::debug(&format!("couldn't open {WTMP_PATH}: {e}")),
    }
}
