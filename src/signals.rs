//! Signal handling for PID 1 (Phase 2).
//!
//! As PID 1, this process is the default target for SIGTERM/SIGINT/SIGQUIT
//! (e.g. from `shutdown`, Ctrl-Alt-Del via the kernel, or a VM manager) and
//! is required to reap SIGCHLD so children never linger as zombies. The
//! handlers below only touch `AtomicBool`s — no allocation, no I/O — which
//! is about the only thing that's safe to do inside a signal handler.

use crate::error::{InitError, Result};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use std::sync::atomic::AtomicBool;

pub static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// SIGUSR1: reload `/etc/mitos/init.conf` (log level, hostname) without a reboot.
pub static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
/// SIGUSR2: dump a one-line status summary of supervised services to the log.
pub static STATUS_DUMP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(raw: i32) {
    use std::sync::atomic::Ordering::SeqCst;
    if raw == Signal::SIGTERM as i32 || raw == Signal::SIGINT as i32 || raw == Signal::SIGQUIT as i32 {
        SHUTDOWN_REQUESTED.store(true, SeqCst);
    } else if raw == Signal::SIGUSR1 as i32 {
        RELOAD_REQUESTED.store(true, SeqCst);
    } else if raw == Signal::SIGUSR2 as i32 {
        STATUS_DUMP_REQUESTED.store(true, SeqCst);
    }
    // SIGCHLD is intentionally left at its default disposition: the main
    // loop already reaps everything via a blocking waitpid(), so a custom
    // handler would only add signal-safety risk for no benefit.
}

/// Installs handlers for the signals PID 1 needs to care about.
///
/// Deliberately does *not* set `SA_RESTART`: we want a blocked `waitpid()`
/// in the main loop to return `EINTR` when one of these arrives, so the
/// loop promptly checks the flags above instead of waiting for the next
/// child to exit first.
pub fn install_handlers() -> Result<()> {
    let action = SigAction::new(SigHandler::Handler(on_signal), SaFlags::empty(), SigSet::empty());
    unsafe {
        signal::sigaction(Signal::SIGTERM, &action).map_err(InitError::Signal)?;
        signal::sigaction(Signal::SIGINT, &action).map_err(InitError::Signal)?;
        signal::sigaction(Signal::SIGQUIT, &action).map_err(InitError::Signal)?;
        signal::sigaction(Signal::SIGUSR1, &action).map_err(InitError::Signal)?;
        signal::sigaction(Signal::SIGUSR2, &action).map_err(InitError::Signal)?;
    }
    Ok(())
}
