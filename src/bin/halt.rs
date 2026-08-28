//! Minimal `halt`: signals PID 1 to stop without cutting power. Needs
//! root or CAP_KILL - see `reboot.rs`.

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

fn main() {
    if let Err(e) = kill(Pid::from_raw(1), Signal::SIGQUIT) {
        eprintln!("halt: couldn't signal PID 1: {e}");
        std::process::exit(1);
    }
}
