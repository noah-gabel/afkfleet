//! The user's sessions (Plan.md P7.6, ADR-0016): `GET /me/sessions` lists
//! the live ones, and `DELETE /me/sessions/{id}` ends one of the others.
//!
//! A session stores no IP address or user agent, so a row is told apart by
//! its times. The current session isn't ended here: `POST /auth/logout`
//! does that.

use fleet_core::id::SessionId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::id_schema;
use crate::time::Timestamp;

/// A live session: not ended, and its refresh token not expired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct SessionResponse {
    /// The ID: one login's, the same across every refresh.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: SessionId,
    /// When its login happened.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// The last time it was used, to the minute.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub last_used_at: Timestamp,
    /// When its refresh token expires unless it's used first.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub expires_at: Timestamp,
    /// Whether it's the session of this request.
    pub current: bool,
}

/// `GET /me/sessions`: one page of live sessions, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct SessionPage {
    /// The sessions.
    pub items: Vec<SessionResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<SessionId>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_page_round_trips_and_leaves_out_its_end() {
        let session = json!({
            "id": "018bcfe5-6800-7606-8606-060606060606",
            "created_at": "2027-01-15T08:00:00.000Z",
            "last_used_at": "2027-01-15T09:00:00.000Z",
            "expires_at": "2027-01-22T08:00:00.000Z",
            "current": true
        });
        let last = json!({ "items": [session.clone()] });
        let more = json!({ "items": [session], "next": "018bcfe5-6800-7606-8606-060606060606" });

        for page in [last, more] {
            let back: SessionPage = serde_json::from_value(page.clone()).unwrap();

            assert_eq!(serde_json::to_value(back).unwrap(), page);
        }
    }
}
