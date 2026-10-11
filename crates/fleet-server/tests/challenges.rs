//! The login's challenges in memory (Plan.md P7.6, P7.7, ADR-0016): an mfa
//! token lives 5 minutes and takes 3 wrong codes, a setup token lives 10
//! minutes, each user has a login slot and a replace slot that never
//! replace each other, a token is found only as its own kind, confirm takes
//! exactly the pending secret it checked, and expired ones are purged.
//!
//! The store is in memory, so time is a `ManualClock`, and nothing waits.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::authz::Role;
use fleet_core::id::{SessionId, UserId};
use fleet_server::app::challenges::{ChallengeUser, Challenges, SetupPurpose};
use fleet_server::infra::crypto::tokens::Sha256Tokens;
use fleet_server::ports::totp::TotpSecret;
use fleet_server::ports::users::PasswordHash;
use fleet_testkit::system::{ManualClock, SeededRandom};
use secrecy::{ExposeSecret as _, SecretBox, SecretString};

const T0: i64 = 1_800_000_000_000;

fn at(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap()
}

fn setup() -> (Challenges, ManualClock) {
    let clock = ManualClock::new(at(T0));
    let challenges = Challenges::new(
        Arc::new(clock.clone()),
        Arc::new(Sha256Tokens::new(Arc::new(SeededRandom::new(41)))),
    );
    (challenges, clock)
}

fn user(n: u8) -> ChallengeUser {
    ChallengeUser {
        id: UserId::new_v7(at(T0), [n; 10]).unwrap(),
        role: Role::Member,
        password_changed_at: at(T0 - 1_000),
    }
}

fn replace(n: u8) -> SetupPurpose {
    SetupPurpose::Replace {
        session_id: SessionId::new_v7(at(T0), [n; 10]).unwrap(),
    }
}

fn secret(byte: u8) -> TotpSecret {
    TotpSecret::new(SecretBox::new(Box::new([byte; 20])))
}

fn token(issued: &SecretString) -> &str {
    issued.expose_secret()
}

// --- Issuing -----------------------------------------------------------------------

#[test]
fn an_mfa_token_is_issued_for_five_minutes_and_found_as_an_mfa_token() {
    let (challenges, _) = setup();

    let issued = challenges.issue_mfa(user(1), None).unwrap();

    assert_eq!(issued.expires_in, 300);
    assert!(token(&issued.token).starts_with("afk_mt_"));
    assert_eq!(challenges.mfa(token(&issued.token)).unwrap().user, user(1));
    assert!(challenges.setup(token(&issued.token)).is_none());
}

#[test]
fn a_setup_token_is_issued_for_ten_minutes_and_found_as_a_setup_token() {
    let (challenges, _) = setup();

    let issued = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();

    assert_eq!(issued.expires_in, 600);
    assert!(token(&issued.token).starts_with("afk_st_"));
    let grant = challenges.setup(token(&issued.token)).unwrap();
    assert_eq!(grant.user, user(1));
    assert_eq!(grant.purpose, SetupPurpose::FirstEnrollment);
    assert!(challenges.mfa(token(&issued.token)).is_none());
}

#[test]
fn an_unknown_or_malformed_token_finds_nothing() {
    let (challenges, _) = setup();
    challenges.issue_mfa(user(1), None).unwrap();

    for presented in [
        "afk_mt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "afk_mt_short",
        "",
        "afk_at_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(challenges.mfa(presented).is_none(), "{presented}");
        assert!(challenges.setup(presented).is_none(), "{presented}");
    }
}

#[test]
fn a_new_password_step_replaces_the_users_login_challenge() {
    let (challenges, _) = setup();
    let first = challenges.issue_mfa(user(1), None).unwrap();

    let second = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();

    assert!(challenges.mfa(token(&first.token)).is_none());
    assert!(challenges.setup(token(&second.token)).is_some());
    assert_eq!(challenges.held(), 1);
}

#[test]
fn a_login_challenge_and_a_replace_token_never_replace_each_other() {
    let (challenges, _) = setup();
    let login = challenges.issue_mfa(user(1), None).unwrap();
    let replacing = challenges.issue_setup(user(1), replace(1)).unwrap();

    let login_again = challenges.issue_mfa(user(1), None).unwrap();

    assert!(challenges.mfa(token(&login.token)).is_none());
    assert!(challenges.mfa(token(&login_again.token)).is_some());
    assert_eq!(
        challenges.setup(token(&replacing.token)).unwrap().purpose,
        replace(1)
    );
    assert_eq!(challenges.held(), 2);
}

#[test]
fn a_new_replacement_replaces_the_previous_one() {
    let (challenges, _) = setup();
    let first = challenges.issue_setup(user(1), replace(1)).unwrap();

    let second = challenges.issue_setup(user(1), replace(2)).unwrap();

    assert!(challenges.setup(token(&first.token)).is_none());
    assert_eq!(
        challenges.setup(token(&second.token)).unwrap().purpose,
        replace(2)
    );
}

#[test]
fn other_users_challenges_are_their_own() {
    let (challenges, _) = setup();
    let alice = challenges.issue_mfa(user(1), None).unwrap();

    let bob = challenges.issue_mfa(user(2), None).unwrap();

    assert_eq!(challenges.mfa(token(&alice.token)).unwrap().user, user(1));
    assert_eq!(challenges.mfa(token(&bob.token)).unwrap().user, user(2));
}

// --- Lifetimes ---------------------------------------------------------------------

#[test]
fn an_mfa_token_is_over_after_five_minutes() {
    let (challenges, clock) = setup();
    let issued = challenges.issue_mfa(user(1), None).unwrap();

    clock.advance(Duration::from_millis(299_999));
    assert!(challenges.mfa(token(&issued.token)).is_some());
    clock.advance(Duration::from_millis(1));

    assert!(challenges.mfa(token(&issued.token)).is_none());
    assert_eq!(challenges.held(), 0, "found over, so dropped");
}

#[test]
fn a_setup_token_is_over_after_ten_minutes() {
    let (challenges, clock) = setup();
    let issued = challenges.issue_setup(user(1), replace(1)).unwrap();

    clock.advance(Duration::from_millis(599_999));
    assert!(challenges.setup(token(&issued.token)).is_some());
    clock.advance(Duration::from_millis(1));

    assert!(challenges.setup(token(&issued.token)).is_none());
}

#[test]
fn the_third_wrong_code_uses_an_mfa_token_up() {
    let (challenges, _) = setup();
    let issued = challenges.issue_mfa(user(1), None).unwrap();
    let grant = challenges.mfa(token(&issued.token)).unwrap();

    challenges.wrong_code(&grant);
    challenges.wrong_code(&grant);
    assert!(challenges.mfa(token(&issued.token)).is_some());
    challenges.wrong_code(&grant);

    assert!(challenges.mfa(token(&issued.token)).is_none());
    assert_eq!(challenges.held(), 0);
}

#[test]
fn purging_drops_exactly_the_challenges_that_are_over() {
    let (challenges, clock) = setup();
    challenges.issue_mfa(user(1), None).unwrap();
    challenges
        .issue_setup(user(2), SetupPurpose::FirstEnrollment)
        .unwrap();
    clock.advance(Duration::from_mins(5));

    let dropped = challenges.purge_expired();

    assert_eq!(dropped, 1);
    assert_eq!(challenges.held(), 1);
}

// --- Taking ------------------------------------------------------------------------

#[test]
fn an_mfa_token_is_taken_once_with_its_rehashed_password() {
    let (challenges, _) = setup();
    let rehash =
        PasswordHash::new(SecretString::from("$argon2id$v=19$m=19456,t=3,p=1$a$b")).unwrap();
    let issued = challenges.issue_mfa(user(1), Some(rehash)).unwrap();
    let grant = challenges.mfa(token(&issued.token)).unwrap();

    let taken = challenges.take_mfa(&grant);
    let again = challenges.take_mfa(&grant);

    assert_eq!(
        taken.unwrap().unwrap().expose_secret(),
        "$argon2id$v=19$m=19456,t=3,p=1$a$b"
    );
    assert!(again.is_none());
    assert!(challenges.mfa(token(&issued.token)).is_none());
}

#[test]
fn an_expired_mfa_token_cant_be_taken() {
    let (challenges, clock) = setup();
    let issued = challenges.issue_mfa(user(1), None).unwrap();
    let grant = challenges.mfa(token(&issued.token)).unwrap();
    clock.advance(Duration::from_mins(5));

    assert!(challenges.take_mfa(&grant).is_none());
}

#[test]
fn a_setup_token_has_no_pending_secret_until_enroll() {
    let (challenges, _) = setup();
    let issued = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();
    let grant = challenges.setup(token(&issued.token)).unwrap();

    assert!(matches!(challenges.pending(&grant), Some(None)));
}

#[test]
fn each_enroll_replaces_the_pending_secret_with_a_new_number() {
    let (challenges, _) = setup();
    let issued = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();
    let grant = challenges.setup(token(&issued.token)).unwrap();

    assert!(challenges.set_pending(&grant, secret(1)));
    let first = challenges.pending(&grant).unwrap().unwrap();
    assert!(challenges.set_pending(&grant, secret(2)));
    let second = challenges.pending(&grant).unwrap().unwrap();

    assert_eq!(first.secret.expose(), &[1; 20]);
    assert_eq!(second.secret.expose(), &[2; 20]);
    assert_ne!(first.generation, second.generation);
}

#[test]
fn confirm_takes_only_the_pending_secret_it_checked() {
    let (challenges, _) = setup();
    let issued = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();
    let grant = challenges.setup(token(&issued.token)).unwrap();
    assert!(challenges.set_pending(&grant, secret(1)));
    let checked = challenges.pending(&grant).unwrap().unwrap();
    assert!(challenges.set_pending(&grant, secret(2)));

    let stale = challenges.take_setup(&grant, checked.generation);
    let current = challenges.pending(&grant).unwrap().unwrap();
    let taken = challenges.take_setup(&grant, current.generation);

    assert!(stale.is_none(), "an enroll in between makes it stale");
    assert_eq!(taken.unwrap().secret.expose(), &[2; 20]);
    assert!(
        challenges.setup(token(&issued.token)).is_none(),
        "taken once"
    );
    assert!(challenges.take_setup(&grant, current.generation).is_none());
}

#[test]
fn a_gone_setup_token_takes_no_pending_secret() {
    let (challenges, clock) = setup();
    let issued = challenges
        .issue_setup(user(1), SetupPurpose::FirstEnrollment)
        .unwrap();
    let grant = challenges.setup(token(&issued.token)).unwrap();
    assert!(challenges.set_pending(&grant, secret(1)));
    let pending = challenges.pending(&grant).unwrap().unwrap();
    clock.advance(Duration::from_mins(10));

    assert!(challenges.pending(&grant).is_none());
    assert!(!challenges.set_pending(&grant, secret(2)));
    assert!(challenges.take_setup(&grant, pending.generation).is_none());
}

#[test]
fn debug_shows_counts_only() {
    let (challenges, _) = setup();
    let issued = challenges.issue_mfa(user(1), None).unwrap();

    let debug = format!("{challenges:?}");

    assert!(debug.contains("login: 1"), "{debug}");
    assert!(!debug.contains(token(&issued.token)), "{debug}");
}
