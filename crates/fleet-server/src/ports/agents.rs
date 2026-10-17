//! The agents and their certificates (Plan.md P10.2, P10.3, Appendix D;
//! ADR-0019): [`Agent`], an enrolled agent, [`AgentCert`], a certificate the
//! server signed for one, and the read and write traits.
//!
//! An agent's identity is its ID, which the server mints and writes into the
//! certificate's SAN, and its certificates' fingerprints, never its name:
//! the name is only the label its own config gives it. Two enabled agents
//! never share a name, whatever its case ([`InsertAgentError::NameTaken`]).
//! A certificate's row is keyed by its fingerprint, and an agent may hold
//! more than one (P13.6's renewal; S1).

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::AgentId;
use fleet_core::pki::CertFingerprint;
use fleet_core::value::{AgentCapacity, AgentName, AgentVersion};

use super::page::Page;
use super::store::StoreError;

/// A stored agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// The ID, which its certificates name.
    pub id: AgentId,
    /// The name its config gave it when it enrolled.
    pub name: AgentName,
    /// Whether it may connect.
    pub enabled: bool,
    /// The capacity its last hello reported; `None` until its first one.
    pub capacity: Option<AgentCapacity>,
    /// The version its last hello reported; `None` until its first one.
    pub version: Option<AgentVersion>,
    /// When the server last heard from it; `None` until it connects.
    pub last_seen_at: Option<DateTime<Utc>>,
    /// When it enrolled.
    pub created_at: DateTime<Utc>,
}

/// An agent to create at its enrollment: enabled, and not yet heard from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgent {
    /// The ID, minted from `created_at`.
    pub id: AgentId,
    /// The name its config gives it.
    pub name: AgentName,
    /// When it enrolls.
    pub created_at: DateTime<Utc>,
}

/// A certificate the server signed for an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCert {
    /// SHA-256 of the certificate's DER.
    pub fingerprint: CertFingerprint,
    /// The agent its SAN names.
    pub agent: AgentId,
    /// The certificate's notAfter: it's expired from then on.
    pub not_after: DateTime<Utc>,
    /// When it was revoked, if it was.
    pub revoked_at: Option<DateTime<Utc>>,
    /// When it was stored.
    pub created_at: DateTime<Utc>,
}

/// A certificate to store: unrevoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewAgentCert {
    /// SHA-256 of the certificate's DER.
    pub fingerprint: CertFingerprint,
    /// The agent its SAN names.
    pub agent: AgentId,
    /// The certificate's notAfter, after `created_at`.
    pub not_after: DateTime<Utc>,
    /// When it's stored.
    pub created_at: DateTime<Utc>,
}

/// What an agent's accepted hello reported, stamped with the server's time
/// (ADR-0019, S29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHello {
    /// The agent, from its certificate.
    pub agent: AgentId,
    /// How many bots it may run.
    pub capacity: AgentCapacity,
    /// Its version.
    pub version: AgentVersion,
    /// When the server read the hello, by its own clock.
    pub at: DateTime<Utc>,
}

/// Why [`AgentWrites::insert`] failed.
#[derive(Debug, thiserror::Error)]
pub enum InsertAgentError {
    /// An enabled agent has this name already, in any case.
    #[error("an enabled agent has this name already")]
    NameTaken,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// An agent as the agents' list shows it: its row, and when its latest
/// unrevoked certificate expires (ADR-0019, S31).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedAgent {
    /// The row.
    pub agent: Agent,
    /// The latest `not_after` of its unrevoked certificates; none once every
    /// one is revoked.
    pub not_after: Option<DateTime<Utc>>,
    /// How many bots it holds: those assigned to it and its open release
    /// records, as its capacity counts them (ADR-0019, S41, R27).
    pub bots: u32,
}

/// Reading agents through the read connections.
#[async_trait]
pub trait AgentReads: Send + Sync + fmt::Debug {
    /// The agent `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&self, id: AgentId) -> Result<Option<Agent>, StoreError>;

    /// Whether an enabled agent has `name`, in any case.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn name_taken(&self, name: &AgentName) -> Result<bool, StoreError>;

    /// The certificates of agent `id`, oldest first.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn certs(&self, id: AgentId) -> Result<Vec<AgentCert>, StoreError>;

    /// The certificate whose fingerprint is `fingerprint`: what a connecting
    /// agent is looked up by.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn cert(&self, fingerprint: CertFingerprint) -> Result<Option<AgentCert>, StoreError>;

    /// The fingerprint of every certificate that admits its agent at `now`:
    /// unrevoked, unexpired, of an enabled agent. The live streams' sweep
    /// reads it once (ADR-0019, S27).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn admissible(&self, now: DateTime<Utc>) -> Result<Vec<CertFingerprint>, StoreError>;

    /// The agents of `page`, newest first: one more than its limit when
    /// another page follows.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn list(&self, page: Page<AgentId>) -> Result<Vec<ListedAgent>, StoreError>;

    /// The agent `id`, as the list shows it.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn listed(&self, id: AgentId) -> Result<Option<ListedAgent>, StoreError>;
}

/// Writing agents inside a [`super::store::WriteTx`].
#[async_trait]
pub trait AgentWrites: Send {
    /// Inserts `agent`, enabled.
    ///
    /// # Errors
    /// [`InsertAgentError::NameTaken`] when an enabled agent has the name
    /// already, or [`InsertAgentError::Store`].
    async fn insert(&mut self, agent: &NewAgent) -> Result<(), InsertAgentError>;

    /// Stores `cert`.
    ///
    /// # Errors
    /// [`StoreError`]: an agent that doesn't exist, a fingerprint stored
    /// already, or a `not_after` that isn't after `created_at`.
    async fn insert_cert(&mut self, cert: &NewAgentCert) -> Result<(), StoreError>;

    /// The agent `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find(&mut self, id: AgentId) -> Result<Option<Agent>, StoreError>;

    /// The certificate whose fingerprint is `fingerprint`, read in this
    /// transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn cert(&mut self, fingerprint: CertFingerprint)
    -> Result<Option<AgentCert>, StoreError>;

    /// Records `hello`'s capacity and version on its agent, and its time as
    /// `last_seen_at` unless a later one is stored; only for an enabled
    /// agent. Whether the agent was enabled and so changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn record_hello(&mut self, hello: &AgentHello) -> Result<bool, StoreError>;

    /// Moves agent `id`'s `last_seen_at` forward to `at`; a later stored time
    /// stays.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn record_seen(&mut self, id: AgentId, at: DateTime<Utc>) -> Result<(), StoreError>;

    /// Disables agent `id` while it's enabled. Whether it changed.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn disable(&mut self, id: AgentId) -> Result<bool, StoreError>;

    /// Revokes every unrevoked certificate of agent `id` at `at`; how many.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn revoke_certs(&mut self, id: AgentId, at: DateTime<Utc>) -> Result<u64, StoreError>;
}
