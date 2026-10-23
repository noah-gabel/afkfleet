//! The flight: what a request that needs Microsoft does, under its
//! account's flight lock, in a task of its own (see the [module
//! docs](super)).
//!
//! 1. Read the account and its owner again: the flight before may have
//!    rotated the token.
//! 2. Store a kept token first, if a store failed before.
//! 3. Open the stored token, refresh it, and store the rotated one with its
//!    `account.refresh` entry.
//! 4. Start the session; check its profile; store a new name.
//!
//! Every write is a short transaction that compares the row first, and its
//! audit entry is built before the transaction opens; sealing, too, happens
//! before (ADR-0015). Logs name the account's ID and an error's text, never a
//! token, a name, a UUID or a byte.

use std::sync::Arc;

use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::id::AccountId;
use fleet_core::value::McUsername;
use secrecy::{ExposeSecret as _, SecretString};
use subtle::ConstantTimeEq as _;
use tokio::sync::OwnedMutexGuard;
use tracing::{error, warn};

use super::slot::{Flight, Slot, Unsaved};
use super::{Inner, SessionTokenError};
use crate::ports::accounts::AccountCredential;
use crate::ports::audit::NewAuditEntry;
use crate::ports::envelopes::SealedRecord;
use crate::ports::msauth::{
    MicrosoftTokens, MinecraftSession, MsAccessToken, RefreshError, RefreshToken, SessionError,
    TokenError,
};
use crate::ports::vault::VaultPurpose;

/// What a refused refresh token logs.
const REAUTH: &str = "Microsoft refused an account's refresh token; it must be linked again";
/// What a refused account logs.
const REFUSED: &str = "Minecraft refused an account; it must be linked again";
/// What a session of another profile logs.
const MISMATCH: &str =
    "an account's refresh token leads to another Minecraft profile; the account was refused";
/// What an unusable stored token logs.
const UNUSABLE: &str = "an account's stored refresh token can't be used";
/// What a failed store of a rotated token logs.
const KEPT: &str = "an account's rotated refresh token couldn't be stored; it's kept until a later request stores it";
/// What a kept token, stored at last, logs.
const KEPT_STORED: &str = "an account's kept refresh token was stored";
/// What a failed rename logs.
const RENAME_FAILED: &str =
    "an account's new Minecraft name couldn't be stored; its next session tries again";

/// How storing a rotated token ended.
enum Stored {
    /// It's stored; the row's status then.
    Done {
        envelope: Vec<u8>,
        status: AccountStatus,
    },
    /// The row holds another envelope: the account was linked again.
    Changed { envelope: Vec<u8> },
    /// The row is gone.
    Gone,
}

/// How a status change ended.
enum Marked {
    /// Changed, and audited.
    Done,
    /// The account isn't active any more: nothing changed.
    NotActive(AccountStatus),
    /// The row holds another envelope: nothing changed.
    Changed,
    /// The row is gone.
    Gone,
}

impl Marked {
    /// The answer to a request whose flight tried to set `status`.
    fn answer(self, status: AccountStatus) -> SessionTokenError {
        match self {
            Self::Done => SessionTokenError::NotActive { status },
            Self::NotActive(status) => SessionTokenError::NotActive { status },
            Self::Changed => SessionTokenError::ChangedMeanwhile,
            Self::Gone => SessionTokenError::NotFound,
        }
    }
}

/// Runs a flight for `id` with its slot's guard `flight`; see the module
/// docs.
pub(super) async fn fly(
    inner: Arc<Inner>,
    id: AccountId,
    slot: Arc<Slot>,
    mut flight: OwnedMutexGuard<Flight>,
) -> Result<MinecraftSession, SessionTokenError> {
    let account = match inner.usable(id).await {
        Ok(account) => account,
        Err(refusal) => {
            if refusal.refuses_the_account() {
                slot.clear_cache();
            }
            if matches!(refusal, SessionTokenError::NotFound) {
                flight.unsaved = None;
            }
            return Err(refusal);
        }
    };
    let (envelope, kept) = store_kept(&inner, id, &slot, &mut flight, &account.envelope).await?;
    if let Some(until) = inner.backoff_until(&slot, inner.ports.clock.now()) {
        return Err(SessionTokenError::BackingOff { until });
    }
    let token = match kept {
        Some(token) => token,
        None => open(&inner, id, &envelope)?,
    };
    if inner.stop.is_cancelled() {
        return Err(SessionTokenError::ShuttingDown);
    }
    let (access, envelope) = refresh(&inner, id, &slot, &mut flight, &token, envelope).await?;
    let mint = slot.begin_mint();
    // Bounded by the port (60 s), like the refresh.
    let session = match inner.ports.microsoft.minecraft_session(&access).await {
        Ok(session) => session,
        Err(SessionError::Failed(failure)) => {
            inner.record_failure(&slot, &failure, inner.ports.clock.now());
            return Err(SessionTokenError::Microsoft(failure));
        }
        Err(SessionError::NotOwned) => {
            return Err(refuse(&inner, &slot, id, &envelope, RefusalReason::NotOwned, None).await?);
        }
        Err(SessionError::NoProfile) => {
            return Err(
                refuse(&inner, &slot, id, &envelope, RefusalReason::NoProfile, None).await?,
            );
        }
        Err(SessionError::Xbox(refusal)) => {
            let (reason, xerr) = refusal.reason();
            return Err(refuse(&inner, &slot, id, &envelope, reason, xerr).await?);
        }
    };
    drop(access);
    if session.uuid != account.uuid {
        return Err(mismatch(&inner, &slot, id, &envelope).await?);
    }
    if session.name != account.name {
        rename(&inner, id, &account.name, &session.name).await;
    }
    slot.cache(&session, mint);
    inner.record_success(&slot);
    Ok(session)
}

/// Stores the token a failed store kept, if there is one: the envelope the
/// row holds afterwards, and the kept token once it's stored. A kept token
/// is discarded only when the row's envelope changed or the row is gone.
async fn store_kept(
    inner: &Inner,
    id: AccountId,
    slot: &Slot,
    flight: &mut Flight,
    current: &[u8],
) -> Result<(Vec<u8>, Option<RefreshToken>), SessionTokenError> {
    let Some(unsaved) = flight.unsaved.take() else {
        return Ok((current.to_vec(), None));
    };
    match store_rotation(inner, id, &unsaved).await {
        Ok(Stored::Done { envelope, status }) => {
            warn!(account_id = %id, "{KEPT_STORED}");
            if status != AccountStatus::Active {
                slot.clear_cache();
                return Err(SessionTokenError::NotActive { status });
            }
            Ok((envelope, Some(unsaved.token)))
        }
        Ok(Stored::Changed { envelope }) => Ok((envelope, None)),
        Ok(Stored::Gone) => {
            slot.clear_cache();
            Err(SessionTokenError::NotFound)
        }
        Err(failed) => {
            error!(account_id = %id, error = %failed, "{KEPT}");
            flight.unsaved = Some(unsaved);
            Err(failed)
        }
    }
}

/// Refreshes `token`, which `envelope` seals, and stores the rotated one:
/// the access token for the session, and the envelope the row now holds.
/// A store that fails keeps the token in `flight`.
async fn refresh(
    inner: &Inner,
    id: AccountId,
    slot: &Slot,
    flight: &mut Flight,
    token: &RefreshToken,
    envelope: Vec<u8>,
) -> Result<(MsAccessToken, Vec<u8>), SessionTokenError> {
    // No timeout of its own: the port bounds the call (20 s, each request
    // 15 s), and dropping it after Microsoft rotated the token would lose
    // the new one.
    let MicrosoftTokens {
        access, refresh, ..
    } = match inner.ports.microsoft.refresh(token).await {
        Ok(tokens) => tokens,
        Err(RefreshError::InvalidGrant) => {
            slot.clear_cache();
            let status = AccountStatus::ReauthRequired;
            let entry = status_entry(inner, id, status, "invalid_grant", None)?;
            let marked = mark(inner, id, &envelope, status, entry).await?;
            if matches!(marked, Marked::Done) {
                warn!(account_id = %id, "{REAUTH}");
            }
            return Err(marked.answer(status));
        }
        Err(RefreshError::Failed(failure)) => {
            inner.record_failure(slot, &failure, inner.ports.clock.now());
            return Err(SessionTokenError::Microsoft(failure));
        }
    };
    let rotated = !bool::from(
        refresh
            .expose_secret()
            .as_bytes()
            .ct_eq(token.expose_secret().as_bytes()),
    );
    let unsaved = Unsaved {
        token: refresh,
        replaces: envelope,
        rotated,
    };
    let stored = match store_rotation(inner, id, &unsaved).await {
        Ok(stored) => stored,
        Err(failed) => {
            error!(account_id = %id, error = %failed, "{KEPT}");
            flight.unsaved = Some(unsaved);
            return Err(failed);
        }
    };
    match stored {
        Stored::Done {
            envelope,
            status: AccountStatus::Active,
        } => Ok((access, envelope)),
        Stored::Done { status, .. } => {
            slot.clear_cache();
            Err(SessionTokenError::NotActive { status })
        }
        Stored::Changed { .. } => {
            slot.clear_cache();
            Err(SessionTokenError::ChangedMeanwhile)
        }
        Stored::Gone => {
            slot.clear_cache();
            Err(SessionTokenError::NotFound)
        }
    }
}

/// The refresh token `envelope` seals for `id`. One that doesn't open or
/// isn't a token logs one `error` line, without any byte of it.
fn open(inner: &Inner, id: AccountId, envelope: &[u8]) -> Result<RefreshToken, SessionTokenError> {
    let plaintext = inner
        .ports
        .vault
        .open(VaultPurpose::MsRefreshToken, id.into(), envelope)
        .map_err(|vault| {
            error!(account_id = %id, error = %vault, "{UNUSABLE}");
            SessionTokenError::Envelope(vault)
        })?;
    let not_a_token = |rule: TokenError| {
        error!(account_id = %id, error = %rule, "{UNUSABLE}");
        SessionTokenError::StoredToken(rule)
    };
    // Not `String::from_utf8`: its error would carry the bytes.
    let text = core::str::from_utf8(plaintext.expose_secret())
        .map_err(|_| not_a_token(TokenError::Character))?;
    RefreshToken::try_from(SecretString::from(text.to_owned())).map_err(not_a_token)
}

/// Stores `unsaved` for `id`, but only while the row holds the envelope it
/// replaces, with its `account.refresh` entry.
async fn store_rotation(
    inner: &Inner,
    id: AccountId,
    unsaved: &Unsaved,
) -> Result<Stored, SessionTokenError> {
    let sealed = inner
        .ports
        .vault
        .seal(
            VaultPurpose::MsRefreshToken,
            id.into(),
            unsaved.token.expose_secret().as_bytes(),
        )
        .map_err(SessionTokenError::Seal)?;
    let entry = inner
        .audit
        .entry(AuditAction::ACCOUNT_REFRESH, AuditOutcome::Success)
        .target(AuditTarget::Account(id))
        .metadata(AuditMetadata::new().flag("rotated", unsaved.rotated)?);
    let mut tx = inner.ports.store.write().await?;
    // An ID never changes its kind: a row that isn't a Microsoft account's is
    // as gone as a missing one.
    let Some(AccountCredential::Microsoft {
        refresh_envelope,
        status,
        ..
    }) = tx.accounts().find(id).await?.map(|row| row.credential)
    else {
        return Ok(Stored::Gone);
    };
    if refresh_envelope != unsaved.replaces
        || !tx
            .envelopes()
            .swap(SealedRecord::RefreshToken(id), &unsaved.replaces, &sealed)
            .await?
    {
        return Ok(Stored::Changed {
            envelope: refresh_envelope,
        });
    }
    tx.commit(entry).await?;
    Ok(Stored::Done {
        envelope: sealed,
        status,
    })
}

/// Sets `id`'s status to `status` with `entry`, but only while the account
/// is still active and holds `used`, the envelope this flight used.
async fn mark(
    inner: &Inner,
    id: AccountId,
    used: &[u8],
    status: AccountStatus,
    entry: NewAuditEntry,
) -> Result<Marked, SessionTokenError> {
    let mut tx = inner.ports.store.write().await?;
    let Some(AccountCredential::Microsoft {
        refresh_envelope,
        status: now,
        ..
    }) = tx.accounts().find(id).await?.map(|row| row.credential)
    else {
        return Ok(Marked::Gone);
    };
    if now != AccountStatus::Active {
        return Ok(Marked::NotActive(now));
    }
    if refresh_envelope != used {
        return Ok(Marked::Changed);
    }
    if !tx.accounts().set_status(id, status).await? {
        return Ok(Marked::Gone);
    }
    let bot = tx.bots().id_of_account(id).await?;
    tx.commit(entry).await?;
    inner.ports.live_hints.bots(bot.as_slice());
    Ok(Marked::Done)
}

/// The `account.set_status` entry for a change from active to `status`.
fn status_entry(
    inner: &Inner,
    id: AccountId,
    status: AccountStatus,
    reason: &str,
    xerr: Option<u32>,
) -> Result<NewAuditEntry, SessionTokenError> {
    let mut metadata = AuditMetadata::new()
        .text("status", status.as_str())?
        .text("previous_status", AccountStatus::Active.as_str())?
        .text("reason", reason)?;
    if let Some(xerr) = xerr {
        metadata = metadata.int("xerr", i64::from(xerr))?;
    }
    Ok(inner
        .audit
        .entry(AuditAction::ACCOUNT_SET_STATUS, AuditOutcome::Success)
        .target(AuditTarget::Account(id))
        .metadata(metadata))
}

/// Refuses `id` for `reason` after Minecraft's side refused it: the
/// request's answer, or why the refusal couldn't be stored (the status is
/// left alone then).
async fn refuse(
    inner: &Inner,
    slot: &Slot,
    id: AccountId,
    envelope: &[u8],
    reason: RefusalReason,
    xerr: Option<u32>,
) -> Result<SessionTokenError, SessionTokenError> {
    slot.clear_cache();
    let status = AccountStatus::Refused(reason);
    let entry = status_entry(inner, id, status, reason.as_str(), xerr)?;
    let marked = mark(inner, id, envelope, status, entry).await?;
    if matches!(marked, Marked::Done) {
        warn!(account_id = %id, reason = reason.as_str(), "{REFUSED}");
    }
    Ok(marked.answer(status))
}

/// Refuses `id` because its session's profile has another UUID: the
/// request's answer, or why the refusal couldn't be stored.
async fn mismatch(
    inner: &Inner,
    slot: &Slot,
    id: AccountId,
    envelope: &[u8],
) -> Result<SessionTokenError, SessionTokenError> {
    slot.clear_cache();
    error!(account_id = %id, "{MISMATCH}");
    let status = AccountStatus::Refused(RefusalReason::ProfileMismatch);
    let metadata = AuditMetadata::new()
        .text("status", status.as_str())?
        .text("previous_status", AccountStatus::Active.as_str())?;
    let entry = inner
        .audit
        .entry(
            AuditAction::SECURITY_ACCOUNT_PROFILE_MISMATCH,
            AuditOutcome::Failure,
        )
        .target(AuditTarget::Account(id))
        .metadata(metadata);
    let marked = mark(inner, id, envelope, status, entry).await?;
    Ok(marked.answer(status))
}

/// Stores `to` as `id`'s name, while it's still `from`, with its
/// `account.rename` entry. A failure only logs a `warn`: the session is
/// handed out with the new name anyway, and its next session tries again.
async fn rename(inner: &Inner, id: AccountId, from: &McUsername, to: &McUsername) {
    if let Err(failed) = try_rename(inner, id, from, to).await {
        warn!(account_id = %id, error = %failed, "{RENAME_FAILED}");
    }
}

async fn try_rename(
    inner: &Inner,
    id: AccountId,
    from: &McUsername,
    to: &McUsername,
) -> Result<(), SessionTokenError> {
    let entry = inner
        .audit
        .entry(AuditAction::ACCOUNT_RENAME, AuditOutcome::Success)
        .target(AuditTarget::Account(id))
        .metadata(
            AuditMetadata::new()
                .text("name", to.as_str())?
                .text("previous_name", from.as_str())?,
        );
    let mut tx = inner.ports.store.write().await?;
    if tx.accounts().rename(id, from, to).await? {
        let bot = tx.bots().id_of_account(id).await?;
        tx.commit(entry).await?;
        inner.ports.live_hints.bots(bot.as_slice());
    }
    Ok(())
}
