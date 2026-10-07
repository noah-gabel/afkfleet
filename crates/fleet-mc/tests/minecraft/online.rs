//! Sessions against an online-mode server with secure chat enforced
//! (Plan.md P3.7; ADR-0008 §9, ADR-0011). Neither uses real credentials: a
//! garbage token, then an offline account. azalea's whole login path runs
//! under the log capture, which must never see the token.

use core::time::Duration;
use std::time::Instant;

use fleet_core::disconnect::{DisconnectClass, DisconnectReason, TranslationKey};
use fleet_core::mc::{SessionCredentials, SessionEvent, SessionHandle};
use fleet_mc::{AzaleaConnector, McConfig};
use fleet_testkit::log_capture;
use secrecy::SecretString;
use uuid::Uuid;

use crate::harness::{Mode, Server, bot, connect, offline, wait_for, within};

/// A garbage token that's easy to find in captured logs.
const MARKER: &str = "afkfleet-redaction-marker-online-5b81e2";
/// The Minecraft server gives up on a stalled login after 30 s; a refused
/// token must end the session well before that (ADR-0008 §9).
const WELL_BEFORE_THE_SERVER: Duration = Duration::from_secs(20);

/// Any expiry will do: the adapter never reads it. The test doesn't depend
/// on chrono, so the type is left to inference.
fn any_expiry<T: Default>() -> T {
    T::default()
}

fn garbage_token() -> SessionCredentials {
    SessionCredentials::Online {
        username: "AfkBot1".parse().unwrap(),
        uuid: Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef),
        access_token: SecretString::from(MARKER.to_owned()),
        expires_at: any_expiry(),
    }
}

#[tokio::test]
async fn slow_online_mode_scenario() {
    log_capture::install().unwrap();
    let server = Server::start(Mode::Online).await;
    let connector = AzaleaConnector::new(&McConfig::default());

    // 1. The session server refuses a garbage token at once.
    let started = Instant::now();
    let (session, mut events) = connect(&connector, &server, bot(1), garbage_token()).await;
    let ended = wait_for(&mut events, "the refused join", |event| {
        matches!(
            event,
            SessionEvent::Disconnected(_) | SessionEvent::ConnectionFailed(_)
        )
    })
    .await;
    assert_eq!(
        ended,
        SessionEvent::Disconnected(DisconnectReason::AuthRejected)
    );
    assert!(
        started.elapsed() < WELL_BEFORE_THE_SERVER,
        "the refused join took {:?}",
        started.elapsed()
    );
    within("the teardown", session.disconnect()).await;

    // 2. The server can't verify an offline account.
    let (session, mut events) = connect(&connector, &server, bot(2), offline("AfkBot2")).await;
    let ended = wait_for(&mut events, "the kick", |event| {
        matches!(
            event,
            SessionEvent::Disconnected(_) | SessionEvent::ConnectionFailed(_)
        )
    })
    .await;
    let SessionEvent::Disconnected(reason @ DisconnectReason::Kicked(kick)) = &ended else {
        panic!("expected a kick, got {ended:?}");
    };
    assert_eq!(
        kick.key().map(TranslationKey::as_str),
        Some("multiplayer.disconnect.unverified_username")
    );
    assert_eq!(reason.classify(), DisconnectClass::AuthInvalid);
    within("the teardown", session.disconnect()).await;

    // 3. azalea's login path never logged the token, at any level.
    log_capture::check_absent(MARKER).unwrap();
}
