//! Tests for a bot's chat routes (Plan.md P11.3, Appendix B; ADR-0020,
//! S3–S8): `POST /api/v1/bots/{id}/chat` sends plain chat and allowlisted
//! commands, and points a Manage holder's unlisted command to
//! `POST /api/v1/bots/{id}/command`, a step-up route for commands. Both
//! answer in the extractors' order, spend the user's and then the bot's chat
//! limit after `authorize()`, store the line as the user's and record it
//! without its text before it's sent, and answer as the bot's agent says
//! within the wait, or pending.
//!
//! The harness has no ingest: a test answers for the agent by reading the
//! line its stream got and publishing the answer on the bus the bots'
//! service waits on, as the ingest does. Time isn't paused (sqlx).

use core::num::NonZeroU32;
use core::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request};
use fleet_core::audit::{AuditAction, RecordedValue};
use fleet_core::authz::{CommandAllowlist, GrantLevel, Role};
use fleet_core::bot::BotState;
use fleet_core::chat::ChatMessage;
use fleet_core::id::{AccountId, BotId, ChatRequestId, UserId};
use fleet_proto::control::{ChatFailedReason, SendChat, ServerMessage};
use fleet_server::app::bots::ChatWait;
use fleet_server::app::events::{EVENT_BUS_CAPACITY, LiveEvent, LiveEventKind};
use fleet_server::config::{Rate, RateLimitConfig};
use fleet_server::http::middleware::Arrival;
use fleet_server::ports::audit::AuditRecord;
use fleet_server::ports::chat::{ChatEntry, ChatRecord};
use fleet_server::ports::page::{Page, PageLimit};
use fleet_server::ports::store::Store as _;
use secrecy::ExposeSecret as _;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::time::Instant;

use super::super::support::{Meanwhile, Options, Person, Seen, Setup, T0, at, setup_with};
use super::{BOTS, Stream, stepped_up};

/// How long a test waits for the line its stream got.
const BOUND: Duration = Duration::from_secs(5);

fn chat_path(bot: BotId) -> String {
    format!("{BOTS}/{bot}/chat")
}

fn command_path(bot: BotId) -> String {
    format!("{BOTS}/{bot}/command")
}

fn online() -> BotState {
    BotState::Online {
        since: at(T0),
        attempt: NonZeroU32::MIN,
    }
}

/// `/spawn` listed, and a wait of 100 ms for an answer, for tests that
/// answer nothing.
fn quick() -> Options {
    Options {
        allowlist: CommandAllowlist::try_new(&["/spawn"]).unwrap(),
        chat_wait: ChatWait::default().with_answer(Duration::from_millis(100)),
        ..Options::default()
    }
}

/// `/spawn` listed, and serve's wait.
fn listed() -> Options {
    Options {
        allowlist: CommandAllowlist::try_new(&["/spawn"]).unwrap(),
        ..Options::default()
    }
}

impl Setup {
    async fn chat(&self, bot: BotId, by: &Person, text: &str) -> Seen {
        self.post(
            &chat_path(bot),
            Some(&by.token),
            Some(json!({ "message": text })),
        )
        .await
    }

    async fn command(&self, bot: BotId, by: &Person, text: &str) -> Seen {
        self.post(
            &command_path(bot),
            Some(&by.token),
            Some(json!({ "message": text })),
        )
        .await
    }

    /// `owner`'s bot `n`, running on agent 1's live stream, which reported
    /// it Online.
    async fn online_bot(&self, owner: UserId, n: u8) -> (AccountId, BotId, Stream) {
        let (account, bot) = self.bot_of(owner, n).await;
        self.running(bot).await;
        self.assigned(bot, 1, None).await;
        let stream = self.holding(1, &[bot]);
        self.report(&stream, bot, online(), None);
        (account, bot, stream)
    }

    /// `bot`'s chat, newest first.
    async fn lines(&self, bot: BotId) -> Vec<ChatRecord> {
        self.db
            .chat()
            .page(
                bot,
                Page {
                    before: None,
                    limit: PageLimit::new(100).unwrap(),
                },
            )
            .await
            .unwrap()
            .into_iter()
            .map(|line| line.record)
            .collect()
    }

    /// The chat routes' entries, oldest first.
    async fn chat_entries(&self) -> Vec<AuditRecord> {
        let mut entries: Vec<AuditRecord> = self
            .audit()
            .await
            .into_iter()
            .filter(|entry| {
                entry.action == AuditAction::BOT_SEND_CHAT
                    || entry.action == AuditAction::BOT_SEND_COMMAND
            })
            .collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    /// Answers the next line `stream` gets for `bot` with `answer`, as the
    /// ingest publishes it: the line's request ID.
    async fn answer(
        &self,
        stream: &mut Stream,
        bot: BotId,
        answer: impl FnOnce(ChatRequestId) -> LiveEventKind,
    ) -> ChatRequestId {
        let (request, _) = stream.line().await;
        self.events.publish(LiveEvent {
            bot,
            agent: stream.lease.agent(),
            generation: stream.lease.generation(),
            at: at(T0),
            kind: answer(request),
        });
        request
    }
}

impl Stream {
    /// The next chat line queued for the agent: its request ID and text.
    async fn line(&mut self) -> (ChatRequestId, ChatMessage) {
        let message = tokio::time::timeout(BOUND, self.queued.recv())
            .await
            .unwrap()
            .unwrap();
        match ServerMessage::try_from(message).unwrap() {
            ServerMessage::SendChat(SendChat {
                request, message, ..
            }) => (request, message),
            other => panic!("not a chat line: {other:?}"),
        }
    }
}

fn users_line(text: &str, by: UserId) -> ChatEntry {
    ChatEntry::UserSent {
        message: ChatMessage::try_from(text).unwrap(),
        by: Some(by),
    }
}

/// An entry's metadata as text values by key.
fn metadata(entry: &AuditRecord) -> Vec<(String, String)> {
    entry
        .metadata
        .entries()
        .iter()
        .map(|(key, value)| {
            let value = match value {
                RecordedValue::Text(text) | RecordedValue::Unrecognized(text) => text.clone(),
                RecordedValue::Int(int) => int.to_string(),
                RecordedValue::Bool(flag) => flag.to_string(),
            };
            (key.clone(), value)
        })
        .collect()
}

fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(found, _)| found == key)
        .map(|(_, value)| value.as_str())
}

// --- Who may send what ------------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn both_routes_answer_404_for_an_unknown_a_malformed_or_an_unseen_bot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let stranger = stepped_up(&setup, 3, "stranger", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let unknown = BotId::new_v7(at(T0), [0xAB; 10]).unwrap();

    let seen = [
        setup.chat(bot, &stranger, "hello").await,
        setup.command(bot, &stranger, "/op AfkBot1").await,
        setup.chat(unknown, &owner, "hello").await,
        setup
            .post(
                &format!("{BOTS}/not-an-id/chat"),
                Some(&owner.token),
                Some(json!({ "message": "hello" })),
            )
            .await,
    ];

    for seen in &seen {
        assert_eq!(seen.status, 404, "{}", seen.text);
        assert_eq!(seen.code(), "not_found");
    }
    assert_eq!(setup.chat_entries().await.len(), 0);
}

/// A View holder may read the bot but not chat, and the bot's chat limit
/// isn't spent by the refusal: the owner's line after it isn't refused for
/// it.
#[sqlx::test(migrations = false)]
async fn a_view_holder_may_not_chat_and_spends_no_chat_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let mut settings = quick();
    settings.limits.chat_per_bot = fleet_core::chat::ChatRate::try_new(60, 1).unwrap();
    let setup = setup_with(options, settings).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let viewer = setup.person(3, "viewer", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, viewer.id, GrantLevel::View, owner.id)
        .await;

    let refused = setup.chat(bot, &viewer, "hello").await;
    let owners = setup.chat(bot, &owner, "hello").await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.code(), "forbidden");
    assert_eq!(owners.code(), "bot_not_online", "{}", owners.text);
    assert_eq!(setup.lines(bot).await.len(), 0);
}

/// A command off the allowlist needs Manage on either route, and the names
/// that aren't exactly a listed one fail closed (ADR-0010, S8).
#[sqlx::test(migrations = false)]
async fn an_unlisted_command_needs_manage_on_either_route(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = stepped_up(&setup, 3, "controller", Role::Member).await;
    let (account, bot) = setup.bot_of(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    let mut seen = Vec::new();
    for text in ["/op AfkBot2", "/Spawn", "/minecraft:spawn", "/", "/ spawn"] {
        seen.push((text, setup.chat(bot, &controller, text).await));
        seen.push((text, setup.command(bot, &controller, text).await));
    }
    let listed = setup.chat(bot, &controller, "/spawn now").await;
    let listed_command = setup.command(bot, &controller, "/spawn").await;

    for (text, seen) in &seen {
        assert_eq!(seen.status, 403, "{text}: {}", seen.text);
        assert_eq!(seen.code(), "forbidden", "{text}");
    }
    assert_eq!(listed.code(), "bot_not_online", "{}", listed.text);
    assert_eq!(listed_command.code(), "bot_not_online");
    assert_eq!(setup.chat_entries().await.len(), 0);
}

/// `/chat` never sends a command off the allowlist: a Manage holder is
/// pointed to `/command` before any chat limit is spent (S4).
#[sqlx::test(migrations = false)]
async fn a_manage_holder_is_pointed_to_the_command_route_and_spends_no_chat_limit(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let mut settings = quick();
    settings.limits.chat_per_user = Rate::per_minute(NonZeroU32::MIN);
    let setup = setup_with(options, settings).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;

    let pointed = setup.chat(bot, &owner, "/op AfkBot2").await;
    let again = setup.chat(bot, &owner, "/op AfkBot2").await;
    let sent = setup.command(bot, &owner, "/op AfkBot2").await;

    assert_eq!(pointed.status, 403);
    assert_eq!(pointed.code(), "command_needs_step_up");
    assert_eq!(again.code(), "command_needs_step_up");
    assert_eq!(sent.code(), "bot_not_online", "{}", sent.text);
    assert_eq!(setup.chat_entries().await.len(), 0);
}

#[sqlx::test(migrations = false)]
async fn the_command_route_needs_a_fresh_step_up_and_writes_sends_and_spends_nothing_without_one(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let mut settings = quick();
    settings.limits.chat_per_user = Rate::per_minute(NonZeroU32::MIN);
    let setup = setup_with(options, settings).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;

    let without = setup.command(bot, &owner, "/op AfkBot2").await;
    setup.stamp(owner.session).await;
    let (with, _) = tokio::join!(
        setup.command(bot, &owner, "/op AfkBot2"),
        setup.answer(&mut stream, bot, LiveEventKind::ChatSent)
    );

    assert_eq!(without.status, 403);
    assert_eq!(without.code(), "step_up_required");
    assert_eq!(with.status, 200, "{}", with.text);
    assert_eq!(setup.lines(bot).await.len(), 1);
    assert_eq!(setup.chat_entries().await.len(), 1);
}

/// A grant lowered between the handler's check and the send's transaction:
/// the transaction asks `authorize()` again, and writes nothing.
#[sqlx::test(migrations = false)]
async fn a_grant_lowered_before_the_transaction_is_403_and_writes_nothing(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let manager = stepped_up(&setup, 3, "manager", Role::Member).await;
    let (account, bot, _stream) = setup.online_bot(owner.id, 10).await;
    setup
        .share(account, manager.id, GrantLevel::Manage, owner.id)
        .await;
    setup.hooked.queue(Meanwhile::SetGrant(
        account,
        manager.id,
        GrantLevel::Control,
        owner.id,
    ));

    let refused = setup.command(bot, &manager, "/op AfkBot2").await;

    assert_eq!(refused.status, 403, "{}", refused.text);
    assert_eq!(refused.code(), "forbidden");
    assert_eq!(setup.lines(bot).await.len(), 0);
    assert_eq!(setup.chat_entries().await.len(), 0);
}

/// A grant revoked between the handler's check and the send's transaction:
/// the transaction asks `authorize()` again, which no longer lets the user
/// see the bot, so it's 404, and nothing is written, recorded or sent.
#[sqlx::test(migrations = false)]
async fn a_grant_revoked_before_the_transaction_is_404_and_nothing_is_written_or_sent(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let (account, bot, mut stream) = setup.online_bot(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;
    setup
        .hooked
        .queue(Meanwhile::RevokeGrant(account, controller.id));

    let refused = setup.chat(bot, &controller, "hello").await;

    assert_eq!(refused.status, 404, "{}", refused.text);
    assert_eq!(refused.code(), "not_found");
    assert_eq!(setup.lines(bot).await.len(), 0);
    assert_eq!(setup.chat_entries().await.len(), 0);
    assert!(stream.next().is_none(), "nothing was sent");
}

// --- The body ---------------------------------------------------------------------------

/// Every broken rule is a 422 on `message` that never echoes the line, and
/// a body's error comes before the bot is looked up, so a plain line to an
/// unseen bot is 422 too (S8).
#[sqlx::test(migrations = false)]
async fn an_invalid_line_is_422_on_message_and_never_echoed(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let stranger = stepped_up(&setup, 3, "stranger", Role::Member).await;
    let (_, bot) = setup.bot_of(owner.id, 10).await;
    let long = format!("secret_label{}", "x".repeat(300));

    let cases = [
        (setup.chat(bot, &owner, "").await, "must not be empty"),
        (
            setup.chat(bot, &owner, "secret_label\nnext").await,
            "must be one line, without control characters, formatting codes or text-direction marks",
        ),
        (
            setup.chat(bot, &owner, &long).await,
            "must be at most 256 characters, an emoji counting as 2",
        ),
        (
            setup.command(bot, &owner, "secret_label").await,
            "must be a command, starting with /",
        ),
        (
            setup.command(bot, &stranger, "secret_label").await,
            "must be a command, starting with /",
        ),
    ];

    for (seen, rule) in &cases {
        assert_eq!(seen.status, 422, "{}", seen.text);
        assert_eq!(seen.body["error"]["fields"][0]["path"], "message");
        assert_eq!(seen.body["error"]["fields"][0]["message"], *rule);
        assert!(!seen.text.contains("secret_label"), "{}", seen.text);
    }
    assert_eq!(setup.chat_entries().await.len(), 0);
}

// --- The limits -------------------------------------------------------------------------

/// Each user's chat limit is spent across bots, and each bot's across
/// users, after `authorize()`: a stranger's 404 spends neither (S3).
#[sqlx::test(migrations = false)]
async fn the_chat_limits_are_spent_per_user_across_bots_and_per_bot_across_users(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let mut settings = quick();
    settings.limits = RateLimitConfig {
        chat_per_user: Rate::per_minute(NonZeroU32::new(2).unwrap()),
        chat_per_bot: fleet_core::chat::ChatRate::try_new(60, 1).unwrap(),
        ..RateLimitConfig::default()
    };
    let setup = setup_with(options, settings).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let controller = setup.person(3, "controller", Role::Member).await;
    let stranger = setup.person(4, "stranger", Role::Member).await;
    let (account, first) = setup.bot_of(owner.id, 10).await;
    let (_, second) = setup.bot_of(owner.id, 11).await;
    let (_, third) = setup.bot_of(owner.id, 12).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    let probe = setup.chat(first, &stranger, "hello").await;
    let owners_first = setup.chat(first, &owner, "hello").await;
    let controllers = setup.chat(first, &controller, "hello").await;
    let owners_second = setup.chat(second, &owner, "hello").await;
    let owners_third = setup.chat(third, &owner, "hello").await;

    assert_eq!(probe.status, 404);
    assert_eq!(
        owners_first.code(),
        "bot_not_online",
        "a refusal spends both"
    );
    assert_eq!(controllers.status, 429, "the bot's limit, across users");
    assert_eq!(controllers.code(), "rate_limited");
    assert_eq!(controllers.headers["retry-after"], "60");
    assert_eq!(owners_second.code(), "bot_not_online");
    assert_eq!(owners_third.status, 429, "the user's limit, across bots");
    assert_eq!(owners_third.headers["retry-after"], "30");
}

// --- What the send answers --------------------------------------------------------------

#[sqlx::test(migrations = false)]
async fn a_bot_that_cant_take_a_line_answers_409_and_nothing_is_written(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, stopped) = setup.bot_of(owner.id, 10).await;
    let (_, streamless) = setup.bot_of(owner.id, 11).await;
    setup.running(streamless).await;
    setup.assigned(streamless, 2, None).await;
    let (_, connecting) = setup.bot_of(owner.id, 12).await;
    setup.running(connecting).await;
    setup.assigned(connecting, 1, None).await;
    let stream = setup.holding(1, &[connecting]);
    setup.report(
        &stream,
        connecting,
        BotState::Connecting {
            attempt: NonZeroU32::MIN,
            auth_retried: false,
        },
        None,
    );

    let seen = [
        (setup.chat(stopped, &owner, "hello").await, "bot_not_online"),
        (
            setup.chat(streamless, &owner, "hello").await,
            "bot_unreachable",
        ),
        (
            setup.chat(connecting, &owner, "hello").await,
            "bot_not_online",
        ),
    ];

    for (seen, code) in &seen {
        assert_eq!(seen.status, 409, "{}", seen.text);
        assert_eq!(seen.code(), *code);
    }
    for bot in [stopped, streamless, connecting] {
        assert_eq!(setup.lines(bot).await.len(), 0);
    }
    assert_eq!(setup.chat_entries().await.len(), 0);
}

/// The line goes to the stream that holds the bot, stored as the user's and
/// recorded first; the agent's `ChatSent` answers 200 with the request's and
/// the row's IDs.
#[sqlx::test(migrations = false)]
async fn a_line_its_agent_sent_answers_200_with_its_request_and_row(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, listed()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;

    let (seen, request) = tokio::join!(
        setup.chat(bot, &owner, "  hello there  "),
        setup.answer(&mut stream, bot, LiveEventKind::ChatSent)
    );

    assert_eq!(seen.status, 200, "{}", seen.text);
    assert_eq!(seen.body["request_id"], request.to_string());
    assert_eq!(seen.body["outcome"], "sent");
    let lines = setup.lines(bot).await;
    let line = lines.first().unwrap();
    assert_eq!(seen.body["message_id"], line.id.get());
    assert_eq!(line.entry, users_line("hello there", owner.id));
    let entries = setup.chat_entries().await;
    assert_eq!(entries.len(), 1);
    let entry = entries.first().unwrap();
    assert_eq!(entry.action, AuditAction::BOT_SEND_CHAT);
    assert_eq!(entry.actor, Some(owner.id));
    let recorded = metadata(entry);
    assert_eq!(
        value(&recorded, "request"),
        Some(request.to_string().as_str())
    );
    assert_eq!(
        value(&recorded, "message"),
        Some(line.id.get().to_string().as_str())
    );
    assert_eq!(value(&recorded, "command"), Some("none"));
    assert!(!format!("{:?}", entry.metadata).contains("hello"));
}

/// Every reason an agent gives, as S5's table answers it, one match without
/// a wildcard on the server; the row and the entry stay each time (S7).
#[sqlx::test(migrations = false)]
async fn every_agent_refusal_answers_as_s5_says_and_keeps_the_row_and_entry(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, listed()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;
    let cases = [
        (ChatFailedReason::NotOnline, 409, "bot_not_online"),
        (ChatFailedReason::Disconnected, 409, "bot_not_online"),
        (ChatFailedReason::NotInWorld, 409, "bot_not_online"),
        (ChatFailedReason::ChatUnavailable, 409, "chat_unavailable"),
        (ChatFailedReason::RateLimited, 429, "rate_limited"),
        (ChatFailedReason::QueueFull, 429, "rate_limited"),
        (ChatFailedReason::TimedOut, 409, "bot_unreachable"),
        (ChatFailedReason::SessionBusy, 409, "bot_unreachable"),
        (ChatFailedReason::AgentBusy, 409, "bot_unreachable"),
        (ChatFailedReason::UnknownBot, 409, "bot_unreachable"),
        (ChatFailedReason::ShuttingDown, 409, "bot_unreachable"),
    ];

    for (n, (reason, status, code)) in (1..).zip(cases) {
        let (seen, _) = tokio::join!(
            setup.chat(bot, &owner, "hello"),
            setup.answer(&mut stream, bot, |request| {
                LiveEventKind::ChatFailed(request, reason)
            })
        );

        assert_eq!(seen.status, status, "{reason:?}: {}", seen.text);
        assert_eq!(seen.code(), code, "{reason:?}");
        if status == 429 {
            assert_eq!(seen.headers["retry-after"], "3", "the bot's interval");
        }
        assert_eq!(setup.lines(bot).await.len(), n, "{reason:?}");
        assert_eq!(setup.chat_entries().await.len(), n, "{reason:?}");
        // The bot's own chat limit refills before the next line.
        setup.clock.advance(Duration::from_secs(3));
    }
}

#[sqlx::test(migrations = false)]
async fn a_line_without_an_answer_in_time_answers_202_pending_and_is_kept(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;

    let seen = setup.chat(bot, &owner, "anyone?").await;
    let (request, _) = stream.line().await;

    assert_eq!(seen.status, 202, "{}", seen.text);
    assert_eq!(seen.body["outcome"], "pending");
    assert_eq!(seen.body["request_id"], request.to_string());
    assert_eq!(setup.lines(bot).await.len(), 1);
    assert_eq!(setup.chat_entries().await.len(), 1);
}

/// A wait that fell behind the bus can't know whether its answer was among
/// what it missed: pending, at once.
#[sqlx::test(migrations = false)]
async fn a_wait_that_falls_behind_the_bus_answers_202(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, listed()).await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;
    let flood = async {
        let (request, _) = stream.line().await;
        for _ in 0..=EVENT_BUS_CAPACITY {
            setup.events.publish(LiveEvent {
                bot,
                agent: stream.lease.agent(),
                generation: stream.lease.generation(),
                at: at(T0),
                kind: LiveEventKind::Died,
            });
        }
        setup.events.publish(LiveEvent {
            bot,
            agent: stream.lease.agent(),
            generation: stream.lease.generation(),
            at: at(T0),
            kind: LiveEventKind::ChatSent(request),
        });
    };

    let (seen, ()) = tokio::join!(setup.chat(bot, &owner, "hello"), flood);

    assert_eq!(seen.status, 202, "{}", seen.text);
    assert_eq!(seen.body["outcome"], "pending");
}

/// The wait ends 2 s before the request would time out: with a 2 s request
/// timeout, at once, well before the answer's 6 s.
#[sqlx::test(migrations = false)]
async fn the_request_timeout_bounds_the_wait(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            chat_wait: ChatWait::new(Duration::from_secs(2)),
            ..Options::default()
        },
    )
    .await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, _stream) = setup.online_bot(owner.id, 10).await;
    let started = Instant::now();

    let seen = setup.chat(bot, &owner, "hello").await;

    assert_eq!(seen.status, 202, "{}", seen.text);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// The wait counts from the request's arrival, never the handler's start
/// (ADR-0020, U6): a request that arrived 30 s ago, under a 19 s request
/// timeout, is answered pending at once, though its agent never answers. A
/// handler counting from its own start would wait the answer's 6 s.
#[sqlx::test(migrations = false)]
async fn the_wait_counts_from_the_requests_arrival_not_the_handlers_start(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(
        options,
        Options {
            chat_wait: ChatWait::new(Duration::from_secs(19)),
            ..Options::default()
        },
    )
    .await;
    let owner = setup.person(2, "owner", Role::Member).await;
    let (_, bot, mut stream) = setup.online_bot(owner.id, 10).await;
    // The seam: the stack keeps an arrival the request already carries.
    let arrived = Instant::now().checked_sub(Duration::from_secs(30)).unwrap();
    let request = Request::builder()
        .method(Method::POST)
        .uri(chat_path(bot))
        .header(
            "authorization",
            format!("Bearer {}", owner.token.expose_secret()),
        )
        .header("content-type", "application/json")
        .extension(Arrival::new(arrived))
        .body(Body::from(json!({ "message": "hello" }).to_string()))
        .unwrap();
    let begun = Instant::now();

    let seen = setup.send(request).await;

    assert_eq!(seen.status, 202, "{}", seen.text);
    assert_eq!(seen.body["outcome"], "pending");
    assert!(
        begun.elapsed() < Duration::from_secs(4),
        "{:?}",
        begun.elapsed()
    );
    let (_, sent) = stream.line().await;
    assert_eq!(sent.as_str(), "hello");
}

// --- The audit --------------------------------------------------------------------------

/// The command route records `bot.send_command`: a stepped-up Control
/// holder's allowlisted command with its name, a Manage holder's unlisted
/// one without (S6, S8).
#[sqlx::test(migrations = false)]
async fn the_command_route_records_whether_the_command_is_listed_and_only_a_listed_name(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup_with(options, quick()).await;
    let owner = stepped_up(&setup, 2, "owner", Role::Member).await;
    let controller = stepped_up(&setup, 3, "controller", Role::Member).await;
    let (account, bot, _stream) = setup.online_bot(owner.id, 10).await;
    setup
        .share(account, controller.id, GrantLevel::Control, owner.id)
        .await;

    let listed = setup.command(bot, &controller, "/spawn here").await;
    let unlisted = setup.command(bot, &owner, "/op AfkBot2").await;

    assert_eq!(listed.status, 202, "{}", listed.text);
    assert_eq!(unlisted.status, 202, "{}", unlisted.text);
    let entries = setup.chat_entries().await;
    let [first, second] = entries.as_slice() else {
        panic!("{entries:?}");
    };
    assert_eq!(first.action, AuditAction::BOT_SEND_COMMAND);
    assert_eq!(first.actor, Some(controller.id));
    let first = metadata(first);
    assert_eq!(value(&first, "command"), Some("allowlisted"));
    assert_eq!(value(&first, "command_name"), Some("/spawn"));
    assert_eq!(second.action, AuditAction::BOT_SEND_COMMAND);
    let second = metadata(second);
    assert_eq!(value(&second, "command"), Some("unlisted"));
    assert_eq!(value(&second, "command_name"), None);
    assert!(
        !format!("{first:?}{second:?}").contains("AfkBot2")
            && !format!("{first:?}").contains("here")
    );
}
