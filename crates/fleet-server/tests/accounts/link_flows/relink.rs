//! Linking an account again (Plan.md P9.6; ADR-0018, group E): `POST
//! /api/v1/accounts/{id}/relink`, a second kind of link flow through
//! serve's own app. Manage starts it, without a step-up, for a Microsoft
//! account that isn't pending; it shares the user's one flow, the daily
//! count and the caps, without a quota; its commit re-checks the account
//! and the grant, refuses another profile, and otherwise replaces the token
//! and makes the account active with the profile's current name.

use core::num::NonZeroU32;
use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::Method;
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget};
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::{AccountId, BotId, LinkFlowId};
use fleet_core::mode::ModePreset;
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::config::{AccountsConfig, LinkFlowLimits, RateLimitConfig};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::bots::NewBot;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::vault::{Vault as _, VaultPurpose};
use fleet_testkit::system::SeededRandom;
use secrecy::{ExposeSecret as _, SecretBox};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::approval;
use crate::support::http::{HttpSetup, Person, Seen, meta, text};
use crate::support::link::USER_CODE;
use crate::support::msauth::{Call, Script, device_flow, session};
use crate::support::service::{DAY, OTHER_PROFILE, T0, at, bounded, profile};

async fn relink(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::POST, &format!("/accounts/{id}/relink"), by, None)
        .await
}

/// The flow a start answered.
fn flow_of(seen: &Seen) -> LinkFlowId {
    assert_eq!(seen.status, 201, "{}", seen.text);
    seen.body["flow_id"].as_str().unwrap().parse().unwrap()
}

/// Scripts the next start; its code expires 15 minutes after T0.
fn script_start(setup: &HttpSetup, code: &str) {
    setup.fake.script_start(Script::Answer(Ok(device_flow(
        code,
        USER_CODE,
        at(T0 + 900_000),
    ))));
}

/// Scripts the approval with `refresh-<n>`, then a session of `uuid` named
/// `name`.
fn script_approval(setup: &HttpSetup, n: u8, uuid: &str, name: &str) {
    setup.fake.script_poll(Script::Answer(Ok(approval(n))));
    setup.fake.script_session(Script::Answer(Ok(session(
        "minecraft-1",
        uuid,
        name,
        at(T0 + DAY),
    ))));
}

/// The flow `id`'s status once it ended; fails the test after 10 seconds.
async fn ended(setup: &HttpSetup, id: LinkFlowId) -> LinkFlowStatus {
    bounded(async {
        loop {
            match setup.link_flows.status(id).map(|view| view.status) {
                Some(LinkFlowStatus::Waiting { .. } | LinkFlowStatus::Finishing) | None => {}
                Some(status) => return status,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

/// The refresh token stored for `id`, opened with the harness's vault key.
async fn stored_token(setup: &HttpSetup, id: AccountId) -> String {
    let AccountCredential::Microsoft {
        refresh_envelope, ..
    } = setup.stored(id).await.unwrap().credential
    else {
        panic!("an offline account has no token");
    };
    let vault = XChaChaVault::new(
        VaultKeys::from_keys(
            BTreeMap::from([(1, SecretBox::new(Box::new([0x21; 32])))]),
            1,
        )
        .unwrap(),
        Arc::new(SeededRandom::new(1)),
    );
    let plaintext = vault
        .open(VaultPurpose::MsRefreshToken, id.into(), &refresh_envelope)
        .unwrap();
    String::from_utf8(plaintext.expose_secret().clone()).unwrap()
}

#[sqlx::test(migrations = false)]
async fn a_relink_replaces_the_token_and_takes_the_profiles_current_name(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::Active))
        .await;
    script_start(&setup, "device-code-1");
    script_approval(&setup, 1, &profile(3), "Renamed_3");

    let started = relink(&setup, &alice, id).await;
    let flow = flow_of(&started);
    let status = ended(&setup, flow).await;
    let read = setup.status(&alice, &flow.to_string()).await;

    assert_eq!(started.body["user_code"], USER_CODE);
    assert_eq!(
        status,
        LinkFlowStatus::Linked {
            account: id,
            status: AccountStatus::Active,
            name: "Renamed_3".parse().unwrap(),
        }
    );
    assert_eq!(read.body["status"], "linked");
    assert_eq!(read.body["account_id"], id.to_string());
    assert_eq!(read.body["account_status"], "active");
    let stored = setup.stored(id).await.unwrap();
    assert_eq!(stored.name.as_str(), "Renamed_3");
    assert_eq!(stored.status(), AccountStatus::Active);
    assert_eq!(stored_token(&setup, id).await, "refresh-1");
    let starts = setup.entries(&AuditAction::ACCOUNT_START_LINK).await;
    assert_eq!(starts.len(), 1);
    assert_eq!(meta(&starts[0], "account"), Some(text(&id.to_string())));
    assert_eq!(meta(&starts[0], "flow"), Some(text(&flow.to_string())));
    let relinks = setup.entries(&AuditAction::ACCOUNT_RELINK).await;
    assert_eq!(relinks.len(), 1);
    let entry = &relinks[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(alice.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "flow"), Some(text(&flow.to_string())));
    assert_eq!(meta(entry, "previous_status"), Some(text("active")));
    assert_eq!(meta(entry, "name"), Some(text("Renamed_3")));
    assert_eq!(meta(entry, "previous_name"), Some(text("AfkBot3")));
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_LINK).await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_refused_or_reauth_account_comes_back_active_without_its_reason(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let statuses = [
        (AccountStatus::ReauthRequired, "reauth_required"),
        (AccountStatus::Refused(RefusalReason::XboxBanned), "refused"),
    ];
    for (n, (status, name)) in (1..).zip(statuses) {
        let owner = setup.person(n, Role::Member).await;
        let id = setup.add_account(owner.id, 10 + n, Some(status)).await;
        script_start(&setup, &format!("device-code-{n}"));
        script_approval(&setup, n, &profile(10 + n), &format!("AfkBot{}", 10 + n));

        let flow = flow_of(&relink(&setup, &owner, id).await);

        assert!(
            matches!(ended(&setup, flow).await, LinkFlowStatus::Linked { .. }),
            "{name}"
        );
        assert_eq!(
            setup.stored(id).await.unwrap().status(),
            AccountStatus::Active
        );
        let entries = setup.entries(&AuditAction::ACCOUNT_RELINK).await;
        let entry = entries.last().unwrap();
        assert_eq!(meta(entry, "previous_status"), Some(text(name)));
        assert_eq!(meta(entry, "previous_name"), None, "the name didn't change");
    }
}

#[sqlx::test(migrations = false)]
async fn another_profile_changes_nothing_and_is_recorded_without_a_uuid(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::ReauthRequired))
        .await;
    let before = setup.stored(id).await;
    script_start(&setup, "device-code-1");
    script_approval(&setup, 1, OTHER_PROFILE, "Somebody");

    let flow = flow_of(&relink(&setup, &alice, id).await);

    assert_eq!(
        ended(&setup, flow).await,
        LinkFlowStatus::Refused(RefusalReason::ProfileMismatch)
    );
    let read = setup.status(&alice, &flow.to_string()).await;
    assert_eq!(
        read.body,
        serde_json::json!({ "status": "refused", "reason": "profile_mismatch" })
    );
    assert_eq!(setup.stored(id).await, before);
    let entries = setup.entries(&AuditAction::ACCOUNT_RELINK).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.outcome, AuditOutcome::Failure);
    assert_eq!(meta(entry, "reason"), Some(text("profile_mismatch")));
    assert_eq!(meta(entry, "flow"), Some(text(&flow.to_string())));
    assert_eq!(entry.metadata.entries().len(), 2);
}

#[sqlx::test(migrations = false)]
async fn a_pending_or_offline_account_is_409_before_microsoft_is_asked(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Admin).await;
    let pending = setup
        .add_account(alice.id, 2, Some(AccountStatus::Pending))
        .await;
    let offline = setup.add_account(alice.id, 3, None).await;

    for id in [pending, offline] {
        let seen = relink(&setup, &alice, id).await;

        assert_eq!(seen.status, 409, "{}", seen.text);
        assert_eq!(seen.code(), "conflict");
    }
    assert_eq!(setup.fake.calls().len(), 0);
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        0
    );
}

#[sqlx::test(migrations = false)]
async fn relinking_needs_manage_and_an_unseen_account_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let control = setup.person(2, Role::Member).await;
    let stranger = setup.person(3, Role::Member).await;
    let id = setup
        .add_account(alice.id, 4, Some(AccountStatus::Active))
        .await;
    setup
        .add_grant(id, control.id, GrantLevel::Control, alice.id)
        .await;
    let unknown = AccountId::new_v7(at(T0), [99; 10]).unwrap();

    let by_control = relink(&setup, &control, id).await;
    let by_stranger = relink(&setup, &stranger, id).await;
    let of_unknown = relink(&setup, &alice, unknown).await;

    assert_eq!(by_control.status, 403, "{}", by_control.text);
    assert_eq!(by_control.code(), "forbidden");
    assert_eq!(by_stranger.status, 404, "{}", by_stranger.text);
    assert_eq!(of_unknown.status, 404, "{}", of_unknown.text);
    assert_eq!(
        by_stranger.body_without_request_id(),
        of_unknown.body_without_request_id()
    );
    assert_eq!(setup.fake.calls().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_relink_shares_the_users_one_flow_and_daily_cap_and_has_no_quota(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let limits = RateLimitConfig {
        link_flows: LinkFlowLimits {
            total: NonZeroU32::new(3).unwrap(),
            per_user_per_day: NonZeroU32::new(2).unwrap(),
        },
        ..RateLimitConfig::default()
    };
    let quota = AccountsConfig {
        max_per_member: NonZeroU32::MIN,
        allow_offline: false,
    };
    let setup = HttpSetup::with(options, limits, quota).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup
        .add_account(alice.id, 2, Some(AccountStatus::Active))
        .await;
    // A new link is over the quota; linking the account again isn't.
    let over = setup.start(&alice).await;
    assert_eq!(over.status, 409, "{}", over.text);
    drop(setup.pace.hold_next());
    drop(setup.pace.hold_next());
    script_start(&setup, "device-code-1");
    script_start(&setup, "device-code-2");

    let first = flow_of(&relink(&setup, &alice, id).await);
    let second = flow_of(&relink(&setup, &alice, id).await);
    let third = relink(&setup, &alice, id).await;

    assert!(
        setup.link_flows.status(first).is_none(),
        "the second replaced it"
    );
    assert!(matches!(
        setup.link_flows.status(second).unwrap().status,
        LinkFlowStatus::Waiting { .. }
    ));
    assert_eq!(
        third.status, 429,
        "the daily cap counts relinks: {}",
        third.text
    );
    assert_eq!(
        setup.entries(&AuditAction::ACCOUNT_START_LINK).await.len(),
        2
    );
}

#[sqlx::test(migrations = false)]
async fn a_grant_revoked_or_an_account_deleted_during_the_flow_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let manager = setup.person(2, Role::Member).await;
    let shared = setup
        .add_account(alice.id, 3, Some(AccountStatus::ReauthRequired))
        .await;
    let doomed = setup
        .add_account(alice.id, 4, Some(AccountStatus::ReauthRequired))
        .await;
    setup
        .add_grant(shared, manager.id, GrantLevel::Manage, alice.id)
        .await;
    let before = setup.stored(shared).await;

    for (by, id, n) in [(&manager, shared, 1_u8), (&alice, doomed, 2)] {
        let mut wait = setup.pace.hold_next();
        script_start(&setup, &format!("device-code-{n}"));
        script_approval(&setup, n, &profile(2 + n), &format!("AfkBot{}", 2 + n));
        let flow = flow_of(&relink(&setup, by, id).await);
        wait.entered().await;
        if id == shared {
            let mut tx = setup.db.write().await.unwrap();
            tx.grants().delete(shared, manager.id).await.unwrap();
            tx.commit(crate::support::service::setup_entry())
                .await
                .unwrap();
        } else {
            let mut tx = setup.db.write().await.unwrap();
            tx.accounts().delete(doomed, at(T0)).await.unwrap();
            tx.commit(crate::support::service::setup_entry())
                .await
                .unwrap();
        }
        wait.release();

        assert_eq!(ended(&setup, flow).await, LinkFlowStatus::Failed, "{n}");
    }
    assert_eq!(setup.stored(shared).await, before);
    assert_eq!(setup.stored(doomed).await, None);
    assert_eq!(setup.entries(&AuditAction::ACCOUNT_RELINK).await.len(), 0);
    assert!(
        setup
            .fake
            .calls()
            .iter()
            .any(|call| matches!(call, Call::MinecraftSession { .. }))
    );
}

#[sqlx::test(migrations = false)]
async fn a_relinked_account_answers_as_active_in_the_list(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::ReauthRequired))
        .await;
    script_start(&setup, "device-code-1");
    script_approval(&setup, 1, &profile(3), "AfkBot3");
    let flow = flow_of(&relink(&setup, &alice, id).await);
    ended(&setup, flow).await;

    let read = setup
        .api(Method::GET, &format!("/accounts/{id}"), &alice, None)
        .await;

    assert_eq!(read.body["status"], "active");
    assert!(read.body.get("reason").is_none());
}

#[sqlx::test(migrations = false)]
async fn a_relink_keeps_the_accounts_bot(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup
        .add_account(alice.id, 3, Some(AccountStatus::Active))
        .await;
    let bot = NewBot {
        id: BotId::new_v7(at(T0), [3; 10]).unwrap(),
        account: id,
        mode: ModePreset::Afk.id(),
    };
    let mut tx = setup.db.write().await.unwrap();
    tx.bots().insert(&bot).await.unwrap();
    tx.commit(crate::support::service::setup_entry())
        .await
        .unwrap();
    script_start(&setup, "device-code-1");
    script_approval(&setup, 1, &profile(3), "Renamed_3");

    let flow = flow_of(&relink(&setup, &alice, id).await);
    let status = ended(&setup, flow).await;

    assert!(
        matches!(status, LinkFlowStatus::Linked { .. }),
        "{status:?}"
    );
    let kept = setup.db.bots().of_account(id).await.unwrap().unwrap();
    assert_eq!(kept.id, bot.id);
    let bots = sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM bots"#)
        .fetch_one(setup.db.read_pool())
        .await
        .unwrap();
    assert_eq!(bots, 1);
}
