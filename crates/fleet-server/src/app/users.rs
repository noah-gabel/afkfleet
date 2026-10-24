//! [`UserService`]: the users' use cases (Plan.md P7.5, P7.9, ADR-0010,
//! ADR-0016).
//!
//! - **The Owner** ([`UserService::create_owner`]), for `user create-owner`.
//! - **Registering with an invite** ([`UserService::register`]): the cheap
//!   checks first (the invite, then the username, on the read pool), then
//!   the password's strength and hash, then one transaction that re-checks
//!   the invite and its creator, inserts the user and uses the invite
//!   (`user.create` and `invite.use`). A real invite that can't be used is
//!   recorded as a `user.create` Failure; an unknown code never is. The new
//!   user has no 2FA: their first login answers a setup token.
//! - **The user's own** ([`UserService::me`], [`UserService::change_password`]):
//!   the profile with the wrong codes since the previous login, and a new
//!   password after stepping up. Its commit re-checks the session, the user
//!   and that the password verified at step-up is still the current one;
//!   it revokes every other session and ends this one's step-up.
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
use fleet_core::auth::{NewPassword, is_stepped_up};
use fleet_core::authz::{Actor, AuthzError, Permission, ResourceContext, Role, UserRef, authorize};
use fleet_core::id::{InviteId, SessionId, UserId};
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_core::value::Username;
use secrecy::SecretString;
use tracing::{debug, info};

use super::audit::AuditService;
use super::invites::{InviteCheck, InviteService, Unusable};
use crate::ports::audit::{FailedCodeAttempts, NewAuditEntry};
use crate::ports::live_hints::{LiveHints, NoLiveHints};
use crate::ports::page::{Page, Paged};
use crate::ports::passwords::{HashError, Passwords, VerifyError};
use crate::ports::reconcile::{Change, NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError, WriteTx};
use crate::ports::users::{InsertUserError, ListedUser, NewUser};

/// How a user came to exist, recorded as `via` in `user.create`'s
/// metadata. Group E adds the invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatedVia {
    /// `afkfleet-server user create-owner`.
    Cli,
    /// `POST /auth/register` with an invite.
    Invite,
}

impl CreatedVia {
    /// The value stored in the metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Invite => "invite",
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

/// A new user, as registering answers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// The ID.
    pub id: UserId,
    /// The normalized username.
    pub username: Username,
    /// The role the invite gave.
    pub role: Role,
}

/// Why a registration was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// The invite can't register anyone: unknown or malformed, used,
    /// revoked, expired, or its creator lost the right to it.
    #[error("the invite can't be used")]
    BadInvite,
    /// Another user has the username.
    #[error("the username is taken")]
    UsernameTaken,
    /// The password is too weak, or couldn't be hashed.
    #[error(transparent)]
    Password(#[from] HashError),
    /// The user's ID couldn't be minted.
    #[error("the user's ID can't be minted")]
    Mint(#[source] MintError),
    /// An audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The user's own view of themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeView {
    /// The ID.
    pub id: UserId,
    /// The normalized username.
    pub username: Username,
    /// The global role.
    pub role: Role,
    /// When the account was created.
    pub created_at: DateTime<Utc>,
    /// When the password was last set.
    pub password_changed_at: DateTime<Utc>,
    /// When the current authenticator was confirmed, if there is one.
    pub mfa_confirmed_at: Option<DateTime<Utc>>,
    /// How many unused recovery codes are left.
    pub recovery_codes_left: u32,
    /// The wrong 2FA codes since the previous login, without the newest
    /// login's own typos.
    pub failed_code_attempts: FailedCodeAttempts,
}

/// Why a password change was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum ChangePasswordError {
    /// The session ended, or the user was disabled or is gone.
    #[error("the session is gone")]
    SessionGone,
    /// The step-up is over, or the password changed since it.
    #[error("a fresh step-up is required")]
    StepUpRequired,
    /// The new password is the current one.
    #[error("the new password is the current one")]
    SamePassword,
    /// The current password couldn't be checked.
    #[error("the current password can't be checked")]
    Verify(#[source] VerifyError),
    /// The new password is too weak, or couldn't be hashed.
    #[error(transparent)]
    Password(#[from] HashError),
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
    reconcile: Arc<dyn Reconcile>,
    live_hints: Arc<dyn LiveHints>,
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
            reconcile: Arc::new(NoReconcile),
            live_hints: Arc::new(NoLiveHints),
        }
    }

    /// The service, telling the live events sockets through `live_hints` when a
    /// user's role changed, once its commit returned (R11).
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
    }

    /// The service, telling `reconcile` to look again after a user is
    /// disabled or enabled, which decides whether their bots run.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
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

    /// Registers `username` with `password` through the invite `code`, for a
    /// client at `ip`; see the [module docs](self).
    ///
    /// # Errors
    /// [`RegisterError::BadInvite`], [`RegisterError::UsernameTaken`],
    /// [`RegisterError::Password`] (a weak password, or a busy or failing
    /// password service), and the server's failures.
    pub async fn register(
        &self,
        invites: &InviteService,
        code: &SecretString,
        username: &Username,
        password: &NewPassword,
        ip: IpAddr,
    ) -> Result<Registered, RegisterError> {
        let Some(hash) = invites.code_hash(code) else {
            debug!("a registration was refused: not an invite code");
            return Err(RegisterError::BadInvite);
        };
        match invites.check(&hash).await? {
            InviteCheck::Usable(_) => {}
            InviteCheck::Unknown => {
                debug!("a registration was refused: an unknown invite");
                return Err(RegisterError::BadInvite);
            }
            InviteCheck::Unusable { id, reason } => {
                self.audit
                    .record(self.unusable_entry(id, reason, ip)?)
                    .await?;
                return Err(RegisterError::BadInvite);
            }
        }
        if self
            .store
            .users()
            .find_by_username(username)
            .await?
            .is_some()
        {
            debug!("a registration was refused: the username is taken");
            return Err(RegisterError::UsernameTaken);
        }
        // Hashing before the transaction: no slow work while it holds the one
        // write connection (ADR-0015).
        let password_hash = self.passwords.hash(password, username).await?;
        let now = self.clock.now();
        let id: UserId = mint(now, self.random.as_ref()).map_err(RegisterError::Mint)?;
        let mut tx = self.store.write().await?;
        let invite = match invites.check_in(tx.as_mut(), &hash, now).await? {
            InviteCheck::Usable(invite) => invite,
            InviteCheck::Unknown => return Err(RegisterError::BadInvite),
            InviteCheck::Unusable {
                id: invite_id,
                reason,
            } => {
                tx.commit(self.unusable_entry(invite_id, reason, ip)?)
                    .await?;
                return Err(RegisterError::BadInvite);
            }
        };
        tx.users()
            .insert(&NewUser {
                id,
                username: username.clone(),
                password_hash,
                role: invite.role,
                created_at: now,
            })
            .await
            .map_err(|error| match error {
                InsertUserError::UsernameTaken => RegisterError::UsernameTaken,
                // The table never holds an Owner invite: a store fault.
                InsertUserError::OwnerExists => {
                    RegisterError::Store(StoreError::Backend("an invite's role is an Owner".into()))
                }
                InsertUserError::Store(source) => RegisterError::Store(source),
            })?;
        if !tx.invites().use_invite(invite.id, id, now).await? {
            return Err(RegisterError::BadInvite);
        }
        let used = self
            .audit
            .entry(AuditAction::INVITE_USE, AuditOutcome::Success)
            .actor(id)
            .ip(ip)
            .target(AuditTarget::Invite(invite.id))
            .metadata(
                AuditMetadata::new()
                    .id("user_id", id)?
                    .text("role", invite.role.as_str())?,
            );
        tx.audit().record(&used).await?;
        let created = self
            .audit
            .entry(AuditAction::USER_CREATE, AuditOutcome::Success)
            .actor(id)
            .ip(ip)
            .target(AuditTarget::User(id))
            .metadata(
                AuditMetadata::new()
                    .text("role", invite.role.as_str())?
                    .text("via", CreatedVia::Invite.as_str())?
                    .id("invite_id", invite.id)?,
            );
        tx.commit(created).await?;
        info!(user_id = %id, invite_id = %invite.id, "a user registered");
        Ok(Registered {
            id,
            username: username.clone(),
            role: invite.role,
        })
    }

    /// The user `user_id`'s own view, if they exist.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn me(&self, user_id: UserId) -> Result<Option<MeView>, StoreError> {
        let Some(user) = self.store.users().get(user_id).await? else {
            return Ok(None);
        };
        let mfa = self.store.mfa().find(user_id).await?;
        let recovery_codes_left = self.store.mfa().count_unused_codes(user_id).await?;
        let failed_code_attempts = self.store.audit().failed_code_attempts(user_id).await?;
        Ok(Some(MeView {
            id: user.id,
            username: user.username,
            role: user.role,
            created_at: user.created_at,
            password_changed_at: user.password_changed_at,
            mfa_confirmed_at: mfa.map(|mfa| mfa.confirmed_at),
            recovery_codes_left,
            failed_code_attempts,
        }))
    }

    /// Sets `user_id`'s password to `new_password` for the request of
    /// session `session_id` from `ip`; see the [module docs](self).
    ///
    /// # Errors
    /// [`ChangePasswordError::SessionGone`],
    /// [`ChangePasswordError::StepUpRequired`],
    /// [`ChangePasswordError::SamePassword`],
    /// [`ChangePasswordError::Password`], and the server's failures.
    pub async fn change_password(
        &self,
        user_id: UserId,
        session_id: SessionId,
        new_password: &NewPassword,
        ip: IpAddr,
    ) -> Result<(), ChangePasswordError> {
        let user = self
            .store
            .users()
            .get(user_id)
            .await?
            .ok_or(ChangePasswordError::SessionGone)?;
        // A copy for the port, wiped when it's dropped.
        let candidate = SecretString::from(new_password.expose_secret());
        if self
            .passwords
            .verify(&candidate, &user.password_hash)
            .await
            .map_err(ChangePasswordError::Verify)?
        {
            debug!(user_id = %user_id, "a password change was refused: the same password");
            return Err(ChangePasswordError::SamePassword);
        }
        let password_hash = self.passwords.hash(new_password, &user.username).await?;
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let session = tx
            .sessions()
            .find(session_id)
            .await?
            .filter(|found| found.session.revoked_at.is_none() && found.session.user_id == user_id)
            .map(|found| found.session);
        let current = tx.users().get(user_id).await?.filter(|user| !user.disabled);
        let (Some(session), Some(current)) = (session, current) else {
            debug!(user_id = %user_id, "a password change was refused: the session is gone");
            return Err(ChangePasswordError::SessionGone);
        };
        // The password verified at step-up must still be the current one.
        let fresh = is_stepped_up(session.stepped_up_at, now)
            && session
                .stepped_up_at
                .is_some_and(|stamp| current.password_changed_at <= stamp);
        if !fresh {
            debug!(user_id = %user_id, "a password change was refused: no fresh step-up");
            return Err(ChangePasswordError::StepUpRequired);
        }
        tx.users()
            .change_password(user_id, &password_hash, now)
            .await?;
        let revoked = tx
            .sessions()
            .revoke_all_for_user(user_id, Some(session_id), now)
            .await?;
        tx.sessions().clear_step_up(session_id).await?;
        let entry = self
            .audit
            .entry(AuditAction::USER_CHANGE_PASSWORD, AuditOutcome::Success)
            .actor(user_id)
            .ip(ip)
            .target(AuditTarget::User(user_id))
            .metadata(AuditMetadata::new().int("sessions_revoked", count(revoked))?);
        tx.commit(entry).await?;
        info!(user_id = %user_id, "a user changed their password");
        Ok(())
    }

    /// The `user.create` Failure for a real invite `id` that can't be used,
    /// tried from `ip`: a sign its code leaked.
    fn unusable_entry(
        &self,
        id: InviteId,
        reason: Unusable,
        ip: IpAddr,
    ) -> Result<NewAuditEntry, AuditError> {
        debug!(invite_id = %id, reason = reason.as_str(), "a registration was refused");
        Ok(self
            .audit
            .entry(AuditAction::USER_CREATE, AuditOutcome::Failure)
            .ip(ip)
            .target(AuditTarget::Invite(id))
            .metadata(AuditMetadata::new().text("reason", reason.as_str())?))
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
        // Whether the user's bots may run changes with it.
        let hint = matches!(change, UserChange::Disabled(_));
        // Told only after a commit, which only a real change makes.
        let role_changed = matches!(change, UserChange::Role(_));
        // The bots whose owner's flag a change sets: what they read as
        // follows it (S42).
        let mut owned = Vec::new();
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
                owned = tx.bots().owned_by(target).await?;
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
            if hint {
                self.reconcile.changed(Change::Store);
            }
            if role_changed {
                self.live_hints.role_changed(target);
            }
            self.live_hints.bots(&owned);
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
