//! Minimal `poweroff`: signals PID 1 to shut down and cut power. Needs
//! root or CAP_KILL - see `reboot.rs`.

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

fn main() {
    if let Err(e) = kill(Pid::from_raw(1), Signal::SIGTERM) {
        eprintln!("poweroff: couldn't signal PID 1: {e}");
        std::process::exit(1);
    }
}
