//! The `OpenAPI` schemas against the TypeScript export (Plan.md P6.10,
//! ADR-0015): both describe the same wire types.
//!
//! A small renderer turns each schema into the TypeScript declaration ts-rs
//! writes for the same type, and the two are compared with comments
//! stripped and whitespace collapsed. The renderer knows only the constructs
//! the DTOs use and fails on anything else, so a new kind of field can't
//! slip past the comparison. What TypeScript can't express (`SafeInt`'s
//! range, each text's cap, an open enum's values) is checked directly.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;

use fleet_api_types::accounts::{
    AccountKind, AccountPage, AccountResponse, AccountStatus, ChangeGrantRequest,
    CreateGrantRequest, CreateOfflineAccountRequest, CreatedOfflineAccountResponse, GrantLevel,
    GrantPage, GrantResponse, LinkFlowResponse, LinkFlowState, LinkStartedResponse,
    MC_NAME_MAX_CHARS, RefusalReason, USER_CODE_MAX_CHARS, VERIFICATION_URI_MAX_CHARS,
};
use fleet_api_types::agents::{
    AGENT_NAME_MAX_CHARS, AGENT_VERSION_MAX_CHARS, AgentPage, AgentResponse, AgentStatus,
    BUNDLE_CHARS, CreatedEnrollmentTokenResponse, FINGERPRINT_CHARS,
};
use fleet_api_types::audit::{
    AUDIT_KEY_MAX_CHARS, AUDIT_TEXT_MAX_CHARS, AuditActionName, AuditDetail, AuditEntryResponse,
    AuditOutcomeName, AuditPage, AuditTargetKind, AuditTargetResponse, IP_MAX_CHARS,
};
use fleet_api_types::auth::{
    ACCESS_TOKEN_MAX_SECS, LoginMfaRequest, LoginRequest, LoginResponse, LoginStatus,
    NEW_PASSWORD_MAX_CHARS, PASSWORD_MAX_CHARS, REFRESH_TOKEN_MAX_SECS, RefreshRequest,
    RegisterRequest, RegisterResponse, StepUpRequest, TokenResponse, USERNAME_MAX_CHARS,
};
use fleet_api_types::bots::{
    AccountRestriction, BotAgent, BotModeRequest, BotPage, BotPatchRequest, BotResponse,
    BotStateKind, BotStateReason, BotStatus, BotStatusKind, ConnectFailure, DesiredState,
    DisconnectKind, KICK_KEY_MAX_CHARS, KICK_MESSAGE_MAX_CHARS, LastDisconnect,
    SERVER_ADDRESS_MAX_CHARS, SessionServerFailure, UnplacedReason,
};
use fleet_api_types::chat::{
    CHAT_MESSAGE_MAX_UNITS, CHAT_SENDER_MAX_CHARS, CHAT_TEXT_MAX_CHARS, ChatCommandRequest,
    ChatDirection, ChatFailure, ChatLine, ChatLineKind, ChatLineSender, ChatPage, ChatSendOutcome,
    ChatSendRequest, ChatSendResponse,
};
use fleet_api_types::error::{
    FIELD_MESSAGE_MAX_CHARS, FIELD_PATH_MAX_CHARS, MESSAGE_MAX_CHARS, REQUEST_ID_MAX_CHARS,
};
use fleet_api_types::events::{
    EVENT_TICKET_MAX_SECS, EventMessage, EventMessageKind, EventTicketResponse,
};
use fleet_api_types::ids::ID_CHARS;
use fleet_api_types::invites::{
    CreateInviteRequest, CreatedInviteResponse, InvitePage, InviteResponse, InviteState,
};
use fleet_api_types::me::{FailedCodeAttempts, MeResponse, PasswordChangeRequest};
use fleet_api_types::mfa::{
    CODE_MAX_CHARS, MfaConfirmRequest, MfaConfirmResponse, MfaEnrollResponse, MfaResetResponse,
    OTPAUTH_URI_MAX_CHARS, QR_SVG_MAX_CHARS, RECOVERY_CODE_MAX_CHARS, SETUP_TOKEN_MAX_SECS,
    TOKEN_MAX_CHARS, TOTP_SECRET_MAX_CHARS,
};
use fleet_api_types::modes::{
    CreateModeRequest, MODE_MESSAGE_MAX_CHARS, MODE_NAME_MAX_CHARS, ModeAction, ModeActionKind,
    ModeActionRequest, ModeOwnership, ModePage, ModeResponse, ModeSchedule, ModeScheduleKind,
    ModeScheduleRequest, ModeStep, ModeStepRequest, ModeVisibility, UpdateModeRequest,
    number_schema,
};
use fleet_api_types::sessions::{SessionPage, SessionResponse};
use fleet_api_types::time::TIMESTAMP_CHARS;
use fleet_api_types::users::{UserLabel, UserPage, UserPatchRequest, UserResponse, UserRole};
use fleet_api_types::{
    BoundedText, ErrorBody, ErrorCode, ErrorResponse, FieldError, Open, OpenEnum, SafeInt,
    Timestamp, openapi, typescript,
};
use rstest::rstest;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tempfile::TempDir;
use ts_rs::{Config, TS};
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ArrayItems, Schema};
use utoipa::{PartialSchema as _, ToSchema};

/// ts-rs's declaration of every exported type, by name. A new exported type
/// must be added here; `the_components_are_exactly_the_exported_types`
/// fails until it is.
fn declarations() -> BTreeMap<String, String> {
    let config = Config::new();
    [
        ("AccountKind", AccountKind::decl(&config)),
        ("AccountPage", AccountPage::decl(&config)),
        ("AccountResponse", AccountResponse::decl(&config)),
        ("AccountStatus", AccountStatus::decl(&config)),
        ("AccountRestriction", AccountRestriction::decl(&config)),
        ("BotAgent", BotAgent::decl(&config)),
        ("BotModeRequest", BotModeRequest::decl(&config)),
        ("BotPage", BotPage::decl(&config)),
        ("BotPatchRequest", BotPatchRequest::decl(&config)),
        ("BotResponse", BotResponse::decl(&config)),
        ("BotStateKind", BotStateKind::decl(&config)),
        ("BotStateReason", BotStateReason::decl(&config)),
        ("BotStatus", BotStatus::decl(&config)),
        ("BotStatusKind", BotStatusKind::decl(&config)),
        ("ConnectFailure", ConnectFailure::decl(&config)),
        ("DesiredState", DesiredState::decl(&config)),
        ("DisconnectKind", DisconnectKind::decl(&config)),
        ("LastDisconnect", LastDisconnect::decl(&config)),
        ("SessionServerFailure", SessionServerFailure::decl(&config)),
        ("UnplacedReason", UnplacedReason::decl(&config)),
        ("ChangeGrantRequest", ChangeGrantRequest::decl(&config)),
        ("ChatCommandRequest", ChatCommandRequest::decl(&config)),
        ("ChatSendOutcome", ChatSendOutcome::decl(&config)),
        ("ChatSendRequest", ChatSendRequest::decl(&config)),
        ("ChatSendResponse", ChatSendResponse::decl(&config)),
        ("ChatDirection", ChatDirection::decl(&config)),
        ("ChatLine", ChatLine::decl(&config)),
        ("ChatLineKind", ChatLineKind::decl(&config)),
        ("ChatLineSender", ChatLineSender::decl(&config)),
        ("ChatPage", ChatPage::decl(&config)),
        ("SafeInt", SafeInt::decl(&config)),
        (
            "CreatedEnrollmentTokenResponse",
            CreatedEnrollmentTokenResponse::decl(&config),
        ),
        ("CreateGrantRequest", CreateGrantRequest::decl(&config)),
        ("GrantLevel", GrantLevel::decl(&config)),
        ("GrantPage", GrantPage::decl(&config)),
        ("GrantResponse", GrantResponse::decl(&config)),
        (
            "CreateOfflineAccountRequest",
            CreateOfflineAccountRequest::decl(&config),
        ),
        (
            "CreatedOfflineAccountResponse",
            CreatedOfflineAccountResponse::decl(&config),
        ),
        ("LinkFlowResponse", LinkFlowResponse::decl(&config)),
        ("LinkFlowState", LinkFlowState::decl(&config)),
        ("LinkStartedResponse", LinkStartedResponse::decl(&config)),
        ("RefusalReason", RefusalReason::decl(&config)),
        ("CreateInviteRequest", CreateInviteRequest::decl(&config)),
        (
            "CreatedInviteResponse",
            CreatedInviteResponse::decl(&config),
        ),
        ("ErrorBody", ErrorBody::decl(&config)),
        ("ErrorCode", ErrorCode::decl(&config)),
        ("ErrorResponse", ErrorResponse::decl(&config)),
        ("FailedCodeAttempts", FailedCodeAttempts::decl(&config)),
        ("FieldError", FieldError::decl(&config)),
        ("InvitePage", InvitePage::decl(&config)),
        ("InviteResponse", InviteResponse::decl(&config)),
        ("InviteState", InviteState::decl(&config)),
        ("LoginMfaRequest", LoginMfaRequest::decl(&config)),
        ("LoginRequest", LoginRequest::decl(&config)),
        ("LoginResponse", LoginResponse::decl(&config)),
        ("LoginStatus", LoginStatus::decl(&config)),
        ("MeResponse", MeResponse::decl(&config)),
        ("MfaConfirmRequest", MfaConfirmRequest::decl(&config)),
        ("MfaConfirmResponse", MfaConfirmResponse::decl(&config)),
        ("MfaEnrollResponse", MfaEnrollResponse::decl(&config)),
        ("MfaResetResponse", MfaResetResponse::decl(&config)),
        (
            "PasswordChangeRequest",
            PasswordChangeRequest::decl(&config),
        ),
        ("RefreshRequest", RefreshRequest::decl(&config)),
        ("RegisterRequest", RegisterRequest::decl(&config)),
        ("RegisterResponse", RegisterResponse::decl(&config)),
        ("SessionPage", SessionPage::decl(&config)),
        ("SessionResponse", SessionResponse::decl(&config)),
        ("StepUpRequest", StepUpRequest::decl(&config)),
        ("TokenResponse", TokenResponse::decl(&config)),
        ("UserLabel", UserLabel::decl(&config)),
        ("UserPage", UserPage::decl(&config)),
        ("UserPatchRequest", UserPatchRequest::decl(&config)),
        ("UserResponse", UserResponse::decl(&config)),
        ("UserRole", UserRole::decl(&config)),
    ]
    .into_iter()
    .chain(mode_declarations(&config))
    .chain(event_declarations(&config))
    .chain(admin_declarations(&config))
    .map(|(name, declaration)| (name.to_owned(), declaration))
    .collect()
}

/// ts-rs's declaration of every agents and audit log type, the admin
/// screens' (ADR-0019, S31; ADR-0020, R65).
fn admin_declarations(config: &Config) -> [(&'static str, String); 10] {
    [
        ("AgentPage", AgentPage::decl(config)),
        ("AgentResponse", AgentResponse::decl(config)),
        ("AgentStatus", AgentStatus::decl(config)),
        ("AuditActionName", AuditActionName::decl(config)),
        ("AuditDetail", AuditDetail::decl(config)),
        ("AuditEntryResponse", AuditEntryResponse::decl(config)),
        ("AuditOutcomeName", AuditOutcomeName::decl(config)),
        ("AuditPage", AuditPage::decl(config)),
        ("AuditTargetKind", AuditTargetKind::decl(config)),
        ("AuditTargetResponse", AuditTargetResponse::decl(config)),
    ]
}

/// ts-rs's declaration of every live events type (ADR-0020, S36, R28).
fn event_declarations(config: &Config) -> [(&'static str, String); 4] {
    [
        ("ChatFailure", ChatFailure::decl(config)),
        ("EventMessage", EventMessage::decl(config)),
        ("EventMessageKind", EventMessageKind::decl(config)),
        ("EventTicketResponse", EventTicketResponse::decl(config)),
    ]
}

/// ts-rs's declaration of every mode type (ADR-0020, S12).
fn mode_declarations(config: &Config) -> [(&'static str, String); 14] {
    [
        ("ModeVisibility", ModeVisibility::decl(config)),
        ("ModeOwnership", ModeOwnership::decl(config)),
        ("ModeActionKind", ModeActionKind::decl(config)),
        ("ModeScheduleKind", ModeScheduleKind::decl(config)),
        ("CreateModeRequest", CreateModeRequest::decl(config)),
        ("UpdateModeRequest", UpdateModeRequest::decl(config)),
        ("ModeStepRequest", ModeStepRequest::decl(config)),
        ("ModeActionRequest", ModeActionRequest::decl(config)),
        ("ModeScheduleRequest", ModeScheduleRequest::decl(config)),
        ("ModeResponse", ModeResponse::decl(config)),
        ("ModeStep", ModeStep::decl(config)),
        ("ModeAction", ModeAction::decl(config)),
        ("ModeSchedule", ModeSchedule::decl(config)),
        ("ModePage", ModePage::decl(config)),
    ]
}

/// Every component schema, by name.
fn components() -> BTreeMap<String, RefOr<Schema>> {
    openapi::components()
}

/// Every component schema as JSON, by name.
fn components_json() -> BTreeMap<String, Value> {
    components()
        .into_iter()
        .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap()))
        .collect()
}

/// A property of a component's schema, as JSON.
fn property<'a>(
    components: &'a BTreeMap<String, Value>,
    component: &str,
    field: &str,
) -> &'a Value {
    &components[component]["properties"][field]
}

/// `text` without `/* … */` comments, with every run of whitespace turned
/// into one space.
fn normalized(text: &str) -> String {
    let mut rest = text;
    let mut kept = String::new();
    while let Some(start) = rest.find("/*") {
        kept.push_str(&rest[..start]);
        let end = rest[start..].find("*/").expect("a comment that never ends");
        rest = &rest[start + end + 2..];
    }
    kept.push_str(rest);
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fails unless the JSON of a schema node has only the keys in `allowed`
/// (and a description, which TypeScript holds as a comment).
fn only(node: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    let extra: Vec<&str> = node
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "description" && !allowed.contains(key))
        .collect();
    if extra.is_empty() {
        Ok(())
    } else {
        Err(format!("the renderer doesn't handle {extra:?} in {node:?}"))
    }
}

/// The TypeScript type ts-rs writes for what `schema` describes, or what the
/// renderer doesn't handle. Each node is checked through its JSON, so a key
/// the renderer doesn't know fails it; children are read from utoipa's own
/// types, whose property map keeps the struct's field order (JSON values
/// would sort it).
fn render(schema: &RefOr<Schema>) -> Result<String, String> {
    let json = serde_json::to_value(schema).map_err(|error| error.to_string())?;
    let node = json
        .as_object()
        .ok_or_else(|| format!("not a schema object: {json}"))?;
    if let Some(reference) = node.get("$ref") {
        only(node, &["$ref"])?;
        let name = reference
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
            .ok_or_else(|| format!("a reference outside the components: {reference}"))?;
        return Ok(name.to_owned());
    }
    match (node.get("type").and_then(Value::as_str), schema) {
        (Some("object"), RefOr::T(Schema::Object(object))) => {
            only(
                node,
                &["type", "properties", "required", "additionalProperties"],
            )?;
            // A request DTO's `deny_unknown_fields`, which TypeScript doesn't
            // express. Any other value (a map's value schema, `true`) fails.
            if let Some(extra) = node.get("additionalProperties")
                && extra != &Value::Bool(false)
            {
                return Err(format!("additionalProperties other than false: {extra}"));
            }
            if !node.contains_key("properties") {
                return Err("an object without properties".to_owned());
            }
            if let Some(stray) = object
                .required
                .iter()
                .find(|name| !object.properties.contains_key(*name))
            {
                return Err(format!("{stray} is required but isn't a property"));
            }
            let mut fields = String::from("{ ");
            for (name, property) in &object.properties {
                let optional = if object.required.contains(name) {
                    ""
                } else {
                    "?"
                };
                write!(fields, "{name}{optional}: {}, ", render(property)?)
                    .map_err(|error| error.to_string())?;
            }
            fields.push('}');
            Ok(fields)
        }
        (Some("array"), RefOr::T(Schema::Array(array))) => {
            only(node, &["type", "items"])?;
            let ArrayItems::RefOrSchema(items) = &array.items else {
                return Err(format!("an array without items: {node:?}"));
            };
            Ok(format!("Array<{}>", render(items)?))
        }
        (Some("string"), _) if node.contains_key("enum") => {
            only(node, &["type", "enum"])?;
            let values = node["enum"]
                .as_array()
                .ok_or("an enum that isn't a list")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(|value| format!("\"{value}\""))
                        .ok_or_else(|| format!("an enum value that isn't a string: {value}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(values.join(" | "))
        }
        (Some("string"), _) => {
            only(node, &["type", "maxLength"])?;
            Ok("string".to_owned())
        }
        // An integer, or an `f32` such as an angle (ADR-0020, S12), with its
        // range: never a `format`, which utoipa's own schemas would add.
        (Some("integer" | "number"), _) => {
            only(node, &["type", "minimum", "maximum"])?;
            Ok("number".to_owned())
        }
        (Some("boolean"), _) => {
            only(node, &["type"])?;
            Ok("boolean".to_owned())
        }
        _ => Err(format!("the renderer doesn't handle {node:?}")),
    }
}

/// The declaration ts-rs writes for the component `name` with `schema`.
fn render_declaration(name: &str, schema: &RefOr<Schema>) -> Result<String, String> {
    Ok(format!("type {name} = {};", render(schema)?))
}

#[test]
fn every_component_renders_as_ts_rs_declares_its_type() {
    let declarations = declarations();

    for (name, schema) in components() {
        let rendered = render_declaration(&name, &schema).unwrap();

        assert_eq!(
            normalized(&rendered),
            normalized(&declarations[&name]),
            "{name}"
        );
    }
}

#[test]
fn the_components_are_exactly_the_exported_api_types() {
    // The API's DTOs only: the desktop app's IPC types (`app`) never cross
    // the HTTP API, so the document leaves them out (ADR-0017).
    let scratch = TempDir::new().unwrap();
    typescript::export_api(scratch.path()).unwrap();
    let exported: BTreeSet<String> = fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            name.strip_suffix(".ts").unwrap().to_owned()
        })
        .collect();

    let named: BTreeSet<String> = components().into_keys().collect();
    let declared: BTreeSet<String> = declarations().into_keys().collect();

    assert_eq!(named, exported);
    assert_eq!(declared, exported);
}

#[test]
fn safe_int_is_a_component_spanning_exactly_the_safe_range() {
    let schema = SafeInt::schema();

    assert_eq!(<SafeInt as ToSchema>::name(), "SafeInt");
    assert_eq!(
        serde_json::to_value(&schema).unwrap(),
        json!({ "type": "integer", "minimum": SafeInt::MIN, "maximum": SafeInt::MAX })
    );
    assert_eq!(
        normalized(&render_declaration("SafeInt", &schema).unwrap()),
        normalized(&SafeInt::decl(&Config::new()))
    );
}

#[rstest]
#[case::error_message("ErrorBody", "message", MESSAGE_MAX_CHARS)]
#[case::request_id("ErrorBody", "request_id", REQUEST_ID_MAX_CHARS)]
#[case::field_path("FieldError", "path", FIELD_PATH_MAX_CHARS)]
#[case::field_message("FieldError", "message", FIELD_MESSAGE_MAX_CHARS)]
#[case::otpauth_uri("MfaEnrollResponse", "otpauth_uri", OTPAUTH_URI_MAX_CHARS)]
#[case::qr_svg("MfaEnrollResponse", "qr_svg", QR_SVG_MAX_CHARS)]
#[case::totp_secret("MfaEnrollResponse", "secret", TOTP_SECRET_MAX_CHARS)]
#[case::confirm_code("MfaConfirmRequest", "code", CODE_MAX_CHARS)]
#[case::setup_token("MfaResetResponse", "setup_token", TOKEN_MAX_CHARS)]
#[case::event_ticket("EventTicketResponse", "ticket", TOKEN_MAX_CHARS)]
#[case::login_username("LoginRequest", "username", USERNAME_MAX_CHARS)]
#[case::login_password("LoginRequest", "password", PASSWORD_MAX_CHARS)]
#[case::login_token("LoginResponse", "token", TOKEN_MAX_CHARS)]
#[case::mfa_token("LoginMfaRequest", "mfa_token", TOKEN_MAX_CHARS)]
#[case::mfa_code("LoginMfaRequest", "code", CODE_MAX_CHARS)]
#[case::access_token("TokenResponse", "access_token", TOKEN_MAX_CHARS)]
#[case::refresh_token("TokenResponse", "refresh_token", TOKEN_MAX_CHARS)]
#[case::refresh_request("RefreshRequest", "refresh_token", TOKEN_MAX_CHARS)]
#[case::step_up_password("StepUpRequest", "password", PASSWORD_MAX_CHARS)]
#[case::label_id("UserLabel", "id", ID_CHARS)]
#[case::label_username("UserLabel", "username", USERNAME_MAX_CHARS)]
#[case::user_id("UserResponse", "id", ID_CHARS)]
#[case::user_username("UserResponse", "username", USERNAME_MAX_CHARS)]
#[case::user_created_at("UserResponse", "created_at", TIMESTAMP_CHARS)]
#[case::user_last_seen_at("UserResponse", "last_seen_at", TIMESTAMP_CHARS)]
#[case::user_page_next("UserPage", "next", ID_CHARS)]
#[case::invite_id("InviteResponse", "id", ID_CHARS)]
#[case::invite_created_at("InviteResponse", "created_at", TIMESTAMP_CHARS)]
#[case::invite_expires_at("InviteResponse", "expires_at", TIMESTAMP_CHARS)]
#[case::invite_used_at("InviteResponse", "used_at", TIMESTAMP_CHARS)]
#[case::invite_revoked_at("InviteResponse", "revoked_at", TIMESTAMP_CHARS)]
#[case::invite_page_next("InvitePage", "next", ID_CHARS)]
#[case::invite_code("CreatedInviteResponse", "code", TOKEN_MAX_CHARS)]
#[case::register_invite("RegisterRequest", "invite", TOKEN_MAX_CHARS)]
#[case::register_username("RegisterRequest", "username", USERNAME_MAX_CHARS)]
#[case::register_password("RegisterRequest", "password", NEW_PASSWORD_MAX_CHARS)]
#[case::registered_id("RegisterResponse", "id", ID_CHARS)]
#[case::registered_username("RegisterResponse", "username", USERNAME_MAX_CHARS)]
#[case::me_id("MeResponse", "id", ID_CHARS)]
#[case::me_username("MeResponse", "username", USERNAME_MAX_CHARS)]
#[case::me_created_at("MeResponse", "created_at", TIMESTAMP_CHARS)]
#[case::me_password_changed_at("MeResponse", "password_changed_at", TIMESTAMP_CHARS)]
#[case::me_mfa_confirmed_at("MeResponse", "mfa_confirmed_at", TIMESTAMP_CHARS)]
#[case::failed_last_at("FailedCodeAttempts", "last_at", TIMESTAMP_CHARS)]
#[case::new_password("PasswordChangeRequest", "new_password", NEW_PASSWORD_MAX_CHARS)]
#[case::session_id("SessionResponse", "id", ID_CHARS)]
#[case::session_created_at("SessionResponse", "created_at", TIMESTAMP_CHARS)]
#[case::session_last_used_at("SessionResponse", "last_used_at", TIMESTAMP_CHARS)]
#[case::session_expires_at("SessionResponse", "expires_at", TIMESTAMP_CHARS)]
#[case::session_page_next("SessionPage", "next", ID_CHARS)]
#[case::link_flow_id("LinkStartedResponse", "flow_id", ID_CHARS)]
#[case::link_user_code("LinkStartedResponse", "user_code", USER_CODE_MAX_CHARS)]
#[case::link_verification_uri(
    "LinkStartedResponse",
    "verification_uri",
    VERIFICATION_URI_MAX_CHARS
)]
#[case::link_started_expires_at("LinkStartedResponse", "expires_at", TIMESTAMP_CHARS)]
#[case::link_expires_at("LinkFlowResponse", "expires_at", TIMESTAMP_CHARS)]
#[case::link_account_id("LinkFlowResponse", "account_id", ID_CHARS)]
#[case::link_name("LinkFlowResponse", "name", MC_NAME_MAX_CHARS)]
#[case::offline_request_name("CreateOfflineAccountRequest", "name", MC_NAME_MAX_CHARS)]
#[case::offline_id("CreatedOfflineAccountResponse", "id", ID_CHARS)]
#[case::offline_name("CreatedOfflineAccountResponse", "name", MC_NAME_MAX_CHARS)]
#[case::account_id("AccountResponse", "id", ID_CHARS)]
#[case::account_name("AccountResponse", "name", MC_NAME_MAX_CHARS)]
#[case::account_linked_at("AccountResponse", "linked_at", TIMESTAMP_CHARS)]
#[case::account_page_next("AccountPage", "next", ID_CHARS)]
#[case::grant_username("CreateGrantRequest", "username", USERNAME_MAX_CHARS)]
#[case::granted_at("GrantResponse", "granted_at", TIMESTAMP_CHARS)]
#[case::grant_page_next("GrantPage", "next", ID_CHARS)]
#[case::enrollment_id("CreatedEnrollmentTokenResponse", "id", ID_CHARS)]
#[case::enrollment_bundle("CreatedEnrollmentTokenResponse", "bundle", BUNDLE_CHARS)]
#[case::enrollment_ca("CreatedEnrollmentTokenResponse", "ca_fingerprint", FINGERPRINT_CHARS)]
#[case::enrollment_expires_at("CreatedEnrollmentTokenResponse", "expires_at", TIMESTAMP_CHARS)]
#[case::agent_id("AgentResponse", "id", ID_CHARS)]
#[case::agent_name("AgentResponse", "name", AGENT_NAME_MAX_CHARS)]
#[case::agent_version("AgentResponse", "version", AGENT_VERSION_MAX_CHARS)]
#[case::agent_connected_since("AgentResponse", "connected_since", TIMESTAMP_CHARS)]
#[case::agent_last_seen_at("AgentResponse", "last_seen_at", TIMESTAMP_CHARS)]
#[case::agent_not_after("AgentResponse", "not_after", TIMESTAMP_CHARS)]
#[case::agent_created_at("AgentResponse", "created_at", TIMESTAMP_CHARS)]
#[case::agent_page_next("AgentPage", "next", ID_CHARS)]
#[case::bot_id("BotResponse", "id", ID_CHARS)]
#[case::bot_server_address("BotResponse", "server_address", SERVER_ADDRESS_MAX_CHARS)]
#[case::bot_mode_id("BotResponse", "mode_id", ID_CHARS)]
#[case::bot_page_next("BotPage", "next", ID_CHARS)]
#[case::bot_agent_id("BotAgent", "id", ID_CHARS)]
#[case::bot_agent_name("BotAgent", "name", AGENT_NAME_MAX_CHARS)]
#[case::bot_since("BotStatus", "since", TIMESTAMP_CHARS)]
#[case::bot_online_since("BotStatus", "online_since", TIMESTAMP_CHARS)]
#[case::bot_agent_last_seen_at("BotStatus", "agent_last_seen_at", TIMESTAMP_CHARS)]
#[case::kick_key("LastDisconnect", "key", KICK_KEY_MAX_CHARS)]
#[case::kick_message("LastDisconnect", "message", KICK_MESSAGE_MAX_CHARS)]
#[case::patch_server_address("BotPatchRequest", "server_address", SERVER_ADDRESS_MAX_CHARS)]
#[case::mode_request_id("BotModeRequest", "mode_id", ID_CHARS)]
#[case::chat_message("ChatSendRequest", "message", CHAT_MESSAGE_MAX_UNITS)]
#[case::command_message("ChatCommandRequest", "message", CHAT_MESSAGE_MAX_UNITS)]
#[case::chat_request_id("ChatSendResponse", "request_id", ID_CHARS)]
#[case::chat_line_at("ChatLine", "at", TIMESTAMP_CHARS)]
#[case::event_bot_id("EventMessage", "bot_id", ID_CHARS)]
#[case::event_at("EventMessage", "at", TIMESTAMP_CHARS)]
#[case::event_request("EventMessage", "request", ID_CHARS)]
#[case::event_mode_id("EventMessage", "mode_id", ID_CHARS)]
#[case::chat_line_text("ChatLine", "text", CHAT_TEXT_MAX_CHARS)]
#[case::chat_sender_name("ChatLineSender", "name", CHAT_SENDER_MAX_CHARS)]
#[case::chat_sender_uuid("ChatLineSender", "uuid", ID_CHARS)]
#[case::mode_request_name("CreateModeRequest", "name", MODE_NAME_MAX_CHARS)]
#[case::mode_update_name("UpdateModeRequest", "name", MODE_NAME_MAX_CHARS)]
#[case::mode_request_message("ModeActionRequest", "message", MODE_MESSAGE_MAX_CHARS)]
#[case::mode_id("ModeResponse", "id", ID_CHARS)]
#[case::mode_name("ModeResponse", "name", MODE_NAME_MAX_CHARS)]
#[case::mode_created_at("ModeResponse", "created_at", TIMESTAMP_CHARS)]
#[case::mode_updated_at("ModeResponse", "updated_at", TIMESTAMP_CHARS)]
#[case::mode_message("ModeAction", "message", MODE_MESSAGE_MAX_CHARS)]
#[case::mode_page_next("ModePage", "next", ID_CHARS)]
#[case::audit_at("AuditEntryResponse", "at", TIMESTAMP_CHARS)]
#[case::audit_actor("AuditEntryResponse", "actor", ID_CHARS)]
#[case::audit_ip("AuditEntryResponse", "ip", IP_MAX_CHARS)]
#[case::audit_target_id("AuditTargetResponse", "id", ID_CHARS)]
#[case::audit_key("AuditDetail", "key", AUDIT_KEY_MAX_CHARS)]
#[case::audit_text("AuditDetail", "text", AUDIT_TEXT_MAX_CHARS)]
fn a_text_field_is_capped_at_its_constant(
    #[case] component: &str,
    #[case] field: &str,
    #[case] max: usize,
) {
    let components = components_json();

    assert_eq!(
        property(&components, component, field),
        &json!({ "type": "string", "maxLength": max })
    );
}

/// Every text field whose cap the cases above check.
const CHECKED_TEXTS: [(&str, &str); 118] = [
    ("ErrorBody", "message"),
    ("ErrorBody", "request_id"),
    ("FieldError", "path"),
    ("FieldError", "message"),
    ("MfaEnrollResponse", "otpauth_uri"),
    ("MfaEnrollResponse", "qr_svg"),
    ("MfaEnrollResponse", "secret"),
    ("MfaConfirmRequest", "code"),
    ("MfaResetResponse", "setup_token"),
    ("EventTicketResponse", "ticket"),
    ("LoginRequest", "username"),
    ("LoginRequest", "password"),
    ("LoginResponse", "token"),
    ("LoginMfaRequest", "mfa_token"),
    ("LoginMfaRequest", "code"),
    ("TokenResponse", "access_token"),
    ("TokenResponse", "refresh_token"),
    ("RefreshRequest", "refresh_token"),
    ("StepUpRequest", "password"),
    ("UserLabel", "id"),
    ("UserLabel", "username"),
    ("UserResponse", "id"),
    ("UserResponse", "username"),
    ("UserResponse", "created_at"),
    ("UserResponse", "last_seen_at"),
    ("UserPage", "next"),
    ("InviteResponse", "id"),
    ("InviteResponse", "created_at"),
    ("InviteResponse", "expires_at"),
    ("InviteResponse", "used_at"),
    ("InviteResponse", "revoked_at"),
    ("InvitePage", "next"),
    ("CreatedInviteResponse", "code"),
    ("RegisterRequest", "invite"),
    ("RegisterRequest", "username"),
    ("RegisterRequest", "password"),
    ("RegisterResponse", "id"),
    ("RegisterResponse", "username"),
    ("MeResponse", "id"),
    ("MeResponse", "username"),
    ("MeResponse", "created_at"),
    ("MeResponse", "password_changed_at"),
    ("MeResponse", "mfa_confirmed_at"),
    ("FailedCodeAttempts", "last_at"),
    ("PasswordChangeRequest", "new_password"),
    ("SessionResponse", "id"),
    ("SessionResponse", "created_at"),
    ("SessionResponse", "last_used_at"),
    ("SessionResponse", "expires_at"),
    ("SessionPage", "next"),
    ("LinkStartedResponse", "flow_id"),
    ("LinkStartedResponse", "user_code"),
    ("LinkStartedResponse", "verification_uri"),
    ("LinkStartedResponse", "expires_at"),
    ("LinkFlowResponse", "expires_at"),
    ("LinkFlowResponse", "account_id"),
    ("LinkFlowResponse", "name"),
    ("CreateOfflineAccountRequest", "name"),
    ("CreatedOfflineAccountResponse", "id"),
    ("CreatedOfflineAccountResponse", "name"),
    ("AccountResponse", "id"),
    ("AccountResponse", "name"),
    ("AccountResponse", "linked_at"),
    ("AccountPage", "next"),
    ("CreateGrantRequest", "username"),
    ("GrantResponse", "granted_at"),
    ("GrantPage", "next"),
    ("CreatedEnrollmentTokenResponse", "id"),
    ("CreatedEnrollmentTokenResponse", "bundle"),
    ("CreatedEnrollmentTokenResponse", "ca_fingerprint"),
    ("CreatedEnrollmentTokenResponse", "expires_at"),
    ("AgentResponse", "id"),
    ("AgentResponse", "name"),
    ("AgentResponse", "version"),
    ("AuditEntryResponse", "at"),
    ("AuditEntryResponse", "actor"),
    ("AuditEntryResponse", "ip"),
    ("AuditTargetResponse", "id"),
    ("AuditDetail", "key"),
    ("AuditDetail", "text"),
    ("AgentResponse", "connected_since"),
    ("AgentResponse", "last_seen_at"),
    ("AgentResponse", "not_after"),
    ("AgentResponse", "created_at"),
    ("AgentPage", "next"),
    ("BotResponse", "id"),
    ("BotResponse", "server_address"),
    ("BotResponse", "mode_id"),
    ("BotPage", "next"),
    ("BotAgent", "id"),
    ("BotAgent", "name"),
    ("BotStatus", "since"),
    ("BotStatus", "online_since"),
    ("BotStatus", "agent_last_seen_at"),
    ("LastDisconnect", "key"),
    ("LastDisconnect", "message"),
    ("BotPatchRequest", "server_address"),
    ("BotModeRequest", "mode_id"),
    ("ChatSendRequest", "message"),
    ("ChatCommandRequest", "message"),
    ("ChatSendResponse", "request_id"),
    ("ChatLine", "at"),
    ("EventMessage", "bot_id"),
    ("EventMessage", "at"),
    ("EventMessage", "request"),
    ("EventMessage", "mode_id"),
    ("ChatLine", "text"),
    ("ChatLineSender", "name"),
    ("ChatLineSender", "uuid"),
    ("CreateModeRequest", "name"),
    ("UpdateModeRequest", "name"),
    ("ModeActionRequest", "message"),
    ("ModeResponse", "id"),
    ("ModeResponse", "name"),
    ("ModeResponse", "created_at"),
    ("ModeResponse", "updated_at"),
    ("ModeAction", "message"),
    ("ModePage", "next"),
];

/// Every text field of every component: the cases above must cover them.
#[test]
fn every_text_field_has_its_cap_checked() {
    let checked = BTreeSet::from(CHECKED_TEXTS);
    let components = components_json();

    let texts: BTreeSet<(&str, &str)> = components
        .iter()
        .filter_map(|(name, schema)| Some((name, schema.get("properties")?.as_object()?)))
        .flat_map(|(name, properties)| {
            properties
                .iter()
                .filter(|(_, property)| {
                    property["type"] == "string" && property.get("enum").is_none()
                })
                .map(move |(field, _)| (name.as_str(), field.as_str()))
        })
        .collect();

    assert_eq!(texts, checked);
}

/// Every request DTO: its schema says unknown fields are refused, as its
/// `deny_unknown_fields` does. A new request DTO must be added here.
#[test]
fn a_request_schema_refuses_unknown_fields_and_responses_dont() {
    let requests = BTreeSet::from([
        "BotModeRequest",
        "BotPatchRequest",
        "ChangeGrantRequest",
        "ChatCommandRequest",
        "ChatSendRequest",
        "CreateGrantRequest",
        "CreateInviteRequest",
        "CreateModeRequest",
        "CreateOfflineAccountRequest",
        "LoginMfaRequest",
        "LoginRequest",
        "MfaConfirmRequest",
        "ModeActionRequest",
        "ModeScheduleRequest",
        "ModeStepRequest",
        "PasswordChangeRequest",
        "RefreshRequest",
        "RegisterRequest",
        "StepUpRequest",
        "UpdateModeRequest",
        "UserPatchRequest",
    ]);
    let components = components_json();

    for (name, schema) in &components {
        let refused = schema.get("additionalProperties") == Some(&json!(false));

        assert_eq!(refused, requests.contains(name.as_str()), "{name}");
    }
}

#[test]
fn the_recovery_codes_are_capped_at_their_constant() {
    let components = components_json();

    assert_eq!(
        property(&components, "MfaConfirmResponse", "recovery_codes"),
        &json!({ "type": "array", "items": { "type": "string", "maxLength": RECOVERY_CODE_MAX_CHARS } })
    );
}

#[test]
fn seconds_left_are_an_integer_with_their_range() {
    let components = components_json();

    assert_eq!(
        property(&components, "MfaResetResponse", "expires_in"),
        &json!({ "type": "integer", "minimum": 0, "maximum": SETUP_TOKEN_MAX_SECS })
    );
}

#[rstest]
#[case::login("LoginResponse", "expires_in", SETUP_TOKEN_MAX_SECS)]
#[case::access("TokenResponse", "access_expires_in", ACCESS_TOKEN_MAX_SECS)]
#[case::refresh("TokenResponse", "refresh_expires_in", REFRESH_TOKEN_MAX_SECS)]
#[case::event_ticket("EventTicketResponse", "expires_in", EVENT_TICKET_MAX_SECS)]
fn every_lifetime_is_an_integer_with_its_range(
    #[case] component: &str,
    #[case] field: &str,
    #[case] max: u32,
) {
    let components = components_json();

    assert_eq!(
        property(&components, component, field),
        &json!({ "type": "integer", "minimum": 0, "maximum": max })
    );
}

#[test]
fn the_login_status_lists_exactly_its_known_values() {
    let components = components_json();
    let known: Vec<&str> = LoginStatus::ALL
        .iter()
        .map(|status| status.as_str())
        .collect();

    assert_eq!(
        property(&components, "LoginResponse", "status")["$ref"],
        "#/components/schemas/LoginStatus"
    );
    assert_eq!(components["LoginStatus"]["enum"], json!(known));
}

#[test]
fn an_open_field_lists_exactly_its_enums_known_values() {
    let components = components_json();
    let known: Vec<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();

    assert_eq!(
        property(&components, "ErrorBody", "code")["$ref"],
        "#/components/schemas/ErrorCode"
    );
    assert_eq!(components["ErrorCode"]["enum"], json!(known));
}

#[rstest]
#[case::last_seen("UserResponse", "last_seen_at")]
#[case::user_page_next("UserPage", "next")]
#[case::used_by("InviteResponse", "used_by")]
#[case::used_at("InviteResponse", "used_at")]
#[case::revoked_at("InviteResponse", "revoked_at")]
#[case::invite_page_next("InvitePage", "next")]
#[case::patch_role("UserPatchRequest", "role")]
#[case::patch_disabled("UserPatchRequest", "disabled")]
#[case::mfa_confirmed_at("MeResponse", "mfa_confirmed_at")]
#[case::failed_last_at("FailedCodeAttempts", "last_at")]
#[case::session_page_next("SessionPage", "next")]
#[case::link_expires_at("LinkFlowResponse", "expires_at")]
#[case::link_reason("LinkFlowResponse", "reason")]
#[case::link_account_id("LinkFlowResponse", "account_id")]
#[case::link_account_status("LinkFlowResponse", "account_status")]
#[case::link_name("LinkFlowResponse", "name")]
#[case::account_reason("AccountResponse", "reason")]
#[case::account_owner("AccountResponse", "owner")]
#[case::account_linked_at("AccountResponse", "linked_at")]
#[case::account_page_next("AccountPage", "next")]
#[case::grant_page_next("GrantPage", "next")]
#[case::bot_server_address("BotResponse", "server_address")]
#[case::bot_agent("BotResponse", "agent")]
#[case::bot_page_next("BotPage", "next")]
#[case::status_state("BotStatus", "state")]
#[case::status_reason("BotStatus", "reason")]
#[case::status_since("BotStatus", "since")]
#[case::status_online_since("BotStatus", "online_since")]
#[case::status_attempt("BotStatus", "attempt")]
#[case::status_restarting("BotStatus", "restarting")]
#[case::status_last_disconnect("BotStatus", "last_disconnect")]
#[case::status_why("BotStatus", "why")]
#[case::status_agent_last_seen_at("BotStatus", "agent_last_seen_at")]
#[case::kick_key("LastDisconnect", "key")]
#[case::kick_message("LastDisconnect", "message")]
#[case::connect_failure("LastDisconnect", "connect_failure")]
#[case::restriction("LastDisconnect", "restriction")]
#[case::session_server_failure("LastDisconnect", "session_server_failure")]
#[case::chat_line_sender("ChatLine", "sender")]
#[case::chat_line_sent_by("ChatLine", "sent_by")]
#[case::chat_sender_uuid("ChatLineSender", "uuid")]
#[case::chat_page_next("ChatPage", "next")]
#[case::mode_request_yaw("ModeActionRequest", "yaw")]
#[case::mode_request_pitch("ModeActionRequest", "pitch")]
#[case::mode_request_max_yaw("ModeActionRequest", "max_yaw")]
#[case::mode_request_max_pitch("ModeActionRequest", "max_pitch")]
#[case::mode_request_on("ModeActionRequest", "on")]
#[case::mode_request_slot("ModeActionRequest", "slot")]
#[case::mode_request_message("ModeActionRequest", "message")]
#[case::mode_request_interval_ms("ModeScheduleRequest", "interval_ms")]
#[case::mode_request_jitter_ms("ModeScheduleRequest", "jitter_ms")]
#[case::mode_yaw("ModeAction", "yaw")]
#[case::mode_pitch("ModeAction", "pitch")]
#[case::mode_max_yaw("ModeAction", "max_yaw")]
#[case::mode_max_pitch("ModeAction", "max_pitch")]
#[case::mode_on("ModeAction", "on")]
#[case::mode_slot("ModeAction", "slot")]
#[case::mode_message("ModeAction", "message")]
#[case::mode_interval_ms("ModeSchedule", "interval_ms")]
#[case::mode_jitter_ms("ModeSchedule", "jitter_ms")]
#[case::mode_owner("ModeResponse", "owner")]
#[case::mode_page_next("ModePage", "next")]
#[case::event_bot_id("EventMessage", "bot_id")]
#[case::event_bot("EventMessage", "bot")]
#[case::event_at("EventMessage", "at")]
#[case::event_state("EventMessage", "state")]
#[case::event_reason("EventMessage", "reason")]
#[case::event_line("EventMessage", "line")]
#[case::event_request("EventMessage", "request")]
#[case::event_failure("EventMessage", "failure")]
#[case::event_mode_id("EventMessage", "mode_id")]
#[case::event_mode("EventMessage", "mode")]
fn every_new_optional_field_is_left_out_never_null(#[case] component: &str, #[case] field: &str) {
    let components = components_json();
    let schema = property(&components, component, field);
    let required = components[component]
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert!(!required.contains(&json!(field)), "{component}.{field}");
    assert!(
        schema.get("oneOf").is_none(),
        "{component}.{field}: {schema}"
    );
    assert_ne!(
        schema["type"],
        json!(["string", "null"]),
        "{component}.{field}"
    );
}

#[rstest]
#[case::user("UserResponse", "role", "UserRole")]
#[case::invite_role("InviteResponse", "role", "UserRole")]
#[case::invite_state("InviteResponse", "state", "InviteState")]
#[case::registered_role("RegisterResponse", "role", "UserRole")]
#[case::me_role("MeResponse", "role", "UserRole")]
#[case::link_status("LinkFlowResponse", "status", "LinkFlowState")]
#[case::link_reason("LinkFlowResponse", "reason", "RefusalReason")]
#[case::link_account_status("LinkFlowResponse", "account_status", "AccountStatus")]
#[case::offline_kind("CreatedOfflineAccountResponse", "kind", "AccountKind")]
#[case::offline_status("CreatedOfflineAccountResponse", "status", "AccountStatus")]
#[case::account_kind("AccountResponse", "kind", "AccountKind")]
#[case::account_status("AccountResponse", "status", "AccountStatus")]
#[case::account_reason("AccountResponse", "reason", "RefusalReason")]
#[case::account_level("AccountResponse", "level", "GrantLevel")]
#[case::grant_level("GrantResponse", "level", "GrantLevel")]
#[case::bot_desired_state("BotResponse", "desired_state", "DesiredState")]
#[case::bot_agent_status("BotAgent", "status", "AgentStatus")]
#[case::status_kind("BotStatus", "kind", "BotStatusKind")]
#[case::status_state("BotStatus", "state", "BotStateKind")]
#[case::status_reason("BotStatus", "reason", "BotStateReason")]
#[case::status_why("BotStatus", "why", "UnplacedReason")]
#[case::disconnect_kind("LastDisconnect", "kind", "DisconnectKind")]
#[case::connect_failure("LastDisconnect", "connect_failure", "ConnectFailure")]
#[case::restriction("LastDisconnect", "restriction", "AccountRestriction")]
#[case::session_server_failure("LastDisconnect", "session_server_failure", "SessionServerFailure")]
#[case::chat_outcome("ChatSendResponse", "outcome", "ChatSendOutcome")]
#[case::chat_direction("ChatLine", "direction", "ChatDirection")]
#[case::chat_kind("ChatLine", "kind", "ChatLineKind")]
#[case::mode_visibility("ModeResponse", "visibility", "ModeVisibility")]
#[case::mode_ownership("ModeResponse", "ownership", "ModeOwnership")]
#[case::mode_action_kind("ModeAction", "type", "ModeActionKind")]
#[case::mode_schedule_kind("ModeSchedule", "type", "ModeScheduleKind")]
#[case::event_kind("EventMessage", "type", "EventMessageKind")]
#[case::event_state("EventMessage", "state", "BotStateKind")]
#[case::event_reason("EventMessage", "reason", "BotStateReason")]
#[case::event_failure("EventMessage", "failure", "ChatFailure")]
fn the_new_open_fields_list_exactly_their_known_values(
    #[case] component: &str,
    #[case] field: &str,
    #[case] enumeration: &str,
) {
    let components = components_json();
    let known: Vec<&str> = match enumeration {
        "UserRole" => UserRole::ALL.iter().map(|role| role.as_str()).collect(),
        "LinkFlowState" => LinkFlowState::ALL
            .iter()
            .map(|state| state.as_str())
            .collect(),
        "RefusalReason" => RefusalReason::ALL
            .iter()
            .map(|reason| reason.as_str())
            .collect(),
        "AccountStatus" => AccountStatus::ALL
            .iter()
            .map(|status| status.as_str())
            .collect(),
        "AccountKind" => AccountKind::ALL.iter().map(|kind| kind.as_str()).collect(),
        "GrantLevel" => GrantLevel::ALL.iter().map(|level| level.as_str()).collect(),
        "DesiredState" => names(DesiredState::ALL),
        "AgentStatus" => names(AgentStatus::ALL),
        "BotStatusKind" => names(BotStatusKind::ALL),
        "BotStateKind" => names(BotStateKind::ALL),
        "BotStateReason" => names(BotStateReason::ALL),
        "UnplacedReason" => names(UnplacedReason::ALL),
        "DisconnectKind" => names(DisconnectKind::ALL),
        "ConnectFailure" => names(ConnectFailure::ALL),
        "AccountRestriction" => names(AccountRestriction::ALL),
        "SessionServerFailure" => names(SessionServerFailure::ALL),
        "ChatSendOutcome" => names(ChatSendOutcome::ALL),
        "ChatDirection" => names(ChatDirection::ALL),
        "ChatLineKind" => names(ChatLineKind::ALL),
        "ModeVisibility" => names(ModeVisibility::ALL),
        "ModeOwnership" => names(ModeOwnership::ALL),
        "ModeActionKind" => names(ModeActionKind::ALL),
        "ModeScheduleKind" => names(ModeScheduleKind::ALL),
        "EventMessageKind" => names(EventMessageKind::ALL),
        "ChatFailure" => names(ChatFailure::ALL),
        _ => InviteState::ALL
            .iter()
            .map(|state| state.as_str())
            .collect(),
    };

    assert_eq!(
        property(&components, component, field)["$ref"],
        format!("#/components/schemas/{enumeration}")
    );
    assert_eq!(components[enumeration]["enum"], json!(known));
}

/// The names of an open enum's known values.
fn names<E: OpenEnum>(all: &[E]) -> Vec<&'static str> {
    all.iter().map(|value| value.as_str()).collect()
}

#[test]
fn an_optional_field_is_left_out_never_null() {
    let components = components_json();
    let fields = property(&components, "ErrorBody", "fields");

    assert_eq!(fields["type"], "array");
    assert!(
        !components["ErrorBody"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("fields"))
    );
}

/// A DTO with every building block, so they render the same in both
/// exports even before a real DTO uses one.
#[derive(Serialize, Deserialize, TS, ToSchema)]
struct Probe {
    /// A 64-bit integer.
    count: SafeInt,
    /// Capped text.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<12>::schema)]
    label: BoundedText<12>,
    /// An open enum.
    #[ts(as = "ErrorCode")]
    #[schema(value_type = ErrorCode)]
    code: Open<ErrorCode>,
    /// An optional list of another DTO.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    errors: Option<Vec<FieldError>>,
    /// A nested DTO.
    nested: ErrorResponse,
    /// A yes or no.
    flag: bool,
    /// A point in time.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    at: Timestamp,
    /// An optional point in time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    maybe_at: Option<Timestamp>,
    /// A number with its range, such as an angle.
    #[schema(schema_with = number_schema::<-90, 90>)]
    angle: f32,
}

#[test]
fn every_building_block_renders_as_ts_rs_declares_it() {
    let rendered = render_declaration("Probe", &Probe::schema()).unwrap();

    assert_eq!(
        normalized(&rendered),
        normalized(&Probe::decl(&Config::new()))
    );
}

#[rstest]
#[case::one_of(json!({ "oneOf": [{ "type": "string" }, { "type": "integer" }] }))]
#[case::all_of(json!({ "allOf": [{ "type": "string" }] }))]
#[case::nullable(json!({ "type": ["string", "null"] }))]
#[case::number_with_a_format(json!({ "type": "number", "format": "float" }))]
#[case::string_format(json!({ "type": "string", "format": "date-time" }))]
#[case::string_pattern(json!({ "type": "string", "pattern": "^a$" }))]
#[case::string_min_length(json!({ "type": "string", "minLength": 1 }))]
#[case::integer_format(json!({ "type": "integer", "format": "int64" }))]
#[case::map(json!({ "type": "object", "additionalProperties": { "type": "string" } }))]
#[case::open_object(json!({ "type": "object", "additionalProperties": true, "properties": { "a": { "type": "string" } } }))]
#[case::object_without_properties(json!({ "type": "object" }))]
#[case::stray_required(json!({ "type": "object", "required": ["a"], "properties": { "b": { "type": "string" } } }))]
#[case::reference_elsewhere(json!({ "$ref": "https://example.com/schema.json" }))]
#[case::boolean_with_a_format(json!({ "type": "boolean", "format": "flag" }))]
#[case::nested_unknown(json!({ "type": "array", "items": { "type": "number", "format": "double" } }))]
#[case::property_unknown(json!({ "type": "object", "properties": { "a": { "type": "number", "format": "double" } } }))]
fn the_renderer_fails_on_a_construct_it_doesnt_handle(#[case] json: Value) {
    let schema: RefOr<Schema> = serde_json::from_value(json.clone()).unwrap();
    // The case must be something utoipa can represent, or reading it would
    // have dropped the construct under test.
    assert_eq!(serde_json::to_value(&schema).unwrap(), json);

    let rendered = render(&schema);

    assert!(rendered.is_err(), "{rendered:?}");
}

#[test]
fn a_boolean_renders_as_boolean() {
    let schema: RefOr<Schema> = serde_json::from_value(json!({ "type": "boolean" })).unwrap();

    assert_eq!(render(&schema).unwrap(), "boolean");
}
