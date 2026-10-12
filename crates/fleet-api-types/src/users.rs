//! The users' DTOs (Plan.md P7.9, ADR-0010, ADR-0016): the list Admins see
//! (`GET /users`) and the one change they make (`PATCH /users/{id}`).
//!
//! Only the Owner changes roles, between Member and Admin; Admins disable
//! and enable Members. Each request changes one thing: a role or `disabled`.

use fleet_core::authz::Role;
use fleet_core::id::UserId;
use garde::Validate;
use garde::error::{Error, Path, Report};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::auth::USERNAME_MAX_CHARS;
use crate::ids::id_schema;
use crate::open::{Open, OpenEnum};
use crate::text::BoundedText;
use crate::time::Timestamp;

/// A user's global role.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    /// Manages their own accounts and the ones they were granted.
    Member,
    /// Manages Members, invites, agents and every account.
    Admin,
    /// The one user who can do everything.
    Owner,
}

impl OpenEnum for UserRole {
    const ALL: &'static [Self] = &[Self::Member, Self::Admin, Self::Owner];

    fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

impl From<Role> for UserRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Member => Self::Member,
            Role::Admin => Self::Admin,
            Role::Owner => Self::Owner,
        }
    }
}

impl From<UserRole> for Role {
    fn from(role: UserRole) -> Self {
        match role {
            UserRole::Member => Self::Member,
            UserRole::Admin => Self::Admin,
            UserRole::Owner => Self::Owner,
        }
    }
}

/// A user named next to something else, such as an invite's creator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct UserLabel {
    /// The user's ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: UserId,
    /// The username, lowercase.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<USERNAME_MAX_CHARS>::schema)]
    pub username: BoundedText<USERNAME_MAX_CHARS>,
}

/// A user as `GET /users` lists them, and as `PATCH /users/{id}` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct UserResponse {
    /// The ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: UserId,
    /// The username, lowercase.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<USERNAME_MAX_CHARS>::schema)]
    pub username: BoundedText<USERNAME_MAX_CHARS>,
    /// The global role.
    #[ts(as = "UserRole")]
    #[schema(value_type = UserRole)]
    pub role: Open<UserRole>,
    /// Whether the user is disabled: they can't log in, and every request
    /// of theirs is refused.
    pub disabled: bool,
    /// When the user was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// Whether the user has confirmed an authenticator. An invitee who never
    /// finished the 2FA setup has none, and whoever first enrolls with their
    /// password owns the account: disable such an account.
    pub mfa_enrolled: bool,
    /// The last time any of their sessions was used, if they have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub last_seen_at: Option<Timestamp>,
}

/// `GET /users`: one page of users, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct UserPage {
    /// The users.
    pub items: Vec<UserResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<UserId>,
}

/// `PATCH /users/{id}`: exactly one change, a role or `disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserPatchRequest {
    /// The new role: `member` or `admin`, and only the Owner changes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub role: Option<UserRole>,
    /// Whether the user is disabled. Changing it, or the role, ends every
    /// session of the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub disabled: Option<bool>,
}

/// The message of a patch that doesn't hold exactly one change.
pub const ONE_CHANGE: &str = "must hold exactly one of `role` and `disabled`";

/// garde has no rule across fields, so the one rule, exactly one change,
/// is checked here, reported on the request as a whole.
impl Validate for UserPatchRequest {
    type Context = ();

    fn validate_into(&self, _context: &(), parent: &mut dyn FnMut() -> Path, report: &mut Report) {
        if self.role.is_some() == self.disabled.is_some() {
            report.append(parent(), Error::new(ONE_CHANGE));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn patch(json: serde_json::Value) -> Result<UserPatchRequest, serde_json::Error> {
        serde_json::from_value(json)
    }

    fn problems(request: UserPatchRequest) -> Vec<(String, String)> {
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    #[test]
    fn a_patch_with_one_change_is_valid() {
        for body in [
            json!({ "role": "admin" }),
            json!({ "role": "member" }),
            json!({ "disabled": true }),
            json!({ "disabled": false }),
        ] {
            let request = patch(body.clone()).unwrap();

            assert_eq!(problems(request), [], "{body}");
        }
    }

    #[test]
    fn a_patch_with_none_or_both_breaks_its_rule() {
        for body in [json!({}), json!({ "role": "admin", "disabled": true })] {
            let request = patch(body.clone()).unwrap();

            assert_eq!(
                problems(request),
                [(String::new(), ONE_CHANGE.to_owned())],
                "{body}"
            );
        }
    }

    #[test]
    fn an_owner_role_reads_and_is_left_to_authorization() {
        let request = patch(json!({ "role": "owner" })).unwrap();

        assert_eq!(request.role, Some(UserRole::Owner));
        assert_eq!(problems(request), []);
    }

    #[test]
    fn a_patch_refuses_unknown_and_mistyped_fields_and_unknown_roles() {
        assert!(patch(json!({ "disabled": true, "extra": 1 })).is_err());
        assert!(patch(json!({ "disabled": "yes" })).is_err());
        assert!(patch(json!({ "role": "superuser" })).is_err());
        assert!(patch(json!({ "role": "Admin" })).is_err());
        assert!(patch(json!({ "role": null, "disabled": null })).is_ok());
    }

    #[test]
    fn every_role_names_itself_as_serde_does() {
        for role in UserRole::ALL {
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                json!(role.as_str()),
                "{role:?}"
            );
        }
    }

    #[test]
    fn the_roles_map_to_and_from_fleet_cores() {
        for role in [Role::Member, Role::Admin, Role::Owner] {
            assert_eq!(Role::from(UserRole::from(role)), role);
            assert_eq!(UserRole::from(role).as_str(), role.as_str());
        }
    }

    #[test]
    fn a_role_a_newer_server_sends_still_reads() {
        let user: UserResponse = serde_json::from_value(json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "username": "alice",
            "role": "auditor",
            "disabled": false,
            "created_at": "2027-01-15T08:00:00.000Z",
            "mfa_enrolled": true
        }))
        .unwrap();

        assert_eq!(user.role.known(), None);
        assert_eq!(user.role.as_str(), "auditor");
    }

    #[test]
    fn the_responses_round_trip_and_leave_out_what_isnt_there() {
        let full = json!({
            "items": [{
                "id": "018bcfe5-6800-7101-8101-010101010101",
                "username": "alice",
                "role": "member",
                "disabled": true,
                "created_at": "2027-01-15T08:00:00.000Z",
                "mfa_enrolled": false,
                "last_seen_at": "2027-01-16T08:00:00.000Z"
            }],
            "next": "018bcfe5-6800-7101-8101-010101010101"
        });
        let bare = json!({
            "items": [{
                "id": "018bcfe5-6800-7101-8101-010101010101",
                "username": "alice",
                "role": "owner",
                "disabled": false,
                "created_at": "2027-01-15T08:00:00.000Z",
                "mfa_enrolled": true
            }]
        });

        for page in [full, bare] {
            let back: UserPage = serde_json::from_value(page.clone()).unwrap();

            assert_eq!(serde_json::to_value(back).unwrap(), page);
        }
    }

    #[test]
    fn a_response_ignores_a_field_a_newer_server_adds() {
        let page: UserPage = serde_json::from_value(json!({ "items": [], "new": 1 })).unwrap();

        assert_eq!(page.items, []);
    }

    #[test]
    fn a_response_with_an_id_that_isnt_v7_is_refused() {
        let page = serde_json::from_value::<UserPage>(json!({
            "items": [],
            "next": "550e8400-e29b-41d4-a716-446655440000"
        }));

        assert!(page.is_err());
    }
}
