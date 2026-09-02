# Security Policy

mitos-init runs as PID 1 with full root privileges, and this project is
pre-1.0 and not yet confirmed working on real hardware (see
CHANGELOG.md) - please report anything that looks like a security issue
rather than opening a public issue for it first.

## Reporting a vulnerability

Email <security@example.invalid> with a description of the issue and,
if possible, steps to reproduce it. (Replace this address with a real
contact before publishing this project - see the note in README.md's
License section about placeholder attribution.)

Please don't open a public GitHub issue for a suspected vulnerability
until there's been a chance to assess and, where needed, fix it first.

## Scope

For this repo specifically: privilege escalation, memory-safety issues in
the small amount of `unsafe` FFI code (`hotplug.rs`, `utmp.rs`,
`mount.rs`), and anything that lets an unprivileged local process
influence PID 1's behavior it shouldn't be able to (spoofed uevents, or
anything that lets a process other than mitos-init/mitos-services signal
its way into the shutdown handshake). Report cgroup/notify-socket
permission issues and service-level privilege concerns to
mitos-services' own `SECURITY.md` instead - that code moved there; see
this repo's CHANGELOG (0.10.0) for the split.

Known, already-documented limitations that are *not* new reports: config/
unit files (parsed by mitos-services, not this repo) are trusted as-is
with no permission or signature checking - called out explicitly in
mitos-services' own README as the current trust model, not an oversight.

## Supported versions

Pre-1.0: only the latest commit on the default branch is supported.
There's no backport policy yet.
