//! [`TokenService`]: sessions and their tokens (Plan.md P7.3, §7.2,
//! ADR-0006, ADR-0016).
//!
//! - **Issuing:** [`TokenService::prepare`] mints a session's first token
//!   pair, its ID and its times before any transaction opens (no crypto
//!   while a `WriteTx` holds the write connection). The caller inserts it
//!   and commits with its own audit entry: D's login, with `auth.login`.
//! - **Refreshing:** [`TokenService::refresh`] first asks the read
//!   connections whether the presented token's hash exists at all, current
//!   or replaced (group D): an unknown token is refused without taking the
//!   one write connection, and a token reaches a client only after its
//!   commit, so this never misses a real one. A known one is looked up again
//!   inside the write transaction, so two refreshes with the same token take
//!   turns and the second finds it replaced, and decided with
//!   `fleet_core::auth::refresh_decision`:
//!   - a rotation commits without an entry ([`UnauditedWrite::SessionRotate`]);
//!   - a reused token revokes its whole session and records
//!     `security.refresh_token_reuse` in the same commit, and one `warn`;
//!   - anything else is refused and changes nothing.
//!
//!   Every refusal is the same [`RefreshError::Rejected`], so the API can't
//!   tell a client why.
//! - **Logging out** (group D): [`TokenService::logout`] revokes the
//!   request's own session and records `auth.logout`.
//! - **Authenticating** (P7.8): [`TokenService::authenticate`] finds the
//!   session of a request's access token, with its user, through the read
//!   connections, and decides with `fleet_core::auth::access_decision`, so a
//!   revocation, a disabled user or a role change counts on the very next
//!   request.
//! - **The touch:** a request notes its session's use (`last_used_at`) at
//!   most once a minute (`fleet_core::auth::touch_due`), with a conditional
//!   write committed without an entry ([`UnauditedWrite::SessionTouch`]). It
//!   never fails or holds up the request: only the wait for the write
//!   connection is bounded, by [`TOUCH_WAIT`]; once the transaction is open,
//!   the write and the commit take milliseconds and run to the end. Past the
//!   bound, or on any error, the touch is skipped and logged at `debug`.

use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::{
    AccessFacts, RefreshDecision, RefreshFacts, RefreshMatch, SessionPolicy, SessionTimes,
    access_decision, is_stepped_up, refresh_decision, touch_due, touch_stale_since,
};
use fleet_core::authz::Actor;
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_core::time::elapsed;
use secrecy::{ExposeSecret as _, SecretString};
use tracing::{debug, info, warn};

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
    /// The access token's whole seconds left when it was issued.
    pub access_expires_in: u64,
    /// The refresh token's whole seconds left when it was issued.
    pub refresh_expires_in: u64,
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

/// Why a logout failed: the server's fault, never the client's.
#[derive(Debug, thiserror::Error)]
pub enum LogoutError {
    /// The entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// How long a request waits for the write connection to note its session's
/// use; past it, the touch is skipped (see the [module docs](self)).
pub const TOUCH_WAIT: Duration = Duration::from_millis(250);

/// Who a request's access token says it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Authenticated {
    /// The user's ID and their role as it is now.
    pub actor: Actor,
    /// The session the token belongs to.
    pub session_id: SessionId,
    /// When the user last entered their password again in this session.
    pub stepped_up_at: Option<DateTime<Utc>>,
}

/// Why a request's credentials were refused, for the log only: the API
/// answers every refusal alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedAccess {
    /// The reason as the log names it: `malformed`, `unknown`, `revoked`,
    /// `expired`, `disabled`, or one the extractor found in the headers.
    pub reason: &'static str,
    /// The session, once one was found.
    pub session_id: Option<SessionId>,
    /// The session's user, once a session was found.
    pub user_id: Option<UserId>,
}

impl RejectedAccess {
    /// A refusal before any session was found.
    #[must_use]
    pub const fn reason(reason: &'static str) -> Self {
        Self {
            reason,
            session_id: None,
            user_id: None,
        }
    }
}

/// Why a request couldn't be authenticated.
#[derive(Debug, thiserror::Error)]
pub enum AuthenticateError {
    /// The access token was refused; the reason is only logged.
    #[error("the access token was refused ({})", .0.reason)]
    Rejected(RejectedAccess),
    /// The database failed: not a refusal.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a touch was skipped.
#[derive(Debug, thiserror::Error)]
enum TouchError {
    /// The write connection stayed taken past [`TOUCH_WAIT`].
    #[error("the write connection stayed taken for {} ms", TOUCH_WAIT.as_millis())]
    TimedOut,
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
        // An unknown token never takes the write connection. A token reaches
        // a client only after its commit, so this never misses a real one.
        if !self.store.sessions().refresh_hash_exists(&hash).await? {
            return Err(RefreshError::Rejected);
        }
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

    /// Revokes `session_id`, the session of `user_id`'s request from `ip`,
    /// and records `auth.logout`. A session revoked meanwhile is left as it
    /// is, without an entry: it's over either way.
    ///
    /// # Errors
    /// [`LogoutError`]: the server's failures.
    pub async fn logout(
        &self,
        user_id: UserId,
        session_id: SessionId,
        ip: IpAddr,
    ) -> Result<(), LogoutError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        if !tx.sessions().revoke(session_id, now).await? {
            return Ok(());
        }
        let entry = self
            .audit
            .entry(AuditAction::AUTH_LOGOUT, AuditOutcome::Success)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::Session(session_id));
        tx.commit(entry).await?;
        info!(user_id = %user_id, session_id = %session_id, "a user logged out");
        Ok(())
    }

    /// Whether `stamp`, when the session's password was last entered again,
    /// allows a step-up action now (`fleet_core::auth::is_stepped_up`).
    #[must_use]
    pub fn is_stepped_up(&self, stamp: Option<DateTime<Utc>>) -> bool {
        is_stepped_up(stamp, self.clock.now())
    }

    /// The session a request's access token belongs to, as the actor it
    /// comes from; see the [module docs](self). A due touch is written on
    /// the way, and never fails the request.
    ///
    /// # Errors
    /// [`AuthenticateError::Rejected`] for every refused token, and
    /// [`AuthenticateError::Store`] when the database fails.
    pub async fn authenticate(
        &self,
        presented: &SecretString,
    ) -> Result<Authenticated, AuthenticateError> {
        let Some(hash) = self
            .tokens
            .hash_presented(TokenKind::Access, presented.expose_secret())
        else {
            return Err(AuthenticateError::Rejected(RejectedAccess::reason(
                "malformed",
            )));
        };
        let Some(found) = self
            .store
            .sessions()
            .find_by_access_hash(&hash)
            .await?
            .filter(|found| found.session.access_hash.ct_eq(&hash))
        else {
            return Err(AuthenticateError::Rejected(RejectedAccess::reason(
                "unknown",
            )));
        };
        let session = &found.session;
        let now = self.clock.now();
        access_decision(
            &self.policy,
            AccessFacts {
                times: times(session),
                revoked: session.revoked_at.is_some(),
                user_disabled: found.disabled,
            },
            now,
        )
        .map_err(|rejection| {
            AuthenticateError::Rejected(RejectedAccess {
                reason: rejection.as_str(),
                session_id: Some(session.id),
                user_id: Some(session.user_id),
            })
        })?;
        if touch_due(session.last_used_at, now) {
            self.touch(session.id, now).await;
        }
        Ok(Authenticated {
            actor: Actor {
                id: session.user_id,
                role: found.role,
            },
            session_id: session.id,
            stepped_up_at: session.stepped_up_at,
        })
    }

    /// Notes session `id`'s use at `now`; a failure is only logged.
    async fn touch(&self, id: SessionId, now: DateTime<Utc>) {
        if let Err(error) = self.try_touch(id, now).await {
            debug!(session_id = %id, %error, "the session's last use wasn't written");
        }
    }

    /// The touch: the wait for the write connection is bounded by
    /// [`TOUCH_WAIT`]; the conditional write and the commit then run to the
    /// end.
    async fn try_touch(&self, id: SessionId, now: DateTime<Utc>) -> Result<(), TouchError> {
        let mut tx = tokio::time::timeout(TOUCH_WAIT, self.store.write())
            .await
            .map_err(|_elapsed| TouchError::TimedOut)??;
        tx.sessions().touch(id, now, touch_stale_since(now)).await?;
        tx.commit_unaudited(UnauditedWrite::SessionTouch).await?;
        Ok(())
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
        let access_expires_at = self.policy.access_expiry(times);
        let refresh_expires_at = self.policy.refresh_expiry(times);
        IssuedTokens {
            session_id: id,
            access_token: access.token,
            refresh_token: refresh.token,
            access_expires_at,
            refresh_expires_at,
            access_expires_in: elapsed(times.refreshed_at, access_expires_at).as_secs(),
            refresh_expires_in: elapsed(times.refreshed_at, refresh_expires_at).as_secs(),
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
