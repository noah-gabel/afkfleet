//! The two settings a user changes on a bot (Plan.md P11.2; ADR-0019, S47,
//! S53, S55): the server it joins (Manage, after a step-up) and its mode
//! (Control, or Manage when the mode sends a command off the allowlist; the
//! mode must be one the user may see).
//!
//! Where an address points isn't checked here: the agent checks the address
//! it resolves before it connects (S46).

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata};
use fleet_core::authz::{AuthzError, Permission, ResourceContext, UserRef, authorize};
use fleet_core::id::{BotId, ModeId};
use fleet_core::value::{Host, ServerAddress};
use tracing::info;

use super::{BotService, BotWriteError, reread};
use crate::app::users::{ActingUser, Rechecked};
use crate::ports::reconcile::Change;

/// What a setting's write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Configured {
    /// It changed, with its audit entry.
    Changed,
    /// The bot has that setting already: nothing was written.
    Unchanged,
}

impl BotService {
    /// Points the bot `id` at `server`, for `by` (`ConfigureBot`): audited
    /// as `bot.set_server` with the address and the previous one.
    ///
    /// # Errors
    /// [`BotWriteError::SessionGone`], [`BotWriteError::NotFound`],
    /// [`BotWriteError::Denied`], and the server's failures.
    pub async fn set_server(
        &self,
        by: &ActingUser,
        id: BotId,
        server: ServerAddress,
    ) -> Result<Configured, BotWriteError> {
        let mut tx = self.store.write().await?;
        let (Rechecked { actor, .. }, bot, account) = reread(tx.as_mut(), by, id).await?;
        authorize(&actor, Permission::ConfigureBot, &account.context())
            .map_err(BotWriteError::Denied)?;
        if bot.server.as_ref() == Some(&server) || !tx.bots().set_server(id, &server).await? {
            // Nothing changed; dropping the transaction writes nothing.
            return Ok(Configured::Unchanged);
        }
        let metadata = addressed(&server, bot.server.as_ref())?;
        tx.commit(self.entry(by, AuditAction::BOT_SET_SERVER, id, metadata))
            .await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, bot_id = %id, "a bot's server address was set");
        Ok(Configured::Changed)
    }

    /// Switches the bot `id` to the mode `mode`, for `by`: `ViewMode` on the
    /// mode, then `SetBotMode` with the allowlist's verdict on its commands;
    /// audited as `bot.set_mode` with the previous mode.
    ///
    /// # Errors
    /// [`BotWriteError::SessionGone`], [`BotWriteError::NotFound`],
    /// [`BotWriteError::UnusableMode`], [`BotWriteError::Denied`], and the
    /// server's failures.
    pub async fn set_mode(
        &self,
        by: &ActingUser,
        id: BotId,
        mode: ModeId,
    ) -> Result<Configured, BotWriteError> {
        let mut tx = self.store.write().await?;
        let (Rechecked { actor, .. }, bot, account) = reread(tx.as_mut(), by, id).await?;
        // The bot first: a stranger learns nothing about the mode.
        authorize(&actor, Permission::ViewBot, &account.context())
            .map_err(BotWriteError::Denied)?;
        let Some(stored) = tx.modes().find(mode).await? else {
            return Err(BotWriteError::UnusableMode);
        };
        let owner = match stored.owner {
            None => None,
            Some(owner) => match tx.users().get(owner).await? {
                Some(user) => Some(UserRef {
                    id: user.id,
                    role: user.role,
                }),
                // A mode's owner never goes: their account can't be deleted.
                None => return Err(BotWriteError::UnusableMode),
            },
        };
        let context = ResourceContext::Mode {
            owner,
            visibility: stored.visibility,
        };
        authorize(&actor, Permission::ViewMode, &context).map_err(|error| match error {
            AuthzError::NotFound | AuthzError::Forbidden => BotWriteError::UnusableMode,
            AuthzError::WrongResource => BotWriteError::Denied(error),
        })?;
        let check = self.allowlist.check_mode(&stored.definition);
        authorize(&actor, Permission::SetBotMode(check), &account.context())
            .map_err(BotWriteError::Denied)?;
        if bot.mode == mode || !tx.bots().set_mode(id, mode).await? {
            return Ok(Configured::Unchanged);
        }
        let metadata = AuditMetadata::new()
            .id("mode", mode)?
            .id("previous_mode", bot.mode)?;
        tx.commit(self.entry(by, AuditAction::BOT_SET_MODE, id, metadata))
            .await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, bot_id = %id, mode_id = %mode, "a bot's mode was switched");
        Ok(Configured::Changed)
    }
}

/// `bot.set_server`'s metadata: the new address's host and port, and the
/// previous one's, each its own field, so neither is ever cut (S53).
fn addressed(
    server: &ServerAddress,
    previous: Option<&ServerAddress>,
) -> Result<AuditMetadata, AuditError> {
    let metadata = address(AuditMetadata::new(), "server_host", "server_port", server)?;
    match previous {
        Some(previous) => address(
            metadata,
            "previous_server_host",
            "previous_server_port",
            previous,
        ),
        None => Ok(metadata),
    }
}

/// `metadata` with `server`'s host at `host` and its port, only when it
/// gave one, at `port`.
fn address(
    metadata: AuditMetadata,
    host: &str,
    port: &str,
    server: &ServerAddress,
) -> Result<AuditMetadata, AuditError> {
    let text = match server.host() {
        Host::Domain(name) => name.clone(),
        Host::Ip(ip) => ip.to_string(),
    };
    let metadata = metadata.text(host, &text)?;
    match server.explicit_port() {
        Some(given) => metadata.int(port, i64::from(given)),
        None => Ok(metadata),
    }
}
