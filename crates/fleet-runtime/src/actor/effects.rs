//! How the actor applies an event: the transition, the snapshot and the
//! effects.

use core::num::NonZeroU32;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fleet_core::bot::{
    BotEvent, BotNotification, BotSnapshot, BotState, Effect, Transition, transition,
};
use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
use fleet_core::mc::{
    ConnectError, ConnectParams, MinecraftConnector, SessionCredentialProvider, SessionHandle as _,
    SessionRequest,
};
use fleet_core::resilience::CircuitBreaker;
use fleet_core::time;
use rand::rngs::StdRng;
use rand::{Rng as _, SeedableRng as _};
use tracing::{Instrument as _, debug, error, info, warn};

use super::BotActor;
use super::respawn::respawn;
use super::session::Session;
use crate::event::{FleetEvent, FleetEventKind};
use crate::mode::ModeRunner;

impl<C: MinecraftConnector, P: SessionCredentialProvider> BotActor<C, P> {
    /// Applies `event`, then every event its effects produce, in order.
    pub(super) async fn feed(&mut self, event: BotEvent) {
        self.pending.push_back(event);
        self.run_pending().await;
    }

    /// Applies the events that effects produced, in order.
    async fn run_pending(&mut self) {
        while let Some(event) = self.pending.pop_front() {
            self.step(event).await;
        }
    }

    /// Takes on the starting point (ADR-0013): its state, published only if
    /// it differs from the last published one, then its effects and the
    /// events they produce. `self.state` still holds the last published
    /// state here.
    pub(super) async fn begin(&mut self, start: Transition) {
        let now = self.clock.now();
        let changed = start.state != self.state;
        self.enter(start, changed, false, now).await;
        self.run_pending().await;
    }

    /// Applies one event: the transition, then its effects, in order.
    async fn step(&mut self, event: BotEvent) {
        let now = self.clock.now();
        let old = self.state;
        let ended_on_its_own = matches!(old, BotState::Connecting { .. } | BotState::Online { .. })
            && matches!(
                event,
                BotEvent::Disconnected(_)
                    | BotEvent::ConnectFailed(_)
                    | BotEvent::WatchdogTimeout
                    | BotEvent::SessionClosed
            );
        let transition = transition(&old, event, now, &self.rules);
        let changed = transition.state != old;
        self.enter(transition, changed, ended_on_its_own, now).await;
    }

    /// Takes on `transition` at `now`: its state, published if `changed`,
    /// then its effects, in order.
    async fn enter(
        &mut self,
        transition: Transition,
        changed: bool,
        ended_on_its_own: bool,
        now: DateTime<Utc>,
    ) {
        self.state = transition.state;
        if changed {
            self.changed(now, ended_on_its_own);
        }
        for effect in transition.effects {
            self.execute(effect, now).await;
        }
        // Credentials live only from `SessionReady` until the connect that
        // takes them, in the same step.
        self.held_credentials = None;
    }

    /// The state changed at `now`: publishes the snapshot and drops the
    /// in-flight work of the state the bot left (ADR-0010).
    fn changed(&mut self, now: DateTime<Utc>, ended_on_its_own: bool) {
        let snapshot = BotSnapshot {
            bot_id: self.spec.id,
            state: self.state,
            since: now,
            last_disconnect: self.last_disconnect.clone(),
        };
        self.snapshot.send_replace(snapshot.clone());
        info!(state = ?self.state, "the bot's state changed");
        if ended_on_its_own
            && matches!(
                self.state,
                BotState::Backoff { .. } | BotState::AwaitingSession { .. }
            )
        {
            self.log_session_end();
        }
        self.publish_at(now, FleetEventKind::StateChanged(snapshot));
        if !matches!(self.state, BotState::AwaitingSession { .. }) {
            self.session_request = None;
        }
        if !matches!(self.state, BotState::Backoff { .. }) {
            self.retry = None;
        }
        if matches!(self.state, BotState::Online { .. }) {
            // The watchdog checks only while the bot is Online (P4.6).
            self.arm_watchdog();
        } else {
            self.respawn = None;
            self.watchdog = None;
        }
    }

    /// Logs a session that ended on its own and is retried, at `warn`: a
    /// recovered fault. A kick's text comes from the server, so it goes only
    /// to `debug`.
    fn log_session_end(&self) {
        let Some(reason) = &self.last_disconnect else {
            return;
        };
        match reason {
            DisconnectReason::Kicked(kick) => warn!(
                reason = "kicked",
                key = kick.key().map_or("", |key| key.as_str()),
                state = ?self.state,
                "the session ended; the bot connects again"
            ),
            other => warn!(
                reason = ?other,
                state = ?self.state,
                "the session ended; the bot connects again"
            ),
        }
        debug!(?reason, "the full reason for the session's end");
    }

    /// Executes one effect at `now`.
    async fn execute(&mut self, effect: Effect, now: DateTime<Utc>) {
        match effect {
            Effect::RequestSession { fresh } => self.request_session(fresh),
            Effect::Connect => self.connect().await,
            Effect::Disconnect => self.disconnect(),
            Effect::ScheduleRetry { attempt } => self.schedule_retry(attempt, now),
            Effect::RecordFailure => self.breaker_effect(|breaker| breaker.record_failure(now)),
            // A closed breaker with no failures is what a new one is, so a
            // reset is a recorded success.
            Effect::RecordSuccess | Effect::ResetBreaker => {
                self.breaker_effect(CircuitBreaker::record_success);
            }
            Effect::StartMode => self.start_mode(),
            Effect::StopMode => self.stop_mode(),
            Effect::Respawn => self.start_respawn(),
            Effect::Notify(notification) => self.notify(notification, now),
        }
    }

    /// Asks for session credentials, bounded by the session-request timeout.
    /// The answer, or the timeout, comes back as exactly one input.
    fn request_session(&mut self, fresh: bool) {
        let provider = Arc::clone(&self.credentials);
        let request = SessionRequest {
            bot_id: self.spec.id,
            account: self.spec.account.clone(),
            fresh,
        };
        let limit = self.config.session_request_timeout;
        self.session_request = Some(Box::pin(async move {
            tokio::time::timeout(limit, provider.session(request)).await
        }));
    }

    /// Starts a session with the credentials from `SessionReady`.
    pub(super) async fn connect(&mut self) {
        let Some(credentials) = self.held_credentials.take() else {
            // `Connect` only follows `SessionReady`, which comes with
            // credentials; this is a bug, so it fails the attempt.
            error!("there are no session credentials to connect with");
            self.connect_failed(ConnectFailure::Other);
            return;
        };
        let params = ConnectParams {
            bot_id: self.spec.id,
            server: self.spec.server.clone(),
            credentials,
            connect_timeout: self.config.connect_timeout,
        };
        let connector = Arc::clone(&self.connector);
        // `connect` resolves once the session has started (port contract),
        // long before the connect timeout; the bound keeps a connector that
        // hangs here from freezing the actor.
        let started =
            tokio::time::timeout(self.config.connect_timeout, connector.connect(params)).await;
        match started {
            Ok(Ok((handle, events))) => {
                self.generation = self.generation.saturating_add(1);
                self.session = Some(Session {
                    handle,
                    generation: self.generation,
                    token: self.run_token.child_token(),
                    runner: None,
                });
                self.session_events = Some(events);
            }
            Ok(Err(ConnectError::HostUnavailable)) => {
                self.connect_failed(ConnectFailure::HostUnavailable);
            }
            Err(_) => self.connect_failed(ConnectFailure::TimedOut),
        }
    }

    fn connect_failed(&mut self, failure: ConnectFailure) {
        self.last_disconnect = Some(DisconnectReason::ConnectFailed { failure });
        self.pending.push_back(BotEvent::ConnectFailed(failure));
    }

    /// Tears the current session down off the actor's loop. Its events are
    /// never read again, and the teardown reports its generation when it's
    /// done.
    fn disconnect(&mut self) {
        self.session_events = None;
        self.respawn = None;
        self.watchdog = None;
        if let Some(session) = self.session.take() {
            self.close_session(session);
        }
    }

    /// Stops what runs in `session` and spawns its teardown.
    pub(super) fn close_session(&mut self, session: Session<C::Session>) {
        if let Some(runner) = &session.runner {
            runner.cancel();
        }
        self.queue.close();
        session.token.cancel();
        let Session {
            handle, generation, ..
        } = session;
        // Unlike the actor's other tasks, the teardown takes no token: it
        // must run to its end, and the port guarantees that `disconnect()`
        // finishes, since fleet-mc abandons a hung host thread after its own
        // timeouts (ADR-0010). The JoinSet owns it, so an aborted actor
        // aborts it too, which drops the handle and ends the host thread
        // (ADR-0011, ADR-0013).
        self.teardowns.spawn(
            async move {
                handle.disconnect().await;
                generation
            }
            .instrument(self.span.clone()),
        );
    }

    /// Waits `RetryPolicy::delay(attempt)`, or until the breaker's cool-down
    /// ends if that's later, then sends `RetryDue` (ADR-0010).
    fn schedule_retry(&mut self, attempt: NonZeroU32, now: DateTime<Utc>) {
        let delay = self.rules.retry.delay(attempt, &mut self.rng);
        let mut due = time::add(now, delay);
        if let Some(until) = self.breaker.open_until() {
            due = due.max(until);
        }
        if let Some(deadline) = self.clock.deadline(due) {
            self.retry = Some(Box::pin(tokio::time::sleep_until(deadline)));
        } else {
            error!(?due, "the next attempt is too far ahead to schedule");
        }
    }

    /// Runs a breaker effect, then publishes a copy of the breaker, so a
    /// restarted actor carries on with it (P4.7).
    fn breaker_effect(&mut self, effect: impl FnOnce(&mut CircuitBreaker)) {
        effect(&mut self.breaker);
        self.breaker_copy.send_replace(self.breaker.clone());
    }

    /// Opens the chat queue, then starts the mode runner, so mode chat never
    /// meets a closed queue (ADR-0013).
    fn start_mode(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let (delivery, chat) = self.queue.open(
            session.handle.clone(),
            self.events.clone(),
            self.clock,
            session.token.child_token(),
        );
        self.deliveries
            .spawn(delivery.run().instrument(self.span.clone()));
        let runner_token = session.token.child_token();
        let runner = ModeRunner::new(
            session.handle.clone(),
            chat,
            self.clock,
            StdRng::seed_from_u64(self.rng.next_u64()),
            self.mode.subscribe(),
        );
        self.runners.spawn(
            runner
                .run(runner_token.clone())
                .instrument(self.span.clone()),
        );
        session.runner = Some(runner_token);
    }

    /// Cancels the mode runner, then closes the chat queue (ADR-0013).
    fn stop_mode(&mut self) {
        if let Some(runner) = self
            .session
            .as_mut()
            .and_then(|session| session.runner.take())
        {
            runner.cancel();
        }
        self.queue.close();
    }

    /// Starts the respawn retry for the current session.
    fn start_respawn(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        self.respawn = Some(Box::pin(respawn(
            session.handle.clone(),
            self.config.respawn_interval,
            self.config.respawn_attempts,
        )));
    }

    /// Alerts a human: logs it and publishes it right after the state change
    /// it belongs to.
    fn notify(&self, notification: BotNotification, now: DateTime<Utc>) {
        match notification {
            BotNotification::Paused { reason } => {
                warn!(?reason, "the bot paused; Resume connects it again");
            }
            BotNotification::Failed { reason } => {
                error!(?reason, "the bot failed; Reset connects it again");
            }
        }
        self.publish_at(now, FleetEventKind::Alert(notification));
    }

    /// Publishes `kind` for this bot, stamped now.
    pub(super) fn publish(&self, kind: FleetEventKind) {
        self.publish_at(self.clock.now(), kind);
    }

    fn publish_at(&self, at: DateTime<Utc>, kind: FleetEventKind) {
        let event = FleetEvent {
            bot_id: self.spec.id,
            at,
            kind,
        };
        // An error only means nobody subscribes right now, so nobody misses
        // the event.
        let _ = self.events.send(event);
    }
}
