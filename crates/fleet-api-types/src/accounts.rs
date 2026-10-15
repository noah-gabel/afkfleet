//! The linked Minecraft accounts' DTOs (Plan.md P9.4, P9.7, ADR-0018):
//! starting a Microsoft link flow, reading where it stands, and creating an
//! offline account for development.
//!
//! A flow's code is in [`LinkStartedResponse`] only, its creator's answer:
//! the user types it at Microsoft's page, and nothing else ever shows it, so
//! [`LinkFlowResponse`] can be polled like any other read. A flow and its
//! result are readable by their creator alone, for 15 minutes after they
//! end.

use fleet_core::account;
use fleet_core::id::{AccountId, LinkFlowId};
use fleet_core::value::McUsername;
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::id_schema;
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The most characters of a Minecraft name: 16 (`McUsername`).
pub const MC_NAME_MAX_CHARS: usize = 16;
/// The most characters of a link flow's user code: 16 (the port's rule).
pub const USER_CODE_MAX_CHARS: usize = 16;
/// The most characters of the verification link (the port's rule).
pub const VERIFICATION_URI_MAX_CHARS: usize = 256;

/// Where a link flow stands.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkFlowState {
    /// Waiting for the user at Microsoft's page, until `expires_at`.
    Waiting,
    /// Microsoft approved it: the server checks the game and the profile,
    /// and stores the account.
    Finishing,
    /// The user declined at Microsoft's page.
    Declined,
    /// The code expired before the user approved it.
    Expired,
    /// The Minecraft account is linked already, by someone.
    Taken,
    /// The user owns their quota of accounts already.
    OverQuota,
    /// Minecraft's side refused the account; `reason` says why.
    Refused,
    /// Microsoft or the server failed, or the user's session ended: start
    /// again.
    Failed,
    /// The account is linked: `account_id`, `account_status` and `name`.
    Linked,
}

impl OpenEnum for LinkFlowState {
    const ALL: &'static [Self] = &[
        Self::Waiting,
        Self::Finishing,
        Self::Declined,
        Self::Expired,
        Self::Taken,
        Self::OverQuota,
        Self::Refused,
        Self::Failed,
        Self::Linked,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Finishing => "finishing",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Taken => "taken",
            Self::OverQuota => "over_quota",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Linked => "linked",
        }
    }
}

/// How an account signs in to Minecraft.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// Through Microsoft.
    Microsoft,
    /// Without any sign-in, for offline-mode servers in development only.
    Offline,
}

impl OpenEnum for AccountKind {
    const ALL: &'static [Self] = &[Self::Microsoft, Self::Offline];

    fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Offline => "offline",
        }
    }
}

impl From<account::AccountKind> for AccountKind {
    fn from(kind: account::AccountKind) -> Self {
        match kind {
            account::AccountKind::Microsoft => Self::Microsoft,
            account::AccountKind::Offline => Self::Offline,
        }
    }
}

/// Where a linked account stands.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// Waiting for someone above the linker to approve it; no bot can use
    /// it yet.
    Pending,
    /// Usable.
    Active,
    /// Microsoft refused its refresh token: it must be linked again.
    ReauthRequired,
    /// Minecraft's side refused it: it must be linked again.
    Refused,
}

impl OpenEnum for AccountStatus {
    const ALL: &'static [Self] = &[
        Self::Pending,
        Self::Active,
        Self::ReauthRequired,
        Self::Refused,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::ReauthRequired => "reauth_required",
            Self::Refused => "refused",
        }
    }
}

impl From<account::AccountStatus> for AccountStatus {
    fn from(status: account::AccountStatus) -> Self {
        match status {
            account::AccountStatus::Pending => Self::Pending,
            account::AccountStatus::Active => Self::Active,
            account::AccountStatus::ReauthRequired => Self::ReauthRequired,
            account::AccountStatus::Refused(_) => Self::Refused,
        }
    }
}

/// Why Minecraft's side refused an account.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The account doesn't own Minecraft.
    NotOwned,
    /// The account has no Minecraft Java profile.
    NoProfile,
    /// Xbox Live banned the account.
    XboxBanned,
    /// The Microsoft account has no Xbox profile.
    XboxNoXboxProfile,
    /// Xbox Live isn't available in the account's country.
    XboxCountryUnavailable,
    /// The account needs adult verification.
    XboxAdultVerification,
    /// A child's account that an adult must add to a Microsoft family.
    XboxChildAccount,
    /// Another Xbox Live refusal.
    XboxOther,
    /// The account's refresh token now leads to another Minecraft profile.
    ProfileMismatch,
}

impl OpenEnum for RefusalReason {
    const ALL: &'static [Self] = &[
        Self::NotOwned,
        Self::NoProfile,
        Self::XboxBanned,
        Self::XboxNoXboxProfile,
        Self::XboxCountryUnavailable,
        Self::XboxAdultVerification,
        Self::XboxChildAccount,
        Self::XboxOther,
        Self::ProfileMismatch,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::NotOwned => "not_owned",
            Self::NoProfile => "no_profile",
            Self::XboxBanned => "xbox_banned",
            Self::XboxNoXboxProfile => "xbox_no_xbox_profile",
            Self::XboxCountryUnavailable => "xbox_country_unavailable",
            Self::XboxAdultVerification => "xbox_adult_verification",
            Self::XboxChildAccount => "xbox_child_account",
            Self::XboxOther => "xbox_other",
            Self::ProfileMismatch => "profile_mismatch",
        }
    }
}

impl From<account::RefusalReason> for RefusalReason {
    fn from(reason: account::RefusalReason) -> Self {
        match reason {
            account::RefusalReason::NotOwned => Self::NotOwned,
            account::RefusalReason::NoProfile => Self::NoProfile,
            account::RefusalReason::XboxBanned => Self::XboxBanned,
            account::RefusalReason::XboxNoXboxProfile => Self::XboxNoXboxProfile,
            account::RefusalReason::XboxCountryUnavailable => Self::XboxCountryUnavailable,
            account::RefusalReason::XboxAdultVerification => Self::XboxAdultVerification,
            account::RefusalReason::XboxChildAccount => Self::XboxChildAccount,
            account::RefusalReason::XboxOther => Self::XboxOther,
            account::RefusalReason::ProfileMismatch => Self::ProfileMismatch,
        }
    }
}

/// `POST /accounts/link`: a started flow, for its creator only.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct LinkStartedResponse {
    /// The flow, for `GET /accounts/link/{flow_id}`.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub flow_id: LinkFlowId,
    /// The code the user types at Microsoft's page: shown this once. Only
    /// a code the user started themselves may be entered (ADR-0018).
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<USER_CODE_MAX_CHARS>::schema)]
    pub user_code: SecretText<USER_CODE_MAX_CHARS>,
    /// Microsoft's page, where the user types the code.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<VERIFICATION_URI_MAX_CHARS>::schema)]
    pub verification_uri: BoundedText<VERIFICATION_URI_MAX_CHARS>,
    /// When the code expires.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub expires_at: Timestamp,
}

/// `GET /accounts/link/{flow_id}`: where the flow stands. Each field but
/// `status` is there only for the states its docs name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct LinkFlowResponse {
    /// Where the flow stands.
    #[ts(as = "LinkFlowState")]
    #[schema(value_type = LinkFlowState)]
    pub status: Open<LinkFlowState>,
    /// `waiting`: when the code expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub expires_at: Option<Timestamp>,
    /// `refused`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<RefusalReason>", optional)]
    #[schema(value_type = Option<RefusalReason>, nullable = false)]
    pub reason: Option<Open<RefusalReason>>,
    /// `linked`: the new account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub account_id: Option<AccountId>,
    /// `linked`: `pending` until approved, or `active` for the Owner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<AccountStatus>", optional)]
    #[schema(value_type = Option<AccountStatus>, nullable = false)]
    pub account_status: Option<Open<AccountStatus>>,
    /// `linked`: the account's Minecraft name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<MC_NAME_MAX_CHARS>::schema, nullable = false)]
    pub name: Option<BoundedText<MC_NAME_MAX_CHARS>>,
}

/// `POST /accounts/offline`: an offline account named `name`, while the
/// server allows them (`[accounts] allow_offline`).
#[derive(Debug, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateOfflineAccountRequest {
    /// The Minecraft name: 3 to 16 letters, digits or `_`, unique in any
    /// case.
    #[garde(custom(valid_mc_name))]
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<MC_NAME_MAX_CHARS>::schema)]
    pub name: SecretText<MC_NAME_MAX_CHARS>,
}

/// The garde rule of a Minecraft name: a valid `McUsername`. The message
/// names the rule, never the text.
fn valid_mc_name<C>(name: &SecretText<MC_NAME_MAX_CHARS>, _context: &C) -> garde::Result {
    name.expose()
        .parse::<McUsername>()
        .map(drop)
        .map_err(|_| garde::Error::new("must be 3 to 16 letters, digits or `_`"))
}

/// The new offline account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct CreatedOfflineAccountResponse {
    /// The account's ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: AccountId,
    /// Its Minecraft name, as sent.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<MC_NAME_MAX_CHARS>::schema)]
    pub name: BoundedText<MC_NAME_MAX_CHARS>,
    /// `offline`.
    #[ts(as = "AccountKind")]
    #[schema(value_type = AccountKind)]
    pub kind: Open<AccountKind>,
    /// `active`: an offline account needs no approval.
    #[ts(as = "AccountStatus")]
    #[schema(value_type = AccountStatus)]
    pub status: Open<AccountStatus>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_state_names_itself_as_serde_does() {
        for state in LinkFlowState::ALL {
            assert_eq!(serde_json::to_value(state).unwrap(), json!(state.as_str()));
        }
    }

    #[test]
    fn every_status_and_reason_names_itself_as_serde_does_and_as_fleet_core_does() {
        for reason in account::RefusalReason::ALL {
            let api = RefusalReason::from(reason);

            assert_eq!(serde_json::to_value(api).unwrap(), json!(reason.as_str()));
            assert_eq!(api.as_str(), reason.as_str());
        }
        for status in [
            account::AccountStatus::Pending,
            account::AccountStatus::Active,
            account::AccountStatus::ReauthRequired,
            account::AccountStatus::Refused(account::RefusalReason::NotOwned),
        ] {
            let api = AccountStatus::from(status);

            assert_eq!(serde_json::to_value(api).unwrap(), json!(status.as_str()));
            assert_eq!(api.as_str(), status.as_str());
        }
        assert_eq!(AccountStatus::ALL.len(), 4);
        assert_eq!(RefusalReason::ALL.len(), account::RefusalReason::ALL.len());
    }

    #[test]
    fn the_responses_round_trip_and_leave_out_what_isnt_there() {
        let started = json!({
            "flow_id": "018bcfe5-6800-7707-8707-070707070707",
            "user_code": "B7X4-KDLQ",
            "verification_uri": "https://www.microsoft.com/link",
            "expires_at": "2027-01-15T08:15:00.000Z"
        });
        let waiting = json!({ "status": "waiting", "expires_at": "2027-01-15T08:15:00.000Z" });
        let refused = json!({ "status": "refused", "reason": "xbox_child_account" });
        let linked = json!({
            "status": "linked",
            "account_id": "018bcfe5-6800-7101-8101-010101010101",
            "account_status": "pending",
            "name": "AfkBot1"
        });
        let declined = json!({ "status": "declined" });

        let started_back: LinkStartedResponse = serde_json::from_value(started.clone()).unwrap();
        assert_eq!(serde_json::to_value(started_back).unwrap(), started);
        for flow in [waiting, refused, linked, declined] {
            let back: LinkFlowResponse = serde_json::from_value(flow.clone()).unwrap();
            assert_eq!(serde_json::to_value(back).unwrap(), flow);
        }
    }

    #[test]
    fn a_state_status_or_reason_a_newer_server_sends_still_reads() {
        let read: LinkFlowResponse = serde_json::from_value(json!({
            "status": "paused",
            "reason": "xbox_new_rule",
            "account_status": "frozen"
        }))
        .unwrap();

        assert_eq!(read.status.known(), None);
        assert_eq!(read.status.as_str(), "paused");
        assert_eq!(read.reason.unwrap().as_str(), "xbox_new_rule");
        assert_eq!(read.account_status.unwrap().as_str(), "frozen");
    }

    #[test]
    fn every_kind_names_itself_as_serde_does_and_as_fleet_core_does() {
        for kind in [
            account::AccountKind::Microsoft,
            account::AccountKind::Offline,
        ] {
            let api = AccountKind::from(kind);

            assert_eq!(serde_json::to_value(api).unwrap(), json!(kind.as_str()));
            assert_eq!(api.as_str(), kind.as_str());
        }
        assert_eq!(AccountKind::ALL.len(), 2);
    }

    #[test]
    fn an_offline_request_takes_a_minecraft_name() {
        let request: CreateOfflineAccountRequest =
            serde_json::from_value(json!({ "name": "Afk_Bot1" })).unwrap();

        assert_eq!(request.name.expose(), "Afk_Bot1");
        assert!(request.validate().is_ok());
    }

    #[test]
    fn an_offline_request_refuses_what_isnt_a_minecraft_name() {
        for name in ["ab", "seventeen_chars_x", "Afk Bot", "Afk-Bot", "Äfk"] {
            let request: CreateOfflineAccountRequest =
                serde_json::from_value(json!({ "name": name })).unwrap();

            let report = request.validate().unwrap_err();

            let text = report.to_string();
            assert!(
                text.contains("must be 3 to 16 letters, digits or `_`"),
                "{text}"
            );
            assert!(!text.contains(name), "{text}");
        }
    }

    #[test]
    fn an_offline_request_refuses_unknown_missing_and_mistyped_fields() {
        let read = |body| serde_json::from_value::<CreateOfflineAccountRequest>(body);

        assert!(read(json!({ "name": "AfkBot1", "kind": "offline" })).is_err());
        assert!(read(json!({})).is_err());
        assert!(read(json!({ "name": 1 })).is_err());
    }

    #[test]
    fn the_offline_answer_round_trips_and_its_request_hides_the_name() {
        let created = json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "name": "AfkBot1",
            "kind": "offline",
            "status": "active"
        });
        let request: CreateOfflineAccountRequest =
            serde_json::from_value(json!({ "name": "Secret_Name1" })).unwrap();

        let back: CreatedOfflineAccountResponse = serde_json::from_value(created.clone()).unwrap();

        assert_eq!(serde_json::to_value(back).unwrap(), created);
        assert!(!format!("{request:?}").contains("Secret_Name1"));
    }

    #[test]
    fn the_started_flow_shows_no_code_in_debug() {
        let started: LinkStartedResponse = serde_json::from_value(json!({
            "flow_id": "018bcfe5-6800-7707-8707-070707070707",
            "user_code": "CODECODE",
            "verification_uri": "https://www.microsoft.com/link",
            "expires_at": "2027-01-15T08:15:00.000Z"
        }))
        .unwrap();

        let shown = format!("{started:?}");

        assert!(!shown.contains("CODECODE"), "{shown}");
    }
}
