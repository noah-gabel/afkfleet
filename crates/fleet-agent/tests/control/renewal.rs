//! The certificate's renewal against the fake server (Plan.md P13.6;
//! ADR-0027), in real time: an agent past two thirds of its certificate's
//! lifetime asks at once, stores only an answer that passes enroll's check,
//! swaps its identity and connects again with it; one that isn't due asks
//! nothing; and a server that doesn't renew, or answers something else,
//! leaves the agent running on the certificate it has.

use core::time::Duration;
use std::fs;
use std::path::Path;

use chrono::Datelike as _;
use fleet_agent::managed::link::LinkTimings;
use fleet_agent::run::Exit;
use fleet_proto::control::AgentMessage;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use tonic::Status;

use crate::agent::{Agent, Options, agent};
use crate::pki::{ALWAYS, TestCa, Validity, agent_key, test_dir};
use crate::server::{Call, FakeServer};

/// What a renewal logs.
const RENEWED: &str = "the agent's certificate was renewed";
/// What a server that doesn't know the call makes the agent log.
const OLD_SERVER: &str = "the server doesn't renew certificates yet: update the server; the agent keeps its certificate and asks again";
/// What any other failed renewal logs.
const FAILED: &str =
    "the certificate's renewal failed; the agent keeps its certificate and tries again";
/// The daily reminder of a certificate in its last days.
const ENDS_SOON: &str = "this agent's certificate ends soon and isn't renewed yet: it renews once it's connected to a server that renews (0.1.2 or later); otherwise enroll this agent again before then";

/// 2020 to 2021: expired whenever these tests run.
const LONG_GONE: Validity = Validity {
    from: (2020, 1, 1),
    until: (2021, 1, 1),
};

const fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// From `before` days before today to `after` days after it, each at
/// midnight UTC.
fn around_today(before: u64, after: u64) -> Validity {
    let today = chrono::Utc::now().date_naive();
    let date = |day: chrono::NaiveDate| {
        (
            day.year(),
            u8::try_from(day.month()).unwrap(),
            u8::try_from(day.day()).unwrap(),
        )
    };
    Validity {
        from: date(today - chrono::Days::new(before)),
        until: date(today + chrono::Days::new(after)),
    }
}

/// 50 of 60 days gone: past the renewal's two thirds, 10 days from the end.
fn past_two_thirds() -> Validity {
    around_today(50, 10)
}

/// The protocol's timings, with a failed renewal tried again after a
/// second.
fn quick_retry() -> LinkTimings {
    LinkTimings {
        renew_retry: secs(1),
        ..LinkTimings::default()
    }
}

fn der(pem: &str) -> Vec<u8> {
    CertificateDer::from_pem_slice(pem.as_bytes())
        .unwrap()
        .to_vec()
}

fn certificate(dir: &Path) -> String {
    fs::read_to_string(dir.join("agent.crt")).unwrap()
}

/// Reads `call` until a heartbeat: the agent is alive and past whatever it
/// did after the welcome.
async fn a_heartbeat(call: &mut Call) {
    loop {
        match call.raw_next(secs(15)).await {
            Some(AgentMessage::Heartbeat) => return,
            Some(_) => {}
            None => panic!("the stream should stay open and beat"),
        }
    }
}

#[tokio::test]
async fn an_agent_past_two_thirds_renews_at_once_and_connects_with_the_new_certificate() {
    let dir = test_dir("control", "renew_at_once");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    let renewed = ca.agent_certificate(&agent_key(1), agent(), ALWAYS);
    server.renewals.answer(Ok(renewed.clone()));
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            ..Options::default()
        },
    );
    let first = certificate(&dir);
    let key = fs::read(dir.join("agent.key")).unwrap();

    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    server.renewals.called(1).await;
    // The agent ends the stream its old certificate opened…
    let ended = call.until_ended(secs(15)).await;
    // …and connects again, presenting the new one.
    let again = server.pending().await;
    let presented = again.certificates.clone();
    let second = again.accept();
    second.welcome(agent(), secs(10)).await;
    let outcome = running.stop().await;

    assert!(ended.is_some(), "the old stream should end");
    assert_eq!(server.renewals.presented(), [der(&first)]);
    assert_eq!(presented.first(), Some(&der(&renewed)));
    assert_eq!(certificate(&dir), renewed, "the file holds the new one");
    assert_eq!(fs::read(dir.join("agent.key")).unwrap(), key);
    let temps: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|kind| kind == "tmp"))
        .collect();
    assert_eq!(temps, Vec::<std::path::PathBuf>::new(), "no temp is left");
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(second);
}

#[tokio::test]
async fn a_renewal_is_logged_with_the_new_certificates_end_and_ends_the_stream_as_renewed() {
    let dir = test_dir("control", "renew_logged");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    server
        .renewals
        .answer(Ok(ca.agent_certificate(&agent_key(1), agent(), ALWAYS)));
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            ..Options::default()
        },
    );

    let call = server.pending().await.accept();
    call.welcome(agent(), secs(10)).await;
    let second = server.pending().await.accept();
    second.welcome(agent(), secs(10)).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    let renewed = logs.lines_with(RENEWED).unwrap();
    assert_eq!(renewed.len(), 1, "{}", logs.text());
    assert_eq!(renewed[0]["level"], "INFO");
    assert_eq!(renewed[0]["not_after"], "2090-01-01 00:00:00 UTC");
    let ended = logs
        .lines_with("the control stream ended; the agent reconnects")
        .unwrap();
    assert_eq!(ended.len(), 1, "{}", logs.text());
    assert_eq!(ended[0]["cause"], "renewed");
    assert_eq!(logs.lines_with(FAILED).unwrap().len(), 0);
    assert_eq!(
        logs.lines_with(ENDS_SOON).unwrap().len(),
        1,
        "before it renewed"
    );
    assert_eq!(outcome.exit, Exit::Stopped);
    drop((call, second));
}

#[tokio::test]
async fn a_certificate_before_its_renewal_point_asks_for_nothing() {
    let dir = test_dir("control", "renew_not_due");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    // 2020 to 2090: two thirds are decades away.
    let running = Agent::start(&dir, &server.url(), &ca);
    let before = certificate(&dir);

    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    a_heartbeat(&mut call).await;
    a_heartbeat(&mut call).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    assert_eq!(server.renewals.calls(), 0);
    assert_eq!(certificate(&dir), before);
    assert_eq!(logs.lines_with(ENDS_SOON).unwrap().len(), 0);
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn a_server_that_doesnt_renew_keeps_the_agent_running_and_is_asked_again() {
    let dir = test_dir("control", "renew_old_server");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    // No script: `UNIMPLEMENTED`, as a server older than the renewal.
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            timings: quick_retry(),
            ..Options::default()
        },
    );
    let before = certificate(&dir);

    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    server.renewals.called(3).await;
    // The stream lives on through every refusal.
    a_heartbeat(&mut call).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    assert_eq!(certificate(&dir), before, "nothing was stored");
    // The third call arrived, so two answers were read at least.
    let old = logs.lines_with(OLD_SERVER).unwrap();
    assert!(old.len() >= 2, "{}", logs.text());
    assert_eq!(old[0]["level"], "WARN", "the first says it aloud");
    assert!(
        old.iter().skip(1).all(|line| line["level"] == "DEBUG"),
        "repeats are quiet: {}",
        logs.text()
    );
    assert_eq!(old[0]["cause"], "unimplemented");
    assert_eq!(logs.lines_with(RENEWED).unwrap().len(), 0);
    // 10 days from its end: said once, however often the renewal fails.
    let soon = logs.lines_with(ENDS_SOON).unwrap();
    assert_eq!(soon.len(), 1, "{}", logs.text());
    assert_eq!(soon[0]["level"], "WARN");
    assert_eq!(
        soon[0]["days_left"],
        9,
        "midnight ten days on: {}",
        logs.text()
    );
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn an_answer_that_fails_enrolls_check_is_never_stored_and_a_good_one_after_it_is() {
    let dir = test_dir("control", "renew_bad_answers");
    let ca = TestCa::new(1);
    let other = TestCa::new(2);
    let mut server = FakeServer::start(&ca).await;
    let good = ca.agent_certificate(&agent_key(1), agent(), ALWAYS);
    for answer in [
        Ok(other.agent_certificate(&agent_key(1), agent(), ALWAYS)),
        Ok(ca.agent_certificate(&agent_key(2), agent(), ALWAYS)),
        Ok(format!("{good}{good}")),
        Ok(String::new()),
        Ok(ca.agent_certificate(&agent_key(1), agent(), LONG_GONE)),
        Err(Status::unavailable("the server's own words")),
        Ok(good.clone()),
    ] {
        server.renewals.answer(answer);
    }
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            timings: quick_retry(),
            ..Options::default()
        },
    );
    let before = certificate(&dir);

    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    // The sixth call arrived: five answers were refused, and the next
    // attempt is a second away.
    server.renewals.called(6).await;
    let untouched = certificate(&dir);
    a_heartbeat(&mut call).await;
    // The seventh answer is this agent's certificate.
    let again = server.pending().await;
    let presented = again.certificates.clone();
    let second = again.accept();
    second.welcome(agent(), secs(10)).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    assert_eq!(untouched, before, "six bad answers stored nothing");
    assert_eq!(certificate(&dir), good);
    assert_eq!(presented.first(), Some(&der(&good)));
    let causes: Vec<String> = logs
        .lines_with(FAILED)
        .unwrap()
        .iter()
        .map(|line| line["cause"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        causes,
        [
            "certificate_not_from_the_ca",
            "certificate_not_our_key",
            "certificate_not_one_pem",
            "answer",
            "certificate_expired",
            "status_14",
        ],
        "{}",
        logs.text()
    );
    assert!(
        !logs.text().contains("the server's own words"),
        "{}",
        logs.text()
    );
    assert_eq!(logs.lines_with(RENEWED).unwrap().len(), 1);
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(second);
}

#[tokio::test]
async fn a_certificate_file_that_cant_be_replaced_keeps_the_agent_on_the_one_it_has() {
    let dir = test_dir("control", "renew_store_fails");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    server
        .renewals
        .answer(Ok(ca.agent_certificate(&agent_key(1), agent(), ALWAYS)));
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            // The default retry is an hour: this test sees one attempt.
            ..Options::default()
        },
    );
    // The agent read its identity at its start. Now a folder with something
    // in it sits at the certificate's name: nothing can be renamed over it.
    let cert = dir.join("agent.crt");
    fs::remove_file(&cert).unwrap();
    fs::create_dir(&cert).unwrap();
    fs::write(cert.join("kept"), "kept").unwrap();

    let mut call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    server.renewals.called(1).await;
    // The stream lives on, on the certificate the agent started with.
    a_heartbeat(&mut call).await;
    a_heartbeat(&mut call).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    let failed = logs.lines_with(FAILED).unwrap();
    assert_eq!(failed.len(), 1, "{}", logs.text());
    assert_eq!(failed[0]["cause"], "store");
    assert_eq!(failed[0]["level"], "WARN");
    let stored = logs
        .lines_with("the renewed certificate couldn't be stored")
        .unwrap();
    assert_eq!(stored.len(), 1, "{}", logs.text());
    assert_eq!(stored[0]["level"], "ERROR");
    assert_eq!(logs.lines_with(RENEWED).unwrap().len(), 0);
    assert_eq!(fs::read_to_string(cert.join("kept")).unwrap(), "kept");
    assert!(
        !dir.join("agent.crt.renew.tmp").exists(),
        "the temp is removed"
    );
    assert_eq!(outcome.exit, Exit::Stopped);
}

#[tokio::test]
async fn a_renewed_certificate_that_is_already_due_isnt_renewed_again_at_once() {
    let dir = test_dir("control", "renew_due_again");
    let ca = TestCa::new(1);
    let mut server = FakeServer::start(&ca).await;
    // A server whose clock is far behind answers a certificate that's past
    // its own two thirds already; so is the one after it.
    for _ in 0..2 {
        server.renewals.answer(Ok(ca.agent_certificate(
            &agent_key(1),
            agent(),
            past_two_thirds(),
        )));
    }
    let running = Agent::launch(
        &dir,
        &server.url(),
        &ca,
        Options {
            validity: past_two_thirds(),
            ..Options::default()
        },
    );

    let call = server.pending().await.accept();
    call.welcome(agent(), secs(1)).await;
    server.renewals.called(1).await;
    let mut second = server.pending().await.accept();
    second.welcome(agent(), secs(1)).await;
    // The new stream lives on: no renewal ends it again.
    a_heartbeat(&mut second).await;
    a_heartbeat(&mut second).await;
    let logs = running.logs.clone();
    let outcome = running.stop().await;

    assert_eq!(
        server.renewals.calls(),
        1,
        "one renewal, then an hour's wait"
    );
    assert_eq!(logs.lines_with(RENEWED).unwrap().len(), 1);
    assert_eq!(outcome.exit, Exit::Stopped);
    drop(call);
}
