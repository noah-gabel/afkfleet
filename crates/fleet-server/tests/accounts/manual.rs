//! The user's hands-on Microsoft checks (Plan.md P9.3, P9.4, ADR-0018): the
//! real adapter against Microsoft, with the user's own account, and the link
//! flow through serve's own app and its routes.
//!
//! **They need the user's real Microsoft account, so only the user runs
//! them**, with `just test-ms-account`; the AI never runs them or the
//! `manual` profile.
//!
//! - The adapter's two checks keep everything in memory. The link flow's
//!   database lives in a fresh folder in the system's temp directory,
//!   sealed with a vault key drawn from the OS for the run and kept only in
//!   memory. The folder is deleted at every end, pass, FAIL or panic; one
//!   that Ctrl+C leaves behind holds a refresh token nobody can open.
//! - It prints only the link, the code, the deadline, and pass or FAIL per
//!   step with an error's text, which names a rule but never a value. Never
//!   a token, the Minecraft name or the UUID.
//! - Every log line, at every level and from every target, is captured in
//!   memory and checked for every value the run saw. A leak names only
//!   targets and counts.
//!
//! The waits are real, since Microsoft sets the poll interval. That's these
//! three tests' approved exception to "never a real sleep" (CLAUDE.md,
//! ADR-0018). The adapter's two checks wait at most 8 minutes each, the link
//! flow's at most 4 minutes for each of its two codes, reading the flow's
//! status every 2 seconds: always inside the `manual` profile's 10-minute
//! limit.

use core::fmt::Display;
use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::account::AccountStatus;
use fleet_core::audit::{AuditAction, AuditOutcome, AuditTarget, RecordedTarget, RecordedValue};
use fleet_core::authz::Role;
use fleet_core::id::AccountId;
use fleet_core::system::{Clock, SecureRandom as _};
use fleet_core::time::{add, elapsed};
use fleet_server::cli::serve::ServerPorts;
use fleet_server::config::{AccountsConfig, RateLimitConfig};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::system::{MonotonicClock, OsRandom, SystemClock, TokioPace};
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::audit::{AuditLimit, AuditPage, AuditRecord};
use fleet_server::ports::msauth::{
    DeviceFlow, Failure, MicrosoftAuthProvider as _, PollOutcome, RefreshError, RefreshToken,
};
use fleet_server::ports::store::Store as _;
use fleet_server::ports::vault::{Vault as _, VaultPurpose};
use fleet_testkit::log_capture;
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use serde_json::Value;
use sqlx::sqlite::SqliteConnectOptions;
use tempfile::Builder;
use tokio::time::Instant;

use crate::support::http::{Person, Server};
use crate::support::service::bounded;

/// The longest wait for the user's answer.
const WAIT: Duration = Duration::from_mins(8);

/// The steps' results, printed as they come.
#[derive(Debug, Default)]
struct Steps {
    failures: Vec<String>,
    /// Every value the run saw, to look for in the logs at the end.
    seen: Vec<String>,
}

/// Prints a step that passed.
fn pass(step: &str) {
    println!("{step}: pass");
}

impl Steps {
    fn fail(&mut self, step: &str, why: impl Display) {
        println!("{step}: FAIL ({why})");
        self.failures.push(format!("{step}: {why}"));
    }

    fn check(&mut self, step: &str, passed: bool, why: &str) {
        if passed {
            pass(step);
        } else {
            self.fail(step, why);
        }
    }

    fn saw(&mut self, value: &str) {
        self.seen.push(value.to_owned());
    }

    /// Checks the logs for every value seen, then fails the test if any
    /// step failed.
    fn finish(mut self) {
        let leaks: Vec<String> = self
            .seen
            .iter()
            .filter_map(|value| log_capture::check_absent(value).err())
            .map(|leak| leak.to_string())
            .collect();
        if leaks.is_empty() {
            pass("redaction");
        } else {
            for leak in leaks {
                self.fail("redaction", leak);
            }
        }
        assert!(
            self.failures.is_empty(),
            "steps failed: {:#?}",
            self.failures
        );
    }
}

/// How waiting for the user's answer ended.
#[derive(Debug)]
enum Waited {
    /// An answer other than "pending" or "slow down".
    Outcome(PollOutcome),
    /// A poll failed.
    Failed(Failure),
    /// No answer by the deadline.
    OutOfTime,
}

/// The real adapter, on the real clock, with aws-lc-rs as the provider.
fn adapter() -> ReqwestMicrosoftAuth {
    // Another test in this process may have installed it already.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    log_capture::install().unwrap();
    ReqwestMicrosoftAuth::new(Arc::new(SystemClock)).unwrap()
}

/// Starts a flow and prints its link, its code and the deadline.
async fn start(
    adapter: &ReqwestMicrosoftAuth,
    steps: &mut Steps,
    what_to_do: &str,
) -> Option<(DeviceFlow, Instant)> {
    let flow = match adapter.start_device_flow().await {
        Ok(flow) => flow,
        Err(error) => {
            steps.fail("start", error);
            return None;
        }
    };
    pass("start");
    steps.saw(flow.code.expose_secret());
    steps.saw(flow.user_code.as_str());
    let now = SystemClock.now();
    let wait = elapsed(now, flow.expires_at).min(WAIT);
    println!();
    println!(
        "Open {} and enter the code {}, then {what_to_do}.",
        flow.verification_uri.as_str(),
        flow.user_code.as_str()
    );
    println!("Waiting until {} UTC.", add(now, wait).format("%H:%M:%S"));
    println!();
    Some((flow, Instant::now() + wait))
}

/// Polls at Microsoft's interval until the user answers, a poll fails or
/// `deadline` passes, counting the "pending" answers.
async fn wait_for_answer(
    adapter: &ReqwestMicrosoftAuth,
    flow: &DeviceFlow,
    deadline: Instant,
) -> (Waited, usize) {
    let mut interval = flow.interval;
    let mut pending = 0;
    loop {
        if Instant::now() + interval.get() > deadline {
            return (Waited::OutOfTime, pending);
        }
        tokio::time::sleep(interval.get()).await;
        match adapter.poll(&flow.code).await {
            Ok(PollOutcome::Pending) => pending += 1,
            Ok(PollOutcome::SlowDown) => interval = interval.slowed_down(),
            Ok(outcome) => return (Waited::Outcome(outcome), pending),
            Err(failure) => return (Waited::Failed(failure), pending),
        }
    }
}

/// What a wait that didn't end as expected found, without any value.
fn describe(waited: &Waited) -> String {
    match waited {
        Waited::Outcome(PollOutcome::Authorized(_)) => "approved".to_owned(),
        Waited::Outcome(PollOutcome::Declined) => "declined".to_owned(),
        Waited::Outcome(PollOutcome::Expired) => "the code expired".to_owned(),
        Waited::Outcome(PollOutcome::Pending | PollOutcome::SlowDown) => "still pending".to_owned(),
        Waited::Failed(failure) => failure.to_string(),
        Waited::OutOfTime => "no answer by the deadline".to_owned(),
    }
}

#[tokio::test]
async fn manual_microsoft_account_check() {
    let adapter = adapter();
    let mut steps = Steps::default();
    let Some((flow, deadline)) = start(&adapter, &mut steps, "approve the sign-in").await else {
        return steps.finish();
    };

    let (waited, pending) = wait_for_answer(&adapter, &flow, deadline).await;
    steps.check(
        "pending seen",
        pending > 0,
        "no authorization_pending before the answer",
    );
    let tokens = match waited {
        Waited::Outcome(PollOutcome::Authorized(tokens)) => {
            pass("authorized");
            tokens
        }
        other => {
            steps.fail("authorized", describe(&other));
            return steps.finish();
        }
    };
    steps.saw(tokens.access.expose_secret());
    steps.saw(tokens.refresh.expose_secret());

    let first = match adapter.minecraft_session(&tokens.access).await {
        Ok(session) => {
            pass("minecraft session (owned, with a profile)");
            session
        }
        Err(error) => {
            steps.fail("minecraft session (owned, with a profile)", error);
            return steps.finish();
        }
    };
    steps.saw(first.token.expose_secret());
    steps.saw(first.name.as_str());
    steps.saw(&first.uuid.to_string());
    steps.saw(&first.uuid.as_uuid().simple().to_string());

    match adapter.refresh(&tokens.refresh).await {
        Ok(refreshed) => {
            pass("refresh");
            let rotated = refreshed.refresh.expose_secret() != tokens.refresh.expose_secret();
            println!(
                "refresh token rotated: {}",
                if rotated { "yes" } else { "no" }
            );
            steps.saw(refreshed.access.expose_secret());
            steps.saw(refreshed.refresh.expose_secret());
            match adapter.minecraft_session(&refreshed.access).await {
                Ok(second) => {
                    steps.saw(second.token.expose_secret());
                    steps.check(
                        "second session, same profile",
                        second.uuid == first.uuid,
                        "another profile's UUID",
                    );
                }
                Err(error) => steps.fail("second session, same profile", error),
            }
        }
        Err(error) => steps.fail("refresh", error),
    }

    let invented =
        RefreshToken::try_from(SecretString::from("afkfleet-invented-refresh-token")).unwrap();
    match adapter.refresh(&invented).await {
        Err(RefreshError::InvalidGrant) => pass("invented refresh token is invalid_grant"),
        Err(error) => steps.fail("invented refresh token is invalid_grant", error),
        Ok(_) => steps.fail(
            "invented refresh token is invalid_grant",
            "Microsoft accepted it",
        ),
    }

    steps.finish();
}

#[tokio::test]
async fn manual_microsoft_declined_link() {
    let adapter = adapter();
    let mut steps = Steps::default();
    let Some((flow, deadline)) = start(&adapter, &mut steps, "press Cancel or Decline").await
    else {
        return steps.finish();
    };

    let (waited, _) = wait_for_answer(&adapter, &flow, deadline).await;
    match waited {
        Waited::Outcome(PollOutcome::Declined) => pass("declined"),
        other => steps.fail("declined", describe(&other)),
    }

    steps.finish();
}

// --- The link flow, end to end (P9.4) ------------------------------------------

/// The longest wait for each of the link flow's codes.
const LINK_WAIT: Duration = Duration::from_mins(4);
/// How often the link flow's status is read.
const READ_EVERY: Duration = Duration::from_secs(2);

/// How a flow's status ended, as its reads saw it.
struct Read {
    /// The last answer's body.
    last: Value,
    /// Whether `waiting` was seen.
    waiting: bool,
    /// Whether `finishing` was seen.
    finishing: bool,
}

/// Starts a flow for `by` through the route, prints its link, its code and
/// the deadline, and reads its status every 2 seconds until it leaves
/// `waiting` and `finishing`, or the deadline passes.
async fn link_once(
    server: &Server,
    by: &Person,
    steps: &mut Steps,
    what_to_do: &str,
) -> Option<Read> {
    let started = server.start_link(by).await;
    if started.status != 201 {
        steps.fail("start", format!("{} {}", started.status, started.code()));
        return None;
    }
    pass("start");
    let flow = started.body["flow_id"].as_str().unwrap().to_owned();
    let user_code = started.body["user_code"].as_str().unwrap().to_owned();
    steps.saw(&user_code);
    let now = SystemClock.now();
    let expires_at: DateTime<Utc> = started.body["expires_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let wait = elapsed(now, expires_at).min(LINK_WAIT);
    println!();
    println!(
        "Open {} and enter the code {user_code}, then {what_to_do}.",
        started.body["verification_uri"].as_str().unwrap()
    );
    println!("Waiting until {} UTC.", add(now, wait).format("%H:%M:%S"));
    println!();
    let deadline = Instant::now() + wait;
    let mut read = Read {
        last: Value::Null,
        waiting: false,
        finishing: false,
    };
    loop {
        let seen = server.status(by, &flow).await;
        if seen.status != 200 {
            steps.fail("status", format!("{} {}", seen.status, seen.code()));
            return None;
        }
        read.last = seen.body;
        match read.last["status"].as_str() {
            Some("waiting") => read.waiting = true,
            Some("finishing") => read.finishing = true,
            _ => return Some(read),
        }
        if Instant::now() + READ_EVERY > deadline {
            return Some(read);
        }
        tokio::time::sleep(READ_EVERY).await;
    }
}

/// The audit entries with `action`, oldest first.
async fn entries(server: &Server, action: &AuditAction) -> Vec<AuditRecord> {
    let page = AuditPage {
        before: None,
        limit: AuditLimit::new(100).unwrap(),
    };
    let mut entries: Vec<AuditRecord> = server
        .db
        .audit()
        .list(page)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| &entry.action == action)
        .collect();
    entries.reverse();
    entries
}

/// The text `entry` holds under `key`.
fn meta_text<'a>(entry: &'a AuditRecord, key: &str) -> Option<&'a str> {
    entry
        .metadata
        .entries()
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| match value {
            RecordedValue::Text(text) => Some(text.as_str()),
            _ => None,
        })
}

/// A vault key drawn from the OS for this run, kept only in memory: a
/// database the run leaves behind can't be opened by anyone.
fn run_key() -> SecretBox<[u8; 32]> {
    SecretBox::init_with_mut(|key: &mut [u8; 32]| OsRandom.fill(key).unwrap())
}

/// The link flow's steps on `server`, with `vault` over the server's key:
/// the Member's link, then the Owner's with the same Microsoft account.
async fn link_steps(server: Arc<Server>, vault: XChaChaVault) -> Steps {
    let member = server.person(1, Role::Member).await;
    let owner = server.person(2, Role::Owner).await;
    let mut steps = Steps::default();
    if let Some(uuid) = member_links(&server, &vault, &member, &mut steps).await {
        owner_finds_it_taken(&server, &owner, &uuid, &mut steps).await;
    }
    steps
}

/// The Member links the account, and the server stores and records it: the
/// profile's UUID, or `None` once a step failed that the next ones need.
async fn member_links(
    server: &Server,
    vault: &XChaChaVault,
    member: &Person,
    steps: &mut Steps,
) -> Option<String> {
    // The Member links the account: it waits for approval.
    let read = link_once(server, member, steps, "approve the sign-in").await?;
    steps.check(
        "waiting seen",
        read.waiting,
        "the first read wasn't waiting",
    );
    println!(
        "finishing seen: {} (for information: it may pass between two reads)",
        if read.finishing { "yes" } else { "no" }
    );
    let linked = read.last["status"] == "linked" && read.last["account_status"] == "pending";
    steps.check(
        "linked, pending",
        linked,
        &format!("ended {}", read.last["status"]),
    );
    if !linked {
        return None;
    }
    let account: AccountId = read.last["account_id"].as_str().unwrap().parse().unwrap();
    let name = read.last["name"].as_str().unwrap().to_owned();
    steps.saw(&name);
    let stored = server.db.accounts().find(account).await.unwrap().unwrap();
    let AccountCredential::Microsoft {
        uuid,
        refresh_envelope,
        status,
    } = stored.credential
    else {
        steps.fail("stored account", "not a Microsoft account");
        return None;
    };
    steps.saw(&uuid.to_string());
    steps.saw(&uuid.as_uuid().simple().to_string());
    steps.check(
        "stored account: the Member's, pending, with the profile's name",
        stored.owner == member.id
            && status == AccountStatus::Pending
            && stored.name.as_str() == name,
        "another owner, status or name",
    );
    match vault.open(
        VaultPurpose::MsRefreshToken,
        account.into(),
        &refresh_envelope,
    ) {
        Ok(plaintext) => {
            let token = core::str::from_utf8(plaintext.expose_secret())
                .ok()
                .map(|text| SecretString::from(text.to_owned()));
            match token.map(RefreshToken::try_from) {
                Some(Ok(token)) => {
                    steps.saw(token.expose_secret());
                    pass("sealed refresh token opens for the account");
                }
                _ => steps.fail("sealed refresh token opens for the account", "not a token"),
            }
        }
        Err(error) => steps.fail("sealed refresh token opens for the account", error),
    }
    let starts = entries(server, &AuditAction::ACCOUNT_START_LINK).await;
    steps.check(
        "account.start_link recorded",
        starts.len() == 1 && starts[0].actor == Some(member.id),
        "missing or another actor",
    );
    let links = entries(server, &AuditAction::ACCOUNT_LINK).await;
    let uuid_text = uuid.to_string();
    steps.check(
        "account.link recorded with the creator, the name and the UUID",
        links.len() == 1
            && links[0].outcome == AuditOutcome::Success
            && links[0].actor == Some(member.id)
            && meta_text(&links[0], "name") == Some(name.as_str())
            && meta_text(&links[0], "uuid") == Some(uuid_text.as_str()),
        "missing, or another actor, name or UUID",
    );
    Some(uuid_text)
}

/// The Owner tries the same Microsoft account, whose profile is `uuid`:
/// it's taken.
async fn owner_finds_it_taken(server: &Server, owner: &Person, uuid: &str, steps: &mut Steps) {
    if let Some(read) = link_once(
        server,
        owner,
        steps,
        "sign in with the SAME Microsoft account and approve",
    )
    .await
    {
        steps.check(
            "the same account again is taken",
            read.last == serde_json::json!({ "status": "taken" }),
            &format!("ended {}", read.last["status"]),
        );
        let links = entries(server, &AuditAction::ACCOUNT_LINK).await;
        let failure = links.get(1);
        steps.check(
            "account.link Failure recorded for the Owner, reason taken, with the UUID",
            failure.is_some_and(|entry| {
                entry.outcome == AuditOutcome::Failure
                    && entry.actor == Some(owner.id)
                    && entry.target == Some(RecordedTarget::Known(AuditTarget::User(owner.id)))
                    && meta_text(entry, "reason") == Some("taken")
                    && meta_text(entry, "uuid") == Some(uuid)
            }),
            "missing or different",
        );
    }
}

#[tokio::test]
async fn manual_link_flow_member_then_owner() {
    // A fresh folder, deleted at every end below, and a key for this run
    // alone: a folder that Ctrl+C leaves behind holds a refresh token nobody
    // can open.
    let dir = Builder::new()
        .prefix("afkfleet-link-flow-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("link-flow.db");
    tokio::fs::File::create(&path).await.unwrap();
    let key = run_key();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let server = Arc::new(
        Server::start(
            SqliteConnectOptions::new().filename(&path),
            &RateLimitConfig::default(),
            AccountsConfig::default(),
            &key,
            ServerPorts {
                limiter_clock: Arc::new(MonotonicClock::start()),
                random: Arc::new(OsRandom),
                microsoft: Arc::new(adapter()),
                pace: Arc::new(TokioPace),
                clock,
            },
        )
        .await,
    );
    let vault = XChaChaVault::new(
        VaultKeys::from_keys(
            BTreeMap::from([(1, SecretBox::new(Box::new(*key.expose_secret())))]),
            1,
        )
        .unwrap(),
        Arc::new(OsRandom),
    );
    drop(key);

    // The steps run in a task of their own, so a panic comes back here as an
    // error: the flows stop, the database closes and its folder goes on
    // every end.
    let ended = tokio::spawn(link_steps(Arc::clone(&server), vault)).await;
    bounded(server.link_flows.shutdown()).await;
    bounded(server.db.close()).await;
    drop(server);
    let deleted = dir.close().is_ok();
    match ended {
        Ok(mut steps) => {
            steps.check("database deleted", deleted, "its folder is still there");
            steps.finish();
        }
        Err(failed) => {
            if !deleted {
                println!("database deleted: FAIL (its folder is still there)");
            }
            std::panic::resume_unwind(failed.into_panic());
        }
    }
}
