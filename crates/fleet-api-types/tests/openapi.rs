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

use fleet_api_types::auth::{
    ACCESS_TOKEN_MAX_SECS, LoginMfaRequest, LoginRequest, LoginResponse, LoginStatus,
    NEW_PASSWORD_MAX_CHARS, PASSWORD_MAX_CHARS, REFRESH_TOKEN_MAX_SECS, RefreshRequest,
    RegisterRequest, RegisterResponse, StepUpRequest, TokenResponse, USERNAME_MAX_CHARS,
};
use fleet_api_types::error::{
    FIELD_MESSAGE_MAX_CHARS, FIELD_PATH_MAX_CHARS, MESSAGE_MAX_CHARS, REQUEST_ID_MAX_CHARS,
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
    .map(|(name, declaration)| (name.to_owned(), declaration))
    .collect()
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
        (Some("integer"), _) => {
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

/// Every text field of every component: the cases above must cover them.
#[test]
fn every_text_field_has_its_cap_checked() {
    let checked = BTreeSet::from([
        ("ErrorBody", "message"),
        ("ErrorBody", "request_id"),
        ("FieldError", "path"),
        ("FieldError", "message"),
        ("MfaEnrollResponse", "otpauth_uri"),
        ("MfaEnrollResponse", "qr_svg"),
        ("MfaEnrollResponse", "secret"),
        ("MfaConfirmRequest", "code"),
        ("MfaResetResponse", "setup_token"),
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
    ]);
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
        "CreateInviteRequest",
        "LoginMfaRequest",
        "LoginRequest",
        "MfaConfirmRequest",
        "PasswordChangeRequest",
        "RefreshRequest",
        "RegisterRequest",
        "StepUpRequest",
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
fn the_new_open_fields_list_exactly_their_known_values(
    #[case] component: &str,
    #[case] field: &str,
    #[case] enumeration: &str,
) {
    let components = components_json();
    let known: Vec<&str> = match enumeration {
        "UserRole" => UserRole::ALL.iter().map(|role| role.as_str()).collect(),
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
#[case::number(json!({ "type": "number" }))]
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
#[case::nested_unknown(json!({ "type": "array", "items": { "type": "number" } }))]
#[case::property_unknown(json!({ "type": "object", "properties": { "a": { "type": "number" } } }))]
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
