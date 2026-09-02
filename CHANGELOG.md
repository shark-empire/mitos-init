# Changelog

All notable changes to mitos-init are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/).

This project is pre-1.0: every version so far has been shaped by an
iterative build-then-verify-in-CI loop rather than a release process, and
none of it has been confirmed booting on real hardware yet. Treat version
numbers as development checkpoints, not stability guarantees, until noted
otherwise.

## [0.10.0] - Unreleased

### Changed
- **Breaking, architectural.** Service management moved out of this
  process entirely, into a new sibling binary,
  [mitos-services](../mitos-services): config/unit parsing, the
  supervisor, cgroups, the readiness protocol, transactional reload,
  dependency ordering, and privilege dropping (`config.rs`,
  `supervisor.rs`, `units.rs`, `cgroups.rs`, `notify.rs`, `rollback.rs`,
  `users.rs`) are all gone from this repo, moved essentially as-is.
  Reason: this process's release profile sets `panic = "abort"` - correct
  for PID 1, since a panic here takes the kernel down with it - and that
  stops being an acceptable trade-off once the logic involved gets
  complicated enough (dependency graphs, cycle detection, arbitrary
  third-party unit files) that the odds of an edge-case bug meaningfully
  rise. A bug in mitos-services can only crash mitos-services, which this
  process then restarts.
- mitos-init now spawns and supervises mitos-services as its one child,
  with the same crash-loop backoff shape services themselves used to get,
  falling back to a rescue shell if that also keeps failing.
- `reboot`/`poweroff`/`halt`/`shutdown` still target PID 1 directly
  (unchanged) - mitos-init relays the same signal to mitos-services, waits
  (bounded, via a FIFO handshake) for it to stop every service, then
  performs the actual `reboot(2)` syscall itself.
- Rescue mode (`single`/`mitos.rescue`) now bypasses mitos-services
  entirely - `exec`s a plain shell directly in place of this process,
  rather than depending on the thing that might be broken to also
  implement its own recovery path.
- Hostname setting moved to mitos-services (it's config-driven, and
  config parsing moved there too).

### Added
- `mount::setup_service_cgroup_root`: mounts cgroup2 and enables the
  "memory" controller for delegation (`cgroup.subtree_control`) before
  mitos-services exists - a real, previously-latent gap this migration
  surfaced (per-service `memory.max` limits need the controller enabled
  at each level of the hierarchy above them to actually take effect, not
  just to be writable).

## [0.9.0]

### Added
- Dependency ordering: `after=`/`After=` (spawn-order only, via
  topological sort) and `after_ready=`/`X-AfterReady=` (blocks briefly on
  the dependency's `READY=1` before starting the dependent).
- Privilege dropping: `user=`/`group=` (`init.conf`) and `User=`/`Group=`
  (unit files) run a service as a specific uid/gid instead of root,
  resolved via `/etc/passwd`/`/etc/group` (`users.rs`).
- `INTEGRATION.md`: the service-author-facing reference, distinct from
  `ASSEMBLY.md`'s "combine mitos-init and mitos-gui specifically" focus.

### Fixed
- `notify.rs`'s per-service sockets are now chowned to match a service's
  resolved `user=`/`group=`, not just left root-owned - without this, a
  privilege-dropped service couldn't have written to its own `0600`
  readiness socket, silently breaking `READY=1` reporting for exactly
  the services most likely to use the new privilege-dropping feature.

## [0.8.0]

### Added
- Rescue/single-user mode: `single` or `mitos.rescue` on the kernel
  command line skips configured services and starts a plain rescue shell
  instead - the escape hatch for a broken config.
- A panic hook that routes any unexpected panic through the normal
  logger (reaching `/dev/kmsg` when available) instead of a silent or
  invisible default stderr write.
- `CHANGELOG.md`, `SECURITY.md`, `CONTRIBUTING.md`, and a Troubleshooting
  section in the README.

## [0.7.0]

### Added
- Security hardening: `hotplug.rs` verifies uevents actually came from
  the kernel via `SCM_CREDENTIALS` (sender pid == 0) before acting on
  them; `notify.rs`'s per-service sockets are restricted to `0600`.
- Unit tests for every pure parsing function (`cmdline`, `config`,
  `units`, `cgroups::parse_size`, `hotplug::parse_event`,
  `supervisor::defs_equal`), plus a `cargo test` CI job.
- Dual MIT/Apache-2.0 licensing.
- An "Installing as your system's init" guide and a security-notes
  section in the README.

## [0.6.0]

### Added
- Transactional config reload: `SIGUSR1` now actually reconciles the
  running service set against the new config (previously it only updated
  log level/hostname), watches whatever it touched for 10 seconds, and
  automatically reverts to the previous config if a touched service fails
  hard in that window.

## [0.5.0]

### Added
- Per-service cgroup v2 containment: `cgroup.kill` on teardown reaches
  grandchildren a tracked pid alone couldn't, and `mem_max=`/`MemoryMax=`
  enforce a memory limit through the same mechanism.
- An sd_notify-compatible readiness protocol (`READY=1` via
  `$NOTIFY_SOCKET`), surfaced in the `SIGUSR2` status dump.

## [0.4.0]

### Added
- Declarative per-service unit files (`/etc/mitos/services.d/*.service`):
  systemd-style `[Section]`/`Key=Value` syntax, launchd-style
  drop-in-a-directory organization.
- Standard commands (`reboot`, `poweroff`, `halt`, `shutdown`) as thin
  wrappers that signal PID 1; `SIGINT`/`SIGTERM`/`SIGQUIT` now map to
  distinct reboot/poweroff/halt outcomes, matching the kernel's own
  Ctrl-Alt-Del convention.
- Boot-time `utmp`/`wtmp` logging (`who -b`, `last reboot`).
- The boot-time parts of FHS layout: `/run/lock`, `/var/run` and
  `/var/lock` compatibility symlinks.

## [0.3.0]

### Added
- A uevent listener (`hotplug.rs`) that fixes device permissions
  (tty, input) as they're hotplugged.
- `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo check`,
  `cargo clippy -D warnings`.

## [0.2.0]

### Added
- Initramfs -> real root switching (`switch_root.rs`): resolves `root=`/
  `rootfstype=`, mounts the real root, moves `/dev`/`/proc`/`/sys` into
  it, frees the initramfs's RAM, `chroot`s in.
- `mount.rs` split into early (`/dev`, `/proc`, `/sys`) and late
  (`/dev/pts`, `/dev/shm`, `/run`, `/tmp`) phases around the switch.

## [0.1.0]

### Added
- The original four boot-roadmap phases: PID 1 detection and
  dependency-light logging (Phase 0); full virtual filesystem mounting
  (Phase 1); signal handling with `SIGUSR1` config reload and `SIGUSR2`
  status dump (Phase 2); multi-service supervision with restart policy
  and crash-loop backoff (Phase 3).
- A dependency-free `init.conf` parser.
