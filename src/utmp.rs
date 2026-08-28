//! Boot-time utmp/wtmp logging, so standard tools (`who -b`, `last reboot`)
//! see the boot the way they would under sysvinit/systemd. Login programs
//! (getty, login) own the USER_PROCESS/DEAD_PROCESS session records for
//! individual logins - this only logs the BOOT_TIME record that init
//! itself is responsible for.
//!
//! Uses glibc's own utmpx functions (`pututxline`/`updwtmpx`) rather than
//! hand-writing the utmp file format - they already know the real record
//! layout, default file paths, and locking, which is exactly the kind of
//! detail not worth re-deriving by hand.

use crate::logging;
use libc::{c_char, timeval, utmpx, BOOT_TIME};

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
    entry.ut_tv = timeval { tv_sec: now as _, tv_usec: 0 };

    unsafe {
        libc::setutxent();
        if libc::pututxline(&entry).is_null() {
            logging::debug("couldn't write utmp boot record");
        }
        libc::endutxent();

        if let Ok(path) = std::ffi::CString::new(WTMP_PATH) {
            libc::updwtmpx(path.as_ptr(), &entry);
        }
    }
}
