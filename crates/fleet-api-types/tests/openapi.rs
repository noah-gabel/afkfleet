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

use fleet_api_types::error::{
    FIELD_MESSAGE_MAX_CHARS, FIELD_PATH_MAX_CHARS, MESSAGE_MAX_CHARS, REQUEST_ID_MAX_CHARS,
};
use fleet_api_types::mfa::{
    CODE_MAX_CHARS, MfaConfirmRequest, MfaConfirmResponse, MfaEnrollResponse, MfaResetResponse,
    OTPAUTH_URI_MAX_CHARS, QR_SVG_MAX_CHARS, RECOVERY_CODE_MAX_CHARS, SETUP_TOKEN_MAX_SECS,
    TOKEN_MAX_CHARS, TOTP_SECRET_MAX_CHARS,
};
use fleet_api_types::{
    BoundedText, ErrorBody, ErrorCode, ErrorResponse, FieldError, Open, OpenEnum, SafeInt, openapi,
    typescript,
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
        ("ErrorBody", ErrorBody::decl(&config)),
        ("ErrorCode", ErrorCode::decl(&config)),
        ("ErrorResponse", ErrorResponse::decl(&config)),
        ("FieldError", FieldError::decl(&config)),
        ("MfaConfirmRequest", MfaConfirmRequest::decl(&config)),
        ("MfaConfirmResponse", MfaConfirmResponse::decl(&config)),
        ("MfaEnrollResponse", MfaEnrollResponse::decl(&config)),
        ("MfaResetResponse", MfaResetResponse::decl(&config)),
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
fn the_components_are_exactly_the_exported_types() {
    let scratch = TempDir::new().unwrap();
    typescript::export(scratch.path()).unwrap();
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
    let requests = BTreeSet::from(["MfaConfirmRequest"]);
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
#[case::boolean(json!({ "type": "boolean" }))]
#[case::string_format(json!({ "type": "string", "format": "date-time" }))]
#[case::string_pattern(json!({ "type": "string", "pattern": "^a$" }))]
#[case::string_min_length(json!({ "type": "string", "minLength": 1 }))]
#[case::integer_format(json!({ "type": "integer", "format": "int64" }))]
#[case::map(json!({ "type": "object", "additionalProperties": { "type": "string" } }))]
#[case::open_object(json!({ "type": "object", "additionalProperties": true, "properties": { "a": { "type": "string" } } }))]
#[case::object_without_properties(json!({ "type": "object" }))]
#[case::stray_required(json!({ "type": "object", "required": ["a"], "properties": { "b": { "type": "string" } } }))]
#[case::reference_elsewhere(json!({ "$ref": "https://example.com/schema.json" }))]
#[case::nested_unknown(json!({ "type": "array", "items": { "type": "boolean" } }))]
#[case::property_unknown(json!({ "type": "object", "properties": { "a": { "type": "boolean" } } }))]
fn the_renderer_fails_on_a_construct_it_doesnt_handle(#[case] json: Value) {
    let schema: RefOr<Schema> = serde_json::from_value(json.clone()).unwrap();
    // The case must be something utoipa can represent, or reading it would
    // have dropped the construct under test.
    assert_eq!(serde_json::to_value(&schema).unwrap(), json);

    let rendered = render(&schema);

    assert!(rendered.is_err(), "{rendered:?}");
}
