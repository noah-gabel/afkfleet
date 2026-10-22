//! The tickets of the live events socket (ADR-0020, S28, S29, R10, R17):
//! every ticket that isn't a live one of the connecting session gets the
//! same 404, a request the upgrade or the query refuses leaves the ticket
//! unspent, and no log line ever holds a ticket.

use core::num::NonZeroU32;
use core::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request};
use fleet_core::authz::Role;
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::ports::tokens::{TokenKind, Tokens as _};
use fleet_testkit::log_capture;
use fleet_testkit::system::SeededRandom;
use secrecy::{ExposeSecret as _, SecretString};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::{Options, setup, setup_with};
use super::{Refused, Served, open, open_with, ticket};

/// A ticket of the right shape that was never issued.
fn never_issued() -> SecretString {
    fleet_server::infra::crypto::tokens::Sha256Tokens::new(std::sync::Arc::new(SeededRandom::new(
        99,
    )))
    .mint(TokenKind::EventTicket)
    .unwrap()
    .token
}

#[sqlx::test(migrations = false)]
async fn every_ticket_that_isnt_a_live_one_of_this_session_is_the_same_404(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let (_, second_session) = setup.session().await;
    let bob = setup.person(2, "bob", Role::Member).await;
    let served = Served::start(&setup).await;
    let expired = ticket(&setup, &alice.token).await;
    setup.clock.advance(Duration::from_secs(30));
    // Minted after the clock moved, so each refusal is its own case's.
    let used = ticket(&setup, &alice.token).await;
    let _socket = open(&served, &alice.token, &used).await.unwrap();
    let bobs = ticket(&setup, &bob.token).await;
    let other_sessions = ticket(&setup, &second_session).await;

    let mut refusals = Vec::new();
    for (case, presented) in [
        ("malformed", SecretString::from("nope")),
        ("too long", SecretString::from("x".repeat(65))),
        ("unknown", never_issued()),
        ("used", used),
        ("expired", expired),
        ("another user's", bobs),
        ("another session's", other_sessions),
    ] {
        let refused = open(&served, &alice.token, &presented).await.unwrap_err();
        refusals.push((case, refused));
    }

    let (_, first) = &refusals[0];
    assert_eq!(first.status, 404);
    assert_eq!(first.body["error"]["code"], "not_found");
    for (case, refused) in &refusals {
        assert_eq!(refused, first, "{case}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_request_that_isnt_an_upgrade_is_400_and_leaves_the_ticket_unspent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;
    let path = format!("/api/v1/events?ticket={}", ticket.expose_secret());

    let plain = setup.get(&path, Some(&alice.token)).await;
    let head = setup
        .send(
            Request::builder()
                .method(Method::HEAD)
                .uri(&path)
                .header(
                    "authorization",
                    format!("Bearer {}", alice.token.expose_secret()),
                )
                .header("connection", "upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(plain.status, 400, "{}", plain.text);
    assert_eq!(head.status, 405);
    assert!(open(&served, &alice.token, &ticket).await.is_ok());
}

#[sqlx::test(migrations = false)]
async fn a_query_that_isnt_one_ticket_is_400_and_spends_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;
    let text = ticket.expose_secret();

    let mut answers = Vec::new();
    for query in [
        String::new(),
        format!("ticket={text}&ticket={text}"),
        format!("ticket={text}&since=1"),
    ] {
        let refused: Refused = open_with(&served, Some(&alice.token), &query)
            .await
            .unwrap_err();
        answers.push((refused.status, refused.body["error"]["code"].clone()));
    }

    for answer in &answers {
        assert_eq!(answer, &(400, "bad_request".into()));
    }
    assert!(open(&served, &alice.token, &ticket).await.is_ok());
}

#[sqlx::test(migrations = false)]
async fn without_a_bearer_both_routes_are_401_and_the_ticket_stays_unspent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let ticket = ticket(&setup, &alice.token).await;

    let minted = setup.post("/api/v1/events/ticket", None, None).await;
    let connect = open_with(&served, None, &format!("ticket={}", ticket.expose_secret()))
        .await
        .unwrap_err();

    assert_eq!(minted.status, 401);
    assert_eq!(connect.status, 401);
    assert_eq!(connect.headers["www-authenticate"], "Bearer");
    assert!(open(&served, &alice.token, &ticket).await.is_ok());
}

#[sqlx::test(migrations = false)]
async fn minting_tickets_spends_the_per_user_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let one = Rate::per_second(NonZeroU32::MIN, NonZeroU32::MIN);
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                per_user: one,
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let alice = setup.alice_person().await;

    let first = setup
        .post("/api/v1/events/ticket", Some(&alice.token), None)
        .await;
    let second = setup
        .post("/api/v1/events/ticket", Some(&alice.token), None)
        .await;

    assert_eq!(first.status, 200);
    assert_eq!(second.status, 429);
    assert!(second.headers.contains_key("retry-after"));
}

#[sqlx::test(migrations = false)]
async fn no_log_line_ever_holds_a_ticket(_pool: SqlitePoolOptions, options: SqliteConnectOptions) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let opened = ticket(&setup, &alice.token).await;
    let plain = ticket(&setup, &alice.token).await;
    let expired = ticket(&setup, &alice.token).await;

    let _socket = open(&served, &alice.token, &opened).await.unwrap();
    let _ = open(&served, &alice.token, &opened).await;
    let _ = setup
        .get(
            &format!("/api/v1/events?ticket={}", plain.expose_secret()),
            Some(&alice.token),
        )
        .await;
    let doubled = format!(
        "ticket={}&ticket={}",
        plain.expose_secret(),
        plain.expose_secret()
    );
    let _ = open_with(&served, Some(&alice.token), &doubled).await;
    setup.clock.advance(Duration::from_secs(30));
    let _ = open(&served, &alice.token, &expired).await;

    assert!(
        log_capture::count(
            "fleet_server::http::handlers::events",
            "a live events ticket was refused"
        ) >= 2,
        "the refusals were logged"
    );
    for ticket in [&opened, &plain, &expired] {
        // The test's own client logs its request at `trace`; the server never.
        log_capture::check_absent_except(
            &ticket.expose_secret()["afk_wt_".len()..],
            "tungstenite::handshake::client",
        )
        .unwrap();
    }
}
