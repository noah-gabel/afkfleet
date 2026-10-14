//! A stored refresh token that can't be used: an envelope that doesn't open,
//! or a plaintext that isn't a token. Nothing is handed out, nothing is
//! written or audited, Microsoft isn't asked, and one `error` line names the
//! account, never a byte.

use fleet_core::account::AccountStatus;
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::msauth::{MAX_TOKEN_BYTES, TokenError};
use fleet_server::ports::vault::{Vault as _, VaultError, VaultPurpose};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::service::{Setup, bounded, json_logs, levels, profile, vault_with_key};

const UNUSABLE: &str = "an account's stored refresh token can't be used";

#[sqlx::test(migrations = false)]
async fn an_envelope_that_doesnt_open_is_refused_without_a_write_or_a_call(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let swapped = setup.seal(Setup::account_id(9), b"refresh-of-another-account");
    let under_key_2 = vault_with_key(2, &setup.random)
        .seal(
            VaultPurpose::MsRefreshToken,
            Setup::account_id(2).into(),
            b"refresh-under-key-2",
        )
        .unwrap();
    let mut malformed = setup.seal(Setup::account_id(3), b"refresh-malformed");
    malformed[0] = 2;
    let cases = [(1, swapped), (2, under_key_2), (3, malformed)];
    let (logs, guard) = json_logs();

    let mut answers = Vec::new();
    for (n, envelope) in cases {
        let id = setup
            .link_with_envelope(
                n,
                &format!("AfkBot{n}"),
                &profile(n),
                AccountStatus::Active,
                envelope,
            )
            .await;
        answers.push(bounded(setup.service.session(id, false)).await);
    }

    drop(guard);
    assert!(
        matches!(
            answers.as_slice(),
            [
                Err(SessionTokenError::Envelope(VaultError::Rejected {
                    key_id: 1
                })),
                Err(SessionTokenError::Envelope(VaultError::UnknownKey {
                    key_id: 2
                })),
                Err(SessionTokenError::Envelope(VaultError::Malformed)),
            ]
        ),
        "{answers:?}"
    );
    assert!(
        answers
            .iter()
            .all(|answer| answer.as_ref().is_err_and(|error| !error.retryable()))
    );
    assert_eq!(setup.fake.calls(), []);
    assert_eq!(setup.server_entries().await, 0);
    assert_eq!(levels(&logs, UNUSABLE), ["ERROR", "ERROR", "ERROR"]);
    let lines = logs.lines_with(UNUSABLE).unwrap();
    assert_eq!(
        lines[0]["account_id"],
        Setup::account_id(1).to_string(),
        "{}",
        logs.text()
    );
}

#[sqlx::test(migrations = false)]
async fn a_stored_text_that_isnt_a_token_is_refused_without_a_write_or_a_call(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let too_long = vec![b'x'; MAX_TOKEN_BYTES + 1];
    let cases: [(u8, &[u8]); 3] = [
        (1, b"has a space"),
        (2, &[0xFF, 0xFE, 0x41]),
        (3, too_long.as_slice()),
    ];
    let (logs, guard) = json_logs();

    let mut answers = Vec::new();
    for (n, plaintext) in cases {
        let envelope = setup.seal(Setup::account_id(n), plaintext);
        let id = setup
            .link_with_envelope(
                n,
                &format!("AfkBot{n}"),
                &profile(n),
                AccountStatus::Active,
                envelope,
            )
            .await;
        answers.push(bounded(setup.service.session(id, false)).await);
    }

    drop(guard);
    assert!(
        matches!(
            answers.as_slice(),
            [
                Err(SessionTokenError::StoredToken(TokenError::Character)),
                Err(SessionTokenError::StoredToken(TokenError::Character)),
                Err(SessionTokenError::StoredToken(TokenError::Length)),
            ]
        ),
        "{answers:?}"
    );
    assert_eq!(setup.fake.calls(), []);
    assert_eq!(setup.server_entries().await, 0);
    assert_eq!(levels(&logs, UNUSABLE), ["ERROR", "ERROR", "ERROR"]);
    assert!(!logs.text().contains("has a space"), "{}", logs.text());
}
