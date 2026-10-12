//! The users: [`User`], [`NewUser`], [`PasswordHash`] and the read and write
//! traits.

use core::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretString};

use super::page::Page;
use super::store::StoreError;

/// A password hash as a PHC string (`$argon2id$v=19$m=…`). It isn't the
/// password, but it's kept out of logs all the same: `Debug` is redacted and
/// the memory is zeroized. Only the password service (P7.2) reads it.
#[derive(Debug)]
pub struct PasswordHash(SecretString);

/// Why a text isn't a PHC string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordHashError {
    /// It doesn't start with `$`, an algorithm id of `a`–`z`, `0`–`9` and `-`,
    /// and another `$`. Most likely a plain password was passed by mistake.
    #[error("not a PHC string")]
    NotPhc,
}

impl PasswordHash {
    /// Wraps `hash` after checking that it's shaped like a PHC string: `$`,
    /// a non-empty algorithm id of `a`–`z`, `0`–`9` and `-`, `$`, the rest.
    /// The `users` table's CHECK enforces the same shape. P7.2 owns the
    /// exact format.
    ///
    /// # Errors
    /// [`PasswordHashError::NotPhc`].
    pub fn new(hash: SecretString) -> Result<Self, PasswordHashError> {
        if is_phc(hash.expose_secret()) {
            Ok(Self(hash))
        } else {
            Err(PasswordHashError::NotPhc)
        }
    }

    /// Returns the hash, for the password service only.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

/// Whether `text` starts like a PHC string: `$`, an algorithm id of
/// `a`–`z`, `0`–`9` and `-`, and another `$`.
fn is_phc(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('$') else {
        return false;
    };
    let Some((algorithm, _)) = rest.split_once('$') else {
        return false;
    };
    !algorithm.is_empty()
        && algorithm
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A stored user (Plan.md Appendix D).
#[derive(Debug)]
pub struct User {
    /// The ID.
    pub id: UserId,
    /// The normalized (lowercase) username.
    pub username: Username,
    /// The password hash.
    pub password_hash: PasswordHash,
    /// The global role.
    pub role: Role,
    /// Whether the user is disabled.
    pub disabled: bool,
    /// When the user was created.
    pub created_at: DateTime<Utc>,
    /// When the current password was set.
    pub password_changed_at: DateTime<Utc>,
}

/// A user to create: enabled, with `password_changed_at` = `created_at`.
#[derive(Debug)]
pub struct NewUser {
    /// The ID, minted from `created_at` (`fleet_core::system::mint`).
    pub id: UserId,
    /// The normalized username.
    pub username: Username,
    /// The password hash.
    pub password_hash: PasswordHash,
    /// The global role.
    pub role: Role,
    /// When the user is created.
    pub created_at: DateTime<Utc>,
}

/// A user as the admins' list shows them: no password hash, plus whether
/// they have 2FA and when they were last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedUser {
    /// The ID.
    pub id: UserId,
    /// The normalized (lowercase) username.
    pub username: Username,
    /// The global role.
    pub role: Role,
    /// Whether the user is disabled.
    pub disabled: bool,
    /// When the user was created.
    pub created_at: DateTime<Utc>,
    /// Whether the user has a confirmed authenticator.
    pub mfa_enrolled: bool,
    /// The newest `last_used_at` of any of the user's sessions, if they
    /// have one.
    pub last_seen_at: Option<DateTime<Utc>>,
}

/// Why a user couldn't be inserted.
#[derive(Debug, thiserror::Error)]
pub enum InsertUserError {
    /// Another user has the username.
    #[error("the username is taken")]
    UsernameTaken,
    /// The user would be a second Owner.
    #[error("there's already an Owner")]
    OwnerExists,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reading users.
#[async_trait]
pub trait UserReads: Send + Sync + fmt::Debug {
    /// The user with this ID, if there is one.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn get(&self, id: UserId) -> Result<Option<User>, StoreError>;

    /// The user with this (normalized) username, if there is one.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn find_by_username(&self, username: &Username) -> Result<Option<User>, StoreError>;

    /// Whether there's an Owner. `user create-owner` asks before any
    /// password is typed; the unique index still decides at insert.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn owner_exists(&self) -> Result<bool, StoreError>;

    /// The users of `page`, newest first: up to `page.limit.fetch()` of
    /// them, one more than the page holds.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn list(&self, page: Page<UserId>) -> Result<Vec<ListedUser>, StoreError>;

    /// The user with this ID as the list shows them.
    ///
    /// # Errors
    /// [`StoreError`], including [`StoreError::Corrupt`].
    async fn listed(&self, id: UserId) -> Result<Option<ListedUser>, StoreError>;
}

/// Writing users, inside a [`super::store::WriteTx`].
#[async_trait]
pub trait UserWrites: Send {
    /// Inserts `user`.
    ///
    /// # Errors
    /// [`InsertUserError::UsernameTaken`], [`InsertUserError::OwnerExists`] or
    /// [`InsertUserError::Store`].
    async fn insert(&mut self, user: &NewUser) -> Result<(), InsertUserError>;

    /// The user with this ID, read in this transaction, so a commit that
    /// acts on a verified password can check that it's unchanged and the
    /// user isn't disabled.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn get(&mut self, id: UserId) -> Result<Option<User>, StoreError>;

    /// Replaces the user's password hash with `hash`, the same password
    /// re-hashed with the current parameters. `password_changed_at` stays:
    /// it isn't a password change. Whether the user exists.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_password_hash(
        &mut self,
        id: UserId,
        hash: &PasswordHash,
    ) -> Result<bool, StoreError>;

    /// Sets the user's new password: its hash, and `password_changed_at` =
    /// `at`, so every commit that acts on the old password refuses.
    /// Whether the user exists.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn change_password(
        &mut self,
        id: UserId,
        hash: &PasswordHash,
        at: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    /// Sets the user's role. Whether the user exists.
    ///
    /// # Errors
    /// [`StoreError`]: a second Owner is [`StoreError::Backend`] (the unique
    /// index refuses it).
    async fn set_role(&mut self, id: UserId, role: Role) -> Result<bool, StoreError>;

    /// Sets whether the user is disabled. Whether the user exists.
    ///
    /// # Errors
    /// [`StoreError`].
    async fn set_disabled(&mut self, id: UserId, disabled: bool) -> Result<bool, StoreError>;
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::argon2id("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g")]
    #[case::other_algorithm("$scrypt-2$x")]
    #[case::digits("$pbkdf2-sha256$i=600000$abc$def")]
    fn phc_strings_are_accepted(#[case] text: &str) {
        let hash = PasswordHash::new(SecretString::from(text)).unwrap();

        assert_eq!(hash.expose_secret(), text);
    }

    #[rstest]
    #[case::plain_password("correct horse battery staple")]
    #[case::empty("")]
    #[case::only_dollar("$")]
    #[case::no_second_dollar("$argon2id")]
    #[case::empty_algorithm("$$v=19")]
    #[case::uppercase_algorithm("$Argon2id$v=19")]
    #[case::space_in_algorithm("$argon 2$x")]
    fn anything_else_is_refused(#[case] text: &str) {
        assert_eq!(
            PasswordHash::new(SecretString::from(text)).err(),
            Some(PasswordHashError::NotPhc)
        );
    }

    #[test]
    fn debug_never_shows_the_hash() {
        let hash = PasswordHash::new(SecretString::from("$argon2id$v=19$secretpart")).unwrap();

        let debug = format!("{hash:?}");

        assert!(!debug.contains("secretpart"), "{debug}");
    }
}
