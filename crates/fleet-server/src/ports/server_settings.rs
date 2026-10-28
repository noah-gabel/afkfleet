//! The servers' settings (Plan.md P11.9; ADR-0020, S74–S78): what every
//! bot on one Minecraft server shares, whoever's bot it is.
//!
//! A row is keyed by its server's [`ServerKey`] and holds the server's
//! [`ConflictTexts`]. It's read back only when its key is in canonical form
//! and its texts are exactly what [`ConflictTexts::iter`] writes, so a
//! stored row is valid as soon as it's read. A row changes only at the
//! version its editor read ([`ServerSettingsWrites::update`], S77), and the
//! fleet holds at most [`fleet_core::server_settings::MAX_SERVER_SETTINGS`]
//! of them, which a create counts in its own transaction (S78).

use core::fmt;
use core::num::NonZeroU32;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::disconnect::ConflictTexts;
use fleet_core::id::ServerSettingsId;
use fleet_core::value::ServerKey;

use super::page::Page;
use super::store::StoreError;

/// One server's stored settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredServerSettings {
    /// The ID.
    pub id: ServerSettingsId,
    /// The server.
    pub server: ServerKey,
    /// The kick texts that count as someone logging in with a bot's account.
    pub conflict_texts: ConflictTexts,
    /// 1 when it's created, one more on every change.
    pub version: NonZeroU32,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// Reading the servers' settings through the read connections.
#[async_trait]
pub trait ServerSettingsReads: Send + Sync + fmt::Debug {
    /// The settings `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back; the error never holds a text.
    async fn find(&self, id: ServerSettingsId) -> Result<Option<StoredServerSettings>, StoreError>;

    /// The settings on `page`, newest first by ID, one more than the page
    /// holds.
    ///
    /// # Errors
    /// [`StoreError`], [`StoreError::Corrupt`] for a row that doesn't read
    /// back.
    async fn page(
        &self,
        page: Page<ServerSettingsId>,
    ) -> Result<Vec<StoredServerSettings>, StoreError>;
}

/// A server's new settings, version 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewServerSettings {
    /// The ID.
    pub id: ServerSettingsId,
    /// The server.
    pub server: ServerKey,
    /// Its conflict texts.
    pub conflict_texts: ConflictTexts,
    /// When they were made.
    pub created_at: DateTime<Utc>,
}

/// A server's settings as an edit leaves them (ADR-0020, S77).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettingsChange {
    /// The settings.
    pub id: ServerSettingsId,
    /// The version the edit read: the row changes only while it has it.
    pub expected: NonZeroU32,
    /// The version it's stored at from now on.
    pub version: NonZeroU32,
    /// The server, which an edit may change.
    pub server: ServerKey,
    /// Its conflict texts.
    pub conflict_texts: ConflictTexts,
    /// When they changed.
    pub updated_at: DateTime<Utc>,
}

/// Why a server's settings weren't stored.
#[derive(Debug, thiserror::Error)]
pub enum WriteServerSettingsError {
    /// Another row has the server: the table's unique key refused it.
    #[error("the server has settings already")]
    ServerTaken,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading and writing the servers' settings inside a
/// [`super::store::WriteTx`], so a write decides on what its own
/// transaction reads.
#[async_trait]
pub trait ServerSettingsWrites: Send {
    /// The settings `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(
        &mut self,
        id: ServerSettingsId,
    ) -> Result<Option<StoredServerSettings>, StoreError>;

    /// How many servers have settings (ADR-0020, S78).
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count(&mut self) -> Result<u32, StoreError>;

    /// Whether `server` has settings in a row other than `except`, the one
    /// being edited.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn server_taken(
        &mut self,
        server: &ServerKey,
        except: Option<ServerSettingsId>,
    ) -> Result<bool, StoreError>;

    /// Stores `settings` at version 1.
    ///
    /// # Errors
    /// [`WriteServerSettingsError::ServerTaken`] when the unique key refuses
    /// the server, or [`WriteServerSettingsError::Store`].
    async fn insert(
        &mut self,
        settings: &NewServerSettings,
    ) -> Result<(), WriteServerSettingsError>;

    /// Stores `change` over the settings still at `change.expected`.
    /// Whether it changed.
    ///
    /// # Errors
    /// [`WriteServerSettingsError::ServerTaken`] when the unique key refuses
    /// the server, or [`WriteServerSettingsError::Store`].
    async fn update(
        &mut self,
        change: &ServerSettingsChange,
    ) -> Result<bool, WriteServerSettingsError>;

    /// Deletes the settings `id`. Whether they were there.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn delete(&mut self, id: ServerSettingsId) -> Result<bool, StoreError>;
}
