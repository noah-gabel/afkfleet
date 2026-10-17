//! The agents' DTOs (Plan.md P10.3, P10.10, Appendix B, ADR-0019): creating
//! an enrollment token, and the agents' list.
//!
//! A token enrolls one new agent within 15 minutes. It reaches the agent in
//! the bundle, `<token>.<CA fingerprint>`, which is in
//! [`CreatedEnrollmentTokenResponse`] only: the server keeps just the
//! token's hash, so it's never shown again. Creating one is the Owner's,
//! after a step-up.
//!
//! The agents' list ([`AgentPage`]) shows every agent, enabled or not, with
//! its status, what its last hello said, and when its latest certificate
//! expires; disabling one answers it as the list shows it.

use fleet_core::agent::AgentStatus as CoreAgentStatus;
use fleet_core::enrollment::EnrollmentBundle;
use fleet_core::id::{AgentId, EnrollmentTokenId};
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::{PartialSchema as _, ToSchema};

use crate::ids::id_schema;
use crate::int::integer_schema;
use crate::open::{Open, OpenEnum};
use crate::secret::SecretText;
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The characters of an enrollment bundle.
pub const BUNDLE_CHARS: usize = EnrollmentBundle::TEXT_LEN;

/// The characters of a CA fingerprint: SHA-256 in openssl's uppercase
/// colon form.
pub const FINGERPRINT_CHARS: usize = CertFingerprint::TEXT_LEN;

/// The characters of an agent's name.
pub const AGENT_NAME_MAX_CHARS: usize = AgentName::MAX_LEN;

/// The characters of an agent's version.
pub const AGENT_VERSION_MAX_CHARS: usize = AgentVersion::MAX_LEN;

/// The most bots an agent may run at once.
pub const AGENT_CAPACITY_MAX: u32 = AgentCapacity::MAX as u32;

/// An agent's status.
/// This union lists the values this version knows; a newer server may send
/// others, so code that branches on it keeps a default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Enabled, with a valid certificate and a live stream.
    Online,
    /// Enabled, with a valid certificate, and not connected.
    Offline,
    /// Enabled, but its certificate expired: it enrolls again.
    Expired,
    /// Disabled for good: its name is free for a new enrollment.
    Disabled,
}

impl OpenEnum for AgentStatus {
    const ALL: &'static [Self] = &[Self::Online, Self::Offline, Self::Expired, Self::Disabled];

    fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
        }
    }
}

impl From<CoreAgentStatus> for AgentStatus {
    fn from(status: CoreAgentStatus) -> Self {
        match status {
            CoreAgentStatus::Online => Self::Online,
            CoreAgentStatus::Offline => Self::Offline,
            CoreAgentStatus::Expired => Self::Expired,
            CoreAgentStatus::Disabled => Self::Disabled,
        }
    }
}

/// An agent as `GET /agents` lists it, and as
/// `POST /agents/{id}/disable` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AgentResponse {
    /// The ID, which its certificates name.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: AgentId,
    /// The name it enrolled with.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<AGENT_NAME_MAX_CHARS>::schema)]
    pub name: BoundedText<AGENT_NAME_MAX_CHARS>,
    /// Whether it's enabled; disabling is final.
    pub enabled: bool,
    /// Its status.
    #[ts(as = "AgentStatus")]
    #[schema(value_type = AgentStatus)]
    pub status: Open<AgentStatus>,
    /// How many bots it holds: those assigned to it, and those it may still
    /// run that the server released (P10.5). A disabled agent's stay until
    /// they're released.
    #[schema(schema_with = integer_schema::<0, { u32::MAX }>)]
    pub bots: u32,
    /// How many bots it may run at once, as its last hello said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<1, AGENT_CAPACITY_MAX>, nullable = false)]
    pub capacity: Option<u32>,
    /// Its version, as its last hello said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<AGENT_VERSION_MAX_CHARS>::schema, nullable = false)]
    pub version: Option<BoundedText<AGENT_VERSION_MAX_CHARS>>,
    /// When its live stream was welcomed, while it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub connected_since: Option<Timestamp>,
    /// When the server last heard from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub last_seen_at: Option<Timestamp>,
    /// When its latest unrevoked certificate expires; none once every one
    /// is revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = Timestamp::schema, nullable = false)]
    pub not_after: Option<Timestamp>,
    /// When it enrolled.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
}

/// `GET /agents`: one page of agents, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct AgentPage {
    /// The agents.
    pub items: Vec<AgentResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<AgentId>,
}

/// `POST /agents/enrollment-tokens`: the new token, in its bundle, shown
/// this once.
#[derive(Debug, Serialize, Deserialize, TS, ToSchema)]
pub struct CreatedEnrollmentTokenResponse {
    /// The token's ID, which its audit entries name.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: EnrollmentTokenId,
    /// The bundle (`afk_et_….<CA fingerprint>`) that `afkfleet-agent
    /// enroll` reads from stdin.
    #[ts(type = "string")]
    #[schema(schema_with = SecretText::<BUNDLE_CHARS>::schema)]
    pub bundle: SecretText<BUNDLE_CHARS>,
    /// The CA's fingerprint, also in the bundle: the one `pki init`
    /// printed.
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<FINGERPRINT_CHARS>::schema)]
    pub ca_fingerprint: BoundedText<FINGERPRINT_CHARS>,
    /// When the token expires, 15 minutes after it was created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub expires_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FINGERPRINT: &str = "AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB";

    fn created_json() -> serde_json::Value {
        json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "bundle": format!("afk_et_BUNDLEBUNDLE.{FINGERPRINT}"),
            "ca_fingerprint": FINGERPRINT,
            "expires_at": "2027-01-15T08:15:00.000Z"
        })
    }

    #[test]
    fn the_limits_are_the_core_types_lengths() {
        assert_eq!(BUNDLE_CHARS, 146);
        assert_eq!(FINGERPRINT_CHARS, FINGERPRINT.len());
    }

    #[test]
    fn a_created_token_round_trips() {
        let created = created_json();

        let back: CreatedEnrollmentTokenResponse = serde_json::from_value(created.clone()).unwrap();

        assert_eq!(serde_json::to_value(back).unwrap(), created);
    }

    fn agent_json() -> serde_json::Value {
        json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "name": "agent-1",
            "enabled": true,
            "status": "online",
            "bots": 3,
            "capacity": 10,
            "version": "0.1.0",
            "connected_since": "2027-01-15T08:00:00.000Z",
            "last_seen_at": "2027-01-15T08:00:30.000Z",
            "not_after": "2027-03-16T08:00:00.000Z",
            "created_at": "2027-01-15T07:00:00.000Z"
        })
    }

    #[test]
    fn an_agent_round_trips_with_every_field_and_without_the_optional_ones() {
        let full = agent_json();
        let bare = json!({
            "id": "018bcfe5-6800-7101-8101-010101010101",
            "name": "agent-1",
            "enabled": false,
            "status": "disabled",
            "bots": 0,
            "created_at": "2027-01-15T07:00:00.000Z"
        });

        for json in [full, bare] {
            let back: AgentResponse = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(back).unwrap(), json);
        }
    }

    #[test]
    fn every_core_status_has_its_name() {
        let names = [
            (CoreAgentStatus::Online, "online"),
            (CoreAgentStatus::Offline, "offline"),
            (CoreAgentStatus::Expired, "expired"),
            (CoreAgentStatus::Disabled, "disabled"),
        ];

        for (status, name) in names {
            assert_eq!(AgentStatus::from(status).as_str(), name);
            assert_eq!(
                serde_json::to_value(AgentStatus::from(status)).unwrap(),
                json!(name)
            );
        }
    }

    #[test]
    fn the_agents_limits_are_the_core_types() {
        assert_eq!(AGENT_NAME_MAX_CHARS, AgentName::MAX_LEN);
        assert_eq!(AGENT_VERSION_MAX_CHARS, AgentVersion::MAX_LEN);
        assert_eq!(AGENT_CAPACITY_MAX, u32::from(AgentCapacity::MAX));
    }

    #[test]
    fn no_dto_shows_a_secret_in_debug() {
        let created: CreatedEnrollmentTokenResponse =
            serde_json::from_value(created_json()).unwrap();

        let shown = format!("{created:?}");

        assert!(!shown.contains("BUNDLEBUNDLE"), "{shown}");
    }
}
