//! Storing a linked account (see the [module docs](super)).
//!
//! Before any transaction, the account's ID and its bot's are minted from
//! one read of the clock, which is also the account's `created_at`, and its
//! refresh token is sealed for it; every audit entry the commit may need is
//! built before each one (ADR-0015). One transaction then:
//! 1. reads the creator and the session that started the flow again: a
//!    revoked session, a logout, or a disabled or deleted creator stores
//!    nothing, and the flow fails;
//! 2. counts the creator's accounts against the Member quota (`over_quota`,
//!    recorded as an `account.link` Failure);
//! 3. inserts the account, `pending`, or `active` for the Owner, and its
//!    bot (no server, `afk`, stopped; ADR-0019, S35); an already linked
//!    Minecraft account is `taken` (recorded the same way, naming nobody in
//!    the answer);
//! 4. commits with `account.link`.
//!
//! Linking an account again (P9.6) seals the token for the account's own ID
//! and commits differently: it reads the creator and their session, then
//! the account, its owner's role and the creator's grant, and asks
//! `authorize()` again (a gone account, a lost grant or a pending account
//! stores nothing, and the flow fails); a profile with another UUID than
//! the account's ends the flow `refused`/`profile_mismatch` with an
//! `account.relink` Failure; otherwise the new envelope replaces exactly
//! the one read, the account is `active` again, with the profile's current
//! name, and `account.relink` is recorded.
//!
//! A busy database is retried with the finishing phase's backoff; any other
//! failure ends the flow `failed` with one `error` line.

use chrono::{DateTime, Utc};
use fleet_core::account::{
    AccountKind, AccountStatus, RefusalReason, new_link_status, over_quota, relinkable,
};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{Permission, authorize};
use fleet_core::id::{AccountId, BotId};
use fleet_core::mode::ModePreset;
use fleet_core::system::mint;
use tracing::{error, info, warn};

use super::poller::{Retries, Running, Target, pause};
use super::{Inner, LinkFlowStatus};
use crate::app::users::recheck;
use crate::ports::accounts::{AccountCredential, InsertAccountError, McAccount};
use crate::ports::audit::NewAuditEntry;
use crate::ports::bots::NewBot;
use crate::ports::msauth::{MinecraftSession, RefreshToken};
use crate::ports::store::StoreError;
use crate::ports::vault::VaultPurpose;

/// What a commit that can't succeed logs.
const FAILED: &str = "a link flow couldn't store the account, and the flow ended";
/// What a busy database that's retried logs.
const BUSY: &str = "the database was busy storing a link flow's account; the flow tries again";
/// What a commit for a gone session or creator logs.
const GONE: &str = "a link flow's session or creator is gone; nothing was stored";
/// What linking an account again logs when it may not any more.
const NOT_RELINKABLE: &str = "a link flow's account is gone, pending, or no longer the creator's to link again; nothing was stored";

/// The entries one commit may record.
struct Entries {
    pending: NewAuditEntry,
    active: NewAuditEntry,
    taken: NewAuditEntry,
    over_quota: NewAuditEntry,
}

/// The IDs a flow's commit stores under: a new account and its bot, or the
/// account linked again, which keeps its bot.
#[derive(Debug, Clone, Copy)]
enum Ids {
    /// A new account.
    Link { account: AccountId, bot: BotId },
    /// An account linked again.
    Relink { account: AccountId },
}

impl Ids {
    /// The account's ID.
    const fn account(self) -> AccountId {
        match self {
            Self::Link { account, .. } | Self::Relink { account } => account,
        }
    }
}

/// Stores the account `session` names with `refresh`; `None` when the flow
/// was cancelled before a commit started.
pub(super) async fn store(
    inner: &Inner,
    running: &Running,
    refresh: RefreshToken,
    session: MinecraftSession,
    retries: &mut Retries,
) -> Option<LinkFlowStatus> {
    let now = inner.ports.clock.now();
    let random = inner.ports.random.as_ref();
    let minted = match running.target {
        // The bot's ID is drawn from the same time, right after the account's.
        Target::Link => mint(now, random)
            .and_then(|account| mint(now, random).map(|bot| Ids::Link { account, bot })),
        Target::Relink { account } => Ok(Ids::Relink { account }),
    };
    let ids = match minted {
        Ok(ids) => ids,
        Err(failure) => {
            error!(flow_id = %running.id, error = %failure, "{FAILED}");
            return Some(LinkFlowStatus::Failed);
        }
    };
    let id = ids.account();
    let sealed = inner.ports.vault.seal(
        VaultPurpose::MsRefreshToken,
        id.into(),
        refresh.expose_secret().as_bytes(),
    );
    drop(refresh);
    let sealed = match sealed {
        Ok(sealed) => sealed,
        Err(failure) => {
            error!(flow_id = %running.id, error = %failure, "{FAILED}");
            return Some(LinkFlowStatus::Failed);
        }
    };
    loop {
        if running.cancel.is_cancelled() {
            return None;
        }
        let committed = match ids {
            Ids::Link { account, bot } => {
                commit(inner, running, account, bot, now, &sealed, &session).await
            }
            Ids::Relink { account } => relink(inner, running, account, &sealed, &session).await,
        };
        match committed {
            Ok(status) => return Some(status),
            Err(Failed::Store(StoreError::Busy)) => {
                let Some(delay) = retries.next(inner.ports.clock.now()) else {
                    error!(flow_id = %running.id, error = %StoreError::Busy, "{FAILED}");
                    return Some(LinkFlowStatus::Failed);
                };
                warn!(flow_id = %running.id, "{BUSY}");
                if !pause(inner, &running.cancel, delay).await {
                    return None;
                }
            }
            Err(failure) => {
                error!(flow_id = %running.id, error = %failure, "{FAILED}");
                return Some(LinkFlowStatus::Failed);
            }
        }
    }
}

/// Why one commit failed.
#[derive(Debug, thiserror::Error)]
enum Failed {
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// One transaction; see the [module docs](self). `created_at` is the time
/// `id` and `bot` were minted from.
async fn commit(
    inner: &Inner,
    running: &Running,
    id: AccountId,
    bot: BotId,
    created_at: DateTime<Utc>,
    sealed: &[u8],
    session: &MinecraftSession,
) -> Result<LinkFlowStatus, Failed> {
    let creator = running.by.actor.id;
    let entries = entries(inner, running, id, bot, session)?;
    let mut tx = inner.ports.store.write().await?;
    let Some(acting) = recheck(tx.as_mut(), &running.by).await? else {
        info!(user_id = %creator, flow_id = %running.id, "{GONE}");
        return Ok(LinkFlowStatus::Failed);
    };
    let role = acting.actor.role;
    let owned = tx.accounts().count_owned_by(creator).await?;
    if over_quota(role, owned, inner.settings.max_per_member.get()) {
        tx.commit(entries.over_quota).await?;
        return Ok(LinkFlowStatus::OverQuota);
    }
    let status = new_link_status(role);
    let account = McAccount {
        id,
        owner: creator,
        name: session.name.clone(),
        created_at,
        credential: AccountCredential::Microsoft {
            uuid: session.uuid,
            refresh_envelope: sealed.to_vec(),
            status,
        },
    };
    match tx.accounts().insert(&account).await {
        Ok(()) => {}
        Err(InsertAccountError::Taken) => {
            tx.commit(entries.taken).await?;
            return Ok(LinkFlowStatus::Taken);
        }
        Err(InsertAccountError::Store(failure)) => return Err(failure.into()),
    }
    tx.bots()
        .insert(&NewBot {
            id: bot,
            account: id,
            mode: ModePreset::Afk.id(),
        })
        .await?;
    let linked = match status {
        AccountStatus::Active => entries.active,
        AccountStatus::Pending | AccountStatus::ReauthRequired | AccountStatus::Refused(_) => {
            entries.pending
        }
    };
    tx.commit(linked).await?;
    Ok(LinkFlowStatus::Linked {
        account: id,
        status,
        name: session.name.clone(),
    })
}

/// One transaction of a link again; see the [module docs](self).
async fn relink(
    inner: &Inner,
    running: &Running,
    id: AccountId,
    sealed: &[u8],
    session: &MinecraftSession,
) -> Result<LinkFlowStatus, Failed> {
    let creator = running.by.actor.id;
    let entry = |outcome| {
        inner
            .audit
            .entry(AuditAction::ACCOUNT_RELINK, outcome)
            .actor(creator)
            .ip(running.by.ip)
            .target(AuditTarget::Account(id))
    };
    let mismatch = entry(AuditOutcome::Failure).metadata(
        AuditMetadata::new()
            .text("reason", RefusalReason::ProfileMismatch.as_str())?
            .id("flow", running.id)?,
    );
    let mut tx = inner.ports.store.write().await?;
    let Some(acting) = recheck(tx.as_mut(), &running.by).await? else {
        info!(user_id = %creator, flow_id = %running.id, "{GONE}");
        return Ok(LinkFlowStatus::Failed);
    };
    // The account as the creator may link it again now, then the envelope
    // the new one replaces.
    let listed = tx.accounts().listed(id, creator).await?.filter(|listed| {
        authorize(&acting.actor, Permission::RelinkAccount, &listed.context()).is_ok()
            && relinkable(listed.kind, listed.status)
    });
    let stored = match listed {
        Some(_) => tx.accounts().find(id).await?,
        None => None,
    };
    let (
        Some(listed),
        Some(AccountCredential::Microsoft {
            refresh_envelope, ..
        }),
    ) = (listed, stored.map(|account| account.credential))
    else {
        info!(user_id = %creator, flow_id = %running.id, account_id = %id, "{NOT_RELINKABLE}");
        return Ok(LinkFlowStatus::Failed);
    };
    if listed.uuid != Some(session.uuid) {
        tx.commit(mismatch).await?;
        return Ok(LinkFlowStatus::Refused(RefusalReason::ProfileMismatch));
    }
    if !tx
        .accounts()
        .relink(id, &refresh_envelope, sealed, &session.name)
        .await?
    {
        info!(user_id = %creator, flow_id = %running.id, account_id = %id, "{NOT_RELINKABLE}");
        return Ok(LinkFlowStatus::Failed);
    }
    let mut metadata = AuditMetadata::new()
        .id("flow", running.id)?
        .text("previous_status", listed.status.as_str())?
        .text("name", session.name.as_str())?;
    if listed.name != session.name {
        metadata = metadata.text("previous_name", listed.name.as_str())?;
    }
    tx.commit(entry(AuditOutcome::Success).metadata(metadata))
        .await?;
    Ok(LinkFlowStatus::Linked {
        account: id,
        status: AccountStatus::Active,
        name: session.name.clone(),
    })
}

/// The commit's entries, built before its transaction: a Success for
/// either status the role may decide, and a Failure for each refusal.
fn entries(
    inner: &Inner,
    running: &Running,
    id: AccountId,
    bot: BotId,
    session: &MinecraftSession,
) -> Result<Entries, AuditError> {
    let creator = running.by.actor.id;
    let entry = |outcome| {
        inner
            .audit
            .entry(AuditAction::ACCOUNT_LINK, outcome)
            .actor(creator)
            .ip(running.by.ip)
    };
    let failure = |reason: &str| -> Result<NewAuditEntry, AuditError> {
        Ok(entry(AuditOutcome::Failure)
            .target(AuditTarget::User(creator))
            .metadata(
                AuditMetadata::new()
                    .text("reason", reason)?
                    .text("kind", AccountKind::Microsoft.as_str())?
                    .text("name", session.name.as_str())?
                    .id("uuid", *session.uuid.as_uuid())?
                    .id("flow", running.id)?,
            ))
    };
    let linked = |status: AccountStatus| -> Result<NewAuditEntry, AuditError> {
        Ok(entry(AuditOutcome::Success)
            .target(AuditTarget::Account(id))
            .metadata(
                AuditMetadata::new()
                    .text("kind", AccountKind::Microsoft.as_str())?
                    .text("name", session.name.as_str())?
                    .id("uuid", *session.uuid.as_uuid())?
                    .text("status", status.as_str())?
                    .id("flow", running.id)?
                    .id("bot", bot)?,
            ))
    };
    Ok(Entries {
        pending: linked(AccountStatus::Pending)?,
        active: linked(AccountStatus::Active)?,
        taken: failure("taken")?,
        over_quota: failure("over_quota")?,
    })
}
