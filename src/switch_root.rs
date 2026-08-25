//! Optional root filesystem switch, for booting from an initramfs.
//!
//! If the kernel booted mitos-init from an initramfs (a `rootfs`/`tmpfs`
//! held entirely in RAM), the real root filesystem named by the kernel's
//! `root=` command line parameter still needs to be mounted and swapped in
//! before boot can continue. This module does that swap. If we're already
//! running from a real, disk-backed root - no initramfs stage, or it
//! already ran - `needed()` returns false and the rest of boot proceeds
//! unchanged.
//!
//! No re-exec happens after the switch: because we `chroot()`/`chdir()`
//! rather than spawning a new process, every path this process resolves
//! afterward (config, service binaries, ...) automatically refers to the
//! new root.

use crate::cmdline;
use crate::error::{InitError, Result};
use crate::logging;
use nix::mount::{mount, MsFlags};
use nix::unistd::{chdir, chroot};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const NEW_ROOT: &str = "/newroot";

/// True if `/` still looks like an initramfs (tmpfs/ramfs/rootfs) rather
/// than a real, disk-backed filesystem.
pub fn needed() -> bool {
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return false; // can't tell; assume no switch needed rather than risk a bad mount
    };
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let _source = fields.next();
        let target = fields.next();
        let fstype = fields.next();
        if target == Some("/") {
            return matches!(fstype, Some("rootfs") | Some("tmpfs") | Some("ramfs"));
        }
    }
    false
}

/// Resolves the kernel's `root=` parameter to an actual device path.
/// Supports a direct device path (`/dev/sda2`) and the common
/// `UUID=`/`LABEL=`/`PARTUUID=` forms *if* the corresponding
/// `/dev/disk/by-*` symlink already exists (normally populated by udev,
/// which this minimal system may not run). There's no blkid-style
/// superblock scanning here - if the symlink isn't there, this returns an
/// error rather than guessing.
fn resolve_root_device(args: &HashMap<String, Option<String>>) -> Result<String> {
    let root = args
        .get("root")
        .and_then(|v| v.clone())
        .ok_or_else(|| InitError::Boot("no root= on kernel command line".to_string()))?;

    if let Some(uuid) = root.strip_prefix("UUID=") {
        return resolve_by_symlink("/dev/disk/by-uuid", uuid);
    }
    if let Some(label) = root.strip_prefix("LABEL=") {
        return resolve_by_symlink("/dev/disk/by-label", label);
    }
    if let Some(partuuid) = root.strip_prefix("PARTUUID=") {
        return resolve_by_symlink("/dev/disk/by-partuuid", partuuid);
    }
    Ok(root) // assume it's already a device path like /dev/sda2
}

fn resolve_by_symlink(dir: &str, name: &str) -> Result<String> {
    let path = Path::new(dir).join(name);
    if path.exists() {
        Ok(path.to_string_lossy().into_owned())
    } else {
        Err(InitError::Boot(format!(
            "{dir}/{name} doesn't exist (no udev rules to populate it?) - pass root=/dev/... directly"
        )))
    }
}

/// Filesystem types the kernel knows about, excluding `nodev` pseudo-fs
/// entries, in `/proc/filesystems` order (the kernel lists its preferred
/// guess first).
fn candidate_fstypes() -> Vec<String> {
    fs::read_to_string("/proc/filesystems")
        .map(|text| {
            text.lines()
                .filter(|l| !l.starts_with("nodev"))
                .map(|l| l.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Mounts `device` at `target`, using `explicit_fstype` if given, otherwise
/// trying each known filesystem type until one works (mount() doesn't
/// autodetect fstype for us).
fn mount_root(device: &str, target: &str, explicit_fstype: Option<&str>) -> Result<()> {
    fs::create_dir_all(target).map_err(InitError::Io)?;

    if let Some(fstype) = explicit_fstype {
        return mount(
            Some(device),
            target,
            Some(fstype),
            MsFlags::empty(),
            None::<&str>,
        )
        .map_err(|e| InitError::Mount {
            target: target.to_string(),
            source: e,
        });
    }

    for fstype in candidate_fstypes() {
        if mount(
            Some(device),
            target,
            Some(fstype.as_str()),
            MsFlags::empty(),
            None::<&str>,
        )
        .is_ok()
        {
            logging::info(&format!("mounted root {device} as {fstype}"));
            return Ok(());
        }
    }

    Err(InitError::Boot(format!(
        "couldn't mount {device} as any known filesystem type"
    )))
}

/// Moves an already-mounted filesystem from its current location under `/`
/// to the same relative path under the new root, instead of unmounting and
/// remounting it.
fn move_mount(name: &str) -> Result<()> {
    let old = format!("/{name}");
    let new = format!("{NEW_ROOT}/{name}");
    if !Path::new(&old).exists() {
        return Ok(()); // wasn't mounted yet; mount::mount_late_vfs handles it after the switch
    }
    fs::create_dir_all(&new).map_err(InitError::Io)?;
    mount(
        Some(old.as_str()),
        new.as_str(),
        None::<&str>,
        MsFlags::MS_MOVE,
        None::<&str>,
    )
    .map_err(|e| InitError::Mount {
        target: new,
        source: e,
    })
}

/// Best-effort recursive delete of the old initramfs content once it's
/// been superseded, to free the RAM it was using. Anything that's a
/// separate mounted filesystem (a different `st_dev`) - /newroot itself,
/// and anything /dev, /proc, /sys were already moved into - is left alone
/// rather than descended into. Must run before the final move-mount below,
/// while the old root is still reachable at "/".
fn cleanup_initramfs() {
    use std::os::unix::fs::MetadataExt;

    let Ok(root_meta) = fs::metadata("/") else {
        return;
    };
    let root_dev = root_meta.dev();

    let Ok(entries) = fs::read_dir("/") else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.dev() != root_dev {
            continue; // separate mounted filesystem - leave it alone
        }
        let result = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(e) = result {
            logging::debug(&format!(
                "initramfs cleanup: couldn't remove {}: {e}",
                path.display()
            ));
        }
    }
}

/// Performs the full initramfs -> real-root handoff.
pub fn perform() -> Result<()> {
    let args = cmdline::parse();
    let device = resolve_root_device(&args)?;
    let fstype = args.get("rootfstype").and_then(|v| v.as_deref());

    logging::info(&format!("switching root to {device}"));
    mount_root(&device, NEW_ROOT, fstype)?;

    for fs_name in ["dev", "proc", "sys"] {
        if let Err(e) = move_mount(fs_name) {
            logging::warn(&format!("couldn't move /{fs_name} into new root: {e}"));
        }
    }

    cleanup_initramfs();

    chdir(NEW_ROOT).map_err(|e| InitError::Boot(format!("chdir({NEW_ROOT}) failed: {e}")))?;
    mount(Some("."), "/", None::<&str>, MsFlags::MS_MOVE, None::<&str>).map_err(|e| {
        InitError::Mount {
            target: "/".to_string(),
            source: e,
        }
    })?;
    chroot(".").map_err(|e| InitError::Boot(format!("chroot failed: {e}")))?;
    chdir("/").map_err(|e| InitError::Boot(format!("chdir(/) after chroot failed: {e}")))?;

    logging::info("root switch complete");
    Ok(())
}
