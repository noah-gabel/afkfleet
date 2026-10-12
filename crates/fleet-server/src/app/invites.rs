//! [`InviteService`]: the invites to register (Plan.md P7.1, P7.9, §7.2,
//! ADR-0010, ADR-0016).
//!
//! - **Creating** ([`InviteService::create`]): the code is minted from the
//!   `SecureRandom` port before the transaction (`afk_iv_…`, stored only as
//!   its SHA-256 hash), and expires `[auth] invite_ttl_hours` later, a time
//!   stored with it. Inside the transaction the creator is re-checked
//!   ([`super::users::recheck`]) and authorized again, and their pending
//!   invites are counted: the 21st is refused
//!   (`fleet_core::auth::MAX_PENDING_INVITES`). The code is answered once.
//! - **Listing** ([`InviteService::list`]): every invite, newest first, each
//!   with its state now.
//! - **Revoking** ([`InviteService::revoke`]): only a pending invite
//!   changes; a used one is refused, a revoked or expired one is left as it
//!   is without a second entry.
//!
//! Registering with an invite arrives with `POST /auth/register` (P7.6).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditError, AuditMetadata, AuditOutcome, AuditTarget};
use fleet_core::auth::{InviteState, InviteTtl, invite_state, pending_cap_reached};
use fleet_core::authz::{AuthzError, Permission, ResourceContext, Role, authorize};
use fleet_core::id::InviteId;
use fleet_core::system::{Clock, MintError, SecureRandom, mint};
use secrecy::SecretString;
use tracing::{debug, info};

use super::audit::AuditService;
use super::users::{ActingUser, recheck};
use crate::ports::invites::{Invite, ListedInvite, NewInvite};
use crate::ports::page::{Page, Paged};
use crate::ports::store::{Store, StoreError};
use crate::ports::tokens::{MintTokenError, TokenKind, Tokens};

/// An invite with its state at the time it was read.
#[derive(Debug, Clone)]
pub struct InviteView {
    /// The invite and its people's names.
    pub invite: ListedInvite,
    /// What it is now.
    pub state: InviteState,
}

/// A new invite and its code, which exists only here.
#[derive(Debug)]
pub struct CreatedInvite {
    /// The code (`afk_iv_…`).
    pub code: SecretString,
    /// The invite, pending.
    pub view: InviteView,
}

/// Why an invite wasn't created.
#[derive(Debug, thiserror::Error)]
pub enum CreateInviteError {
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("the invite isn't allowed")]
    Denied(#[source] AuthzError),
    /// The creator already holds the most pending invites.
    #[error("the creator holds the most pending invites")]
    CapReached,
    /// The code couldn't be minted.
    #[error("the invite's code can't be minted")]
    Token(#[source] MintTokenError),
    /// The ID couldn't be minted.
    #[error("the invite's ID can't be minted")]
    Id(#[source] MintError),
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why an invite wasn't revoked.
#[derive(Debug, thiserror::Error)]
pub enum RevokeInviteError {
    /// There's no such invite.
    #[error("the invite doesn't exist")]
    NotFound,
    /// The acting user's session ended, or they were disabled, meanwhile.
    #[error("the acting session is gone")]
    SessionGone,
    /// `authorize()` refused it on the fresh facts.
    #[error("revoking the invite isn't allowed")]
    Denied(#[source] AuthzError),
    /// The invite registered a user: it's history, not revocable.
    #[error("the invite was used")]
    Used,
    /// The audit entry couldn't be built.
    #[error("the audit entry can't be built")]
    Audit(#[from] AuditError),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The invites' use cases.
#[derive(Debug, Clone)]
pub struct InviteService {
    clock: Arc<dyn Clock>,
    random: Arc<dyn SecureRandom>,
    store: Arc<dyn Store>,
    tokens: Arc<dyn Tokens>,
    ttl: InviteTtl,
    audit: AuditService,
}

impl InviteService {
    /// The service over `store`, minting codes with `tokens` and IDs from
    /// `random`, stamping with `clock`; invites live `ttl`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        random: Arc<dyn SecureRandom>,
        store: Arc<dyn Store>,
        tokens: Arc<dyn Tokens>,
        ttl: InviteTtl,
    ) -> Self {
        let audit = AuditService::new(Arc::clone(&clock), Arc::clone(&store));
        Self {
            clock,
            random,
            store,
            tokens,
            ttl,
            audit,
        }
    }

    /// Creates an invite for `role` by `by`; see the [module docs](self).
    ///
    /// # Errors
    /// [`CreateInviteError::SessionGone`], [`CreateInviteError::Denied`],
    /// [`CreateInviteError::CapReached`], and the server's failures.
    pub async fn create(
        &self,
        by: &ActingUser,
        role: Role,
    ) -> Result<CreatedInvite, CreateInviteError> {
        let code = self
            .tokens
            .mint(TokenKind::Invite)
            .map_err(CreateInviteError::Token)?;
        let now = self.clock.now();
        let id: InviteId = mint(now, self.random.as_ref()).map_err(CreateInviteError::Id)?;
        let expires_at = self.ttl.expires_at(now);
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, "an invite was refused: the session is gone");
            return Err(CreateInviteError::SessionGone);
        };
        authorize(
            &acting.actor,
            Permission::CreateInvite { role },
            &ResourceContext::Global,
        )
        .map_err(|error| {
            debug!(user_id = %by.actor.id, "an invite was refused");
            CreateInviteError::Denied(error)
        })?;
        if pending_cap_reached(tx.invites().count_pending_by(acting.actor.id, now).await?) {
            debug!(user_id = %by.actor.id, "an invite was refused: too many pending");
            return Err(CreateInviteError::CapReached);
        }
        let invite = Invite {
            id,
            code_hash: code.hash,
            role,
            created_by: acting.actor.id,
            created_at: now,
            expires_at,
            used_by: None,
            used_at: None,
            revoked_at: None,
        };
        tx.invites()
            .insert(&NewInvite {
                id,
                code_hash: invite.code_hash,
                role,
                created_by: invite.created_by,
                created_at: now,
                expires_at,
            })
            .await?;
        let metadata = AuditMetadata::new()
            .text("role", role.as_str())?
            .time("expires_at", expires_at)?;
        let entry = self
            .audit
            .entry(AuditAction::INVITE_CREATE, AuditOutcome::Success)
            .actor(by.actor.id)
            .ip(by.ip)
            .target(AuditTarget::Invite(id))
            .metadata(metadata);
        tx.commit(entry).await?;
        info!(user_id = %by.actor.id, invite_id = %id, "an invite was created");
        Ok(CreatedInvite {
            code: code.token,
            view: InviteView {
                invite: ListedInvite {
                    invite,
                    created_by_name: acting.username,
                    used_by_name: None,
                },
                state: InviteState::Pending,
            },
        })
    }

    /// The invites of `page`, newest first, each with its state now, and
    /// the next page's cursor.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn list(
        &self,
        page: Page<InviteId>,
    ) -> Result<Paged<InviteView, InviteId>, StoreError> {
        let rows = self.store.invites().list(page).await?;
        let now = self.clock.now();
        let views = rows.into_iter().map(|invite| view(invite, now)).collect();
        Ok(Paged::split(views, page.limit, |view| {
            view.invite.invite.id
        }))
    }

    /// The role of invite `id`, for the handler's check before
    /// [`InviteService::revoke`].
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn role_of(&self, id: InviteId) -> Result<Option<Role>, StoreError> {
        Ok(self
            .store
            .invites()
            .find(id)
            .await?
            .map(|invite| invite.role))
    }

    /// Revokes invite `id` for `by`; see the [module docs](self).
    ///
    /// # Errors
    /// [`RevokeInviteError::SessionGone`], [`RevokeInviteError::NotFound`],
    /// [`RevokeInviteError::Denied`], [`RevokeInviteError::Used`], and the
    /// server's failures.
    pub async fn revoke(&self, by: &ActingUser, id: InviteId) -> Result<(), RevokeInviteError> {
        let now = self.clock.now();
        let mut tx = self.store.write().await?;
        let Some(acting) = recheck(tx.as_mut(), by).await? else {
            debug!(user_id = %by.actor.id, "a revocation was refused: the session is gone");
            return Err(RevokeInviteError::SessionGone);
        };
        let invite = tx
            .invites()
            .find(id)
            .await?
            .ok_or(RevokeInviteError::NotFound)?;
        authorize(
            &acting.actor,
            Permission::RevokeInvite,
            &ResourceContext::Invite { role: invite.role },
        )
        .map_err(|error| {
            debug!(user_id = %by.actor.id, invite_id = %id, "a revocation was refused");
            RevokeInviteError::Denied(error)
        })?;
        match invite_state(&invite.facts(), now) {
            InviteState::Used => Err(RevokeInviteError::Used),
            // Over either way: nothing to write, and no second entry.
            InviteState::Revoked | InviteState::Expired => Ok(()),
            InviteState::Pending => {
                if tx.invites().revoke(id, now).await? {
                    let metadata = AuditMetadata::new().text("role", invite.role.as_str())?;
                    let entry = self
                        .audit
                        .entry(AuditAction::INVITE_REVOKE, AuditOutcome::Success)
                        .actor(by.actor.id)
                        .ip(by.ip)
                        .target(AuditTarget::Invite(id))
                        .metadata(metadata);
                    tx.commit(entry).await?;
                    info!(user_id = %by.actor.id, invite_id = %id, "an invite was revoked");
                }
                Ok(())
            }
        }
    }
}

/// `invite` with its state at `now`.
fn view(invite: ListedInvite, now: DateTime<Utc>) -> InviteView {
    let state = invite_state(&invite.invite.facts(), now);
    InviteView { invite, state }
}
