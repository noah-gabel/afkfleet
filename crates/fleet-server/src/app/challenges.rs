//! [`Challenges`]: the login's challenges, held in memory (Plan.md P7.6,
//! P7.7, §7.2, ADR-0016).
//!
//! The password step never hands out real tokens. It hands out:
//! - an **mfa token** (`afk_mt_`, 5 minutes) when the user has 2FA, for the
//!   code step, which takes at most 3 wrong codes;
//! - a **setup token** (`afk_st_`, 10 minutes) when the user must enroll
//!   first, for enroll and confirm only.
//!
//! A 2FA replacement hands out a setup token too, bound to the session that
//! asked. Each user has two slots: the login slot (an mfa or setup token; a
//! new password step replaces it) and the replace slot (a replacement's
//! setup token; a new replacement replaces it). Neither ever replaces the
//! other, so the store holds at most two challenges per user.
//!
//! Only each token's SHA-256 is held, and a presented token is found by
//! comparing hashes in constant time. A restart drops every challenge, and
//! the user logs in again. Expired ones are dropped when they're looked up
//! and by the server's housekeeping ([`Challenges::purge_expired`]).
//!
//! A setup challenge carries enroll's pending secret until confirm writes
//! it, and an mfa challenge the re-hashed password, if any, until the code
//! step writes it. Each records the user's `password_changed_at` as the
//! password step saw it, for the commit's re-check. The lock is a std
//! `Mutex`, never held across an `.await`.

use core::fmt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use fleet_core::auth::{MFA_TOKEN_TTL, SETUP_TOKEN_TTL, challenge_expired, expires_in, used_up};
use fleet_core::authz::Role;
use fleet_core::id::{LoginStepId, SessionId, UserId};
use fleet_core::system::Clock;
use secrecy::SecretString;

use crate::ports::tokens::{MintTokenError, TokenHash, TokenKind, Tokens};
use crate::ports::totp::TotpSecret;
use crate::ports::users::PasswordHash;

/// Who a challenge is for, as the step that issued it found them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChallengeUser {
    /// The user.
    pub id: UserId,
    /// Their role then, for authorizing a setup token's requests.
    pub role: Role,
    /// When their current password was set then. The commit that acts on
    /// the challenge refuses if it changed since.
    pub password_changed_at: DateTime<Utc>,
}

/// What a setup token is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPurpose {
    /// A user without 2FA enrolls their first authenticator; from the
    /// password step.
    FirstEnrollment,
    /// A user replaces their authenticator; from `POST /me/mfa/reset`, bound
    /// to the session that asked, which must still be live at confirm.
    Replace {
        /// The session that asked.
        session_id: SessionId,
    },
}

/// A challenge just issued: its token, for the client only, and its
/// seconds left.
#[derive(Debug)]
pub struct IssuedChallenge {
    /// The token.
    pub token: SecretString,
    /// Its whole seconds left: its lifetime.
    pub expires_in: u64,
}

/// A live setup token, as presented.
#[derive(Debug, Clone, Copy)]
pub struct SetupGrant {
    hash: TokenHash,
    /// Who it's for.
    pub user: ChallengeUser,
    /// What it's for.
    pub purpose: SetupPurpose,
}

/// A live mfa token, as presented.
#[derive(Debug, Clone, Copy)]
pub struct MfaGrant {
    hash: TokenHash,
    /// Who it's for.
    pub user: ChallengeUser,
    /// The login step it belongs to, for the code step's audit entries.
    pub step: LoginStepId,
}

/// A pending secret, numbered so confirm takes exactly the one it checked.
#[derive(Debug)]
pub struct PendingSecret {
    /// The secret.
    pub secret: TotpSecret,
    /// Which enroll made it; a later enroll makes the next.
    pub generation: u64,
}

/// One challenge.
struct Entry {
    hash: TokenHash,
    issued_at: DateTime<Utc>,
    user: ChallengeUser,
    kind: Kind,
}

/// What kind of challenge, with what it carries.
enum Kind {
    Mfa {
        wrong_codes: u32,
        rehash: Option<PasswordHash>,
        step: LoginStepId,
    },
    Setup {
        purpose: SetupPurpose,
        pending: Option<PendingSecret>,
    },
}

impl Entry {
    /// Whether it's over at `now`: expired, or an mfa token used up.
    fn is_over(&self, now: DateTime<Utc>) -> bool {
        match &self.kind {
            Kind::Mfa { wrong_codes, .. } => {
                used_up(*wrong_codes) || challenge_expired(self.issued_at, MFA_TOKEN_TTL, now)
            }
            Kind::Setup { .. } => challenge_expired(self.issued_at, SETUP_TOKEN_TTL, now),
        }
    }
}

/// The two slots of every user, and the pending secrets' counter.
#[derive(Default)]
struct Slots {
    login: HashMap<UserId, Entry>,
    replace: HashMap<UserId, Entry>,
    generations: u64,
}

impl Slots {
    /// The live entry in either slot whose hash equals `hash`, compared in
    /// constant time; an entry that's over is removed instead.
    fn find(&mut self, hash: &TokenHash, now: DateTime<Utc>) -> Option<&mut Entry> {
        let in_login = self
            .login
            .iter()
            .find(|(_, entry)| entry.hash.ct_eq(hash))
            .map(|(user, entry)| (*user, entry.is_over(now)));
        let in_replace = self
            .replace
            .iter()
            .find(|(_, entry)| entry.hash.ct_eq(hash))
            .map(|(user, entry)| (*user, entry.is_over(now)));
        match (in_login, in_replace) {
            (Some((user, true)), _) => {
                self.login.remove(&user);
                None
            }
            (Some((user, false)), _) => self.login.get_mut(&user),
            (None, Some((user, true))) => {
                self.replace.remove(&user);
                None
            }
            (None, Some((user, false))) => self.replace.get_mut(&user),
            (None, None) => None,
        }
    }

    /// Removes the entry whose hash equals `hash` from its slot.
    fn remove(&mut self, hash: &TokenHash) -> Option<Entry> {
        let user = self
            .login
            .iter()
            .find(|(_, entry)| entry.hash.ct_eq(hash))
            .map(|(user, _)| *user);
        if let Some(user) = user {
            return self.login.remove(&user);
        }
        let user = self
            .replace
            .iter()
            .find(|(_, entry)| entry.hash.ct_eq(hash))
            .map(|(user, _)| *user)?;
        self.replace.remove(&user)
    }
}

/// The challenges; clones share them. See the [module docs](self).
#[derive(Clone)]
pub struct Challenges {
    clock: Arc<dyn Clock>,
    tokens: Arc<dyn Tokens>,
    slots: Arc<Mutex<Slots>>,
}

/// Counts only: no token, hash or secret.
impl fmt::Debug for Challenges {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let slots = self.slots();
        f.debug_struct("Challenges")
            .field("login", &slots.login.len())
            .field("replace", &slots.replace.len())
            .finish_non_exhaustive()
    }
}

impl Challenges {
    /// No challenges yet; tokens minted by `tokens`, lifetimes on `clock`
    /// (the wall clock, like the sessions').
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, tokens: Arc<dyn Tokens>) -> Self {
        Self {
            clock,
            tokens,
            slots: Arc::default(),
        }
    }

    /// Locks the slots. A panic while holding them leaves them consistent,
    /// so a poisoned lock is used as it is.
    fn slots(&self) -> MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Issues an mfa token for `user` and the login step `step`, replacing
    /// their login slot, with the re-hashed password the code step writes,
    /// if any.
    ///
    /// # Errors
    /// [`MintTokenError`] when the token can't be minted.
    pub fn issue_mfa(
        &self,
        user: ChallengeUser,
        rehash: Option<PasswordHash>,
        step: LoginStepId,
    ) -> Result<IssuedChallenge, MintTokenError> {
        self.issue(
            TokenKind::Mfa,
            user,
            Kind::Mfa {
                wrong_codes: 0,
                rehash,
                step,
            },
        )
    }

    /// Issues a setup token for `user` and `purpose`: a first enrollment
    /// replaces their login slot, a replacement their replace slot.
    ///
    /// # Errors
    /// [`MintTokenError`] when the token can't be minted.
    pub fn issue_setup(
        &self,
        user: ChallengeUser,
        purpose: SetupPurpose,
    ) -> Result<IssuedChallenge, MintTokenError> {
        self.issue(
            TokenKind::Setup,
            user,
            Kind::Setup {
                purpose,
                pending: None,
            },
        )
    }

    /// Mints the token and stores the challenge in its slot.
    fn issue(
        &self,
        kind: TokenKind,
        user: ChallengeUser,
        challenge: Kind,
    ) -> Result<IssuedChallenge, MintTokenError> {
        let minted = self.tokens.mint(kind)?;
        let now = self.clock.now();
        let (ttl, replace_slot) = match &challenge {
            Kind::Mfa { .. } => (MFA_TOKEN_TTL, false),
            Kind::Setup { purpose, .. } => (
                SETUP_TOKEN_TTL,
                matches!(purpose, SetupPurpose::Replace { .. }),
            ),
        };
        let entry = Entry {
            hash: minted.hash,
            issued_at: now,
            user,
            kind: challenge,
        };
        let mut slots = self.slots();
        if replace_slot {
            slots.replace.insert(user.id, entry);
        } else {
            slots.login.insert(user.id, entry);
        }
        drop(slots);
        Ok(IssuedChallenge {
            token: minted.token,
            expires_in: expires_in(now, ttl, now),
        })
    }

    /// The live setup token `presented` is, if it's one.
    #[must_use]
    pub fn setup(&self, presented: &str) -> Option<SetupGrant> {
        let hash = self.tokens.hash_presented(TokenKind::Setup, presented)?;
        let now = self.clock.now();
        let mut slots = self.slots();
        let entry = slots.find(&hash, now)?;
        match &entry.kind {
            Kind::Setup { purpose, .. } => Some(SetupGrant {
                hash,
                user: entry.user,
                purpose: *purpose,
            }),
            Kind::Mfa { .. } => None,
        }
    }

    /// Makes `secret` the pending secret of `grant`'s challenge, replacing
    /// an earlier one. `false` when the challenge is gone or over.
    #[must_use]
    pub fn set_pending(&self, grant: &SetupGrant, secret: TotpSecret) -> bool {
        let now = self.clock.now();
        let mut slots = self.slots();
        let generation = slots.generations.wrapping_add(1);
        let Some(entry) = slots.find(&grant.hash, now) else {
            return false;
        };
        let Kind::Setup { pending, .. } = &mut entry.kind else {
            return false;
        };
        *pending = Some(PendingSecret { secret, generation });
        slots.generations = generation;
        true
    }

    /// A copy of `grant`'s pending secret, with its number: `None` when the
    /// challenge is gone or over, `Some(None)` when enroll hasn't made one.
    #[must_use]
    pub fn pending(&self, grant: &SetupGrant) -> Option<Option<PendingSecret>> {
        let now = self.clock.now();
        let mut slots = self.slots();
        let entry = slots.find(&grant.hash, now)?;
        let Kind::Setup {
            pending: Some(pending),
            ..
        } = &entry.kind
        else {
            return Some(None);
        };
        Some(
            TotpSecret::from_opened(pending.secret.expose())
                .ok()
                .map(|secret| PendingSecret {
                    secret,
                    generation: pending.generation,
                }),
        )
    }

    /// Takes `grant`'s challenge out of the store, so no other request can
    /// use it, if it's live and its pending secret is still number
    /// `generation`: the one confirm checked the code against. `None`
    /// otherwise, and the challenge stays as it was.
    #[must_use]
    pub fn take_setup(&self, grant: &SetupGrant, generation: u64) -> Option<PendingSecret> {
        let now = self.clock.now();
        let mut slots = self.slots();
        let entry = slots.find(&grant.hash, now)?;
        let Kind::Setup {
            pending: Some(pending),
            ..
        } = &entry.kind
        else {
            return None;
        };
        if pending.generation != generation {
            return None;
        }
        match slots.remove(&grant.hash)?.kind {
            Kind::Setup { pending, .. } => pending,
            Kind::Mfa { .. } => None,
        }
    }

    /// The live mfa token `presented` is, if it's one.
    #[must_use]
    pub fn mfa(&self, presented: &str) -> Option<MfaGrant> {
        let hash = self.tokens.hash_presented(TokenKind::Mfa, presented)?;
        let now = self.clock.now();
        let mut slots = self.slots();
        let entry = slots.find(&hash, now)?;
        match &entry.kind {
            Kind::Mfa { step, .. } => Some(MfaGrant {
                hash,
                user: entry.user,
                step: *step,
            }),
            Kind::Setup { .. } => None,
        }
    }

    /// Counts a wrong code against `grant`'s challenge; the last allowed
    /// one uses it up, and it's removed.
    pub fn wrong_code(&self, grant: &MfaGrant) {
        let now = self.clock.now();
        let mut slots = self.slots();
        let Some(entry) = slots.find(&grant.hash, now) else {
            return;
        };
        if let Kind::Mfa { wrong_codes, .. } = &mut entry.kind {
            *wrong_codes = wrong_codes.saturating_add(1);
        }
        if entry.is_over(now) {
            slots.remove(&grant.hash);
        }
    }

    /// Takes `grant`'s challenge out of the store, so no other request can
    /// use it: `Some` with its re-hashed password, if any, when it was still
    /// live, `None` when it's gone or over.
    #[must_use]
    pub fn take_mfa(&self, grant: &MfaGrant) -> Option<Option<PasswordHash>> {
        let now = self.clock.now();
        let mut slots = self.slots();
        slots.find(&grant.hash, now)?;
        match slots.remove(&grant.hash)?.kind {
            Kind::Mfa { rehash, .. } => Some(rehash),
            Kind::Setup { .. } => None,
        }
    }

    /// Drops every challenge that's over; how many it dropped.
    #[must_use]
    pub fn purge_expired(&self) -> usize {
        let now = self.clock.now();
        let mut slots = self.slots();
        let before = slots.login.len().saturating_add(slots.replace.len());
        slots.login.retain(|_, entry| !entry.is_over(now));
        slots.replace.retain(|_, entry| !entry.is_over(now));
        let after = slots.login.len().saturating_add(slots.replace.len());
        before.saturating_sub(after)
    }

    /// How many challenges are held, live or not yet purged.
    #[must_use]
    pub fn held(&self) -> usize {
        let slots = self.slots();
        slots.login.len().saturating_add(slots.replace.len())
    }
}
