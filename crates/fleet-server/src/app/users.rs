//! [`UserService`]: the users' use cases (Plan.md P7.5, P7.9, ADR-0010,
//! ADR-0016).
//!
//! - **The Owner** ([`UserService::create_owner`]), for `user create-owner`.
//! - **The admins' list** ([`UserService::list`]), newest first in pages.
//! - **The admins' change** ([`UserService::set`]): one change per request,
//!   a role (only the Owner, between Member and Admin) or `disabled` (the
//!   Owner, or an Admin for a Member). A value equal to the current one
//!   changes nothing. A change revokes every session of the user, and a
//!   demotion to Member or disabling also revokes their pending invites,
//!   each with its own `invite.revoke`, in the same commit.
//! - **The re-check of an admin's write** ([`recheck`]): inside the write
//!   transaction, the acting user's session must still be live and theirs
//!   and the user enabled; the caller then asks `authorize()` again on the
//!   fresh role and target. So a change that landed after the handler
//!   authorized (the actor demoted, the target promoted) can't turn an
//!   allowed action into a forbidden one. The invites' writes use it too.

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::NewPassword;
use fleet_core::authz::{Actor, AuthzError, Permission, ResourceContext, Role, UserRef, authorize};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_core::value::Username;
use tracing::{debug, info};

use super::audit::AuditService;
use crate::ports::page::{Page, Paged};
use crate::ports::passwords::{HashError, Passwords};
use crate::ports::store::{Store, StoreError, WriteTx};
use crate::ports::users::{InsertUserError, ListedUser, NewUser};

/// How a user came to exist, recorded as `via` in `user.create`'s
/// metadata. Group E adds the invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatedVia {
    /// `afkfleet-server user create-owner`.
    Cli,
}

impl CreatedVia {
    /// The value stored in the metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
        }
    }
}

/// Why the Owner couldn't be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateOwnerError {
    /// There's an Owner already.
    #[error("there's already an Owner")]
    OwnerExists,
    /// Another user has the username.
    #[error("the username is taken")]
    UsernameTaken,
    /// The password is too weak, or couldn't be hashed.
    #[error(transparent)]
    Password(#[from] HashError),
    /// The user's ID couldn't be minted (the OS's randomness, or a clock
    /// outside an ID's range).
    #[error("the user's ID can't be minted")]
    Mint(#[source] MintError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Who makes an admin's write: their actor as the request authenticated
/// it, their session, and their client's address for the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActingUser {
    /// The actor.
    pub actor: Actor,
    /// The request's session.
    pub session_id: SessionId,
    /// The client's address.
    pub ip: IpAddr,
}

/// The acting user as [`recheck`] read them in the transaction.
#[derive(Debug)]
pub struct Rechecked {
    /// Their current role.
    pub actor: Actor,
    /// Their username.
    pub username: Username,
}

/// Re-reads the acting user inside `tx`: `None` when their session is
/// revoked or another user's, or they're disabled or gone. The caller asks
/// `authorize()` again with the returned actor; see the
/// [module docs](self).
///
/// # Errors
/// [`StoreError`].
pub async fn recheck(
    tx: &mut dyn WriteTx,
    by: &ActingUser,
) -> Result<Option<Rechecked>, StoreError> {
    let live = tx
        .sessions()
        .find(by.session_id)
        .await?
        .is_some_and(|found| {
            found.session.revoked_at.is_none() && found.session.user_id == by.actor.id
        });
    let user = tx
        .users()
        .get(by.actor.id)
        .await?
        .filter(|user| !user.disabled);
    let (true, Some(user)) = (live, user) else {
        return Ok(None);
    };
    Ok(Some(Rechecked {
        actor: Actor {
            id: user.id,
            role: user.role,
        },
        username: user.username,
    }))
}

/// One change of a user: a role or `disabled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserChange {
    /// The new role.
    Role(Role),
    /// Whether the user is disabled.
    Disabled(bool),
}

impl UserChange {
    /// The permission the change needs.
    #[must_use]
    pub const fn permission(self) -> Permission {
        match self {
            Self::Role(role) => Permission::SetRole { role },
            Self::Disabled(_) => Permission::SetDisabled,
        }
    }
}

/// Why a user's change was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum SetUserError {
    /// There's no such user.
    #[error("the user doesn't exist")]
    NotFound,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused the change on the fresh facts.
    #[error("the change isn't allowed")]
    Denied(#[source] AuthzError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a creator's pending invites were revoked, recorded as `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreatorChange {
    /// The creator was disabled.
    Disabled,
    /// The creator was demoted to Member.
    Demoted,
}

impl CreatorChange {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "creator_disabled",
            Self::Demoted => "creator_demoted",
        }
    }
}

/// The users' use cases.
#[derive(Debug, Clone)]
pub struct UserService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    passwords: Arc<dyn Passwords>,
    audit: AuditService,
}

impl UserService {
    /// The service over `store`, hashing with `passwords`, stamping with
    /// `clock` and minting IDs from `random`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        passwords: Arc<dyn Passwords>,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            passwords,
            audit,
        }
    }

    /// Whether there's an Owner.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn owner_exists(&self) -> Result<bool, StoreError> {
        self.store.users().owner_exists().await
    }

    /// Creates the Owner `username` with `password`, which the password
    /// service checks and hashes first, outside the write transaction. The
    /// user and its `user.create` entry commit together.
    ///
    /// # Errors
    /// [`CreateOwnerError::OwnerExists`] (the unique index decides),
    /// [`CreateOwnerError::UsernameTaken`], [`CreateOwnerError::Password`]
    /// (a weak password, or a busy or failing password service),
    /// [`CreateOwnerError::Mint`], [`CreateOwnerError::Audit`] or
    /// [`CreateOwnerError::Store`].
    pub async fn create_owner(
        &self,
        username: &Username,
        password: &NewPassword,
    ) -> Result<UserId, CreateOwnerError> {
        // Hashing first: no slow work while the write transaction holds the
        // one write connection (ADR-0015).
        let password_hash = self.passwords.hash(password, username).await?;
        let at = self.clock.now();
        let id: UserId = mint(at, self.random.as_ref()).map_err(CreateOwnerError::Mint)?;
        let user = NewUser {
            id,
            username: username.clone(),
            password_hash,
            role: Role::Owner,
            created_at: at,
        };
        let metadata = AuditMetadata::new()
            .text("role", Role::Owner.as_str())?
            .text("via", CreatedVia::Cli.as_str())?;
        let entry = self
            .audit
            .entry(AuditAction::USER_CREATE, AuditOutcome::Success)
            .target(AuditTarget::User(id))
            .metadata(metadata);
        let mut tx = self.store.write().await?;
        tx.users()
            .insert(&user)
            .await
            .map_err(|error| match error {
                InsertUserError::OwnerExists => CreateOwnerError::OwnerExists,
                InsertUserError::UsernameTaken => CreateOwnerError::UsernameTaken,
                InsertUserError::Store(source) => CreateOwnerError::Store(source),
            })?;
        tx.commit(entry).await?;
        Ok(id)
    }

    /// The users of `page`, newest first, and the next page's cursor.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(&self, page: Page<UserId>) -> Result<Paged<ListedUser, UserId>, StoreError> {
        let rows = self.store.users().list(page).await?;
        Ok(Paged::split(rows, page.limit, |user| user.id))
    }

    /// The user `id` as `authorize()` sees them, for the handler's check
    /// before [`UserService::set`].
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn user_ref(&self, id: UserId) -> Result<Option<UserRef>, StoreError> {
        Ok(self.store.users().listed(id).await?.map(|user| UserRef {
            id: user.id,
            role: user.role,
        }))
    }

    /// Makes `change` to user `target` for `by`, and answers the user as
    /// the list shows them; see the [module docs](self). The re-check, the
    /// authorization on the fresh row and the no-op decision all happen in
    /// the write transaction.
    ///
    /// # Errors
    /// [`SetUserError::SessionGone`], [`SetUserError::NotFound`],
    /// [`SetUserError::Denied`], and the server's failures.
    pub async fn set(
        &self,
        by: &ActingUser,
        target: UserId,
        change: UserChange,
    ) -> Result<ListedUser, SetUserError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, "an admin's change was refused: the session is gone");
            return Err(SetUserError::SessionGone);
        };
        let user = tx
            .users()
            .get(target)
            .await?
            .ok_or(SetUserError::NotFound)?;
        authorize(
            &acting.actor,
            change.permission(),
            &ResourceContext::User(UserRef {
                id: user.id,
                role: user.role,
            }),
        )
        .map_err(|error| {
            debug!(user_id = %by.actor.id, target = %target, "an admin's change was refused");
            SetUserError::Denied(error)
        })?;
        let entry = match change {
            UserChange::Role(role) if role == user.role => None,
            UserChange::Disabled(disabled) if disabled == user.disabled => None,
            UserChange::Role(role) => {
                tx.users().set_role(target, role).await?;
                let revoked = tx.sessions().revoke_all_for_user(target, None, now).await?;
                if role == Role::Member {
                    self.revoke_invites(tx.as_mut(), by, target, CreatorChange::Demoted, now)
                        .await?;
                }
                let metadata = AuditMetadata::new()
                    .text("role", role.as_str())?
                    .text("previous_role", user.role.as_str())?
                    .int("sessions_revoked", count(revoked))?;
                Some(
                    self.audit
                        .entry(AuditAction::USER_SET_ROLE, AuditOutcome::Success)
                        .metadata(metadata),
                )
            }
            UserChange::Disabled(disabled) => {
                tx.users().set_disabled(target, disabled).await?;
                let revoked = tx.sessions().revoke_all_for_user(target, None, now).await?;
                if disabled {
                    self.revoke_invites(tx.as_mut(), by, target, CreatorChange::Disabled, now)
                        .await?;
                }
                let metadata = AuditMetadata::new()
                    .flag("disabled", disabled)?
                    .int("sessions_revoked", count(revoked))?;
                Some(
                    self.audit
                        .entry(AuditAction::USER_SET_DISABLED, AuditOutcome::Success)
                        .metadata(metadata),
                )
            }
        };
        if let Some(entry) = entry {
            tx.commit(
                entry
                    .actor(by.actor.id)
                    .ip(by.ip)
                    .target(AuditTarget::User(target)),
            )
            .await?;
            info!(user_id = %by.actor.id, target = %target, "a user was changed");
        } else {
            // Nothing to change; dropping the transaction writes nothing.
            drop(tx);
        }
        self.store
            .users()
            .listed(target)
            .await?
            .ok_or(SetUserError::NotFound)
    }

    /// Revokes `creator`'s pending invites in `tx`, one `invite.revoke` each.
    async fn revoke_invites(
        &self,
        tx: &mut dyn WriteTx,
        by: &ActingUser,
        creator: UserId,
        why: CreatorChange,
        now: DateTime<Utc>,
    ) -> Result<(), SetUserError> {
        let revoked = tx.invites().revoke_pending_by(creator, now).await?;
        for invite in revoked {
            let metadata = AuditMetadata::new()
                .text("role", invite.role.as_str())?
                .text("reason", why.as_str())?;
            let entry = self
                .audit
                .entry(AuditAction::INVITE_REVOKE, AuditOutcome::Success)
                .actor(by.actor.id)
                .ip(by.ip)
                .target(AuditTarget::Invite(invite.id))
                .metadata(metadata);
            tx.audit().record(&entry).await?;
        }
        Ok(())
    }
}

/// A count of rows as audit metadata; no count comes near `i64::MAX`.
fn count(rows: u64) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}
