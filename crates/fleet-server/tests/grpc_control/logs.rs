//! What never reaches a log line (S24): the peer's address, the name a
//! hello gave and the certificate's bytes, of any target.

use core::fmt::Write as _;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::support::{Settings, hello, start};

#[sqlx::test(migrations = false)]
async fn no_log_line_holds_the_peer_a_hellos_name_or_a_certificate(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    fleet_testkit::log_capture::install().unwrap();
    let port = start(options, Settings::default()).await;
    let agent = port.enroll(1, "agent-1").await;
    let revoked = port.enroll(2, "agent-2").await;
    port.revoke(&revoked).await;
    let stranger = port.stranger(3);

    let (mut call, _) = port.welcomed(&agent).await;
    call.close();
    let _ = call.next().await;
    let mut named = port.open(&agent).await.unwrap();
    named.send(hello("intruder-name-7f3a")).await;
    let _ = named.rejected().await;
    let _ = port.open(&revoked).await.unwrap_err();
    let _ = port.open(&stranger).await.unwrap_err();
    let _ = port.disconnect(&agent).await;
    port.stop().await;

    fleet_testkit::log_capture::check_absent("intruder-name-7f3a").unwrap();
    for certificate in [&agent, &revoked, &stranger] {
        let hex = certificate.der[20..44]
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            });
        let bytes = format!("{:?}", &certificate.der[20..44]);
        let base64 = certificate.pem.lines().nth(2).unwrap();
        for form in [hex.as_str(), &bytes[1..bytes.len() - 1], base64] {
            fleet_testkit::log_capture::check_absent(form).unwrap();
        }
    }
    // The test's own client logs the address it dials; no target of the
    // server's side does.
    let peer = fleet_testkit::log_capture::check_absent("127.0.0.1")
        .err()
        .map(|leak| leak.hits.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        peer.iter()
            .all(|target| target == "hyper_util::client::legacy::connect::http"),
        "{peer:?}"
    );
    // The lines that are there name the agent.
    assert!(
        fleet_testkit::log_capture::count("fleet_server::grpc::control::session", "agent-1") >= 2,
        "the stream's lines name the agent"
    );
}
