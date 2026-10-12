//! The invites' DTOs (Plan.md P7.9, P7.1, §7.2, ADR-0016): creating one,
//! listing them, and the invite as both show it.
//!
//! An invite registers one user with a fixed role and expires. Its code is
//! in [`CreatedInviteResponse`] only: the server keeps just its hash, so it's
//! never shown again. Admins create and revoke Member invites; Admin
//! invites are the Owner's; an Owner invite can't exist.

use fleet_core::auth;
use fleet_core::id::InviteId;
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::id_schema;
use crate::mfa::TOKEN_MAX_CHARS;
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;
use crate::time::Timestamp;
use crate::users::{UserLabel, UserRole};

/// What an invite is now.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InviteState {
    /// It can still register a user.
    Pending,
    /// It registered a user.
    Used,
    /// It was revoked before it was used.
    Revoked,
    /// It wasn't used before it expired.
    Expired,
}

impl OpenEnum for InviteState {
    const ALL: &'static [Self] = &[Self::Pending, Self::Used, Self::Revoked, Self::Expired];

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Used => "used",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

impl From<auth::InviteState> for InviteState {
    fn from(state: auth::InviteState) -> Self {
        match state {
            auth::InviteState::Pending => Self::Pending,
            auth::InviteState::Used => Self::Used,
            auth::InviteState::Revoked => Self::Revoked,
            auth::InviteState::Expired => Self::Expired,
        }
    }
}

/// An invite as `GET /invites` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct InviteResponse {
    /// The ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: InviteId,
    /// The role the new user gets.
    #[ts(as = "UserRole")]
    #[schema(value_type = UserRole)]
    pub role: Open<UserRole>,
    /// What it is now.
    #[ts(as = "InviteState")]
    #[schema(value_type = InviteState)]
    pub state: Open<InviteState>,
    /// Who created it.
    pub created_by: UserLabel,
    /// When it was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// When it expires, or expired.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub expires_at: Timestamp,
    /// The user it registered, once used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub used_by: Option<UserLabel>,
    /// When it registered that user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub used_at: Option<Timestamp>,
    /// When it was revoked, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub revoked_at: Option<Timestamp>,
}

/// `GET /invites`: one page of invites, newest first, in every state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct InvitePage {
    /// The invites.
    pub items: Vec<InviteResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<InviteId>,
}

/// `POST /invites`: a new invite for `role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInviteRequest {
    /// The role the new user gets: `member` (Admins and the Owner) or
    /// `admin` (the Owner only).
    #[garde(skip)]
    pub role: UserRole,
}

/// The new invite, with its code: shown this once.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct CreatedInviteResponse {
    /// The code (`afk_iv_…`) for the invitee's `POST /auth/register`.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<TOKEN_MAX_CHARS>::schema)]
    pub code: SecretText<TOKEN_MAX_CHARS>,
    /// The invite as `GET /invites` lists it.
    pub invite: InviteResponse,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn invite_json() -> serde_json::Value {
        json!({
            "id": "018bcfe5-6800-7707-8707-070707070707",
            "role": "member",
            "state": "used",
            "created_by": { "id": "018bcfe5-6800-7101-8101-010101010101", "username": "owner" },
            "created_at": "2027-01-15T08:00:00.000Z",
            "expires_at": "2027-01-18T08:00:00.000Z",
            "used_by": { "id": "018bcfe5-6800-7202-8202-020202020202", "username": "alice" },
            "used_at": "2027-01-16T08:00:00.000Z"
        })
    }

    #[test]
    fn a_create_request_takes_a_role() {
        let request: CreateInviteRequest =
            serde_json::from_value(json!({ "role": "admin" })).unwrap();

        assert_eq!(request.role, UserRole::Admin);
        assert!(request.validate().is_ok());
    }

    #[test]
    fn a_create_request_refuses_unknown_missing_and_mistyped_fields() {
        let read = |body| serde_json::from_value::<CreateInviteRequest>(body);

        assert!(read(json!({ "role": "member", "ttl": 1 })).is_err());
        assert!(read(json!({})).is_err());
        assert!(read(json!({ "role": 1 })).is_err());
        assert!(read(json!({ "role": "guest" })).is_err());
    }

    #[test]
    fn every_state_names_itself_as_serde_does_and_as_fleet_core_does() {
        for state in auth::InviteState::ALL {
            let api = InviteState::from(state);

            assert_eq!(serde_json::to_value(api).unwrap(), json!(state.as_str()));
            assert_eq!(api.as_str(), state.as_str());
        }
    }

    #[test]
    fn the_responses_round_trip_and_leave_out_what_isnt_there() {
        let pending = json!({
            "id": "018bcfe5-6800-7707-8707-070707070707",
            "role": "admin",
            "state": "pending",
            "created_by": { "id": "018bcfe5-6800-7101-8101-010101010101", "username": "owner" },
            "created_at": "2027-01-15T08:00:00.000Z",
            "expires_at": "2027-01-18T08:00:00.000Z"
        });
        let page = json!({
            "items": [invite_json(), pending.clone()],
            "next": "018bcfe5-6800-7707-8707-070707070707"
        });
        let created = json!({ "code": "afk_iv_x", "invite": pending });

        let page_back: InvitePage = serde_json::from_value(page.clone()).unwrap();
        let created_back: CreatedInviteResponse = serde_json::from_value(created.clone()).unwrap();

        assert_eq!(serde_json::to_value(page_back).unwrap(), page);
        assert_eq!(serde_json::to_value(created_back).unwrap(), created);
    }

    #[test]
    fn a_state_a_newer_server_sends_still_reads() {
        let mut invite = invite_json();
        invite["state"] = json!("archived");

        let read: InviteResponse = serde_json::from_value(invite).unwrap();

        assert_eq!(read.state.known(), None);
        assert_eq!(read.state.as_str(), "archived");
    }

    #[test]
    fn no_dto_shows_a_secret_in_debug() {
        let created: CreatedInviteResponse = serde_json::from_value(json!({
            "code": "afk_iv_CODECODECODE",
            "invite": invite_json()
        }))
        .unwrap();

        let shown = format!("{created:?}");

        assert!(!shown.contains("CODECODE"), "{shown}");
    }
}
