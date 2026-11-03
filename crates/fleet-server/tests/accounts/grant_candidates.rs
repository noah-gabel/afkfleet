//! The suggestions behind "Share with" (Plan.md P11.11; ADR-0020, U110,
//! S127): `GET /api/v1/accounts/{id}/grant-candidates?q=`, through serve's own
//! app. Manage on the account asks, before any lookup; the answer is at most
//! 8 usernames starting with what's typed, in any case, and every one of them
//! can be given a grant: never the caller, the owner, a disabled user, a
//! grantee, or anyone with implicit Manage. It spends a limit of its own, per
//! user.

use core::num::NonZeroU32;

use axum::http::Method;
use fleet_core::account::AccountStatus;
use fleet_core::authz::{Actor, GrantLevel, Role, UserRef, implicit_manage};
use fleet_core::id::{AccountId, UserId};
use fleet_server::config::{AccountsConfig, Rate, RateLimitConfig};
use fleet_testkit::log_capture;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::support::http::{HttpSetup, Person, Seen};

async fn ask(setup: &HttpSetup, by: &Person, id: AccountId, q: &str) -> Seen {
    setup
        .api(
            Method::GET,
            &format!("/accounts/{id}/grant-candidates?q={q}"),
            by,
            None,
        )
        .await
}

/// The usernames an answer holds, after it said 200.
fn names(seen: &Seen) -> Vec<String> {
    assert_eq!(seen.status, 200, "{}", seen.text);
    seen.body["usernames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect()
}

/// `limits` with the suggestions' limit at `count` an hour.
fn limited(count: u32) -> RateLimitConfig {
    RateLimitConfig {
        grant_candidates: Rate::per_hour(NonZeroU32::new(count).unwrap()),
        ..RateLimitConfig::default()
    }
}

#[sqlx::test(migrations = false)]
async fn without_a_token_it_answers_401(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;

    let seen = setup
        .request(
            Method::GET,
            &format!("/api/v1/accounts/{id}/grant-candidates?q=u"),
            None,
            None,
        )
        .await;

    assert_eq!(seen.status, 401, "{}", seen.text);
}

#[sqlx::test(migrations = false)]
async fn an_account_the_caller_cant_see_is_404_like_one_that_doesnt_exist(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let mallory = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    let unknown = AccountId::new_v7(crate::support::service::at(0), [9; 10]).unwrap();

    let hidden = ask(&setup, &mallory, id, "u").await;
    let missing = ask(&setup, &mallory, unknown, "u").await;

    assert_eq!(hidden.status, 404, "{}", hidden.text);
    assert_eq!(missing.status, 404, "{}", missing.text);
    assert_eq!(
        hidden.body_without_request_id(),
        missing.body_without_request_id()
    );
}

#[sqlx::test(migrations = false)]
async fn a_view_or_control_grantee_gets_403(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let viewer = setup.person(2, Role::Member).await;
    let controller = setup.person(4, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    setup
        .add_grant(id, viewer.id, GrantLevel::View, alice.id)
        .await;
    setup
        .add_grant(id, controller.id, GrantLevel::Control, alice.id)
        .await;

    assert_eq!(ask(&setup, &viewer, id, "u").await.status, 403);
    assert_eq!(ask(&setup, &controller, id, "u").await.status, 403);
}

#[sqlx::test(migrations = false)]
async fn a_manage_grantee_the_owner_and_an_admin_over_a_member_may_ask(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let manager = setup.person(2, Role::Member).await;
    let admin = setup.person(4, Role::Admin).await;
    let owner = setup.person(5, Role::Owner).await;
    let id = setup.add_account(alice.id, 3, None).await;
    setup
        .add_grant(id, manager.id, GrantLevel::Manage, alice.id)
        .await;

    for by in [&alice, &manager, &admin, &owner] {
        assert_eq!(ask(&setup, by, id, "u").await.status, 200);
    }
}

#[sqlx::test(migrations = false)]
async fn a_pending_account_is_409_and_spends_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, &limited(1), AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    let pending = setup
        .add_account(alice.id, 3, Some(AccountStatus::Pending))
        .await;
    let active = setup.add_account(alice.id, 4, None).await;

    assert_eq!(ask(&setup, &alice, pending, "u").await.status, 409);
    assert_eq!(ask(&setup, &alice, pending, "u").await.status, 409);

    assert_eq!(ask(&setup, &alice, active, "u").await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn what_isnt_a_start_of_a_username_is_422_for_any_account(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    let too_long = "a".repeat(33);

    for q in [
        "",
        "_a",
        ".a",
        "-a",
        "a%20b",
        "a%25",
        "%C3%A4",
        too_long.as_str(),
    ] {
        let own = ask(&setup, &alice, id, q).await;
        let others = ask(&setup, &bob, id, q).await;

        assert_eq!(own.status, 422, "{q}: {}", own.text);
        assert_eq!(own.code(), "validation_failed", "{q}");
        assert_eq!(others.status, 422, "{q}: tells nothing about the account");
    }
}

#[sqlx::test(migrations = false)]
async fn a_query_of_the_wrong_shape_is_400(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;

    for query in ["", "?other=u", "?q=u&q=v", "?q=u&limit=100"] {
        let seen = setup
            .api(
                Method::GET,
                &format!("/accounts/{id}/grant-candidates{query}"),
                &alice,
                None,
            )
            .await;

        assert_eq!(seen.status, 400, "{query}: {}", seen.text);
    }
}

#[sqlx::test(migrations = false)]
async fn the_suggestions_spend_their_own_per_user_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let limits = RateLimitConfig {
        grants_by_name: Rate::per_hour(NonZeroU32::new(1).unwrap()),
        ..limited(2)
    };
    let setup = HttpSetup::with(options, &limits, AccountsConfig::default()).await;
    let alice = setup.stepped_up(1, Role::Member).await;
    let carol = setup.person(2, Role::Member).await;
    let first = setup.add_account(alice.id, 3, None).await;
    let second = setup.add_account(alice.id, 4, None).await;
    let carols = setup.add_account(carol.id, 5, None).await;

    assert_eq!(ask(&setup, &alice, first, "u").await.status, 200);
    assert_eq!(ask(&setup, &alice, second, "u").await.status, 200);
    let third = ask(&setup, &alice, first, "u").await;

    assert_eq!(third.status, 429, "one bucket across a user's accounts");
    assert_eq!(third.retry_after.as_deref(), Some("1800"));
    assert_eq!(
        ask(&setup, &carol, carols, "u").await.status,
        200,
        "another user's limit is their own"
    );
    let by_name = setup
        .api(
            Method::POST,
            &format!("/accounts/{first}/grants"),
            &alice,
            Some(json!({ "username": "nobody", "level": "view" })),
        )
        .await;
    assert_eq!(
        by_name.status, 422,
        "granting by name has a bucket of its own"
    );
    setup.clock.advance(core::time::Duration::from_mins(30));
    let later = setup.session_of(alice.id, Role::Member).await;
    assert_eq!(ask(&setup, &later, first, "u").await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn a_call_refused_before_the_spend_doesnt_spend(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::with(options, &limited(1), AccountsConfig::default()).await;
    let alice = setup.person(1, Role::Member).await;
    let viewer = setup.person(2, Role::Member).await;
    let id = setup.add_account(alice.id, 3, None).await;
    let others = setup.add_account(viewer.id, 4, None).await;
    setup
        .add_grant(others, alice.id, GrantLevel::View, viewer.id)
        .await;
    let hidden = setup.add_user(5, Role::Member).await;
    let hiddens = setup.add_account(hidden, 6, None).await;

    assert_eq!(ask(&setup, &alice, hiddens, "u").await.status, 404);
    assert_eq!(ask(&setup, &alice, others, "u").await.status, 403);
    assert_eq!(ask(&setup, &alice, id, "_").await.status, 422);

    assert_eq!(ask(&setup, &alice, id, "u").await.status, 200);
}

#[sqlx::test(migrations = false)]
async fn it_leaves_out_the_caller_the_owner_the_disabled_and_every_grantee(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let manager = setup.person(2, Role::Member).await;
    let viewer = setup.add_user(3, Role::Member).await;
    let gone = setup.add_user(4, Role::Member).await;
    let free = setup.add_user(5, Role::Member).await;
    let id = setup.add_account(owner.id, 6, None).await;
    setup
        .add_grant(id, manager.id, GrantLevel::Manage, owner.id)
        .await;
    setup
        .add_grant(id, viewer, GrantLevel::View, owner.id)
        .await;
    setup.set_disabled(gone, true).await;
    assert_ne!(free, gone);

    let by_owner = ask(&setup, &owner, id, "user").await;
    let by_manager = ask(&setup, &manager, id, "user").await;

    assert_eq!(names(&by_owner), ["user5"]);
    assert_eq!(names(&by_manager), ["user5"]);
}

#[sqlx::test(migrations = false)]
async fn a_grant_on_another_account_doesnt_leave_them_out(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let elsewhere = setup.add_user(2, Role::Member).await;
    let id = setup.add_account(owner.id, 3, None).await;
    let other = setup.add_account(owner.id, 4, None).await;
    setup
        .add_grant(other, elsewhere, GrantLevel::View, owner.id)
        .await;

    assert_eq!(names(&ask(&setup, &owner, id, "user").await), ["user2"]);
}

/// The roles come from the account's owner, never the caller's: a Member who
/// manages an Admin's account is offered another Admin, who has no implicit
/// Manage on it (PR #90's review).
#[sqlx::test(migrations = false)]
async fn a_member_managing_an_admins_account_is_offered_another_admin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let admin = setup.person(1, Role::Admin).await;
    let manager = setup.person(2, Role::Member).await;
    setup.add_user(3, Role::Admin).await;
    let id = setup.add_account(admin.id, 4, None).await;
    setup
        .add_grant(id, manager.id, GrantLevel::Manage, admin.id)
        .await;

    let seen = ask(&setup, &manager, id, "user").await;

    assert_eq!(names(&seen), ["user3"]);
}

/// Whether the SQL's leaving out agrees with fleet-core's `implicit_manage` for
/// a user of `candidate`'s role on an account `owner`'s role owns: an exhaustive
/// pin, since the query copies its rule.
async fn pin(options: SqliteConnectOptions, owner: Role, candidate: Role) {
    let setup = HttpSetup::new(options).await;
    let person = setup.person(1, owner).await;
    let other = setup.add_user(2, candidate).await;
    let id = setup.add_account(person.id, 3, None).await;
    let implicit = implicit_manage(
        &Actor {
            id: other,
            role: candidate,
        },
        UserRef {
            id: person.id,
            role: owner,
        },
    );

    let shown = names(&ask(&setup, &person, id, "user2").await);

    assert_eq!(
        shown.is_empty(),
        implicit,
        "a {candidate} on a {owner}'s account"
    );
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_member_over_member(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    pin(options, Role::Member, Role::Member).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_admin_over_member(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    pin(options, Role::Member, Role::Admin).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_owner_over_member(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    pin(options, Role::Member, Role::Owner).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_member_over_admin(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    pin(options, Role::Admin, Role::Member).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_admin_over_admin(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    pin(options, Role::Admin, Role::Admin).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_owner_over_admin(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    pin(options, Role::Admin, Role::Owner).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_member_over_owner(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    pin(options, Role::Owner, Role::Member).await;
}

#[sqlx::test(migrations = false)]
async fn implicit_manage_admin_over_owner(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    pin(options, Role::Owner, Role::Admin).await;
}

#[sqlx::test(migrations = false)]
async fn it_answers_at_most_eight_names_in_order(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let id = setup.add_account(owner.id, 2, None).await;
    for n in [29, 28, 27, 26, 25, 24, 23, 22, 21, 20] {
        setup.add_user(n, Role::Member).await;
    }

    let shown = names(&ask(&setup, &owner, id, "user2").await);

    assert_eq!(
        shown,
        [
            "user20", "user21", "user22", "user23", "user24", "user25", "user26", "user27"
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn it_matches_from_the_first_character_in_any_case(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let id = setup.add_account(owner.id, 2, None).await;
    setup.add_user(3, Role::Member).await;
    setup.add_user(30, Role::Member).await;
    setup.add_user(4, Role::Member).await;
    setup.add_user_named(5, "ser3x", Role::Member).await;

    assert_eq!(
        names(&ask(&setup, &owner, id, "u").await),
        ["user3", "user30", "user4"]
    );
    assert_eq!(
        names(&ask(&setup, &owner, id, "USER3").await),
        ["user3", "user30"]
    );
    assert_eq!(names(&ask(&setup, &owner, id, "user30").await), ["user30"]);
    assert_eq!(
        names(&ask(&setup, &owner, id, "user31").await),
        Vec::<String>::new()
    );
    assert_eq!(names(&ask(&setup, &owner, id, "s").await), ["ser3x"]);
}

#[sqlx::test(migrations = false)]
async fn a_character_a_pattern_would_treat_specially_counts_as_itself(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let id = setup.add_account(owner.id, 2, None).await;
    setup.add_user_named(3, "a_b", Role::Member).await;
    setup.add_user_named(4, "axb", Role::Member).await;
    setup.add_user_named(5, "a.b", Role::Member).await;

    assert_eq!(names(&ask(&setup, &owner, id, "a_").await), ["a_b"]);
    assert_eq!(names(&ask(&setup, &owner, id, "a.").await), ["a.b"]);
    assert_eq!(
        names(&ask(&setup, &owner, id, "a").await),
        ["a.b", "a_b", "axb"]
    );
}

#[sqlx::test(migrations = false)]
async fn a_name_that_sorts_last_in_its_prefix_is_found(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let id = setup.add_account(owner.id, 2, None).await;
    setup.add_user_named(3, "azz", Role::Member).await;
    setup.add_user_named(4, "b00", Role::Member).await;

    assert_eq!(names(&ask(&setup, &owner, id, "az").await), ["azz"]);
    assert_eq!(
        names(&ask(&setup, &owner, id, "z").await),
        Vec::<String>::new()
    );
}

#[sqlx::test(migrations = false)]
async fn every_name_it_suggests_can_be_given_a_grant(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = HttpSetup::new(options).await;
    let owner = setup.stepped_up(1, Role::Admin).await;
    setup.add_user(2, Role::Member).await;
    setup.add_user(3, Role::Admin).await;
    let id = setup.add_account(owner.id, 4, None).await;

    let shown = names(&ask(&setup, &owner, id, "user").await);
    assert_eq!(shown, ["user2", "user3"]);

    for name in shown {
        let created = setup
            .api(
                Method::POST,
                &format!("/accounts/{id}/grants"),
                &owner,
                Some(json!({ "username": name, "level": "view" })),
            )
            .await;
        assert_eq!(created.status, 201, "{name}: {}", created.text);
    }
    assert_eq!(
        names(&ask(&setup, &owner, id, "user").await),
        Vec::<String>::new()
    );
}

#[sqlx::test(migrations = false)]
async fn what_was_typed_reaches_no_log_and_only_usernames_are_answered(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Member).await;
    let id = setup.add_account(owner.id, 2, None).await;
    setup.add_user(3, Role::Member).await;
    let typed = "typed-by-mistake-here";

    let seen = ask(&setup, &owner, id, typed).await;
    let other: UserId = owner.id;
    let found = ask(&setup, &owner, id, "user").await;

    assert_eq!(seen.status, 200);
    log_capture::check_absent(typed).unwrap();
    assert_eq!(found.body, json!({ "usernames": ["user3"] }));
    assert!(!found.text.contains(&other.to_string()));
}
