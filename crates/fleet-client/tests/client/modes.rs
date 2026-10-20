//! The mode routes (Plan.md P11.4; ADR-0020, S12–S16, S19) through the
//! real server: listing, creating, the pointer to the step-up route and a
//! field error's path. What each viewer sees of a mode is fleet-server's to
//! test.

use fleet_api_types::modes::{CreateModeRequest, ModePage};
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
