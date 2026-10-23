//! A session: the refresh, the store of its rotated token (`account.refresh`)
//! before the session starts, the cache until 10 minutes before expiry,
//! fresh requests, and a renamed profile (`account.rename`).

use core::time::Duration;

use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, RecordedValue};
use fleet_server::app::session_tokens::REUSE_MARGIN;
use fleet_server::ports::msauth::MinecraftSession;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::{Call, Script, gated, session};
use crate::support::service::{
    DAY, PROFILE, Setup, T0, assert_targets, at, bounded, json_logs, levels, mc_session, metadata,
    ms_tokens, poll_once, profile, text,
};

/// The rename's warning when its store fails.
const RENAME_FAILED: &str =
    "an account's new Minecraft name couldn't be stored; its next session tries again";

/// Scripts one refresh to `refresh` and one session `minecraft` of
/// `PROFILE` named `AfkBot1`.
fn script_session(setup: &Setup, n: u8) {
    setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
        &format!("access-{n}"),
        &format!("refresh-{n}"),
    ))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        &format!("minecraft-{n}"),
        PROFILE,
        "AfkBot1",
    ))));
}

async fn active(setup: &Setup) -> fleet_core::id::AccountId {
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await
}

fn token(session: &MinecraftSession) -> &str {
    session.token.expose_secret()
}

#[sqlx::test(migrations = false)]
async fn a_first_request_refreshes_stores_the_rotated_token_then_starts_the_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);

    let session = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(token(&session), "minecraft-1");
    assert_eq!(session.uuid.to_string(), PROFILE);
    assert_eq!(session.name.as_str(), "AfkBot1");
    assert_eq!(session.expires_at, at(T0 + DAY));
    assert_eq!(
        setup.fake.calls(),
        [
            Call::Refresh {
                refresh_token: "refresh-0".to_owned()
            },
            Call::MinecraftSession {
                access_token: "access-1".to_owned()
            },
        ]
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    let entries = setup.entries(&AuditAction::ACCOUNT_REFRESH).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, AuditOutcome::Success);
    assert_targets(&entries[0], id);
    assert_eq!(
        entries[0].metadata.entries(),
        metadata(&[("rotated", RecordedValue::Bool(true))])
    );
    assert_eq!(setup.server_entries().await, 1);
    assert_eq!(setup.status(id).await, AccountStatus::Active);
}

#[sqlx::test(migrations = false)]
async fn the_rotated_token_is_stored_before_the_session_starts(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    let (script, mut gate) = gated();
    setup.fake.script_session(script);

    let (answer, stored_meanwhile) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            let stored = setup.stored_token(id).await;
            let recorded = setup.entries(&AuditAction::ACCOUNT_REFRESH).await.len();
            gate.release(Ok(mc_session("minecraft-1", PROFILE, "AfkBot1")));
            (stored, recorded)
        })
    })
    .await;

    assert_eq!(token(&answer.unwrap()), "minecraft-1");
    assert_eq!(stored_meanwhile, ("refresh-1".to_owned(), 1));
}

#[sqlx::test(migrations = false)]
async fn a_refresh_without_a_new_token_stores_the_sent_one_as_not_rotated(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-0"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));

    bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(setup.stored_token(id).await, "refresh-0");
    let entries = setup.entries(&AuditAction::ACCOUNT_REFRESH).await;
    assert_eq!(
        entries[0].metadata.entries(),
        metadata(&[("rotated", RecordedValue::Bool(false))])
    );
}

#[sqlx::test(migrations = false)]
async fn a_second_request_reuses_the_cached_session(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);
    bounded(setup.service.session(id, false)).await.unwrap();
    setup.clock.advance(Duration::from_hours(12));

    let again = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(token(&again), "minecraft-1");
    assert_eq!(setup.fake.calls().len(), 2);
    assert_eq!(setup.server_entries().await, 1);
}

#[sqlx::test(migrations = false)]
async fn a_session_is_reused_only_while_more_than_the_margin_is_left(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);
    script_session(&setup, 2);
    bounded(setup.service.session(id, false)).await.unwrap();
    let margin = i64::try_from(REUSE_MARGIN.as_millis()).unwrap();

    setup.clock.set(at(T0 + DAY - margin - 1));
    let before = bounded(setup.service.session(id, false)).await.unwrap();
    setup.clock.set(at(T0 + DAY - margin));
    let at_the_margin = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(token(&before), "minecraft-1");
    assert_eq!(token(&at_the_margin), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_session_minted_inside_the_margin_is_handed_out_but_not_reused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    for n in 1..=2 {
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Ok(session(
            &format!("minecraft-{n}"),
            PROFILE,
            "AfkBot1",
            at(T0 + 5 * 60_000),
        ))));
    }

    let first = bounded(setup.service.session(id, false)).await.unwrap();
    let second = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(token(&first), "minecraft-1");
    assert_eq!(token(&second), "minecraft-2");
}

#[sqlx::test(migrations = false)]
async fn a_fresh_request_bypasses_the_cache(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    script_session(&setup, 1);
    script_session(&setup, 2);
    bounded(setup.service.session(id, false)).await.unwrap();

    let fresh = bounded(setup.service.session(id, true)).await.unwrap();
    let cached = bounded(setup.service.session(id, false)).await.unwrap();

    assert_eq!(token(&fresh), "minecraft-2");
    assert_eq!(token(&cached), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_fresh_request_reuses_a_session_whose_minting_started_after_it_arrived(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        PROFILE,
        "AfkBot1",
    ))));
    let mut first = Box::pin(setup.service.session(id, false));
    let mut fresh = Box::pin(setup.service.session(id, true));

    bounded(async {
        tokio::select! {
            biased;
            answer = &mut first => panic!("answered before the refresh: {answer:?}"),
            () = gate.entered() => {}
        }
    })
    .await;
    // The fresh request arrives while the refresh runs, before the minting.
    assert!(poll_once(&mut fresh).await.is_none());
    gate.release(Ok(ms_tokens("access-1", "refresh-1")));
    let (first, fresh) = bounded(async { tokio::join!(first, fresh) }).await;

    assert_eq!(token(&first.unwrap()), "minecraft-1");
    assert_eq!(token(&fresh.unwrap()), "minecraft-1");
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_fresh_request_doesnt_reuse_a_session_whose_minting_started_before_it_arrived(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    let (script, mut gate) = gated();
    setup.fake.script_session(script);
    script_session(&setup, 2);
    let mut first = Box::pin(setup.service.session(id, false));
    let mut fresh = Box::pin(setup.service.session(id, true));

    bounded(async {
        tokio::select! {
            biased;
            answer = &mut first => panic!("answered before the session: {answer:?}"),
            () = gate.entered() => {}
        }
    })
    .await;
    // The fresh request arrives while the session is being minted.
    assert!(poll_once(&mut fresh).await.is_none());
    gate.release(Ok(mc_session("minecraft-1", PROFILE, "AfkBot1")));
    let (first, fresh) = bounded(async { tokio::join!(first, fresh) }).await;

    assert_eq!(token(&first.unwrap()), "minecraft-1");
    assert_eq!(token(&fresh.unwrap()), "minecraft-2");
    assert_eq!(setup.refreshed_with(), ["refresh-0", "refresh-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_renamed_profile_is_handed_out_and_stored_even_when_only_its_case_changed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let renamed = setup
        .link(
            1,
            "AfkBot1",
            &profile(1),
            AccountStatus::Active,
            "refresh-a",
        )
        .await;
    let recased = setup
        .link(
            2,
            "afkbot2",
            &profile(2),
            AccountStatus::Active,
            "refresh-b",
        )
        .await;
    for (n, name) in [(1, "NewName_9"), (2, "AfkBot2")] {
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            &format!("minecraft-{n}"),
            &profile(n),
            name,
        ))));
    }

    let first = bounded(setup.service.session(renamed, false))
        .await
        .unwrap();
    let second = bounded(setup.service.session(recased, false))
        .await
        .unwrap();

    assert_eq!(first.name.as_str(), "NewName_9");
    assert_eq!(second.name.as_str(), "AfkBot2");
    assert_eq!(setup.account(renamed).await.name.as_str(), "NewName_9");
    assert_eq!(setup.account(recased).await.name.as_str(), "AfkBot2");
    let entries = setup.entries(&AuditAction::ACCOUNT_RENAME).await;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].outcome, AuditOutcome::Success);
    assert_targets(&entries[0], renamed);
    assert_eq!(
        entries[0].metadata.entries(),
        metadata(&[
            ("name", text("NewName_9")),
            ("previous_name", text("AfkBot1"))
        ])
    );
    assert_targets(&entries[1], recased);
    assert_eq!(
        entries[1].metadata.entries(),
        metadata(&[
            ("name", text("AfkBot2")),
            ("previous_name", text("afkbot2"))
        ])
    );
}

#[sqlx::test(migrations = false)]
async fn a_rename_tells_the_sockets_its_bot_after_its_commit_and_the_same_name_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let renamed = setup
        .link(
            1,
            "AfkBot1",
            &profile(1),
            AccountStatus::Active,
            "refresh-a",
        )
        .await;
    let bot = setup.add_bot(1).await;
    for (n, name) in [(1, "NewName_9"), (2, "NewName_9")] {
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            &format!("minecraft-{n}"),
            &profile(1),
            name,
        ))));
    }

    bounded(setup.service.session(renamed, false))
        .await
        .unwrap();
    let after_renaming = (setup.heard.take(), setup.heard.once(&[bot]));
    bounded(setup.service.session(renamed, true)).await.unwrap();

    assert_eq!(after_renaming.0, after_renaming.1);
    assert_eq!(setup.heard.take(), [], "the same name again");
}

#[sqlx::test(migrations = false)]
async fn a_failed_rename_still_hands_out_the_session_and_its_next_session_tries_again(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    for n in 1..=2 {
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Ok(mc_session(
            &format!("minecraft-{n}"),
            PROFILE,
            "NewName_9",
        ))));
    }
    // The token's store passes; the rename's write is refused.
    setup.store.fail_writes_after(1, 1);
    let (logs, guard) = json_logs();

    let first = bounded(setup.service.session(id, false)).await.unwrap();
    let kept_name = setup.account(id).await.name;
    let fresh = bounded(setup.service.session(id, true)).await.unwrap();

    drop(guard);
    assert_eq!(first.name.as_str(), "NewName_9");
    assert_eq!(kept_name.as_str(), "AfkBot1");
    assert_eq!(levels(&logs, RENAME_FAILED), ["WARN"], "{}", logs.text());
    assert_eq!(fresh.name.as_str(), "NewName_9");
    assert_eq!(setup.account(id).await.name.as_str(), "NewName_9");
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_RENAME).await.len(), 1);
    assert_eq!(setup.stored_token(id).await, "refresh-2");
}
