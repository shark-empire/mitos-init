# Contributing

## Before you start

This replaces PID 1 on whatever machine runs it - a subtle bug here is a
machine that won't boot, not a crashed request handler. A few habits the
existing code already follows, worth keeping up:

- **No panics in reachable code paths.** Every fallible operation returns
  `Result` (see `error.rs`) instead of `.unwrap()`/`.expect()`/`panic!()`.
  A panic in PID 1 takes the kernel down with it - `install_panic_hook`
  in `main.rs` exists to make an *unexpected* one visible in `dmesg`, not
  as a substitute for handling errors properly.
- **Degrade, don't abort.** A missing optional subsystem (an old kernel
  without a feature, a mounting failure) should log a warning and
  continue, not stop boot. Check how `mount.rs`'s mount failures, or
  `mount::setup_service_cgroup_root`'s fallback, are handled for the
  pattern.
- **New parsing logic gets tests.** Anything that parses text should have
  unit tests alongside it - see the `#[cfg(test)]` module at the bottom
  of `cmdline.rs` for the existing pattern (most of what used to be
  tested here - config, units, `cgroups::parse_size`,
  `supervisor::defs_equal` - moved to mitos-services along with the code
  it tests; `hotplug::parse_event` is the one still living here).
  Logic that needs root or a real kernel (mounts, the actual boot
  sequence) doesn't have automated tests yet; that's what VM testing is
  for.

## Workflow

1. `cargo fmt --all` before committing (or let `.github/workflows/format.yml`
   do it for you on push to `main`).
2. `cargo clippy --all-targets -- -D warnings` and `cargo test` should
   both be clean - CI (`.github/workflows/ci.yml`) runs both, along with
   `cargo check` and a `fmt --check`.
3. Test any change touching boot behavior in a VM before considering it
   done - see the README's "Installing as your system's init" section
   for a QEMU example. This project doesn't have hardware-in-the-loop CI,
   so that step doesn't happen automatically.

## Where things live

See the "Layout" section in README.md for a one-line-per-file map of the
codebase, and ASSEMBLY.md for how this repo relates to mitos-services
and mitos-gui, and the rest of MITOS. Service-management contributions
(config parsing, supervision, cgroups, IPC, ...) belong in
mitos-services now, with its own CONTRIBUTING.md - this repo is just the
minimal PID 1 bridge.
