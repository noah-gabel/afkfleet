//! The grants review at a role change (Plan.md P11.9; ADR-0020, S50):
//! `GET /api/v1/users/{id}/grants` and `GET /api/v1/users/{id}/shared-accounts`,
//! the Owner's, through serve's own app. After a promotion the grants the
//! user holds on Members' accounts read dormant, after a demotion they
//! count again; the user's own shared accounts list beside them, their
//! grants on the existing route, which also revokes them. Everyone else is
//! refused as `PATCH /users/{id}` refuses them; the query's rules, the
//! limit, and nothing on the bus.

use core::num::NonZeroU32;

use axum::http::Method;
use fleet_core::authz::{GrantLevel, Role};
use fleet_core::id::{AccountId, UserId};
use fleet_server::config::{AccountsConfig, Rate, RateLimitConfig};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::{HttpSetup, Person, Seen};

async fn held(setup: &HttpSetup, by: &Person, user: UserId, query: &str) -> Seen {
    setup
        .api(
            Method::GET,
            &format!("/users/{user}/grants{query}"),
            by,
            None,
        )
        .await
}

async fn shared(setup: &HttpSetup, by: &Person, user: UserId) -> Seen {
    setup
        .api(
            Method::GET,
            &format!("/users/{user}/shared-accounts"),
            by,
            None,
        )
        .await
}

/// The accounts a page answered, in its order.
fn accounts(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            item["account"]["id"]
                .as_str()
                .or_else(|| item["id"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Alice's account shared with Bob at Control, and Bob's account shared
/// with Alice at View, both Members; the Owner reviews Bob.
struct Sharing {
    setup: HttpSetup,
    owner: Person,
    alice: Person,
    bob: Person,
    alices: AccountId,
    bobs: AccountId,
}

async fn sharing(options: SqliteConnectOptions) -> Sharing {
    let setup = HttpSetup::new(options).await;
    let owner = setup.stepped_up(1, Role::Owner).await;
    let alice = setup.person(2, Role::Member).await;
    let bob = setup.person(3, Role::Member).await;
    let alices = setup.add_account(alice.id, 4, None).await;
    let bobs = setup.add_account(bob.id, 5, None).await;
    setup
        .add_grant(alices, bob.id, GrantLevel::Control, alice.id)
        .await;
    setup
        .add_grant(bobs, alice.id, GrantLevel::View, bob.id)
        .await;
    Sharing {
        setup,
        owner,
        alice,
        bob,
        alices,
        bobs,
    }
}

#[sqlx::test(migrations = false)]
async fn after_a_promotion_the_grants_a_user_holds_read_dormant_and_after_a_demotion_they_count(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;

    s.setup.set_role(s.bob.id, Role::Admin).await;
    let promoted = held(&s.setup, &s.owner, s.bob.id, "").await;
    s.setup.set_role(s.bob.id, Role::Member).await;
    let demoted = held(&s.setup, &s.owner, s.bob.id, "").await;

    assert_eq!(promoted.status, 200, "{}", promoted.text);
    assert_eq!(
        promoted.body["items"][0],
        json!({
            "account": { "id": s.alices.to_string(), "name": "AfkBot4" },
            "owner": { "id": s.alice.id.to_string(), "username": "user2" },
            "level": "control",
            "granted_by": { "id": s.alice.id.to_string(), "username": "user2" },
            "granted_at": promoted.body["items"][0]["granted_at"],
            "dormant": true
        })
    );
    assert_eq!(promoted.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(demoted.status, 200, "{}", demoted.text);
    assert_eq!(demoted.body["items"][0]["dormant"], json!(false));
}

#[sqlx::test(migrations = false)]
async fn the_owner_sees_the_users_shared_accounts_and_a_revoke_on_the_grants_route_leaves_the_review(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;

    let accounts_shared = shared(&s.setup, &s.owner, s.bob.id).await;
    let revoked = s
        .setup
        .api(
            Method::DELETE,
            &format!("/accounts/{}/grants/{}", s.alices, s.bob.id),
            &s.owner,
            None,
        )
        .await;
    let after = held(&s.setup, &s.owner, s.bob.id, "").await;

    assert_eq!(accounts_shared.status, 200, "{}", accounts_shared.text);
    assert_eq!(accounts(&accounts_shared.body), [s.bobs.to_string()]);
    assert_eq!(revoked.status, 204, "{}", revoked.text);
    assert_eq!(after.status, 200, "{}", after.text);
    assert_eq!(after.body["items"], json!([]));
}

#[sqlx::test(migrations = false)]
async fn the_held_grants_page_newest_account_first(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;
    let another = s.setup.add_account(s.alice.id, 6, None).await;
    s.setup
        .add_grant(another, s.bob.id, GrantLevel::View, s.alice.id)
        .await;

    let first = held(&s.setup, &s.owner, s.bob.id, "?limit=1").await;
    let rest = held(
        &s.setup,
        &s.owner,
        s.bob.id,
        &format!("?before={another}&limit=1"),
    )
    .await;

    assert_eq!(accounts(&first.body), [another.to_string()]);
    assert_eq!(first.body["next"], json!(another.to_string()));
    assert_eq!(accounts(&rest.body), [s.alices.to_string()]);
    assert!(rest.body.get("next").is_none(), "{}", rest.text);
}

#[sqlx::test(migrations = false)]
async fn an_admin_is_refused_and_a_member_gets_404_for_another_and_403_for_themselves(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;
    let admin = s.setup.person(7, Role::Admin).await;

    let mut seen = Vec::new();
    for (by, user) in [(&admin, s.bob.id), (&s.alice, s.bob.id), (&s.bob, s.bob.id)] {
        seen.push(held(&s.setup, by, user, "").await);
        seen.push(shared(&s.setup, by, user).await);
    }

    let answers: Vec<(u16, &str)> = seen.iter().map(|seen| (seen.status, seen.code())).collect();
    assert_eq!(
        answers,
        [
            (403, "forbidden"),
            (403, "forbidden"),
            (404, "not_found"),
            (404, "not_found"),
            (403, "forbidden"),
            (403, "forbidden"),
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn an_unknown_or_malformed_user_is_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;
    let unknown = UserId::new_v7(chrono::DateTime::UNIX_EPOCH, [9; 10]).unwrap();

    let missing = held(&s.setup, &s.owner, unknown, "").await;
    let missing_shared = shared(&s.setup, &s.owner, unknown).await;
    let malformed = s
        .setup
        .api(Method::GET, "/users/not-an-id/grants", &s.owner, None)
        .await;

    for seen in [&missing, &missing_shared, &malformed] {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(seen.code(), "not_found");
    }
}

#[sqlx::test(migrations = false)]
async fn without_credentials_the_review_is_401(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;

    let held = s
        .setup
        .request(
            Method::GET,
            &format!("/api/v1/users/{}/grants", s.bob.id),
            None,
            None,
        )
        .await;
    let shared = s
        .setup
        .request(
            Method::GET,
            &format!("/api/v1/users/{}/shared-accounts", s.bob.id),
            None,
            None,
        )
        .await;

    assert_eq!(held.status, 401, "{}", held.text);
    assert_eq!(shared.status, 401, "{}", shared.text);
}

#[sqlx::test(migrations = false)]
async fn a_query_that_doesnt_parse_is_400_and_a_limit_out_of_range_422(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;

    let unparsed = held(&s.setup, &s.owner, s.bob.id, "?before=nonsense").await;
    let zero = held(&s.setup, &s.owner, s.bob.id, "?limit=0").await;
    let too_many = held(&s.setup, &s.owner, s.bob.id, "?limit=101").await;

    assert_eq!(unparsed.status, 400, "{}", unparsed.text);
    assert_eq!(zero.status, 422, "{}", zero.text);
    assert_eq!(too_many.status, 422, "{}", too_many.text);
}

#[sqlx::test(migrations = false)]
async fn the_review_spends_the_users_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(
        options,
        &RateLimitConfig {
            per_user: Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN),
            ..RateLimitConfig::default()
        },
        AccountsConfig::default(),
    )
    .await;
    let owner = setup.person(1, Role::Owner).await;
    let member = setup.person(2, Role::Member).await;
    assert_eq!(held(&setup, &owner, member.id, "").await.status, 200);

    let seen = shared(&setup, &owner, member.id).await;

    assert_eq!(seen.status, 429, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn the_review_tells_the_sockets_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let s = sharing(options).await;
    let mut bus = s.setup.app.as_ref().unwrap().events.subscribe();

    let seen = held(&s.setup, &s.owner, s.bob.id, "").await;
    let accounts = shared(&s.setup, &s.owner, s.bob.id).await;

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(accounts.status, 200, "{}", accounts.text);
    assert!(bus.try_recv().is_err());
}
