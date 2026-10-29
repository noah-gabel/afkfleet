//! The servers' settings routes (Plan.md P11.9; ADR-0020, U54, U58, S77,
//! S78, R134) through the real server: listing, creating, editing and
//! deleting, the step-up every write needs, a stale version, a missing row
//! and a field error's path, and the chat formats' preview. Who may do what
//! is fleet-server's to test.

use fleet_api_types::ErrorCode;
use fleet_api_types::server_settings::{ChatFormatsInput, ConflictTextsInput};
use fleet_api_types::server_settings::{
    CreateServerSettingsRequest, PreviewChatFormatsRequest, ServerSettingsPageQuery,
    UpdateServerSettingsRequest,
};

use crate::support::{Server, is_api, step_up};

fn create(server: &str, texts: &[&str]) -> CreateServerSettingsRequest {
    CreateServerSettingsRequest {
        server: server.to_owned(),
        conflict_texts: ConflictTextsInput(texts.iter().map(|text| (*text).to_owned()).collect()),
        chat_formats: ChatFormatsInput::default(),
    }
}

fn update(version: u32, server: &str, texts: &[&str]) -> UpdateServerSettingsRequest {
    UpdateServerSettingsRequest {
        version,
        server: server.to_owned(),
        conflict_texts: ConflictTextsInput(texts.iter().map(|text| (*text).to_owned()).collect()),
        chat_formats: ChatFormatsInput::default(),
    }
}

fn first_page() -> ServerSettingsPageQuery {
    ServerSettingsPageQuery {
        before: None,
        limit: None,
    }
}

/// The Owner creates a server's settings after a step-up, lists them, edits
/// them and deletes them; each write without a fresh step-up is refused.
#[tokio::test]
async fn the_owner_creates_lists_edits_and_deletes_a_servers_settings() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;

    let without = owner
        .create_server_settings(&create("mc.example.com", &["Already connected"]))
        .await
        .unwrap_err();
    step_up(&owner).await;
    let created = owner
        .create_server_settings(&create("mc.example.com", &["Already connected"]))
        .await
        .unwrap();
    let listed = owner.server_settings(&first_page()).await.unwrap();
    let edited = owner
        .update_server_settings(created.id, &update(1, "mc.example.com", &["Two"]))
        .await
        .unwrap();
    owner.delete_server_settings(created.id).await.unwrap();
    let after = owner.server_settings(&first_page()).await.unwrap();

    assert!(
        is_api(&without, 403, ErrorCode::StepUpRequired),
        "{without:?}"
    );
    assert_eq!(created.server.as_str(), "mc.example.com:25565");
    assert_eq!(created.version, 1);
    assert_eq!(listed.items, core::slice::from_ref(&created));
    assert_eq!(edited.version, 2);
    assert_eq!(edited.conflict_texts[0].lines[0].as_str(), "Two");
    assert_eq!(after.items, []);
    server.stop().await;
}

/// A stale version is 409 `server_settings_changed`; a missing row 404; a
/// broken rule one field error at its path.
#[tokio::test]
async fn a_stale_version_a_missing_row_and_a_broken_rule_come_back_as_api_errors() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let created = owner
        .create_server_settings(&create("mc.example.com", &["One"]))
        .await
        .unwrap();
    owner
        .update_server_settings(created.id, &update(1, "mc.example.com", &["Two"]))
        .await
        .unwrap();

    let stale = owner
        .update_server_settings(created.id, &update(1, "mc.example.com", &["Three"]))
        .await
        .unwrap_err();
    owner.delete_server_settings(created.id).await.unwrap();
    let missing = owner.delete_server_settings(created.id).await.unwrap_err();
    let broken = owner
        .create_server_settings(&create("play.example.com", &["One", ""]))
        .await
        .unwrap_err();

    assert!(
        is_api(&stale, 409, ErrorCode::ServerSettingsChanged),
        "{stale:?}"
    );
    assert!(is_api(&missing, 404, ErrorCode::NotFound), "{missing:?}");
    let fleet_client::ClientError::Api(failure) = &broken else {
        panic!("an API error: {broken:?}");
    };
    assert_eq!(failure.status, 422);
    assert_eq!(
        failure
            .fields
            .iter()
            .map(|field| (field.path.as_str(), field.message.as_str()))
            .collect::<Vec<_>>(),
        [("conflict_texts[1]", "must not be empty")]
    );
    server.stop().await;
}

/// A Member is refused the list.
#[tokio::test]
async fn a_member_is_refused_the_list() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;

    let refused = alice.server_settings(&first_page()).await.unwrap_err();

    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    server.stop().await;
}

fn preview(formats: &[&str], line: &str) -> PreviewChatFormatsRequest {
    PreviewChatFormatsRequest {
        chat_formats: ChatFormatsInput(formats.iter().map(|format| (*format).to_owned()).collect()),
        line: line.to_owned(),
    }
}

/// The Owner previews unsaved formats without a step-up: the sender the
/// first fitting one reads and its place, or nothing; nothing is stored.
#[tokio::test]
async fn the_owner_previews_chat_formats_without_a_step_up() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    let formats = ["[{any}] {sender} : ", "{sender} : "];

    let clan = owner
        .preview_chat_formats(&preview(&formats, "[Clan] Notch : hello"))
        .await
        .unwrap();
    let plain = owner
        .preview_chat_formats(&preview(&formats, "Notch : hello"))
        .await
        .unwrap();
    let none = owner
        .preview_chat_formats(&preview(&formats, "Server restarting soon"))
        .await
        .unwrap();
    let stored = owner.server_settings(&first_page()).await.unwrap();

    assert_eq!(
        clan.sender
            .as_ref()
            .map(fleet_api_types::BoundedText::as_str),
        Some("Notch")
    );
    assert_eq!(clan.format, Some(0));
    assert_eq!(plain.format, Some(1));
    assert_eq!((none.sender, none.format), (None, None));
    assert_eq!(stored.items, []);
    server.stop().await;
}

/// A Member is refused the preview; a broken format is one field error at
/// its path.
#[tokio::test]
async fn a_members_preview_and_a_broken_format_come_back_as_api_errors() {
    let server = Server::start().await;
    let (owner, _, _) = server.owner().await;
    step_up(&owner).await;
    let (alice, _) = server.member(&owner, "alice").await;

    let refused = alice
        .preview_chat_formats(&preview(&["{sender} : "], "Notch : hi"))
        .await
        .unwrap_err();
    let broken = owner
        .preview_chat_formats(&preview(&["{sender} : ", "{Sender} : "], "Notch : hi"))
        .await
        .unwrap_err();

    assert!(is_api(&refused, 403, ErrorCode::Forbidden), "{refused:?}");
    let fleet_client::ClientError::Api(failure) = &broken else {
        panic!("an API error: {broken:?}");
    };
    assert_eq!(failure.status, 422);
    assert_eq!(
        failure
            .fields
            .iter()
            .map(|field| field.path.as_str())
            .collect::<Vec<_>>(),
        ["chat_formats[1]"]
    );
    server.stop().await;
}
