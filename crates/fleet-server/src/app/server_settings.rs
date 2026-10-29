//! The servers' settings' use cases (Plan.md P11.9; ADR-0020, U54,
//! S74–S80, R141, R142): what every bot on one Minecraft server shares, whoever's bot
//! it is. Admins and the Owner read them and, after a step-up, write them.
//!
//! - **Listing** ([`ServerSettingsService::list`]): every row, newest
//!   first; the handler asks `authorize()` for `ViewServerSettings` first.
//! - **Creating** ([`ServerSettingsService::create`]): one write
//!   transaction re-reads the acting user, asks `authorize()` for
//!   `ConfigureServerSettings`, counts the rows against the cap (S78),
//!   checks that the server has no row yet, stores the row at version 1
//!   and records `server_settings.create` with the server and how many
//!   texts and chat formats; never a text or a format.
//! - **Editing** ([`ServerSettingsService::update`]): the same re-read and
//!   authorization, then the row, a missing one being `NotFound`. An edit
//!   that changes nothing writes nothing (S21's order: before the version).
//!   Otherwise it refuses a stale `version` (S77) and a server another row
//!   has, writes the next version only over the version it read, and
//!   records `server_settings.update` with the server, the previous one when
//!   it changed, and the counts of texts and formats, and whether each
//!   changed.
//! - **Deleting** ([`ServerSettingsService::delete`]): the same re-read and
//!   authorization, the row, then the delete and `server_settings.delete`
//!   with the version, the server and the counts.
//! - **Previewing** ([`ServerSettingsService::preview`]): which of unsaved
//!   formats fits a sample line, cleaned as received chat is (U58, R134);
//!   pure, so nothing is read, written or recorded.
//!
//! A row whose chat formats don't read back is logged here for the chat
//! reads, which give its bots none (`unreadable_formats`, S81).
//!
//! Every write that commits tells the reconciler (`Change::Store`), so each
//! bot on the server, old and new, gets its texts at once; a change of the
//! formats alone hints too. No live events socket is told: no frame shows
//! the settings (S78).

use core::num::NonZeroU32;
use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{AuthzError, Permission, ResourceContext, authorize};
use fleet_core::chat::{ChatFormats, FormatMatch, IncomingChat};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::server_settings::server_settings_cap_reached;
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_core::value::ServerKey;
use tracing::{debug, error, info};

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::page::{Page, Paged};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};
use crate::ports::server_settings::{
    NewServerSettings, ServerSettingsChange, StoredServerSettings, UnreadableFormats,
    WriteServerSettingsError,
};
use crate::ports::store::{Store, StoreError, WriteTx};

/// Why a server's settings weren't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateServerSettingsError {
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the settings may not be created")]
    Denied(#[source] AuthzError),
    /// The fleet holds [`fleet_core::server_settings::MAX_SERVER_SETTINGS`]
    /// rows already (S78).
    #[error("the fleet holds the most server settings allowed")]
    CapReached,
    /// The server has settings already.
    #[error("the server has settings already")]
    ServerTaken,
    /// The row's ID couldn't be minted.
    #[error(transparent)]
    Mint(#[from] MintError),
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What an edit asks for: the settings as they should be, and the version
/// its editor read (S77).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettingsEdit {
    /// The version the editor read.
    pub version: NonZeroU32,
    /// The server.
    pub server: ServerKey,
    /// The conflict texts.
    pub conflict_texts: ConflictTexts,
    /// The chat formats.
    pub chat_formats: ChatFormats,
}

/// What an edit did; either way the settings are answered as they now are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditedSettings {
    /// They changed, with their audit entry, at the next version.
    Changed(StoredServerSettings),
    /// The edit asked for what's stored: nothing was written (S21).
    Unchanged(StoredServerSettings),
}

impl EditedSettings {
    /// The settings as they now are.
    #[must_use]
    pub const fn settings(&self) -> &StoredServerSettings {
        match self {
            Self::Changed(settings) | Self::Unchanged(settings) => settings,
        }
    }
}

/// Why a server's settings weren't edited.
#[derive(Debug, thiserror::Error)]
pub enum UpdateServerSettingsError {
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the settings may not be edited")]
    Denied(#[source] AuthzError),
    /// The settings don't exist (any more).
    #[error("the settings don't exist")]
    NotFound,
    /// The settings changed since their editor read them (S77).
    #[error("the settings changed since they were read")]
    Changed,
    /// Another row has the server.
    #[error("the server has settings already")]
    ServerTaken,
    /// The settings are at the last version a `u32` holds.
    #[error("the settings have no next version")]
    VersionExhausted,
    /// The guarded write found no row at the version the transaction read,
    /// which nothing on the one write connection can change in between: a
    /// bug, never a success.
    #[error("the settings' write matched no row at the version just read")]
    Lost,
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a server's settings weren't deleted.
#[derive(Debug, thiserror::Error)]
pub enum DeleteServerSettingsError {
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the settings may not be deleted")]
    Denied(#[source] AuthzError),
    /// The settings don't exist (any more).
    #[error("the settings don't exist")]
    NotFound,
    /// The delete found no row the transaction had just read: a bug, never
    /// a success.
    #[error("the settings' delete matched no row it had just read")]
    Lost,
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The servers' settings' use cases; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct ServerSettingsService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    audit: AuditService,
    reconcile: Arc<dyn Reconcile>,
}

impl ServerSettingsService {
    /// The settings over `store`, minting IDs from `clock` and `random`, with
    /// no reconciler to tell until [`ServerSettingsService::with_reconcile`].
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            audit,
            reconcile: Arc::new(NoReconcile),
        }
    }

    /// The same, telling `reconcile` to look again after each real write,
    /// so the bots on the servers it touched get their texts.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
    }

    /// The settings on `page`, newest first, and the next page's cursor.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(
        &self,
        page: Page<ServerSettingsId>,
    ) -> Result<Paged<StoredServerSettings, ServerSettingsId>, StoreError> {
        let rows = self.store.server_settings().page(page).await?;
        Ok(Paged::split(
            rows,
            page.limit,
            |row: &StoredServerSettings| row.id,
        ))
    }

    /// Gives `server` the conflict texts `texts` and the chat formats
    /// `formats` for `by`; see the [module docs](self).
    ///
    /// # Errors
    /// [`CreateServerSettingsError::SessionGone`],
    /// [`CreateServerSettingsError::Denied`],
    /// [`CreateServerSettingsError::CapReached`],
    /// [`CreateServerSettingsError::ServerTaken`], and the server's failures.
    pub async fn create(
        &self,
        by: &ActingUser,
        server: ServerKey,
        texts: ConflictTexts,
        formats: ChatFormats,
    ) -> Result<StoredServerSettings, CreateServerSettingsError> {
        let now = self.clock.now();
        let id: ServerSettingsId = mint(now, self.random.as_ref())?;
        let mut tx = self.store.write().await?;
        match configuring(tx.as_mut(), by).await? {
            None => {
                debug!(user_id = %by.actor.id, "server settings were refused: the session is gone");
                return Err(CreateServerSettingsError::SessionGone);
            }
            Some(Err(error)) => {
                debug!(user_id = %by.actor.id, "server settings were refused");
                return Err(CreateServerSettingsError::Denied(error));
            }
            Some(Ok(())) => {}
        }
        if server_settings_cap_reached(tx.server_settings().count().await?) {
            debug!(user_id = %by.actor.id, "server settings were refused: the fleet holds the most");
            return Err(CreateServerSettingsError::CapReached);
        }
        if tx.server_settings().server_taken(&server, None).await? {
            return Err(CreateServerSettingsError::ServerTaken);
        }
        let settings = NewServerSettings {
            id,
            server,
            conflict_texts: texts,
            chat_formats: formats,
            created_at: now,
        };
        tx.server_settings()
            .insert(&settings)
            .await
            .map_err(|error| match error {
                WriteServerSettingsError::ServerTaken => CreateServerSettingsError::ServerTaken,
                WriteServerSettingsError::Store(error) => CreateServerSettingsError::Store(error),
            })?;
        let metadata = with_server(
            AuditMetadata::new(),
            "server_host",
            "server_port",
            &settings.server,
        )?
        .int("texts", count(&settings.conflict_texts))?
        .int("formats", formats_count(&settings.chat_formats))?;
        let entry = self
            .audit
            .entry(AuditAction::SERVER_SETTINGS_CREATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::ServerSettings(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, server_settings_id = %id, "a server's settings were created");
        Ok(StoredServerSettings {
            id,
            server: settings.server,
            conflict_texts: settings.conflict_texts,
            chat_formats: settings.chat_formats,
            version: NonZeroU32::MIN,
            created_at: now,
            updated_at: now,
        })
    }

    /// Edits the settings `id` for `by` as `edit` asks; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`UpdateServerSettingsError::SessionGone`],
    /// [`UpdateServerSettingsError::Denied`],
    /// [`UpdateServerSettingsError::NotFound`],
    /// [`UpdateServerSettingsError::Changed`],
    /// [`UpdateServerSettingsError::ServerTaken`],
    /// [`UpdateServerSettingsError::VersionExhausted`],
    /// [`UpdateServerSettingsError::Lost`], and the server's failures.
    pub async fn update(
        &self,
        by: &ActingUser,
        id: ServerSettingsId,
        edit: ServerSettingsEdit,
    ) -> Result<EditedSettings, UpdateServerSettingsError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        match configuring(tx.as_mut(), by).await? {
            None => {
                debug!(user_id = %by.actor.id, server_settings_id = %id, "a server settings edit was refused: the session is gone");
                return Err(UpdateServerSettingsError::SessionGone);
            }
            Some(Err(error)) => {
                debug!(user_id = %by.actor.id, server_settings_id = %id, "a server settings edit was refused");
                return Err(UpdateServerSettingsError::Denied(error));
            }
            Some(Ok(())) => {}
        }
        let Some(stored) = tx.server_settings().find(id).await? else {
            return Err(UpdateServerSettingsError::NotFound);
        };
        if stored.server == edit.server
            && stored.conflict_texts == edit.conflict_texts
            && stored.chat_formats == edit.chat_formats
        {
            // Dropping the transaction writes nothing (S21).
            return Ok(EditedSettings::Unchanged(stored));
        }
        if edit.version != stored.version {
            debug!(user_id = %by.actor.id, server_settings_id = %id, "a server settings edit was refused: they changed");
            return Err(UpdateServerSettingsError::Changed);
        }
        if edit.server != stored.server
            && tx
                .server_settings()
                .server_taken(&edit.server, Some(id))
                .await?
        {
            return Err(UpdateServerSettingsError::ServerTaken);
        }
        let version = stored
            .version
            .checked_add(1)
            .ok_or(UpdateServerSettingsError::VersionExhausted)?;
        let change = ServerSettingsChange {
            id,
            expected: stored.version,
            version,
            server: edit.server,
            conflict_texts: edit.conflict_texts,
            chat_formats: edit.chat_formats,
            updated_at: now,
        };
        let wrote = tx
            .server_settings()
            .update(&change)
            .await
            .map_err(|error| match error {
                WriteServerSettingsError::ServerTaken => UpdateServerSettingsError::ServerTaken,
                WriteServerSettingsError::Store(error) => UpdateServerSettingsError::Store(error),
            })?;
        if !wrote {
            error!(server_settings_id = %id, "a server's settings write matched no row at the version its transaction read; nothing was written");
            return Err(UpdateServerSettingsError::Lost);
        }
        let metadata = edited(&stored, &change)?;
        let entry = self
            .audit
            .entry(AuditAction::SERVER_SETTINGS_UPDATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::ServerSettings(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, server_settings_id = %id, "a server's settings were changed");
        Ok(EditedSettings::Changed(StoredServerSettings {
            id,
            server: change.server,
            conflict_texts: change.conflict_texts,
            chat_formats: change.chat_formats,
            version,
            created_at: stored.created_at,
            updated_at: now,
        }))
    }

    /// Deletes the settings `id` for `by`; see the [module docs](self).
    ///
    /// # Errors
    /// [`DeleteServerSettingsError::SessionGone`],
    /// [`DeleteServerSettingsError::Denied`],
    /// [`DeleteServerSettingsError::NotFound`],
    /// [`DeleteServerSettingsError::Lost`], and the server's failures.
    pub async fn delete(
        &self,
        by: &ActingUser,
        id: ServerSettingsId,
    ) -> Result<(), DeleteServerSettingsError> {
        let mut tx = self.store.write().await?;
        match configuring(tx.as_mut(), by).await? {
            None => {
                debug!(user_id = %by.actor.id, server_settings_id = %id, "a server settings delete was refused: the session is gone");
                return Err(DeleteServerSettingsError::SessionGone);
            }
            Some(Err(error)) => {
                debug!(user_id = %by.actor.id, server_settings_id = %id, "a server settings delete was refused");
                return Err(DeleteServerSettingsError::Denied(error));
            }
            Some(Ok(())) => {}
        }
        let Some(stored) = tx.server_settings().find(id).await? else {
            return Err(DeleteServerSettingsError::NotFound);
        };
        if !tx.server_settings().delete(id).await? {
            error!(server_settings_id = %id, "a server's settings delete matched no row its transaction had just read; nothing was deleted");
            return Err(DeleteServerSettingsError::Lost);
        }
        let metadata = AuditMetadata::new().int("version", i64::from(stored.version.get()))?;
        let metadata = with_server(metadata, "server_host", "server_port", &stored.server)?
            .int("texts", count(&stored.conflict_texts))?
            .int("formats", formats_count(&stored.chat_formats))?;
        let entry = self
            .audit
            .entry(AuditAction::SERVER_SETTINGS_DELETE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::ServerSettings(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, server_settings_id = %id, "a server's settings were deleted");
        Ok(())
    }

    /// Which of `formats` fits `line` first, and the sender it reads:
    /// the line cleaned as received chat is, since it stands in for a
    /// server's text (ADR-0020, U58, R134). Nothing is read or written.
    #[must_use]
    pub fn preview(&self, formats: &ChatFormats, line: &str) -> Option<FormatMatch> {
        formats.matched(&IncomingChat::system(line))
    }
}

/// The acting user of `by`, re-read in `tx`, once `authorize()` lets them
/// write the servers' settings; `None` when the session is gone.
async fn configuring(
    tx: &mut dyn WriteTx,
    by: &ActingUser,
) -> Result<Option<Result<(), AuthzError>>, StoreError> {
    let Some(acting) = recheck(tx, by).await? else {
        return Ok(None);
    };
    Ok(Some(authorize(
        &acting.actor,
        Permission::ConfigureServerSettings,
        &ResourceContext::Global,
    )))
}

/// `metadata` with `server`'s host at `host` and its port at `port`: each
/// its own field, so neither is ever cut.
fn with_server(
    metadata: AuditMetadata,
    host: &str,
    port: &str,
    server: &ServerKey,
) -> Result<AuditMetadata, AuditError> {
    metadata
        .text(host, &server.host_text())?
        .int(port, i64::from(server.port()))
}

/// `server_settings.update`'s metadata: the version written, the server,
/// the one before only when it changed, and the counts, never a text or a
/// format (S78, R141).
fn edited(
    stored: &StoredServerSettings,
    change: &ServerSettingsChange,
) -> Result<AuditMetadata, AuditError> {
    let metadata = AuditMetadata::new().int("version", i64::from(change.version.get()))?;
    let mut metadata = with_server(metadata, "server_host", "server_port", &change.server)?;
    if change.server != stored.server {
        metadata = with_server(
            metadata,
            "previous_server_host",
            "previous_server_port",
            &stored.server,
        )?;
    }
    metadata
        .int("texts", count(&change.conflict_texts))?
        .int("previous_texts", count(&stored.conflict_texts))?
        .flag(
            "texts_changed",
            change.conflict_texts != stored.conflict_texts,
        )?
        .int("formats", formats_count(&change.chat_formats))?
        .int("previous_formats", formats_count(&stored.chat_formats))?
        .flag(
            "formats_changed",
            change.chat_formats != stored.chat_formats,
        )
}

/// How many texts `texts` holds, for an entry's metadata.
fn count(texts: &ConflictTexts) -> i64 {
    i64::try_from(texts.iter().count()).unwrap_or(i64::MAX)
}

/// How many formats `formats` holds, for an entry's metadata.
fn formats_count(formats: &ChatFormats) -> i64 {
    i64::try_from(formats.len()).unwrap_or(i64::MAX)
}

/// Logs a settings row whose server or chat formats don't read back, for
/// the chat reads that give its bots no formats (ADR-0020, S81): its ID and
/// server when they read, its rowid and the column; never a format. The
/// history logs it at each read that meets it, the rights when it turns
/// unreadable.
pub(crate) fn unreadable_formats(row: &UnreadableFormats) {
    error!(
        server_settings_id = row.id.map(tracing::field::display),
        server = row.server.as_ref().map(tracing::field::display),
        rowid = row.rowid,
        column = row.column,
        "a server's chat formats don't read back: its bots' lines show no parsed sender until they're fixed"
    );
}
