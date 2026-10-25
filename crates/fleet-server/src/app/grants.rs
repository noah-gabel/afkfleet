//! [`GrantService`]: sharing a linked account with other users (Plan.md
//! P9.1, P9.6, §7.3; ADR-0010, ADR-0018).
//!
//! A grant gives one user View, Control or Manage on one account; only
//! Manage edits grants, and nobody grants to themselves (`authorize()`).
//!
//! - **Creating one by name** ([`GrantService::create_by_name`]): the
//!   grantee is named by their exact username, since Members can't list
//!   users. Every name that can't be granted gets the same answer
//!   ([`CreateGrantError::Unusable`]): an unknown name, a disabled user, and
//!   anyone who already has implicit Manage on the account (its owner, the
//!   Owner, an Admin on a Member's account), so a grant never stands for a
//!   Manage that would end with a role change. Each is recorded as a
//!   `grant.create` Failure with its reason and never the name, so an Admin
//!   sees someone probing names; the route spends its own per-user limit for
//!   it. A pending account takes no grant until it's approved, so approving
//!   it approves one person (ADR-0018, group E).
//! - **Changing one** ([`GrantService::set_level`]): by the grantee's ID. A
//!   raise is refused for a pending account and for a grantee who has
//!   implicit Manage by now; lowering always works. The same level changes
//!   nothing.
//! - **Revoking one** ([`GrantService::revoke`]): in any status.
//!
//! Each write reads the acting user, the account and the grantee again in
//! its transaction and asks `authorize()` again on those facts.

use std::sync::Arc;

use fleet_core::account::awaits_review;
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::authz::{
    Actor, AuthzError, GrantLevel, Permission, Role, authorize, implicit_manage,
};
use fleet_core::id::{AccountId, UserId};
use fleet_core::system::Clock;
use fleet_core::value::Username;
use tracing::{debug, info};

use super::accounts::{Reread, reread};
use super::audit::AuditService;
use super::users::ActingUser;
use crate::ports::audit::NewAuditEntry;
use crate::ports::grants::{AccountGrant, HeldGrant, InsertGrantError, ListedGrant};
use crate::ports::live_hints::{LiveHints, NoLiveHints};
use crate::ports::page::{Page, Paged};
use crate::ports::store::{Store, StoreError, WriteTx};

/// What a new grant logs.
const GRANTED: &str = "an account was shared";
/// What a name that can't be granted logs.
const UNUSABLE: &str = "a grant by name was refused";
/// What a changed grant logs.
const CHANGED: &str = "a grant was changed";
/// What a revoked grant logs.
const REVOKED: &str = "a grant was revoked";
/// What a grantee leaving an account logs.
const LEFT: &str = "a grantee left an account";

/// Why a name can't be granted. The API answers every one alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusableName {
    /// No user has the name.
    UnknownUser,
    /// The user is disabled.
    Disabled,
    /// The user has implicit Manage on the account: its owner, the Owner, or
    /// an Admin on a Member's account.
    ImplicitManage,
}

impl UnusableName {
    /// The name recorded as the `grant.create` Failure's `reason`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownUser => "unknown_user",
            Self::Disabled => "disabled",
            Self::ImplicitManage => "implicit_manage",
        }
    }
}

/// A grant one user holds, and whether it's dormant now: their role gives
/// Manage on the account already, so it adds nothing until a demotion
/// (ADR-0020, S50).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldView {
    /// The grant, its account and their people.
    pub held: HeldGrant,
    /// Whether the holder's role gives Manage on the account now.
    pub dormant: bool,
}

/// Why a grantee couldn't leave an account.
#[derive(Debug, thiserror::Error)]
pub enum LeaveGrantError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts: no grant to give up
    /// (404), or implicit Manage instead (403).
    #[error("the actor may not leave the account")]
    Denied(#[source] AuthzError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a grant wasn't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateGrantError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// `authorize()` refused it on the fresh facts.
    #[error("the grant isn't allowed")]
    Denied(#[source] AuthzError),
    /// The account is pending: it takes no grant until it's approved.
    #[error("the account is pending")]
    Pending,
    /// The name can't be granted; why is recorded, never answered.
    #[error("the name can't be granted")]
    Unusable(UnusableName),
    /// The user holds a grant on the account already.
    #[error("the user holds a grant already")]
    Exists,
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a grant wasn't changed.
#[derive(Debug, thiserror::Error)]
pub enum ChangeGrantError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// The user holds no grant on the account.
    #[error("the user holds no grant on the account")]
    NoGrant,
    /// `authorize()` refused it on the fresh facts.
    #[error("the change isn't allowed")]
    Denied(#[source] AuthzError),
    /// A raise on a pending account.
    #[error("the account is pending")]
    Pending,
    /// A raise for a grantee who has implicit Manage on the account by now.
    #[error("the grantee has implicit manage")]
    ImplicitManage,
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a grant wasn't revoked.
#[derive(Debug, thiserror::Error)]
pub enum RevokeGrantError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// There's no such account.
    #[error("the account doesn't exist")]
    NotFound,
    /// The user holds no grant on the account.
    #[error("the user holds no grant on the account")]
    NoGrant,
    /// `authorize()` refused it on the fresh facts.
    #[error("revoking the grant isn't allowed")]
    Denied(#[source] AuthzError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The grants' use cases; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct GrantService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    audit: AuditService,
    live_hints: Arc<dyn LiveHints>,
}

impl GrantService {
    /// The service over `store`, stamping with `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, store: Arc<dyn Store>) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            store,
            audit,
            live_hints: Arc::new(NoLiveHints),
        }
    }

    /// The service, telling the live events sockets through `live_hints`
    /// whose bot a grant changed, once its commit returned (ADR-0020, S42).
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
    }

    /// The grants on `account` on `page`, newest grantee first, and the
    /// next page's cursor. The caller asks `authorize()` for
    /// `ManageGrants` first.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(
        &self,
        account: AccountId,
        page: Page<UserId>,
    ) -> Result<Paged<ListedGrant, UserId>, StoreError> {
        let rows = self.store.grants().list(account, page).await?;
        Ok(Paged::split(rows, page.limit, |grant| grant.grant.user))
    }

    /// Ends `by`'s own grant on `account`: they leave a shared account
    /// (ADR-0020, S51). The actor and the account are read again and
    /// `LeaveAccount` authorized in the transaction, so a grant revoked
    /// meanwhile is 404 and one whose holder has implicit Manage now is
    /// refused; recorded as `grant.revoke` with `via` = `leave`, and the
    /// account's bot told to the sockets after the commit.
    ///
    /// # Errors
    /// [`LeaveGrantError`].
    pub async fn leave(&self, by: &ActingUser, account: AccountId) -> Result<(), LeaveGrantError> {
        let mut tx = self.store.write().await?;
        let (actor, listed) = match reread(tx.as_mut(), by, account).await? {
            Reread::SessionGone => return Err(LeaveGrantError::SessionGone),
            Reread::NotFound => return Err(LeaveGrantError::NotFound),
            Reread::Found { actor, account, .. } => (actor, account),
        };
        authorize(&actor, Permission::LeaveAccount, &listed.context())
            .map_err(LeaveGrantError::Denied)?;
        // `authorize()` allows leaving only with a grant, read in this
        // transaction, so neither refusal below can come.
        let level = listed
            .grant
            .ok_or(LeaveGrantError::Denied(AuthzError::NotFound))?;
        if !tx.grants().delete(account, actor.id).await? {
            return Err(LeaveGrantError::Denied(AuthzError::NotFound));
        }
        let bot = tx.bots().id_of_account(account).await?;
        let entry = self
            .entry(
                AuditAction::GRANT_REVOKE,
                AuditOutcome::Success,
                by,
                account,
            )
            .metadata(
                AuditMetadata::new()
                    .id("grantee", actor.id)?
                    .text("level", level.as_str())?
                    .text("via", "leave")?,
            );
        tx.commit(entry).await?;
        self.live_hints.bots(bot.as_slice());
        info!(user_id = %actor.id, account_id = %account, "{LEFT}");
        Ok(())
    }

    /// The grants `holder` holds on `page`, newest account first, each
    /// with whether it's dormant (the holder's role gives Manage on that
    /// account now, so the grant adds nothing until a demotion), and the
    /// next page's cursor: the review at a role change (ADR-0020, S50).
    /// The caller asks `authorize()` first.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn held(
        &self,
        holder: &Actor,
        page: Page<AccountId>,
    ) -> Result<Paged<HeldView, AccountId>, StoreError> {
        let rows = self.store.grants().held_by(holder.id, page).await?;
        let held = Paged::split(rows, page.limit, |held| held.grant.account);
        Ok(Paged {
            items: held
                .items
                .into_iter()
                .map(|held| HeldView {
                    dormant: implicit_manage(holder, held.owner),
                    held,
                })
                .collect(),
            next: held.next,
        })
    }

    /// Gives the user named `username` `level` on `account`, for `by`; see
    /// the [module docs](self).
    ///
    /// # Errors
    /// [`CreateGrantError::SessionGone`], [`CreateGrantError::NotFound`],
    /// [`CreateGrantError::Denied`], [`CreateGrantError::Pending`],
    /// [`CreateGrantError::Unusable`], [`CreateGrantError::Exists`], and the
    /// server's failures.
    pub async fn create_by_name(
        &self,
        by: &ActingUser,
        account: AccountId,
        username: &Username,
        level: GrantLevel,
    ) -> Result<ListedGrant, CreateGrantError> {
        let mut tx = self.store.write().await?;
        let (actor, actor_name, listed) = match reread(tx.as_mut(), by, account).await? {
            Reread::SessionGone => return Err(CreateGrantError::SessionGone),
            Reread::NotFound => return Err(CreateGrantError::NotFound),
            Reread::Found {
                actor,
                name,
                account,
            } => (actor, name, account),
        };
        authorize(&actor, Permission::ManageGrants, &listed.context())
            .map_err(CreateGrantError::Denied)?;
        if awaits_review(listed.status) {
            return Err(CreateGrantError::Pending);
        }
        let grantee = tx.users().find_by_username(username).await?;
        let unusable = match &grantee {
            None => Some(UnusableName::UnknownUser),
            Some(user) if user.disabled => Some(UnusableName::Disabled),
            Some(user) if implicit_manage(&actor_of(user.id, user.role), listed.owner) => {
                Some(UnusableName::ImplicitManage)
            }
            Some(_) => None,
        };
        let (Some(grantee), None) = (grantee, unusable) else {
            let reason = unusable.unwrap_or(UnusableName::UnknownUser);
            let entry = self
                .entry(
                    AuditAction::GRANT_CREATE,
                    AuditOutcome::Failure,
                    by,
                    account,
                )
                .metadata(AuditMetadata::new().text("reason", reason.as_str())?);
            tx.commit(entry).await?;
            debug!(user_id = %by.actor.id, account_id = %account, reason = reason.as_str(), "{UNUSABLE}");
            return Err(CreateGrantError::Unusable(reason));
        };
        authorize(
            &actor,
            Permission::Grant {
                level,
                grantee: grantee.id,
            },
            &listed.context(),
        )
        .map_err(CreateGrantError::Denied)?;
        let grant = AccountGrant {
            account,
            user: grantee.id,
            level,
            granted_by: actor.id,
            granted_at: self.clock.now(),
        };
        match tx.grants().insert(&grant).await {
            Ok(()) => {}
            Err(InsertGrantError::Exists) => return Err(CreateGrantError::Exists),
            Err(InsertGrantError::Store(error)) => return Err(error.into()),
        }
        let bot = tx.bots().id_of_account(account).await?;
        let entry = self
            .entry(
                AuditAction::GRANT_CREATE,
                AuditOutcome::Success,
                by,
                account,
            )
            .metadata(
                AuditMetadata::new()
                    .id("grantee", grantee.id)?
                    .text("level", level.as_str())?,
            );
        tx.commit(entry).await?;
        self.live_hints.bots(bot.as_slice());
        info!(user_id = %by.actor.id, account_id = %account, grantee = %grantee.id, "{GRANTED}");
        Ok(ListedGrant {
            grant,
            user_name: grantee.username,
            granted_by_name: actor_name,
        })
    }

    /// Sets `user`'s grant on `account` to `level`, for `by`; see the
    /// [module docs](self).
    ///
    /// # Errors
    /// [`ChangeGrantError::SessionGone`], [`ChangeGrantError::NotFound`],
    /// [`ChangeGrantError::NoGrant`], [`ChangeGrantError::Denied`],
    /// [`ChangeGrantError::Pending`], [`ChangeGrantError::ImplicitManage`],
    /// and the server's failures.
    pub async fn set_level(
        &self,
        by: &ActingUser,
        account: AccountId,
        user: UserId,
        level: GrantLevel,
    ) -> Result<ListedGrant, ChangeGrantError> {
        let mut tx = self.store.write().await?;
        let (actor, actor_name, listed) = match reread(tx.as_mut(), by, account).await? {
            Reread::SessionGone => return Err(ChangeGrantError::SessionGone),
            Reread::NotFound => return Err(ChangeGrantError::NotFound),
            Reread::Found {
                actor,
                name,
                account,
            } => (actor, name, account),
        };
        authorize(
            &actor,
            Permission::Grant {
                level,
                grantee: user,
            },
            &listed.context(),
        )
        .map_err(ChangeGrantError::Denied)?;
        let current = tx
            .grants()
            .find(account, user)
            .await?
            .ok_or(ChangeGrantError::NoGrant)?;
        let grantee = tx
            .users()
            .get(user)
            .await?
            .ok_or(ChangeGrantError::NoGrant)?;
        if level == current.level {
            let granted_by_name = names(tx.as_mut(), current.granted_by).await?;
            return Ok(ListedGrant {
                grant: current,
                user_name: grantee.username,
                granted_by_name,
            });
        }
        if level > current.level {
            if awaits_review(listed.status) {
                return Err(ChangeGrantError::Pending);
            }
            if implicit_manage(&actor_of(grantee.id, grantee.role), listed.owner) {
                return Err(ChangeGrantError::ImplicitManage);
            }
        }
        let now = self.clock.now();
        if !tx
            .grants()
            .set_level(account, user, level, actor.id, now)
            .await?
        {
            return Err(ChangeGrantError::NoGrant);
        }
        let bot = tx.bots().id_of_account(account).await?;
        let entry = self
            .entry(
                AuditAction::GRANT_SET_LEVEL,
                AuditOutcome::Success,
                by,
                account,
            )
            .metadata(
                AuditMetadata::new()
                    .id("grantee", user)?
                    .text("level", level.as_str())?
                    .text("previous_level", current.level.as_str())?,
            );
        tx.commit(entry).await?;
        self.live_hints.bots(bot.as_slice());
        info!(user_id = %by.actor.id, account_id = %account, grantee = %user, "{CHANGED}");
        Ok(ListedGrant {
            grant: AccountGrant {
                level,
                granted_by: actor.id,
                granted_at: now,
                ..current
            },
            user_name: grantee.username,
            granted_by_name: actor_name,
        })
    }

    /// Revokes `user`'s grant on `account`, for `by`; see the [module
    /// docs](self).
    ///
    /// # Errors
    /// [`RevokeGrantError::SessionGone`], [`RevokeGrantError::NotFound`],
    /// [`RevokeGrantError::NoGrant`], [`RevokeGrantError::Denied`], and the
    /// server's failures.
    pub async fn revoke(
        &self,
        by: &ActingUser,
        account: AccountId,
        user: UserId,
    ) -> Result<(), RevokeGrantError> {
        let mut tx = self.store.write().await?;
        let (actor, listed) = match reread(tx.as_mut(), by, account).await? {
            Reread::SessionGone => return Err(RevokeGrantError::SessionGone),
            Reread::NotFound => return Err(RevokeGrantError::NotFound),
            Reread::Found { actor, account, .. } => (actor, account),
        };
        authorize(&actor, Permission::ManageGrants, &listed.context())
            .map_err(RevokeGrantError::Denied)?;
        let current = tx
            .grants()
            .find(account, user)
            .await?
            .ok_or(RevokeGrantError::NoGrant)?;
        if !tx.grants().delete(account, user).await? {
            return Err(RevokeGrantError::NoGrant);
        }
        let bot = tx.bots().id_of_account(account).await?;
        let entry = self
            .entry(
                AuditAction::GRANT_REVOKE,
                AuditOutcome::Success,
                by,
                account,
            )
            .metadata(
                AuditMetadata::new()
                    .id("grantee", user)?
                    .text("level", current.level.as_str())?,
            );
        tx.commit(entry).await?;
        self.live_hints.bots(bot.as_slice());
        info!(user_id = %by.actor.id, account_id = %account, grantee = %user, "{REVOKED}");
        Ok(())
    }

    /// An entry of `by` on `account`, without its metadata.
    fn entry(
        &self,
        action: AuditAction,
        outcome: AuditOutcome,
        by: &ActingUser,
        account: AccountId,
    ) -> NewAuditEntry {
        self.audit
            .entry(action, outcome)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Account(account))
    }
}

/// A user as `authorize()`'s helpers see one.
const fn actor_of(id: UserId, role: Role) -> Actor {
    Actor { id, role }
}

/// The username of `id`, read in `tx`. The foreign key keeps a grant's
/// granter; one that's gone anyway counts as no grant.
async fn names(tx: &mut dyn WriteTx, id: UserId) -> Result<Username, ChangeGrantError> {
    tx.users()
        .get(id)
        .await?
        .map(|user| user.username)
        .ok_or(ChangeGrantError::NoGrant)
}
