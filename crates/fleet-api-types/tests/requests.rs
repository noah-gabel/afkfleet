//! The request DTOs as `fleet-client` sends them (Plan.md P8.1, ADR-0017):
//! each one serializes to exactly the JSON the server reads, which reads
//! back valid, and a field without a value is left out, never `null`.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use fleet_api_types::accounts::{
    ChangeGrantRequest, CreateGrantRequest, CreateOfflineAccountRequest,
};
use fleet_api_types::auth::{
    LoginMfaRequest, LoginRequest, RefreshRequest, RegisterRequest, StepUpRequest,
};
use fleet_api_types::invites::CreateInviteRequest;
use fleet_api_types::me::PasswordChangeRequest;
use fleet_api_types::mfa::MfaConfirmRequest;
use fleet_api_types::modes::CreateModeRequest;
use fleet_api_types::users::UserPatchRequest;
use fleet_api_types::{InviteId, PageQuery, SessionId, UserId};
use garde::Validate;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Reads `wire` as the server does, writes it back as the client does, and
/// checks that the two are the same JSON and that it reads back valid.
fn round_trip<T>(wire: &Value)
where
    T: Serialize + DeserializeOwned + Validate<Context = ()>,
{
    let request: T = serde_json::from_value(wire.clone()).unwrap();

    let sent = serde_json::to_value(&request).unwrap();

    assert_eq!(&sent, wire);
    let read: T = serde_json::from_value(sent).unwrap();
    assert!(read.validate().is_ok(), "{wire}");
}

#[test]
fn every_request_serializes_to_the_json_the_server_reads() {
    let password = "correct horse battery staple";
    round_trip::<LoginRequest>(&json!({ "username": "alice", "password": password }));
    round_trip::<LoginMfaRequest>(&json!({ "mfa_token": "afk_mt_x", "code": "123456" }));
    round_trip::<RefreshRequest>(&json!({ "refresh_token": "afk_rt_x" }));
    round_trip::<StepUpRequest>(&json!({ "password": password }));
    round_trip::<RegisterRequest>(
        &json!({ "invite": "afk_iv_x", "username": "alice", "password": password }),
    );
    round_trip::<PasswordChangeRequest>(&json!({ "new_password": password }));
    round_trip::<MfaConfirmRequest>(&json!({ "code": "123456" }));
    round_trip::<CreateInviteRequest>(&json!({ "role": "member" }));
    round_trip::<UserPatchRequest>(&json!({ "role": "admin" }));
    round_trip::<UserPatchRequest>(&json!({ "disabled": true }));
    round_trip::<CreateOfflineAccountRequest>(&json!({ "name": "AfkBot1" }));
    round_trip::<CreateGrantRequest>(&json!({ "username": "alice", "level": "control" }));
    round_trip::<ChangeGrantRequest>(&json!({ "level": "view" }));
    round_trip::<CreateModeRequest>(&json!({
        "name": "Greeter",
        "visibility": "private",
        "steps": [
            { "action": { "type": "look", "yaw": 90.0, "pitch": -10.5 }, "schedule": { "type": "at_start" }, "probability": 100 },
            { "action": { "type": "send_chat", "message": "hello" }, "schedule": { "type": "every", "interval_ms": 60000, "jitter_ms": 0 }, "probability": 50 }
        ]
    }));
}

#[test]
fn a_patch_leaves_out_the_change_it_doesnt_make() {
    let role: UserPatchRequest = serde_json::from_value(json!({ "role": "admin" })).unwrap();
    let disabled: UserPatchRequest = serde_json::from_value(json!({ "disabled": false })).unwrap();

    let role = serde_json::to_value(role).unwrap();
    let disabled = serde_json::to_value(disabled).unwrap();

    assert_eq!(role, json!({ "role": "admin" }));
    assert_eq!(disabled, json!({ "disabled": false }));
}

#[test]
fn a_page_query_names_the_reexported_ids() {
    let user: UserId = "01900000-0000-7000-8000-000000000001".parse().unwrap();
    let session: SessionId = "01900000-0000-7000-8000-000000000002".parse().unwrap();
    let invite: InviteId = "01900000-0000-7000-8000-000000000003".parse().unwrap();

    let queries = [
        serde_json::to_value(PageQuery {
            before: Some(user),
            limit: None,
        })
        .unwrap(),
        serde_json::to_value(PageQuery {
            before: Some(session),
            limit: Some(5),
        })
        .unwrap(),
        serde_json::to_value(PageQuery {
            before: Some(invite),
            limit: None,
        })
        .unwrap(),
    ];

    assert_eq!(
        queries,
        [
            json!({ "before": "01900000-0000-7000-8000-000000000001" }),
            json!({ "before": "01900000-0000-7000-8000-000000000002", "limit": 5 }),
            json!({ "before": "01900000-0000-7000-8000-000000000003" }),
        ]
    );
}
