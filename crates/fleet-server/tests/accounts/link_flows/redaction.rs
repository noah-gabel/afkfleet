//! Nothing secret or personal reaches a log or `Debug` (security rule 4,
//! ADR-0018): never a device code, a user code, a token, a Minecraft name or
//! a profile's UUID, on any path the flows take.

use fleet_core::account::AccountStatus;
use fleet_core::authz::Role;
use fleet_core::id::AccountId;
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::ports::msauth::{Failure, PollOutcome, SessionError, XboxRefusal};
use fleet_testkit::log_capture;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::{approval, held_start};
use crate::support::link::{LinkSetup, USER_CODE};
use crate::support::msauth::{Script, session};
use crate::support::service::{DAY, OTHER_PROFILE, PROFILE, T0, at};

#[sqlx::test(migrations = false)]
async fn no_code_token_name_or_uuid_reaches_any_log_on_any_path(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    log_capture::install().unwrap();
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    let bob = setup.person(2, Role::Member).await;
    let carol = setup.person(3, Role::Owner).await;
    let fake = &setup.fake;
    // Alice: a passing poll failure, a slow down, a retried session, a busy
    // commit, then linked.
    fake.script_poll(Script::Answer(Err(Failure::Unreachable)));
    fake.script_poll(Script::Answer(Ok(PollOutcome::SlowDown)));
    fake.script_poll(Script::Answer(Ok(approval(1))));
    fake.script_session(Script::Answer(Err(SessionError::Failed(Failure::TimedOut))));
    fake.script_session(Script::Answer(Ok(session(
        "minecraft-1",
        PROFILE,
        "Secret_Name1",
        at(T0 + DAY),
    ))));
    let mut wait = setup.pace.hold_next();
    let alices = setup.started(&alice, "device-code-alice").await;
    wait.entered().await;
    setup.store.fail_next_writes(1);
    wait.release();
    assert!(matches!(
        setup.ended(alices).await,
        LinkFlowStatus::Linked { .. }
    ));
    // Bob: the same profile, taken.
    fake.script_poll(Script::Answer(Ok(approval(2))));
    fake.script_session(Script::Answer(Ok(session(
        "minecraft-2",
        PROFILE,
        "Secret_Name1",
        at(T0 + DAY),
    ))));
    let bobs = setup.started(&bob, "device-code-bob").await;
    assert_eq!(setup.ended(bobs).await, LinkFlowStatus::Taken);
    // Carol: refused by Xbox Live, then a failure that won't pass, then
    // another profile linked after a replaced flow.
    fake.script_poll(Script::Answer(Ok(approval(3))));
    fake.script_session(Script::Answer(Err(SessionError::Xbox(
        XboxRefusal::Other {
            xerr: 2_148_916_262,
        },
    ))));
    let refused = setup.started(&carol, "device-code-carol-1").await;
    assert!(matches!(
        setup.ended(refused).await,
        LinkFlowStatus::Refused(_)
    ));
    fake.script_poll(Script::Answer(Err(Failure::Certificate)));
    let failed = setup.started(&carol, "device-code-carol-2").await;
    assert_eq!(setup.ended(failed).await, LinkFlowStatus::Failed);
    held_start(&setup, &carol, "device-code-carol-3").await;
    fake.script_poll(Script::Answer(Ok(approval(4))));
    fake.script_session(Script::Answer(Ok(session(
        "minecraft-4",
        OTHER_PROFILE,
        "Other_Name4",
        at(T0 + DAY),
    ))));
    let replacing = setup.started(&carol, "device-code-carol-4").await;
    assert!(matches!(
        setup.ended(replacing).await,
        LinkFlowStatus::Linked {
            status: AccountStatus::Active,
            ..
        }
    ));

    // The capture works: every flow's end was logged.
    assert!(log_capture::count("fleet_server::app::link_flows::poller", "flow_id") >= 5);
    let mut secrets = vec![
        USER_CODE,
        "device-code-alice",
        "device-code-bob",
        "device-code-carol-1",
        "device-code-carol-2",
        "device-code-carol-3",
        "device-code-carol-4",
        "Secret_Name1",
        "Other_Name4",
    ];
    let simple: Vec<String> = [PROFILE, OTHER_PROFILE]
        .iter()
        .map(|uuid| uuid.replace('-', ""))
        .collect();
    let tokens: Vec<String> = (1..=4)
        .flat_map(|n| {
            [
                format!("access-{n}"),
                format!("refresh-{n}"),
                format!("minecraft-{n}"),
            ]
        })
        .collect();
    secrets.extend([PROFILE, OTHER_PROFILE]);
    secrets.extend(simple.iter().map(String::as_str));
    secrets.extend(tokens.iter().map(String::as_str));
    for secret in secrets {
        log_capture::check_absent(secret).unwrap();
    }
}

#[test]
fn a_flows_status_and_view_debug_without_the_minecraft_name() {
    let linked = LinkFlowStatus::Linked {
        account: AccountId::new_v7(at(T0), [1; 10]).unwrap(),
        status: AccountStatus::Pending,
        name: "Secret_Name1".parse().unwrap(),
    };

    let shown = format!("{linked:?}");

    assert!(!shown.contains("Secret_Name1"), "{shown}");
    assert!(shown.contains("Pending"), "{shown}");
}

#[sqlx::test(migrations = false)]
async fn a_started_flow_and_the_service_debug_without_secrets(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = LinkSetup::new(options).await;
    let alice = setup.person(1, Role::Member).await;
    drop(setup.pace.hold_next());
    setup.script_start("device-code-1");

    let started = setup.start(&alice).await.unwrap();

    let shown = format!("{started:?} {:?}", setup.service);
    assert!(!shown.contains(USER_CODE), "{shown}");
    assert!(!shown.contains("device-code-1"), "{shown}");
    assert!(shown.contains("active: 1"), "{shown}");
}
