//! mitos-init — the PID 1 process for MITOS.
//!
//! Boot sequence: mount the virtual filesystem tree (Phase 1), load config
//! and install signal handlers (Phase 0/2), spawn the configured services
//! (Phase 3), then sit in an event loop reaping children and reacting to
//! signals until it's time to shut down.

mod cmdline;
mod config;
mod error;
mod hotplug;
mod logging;
mod mount;
mod signals;
mod supervisor;
mod switch_root;

use nix::sys::wait::waitpid;
use nix::unistd::Pid;
use std::sync::atomic::Ordering;
use std::time::Duration;
use supervisor::{Outcome, Supervisor};

const CONFIG_PATH: &str = "/etc/mitos/init.conf";

fn main() {
    logging::init();
    logging::info("mitos-init starting");

    if nix::unistd::getpid().as_raw() != 1 {
        logging::warn(
            "not running as PID 1 - mount/reboot calls will likely fail; continuing anyway",
        );
    }

    for err in mount::mount_early_vfs() {
        logging::warn(&format!("continuing despite mount failure: {err}"));
    }

    if switch_root::needed() {
        match switch_root::perform() {
            Ok(()) => logging::info("now running from the real root filesystem"),
            Err(e) => logging::error(&format!(
                "root switch failed, continuing from initramfs: {e}"
            )),
        }
    }

    for err in mount::mount_late_vfs() {
        logging::warn(&format!("continuing despite mount failure: {err}"));
    }

    let cfg = config::load_or_default(CONFIG_PATH);
    logging::set_level(cfg.loglevel);

    if let Some(hostname) = &cfg.hostname {
        if let Err(e) = nix::unistd::sethostname(hostname) {
            logging::warn(&format!("failed to set hostname to '{hostname}': {e}"));
        }
    }

    if let Err(e) = signals::install_handlers() {
        logging::error(&format!("failed to install signal handlers: {e}"));
    }
    // Block the signals we handle on this (main) thread before spawning any
    // worker threads, so they inherit the block and can't steal delivery
    // from the thread that's actually waiting on it - see signals::block_handled.
    if let Err(e) = signals::block_handled() {
        logging::warn(&format!(
            "failed to block signals ahead of worker threads: {e}"
        ));
    }

    hotplug::spawn_listener();

    let mut sup = Supervisor::new();
    sup.spawn_all(&cfg.services);

    if let Err(e) = signals::unblock_handled() {
        logging::error(&format!("failed to unblock signals: {e}"));
    }

    run_event_loop(&mut sup, Duration::from_secs(cfg.shutdown_timeout_secs));

    power_off();
}

/// The core PID 1 loop: block in `waitpid` reaping whatever exits (tracked
/// services and reparented orphans alike), and bail out to shut down when
/// either a critical service dies or a shutdown signal arrives. SIGUSR1/
/// SIGUSR2 are handled here too since they need to log and touch the
/// supervisor, neither of which is safe from inside a signal handler.
fn run_event_loop(sup: &mut Supervisor, shutdown_grace: Duration) {
    loop {
        if signals::SHUTDOWN_REQUESTED.swap(false, Ordering::SeqCst) {
            logging::info("shutdown signal received, stopping services");
            sup.shutdown_all(shutdown_grace);
            return;
        }

        if signals::RELOAD_REQUESTED.swap(false, Ordering::SeqCst) {
            logging::info("SIGUSR1: reloading config");
            let cfg = config::load_or_default(CONFIG_PATH);
            logging::set_level(cfg.loglevel);
            if let Some(h) = &cfg.hostname {
                if let Err(e) = nix::unistd::sethostname(h) {
                    logging::warn(&format!("failed to apply reloaded hostname: {e}"));
                }
            }
        }

        if signals::STATUS_DUMP_REQUESTED.swap(false, Ordering::SeqCst) {
            logging::info(&sup.status_summary());
        }

        match waitpid(Pid::from_raw(-1), None) {
            Ok(status) => {
                if matches!(sup.handle_exit(status), Outcome::Halt) {
                    logging::info("critical service exited, shutting down");
                    sup.shutdown_all(shutdown_grace);
                    return;
                }
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => {
                logging::warn("no children left to wait for");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => logging::error(&format!("waitpid failed: {e}")),
        }
    }
}

/// Syncs disks and asks the kernel to power off. On real hardware/VMs
/// running as actual PID 1 this doesn't return. Anywhere else (a dev
/// container, running as a normal process) it fails with EPERM - that's
/// expected there, and we just exit instead.
fn power_off() {
    logging::info("mitos-init halted, powering off");
    unsafe {
        libc::sync();
    }

    let Err(e) = nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_POWER_OFF);
    logging::error(&format!(
        "reboot(RB_POWER_OFF) failed: {e} - exiting instead"
    ));
    std::process::exit(0);
}
