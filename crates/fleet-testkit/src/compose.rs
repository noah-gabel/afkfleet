//! A line reader of the repository's compose files, for the tests that pin
//! them (ADR-0014; ADR-0022, S14–S17): fleet-agent's and fleet-server's
//! `tests/deploy.rs` read `deploy/compose*.yaml` through it.
//!
//! It knows only the shape those files have: top-level keys at column 0,
//! services two spaces in under `services:`, their keys four spaces in, and
//! a list item or a mapping's key one level deeper. It never interprets
//! YAML beyond that: a test asserts lines as written, so what it pins is
//! what an operator reads.

/// The lines of the top-level block `key:` of `text` (its own line left
/// out), up to the next top-level key; empty when there's none.
#[must_use]
pub fn block<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    let header = format!("{key}:");
    text.lines()
        .skip_while(|line| line.trim_end() != header)
        .skip(1)
        .take_while(|line| {
            line.is_empty() || line.starts_with(' ') || line.trim_start().starts_with('#')
        })
        .collect()
}

/// The lines of the service `name` of `text` (its own line left out), up to
/// the next service or top-level key; empty when there's none.
#[must_use]
pub fn service<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let header = format!("  {name}:");
    block(text, "services")
        .into_iter()
        .skip_while(|line| line.trim_end() != header)
        .skip(1)
        .take_while(|line| {
            line.is_empty() || line.starts_with("    ") || line.trim_start().starts_with('#')
        })
        .collect()
}

/// The value of `key` among `lines`, on its own line (`key: value`), the
/// first one at any depth; `None` when no line sets it so.
#[must_use]
pub fn value<'a>(lines: &[&'a str], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    lines.iter().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// The items of the list `key` among `lines` (`key:` then `- item` lines one
/// level deeper), as written without the `- `; empty when there's none.
#[must_use]
pub fn list<'a>(lines: &[&'a str], key: &str) -> Vec<&'a str> {
    let header = format!("{key}:");
    let mut found = lines.iter().skip_while(|line| line.trim() != header);
    let Some(first) = found.next() else {
        return Vec::new();
    };
    let indent = first.len() - first.trim_start().len();
    found
        .take_while(|line| {
            let depth = line.len() - line.trim_start().len();
            line.trim().is_empty() || depth > indent
        })
        .filter_map(|line| line.trim().strip_prefix("- "))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
name: demo

services:
  server:
    image: server:1
    # A comment.
    volumes:
      - data:/data
      - keys:/keys:ro
    environment:
      KEY: value

  agent:
    image: agent:1
    networks: [fleet]

volumes:
  data:
  keys:
";

    #[test]
    fn a_service_runs_to_the_next_one() {
        let server = service(FILE, "server");

        assert_eq!(value(&server, "image"), Some("server:1"));
        assert_eq!(value(&server, "KEY"), Some("value"));
        assert_eq!(value(&server, "networks"), None, "the agent's");
        assert_eq!(service(FILE, "caddy"), Vec::<&str>::new());
    }

    #[test]
    fn a_list_is_its_items() {
        let server = service(FILE, "server");

        assert_eq!(list(&server, "volumes"), ["data:/data", "keys:/keys:ro"]);
        assert_eq!(list(&server, "ports"), Vec::<&str>::new());
    }

    #[test]
    fn a_block_runs_to_the_next_top_level_key() {
        assert_eq!(block(FILE, "volumes"), ["  data:", "  keys:"]);
        assert_eq!(value(&block(FILE, ""), "name"), None);
    }
}
