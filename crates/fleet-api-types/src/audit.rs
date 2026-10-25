//! The audit log's DTOs (Plan.md P11.9, Appendix B; ADR-0020, S47, S48,
//! R64–R66): `GET /audit`'s query and pages, Admin+.
//!
//! Each entry shows what the audit log stores, built the same for every
//! viewer: when, who (the acting user's ID and the client's IP address,
//! none for the server's own entries, such as `retention.purge`), which
//! action, on what target, its outcome, and its metadata, every value of
//! which the server made (names from closed sets, IDs, times and counts,
//! never text a client sent; ADR-0016). An entry may name a target that
//! was deleted since, an invite or a session say; it lists as it is.
//!
//! The action's name, the target's kind and the outcome are [`Open`]: an
//! older app reads a name only a newer server records as unknown text, and
//! shows it. The entries' cursor is their row ID, a [`SafeInt`].

use fleet_core::audit::AuditMetadata;
use garde::Validate;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::IntoParams;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::openapi::{RefOr, Required};
use utoipa::{PartialSchema, ToSchema};

use crate::chat::{cursor_schema, positive_cursor};
use crate::ids::{ID_CHARS, UserId, id_schema};
use crate::int::{SafeInt, integer_schema};
use crate::open::{Open, OpenEnum};
use crate::page::{PAGE_DEFAULT, PAGE_MAX};
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The characters of an IP address's text: an IPv6 address's longest form.
pub const IP_MAX_CHARS: usize = 45;

/// The characters of a metadata key.
pub const AUDIT_KEY_MAX_CHARS: usize = AuditMetadata::MAX_KEY_LEN;

/// The characters of a metadata text.
pub const AUDIT_TEXT_MAX_CHARS: usize = AuditMetadata::MAX_TEXT_CHARS;

/// `AuditActionName` and its names, one per line, in fleet-core's
/// `AuditAction::ALL` order, which a test keeps.
macro_rules! audit_action_names {
    ($($(#[doc = $doc:literal])* $variant:ident = $name:literal;)*) => {
        /// An audited action's name, as the audit log stores it
        /// (fleet-core's `AuditAction`). This union lists the names this
        /// version knows; a newer server may record others, so code that
        /// branches on one always keeps a default branch.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        pub enum AuditActionName {
            $(
                $(#[doc = $doc])*
                #[serde(rename = $name)]
                $variant,
            )*
        }

        impl OpenEnum for AuditActionName {
            const ALL: &'static [Self] = &[$(Self::$variant),*];

            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

audit_action_names! {
    /// A user was created: the Owner from the host, or a user through an invite.
    UserCreate = "user.create";
    /// A replaced refresh token came back: its session family was ended.
    SecurityRefreshTokenReuse = "security.refresh_token_reuse";
    /// A username was locked after failed logins.
    SecurityLockout = "security.lockout";
    /// A user confirmed their authenticator.
    MfaEnroll = "mfa.enroll";
    /// A user replaced their authenticator.
    MfaReset = "mfa.reset";
    /// A login, its outcome saying how it went.
    AuthLogin = "auth.login";
    /// A logout.
    AuthLogout = "auth.logout";
    /// A user entered their password again for an action that needs it.
    AuthStepUp = "auth.step_up";
    /// A user's role changed.
    UserSetRole = "user.set_role";
    /// A user was disabled or enabled.
    UserSetDisabled = "user.set_disabled";
    /// An invite was created.
    InviteCreate = "invite.create";
    /// An invite was revoked.
    InviteRevoke = "invite.revoke";
    /// An invite was used, or a real one couldn't be.
    InviteUse = "invite.use";
    /// A user changed their password.
    UserChangePassword = "user.change_password";
    /// A session was revoked.
    SessionRevoke = "session.revoke";
    /// The vault's key was rotated.
    VaultRotateKey = "vault.rotate_key";
    /// A Microsoft account's tokens were refreshed.
    AccountRefresh = "account.refresh";
    /// An account's status changed.
    AccountSetStatus = "account.set_status";
    /// An account was renamed.
    AccountRename = "account.rename";
    /// A Microsoft account's profile didn't match the linked one.
    SecurityAccountProfileMismatch = "security.account_profile_mismatch";
    /// A Microsoft link was started.
    AccountStartLink = "account.start_link";
    /// A Microsoft account was linked.
    AccountLink = "account.link";
    /// A linked account was approved.
    AccountApprove = "account.approve";
    /// A linked account was refused.
    AccountRefuse = "account.refuse";
    /// An account was deleted, its bot and chat with it.
    AccountDelete = "account.delete";
    /// A Microsoft account was linked again.
    AccountRelink = "account.relink";
    /// An account was shared.
    GrantCreate = "grant.create";
    /// A grant's level changed.
    GrantSetLevel = "grant.set_level";
    /// A grant was revoked, or its holder left.
    GrantRevoke = "grant.revoke";
    /// An agent's enrollment token was created.
    EnrollmentTokenCreate = "enrollment_token.create";
    /// An enrollment token was used, or a real one couldn't be.
    EnrollmentTokenUse = "enrollment_token.use";
    /// An agent enrolled.
    AgentEnroll = "agent.enroll";
    /// An agent connected, or was refused.
    AgentConnect = "agent.connect";
    /// An agent's stream ended.
    AgentDisconnect = "agent.disconnect";
    /// Two agents claimed one identity.
    SecurityAgentIdentityConflict = "security.agent_identity_conflict";
    /// An agent was disabled.
    AgentDisable = "agent.disable";
    /// A bot was placed on an agent.
    BotAssign = "bot.assign";
    /// A bot was taken off an agent.
    BotUnassign = "bot.unassign";
    /// A bot was detached from an agent that no longer holds it.
    BotDetach = "bot.detach";
    /// An agent reported a bot it doesn't hold.
    BotStray = "bot.stray";
    /// A disabled agent's bots were released.
    AgentRelease = "agent.release";
    /// A bot's server address was set.
    BotSetServer = "bot.set_server";
    /// A bot's mode was switched.
    BotSetMode = "bot.set_mode";
    /// A bot was started.
    BotStart = "bot.start";
    /// A bot was stopped.
    BotStop = "bot.stop";
    /// A bot was restarted.
    BotRestart = "bot.restart";
    /// A failed bot was reset.
    BotReset = "bot.reset";
    /// A paused bot was resumed.
    BotResume = "bot.resume";
    /// A user had a bot send a chat line.
    BotSendChat = "bot.send_chat";
    /// A user had a bot send a command.
    BotSendCommand = "bot.send_command";
    /// A mode was created.
    ModeCreate = "mode.create";
    /// A mode was edited.
    ModeUpdate = "mode.update";
    /// A mode was deleted.
    ModeDelete = "mode.delete";
    /// The retention deleted what it no longer keeps: the server's own
    /// summary, every hour that deleted anything and at the stop.
    RetentionPurge = "retention.purge";
}

/// The names as one string enum: written by hand, since utoipa's derive
/// doesn't read the names a macro gives serde, as serde and ts-rs do.
impl PartialSchema for AuditActionName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .description(Some(concat!(
                "An audited action's name, as the audit log stores it. A newer server may ",
                "record others, so code that branches on one always keeps a default branch.",
            )))
            .schema_type(Type::String)
            .enum_values(Some(Self::ALL.iter().map(|name| name.as_str())))
            .into()
    }
}

impl ToSchema for AuditActionName {}

/// What an audit entry's target is.
/// This union lists the kinds this version knows; a newer server may
/// record others, so code that branches on one always keeps a default
/// branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditTargetKind {
    /// A user.
    User,
    /// A linked Minecraft account.
    Account,
    /// A bot.
    Bot,
    /// An agent.
    Agent,
    /// A mode.
    Mode,
    /// A session.
    Session,
    /// An invite.
    Invite,
    /// An agent's enrollment token.
    EnrollmentToken,
}

impl OpenEnum for AuditTargetKind {
    const ALL: &'static [Self] = &[
        Self::User,
        Self::Account,
        Self::Bot,
        Self::Agent,
        Self::Mode,
        Self::Session,
        Self::Invite,
        Self::EnrollmentToken,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Account => "account",
            Self::Bot => "bot",
            Self::Agent => "agent",
            Self::Mode => "mode",
            Self::Session => "session",
            Self::Invite => "invite",
            Self::EnrollmentToken => "enrollment_token",
        }
    }
}

/// How an audited action ended.
/// This union lists the outcomes this version knows; a newer server may
/// record others, so code that branches on one always keeps a default
/// branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcomeName {
    /// It was done.
    Success,
    /// It failed: a wrong password, an expired invite, a refused agent.
    Failure,
    /// The actor wasn't allowed to do it.
    Denied,
}

impl OpenEnum for AuditOutcomeName {
    const ALL: &'static [Self] = &[Self::Success, Self::Failure, Self::Denied];

    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
        }
    }
}

/// What an audit entry touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AuditTargetResponse {
    /// What it is.
    #[ts(as = "AuditTargetKind")]
    #[schema(value_type = AuditTargetKind)]
    pub kind: Open<AuditTargetKind>,
    /// Its ID, which may name something deleted since.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: BoundedText<ID_CHARS>,
}

/// One value of an audit entry's metadata, under its key: a text, an
/// integer or a flag, each the server's. A value only a newer server
/// records comes as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AuditDetail {
    /// The key.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<AUDIT_KEY_MAX_CHARS>::schema)]
    pub key: BoundedText<AUDIT_KEY_MAX_CHARS>,
    /// A text value: a name from a closed set, an ID or a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<AUDIT_TEXT_MAX_CHARS>::schema, nullable = false)]
    pub text: Option<BoundedText<AUDIT_TEXT_MAX_CHARS>>,
    /// An integer value: a count, a number or a row ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub int: Option<SafeInt>,
    /// A flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub flag: Option<bool>,
}

/// One entry of the audit log, as `GET /audit` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AuditEntryResponse {
    /// The entry's ID, in commit order: the list's cursor.
    pub id: SafeInt,
    /// When it happened, by the server's clock.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub at: Timestamp,
    /// Who did it; none for the server's own entries and for logins that
    /// named no user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub actor: Option<UserId>,
    /// The client's IP address; none for the server's own entries and the
    /// host's commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<IP_MAX_CHARS>::schema, nullable = false)]
    pub ip: Option<BoundedText<IP_MAX_CHARS>>,
    /// What was done.
    #[ts(as = "AuditActionName")]
    #[schema(value_type = AuditActionName)]
    pub action: Open<AuditActionName>,
    /// What it touched, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub target: Option<AuditTargetResponse>,
    /// How it ended.
    #[ts(as = "AuditOutcomeName")]
    #[schema(value_type = AuditOutcomeName)]
    pub outcome: Open<AuditOutcomeName>,
    /// Its details, by key.
    pub metadata: Vec<AuditDetail>,
}

/// `GET /audit`: one page of the audit log, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AuditPage {
    /// The entries.
    pub items: Vec<AuditEntryResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(nullable = false)]
    pub next: Option<SafeInt>,
}

/// `GET /audit?before=<id>&limit=<n>`: which page of the audit log to
/// answer, newest first. Like [`crate::PageQuery`], with the log's own
/// cursor, a row ID, which crosses as a [`SafeInt`] (ADR-0015). A parameter
/// that isn't one of these two, or one given twice, is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct AuditPageQuery {
    /// Only entries older than this one: the `next` of the page before. At
    /// least 1, as every row ID is.
    #[garde(inner(custom(|cursor: &SafeInt, &(): &()| positive_cursor(*cursor))))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<SafeInt>,
    /// How many entries at most: 1 to [`PAGE_MAX`].
    #[garde(range(min = 1, max = PAGE_MAX))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl AuditPageQuery {
    /// The page size: `limit`, or [`PAGE_DEFAULT`].
    #[must_use]
    pub fn limit_or_default(&self) -> u32 {
        self.limit.unwrap_or(PAGE_DEFAULT)
    }
}

impl IntoParams for AuditPageQuery {
    fn into_params(_parameter_in_provider: impl Fn() -> Option<ParameterIn>) -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("before")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some(
                    "Only entries older than this ID: the `next` of the page before.",
                ))
                .schema(Some(cursor_schema()))
                .build(),
            ParameterBuilder::new()
                .name("limit")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some("How many entries at most; 50 by default."))
                .schema(Some(integer_schema::<1, PAGE_MAX>()))
                .build(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget};
    use fleet_core::id::{
        AccountId, AgentId, BotId, EnrollmentTokenId, InviteId, ModeId, SessionId,
    };
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    #[test]
    fn every_action_fleet_core_records_has_its_name_in_the_same_order() {
        let names: Vec<&str> = AuditActionName::ALL
            .iter()
            .map(|name| name.as_str())
            .collect();
        let core: Vec<&str> = AuditAction::ALL.iter().map(AuditAction::as_str).collect();

        assert_eq!(names, core);
    }

    #[test]
    fn every_target_kind_fleet_core_records_has_its_name_in_the_same_order() {
        let at = chrono::DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        let random = [7; 10];
        let targets = [
            AuditTarget::User(UserId::new_v7(at, random).unwrap()),
            AuditTarget::Account(AccountId::new_v7(at, random).unwrap()),
            AuditTarget::Bot(BotId::new_v7(at, random).unwrap()),
            AuditTarget::Agent(AgentId::new_v7(at, random).unwrap()),
            AuditTarget::Mode(ModeId::new_v7(at, random).unwrap()),
            AuditTarget::Session(SessionId::new_v7(at, random).unwrap()),
            AuditTarget::Invite(InviteId::new_v7(at, random).unwrap()),
            AuditTarget::EnrollmentToken(EnrollmentTokenId::new_v7(at, random).unwrap()),
        ];

        let names: Vec<&str> = AuditTargetKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        let core: Vec<&str> = targets.iter().map(AuditTarget::kind).collect();

        assert_eq!(names, core);
    }

    #[test]
    fn every_outcome_fleet_core_records_has_its_name_in_the_same_order() {
        let names: Vec<&str> = AuditOutcomeName::ALL
            .iter()
            .map(|outcome| outcome.as_str())
            .collect();
        let core: Vec<&str> = AuditOutcome::ALL
            .iter()
            .map(|outcome| outcome.as_str())
            .collect();

        assert_eq!(names, core);
    }

    #[test]
    fn every_wire_name_is_serdes_name() {
        for name in AuditActionName::ALL {
            assert_eq!(serde_json::to_value(name).unwrap(), json!(name.as_str()));
        }
        for kind in AuditTargetKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
        for outcome in AuditOutcomeName::ALL {
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                json!(outcome.as_str())
            );
        }
    }

    #[test]
    fn the_limits_are_the_core_types() {
        assert_eq!(AUDIT_KEY_MAX_CHARS, AuditMetadata::MAX_KEY_LEN);
        assert_eq!(AUDIT_TEXT_MAX_CHARS, AuditMetadata::MAX_TEXT_CHARS);
        assert_eq!(
            IP_MAX_CHARS,
            "ffff:ffff:ffff:ffff:ffff:ffff:255.255.255.255".len()
        );
    }

    fn entry_json() -> serde_json::Value {
        json!({
            "id": 42,
            "at": "2027-01-15T08:00:00.000Z",
            "actor": "018bcfe5-6800-7101-8101-010101010101",
            "ip": "192.0.2.10",
            "action": "bot.set_server",
            "target": { "kind": "bot", "id": "018bcfe5-6800-7202-8202-020202020202" },
            "outcome": "success",
            "metadata": [
                { "key": "address", "text": "example.com" },
                { "key": "count", "int": 3 },
                { "key": "fresh", "flag": true }
            ]
        })
    }

    #[test]
    fn an_entry_and_a_page_round_trip_with_every_field_and_without_the_optional_ones() {
        let bare = json!({
            "id": 7,
            "at": "2027-01-15T08:00:00.000Z",
            "action": "retention.purge",
            "outcome": "success",
            "metadata": []
        });
        let page = json!({ "items": [entry_json(), bare.clone()], "next": 7 });

        for json in [entry_json(), bare] {
            let back: AuditEntryResponse = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(back).unwrap(), json);
        }
        let back: AuditPage = serde_json::from_value(page.clone()).unwrap();
        assert_eq!(serde_json::to_value(back).unwrap(), page);
    }

    #[test]
    fn names_only_a_newer_server_records_read_back_as_text() {
        let mut json = entry_json();
        json["action"] = json!("agent.teleport");
        json["outcome"] = json!("postponed");
        json["target"]["kind"] = json!("planet");

        let entry: AuditEntryResponse = serde_json::from_value(json).unwrap();

        assert_eq!(entry.action.known(), None);
        assert_eq!(entry.action.as_str(), "agent.teleport");
        assert_eq!(entry.outcome.as_str(), "postponed");
        assert_eq!(entry.target.unwrap().kind.as_str(), "planet");
    }

    #[rstest]
    #[case::the_first(json!({ "before": 1 }), true)]
    #[case::the_largest(json!({ "before": 9_007_199_254_740_991_i64, "limit": 100 }), true)]
    #[case::none(json!({}), true)]
    #[case::zero(json!({ "before": 0 }), false)]
    #[case::below(json!({ "before": -5 }), false)]
    #[case::no_entries(json!({ "limit": 0 }), false)]
    #[case::too_many(json!({ "limit": 101 }), false)]
    fn a_query_keeps_its_cursor_and_limit_in_range(
        #[case] json: serde_json::Value,
        #[case] valid: bool,
    ) {
        let query: AuditPageQuery = serde_json::from_value(json).unwrap();

        assert_eq!(query.validate().is_ok(), valid, "{query:?}");
    }

    #[test]
    fn a_query_refuses_an_unknown_parameter() {
        let error = serde_json::from_value::<AuditPageQuery>(json!({ "action": "auth.login" }));

        assert!(error.is_err());
    }

    #[test]
    fn the_default_page_holds_fifty() {
        let query: AuditPageQuery = serde_json::from_value(json!({})).unwrap();

        assert_eq!(query.limit_or_default(), PAGE_DEFAULT);
    }
}
