//! The servers' settings commands (Plan.md P11.9; ADR-0020, U54, U56, S77): a
//! page of the servers' settings, and a create, an edit and a delete, each
//! after a step-up, against the real server.

use fleet_api_types::app::CommandErrorCode;
use fleet_api_types::server_settings::{
    ChatFormatsInput, ConflictTextsInput, CreateServerSettingsRequest, PreviewChatFormatsRequest,
    PreviewChatFormatsResponse, SERVER_SETTINGS_PAGE_MAX, UpdateServerSettingsRequest,
};
use fleet_api_types::{BoundedText, ErrorCode, ServerSettingsId};

use crate::support::{Bridge, Server, is_api, is_invalid, step_up_request};

fn create(server: &str, texts: &[&str]) -> CreateServerSettingsRequest {
    CreateServerSettingsRequest {
        server: server.to_owned(),
        conflict_texts: ConflictTextsInput(texts.iter().map(|&text| text.to_owned()).collect()),
        chat_formats: ChatFormatsInput::default(),
    }
}

fn update(version: u32, server: &str, texts: &[&str]) -> UpdateServerSettingsRequest {
    UpdateServerSettingsRequest {
        version,
        server: server.to_owned(),
        conflict_texts: ConflictTextsInput(texts.iter().map(|&text| text.to_owned()).collect()),
        chat_formats: ChatFormatsInput::default(),
    }
}

fn unknown() -> ServerSettingsId {
    "01900000-0000-7000-8000-00000000000d".parse().unwrap()
}

#[tokio::test]
async fn without_a_server_every_server_settings_command_is_no_server() {
    let bridge = Bridge::new();
    let state = &bridge.state;

    let answers = [
        state.server_settings(None, None).await.map(drop),
        state
            .create_server_settings(create("mc.example.com", &["One"]))
            .await
            .map(drop),
        state
            .update_server_settings(unknown(), update(1, "mc.example.com", &["One"]))
            .await
            .map(drop),
        state.delete_server_settings(unknown()).await,
    ];

    for answer in answers {
        assert_eq!(answer.unwrap_err().code, CommandErrorCode::NoServer);
    }
}

#[tokio::test]
async fn a_page_past_ten_or_a_broken_request_is_refused_before_any_request() {
    let bridge = Bridge::new();
    let long = "x".repeat(1025);

    for limit in [0, SERVER_SETTINGS_PAGE_MAX + 1] {
        let page = bridge
            .state
            .server_settings(None, Some(limit))
            .await
            .unwrap_err();
        assert!(is_invalid(&page, "limit"), "{page:?}");
    }
    let created = bridge
        .state
        .create_server_settings(create("mc.example.com", &["One", &long]))
        .await
        .unwrap_err();
    let edited = bridge
        .state
        .update_server_settings(unknown(), update(0, "mc.example.com", &["One"]))
        .await
        .unwrap_err();

    assert!(is_invalid(&created, "conflict_texts[1]"), "{created:?}");
    assert!(is_invalid(&edited, "version"), "{edited:?}");
}

#[tokio::test]
async fn the_owner_adds_lists_edits_and_deletes_a_servers_settings_after_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;

    let unstepped = owner
        .state
        .create_server_settings(create("MC.example.com", &["Already connected"]))
        .await
        .unwrap_err();
    owner.state.step_up(step_up_request()).await.unwrap();
    let created = owner
        .state
        .create_server_settings(create("MC.example.com", &["Already connected"]))
        .await
        .unwrap();
    let listed = owner.state.server_settings(None, None).await.unwrap();
    let edited = owner
        .state
        .update_server_settings(created.id, update(1, "mc.example.com", &["Two\nlines"]))
        .await
        .unwrap();
    let stale = owner
        .state
        .update_server_settings(created.id, update(1, "mc.example.com", &["Three"]))
        .await
        .unwrap_err();
    owner
        .state
        .delete_server_settings(created.id)
        .await
        .unwrap();
    let gone = owner
        .state
        .delete_server_settings(created.id)
        .await
        .unwrap_err();
    let after = owner.state.server_settings(None, None).await.unwrap();

    assert!(
        is_api(&unstepped, 403, ErrorCode::StepUpRequired),
        "{unstepped:?}"
    );
    assert_eq!(created.server.as_str(), "mc.example.com:25565");
    assert_eq!(
        listed.items.iter().map(|row| row.id).collect::<Vec<_>>(),
        [created.id]
    );
    assert_eq!(edited.version, 2);
    assert_eq!(
        edited.conflict_texts[0]
            .lines
            .iter()
            .map(BoundedText::as_str)
            .collect::<Vec<_>>(),
        ["Two", "lines"]
    );
    assert!(
        is_api(&stale, 409, ErrorCode::ServerSettingsChanged),
        "{stale:?}"
    );
    assert!(is_api(&gone, 404, ErrorCode::NotFound), "{gone:?}");
    assert_eq!(after.items, []);
    server.stop().await;
}

/// A preview's request: `formats` tried on `line`.
fn preview(formats: &[&str], line: &str) -> PreviewChatFormatsRequest {
    PreviewChatFormatsRequest {
        chat_formats: ChatFormatsInput(formats.iter().map(|&format| format.to_owned()).collect()),
        line: line.to_owned(),
    }
}

#[tokio::test]
async fn without_a_server_a_preview_is_no_server() {
    let bridge = Bridge::new();

    let answer = bridge
        .state
        .preview_chat_formats(preview(&["{sender} : "], "Notch : hi"))
        .await;

    assert_eq!(
        answer.map_err(|error| error.code),
        Err(CommandErrorCode::NoServer)
    );
}

#[tokio::test]
async fn a_preview_with_a_broken_format_or_a_long_line_is_refused_before_any_request() {
    let bridge = Bridge::new();

    let format = bridge
        .state
        .preview_chat_formats(preview(&["{sender} : ", "{Sender} : "], "Notch : hi"))
        .await;
    let line = bridge
        .state
        .preview_chat_formats(preview(&["{sender} : "], &"x".repeat(1025)))
        .await;

    assert!(
        format
            .as_ref()
            .is_err_and(|error| is_invalid(error, "chat_formats[1]")),
        "{format:?}"
    );
    assert!(
        line.as_ref().is_err_and(|error| is_invalid(error, "line")),
        "{line:?}"
    );
}

/// The Owner tries unsaved formats on lines, without a step-up: the first
/// format that fits names the sender, counted from 0, and a line none fits
/// answers nothing (ADR-0020, U58, R142).
#[tokio::test]
async fn the_owner_tries_chat_formats_on_a_line_without_a_step_up() {
    let server = Server::start().await;
    let (owner, _) = server.owner().await;
    let formats = ["<{sender}> ", "[{any}] {sender} : "];

    let matched = owner
        .state
        .preview_chat_formats(preview(&formats, "[Clan] Notch : hello"))
        .await
        .unwrap();
    let unmatched = owner
        .state
        .preview_chat_formats(preview(&formats, "Server restarting soon"))
        .await
        .unwrap();

    assert_eq!(
        matched.sender.as_ref().map(BoundedText::as_str),
        Some("Notch")
    );
    assert_eq!(matched.format, Some(1));
    assert_eq!(
        unmatched,
        PreviewChatFormatsResponse {
            sender: None,
            format: None
        }
    );
    server.stop().await;
}
