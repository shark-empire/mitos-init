use nix::mount::{mount, MsFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::Pid;
use std::fs;
use std::process::Command;

/// Mounts the essential virtual filesystems required by Linux userspace.
fn mount_vfs() {
    let mounts = [
        ("devtmpfs", "/dev", "devtmpfs"),
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
    ];

    for (src, target, fstype) in mounts.iter() {
        // Ensure the mount point exists
        let _ = fs::create_dir_all(target);

        if let Err(e) = mount(
            Some(*src),
            *target,
            Some(*fstype),
            MsFlags::empty(),
            None::<&str>,
        ) {
            eprintln!("mitos-init [WARN]: Failed to mount {} - {}", target, e);
        } else {
            println!("mitos-init [OK]: Mounted {}", target);
        }
    }
}

fn main() {
    if std::process::id() != 1 {
        eprintln!("mitos-init [WARN]: Running outside of PID 1. System calls may fail.");
    }

    println!("Starting MITOS Initialization...");
    
    // 1. Prepare the kernel environment
    mount_vfs();

    // 2. Spawn the primary user environment (Shell or GUI)
    println!("mitos-init [INFO]: Launching mitos-shell...");
    let mut shell = Command::new("/bin/mitos-shell")
        .spawn()
        .unwrap_or_else(|_| {
            eprintln!("mitos-init [FAIL]: /bin/mitos-shell not found. Falling back to /bin/sh.");
            Command::new("/bin/sh").spawn().expect("Failed to execute fallback shell")
        });

    let shell_pid = Pid::from_raw(shell.id() as i32);

    // 3. The Infinite Reaper Loop
    // PID 1 must wait on ALL orphaned child processes to prevent zombie exhaustion.
    loop {
        // waitpid with -1 waits for ANY child process. 
        match waitpid(Pid::from_raw(-1), None) {
            Ok(WaitStatus::Exited(pid, status)) => {
                if pid == shell_pid {
                    println!("mitos-init [INFO]: Primary shell exited (Status: {}). Halting.", status);
                    break; 
                }
            },
            Ok(WaitStatus::Signaled(pid, signal, _)) => {
                if pid == shell_pid {
                    println!("mitos-init [FATAL]: Primary shell killed by signal {:?}. Halting.", signal);
                    break;
                }
            },
            Ok(_) => {
                // Another orphaned process was cleanly reaped. Continue looping.
            },
            Err(e) => {
                eprintln!("mitos-init [ERROR]: waitpid failed: {}", e);
            }
        }
    }

    // In a real shutdown sequence, you would unmount filesystems and call sync() here.
    println!("MITOS halted.");
    
    // Halt the kernel gracefully using the reboot system call (LINUX_REBOOT_CMD_POWER_OFF)
    // For now, exiting PID 1 will cause a kernel panic (which acts as a hard halt).
    std::process::exit(0);
}

