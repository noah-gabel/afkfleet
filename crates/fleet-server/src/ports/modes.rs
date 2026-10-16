//! The modes (Plan.md P11.1, Appendix D; ADR-0019, S38): what a bot does
//! while it's online.
//!
//! A mode's steps are stored as the JSON fleet-core's
//! [`ModeDefinition`] writes, and read back only through its validated
//! deserializer, so a stored mode is valid as soon as it's read. The built-in
//! modes have no owner; the migration seeds them with the fixed IDs
//! [`fleet_core::mode::ModePreset::id`] names. Their names and the users' own
//! modes arrive with P11.4.

use core::fmt;
use core::num::NonZeroU32;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::authz::ModeVisibility;
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::ModeDefinition;

use super::store::StoreError;

/// A stored mode.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMode {
    /// The ID.
    pub id: ModeId,
    /// Who made it; `None` for a built-in mode.
    pub owner: Option<UserId>,
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

/// Reading modes through the read connections.
#[async_trait]
pub trait ModeReads: Send + Sync + fmt::Debug {
    /// The mode `id`.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`] for a row that
    /// doesn't read back, its JSON included; the error never holds the JSON.
    async fn find(&self, id: ModeId) -> Result<Option<StoredMode>, StoreError>;
}
