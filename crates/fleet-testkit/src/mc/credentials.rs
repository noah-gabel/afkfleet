//! [`FakeCredentials`]: hands out scripted session credentials.

use core::future::Future;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fleet_core::bot::BotAccount;
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider, SessionCredentials, SessionRequest,
};

/// A fake [`SessionCredentialProvider`] (Plan.md P4.3, ADR-0013). Clones
/// share the same script and log.
///
/// Every request is recorded. It gets the next scripted answer; with nothing
/// scripted, an offline account gets offline credentials and an online one is
/// refused as not retryable, as fleet-runtime's `OfflineCredentials` does. A
/// test that needs online credentials scripts them.
#[derive(Debug, Clone, Default)]
pub struct FakeCredentials {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    script: VecDeque<Scripted>,
    requests: Vec<SessionRequest>,
}

/// What the fake does with one request.
#[derive(Debug)]
enum Scripted {
    Answer(Result<SessionCredentials, CredentialError>),
    NeverAnswer,
}

impl FakeCredentials {
    /// Creates a fake with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts the answer to a later request. Scripted answers are used in
    /// the order they were pushed.
    pub fn push_answer(&self, answer: Result<SessionCredentials, CredentialError>) {
        self.lock().script.push_back(Scripted::Answer(answer));
    }

    /// Scripts a later request that never gets an answer, for testing the
    /// caller's timeout.
    pub fn push_never_answer(&self) {
        self.lock().script.push_back(Scripted::NeverAnswer);
    }

    /// Returns every request so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<SessionRequest> {
        self.lock().requests.clone()
    }

    /// Records `request` and returns what the script says to do with it.
    fn take(&self, request: SessionRequest) -> Scripted {
        let mut state = self.lock();
        let scripted = state
            .script
            .pop_front()
            .unwrap_or_else(|| Scripted::Answer(default_answer(&request.account)));
        state.requests.push(request);
        scripted
    }

    /// Locks the state. A test that panicked while holding the lock leaves
    /// consistent data behind, so a poisoned lock is used as it is.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SessionCredentialProvider for FakeCredentials {
    fn session(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<SessionCredentials, CredentialError>> + Send {
        let scripted = self.take(request);
        async move {
            match scripted {
                Scripted::Answer(answer) => answer,
                Scripted::NeverAnswer => core::future::pending().await,
            }
        }
    }
}

/// The answer without a script: offline credentials for an offline account,
/// and a refusal for good for an online one.
fn default_answer(account: &BotAccount) -> Result<SessionCredentials, CredentialError> {
    match account {
        BotAccount::Offline(username) => Ok(SessionCredentials::Offline {
            username: username.clone(),
        }),
        BotAccount::Online(_) => Err(CredentialError { retryable: false }),
    }
}
