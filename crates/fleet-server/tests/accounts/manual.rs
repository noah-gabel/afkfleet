//! The user's hands-on Microsoft check (Plan.md P9.3, ADR-0018): the real
//! adapter against Microsoft, with the user's own account.
//!
//! **It needs the user's real Microsoft account, so only the user runs it**,
//! with `just test-ms-account`; the AI never runs it or the `manual`
//! profile.
//!
//! - Everything stays in memory, and nothing is written to disk.
//! - It prints only the link, the code, the deadline, and pass or FAIL per
//!   step with an error's text, which names a rule but never a value. Never
//!   a token, the Minecraft name or the UUID.
//! - Every log line, at every level and from every target, is captured in
//!   memory and checked for every value the run saw. A leak names only
//!   targets and counts.
//!
//! The waits are real, since Microsoft sets the poll interval. That's these
//! two tests' approved exception to "never a real sleep" (CLAUDE.md,
//! ADR-0018). Each waits at most 8 minutes, or until the code expires if
//! that's sooner, inside the `manual` profile's 10-minute limit.

use core::fmt::Display;
use core::time::Duration;
use std::sync::Arc;

use fleet_core::system::Clock as _;
use fleet_core::time::{add, elapsed};
use fleet_server::infra::msauth::ReqwestMicrosoftAuth;
use fleet_server::infra::system::SystemClock;
use fleet_server::ports::msauth::{
    DeviceFlow, Failure, MicrosoftAuthProvider as _, PollOutcome, RefreshError, RefreshToken,
};
use fleet_testkit::log_capture;
use secrecy::SecretString;
use tokio::time::Instant;

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
