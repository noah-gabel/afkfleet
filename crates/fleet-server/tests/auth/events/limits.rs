//! What a socket takes from its client, and how it keeps up (ADR-0020,
//! S35, U13, R19): a pong answers a ping, and a close; anything else closes
//! the socket with 1008; a client that doesn't answer the pings is closed
//! with 4000; a socket that fell behind the bus says `resync`, without a
//! count, at most once per gap.

use core::num::NonZeroU32;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_api_types::events::SocketClose;
use fleet_core::id::{AgentId, BotId};
use fleet_server::app::events::{LiveEvent, LiveEventKind};
use fleet_server::config::RateLimitConfig;
use fleet_server::http::ws::SocketTimings;
use fleet_testkit::log_capture;
use futures_util::SinkExt as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_util::sync::CancellationToken;

use super::super::support::{Options, Setup, setup, setup_with};
use super::{
    PATIENCE, Served, Socket, closed, commit_anything, next, next_event_after_pings, ready, said,
};

/// Short bounds: a ping every 100 ms, closed after 300 ms without a pong.
const SHORT: SocketTimings = SocketTimings {
    ping_interval: Duration::from_millis(100),
    idle_timeout: Duration::from_millis(300),
    send_timeout: Duration::from_secs(1),
    close_timeout: Duration::from_millis(200),
    resync_gap: Duration::from_millis(300),
};

async fn short(options: SqliteConnectOptions) -> Setup {
    setup_with(
        options,
        Options {
            sockets: SHORT,
            ..Options::default()
        },
    )
    .await
}

#[sqlx::test(migrations = false)]
async fn a_frame_the_server_doesnt_take_closes_the_socket_with_1008(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // Room for every case's socket, however fast each one ends.
    let setup = setup_with(
        options,
        Options {
            limits: RateLimitConfig {
                ws_connections_per_user: NonZeroU32::new(10).unwrap(),
                ..RateLimitConfig::default()
            },
            ..Options::default()
        },
    )
    .await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;

    let mut closes = Vec::new();
    for (case, frame) in [
        ("text", Message::Text("hello".into())),
        ("binary", Message::Binary(Bytes::from_static(b"\x00\x01"))),
        ("ping", Message::Ping(Bytes::new())),
        ("too large", Message::Text("x".repeat(126).into())),
        ("a pong nobody asked for", Message::Pong(Bytes::new())),
    ] {
        let mut socket = ready(&setup, &served, &alice.token).await;
        socket.send(frame).await.unwrap();
        closes.push((case, closed(&mut socket).await));
    }

    for (case, close) in closes {
        assert_eq!(close, said(SocketClose::RefusedFrame), "{case}");
    }
}

#[sqlx::test(migrations = false)]
async fn a_frame_too_large_is_refused_at_its_header_before_its_payload(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // The default bounds: without the frame limit, the server would wait
    // 45 s for the payload.
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    // A binary frame that claims 1000 bytes, and sends none.
    socket
        .get_mut()
        .write_all(&masked(0x82, 1000, &[]))
        .await
        .unwrap();

    assert_eq!(closed(&mut socket).await, said(SocketClose::RefusedFrame));
}

#[sqlx::test(migrations = false)]
async fn a_message_too_large_is_refused_before_its_last_fragment(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    // Two fragments of 100 bytes, each within the frame limit, neither the
    // last: without the message limit the server would wait for the rest.
    let mut fragments = masked(0x02, 100, &[0; 100]);
    fragments.extend(masked(0x00, 100, &[0; 100]));
    socket.get_mut().write_all(&fragments).await.unwrap();

    assert_eq!(closed(&mut socket).await, said(SocketClose::RefusedFrame));
}

/// A client frame: `first` (FIN and opcode), a header that claims `length`,
/// a mask, and `payload` masked with it, however long it really is.
fn masked(first: u8, length: u16, payload: &[u8]) -> Vec<u8> {
    const MASK: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
    let mut frame = vec![first];
    match u8::try_from(length) {
        Ok(short) if short <= 125 => frame.push(0x80 | short),
        _ => {
            frame.push(0x80 | 0x7E);
            frame.extend(length.to_be_bytes());
        }
    }
    frame.extend(MASK);
    frame.extend(
        payload
            .iter()
            .zip(MASK.iter().cycle())
            .map(|(byte, mask)| byte ^ mask),
    );
    frame
}

#[sqlx::test(migrations = false)]
async fn a_client_that_drops_its_connection_without_a_close_is_lost_not_refused(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = setup(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let socket = ready(&setup, &served, &alice.token).await;

    // A FIN with no close frame, as a client killed or a network gone
    // sends; what the server writes back is still read.
    let mut stream = socket.into_inner();
    stream.shutdown().await.unwrap();
    let mut back = Vec::new();
    timeout(PATIENCE, stream.read_to_end(&mut back))
        .await
        .expect("the server ended the connection in time")
        .unwrap();

    assert!(
        back.is_empty(),
        "the server wrote {} bytes back",
        back.len()
    );
    assert_eq!(
        log_capture::count("fleet_server::http::ws", "why=the connection was lost"),
        1
    );
    assert_eq!(
        log_capture::count("fleet_server::http::ws", "doesn't take"),
        0
    );
}

#[sqlx::test(migrations = false)]
async fn a_client_that_answers_the_pings_stays_open(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = short(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    // Reading answers each ping; five pings take 500 ms, past the 300 ms
    // a socket stays open without a pong.
    let mut pings = 0;
    while pings < 5 {
        match next(&mut socket).await {
            Some(Message::Ping(_)) => pings += 1,
            other => panic!("not a ping: {other:?}"),
        }
    }

    assert_eq!(setup.live_events.sockets().open(), 1);
}

#[sqlx::test(migrations = false)]
async fn a_client_that_never_answers_a_ping_is_closed_with_4000(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = short(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let socket = ready(&setup, &served, &alice.token).await;

    // Read the raw connection: tungstenite would answer every ping.
    let frames = raw_frames(socket).await;

    let pings = frames.iter().filter(|(opcode, _)| *opcode == 0x9).count();
    let close = frames.iter().find(|(opcode, _)| *opcode == 0x8).unwrap();
    assert!(pings >= 2, "{pings} pings came first");
    assert_eq!(close_code(&close.1), SocketClose::Idle.code());
}

#[sqlx::test(migrations = false)]
async fn under_a_stream_of_commits_a_silent_client_is_still_closed_with_4000(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = short(options).await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let socket = ready(&setup, &served, &alice.token).await;
    setup.hooked.queue_socket_checks_behind_writes();
    let done = CancellationToken::new();

    let committing = async {
        while !done.is_cancelled() {
            commit_anything(&setup).await;
        }
    };
    let watching = async {
        let frames = raw_frames(socket).await;
        done.cancel();
        frames
    };
    let ((), frames) = tokio::join!(committing, watching);

    let close = frames.iter().find(|(opcode, _)| *opcode == 0x8).unwrap();
    assert_eq!(close_code(&close.1), SocketClose::Idle.code());
}

#[sqlx::test(migrations = false)]
async fn a_lagging_socket_gets_one_resync_without_a_count_at_most_once_per_gap(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    // A gap of 1 s, so the check below has room for a slow test machine.
    let setup = setup_with(
        options,
        Options {
            sockets: SocketTimings {
                resync_gap: Duration::from_secs(1),
                ..SHORT
            },
            ..Options::default()
        },
    )
    .await;
    let alice = setup.alice_person().await;
    let served = Served::start(&setup).await;
    let mut socket = ready(&setup, &served, &alice.token).await;

    flood(&setup);
    let first = next_event_after_pings(&mut socket).await;
    let sent = Instant::now();
    flood(&setup);
    let second = next_event_after_pings(&mut socket).await;

    assert_eq!(first, json!({ "type": "resync" }));
    assert_eq!(second, json!({ "type": "resync" }));
    assert!(
        sent.elapsed() >= Duration::from_millis(750),
        "the second came one gap after the first"
    );
}

/// More events than the bus holds, published without a yield, so every
/// socket falls behind.
fn flood(setup: &Setup) {
    let at = DateTime::<Utc>::from_timestamp_millis(1_800_000_000_000).unwrap();
    let bot = BotId::new_v7(at, [7; 10]).unwrap();
    let agent = AgentId::new_v7(at, [8; 10]).unwrap();
    for _ in 0..1_100 {
        setup.events.publish(LiveEvent {
            bot,
            agent,
            generation: 1,
            at,
            kind: LiveEventKind::Died,
        });
    }
}

/// Every frame the server sends from now until it ends the connection, read
/// from the raw connection, so nothing answers a ping: each as its opcode
/// and payload.
async fn raw_frames(socket: Socket) -> Vec<(u8, Vec<u8>)> {
    let mut stream = socket.into_inner();
    let mut bytes = Vec::new();
    timeout(PATIENCE, stream.read_to_end(&mut bytes))
        .await
        .expect("the server ended the connection in time")
        .unwrap();
    let mut frames = Vec::new();
    let mut rest = bytes.as_slice();
    while let [first, second, tail @ ..] = rest {
        // The server's frames are unmasked; these are all short.
        let length = usize::from(second & 0x7F);
        assert!(length <= 125, "a frame this test doesn't expect");
        let (payload, after) = tail.split_at(length);
        frames.push((first & 0x0F, payload.to_vec()));
        rest = after;
    }
    frames
}

/// A close frame's code.
fn close_code(payload: &[u8]) -> u16 {
    u16::from_be_bytes([payload[0], payload[1]])
}
