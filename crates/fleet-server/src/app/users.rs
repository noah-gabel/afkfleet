//! [`UserService`]: the users' use cases (Plan.md P7.5, ADR-0016).
//!
//! Group A creates the Owner, for `user create-owner`. Group E adds
//! registration through an invite and the admins' changes (P7.9).

use std::sync::Arc;

use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::NewPassword;
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use fleet_core::value::Username;

use super::audit::AuditService;
use crate::ports::passwords::{HashError, Passwords};
use crate::ports::store::{Store, StoreError};
use crate::ports::users::{InsertUserError, NewUser};

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
}
