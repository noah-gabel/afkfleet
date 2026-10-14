//! [`FakeMicrosoftAuth`]: the [`MicrosoftAuthProvider`] for the account
//! services' tests (ADR-0018). It lives in this test binary, so it never
//! reaches a release build, and the account tests of later groups join
//! the binary to use it.
//!
//! - Each method answers from its own queue of scripted results, in order.
//! - A call with nothing scripted panics and names the method: that's a bug
//!   in the test.
//! - A scripted [`Script::Hang`] never answers, for the callers' bounds and
//!   cancellation.
//! - The log records each call and the token it got.
//!
//! Every token here is invented.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_server::ports::msauth::{
    DeviceCode, DeviceFlow, Failure, McToken, MicrosoftAuthProvider, MicrosoftTokens,
    MinecraftSession, MsAccessToken, PollInterval, PollOutcome, RefreshError, RefreshToken,
    SessionError, UserCode, VerificationUri,
};
use secrecy::SecretString;

/// What a call answers.
#[derive(Debug)]
pub(crate) enum Script<T> {
    /// This result.
    Answer(T),
    /// Nothing, ever: the caller has to give up.
    Hang,
}

/// A call the fake received, with the token it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    StartDeviceFlow,
    Poll { device_code: String },
    Refresh { refresh_token: String },
    MinecraftSession { access_token: String },
}

#[derive(Debug, Default)]
struct Scripts {
    start: VecDeque<Script<Result<DeviceFlow, Failure>>>,
    poll: VecDeque<Script<Result<PollOutcome, Failure>>>,
    refresh: VecDeque<Script<Result<MicrosoftTokens, RefreshError>>>,
    session: VecDeque<Script<Result<MinecraftSession, SessionError>>>,
    calls: Vec<Call>,
}

/// The fake; see the [module docs](self).
#[derive(Debug, Default)]
pub(crate) struct FakeMicrosoftAuth {
    scripts: Mutex<Scripts>,
}

impl FakeMicrosoftAuth {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn script_start(&self, script: Script<Result<DeviceFlow, Failure>>) {
        self.lock().start.push_back(script);
    }

    pub(crate) fn script_poll(&self, script: Script<Result<PollOutcome, Failure>>) {
        self.lock().poll.push_back(script);
    }

    pub(crate) fn script_refresh(&self, script: Script<Result<MicrosoftTokens, RefreshError>>) {
        self.lock().refresh.push_back(script);
    }

    pub(crate) fn script_session(&self, script: Script<Result<MinecraftSession, SessionError>>) {
        self.lock().session.push_back(script);
    }

    /// Every call so far, in order.
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// The scripts, poison-tolerantly: a test that panicked in a call leaves
    /// them usable.
    fn lock(&self) -> MutexGuard<'_, Scripts> {
        self.scripts.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The next script of `method`'s `queue`.
fn next<T>(queue: &mut VecDeque<Script<T>>, method: &str) -> Script<T> {
    queue
        .pop_front()
        .unwrap_or_else(|| panic!("FakeMicrosoftAuth::{method} was called with nothing scripted"))
}

/// What `script` answers, or never.
async fn answer<T>(script: Script<T>) -> T {
    match script {
        Script::Answer(result) => result,
        Script::Hang => std::future::pending().await,
    }
}

#[async_trait]
impl MicrosoftAuthProvider for FakeMicrosoftAuth {
    async fn start_device_flow(&self) -> Result<DeviceFlow, Failure> {
        let script = {
            let mut scripts = self.lock();
            scripts.calls.push(Call::StartDeviceFlow);
            next(&mut scripts.start, "start_device_flow")
        };
        answer(script).await
    }

    async fn poll(&self, code: &DeviceCode) -> Result<PollOutcome, Failure> {
        let script = {
            let mut scripts = self.lock();
            scripts.calls.push(Call::Poll {
                device_code: code.expose_secret().to_owned(),
            });
            next(&mut scripts.poll, "poll")
        };
        answer(script).await
    }

    async fn refresh(&self, token: &RefreshToken) -> Result<MicrosoftTokens, RefreshError> {
        let script = {
            let mut scripts = self.lock();
            scripts.calls.push(Call::Refresh {
                refresh_token: token.expose_secret().to_owned(),
            });
            next(&mut scripts.refresh, "refresh")
        };
        answer(script).await
    }

    async fn minecraft_session(
        &self,
        access: &MsAccessToken,
    ) -> Result<MinecraftSession, SessionError> {
        let script = {
            let mut scripts = self.lock();
            scripts.calls.push(Call::MinecraftSession {
                access_token: access.expose_secret().to_owned(),
            });
            next(&mut scripts.session, "minecraft_session")
        };
        answer(script).await
    }
}

// --- Invented values ---------------------------------------------------------

fn secret(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

pub(crate) fn device_code(text: &str) -> DeviceCode {
    DeviceCode::try_from(secret(text)).unwrap()
}

pub(crate) fn refresh_token(text: &str) -> RefreshToken {
    RefreshToken::try_from(secret(text)).unwrap()
}

pub(crate) fn access_token(text: &str) -> MsAccessToken {
    MsAccessToken::try_from(secret(text)).unwrap()
}

/// A started flow at Microsoft's link, polled every 5 seconds.
pub(crate) fn device_flow(code: &str, user_code: &str, expires_at: DateTime<Utc>) -> DeviceFlow {
    DeviceFlow {
        code: device_code(code),
        user_code: UserCode::try_from(user_code).unwrap(),
        verification_uri: VerificationUri::try_from("https://www.microsoft.com/link").unwrap(),
        expires_at,
        interval: PollInterval::DEFAULT,
    }
}

pub(crate) fn tokens(
    access: &str,
    refresh: &str,
    access_expires_at: DateTime<Utc>,
) -> MicrosoftTokens {
    MicrosoftTokens {
        access: access_token(access),
        access_expires_at,
        refresh: refresh_token(refresh),
    }
}

/// A session of the profile `uuid` (lowercase hyphenated) named `name`.
pub(crate) fn session(
    token: &str,
    uuid: &str,
    name: &str,
    expires_at: DateTime<Utc>,
) -> MinecraftSession {
    MinecraftSession {
        token: McToken::try_from(secret(token)).unwrap(),
        expires_at,
        uuid: uuid.parse().unwrap(),
        name: name.parse().unwrap(),
    }
}
