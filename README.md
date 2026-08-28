# mitosOS-on-Linux

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
| Standard commands | `reboot`/`poweroff`/`halt`/`shutdown`, utmp/wtmp boot record | done |
| Declarative services | `/etc/mitos/services.d/*.service` unit files | done |
| FHS (boot-time parts) | `/run/lock`, `/var/run` and `/var/lock` compat symlinks | done |

SIGUSR1 triggers a config reload (log level / hostname, and re-scans
`services.d`); SIGUSR2 dumps a status summary of supervised services to the
log. SIGCHLD needs no explicit handler since the main loop already reaps
via a blocking `waitpid()`.

The three shutdown-family signals now map to three distinct outcomes,
matching the convention the kernel itself uses for Ctrl-Alt-Del: **SIGINT
= reboot**, **SIGTERM = poweroff**, **SIGQUIT = halt** (stop without
cutting power). The `reboot`/`poweroff`/`halt`/`shutdown` binaries under
`src/bin/` are thin wrappers that just send the matching signal to PID 1 -
same as traditional sysvinit tooling. They need root or `CAP_KILL`, same
as real `reboot`/`shutdown` normally being setuid or root-only.

At boot, mitos-init writes a `BOOT_TIME` record via glibc's `utmpx` API
(`pututxline`/`updwtmpx`) so `who -b` and `last reboot` work as expected.
Per-login session records (`USER_PROCESS`/`DEAD_PROCESS`) are a getty/login
program's job, not init's.

Services can now come from three places, merged together: `init.conf`'s
inline `service` lines, and `/etc/mitos/services.d/*.service` unit files -
plain `[Service]` / `ExecStart=`/`Restart=` systemd-style syntax (not real
systemd, just the same easy-to-parse format), organized one-file-per-service
the way launchd's LaunchDaemons directory is (not real XML plists - see
`units.rs` for why). `X-Critical=true` is a systemd-spec-legal vendor
extension key for marking a unit critical the way `init.conf`'s inline
`critical=true` does.

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
- `src/mount.rs` - Phase 1 virtual filesystem mounts (early + late), `/run` FHS setup
- `src/switch_root.rs` - initramfs -> real root switch
- `src/cmdline.rs` - `/proc/cmdline` parser (used by switch_root)
- `src/signals.rs` - Phase 2 signal handlers, reboot/poweroff/halt semantics
- `src/hotplug.rs` - uevent listener, fixes device permissions on hotplug
- `src/supervisor.rs` - Phase 3 service spawning, restart policy, reaping
- `src/config.rs` - parses `/etc/mitos/init.conf`, merges in services.d units
- `src/units.rs` - `/etc/mitos/services.d/*.service` unit file loader
- `src/utmp.rs` - boot-time utmp/wtmp record (`who`/`last`)
- `src/bin/{reboot,poweroff,halt,shutdown}.rs` - PID 1-signaling companion commands
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
