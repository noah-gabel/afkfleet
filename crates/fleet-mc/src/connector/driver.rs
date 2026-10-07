//! A session's driver: the long-lived job on its host thread that starts
//! azalea, feeds the event bridge and tears the session down (ADR-0008 §1,
//! §5, §10; ADR-0011).
//!
//! It holds the session's only counting [`EventSink`], and no handle of its
//! host thread: if the thread goes away, the sink drops and the session ends
//! as crashed. Every wait before the bot has joined also watches the stop
//! signal, the connect deadline and the ECS runner, so nothing before `Joined`
//! waits unbounded.

use core::any::Any;
use core::fmt;
use core::future::{Future, pending};
use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use azalea::Client;
use azalea::Event;
use azalea::account::Account;
use azalea::app::{App, AppExit};
use azalea::ecs::entity::Entity;
use azalea::join::{ConnectOpts, StartJoinServerEvent};
use azalea::protocol::address::{ResolvedAddr, ServerAddr};
use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
use fleet_core::id::BotId;
use fleet_core::value::{Host, ServerAddress};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{self, Instant};
use tracing::{Instrument as _, debug, info_span, warn};

#[cfg(feature = "fault-injection")]
use super::AppHook;
use super::app::{build_app, event_channel};
use crate::account::AuthReports;
use crate::events::{EventSink, LivenessStamps, Phase, Terminal};
use crate::signing::SigningPlugin;

/// Where a session's `Client` lives while the bot is in a world. Only the
/// host thread clones the `Client` out of it, so no handle outside the thread
/// keeps the World alive; the driver empties it when it ends.
#[derive(Default)]
pub(super) struct ClientSlot(Mutex<Option<Client>>);

impl fmt::Debug for ClientSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientSlot")
            .field("set", &self.lock().is_some())
            .finish()
    }
}

impl ClientSlot {
    fn set(&self, client: Client) {
        *self.lock() = Some(client);
    }

    /// A clone of the `Client`, if the session has one.
    pub(super) fn get(&self) -> Option<Client> {
        self.lock().clone()
    }

    /// Empties the slot. The `Client` is dropped after the lock is released.
    fn clear(&self) {
        let client = self.lock().take();
        drop(client);
    }

    /// Locks the slot, poison-tolerantly: it holds a plain value.
    fn lock(&self) -> MutexGuard<'_, Option<Client>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Empties the slot when the driver ends, however it ends.
struct ClearOnDrop(Arc<ClientSlot>);

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// The ECS Worlds of a connector's sessions, as `Weak`s, for
/// [`AzaleaConnector::live_worlds`](super::AzaleaConnector::live_worlds).
#[derive(Debug, Default)]
pub(super) struct Worlds(Mutex<Vec<Weak<dyn Any + Send + Sync>>>);

impl Worlds {
    /// Starts tracking `world`, and forgets the ones already freed.
    fn track(&self, world: Weak<impl Any + Send + Sync>) {
        let world: Weak<dyn Any + Send + Sync> = world;
        let mut worlds = self.lock();
        worlds.retain(|world| world.strong_count() > 0);
        worlds.push(world);
    }

    /// How many tracked Worlds are still alive.
    pub(super) fn live(&self) -> usize {
        let mut worlds = self.lock();
        worlds.retain(|world| world.strong_count() > 0);
        worlds.len()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Weak<dyn Any + Send + Sync>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything a session's driver needs, moved onto its host thread.
pub(super) struct Driver {
    pub(super) bot_id: BotId,
    pub(super) server: ServerAddress,
    /// When `connect()` was called. The connect timeout runs from here, in
    /// std time, so a paused clock on the caller's runtime can't move it.
    pub(super) started: std::time::Instant,
    pub(super) connect_timeout: Duration,
    pub(super) account: Account,
    pub(super) reports: AuthReports,
    pub(super) sink: EventSink,
    pub(super) stamps: Arc<LivenessStamps>,
    /// Publishes where the session's chat signing is.
    pub(super) signing: SigningPlugin,
    pub(super) slot: Arc<ClientSlot>,
    pub(super) worlds: Arc<Worlds>,
    /// Sent, or closed, when the session is to be torn down.
    pub(super) stop: oneshot::Receiver<()>,
    /// Dropped when the driver ends, which is how the session learns it.
    pub(super) ended: watch::Sender<()>,
    pub(super) app_exit_timeout: Duration,
    #[cfg(feature = "fault-injection")]
    pub(super) hook: Option<AppHook>,
}

impl Driver {
    /// Drives the session from connecting to its teardown, in a span named
    /// after the bot.
    pub(super) async fn run(self) {
        let span = info_span!("mc_session", bot_id = %self.bot_id);
        self.drive().instrument(span).await;
    }

    /// The session's lifecycle, in ADR-0008's order. The ECS handle stays in
    /// this function: its lock type is `parking_lot`'s, which azalea doesn't
    /// re-export, so everything else lives in the helpers below.
    async fn drive(self) {
        let Self {
            bot_id,
            server,
            started,
            connect_timeout,
            account,
            reports,
            sink,
            stamps,
            signing,
            slot,
            worlds,
            mut stop,
            ended,
            app_exit_timeout,
            #[cfg(feature = "fault-injection")]
            hook,
        } = self;
        // Dropped last, when the driver ends or the host thread drops it.
        let _ended = ended;
        let _clear = ClearOnDrop(Arc::clone(&slot));
        let deadline = connect_deadline(started, connect_timeout);
        let mut reports = Some(reports);
        let Some(address) = resolve(&server, deadline, &mut stop, &sink).await else {
            return;
        };

        // The bot's events: azalea gets the sender the moment it spawns the
        // bot, so nothing it reports in that same frame is lost (found in CI
        // on Linux, where a refused connect fails within that frame).
        let (events_tx, mut events) = event_channel();
        #[cfg(feature = "fault-injection")]
        let mut app = session_app(stamps, events_tx, signing, bot_id, hook.as_ref());
        #[cfg(not(feature = "fault-injection"))]
        let mut app = session_app(stamps, events_tx, signing, bot_id);
        // Variant C (ADR-0008 §1). The runner is a task of the host thread's
        // LocalSet, so closing the thread drops it and with it the World.
        let (ecs, start_running_systems, app_exit) = azalea::start_ecs_runner(app.main_mut());
        start_running_systems();
        drop(app);
        worlds.track(Arc::downgrade(&ecs));
        let mut app_exit = Some(app_exit);

        #[expect(
            clippy::disallowed_methods,
            reason = "azalea's join callback takes an unbounded sender; it carries one entity (ADR-0011, approved)"
        )]
        let (callback_tx, mut callback_rx) = mpsc::unbounded_channel();
        ecs.write().write_message(StartJoinServerEvent {
            account,
            connect_opts: ConnectOpts {
                address,
                server_proxy: None,
                sessionserver_proxy: None,
            },
            start_join_callback_tx: Some(callback_tx),
        });
        // The callback comes right after the entity is spawned, before the
        // connect; the deadline, the stop signal and the runner bound it.
        let joined = before_join(callback_rx.recv(), deadline, &mut stop, &mut app_exit).await;

        if let Some(entity) = joining_entity(joined, &sink) {
            slot.set(Client::new(entity, Arc::clone(&ecs)));
            // After `Joined`, nothing here waits on the network but the event
            // stream; the watchdog bounds a session that goes quiet (P4.6).
            let end = session_loop(
                &sink,
                &mut events,
                &mut reports,
                &mut app_exit,
                &mut stop,
                deadline,
            )
            .await;
            debug!(?end, "the session loop ended");
        }

        // The teardown (ADR-0008 §10): exit, wait for the runner, then drop
        // the `Client` and the event receiver. Closing the host thread, the
        // last step, is the session handle's.
        ecs.write().write_message(AppExit::Success);
        wait_for_runner(app_exit.take(), app_exit_timeout).await;
        slot.clear();
        drop(events);
        debug!("the session's driver ended");
    }
}

/// Resolves `server` before the deadline. A failure is reported to the
/// bridge, and then there's no address.
async fn resolve(
    server: &ServerAddress,
    deadline: Option<Instant>,
    stop: &mut oneshot::Receiver<()>,
    sink: &EventSink,
) -> Option<ResolvedAddr> {
    // A network call; the deadline bounds it. azalea's resolver has no
    // timeout of its own.
    match before_join(
        ResolvedAddr::new(server_addr(server)),
        deadline,
        stop,
        &mut None,
    )
    .await
    {
        Ok(Ok(address)) => Some(address),
        Ok(Err(error)) => {
            debug!(%error, "the server address couldn't be resolved");
            sink.terminate(Terminal::ConnectionFailed(ConnectFailure::Unresolvable));
            None
        }
        Err(interrupted) => {
            end_early(sink, interrupted);
            None
        }
    }
}

/// The session's App, with the test's hook if one is set.
#[cfg(feature = "fault-injection")]
fn session_app(
    stamps: Arc<LivenessStamps>,
    events: mpsc::UnboundedSender<Event>,
    signing: SigningPlugin,
    bot_id: BotId,
    hook: Option<&AppHook>,
) -> App {
    build_app(stamps, events, signing, |app| {
        if let Some(hook) = hook {
            hook(bot_id, app);
        }
    })
}

/// The session's App.
#[cfg(not(feature = "fault-injection"))]
fn session_app(
    stamps: Arc<LivenessStamps>,
    events: mpsc::UnboundedSender<Event>,
    signing: SigningPlugin,
    _bot_id: BotId,
) -> App {
    build_app(stamps, events, signing, |_| {})
}

/// The bot's entity from the join callback, or `None` after reporting why
/// there is none.
fn joining_entity(joined: Result<Option<Entity>, Interrupted>, sink: &EventSink) -> Option<Entity> {
    match joined {
        Ok(Some(entity)) => Some(entity),
        Ok(None) => {
            warn!("azalea dropped the join callback, so the session ended as crashed");
            sink.terminate(Terminal::Disconnected(DisconnectReason::SessionCrashed));
            None
        }
        Err(interrupted) => {
            end_early(sink, interrupted);
            None
        }
    }
}

/// Waits for azalea's runner to end after `exit()`, up to `timeout`. A
/// runner that hangs is left to the host thread's shutdown, which drops it.
async fn wait_for_runner(app_exit: Option<oneshot::Receiver<AppExit>>, timeout: Duration) {
    let Some(app_exit) = app_exit else {
        return;
    };
    if time::timeout(timeout, app_exit).await.is_err() {
        warn!("azalea's runner didn't end in time after exit; the host thread's shutdown drops it");
    }
}

/// When connecting must have finished: `connect_timeout` after `started`, on
/// tokio's clock. `None` if that's too far to represent, which only a
/// nonsensical timeout reaches.
fn connect_deadline(started: std::time::Instant, connect_timeout: Duration) -> Option<Instant> {
    started.checked_add(connect_timeout).map(Instant::from_std)
}

/// azalea's address for `server`, built from its parts. An IP address goes in
/// bare (an IPv6 one without brackets), so azalea doesn't look it up.
fn server_addr(server: &ServerAddress) -> ServerAddr {
    let host = match server.host() {
        Host::Domain(domain) => domain.clone(),
        Host::Ip(ip) => ip.to_string(),
    };
    ServerAddr {
        host,
        port: server.port(),
    }
}

/// What ended a wait before the bot joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Interrupted {
    /// The session is being torn down.
    Stopped,
    /// The connect deadline passed.
    TimedOut,
    /// azalea's ECS runner ended without being asked to.
    Crashed,
}

/// Reports an early end to the bridge. A stop reports nothing: the session's
/// handle closed the bridge, or every handle is gone.
fn end_early(sink: &EventSink, interrupted: Interrupted) {
    let terminal = match interrupted {
        Interrupted::Stopped => return,
        Interrupted::TimedOut => Terminal::ConnectionFailed(ConnectFailure::TimedOut),
        Interrupted::Crashed => Terminal::Disconnected(DisconnectReason::SessionCrashed),
    };
    sink.terminate(terminal);
}

/// Waits for `work`, unless the session is stopped, the connect deadline
/// passes or azalea's runner ends first. They're checked in that order.
pub(super) async fn before_join<F: Future>(
    work: F,
    deadline: Option<Instant>,
    stop: &mut oneshot::Receiver<()>,
    app_exit: &mut Option<oneshot::Receiver<AppExit>>,
) -> Result<F::Output, Interrupted> {
    tokio::select! {
        biased;
        // Sent by `disconnect()`, or closed once every handle is gone.
        _ = stop => Err(Interrupted::Stopped),
        () = until(deadline) => Err(Interrupted::TimedOut),
        exit = runner_end(app_exit) => {
            *app_exit = None;
            warn!(?exit, "azalea's runner ended before the bot joined, so the session ended as crashed");
            Err(Interrupted::Crashed)
        }
        output = work => Ok(output),
    }
}

/// Where a session's azalea events come from: azalea's receiver, or a test's.
pub(super) trait EventSource {
    /// The next event, or `None` once no more can come.
    fn recv(&mut self) -> impl Future<Output = Option<Event>>;
}

impl EventSource for mpsc::UnboundedReceiver<Event> {
    fn recv(&mut self) -> impl Future<Output = Option<Event>> {
        mpsc::UnboundedReceiver::recv(self)
    }
}

/// Why [`session_loop`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopEnd {
    /// The stop signal came.
    Stopped,
    /// The session ended, or its bridge was closed.
    Ended,
}

/// Feeds the bridge until the session ends or is stopped.
///
/// The branches are checked in a fixed order: the stop signal, the account's
/// auth reports (so a refused join wins over the kick that follows it), the
/// connect deadline until the bot has joined (so a server that keeps sending
/// events can't starve it), the ECS runner, then the events. Every branch is
/// cancel-safe: a oneshot, `mpsc::recv` and a sleep lose nothing when another
/// branch wins.
pub(super) async fn session_loop(
    sink: &EventSink,
    events: &mut impl EventSource,
    reports: &mut Option<AuthReports>,
    app_exit: &mut Option<oneshot::Receiver<AppExit>>,
    stop: &mut oneshot::Receiver<()>,
    deadline: Option<Instant>,
) -> LoopEnd {
    loop {
        let connecting = sink.phase() == Phase::Starting;
        tokio::select! {
            biased;
            // Sent by `disconnect()`, or closed once every handle is gone.
            _ = &mut *stop => return LoopEnd::Stopped,
            report = next_report(reports) => match report {
                Some(reason) => {
                    sink.terminate(Terminal::Disconnected(reason));
                }
                // An offline account's channel is closed from the start.
                None => *reports = None,
            },
            // Ahead of the events: a server that keeps sending them before
            // the join must not starve the connect timeout.
            () = until(deadline), if connecting => {
                sink.terminate(Terminal::ConnectionFailed(ConnectFailure::TimedOut));
            }
            exit = runner_end(app_exit) => {
                *app_exit = None;
                warn!(?exit, "azalea's runner ended without being asked to, so the session ended as crashed");
                sink.terminate(Terminal::Disconnected(DisconnectReason::SessionCrashed));
            }
            event = events.recv() => {
                if let Some(event) = event {
                    sink.forward(&event);
                } else {
                    warn!("azalea's event channel closed, so the session ended as crashed");
                    sink.terminate(Terminal::Disconnected(DisconnectReason::SessionCrashed));
                }
            }
        }
        if matches!(sink.phase(), Phase::Ended | Phase::Closed) {
            return LoopEnd::Ended;
        }
    }
}

/// The next auth report; pending forever once the channel is gone.
async fn next_report(reports: &mut Option<AuthReports>) -> Option<DisconnectReason> {
    match reports {
        Some(reports) => reports.recv().await,
        None => pending().await,
    }
}

/// How azalea's runner ended; pending forever once that's known. A oneshot
/// receiver must not be polled again after it finished, so the caller sets it
/// to `None` then.
async fn runner_end(
    app_exit: &mut Option<oneshot::Receiver<AppExit>>,
) -> Result<AppExit, oneshot::error::RecvError> {
    match app_exit {
        Some(app_exit) => app_exit.await,
        None => pending().await,
    }
}

/// Resolves at `deadline`, or never without one.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => time::sleep_until(deadline).await,
        None => pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroUsize;
    use core::task::Poll;

    use fleet_core::mc::SessionEvent;
    use rstest::rstest;

    use crate::events::{EventCounters, McEvents, bridge};

    const SECOND: Duration = Duration::from_secs(1);

    impl EventSource for mpsc::Receiver<Event> {
        fn recv(&mut self) -> impl Future<Output = Option<Event>> {
            mpsc::Receiver::recv(self)
        }
    }

    /// An event source that always has an event ready, like azalea's channel
    /// with a backlog, until it has served `limit` of them.
    struct Flood {
        served: usize,
        limit: usize,
    }

    impl EventSource for Flood {
        fn recv(&mut self) -> impl Future<Output = Option<Event>> {
            let ready = self.served < self.limit;
            if ready {
                self.served += 1;
            }
            async move {
                if ready {
                    Some(Event::Tick)
                } else {
                    pending().await
                }
            }
        }
    }

    fn open() -> (EventSink, McEvents) {
        bridge(
            "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap(),
            NonZeroUsize::new(8).unwrap(),
            Arc::new(EventCounters::default()),
            Arc::new(LivenessStamps::new(std::time::Instant::now())),
        )
    }

    /// Everything the bridge holds right now, without waiting.
    async fn drain(events: &mut McEvents) -> Vec<SessionEvent> {
        use fleet_core::mc::SessionEvents as _;
        let mut out = Vec::new();
        loop {
            let mut next = core::pin::pin!(events.next());
            match core::future::poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx))).await {
                Poll::Ready(Some(event)) => out.push(event),
                Poll::Ready(None) | Poll::Pending => return out,
            }
        }
    }

    fn in_a_second() -> Instant {
        Instant::now() + SECOND
    }

    // --- Waiting before the join ---

    #[tokio::test(start_paused = true)]
    async fn before_join_returns_the_work_when_it_finishes_first() {
        let (_stop_tx, mut stop) = oneshot::channel();

        let done = before_join(async { 7 }, Some(in_a_second()), &mut stop, &mut None).await;

        assert_eq!(done, Ok(7));
    }

    #[tokio::test(start_paused = true)]
    async fn before_join_times_out_at_the_deadline() {
        let (_stop_tx, mut stop) = oneshot::channel();
        let started = Instant::now();

        let done = before_join(pending::<()>(), Some(in_a_second()), &mut stop, &mut None).await;

        assert_eq!(done, Err(Interrupted::TimedOut));
        assert_eq!(started.elapsed(), SECOND);
    }

    #[rstest]
    #[case::sent(true)]
    #[case::closed(false)]
    #[tokio::test(start_paused = true)]
    async fn before_join_ends_on_the_stop_signal(#[case] sent: bool) {
        let (stop_tx, mut stop) = oneshot::channel();
        if sent {
            stop_tx.send(()).unwrap();
        } else {
            drop(stop_tx);
        }

        let done = before_join(pending::<()>(), Some(in_a_second()), &mut stop, &mut None).await;

        assert_eq!(done, Err(Interrupted::Stopped));
    }

    #[tokio::test(start_paused = true)]
    async fn before_join_ends_when_the_runner_dies() {
        let (_stop_tx, mut stop) = oneshot::channel();
        let (exit_tx, exit_rx) = oneshot::channel::<AppExit>();
        let mut app_exit = Some(exit_rx);
        drop(exit_tx);

        let done = before_join(
            pending::<()>(),
            Some(in_a_second()),
            &mut stop,
            &mut app_exit,
        )
        .await;

        assert_eq!(done, Err(Interrupted::Crashed));
        assert!(
            app_exit.is_none(),
            "a finished receiver must not be polled again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn before_join_without_a_deadline_waits_for_the_work() {
        let (_stop_tx, mut stop) = oneshot::channel();

        let done = before_join(
            async {
                time::sleep(Duration::from_secs(3600)).await;
                "done"
            },
            None,
            &mut stop,
            &mut None,
        )
        .await;

        assert_eq!(done, Ok("done"));
    }

    // --- The session loop ---

    /// Runs the loop with `events` and the given inputs until it returns.
    struct Inputs {
        reports: Option<AuthReports>,
        app_exit: Option<oneshot::Receiver<AppExit>>,
        stop: oneshot::Receiver<()>,
        deadline: Option<Instant>,
    }

    struct Senders {
        reports: mpsc::Sender<DisconnectReason>,
        app_exit: oneshot::Sender<AppExit>,
        stop: oneshot::Sender<()>,
    }

    fn inputs() -> (Inputs, Senders) {
        let (reports_tx, reports_rx) = mpsc::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        (
            Inputs {
                reports: Some(reports_rx),
                app_exit: Some(exit_rx),
                stop: stop_rx,
                deadline: Some(in_a_second()),
            },
            Senders {
                reports: reports_tx,
                app_exit: exit_tx,
                stop: stop_tx,
            },
        )
    }

    async fn run_loop(
        sink: &EventSink,
        events: &mut impl EventSource,
        inputs: &mut Inputs,
    ) -> LoopEnd {
        session_loop(
            sink,
            events,
            &mut inputs.reports,
            &mut inputs.app_exit,
            &mut inputs.stop,
            inputs.deadline,
        )
        .await
    }

    #[tokio::test(start_paused = true)]
    async fn a_flood_of_events_before_the_join_still_times_out_at_the_deadline() {
        const LIMIT: usize = 100_000;
        let (sink, mut events) = open();
        let (mut inputs, _senders) = inputs();
        let mut flood = Flood {
            served: 0,
            limit: LIMIT,
        };
        // The deadline passes while events keep coming.
        time::advance(2 * SECOND).await;

        let end = run_loop(&sink, &mut flood, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert!(
            flood.served < LIMIT,
            "the connect timeout only fired once the flood of events had ended"
        );
        assert_eq!(
            drain(&mut events).await,
            [SessionEvent::ConnectionFailed(ConnectFailure::TimedOut)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_server_times_out_at_the_deadline() {
        let (sink, mut events) = open();
        let (mut inputs, _senders) = inputs();
        let (_tx, mut rx) = mpsc::channel::<Event>(1);
        let started = Instant::now();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert_eq!(started.elapsed(), SECOND);
        assert_eq!(
            drain(&mut events).await,
            [SessionEvent::ConnectionFailed(ConnectFailure::TimedOut)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn after_the_join_the_deadline_no_longer_fires() {
        let (sink, mut events) = open();
        let (mut inputs, senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        tx.send(Event::Spawn).await.unwrap();

        let looped = run_loop(&sink, &mut rx, &mut inputs);
        let outcome = time::timeout(10 * SECOND, looped).await;

        assert!(outcome.is_err(), "the loop ended after the bot had joined");
        assert_eq!(drain(&mut events).await, [SessionEvent::Joined]);
        drop(senders);
    }

    #[tokio::test(start_paused = true)]
    async fn an_auth_report_wins_over_a_kick_that_arrives_with_it() {
        let (sink, mut events) = open();
        let (mut inputs, senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        senders
            .reports
            .send(DisconnectReason::AuthRejected)
            .await
            .unwrap();
        tx.send(Event::Disconnect(None)).await.unwrap();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert_eq!(
            drain(&mut events).await,
            [SessionEvent::Disconnected(DisconnectReason::AuthRejected)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_report_channel_is_left_alone() {
        let (sink, mut events) = open();
        let (mut inputs, senders) = inputs();
        drop(senders.reports);
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        tx.send(Event::Spawn).await.unwrap();
        tx.send(Event::Disconnect(None)).await.unwrap();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert!(inputs.reports.is_none());
        assert_eq!(
            drain(&mut events).await,
            [
                SessionEvent::Joined,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed)
            ]
        );
    }

    #[rstest]
    #[case::runner_died(None)]
    #[case::runner_exited_unasked(Some(AppExit::Success))]
    #[tokio::test(start_paused = true)]
    async fn a_runner_end_nobody_asked_for_is_a_crash(#[case] exit: Option<AppExit>) {
        let (sink, mut events) = open();
        let (mut inputs, senders) = inputs();
        let (_tx, mut rx) = mpsc::channel::<Event>(4);
        match exit {
            Some(exit) => senders.app_exit.send(exit).unwrap(),
            None => drop(senders.app_exit),
        }

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert!(
            inputs.app_exit.is_none(),
            "a finished receiver must not be polled again"
        );
        assert_eq!(
            drain(&mut events).await,
            [SessionEvent::Disconnected(DisconnectReason::SessionCrashed)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_event_channel_is_a_crash() {
        let (sink, mut events) = open();
        let (mut inputs, _senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        tx.send(Event::Spawn).await.unwrap();
        drop(tx);

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert_eq!(
            drain(&mut events).await,
            [
                SessionEvent::Joined,
                SessionEvent::Disconnected(DisconnectReason::SessionCrashed)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_terminal_event_ends_the_loop() {
        let (sink, mut events) = open();
        let (mut inputs, _senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        tx.send(Event::Spawn).await.unwrap();
        tx.send(Event::Disconnect(None)).await.unwrap();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
        assert_eq!(
            drain(&mut events).await,
            [
                SessionEvent::Joined,
                SessionEvent::Disconnected(DisconnectReason::ConnectionClosed)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_stop_signal_ends_the_loop_without_an_event() {
        let (sink, mut events) = open();
        let (mut inputs, senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        tx.send(Event::Spawn).await.unwrap();
        senders.stop.send(()).unwrap();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Stopped);
        assert_eq!(drain(&mut events).await, []);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_bridge_ends_the_loop() {
        let (sink, _events) = open();
        let (mut inputs, _senders) = inputs();
        let (tx, mut rx) = mpsc::channel::<Event>(4);
        sink.control().close();
        tx.send(Event::Tick).await.unwrap();

        let end = run_loop(&sink, &mut rx, &mut inputs).await;

        assert_eq!(end, LoopEnd::Ended);
    }

    // --- Pieces ---

    #[rstest]
    #[case::domain("mc.example.com", "mc.example.com", 25_565)]
    #[case::domain_and_port("mc.example.com:25570", "mc.example.com", 25_570)]
    #[case::ipv4("192.0.2.10:25566", "192.0.2.10", 25_566)]
    #[case::ipv6("[2001:db8::1]:25567", "2001:db8::1", 25_567)]
    fn azaleas_address_comes_from_the_parts(
        #[case] server: &str,
        #[case] host: &str,
        #[case] port: u16,
    ) {
        let address = server_addr(&server.parse().unwrap());

        assert_eq!(address.host, host);
        assert_eq!(address.port, port);
    }

    #[test]
    fn the_deadline_is_the_timeout_after_the_start() {
        let started = std::time::Instant::now();

        let deadline = connect_deadline(started, SECOND);

        assert_eq!(deadline.map(Instant::into_std), Some(started + SECOND));
        assert_eq!(connect_deadline(started, Duration::MAX), None);
    }

    #[test]
    fn worlds_counts_only_live_ones() {
        let worlds = Worlds::default();
        let kept = Arc::new(1_u8);
        let freed = Arc::new(2_u8);
        worlds.track(Arc::downgrade(&kept));
        worlds.track(Arc::downgrade(&freed));

        drop(freed);

        assert_eq!(worlds.live(), 1);
        drop(kept);
        assert_eq!(worlds.live(), 0);
    }

    #[test]
    fn an_empty_slot_has_no_client() {
        let slot = ClientSlot::default();

        assert!(slot.get().is_none());
        slot.clear();
        assert_eq!(format!("{slot:?}"), "ClientSlot { set: false }");
    }
}
