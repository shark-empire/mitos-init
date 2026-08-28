//! Tiny, dependency-free config file parser.
//!
//! We deliberately don't pull in `serde`/`toml` here — for a binary that
//! only ever parses one small file at boot, a hand-rolled parser is
//! smaller, has no build-time cost, and is one less thing that can break
//! before a real root filesystem is even mounted.

use crate::logging::Level;
use std::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, Clone)]
pub struct ServiceDef {
    pub name: String,
    pub path: String,
    pub args: Vec<String>,
    pub critical: bool,
    pub restart: RestartPolicy,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub hostname: Option<String>,
    pub loglevel: Level,
    pub shutdown_timeout_secs: u64,
    pub services: Vec<ServiceDef>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hostname: Some("mitos".to_string()),
            loglevel: Level::Info,
            shutdown_timeout_secs: 5,
            services: vec![ServiceDef {
                name: "shell".to_string(),
                path: "/bin/mitos-shell".to_string(),
                args: vec![],
                critical: true,
                restart: RestartPolicy::Never,
            }],
        }
    }
}

/// Loads `/etc/mitos/init.conf` if present, otherwise falls back to a
/// single-service default (mitos-shell, falling back to /bin/sh at spawn
/// time) so the system is always bootable even with no config on disk.
pub fn load_or_default(path: &str) -> Config {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(_) => Config::default(),
    }
}

fn parse(text: &str) -> Config {
    let mut cfg = Config {
        services: Vec::new(),
        ..Config::default()
    };

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("service ") {
            match parse_service(rest) {
                Ok(svc) => cfg.services.push(svc),
                Err(e) => eprintln!("mitos-init [WARN]: skipping bad service line ({e}): {line}"),
            }
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "hostname" => cfg.hostname = Some(value.to_string()),
                "loglevel" => {
                    if let Some(lvl) = Level::parse(value) {
                        cfg.loglevel = lvl;
                    }
                }
                "shutdown_timeout" => {
                    if let Ok(secs) = value.parse() {
                        cfg.shutdown_timeout_secs = secs;
                    }
                }
                _ => eprintln!("mitos-init [WARN]: unknown config key '{key}'"),
            }
        }
    }

    if cfg.services.is_empty() {
        cfg.services = Config::default().services;
    }
    cfg
}

/// Merges services from `init.conf`'s inline `service` lines with those
/// loaded from `/etc/mitos/services.d/*.service` unit files (see
/// `units.rs`). A name collision keeps whichever was seen first and warns
/// about the rest, rather than silently letting one replace the other.
pub fn merge_services(mut base: Vec<ServiceDef>, extra: Vec<ServiceDef>) -> Vec<ServiceDef> {
    for svc in extra {
        if base.iter().any(|s| s.name == svc.name) {
            eprintln!(
                "mitos-init [WARN]: duplicate service name '{}', keeping the first one seen",
                svc.name
            );
        } else {
            base.push(svc);
        }
    }
    base
}

fn parse_service(rest: &str) -> std::result::Result<ServiceDef, String> {
    let mut parts = rest.split_whitespace();
    let name = parts.next().ok_or("missing service name")?.to_string();

    let mut path = None;
    let mut args = Vec::new();
    let mut critical = false;
    let mut restart = RestartPolicy::OnFailure;

    for field in parts {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("bad field '{field}'"))?;
        let value = value.trim_matches('"');
        match key {
            "path" => path = Some(value.to_string()),
            "args" => {
                args = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
            "critical" => critical = value.eq_ignore_ascii_case("true"),
            "restart" => {
                restart = match value {
                    "always" => RestartPolicy::Always,
                    "never" => RestartPolicy::Never,
                    _ => RestartPolicy::OnFailure,
                };
            }
            _ => {}
        }
    }

    let path = path.ok_or("missing path=")?;
    Ok(ServiceDef {
        name,
        path,
        args,
        critical,
        restart,
    })
}
