//! The live events' tickets in memory (Plan.md P11.5, §7.4; ADR-0020, S29):
//! a ticket is taken once, by whoever presents it first, even when several
//! threads present it at once; it lives 30 s on the server's clock; a user
//! holds at most three; expired ones are dropped; and nothing shows a ticket
//! or its hash.
//!
//! The store is in memory, so time is a `ManualClock`, and nothing waits.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::{Arc, Barrier};

use chrono::{DateTime, Utc};
use fleet_core::id::{SessionId, UserId};
use fleet_core::system::RandomError;
use fleet_server::app::event_tickets::{
    EventTickets, TICKETS_PER_USER, TicketGrant, TicketRefusal,
};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::ports::tokens::{MintTokenError, TokenKind, Tokens as _};
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretString};

const T0: i64 = 1_800_000_000_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

struct Setup {
    tickets: EventTickets,
    clock: ManualClock,
    random: SeededRandom,
}

fn setup() -> Setup {
    let clock = ManualClock::new(at(T0));
    let random = SeededRandom::new(51);
    let tickets = EventTickets::new(
        Arc::new(clock.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(random.clone()))),
    );
    Setup {
        tickets,
        clock,
        random,
    }
}

fn user(n: u8) -> UserId {
    UserId::new_v7(at(T0), [n; 10]).unwrap()
}

fn session(n: u8) -> SessionId {
    SessionId::new_v7(at(T0), [n; 10]).unwrap()
}

fn text(ticket: &SecretString) -> &str {
    ticket.expose_secret()
}

#[test]
fn an_issued_ticket_is_taken_once_with_its_user_and_session() {
    let setup = setup();
    let issued = setup.tickets.issue(user(1), session(2)).unwrap();

    let first = setup.tickets.take(text(&issued.ticket));
    let second = setup.tickets.take(text(&issued.ticket));

    assert_eq!(
        first,
        Ok(TicketGrant {
            user: user(1),
            session: session(2),
        })
    );
    assert_eq!(second, Err(TicketRefusal::Unknown));
    assert_eq!(setup.tickets.held(), 0);
}

#[test]
fn an_issued_ticket_is_an_afk_wt_token_with_30_s_left() {
    let setup = setup();

    let issued = setup.tickets.issue(user(1), session(1)).unwrap();

    assert!(text(&issued.ticket).starts_with("afk_wt_"));
    assert_eq!(text(&issued.ticket).len(), 50);
    assert_eq!(issued.expires_in, 30);
    assert_eq!(setup.tickets.held(), 1);
}

#[test]
fn two_threads_presenting_one_ticket_never_both_pass() {
    const THREADS: usize = 8;
    for round in 0..50 {
        let setup = setup();
        let issued = setup.tickets.issue(user(1), session(1)).unwrap();
        let barrier = Barrier::new(THREADS);

        let passed = std::thread::scope(|scope| {
            let takers: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        setup.tickets.take(text(&issued.ticket)).is_ok()
                    })
                })
                .collect();
            takers
                .into_iter()
                .map(|taker| taker.join().unwrap())
                .filter(|passed| *passed)
                .count()
        });

        assert_eq!(passed, 1, "round {round}");
    }
}

#[test]
fn a_ticket_lives_30_s_on_the_servers_clock() {
    let setup = setup();
    let early = setup.tickets.issue(user(1), session(1)).unwrap();
    let late = setup.tickets.issue(user(1), session(1)).unwrap();

    setup.clock.advance(Duration::from_millis(29_999));
    let before_its_end = setup.tickets.take(text(&early.ticket));
    setup.clock.advance(Duration::from_millis(1));
    let at_its_end = setup.tickets.take(text(&late.ticket));

    assert!(before_its_end.is_ok());
    assert_eq!(at_its_end, Err(TicketRefusal::Expired));
}

#[test]
fn an_expired_ticket_is_burned_by_its_presentation() {
    let setup = setup();
    let issued = setup.tickets.issue(user(1), session(1)).unwrap();
    setup.clock.advance(Duration::from_secs(31));

    let first = setup.tickets.take(text(&issued.ticket));
    setup.clock.advance(Duration::from_secs(1));
    let second = setup.tickets.take(text(&issued.ticket));

    assert_eq!(first, Err(TicketRefusal::Expired));
    assert_eq!(second, Err(TicketRefusal::Unknown));
}

#[test]
fn another_kinds_token_or_anything_else_is_malformed_and_spends_nothing() {
    let setup = setup();
    let issued = setup.tickets.issue(user(1), session(1)).unwrap();
    let tokens = Sha256Tokens::new(Arc::new(SeededRandom::new(52)));
    let access = tokens.mint(TokenKind::Access).unwrap();
    let enrollment = tokens.mint(TokenKind::Enrollment).unwrap();
    let cut = &text(&issued.ticket)[..49];

    for presented in [
        text(&access.token),
        text(&enrollment.token),
        cut,
        "",
        "ticket",
        "afk_wt_",
    ] {
        assert_eq!(
            setup.tickets.take(presented),
            Err(TicketRefusal::Malformed),
            "{presented}"
        );
    }
    assert_eq!(setup.tickets.held(), 1);
}

#[test]
fn a_well_shaped_ticket_never_issued_is_unknown() {
    let setup = setup();
    setup.tickets.issue(user(1), session(1)).unwrap();
    let never = Sha256Tokens::new(Arc::new(SeededRandom::new(53)))
        .mint(TokenKind::EventTicket)
        .unwrap();

    assert_eq!(
        setup.tickets.take(text(&never.token)),
        Err(TicketRefusal::Unknown)
    );
    assert_eq!(setup.tickets.held(), 1);
}

#[test]
fn a_users_fourth_ticket_evicts_their_oldest_and_no_one_elses() {
    let setup = setup();
    let other = setup.tickets.issue(user(2), session(2)).unwrap();
    let mut mine = Vec::new();
    for _ in 0..=TICKETS_PER_USER {
        setup.clock.advance(Duration::from_millis(1));
        mine.push(setup.tickets.issue(user(1), session(1)).unwrap());
    }

    assert_eq!(setup.tickets.held(), TICKETS_PER_USER + 1);
    assert_eq!(
        setup.tickets.take(text(&mine[0].ticket)),
        Err(TicketRefusal::Unknown),
        "the oldest went"
    );
    for kept in &mine[1..] {
        assert!(setup.tickets.take(text(&kept.ticket)).is_ok());
    }
    assert!(setup.tickets.take(text(&other.ticket)).is_ok());
}

#[test]
fn expired_tickets_are_dropped_on_issue_take_and_purge() {
    let setup = setup();
    setup.tickets.issue(user(1), session(1)).unwrap();
    setup.tickets.issue(user(2), session(2)).unwrap();
    setup.clock.advance(Duration::from_secs(30));

    setup.tickets.issue(user(3), session(3)).unwrap();
    let after_issue = setup.tickets.held();
    setup.tickets.issue(user(4), session(4)).unwrap();
    setup.clock.advance(Duration::from_secs(30));
    let _ = setup
        .tickets
        .take("afk_wt_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq");
    let after_take = setup.tickets.held();
    setup.tickets.issue(user(5), session(5)).unwrap();
    setup.clock.advance(Duration::from_secs(30));
    let purged = setup.tickets.purge_expired();

    assert_eq!(after_issue, 1, "both 30-s-old tickets went at the issue");
    assert_eq!(after_take, 0, "both went at the take");
    assert_eq!(purged, 1);
    assert_eq!(setup.tickets.held(), 0);
}

#[test]
fn a_purge_keeps_the_live_tickets() {
    let setup = setup();
    setup.tickets.issue(user(1), session(1)).unwrap();
    setup.clock.advance(Duration::from_secs(29));

    assert_eq!(setup.tickets.purge_expired(), 0);
    assert_eq!(setup.tickets.held(), 1);
}

#[test]
fn debug_shows_counts_only() {
    let setup = setup();
    let issued = setup.tickets.issue(user(1), session(1)).unwrap();
    let secret_part = &text(&issued.ticket)["afk_wt_".len()..];

    let shown = format!("{:?} {issued:?}", setup.tickets);

    assert!(!shown.contains(secret_part), "{shown}");
    assert!(shown.contains("held: 1"), "{shown}");
}

#[test]
fn a_failing_random_source_issues_nothing() {
    let setup = setup();
    setup.random.fail_next(RandomError::Unavailable);

    let issued = setup.tickets.issue(user(1), session(1));

    assert!(matches!(issued, Err(MintTokenError::Random(_))));
    assert_eq!(setup.tickets.held(), 0);
}
