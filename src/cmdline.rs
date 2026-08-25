//! Minimal kernel command line parser.
//!
//! `/proc/cmdline` is space-separated `key`/`key=value` tokens, with
//! `key="value with spaces"` allowed. That's all we need to handle - this
//! isn't a general shell-quoting parser.

use std::collections::HashMap;
use std::fs;

pub fn parse() -> HashMap<String, Option<String>> {
    let raw = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    parse_str(raw.trim())
}

fn push_token(tok: &str, map: &mut HashMap<String, Option<String>>) {
    if tok.is_empty() {
        return;
    }
    match tok.split_once('=') {
        Some((k, v)) => {
            map.insert(k.to_string(), Some(v.trim_matches('"').to_string()));
        }
        None => {
            map.insert(tok.to_string(), None);
        }
    }
}

fn parse_str(s: &str) -> HashMap<String, Option<String>> {
    let mut map = HashMap::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ' ' if !in_quotes => {
                push_token(&current, &mut map);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    push_token(&current, &mut map);
    map
}
