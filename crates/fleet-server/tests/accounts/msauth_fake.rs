//! The Microsoft port's fake keeps its contract (ADR-0018): scripts answered
//! in order per method, a panic naming the method on an unscripted call, a
//! log of every call with its token, a hang that never answers, and a gate
//! that answers only once the test releases it.
//!
//! Every test runs on paused time and bounds each call, so a fake that
//! hangs where it should answer fails the test at once instead of blocking
//! it.

use core::future::Future;
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_server::ports::msauth::{
    Failure, MicrosoftAuthProvider as _, PollOutcome, RefreshError, SessionError,
};

use crate::support::msauth::{
    Call, FakeMicrosoftAuth, Script, access_token, device_code, device_flow, gated, refresh_token,
    session, tokens,
};

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

/// `call`'s answer; a call that hangs fails the test.
async fn answered<T>(call: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), call)
        .await
        .expect("the fake answered")
}

/// Whether `call` hangs: still unanswered after an hour of paused time.
async fn hangs<T>(call: impl Future<Output = T>) -> bool {
    tokio::time::timeout(Duration::from_secs(3_600), call)
        .await
        .is_err()
}

#[tokio::test(start_paused = true)]
async fn each_method_answers_its_scripts_in_order() {
    let fake = FakeMicrosoftAuth::new();
    fake.script_start(Script::Answer(Ok(device_flow("code-1", "ABCD1234", at()))));
    fake.script_start(Script::Answer(Err(Failure::RateLimited)));
    fake.script_poll(Script::Answer(Ok(PollOutcome::Pending)));
    fake.script_poll(Script::Answer(Ok(PollOutcome::Declined)));
    fake.script_refresh(Script::Answer(Ok(tokens("access-2", "refresh-2", at()))));
    fake.script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    fake.script_session(Script::Answer(Ok(session(
        "minecraft-1",
        "4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b",
        "AfkBot1",
        at(),
    ))));
    fake.script_session(Script::Answer(Err(SessionError::NoProfile)));
    let code = device_code("code-1");
    let refresh = refresh_token("refresh-1");
    let access = access_token("access-1");

    let flow = answered(fake.start_device_flow()).await.unwrap();
    let second_start = answered(fake.start_device_flow()).await.unwrap_err();
    let first_poll = answered(fake.poll(&code)).await.unwrap();
    let second_poll = answered(fake.poll(&code)).await.unwrap();
    let refreshed = answered(fake.refresh(&refresh)).await.unwrap();
    let second_refresh = answered(fake.refresh(&refresh)).await.unwrap_err();
    let first_session = answered(fake.minecraft_session(&access)).await.unwrap();
    let second_session = answered(fake.minecraft_session(&access)).await.unwrap_err();

    assert_eq!(flow.code.expose_secret(), "code-1");
    assert_eq!(flow.user_code.as_str(), "ABCD1234");
    assert_eq!(
        flow.verification_uri.as_str(),
        "https://www.microsoft.com/link"
    );
    assert_eq!(flow.expires_at, at());
    assert_eq!(second_start, Failure::RateLimited);
    assert!(matches!(first_poll, PollOutcome::Pending), "{first_poll:?}");
    assert!(
        matches!(second_poll, PollOutcome::Declined),
        "{second_poll:?}"
    );
    assert_eq!(refreshed.access.expose_secret(), "access-2");
    assert_eq!(refreshed.refresh.expose_secret(), "refresh-2");
    assert_eq!(second_refresh, RefreshError::InvalidGrant);
    assert_eq!(first_session.token.expose_secret(), "minecraft-1");
    assert_eq!(
        first_session.uuid.to_string(),
        "4f1c9a2e-7b3d-4e5f-8a6b-0c1d2e3f4a5b"
    );
    assert_eq!(first_session.name.as_str(), "AfkBot1");
    assert_eq!(second_session, SessionError::NoProfile);
}

#[tokio::test(start_paused = true)]
async fn the_log_records_each_call_with_its_token() {
    let fake = FakeMicrosoftAuth::new();
    fake.script_start(Script::Answer(Err(Failure::Unreachable)));
    fake.script_poll(Script::Answer(Ok(PollOutcome::Expired)));
    fake.script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    fake.script_session(Script::Answer(Err(SessionError::NotOwned)));

    let _ = answered(fake.start_device_flow()).await;
    let _ = answered(fake.poll(&device_code("code-1"))).await;
    let _ = answered(fake.refresh(&refresh_token("refresh-1"))).await;
    let _ = answered(fake.minecraft_session(&access_token("access-1"))).await;

    assert_eq!(
        fake.calls(),
        [
            Call::StartDeviceFlow,
            Call::Poll {
                device_code: "code-1".to_owned()
            },
            Call::Refresh {
                refresh_token: "refresh-1".to_owned()
            },
            Call::MinecraftSession {
                access_token: "access-1".to_owned()
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "FakeMicrosoftAuth::start_device_flow was called with nothing scripted")]
async fn an_unscripted_start_panics_naming_the_method() {
    let _ = answered(FakeMicrosoftAuth::new().start_device_flow()).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "FakeMicrosoftAuth::poll was called with nothing scripted")]
async fn an_unscripted_poll_panics_naming_the_method() {
    let _ = answered(FakeMicrosoftAuth::new().poll(&device_code("code-1"))).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "FakeMicrosoftAuth::refresh was called with nothing scripted")]
async fn an_unscripted_refresh_panics_naming_the_method() {
    let _ = answered(FakeMicrosoftAuth::new().refresh(&refresh_token("refresh-1"))).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "FakeMicrosoftAuth::minecraft_session was called with nothing scripted")]
async fn an_unscripted_session_panics_naming_the_method() {
    let _ = answered(FakeMicrosoftAuth::new().minecraft_session(&access_token("access-1"))).await;
}

#[tokio::test(start_paused = true)]
async fn a_hang_never_answers_but_is_logged() {
    let fake = FakeMicrosoftAuth::new();
    fake.script_poll(Script::Hang);
    fake.script_session(Script::Hang);
    let code = device_code("code-1");
    let access = access_token("access-1");

    let poll_hangs = hangs(fake.poll(&code)).await;
    let session_hangs = hangs(fake.minecraft_session(&access)).await;

    assert!(poll_hangs, "the poll answered");
    assert!(session_hangs, "the session answered");
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_hang_lets_the_next_script_answer_the_next_call() {
    let fake = FakeMicrosoftAuth::new();
    fake.script_start(Script::Hang);
    fake.script_start(Script::Answer(Ok(device_flow("code-2", "WXYZ9876", at()))));
    fake.script_refresh(Script::Hang);
    fake.script_refresh(Script::Answer(Err(RefreshError::InvalidGrant)));
    let refresh = refresh_token("refresh-1");

    let start_hangs = hangs(fake.start_device_flow()).await;
    let flow = answered(fake.start_device_flow()).await.unwrap();
    let refresh_hangs = hangs(fake.refresh(&refresh)).await;
    let refused = answered(fake.refresh(&refresh)).await.unwrap_err();

    assert!(start_hangs, "the first start answered");
    assert_eq!(flow.code.expose_secret(), "code-2");
    assert!(refresh_hangs, "the first refresh answered");
    assert_eq!(refused, RefreshError::InvalidGrant);
}

#[tokio::test(start_paused = true)]
async fn a_gated_call_tells_it_arrived_and_answers_only_once_released() {
    let fake = FakeMicrosoftAuth::new();
    let (script, mut gate) = gated();
    fake.script_refresh(script);
    let refresh = refresh_token("refresh-1");
    let mut call = Box::pin(fake.refresh(&refresh));

    let early = tokio::select! {
        biased;
        answer = &mut call => Some(answer),
        () = gate.entered() => None,
    };
    let still_waiting = hangs(&mut call).await;
    gate.release(Ok(tokens("access-2", "refresh-2", at())));
    let answer = answered(call).await.unwrap();

    assert!(early.is_none(), "{early:?}");
    assert!(still_waiting);
    assert_eq!(answer.refresh.expose_secret(), "refresh-2");
    assert_eq!(
        fake.calls(),
        [Call::Refresh {
            refresh_token: "refresh-1".to_owned()
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn a_gate_dropped_unreleased_never_answers() {
    let fake = FakeMicrosoftAuth::new();
    let (script, gate) = gated::<Result<_, SessionError>>();
    fake.script_session(script);
    drop(gate);

    assert!(hangs(fake.minecraft_session(&access_token("access-1"))).await);
}
