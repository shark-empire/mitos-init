# mitosOS-on-Linux

`mitos-init` is the PID 1 process for MITOS: the first userspace program
the kernel runs. Deliberately minimal - it mounts the kernel-facing
filesystem tree, handles the initramfs -> real-root handoff, sets up
device permissions, and spawns/supervises exactly one child,
[mitos-services](../mitos-services), which does everything else (parsing
config, spawning and supervising the actual services, IPC, ...).

MITOS itself is a custom OS/distro built on the real Linux kernel - this
repo is one of three pieces, alongside `mitos-services` (the service
manager) and `mitos-gui` (the Wayland compositor/shell). See
[ASSEMBLY.md](ASSEMBLY.md) for how all three combine into an actual
bootable system.

## Why service management is a separate binary

Until recently, this repo *was* the service manager too. It moved out to
`mitos-services` because this process's release profile sets
`panic = "abort"` - the right call for PID 1, since a panic here doesn't
just crash a process, it takes the kernel down with it
(`Kernel panic - not syncing: Attempted to kill init!`). That's an
acceptable trade-off for a small, mostly `Result`-based boot sequence. It
stops being acceptable once the logic involved gets complicated enough -
dependency-graph cycle detection, parsing arbitrary third-party unit
files, IPC - that the odds of an edge-case bug meaningfully rise. A bug
in mitos-services can now only crash mitos-services, which this process
restarts (with the same crash-loop backoff shape individual services
used to get, falling back to a rescue shell if that also keeps failing).
See `CHANGELOG.md`'s 0.10.0 entry for the full migration, and
mitos-services' own README for what moved there.

## Status

| Phase | Scope | Status |
|---|---|---|
| 0 - Foundation | PID 1 detection, early logging, error handling | done |
| 1 - Kernel/userspace prep | /proc, /sys, /dev, /run, /tmp, /dev/pts, /dev/shm, cgroup2 | done |
| 2 - Signal handling | catch + relay SIGTERM/SIGINT/SIGQUIT/SIGUSR1/SIGUSR2 to mitos-services | done |
| 3 - Child supervision | spawn/restart mitos-services (single child, crash-loop backoff) | done |
| Root switch | initramfs -> real root (`root=`/`rootfstype=`, move-mount, chroot) | done |
| Hotplug | uevent listener for device permission fixups | done (small ruleset) |
| Standard commands | `reboot`/`poweroff`/`halt`/`shutdown`, utmp/wtmp boot record | done |
| FHS (boot-time parts) | `/run/lock`, `/var/run` and `/var/lock` compat symlinks | done |
| cgroup delegation | mounts cgroup2, enables "memory" controller for mitos-services | done |
| Rescue mode | `single`/`mitos.rescue` bypasses mitos-services, execs a plain shell | done |
| Panic diagnostics | unexpected panics routed through the logger (reaches `/dev/kmsg`) | done |
| Shutdown handshake | FIFO-based ack from mitos-services before the actual `reboot(2)` call | done |

SIGCHLD needs no explicit handler since the main loop already reaps via a
blocking `waitpid()`.

## How this talks to mitos-services

- `reboot`/`poweroff`/`halt`/`shutdown` (see `src/bin/`) still signal
  PID 1 directly - a kernel/sysvinit convention, unchanged by the split.
  **SIGINT = reboot**, **SIGTERM = poweroff**, **SIGQUIT = halt**,
  matching the same convention the kernel itself uses for Ctrl-Alt-Del.
- On receiving one of those, this process relays the same signal to
  mitos-services, waits (bounded, 20s) for it to stop every service and
  write an acknowledgement to `/run/mitos-init/shutdown-ack` (a FIFO this
  process creates), then performs the actual `reboot(2)`/sync itself -
  only this process ever does that.
- `SIGUSR1`/`SIGUSR2` (reload/status) are relayed the same way; `kill
  -USR1 1` / `kill -USR2 1` keep working exactly as before, just via a
  relay now. `mitosctl` (mitos-services' own CLI) is the richer
  alternative - see mitos-services' `INTEGRATION.md`.
- If mitos-services can't be started at all, or crashes more than 5
  times within 10 seconds, this process falls back to a rescue shell
  directly - the same escape hatch rescue mode uses, just triggered by a
  different condition.

The hotplug listener runs on its own thread, so signal delivery is
explicitly pinned to the main thread (`signals::block_handled` /
`unblock_handled`) before it's spawned - otherwise a signal could land on
the worker thread and never interrupt the main thread's `waitpid()`.

At boot, mitos-init writes a `BOOT_TIME` record via glibc's `utmpx` API
(`pututxline`) so `who -b` and `last reboot` work as expected; wtmp gets
the same record appended directly (`libc` doesn't bind `updwtmpx()`, so
`utmp.rs` does what that function does internally: open for append, write
the raw record bytes). Per-login session records
(`USER_PROCESS`/`DEAD_PROCESS`) are a getty/login program's job, not
init's.

If mitos-init is launched from an initramfs, it mounts the real root named
by the kernel's `root=` parameter, moves `/dev`, `/proc`, `/sys` into it,
frees the initramfs's RAM, then `chroot`s in - all before Phase 1's
tmpfs-backed mounts (`/dev/pts`, `/dev/shm`, `/run`, `/tmp`, `cgroup2`)
happen. `UUID=`/`LABEL=`/`PARTUUID=` are resolved via `/dev/disk/by-*`
symlinks if present; there's no built-in blkid-style superblock scanning,
so without udev those forms need a direct `root=/dev/...` path instead.
If mitos-init is already running from the real root (no initramfs stage),
this whole step is skipped automatically.

**cgroup delegation.** mitos-init mounts cgroup2 at `/sys/fs/cgroup` and
creates `/sys/fs/cgroup/mitos-init/` - the parent every per-service
cgroup mitos-services creates lives under - then enables the "memory"
controller for that subtree via `cgroup.subtree_control` at both the real
root cgroup and this one. That enabling step has to happen here, before
mitos-services even exists: a cgroup only grants a controller to its
*children* once the controller is listed in the cgroup's own
subtree_control, and this is the only point in the boot sequence closer
to the real root cgroup than mitos-services - itself a child process -
has any reason to reach.

## Layout

- `src/main.rs` - boot sequence, and the event loop that supervises
  mitos-services and handles the shutdown handshake
- `src/mount.rs` - Phase 1 virtual filesystem mounts (early + late),
  `/run` FHS setup, cgroup2 mount + delegation setup
- `src/switch_root.rs` - initramfs -> real root switch
- `src/cmdline.rs` - `/proc/cmdline` parser (used by switch_root and
  rescue-mode detection)
- `src/signals.rs` - catches SIGTERM/SIGINT/SIGQUIT/SIGUSR1/SIGUSR2 for
  relay to mitos-services
- `src/hotplug.rs` - uevent listener, fixes device permissions on hotplug
- `src/utmp.rs` - boot-time utmp/wtmp record (`who`/`last`)
- `src/bin/{reboot,poweroff,halt,shutdown}.rs` - PID 1-signaling companion commands
- `src/logging.rs` - dependency-free logger, writes to `/dev/kmsg` when available
- `src/error.rs` - shared error type
- `.github/workflows/ci.yml` - fmt/check/clippy/test on push and PR
- `.github/workflows/format.yml` - auto-formats and commits on push to main
- `rustfmt.toml` - pins the 2021-edition formatting rules
- `LICENSE-MIT` / `LICENSE-APACHE` - dual-licensed, matching the Rust ecosystem norm
- `ASSEMBLY.md` - how mitos-init, mitos-services, and mitos-gui combine into a bootable MITOS
- `CHANGELOG.md` / `SECURITY.md` / `CONTRIBUTING.md` - the usual public-project paperwork

Config parsing, unit files, cgroup *usage* (as opposed to the mount/
delegation setup above), the readiness protocol, transactional reload,
dependency ordering, and privilege dropping all live in mitos-services
now - see that repo's own README and layout section, and
`INTEGRATION.md` there for the service-author-facing reference.

## Build

```
cargo build --release
cargo test
```

The release profile (see `Cargo.toml`) is tuned for a small, fast-loading
binary - size optimization, LTO, one codegen unit, stripped symbols, no
unwind tables - since this is the very first thing the kernel loads off
disk and none of its work is compute-bound. Unit tests cover the pure
parsing functions that remain here (`cmdline`) - most of what used to
have test coverage in this repo (config/unit parsing, `defs_equal`,
`parse_size`, uevent parsing) moved to mitos-services along with the code
it tests. `hotplug::parse_event` still lives and is tested here.

## Installing as your system's init

This replaces PID 1. Getting it wrong means a machine that won't boot -
**test in a VM before real hardware**, every time, no exceptions.

1. `cargo build --release` in both this repo and mitos-services. Copy
   `target/release/mitos-init` onto the target rootfs - e.g. as
   `/sbin/init`, or wherever your bootloader's `init=` parameter will
   point - and copy mitos-services' `target/release/mitos-services` to
   `/sbin/mitos-services` (that exact path is currently hardcoded, see
   `main.rs`'s `SERVICES_BIN`).
2. Copy `target/release/{reboot,poweroff,halt,shutdown}` (this repo) and
   `mitosctl` (mitos-services) onto the rootfs too (e.g. `/sbin/`),
   somewhere on `PATH` for a root shell.
3. Set up config for mitos-services - see that repo's README/
   `INTEGRATION.md`. Not required; mitos-services falls back to a single
   default shell service with nothing on disk, same as before the split.
4. Point the kernel at it via the `init=` kernel command line parameter
   (GRUB, extlinux, whatever your bootloader is) - e.g. `init=/sbin/init`.
   If you're booting through an initramfs and want `switch_root.rs`'s
   handoff to the real root, make sure `root=` (and optionally
   `rootfstype=`) is set too - that's a standard kernel parameter, nothing
   mitos-init-specific.
5. If you're using an initramfs, rebuild it so both new binaries are
   actually inside the image you're booting.

A fast, low-risk way to iterate before touching real hardware:

```
qemu-system-x86_64 -kernel /path/to/vmlinuz -append "root=/dev/vda1 init=/sbin/init console=ttyS0" -drive file=disk.img,format=raw -nographic
```

Keep a known-working init binary available as a fallback boot entry
(a second GRUB entry, an initramfs you know boots) until you've confirmed
the new one works - the usual advice for anything that replaces PID 1.

## Security notes

**`hotplug.rs`** verifies every netlink message it acts on actually came
from the kernel (`SCM_CREDENTIALS`, sender pid == 0) before touching
device permissions - binding to the kobject-uevent multicast group alone
doesn't guarantee that, and this is the same check udev/systemd-udevd
do for the same reason. `DEVNAME` values are also rejected if they'd walk
outside `/dev` (`..`, a leading `/`), as defense in depth.

Everything about service-level security (privilege dropping, notify
socket permissions, config/unit file trust) now lives in mitos-services -
see that repo's own README and `SECURITY.md`.

## Troubleshooting

**System won't boot, or mitos-services/its shell isn't there.** Add
`single` or `mitos.rescue` to the kernel command line (edit the GRUB/
extlinux entry at the boot menu, or add it permanently while debugging).
That bypasses mitos-services entirely and execs a plain root shell - the
escape hatch for "something in the more complicated part is broken."

**mitos-services keeps crash-looping.** After 5 restarts within 10
seconds, mitos-init gives up and falls back to a rescue shell on its own
- you don't need to trigger rescue mode manually for this case, though
you still can to skip straight there.

**Where are the logs?** `dmesg` (or `cat /dev/kmsg`), if `/dev/kmsg` was
writable at the point each message was logged - mitos-init (and
mitos-services) prefer it specifically so logs survive even before a
syslog daemon exists. If `/dev/kmsg` wasn't available yet, messages fall
back to stdout/stderr, which on real hardware usually means the kernel's
console (serial or the main display) rather than anywhere persistent.

**Shutdown/reboot hangs.** mitos-init waits up to 20 seconds for
mitos-services to acknowledge it's finished stopping services before
proceeding on its own. If that's routinely timing out, something in
mitos-services' own shutdown path is stuck - see its Troubleshooting
notes.

**Something looks like a security issue**, as opposed to an ordinary
bug: see `SECURITY.md` rather than filing a public issue for it first.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache License 2.0](LICENSE-APACHE),
matching the Rust ecosystem's usual convention - use whichever suits your
project. `Cargo.toml`'s `authors` field is still the placeholder from the
original upload; update it (and the copyright line in both LICENSE files,
and the contact address in `SECURITY.md`) with real attribution before
publishing.
