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
use crate::ports::modes::{InsertModeError, ListedMode, ModeOwner, NewMode, StoredMode};
use crate::ports::page::{Page, Paged};
use crate::ports::store::{Store, StoreError};

/// Which route a mode write came through (S16): the plain one takes only
/// steps that send no command off the allowlist; the step-up twin
/// (`/with-commands`) takes any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRoute {
    /// `POST /modes`.
    Plain,
    /// `POST /modes/with-commands`, after a step-up.
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
}

impl ModeService {
    /// The modes over `store`, minting IDs from `clock` and `random`, with
    /// an empty allowlist until [`ModeService::with_allowlist`] sets one.
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
        }
    }

    /// The same, with the server's `[chat] command_allowlist`.
    #[must_use]
    pub fn with_allowlist(self, allowlist: CommandAllowlist) -> Self {
        Self { allowlist, ..self }
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
            .name_taken(acting.actor.id, &name, shared)
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
                InsertModeError::NameTaken => CreateModeError::NameTaken,
                InsertModeError::Store(error) => CreateModeError::Store(error),
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
