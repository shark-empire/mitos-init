//! Process supervision (Phase 3): spawns configured services, reaps
//! whatever exits (tracked services and reparented orphans alike), and
//! restarts supervised services according to their restart policy.

use crate::config::{RestartPolicy, ServiceDef};
use crate::logging;
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::WaitStatus;
use nix::unistd::Pid;
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

/// If a service restarts more than this many times within `BACKOFF_WINDOW`,
/// supervision gives up on it rather than burning CPU in a restart storm.
const MAX_RESTARTS_IN_WINDOW: usize = 5;
const BACKOFF_WINDOW: Duration = Duration::from_secs(10);

struct Supervised {
    def: ServiceDef,
    restart_times: Vec<Instant>,
}

pub enum Outcome {
    /// Keep looping; nothing the caller needs to act on.
    Continue,
    /// A critical service exited — time to shut the system down.
    Halt,
}

pub struct Supervisor {
    services: HashMap<i32, Supervised>, // keyed by current pid
}

impl Supervisor {
    pub fn new() -> Self {
        Supervisor { services: HashMap::new() }
    }

    pub fn spawn_all(&mut self, defs: &[ServiceDef]) {
        for def in defs {
            self.spawn_with_history(def.clone(), Vec::new());
        }
    }

    fn spawn_with_history(&mut self, def: ServiceDef, restart_times: Vec<Instant>) {
        match Command::new(&def.path).args(&def.args).spawn() {
            Ok(child) => {
                let pid = child.id() as i32;
                logging::info(&format!("started '{}' ({}) as pid {pid}", def.name, def.path));
                self.services.insert(pid, Supervised { def, restart_times });
            }
            Err(e) => {
                logging::error(&format!("failed to start '{}' ({}): {e}", def.name, def.path));
                // A critical service that won't even start leaves the
                // machine unreachable — fall back to a rescue shell rather
                // than continue with no console at all.
                if def.critical {
                    if let Ok(child) = Command::new("/bin/sh").spawn() {
                        let pid = child.id() as i32;
                        logging::warn("fell back to /bin/sh");
                        self.services.insert(pid, Supervised { def: fallback_shell(), restart_times: Vec::new() });
                    }
                }
            }
        }
    }

    /// Feeds one `waitpid` result into the supervisor. Returns `Halt` if a
    /// critical service just exited and the caller should begin shutdown.
    pub fn handle_exit(&mut self, status: WaitStatus) -> Outcome {
        let (pid, summary) = match status {
            WaitStatus::Exited(pid, code) => (pid.as_raw(), format!("exited with status {code}")),
            WaitStatus::Signaled(pid, sig, _) => (pid.as_raw(), format!("killed by signal {sig:?}")),
            _ => return Outcome::Continue, // Stopped/Continued/etc: not a real exit
        };

        let Some(sup) = self.services.remove(&pid) else {
            // Reaped an orphan we weren't tracking — nothing more to do.
            return Outcome::Continue;
        };

        logging::warn(&format!("service '{}' {summary}", sup.def.name));

        if sup.def.critical {
            return Outcome::Halt;
        }

        let should_restart = match sup.def.restart {
            RestartPolicy::Never => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => !matches!(status, WaitStatus::Exited(_, 0)),
        };

        if should_restart {
            let mut restart_times = sup.restart_times;
            let now = Instant::now();
            restart_times.retain(|t| now.duration_since(*t) < BACKOFF_WINDOW);
            restart_times.push(now);

            if restart_times.len() > MAX_RESTARTS_IN_WINDOW {
                logging::error(&format!(
                    "service '{}' restarted {} times within {BACKOFF_WINDOW:?}, giving up",
                    sup.def.name,
                    restart_times.len()
                ));
            } else {
                self.spawn_with_history(sup.def.clone(), restart_times);
            }
        }

        Outcome::Continue
    }

    /// Sends SIGTERM to every remaining supervised child, gives them
    /// `grace` to exit on their own, then SIGKILLs whatever's left.
    pub fn shutdown_all(&mut self, grace: Duration) {
        for pid in self.services.keys() {
            let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        }

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && !self.services.is_empty() {
            match nix::sys::wait::waitpid(Pid::from_raw(-1), Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                    self.services.remove(&pid.as_raw());
                }
                Ok(WaitStatus::StillAlive) => std::thread::sleep(Duration::from_millis(50)),
                Ok(_) => {}
                Err(nix::errno::Errno::ECHILD) => break, // nothing left to wait for
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }

        for pid in self.services.keys() {
            logging::warn(&format!("pid {pid} didn't exit in time, sending SIGKILL"));
            let _ = kill(Pid::from_raw(*pid), Signal::SIGKILL);
        }
    }

    pub fn status_summary(&self) -> String {
        if self.services.is_empty() {
            return "no supervised services running".to_string();
        }
        let mut lines = vec![format!("{} supervised service(s):", self.services.len())];
        for (pid, sup) in &self.services {
            let crit = if sup.def.critical { " [critical]" } else { "" };
            lines.push(format!("  pid {pid}: {} ({}){crit}", sup.def.name, sup.def.path));
        }
        lines.join("\n")
    }
}

fn fallback_shell() -> ServiceDef {
    ServiceDef {
        name: "shell".into(),
        path: "/bin/sh".into(),
        args: vec![],
        critical: true,
        restart: RestartPolicy::Never,
    }
}
