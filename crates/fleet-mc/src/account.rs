//! The account a session logs in with (Plan.md P3.3; ADR-0008 §9, ADR-0011).
//!
//! An agent never holds a Microsoft token (security rule 6): the server
//! issues it a short-lived Minecraft session per bot, and [`account`] turns
//! those [`SessionCredentials`] into azalea's [`Account`]:
//!
//! - **Offline** credentials become azalea's own offline account, which never
//!   talks to the session server.
//! - **Online** credentials become a [`TokenAccount`], which joins through the
//!   session server with the server-issued token. The token stays a
//!   [`SecretString`], and `Debug` never shows it.
//!
//! azalea raises no event when the session server refuses a join: it logs the
//! error, and the login stalls until the Minecraft server gives up about 30 s
//! later (ADR-0008 §9). So the account reports the outcome itself, as a
//! classified [`DisconnectReason`], over a bounded channel that the session
//! drains (P3.4). azalea calls `join()` on Bevy's IO pool, not on the bot's
//! host thread, and inside `async_compat`, which provides tokio's timer.
//!
//! The adapter doesn't check the credentials' `expires_at`: the credential
//! provider (P4.3) and the session server decide.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;
use std::sync::{Mutex, MutexGuard, PoisonError};

use azalea::account::{Account, AccountTrait};
use azalea::auth::AuthError;
use azalea::auth::certs::Certificates;
use azalea::auth::sessionserver::{self, ClientSessionServerError, SessionServerJoinOpts};
use fleet_core::disconnect::{AccountRestriction, DisconnectReason, SessionServerFailure};
use fleet_core::id::BotId;
use fleet_core::mc::SessionCredentials;
use fleet_core::value::McUsername;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;
use tracing::debug;
use uuid::Uuid;

/// The boxed future of azalea's `AccountTrait`. azalea's own alias is
/// crate-private.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What [`TokenAccount::join`] hands azalea when the session server doesn't
/// answer in time. It only shapes azalea's own error log; the session learns
/// `SessionServerFailed { TimedOut }`.
const TIMED_OUT: &str = "no answer within the session-join timeout";

/// The session's end of an account's reports: the [`DisconnectReason`] of a
/// failed join. An offline account never reports, so its channel is closed
/// from the start.
pub(crate) type AuthReports = mpsc::Receiver<DisconnectReason>;

/// Builds the azalea account that `bot_id`'s session logs in with, and the
/// channel its failed joins are reported on.
///
/// An online account's join call to the session server may take
/// `join_timeout` (ADR-0011: 10 s).
pub(crate) fn account(
    credentials: SessionCredentials,
    bot_id: BotId,
    join_timeout: Duration,
) -> (Account, AuthReports) {
    // One report is enough: the session ends on the first terminal event
    // (P3.5), and azalea joins at most twice per login.
    let (reports, receiver) = mpsc::channel(1);
    let account = match credentials {
        SessionCredentials::Offline { username } => Account::offline(username.as_str()),
        SessionCredentials::Online {
            username,
            uuid,
            access_token,
            expires_at: _,
        } => Account::from(TokenAccount {
            bot_id,
            username,
            uuid,
            token: access_token,
            certs: Slot::default(),
            join_timeout,
            reports,
        }),
    };
    (account, receiver)
}

/// An online account with a server-issued Minecraft token (ADR-0008 §9).
struct TokenAccount {
    bot_id: BotId,
    username: McUsername,
    uuid: Uuid,
    token: SecretString,
    /// The chat-signing certificates azalea fetches with the token. Chat
    /// signing fails without them.
    certs: Slot<Certificates>,
    join_timeout: Duration,
    reports: mpsc::Sender<DisconnectReason>,
}

/// Hand-written, so it never shows the token or the certificates. azalea's
/// `Account` forwards its `Debug` here.
impl fmt::Debug for TokenAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenAccount")
            .field("bot_id", &self.bot_id)
            .field("username", &self.username)
            .field("uuid", &self.uuid)
            // `SecretString`'s `Debug` is redacted.
            .field("token", &self.token)
            .field("certs", &self.certs)
            .finish_non_exhaustive()
    }
}

impl TokenAccount {
    /// Finishes a join: `join`, the call to the session server, gets the
    /// join timeout, and a failure is reported to the session. Returns what
    /// azalea gets back.
    async fn finish_join(
        &self,
        join: impl Future<Output = Result<(), ClientSessionServerError>>,
    ) -> Result<(), ClientSessionServerError> {
        // A network call: azalea's HTTP client has no timeout (ADR-0011).
        match tokio::time::timeout(self.join_timeout, join).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.report(join_failure(&error), label(&error));
                Err(error)
            }
            Err(_elapsed) => {
                self.report(
                    session_server_failed(SessionServerFailure::TimedOut),
                    "timed_out",
                );
                Err(ClientSessionServerError::Unknown(TIMED_OUT.to_owned()))
            }
        }
    }

    /// Reports a failed join to the session, without waiting. A full channel
    /// already holds the report that ends the session, and a closed one means
    /// the session is gone, so either way this one isn't needed.
    fn report(&self, reason: DisconnectReason, error: &'static str) {
        debug!(bot_id = %self.bot_id, error, "the join through the session server failed");
        if self.reports.try_send(reason).is_err() {
            debug!(bot_id = %self.bot_id, "a join failure was already reported, or the session is gone");
        }
    }
}

impl AccountTrait for TokenAccount {
    fn username(&self) -> &str {
        self.username.as_str()
    }

    fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// azalea needs a plain `String` copy, which can't be zeroized. That's a
    /// known limit (ADR-0011).
    fn access_token(&self) -> Option<String> {
        Some(self.token.expose_secret().to_owned())
    }

    /// A no-op that succeeds (ADR-0011). The core state machine asks for a
    /// fresh token, never the adapter (ADR-0010), and an error could only be
    /// a Microsoft-flow `AuthError`, which azalea would log as if it were
    /// true. After it, azalea retries the join once with the same token, which
    /// fails the same way.
    fn refresh(&self) -> BoxFuture<'_, Result<(), AuthError>> {
        Box::pin(async { Ok(()) })
    }

    fn certs(&self) -> Option<Certificates> {
        self.certs.get()
    }

    fn set_certs(&self, certs: Certificates) {
        self.certs.set(certs);
    }

    /// Tells the session server that the bot is joining, with the
    /// server-issued token, and reports a failure to the session.
    fn join<'a>(
        &'a self,
        public_key: &'a [u8],
        private_key: &'a [u8; 16],
        server_id: &'a str,
        proxy: Option<reqwest::Proxy>,
    ) -> BoxFuture<'a, Result<(), ClientSessionServerError>> {
        Box::pin(self.finish_join(sessionserver::join(SessionServerJoinOpts {
            access_token: self.token.expose_secret(),
            public_key,
            private_key,
            uuid: &self.uuid,
            server_id,
            proxy,
        })))
    }
}

/// What a failed join means for the session (ADR-0011). The match is
/// exhaustive, so an azalea bump that adds an error doesn't build until the
/// error is classified.
fn join_failure(error: &ClientSessionServerError) -> DisconnectReason {
    match error {
        // The token is invalid or expired.
        ClientSessionServerError::InvalidSession | ClientSessionServerError::ForbiddenOperation => {
            DisconnectReason::AuthRejected
        }
        ClientSessionServerError::Banned => restricted(AccountRestriction::Banned),
        ClientSessionServerError::MultiplayerDisabled => {
            restricted(AccountRestriction::MultiplayerDisabled)
        }
        ClientSessionServerError::AuthServersUnreachable
        | ClientSessionServerError::HttpError(_) => {
            session_server_failed(SessionServerFailure::Unreachable)
        }
        ClientSessionServerError::RateLimited => {
            session_server_failed(SessionServerFailure::RateLimited)
        }
        ClientSessionServerError::Unknown(_)
        | ClientSessionServerError::UnexpectedResponse { .. } => {
            session_server_failed(SessionServerFailure::Unexpected)
        }
    }
}

/// A fixed name for the kind of `error`, for the log. azalea's own text can
/// carry the session server's response body, so it's never logged here.
fn label(error: &ClientSessionServerError) -> &'static str {
    match error {
        ClientSessionServerError::InvalidSession => "invalid_session",
        ClientSessionServerError::ForbiddenOperation => "forbidden_operation",
        ClientSessionServerError::Banned => "banned",
        ClientSessionServerError::MultiplayerDisabled => "multiplayer_disabled",
        ClientSessionServerError::AuthServersUnreachable => "auth_servers_unreachable",
        ClientSessionServerError::HttpError(_) => "http_error",
        ClientSessionServerError::RateLimited => "rate_limited",
        ClientSessionServerError::Unknown(_) => "unknown",
        ClientSessionServerError::UnexpectedResponse { .. } => "unexpected_response",
    }
}

const fn restricted(restriction: AccountRestriction) -> DisconnectReason {
    DisconnectReason::AccountRestricted { restriction }
}

const fn session_server_failed(failure: SessionServerFailure) -> DisconnectReason {
    DisconnectReason::SessionServerFailed { failure }
}

/// A value that azalea's threads share: the account's certificates. It's a
/// plain lock, never held across an await. The value stays consistent if a
/// holder panicked, so a poisoned lock is used as it is.
struct Slot<T>(Mutex<Option<T>>);

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

/// Shows only whether a value is set, never the value.
impl<T> fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Slot")
            .field("set", &self.lock().is_some())
            .finish()
    }
}

impl<T> Slot<T> {
    fn set(&self, value: T) {
        *self.lock() = Some(value);
    }

    fn lock(&self) -> MutexGuard<'_, Option<T>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: Clone> Slot<T> {
    fn get(&self) -> Option<T> {
        self.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_capture;
    use rstest::rstest;
    use std::sync::Arc;
    use tokio::sync::mpsc::error::TryRecvError;

    const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";
    const NAME: &str = "AfkBot1";
    const UUID: Uuid = Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef);
    const TOKEN: &str = "placeholder-minecraft-token";
    const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

    fn bot() -> BotId {
        BOT.parse().unwrap()
    }

    fn name() -> McUsername {
        NAME.parse().unwrap()
    }

    fn offline() -> SessionCredentials {
        SessionCredentials::Offline { username: name() }
    }

    fn online(token: &str) -> SessionCredentials {
        SessionCredentials::Online {
            username: name(),
            uuid: UUID,
            access_token: SecretString::from(token.to_owned()),
            expires_at: any_expiry(),
        }
    }

    /// Any expiry will do: the adapter never reads it. fleet-mc doesn't
    /// depend on chrono, so the type is left to inference.
    fn any_expiry<T: Default>() -> T {
        T::default()
    }

    /// A bare `TokenAccount`, for the join bookkeeping that azalea's wrapper
    /// hides.
    fn token_account(token: &str) -> (TokenAccount, AuthReports) {
        let (reports, receiver) = mpsc::channel(1);
        let account = TokenAccount {
            bot_id: bot(),
            username: name(),
            uuid: UUID,
            token: SecretString::from(token.to_owned()),
            certs: Slot::default(),
            join_timeout: JOIN_TIMEOUT,
            reports,
        };
        (account, receiver)
    }

    /// A real `reqwest::Error`, from a proxy URL that can't be parsed.
    fn http_error() -> ClientSessionServerError {
        ClientSessionServerError::HttpError(reqwest::Proxy::http("http://[::1").unwrap_err())
    }

    /// One error of every kind azalea's session-server call returns.
    fn every_error() -> Vec<ClientSessionServerError> {
        vec![
            ClientSessionServerError::InvalidSession,
            ClientSessionServerError::ForbiddenOperation,
            ClientSessionServerError::Banned,
            ClientSessionServerError::MultiplayerDisabled,
            ClientSessionServerError::AuthServersUnreachable,
            http_error(),
            ClientSessionServerError::RateLimited,
            ClientSessionServerError::Unknown("SomethingNewException".to_owned()),
            ClientSessionServerError::UnexpectedResponse {
                status_code: 500,
                body: "Internal Server Error".to_owned(),
            },
        ]
    }

    // --- Building the account ---

    #[test]
    fn offline_credentials_give_an_offline_account_that_never_reports() {
        let (account, mut reports) = account(offline(), bot(), JOIN_TIMEOUT);

        assert_eq!(account.username(), NAME);
        assert_eq!(account.uuid(), Account::offline(NAME).uuid());
        assert_eq!(account.access_token(), None);
        assert_eq!(reports.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn online_credentials_give_an_account_with_their_name_uuid_and_token() {
        let (account, mut reports) = account(online(TOKEN), bot(), JOIN_TIMEOUT);

        assert_eq!(account.username(), NAME);
        assert_eq!(account.uuid(), UUID);
        assert_eq!(account.access_token().as_deref(), Some(TOKEN));
        assert_eq!(account.certs().map(|_| ()), None);
        assert_eq!(reports.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn debug_output_never_shows_the_token() {
        let (account, _reports) = account(online(TOKEN), bot(), JOIN_TIMEOUT);

        let debug = format!("{account:?}");

        assert!(!debug.contains(TOKEN));
        assert!(debug.contains("TokenAccount"));
        assert!(debug.contains(NAME));
        assert!(debug.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn refresh_succeeds_and_keeps_the_token() {
        let (account, _reports) = account(online(TOKEN), bot(), JOIN_TIMEOUT);

        let refreshed = account.refresh().await;

        assert!(refreshed.is_ok());
        assert_eq!(account.access_token().as_deref(), Some(TOKEN));
    }

    // --- Certificate storage ---

    #[test]
    fn a_slot_is_empty_until_set_and_keeps_the_latest_value() {
        let slot = Slot::default();
        assert_eq!(slot.get(), None);

        slot.set(1);
        assert_eq!(slot.get(), Some(1));

        slot.set(2);
        assert_eq!(slot.get(), Some(2));
        assert_eq!(format!("{slot:?}"), "Slot { set: true }");
    }

    #[test]
    fn a_poisoned_slot_still_stores_and_returns_values() {
        let slot = Arc::new(Slot::default());
        slot.set(1);
        let holder = Arc::clone(&slot);
        let panicked = std::thread::spawn(move || {
            let _guard = holder.lock();
            panic!("poisons the lock");
        })
        .join();
        assert!(panicked.is_err());
        assert!(slot.0.is_poisoned());

        assert_eq!(slot.get(), Some(1));
        slot.set(2);
        assert_eq!(slot.get(), Some(2));
    }

    // --- Session-server errors ---

    #[rstest]
    #[case::invalid_session(
        ClientSessionServerError::InvalidSession,
        DisconnectReason::AuthRejected
    )]
    #[case::forbidden_operation(
        ClientSessionServerError::ForbiddenOperation,
        DisconnectReason::AuthRejected
    )]
    #[case::banned(
        ClientSessionServerError::Banned,
        restricted(AccountRestriction::Banned)
    )]
    #[case::multiplayer_disabled(
        ClientSessionServerError::MultiplayerDisabled,
        restricted(AccountRestriction::MultiplayerDisabled)
    )]
    #[case::auth_servers_unreachable(
        ClientSessionServerError::AuthServersUnreachable,
        session_server_failed(SessionServerFailure::Unreachable)
    )]
    #[case::http_error(http_error(), session_server_failed(SessionServerFailure::Unreachable))]
    #[case::rate_limited(
        ClientSessionServerError::RateLimited,
        session_server_failed(SessionServerFailure::RateLimited)
    )]
    #[case::unknown(
        ClientSessionServerError::Unknown("SomethingNewException".to_owned()),
        session_server_failed(SessionServerFailure::Unexpected)
    )]
    #[case::unexpected_response(
        ClientSessionServerError::UnexpectedResponse {
            status_code: 500,
            body: "Internal Server Error".to_owned(),
        },
        session_server_failed(SessionServerFailure::Unexpected)
    )]
    fn join_errors_become_disconnect_reasons(
        #[case] error: ClientSessionServerError,
        #[case] expected: DisconnectReason,
    ) {
        assert_eq!(join_failure(&error), expected);
    }

    // --- Finishing a join ---

    #[tokio::test(start_paused = true)]
    async fn a_successful_join_reports_nothing() {
        let (account, mut reports) = token_account(TOKEN);

        let joined = account.finish_join(async { Ok(()) }).await;

        assert!(joined.is_ok());
        assert_eq!(reports.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_join_reports_its_reason_and_returns_azaleas_error() {
        let (account, mut reports) = token_account(TOKEN);

        let joined = account
            .finish_join(async { Err(ClientSessionServerError::ForbiddenOperation) })
            .await;

        assert!(matches!(
            joined,
            Err(ClientSessionServerError::ForbiddenOperation)
        ));
        assert_eq!(reports.try_recv(), Ok(DisconnectReason::AuthRejected));
    }

    #[tokio::test(start_paused = true)]
    async fn a_join_without_an_answer_times_out_and_reports_it() {
        let (account, mut reports) = token_account(TOKEN);
        let started = tokio::time::Instant::now();

        let joined = account.finish_join(core::future::pending()).await;

        assert_eq!(started.elapsed(), JOIN_TIMEOUT);
        assert!(matches!(
            joined,
            Err(ClientSessionServerError::Unknown(ref text)) if text == TIMED_OUT
        ));
        assert_eq!(
            reports.try_recv(),
            Ok(session_server_failed(SessionServerFailure::TimedOut))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_join_that_answers_just_in_time_succeeds() {
        let (account, mut reports) = token_account(TOKEN);
        let almost = JOIN_TIMEOUT.checked_sub(Duration::from_millis(1)).unwrap();

        let joined = account
            .finish_join(async {
                tokio::time::sleep(almost).await;
                Ok(())
            })
            .await;

        assert!(joined.is_ok());
        assert_eq!(reports.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_failure_while_a_report_is_pending_keeps_the_first() {
        let (account, mut reports) = token_account(TOKEN);

        let first = account
            .finish_join(async { Err(ClientSessionServerError::InvalidSession) })
            .await;
        let second = account
            .finish_join(async { Err(ClientSessionServerError::RateLimited) })
            .await;

        assert!(matches!(
            first,
            Err(ClientSessionServerError::InvalidSession)
        ));
        assert!(matches!(second, Err(ClientSessionServerError::RateLimited)));
        assert_eq!(reports.try_recv(), Ok(DisconnectReason::AuthRejected));
        assert_eq!(reports.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_after_the_session_is_gone_still_returns_the_error() {
        let (account, reports) = token_account(TOKEN);
        drop(reports);

        let joined = account
            .finish_join(async { Err(ClientSessionServerError::Banned) })
            .await;

        assert!(matches!(joined, Err(ClientSessionServerError::Banned)));
    }

    // --- Log redaction (ADR-0011) ---

    /// Runs every path of the adapter with a marker token while every level
    /// of every target is captured, and checks that the marker never shows.
    /// azalea's own login path is covered by the online-mode slow scenario
    /// (P3.7).
    #[tokio::test(start_paused = true)]
    async fn the_token_never_appears_in_the_adapters_logs() {
        const MARKER: &str = "afkfleet-redaction-marker-7f3a9c";
        log_capture::install();
        let (wrapped, _wrapped_reports) = account(online(MARKER), bot(), JOIN_TIMEOUT);
        let (bare, _bare_reports) = token_account(MARKER);

        tracing::trace!(account = ?wrapped, "an azalea account");
        tracing::trace!(?bare, "a token account");
        tracing::trace!("{wrapped:?} {bare:#?}");
        for error in every_error() {
            let _ = bare.finish_join(async { Err(error) }).await;
        }
        let _ = bare.finish_join(core::future::pending()).await;
        let _ = wrapped.refresh().await;
        let _ = (wrapped.access_token(), wrapped.certs().is_some());

        assert!(
            log_capture::count(
                "fleet_mc::account",
                "the join through the session server failed"
            ) > 0,
            "the adapter's own log lines weren't captured"
        );
        log_capture::assert_absent(MARKER);
    }
}
