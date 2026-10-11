//! [`TokenService`]: sessions and their tokens (Plan.md P7.3, §7.2,
//! ADR-0006, ADR-0016).
//!
//! - **Issuing:** [`TokenService::prepare`] mints a session's first token
//!   pair, its ID and its times before any transaction opens (no crypto
//!   while a `WriteTx` holds the write connection). The caller inserts it
//!   and commits with its own audit entry: D's login, with `auth.login`.
//! - **Refreshing:** [`TokenService::refresh`] looks the presented token up
//!   inside the write transaction, so two refreshes with the same token take
//!   turns and the second finds it replaced, and decides with
//!   `fleet_core::auth::refresh_decision`:
//!   - a rotation commits without an entry ([`UnauditedWrite::SessionRotate`]);
//!   - a reused token revokes its whole session and records
//!     `security.refresh_token_reuse` in the same commit, and one `warn`;
//!   - anything else is refused and changes nothing.
//!
//!   Every refusal is the same [`RefreshError::Rejected`], so the API can't
//!   tell a client why.

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::{
    RefreshDecision, RefreshFacts, RefreshMatch, SessionPolicy, SessionTimes, refresh_decision,
};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use secrecy::{ExposeSecret as _, SecretString};
use tracing::warn;

use super::audit::AuditService;
use crate::ports::sessions::{NewSession, RefreshLookup, Rotation, Session};
use crate::ports::store::{Store, StoreError, UnauditedWrite};
use crate::ports::tokens::{MintTokenError, MintedToken, TokenHash, TokenKind, Tokens};

/// A session's tokens, for the client, and when they expire. Its `Debug`
/// shows neither token.
#[derive(Debug)]
pub struct IssuedTokens {
    /// The session's ID, the same across every refresh.
    pub session_id: SessionId,
    /// The access token, sent with every request.
    pub access_token: SecretString,
    /// The refresh token, sent only to refresh.
    pub refresh_token: SecretString,
    /// When the access token expires.
    pub access_expires_at: DateTime<Utc>,
    /// When the refresh token expires, unless it's used before: idle, or
    /// the session's absolute end.
    pub refresh_expires_at: DateTime<Utc>,
}

/// A new session, ready to insert, and its tokens for the client. Its
/// `Debug` shows neither token nor hash.
#[derive(Debug)]
pub struct PreparedSession {
    /// The row to insert in the caller's transaction.
    pub new_session: NewSession,
    /// The tokens to hand out once it's committed.
    pub tokens: IssuedTokens,
}

/// Why a session couldn't be prepared.
#[derive(Debug, thiserror::Error)]
pub enum IssueError {
    /// A token couldn't be minted (the random source failed).
    #[error("the session's tokens can't be minted")]
    Token(#[source] MintTokenError),
    /// The session's ID couldn't be minted (the random source failed, or the
    /// clock is outside an ID's range).
    #[error("the session's ID can't be minted")]
    Id(#[source] MintError),
}

/// Why a refresh failed.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The token was refused: malformed, unknown, revoked, expired, a
    /// disabled user's, or reused (its session is revoked now). The API
    /// answers every one alike.
    #[error("the refresh token was refused")]
    Rejected,
    /// The new tokens couldn't be minted (the random source failed).
    #[error("the new tokens can't be minted")]
    Token(#[source] MintTokenError),
    /// The reuse entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Sessions and their tokens.
#[derive(Debug, Clone)]
pub struct TokenService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    policy: SessionPolicy,
    audit: AuditService,
}

impl TokenService {
    /// The service over `store`, minting tokens with `tokens` and IDs from
    /// `random`, stamping with `clock`, with the lifetimes of `policy`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        tokens: Arc<dyn Tokens>,
        policy: SessionPolicy,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            tokens,
            policy,
            audit,
        }
    }

    /// A new session for `user_id`: both tokens, their hashes, the session's
    /// ID and its times, made now and before any transaction. The caller
    /// inserts [`PreparedSession::new_session`] with `tx.sessions().insert`
    /// and commits with its own audit entry; only then are the tokens handed
    /// out.
    ///
    /// # Errors
    /// [`IssueError::Token`] or [`IssueError::Id`].
    pub fn prepare(&self, user_id: UserId) -> Result<PreparedSession, IssueError> {
        let access = self
            .tokens
            .mint(TokenKind::Access)
            .map_err(IssueError::Token)?;
        let refresh = self
            .tokens
            .mint(TokenKind::Refresh)
            .map_err(IssueError::Token)?;
        let now = self.clock.now();
        let id: SessionId = mint(now, self.random.as_ref()).map_err(IssueError::Id)?;
        let new_session = NewSession {
            id,
            user_id,
            access_hash: access.hash,
            refresh_hash: refresh.hash,
            created_at: now,
        };
        let times = SessionTimes {
            created_at: now,
            refreshed_at: now,
        };
        Ok(PreparedSession {
            new_session,
            tokens: self.issued(id, times, access, refresh),
        })
    }

    /// Refreshes the session `presented` belongs to: a new token pair when
    /// it's the session's current refresh token; see the
    /// [module docs](self). `ip` is the client's, recorded if the token was
    /// reused.
    ///
    /// # Errors
    /// [`RefreshError::Rejected`] for every refused token,
    /// [`RefreshError::Token`], [`RefreshError::Audit`] or
    /// [`RefreshError::Store`].
    pub async fn refresh(
        &self,
        presented: &SecretString,
        ip: IpAddr,
    ) -> Result<IssuedTokens, RefreshError> {
        let Some(hash) = self
            .tokens
            .hash_presented(TokenKind::Refresh, presented.expose_secret())
        else {
            return Err(RefreshError::Rejected);
        };
        // Minted before the transaction (no crypto while it's open), and
        // wiped unused if the token is refused.
        let access = self
            .tokens
            .mint(TokenKind::Access)
            .map_err(RefreshError::Token)?;
        let refresh = self
            .tokens
            .mint(TokenKind::Refresh)
            .map_err(RefreshError::Token)?;
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let found = tx
            .sessions()
            .find_by_refresh_hash(&hash)
            .await?
            .filter(|found| matched_hash(found).ct_eq(&hash));
        let decision = refresh_decision(&self.policy, found.as_ref().map(facts), now);
        match (decision, found) {
            (RefreshDecision::Rotate, Some(RefreshLookup::Current(found))) => {
                let session = found.session;
                let rotation = Rotation {
                    session_id: session.id,
                    replaced: hash,
                    access_hash: access.hash,
                    refresh_hash: refresh.hash,
                    at: now,
                };
                if !tx.sessions().rotate(&rotation).await? {
                    return Err(RefreshError::Rejected);
                }
                tx.commit_unaudited(UnauditedWrite::SessionRotate).await?;
                let times = SessionTimes {
                    created_at: session.created_at,
                    refreshed_at: now,
                };
                Ok(self.issued(session.id, times, access, refresh))
            }
            (
                RefreshDecision::Reuse,
                Some(RefreshLookup::Rotated {
                    session: found,
                    rotated_at,
                    ..
                }),
            ) => {
                let session = found.session;
                tx.sessions().revoke(session.id, now).await?;
                let metadata = AuditMetadata::new()
                    .id("session_id", session.id)?
                    .time("rotated_at", rotated_at)?;
                let entry = self
                    .audit
                    .entry(
                        AuditAction::SECURITY_REFRESH_TOKEN_REUSE,
                        AuditOutcome::Failure,
                    )
                    .ip(ip)
                    .target(AuditTarget::User(session.user_id))
                    .metadata(metadata);
                tx.commit(entry).await?;
                warn!(
                    user_id = %session.user_id,
                    session_id = %session.id,
                    "a reused refresh token revoked its session"
                );
                Err(RefreshError::Rejected)
            }
            // Refused: dropping the transaction rolls back nothing written.
            _ => Err(RefreshError::Rejected),
        }
    }

    /// The tokens of session `id` as the client gets them, with their
    /// expiries for `times`.
    fn issued(
        &self,
        id: SessionId,
        times: SessionTimes,
        access: MintedToken,
        refresh: MintedToken,
    ) -> IssuedTokens {
        IssuedTokens {
            session_id: id,
            access_token: access.token,
            refresh_token: refresh.token,
            access_expires_at: self.policy.access_expiry(times),
            refresh_expires_at: self.policy.refresh_expiry(times),
        }
    }
}

/// The stored hash a lookup matched: the session's current refresh hash, or
/// the replaced one from its history.
fn matched_hash(found: &RefreshLookup) -> &TokenHash {
    match found {
        RefreshLookup::Current(found) => &found.session.refresh_hash,
        RefreshLookup::Rotated { hash, .. } => hash,
    }
}

/// The facts the refresh rule decides on.
fn facts(found: &RefreshLookup) -> RefreshFacts {
    let (found, matched) = match found {
        RefreshLookup::Current(found) => (found, RefreshMatch::Current),
        RefreshLookup::Rotated { session, .. } => (session, RefreshMatch::Rotated),
    };
    RefreshFacts {
        matched,
        times: times(&found.session),
        revoked: found.session.revoked_at.is_some(),
        user_disabled: found.disabled,
    }
}

/// A stored session's anchors.
const fn times(session: &Session) -> SessionTimes {
    SessionTimes {
        created_at: session.created_at,
        refreshed_at: session.refreshed_at,
    }
}
