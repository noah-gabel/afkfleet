//! The agents' enrollment tokens (Plan.md P10.3, Appendix D; ADR-0019):
//! [`EnrollmentToken`], one single-use token that enrolls one new agent, and
//! the read and write traits.
//!
//! The token is stored only as its SHA-256 hash ([`TokenHash`]); a lookup by
//! hash is confirmed with [`TokenHash::ct_eq`], like a session's token. Using
//! a token is a conditional write ([`EnrollmentTokenWrites::use_token`]), so
//! two enrollments that both checked it first can't both use it.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::id::{AgentId, EnrollmentTokenId, UserId};

use super::store::StoreError;
use super::tokens::TokenHash;

/// A stored enrollment token.
#[derive(Debug, Clone)]
pub struct EnrollmentToken {
    /// The ID.
    pub id: EnrollmentTokenId,
    /// SHA-256 of the token's text.
    pub token_hash: TokenHash,
    /// The Owner who created it; `None` when the server's host operator did.
    pub created_by: Option<UserId>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// When it enrolled an agent, once used.
    pub used_at: Option<DateTime<Utc>>,
    /// The agent it enrolled, once used.
    pub agent: Option<AgentId>,
}

/// A token to create: unused.
#[derive(Debug, Clone)]
pub struct NewEnrollmentToken {
    /// The ID, minted from `created_at`.
    pub id: EnrollmentTokenId,
    /// SHA-256 of the token's text.
    pub token_hash: TokenHash,
    /// The Owner who creates it; `None` for the server's host operator.
    pub created_by: Option<UserId>,
    /// When it's created.
    pub created_at: DateTime<Utc>,
    /// When it expires: after `created_at`.
    pub expires_at: DateTime<Utc>,
}

/// Reading enrollment tokens through the read connections.
#[async_trait]
pub trait EnrollmentTokenReads: Send + Sync + fmt::Debug {
    /// The token whose text hashes to `hash`, for the caller to confirm with
    /// [`TokenHash::ct_eq`].
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find_by_hash(&self, hash: &TokenHash) -> Result<Option<EnrollmentToken>, StoreError>;
}

/// Reading and writing enrollment tokens inside a
/// [`super::store::WriteTx`].
#[async_trait]
pub trait EnrollmentTokenWrites: Send {
    /// Inserts `token`.
    ///
    /// # Errors
    /// [`StoreError`]: a creator that doesn't exist, a hash stored already,
    /// or an expiry that isn't after the creation.
    async fn insert(&mut self, token: &NewEnrollmentToken) -> Result<(), StoreError>;

    /// The token whose text hashes to `hash`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn find_by_hash(
        &mut self,
        hash: &TokenHash,
    ) -> Result<Option<EnrollmentToken>, StoreError>;

    /// How many tokens are unused and unexpired at `now`.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_pending(&mut self, now: DateTime<Utc>) -> Result<u32, StoreError>;

    /// Marks token `id` used at `now` by `agent`, only while it's unused and
    /// unexpired at `now`. Whether it did.
    ///
    /// # Errors
    /// [`StoreError`]: an agent that doesn't exist, or one another token
    /// enrolled already.
    async fn use_token(
        &mut self,
        id: EnrollmentTokenId,
        agent: AgentId,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError>;
}
