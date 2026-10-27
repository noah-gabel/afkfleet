//! The managed check's server pin matches the dev stack's, as fleet-mc's slow
//! tests' does (ADR-0003, ADR-0011), and so does the definition-of-done checks' container
//! (ADR-0020, S72), so a version bump can't update one and forget another.
//! These run in the default profile.

use super::minecraft::{IMAGE, TAG, VERSION};

const COMPOSE: &str = include_str!("../../../../../deploy/compose.dev.yaml");

/// The value of the first `key:` line in the compose file, without quotes.
fn compose_value(key: &str) -> Option<&'static str> {
    value_in(COMPOSE, key)
}

/// The value of `compose`'s first `key:` line, without quotes.
fn value_in<'a>(compose: &'a str, key: &str) -> Option<&'a str> {
    compose.lines().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix(':')?;
        Some(value.trim().trim_matches('"'))
    })
}

#[test]
fn the_checks_image_is_the_one_compose_dev_pins() {
    assert_eq!(
        compose_value("image"),
        Some(format!("{IMAGE}:{TAG}").as_str())
    );
}

#[test]
fn the_checks_minecraft_version_is_the_one_compose_dev_pins() {
    assert_eq!(compose_value("VERSION"), Some(VERSION));
}

/// `deploy/compose.dod.yaml`, read at run time.
fn dod_compose() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/compose.dod.yaml"),
    )
    .unwrap()
}

#[test]
fn the_dod_containers_image_is_the_one_compose_dev_and_the_check_pin() {
    let dod = dod_compose();

    assert_eq!(value_in(&dod, "image"), compose_value("image"));
    assert_eq!(
        value_in(&dod, "image"),
        Some(format!("{IMAGE}:{TAG}").as_str())
    );
}

#[test]
fn the_dod_containers_minecraft_version_is_the_one_compose_dev_and_the_check_pin() {
    let dod = dod_compose();

    assert_eq!(value_in(&dod, "VERSION"), compose_value("VERSION"));
    assert_eq!(value_in(&dod, "VERSION"), Some(VERSION));
}
