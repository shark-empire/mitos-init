//! Mounts the virtual filesystems userspace expects to find in place
//! (Phase 1 of the boot roadmap), split into two stages:
//!
//! - `mount_early_vfs`: /dev, /proc, /sys - needed before we can read
//!   /proc/cmdline or find block devices for a possible root switch.
//! - `mount_late_vfs`: /dev/pts, /dev/shm, /run, /tmp - fresh tmpfs mounts
//!   that belong on the final root, so these run after
//!   `switch_root::perform()` (if it ran at all).

use crate::error::{InitError, Result};
use crate::logging;
use nix::mount::{mount, MsFlags};
use std::fs;

struct VfsMount {
    source: &'static str,
    target: &'static str,
    fstype: &'static str,
    flags: MsFlags,
    data: Option<&'static str>,
}

/// Returns true if `target` already appears as a mount point in
/// /proc/mounts. Re-mounting something the kernel or an initramfs already
/// mounted just produces noisy EBUSY warnings, so we skip it instead.
fn already_mounted(target: &str) -> bool {
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return false;
    };
    mounts.lines().any(|line| line.split_whitespace().nth(1) == Some(target))
}

fn do_mount(m: &VfsMount) -> Result<()> {
    fs::create_dir_all(m.target).map_err(InitError::Io)?;

    if already_mounted(m.target) {
        logging::debug(&format!("{} already mounted, skipping", m.target));
        return Ok(());
    }

    mount(Some(m.source), m.target, Some(m.fstype), m.flags, m.data)
        .map_err(|e| InitError::Mount { target: m.target.to_string(), source: e })?;
    logging::info(&format!("mounted {} ({})", m.target, m.fstype));
    Ok(())
}

/// Individual failures are logged and skipped rather than aborting boot -
/// a missing /dev/shm shouldn't stop the machine from coming up - but the
/// caller gets back anything that failed in case it needs to react.
fn run_mounts(mounts: &[VfsMount]) -> Vec<InitError> {
    let mut errors = Vec::new();
    for m in mounts {
        if let Err(e) = do_mount(m) {
            logging::warn(&format!("{e}"));
            errors.push(e);
        }
    }
    errors
}

pub fn mount_early_vfs() -> Vec<InitError> {
    use MsFlags as F;
    let mounts = [
        VfsMount { source: "devtmpfs", target: "/dev", fstype: "devtmpfs", flags: F::MS_NOSUID, data: Some("mode=755") },
        VfsMount { source: "proc", target: "/proc", fstype: "proc", flags: F::MS_NOSUID | F::MS_NOEXEC | F::MS_NODEV, data: None },
        VfsMount { source: "sysfs", target: "/sys", fstype: "sysfs", flags: F::MS_NOSUID | F::MS_NOEXEC | F::MS_NODEV, data: None },
    ];
    run_mounts(&mounts)
}

pub fn mount_late_vfs() -> Vec<InitError> {
    use MsFlags as F;
    let mounts = [
        VfsMount { source: "devpts", target: "/dev/pts", fstype: "devpts", flags: F::MS_NOSUID | F::MS_NOEXEC, data: Some("mode=620,gid=5") },
        VfsMount { source: "tmpfs", target: "/dev/shm", fstype: "tmpfs", flags: F::MS_NOSUID | F::MS_NODEV, data: Some("mode=1777") },
        VfsMount { source: "tmpfs", target: "/run", fstype: "tmpfs", flags: F::MS_NOSUID | F::MS_NODEV, data: Some("mode=755") },
        VfsMount { source: "tmpfs", target: "/tmp", fstype: "tmpfs", flags: F::MS_NOSUID | F::MS_NODEV, data: Some("mode=1777") },
    ];
    run_mounts(&mounts)
}
