//! A secret on the wire never prints: not in a generated message's `Debug`, not
//! in an envelope or a request around it, and not in a conversion's error. Any
//! field named like a secret belongs to a redacted message (ADR-0019).

use std::collections::BTreeSet;

use fleet_proto::control::ServerMessage;
use fleet_proto::generated as wire;

use crate::WIRE_SCHEMA;

const TOKEN: &str = "eyJhbGciOi.secret-token-value";
const NAME: &str = "AfkBot1";
const PROFILE: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x4d, 0xef, 0x81, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
];
const BOT: [u8; 16] = [
    0x01, 0x8b, 0xcf, 0xe5, 0x68, 0x00, 0x7b, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
];

fn online(token: &str) -> wire::OnlineSession {
    wire::OnlineSession {
        username: NAME.to_owned(),
        uuid: PROFILE.to_vec(),
        access_token: token.to_owned(),
        expires_at_unix_ms: Some(1_790_000_000_000),
    }
}

fn grant(token: &str) -> wire::ServerMessage {
    wire::ServerMessage {
        msg: Some(wire::server_message::Msg::SessionGrant(
            wire::SessionGrant {
                id: 1,
                bot_id: BOT.to_vec(),
                session: Some(wire::session_grant::Session::Online(online(token))),
            },
        )),
    }
}

/// The ways the profile's UUID could show up in a `Debug` output: prost's byte
/// list, and the UUID's text.
fn profile_forms() -> [String; 3] {
    [
        format!("{:?}", PROFILE.to_vec()),
        uuid::Uuid::from_bytes(PROFILE).hyphenated().to_string(),
        uuid::Uuid::from_bytes(PROFILE).simple().to_string(),
    ]
}

fn assert_hides_the_session(output: &str) {
    assert!(!output.contains(TOKEN), "the token printed: {output}");
    assert!(!output.contains(NAME), "the name printed: {output}");
    for form in profile_forms() {
        assert!(!output.contains(&form), "the profile printed: {output}");
    }
}

#[test]
fn an_online_session_prints_its_expiry_only() {
    assert_eq!(
        format!("{:?}", online(TOKEN)),
        "OnlineSession { expires_at_unix_ms: Some(1790000000000), .. }"
    );
    assert_eq!(
        format!("{:#?}", online(TOKEN)),
        "OnlineSession {\n    expires_at_unix_ms: Some(\n        1790000000000,\n    ),\n    ..\n}"
    );
}

#[test]
fn a_grant_and_everything_around_it_hide_the_session() {
    let message = grant(TOKEN);
    let wire::server_message::Msg::SessionGrant(inner) = message.msg.clone().unwrap() else {
        unreachable!("a grant");
    };
    for output in [
        format!("{inner:?}"),
        format!("{:?}", inner.session),
        format!("{message:?}"),
        format!("{message:#?}"),
        format!("{:?}", tonic::Request::new(message.clone())),
    ] {
        assert_hides_the_session(&output);
    }
}

#[test]
fn the_validated_grant_keeps_its_token_secret() {
    let message = ServerMessage::try_from(grant(TOKEN)).unwrap();
    for output in [format!("{message:?}"), format!("{message:#?}")] {
        assert!(!output.contains(TOKEN), "the token printed: {output}");
    }
}

fn offline_grant() -> wire::ServerMessage {
    wire::ServerMessage {
        msg: Some(wire::server_message::Msg::SessionGrant(
            wire::SessionGrant {
                id: 1,
                bot_id: BOT.to_vec(),
                session: Some(wire::session_grant::Session::Offline(
                    wire::OfflineSession {
                        username: NAME.to_owned(),
                    },
                )),
            },
        )),
    }
}

/// The validated grant, and the message around it, print the session's
/// kind and expiry: never the name or the UUID (the threat model), like
/// the wire's `OnlineSession`.
#[rstest::rstest]
#[case::online(
    grant(TOKEN),
    "credentials: Online { expires_at: 2026-09-21T14:13:20Z, .. }"
)]
#[case::offline(offline_grant(), "credentials: Offline { .. }")]
fn the_validated_grant_shows_its_kind_and_expiry_never_the_name_or_the_uuid(
    #[case] message: wire::ServerMessage,
    #[case] kind: &str,
) {
    let ServerMessage::SessionGrant(grant) = ServerMessage::try_from(message).unwrap() else {
        unreachable!("a grant");
    };
    let message = ServerMessage::SessionGrant(grant.clone());

    for output in [
        format!("{grant:?}"),
        format!("{grant:#?}"),
        format!("{message:?}"),
        format!("{message:#?}"),
    ] {
        assert_hides_the_session(&output);
    }
    assert!(format!("{grant:?}").contains(kind), "{grant:?}");
}

#[test]
fn a_refused_token_never_shows_in_the_error() {
    let refused = format!("{TOKEN} with a space");
    let error = ServerMessage::try_from(grant(&refused)).unwrap_err();
    for output in [error.to_string(), format!("{error:?}")] {
        assert!(
            !output.contains("secret-token"),
            "the token printed: {output}"
        );
    }
}

/// Fields named like a secret that aren't one, each with its reason.
const NOT_SECRETS: [(&str, &str, &str); 1] = [(
    "fleet.v1.Kick",
    "key",
    "a kick's translation key, such as multiplayer.disconnect.banned",
)];

/// Words that make a field name look like a secret's.
const SECRET_WORDS: [&str; 4] = ["token", "secret", "password", "key"];

#[test]
fn every_field_named_like_a_secret_is_in_a_redacted_message() {
    let redacted: BTreeSet<&str> = WIRE_SCHEMA
        .lines()
        .filter_map(|line| line.strip_prefix("redacted "))
        .collect();
    let messages: BTreeSet<&str> = WIRE_SCHEMA
        .lines()
        .filter_map(|line| line.strip_prefix("message "))
        .collect();
    assert!(!redacted.is_empty());
    assert!(
        redacted.is_subset(&messages),
        "a redacted message doesn't exist"
    );

    let mut message = "";
    let mut seen = Vec::new();
    let mut flagged = Vec::new();
    for line in WIRE_SCHEMA.lines() {
        if let Some(name) = line.strip_prefix("message ") {
            message = name;
        } else if line.starts_with("enum ") || line.starts_with("service ") {
            message = "";
        } else if let Some(field) = line.strip_prefix("  ").filter(|_| !message.is_empty()) {
            let Some((_, rest)) = field.split_once(' ') else {
                continue;
            };
            let Some((name, _)) = rest.split_once(':') else {
                continue;
            };
            if !SECRET_WORDS.iter().any(|word| name.contains(word)) {
                continue;
            }
            seen.push(format!("{message}.{name}"));
            let allowed = redacted.contains(message)
                || NOT_SECRETS
                    .iter()
                    .any(|(owner, field, _)| *owner == message && *field == name);
            if !allowed {
                flagged.push(format!("{message}.{name}"));
            }
        }
    }
    // The guard really reads the schema's fields: it finds the token and the
    // kick's key.
    assert!(
        seen.contains(&"fleet.v1.OnlineSession.access_token".to_owned()),
        "{seen:?}"
    );
    assert!(seen.contains(&"fleet.v1.Kick.key".to_owned()), "{seen:?}");
    assert!(
        seen.contains(&"fleet.v1.EnrollRequest.token".to_owned()),
        "{seen:?}"
    );
    assert!(
        flagged.is_empty(),
        "fields named like a secret outside a redacted message: {flagged:?}"
    );
}
