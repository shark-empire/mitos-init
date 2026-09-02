# Assembling a bootable MITOS

mitos-init, mitos-services, and mitos-gui are separate binaries that
only become "MITOS" once they're combined onto a real root filesystem
the Linux kernel can boot into. This is the missing piece between
"three Rust projects that build" and "an OS you can boot" - this doc is
that piece.

(mitos-init used to be the whole service-management story by itself;
that moved to mitos-services - see this repo's own README, "Why service
management is a separate binary" - so as of that split, assembling MITOS
means combining three pieces, not two.)

## The pieces

- **Base rootfs.** MITOS runs on the real Linux kernel (see this repo's
  own README title), so you need *some* base userspace under it - at
  minimum glibc, since all three binaries link against it. Two realistic
  starting points:
  - **Minimal/custom** (buildroot, or a hand-rolled rootfs): smallest,
    fully under your control, more work up front.
  - **Stripped existing distro** (a minimal Arch/Debian/Alpine install
    with systemd and friends removed): faster to get a boot working,
    more baggage to strip out.

  mitos-init/mitos-services exist specifically to replace systemd/
  sysvinit, so the minimal/custom route is the more coherent long-term
  choice - but stripping an existing distro's rootfs is a reasonable way
  to get something booting sooner while you decide.

- **mitos-init as PID 1.** Covered in this repo's own README
  ("Installing as your system's init"): build both mitos-init and
  mitos-services, drop the binaries wherever `init=` and
  `SERVICES_BIN` (`/sbin/mitos-services`) point.

- **mitos-services as the service manager.** Spawned and supervised by
  mitos-init automatically - nothing extra to wire up beyond getting the
  binary onto the rootfs at the right path. Config lives at
  `/etc/mitos/init.conf` and/or `/etc/mitos/services.d/` - see
  mitos-services' own README/`INTEGRATION.md`.

- **mitos-gui as the session.** Not yet, and that's fine - it needs
  Stage 5 (DRM/GBM/libseat, direct hardware boot) before it can run as a
  *critical* service with no existing display session underneath it.
  Until Stage 5 lands, keep `/bin/mitos-shell` (or `/bin/sh`) as the
  critical service, and run mitos-gui the way you'd test any Wayland
  compositor today - manually, from a TTY, under whatever session is
  already there.

## Once mitos-gui reaches Stage 5

Copy `mitos-services/services.d.example/mitos-gui.service` into
`/etc/mitos/services.d/` and remove (or de-prioritize) the `mitos-shell`
entry - mitos-services' `reload_services` will pick up the swap on the
next reload (`mitosctl reload`, or `SIGUSR1` relayed through mitos-init),
transactionally: if mitos-gui doesn't come up cleanly, the swap reverts
back to the shell automatically rather than leaving the machine with no
session at all.

Have mitos-gui call `sd_notify`-compatible `READY=1` - write `READY=1\n`
to the path in the `$NOTIFY_SOCKET` env var mitos-services sets for
every service - once it actually has an output rendering. That's the
same protocol real systemd-aware daemons use, not mitos-specific
plumbing, and it's what makes `mitosctl status` show mitos-gui as
genuinely "ready" instead of just "spawned, who knows."

## A minimal build/assemble script

A starting skeleton, not a finished pipeline - adjust paths for your
actual repo layout:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOTFS=./mitos-rootfs      # wherever your base rootfs lives
MITOS_INIT=../mitos-init
MITOS_SERVICES=../mitos-services
MITOS_GUI=../mitos-gui

# Build
( cd "$MITOS_INIT" && cargo build --release )
( cd "$MITOS_SERVICES" && cargo build --release )
( cd "$MITOS_GUI" && cargo build --release )

# Install into the rootfs
install -Dm755 "$MITOS_INIT/target/release/mitos-init" "$ROOTFS/sbin/init"
for bin in reboot poweroff halt shutdown; do
  install -Dm755 "$MITOS_INIT/target/release/$bin" "$ROOTFS/sbin/$bin"
done
install -Dm755 "$MITOS_SERVICES/target/release/mitos-services" "$ROOTFS/sbin/mitos-services"
install -Dm755 "$MITOS_SERVICES/target/release/mitosctl" "$ROOTFS/sbin/mitosctl"
install -Dm755 "$MITOS_GUI/target/release/mitos-gui" "$ROOTFS/usr/bin/mitos-gui"
install -Dm644 "$MITOS_SERVICES/init.conf.example" "$ROOTFS/etc/mitos/init.conf"

echo "rootfs staged at $ROOTFS - point your bootloader's init= and root= at it"
```

Test every change in a VM (see the README's QEMU example) before real
hardware - same advice as installing mitos-init at all, doubly true now
that there are three cooperating processes in the boot path instead of
one.
