//! [`AccountService`]: the linked Minecraft accounts' use cases beside the
//! link flow (Plan.md P9.6, P9.7, ADR-0018).
//!
//! - **Offline accounts** (P9.7): creating one for development, while
//!   `[accounts] allow_offline` is on.
//! - **What a viewer sees** (P9.6): the accounts they may see, the pending
//!   links they may review, and one account, each as the viewer sees it
//!   ([`ListedAccount`]). The lists are filtered in SQL by `authorize()`'s
//!   rule, and every row is still checked by `authorize()` itself: a row
//!   the two disagree on is an error, never shown.
//! - **Reviewing a pending link** (P9.6): someone above its owner approves
//!   it, which makes it active, or refuses it, which deletes it with its
//!   refresh token and grants and frees the owner's quota slot and the
//!   Minecraft account (ADR-0018, group E). Only a pending account is
//!   reviewed.
//! - **Deleting an account** (P9.6): Manage, any status and kind, offline
//!   ones too while `allow_offline` is off; its grants go with it.
//!
//! A deletion and a review tell the reconciler to look again after their
//! commit ([`Reconcile`], P10.5): a bot may run, or must stop, now.
//!
//! Every write reads the acting user and their session again inside its
//! transaction ([`recheck`]), then the account with
//! its owner's current role and the actor's grant, and asks `authorize()`
//! again on those, so a change that landed after the handler authorized
//! can't turn an allowed write into a forbidden one. Its audit entry is
//! committed with it.

mod delete;
mod offline;
mod review;
mod view;

use std::sync::Arc;

use core::num::NonZeroU32;
use fleet_core::authz::Actor;
use fleet_core::id::AccountId;
use fleet_core::system::{Clock, SecureRandom};
use fleet_core::value::Username;

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::accounts::ListedAccount;
use crate::ports::live_hints::{LiveHints, NoLiveHints};
use crate::ports::reconcile::{NoReconcile, Reconcile};
use crate::ports::store::{Store, StoreError, WriteTx};

pub use delete::DeleteAccountError;
pub use offline::CreateOfflineError;
pub use review::ReviewError;
pub use view::ListAccountsError;

/// The accounts' settings (`[accounts]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSettings {
    /// Whether offline accounts may be created (`allow_offline`).
    pub allow_offline: bool,
    /// How many accounts a Member may own (`max_per_member`).
    pub max_per_member: NonZeroU32,
}

/// The accounts' use cases; see the [module docs](self).
#[derive(Debug, Clone)]
pub struct AccountService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    audit: AuditService,
    settings: AccountSettings,
    reconcile: Arc<dyn Reconcile>,
    live_hints: Arc<dyn LiveHints>,
}

impl AccountService {
    /// The service over `store`, minting IDs from `random`, stamping with
    /// `clock`, with `settings`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        settings: AccountSettings,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            audit,
            settings,
            reconcile: Arc::new(NoReconcile),
            live_hints: Arc::new(NoLiveHints),
        }
    }

    /// The service, telling the live events sockets through `live_hints`
    /// which bot a write changed, once its commit returned (ADR-0020, S42):
    /// a deletion, a review, a new offline account.
    #[must_use]
    pub fn with_live_hints(self, live_hints: Arc<dyn LiveHints>) -> Self {
        Self { live_hints, ..self }
    }

    /// The service, telling `reconcile` to look again after each write
    /// that may change where bots run: a deletion and a review.
    #[must_use]
    pub fn with_reconcile(self, reconcile: Arc<dyn Reconcile>) -> Self {
        Self { reconcile, ..self }
    }

    /// Whether offline accounts may be created.
    #[must_use]
    pub const fn offline_allowed(&self) -> bool {
        self.settings.allow_offline
    }
}

/// What a write on an account reads first in its transaction.
pub(super) enum Reread {
    /// The acting session ended, or its user was disabled, meanwhile.
    SessionGone,
    /// The account is gone.
    NotFound,
    /// The acting user with their current role and their name, and the
    /// account as they see it now.
    Found {
        actor: Actor,
        name: Username,
        account: ListedAccount,
    },
}

/// Reads the acting user `by` again, then the account `id` as they see it,
/// inside `tx`.
pub(super) async fn reread(
    tx: &mut dyn WriteTx,
    by: &ActingUser,
    id: AccountId,
) -> Result<Reread, StoreError> {
    let Some(acting) = recheck(tx, by).await? else {
        return Ok(Reread::SessionGone);
    };
    Ok(match tx.accounts().listed(id, acting.actor.id).await? {
        Some(account) => Reread::Found {
            actor: acting.actor,
            name: acting.username,
            account,
        },
        None => Reread::NotFound,
    })
}
