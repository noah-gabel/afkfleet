//! [`LoginService`]: logging in and stepping up (Plan.md P7.6, P7.7, §7.2,
//! §7.4, ADR-0016).
//!
//! # The password step ([`LoginService::password_step`])
//! The lockout's attempt begins first (a locked name gets 429 before any
//! check), then the password is verified: against the user's hash, or the
//! dummy hash for an unknown name, so both cost the same. A wrong password,
//! an unknown name and a disabled user are one refusal, counted toward the
//! lockout and audited as `auth.login` Failure in the same commit; the
//! typed name is never stored. A right one never gets real tokens:
//! - a user with 2FA gets an mfa token, and a stored hash made with other
//!   argon2 parameters is re-hashed on the way (best-effort: a full queue
//!   or a failure leaves it for the next login); the new hash waits in the
//!   token's challenge;
//! - a user without 2FA gets a setup token, for enrolling first.
//!
//! # The code step ([`LoginService::code_step`])
//! The mfa token's user's attempt begins, then the code is checked (a TOTP
//! code of an allowed step after the last one used, or an unused recovery
//! code). A wrong code counts against the token (the third uses it up) and
//! toward the lockout. A right one takes the token, and one commit:
//! re-reads the user (disabled: refused and counted; password changed:
//! refused, not counted), accepts the step or uses the code with a
//! conditional write (refused, not counted, if the authenticator changed
//! meanwhile), writes the re-hashed password, inserts the session, clears
//! the lockout's streak and records `auth.login` Success.
//!
//! # Step-up ([`LoginService::step_up`])
//! The password again, counted toward the lockout when it's wrong. The
//! commit re-reads the session and the user: a session gone or revoked, or
//! a disabled user, is 401; a password changed since it was verified is
//! refused, not counted. Otherwise the session's `stepped_up_at` is stamped
//! and the streak cleared, recorded as `auth.step_up`.
//!
//! Argon2 and every other crypto run before any write transaction opens.

use core::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::SecondFactor;
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::Clock;
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};
use tracing::{debug, info, warn};

use super::audit::AuditService;
use super::auth::{IssueError, IssuedTokens, PreparedSession, TokenService};
use super::challenges::{ChallengeUser, IssuedChallenge, SetupPurpose};
use super::lockout::{Attempt, LockoutError, LockoutService, RecordFailureError};
use super::mfa::{CheckError, CodeCheck, MfaService};
use crate::ports::passwords::{HashError, Passwords, VerifyError};
use crate::ports::store::{Store, StoreError, WriteTx};
use crate::ports::tokens::MintTokenError;
use crate::ports::users::PasswordHash;

/// What the password step leads to.
#[derive(Debug)]
pub enum PasswordStep {
    /// The user has 2FA: an mfa token for the code step.
    MfaRequired(IssuedChallenge),
    /// The user has no 2FA yet: a setup token for enrolling first.
    MfaSetupRequired(IssuedChallenge),
}

/// Why a login step failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The username, the password or the code was wrong, the user is
    /// disabled, or the mfa token is gone; nothing tells which (401).
    #[error("the credentials were refused")]
    Refused,
    /// The username is locked, or another attempt for it is in flight (429).
    #[error(transparent)]
    Lockout(#[from] LockoutError),
    /// The password couldn't be checked: the queue is full, or the stored
    /// hash is corrupt.
    #[error("the password can't be checked")]
    Verify(#[source] VerifyError),
    /// The code couldn't be checked: the stored secret doesn't open.
    #[error(transparent)]
    Check(#[from] CheckError),
    /// The session couldn't be prepared.
    #[error("the session can't be issued")]
    Issue(#[source] IssueError),
    /// The login's token couldn't be minted.
    #[error("the login's token can't be minted")]
    Token(#[source] MintTokenError),
    /// A failure couldn't be recorded.
    #[error(transparent)]
    Record(#[from] RecordFailureError),
    /// An audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a step-up failed.
#[derive(Debug, thiserror::Error)]
pub enum StepUpError {
    /// The session is gone or revoked, or its user disabled (401).
    #[error("the session is over")]
    SessionGone,
    /// The password was wrong, or changed since it was checked (403).
    #[error("the password was refused")]
    Refused,
    /// The username is locked, or another attempt for it is in flight (429).
    #[error(transparent)]
    Lockout(#[from] LockoutError),
    /// The password couldn't be checked.
    #[error("the password can't be checked")]
    Verify(#[source] VerifyError),
    /// A failure couldn't be recorded.
    #[error(transparent)]
    Record(#[from] RecordFailureError),
    /// An audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Which login step refused (the `step` metadata).
#[derive(Debug, Clone, Copy)]
enum Step {
    Password,
    Code,
}

impl Step {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Code => "code",
        }
    }
}

/// Why a step refused (the `reason` metadata).
#[derive(Debug, Clone, Copy)]
enum Reason {
    WrongPassword,
    WrongCode,
    Disabled,
    PasswordChanged,
    MfaChanged,
}

impl Reason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WrongPassword => "wrong_password",
            Self::WrongCode => "wrong_code",
            Self::Disabled => "disabled",
            Self::PasswordChanged => "password_changed",
            Self::MfaChanged => "mfa_changed",
        }
    }
}

/// How the code step was passed (the `method` metadata).
const fn method(factor: &SecondFactor) -> &'static str {
    match factor {
        SecondFactor::Totp(_) => "totp",
        SecondFactor::Recovery(_) => "recovery_code",
    }
}

/// A code step whose code matched, for its commit.
struct Accepted {
    user_id: UserId,
    /// The password's `password_changed_at` when the password step checked it.
    password_changed_at: DateTime<Utc>,
    check: CodeCheck,
    rehash: Option<PasswordHash>,
    prepared: PreparedSession,
    method: &'static str,
}

/// Logging in and stepping up; see the [module docs](self).
#[derive(Clone)]
pub struct LoginService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    passwords: Arc<dyn Passwords>,
    lockout: LockoutService,
    tokens: TokenService,
    mfa: MfaService,
    audit: AuditService,
}

/// The services it works with, not their state.
impl fmt::Debug for LoginService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginService").finish_non_exhaustive()
    }
}

impl LoginService {
    /// The login over `store`, checking passwords with `passwords`, counting
    /// failures with `lockout`, issuing sessions with `tokens` and checking
    /// codes with `mfa`, stamping with `clock`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        passwords: Arc<dyn Passwords>,
        lockout: LockoutService,
        tokens: TokenService,
        mfa: MfaService,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            passwords,
            lockout,
            tokens,
            mfa,
            audit,
        }
    }

    /// The password step for `username` and `password` from `ip`; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`LoginError::Refused`], [`LoginError::Lockout`], and the server's
    /// failures.
    pub async fn password_step(
        &self,
        username: &Username,
        password: &SecretString,
        ip: IpAddr,
    ) -> Result<PasswordStep, LoginError> {
        let attempt = self.lockout.begin(username).await?;
        let found = self.store.users().find_by_username(username).await?;
        let verified = match &found {
            Some(user) => self.passwords.verify(password, &user.password_hash).await,
            None => self.passwords.verify_unknown_user(password).await,
        }
        .map_err(LoginError::Verify)?;
        let user = match found {
            Some(user) if verified && !user.disabled => user,
            Some(user) => {
                let reason = if verified {
                    Reason::Disabled
                } else {
                    Reason::WrongPassword
                };
                let tx = self.store.write().await?;
                return self
                    .refuse(
                        tx,
                        Some(&attempt),
                        Some(user.id),
                        Step::Password,
                        reason,
                        ip,
                    )
                    .await;
            }
            None => {
                let tx = self.store.write().await?;
                return self
                    .refuse(
                        tx,
                        Some(&attempt),
                        None,
                        Step::Password,
                        Reason::WrongPassword,
                        ip,
                    )
                    .await;
            }
        };
        let challenge_user = ChallengeUser {
            id: user.id,
            role: user.role,
            password_changed_at: user.password_changed_at,
        };
        if self.store.mfa().find(user.id).await?.is_none() {
            let issued = self
                .mfa
                .challenges()
                .issue_setup(challenge_user, SetupPurpose::FirstEnrollment)
                .map_err(LoginError::Token)?;
            debug!(user_id = %user.id, "a password step led to enrollment");
            return Ok(PasswordStep::MfaSetupRequired(issued));
        }
        let rehash = if self.passwords.needs_rehash(&user.password_hash) {
            self.rehash(user.id, password).await
        } else {
            None
        };
        let issued = self
            .mfa
            .challenges()
            .issue_mfa(challenge_user, rehash)
            .map_err(LoginError::Token)?;
        drop(attempt);
        Ok(PasswordStep::MfaRequired(issued))
    }

    /// The re-hash of a password that just verified against a stale hash,
    /// or `None`, logged, when the queue is full or hashing fails: the next
    /// login tries again, and this one goes on.
    async fn rehash(&self, user_id: UserId, password: &SecretString) -> Option<PasswordHash> {
        match self.passwords.rehash_verified(password).await {
            Ok(hash) => Some(hash),
            Err(HashError::Busy) => {
                debug!(
                    user_id = %user_id,
                    "a stale password hash wasn't re-hashed because the queue is full; the next login tries again"
                );
                None
            }
            Err(error) => {
                warn!(
                    user_id = %user_id,
                    error = %error,
                    "a stale password hash couldn't be re-hashed; the next login tries again"
                );
                None
            }
        }
    }

    /// The code step for `mfa_token` and `factor` from `ip`; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`LoginError::Refused`], [`LoginError::Lockout`], and the server's
    /// failures.
    pub async fn code_step(
        &self,
        mfa_token: &SecretString,
        factor: &SecondFactor,
        ip: IpAddr,
    ) -> Result<IssuedTokens, LoginError> {
        let Some(grant) = self.mfa.challenges().mfa(mfa_token.expose_secret()) else {
            debug!("a code step's mfa token was refused");
            return Err(LoginError::Refused);
        };
        let Some(user) = self.store.users().get(grant.user.id).await? else {
            return Err(LoginError::Refused);
        };
        let attempt = self.lockout.begin(&user.username).await?;
        let check = self.mfa.check_code(user.id, factor).await?;
        if matches!(check, CodeCheck::Wrong) {
            self.mfa.challenges().wrong_code(&grant);
            let tx = self.store.write().await?;
            return self
                .refuse(
                    tx,
                    Some(&attempt),
                    Some(user.id),
                    Step::Code,
                    Reason::WrongCode,
                    ip,
                )
                .await;
        }
        let prepared = self.tokens.prepare(user.id).map_err(LoginError::Issue)?;
        let Some(rehash) = self.mfa.challenges().take_mfa(&grant) else {
            return Err(LoginError::Refused);
        };
        let accepted = Accepted {
            user_id: user.id,
            password_changed_at: grant.user.password_changed_at,
            check,
            rehash,
            prepared,
            method: method(factor),
        };
        self.accept(accepted, &attempt, ip).await
    }

    /// The code step's commit for a code that matched; see the
    /// [module docs](self).
    async fn accept(
        &self,
        accepted: Accepted,
        attempt: &Attempt,
        ip: IpAddr,
    ) -> Result<IssuedTokens, LoginError> {
        let Accepted {
            user_id,
            password_changed_at,
            check,
            rehash,
            prepared,
            method,
        } = accepted;
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let Some(current) = tx.users().get(user_id).await? else {
            return Err(LoginError::Refused);
        };
        let refused = if current.disabled {
            Some((Some(attempt), Reason::Disabled))
        } else if current.password_changed_at != password_changed_at {
            Some((None, Reason::PasswordChanged))
        } else {
            None
        };
        if let Some((counted, reason)) = refused {
            return self
                .refuse(tx, counted, Some(user_id), Step::Code, reason, ip)
                .await;
        }
        let accepted = match &check {
            CodeCheck::Totp { step, confirmed_at } => {
                tx.mfa().advance_step(user_id, *step, *confirmed_at).await?
            }
            CodeCheck::Recovery { hash } => tx.mfa().use_code(user_id, hash, now).await?,
            CodeCheck::Wrong => false,
        };
        if !accepted {
            return self
                .refuse(tx, None, Some(user_id), Step::Code, Reason::MfaChanged, ip)
                .await;
        }
        let rehashed = match &rehash {
            Some(hash) => tx.users().set_password_hash(user_id, hash).await?,
            None => false,
        };
        tx.sessions().insert(&prepared.new_session).await?;
        attempt.clear(&mut *tx).await?;
        let mut metadata = AuditMetadata::new()
            .id("session_id", prepared.new_session.id)?
            .text("method", method)?;
        if rehashed {
            metadata = metadata.flag("rehashed", true)?;
        }
        let entry = self
            .audit
            .entry(AuditAction::AUTH_LOGIN, AuditOutcome::Success)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::User(user_id))
            .metadata(metadata);
        tx.commit(entry).await?;
        info!(
            user_id = %user_id,
            session_id = %prepared.new_session.id,
            method,
            "a user logged in"
        );
        Ok(prepared.tokens)
    }

    /// Records a refused login step in `tx`: the failure toward the lockout
    /// when `attempt` counts it, and `auth.login` Failure; then refuses.
    async fn refuse<T>(
        &self,
        mut tx: Box<dyn WriteTx>,
        attempt: Option<&Attempt>,
        target: Option<UserId>,
        step: Step,
        reason: Reason,
        ip: IpAddr,
    ) -> Result<T, LoginError> {
        if let Some(attempt) = attempt {
            attempt.record_failure(&mut *tx, target, ip).await?;
        }
        let metadata = AuditMetadata::new()
            .text("step", step.as_str())?
            .text("reason", reason.as_str())?;
        let mut entry = self
            .audit
            .entry(AuditAction::AUTH_LOGIN, AuditOutcome::Failure)
            .ip(ip)
            .metadata(metadata);
        if let Some(user) = target {
            entry = entry.target(AuditTarget::User(user));
        }
        tx.commit(entry).await?;
        debug!(
            user_id = target.map(tracing::field::display),
            step = step.as_str(),
            reason = reason.as_str(),
            "a login step was refused"
        );
        Err(LoginError::Refused)
    }

    /// Stamps `session_id`'s step-up when `password` is `user_id`'s, from
    /// `ip`; see the [module docs](self).
    ///
    /// # Errors
    /// [`StepUpError::SessionGone`], [`StepUpError::Refused`],
    /// [`StepUpError::Lockout`], and the server's failures.
    pub async fn step_up(
        &self,
        user_id: UserId,
        session_id: SessionId,
        password: &SecretString,
        ip: IpAddr,
    ) -> Result<(), StepUpError> {
        let Some(user) = self.store.users().get(user_id).await? else {
            return Err(StepUpError::SessionGone);
        };
        let attempt = self.lockout.begin(&user.username).await?;
        let verified = self
            .passwords
            .verify(password, &user.password_hash)
            .await
            .map_err(StepUpError::Verify)?;
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let live = tx.sessions().find(session_id).await?.is_some_and(|found| {
            found.session.revoked_at.is_none() && found.session.user_id == user_id
        });
        let current = tx.users().get(user_id).await?.filter(|user| !user.disabled);
        let (true, Some(current)) = (live, current) else {
            return Err(StepUpError::SessionGone);
        };
        if current.password_changed_at != user.password_changed_at {
            return self
                .refuse_step_up(tx, None, user_id, session_id, Reason::PasswordChanged, ip)
                .await;
        }
        if !verified {
            return self
                .refuse_step_up(
                    tx,
                    Some(&attempt),
                    user_id,
                    session_id,
                    Reason::WrongPassword,
                    ip,
                )
                .await;
        }
        if !tx.sessions().stamp_step_up(session_id, now).await? {
            return Err(StepUpError::SessionGone);
        }
        attempt.clear(&mut *tx).await?;
        let entry = self
            .audit
            .entry(AuditAction::AUTH_STEP_UP, AuditOutcome::Success)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::Session(session_id));
        tx.commit(entry).await?;
        info!(user_id = %user_id, session_id = %session_id, "a user stepped up");
        Ok(())
    }

    /// Records a refused step-up in `tx`, like [`LoginService::refuse`].
    async fn refuse_step_up(
        &self,
        mut tx: Box<dyn WriteTx>,
        attempt: Option<&Attempt>,
        user_id: UserId,
        session_id: SessionId,
        reason: Reason,
        ip: IpAddr,
    ) -> Result<(), StepUpError> {
        if let Some(attempt) = attempt {
            attempt.record_failure(&mut *tx, Some(user_id), ip).await?;
        }
        let entry = self
            .audit
            .entry(AuditAction::AUTH_STEP_UP, AuditOutcome::Failure)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::Session(session_id))
            .metadata(AuditMetadata::new().text("reason", reason.as_str())?);
        tx.commit(entry).await?;
        debug!(
            user_id = %user_id,
            session_id = %session_id,
            reason = reason.as_str(),
            "a step-up was refused"
        );
        Err(StepUpError::Refused)
    }
}
