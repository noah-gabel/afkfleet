//! Enrolling agents (Plan.md P10.3; ADR-0019, S15–S17, S21):
//! [`EnrollmentTokenService`] creates the single-use tokens, and
//! [`EnrollmentService`] trades one for an agent's certificate.
//!
//! - **Creating a token** ([`EnrollmentTokenService::create`]): the token is
//!   minted from the `SecureRandom` port before the transaction (`afk_et_…`,
//!   stored only as its SHA-256 hash) and lives 15 minutes. Only the server's
//!   host operator creates one, with `afkfleet-server agent
//!   create-enrollment-token` (ADR-0020, U76). At most 20 tokens may be
//!   pending. The answer is the [`EnrollmentBundle`] (the token and the CA's
//!   fingerprint), shown once.
//! - **Enrolling** ([`EnrollmentService::enroll`]), in S21's order: the
//!   token on the read pool (one answer for every problem; a real token that
//!   can't be used is audited, at most 10 times per token), the name, then
//!   the agent's ID is minted and the CSR checked and signed in
//!   `spawn_blocking`, outside any transaction. One transaction then
//!   re-checks the token, creates the agent, stores its certificate's
//!   fingerprint and uses the token, so of two enrollments with one token
//!   exactly one gets a certificate. A certificate that loses is dropped and
//!   never sent.
//! - **The deadline** bounds only the token, the name, the CSR's check and
//!   the signing, which write nothing ([`EnrollError::TooSlow`]). A
//!   refusal's record and the enrollment's transaction run to their end,
//!   bounded by the write pool's acquire timeout and SQLite's busy timeout:
//!   sqlx finishes a COMMIT already sent even when its future is dropped,
//!   so a deadline there could store an agent whose certificate is never
//!   sent.

use core::fmt;
use core::time::Duration;
use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::enrollment::{
    EnrollmentBundle, MAX_AUDITED_REFUSALS, MAX_PENDING_TOKENS, TOKEN_TTL,
};
use fleet_core::id::{AgentId, EnrollmentTokenId};
use fleet_core::pki::CertFingerprint;
use fleet_core::system::{Clock, SecureRandom, mint};
use fleet_core::value::AgentName;
use fleet_proto::enrollment::EnrollRequest;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::time::timeout;
use tracing::{debug, info};

use super::audit::AuditService;
use crate::ports::agents::{InsertAgentError, NewAgent, NewAgentCert};
use crate::ports::enrollment_tokens::{EnrollmentToken, NewEnrollmentToken};
use crate::ports::pki::{AgentCertificates, CsrRefusal, IssueError};
use crate::ports::store::{Store, StoreError};
use crate::ports::tokens::{TokenHash, TokenKind, Tokens};

/// A new token: its bundle, which exists only here, and when it expires.
pub struct CreatedEnrollmentToken {
    /// The token's ID.
    pub id: EnrollmentTokenId,
    /// The token and the CA's fingerprint, for the agent.
    pub bundle: EnrollmentBundle,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for CreatedEnrollmentToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedEnrollmentToken")
            .field("id", &self.id)
            .field("bundle", &self.bundle)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Why no token was created.
#[derive(Debug, thiserror::Error)]
pub enum CreateTokenError {
    /// 20 tokens are pending already.
    #[error("the most enrollment tokens are pending already")]
    CapReached,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The token, its ID, its expiry or its audit entry couldn't be made.
    #[error("the enrollment token couldn't be made")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The tokens' use case.
#[derive(Debug, Clone)]
pub struct EnrollmentTokenService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    ca_fingerprint: CertFingerprint,
    audit: AuditService,
}

impl EnrollmentTokenService {
    /// The service over `store`, minting tokens with `tokens` and IDs from
    /// `random`, stamping with `clock`; bundles pin `ca_fingerprint`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        tokens: Arc<dyn Tokens>,
        ca_fingerprint: CertFingerprint,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            tokens,
            ca_fingerprint,
            audit,
        }
    }

    /// Creates a token; see the [module docs](self).
    ///
    /// # Errors
    /// [`CreateTokenError::CapReached`], and the server's failures.
    pub async fn create(&self) -> Result<CreatedEnrollmentToken, CreateTokenError> {
        let minted = self.tokens.mint(TokenKind::Enrollment).map_err(internal)?;
        let now = self.clock.now();
        let id: EnrollmentTokenId = mint(now, self.random.as_ref()).map_err(internal)?;
        let expires_at = now
            .checked_add_signed(TOKEN_TTL)
            .ok_or_else(|| internal(OutOfRange))?;
        let bundle = EnrollmentBundle::new(minted.token, self.ca_fingerprint).map_err(internal)?;
        let mut tx = self.store.write().await?;
        if tx.enrollment_tokens().count_pending(now).await? >= MAX_PENDING_TOKENS {
            debug!("an enrollment token was refused: too many pending");
            return Err(CreateTokenError::CapReached);
        }
        tx.enrollment_tokens()
            .insert(&NewEnrollmentToken {
                id,
                token_hash: minted.hash,
                created_at: now,
                expires_at,
            })
            .await?;
        let metadata = AuditMetadata::new()
            .time("expires_at", expires_at)
            .and_then(|metadata| metadata.text("via", "cli"))
            .map_err(internal)?;
        let entry = self
            .audit
            .entry(AuditAction::ENROLLMENT_TOKEN_CREATE, AuditOutcome::Success)
            .target(AuditTarget::EnrollmentToken(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        info!(enrollment_token_id = %id, "an enrollment token was created");
        Ok(CreatedEnrollmentToken {
            id,
            bundle,
            expires_at,
        })
    }
}

/// A failure the caller only reports.
fn internal(error: impl std::error::Error + Send + Sync + 'static) -> CreateTokenError {
    CreateTokenError::Internal(Box::new(error))
}

/// A token's expiry falls outside the times the server stores.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("the token's expiry is out of range")]
struct OutOfRange;

/// What a token was, when it couldn't enroll: for the refusal's log line and
/// its audit entry. Every one gets the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenProblem {
    /// It isn't an enrollment token's shape.
    Malformed,
    /// No token has it.
    Unknown,
    /// It expired.
    Expired,
    /// It enrolled an agent already.
    Used {
        /// The agent it enrolled.
        agent: AgentId,
    },
    /// Another enrollment used it, or it expired, after it was checked.
    Raced,
}

impl TokenProblem {
    /// The problem's name in a log line and an audit entry's `reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Unknown => "unknown",
            Self::Expired => "expired",
            Self::Used { .. } => "used",
            Self::Raced => "raced",
        }
    }
}

/// Why an agent wasn't enrolled.
#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    /// The token can't enroll: one answer, whatever the problem.
    #[error("the enrollment token isn't valid")]
    Token(TokenProblem),
    /// An enabled agent has the name already, in any case. The token stays
    /// pending.
    #[error("an enabled agent has this name already")]
    NameTaken,
    /// The CSR was refused. The token stays pending.
    #[error(transparent)]
    Csr(#[from] CsrRefusal),
    /// The database stayed busy.
    #[error("the database is busy")]
    Busy,
    /// The checks and the signing took longer than the call's deadline.
    /// Nothing was stored.
    #[error("the enrollment took longer than its deadline")]
    TooSlow,
    /// Something failed on the server's side.
    #[error("the enrollment failed")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// An enrolled agent, for its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrolled {
    /// The new agent's ID.
    pub agent: AgentId,
    /// Its name.
    pub name: AgentName,
    /// Its certificate, DER-encoded.
    pub certificate_der: Vec<u8>,
    /// The same certificate as one PEM `CERTIFICATE` section, for the
    /// agent.
    pub certificate_pem: String,
    /// When the certificate stops being valid.
    pub not_after: DateTime<Utc>,
}

/// The enrollment's use case.
#[derive(Debug, Clone)]
pub struct EnrollmentService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    certificates: Arc<dyn AgentCertificates>,
    audit: AuditService,
}

impl EnrollmentService {
    /// The service over `store`, checking tokens with `tokens`, minting
    /// agents' IDs from `random`, stamping with `clock`, and signing with
    /// `certificates`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        tokens: Arc<dyn Tokens>,
        certificates: Arc<dyn AgentCertificates>,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            tokens,
            certificates,
            audit,
        }
    }

    /// Enrolls the agent of `request`, which came from `peer`, its checks
    /// and signing within `deadline`; see the [module docs](self).
    ///
    /// # Errors
    /// [`EnrollError::Token`], [`EnrollError::NameTaken`],
    /// [`EnrollError::Csr`], [`EnrollError::TooSlow`], and the server's
    /// failures.
    pub async fn enroll(
        &self,
        request: EnrollRequest,
        peer: IpAddr,
        deadline: Duration,
    ) -> Result<Enrolled, EnrollError> {
        let now = self.clock.now();
        // The deadline bounds only the checks and the signing, which write
        // nothing, so dropping them leaves nothing behind. The writes that
        // follow run to their end, bounded by the write pool's acquire
        // timeout and SQLite's busy timeout: sqlx finishes a COMMIT already
        // sent even when its future is dropped, so cutting the transaction
        // off could store an agent whose certificate is never sent.
        let checked = timeout(deadline, self.check(request, now))
            .await
            .unwrap_or(Err(EnrollError::TooSlow))?;
        let signed = match checked {
            Checked::Signed(signed) => signed,
            Checked::Refused(refusal) => return self.refuse(refusal, peer).await,
        };
        self.store_enrollment(&signed, peer, now).await?;
        let Signed {
            token, enrolled, ..
        } = signed;
        info!(agent_id = %enrolled.agent, enrollment_token_id = %token, not_after = %enrolled.not_after, "an agent enrolled");
        Ok(enrolled)
    }

    /// S21's checks of `request` at `now`, and the signing: the token, the
    /// name, then the CSR. Nothing here writes; a token's refusal is
    /// recorded afterwards.
    async fn check(
        &self,
        request: EnrollRequest,
        now: DateTime<Utc>,
    ) -> Result<Checked, EnrollError> {
        let EnrollRequest {
            token,
            name,
            csr_der,
        } = request;

        // The token, on the read pool: nothing is signed for a caller without
        // a usable one.
        let (hash, found) = match self.usable_token(&token, now).await? {
            Ok(usable) => usable,
            Err(refusal) => return Ok(Checked::Refused(refusal)),
        };
        drop(token);

        // The name, only for a token holder, so names can't be probed
        // without one.
        if self
            .store
            .agents()
            .name_taken(&name)
            .await
            .map_err(store_error)?
        {
            debug!(enrollment_token_id = %found.id, "an enrollment was refused: the name is taken");
            return Err(EnrollError::NameTaken);
        }

        // The CSR, checked and signed off the runtime and outside any
        // transaction, for an ID only the server mints.
        let agent: AgentId = mint(now, self.random.as_ref()).map_err(internal_enroll)?;
        let certificates = Arc::clone(&self.certificates);
        let issued = tokio::task::spawn_blocking(move || certificates.issue(&csr_der, agent, now))
            .await
            .map_err(internal_enroll)?
            .map_err(|error| match error {
                IssueError::Csr(refusal) => {
                    debug!(enrollment_token_id = %found.id, reason = refusal.as_str(), "an enrollment was refused: the CSR");
                    EnrollError::Csr(refusal)
                }
                other => internal_enroll(other),
            })?;

        Ok(Checked::Signed(Signed {
            hash,
            token: found.id,
            fingerprint: issued.fingerprint,
            enrolled: Enrolled {
                agent,
                name,
                certificate_der: issued.der,
                certificate_pem: issued.pem,
                not_after: issued.not_after,
            },
        }))
    }

    /// The token `presented` stands for, with its hash, if it can enroll an
    /// agent at `now`; otherwise its refusal, still to be recorded.
    async fn usable_token(
        &self,
        presented: &SecretString,
        now: DateTime<Utc>,
    ) -> Result<Result<(TokenHash, EnrollmentToken), TokenRefusal>, EnrollError> {
        let Some(hash) = self
            .tokens
            .hash_presented(TokenKind::Enrollment, presented.expose_secret())
        else {
            return Ok(Err(TokenRefusal::unrecorded(TokenProblem::Malformed)));
        };
        let Some(found) = self
            .store
            .enrollment_tokens()
            .find_by_hash(&hash)
            .await
            .map_err(store_error)?
            .filter(|found| found.token_hash.ct_eq(&hash))
        else {
            return Ok(Err(TokenRefusal::unrecorded(TokenProblem::Unknown)));
        };
        if let Some(problem) = decide(&found, now) {
            return Ok(Err(TokenRefusal {
                answer: problem,
                recorded: Some((found.id, problem)),
            }));
        }
        Ok(Ok((hash, found)))
    }

    /// Stores `signed` in one transaction: the token again, the agent, its
    /// certificate's fingerprint, and the token used, with both audit
    /// entries, or nothing.
    async fn store_enrollment(
        &self,
        signed: &Signed,
        peer: IpAddr,
        now: DateTime<Utc>,
    ) -> Result<(), EnrollError> {
        let Signed {
            hash,
            token,
            fingerprint,
            enrolled,
        } = signed;
        let (token, fingerprint) = (*token, *fingerprint);
        let agent = enrolled.agent;
        let mut tx = self.store.write().await.map_err(store_error)?;
        let again = tx
            .enrollment_tokens()
            .find_by_hash(hash)
            .await
            .map_err(store_error)?
            .filter(|again| again.token_hash.ct_eq(hash));
        let raced = match again.as_ref() {
            None => Some(TokenProblem::Raced),
            Some(again) => decide(again, now),
        };
        if let Some(problem) = raced {
            drop(tx);
            return self.refuse(TokenRefusal::raced(token, problem), peer).await;
        }
        match tx
            .agents()
            .insert(&NewAgent {
                id: agent,
                name: enrolled.name.clone(),
                created_at: now,
            })
            .await
        {
            Ok(()) => {}
            Err(InsertAgentError::NameTaken) => {
                debug!(enrollment_token_id = %token, "an enrollment was refused: the name was taken meanwhile");
                return Err(EnrollError::NameTaken);
            }
            Err(InsertAgentError::Store(error)) => return Err(store_error(error)),
        }
        tx.agents()
            .insert_cert(&NewAgentCert {
                fingerprint,
                agent,
                not_after: enrolled.not_after,
                created_at: now,
            })
            .await
            .map_err(store_error)?;
        if !tx
            .enrollment_tokens()
            .use_token(token, agent, now)
            .await
            .map_err(store_error)?
        {
            drop(tx);
            return self
                .refuse(TokenRefusal::raced(token, TokenProblem::Raced), peer)
                .await;
        }
        let used = self
            .audit
            .entry(AuditAction::ENROLLMENT_TOKEN_USE, AuditOutcome::Success)
            .ip(peer)
            .target(AuditTarget::EnrollmentToken(token))
            .metadata(
                AuditMetadata::new()
                    .id("agent", agent)
                    .map_err(internal_enroll)?,
            );
        tx.audit().record(&used).await.map_err(store_error)?;
        let metadata = AuditMetadata::new()
            .text("fingerprint", &fingerprint.to_string())
            .and_then(|metadata| metadata.time("not_after", enrolled.not_after))
            .and_then(|metadata| metadata.text("name", enrolled.name.as_str()))
            .and_then(|metadata| metadata.id("enrollment", token))
            .map_err(internal_enroll)?;
        let entry = self
            .audit
            .entry(AuditAction::AGENT_ENROLL, AuditOutcome::Success)
            .ip(peer)
            .target(AuditTarget::Agent(agent))
            .metadata(metadata);
        tx.commit(entry).await.map(drop).map_err(store_error)
    }

    /// Refuses with `refusal`'s answer, and records a real token's refusal
    /// as an `agent.enroll` Failure, at most [`MAX_AUDITED_REFUSALS`] times
    /// per token.
    async fn refuse<T>(&self, refusal: TokenRefusal, peer: IpAddr) -> Result<T, EnrollError> {
        let TokenRefusal { answer, recorded } = refusal;
        debug!(reason = answer.as_str(), enrollment_token_id = ?recorded.map(|(id, _)| id), "an enrollment was refused");
        let Some((id, problem)) = recorded else {
            return Err(EnrollError::Token(answer));
        };
        let target = AuditTarget::EnrollmentToken(id);
        let mut metadata = AuditMetadata::new()
            .text("reason", problem.as_str())
            .map_err(internal_enroll)?;
        if let TokenProblem::Used { agent } = problem {
            metadata = metadata.id("agent", agent).map_err(internal_enroll)?;
        }
        let mut tx = self.store.write().await.map_err(store_error)?;
        // Counted where it's written, so two refusals can't both take the
        // last place.
        if tx
            .audit()
            .failures_on(target, &AuditAction::AGENT_ENROLL)
            .await
            .map_err(store_error)?
            < MAX_AUDITED_REFUSALS
        {
            let entry = self
                .audit
                .entry(AuditAction::AGENT_ENROLL, AuditOutcome::Failure)
                .ip(peer)
                .target(target)
                .metadata(metadata);
            tx.commit(entry).await.map_err(store_error)?;
        }
        Err(EnrollError::Token(answer))
    }
}

/// What [`EnrollmentService::check`] decided.
enum Checked {
    /// A usable token's certificate, signed and still to be stored.
    Signed(Signed),
    /// The token can't enroll.
    Refused(TokenRefusal),
}

/// A certificate signed for a usable token.
struct Signed {
    /// The token's hash, to find it again inside the transaction.
    hash: TokenHash,
    /// The token's ID.
    token: EnrollmentTokenId,
    /// The certificate's fingerprint, for `agent_certs`.
    fingerprint: CertFingerprint,
    /// The agent and its certificate.
    enrolled: Enrolled,
}

/// A token's refusal, still to be recorded.
#[derive(Debug, Clone, Copy)]
struct TokenRefusal {
    /// The caller's answer.
    answer: TokenProblem,
    /// For a real token: its ID and what to record.
    recorded: Option<(EnrollmentTokenId, TokenProblem)>,
}

impl TokenRefusal {
    /// `answer` for a token that isn't a real one, recorded nowhere.
    const fn unrecorded(answer: TokenProblem) -> Self {
        Self {
            answer,
            recorded: None,
        }
    }

    /// A race token `token` lost to `problem`, recorded.
    const fn raced(token: EnrollmentTokenId, problem: TokenProblem) -> Self {
        Self {
            answer: TokenProblem::Raced,
            recorded: Some((token, problem)),
        }
    }
}

/// What's wrong with `token` at `now`, if anything.
fn decide(token: &EnrollmentToken, now: DateTime<Utc>) -> Option<TokenProblem> {
    if let Some(agent) = token.agent {
        return Some(TokenProblem::Used { agent });
    }
    if now >= token.expires_at {
        return Some(TokenProblem::Expired);
    }
    None
}

/// A store error as the enrollment's: busy, or a failure.
fn store_error(error: StoreError) -> EnrollError {
    match error {
        StoreError::Busy => EnrollError::Busy,
        other => EnrollError::Internal(Box::new(other)),
    }
}

/// A failure the caller only reports.
fn internal_enroll(error: impl std::error::Error + Send + Sync + 'static) -> EnrollError {
    EnrollError::Internal(Box::new(error))
}
