//! [`MfaService`]: the users' second factor (Plan.md P7.7, §7.2, ADR-0016):
//! enrolling a TOTP authenticator, confirming it, starting its replacement,
//! and checking a code at login.
//!
//! - **Enroll** ([`MfaService::enroll`]) makes a 20-byte secret from the
//!   `SecureRandom` port and keeps it in the setup token's challenge, in
//!   memory; nothing is stored. It may be called again with the same token,
//!   and each call replaces the pending secret.
//! - **Confirm** ([`MfaService::confirm`]) checks the authenticator's first
//!   code against the pending secret, takes the token out of the store
//!   once the code matched (so a second confirm with it gets 401), seals the
//!   secret with the vault (bound to the user's ID), makes 10 recovery
//!   codes, and commits everything in one transaction. That transaction
//!   re-reads the user (still enabled, password unchanged) and, for a
//!   replacement, the session that asked (still live). A first enrollment
//!   is recorded as `mfa.enroll`; a replacement swaps the authenticator and
//!   the codes, revokes every other session of the user, and is recorded as
//!   `mfa.reset`. The confirming code's step becomes the last one used, so
//!   it can't log in afterwards.
//! - **Replacing** ([`MfaService::start_replace`]) only issues a setup token
//!   bound to the session; the current authenticator keeps working until
//!   confirm.
//! - **At login** ([`MfaService::check_code`]) a TOTP code is checked against
//!   the steps `fleet_core::auth::candidate_steps` allows, after the last
//!   one used, and a recovery code against the user's unused ones. Nothing
//!   is written here: the login's commit accepts the step or uses the code
//!   with a conditional write, so a code counts once even under a race.
//!
//! The crypto (the vault, HMAC, SHA-256) runs before any write transaction
//! opens.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::{
    RECOVERY_CODE_LEN, RECOVERY_CODES, RecoveryCode, SecondFactor, TOTP_SECRET_LEN, TotpCode,
    candidate_steps,
};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::{Clock, RandomError, SecureRandom};
use secrecy::{ExposeSecret as _, ExposeSecretMut as _, SecretBox, SecretString};
use tracing::info;
use zeroize::Zeroizing;

use super::audit::AuditService;
use super::challenges::{
    ChallengeUser, Challenges, IssuedChallenge, PendingSecret, SetupGrant, SetupPurpose,
};
use crate::ports::mfa::{InsertMfaError, UserMfa};
use crate::ports::qr::{QrCodes, QrError};
use crate::ports::recovery_codes::{RecoveryCodeHash, RecoveryCodes};
use crate::ports::store::{Store, StoreError};
use crate::ports::tokens::MintTokenError;
use crate::ports::totp::{TotpCodes, TotpSecret, TotpSecretLengthError, TotpUriError};
use crate::ports::vault::{Vault, VaultError, VaultPurpose};

/// A new secret for the authenticator app: what enroll answers.
#[derive(Debug)]
pub struct Enrollment {
    /// The `otpauth://` URI.
    pub otpauth_uri: SecretString,
    /// The URI as an SVG QR code's `data:` URI.
    pub qr_svg: SecretString,
    /// The secret as base32, for typing it in.
    pub secret: SecretString,
}

/// Why enroll failed.
#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    /// The setup token's challenge is gone or over, or its user is (401).
    #[error("the setup token is no longer valid")]
    Gone,
    /// The random source failed.
    #[error("no random bytes for the TOTP secret")]
    Random(#[from] RandomError),
    /// The otpauth URI couldn't be built.
    #[error(transparent)]
    Uri(#[from] TotpUriError),
    /// The QR code couldn't be drawn.
    #[error(transparent)]
    Qr(#[from] QrError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why confirm failed.
#[derive(Debug, thiserror::Error)]
pub enum ConfirmError {
    /// Enroll hasn't made a secret yet (409).
    #[error("there's no secret to confirm: enroll first")]
    NotEnrolled,
    /// The code isn't the pending secret's (422 on `code`).
    #[error("the code doesn't match the new authenticator")]
    WrongCode,
    /// The setup token is gone, over or used, its user disabled or their
    /// password changed, or the replacing session is gone (401).
    #[error("the setup token is no longer valid")]
    Gone,
    /// A first enrollment found 2FA already there (409).
    #[error("the user already has 2FA")]
    AlreadyEnrolled,
    /// The random source failed.
    #[error("no random bytes for the recovery codes")]
    Random(#[from] RandomError),
    /// The vault couldn't seal the secret.
    #[error("the TOTP secret can't be sealed")]
    Vault(#[source] VaultError),
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why starting a replacement failed.
#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    /// The user has no 2FA to replace (409).
    #[error("the user has no 2FA to replace")]
    NotEnrolled,
    /// The session's user is gone (401).
    #[error("the user no longer exists")]
    Gone,
    /// The setup token couldn't be minted.
    #[error("the setup token can't be minted")]
    Token(#[source] MintTokenError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a code at login matched. The login's commit accepts it with a
/// conditional write.
#[derive(Debug)]
pub enum CodeCheck {
    /// A TOTP code of `step`, of the authenticator confirmed at
    /// `confirmed_at`.
    Totp {
        /// The step it matched.
        step: u64,
        /// The authenticator's `confirmed_at`, read with its secret.
        confirmed_at: DateTime<Utc>,
    },
    /// An unused recovery code.
    Recovery {
        /// Its hash.
        hash: RecoveryCodeHash,
    },
    /// Neither: a wrong, used or replayed code, or a user without 2FA.
    Wrong,
}

/// Why a code couldn't be checked: the server's fault, never the code's.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// The stored secret doesn't open: a missing or wrong vault key, or a
    /// changed row. A human must act.
    #[error("the stored TOTP secret can't be opened")]
    Vault(#[source] VaultError),
    /// The opened secret has the wrong length: a changed row.
    #[error("the stored TOTP secret has the wrong length")]
    SecretLength(#[source] TotpSecretLengthError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The 2FA service; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct MfaService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    vault: Arc<dyn Vault>,
    totp: Arc<dyn TotpCodes>,
    recovery: Arc<dyn RecoveryCodes>,
    qr: Arc<dyn QrCodes>,
    challenges: Challenges,
    audit: AuditService,
}

/// The ports the 2FA service works through.
#[derive(Debug, Clone)]
pub struct MfaPorts {
    /// TOTP's crypto.
    pub totp: Arc<dyn TotpCodes>,
    /// The recovery codes' hashes.
    pub recovery: Arc<dyn RecoveryCodes>,
    /// The QR codes.
    pub qr: Arc<dyn QrCodes>,
}

impl MfaService {
    /// The service over `store`, sealing secrets with `vault`, through
    /// `ports`, holding its challenges in `challenges` (the login's, which
    /// the setup token's extractor reads too), stamping with `clock` and
    /// drawing secrets from `random`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        vault: Arc<dyn Vault>,
        ports: MfaPorts,
        challenges: Challenges,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            vault,
            totp: ports.totp,
            recovery: ports.recovery,
            qr: ports.qr,
            challenges,
            audit,
        }
    }

    /// The login's challenges.
    #[must_use]
    pub const fn challenges(&self) -> &Challenges {
        &self.challenges
    }

    /// Makes a new pending secret for `grant`'s challenge; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`EnrollError::Gone`] when the challenge or its user is gone, and the
    /// server's failures.
    pub async fn enroll(&self, grant: &SetupGrant) -> Result<Enrollment, EnrollError> {
        let mut bytes = SecretBox::new(Box::new([0_u8; TOTP_SECRET_LEN]));
        self.random.fill(bytes.expose_secret_mut())?;
        let secret = TotpSecret::new(bytes);
        let user = self
            .store
            .users()
            .get(grant.user.id)
            .await?
            .ok_or(EnrollError::Gone)?;
        let otpauth_uri = self.totp.otpauth_uri(&secret, &user.username)?;
        let qr_svg = self.qr.svg_data_uri(&otpauth_uri)?;
        let base32 = self.totp.base32(&secret);
        if !self.challenges.set_pending(grant, secret) {
            return Err(EnrollError::Gone);
        }
        Ok(Enrollment {
            otpauth_uri,
            qr_svg,
            secret: base32,
        })
    }

    /// Confirms `grant`'s pending secret with `code`, from `ip`, and returns
    /// the recovery codes as shown; see the [module docs](self).
    ///
    /// # Errors
    /// [`ConfirmError::NotEnrolled`], [`ConfirmError::WrongCode`],
    /// [`ConfirmError::Gone`], [`ConfirmError::AlreadyEnrolled`], and the
    /// server's failures.
    pub async fn confirm(
        &self,
        grant: &SetupGrant,
        code: &TotpCode,
        ip: std::net::IpAddr,
    ) -> Result<Vec<SecretString>, ConfirmError> {
        let pending = self
            .challenges
            .pending(grant)
            .ok_or(ConfirmError::Gone)?
            .ok_or(ConfirmError::NotEnrolled)?;
        let now = self.clock.now();
        let step = candidate_steps(now, None)
            .into_iter()
            .find(|step| self.totp.matches(&pending.secret, *step, code))
            .ok_or(ConfirmError::WrongCode)?;
        let PendingSecret { secret, .. } = self
            .challenges
            .take_setup(grant, pending.generation)
            .ok_or(ConfirmError::Gone)?;
        let user_id = grant.user.id;
        let envelope = self
            .vault
            .seal(
                VaultPurpose::TotpSecret,
                user_id.into(),
                secret.expose().as_slice(),
            )
            .map_err(ConfirmError::Vault)?;
        let codes = self.recovery_codes()?;
        let hashes: Vec<RecoveryCodeHash> =
            codes.iter().map(|code| self.recovery.hash(code)).collect();
        let mfa = UserMfa {
            user_id,
            secret_envelope: envelope,
            last_used_step: step,
            confirmed_at: now,
        };
        let entry = self
            .audit
            .entry(AuditAction::MFA_ENROLL, AuditOutcome::Success)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::User(user_id));
        let mut tx = self.store.write().await?;
        let current = tx.users().get(user_id).await?;
        if !current.is_some_and(|user| {
            !user.disabled && user.password_changed_at == grant.user.password_changed_at
        }) {
            return Err(ConfirmError::Gone);
        }
        let entry = match grant.purpose {
            SetupPurpose::FirstEnrollment => {
                tx.mfa().insert(&mfa).await.map_err(|error| match error {
                    InsertMfaError::Exists => ConfirmError::AlreadyEnrolled,
                    InsertMfaError::Store(error) => ConfirmError::Store(error),
                })?;
                tx.mfa().replace_codes(user_id, &hashes).await?;
                entry
            }
            SetupPurpose::Replace { session_id } => {
                let live = tx.sessions().find(session_id).await?.is_some_and(|found| {
                    found.session.revoked_at.is_none()
                        && !found.disabled
                        && found.session.user_id == user_id
                });
                if !live {
                    return Err(ConfirmError::Gone);
                }
                if !tx.mfa().replace(&mfa).await? {
                    return Err(ConfirmError::NotEnrolled);
                }
                tx.mfa().replace_codes(user_id, &hashes).await?;
                let revoked = tx
                    .sessions()
                    .revoke_all_for_user(user_id, Some(session_id), now)
                    .await?;
                let metadata = AuditMetadata::new().int(
                    "sessions_revoked",
                    i64::try_from(revoked).unwrap_or(i64::MAX),
                )?;
                self.audit
                    .entry(AuditAction::MFA_RESET, AuditOutcome::Success)
                    .actor(user_id)
                    .ip(ip)
                    .target(AuditTarget::User(user_id))
                    .metadata(metadata)
            }
        };
        tx.commit(entry).await?;
        match grant.purpose {
            SetupPurpose::FirstEnrollment => {
                info!(user_id = %user_id, "a user enrolled their first authenticator");
            }
            SetupPurpose::Replace { session_id } => {
                info!(
                    user_id = %user_id,
                    session_id = %session_id,
                    "a user replaced their authenticator"
                );
            }
        }
        Ok(codes.iter().map(RecoveryCode::display).collect())
    }

    /// [`RECOVERY_CODES`] new recovery codes, 100 bits each.
    fn recovery_codes(&self) -> Result<Vec<RecoveryCode>, RandomError> {
        (0..RECOVERY_CODES)
            .map(|_| {
                let mut bytes = Zeroizing::new([0_u8; RECOVERY_CODE_LEN]);
                self.random.fill(bytes.as_mut_slice())?;
                Ok(RecoveryCode::from_random(&bytes))
            })
            .collect()
    }

    /// Issues a setup token that replaces `user_id`'s authenticator, bound
    /// to `session_id`, the session that asked after stepping up.
    ///
    /// # Errors
    /// [`ReplaceError::NotEnrolled`] when the user has no 2FA, and the
    /// server's failures.
    pub async fn start_replace(
        &self,
        user_id: UserId,
        session_id: SessionId,
    ) -> Result<IssuedChallenge, ReplaceError> {
        let user = self
            .store
            .users()
            .get(user_id)
            .await?
            .ok_or(ReplaceError::Gone)?;
        if self.store.mfa().find(user_id).await?.is_none() {
            return Err(ReplaceError::NotEnrolled);
        }
        self.challenges
            .issue_setup(
                ChallengeUser {
                    id: user_id,
                    role: user.role,
                    password_changed_at: user.password_changed_at,
                },
                SetupPurpose::Replace { session_id },
            )
            .map_err(ReplaceError::Token)
    }

    /// What `factor` matches for `user_id` now; see the
    /// [module docs](self). Nothing is written.
    ///
    /// # Errors
    /// [`CheckError`]: the server's failures, never a wrong code.
    pub async fn check_code(
        &self,
        user_id: UserId,
        factor: &SecondFactor,
    ) -> Result<CodeCheck, CheckError> {
        match factor {
            SecondFactor::Totp(code) => {
                let Some(mfa) = self.store.mfa().find(user_id).await? else {
                    return Ok(CodeCheck::Wrong);
                };
                let opened = self
                    .vault
                    .open(
                        VaultPurpose::TotpSecret,
                        user_id.into(),
                        &mfa.secret_envelope,
                    )
                    .map_err(CheckError::Vault)?;
                let secret = TotpSecret::from_opened(opened.expose_secret())
                    .map_err(CheckError::SecretLength)?;
                let now = self.clock.now();
                Ok(candidate_steps(now, Some(mfa.last_used_step))
                    .into_iter()
                    .find(|step| self.totp.matches(&secret, *step, code))
                    .map_or(CodeCheck::Wrong, |step| CodeCheck::Totp {
                        step,
                        confirmed_at: mfa.confirmed_at,
                    }))
            }
            SecondFactor::Recovery(code) => {
                let hash = self.recovery.hash(code);
                let found = self.store.mfa().find_unused_code(user_id, &hash).await?;
                Ok(match found {
                    Some(stored) if stored.ct_eq(&hash) => CodeCheck::Recovery { hash },
                    _ => CodeCheck::Wrong,
                })
            }
        }
    }
}
