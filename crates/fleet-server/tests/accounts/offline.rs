//! Offline accounts (Plan.md P9.7, ADR-0018): created through `POST
//! /api/v1/accounts/offline` while `[accounts] allow_offline` is on, a
//! Minecraft name unique in any case, active at once, counted toward the
//! Member quota, and audited; with the switch off, the route is a 404 like
//! an unknown one, and the accounts kept from before stay.

use core::num::NonZeroU32;

use axum::http::Method;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_core::bot::DesiredRunState;
use fleet_core::id::AccountId;
use fleet_core::mode::ModePreset;
use fleet_core::system::RandomError;
use fleet_server::app::accounts::{AccountService, AccountSettings, CreateOfflineError};
use fleet_server::config::{AccountsConfig, Rate, RateLimitConfig};
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::bots::Bot;
use fleet_server::ports::store::Store as _;
use fleet_testkit::system::{ManualClock, SeededRandom};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Arc;

use crate::support::http::{HttpSetup, Person, Seen};
use crate::support::service::{T0, at, bounded};

const OFFLINE: &str = "/api/v1/accounts/offline";

fn allowed(max: u32) -> AccountsConfig {
    AccountsConfig {
        max_per_member: NonZeroU32::new(max).unwrap(),
        allow_offline: true,
    }
}

async fn on(options: SqliteConnectOptions, max: u32) -> HttpSetup {
    HttpSetup::with(options, RateLimitConfig::default(), allowed(max)).await
}

async fn create(setup: &HttpSetup, by: &Person, body: Value) -> Seen {
    setup
        .request(Method::POST, OFFLINE, Some(&by.access), Some(body))
        .await
}

/// Every audit entry with `action`, oldest first.
async fn entries(setup: &HttpSetup, action: &AuditAction) -> Vec<AuditRecord> {
    let page = AuditPage {
        before: None,
        limit: AuditLimit::new(100).unwrap(),
    };
    let mut entries: Vec<AuditRecord> = setup
        .db
        .audit()
        .list(page)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| &entry.action == action)
        .collect();
    entries.reverse();
    entries
}

fn text(value: &str) -> RecordedValue {
    RecordedValue::Text(value.to_owned())
}

fn meta(entry: &AuditRecord, key: &str) -> Option<RecordedValue> {
    entry
        .metadata
        .entries()
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

#[sqlx::test(migrations = false)]
async fn a_member_creates_an_active_offline_account_audited_as_account_link(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options, 3).await;
    let alice = setup.person(1, Role::Member).await;

    let created = create(&setup, &alice, json!({ "name": "Afk_Bot1" })).await;

    assert_eq!(created.status, 201, "{}", created.text);
    let id: AccountId = created.body["id"].as_str().unwrap().parse().unwrap();
    let mut shown = created.body.clone();
    shown["id"] = json!("[account]");
    insta::assert_json_snapshot!("offline_created", shown);
    let stored = setup.db.accounts().find(id).await.unwrap().unwrap();
    assert_eq!(stored.owner, alice.id);
    assert_eq!(stored.name.as_str(), "Afk_Bot1");
    assert_eq!(stored.credential, AccountCredential::Offline);
    let links = entries(&setup, &AuditAction::ACCOUNT_LINK).await;
    assert_eq!(links.len(), 1);
    let entry = &links[0];
    assert_eq!(entry.outcome, AuditOutcome::Success);
    assert_eq!(entry.actor, Some(alice.id));
    assert_eq!(
        entry.target,
        Some(RecordedTarget::Known(AuditTarget::Account(id)))
    );
    assert_eq!(meta(entry, "kind"), Some(text("offline")));
    assert_eq!(meta(entry, "name"), Some(text("Afk_Bot1")));
    assert_eq!(meta(entry, "status"), Some(text("active")));
    let bot = setup.db.bots().of_account(id).await.unwrap().unwrap();
    assert_eq!(meta(entry, "bot"), Some(text(&bot.id.to_string())));
    assert_eq!(entry.metadata.entries().len(), 4);
}

#[sqlx::test(migrations = false)]
async fn every_role_may_create_one(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = on(options, 3).await;

    for (n, role) in [(1_u8, Role::Member), (2, Role::Admin), (3, Role::Owner)] {
        let user = setup.person(n, role).await;

        let created = create(&setup, &user, json!({ "name": format!("Bot{n}") })).await;

        assert_eq!(created.status, 201, "{role:?}: {}", created.text);
    }
}

#[sqlx::test(migrations = false)]
async fn a_name_taken_in_any_case_is_409_naming_nobody_with_a_failure_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options, 3).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    assert_eq!(
        create(&setup, &alice, json!({ "name": "AfkBot1" }))
            .await
            .status,
        201
    );

    let taken = create(&setup, &bob, json!({ "name": "afkbot1" })).await;

    assert_eq!(taken.status, 409, "{}", taken.text);
    assert_eq!(taken.code(), "conflict");
    assert!(!taken.text.contains("user1"), "{}", taken.text);
    assert_eq!(setup.db.accounts().count_owned_by(bob.id).await.unwrap(), 0);
    let links = entries(&setup, &AuditAction::ACCOUNT_LINK).await;
    let failure = &links[1];
    assert_eq!(failure.outcome, AuditOutcome::Failure);
    assert_eq!(failure.actor, Some(bob.id));
    assert_eq!(
        failure.target,
        Some(RecordedTarget::Known(AuditTarget::User(bob.id)))
    );
    assert_eq!(meta(failure, "reason"), Some(text("taken")));
    assert_eq!(meta(failure, "name"), Some(text("afkbot1")));
    assert_eq!(meta(failure, "kind"), Some(text("offline")));
}

#[sqlx::test(migrations = false)]
async fn a_member_at_the_quota_is_409_with_a_failure_entry_and_admins_have_none(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options, 1).await;
    let alice = setup.person(1, Role::Member).await;
    let admin = setup.person(2, Role::Admin).await;
    assert_eq!(
        create(&setup, &alice, json!({ "name": "AliceBot" }))
            .await
            .status,
        201
    );

    let over = create(&setup, &alice, json!({ "name": "AliceBot2" })).await;
    let admins = [
        create(&setup, &admin, json!({ "name": "AdminBot" })).await,
        create(&setup, &admin, json!({ "name": "AdminBot2" })).await,
    ];

    assert_eq!(over.status, 409, "{}", over.text);
    assert_eq!(over.code(), "conflict");
    let links = entries(&setup, &AuditAction::ACCOUNT_LINK).await;
    assert_eq!(links[1].outcome, AuditOutcome::Failure);
    assert_eq!(meta(&links[1], "reason"), Some(text("over_quota")));
    for seen in admins {
        assert_eq!(seen.status, 201, "{}", seen.text);
    }
}

#[sqlx::test(migrations = false)]
async fn an_offline_account_fills_the_quota_for_a_link_start(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options, 1).await;
    let alice = setup.person(1, Role::Member).await;
    assert_eq!(
        create(&setup, &alice, json!({ "name": "AliceBot" }))
            .await
            .status,
        201
    );

    let refused = setup.start(&alice).await;

    assert_eq!(refused.status, 409, "{}", refused.text);
    assert_eq!(setup.fake.calls().len(), 0);
}

#[sqlx::test(migrations = false)]
async fn a_name_that_isnt_a_minecraft_name_is_422_and_a_wrong_shape_400(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = on(options, 3).await;
    let alice = setup.person(1, Role::Member).await;

    for name in ["ab", "seventeen_chars_x", "Afk Bot", "Afk-Bot"] {
        let refused = create(&setup, &alice, json!({ "name": name })).await;

        assert_eq!(refused.status, 422, "{name}: {}", refused.text);
        assert_eq!(refused.body["error"]["fields"][0]["path"], "name");
        assert!(!refused.text.contains(name), "{}", refused.text);
    }
    let unknown = create(
        &setup,
        &alice,
        json!({ "name": "AfkBot1", "kind": "offline" }),
    )
    .await;
    let missing = create(&setup, &alice, json!({})).await;
    assert_eq!(unknown.status, 400, "{}", unknown.text);
    assert_eq!(missing.status, 400, "{}", missing.text);
    assert_eq!(
        setup.db.accounts().count_owned_by(alice.id).await.unwrap(),
        0
    );
}

#[sqlx::test(migrations = false)]
async fn when_off_it_is_404_like_an_unknown_route_whatever_the_body(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let unknown = setup
        .request(
            Method::POST,
            "/api/v1/nothing-here",
            Some(&alice.access),
            None,
        )
        .await;

    let answers = [
        create(&setup, &alice, json!({ "name": "AfkBot1" })).await,
        create(&setup, &alice, json!({ "name": "x" })).await,
        create(&setup, &alice, json!({ "unknown": true })).await,
        setup
            .request(Method::POST, OFFLINE, Some(&alice.access), None)
            .await,
    ];

    assert_eq!(unknown.status, 404);
    for seen in answers {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(
            seen.body_without_request_id(),
            unknown.body_without_request_id()
        );
    }
    assert_eq!(
        setup.db.accounts().count_owned_by(alice.id).await.unwrap(),
        0
    );
}

#[sqlx::test(migrations = false)]
async fn without_credentials_it_is_401_even_when_off(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;

    let refused = setup
        .request(
            Method::POST,
            OFFLINE,
            None,
            Some(json!({ "name": "AfkBot1" })),
        )
        .await;

    assert_eq!(refused.status, 401, "{}", refused.text);
}

#[sqlx::test(migrations = false)]
async fn it_spends_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let limits = RateLimitConfig {
        per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(options, limits, allowed(3)).await;
    let alice = setup.person(1, Role::Member).await;

    let first = create(&setup, &alice, json!({ "name": "AfkBot1" })).await;
    let second = create(&setup, &alice, json!({ "name": "AfkBot2" })).await;

    assert_eq!(first.status, 201, "{}", first.text);
    assert_eq!(second.status, 429, "{}", second.text);
}

/// The service, over `setup`'s database.
fn service(setup: &HttpSetup, allow_offline: bool) -> AccountService {
    service_with(setup, allow_offline, SeededRandom::new(5))
}

/// The service, over `setup`'s database, drawing from `random`.
fn service_with(setup: &HttpSetup, allow_offline: bool, random: SeededRandom) -> AccountService {
    AccountService::new(
        Arc::new(ManualClock::new(at(T0))),
        Arc::new(random),
        Arc::new(setup.db.clone()),
        AccountSettings {
            allow_offline,
            max_per_member: NonZeroU32::new(3).unwrap(),
        },
    )
}

#[sqlx::test(migrations = false)]
async fn a_session_gone_meanwhile_creates_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let logout = setup
        .request(
            Method::POST,
            "/api/v1/auth/logout",
            Some(&alice.access),
            None,
        )
        .await;
    assert_eq!(logout.status, 204);

    let refused =
        bounded(service(&setup, true).create_offline(&alice.acting(), "AfkBot1".parse().unwrap()))
            .await;

    assert!(
        matches!(refused, Err(CreateOfflineError::SessionGone)),
        "{refused:?}"
    );
    assert_eq!(
        setup.db.accounts().count_owned_by(alice.id).await.unwrap(),
        0
    );
}

#[sqlx::test(migrations = false)]
async fn the_service_refuses_while_off_and_kept_accounts_stay(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let kept =
        bounded(service(&setup, true).create_offline(&alice.acting(), "KeptBot".parse().unwrap()))
            .await
            .unwrap();

    let refused =
        bounded(service(&setup, false).create_offline(&alice.acting(), "NewBot".parse().unwrap()))
            .await;

    assert!(
        matches!(refused, Err(CreateOfflineError::Disabled)),
        "{refused:?}"
    );
    assert!(setup.db.accounts().find(kept.id).await.unwrap().is_some());
    assert_eq!(setup.db.accounts().count_offline().await.unwrap(), 1);
}

/// How many bots the database holds.
async fn bot_rows(setup: &HttpSetup) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!: i64" FROM bots"#)
        .fetch_one(setup.db.read_pool())
        .await
        .unwrap()
}

#[sqlx::test(migrations = false)]
async fn an_offline_account_gets_its_bot_in_its_transaction_minted_with_it(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;

    let created =
        bounded(service(&setup, true).create_offline(&alice.acting(), "AfkBot1".parse().unwrap()))
            .await
            .unwrap();

    let bot = setup
        .db
        .bots()
        .of_account(created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bot,
        Bot {
            id: bot.id,
            account: created.id,
            server: None,
            mode: ModePreset::Afk.id(),
            desired: DesiredRunState::Stopped,
            agent: None,
            last: None,
        }
    );
    assert_ne!(bot.id.to_string(), created.id.to_string());
    // Minted from the same time: the same 48-bit timestamp.
    assert_eq!(
        bot.id.to_string().get(..13),
        created.id.to_string().get(..13)
    );
    assert_eq!(bot_rows(&setup).await, 1);
}

#[sqlx::test(migrations = false)]
async fn an_offline_account_whose_bot_cant_be_minted_stores_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let random = SeededRandom::new(5);
    // The account's ID draws first, then its bot's.
    random.fail_after(1, RandomError::Unavailable);

    let refused = bounded(
        service_with(&setup, true, random)
            .create_offline(&alice.acting(), "AfkBot1".parse().unwrap()),
    )
    .await;

    assert!(
        matches!(refused, Err(CreateOfflineError::Mint(_))),
        "{refused:?}"
    );
    assert_eq!(
        setup.db.accounts().count_owned_by(alice.id).await.unwrap(),
        0
    );
    assert_eq!(bot_rows(&setup).await, 0);
    assert_eq!(
        entries(&setup, &AuditAction::ACCOUNT_LINK).await,
        Vec::new()
    );
}

#[sqlx::test(migrations = false)]
async fn a_refused_offline_account_leaves_no_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let service = service(&setup, true);
    bounded(service.create_offline(&alice.acting(), "AfkBot1".parse().unwrap()))
        .await
        .unwrap();

    let taken = bounded(service.create_offline(&alice.acting(), "afkbot1".parse().unwrap())).await;

    assert!(matches!(taken, Err(CreateOfflineError::Taken)), "{taken:?}");
    assert_eq!(bot_rows(&setup).await, 1);
}
