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
//! need (ADR-0020, S14).

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
}

/// Reading modes inside a [`super::store::WriteTx`], so a write that
/// depends on a mode decides on what its own transaction reads.
#[async_trait]
pub trait ModeWrites: Send {
    /// The mode `id`, read in this transaction.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back.
    async fn find(&mut self, id: ModeId) -> Result<Option<StoredMode>, StoreError>;
}
