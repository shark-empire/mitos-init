//! Minimal `reboot`: signals PID 1 to reboot, the same convention the
//! kernel itself uses for Ctrl-Alt-Del (see `signals.rs`). Needs root or
//! CAP_KILL - the kernel restricts who may signal PID 1, same as real
//! `reboot`/`shutdown`/`poweroff` normally being setuid or run as root.

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

fn main() {
    if let Err(e) = kill(Pid::from_raw(1), Signal::SIGINT) {
        eprintln!("reboot: couldn't signal PID 1: {e}");
        std::process::exit(1);
    }
}
