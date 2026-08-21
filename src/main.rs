use std::process::{Command, exit};
use std::thread;
use std::time::Duration;

fn main() {
    // 1. Check if we are actually PID 1
    if std::process::id() != 1 {
        eprintln!("Warning: mitos-init is designed to run as PID 1.");
    }

    println!("Welcome to MITOS!");
    println!("Initializing system...");

    // TODO: Mount virtual filesystems (/dev, /proc, /sys)
    // mount_filesystems();

    // 2. Spawn the shell (or next component in the chain)
    println!("Starting mitos-shell...");
    let mut child = Command::new("/bin/mitos-shell")
        .spawn()
        .unwrap_or_else(|err| {
            eprintln!("Failed to start shell: {}", err);
            // Fallback to standard sh if mitos-shell isn't ready yet
            Command::new("/bin/sh").spawn().expect("Failed to start fallback shell")
        });

    // 3. The PID 1 Loop (Reaping children)
    // As PID 1, we must never exit. We need to loop and wait for orphaned processes.
    loop {
        // In a complete implementation, we would use waitpid(-1, ...) here.
        // For now, we will just wait on our primary shell process.
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("Shell exited with status: {}. Halting system.", status);
                break;
            }
            Ok(None) => {
                // Process is still running, sleep to prevent CPU hogging
                thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                eprintln!("Error waiting on shell: {}", e);
                break;
            }
        }
    }

    println!("System halted.");
    exit(1);
}
