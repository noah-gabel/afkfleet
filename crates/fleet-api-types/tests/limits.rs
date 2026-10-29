//! The UI's copy of the server's input limits against the Rust constants
//! (Plan.md P8.5, ADR-0017): `packages/ui/src/api/limits.ts` holds only
//! `export const NAME = <number>;` lines, which the UI's zod schemas use.
//! This test parses that file and fails on any value that differs, any name
//! missing, or any name it doesn't know, so the two can't drift.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use fleet_api_types::accounts::MC_NAME_MAX_CHARS;
use fleet_api_types::app::{CA_PEM_MAX_BYTES, SERVER_URL_MAX_BYTES};
use fleet_api_types::auth::{NEW_PASSWORD_MAX_CHARS, PASSWORD_MAX_CHARS, USERNAME_MAX_CHARS};
use fleet_api_types::bots::SERVER_ADDRESS_MAX_CHARS;
use fleet_api_types::chat::CHAT_MESSAGE_MAX_UNITS;
use fleet_api_types::ids::ID_CHARS;
use fleet_api_types::mfa::{CODE_MAX_CHARS, TOKEN_MAX_CHARS};
use fleet_api_types::modes::{MODE_MAX_STEPS, MODE_NAME_MAX_CHARS};
use fleet_api_types::page::PAGE_MAX;
use fleet_api_types::server_settings::{
    CHAT_FORMAT_MAX_CHARS, CHAT_FORMATS_MAX, CHAT_PREVIEW_LINE_MAX_CHARS, CONFLICT_TEXT_MAX_CHARS,
    CONFLICT_TEXTS_MAX, SERVER_SETTINGS_MAX, SERVER_SETTINGS_PAGE_MAX,
};
use fleet_core::auth::{NewPassword, STEP_UP_WINDOW};
use fleet_core::value::{McUsername, Username};

/// Every limit the UI mirrors, by its name in both languages.
fn expected() -> BTreeMap<&'static str, u64> {
    let usize = |value: usize| u64::try_from(value).unwrap();
    BTreeMap::from([
        ("USERNAME_MIN_CHARS", usize(Username::MIN_LEN)),
        ("USERNAME_MAX_CHARS", usize(USERNAME_MAX_CHARS)),
        ("PASSWORD_MAX_CHARS", usize(PASSWORD_MAX_CHARS)),
        ("NEW_PASSWORD_MIN_CHARS", usize(NewPassword::MIN_CHARS)),
        ("NEW_PASSWORD_MAX_CHARS", usize(NEW_PASSWORD_MAX_CHARS)),
        ("CODE_MAX_CHARS", usize(CODE_MAX_CHARS)),
        ("TOKEN_MAX_CHARS", usize(TOKEN_MAX_CHARS)),
        ("ID_CHARS", usize(ID_CHARS)),
        ("PAGE_MAX", u64::from(PAGE_MAX)),
        ("SERVER_URL_MAX_BYTES", usize(SERVER_URL_MAX_BYTES)),
        ("CA_PEM_MAX_BYTES", usize(CA_PEM_MAX_BYTES)),
        ("STEP_UP_WINDOW_SECS", STEP_UP_WINDOW.as_secs()),
        ("MC_NAME_MIN_CHARS", usize(McUsername::MIN_LEN)),
        ("MC_NAME_MAX_CHARS", usize(MC_NAME_MAX_CHARS)),
        ("MODE_NAME_MAX_CHARS", usize(MODE_NAME_MAX_CHARS)),
        ("MODE_MAX_STEPS", usize(MODE_MAX_STEPS)),
        ("SERVER_ADDRESS_MAX_CHARS", usize(SERVER_ADDRESS_MAX_CHARS)),
        ("CHAT_MESSAGE_MAX_UNITS", usize(CHAT_MESSAGE_MAX_UNITS)),
        ("CONFLICT_TEXTS_MAX", usize(CONFLICT_TEXTS_MAX)),
        ("CONFLICT_TEXT_MAX_CHARS", usize(CONFLICT_TEXT_MAX_CHARS)),
        ("SERVER_SETTINGS_MAX", u64::from(SERVER_SETTINGS_MAX)),
        ("CHAT_FORMATS_MAX", usize(CHAT_FORMATS_MAX)),
        ("CHAT_FORMAT_MAX_CHARS", usize(CHAT_FORMAT_MAX_CHARS)),
        (
            "CHAT_PREVIEW_LINE_MAX_CHARS",
            usize(CHAT_PREVIEW_LINE_MAX_CHARS),
        ),
        (
            "SERVER_SETTINGS_PAGE_MAX",
            u64::from(SERVER_SETTINGS_PAGE_MAX),
        ),
    ])
}

/// The constants in `text`, refusing any line that isn't a comment, blank,
/// or `export const NAME = <number>;`.
fn parse(text: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut constants = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let parsed = line
            .strip_prefix("export const ")
            .and_then(|rest| rest.strip_suffix(';'))
            .and_then(|rest| rest.split_once(" = "))
            .and_then(|(name, value)| Some((name.to_owned(), value.parse::<u64>().ok()?)));
        let Some((name, value)) = parsed else {
            return Err(format!("not a numeric constant: {line}"));
        };
        if constants.insert(name.clone(), value).is_some() {
            return Err(format!("{name} is declared twice"));
        }
    }
    Ok(constants)
}

#[test]
fn the_uis_limits_are_exactly_the_servers() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/ui/src/api/limits.ts");
    let text = fs::read_to_string(&path).unwrap();

    let constants = parse(&text).unwrap();

    let expected: BTreeMap<String, u64> = expected()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    assert_eq!(constants, expected);
}

#[test]
fn the_parser_refuses_anything_but_numeric_constants() {
    for text in [
        "export const A = 1;\nexport function b() {}\n",
        "export const A = \"1\";\n",
        "export const A = 1;\nexport const A = 2;\n",
        "const A = 1;\n",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
    assert_eq!(
        parse("// a comment\n\nexport const A = 1;\n").unwrap(),
        BTreeMap::from([("A".to_owned(), 1)])
    );
}
