//! IDs in DTOs (Plan.md P7.6, P7.9, P9.4, P10.3, P10.10, P11.3, ADR-0016):
//! fleet-core's typed IDs (`UserId`, `InviteId`, `SessionId`, `AccountId`,
//! `LinkFlowId`, `EnrollmentTokenId`, `AgentId`, `BotId`, `ModeId`,
//! `ChatRequestId`), each
//! a version 7 UUID that
//! serializes as its lowercase hyphenated text and reads back only as a
//! version 7 UUID.
//!
//! A field holding one carries `#[ts(type = "string")]` (an optional one
//! `#[ts(as = "Option<String>", optional)]`) and `#[schema(schema_with =
//! id_schema)]`, so the TypeScript and the `OpenAPI` schema both say
//! string.

use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};

/// The IDs the DTOs carry, re-exported so `fleet-client` and the app name
/// them through this crate, which is the only one Plan.md §4 lets them
/// depend on (ADR-0017). A DTO that carries another core type re-exports
/// it too.
pub use fleet_core::id::{
    AccountId, AgentId, BotId, ChatRequestId, EnrollmentTokenId, InviteId, LinkFlowId, ModeId,
    SessionId, UserId,
};

/// The characters of an ID's text.
pub const ID_CHARS: usize = 36;

/// The schema of an ID: a string of [`ID_CHARS`] characters (the renderer
/// that keeps it equal to the TypeScript knows no minimum, so it says at
/// most).
#[must_use]
pub fn id_schema() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .max_length(Some(ID_CHARS))
        .into()
}

#[cfg(test)]
mod tests {
    use fleet_core::id::UserId;

    use super::*;

    #[test]
    fn an_id_is_a_string_of_its_length() {
        let schema = serde_json::to_value(id_schema()).unwrap();
        let id = UserId::new_v7(
            chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
            [1; 10],
        )
        .unwrap();

        assert_eq!(
            schema,
            serde_json::json!({ "type": "string", "maxLength": 36 })
        );
        assert_eq!(id.to_string().chars().count(), ID_CHARS);
    }
}
