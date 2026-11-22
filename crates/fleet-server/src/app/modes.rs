//! The modes' use cases (Plan.md P11.4; ADR-0010; ADR-0020, S12–S19).
//!
//! - **Listing** ([`ModeService::list`]): the modes a viewer may see,
//!   newest first, filtered by the database and checked again, row by row,
//!   with `authorize(ViewMode)`; a row the two disagree on is an internal
//!   error that shows nothing (as `GET /bots` answers, ADR-0019, S54). What
//!   each viewer sees of a mode (its owner, the rights, a command's
//!   arguments) is the HTTP layer's field rule (S14, S15).
//! - **Creating** ([`ModeService::create`]): a mode whose steps send a
//!   command off the allowlist only through the twin route (S16); its
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
//! - **A scripted mode** (ADR-0036, D13 to D15): its body's rules were
//!   checked at the API's edge; before any transaction the service checks
//!   that its text is a program, with fleet-script's compiler in
//!   `spawn_blocking` ([`Compiler`]), never running a hook: what isn't is
//!   [`CreateModeError::Script`] with the error and its place, and a
//!   compiler that fails is an internal fault. `authorize()` asks with the
//!   mode's kind, so only an Admin or the Owner writes one. An edit that
//!   makes the mode a script's is refused while a bot that uses it is
//!   assigned to an agent whose live stream runs a lower script API level
//!   ([`UpdateModeError::AgentTooOld`]). The audit entries say whether the
//!   mode is steps or a script, and for a script how many of its chat
//!   patterns count at any time (ADR-0037, C5).
//! - **The stopgap `enter_minecart`** (ADR-0034; ADR-0036, D17): a new mode
//!   with it is refused before anything is read
//!   ([`CreateModeError::Stopgap`]), and so is an edit that brings it into
//!   a mode that had none, once the stored mode is read
//!   ([`UpdateModeError::Stopgap`]). A mode that has it keeps it through
//!   an edit and keeps running: its removal is the next release's.
//! - **Deleting** ([`ModeService::delete`]): one write
//!   transaction re-reads the acting user and the mode, asks `authorize()`
//!   for `DeleteMode`, refuses a mode any bot uses, whoever's (S17), deletes
//!   it and records `mode.delete` with the version and the visibility. No
//!   bot used it, so the reconciler has nothing to do.

use core::fmt;
use core::num::NonZeroU32;
use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{
    Actor, AuthzError, CommandAllowlist, CommandCheck, ModeKind, ModeVisibility, Permission,
    ResourceContext, UserRef, authorize,
};
use fleet_core::id::{ModeId, UserId};
use fleet_core::mode::{Action, ModeDefinition, ModeName, mode_cap_reached};
use fleet_core::script::{Script, ScriptDraft, When};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_script::CompileError;
use tracing::{debug, error, info};

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::live_agents::LiveAgents;
use crate::ports::live_hints::{LiveHints, ModeEvent, NoLiveHints};
use crate::ports::modes::{ListedMode, ModeChange, ModeOwner, NewMode, StoredMode, WriteModeError};
use crate::ports::page::{Page, Paged};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError, WriteTx};

/// Which route a mode write came through (S16): the plain one takes only
/// steps that send no command off the allowlist; its twin
/// (`/with-commands`) takes any. Creating and editing each have both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRoute {
    /// `POST /modes` or `PUT /modes/{id}`.
    Plain,
    /// `POST /modes/with-commands` or `PUT /modes/{id}/with-commands`.
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

/// What checks a scripted mode's text as a program at save: fleet-script's
/// compiler, unless a test hands in another. It runs in `spawn_blocking`
/// and never runs a hook (ADR-0036, D14).
#[derive(Clone)]
pub struct Compiler(Arc<CompileCheck>);

/// A check of a script as a program.
type CompileCheck = dyn Fn(&Script) -> Result<(), CompileError> + Send + Sync;

impl Compiler {
    /// fleet-script's compiler: the engine an agent runs, with its limits.
    #[must_use]
    pub fn fleet_script() -> Self {
        Self(Arc::new(|script| fleet_script::compile(script).map(|_| ())))
    }

    /// `check` instead, for a test.
    #[must_use]
    pub fn new(
        check: impl Fn(&Script) -> Result<(), CompileError> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(check))
    }
}

impl fmt::Debug for Compiler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Compiler")
    }
}

/// Why a scripted mode's text wasn't taken as a program.
#[derive(Debug, thiserror::Error)]
pub enum ScriptCheckError {
    /// It isn't one: the error, about the caller's own script, with its
    /// place.
    #[error("the script isn't a program: {0}")]
    NotAProgram(CompileError),
    /// The compiler's task failed: an internal fault, never the script's.
    #[error("the script's compiler failed")]
    CompilerFailed(#[source] tokio::task::JoinError),
}

/// Why a mode wasn't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateModeError {
    /// A scripted mode whose text isn't a program, or whose compiler failed.
    #[error(transparent)]
    Script(ScriptCheckError),
    /// The step at `step` (from 0) is the stopgap `enter_minecart`, which no
    /// new mode takes: it goes in the next release (ADR-0036, D17).
    #[error("step {step} is the stopgap that's going away")]
    Stopgap {
        /// The step's place in the mode.
        step: usize,
    },
    /// The steps send a command off the allowlist, which only the twin
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
    /// A scripted mode whose text isn't a program, or whose compiler failed.
    #[error(transparent)]
    Script(ScriptCheckError),
    /// The edit brings the stopgap `enter_minecart` into a mode that had no
    /// such step, at `step` (from 0); a mode that has one keeps it
    /// (ADR-0036, D17).
    #[error("step {step} brings in the stopgap that's going away")]
    Stopgap {
        /// The step's place in the edited mode.
        step: usize,
    },
    /// The edit makes the mode need a script API level that the agent of a
    /// bot that uses it doesn't run, by its live stream (ADR-0036, D13).
    #[error("a bot that uses the mode is on an agent that doesn't run it")]
    AgentTooOld,
    /// The steps send a command off the allowlist, which only the twin
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

impl From<WriteModeError> for UpdateModeError {
    /// A write's refusal: a name that's taken (the database's own check), or
    /// the database's failure.
    fn from(error: WriteModeError) -> Self {
        match error {
            WriteModeError::NameTaken => Self::NameTaken,
            WriteModeError::Store(error) => Self::Store(error),
        }
    }
}

/// Why a mode wasn't deleted.
#[derive(Debug, thiserror::Error)]
pub enum DeleteModeError {
    /// The acting session ended, or its user was disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// The mode doesn't exist (any more).
    #[error("the mode doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("the mode may not be deleted")]
    Denied(#[source] AuthzError),
    /// A bot uses the mode (S17).
    #[error("a bot uses the mode")]
    InUse,
    /// The delete found no row the transaction had just read: a bug, never
    /// a success.
    #[error("the mode's delete matched no row it had just read")]
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
    live_hints: Arc<dyn LiveHints>,
    compiler: Compiler,
    live: Option<Arc<dyn LiveAgents>>,
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
            live_hints: Arc::new(NoLiveHints),
            compiler: Compiler::fleet_script(),
            live: None,
        }
    }

    /// The same, checking a scripted mode's text with `compiler`.
    #[must_use]
    pub fn with_compiler(self, compiler: Compiler) -> Self {
        Self { compiler, ..self }
    }

    /// The same, reading the agents' live streams in `live`: an edit that
    /// makes a mode a script's is refused while a bot that uses it is on an
    /// agent whose stream runs a lower script API level. Without it, the
    /// reconciler alone holds the agents to their level.
    #[must_use]
    pub fn with_live_agents(self, live: Arc<dyn LiveAgents>) -> Self {
        Self {
            live: Some(live),
            ..self
        }
    }

    /// Refuses an edit, by `user`, that makes mode `id` need a script API
    /// level that the live stream of the agent of a bot that uses it
    /// doesn't run. An agent that's offline is held to the level by the
    /// reconciler at its next hello.
    ///
    /// # Errors
    /// [`UpdateModeError::AgentTooOld`], and the store's failures.
    async fn agents_run(
        &self,
        tx: &mut dyn WriteTx,
        (user, id): (UserId, ModeId),
        definition: &ModeDefinition,
    ) -> Result<(), UpdateModeError> {
        let (Some(needs), Some(live)) = (definition.script_api(), &self.live) else {
            return Ok(());
        };
        for agent in tx.bots().agents_with_mode(id).await? {
            if live
                .live(agent)
                .is_some_and(|stream| !needs.runs_on(stream.script_api))
            {
                debug!(user_id = %user, mode_id = %id, agent_id = %agent, "a mode edit was refused: a bot that uses it is on an agent that doesn't run its script");
                return Err(UpdateModeError::AgentTooOld);
            }
        }
        Ok(())
    }

    /// Checks `definition`'s script, if it has one, as a program, in
    /// `spawn_blocking`; never runs a hook.
    async fn checked(&self, definition: &ModeDefinition) -> Result<(), ScriptCheckError> {
        let Some(script) = definition.script() else {
            return Ok(());
        };
        let script = script.clone();
        let compiler = self.compiler.clone();
        // A compile is CPU work, a few milliseconds for the longest script
        // (ADR-0036, D3): off the async workers, and before the write
        // transaction opens (ADR-0015).
        tokio::task::spawn_blocking(move || (compiler.0)(&script))
            .await
            .map_err(ScriptCheckError::CompilerFailed)?
            .map_err(ScriptCheckError::NotAProgram)
    }

    /// The same, telling the live events sockets through `live_hints` when a
    /// mode was created, changed or deleted, once its commit returned (S33).
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
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
    /// [`UpdateModeError::InUseByOthers`], [`UpdateModeError::Script`],
    /// [`UpdateModeError::AgentTooOld`],
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
        self.checked(&edit.definition)
            .await
            .map_err(UpdateModeError::Script)?;
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
        // The right to edit it as it is, then as it will be (ADR-0010): a
        // script before or after is an Admin's or the Owner's (ADR-0036, D15).
        let as_it_is = (
            stored.mode.visibility,
            ModeKind::of(&stored.mode.definition),
        );
        let as_it_will_be = (edit.visibility, ModeKind::of(&edit.definition));
        for (visibility, kind) in [as_it_is, as_it_will_be] {
            authorize(
                &acting.actor,
                Permission::UpdateMode { visibility, kind },
                &context,
            )
            .map_err(|error| {
                debug!(user_id = %by.actor.id, mode_id = %id, "a mode edit was refused");
                UpdateModeError::Denied(error)
            })?;
        }
        keeps_the_stopgap_out(&stored.mode.definition, &edit.definition).inspect_err(|_| {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode edit was refused: it brings in the stopgap that's going away");
        })?;
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
        self.agents_run(tx.as_mut(), (by.actor.id, id), &edit.definition)
            .await?;
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
        let wrote = tx.modes().update(&change).await?;
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
        self.live_hints.mode(ModeEvent {
            id,
            was: Some(stored.seen()),
        });
        info!(user_id = %by.actor.id, mode_id = %id, "a mode was edited");
        Ok(Edited::Changed(changed(stored, change, owner)))
    }

    /// What the allowlist says of the commands `definition`'s steps send:
    /// whether one is off it, which only the twin route takes (S16).
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
    /// [`CreateModeError::NameTaken`], [`CreateModeError::Script`], and the
    /// server's failures.
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
        if let Some(step) = stopgap(&definition) {
            debug!(user_id = %by.actor.id, "a mode was refused: it holds the stopgap that's going away");
            return Err(CreateModeError::Stopgap { step });
        }
        self.checked(&definition)
            .await
            .map_err(CreateModeError::Script)?;
        let now = self.clock.now();
        let id: ModeId = mint(now, self.random.as_ref())?;
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, "a mode was refused: the session is gone");
            return Err(CreateModeError::SessionGone);
        };
        authorize(
            &acting.actor,
            Permission::CreateMode {
                visibility,
                kind: ModeKind::of(&definition),
            },
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
        let metadata = body(
            AuditMetadata::new()
                .text("visibility", visibility.as_str())?
                .text("commands", commands(&mode.definition, check))?
                .text("route", route.as_str())?,
            &mode.definition,
        )?;
        let entry = self
            .audit
            .entry(AuditAction::MODE_CREATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Mode(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.live_hints.mode(ModeEvent { id, was: None });
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

    /// Deletes the mode `id` for `by`; see the [module docs](self).
    ///
    /// # Errors
    /// [`DeleteModeError::SessionGone`], [`DeleteModeError::NotFound`],
    /// [`DeleteModeError::Denied`], [`DeleteModeError::InUse`],
    /// [`DeleteModeError::Lost`], and the server's failures.
    pub async fn delete(&self, by: &ActingUser, id: ModeId) -> Result<(), DeleteModeError> {
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode delete was refused: the session is gone");
            return Err(DeleteModeError::SessionGone);
        };
        let Some(stored) = tx.modes().listed(id).await? else {
            return Err(DeleteModeError::NotFound);
        };
        authorize(&acting.actor, Permission::DeleteMode, &stored.context()).map_err(|error| {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode delete was refused");
            DeleteModeError::Denied(error)
        })?;
        // Any bot, whoever's: a refusal only for bots the deleter can't see
        // would tell them those bots exist (S17).
        if !tx.bots().accounts_with_mode(id).await?.is_empty() {
            debug!(user_id = %by.actor.id, mode_id = %id, "a mode delete was refused: a bot uses it");
            return Err(DeleteModeError::InUse);
        }
        if !tx.modes().delete(id).await? {
            error!(mode_id = %id, "a mode's delete matched no row its transaction had just read; nothing was deleted");
            return Err(DeleteModeError::Lost);
        }
        let metadata = AuditMetadata::new()
            .int("version", i64::from(stored.mode.version.get()))?
            .text("visibility", stored.mode.visibility.as_str())?;
        let entry = self
            .audit
            .entry(AuditAction::MODE_DELETE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Mode(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        self.live_hints.mode(ModeEvent {
            id,
            was: Some(stored.seen()),
        });
        info!(user_id = %by.actor.id, mode_id = %id, "a mode was deleted");
        Ok(())
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
/// The place of `definition`'s first `enter_minecart` step, the stopgap a
/// script's `vehicle.mount("minecart")` replaces (ADR-0034; ADR-0036, D17).
fn stopgap(definition: &ModeDefinition) -> Option<usize> {
    definition
        .steps()
        .iter()
        .position(|step| step.action == Action::EnterMinecart)
}

/// Whether an edit from `was` to `will_be` keeps the stopgap out: a mode
/// that has it keeps it, and none gains it (ADR-0036, D17).
///
/// # Errors
/// [`UpdateModeError::Stopgap`] at the step that brings it in.
fn keeps_the_stopgap_out(
    was: &ModeDefinition,
    will_be: &ModeDefinition,
) -> Result<(), UpdateModeError> {
    match (stopgap(was), stopgap(will_be)) {
        (None, Some(step)) => Err(UpdateModeError::Stopgap { step }),
        _ => Ok(()),
    }
}

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

/// Whether `one` and `other` are stored as the same bytes: the JSON the
/// `modes` table holds, so `-0.0` and `0.0` differ where `==` reads them as
/// one angle (S21).
///
/// # Errors
/// [`StoreError::Backend`] if a definition doesn't serialize, as the write
/// would report it.
fn stored_alike(one: &ModeDefinition, other: &ModeDefinition) -> Result<bool, StoreError> {
    let json = |definition: &ModeDefinition| {
        serde_json::to_string(definition).map_err(|error| StoreError::Backend(Box::new(error)))
    };
    Ok(json(one)? == json(other)?)
}

/// `stored` as `change` wrote it, by its `owner`.
fn changed(stored: ListedMode, change: ModeChange, owner: UserId) -> ListedMode {
    ListedMode {
        mode: StoredMode {
            id: change.id,
            owner: Some(owner),
            name: change.name,
            visibility: change.visibility,
            definition: change.definition,
            version: change.version,
            created_at: stored.mode.created_at,
            updated_at: change.updated_at,
        },
        owner: stored.owner,
    }
}

/// `mode.update`'s metadata for `change` over `stored`, through `route`,
/// whose commands `check` judged, after checking `bots` bots: never the name
/// or a step's text (ADR-0016). `definition_changed` compares the steps as
/// stored, byte for byte (S21).
///
/// # Errors
/// [`UpdateModeError::Audit`], or [`UpdateModeError::Store`] if a definition
/// doesn't serialize.
fn edited(
    stored: &StoredMode,
    change: &ModeChange,
    check: CommandCheck,
    route: ModeRoute,
    bots: i64,
) -> Result<AuditMetadata, UpdateModeError> {
    let changed = !stored_alike(&stored.definition, &change.definition)?;
    let metadata = AuditMetadata::new()
        .int("version", i64::from(change.version.get()))?
        .flag("definition_changed", changed)?
        .text("visibility", change.visibility.as_str())?;
    let metadata = if change.visibility == stored.visibility {
        metadata
    } else {
        metadata.text("previous_visibility", stored.visibility.as_str())?
    };
    let metadata = metadata
        .text("commands", commands(&change.definition, check))?
        .text("route", route.as_str())?
        .int("bots_checked", bots)?;
    Ok(body(metadata, &change.definition)?)
}

/// `metadata` with what `definition` is made of (`body`: `steps` or
/// `script`) and, for a script, how many of its chat patterns count at any
/// time (`any_time_patterns`, ADR-0037, C5) and its API level; never its
/// text (ADR-0016).
///
/// # Errors
/// [`AuditError`].
fn body(metadata: AuditMetadata, definition: &ModeDefinition) -> Result<AuditMetadata, AuditError> {
    let metadata = metadata.text("body", ModeKind::of(definition).as_str())?;
    let Some(script) = definition.script() else {
        return Ok(metadata);
    };
    let draft = ScriptDraft::from(script.clone());
    let any_time = draft
        .patterns
        .iter()
        .filter(|pattern| pattern.when == When::AnyTime)
        .count();
    metadata
        .int(
            "any_time_patterns",
            i64::try_from(any_time).unwrap_or(i64::MAX),
        )?
        .int("script_api", i64::from(script.api().get()))
}

/// What an audit entry says of the commands `definition`'s steps send,
/// which `check` judged: `none`, `allowlisted` (every one on the
/// allowlist), or `unlisted` (one off it). Names from a closed set, never a
/// command (ADR-0016).
fn commands(definition: &ModeDefinition, check: CommandCheck) -> &'static str {
    if check.needs_manage() {
        "unlisted"
    } else if definition.command_names().next().is_some() {
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
