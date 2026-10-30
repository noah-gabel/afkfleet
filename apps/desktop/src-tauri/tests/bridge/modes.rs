//! The modes' write commands (Plan.md P11.9; ADR-0020, U62, S16–S18): a create
//! and an edit, each with a step-up twin for a command off the allowlist, and
//! a delete after a step-up, against the real server.

use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::modes::{
    CreateModeRequest, ModeActionKind, ModeActionRequest, ModeScheduleKind, ModeScheduleRequest,
    ModeStepRequest, ModeSteps, ModeVisibility, UpdateModeRequest,
};
use fleet_api_types::{ErrorCode, ModeId};

use crate::support::{Bridge, Server, is_api, is_invalid, step_up_request};

/// A step at the mode's start that sends `message`, or jumps without one.
fn step(message: Option<&str>) -> ModeStepRequest {
    ModeStepRequest {
        action: ModeActionRequest {
            kind: message.map_or(ModeActionKind::Jump, |_| ModeActionKind::SendChat),
            yaw: None,
            pitch: None,
            max_yaw: None,
            max_pitch: None,
            on: None,
            slot: None,
            message: message.map(str::to_owned),
        },
        schedule: ModeScheduleRequest {
            kind: ModeScheduleKind::AtStart,
            interval_ms: None,
            jitter_ms: None,
        },
        probability: 100,
    }
}

fn create(name: &str, message: Option<&str>) -> CreateModeRequest {
    CreateModeRequest {
        name: name.to_owned(),
        visibility: ModeVisibility::Private,
        steps: ModeSteps(vec![step(message)]),
    }
}

fn update(version: u32, name: &str, message: Option<&str>) -> UpdateModeRequest {
    UpdateModeRequest {
        version,
        name: name.to_owned(),
        visibility: ModeVisibility::Private,
        steps: ModeSteps(vec![step(message)]),
    }
}

fn unknown() -> ModeId {
    "01900000-0000-7000-8000-00000000000c".parse().unwrap()
}

#[tokio::test]
async fn without_a_server_every_mode_write_is_no_server() {
    let bridge = Bridge::new();
    let state = &bridge.state;

    let answers = [
        state.create_mode(create("Fishing", None)).await.map(drop),
        state
            .create_mode_with_commands(create("Fishing", None))
            .await
            .map(drop),
        state
            .update_mode(unknown(), update(1, "Fishing", None))
            .await
            .map(drop),
        state
            .update_mode_with_commands(unknown(), update(1, "Fishing", None))
            .await
            .map(drop),
        state.delete_mode(unknown()).await,
    ];

    for answer in answers {
        assert_eq!(answer.unwrap_err().code, CommandErrorCode::NoServer);
    }
}

#[tokio::test]
async fn a_broken_request_is_refused_before_any_request() {
    let bridge = Bridge::new();
    let state = &bridge.state;

    let name = state.create_mode(create("", None)).await.unwrap_err();
    let twin_name = state
        .create_mode_with_commands(create("afk", None))
        .await
        .unwrap_err();
    let version = state
        .update_mode(unknown(), update(0, "Fishing", None))
        .await
        .unwrap_err();
    let twin_version = state
        .update_mode_with_commands(unknown(), update(0, "Fishing", None))
        .await
        .unwrap_err();

    assert!(is_invalid(&name, "name"), "{name:?}");
    assert!(is_invalid(&twin_name, "name"), "{twin_name:?}");
    assert!(is_invalid(&version, "version"), "{version:?}");
    assert!(is_invalid(&twin_version, "version"), "{twin_version:?}");
}

#[tokio::test]
async fn a_user_creates_edits_and_deletes_a_mode_with_the_step_ups_it_needs() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let state = &owner.state;

    let created = state.create_mode(create("Fishing", None)).await.unwrap();
    let edited = state
        .update_mode(created.id, update(1, "Fishing 2", None))
        .await
        .unwrap();
    let old_version = state
        .update_mode(created.id, update(1, "Fishing 3", None))
        .await
        .unwrap_err();
    let unstepped = state.delete_mode(created.id).await.unwrap_err();
    state.step_up(step_up_request()).await.unwrap();
    state.delete_mode(created.id).await.unwrap();
    let gone = state.delete_mode(created.id).await.unwrap_err();

    assert_eq!(created.version, 1);
    assert_eq!(edited.version, 2);
    assert_eq!(edited.name.as_str(), "Fishing 2");
    assert!(
        is_api(&old_version, 409, ErrorCode::ModeChanged),
        "{old_version:?}"
    );
    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
}

#[tokio::test]
async fn a_command_off_the_allowlist_goes_through_each_twin_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let state = &owner.state;
    let plain = state.create_mode(create("Plain", None)).await.unwrap();

    let create_pointer = state
        .create_mode(create("Warp", Some("/warp home")))
        .await
        .unwrap_err();
    let update_pointer = state
        .update_mode(plain.id, update(1, "Plain", Some("/warp home")))
        .await
        .unwrap_err();
    let create_unstepped = state
        .create_mode_with_commands(create("Warp", Some("/warp home")))
        .await
        .unwrap_err();
    let update_unstepped = state
        .update_mode_with_commands(plain.id, update(1, "Plain", Some("/warp home")))
        .await
        .unwrap_err();
    state.step_up(step_up_request()).await.unwrap();
    let created = state
        .create_mode_with_commands(create("Warp", Some("/warp home")))
        .await
        .unwrap();
    let edited = state
        .update_mode_with_commands(plain.id, update(1, "Plain", Some("/warp home")))
        .await
        .unwrap();

    assert!(
        is_api(&create_pointer, 403, ErrorCode::ModeNeedsStepUp),
        "{create_pointer:?}"
    );
    assert!(
        is_api(&update_pointer, 403, ErrorCode::ModeNeedsStepUp),
        "{update_pointer:?}"
    );
    assert!(
        is_api(&create_unstepped, 403, ErrorCode::StepUpRequired),
        "{create_unstepped:?}"
    );
    assert!(
        is_api(&update_unstepped, 403, ErrorCode::StepUpRequired),
        "{update_unstepped:?}"
    );
    assert!(created.unlisted_command);
    assert!(edited.unlisted_command);
    assert_eq!(edited.version, 2);
}
