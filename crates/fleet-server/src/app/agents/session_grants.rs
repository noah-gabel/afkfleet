//! [`SessionGrantService`]: the answers to agents' session requests (Plan.md
//! P10.6; ADR-0019, S57–S61, U14, R39–R46), the one place Minecraft tokens
//! leave the server, behind the [`SessionGrants`] port the gRPC port calls.
//!
//! # The steps (S59)
//! The stream's registry passed the request: the bot was held assigned by
//! a reconciled stream that hadn't ended. Then:
//! 1. **The pre-check,** on the read connections: the stream's own
//!    certificate, by its fingerprint, naming the stream's agent and
//!    admitting it by fleet-core's rule (unrevoked, unexpired by the
//!    server's clock, its agent enabled); then the bot's row must name that
//!    agent; then fleet-core's `placeable`, as the reconciler judges it
//!    (a server, desired running, an enabled owner, and an active Microsoft
//!    account, or an offline one while `[accounts] allow_offline` is on).
//!    The account comes from that row, never from the request (S3). Last,
//!    in memory, the stream of that generation must still hold the bot
//!    assigned, so a request whose bot was released or taken over by a
//!    later stream while it waited never reaches the token service.
//! 2. **The session:** an online account's from [`MinecraftSessions`] with
//!    the request's `fresh`; an offline account's grant is its name.
//! 3. **At the moment of issue, fenced:** inside a write transaction that
//!    writes nothing (`BEGIN IMMEDIATE` holds every other writer off, the
//!    host's disable included), the pre-check again and the same account;
//!    then, with the transaction still open, the registry checks the stream
//!    of that generation still holds the bot assigned and queues the grant,
//!    in memory, without an await. The transaction is dropped after.
//!
//! # Denials (S60, U14)
//! Every denial this service makes itself is retryable: each races with
//! something that sets it right without a sticky state (the reconciler, its
//! stop of a bot that stopped qualifying, the sweep). The session-token
//! service's errors keep Phase 9's mapping ([`SessionTokenError::retryable`]),
//! except `OwnerDisabled`, which is retryable here (U14): an owner disabled
//! while a grant waits for its account's earlier flight is as passing as
//! one this service finds itself. The wire says only whether a denial is
//! retryable; why is in the server's log.
//!
//! # Logs (S60, R43)
//! Each grant is one `info` line (the user's change at the plan review):
//! its stream, its bot, the request's number, its kind, whether it was fresh
//! and its expiry. Each denial is one line at its level, with a fixed
//! reason and the error's text. Never a token, a name or a UUID.
//!
//! # No audit (S61)
//! A grant or a denial writes no audit entry: it changes no row, the
//! assignments' entries name the only agent that could receive a token, and
//! every new token's refresh is `account.refresh` already.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fleet_core::agent::{CertificateFacts, ConnectRefusal, admit};
use fleet_core::bot::BotAccount;
use fleet_core::id::BotId;
use fleet_core::mc::SessionCredentials;
use fleet_core::placement::{PlaceFacts, Unplaced, placeable};
use fleet_core::system::Clock;
use secrecy::SecretString;
use tracing::{Level, debug, error, info, warn};

use crate::app::session_tokens::{MinecraftSessions, SessionTokenError};
use crate::ports::bots::BotRecord;
use crate::ports::live_agents::{AnswerRefused, LiveAgents, SessionAnswer, SessionOutcome};
use crate::ports::msauth::MinecraftSession;
use crate::ports::session_grants::{GrantRequest, GrantStream, SessionGrants};
use crate::ports::store::{Store, StoreError};

/// The session grants; see the [module docs](self).
#[derive(Debug)]
pub struct SessionGrantService {
    clock: Arc<dyn Clock>,
    store: Arc<dyn Store>,
    live: Arc<dyn LiveAgents>,
    tokens: Arc<dyn MinecraftSessions>,
    allow_offline: bool,
}

/// Why this service itself denied a request (S60): every one is retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The stream's certificate is unknown, names another agent, or its
    /// agent is gone.
    UnknownCertificate,
    /// The stream's certificate doesn't admit its agent any more.
    NotAdmitted(ConnectRefusal),
    /// The bot is gone.
    NoBot,
    /// The bot's row names another agent, or none.
    AssignedElsewhere,
    /// The bot may not run.
    Unplaced(Unplaced),
    /// The bot's account changed while its session was minted.
    AccountChanged,
    /// The stream doesn't hold the bot assigned any more.
    NotHeld,
}

/// Why a request got no grant.
#[derive(Debug)]
enum Denial {
    /// This service's own refusal.
    Refused(Refusal),
    /// This service's own read failed.
    Store(StoreError),
    /// The session-token service's refusal.
    Tokens(SessionTokenError),
    /// The grant couldn't be written for the wire.
    Unsendable,
    /// The stream that asked ended: nobody is left to answer.
    Undelivered,
}

/// What a grant handed out, for its log line.
#[derive(Debug, Clone, Copy)]
enum Issued {
    /// An online account's session, expiring then.
    Online(DateTime<Utc>),
    /// An offline account's name.
    Offline,
}

impl SessionGrantService {
    /// The grants over `store`, checking with `clock`, answering through
    /// `live` with sessions from `tokens`, an offline account's bot only
    /// while `allow_offline`.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        store: Arc<dyn Store>,
        live: Arc<dyn LiveAgents>,
        tokens: Arc<dyn MinecraftSessions>,
        allow_offline: bool,
    ) -> Self {
        Self {
            clock,
            store,
            live,
            tokens,
            allow_offline,
        }
    }
}

impl SessionGrantService {
    /// Works out `request`'s grant from `stream` and queues it there; see
    /// the [module docs](self).
    async fn issue(&self, stream: GrantStream, request: GrantRequest) -> Result<Issued, Denial> {
        let record = self.check(stream, request.bot).await?;
        // The stream's own hold, right before the token service: a bot
        // released while its request waited never costs a refresh.
        if self
            .live
            .held(stream.agent, request.bot)
            .is_none_or(|held| held.generation != stream.generation)
        {
            return Err(Denial::Refused(Refusal::NotHeld));
        }
        let (credentials, issued) = match &record.account {
            BotAccount::Offline(name) => (
                SessionCredentials::Offline {
                    username: name.clone(),
                },
                Issued::Offline,
            ),
            BotAccount::Online(account) => {
                let session = self
                    .tokens
                    .session(*account, request.fresh)
                    .await
                    .map_err(Denial::Tokens)?;
                let expires_at = session.expires_at;
                (online(session), Issued::Online(expires_at))
            }
        };
        let answer = SessionAnswer {
            number: request.number,
            bot: request.bot,
            outcome: SessionOutcome::Grant(credentials),
        };
        // At the moment of issue (S59): while this transaction holds the
        // write lock, nothing commits, the host's disable included, so what
        // the check reads stays true until the grant is queued. It writes
        // nothing; dropping it rolls nothing back.
        let fence = self.store.write().await.map_err(Denial::Store)?;
        let again = self.check(stream, request.bot).await?;
        if again.account != record.account {
            return Err(Denial::Refused(Refusal::AccountChanged));
        }
        let sent = self
            .live
            .answer_session(stream.agent, stream.generation, answer);
        drop(fence);
        match sent {
            Ok(()) => Ok(issued),
            Err(AnswerRefused::NotHeld) => Err(Denial::Refused(Refusal::NotHeld)),
            Err(AnswerRefused::NotConnected | AnswerRefused::QueueFull) => Err(Denial::Undelivered),
            Err(AnswerRefused::Unsendable) => Err(Denial::Unsendable),
        }
    }

    /// The permission to grant `bot` a session on `stream`, read on the
    /// read connections (S59): the stream's own certificate admitting its
    /// agent, the bot's row naming that agent, and the bot allowed to run.
    /// The bot's record, whose account the session is for (S3).
    async fn check(&self, stream: GrantStream, bot: BotId) -> Result<BotRecord, Denial> {
        let now = self.clock.now();
        let agents = self.store.agents();
        let certificate = agents
            .cert(stream.fingerprint)
            .await
            .map_err(Denial::Store)?
            .filter(|certificate| certificate.agent == stream.agent)
            .ok_or(Denial::Refused(Refusal::UnknownCertificate))?;
        let agent = agents
            .find(stream.agent)
            .await
            .map_err(Denial::Store)?
            .ok_or(Denial::Refused(Refusal::UnknownCertificate))?;
        admit(
            CertificateFacts {
                revoked: certificate.revoked_at.is_some(),
                not_after: certificate.not_after,
                agent_enabled: agent.enabled,
            },
            now,
        )
        .map_err(|refusal| Denial::Refused(Refusal::NotAdmitted(refusal)))?;
        let record = self
            .store
            .bots()
            .record(bot)
            .await
            .map_err(Denial::Store)?
            .ok_or(Denial::Refused(Refusal::NoBot))?;
        if record.bot.agent != Some(stream.agent) {
            return Err(Denial::Refused(Refusal::AssignedElsewhere));
        }
        placeable(&PlaceFacts {
            has_server: record.bot.server.is_some(),
            desired: record.bot.desired,
            owner_enabled: record.owner_enabled,
            kind: record.kind,
            status: record.status,
            allow_offline: self.allow_offline,
        })
        .map_err(|why| Denial::Refused(Refusal::Unplaced(why)))?;
        Ok(record)
    }

    /// Denies `request` on `stream`, `retryable` or not.
    fn deny(&self, stream: GrantStream, request: GrantRequest, retryable: bool) {
        let answer = SessionAnswer {
            number: request.number,
            bot: request.bot,
            outcome: SessionOutcome::Denied { retryable },
        };
        if let Err(failure) = self
            .live
            .answer_session(stream.agent, stream.generation, answer)
        {
            debug!(
                request_id = %stream.request,
                agent_id = %stream.agent,
                bot_id = %request.bot,
                error = %failure,
                "a session request's denial wasn't queued"
            );
        }
    }
}

#[async_trait]
impl SessionGrants for SessionGrantService {
    async fn grant(&self, stream: GrantStream, request: GrantRequest) {
        match self.issue(stream, request).await {
            Ok(Issued::Online(expires_at)) => info!(
                request_id = %stream.request,
                agent_id = %stream.agent,
                bot_id = %request.bot,
                session_request = request.number.get(),
                kind = "online",
                fresh = request.fresh,
                expires_at = %expires_at,
                "a session was granted"
            ),
            Ok(Issued::Offline) => info!(
                request_id = %stream.request,
                agent_id = %stream.agent,
                bot_id = %request.bot,
                session_request = request.number.get(),
                kind = "offline",
                fresh = request.fresh,
                "a session was granted"
            ),
            Err(Denial::Undelivered) => debug!(
                request_id = %stream.request,
                agent_id = %stream.agent,
                bot_id = %request.bot,
                session_request = request.number.get(),
                "a session request's stream ended before its answer"
            ),
            Err(denial) => {
                let retryable = retryable(&denial);
                log_denial(stream, request, &denial, retryable);
                self.deny(stream, request, retryable);
            }
        }
    }
}

/// `session` as the agent gets it: its token moved into a `SecretString` of
/// exactly its length (S7), the original wiped when `session` drops.
fn online(session: MinecraftSession) -> SessionCredentials {
    SessionCredentials::Online {
        access_token: SecretString::from(session.token.expose_secret()),
        username: session.name,
        uuid: session.uuid.into(),
        expires_at: session.expires_at,
    }
}

/// Whether `denial` may pass if the agent asks again (S60, U14).
const fn retryable(denial: &Denial) -> bool {
    match denial {
        // An owner disabled while the grant waited for an earlier flight
        // passes as one the grant finds itself does (U14).
        Denial::Tokens(SessionTokenError::OwnerDisabled)
        | Denial::Refused(_)
        | Denial::Store(_)
        | Denial::Unsendable
        | Denial::Undelivered => true,
        Denial::Tokens(error) => error.retryable(),
    }
}

/// The level `denial`'s line is logged at (S60).
const fn level(denial: &Denial) -> Level {
    match denial {
        Denial::Refused(_)
        | Denial::Undelivered
        | Denial::Tokens(
            SessionTokenError::BackingOff { .. }
            | SessionTokenError::InFlight
            | SessionTokenError::ChangedMeanwhile
            | SessionTokenError::ShuttingDown
            | SessionTokenError::Interrupted,
        ) => Level::DEBUG,
        Denial::Store(StoreError::Busy | StoreError::Backend(_))
        | Denial::Tokens(
            SessionTokenError::Microsoft(_)
            | SessionTokenError::TimedOut
            | SessionTokenError::Seal(_)
            | SessionTokenError::Store(StoreError::Busy | StoreError::Backend(_)),
        ) => Level::WARN,
        Denial::Tokens(
            SessionTokenError::NotFound
            | SessionTokenError::Offline
            | SessionTokenError::NotActive { .. }
            | SessionTokenError::OwnerDisabled
            | SessionTokenError::Envelope(_)
            | SessionTokenError::StoredToken(_),
        ) => Level::INFO,
        Denial::Store(StoreError::Corrupt { .. } | StoreError::Unstorable { .. })
        | Denial::Unsendable
        | Denial::Tokens(
            SessionTokenError::Store(StoreError::Corrupt { .. } | StoreError::Unstorable { .. })
            | SessionTokenError::Audit(_),
        ) => Level::ERROR,
    }
}

/// `denial`'s fixed name, the log line's `reason`.
const fn reason(denial: &Denial) -> &'static str {
    match denial {
        Denial::Refused(Refusal::UnknownCertificate) => "unknown_certificate",
        Denial::Refused(Refusal::NotAdmitted(refusal)) => refusal.as_str(),
        Denial::Refused(Refusal::NoBot) => "no_bot",
        Denial::Refused(Refusal::AssignedElsewhere) => "assigned_elsewhere",
        Denial::Refused(Refusal::Unplaced(why)) => why.as_str(),
        Denial::Refused(Refusal::AccountChanged) => "account_changed",
        Denial::Refused(Refusal::NotHeld) => "not_held",
        Denial::Store(_) => "store",
        Denial::Tokens(_) => "session_token",
        Denial::Unsendable => "unsendable",
        Denial::Undelivered => "undelivered",
    }
}

/// Logs `denial` of `request` on `stream` at its level (S60): never a
/// token, a name or a UUID; an error's text names none.
fn log_denial(stream: GrantStream, request: GrantRequest, denial: &Denial, retryable: bool) {
    let detail = match denial {
        Denial::Tokens(error) => error.to_string(),
        Denial::Store(error) => error.to_string(),
        Denial::Refused(_) | Denial::Unsendable | Denial::Undelivered => reason(denial).to_owned(),
    };
    macro_rules! denied {
        ($level:ident) => {
            $level!(
                request_id = %stream.request,
                agent_id = %stream.agent,
                bot_id = %request.bot,
                session_request = request.number.get(),
                retryable,
                reason = reason(denial),
                error = %detail,
                "a session request was denied"
            )
        };
    }
    match level(denial) {
        Level::ERROR => denied!(error),
        Level::WARN => denied!(warn),
        Level::INFO => denied!(info),
        _ => denied!(debug),
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use fleet_core::account::AccountStatus;
    use fleet_core::audit::AuditError;
    use rstest::rstest;

    use super::*;
    use crate::ports::msauth::{Endpoint, Failure, Rule, TokenError};
    use crate::ports::vault::VaultError;

    fn corrupt() -> StoreError {
        StoreError::Corrupt {
            table: "bots",
            column: "desired_state",
            rowid: 1,
        }
    }

    #[rstest]
    #[case::backing_off(SessionTokenError::BackingOff { until: DateTime::UNIX_EPOCH }, true, Level::DEBUG)]
    #[case::in_flight(SessionTokenError::InFlight, true, Level::DEBUG)]
    #[case::changed_meanwhile(SessionTokenError::ChangedMeanwhile, true, Level::DEBUG)]
    #[case::shutting_down(SessionTokenError::ShuttingDown, true, Level::DEBUG)]
    #[case::interrupted(SessionTokenError::Interrupted, true, Level::DEBUG)]
    #[case::rate_limited(SessionTokenError::Microsoft(Failure::RateLimited), true, Level::WARN)]
    #[case::unexpected(
        SessionTokenError::Microsoft(Failure::Unexpected { endpoint: Endpoint::Refresh, rule: Rule::NotJson }),
        true,
        Level::WARN
    )]
    #[case::timed_out(SessionTokenError::TimedOut, true, Level::WARN)]
    #[case::seal(SessionTokenError::Seal(VaultError::TooLarge), true, Level::WARN)]
    #[case::busy(SessionTokenError::Store(StoreError::Busy), true, Level::WARN)]
    #[case::backend(SessionTokenError::Store(StoreError::Backend("x".into())), true, Level::WARN)]
    #[case::owner_disabled(SessionTokenError::OwnerDisabled, true, Level::INFO)]
    #[case::not_found(SessionTokenError::NotFound, false, Level::INFO)]
    #[case::offline(SessionTokenError::Offline, false, Level::INFO)]
    #[case::pending(SessionTokenError::NotActive { status: AccountStatus::Pending }, false, Level::INFO)]
    #[case::reauth(SessionTokenError::NotActive { status: AccountStatus::ReauthRequired }, false, Level::INFO)]
    #[case::envelope(SessionTokenError::Envelope(VaultError::Malformed), false, Level::INFO)]
    #[case::stored_token(SessionTokenError::StoredToken(TokenError::Length), false, Level::INFO)]
    #[case::corrupt(SessionTokenError::Store(corrupt()), false, Level::ERROR)]
    #[case::unstorable(
        SessionTokenError::Store(StoreError::Unstorable { table: "mc_accounts", column: "mc_name" }),
        false,
        Level::ERROR
    )]
    #[case::audit(SessionTokenError::Audit(AuditError::ActionShape), false, Level::ERROR)]
    fn a_session_token_error_keeps_its_mapping_but_a_disabled_owner_is_retryable(
        #[case] error: SessionTokenError,
        #[case] expected: bool,
        #[case] at: Level,
    ) {
        let denial = Denial::Tokens(error);

        assert_eq!(retryable(&denial), expected, "{denial:?}");
        assert_eq!(level(&denial), at, "{denial:?}");
        if let Denial::Tokens(error) = &denial
            && !matches!(error, SessionTokenError::OwnerDisabled)
        {
            assert_eq!(retryable(&denial), error.retryable(), "Phase 9's mapping");
        }
    }

    #[rstest]
    #[case::unknown_certificate(Denial::Refused(Refusal::UnknownCertificate), Level::DEBUG)]
    #[case::revoked(
        Denial::Refused(Refusal::NotAdmitted(ConnectRefusal::Revoked)),
        Level::DEBUG
    )]
    #[case::expired(
        Denial::Refused(Refusal::NotAdmitted(ConnectRefusal::Expired)),
        Level::DEBUG
    )]
    #[case::disabled(
        Denial::Refused(Refusal::NotAdmitted(ConnectRefusal::Disabled)),
        Level::DEBUG
    )]
    #[case::no_bot(Denial::Refused(Refusal::NoBot), Level::DEBUG)]
    #[case::elsewhere(Denial::Refused(Refusal::AssignedElsewhere), Level::DEBUG)]
    #[case::no_server(Denial::Refused(Refusal::Unplaced(Unplaced::NoServer)), Level::DEBUG)]
    #[case::stopped(Denial::Refused(Refusal::Unplaced(Unplaced::Stopped)), Level::DEBUG)]
    #[case::owner_disabled(
        Denial::Refused(Refusal::Unplaced(Unplaced::OwnerDisabled)),
        Level::DEBUG
    )]
    #[case::offline_off(
        Denial::Refused(Refusal::Unplaced(Unplaced::OfflineNotAllowed)),
        Level::DEBUG
    )]
    #[case::pending(Denial::Refused(Refusal::Unplaced(Unplaced::Pending)), Level::DEBUG)]
    #[case::reauth(
        Denial::Refused(Refusal::Unplaced(Unplaced::ReauthRequired)),
        Level::DEBUG
    )]
    #[case::refused(Denial::Refused(Refusal::Unplaced(Unplaced::Refused)), Level::DEBUG)]
    #[case::account_changed(Denial::Refused(Refusal::AccountChanged), Level::DEBUG)]
    #[case::not_held(Denial::Refused(Refusal::NotHeld), Level::DEBUG)]
    #[case::busy(Denial::Store(StoreError::Busy), Level::WARN)]
    #[case::backend(Denial::Store(StoreError::Backend("x".into())), Level::WARN)]
    #[case::corrupt(Denial::Store(corrupt()), Level::ERROR)]
    #[case::unsendable(Denial::Unsendable, Level::ERROR)]
    #[case::undelivered(Denial::Undelivered, Level::DEBUG)]
    fn every_denial_of_the_grants_own_is_retryable(#[case] denial: Denial, #[case] at: Level) {
        assert!(retryable(&denial), "{denial:?}");
        assert_eq!(level(&denial), at, "{denial:?}");
    }
}
