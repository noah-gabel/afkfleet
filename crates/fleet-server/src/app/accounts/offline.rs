//! Creating an offline account (Plan.md P9.7, ADR-0018).
//!
//! An offline account is only a Minecraft name, for offline-mode servers in
//! development. It exists only while `[accounts] allow_offline` is on: with
//! the switch off, creating one is refused as if the route didn't exist,
//! and the ones kept from before stay untouched. It needs no approval and
//! is always active. Its name follows `McUsername`'s rules and is unique in
//! any case; a taken name is refused without naming who holds it. It counts
//! toward the Member quota like any account.
//!
//! The account's ID, its bot's (minted from the same time) and every audit
//! entry are made before the transaction, which reads the creator and their
//! session again, then the quota, then inserts the account and its bot
//! (ADR-0015; ADR-0019, S35). Each outcome that touches the database is audited:
//! `account.link` Success, or a Failure for `taken` and `over_quota`.

use fleet_core::account::{AccountKind, AccountStatus, over_quota};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::id::{AccountId, BotId};
use fleet_core::mode::ModePreset;
use fleet_core::system::{MintError, mint};
use fleet_core::value::McUsername;
use tracing::{debug, info};

use super::AccountService;
use crate::app::users::{ActingUser, recheck};
use crate::ports::accounts::{AccountCredential, InsertAccountError, McAccount};
use crate::ports::audit::NewAuditEntry;
use crate::ports::bots::NewBot;
use crate::ports::store::StoreError;

/// What creating an offline account logs.
const CREATED: &str = "an offline account was created";

/// Why an offline account wasn't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateOfflineError {
    /// `[accounts] allow_offline` is off.
    #[error("offline accounts are off")]
    Disabled,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// The Member owns their quota of accounts already.
    #[error("the member owns their quota of accounts")]
    OverQuota,
    /// An offline account has this name already, in any case.
    #[error("the name is taken")]
    Taken,
    /// The account's ID, or its bot's, couldn't be minted.
    #[error("the account's ID can't be minted")]
    Mint(#[source] MintError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The entries one creation may record.
struct Entries {
    created: NewAuditEntry,
    taken: NewAuditEntry,
    over_quota: NewAuditEntry,
}

impl AccountService {
    /// Creates the offline account `name` for `by`; see the [module
    /// docs](super).
    ///
    /// # Errors
    /// [`CreateOfflineError::Disabled`], [`CreateOfflineError::SessionGone`],
    /// [`CreateOfflineError::OverQuota`], [`CreateOfflineError::Taken`], and
    /// the server's failures.
    pub async fn create_offline(
        &self,
        by: &ActingUser,
        name: McUsername,
    ) -> Result<McAccount, CreateOfflineError> {
        if !self.settings.allow_offline {
            return Err(CreateOfflineError::Disabled);
        }
        let creator = by.actor.id;
        let now = self.clock.now();
        let id: AccountId = mint(now, self.random.as_ref()).map_err(CreateOfflineError::Mint)?;
        let bot: BotId = mint(now, self.random.as_ref()).map_err(CreateOfflineError::Mint)?;
        let entries = self.entries(by, id, bot, &name)?;
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %creator, "an offline account was refused: the session is gone");
            return Err(CreateOfflineError::SessionGone);
        };
        let owned = tx.accounts().count_owned_by(creator).await?;
        if over_quota(acting.actor.role, owned, self.settings.max_per_member.get()) {
            tx.commit(entries.over_quota).await?;
            return Err(CreateOfflineError::OverQuota);
        }
        let account = McAccount {
            id,
            owner: creator,
            name,
            created_at: now,
            credential: AccountCredential::Offline,
        };
        match tx.accounts().insert(&account).await {
            Ok(()) => {}
            Err(InsertAccountError::Taken) => {
                tx.commit(entries.taken).await?;
                return Err(CreateOfflineError::Taken);
            }
            Err(InsertAccountError::Store(error)) => return Err(error.into()),
        }
        tx.bots()
            .insert(&NewBot {
                id: bot,
                account: id,
                mode: ModePreset::Afk.id(),
            })
            .await?;
        tx.commit(entries.created).await?;
        info!(user_id = %creator, account_id = %id, "{CREATED}");
        Ok(account)
    }

    /// The creation's entries, built before its transaction.
    fn entries(
        &self,
        by: &ActingUser,
        id: AccountId,
        bot: BotId,
        name: &McUsername,
    ) -> Result<Entries, AuditError> {
        let entry = |outcome| {
            self.audit
                .entry(AuditAction::ACCOUNT_LINK, outcome)
                .actor(by.actor.id)
                .ip(by.ip)
        };
        let metadata = || {
            AuditMetadata::new()
                .text("kind", AccountKind::Offline.as_str())?
                .text("name", name.as_str())
        };
        let failure = |reason: &str| -> Result<NewAuditEntry, AuditError> {
            Ok(entry(AuditOutcome::Failure)
                .target(AuditTarget::User(by.actor.id))
                .metadata(metadata()?.text("reason", reason)?))
        };
        Ok(Entries {
            created: entry(AuditOutcome::Success)
                .target(AuditTarget::Account(id))
                .metadata(
                    metadata()?
                        .text("status", AccountStatus::Active.as_str())?
                        .id("bot", bot)?,
                ),
            taken: failure("taken")?,
            over_quota: failure("over_quota")?,
        })
    }
}
