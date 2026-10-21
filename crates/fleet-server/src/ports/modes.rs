//! The modes (Plan.md P11.1, Appendix D; ADR-0019, S38): what a bot does
//! while it's online.
//!
//! A mode's steps are stored as the JSON fleet-core's
//! [`ModeDefinition`] writes, and read back only through its validated
//! deserializer, so a stored mode is valid as soon as it's read. The built-in
//! modes have no owner; the migration seeds them with the fixed IDs
//! [`fleet_core::mode::ModePreset::id`] names. Users list the modes they may
//! see ([`ModeReads::visible`], P11.4), each with its owner's current role
//! and name ([`ListedMode`]), which `authorize()` and the API's field rules
//! need (ADR-0020, S14). A user's mode changes only at the version its
//! editor read ([`ModeWrites::update`], S18).

use core::fmt;
use core::num::NonZeroU32;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::authz::{Actor, ModeVisibility, ResourceContext, UserRef};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::{ModeDefinition, ModeName};
use fleet_core::value::Username;

use super::page::Page;
use super::store::StoreError;

/// A stored mode.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMode {
    /// The ID.
    pub id: ModeId,
    /// Who made it; `None` for a built-in mode.
    pub owner: Option<UserId>,
    /// Its name (ADR-0020, S13): a built-in mode's is its preset's.
    pub name: ModeName,
    /// Who may see it.
    pub visibility: ModeVisibility,
    /// Its steps.
    pub definition: ModeDefinition,
    /// 1 when it's created, one more on every change.
    pub version: NonZeroU32,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// Who owns a listed mode: their ID, current role and username.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeOwner {
    /// Who, with their current role.
    pub user: UserRef,
    /// Their username.
    pub username: Username,
}

/// A mode as it's listed: the stored mode and its owner, if it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedMode {
    /// The mode.
    pub mode: StoredMode,
    /// Its owner; `None` for a built-in mode.
    pub owner: Option<ModeOwner>,
}

impl ListedMode {
    /// The facts `authorize()` decides on.
    #[must_use]
    pub fn context(&self) -> ResourceContext {
        ResourceContext::Mode {
            owner: self.owner.as_ref().map(|owner| owner.user),
            visibility: self.mode.visibility,
        }
    }
}

/// Reading modes through the read connections.
#[async_trait]
pub trait ModeReads: Send + Sync + fmt::Debug {
    /// The mode `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back, its JSON included; the error never holds the JSON.
    async fn find(&self, id: ModeId) -> Result<Option<StoredMode>, StoreError>;

    /// The modes `viewer` may see on `page`, newest first by ID, one more
    /// than the page holds, each with its owner. The query repeats
    /// `authorize()`'s rule for `ViewMode` (ADR-0010): the built-in and
    /// shared modes, the viewer's own, every private one for the Owner, and
    /// Members' for an Admin, by the owner's current role.
    ///
    /// # Errors
    /// [`StoreError`], [`StoreError::Corrupt`] for a row that doesn't read
    /// back.
    async fn visible(
        &self,
        viewer: Actor,
        page: Page<ModeId>,
    ) -> Result<Vec<ListedMode>, StoreError>;

    /// The mode `id` with its owner's current role and name, for checking a
    /// write before its transaction (P11.4).
    ///
    /// # Errors
    /// [`StoreError`], [`StoreError::Corrupt`] for a row that doesn't read
    /// back.
    async fn listed(&self, id: ModeId) -> Result<Option<ListedMode>, StoreError>;
}

/// A user's new mode, version 1 (P11.4).
#[derive(Debug, Clone, PartialEq)]
pub struct NewMode {
    /// The ID.
    pub id: ModeId,
    /// Who made it.
    pub owner: UserId,
    /// Its name.
    pub name: ModeName,
    /// Who may see it.
    pub visibility: ModeVisibility,
    /// Its steps.
    pub definition: ModeDefinition,
    /// When it was made.
    pub created_at: DateTime<Utc>,
}

/// A user's mode as an edit leaves it (P11.4; ADR-0020, S18).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeChange {
    /// The mode.
    pub id: ModeId,
    /// The version the edit read: the row changes only while it has it.
    pub expected: NonZeroU32,
    /// The version it's stored at from now on.
    pub version: NonZeroU32,
    /// Its name.
    pub name: ModeName,
    /// Who may see it.
    pub visibility: ModeVisibility,
    /// Its steps.
    pub definition: ModeDefinition,
    /// When it changed.
    pub updated_at: DateTime<Utc>,
}

/// Why a mode wasn't stored.
#[derive(Debug, thiserror::Error)]
pub enum WriteModeError {
    /// One of the owner's modes, or for a shared mode a shared one, has the
    /// name, ASCII case ignored: the table's unique indexes refused it
    /// (ADR-0020, S13).
    #[error("the name is taken")]
    NameTaken,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading and writing modes inside a [`super::store::WriteTx`], so a write
/// that depends on a mode decides on what its own transaction reads.
#[async_trait]
pub trait ModeWrites: Send {
    /// The mode `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&mut self, id: ModeId) -> Result<Option<StoredMode>, StoreError>;

    /// The mode `id` with its owner's current role and name, read in this
    /// transaction (P11.4).
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn listed(&mut self, id: ModeId) -> Result<Option<ListedMode>, StoreError>;

    /// How many modes `owner` owns, private and shared together (ADR-0020,
    /// S19).
    ///
    /// # Errors
    /// [`StoreError`].
    async fn count_owned_by(&mut self, owner: UserId) -> Result<u32, StoreError>;

    /// Whether `name` is taken for a mode of `owner`, ASCII case ignored: by
    /// another of the owner's modes, or, when the mode is `shared`, by any
    /// shared mode, the built-in ones included (ADR-0020, S13). The mode
    /// `except`, the one being renamed, never takes it.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn name_taken(
        &mut self,
        owner: UserId,
        name: &ModeName,
        shared: bool,
        except: Option<ModeId>,
    ) -> Result<bool, StoreError>;

    /// Stores `mode` at version 1.
    ///
    /// # Errors
    /// [`WriteModeError::NameTaken`] when a unique index refuses the name,
    /// or [`WriteModeError::Store`].
    async fn insert(&mut self, mode: &NewMode) -> Result<(), WriteModeError>;

    /// Stores `change` over a user's mode that's still at
    /// `change.expected`; never over a built-in mode. Whether it changed.
    ///
    /// # Errors
    /// [`WriteModeError::NameTaken`] when a unique index refuses the name,
    /// or [`WriteModeError::Store`].
    async fn update(&mut self, change: &ModeChange) -> Result<bool, WriteModeError>;

    /// Deletes the user's mode `id`; never a built-in mode. Whether it was
    /// there. The bots' foreign key refuses a mode a bot runs.
    ///
    /// # Errors
    /// [`StoreError`], a mode a bot runs among them.
    async fn delete(&mut self, id: ModeId) -> Result<bool, StoreError>;
}
