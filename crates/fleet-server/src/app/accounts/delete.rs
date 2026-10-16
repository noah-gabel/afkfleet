//! Deleting an account (Plan.md P9.6, ADR-0018): Manage, in any status and
//! of any kind, offline ones too while `[accounts] allow_offline` is off, so
//! a Member can free a quota slot. Its refresh token, grants and bot go
//! with it, and the deletion is recorded as `account.delete`, naming the
//! bot. A session-token
//! request for it finds the row gone (ADR-0018, group C).

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{AuthzError, Permission, authorize};
use fleet_core::id::AccountId;
use tracing::{debug, info};

use super::{AccountService, Reread, reread};
use crate::app::users::ActingUser;
use crate::ports::accounts::{DeletedAccount, ListedAccount};
use crate::ports::audit::NewAuditEntry;
use crate::ports::store::StoreError;

/// What a deletion logs.
const DELETED: &str = "an account was deleted";

/// Why an account wasn't deleted.
#[derive(Debug, thiserror::Error)]
pub enum DeleteAccountError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("deleting the account isn't allowed")]
    Denied(#[source] AuthzError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl AccountService {
    /// Deletes the account `id` for `by`; see the [module docs](super).
    ///
    /// # Errors
    /// [`DeleteAccountError::SessionGone`], [`DeleteAccountError::NotFound`],
    /// [`DeleteAccountError::Denied`], and the server's failures.
    pub async fn delete(&self, by: &ActingUser, id: AccountId) -> Result<(), DeleteAccountError> {
        let mut tx = self.store.write().await?;
        let (actor, account) = match reread(tx.as_mut(), by, id).await? {
            Reread::SessionGone => {
                debug!(user_id = %by.actor.id, "a deletion was refused: the session is gone");
                return Err(DeleteAccountError::SessionGone);
            }
            Reread::NotFound => return Err(DeleteAccountError::NotFound),
            Reread::Found { actor, account, .. } => (actor, account),
        };
        authorize(&actor, Permission::DeleteAccount, &account.context()).map_err(|error| {
            debug!(user_id = %by.actor.id, account_id = %id, "a deletion was refused");
            DeleteAccountError::Denied(error)
        })?;
        let Some(deleted) = tx.accounts().delete(id).await? else {
            return Err(DeleteAccountError::NotFound);
        };
        tx.commit(self.deleted(by, &account, deleted)?).await?;
        info!(user_id = %by.actor.id, account_id = %id, "{DELETED}");
        Ok(())
    }

    /// The deletion's entry: whose account it was, what it was, and the bot
    /// that went with it.
    fn deleted(
        &self,
        by: &ActingUser,
        account: &ListedAccount,
        deleted: DeletedAccount,
    ) -> Result<NewAuditEntry, AuditError> {
        let mut metadata = AuditMetadata::new()
            .id("owner", account.owner.id)?
            .text("kind", account.kind.as_str())?
            .text("name", account.name.as_str())?
            .text("status", account.status.as_str())?;
        if let Some(uuid) = account.uuid {
            metadata = metadata.id("uuid", *uuid.as_uuid())?;
        }
        if let Some(bot) = deleted.bot {
            metadata = metadata.id("bot", bot)?;
        }
        Ok(self
            .audit
            .entry(AuditAction::ACCOUNT_DELETE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Account(account.id))
            .metadata(metadata))
    }
}
