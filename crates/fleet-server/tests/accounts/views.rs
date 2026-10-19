//! What a viewer sees of the accounts (Plan.md P9.6; ADR-0018, group E):
//! `GET /api/v1/accounts`, `GET /api/v1/accounts/awaiting-approval` and
//! `GET /api/v1/accounts/{id}`, through serve's own app and the real
//! router. Each viewer sees what `authorize()` lets them, with their own
//! level; who owns an account and since when shows only to Admins, the
//! Owner and Manage holders; an account the viewer may not see is the same
//! 404 as an unknown one.

use std::collections::BTreeMap;

use axum::http::Method;
use fleet_core::account::{AccountStatus, RefusalReason};
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::AccountId;
use fleet_server::config::{Rate, RateLimitConfig};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::{HttpSetup, Person, Seen};

/// The IDs a list answered, in its order.
fn ids(seen: &Seen) -> Vec<AccountId> {
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

/// `body` with every ID replaced, for a snapshot.
fn redacted(mut body: Value) -> Value {
    if body.get("id").is_some() {
        body["id"] = json!("[account]");
    }
    if let Some(owner) = body.get_mut("owner") {
        owner["id"] = json!("[owner]");
    }
    body
}

/// The Owner, two Admins and two Members, each with a live session.
struct Cast {
    owner: Person,
    admin: Person,
    other_admin: Person,
    member: Person,
    other_member: Person,
}

async fn cast(setup: &HttpSetup) -> Cast {
    Cast {
        owner: setup.person(1, Role::Owner).await,
        admin: setup.person(2, Role::Admin).await,
        other_admin: setup.person(3, Role::Admin).await,
        member: setup.person(4, Role::Member).await,
        other_member: setup.person(5, Role::Member).await,
    }
}

async fn get(setup: &HttpSetup, by: &Person, id: AccountId) -> Seen {
    setup
        .api(Method::GET, &format!("/accounts/{id}"), by, None)
        .await
}

async fn list(setup: &HttpSetup, by: &Person, query: &str) -> Seen {
    setup
        .api(Method::GET, &format!("/accounts{query}"), by, None)
        .await
}

async fn awaiting(setup: &HttpSetup, by: &Person, query: &str) -> Seen {
    setup
        .api(
            Method::GET,
            &format!("/accounts/awaiting-approval{query}"),
            by,
            None,
        )
        .await
}

#[sqlx::test(migrations = false)]
async fn each_viewer_sees_an_account_with_their_level_and_the_owner_only_from_manage_up(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let c = cast(&setup).await;
    let control = setup.person(6, Role::Member).await;
    let manage = setup.person(7, Role::Member).await;
    let account = setup
        .add_account(
            c.member.id,
            1,
            Some(AccountStatus::Refused(RefusalReason::NotOwned)),
        )
        .await;
    setup
        .add_grant(account, c.other_member.id, GrantLevel::View, c.member.id)
        .await;
    setup
        .add_grant(account, control.id, GrantLevel::Control, c.member.id)
        .await;
    setup
        .add_grant(account, manage.id, GrantLevel::Manage, c.member.id)
        .await;
    // An Admin who sees another Admin's account through a View grant.
    let admins = setup.add_account(c.other_admin.id, 2, None).await;
    setup
        .add_grant(admins, c.admin.id, GrantLevel::View, c.other_admin.id)
        .await;

    let mut seen = BTreeMap::new();
    for (viewer, person) in [
        ("1 the owner", &c.member),
        ("2 a grantee with view", &c.other_member),
        ("3 a grantee with control", &control),
        ("4 a grantee with manage", &manage),
        ("5 an admin", &c.admin),
        ("6 the Owner", &c.owner),
    ] {
        let answer = get(&setup, person, account).await;
        assert_eq!(answer.status, 200, "{viewer}: {}", answer.text);
        seen.insert(viewer, redacted(answer.body));
    }
    let admin_with_view = get(&setup, &c.admin, admins).await;
    seen.insert(
        "7 an admin with view on another admin's",
        redacted(admin_with_view.body),
    );

    insta::assert_json_snapshot!("account_as_each_viewer_sees_it", seen);
}

#[sqlx::test(migrations = false)]
async fn each_viewer_lists_what_they_may_see_newest_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let c = cast(&setup).await;
    let owners = setup
        .add_account(c.owner.id, 1, Some(AccountStatus::Active))
        .await;
    let admins = setup
        .add_account(c.admin.id, 2, Some(AccountStatus::Pending))
        .await;
    let other_admins = setup.add_account(c.other_admin.id, 3, None).await;
    let members = setup
        .add_account(c.member.id, 4, Some(AccountStatus::ReauthRequired))
        .await;
    let other_members = setup
        .add_account(c.other_member.id, 5, Some(AccountStatus::Active))
        .await;
    setup
        .add_grant(owners, c.member.id, GrantLevel::View, c.owner.id)
        .await;
    setup
        .add_grant(
            other_admins,
            c.admin.id,
            GrantLevel::Control,
            c.other_admin.id,
        )
        .await;

    let expected = [
        (
            &c.owner,
            vec![other_members, members, other_admins, admins, owners],
        ),
        (&c.admin, vec![other_members, members, other_admins, admins]),
        (&c.other_admin, vec![other_members, members, other_admins]),
        (&c.member, vec![members, owners]),
        (&c.other_member, vec![other_members]),
    ];
    for (viewer, accounts) in expected {
        let seen = list(&setup, viewer, "").await;

        assert_eq!(ids(&seen), accounts, "{:?}", viewer.role);
        assert!(seen.body.get("next").is_none(), "{}", seen.text);
    }
}

#[sqlx::test(migrations = false)]
async fn the_list_pages_with_before_and_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mut accounts = Vec::new();
    for n in 1..=3 {
        accounts.push(setup.add_account(alice.id, n, None).await);
    }

    let first = list(&setup, &alice, "?limit=2").await;
    let next = first.body["next"].as_str().unwrap().to_owned();
    let rest = list(&setup, &alice, &format!("?limit=2&before={next}")).await;

    assert_eq!(ids(&first), [accounts[2], accounts[1]]);
    assert_eq!(next, accounts[1].to_string());
    assert_eq!(ids(&rest), [accounts[0]]);
    assert!(rest.body.get("next").is_none());
}

#[sqlx::test(migrations = false)]
async fn the_awaiting_list_holds_exactly_the_links_the_caller_may_review(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let c = cast(&setup).await;
    let members = setup
        .add_account(c.member.id, 1, Some(AccountStatus::Pending))
        .await;
    let admins = setup
        .add_account(c.admin.id, 2, Some(AccountStatus::Pending))
        .await;
    setup
        .add_account(c.member.id, 3, Some(AccountStatus::Active))
        .await;
    // A grant never makes a reviewer.
    setup
        .add_grant(admins, c.other_admin.id, GrantLevel::Manage, c.admin.id)
        .await;
    setup
        .add_grant(members, c.other_member.id, GrantLevel::Manage, c.member.id)
        .await;

    let expected = [
        (&c.owner, vec![admins, members]),
        (&c.admin, vec![members]),
        (&c.other_admin, vec![members]),
        (&c.member, vec![]),
        (&c.other_member, vec![]),
    ];
    for (viewer, accounts) in expected {
        let seen = awaiting(&setup, viewer, "").await;

        assert_eq!(ids(&seen), accounts, "{:?}", viewer.role);
    }
    // Who linked it, and when, for the one who reviews it (ADR-0018).
    let seen = awaiting(&setup, &c.admin, "").await;
    assert_eq!(seen.body["items"][0]["owner"]["username"], "user4");
    assert_eq!(
        seen.body["items"][0]["linked_at"],
        "2027-01-15T08:00:00.000Z"
    );
}

#[sqlx::test(migrations = false)]
async fn an_account_the_caller_may_not_see_is_the_same_404_as_an_unknown_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let c = cast(&setup).await;
    let members = setup.add_account(c.member.id, 1, None).await;
    let owners = setup.add_account(c.owner.id, 2, None).await;
    let unknown = AccountId::new_v7(chrono::DateTime::UNIX_EPOCH, [9; 10]).unwrap();
    let reference = get(&setup, &c.other_member, unknown).await;

    let hidden = [
        get(&setup, &c.other_member, members).await,
        get(&setup, &c.admin, owners).await,
        setup
            .api(Method::GET, "/accounts/not-an-id", &c.member, None)
            .await,
    ];

    assert_eq!(reference.status, 404);
    for seen in hidden {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(
            seen.body_without_request_id(),
            reference.body_without_request_id()
        );
    }
}

#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_a_limit_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;

    for path in ["/accounts", "/accounts/awaiting-approval"] {
        let bad = setup
            .api(Method::GET, &format!("{path}?before=x"), &alice, None)
            .await;
        let zero = setup
            .api(Method::GET, &format!("{path}?limit=0"), &alice, None)
            .await;
        let too_many = setup
            .api(Method::GET, &format!("{path}?limit=101"), &alice, None)
            .await;

        assert_eq!(bad.status, 400, "{path}: {}", bad.text);
        assert_eq!(zero.status, 422, "{path}: {}", zero.text);
        assert_eq!(zero.body["error"]["fields"][0]["path"], "limit");
        assert_eq!(too_many.status, 422, "{path}: {}", too_many.text);
    }
}

#[sqlx::test(migrations = false)]
async fn the_reads_spend_the_users_request_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(core::num::NonZeroU32::MIN, core::num::NonZeroU32::MIN);
    let limits = RateLimitConfig {
        per_user: one,
        ..RateLimitConfig::default()
    };
    let setup = HttpSetup::with(
        options,
        &limits,
        fleet_server::config::AccountsConfig::default(),
    )
    .await;
    let alice = setup.person(1, Role::Member).await;
    let account = setup.add_account(alice.id, 1, None).await;

    for path in [
        "/accounts".to_owned(),
        "/accounts/awaiting-approval".to_owned(),
        format!("/accounts/{account}"),
    ] {
        let first = setup.api(Method::GET, &path, &alice, None).await;
        let second = setup.api(Method::GET, &path, &alice, None).await;

        assert_eq!(first.status, 200, "{path}: {}", first.text);
        assert_eq!(second.status, 429, "{path}: {}", second.text);
        assert!(second.retry_after.is_some(), "{path}");
        setup.clock.advance(core::time::Duration::from_secs(1));
    }
}
