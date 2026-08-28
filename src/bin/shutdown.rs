//! Minimal `shutdown`: `shutdown [-r|-h|-P|-H] (now|+MINUTES)`.
//!
//! `-r` reboot, `-h`/`-P` poweroff (the traditional default), `-H` halt
//! without powering off. Doesn't support wall broadcast messages or
//! cancellation (`shutdown -c`) - this is deliberately a small subset, not
//! a drop-in replacement for util-linux's `shutdown`. Needs root or
//! CAP_KILL - see `reboot.rs`.

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut signal = Signal::SIGTERM; // default: poweroff, matching traditional `shutdown`
    let mut delay = Duration::ZERO;

    for arg in &args {
        match arg.as_str() {
            "-r" => signal = Signal::SIGINT,
            "-h" | "-P" => signal = Signal::SIGTERM,
            "-H" => signal = Signal::SIGQUIT,
            "now" => delay = Duration::ZERO,
            s if s.starts_with('+') => {
                if let Ok(minutes) = s[1..].parse::<u64>() {
                    delay = Duration::from_secs(minutes * 60);
                }
            }
            _ => {}
        }
    }

    if !delay.is_zero() {
        println!("Shutdown scheduled in {} minute(s)", delay.as_secs() / 60);
        std::thread::sleep(delay);
    }

    if let Err(e) = kill(Pid::from_raw(1), signal) {
        eprintln!("shutdown: couldn't signal PID 1: {e}");
        std::process::exit(1);
    }
}
