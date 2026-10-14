//! The SQL behind [`crate::ports::envelopes`]: every stored envelope, the
//! conditional swap and the count of envelopes under other keys.
//!
//! An envelope's key is the 4 bytes after its version byte, so SQL compares
//! the first 5 bytes, `substr(envelope, 1, 5)`, with the header of the key
//! (`header_of`). A malformed envelope never matches a header.

use sqlx::{SqliteConnection, SqlitePool};

use super::convert::{id_text, parse_id};
use super::error::store_error;
use crate::ports::envelopes::{SealedEnvelope, SealedRecord};
use crate::ports::store::StoreError;
use crate::ports::vault::header_of;

/// A row's ID and envelope, before the ID is checked.
struct EnvelopeRow {
    rowid: i64,
    id: String,
    envelope: Vec<u8>,
}

/// Every stored envelope: the TOTP secrets, then the Microsoft refresh
/// tokens. Two statements on the read pool: a row that changes between
/// them is caught by the rotation's conditional swaps and its final count.
pub(super) async fn all(pool: &SqlitePool) -> Result<Vec<SealedEnvelope>, StoreError> {
    let totp = sqlx::query_as!(
        EnvelopeRow,
        r#"SELECT rowid AS "rowid!: i64", user_id AS "id!",
                  secret_envelope AS "envelope!: Vec<u8>"
           FROM user_mfa ORDER BY user_id"#
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?;
    let refresh = sqlx::query_as!(
        EnvelopeRow,
        r#"SELECT rowid AS "rowid!: i64", id AS "id!",
                  refresh_envelope AS "envelope!: Vec<u8>"
           FROM mc_accounts WHERE refresh_envelope IS NOT NULL ORDER BY id"#
    )
    .fetch_all(pool)
    .await
    .map_err(store_error)?;
    let corrupt = |table, column, rowid| StoreError::Corrupt {
        table,
        column,
        rowid,
    };
    let totp = totp.into_iter().map(|row| {
        let user = parse_id(&row.id).ok_or_else(|| corrupt("user_mfa", "user_id", row.rowid))?;
        Ok(SealedEnvelope {
            record: SealedRecord::TotpSecret(user),
            envelope: row.envelope,
        })
    });
    let refresh = refresh.into_iter().map(|row| {
        let account = parse_id(&row.id).ok_or_else(|| corrupt("mc_accounts", "id", row.rowid))?;
        Ok(SealedEnvelope {
            record: SealedRecord::RefreshToken(account),
            envelope: row.envelope,
        })
    });
    totp.chain(refresh).collect()
}

/// Replaces `record`'s envelope `old` with `new` while the row holds `old`.
pub(super) async fn swap(
    conn: &mut SqliteConnection,
    record: SealedRecord,
    old: &[u8],
    new: &[u8],
) -> Result<bool, StoreError> {
    let done = match record {
        SealedRecord::TotpSecret(user) => {
            let user = id_text(user);
            sqlx::query!(
                "UPDATE user_mfa SET secret_envelope = ?
                 WHERE user_id = ? AND secret_envelope = ?",
                new,
                user,
                old
            )
            .execute(conn)
            .await
        }
        SealedRecord::RefreshToken(account) => {
            let account = id_text(account);
            sqlx::query!(
                "UPDATE mc_accounts SET refresh_envelope = ?
                 WHERE id = ? AND refresh_envelope = ?",
                new,
                account,
                old
            )
            .execute(conn)
            .await
        }
    }
    .map_err(store_error)?;
    Ok(done.rows_affected() == 1)
}

/// How many stored envelopes don't start with `key_id`'s header.
pub(super) async fn count_not_under(
    conn: &mut SqliteConnection,
    key_id: u32,
) -> Result<u64, StoreError> {
    let header = header_of(key_id);
    let header = header.as_slice();
    let count = sqlx::query_scalar!(
        r#"SELECT (SELECT count(*) FROM user_mfa WHERE substr(secret_envelope, 1, 5) != ?)
                + (SELECT count(*) FROM mc_accounts
                   WHERE refresh_envelope IS NOT NULL AND substr(refresh_envelope, 1, 5) != ?)
           AS "count!: i64""#,
        header,
        header
    )
    .fetch_one(conn)
    .await
    .map_err(store_error)?;
    // A count is never negative.
    Ok(u64::try_from(count).unwrap_or(0))
}
