//! Approving and refusing a pending link (Plan.md P9.6; ADR-0018, group E):
//! someone above its owner, by the owner's current role. Approving makes
//! the account active (`account.approve`). Refusing deletes it with its
//! refresh token and grants (`account.refuse`), which frees the owner's
//! quota slot and the Minecraft account at once. Only a pending account is
//! reviewed; anything else is left to linking again or deleting.

use fleet_core::account::{AccountStatus, awaits_review};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{AuthzError, Permission, authorize};
use fleet_core::id::{AccountId, BotId};
use tracing::{debug, info};

use super::{AccountService, Reread, reread};
use crate::app::users::ActingUser;
use crate::ports::accounts::ListedAccount;
use crate::ports::audit::NewAuditEntry;
use crate::ports::reconcile::Change;
use crate::ports::store::{StoreError, WriteTx};

/// What an approval logs.
const APPROVED: &str = "a pending link was approved";
/// What a refusal logs.
const REFUSED: &str = "a pending link was refused and deleted";

/// Why a pending link wasn't approved or refused.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("reviewing the link isn't allowed")]
    Denied(#[source] AuthzError),
    /// The account isn't pending: approved, refused or reviewed already.
    #[error("the account isn't pending")]
    NotPending,
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl AccountService {
    /// Approves the pending link `id` for `by`, and answers the account as
    /// they see it now; see the [module docs](super).
    ///
    /// # Errors
    /// [`ReviewError::SessionGone`], [`ReviewError::NotFound`],
    /// [`ReviewError::Denied`], [`ReviewError::NotPending`], and the
    /// server's failures.
    pub async fn approve(
        &self,
        by: &ActingUser,
        id: AccountId,
    ) -> Result<ListedAccount, ReviewError> {
        let mut tx = self.store.write().await?;
        let account = reviewed(tx.as_mut(), by, id).await?;
        if !tx.accounts().set_status(id, AccountStatus::Active).await? {
            return Err(ReviewError::NotFound);
        }
        let bot = tx.bots().id_of_account(id).await?;
        let entry = self.entry(AuditAction::ACCOUNT_APPROVE, by, &account, None)?;
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        self.live_hints.bots(bot.as_slice());
        info!(user_id = %by.actor.id, account_id = %id, "{APPROVED}");
        Ok(ListedAccount {
            status: AccountStatus::Active,
            ..account
        })
    }

    /// Refuses the pending link `id` for `by`, deleting it; see the
    /// [module docs](super).
    ///
    /// # Errors
    /// [`ReviewError::SessionGone`], [`ReviewError::NotFound`],
    /// [`ReviewError::Denied`], [`ReviewError::NotPending`], and the
    /// server's failures.
    pub async fn refuse(&self, by: &ActingUser, id: AccountId) -> Result<(), ReviewError> {
        let mut tx = self.store.write().await?;
        let account = reviewed(tx.as_mut(), by, id).await?;
        let Some(deleted) = tx.accounts().delete(id, self.clock.now()).await? else {
            return Err(ReviewError::NotFound);
        };
        let entry = self.entry(AuditAction::ACCOUNT_REFUSE, by, &account, deleted.bot)?;
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        self.live_hints.bots(deleted.bot.as_slice());
        info!(user_id = %by.actor.id, account_id = %id, "{REFUSED}");
        Ok(())
    }

    /// A review's entry: who linked which Minecraft account, and for a
    /// refusal the bot that went with it.
    fn entry(
        &self,
        action: AuditAction,
        by: &ActingUser,
        account: &ListedAccount,
        bot: Option<BotId>,
    ) -> Result<NewAuditEntry, AuditError> {
        let mut metadata = AuditMetadata::new()
            .id("owner", account.owner.id)?
            .text("name", account.name.as_str())?;
        if let Some(uuid) = account.uuid {
            metadata = metadata.id("uuid", *uuid.as_uuid())?;
        }
        if let Some(bot) = bot {
            metadata = metadata.id("bot", bot)?;
        }
        Ok(self
            .audit
            .entry(action, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Account(account.id))
            .metadata(metadata))
    }
}

/// Reads the reviewer and the account again in `tx`, and checks that the
/// reviewer may still review it and that it's still pending.
async fn reviewed(
    tx: &mut dyn WriteTx,
    by: &ActingUser,
    id: AccountId,
) -> Result<ListedAccount, ReviewError> {
    let (actor, account) = match reread(tx, by, id).await? {
        Reread::SessionGone => {
            debug!(user_id = %by.actor.id, "a review was refused: the session is gone");
            return Err(ReviewError::SessionGone);
        }
        Reread::NotFound => return Err(ReviewError::NotFound),
        Reread::Found { actor, account, .. } => (actor, account),
    };
    authorize(&actor, Permission::ReviewLink, &account.context()).map_err(|error| {
        debug!(user_id = %by.actor.id, account_id = %id, "a review was refused");
        ReviewError::Denied(error)
    })?;
    if !awaits_review(account.status) {
        return Err(ReviewError::NotPending);
    }
    Ok(account)
}
