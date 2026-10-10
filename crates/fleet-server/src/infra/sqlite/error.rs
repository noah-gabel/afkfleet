//! Turns sqlx's errors into [`StoreError`]s, and recognizes the unique
//! violations a repository maps to its own errors.

use crate::ports::store::StoreError;

/// SQLite's extended code for a UNIQUE constraint violation.
const SQLITE_CONSTRAINT_UNIQUE: &str = "2067";

/// `SQLITE_BUSY` and `SQLITE_LOCKED`, the primary codes of a lock wait that ran
/// out.
const BUSY_CODES: [i32; 2] = [5, 6];

/// Maps a sqlx error: an acquire timeout or a lock that stayed taken is
/// [`StoreError::Busy`], anything else [`StoreError::Backend`] with the sqlx
/// error as its source.
pub(super) fn store_error(error: sqlx::Error) -> StoreError {
    let busy = match &error {
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Database(database) => database.code().is_some_and(|code| is_busy(&code)),
        _ => false,
    };
    if busy {
        StoreError::Busy
    } else {
        StoreError::Backend(Box::new(error))
    }
}

/// The `table.column` a UNIQUE violation names (SQLite's message is
/// `UNIQUE constraint failed: users.username`), or `None` when `error` isn't
/// a UNIQUE violation.
pub(super) fn unique_violation(error: &sqlx::Error) -> Option<String> {
    let database = error.as_database_error()?;
    if database.code().as_deref() != Some(SQLITE_CONSTRAINT_UNIQUE) {
        return None;
    }
    database
        .message()
        .strip_prefix("UNIQUE constraint failed: ")
        .map(str::to_owned)
}

/// Whether a database error's code says a lock wait ran out.
fn is_busy(code: &str) -> bool {
    code.parse::<i32>()
        .is_ok_and(|code| BUSY_CODES.contains(&(code & 0xff)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_acquire_timeout_is_busy() {
        assert!(matches!(
            store_error(sqlx::Error::PoolTimedOut),
            StoreError::Busy
        ));
    }

    #[test]
    fn other_errors_keep_sqlxs_error_as_their_source() {
        let error = store_error(sqlx::Error::RowNotFound);

        let source = std::error::Error::source(&error).unwrap();
        assert!(matches!(
            source.downcast_ref::<sqlx::Error>(),
            Some(sqlx::Error::RowNotFound)
        ));
    }

    #[test]
    fn busy_and_locked_codes_count_as_busy() {
        assert!(is_busy("5"));
        assert!(is_busy("6"));
        assert!(is_busy("517"), "SQLITE_BUSY_SNAPSHOT");
        assert!(!is_busy("19"));
        assert!(!is_busy("2067"));
        assert!(!is_busy("x"));
    }

    #[test]
    fn a_non_database_error_is_no_unique_violation() {
        assert_eq!(unique_violation(&sqlx::Error::RowNotFound), None);
    }
}
