//! [`ScriptedCredentials`]: session credentials with per-bot faults, for the
//! chaos test (P4.8). fleet-testkit's `FakeCredentials` scripts one queue
//! for the whole fleet, which serves whichever bot asks next; this one
//! knows which bot each fault is for. Unscripted requests go to the fake,
//! which answers an offline account with its name.

use core::future::Future;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use fleet_core::id::BotId;
use fleet_core::mc::{
    CredentialError, SessionCredentialProvider, SessionCredentials, SessionRequest,
};
use fleet_testkit::mc::FakeCredentials;
use tokio::time::Instant;

/// A scripted answer to one of a bot's session requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// No credentials; `retryable` says whether asking later may help.
    Refuse { retryable: bool },
    /// No answer at all, until the session-request timeout gives up.
    Never,
}

/// A scripted answer that a request used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Used {
    pub(crate) bot: BotId,
    pub(crate) at: Instant,
    pub(crate) answer: Answer,
}

#[derive(Debug, Default)]
struct Shared {
    scripts: BTreeMap<BotId, VecDeque<Answer>>,
    used: Vec<Used>,
}

/// Answers each bot's session requests from its own script first.
#[derive(Debug, Clone)]
pub(crate) struct ScriptedCredentials {
    fake: FakeCredentials,
    shared: Arc<Mutex<Shared>>,
}

impl ScriptedCredentials {
    pub(crate) fn new() -> Self {
        Self {
            fake: FakeCredentials::new(),
            shared: Arc::default(),
        }
    }

    /// Answers `bot`'s next unscripted request with `answer`.
    pub(crate) fn push(&self, bot: BotId, answer: Answer) {
        self.shared
            .lock()
            .unwrap()
            .scripts
            .entry(bot)
            .or_default()
            .push_back(answer);
    }

    /// How many of `bot`'s scripted answers are left.
    pub(crate) fn pending(&self, bot: BotId) -> usize {
        self.shared
            .lock()
            .unwrap()
            .scripts
            .get(&bot)
            .map_or(0, VecDeque::len)
    }

    /// Every scripted answer a request used, in order.
    pub(crate) fn used(&self) -> Vec<Used> {
        self.shared.lock().unwrap().used.clone()
    }
}

impl SessionCredentialProvider for ScriptedCredentials {
    fn session(
        &self,
        request: SessionRequest,
    ) -> impl Future<Output = Result<SessionCredentials, CredentialError>> + Send {
        let scripted = {
            let mut shared = self.shared.lock().unwrap();
            let answer = shared
                .scripts
                .get_mut(&request.bot_id)
                .and_then(VecDeque::pop_front);
            if let Some(answer) = answer {
                shared.used.push(Used {
                    bot: request.bot_id,
                    at: Instant::now(),
                    answer,
                });
            }
            answer
        };
        let fake = self.fake.clone();
        async move {
            match scripted {
                Some(Answer::Refuse { retryable }) => Err(CredentialError { retryable }),
                Some(Answer::Never) => core::future::pending().await,
                None => fake.session(request).await,
            }
        }
    }
}
