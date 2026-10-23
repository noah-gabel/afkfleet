//! Microsoft's refusals: `invalid_grant` marks the account `reauth_required`,
//! `NotOwned`, `NoProfile` and the Xbox refusals mark it `refused` with their
//! reason (`account.set_status`), and a profile with another UUID marks it
//! `refused` with `profile_mismatch` (`security.account_profile_mismatch`).
//! Each change is made only while the account is still active with the
//! envelope that was used.

use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, RecordedValue};
use fleet_server::app::session_tokens::SessionTokenError;
use fleet_server::ports::msauth::{RefreshError, SessionError, XboxRefusal};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::msauth::{Script, gated};
use crate::support::service::{
    OTHER_PROFILE, PROFILE, Setup, assert_targets, bounded, json_logs, levels, mc_session,
    metadata, ms_tokens, profile, text,
};

const REAUTH: &str = "Microsoft refused an account's refresh token; it must be linked again";
const REFUSED: &str = "Minecraft refused an account; it must be linked again";
const MISMATCH: &str =
    "an account's refresh token leads to another Minecraft profile; the account was refused";

async fn active(setup: &Setup) -> fleet_core::id::AccountId {
    setup
        .link(1, "AfkBot1", PROFILE, AccountStatus::Active, "refresh-0")
        .await
}

#[sqlx::test(migrations = false)]
async fn invalid_grant_marks_the_account_reauth_required(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    let (logs, guard) = json_logs();

    let answer = bounded(setup.service.session(id, false)).await;
    let again = bounded(setup.service.session(id, true)).await;

    drop(guard);
    for answer in [answer, again] {
        assert!(
            matches!(
                answer,
                Err(SessionTokenError::NotActive {
                    status: AccountStatus::ReauthRequired
                })
            ),
            "{answer:?}"
        );
    }
    assert_eq!(setup.status(id).await, AccountStatus::ReauthRequired);
    let entries = setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, AuditOutcome::Success);
    assert_targets(&entries[0], id);
    assert_eq!(
        entries[0].metadata.entries(),
        metadata(&[
            ("previous_status", text("active")),
            ("reason", text("invalid_grant")),
            ("status", text("reauth_required")),
        ])
    );
    assert_eq!(setup.server_entries().await, 1);
    assert_eq!(levels(&logs, REAUTH), ["WARN"], "{}", logs.text());
    assert_eq!(setup.refreshed_with(), ["refresh-0"]);
}

#[sqlx::test(migrations = false)]
async fn a_status_mark_tells_the_sockets_its_bot_after_its_commit_and_a_refusal_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let bot = setup.add_bot(1).await;
    setup
        .fake
        .script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));

    let marked = bounded(setup.service.session(id, false)).await;
    let after_marking = (setup.heard.take(), setup.heard.once(&[bot]));
    let refused = bounded(setup.service.session(id, true)).await;

    assert!(marked.is_err(), "{marked:?}");
    assert_eq!(after_marking.0, after_marking.1);
    assert!(
        matches!(refused, Err(SessionTokenError::NotActive { .. })),
        "{refused:?}"
    );
    assert_eq!(setup.heard.take(), []);
}

#[sqlx::test(migrations = false)]
async fn a_profile_mismatch_tells_the_sockets_its_bot_after_its_commit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let bot = setup.add_bot(1).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        OTHER_PROFILE,
        "AfkBot1",
    ))));

    let refused = bounded(setup.service.session(id, false)).await;

    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        setup.status(id).await,
        AccountStatus::Refused(RefusalReason::ProfileMismatch)
    );
    assert_eq!(setup.heard.take(), setup.heard.once(&[bot]));
}

#[sqlx::test(migrations = false)]
async fn invalid_grant_after_a_link_again_meanwhile_changes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.relink(id, "relinked").await;
            gate.release(Err(RefreshError::InvalidGrant));
        })
    })
    .await;

    assert!(
        matches!(answer, Err(SessionTokenError::ChangedMeanwhile)),
        "{answer:?}"
    );
    assert_eq!(setup.status(id).await, AccountStatus::Active);
    assert_eq!(setup.stored_token(id).await, "relinked");
    assert_eq!(setup.server_entries().await, 0);
}

#[sqlx::test(migrations = false)]
async fn each_refusal_marks_the_account_refused_with_its_reason_after_storing_the_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let cases = [
        (SessionError::NotOwned, RefusalReason::NotOwned, None),
        (SessionError::NoProfile, RefusalReason::NoProfile, None),
        (
            SessionError::Xbox(XboxRefusal::Banned),
            RefusalReason::XboxBanned,
            None,
        ),
        (
            SessionError::Xbox(XboxRefusal::NoXboxProfile),
            RefusalReason::XboxNoXboxProfile,
            None,
        ),
        (
            SessionError::Xbox(XboxRefusal::CountryUnavailable),
            RefusalReason::XboxCountryUnavailable,
            None,
        ),
        (
            SessionError::Xbox(XboxRefusal::AdultVerification),
            RefusalReason::XboxAdultVerification,
            None,
        ),
        (
            SessionError::Xbox(XboxRefusal::ChildAccount),
            RefusalReason::XboxChildAccount,
            None,
        ),
        (
            SessionError::Xbox(XboxRefusal::Other {
                xerr: 2_148_916_240,
            }),
            RefusalReason::XboxOther,
            Some(2_148_916_240),
        ),
    ];
    let (logs, guard) = json_logs();

    for (n, (refusal, reason, xerr)) in (1_u8..).zip(cases) {
        let id = setup
            .link(
                n,
                &format!("AfkBot{n}"),
                &profile(n),
                AccountStatus::Active,
                "refresh-0",
            )
            .await;
        setup.fake.script_refresh(Script::Answer(Ok(ms_tokens(
            "access-1",
            &format!("rotated-{n}"),
        ))));
        setup.fake.script_session(Script::Answer(Err(refusal)));

        let answer = bounded(setup.service.session(id, false)).await;

        let refused = AccountStatus::Refused(reason);
        assert!(
            matches!(answer, Err(SessionTokenError::NotActive { status }) if status == refused),
            "{reason:?}: {answer:?}"
        );
        assert_eq!(setup.status(id).await, refused);
        assert_eq!(setup.stored_token(id).await, format!("rotated-{n}"));
        let entries = setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await;
        let entry = entries.last().unwrap();
        assert_targets(entry, id);
        let mut expected = vec![
            ("previous_status", text("active")),
            ("reason", text(reason.as_str())),
            ("status", text("refused")),
        ];
        if let Some(xerr) = xerr {
            expected.push(("xerr", RecordedValue::Int(xerr)));
        }
        assert_eq!(entry.metadata.entries(), metadata(&expected), "{reason:?}");
    }
    drop(guard);
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await.len(),
        8
    );
    assert_eq!(levels(&logs, REFUSED).len(), 8, "{}", logs.text());
    assert!(levels(&logs, REFUSED).iter().all(|level| level == "WARN"));
}

#[sqlx::test(migrations = false)]
async fn a_profile_with_another_uuid_refuses_the_account_and_records_a_security_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    setup.fake.script_session(Script::Answer(Ok(mc_session(
        "minecraft-1",
        OTHER_PROFILE,
        "AfkBot1",
    ))));
    let (logs, guard) = json_logs();

    let answer = bounded(setup.service.session(id, false)).await;
    let again = bounded(setup.service.session(id, false)).await;

    drop(guard);
    let mismatch = AccountStatus::Refused(RefusalReason::ProfileMismatch);
    for answer in [answer, again] {
        assert!(
            matches!(answer, Err(SessionTokenError::NotActive { status }) if status == mismatch),
            "{answer:?}"
        );
    }
    assert_eq!(setup.status(id).await, mismatch);
    let entries = setup
        .entries(&AuditAction::SECURITY_ACCOUNT_PROFILE_MISMATCH)
        .await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].outcome, AuditOutcome::Failure);
    assert_targets(&entries[0], id);
    assert_eq!(
        entries[0].metadata.entries(),
        metadata(&[
            ("previous_status", text("active")),
            ("status", text("refused"))
        ])
    );
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await, []);
    assert_eq!(levels(&logs, MISMATCH), ["ERROR"], "{}", logs.text());
    assert_eq!(setup.sessions_started_with(), ["access-1"]);
}

#[sqlx::test(migrations = false)]
async fn a_refusal_after_the_status_changed_meanwhile_keeps_the_new_status(
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

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.set_status(id, AccountStatus::Pending).await;
            gate.release(Err(SessionError::NotOwned));
        })
    })
    .await;

    assert!(
        matches!(
            answer,
            Err(SessionTokenError::NotActive {
                status: AccountStatus::Pending
            })
        ),
        "{answer:?}"
    );
    assert_eq!(setup.status(id).await, AccountStatus::Pending);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await, []);
}

/// A status changed while Microsoft rotated the token: the rotated token is
/// still stored, and no session is started.
#[sqlx::test(migrations = false)]
async fn a_status_change_during_the_refresh_still_stores_the_rotated_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.set_status(id, AccountStatus::Pending).await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
        })
    })
    .await;

    assert!(
        matches!(
            answer,
            Err(SessionTokenError::NotActive {
                status: AccountStatus::Pending
            })
        ),
        "{answer:?}"
    );
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_REFRESH).await.len(), 1);
    assert_eq!(setup.sessions_started_with(), Vec::<String>::new());
}

/// A link again while Microsoft rotated the old token: the new link's token
/// stays, the old rotated one is dropped.
#[sqlx::test(migrations = false)]
async fn a_link_again_during_the_refresh_keeps_the_new_links_token(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    let (script, mut gate) = gated();
    setup.fake.script_refresh(script);

    let (answer, ()) = bounded(async {
        tokio::join!(setup.service.session(id, false), async {
            gate.entered().await;
            setup.relink(id, "relinked").await;
            gate.release(Ok(ms_tokens("access-1", "refresh-1")));
        })
    })
    .await;

    assert!(
        matches!(answer, Err(SessionTokenError::ChangedMeanwhile)),
        "{answer:?}"
    );
    assert_eq!(setup.stored_token(id).await, "relinked");
    assert_eq!(setup.server_entries().await, 0);
    assert_eq!(setup.sessions_started_with(), Vec::<String>::new());
}

#[sqlx::test(migrations = false)]
async fn an_account_deleted_during_the_refresh_is_not_found_whatever_microsoft_answered(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let answers = [
        Ok(ms_tokens("access-1", "refresh-1")),
        Err(RefreshError::InvalidGrant),
    ];

    for (n, refreshed) in (1_u8..).zip(answers) {
        let id = setup
            .link(
                n,
                &format!("AfkBot{n}"),
                &profile(n),
                AccountStatus::Active,
                "refresh-0",
            )
            .await;
        let (script, mut gate) = gated();
        setup.fake.script_refresh(script);

        let (answer, ()) = bounded(async {
            tokio::join!(setup.service.session(id, false), async {
                gate.entered().await;
                setup.delete(id).await;
                gate.release(refreshed);
            })
        })
        .await;

        assert!(
            matches!(answer, Err(SessionTokenError::NotFound)),
            "{n}: {answer:?}"
        );
    }
    assert_eq!(setup.server_entries().await, 0);
}

#[sqlx::test(migrations = false)]
async fn a_refusal_whose_status_write_fails_leaves_the_status_alone(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = Setup::new(options).await;
    let id = active(&setup).await;
    setup
        .fake
        .script_refresh(Script::Answer(Ok(ms_tokens("access-1", "refresh-1"))));
    setup
        .fake
        .script_session(Script::Answer(Err(SessionError::NotOwned)));
    // The token's store passes; the status write is refused.
    setup.store.fail_writes_after(1, 1);

    let answer = bounded(setup.service.session(id, false)).await;

    assert!(
        matches!(answer, Err(SessionTokenError::Store(_))),
        "{answer:?}"
    );
    assert_eq!(setup.status(id).await, AccountStatus::Active);
    assert_eq!(setup.stored_token(id).await, "refresh-1");
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_SET_STATUS).await, []);
}
