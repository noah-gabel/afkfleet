//! [`VaultService`]: what the vault's keys seal, and re-encrypting every
//! secret under the active key (Plan.md P9.2, §7.5, ADR-0018), for
//! `afkfleet-server vault status` and `vault rotate-key`.
//!
//! # Status
//! It reads only the envelopes' headers ([`key_id_of`]), never a key: how
//! many TOTP secrets and Microsoft refresh tokens each key ID seals, which
//! configured key seals nothing, which key ID seals secrets without being
//! configured, and how many envelopes are malformed.
//!
//! # Rotation
//! It re-encrypts only the envelopes under other keys than the vault's
//! active one, in one transaction, and the crypto runs before it, since a
//! write transaction holds the one write connection (ADR-0015):
//! 1. Every envelope not under the active key is opened and sealed again
//!    under it, each plaintext dropped (and wiped) at once. Every one is
//!    tried before anything is decided.
//! 2. If any doesn't open, nothing is re-encrypted, and one
//!    `vault.rotate_key` Failure entry per such envelope is recorded in one
//!    commit: one that doesn't open may mean someone changed the database.
//! 3. Otherwise one write transaction swaps each row, but only while it
//!    still holds the envelope that was read, checks that no envelope under
//!    another key is left, and commits with the `vault.rotate_key` Success
//!    entry. If anything changed meanwhile (a running server wrote), it rolls
//!    back and nothing is written.
//!
//! Nothing to re-encrypt writes nothing, and records nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome};
use fleet_core::system::Clock;
use secrecy::ExposeSecret as _;
use tracing::{error, info};

use super::audit::AuditService;
use crate::ports::audit::NewAuditEntry;
use crate::ports::envelopes::{SealedEnvelope, SealedRecord};
use crate::ports::store::{Store, StoreError};
use crate::ports::vault::{Vault, VaultError, VaultPurpose, key_id_of};

/// How many secrets of each purpose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurposeCounts {
    /// Users' TOTP secrets.
    pub totp_secrets: u64,
    /// Microsoft accounts' refresh tokens.
    pub refresh_tokens: u64,
}

impl PurposeCounts {
    /// Counts one secret of `purpose`.
    fn add(&mut self, purpose: VaultPurpose) {
        let count = match purpose {
            VaultPurpose::TotpSecret => &mut self.totp_secrets,
            VaultPurpose::MsRefreshToken => &mut self.refresh_tokens,
        };
        *count = count.saturating_add(1);
    }

    /// Whether there are none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.totp_secrets == 0 && self.refresh_tokens == 0
    }
}

/// One key ID's place in the vault.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyUsage {
    /// Whether `[vault] master_key_files` lists it.
    pub configured: bool,
    /// What it seals.
    pub counts: PurposeCounts,
}

/// What every key ID seals: each configured one, and each one an envelope
/// names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    /// The active key's ID.
    pub active: u32,
    /// Each key ID, in order.
    pub keys: BTreeMap<u32, KeyUsage>,
    /// The envelopes whose header isn't a version 1 header.
    pub malformed: PurposeCounts,
}

impl VaultStatus {
    /// Whether every secret can be opened as far as the headers tell: no
    /// envelope names a key that isn't configured, and none is malformed.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.malformed.is_empty()
            && self
                .keys
                .values()
                .all(|usage| usage.configured || usage.counts.is_empty())
    }
}

/// How a rotation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rotation {
    /// Every secret was under the active key already: nothing was written.
    NothingToDo,
    /// The secrets under other keys were re-encrypted under `key_id`.
    Rotated {
        /// The active key.
        key_id: u32,
        /// How many TOTP secrets were re-encrypted.
        totp_secrets: u64,
        /// How many refresh tokens were re-encrypted.
        refresh_tokens: u64,
    },
    /// These envelopes didn't open, so nothing was re-encrypted; each has a
    /// Failure entry.
    Refused(Vec<BadEnvelope>),
    /// A row changed while rotating, so nothing was written.
    ChangedMeanwhile,
}

/// An envelope that didn't open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadEnvelope {
    /// Its row.
    pub record: SealedRecord,
    /// Why.
    pub reason: BadReason,
    /// The key its header names, when the header is readable.
    pub key_id: Option<u32>,
}

/// Why an envelope didn't open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadReason {
    /// Its key isn't configured.
    UnknownKey,
    /// It doesn't authenticate: changed, or under another key with the same
    /// ID, or moved to another row.
    Rejected,
    /// It isn't a version 1 envelope.
    Malformed,
}

impl BadReason {
    /// The name the audit entry's `reason` holds.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownKey => "unknown_key",
            Self::Rejected => "rejected",
            Self::Malformed => "malformed",
        }
    }
}

/// Why a rotation failed before it could decide anything. Nothing was
/// written.
#[derive(Debug, thiserror::Error)]
pub enum RotateError {
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A secret couldn't be sealed again: the random source failed.
    #[error("a secret couldn't be sealed again")]
    Seal(#[source] VaultError),
    /// An audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
}

/// The vault's status and its rotation.
#[derive(Debug, Clone)]
pub struct VaultService {
    store: Arc<dyn Store>,
    audit: AuditService,
}

impl VaultService {
    /// A service over `store`, stamping its entries with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>) -> Self {
        Self {
            audit: AuditService::new(clock, Arc::clone(&store)),
            store,
        }
    }

    /// What each key ID seals, read from the envelopes' headers, with
    /// `configured` the key IDs `[vault]` lists and `active` the active one.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn status(
        &self,
        configured: &BTreeSet<u32>,
        active: u32,
    ) -> Result<VaultStatus, StoreError> {
        let mut keys: BTreeMap<u32, KeyUsage> = configured
            .iter()
            .map(|&key_id| {
                (
                    key_id,
                    KeyUsage {
                        configured: true,
                        counts: PurposeCounts::default(),
                    },
                )
            })
            .collect();
        let mut malformed = PurposeCounts::default();
        for sealed in self.store.envelopes().all().await? {
            let purpose = sealed.record.purpose();
            match key_id_of(&sealed.envelope) {
                Ok(key_id) => keys.entry(key_id).or_default().counts.add(purpose),
                Err(_) => malformed.add(purpose),
            }
        }
        Ok(VaultStatus {
            active,
            keys,
            malformed,
        })
    }

    /// Re-encrypts every secret under another key than `vault`'s active one
    /// (module docs).
    ///
    /// # Errors
    /// [`RotateError`]; nothing was written then.
    pub async fn rotate(&self, vault: &dyn Vault) -> Result<Rotation, RotateError> {
        let active = vault.active_key_id();
        let mut resealed = Vec::new();
        let mut bad = Vec::new();
        for sealed in self.store.envelopes().all().await? {
            let key_id = key_id_of(&sealed.envelope).ok();
            if key_id == Some(active) {
                continue;
            }
            match reseal(vault, &sealed)? {
                Ok(new) => resealed.push((sealed, new)),
                Err(reason) => bad.push(BadEnvelope {
                    record: sealed.record,
                    reason,
                    key_id,
                }),
            }
        }
        if !bad.is_empty() {
            self.record_refusal(&bad).await?;
            return Ok(Rotation::Refused(bad));
        }
        if resealed.is_empty() {
            return Ok(Rotation::NothingToDo);
        }
        let mut counts = PurposeCounts::default();
        for (sealed, _) in &resealed {
            counts.add(sealed.record.purpose());
        }
        let entry = self
            .audit
            .entry(AuditAction::VAULT_ROTATE_KEY, AuditOutcome::Success)
            .metadata(
                AuditMetadata::new()
                    .int("sealed_with", i64::from(active))?
                    .int("totp", count(counts.totp_secrets))?
                    .int("ms_refresh", count(counts.refresh_tokens))?,
            );
        let mut tx = self.store.write().await?;
        for (sealed, new) in &resealed {
            if !tx
                .envelopes()
                .swap(sealed.record, &sealed.envelope, new)
                .await?
            {
                // Dropping the transaction rolls every swap back.
                drop(tx);
                return Ok(changed_meanwhile());
            }
        }
        if tx.envelopes().count_not_under(active).await? > 0 {
            drop(tx);
            return Ok(changed_meanwhile());
        }
        tx.commit(entry).await?;
        info!(
            key_id = active,
            totp_secrets = counts.totp_secrets,
            refresh_tokens = counts.refresh_tokens,
            "the vault's secrets are re-encrypted under the active key"
        );
        Ok(Rotation::Rotated {
            key_id: active,
            totp_secrets: counts.totp_secrets,
            refresh_tokens: counts.refresh_tokens,
        })
    }

    /// Records one Failure entry per envelope in `bad`, in one commit, and
    /// logs each.
    async fn record_refusal(&self, bad: &[BadEnvelope]) -> Result<(), RotateError> {
        let mut entries = bad
            .iter()
            .map(|envelope| self.failure_entry(envelope))
            .collect::<Result<Vec<_>, AuditError>>()?;
        let Some(last) = entries.pop() else {
            return Ok(());
        };
        let mut tx = self.store.write().await?;
        for entry in &entries {
            tx.audit().record(entry).await?;
        }
        tx.commit(last).await?;
        for envelope in bad {
            error!(
                record = %envelope.record,
                reason = envelope.reason.as_str(),
                key_id = envelope.key_id,
                "a secret doesn't open, so nothing was re-encrypted"
            );
        }
        Ok(())
    }

    /// The Failure entry for an envelope that didn't open.
    fn failure_entry(&self, envelope: &BadEnvelope) -> Result<NewAuditEntry, AuditError> {
        let metadata = AuditMetadata::new().text("reason", envelope.reason.as_str())?;
        let metadata = match envelope.key_id {
            Some(key_id) => metadata.int("sealed_with", i64::from(key_id))?,
            None => metadata,
        };
        Ok(self
            .audit
            .entry(AuditAction::VAULT_ROTATE_KEY, AuditOutcome::Failure)
            .target(envelope.record.audit_target())
            .metadata(metadata))
    }
}

/// Opens `sealed` and seals its plaintext again under `vault`'s active key.
/// The plaintext lives only inside this function: its `SecretBox` wipes it
/// on drop. Why it didn't open is the inner error; a failure to seal is the
/// outer one.
fn reseal(
    vault: &dyn Vault,
    sealed: &SealedEnvelope,
) -> Result<Result<Vec<u8>, BadReason>, RotateError> {
    let purpose = sealed.record.purpose();
    let record = sealed.record.record();
    let plaintext = match vault.open(purpose, record, &sealed.envelope) {
        Ok(plaintext) => plaintext,
        Err(VaultError::UnknownKey { .. }) => return Ok(Err(BadReason::UnknownKey)),
        Err(VaultError::Rejected { .. }) => return Ok(Err(BadReason::Rejected)),
        Err(VaultError::Malformed) => return Ok(Err(BadReason::Malformed)),
        // `open` draws no randomness and refuses no size; kept apart anyway.
        Err(other @ (VaultError::Random(_) | VaultError::TooLarge)) => {
            return Err(RotateError::Seal(other));
        }
    };
    vault
        .seal(purpose, record, plaintext.expose_secret())
        .map(Ok)
        .map_err(RotateError::Seal)
}

/// Logs a rotation that something changed under, and says so.
fn changed_meanwhile() -> Rotation {
    error!("something changed while rotating, so nothing was written");
    Rotation::ChangedMeanwhile
}

/// A count of secrets as audit metadata; no count comes near `i64::MAX`.
fn count(secrets: u64) -> i64 {
    i64::try_from(secrets).unwrap_or(i64::MAX)
}
