//! The modes' use cases (Plan.md P11.4; ADR-0010; ADR-0020, S12–S19).
//!
//! - **Listing** ([`ModeService::list`]): the modes a viewer may see,
//!   newest first, filtered by the database and checked again, row by row,
//!   with `authorize(ViewMode)`; a row the two disagree on is an internal
//!   error that shows nothing (as `GET /bots` answers, ADR-0019, S54). What
//!   each viewer sees of a mode (its owner, the rights, a command's
//!   arguments) is the HTTP layer's field rule (S14, S15).
//! - **Creating** ([`ModeService::create`]): a mode whose steps send a
//!   command off the allowlist only through the step-up route (S16); its
//!   route is checked before anything is read, a mismatch being a bug the
//!   handler should have caught (`WrongRoute`, as S10 does for chat). Then
//!   one write transaction re-reads the acting user, asks `authorize()` for
//!   `CreateMode`, counts the owner's modes against the cap (S19), checks
//!   the name (S13), stores the mode at version 1 and records `mode.create`
//!   with its visibility, whether its steps send commands, and the route;
//!   never the name or a step's text (ADR-0016).
//! - **Editing** ([`ModeService::update`]): the same route rule, then one
//!   write transaction re-reads the acting user and the mode, asks
//!   `authorize()` for `UpdateMode` with the mode's visibility and then
//!   with the requested one, and answers an edit that shows the same as the
//!   stored mode as it is, writing nothing (S21). Otherwise it refuses a
//!   stale `version` (S18) and a taken name (S13), checks that the editor
//!   could set the edited mode on every bot that uses it (`SetBotMode`,
//!   S17), writes the next version only over the version it read, records
//!   `mode.update` and tells the reconciler (`Change::Store`), so the bots
//!   get the edit.

use core::num::NonZeroU32;
use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{
    Actor, AuthzError, CommandAllowlist, CommandCheck, ModeVisibility, Permission, ResourceContext,
    UserRef, authorize,
};
use fleet_core::id::ModeId;
use fleet_core::mode::{ModeDefinition, ModeName, mode_cap_reached};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use tracing::{debug, error, info};

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::modes::{ListedMode, ModeChange, ModeOwner, NewMode, StoredMode, WriteModeError};
use crate::ports::page::{Page, Paged};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError, WriteTx};

/// Which route a mode write came through (S16): the plain one takes only
/// steps that send no command off the allowlist; the step-up twin
/// (`/with-commands`) takes any. Creating and editing each have both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRoute {
    /// `POST /modes` or `PUT /modes/{id}`.
    Plain,
    /// `POST /modes/with-commands` or `PUT /modes/{id}/with-commands`,
    /// after a step-up.
    WithCommands,
}

impl ModeRoute {
    /// Whether this route takes steps whose commands `check` judged.
    #[must_use]
    pub const fn takes(self, check: CommandCheck) -> bool {
        match self {
            Self::Plain => !check.needs_manage(),
            Self::WithCommands => true,
        }
    }

    /// Its name in an audit entry.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::WithCommands => "with_commands",
        }
    }
}

/// Why a mode wasn't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateModeError {
    /// The steps send a command off the allowlist, which only the step-up
    /// route takes: the handler should have pointed there first, so this is
    /// a bug (S16).
    #[error("the {route:?} route doesn't take these steps")]
    WrongRoute {
        /// The route it came through.
        route: ModeRoute,
    },
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the mode may not be created")]
    Denied(#[source] AuthzError),
    /// The owner owns [`fleet_core::mode::MAX_MODES_PER_OWNER`] modes
    /// already (S19).
    #[error("the owner owns the most modes allowed")]
    CapReached,
    /// The name is taken (S13).
    #[error("the name is taken")]
    NameTaken,
    /// The mode's ID couldn't be minted.
    #[error(transparent)]
    Mint(#[from] MintError),
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What an edit asks for: the mode as it should be, and the version its
/// editor read (S18).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeEdit {
    /// The version the editor read.
    pub version: NonZeroU32,
    /// The name.
    pub name: ModeName,
    /// Who may see it.
    pub visibility: ModeVisibility,
    /// The steps.
    pub definition: ModeDefinition,
}

/// What an edit did; either way the mode is answered as it now is.
#[derive(Debug, Clone, PartialEq)]
pub enum Edited {
    /// It changed, with its audit entry, at the next version.
    Changed(ListedMode),
    /// It shows the same as the stored mode: nothing was written (S21).
    Unchanged(ListedMode),
}

impl Edited {
    /// The mode as it now is.
    #[must_use]
    pub const fn mode(&self) -> &ListedMode {
        match self {
            Self::Changed(mode) | Self::Unchanged(mode) => mode,
        }
    }
}

/// Why a mode wasn't edited.
#[derive(Debug, thiserror::Error)]
pub enum UpdateModeError {
    /// The steps send a command off the allowlist, which only the step-up
    /// route takes: the handler should have pointed there first (S16).
    #[error("the {route:?} route doesn't take these steps")]
    WrongRoute {
        /// The route it came through.
        route: ModeRoute,
    },
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// The mode doesn't exist (any more).
    #[error("the mode doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("the mode may not be edited")]
    Denied(#[source] AuthzError),
    /// The mode changed since its editor read it (S18).
    #[error("the mode changed since it was read")]
    Changed,
    /// The name is taken (S13).
    #[error("the name is taken")]
    NameTaken,
    /// A bot uses the mode on which the editor couldn't set the edited mode
    /// now (S17).
    #[error("a bot the editor can't set the mode on uses it")]
    InUseByOthers,
    /// The mode is at the last version a `u32` holds.
    #[error("the mode has no next version")]
    VersionExhausted,
    /// The guarded write found no row at the version the transaction read,
    /// which nothing on the one write connection can change in between: a
    /// bug, never a success.
    #[error("the mode's write matched no row at the version just read")]
    Lost,
    /// The audit entry couldn't be built.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a list of modes couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ListModesError {
    /// The database listed a mode `authorize()` doesn't let the viewer see:
    /// a bug in the query, answered as an internal error rather than shown.
    #[error("the database listed a mode the viewer may not see")]
    Disagreement,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a list logs when the database and `authorize()` disagree.
const DISAGREEMENT: &str = "the database listed a mode authorize() refuses; nothing was shown";

/// The modes' use cases; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct ModeService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    audit: AuditService,
    allowlist: CommandAllowlist,
    reconcile: Arc<dyn Reconcile>,
}

impl ModeService {
    /// The modes over `store`, minting IDs from `clock` and `random`, with
    /// an empty allowlist until [`ModeService::with_allowlist`] sets one,
    /// and no reconciler to tell until [`ModeService::with_reconcile`].
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
            allowlist: CommandAllowlist::default(),
            reconcile: Arc::new(NoReconcile),
        }
    }

    /// The same, with the server's `[chat] command_allowlist`.
    #[must_use]
    pub fn with_allowlist(self, allowlist: CommandAllowlist) -> Self {
        Self { allowlist, ..self }
    }

    /// The same, telling `reconcile` to look again after each real edit,
    /// so the bots that use the mode get it.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
    }

    /// The mode `id` with its owner, as the read connections see it now,
    /// for a write's checks before its transaction.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn find(&self, id: ModeId) -> Result<Option<ListedMode>, StoreError> {
        self.store.modes().listed(id).await
    }

    /// Edits the mode `id` for `by` through `route` as `edit` asks; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`UpdateModeError::WrongRoute`], [`UpdateModeError::SessionGone`],
    /// [`UpdateModeError::NotFound`], [`UpdateModeError::Denied`],
    /// [`UpdateModeError::Changed`], [`UpdateModeError::NameTaken`],
    /// [`UpdateModeError::InUseByOthers`],
    /// [`UpdateModeError::VersionExhausted`], [`UpdateModeError::Lost`], and
    /// the server's failures.
    pub async fn update(
        &self,
        by: &ActingUser,
        route: ModeRoute,
        id: ModeId,
        edit: ModeEdit,
    ) -> Result<Edited, UpdateModeError> {
        let check = self.allowlist.check_mode(&edit.definition);
        if !route.takes(check) {
            return Err(UpdateModeError::WrongRoute { route });
        }
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode edit was refused: the session is gone");
            return Err(UpdateModeError::SessionGone);
        };
        let Some(stored) = tx.modes().listed(id).await? else {
            return Err(UpdateModeError::NotFound);
        };
        let context = stored.context();
        // The right to edit it as it is, then as it will be (ADR-0010).
        for visibility in [stored.mode.visibility, edit.visibility] {
            authorize(
                &acting.actor,
                Permission::UpdateMode { visibility },
                &context,
            )
            .map_err(|error| {
                debug!(user_id = %by.actor.id, mode_id = %id, "a mode edit was refused");
                UpdateModeError::Denied(error)
            })?;
        }
        if shows_the_same(&stored.mode, &edit) {
            // Dropping the transaction writes nothing (S21).
            return Ok(Edited::Unchanged(stored));
        }
        if edit.version != stored.mode.version {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode edit was refused: the mode changed");
            return Err(UpdateModeError::Changed);
        }
        // authorize() never lets anyone edit a built-in mode, the one kind
        // without an owner.
        let owner = stored
            .mode
            .owner
            .ok_or(UpdateModeError::Denied(AuthzError::Forbidden))?;
        let shared = edit.visibility == ModeVisibility::Shared;
        let checks_name =
            edit.name != stored.mode.name || (shared && stored.mode.visibility != edit.visibility);
        if checks_name
            && tx
                .modes()
                .name_taken(owner, &edit.name, shared, Some(id))
                .await?
        {
            return Err(UpdateModeError::NameTaken);
        }
        let bots = checked_bots(tx.as_mut(), &acting.actor, id, check).await?;
        let version = stored
            .mode
            .version
            .checked_add(1)
            .ok_or(UpdateModeError::VersionExhausted)?;
        let change = ModeChange {
            id,
            expected: stored.mode.version,
            version,
            name: edit.name,
            visibility: edit.visibility,
            definition: edit.definition,
            updated_at: now,
        };
        let wrote = tx
            .modes()
            .update(&change)
            .await
            .map_err(|error| match error {
                WriteModeError::NameTaken => UpdateModeError::NameTaken,
                WriteModeError::Store(error) => UpdateModeError::Store(error),
            })?;
        if !wrote {
            error!(mode_id = %id, "a mode's write matched no row at the version its transaction read; nothing was written");
            return Err(UpdateModeError::Lost);
        }
        let metadata = edited(&stored.mode, &change, check, route, bots)?;
        let entry = self
            .audit
            .entry(AuditAction::MODE_UPDATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Mode(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.reconcile.changed(Change::Store);
        info!(user_id = %by.actor.id, mode_id = %id, "a mode was edited");
        Ok(Edited::Changed(ListedMode {
            mode: StoredMode {
                id,
                owner: Some(owner),
                name: change.name,
                visibility: change.visibility,
                definition: change.definition,
                version,
                created_at: stored.mode.created_at,
                updated_at: now,
            },
            owner: stored.owner,
        }))
    }

    /// What the allowlist says of the commands `definition`'s steps send:
    /// whether one is off it, which only the step-up route takes (S16).
    #[must_use]
    pub fn command_check(&self, definition: &ModeDefinition) -> CommandCheck {
        self.allowlist.check_mode(definition)
    }

    /// Creates the mode `name` with `visibility` and `definition` for `by`,
    /// through `route`; see the [module docs](self).
    ///
    /// # Errors
    /// [`CreateModeError::WrongRoute`], [`CreateModeError::SessionGone`],
    /// [`CreateModeError::Denied`], [`CreateModeError::CapReached`],
    /// [`CreateModeError::NameTaken`], and the server's failures.
    pub async fn create(
        &self,
        by: &ActingUser,
        route: ModeRoute,
        name: ModeName,
        visibility: ModeVisibility,
        definition: ModeDefinition,
    ) -> Result<ListedMode, CreateModeError> {
        let check = self.allowlist.check_mode(&definition);
        if !route.takes(check) {
            return Err(CreateModeError::WrongRoute { route });
        }
        let now = self.clock.now();
        let id: ModeId = mint(now, self.random.as_ref())?;
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, "a mode was refused: the session is gone");
            return Err(CreateModeError::SessionGone);
        };
        authorize(
            &acting.actor,
            Permission::CreateMode { visibility },
            &ResourceContext::Global,
        )
        .map_err(|error| {
            debug!(user_id = %by.actor.id, "a mode was refused");
            CreateModeError::Denied(error)
        })?;
        if mode_cap_reached(tx.modes().count_owned_by(acting.actor.id).await?) {
            debug!(user_id = %by.actor.id, "a mode was refused: the owner has the most modes");
            return Err(CreateModeError::CapReached);
        }
        let shared = visibility == ModeVisibility::Shared;
        if tx
            .modes()
            .name_taken(acting.actor.id, &name, shared, None)
            .await?
        {
            return Err(CreateModeError::NameTaken);
        }
        let mode = NewMode {
            id,
            owner: acting.actor.id,
            name,
            visibility,
            definition,
            created_at: now,
        };
        tx.modes()
            .insert(&mode)
            .await
            .map_err(|error| match error {
                WriteModeError::NameTaken => CreateModeError::NameTaken,
                WriteModeError::Store(error) => CreateModeError::Store(error),
            })?;
        let metadata = AuditMetadata::new()
            .text("visibility", visibility.as_str())?
            .text("commands", commands(&mode.definition, check))?
            .text("route", route.as_str())?;
        let entry = self
            .audit
            .entry(AuditAction::MODE_CREATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Mode(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        info!(user_id = %by.actor.id, mode_id = %id, "a mode was created");
        Ok(ListedMode {
            mode: StoredMode {
                id,
                owner: Some(acting.actor.id),
                name: mode.name,
                visibility,
                definition: mode.definition,
                version: NonZeroU32::MIN,
                created_at: now,
                updated_at: now,
            },
            owner: Some(ModeOwner {
                user: UserRef {
                    id: acting.actor.id,
                    role: acting.actor.role,
                },
                username: acting.username,
            }),
        })
    }

    /// The modes `viewer` may see on `page`, newest first, and the next
    /// page's cursor; every one is checked with `authorize()`.
    ///
    /// # Errors
    /// [`ListModesError::Disagreement`], or [`ListModesError::Store`].
    pub async fn list(
        &self,
        viewer: &Actor,
        page: Page<ModeId>,
    ) -> Result<Paged<ListedMode, ModeId>, ListModesError> {
        let rows = self.store.modes().visible(*viewer, page).await?;
        let rows = checked(viewer, rows)?;
        Ok(Paged::split(rows, page.limit, |row: &ListedMode| {
            row.mode.id
        }))
    }
}

/// Whether `edit` shows the same as `stored` (S21): the same name and
/// visibility, and steps that read the same in an answer. Only whether
/// anything is written; an edit that changes something writes its own
/// steps, exactly as sent.
fn shows_the_same(stored: &StoredMode, edit: &ModeEdit) -> bool {
    stored.name == edit.name
        && stored.visibility == edit.visibility
        && edit.definition.same_as_shown(&stored.definition)
}

/// How many bots use the mode `id`, read in `tx`, once `actor` is found able
/// to set the edited mode, whose commands `check` judged, on every one of
/// them (S17). The first that refuses stops the edit, naming nothing: a bot
/// the actor can't see and one they see without the level read the same.
///
/// # Errors
/// [`UpdateModeError::InUseByOthers`], or [`UpdateModeError::Store`].
async fn checked_bots(
    tx: &mut dyn WriteTx,
    actor: &Actor,
    id: ModeId,
    check: CommandCheck,
) -> Result<i64, UpdateModeError> {
    let accounts = tx.bots().accounts_with_mode(id).await?;
    for account in &accounts {
        let allowed = match tx.accounts().listed(*account, actor.id).await? {
            Some(listed) => {
                authorize(actor, Permission::SetBotMode(check), &listed.context()).is_ok()
            }
            // An account goes with its bot, so a bot's row always names one.
            None => false,
        };
        if !allowed {
            debug!(user_id = %actor.id, mode_id = %id, "a mode edit was refused: a bot uses it");
            return Err(UpdateModeError::InUseByOthers);
        }
    }
    Ok(i64::try_from(accounts.len()).unwrap_or(i64::MAX))
}

/// `mode.update`'s metadata for `change` over `stored`, through `route`,
/// whose commands `check` judged, after checking `bots` bots: never the name
/// or a step's text (ADR-0016).
fn edited(
    stored: &StoredMode,
    change: &ModeChange,
    check: CommandCheck,
    route: ModeRoute,
    bots: i64,
) -> Result<AuditMetadata, AuditError> {
    let metadata = AuditMetadata::new()
        .int("version", i64::from(change.version.get()))?
        .flag("definition_changed", change.definition != stored.definition)?
        .text("visibility", change.visibility.as_str())?;
    let metadata = if change.visibility == stored.visibility {
        metadata
    } else {
        metadata.text("previous_visibility", stored.visibility.as_str())?
    };
    metadata
        .text("commands", commands(&change.definition, check))?
        .text("route", route.as_str())?
        .int("bots_checked", bots)
}

/// What an audit entry says of the commands `definition`'s steps send,
/// which `check` judged: `none`, `allowlisted` (every one on the
/// allowlist), or `unlisted` (one off it). Names from a closed set, never a
/// command (ADR-0016).
fn commands(definition: &ModeDefinition, check: CommandCheck) -> &'static str {
    if check.needs_manage() {
        "unlisted"
    } else if definition.commands().next().is_some() {
        "allowlisted"
    } else {
        "none"
    }
}

/// `rows` if `authorize()` lets `viewer` see every one, as the query
/// promised; otherwise the first one it doesn't is logged by its ID alone.
///
/// # Errors
/// [`ListModesError::Disagreement`].
fn checked(viewer: &Actor, rows: Vec<ListedMode>) -> Result<Vec<ListedMode>, ListModesError> {
    if let Some(row) = rows
        .iter()
        .find(|row| authorize(viewer, Permission::ViewMode, &row.context()).is_err())
    {
        error!(mode_id = %row.mode.id, "{DISAGREEMENT}");
        return Err(ListModesError::Disagreement);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use chrono::DateTime;
    use fleet_core::authz::{ModeVisibility, Role, UserRef};
    use fleet_core::id::UserId;
    use fleet_core::mode::{ModeName, ModePreset};
    use fleet_core::value::Username;

    use super::*;
    use crate::ports::modes::{ModeOwner, StoredMode};

    /// A private mode of the user `owner`, a Member.
    fn private_mode(owner: UserId) -> ListedMode {
        let at = DateTime::from_timestamp_millis(1_800_000_000_000).unwrap();
        ListedMode {
            mode: StoredMode {
                id: ModeId::new_v7(at, [7; 10]).unwrap(),
                owner: Some(owner),
                name: ModeName::try_from("Fishing").unwrap(),
                visibility: ModeVisibility::Private,
                definition: ModePreset::Afk.definition(),
                version: NonZeroU32::MIN,
                created_at: at,
                updated_at: at,
            },
            owner: Some(ModeOwner {
                user: UserRef {
                    id: owner,
                    role: Role::Member,
                },
                username: Username::try_from("fisher").unwrap(),
            }),
        }
    }

    fn user(n: u8) -> UserId {
        UserId::new_v7(
            DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
            [n; 10],
        )
        .unwrap()
    }

    /// What the allowlist `["/spawn"]` says of a mode that sends `message`.
    fn check_of(message: &str) -> CommandCheck {
        let definition: ModeDefinition = serde_json::from_value(serde_json::json!({ "steps": [
            { "action": { "type": "send_chat", "message": message }, "schedule": { "type": "at_start" }, "probability": 100 }
        ] }))
        .unwrap();
        CommandAllowlist::try_new(&["/spawn"])
            .unwrap()
            .check_mode(&definition)
    }

    #[test]
    fn the_plain_route_takes_no_command_off_the_allowlist_and_the_twin_takes_any() {
        let cases = [
            ("hello", true, true),
            ("/spawn home", true, true),
            ("/op AfkBot2", false, true),
        ];

        for (message, plain, twin) in cases {
            let check = check_of(message);
            assert_eq!(ModeRoute::Plain.takes(check), plain, "{message}");
            assert_eq!(ModeRoute::WithCommands.takes(check), twin, "{message}");
        }
    }

    #[test]
    fn rows_the_viewer_may_see_pass() {
        let owner = user(1);
        let rows = vec![private_mode(owner)];
        let viewer = Actor {
            id: owner,
            role: Role::Member,
        };

        assert_eq!(checked(&viewer, rows.clone()).unwrap(), rows);
    }

    #[test]
    fn a_row_the_viewer_may_not_see_is_a_disagreement() {
        let viewer = Actor {
            id: user(2),
            role: Role::Member,
        };

        let result = checked(&viewer, vec![private_mode(user(1))]);

        assert!(
            matches!(result, Err(ListModesError::Disagreement)),
            "{result:?}"
        );
    }
}
