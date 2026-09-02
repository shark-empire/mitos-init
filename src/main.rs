//! mitos-init — the PID 1 process for MITOS.
//!
//! Deliberately minimal. Everything a real Linux boot needs before any
//! configurable logic can run - mounting the kernel-facing filesystem
//! tree, the initramfs -> real-root handoff, device permissions - stays
//! here. Everything else (parsing config, spawning and supervising
//! services, the readiness protocol, IPC) has moved to mitos-services, a
//! separate binary this process spawns and supervises as a single child.
//! See INTEGRATION.md for why: the short version is that
//! `panic = "abort"` PID 1 code has no room for the kind of complexity a
//! full service manager needs (dependency graphs, cycle detection, ...)
//! without meaningfully raising the odds of a kernel panic. A bug in
//! mitos-services can only crash mitos-services, which this process then
//! restarts.

mod cmdline;
mod error;
mod hotplug;
mod logging;
mod mount;
mod signals;
mod switch_root;
mod utmp;

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SERVICES_BIN: &str = "/sbin/mitos-services";
const SHUTDOWN_ACK_FIFO: &str = "/run/mitos-init/shutdown-ack";
const MAX_RESTARTS_IN_WINDOW: usize = 5;
const BACKOFF_WINDOW: Duration = Duration::from_secs(10);
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(20);

fn main() {
    logging::init();
        // Example: Check for a MITOS_LOG_LEVEL environment variable
    if let Ok(level_str) = std::env::var("MITOS_LOG_LEVEL") {
        if let Some(level) = logging::Level::parse(&level_str) {
            logging::set_level(level);
        }
    }
    install_panic_hook();
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
    mount::populate_run();

    if !mount::setup_service_cgroup_root() {
        logging::warn(
            "cgroup delegation unavailable - mitos-services will fall back to plain pid-based supervision",
        );
    }

    utmp::log_boot();

    if cmdline::rescue_requested(&cmdline::parse()) {
        logging::warn(
            "rescue mode requested on the kernel command line (single/mitos.rescue) - \
             starting a rescue shell directly, without mitos-services",
        );
        run_rescue_only();
        return; // unreachable in practice - run_rescue_only either execs or exits
    }

    if let Err(e) = signals::install_handlers() {
        logging::error(&format!("failed to install signal handlers: {e}"));
    }
    if let Err(e) = signals::block_handled() {
        logging::warn(&format!(
            "failed to block signals ahead of worker threads: {e}"
        ));
    }

    hotplug::spawn_listener();
    let have_fifo = create_ack_fifo();

    if let Err(e) = signals::unblock_handled() {
        logging::error(&format!("failed to unblock signals: {e}"));
    }

    let kind = run_event_loop(have_fifo);
    finish(kind);
}

/// Bypasses mitos-services entirely: execs a plain shell directly in
/// place of this process, rather than spawning and supervising it.
/// There's nothing left for mitos-init to do once this runs - if the
/// rescue shell exits, that's the end of the session, the same as it
/// would be for any process that's replaced PID 1.
fn run_rescue_only() {
    let err = std::process::Command::new("/bin/sh").exec();
    // exec() only returns on failure.
    logging::error(&format!("couldn't exec /bin/sh for rescue mode: {err}"));
    std::process::exit(1);
}

fn create_ack_fifo() -> bool {
    match nix::unistd::mkfifo(
        SHUTDOWN_ACK_FIFO,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    ) {
        Ok(()) => true,
        Err(nix::errno::Errno::EEXIST) => true,
        Err(e) => {
            logging::warn(&format!(
                "couldn't create {SHUTDOWN_ACK_FIFO}: {e} - shutdown will proceed on a fixed \
                 timeout instead of waiting for mitos-services to confirm"
            ));
            false
        }
    }
}

fn spawn_services() -> Option<i32> {
    match std::process::Command::new(SERVICES_BIN).spawn() {
        Ok(child) => {
            let pid = child.id() as i32;
            logging::info(&format!("started mitos-services as pid {pid}"));
            Some(pid)
        }
        Err(e) => {
            logging::error(&format!("failed to start {SERVICES_BIN}: {e}"));
            None
        }
    }
}

enum ShutdownKind {
    Reboot,
    PowerOff,
    Halt,
}

/// The core PID 1 loop: supervises exactly one child, mitos-services,
/// restarting it (with the same crash-loop backoff shape services
/// themselves used to get from `supervisor.rs`, before that moved) if it
/// exits unexpectedly, relaying shutdown/reload/status signals to it,
/// and waiting - bounded - for its shutdown-ack before this process
/// performs the actual `reboot(2)` syscall.
fn run_event_loop(have_fifo: bool) -> ShutdownKind {
    let mut services_pid = spawn_services();
    let mut restart_times: Vec<Instant> = Vec::new();
    if services_pid.is_none() {
        logging::error(
            "mitos-services couldn't be started at all - falling back to a rescue shell",
        );
        run_rescue_only();
    }

    loop {
        if let Some(kind) = check_shutdown(&mut services_pid, have_fifo) {
            return kind;
        }

        if signals::RELOAD_REQUESTED.swap(false, Ordering::SeqCst) {
            relay(services_pid, Signal::SIGUSR1, "reload");
        }
        if signals::STATUS_DUMP_REQUESTED.swap(false, Ordering::SeqCst) {
            relay(services_pid, Signal::SIGUSR2, "status dump");
        }

        match waitpid(Pid::from_raw(-1), None) {
            Ok(status) => {
                let exited_pid = match status {
                    WaitStatus::Exited(pid, _) | WaitStatus::Signaled(pid, _, _) => {
                        Some(pid.as_raw())
                    }
                    _ => None,
                };
                if exited_pid.is_some() && exited_pid == services_pid {
                    logging::error("mitos-services exited unexpectedly");
                    services_pid = restart_with_backoff(&mut restart_times);
                }
                // Anything else reaped here is an orphan mitos-init itself
                // ended up with - not one of a service's descendants,
                // since mitos-services' own PR_SET_CHILD_SUBREAPER catches
                // those first. Reaping it via this same waitpid(-1) call
                // is already all that's needed.
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

fn relay(services_pid: Option<i32>, signal: Signal, what: &str) {
    match services_pid {
        Some(pid) => {
            let _ = kill(Pid::from_raw(pid), signal);
        }
        None => logging::warn(&format!(
            "mitos-services isn't running, nothing to relay this {what} to"
        )),
    }
}

fn restart_with_backoff(restart_times: &mut Vec<Instant>) -> Option<i32> {
    let now = Instant::now();
    restart_times.retain(|t| now.duration_since(*t) < BACKOFF_WINDOW);
    restart_times.push(now);

    if restart_times.len() > MAX_RESTARTS_IN_WINDOW {
        logging::error(&format!(
            "mitos-services restarted {} times within {BACKOFF_WINDOW:?}, giving up - \
             falling back to a rescue shell",
            restart_times.len()
        ));
        run_rescue_only();
        return None; // unreachable - run_rescue_only either execs or exits
    }

    spawn_services()
}

fn check_shutdown(services_pid: &mut Option<i32>, have_fifo: bool) -> Option<ShutdownKind> {
    // Evaluated unconditionally so every flag actually gets cleared,
    // regardless of which one(s) are set - see the same note in
    // mitos-services' own event loop.
    let reboot = signals::REBOOT_REQUESTED.swap(false, Ordering::SeqCst);
    let poweroff = signals::POWEROFF_REQUESTED.swap(false, Ordering::SeqCst);
    let halt = signals::HALT_REQUESTED.swap(false, Ordering::SeqCst);

    let (kind, signal) = if reboot {
        (ShutdownKind::Reboot, Signal::SIGINT)
    } else if poweroff {
        (ShutdownKind::PowerOff, Signal::SIGTERM)
    } else if halt {
        (ShutdownKind::Halt, Signal::SIGQUIT)
    } else {
        return None;
    };

    logging::info(
        "shutdown requested - relaying to mitos-services and waiting for it to stop services",
    );
    relay(*services_pid, signal, "shutdown");

    if have_fifo {
        wait_for_ack(SHUTDOWN_ACK_TIMEOUT);
    } else {
        std::thread::sleep(Duration::from_secs(3));
    }
    *services_pid = None;
    Some(kind)
}

/// Blocks (bounded) until mitos-services writes to the shutdown-ack FIFO,
/// or `timeout` passes - whichever comes first. Opening a FIFO for
/// reading blocks by nature, so the actual open+read happens on a
/// throwaway thread and this just polls a flag - the same pattern
/// `rollback.rs` used for its bounded reload watch before that moved to
/// mitos-services.
fn wait_for_ack(timeout: Duration) {
    let acked = Arc::new(AtomicBool::new(false));
    let flag = acked.clone();
    std::thread::spawn(move || {
        if let Ok(mut f) = std::fs::File::open(SHUTDOWN_ACK_FIFO) {
            let mut buf = [0u8; 1];
            let _ = f.read(&mut buf);
        }
        flag.store(true, Ordering::SeqCst);
    });

    let deadline = Instant::now() + timeout;
    while !acked.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !acked.load(Ordering::SeqCst) {
        logging::warn(
            "timed out waiting for mitos-services to confirm shutdown, proceeding anyway",
        );
    }
}

/// Routes panic messages through our own logger (which reaches
/// `/dev/kmsg` when available) instead of the default hook's plain
/// stderr write. The release profile sets `panic = "abort"`, so there's
/// no unwinding to interfere with - this just makes sure that if an
/// unexpected panic ever *does* slip past the `Result`-based error
/// handling everywhere else in this codebase, the reason ends up
/// somewhere a `dmesg` after the fact can actually find it.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        logging::error(&format!("PANIC: {info}"));
    }));
}

/// Syncs disks and asks the kernel to reboot/power off/halt, matching
/// whichever `ShutdownKind` the event loop exited with. On real
/// hardware/VMs running as actual PID 1 this doesn't return. Anywhere
/// else (a dev container, running as a normal process) it fails with
/// EPERM - that's expected there, and we just exit instead.
fn finish(kind: ShutdownKind) {
    logging::info("mitos-init halted");
    unsafe {
        libc::sync();
    }

    let mode = match kind {
        ShutdownKind::Reboot => nix::sys::reboot::RebootMode::RB_AUTOBOOT,
        ShutdownKind::PowerOff => nix::sys::reboot::RebootMode::RB_POWER_OFF,
        ShutdownKind::Halt => nix::sys::reboot::RebootMode::RB_HALT_SYSTEM,
    };
    let Err(e) = nix::sys::reboot::reboot(mode);
    logging::error(&format!("reboot({mode:?}) failed: {e} - exiting instead"));
    std::process::exit(0);
}
