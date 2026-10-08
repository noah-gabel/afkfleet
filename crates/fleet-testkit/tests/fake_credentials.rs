//! Tests for `FakeCredentials`: scripted answers, requests that never get an
//! answer, and the request log (Plan.md P4.3).
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::time::Duration;

use fleet_core::bot::BotAccount;
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider, SessionCredentials, SessionRequest,
};
use fleet_testkit::mc::FakeCredentials;

fn offline_request(fresh: bool) -> SessionRequest {
    SessionRequest {
        bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
        account: BotAccount::Offline("AfkBot1".parse().unwrap()),
        fresh,
    }
}

fn online_request() -> SessionRequest {
    SessionRequest {
        bot_id: "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
        account: BotAccount::Online("018bcfe5-6800-7cdc-8dcd-cdcdcdcdcdcd".parse().unwrap()),
        fresh: false,
    }
}

/// Scripted credentials that the default would never give: another name.
fn scripted_credentials() -> SessionCredentials {
    SessionCredentials::Offline {
        username: "AfkBot2".parse().unwrap(),
    }
}

/// The login name of `credentials`, or the error.
fn username(
    result: Result<SessionCredentials, CredentialError>,
) -> Result<String, CredentialError> {
    result.map(|credentials| credentials.username().as_str().to_owned())
}

#[tokio::test(start_paused = true)]
async fn without_a_script_an_offline_account_gets_offline_credentials() {
    let fake = FakeCredentials::new();

    let result = fake.session(offline_request(false)).await;

    let Ok(SessionCredentials::Offline { username }) = result else {
        panic!("expected offline credentials, got {result:?}");
    };
    assert_eq!(username.as_str(), "AfkBot1");
}

#[tokio::test(start_paused = true)]
async fn without_a_script_an_online_account_is_refused_for_good() {
    let fake = FakeCredentials::new();

    let result = fake.session(online_request()).await;

    assert_eq!(result.err(), Some(CredentialError { retryable: false }));
}

#[tokio::test(start_paused = true)]
async fn scripted_answers_are_used_in_order_then_the_default_again() {
    let fake = FakeCredentials::new();
    fake.push_answer(Err(CredentialError { retryable: true }));
    fake.push_answer(Ok(scripted_credentials()));

    let first = fake.session(offline_request(false)).await;
    let second = fake.session(online_request()).await;
    let third = fake.session(offline_request(false)).await;

    assert_eq!(first.err(), Some(CredentialError { retryable: true }));
    assert_eq!(username(second).unwrap(), "AfkBot2");
    assert_eq!(username(third).unwrap(), "AfkBot1");
}

#[tokio::test(start_paused = true)]
async fn a_request_scripted_to_never_answer_stays_pending() {
    let fake = FakeCredentials::new();
    fake.push_never_answer();

    let waited = tokio::time::timeout(
        Duration::from_secs(3600),
        fake.session(offline_request(false)),
    )
    .await;
    let next = fake.session(offline_request(false)).await;

    assert!(waited.is_err(), "expected no answer, got {waited:?}");
    assert_eq!(username(next).unwrap(), "AfkBot1");
}

#[tokio::test(start_paused = true)]
async fn every_request_is_recorded_in_order() {
    let fake = FakeCredentials::new();
    fake.push_never_answer();

    let _ =
        tokio::time::timeout(Duration::from_secs(1), fake.session(offline_request(false))).await;
    let _ = fake.session(online_request()).await;
    let _ = fake.session(offline_request(true)).await;

    assert_eq!(
        fake.requests(),
        [
            offline_request(false),
            online_request(),
            offline_request(true)
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn clones_share_the_script_and_the_log() {
    let fake = FakeCredentials::new();
    let clone = fake.clone();
    clone.push_answer(Err(CredentialError { retryable: true }));

    let result = fake.session(offline_request(true)).await;

    assert_eq!(result.err(), Some(CredentialError { retryable: true }));
    assert_eq!(clone.requests(), [offline_request(true)]);
}
