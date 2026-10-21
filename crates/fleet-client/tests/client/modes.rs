//! The mode routes (Plan.md P11.4; ADR-0020, S12–S19) through the real
//! server: listing, creating, editing and deleting, the pointer to the
//! step-up route, a stale version and a field error's path. What each
//! viewer sees of a mode is fleet-server's to test.

use fleet_api_types::modes::{CreateModeRequest, ModePage, ModeResponse, UpdateModeRequest};
use fleet_api_types::{ErrorCode, PageQuery};
use serde_json::json;

use crate::support::{Server, is_api, step_up};

/// A new private mode named `name` that sends `message` every minute.
fn chatting(name: &str, message: &str) -> CreateModeRequest {
    serde_json::from_value(json!({ "name": name, "visibility": "private", "steps": [
        { "action": { "type": "send_chat", "message": message }, "schedule": { "type": "every", "interval_ms": 60_000, "jitter_ms": 0 }, "probability": 100 },
    ] }))
    .unwrap()
}

fn first_page() -> PageQuery<fleet_api_types::ModeId> {
    PageQuery {
        before: None,
        limit: None,
    }
}

fn names(page: &ModePage) -> Vec<&str> {
    page.items.iter().map(|mode| mode.name.as_str()).collect()
}

/// A Member sees the built-in modes, and their own once they made it.
#[tokio::test]
async fn a_member_lists_the_built_in_modes_and_then_their_own() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;

    let before = alice.modes(&first_page()).await.unwrap();
    let created = alice
        .create_mode(&chatting("Greeter", "hello"))
        .await
        .unwrap();
    let after = alice.modes(&first_page()).await.unwrap();

    assert_eq!(names(&before), ["farm", "afk"]);
    assert_eq!(created.name.as_str(), "Greeter");
    assert_eq!(created.version, 1);
    assert_eq!(names(&after), ["Greeter", "farm", "afk"]);
    assert_eq!(after.items.first(), Some(&created));
    server.stop().await;
}

/// A mode that sends a command off the allowlist is pointed to the step-up
/// route, which needs a fresh step-up, and then takes it (S16).
#[tokio::test]
async fn a_command_off_the_allowlist_needs_the_step_up_route() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let mode = chatting("Ops", "/op AfkBot2");

    let pointed = owner.create_mode(&mode).await.unwrap_err();
    let without = owner.create_mode_with_commands(&mode).await.unwrap_err();
    step_up(&owner).await;
    let created = owner.create_mode_with_commands(&mode).await.unwrap();

    assert!(
        is_api(&pointed, 403, ErrorCode::ModeNeedsStepUp),
        "{pointed:?}"
    );
    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert_eq!(created.name.as_str(), "Ops");
    server.stop().await;
}

/// A rule a field breaks comes back as one field error at its path.
#[tokio::test]
async fn a_field_that_breaks_its_rule_is_422_at_its_path() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;

    let error = owner
        .create_mode(&chatting("Greeter", "hi"))
        .await
        .and(owner.create_mode(&chatting("GREETER", "hello")).await)
        .unwrap_err();

    assert!(
        is_api(&error, 422, ErrorCode::ValidationFailed),
        "{error:?}"
    );
    let fields: Vec<(&str, &str)> = error
        .api()
        .unwrap()
        .fields
        .iter()
        .map(|field| (field.path.as_str(), field.message.as_str()))
        .collect();
    assert_eq!(fields, [("name", "is already another mode's name")]);
    server.stop().await;
}

/// `mode` as it should be: named `name`, sending `message` every minute,
/// at the version it was answered with.
fn edited(mode: &ModeResponse, name: &str, message: &str) -> UpdateModeRequest {
    serde_json::from_value(json!({ "version": mode.version, "name": name, "visibility": "private", "steps": [
        { "action": { "type": "send_chat", "message": message }, "schedule": { "type": "every", "interval_ms": 60_000, "jitter_ms": 0 }, "probability": 100 },
    ] }))
    .unwrap()
}

/// An edit at the version read answers the next one; the same edit again,
/// at the old version, shows the same and changes nothing; another change
/// at the old version is `mode_changed` (S18, S21).
#[tokio::test]
async fn an_edit_carries_the_version_it_read() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let created = owner
        .create_mode(&chatting("Greeter", "hello"))
        .await
        .unwrap();

    let first = owner
        .update_mode(created.id, &edited(&created, "Greeter", "hello there"))
        .await
        .unwrap();
    let again = owner
        .update_mode(created.id, &edited(&created, "Greeter", "hello there"))
        .await
        .unwrap();
    let stale = owner
        .update_mode(created.id, &edited(&created, "Greeter", "bye"))
        .await
        .unwrap_err();

    assert_eq!(first.version, 2);
    assert_eq!(again, first);
    assert!(is_api(&stale, 409, ErrorCode::ModeChanged), "{stale:?}");
    server.stop().await;
}

/// An edit with a command off the allowlist is pointed to the step-up
/// twin, which then takes it (S16).
#[tokio::test]
async fn an_edit_with_a_command_off_the_allowlist_needs_the_step_up_route() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let created = owner
        .create_mode(&chatting("Greeter", "hello"))
        .await
        .unwrap();
    let ops = edited(&created, "Greeter", "/op AfkBot2");

    let pointed = owner.update_mode(created.id, &ops).await.unwrap_err();
    step_up(&owner).await;
    let taken = owner
        .update_mode_with_commands(created.id, &ops)
        .await
        .unwrap();

    assert!(
        is_api(&pointed, 403, ErrorCode::ModeNeedsStepUp),
        "{pointed:?}"
    );
    assert_eq!(taken.version, 2);
    server.stop().await;
}

/// A delete needs a fresh step-up, then answers 204, and the mode is gone.
#[tokio::test]
async fn a_delete_needs_the_step_up_and_then_the_mode_is_gone() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let created = owner
        .create_mode(&chatting("Greeter", "hello"))
        .await
        .unwrap();

    let without = owner.delete_mode(created.id).await.unwrap_err();
    step_up(&owner).await;
    owner.delete_mode(created.id).await.unwrap();
    let after = owner.modes(&first_page()).await.unwrap();

    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert_eq!(names(&after), ["farm", "afk"]);
    server.stop().await;
}
