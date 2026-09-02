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

/// True if the kernel command line asks for a rescue/single-user boot -
/// recognizes both the traditional sysvinit `single` and an explicit
/// `mitos.rescue`, so either convention works. Checked once at boot
/// (`main.rs`) to skip configured services entirely and start a plain
/// shell instead - the escape hatch for "config is broken and the system
/// won't come up".
pub fn rescue_requested(args: &HashMap<String, Option<String>>) -> bool {
    args.contains_key("single") || args.contains_key("mitos.rescue")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_valued_tokens() {
        let map = parse_str("root=/dev/sda1 quiet splash");
        assert_eq!(map.get("root"), Some(&Some("/dev/sda1".to_string())));
        assert_eq!(map.get("quiet"), Some(&None));
        assert_eq!(map.get("splash"), Some(&None));
    }

    #[test]
    fn keeps_only_the_first_equals_sign() {
        let map = parse_str("root=UUID=1234-5678 rootfstype=ext4");
        assert_eq!(map.get("root"), Some(&Some("UUID=1234-5678".to_string())));
        assert_eq!(map.get("rootfstype"), Some(&Some("ext4".to_string())));
    }

    #[test]
    fn strips_quotes_from_quoted_values() {
        let map = parse_str(r#"foo="bar baz" quiet"#);
        assert_eq!(map.get("foo"), Some(&Some("bar baz".to_string())));
    }

    #[test]
    fn ignores_extra_whitespace() {
        let map = parse_str("  root=/dev/sda1   quiet  ");
        assert_eq!(map.get("root"), Some(&Some("/dev/sda1".to_string())));
        assert_eq!(map.get("quiet"), Some(&None));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn recognizes_rescue_triggers() {
        assert!(rescue_requested(&parse_str("root=/dev/sda1 single")));
        assert!(rescue_requested(&parse_str("root=/dev/sda1 mitos.rescue")));
        assert!(!rescue_requested(&parse_str("root=/dev/sda1 quiet")));
    }
}
