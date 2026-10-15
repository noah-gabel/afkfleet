//! What a viewer sees of the accounts: the ones they may see, the pending
//! links they may review, and one account (Plan.md P9.6, ADR-0018).

use fleet_core::account::awaits_review;
use fleet_core::authz::{Actor, Permission, authorize};
use fleet_core::id::AccountId;
use tracing::error;

use super::AccountService;
use crate::ports::accounts::ListedAccount;
use crate::ports::page::{Page, Paged};
use crate::ports::store::StoreError;

/// Why a list of accounts couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ListAccountsError {
    /// The database listed an account `authorize()` doesn't let the viewer
    /// see or review: a bug in the query, answered as an internal error
    /// rather than shown.
    #[error("the database listed an account the viewer may not see")]
    Disagreement,
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What a list logs when the database and `authorize()` disagree.
const DISAGREEMENT: &str = "the database listed an account authorize() refuses; nothing was shown";

impl AccountService {
    /// The accounts `viewer` may see on `page`, newest first, and the next
    /// page's cursor.
    ///
    /// # Errors
    /// [`ListAccountsError::Disagreement`], or [`ListAccountsError::Store`].
    pub async fn list(
        &self,
        viewer: &Actor,
        page: Page<AccountId>,
    ) -> Result<Paged<ListedAccount, AccountId>, ListAccountsError> {
        let rows = self.store.accounts().list_visible(*viewer, page).await?;
        checked(viewer, Permission::ViewBot, &rows)?;
        Ok(Paged::split(rows, page.limit, |account| account.id))
    }

    /// The pending links `viewer` may approve or refuse on `page`, newest
    /// first, and the next page's cursor.
    ///
    /// # Errors
    /// [`ListAccountsError::Disagreement`], or [`ListAccountsError::Store`].
    pub async fn awaiting_review(
        &self,
        viewer: &Actor,
        page: Page<AccountId>,
    ) -> Result<Paged<ListedAccount, AccountId>, ListAccountsError> {
        let rows = self.store.accounts().awaiting_review(*viewer, page).await?;
        checked(viewer, Permission::ReviewLink, &rows)?;
        if let Some(account) = rows.iter().find(|account| !awaits_review(account.status)) {
            error!(account_id = %account.id, "{DISAGREEMENT}");
            return Err(ListAccountsError::Disagreement);
        }
        Ok(Paged::split(rows, page.limit, |account| account.id))
    }

    /// The account `id` as `viewer` sees it, whether or not they may: the
    /// caller asks `authorize()`.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn view(
        &self,
        viewer: &Actor,
        id: AccountId,
    ) -> Result<Option<ListedAccount>, StoreError> {
        self.store.accounts().listed(id, viewer.id).await
    }
}

/// Checks every row the database listed for `viewer` against `authorize()`:
/// one it refuses is never shown, and the whole list fails.
fn checked(
    viewer: &Actor,
    permission: Permission,
    rows: &[ListedAccount],
) -> Result<(), ListAccountsError> {
    match rows
        .iter()
        .find(|account| authorize(viewer, permission, &account.context()).is_err())
    {
        Some(account) => {
            error!(account_id = %account.id, "{DISAGREEMENT}");
            Err(ListAccountsError::Disagreement)
        }
        None => Ok(()),
    }
}
