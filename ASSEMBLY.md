# Assembling a bootable MITOS

mitos-init and mitos-gui are separate binaries that only become "MITOS"
once they're combined onto a real root filesystem the Linux kernel can
boot into. This is the missing piece between "two Rust projects that
build" and "an OS you can boot" - this doc is that piece.

## The pieces

- **Base rootfs.** MITOS runs on the real Linux kernel (see this repo's
  own README title), so you need *some* base userspace under it - at
  minimum glibc, since both mitos-init and mitos-gui link against it.
  Two realistic starting points:
  - **Minimal/custom** (buildroot, or a hand-rolled rootfs): smallest,
    fully under your control, more work up front.
  - **Stripped existing distro** (a minimal Arch/Debian/Alpine install
    with systemd and friends removed): faster to get a boot working,
    more baggage to strip out.

  mitos-init exists specifically to replace systemd/sysvinit, so the
  minimal/custom route is the more coherent long-term choice - but
  stripping an existing distro's rootfs is a reasonable way to get
  something booting sooner while you decide.

- **mitos-init as PID 1.** Covered in this repo's own README
  ("Installing as your system's init"): build it, drop the binary
  wherever `init=` points, set up `/etc/mitos/init.conf` and/or
  `services.d/`.

- **mitos-gui as the session.** Not yet, and that's fine - it needs
  Stage 5 (DRM/GBM/libseat, direct hardware boot) before it can run as
  mitos-init's *critical* service with no existing display session
  underneath it. Until Stage 5 lands, keep `/bin/mitos-shell` (or
  `/bin/sh`) as the critical service, and run mitos-gui the way you'd
  test any Wayland compositor today - manually, from a TTY, under
  whatever session is already there.

## Once mitos-gui reaches Stage 5

Copy `services.d.example/mitos-gui.service` into
`/etc/mitos/services.d/` and remove (or de-prioritize) the `mitos-shell`
entry - `Supervisor::reload_services` will pick up the swap on the next
`SIGUSR1`, transactionally (see `rollback.rs`): if mitos-gui doesn't come
up cleanly, mitos-init reverts back to the shell automatically rather
than leaving the machine with no session at all.

Have mitos-gui call `sd_notify`-compatible `READY=1` - write `READY=1\n`
to the path in the `$NOTIFY_SOCKET` env var mitos-init already sets for
every service - once it actually has an output rendering. That's the
same protocol real systemd-aware daemons use (see `notify.rs`), not
mitos-specific plumbing, and it's what makes mitos-init's SIGUSR2 status
dump show mitos-gui as genuinely "ready" instead of just "spawned, who
knows."

## A minimal build/assemble script

A starting skeleton, not a finished pipeline - adjust paths for your
actual repo layout:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOTFS=./mitos-rootfs      # wherever your base rootfs lives
MITOS_INIT=../mitos-init
MITOS_GUI=../mitos-gui

# Build
( cd "$MITOS_INIT" && cargo build --release )
( cd "$MITOS_GUI" && cargo build --release )

# Install into the rootfs
install -Dm755 "$MITOS_INIT/target/release/mitos-init" "$ROOTFS/sbin/init"
for bin in reboot poweroff halt shutdown; do
  install -Dm755 "$MITOS_INIT/target/release/$bin" "$ROOTFS/sbin/$bin"
done
install -Dm755 "$MITOS_GUI/target/release/mitos-gui" "$ROOTFS/usr/bin/mitos-gui"
install -Dm644 "$MITOS_INIT/init.conf.example" "$ROOTFS/etc/mitos/init.conf"

echo "rootfs staged at $ROOTFS - point your bootloader's init= and root= at it"
```

Test every change in a VM (see the README's QEMU example) before real
hardware - same advice as installing mitos-init at all, doubly true once
mitos-gui is in the boot path too.
