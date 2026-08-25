# mitos-init

`mitos-init` is the PID 1 process for MITOS: the first userspace program
the kernel runs, responsible for preparing the environment and supervising
everything else that runs on top of it.

## Status

| Phase | Scope | Status |
|---|---|---|
| 0 - Foundation | PID 1 detection, early logging, error handling, config | done |
| 1 - Kernel/userspace prep | /proc, /sys, /dev, /run, /tmp, /dev/pts, /dev/shm | done |
| 2 - Signal handling | SIGTERM, SIGINT, SIGQUIT, SIGCHLD, SIGUSR1, SIGUSR2 | done |
| 3 - Process supervision | spawn, restart policy, reap zombies | done |
| Root switch | initramfs -> real root (`root=`/`rootfstype=`, move-mount, chroot) | done |
| Hotplug | uevent listener for device permission fixups | done (small ruleset) |

SIGUSR1 triggers a config reload (log level / hostname); SIGUSR2 dumps a
status summary of supervised services to the log. SIGCHLD needs no explicit
handler since the main loop already reaps via a blocking `waitpid()`.

The hotplug listener runs on its own thread, so signal delivery is
explicitly pinned to the main thread (`signals::block_handled` /
`unblock_handled`) before it's spawned - otherwise a signal could land on
the worker thread and never interrupt the main thread's `waitpid()`.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo check`,
and `cargo clippy -D warnings` on every push/PR.

If mitos-init is launched from an initramfs, it mounts the real root named
by the kernel's `root=` parameter, moves `/dev`, `/proc`, `/sys` into it,
frees the initramfs's RAM, then `chroot`s in - all before Phase 1's
tmpfs-backed mounts (`/dev/pts`, `/dev/shm`, `/run`, `/tmp`) and Phase 0's
config load happen. `UUID=`/`LABEL=`/`PARTUUID=` are resolved via
`/dev/disk/by-*` symlinks if present; there's no built-in blkid-style
superblock scanning, so without udev those forms need a direct `root=/dev/...`
path instead. If mitos-init is already running from the real root (no
initramfs stage), this whole step is skipped automatically.

## Layout

- `src/main.rs` - boot sequence and the PID 1 event loop
- `src/mount.rs` - Phase 1 virtual filesystem mounts (early + late)
- `src/switch_root.rs` - initramfs -> real root switch
- `src/cmdline.rs` - `/proc/cmdline` parser (used by switch_root)
- `src/signals.rs` - Phase 2 signal handlers
- `src/hotplug.rs` - uevent listener, fixes device permissions on hotplug
- `src/supervisor.rs` - Phase 3 service spawning, restart policy, reaping
- `src/config.rs` - parses `/etc/mitos/init.conf` (see `init.conf.example`)
- `src/logging.rs` - dependency-free logger, writes to `/dev/kmsg` when available
- `src/error.rs` - shared error type
- `.github/workflows/ci.yml` - fmt/check/clippy on push and PR
- `rustfmt.toml` - pins the 2021-edition formatting rules

## Build

```
cargo build --release
```

The release profile (see `Cargo.toml`) is tuned for a small, fast-loading
binary - size optimization, LTO, one codegen unit, stripped symbols, no
unwind tables - since this is the very first thing the kernel loads off
disk and none of its work is compute-bound.

## Config

Copy `init.conf.example` to `/etc/mitos/init.conf` on the target rootfs and
edit as needed. With no config file present, mitos-init falls back to
spawning `/bin/mitos-shell` (or `/bin/sh` if that's missing) as the sole,
critical service - the system is always bootable even with nothing on disk.
