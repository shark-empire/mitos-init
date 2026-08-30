//! Process supervision (Phase 3): spawns configured services, reaps
//! whatever exits (tracked services and reparented orphans alike), and
//! restarts supervised services according to their restart policy.
//!
//! Three things beyond plain pid tracking, all wired in here:
//! - Every service gets a cgroup (`cgroups.rs`) so teardown can reach
//!   grandchildren the tracked pid alone never could.
//! - Every service gets its own readiness socket (`notify.rs`) so
//!   `status_summary` can report actual readiness, not just "spawned".
//! - `reload_services` reconciles a running set against a new config
//!   (start/stop/restart only what changed) - the mechanism
//!   `rollback.rs`'s transactional reload is built on.

use crate::config::{RestartPolicy, ServiceDef};
use crate::logging;
use crate::notify::ReadyState;
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::WaitStatus;
use nix::unistd::Pid;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
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
    /// A critical service exited (carries its name) — time to shut the
    /// system down, unless an active reload watch intercepts this first.
    Halt(String),
    /// A service exhausted its restart budget and won't be retried again
    /// (carries its name) - lets a reload watch tell whether this was one
    /// of the services *it* just touched.
    GaveUp(String),
}

pub struct Supervisor {
    services: HashMap<i32, Supervised>, // keyed by current pid
    ready_state: Arc<ReadyState>,
}

impl Supervisor {
    pub fn new() -> Self {
        Supervisor {
            services: HashMap::new(),
            ready_state: Arc::new(ReadyState::default()),
        }
    }

    pub fn spawn_all(&mut self, defs: &[ServiceDef]) {
        for def in defs {
            self.spawn_with_history(def.clone(), Vec::new());
        }
    }

    fn spawn_with_history(&mut self, def: ServiceDef, restart_times: Vec<Instant>) {
        let mut cmd = Command::new(&def.path);
        cmd.args(&def.args);
        if let Some(sock) = crate::notify::listen_for(&def.name, self.ready_state.clone()) {
            cmd.env("NOTIFY_SOCKET", sock);
        }

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id() as i32;
                logging::info(&format!(
                    "started '{}' ({}) as pid {pid}",
                    def.name, def.path
                ));
                crate::cgroups::create_for(&def.name, def.memory_limit);
                crate::cgroups::attach(&def.name, pid);
                self.services.insert(pid, Supervised { def, restart_times });
            }
            Err(e) => {
                logging::error(&format!(
                    "failed to start '{}' ({}): {e}",
                    def.name, def.path
                ));
                // A critical service that won't even start leaves the
                // machine unreachable — fall back to a rescue shell rather
                // than continue with no console at all.
                if def.critical {
                    if let Ok(child) = Command::new("/bin/sh").spawn() {
                        let pid = child.id() as i32;
                        logging::warn("fell back to /bin/sh");
                        let def = fallback_shell();
                        crate::cgroups::create_for(&def.name, None);
                        crate::cgroups::attach(&def.name, pid);
                        self.services.insert(
                            pid,
                            Supervised {
                                def,
                                restart_times: Vec::new(),
                            },
                        );
                    }
                }
            }
        }
    }

    /// Feeds one `waitpid` result into the supervisor.
    pub fn handle_exit(&mut self, status: WaitStatus) -> Outcome {
        let (pid, summary) = match status {
            WaitStatus::Exited(pid, code) => (pid.as_raw(), format!("exited with status {code}")),
            WaitStatus::Signaled(pid, sig, _) => {
                (pid.as_raw(), format!("killed by signal {sig:?}"))
            }
            _ => return Outcome::Continue, // Stopped/Continued/etc: not a real exit
        };

        let Some(sup) = self.services.remove(&pid) else {
            // Reaped an orphan we weren't tracking — nothing more to do.
            return Outcome::Continue;
        };

        logging::warn(&format!("service '{}' {summary}", sup.def.name));
        crate::cgroups::kill_and_remove(&sup.def.name); // sweep any grandchildren this exit left behind

        if sup.def.critical {
            return Outcome::Halt(sup.def.name.clone());
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
                return Outcome::GaveUp(sup.def.name.clone());
            }
            self.spawn_with_history(sup.def.clone(), restart_times);
        }

        Outcome::Continue
    }

    /// Reconciles the running service set against `new_defs`: stops
    /// services no longer present, restarts ones whose definition
    /// changed, leaves unchanged ones running untouched, and starts
    /// brand-new ones. Returns the names of every service this call
    /// actually started or restarted - `rollback.rs` uses this to scope
    /// its failure watch to only the services a given reload is
    /// responsible for.
    pub fn reload_services(&mut self, new_defs: &[ServiceDef]) -> Vec<String> {
        let mut touched = Vec::new();

        let removed_pids: Vec<i32> = self
            .services
            .iter()
            .filter(|(_, sup)| !new_defs.iter().any(|d| d.name == sup.def.name))
            .map(|(&pid, _)| pid)
            .collect();
        for pid in removed_pids {
            if let Some(sup) = self.services.remove(&pid) {
                logging::info(&format!(
                    "reload: stopping removed service '{}'",
                    sup.def.name
                ));
                stop_one(pid, &sup.def.name);
            }
        }

        for def in new_defs {
            // Collected as owned data so this borrow of self.services
            // doesn't overlap the mutation below.
            let found: Option<(i32, bool)> = self
                .services
                .iter()
                .find(|(_, sup)| sup.def.name == def.name)
                .map(|(&pid, sup)| (pid, defs_equal(&sup.def, def)));

            match found {
                Some((_, true)) => continue, // unchanged, leave it running
                Some((pid, false)) => {
                    logging::info(&format!(
                        "reload: restarting changed service '{}'",
                        def.name
                    ));
                    self.services.remove(&pid);
                    stop_one(pid, &def.name);
                    self.spawn_with_history(def.clone(), Vec::new());
                    touched.push(def.name.clone());
                }
                None => {
                    logging::info(&format!("reload: starting new service '{}'", def.name));
                    self.spawn_with_history(def.clone(), Vec::new());
                    touched.push(def.name.clone());
                }
            }
        }

        touched
    }

    /// Sends SIGTERM to every remaining supervised child, gives them
    /// `grace` to exit on their own, then SIGKILLs whatever's left, then
    /// unconditionally sweeps every service's cgroup - regardless of
    /// whether its tracked pid exited gracefully or had to be SIGKILLed -
    /// since that's the only way to also catch grandchildren a service
    /// forked off that plain pid-based signaling never reaches.
    pub fn shutdown_all(&mut self, grace: Duration) {
        let all_names: Vec<String> = self.services.values().map(|s| s.def.name.clone()).collect();

        for pid in self.services.keys() {
            let _ = kill(Pid::from_raw(*pid), Signal::SIGTERM);
        }

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && !self.services.is_empty() {
            match nix::sys::wait::waitpid(
                Pid::from_raw(-1),
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
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

        for name in &all_names {
            crate::cgroups::kill_and_remove(name);
        }
    }

    pub fn status_summary(&self) -> String {
        if self.services.is_empty() {
            return "no supervised services running".to_string();
        }
        let mut lines = vec![format!("{} supervised service(s):", self.services.len())];
        for (pid, sup) in &self.services {
            let crit = if sup.def.critical { " [critical]" } else { "" };
            let ready = if self.ready_state.is_ready(&sup.def.name) {
                " ready"
            } else {
                " starting"
            };
            lines.push(format!(
                "  pid {pid}: {} ({}){crit}{ready}",
                sup.def.name, sup.def.path
            ));
        }
        lines.join("\n")
    }
}

/// Sends SIGTERM to a single pid, gives it a short grace period, SIGKILLs
/// it if it's still around, then sweeps its cgroup. Used by
/// `reload_services` for services being stopped or replaced outside the
/// full-shutdown path.
fn stop_one(pid: i32, name: &str) {
    let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() >= deadline {
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            break;
        }
        match nix::sys::wait::waitpid(
            Pid::from_raw(pid),
            Some(nix::sys::wait::WaitPidFlag::WNOHANG),
        ) {
            Ok(WaitStatus::Exited(_, _)) | Ok(WaitStatus::Signaled(_, _, _)) => break,
            Ok(_) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break, // already reaped elsewhere, or no such process
        }
    }
    crate::cgroups::kill_and_remove(name);
}

fn defs_equal(a: &ServiceDef, b: &ServiceDef) -> bool {
    a.path == b.path
        && a.args == b.args
        && a.critical == b.critical
        && a.restart == b.restart
        && a.memory_limit == b.memory_limit
}

fn fallback_shell() -> ServiceDef {
    ServiceDef {
        name: "shell".into(),
        path: "/bin/sh".into(),
        args: vec![],
        critical: true,
        restart: RestartPolicy::Never,
        memory_limit: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(path: &str, mem: Option<u64>) -> ServiceDef {
        ServiceDef {
            name: "x".into(),
            path: path.into(),
            args: vec![],
            critical: false,
            restart: RestartPolicy::Never,
            memory_limit: mem,
        }
    }

    #[test]
    fn identical_defs_are_equal() {
        assert!(defs_equal(&svc("/bin/a", None), &svc("/bin/a", None)));
    }

    #[test]
    fn a_changed_path_is_not_equal() {
        assert!(!defs_equal(&svc("/bin/a", None), &svc("/bin/b", None)));
    }

    #[test]
    fn a_changed_memory_limit_is_not_equal() {
        assert!(!defs_equal(
            &svc("/bin/a", None),
            &svc("/bin/a", Some(1024))
        ));
    }
}
