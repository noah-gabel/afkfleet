//! The modes' use cases (Plan.md P11.4; ADR-0010; ADR-0020, S12–S19).
//!
//! - **Listing** ([`ModeService::list`]): the modes a viewer may see,
//!   newest first, filtered by the database and checked again, row by row,
//!   with `authorize(ViewMode)`; a row the two disagree on is an internal
//!   error that shows nothing (as `GET /bots` answers, ADR-0019, S54). What
//!   each viewer sees of a mode (its owner, the rights, a command's
//!   arguments) is the HTTP layer's field rule (S14, S15).

use std::sync::Arc;

use fleet_core::authz::{Actor, Permission, authorize};
use fleet_core::id::ModeId;
use tracing::error;

use crate::ports::modes::ListedMode;
use crate::ports::page::{Page, Paged};
use crate::ports::store::{Store, StoreError};

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
    store: Arc<dyn Store>,
}

impl ModeService {
    /// The modes over `store`.
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
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
