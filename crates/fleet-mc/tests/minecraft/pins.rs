//! The slow tests' server pin matches the dev stack's (ADR-0003, ADR-0011),
//! so a version bump can't update one and forget the other.

use crate::harness::{IMAGE, TAG, VERSION};

const COMPOSE: &str = include_str!("../../../../deploy/compose.dev.yaml");

/// The value of the first `key:` line in the compose file, without quotes.
fn compose_value(key: &str) -> Option<&'static str> {
    COMPOSE.lines().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix(':')?;
        Some(value.trim().trim_matches('"'))
    })
}

#[test]
fn the_test_servers_image_is_the_one_compose_dev_pins() {
    assert_eq!(
        compose_value("image"),
        Some(format!("{IMAGE}:{TAG}").as_str())
    );
}

#[test]
fn the_test_servers_minecraft_version_is_the_one_compose_dev_pins() {
    assert_eq!(compose_value("VERSION"), Some(VERSION));
}
