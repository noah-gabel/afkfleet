//! Component tests for a bot's outbound chat queue (Plan.md P4.5; ADR-0013),
//! against fleet-testkit's fake session on paused time.
// Integration tests are test code, but clippy only applies the test allowances
// of clippy.toml (unwrap, panic, …) inside `#[cfg(test)]`; without this, the
// helper functions below would count as library code.
#![cfg(test)]

use core::num::{NonZeroU32, NonZeroUsize};
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::chat::ChatMessage;
use fleet_core::id::BotId;
use fleet_core::mc::{
    ConnectParams, MinecraftConnector, SessionCredentials, SessionError, SessionEvent,
};
use fleet_core::time;
use fleet_runtime::{
    ChatBucket, ChatDelivery, ChatError, ChatFailure, ChatQueue, ChatTicket, ChatTickets,
    FleetEvent, FleetEventKind, ModeChat, RuntimeClock, RuntimeConfig,
};
use fleet_testkit::mc::{EmitOutcome, FakeConnector, FakeSession, Performed, SessionController};
use proptest::collection::vec;
use proptest::prelude::*;
use rstest::rstest;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

fn bot_id() -> BotId {
    "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap()
}

/// The runtime clock starts here in every test.
fn anchor() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

/// `ms` milliseconds after the anchor.
fn after(ms: u64) -> DateTime<Utc> {
    time::add(anchor(), Duration::from_millis(ms))
}

fn message(text: &str) -> ChatMessage {
    text.parse().unwrap()
}

fn chat(text: &str) -> Performed {
    Performed::Chat(message(text))
}

fn event(at: DateTime<Utc>, kind: FleetEventKind) -> FleetEvent {
    FleetEvent {
        bot_id: bot_id(),
        at,
        kind,
    }
}

const fn failed(ticket: ChatTicket, reason: ChatFailure) -> FleetEventKind {
    FleetEventKind::ChatFailed { ticket, reason }
}

/// The chat settings: one message per `interval_ms`, `burst` at once, and a
/// queue of `queue`.
fn config(interval_ms: u64, burst: u32, queue: usize) -> RuntimeConfig {
    RuntimeConfig {
        chat_interval: Duration::from_millis(interval_ms),
        chat_burst: NonZeroU32::new(burst).unwrap(),
        chat_queue: NonZeroUsize::new(queue).unwrap(),
    }
}

fn params() -> ConnectParams {
    ConnectParams {
        bot_id: bot_id(),
        server: "localhost".try_into().unwrap(),
        credentials: SessionCredentials::Offline {
            username: "AfkBot1".try_into().unwrap(),
        },
        connect_timeout: Duration::from_secs(30),
    }
}

/// Lets every spawned task run until it waits.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// One bot's chat queue with fake sessions, wired as the actor will wire it.
struct Bot {
    queue: ChatQueue,
    connector: FakeConnector,
    events: broadcast::Sender<FleetEvent>,
    subscriber: broadcast::Receiver<FleetEvent>,
    clock: RuntimeClock,
    tasks: JoinSet<()>,
}

impl Bot {
    fn new(config: RuntimeConfig) -> Self {
        Self::with_tickets(config, ChatTickets::new())
    }

    fn with_tickets(config: RuntimeConfig, tickets: ChatTickets) -> Self {
        let bucket = ChatBucket::new(config.chat_interval, config.chat_burst).unwrap();
        let (events, subscriber) = broadcast::channel(64);
        Self {
            queue: ChatQueue::new(bot_id(), config.chat_queue, bucket, tickets),
            connector: FakeConnector::new(),
            events,
            subscriber,
            clock: RuntimeClock::new(anchor()),
            tasks: JoinSet::new(),
        }
    }

    /// Starts a fake session that has joined, and opens the queue for it.
    /// The delivery isn't running, so whatever is queued stays queued.
    async fn open_unspawned(&mut self) -> (ChatDelivery<FakeSession>, ModeChat, SessionController) {
        let index = self.connector.session_count();
        let (session, _events) = self.connector.connect(params()).await.unwrap();
        let controller = self.connector.session(index).await;
        assert_eq!(controller.emit(SessionEvent::Joined), EmitOutcome::Queued);
        let (delivery, mode_chat) = self.queue.open(
            session,
            self.events.clone(),
            self.clock,
            CancellationToken::new(),
        );
        (delivery, mode_chat, controller)
    }

    /// Like [`open_unspawned`](Self::open_unspawned), with the delivery
    /// running.
    async fn open(&mut self) -> (ModeChat, SessionController) {
        let (delivery, mode_chat, controller) = self.open_unspawned().await;
        self.tasks.spawn(delivery.run());
        (mode_chat, controller)
    }

    /// Every event published since the last call.
    fn published(&mut self) -> Vec<FleetEvent> {
        let mut events = Vec::new();
        loop {
            match self.subscriber.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => return events,
                Err(error) => panic!("the subscriber failed: {error:?}"),
            }
        }
    }

    /// Waits for the next spawned task to end; fails the test if none ends
    /// within a minute of paused time.
    async fn task_ended(&mut self) {
        let ended = tokio::time::timeout(Duration::from_secs(60), self.tasks.join_next()).await;
        assert!(
            matches!(ended, Ok(Some(Ok(())))),
            "no task ended: {ended:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn chat_is_refused_with_not_online_while_the_queue_is_closed() {
    let mut bot = Bot::new(RuntimeConfig::default());
    assert_eq!(bot.queue.send(message("hello")), Err(ChatError::NotOnline));

    let (mode_chat, controller) = bot.open().await;
    bot.queue.close();
    bot.task_ended().await;

    assert_eq!(bot.queue.send(message("hello")), Err(ChatError::NotOnline));
    assert_eq!(mode_chat.send(message("hello")), Err(ChatError::NotOnline));
    assert_eq!(controller.log(), []);
    assert_eq!(bot.published(), []);
}

#[tokio::test(start_paused = true)]
async fn closing_a_closed_queue_does_nothing() {
    let mut bot = Bot::new(RuntimeConfig::default());

    bot.queue.close();
    bot.queue.close();

    assert_eq!(bot.queue.send(message("hello")), Err(ChatError::NotOnline));
}

#[tokio::test(start_paused = true)]
async fn a_queued_message_is_sent_and_its_ticket_reported() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, controller) = bot.open().await;
    tokio::time::advance(Duration::from_secs(5)).await;

    let ticket = bot.queue.send(message("hello")).unwrap();
    settle().await;

    assert_eq!(ticket.get(), 1);
    assert_eq!(controller.log(), [chat("hello")]);
    assert_eq!(
        bot.published(),
        [event(after(5_000), FleetEventKind::ChatSent { ticket })]
    );
}

#[tokio::test(start_paused = true)]
async fn messages_are_sent_in_the_order_they_were_queued() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (mode_chat, controller) = bot.open().await;

    let first = bot.queue.send(message("one")).unwrap();
    mode_chat.send(message("two")).unwrap();
    let third = bot.queue.send(message("three")).unwrap();
    settle().await;

    assert_eq!(controller.log(), [chat("one"), chat("two"), chat("three")]);
    assert_eq!(
        bot.published(),
        [
            event(anchor(), FleetEventKind::ChatSent { ticket: first }),
            event(
                anchor(),
                FleetEventKind::ModeChatSent {
                    message: message("two")
                }
            ),
            event(anchor(), FleetEventKind::ChatSent { ticket: third }),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_bucket_lets_3_messages_through_at_once_then_one_every_3_s() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, controller) = bot.open().await;

    for text in ["one", "two", "three"] {
        bot.queue.send(message(text)).unwrap();
    }
    assert_eq!(bot.queue.send(message("four")), Err(ChatError::RateLimited));

    tokio::time::advance(Duration::from_millis(2_999)).await;
    assert_eq!(bot.queue.send(message("four")), Err(ChatError::RateLimited));
    tokio::time::advance(Duration::from_millis(1)).await;
    bot.queue.send(message("four")).unwrap();
    assert_eq!(bot.queue.send(message("five")), Err(ChatError::RateLimited));
    settle().await;

    assert_eq!(
        controller.log(),
        [chat("one"), chat("two"), chat("three"), chat("four")]
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_refuses_with_queue_full_and_uses_no_token() {
    let mut bot = Bot::new(config(3_000, 2, 1));
    let (delivery, _mode_chat, controller) = bot.open_unspawned().await;

    bot.queue.send(message("one")).unwrap();
    assert_eq!(bot.queue.send(message("two")), Err(ChatError::QueueFull));
    bot.tasks.spawn(delivery.run());
    settle().await;

    // The refused message took no token, so one is left, and then none.
    bot.queue.send(message("three")).unwrap();
    settle().await;
    assert_eq!(bot.queue.send(message("four")), Err(ChatError::RateLimited));
    assert_eq!(controller.log(), [chat("one"), chat("three")]);
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_message_holds_no_queue_slot() {
    let mut bot = Bot::new(config(3_000, 1, 2));
    let (_delivery, _mode_chat, _controller) = bot.open_unspawned().await;

    bot.queue.send(message("one")).unwrap();
    assert_eq!(bot.queue.send(message("two")), Err(ChatError::RateLimited));
    tokio::time::advance(Duration::from_secs(3)).await;

    // The refused message left the second slot free.
    bot.queue.send(message("three")).unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(bot.queue.send(message("four")), Err(ChatError::QueueFull));
}

#[tokio::test(start_paused = true)]
async fn chat_that_cant_be_signed_fails_without_retry_or_refund() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, controller) = bot.open().await;
    controller.fail_chat(SessionError::ChatUnavailable);

    let tickets: Vec<_> = (0..3)
        .map(|_| bot.queue.send(message("hello")).unwrap())
        .collect();
    settle().await;
    controller.succeed_chat();
    settle().await;

    assert_eq!(
        bot.queue.send(message("hello")),
        Err(ChatError::RateLimited),
        "the failed sends kept their tokens"
    );
    assert_eq!(controller.log(), [], "nothing was retried");
    assert_eq!(
        bot.published(),
        tickets
            .iter()
            .map(|&ticket| event(anchor(), failed(ticket, ChatFailure::ChatUnavailable)))
            .collect::<Vec<_>>()
    );
}

#[rstest]
#[case::closed(SessionError::Closed, ChatFailure::Disconnected)]
#[case::timed_out(SessionError::TimedOut, ChatFailure::TimedOut)]
#[case::session_busy(SessionError::QueueFull, ChatFailure::SessionBusy)]
#[case::not_in_world(SessionError::NotInWorld, ChatFailure::NotInWorld)]
#[tokio::test(start_paused = true)]
async fn a_failed_send_is_reported_with_its_reason(
    #[case] error: SessionError,
    #[case] reason: ChatFailure,
) {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, controller) = bot.open().await;
    controller.fail_chat(error);

    let ticket = bot.queue.send(message("hello")).unwrap();
    settle().await;

    assert_eq!(bot.published(), [event(anchor(), failed(ticket, reason))]);
}

#[tokio::test(start_paused = true)]
async fn chat_in_a_session_that_ended_fails_as_disconnected() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, controller) = bot.open().await;
    let kicked =
        SessionEvent::Disconnected(fleet_core::disconnect::DisconnectReason::ConnectionClosed);
    assert_eq!(controller.emit(kicked), EmitOutcome::Queued);

    let ticket = bot.queue.send(message("hello")).unwrap();
    settle().await;

    assert_eq!(
        bot.published(),
        [event(anchor(), failed(ticket, ChatFailure::Disconnected))]
    );
}

#[tokio::test(start_paused = true)]
async fn closing_the_queue_fails_every_queued_message_as_disconnected_in_order() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (mode_chat, controller) = bot.open().await;
    let first = bot.queue.send(message("one")).unwrap();
    mode_chat.send(message("two")).unwrap();
    let third = bot.queue.send(message("three")).unwrap();

    bot.queue.close();
    bot.task_ended().await;

    assert_eq!(controller.log(), [], "nothing was sent after the close");
    assert_eq!(
        bot.published(),
        [
            event(anchor(), failed(first, ChatFailure::Disconnected)),
            event(anchor(), failed(third, ChatFailure::Disconnected)),
        ],
        "mode chat has no ticket, so it's only logged"
    );
}

#[tokio::test(start_paused = true)]
async fn mode_chat_is_sent_and_published_as_mode_chat_sent() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (mode_chat, controller) = bot.open().await;
    tokio::time::advance(Duration::from_secs(30)).await;

    mode_chat.send(message("/spawn")).unwrap();
    settle().await;

    assert_eq!(controller.log(), [chat("/spawn")]);
    assert_eq!(
        bot.published(),
        [event(
            after(30_000),
            FleetEventKind::ModeChatSent {
                message: message("/spawn")
            }
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn mode_chat_shares_the_bucket_with_user_chat() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (mode_chat, _controller) = bot.open().await;

    for _ in 0..3 {
        bot.queue.send(message("hello")).unwrap();
    }

    assert_eq!(
        mode_chat.send(message("/spawn")),
        Err(ChatError::RateLimited)
    );
    tokio::time::advance(Duration::from_secs(3)).await;
    mode_chat.send(message("/spawn")).unwrap();
    assert_eq!(
        bot.queue.send(message("hello")),
        Err(ChatError::RateLimited)
    );
}

#[tokio::test(start_paused = true)]
async fn mode_chat_shares_the_queue_with_user_chat() {
    let mut bot = Bot::new(config(3_000, 3, 1));
    let (_delivery, mode_chat, _controller) = bot.open_unspawned().await;

    bot.queue.send(message("hello")).unwrap();

    assert_eq!(mode_chat.send(message("/spawn")), Err(ChatError::QueueFull));
}

#[rstest]
#[case::chat_unavailable(SessionError::ChatUnavailable)]
#[case::timed_out(SessionError::TimedOut)]
#[case::closed(SessionError::Closed)]
#[tokio::test(start_paused = true)]
async fn mode_chat_that_isnt_sent_publishes_nothing(#[case] error: SessionError) {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (mode_chat, controller) = bot.open().await;
    controller.fail_chat(error);

    mode_chat.send(message("/spawn")).unwrap();
    settle().await;

    assert_eq!(controller.log(), []);
    assert_eq!(bot.published(), []);
}

#[tokio::test(start_paused = true)]
async fn the_bucket_carries_over_to_the_next_session() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (_mode_chat, _first) = bot.open().await;
    for _ in 0..3 {
        bot.queue.send(message("hello")).unwrap();
    }
    settle().await;
    bot.queue.close();
    bot.task_ended().await;

    let (_mode_chat, second) = bot.open().await;

    assert_eq!(
        bot.queue.send(message("again")),
        Err(ChatError::RateLimited)
    );
    tokio::time::advance(Duration::from_secs(3)).await;
    bot.queue.send(message("again")).unwrap();
    settle().await;
    assert_eq!(second.log(), [chat("again")]);
}

#[tokio::test(start_paused = true)]
async fn opening_again_closes_the_previous_session_first() {
    let mut bot = Bot::new(RuntimeConfig::default());
    let (first_mode_chat, first) = bot.open().await;
    let queued = bot.queue.send(message("one")).unwrap();

    let (_mode_chat, second) = bot.open().await;
    bot.task_ended().await;
    let sent = bot.queue.send(message("two")).unwrap();
    settle().await;

    assert_eq!(
        first_mode_chat.send(message("/spawn")),
        Err(ChatError::NotOnline)
    );
    assert_eq!(first.log(), []);
    assert_eq!(second.log(), [chat("two")]);
    assert_eq!(
        bot.published(),
        [
            event(anchor(), failed(queued, ChatFailure::Disconnected)),
            event(anchor(), FleetEventKind::ChatSent { ticket: sent }),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn queues_that_share_a_ticket_counter_never_repeat_a_ticket() {
    let tickets = ChatTickets::new();
    let mut first = Bot::with_tickets(RuntimeConfig::default(), tickets.clone());
    let mut second = Bot::with_tickets(RuntimeConfig::default(), tickets);
    let _first_session = first.open().await;
    let _second_session = second.open().await;

    let numbers = [
        first.queue.send(message("a")).unwrap(),
        second.queue.send(message("b")).unwrap(),
        second.queue.send(message("c")).unwrap(),
        first.queue.send(message("d")).unwrap(),
    ]
    .map(ChatTicket::get);

    assert_eq!(numbers, [1, 2, 3, 4]);
}

/// The bucket as a reference model: GCRA with one message per `INTERVAL`
/// and a burst of `BURST`. A message at `now` goes through if the bucket's
/// theoretical arrival time is at most `BURST - 1` intervals ahead of `now`.
struct Gcra {
    arrival: u64,
}

impl Gcra {
    const INTERVAL: u64 = 3_000;
    const BURST: u64 = 3;

    fn admit(&mut self, now: u64) -> bool {
        let arrival = self.arrival.max(now);
        if arrival - now <= (Self::BURST - 1) * Self::INTERVAL {
            self.arrival = arrival + Self::INTERVAL;
            true
        } else {
            false
        }
    }
}

proptest! {
    #[test]
    fn the_bucket_never_lets_more_through_than_its_rate(
        steps in vec((prop_oneof![Just(0_u64), Just(3_000_u64), 0..=7_000_u64], 1..=5_usize), 1..=40),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut bot = Bot::new(config(Gcra::INTERVAL, 3, 256));
            let _session = bot.open().await;
            let mut model = Gcra { arrival: 0 };
            let (mut elapsed, mut admitted) = (0_u64, 0_u64);

            for (ms, sends) in steps {
                tokio::time::advance(Duration::from_millis(ms)).await;
                elapsed += ms;
                for _ in 0..sends {
                    let result = bot.queue.send(message("hello"));
                    prop_assert_eq!(result.is_ok(), model.admit(elapsed), "at {} ms", elapsed);
                    match result {
                        Ok(_) => admitted += 1,
                        Err(error) => prop_assert_eq!(error, ChatError::RateLimited),
                    }
                }
                prop_assert!(
                    admitted <= Gcra::BURST + elapsed / Gcra::INTERVAL,
                    "{} messages in {} ms", admitted, elapsed
                );
            }
            Ok(())
        })?;
    }
}
