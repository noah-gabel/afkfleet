//! `[rate_limits] connect_per_cert_per_min` (S30): spent in the gate per
//! presented certificate, before any lookup. Its production value, 10.

use core::time::Duration;

use fleet_server::grpc::control::{RATE_LIMITED, RETRY_AFTER};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::Code;

use super::support::{Settings, request_id, start};

#[sqlx::test(migrations = false)]
async fn the_eleventh_connect_in_a_minute_is_refused_before_any_lookup(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let port = start(options, Settings::default()).await;
    let stranger = port.stranger(1);
    let agent = port.enroll(2, "agent-2").await;
    for _ in 0..10 {
        let status = port.open(&stranger).await.unwrap_err();
        assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    }
    // Another certificate keeps its own budget.
    let (mut call, _) = port.welcomed(&agent).await;
    call.close();
    let _ = call.next().await;
    // Anything that reached the database would now fail another way.
    port.db.close().await;

    let status = port.open(&stranger).await.unwrap_err();

    assert_eq!(status.code(), Code::ResourceExhausted, "{status:?}");
    assert_eq!(status.message(), RATE_LIMITED);
    assert_eq!(
        status
            .metadata()
            .get(RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap(),
        "6"
    );
    let _ = request_id(status.metadata());
    port.clock.advance(Duration::from_secs(6));
    let again = port.open(&stranger).await.unwrap_err();
    assert_ne!(again.code(), Code::ResourceExhausted, "{again:?}");
    port.stop().await;
}
