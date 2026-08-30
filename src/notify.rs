//! sd_notify-compatible service readiness protocol.
//!
//! Real systemd uses one shared datagram socket plus `SCM_CREDENTIALS`
//! ancillary messages to authenticate which process a given `READY=1`
//! came from - correct, but `recvmsg`/cmsg parsing needs exact
//! alignment/padding that's genuinely easy to get subtly wrong without a
//! compiler to check it against (see how much of this project's riskier
//! FFI work has already needed a CI round-trip to fix).
//!
//! Since every service here already has a unique identity the supervisor
//! assigns it, we sidestep sender authentication entirely: each service
//! gets its *own* notify socket
//! (`/run/mitos-init/notify/<name>.sock`), so the socket a datagram
//! arrived on already says unambiguously which service it's from. The
//! wire format is unchanged - `NOTIFY_SOCKET` env var, `READY=1` payload -
//! so real systemd-aware daemons calling `sd_notify()` work against this
//! without modification; only our half of the implementation is simpler.

use crate::logging;
use std::collections::HashSet;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

const NOTIFY_DIR: &str = "/run/mitos-init/notify";

/// Ready-state shared between each per-service listener thread and
/// whatever wants to query it (currently `Supervisor::status_summary`).
#[derive(Default)]
pub struct ReadyState {
    ready: Mutex<HashSet<String>>,
}

impl ReadyState {
    pub fn is_ready(&self, name: &str) -> bool {
        self.ready.lock().map(|s| s.contains(name)).unwrap_or(false)
    }

    fn mark_ready(&self, name: &str) {
        if let Ok(mut s) = self.ready.lock() {
            s.insert(name.to_string());
        }
    }
}

/// Creates `name`'s notify socket and spawns a thread listening on it,
/// returning the path to set as `NOTIFY_SOCKET` in that service's
/// environment. Best-effort: if the socket can't be created, the service
/// just runs without readiness tracking - equivalent to how it behaves
/// against any init that doesn't support sd_notify at all, since a
/// well-behaved `sd_notify()` caller treats a missing/unset
/// `NOTIFY_SOCKET` as "readiness notification isn't supported here" and
/// carries on regardless.
pub fn listen_for(name: &str, state: Arc<ReadyState>) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(NOTIFY_DIR) {
        logging::debug(&format!("couldn't create {NOTIFY_DIR}: {e}"));
        return None;
    }
    let _ = std::fs::set_permissions(NOTIFY_DIR, std::fs::Permissions::from_mode(0o700));

    let path = PathBuf::from(NOTIFY_DIR).join(format!("{name}.sock"));
    let _ = std::fs::remove_file(&path); // stale socket from a previous instance of this service

    let socket = match UnixDatagram::bind(&path) {
        Ok(s) => s,
        Err(e) => {
            logging::debug(&format!("couldn't bind notify socket for '{name}': {e}"));
            return None;
        }
    };
    // Since every service is given its own socket rather than one shared,
    // credential-authenticated socket (see the module doc comment),
    // filesystem permissions are what actually stop a *different* local
    // process from writing a spoofed READY=1 here. Every currently
    // spawned service runs as the same uid as mitos-init (root - there's
    // no privilege-dropping yet), so root-only is correct for today's
    // threat model; if that changes, this would need to chown to the
    // specific service's uid instead of just restricting the mode bits.
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    let thread_name = name.to_string();
    let ret_path = path.clone();
    thread::spawn(move || run(socket, &thread_name, state));
    Some(ret_path)
}

fn run(socket: UnixDatagram, name: &str, state: Arc<ReadyState>) {
    let mut buf = [0u8; 4096];
    loop {
        let n = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return, // socket gone (service's cgroup/dir cleaned up) - stop listening
        };
        let Ok(text) = std::str::from_utf8(&buf[..n]) else {
            continue;
        };
        // Real sd_notify payloads can carry several `KEY=VALUE` lines
        // (STATUS=, MAINPID=, WATCHDOG=1, ...) - we only act on READY=1
        // for now, matching the scope of what `status_summary` surfaces.
        for line in text.lines() {
            if line == "READY=1" {
                state.mark_ready(name);
                logging::debug(&format!("'{name}' reported READY=1"));
            }
        }
    }
}
