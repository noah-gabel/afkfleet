//! proptest strategies for the core values the protocol carries. fleet-core's
//! own strategies are private to its tests, so these are the protocol's.

use core::num::{NonZeroU32, NonZeroU64};
use core::time::Duration;

use chrono::{DateTime, Utc};
use fleet_core::bot::{
    BotAccount, BotNotification, BotSnapshot, BotSpec, BotState, DesiredRunState, FailReason,
    Lifecycle, PauseReason, StickyState,
};
use fleet_core::chat::{ChatFormats, ChatMessage, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::{
    AccountRestriction, ConflictKind, ConflictTexts, ConnectFailure, DisconnectReason,
    PermanentKind, SessionServerFailure,
};
use fleet_core::id::{AccountId, AgentId, BotId, ChatRequestId};
use fleet_core::mc::SessionCredentials;
use fleet_core::mode::{Action, HotbarSlot, ModeDefinition, ModeDraft, Schedule, Step};
use fleet_core::script::menu::{
    BotInventory, Item, ItemData, Screen, ScreenKind, SignDraft, SignSide,
};
use fleet_core::script::{BlockPosition, Identifier};
use fleet_core::script::{
    CaptureDraft, CaptureKind, DecimalMark, Hook, LogKind, MessageDraft, PatternDraft, ProblemKind,
    ReportText, Script, ScriptDraft, ScriptLogLine, ScriptPlace, ScriptProblem, ScriptState,
    ValueDraft, ValueKind, When,
};
use fleet_core::value::{AgentCapacity, McUsername, ServerAddress};
use fleet_proto::control::{
    AgentMessage, Assignment, BotEvent, BotEventKind, ChatFailedReason, Hello, LifecycleCommand,
    ScreenAnswer, ScreenOutcome, ScreenRequest, ScreenRequestId, ScreenUnseen, Seen, SeenSign,
    SendChat, ServerMessage, SessionDenied, SessionGrant, SessionRequest, SessionRequestId,
    Welcome,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;
use secrecy::SecretString;
use uuid::Uuid;

use crate::REJECT_REASONS;

/// The earliest and latest times the wire accepts, in milliseconds.
pub(crate) const EARLIEST_MS: i64 = -62_167_219_200_000;
pub(crate) const LATEST_MS: i64 = 253_402_300_799_999;

/// A time the wire carries exactly: whole milliseconds, years 0 to 9999.
pub(crate) fn time() -> impl Strategy<Value = DateTime<Utc>> {
    prop_oneof![
        EARLIEST_MS..=LATEST_MS,
        1_600_000_000_000_i64..1_900_000_000_000,
    ]
    .prop_map(|millis| DateTime::from_timestamp_millis(millis).unwrap())
}

fn v7() -> impl Strategy<Value = (DateTime<Utc>, [u8; 10])> {
    (0..(1_i64 << 48), any::<[u8; 10]>())
        .prop_map(|(millis, random)| (DateTime::from_timestamp_millis(millis).unwrap(), random))
}

pub(crate) fn bot_id() -> impl Strategy<Value = BotId> {
    v7().prop_map(|(at, random)| BotId::new_v7(at, random).unwrap())
}

pub(crate) fn account_id() -> impl Strategy<Value = AccountId> {
    v7().prop_map(|(at, random)| AccountId::new_v7(at, random).unwrap())
}

pub(crate) fn agent_id() -> impl Strategy<Value = AgentId> {
    v7().prop_map(|(at, random)| AgentId::new_v7(at, random).unwrap())
}

pub(crate) fn chat_request_id() -> impl Strategy<Value = ChatRequestId> {
    v7().prop_map(|(at, random)| ChatRequestId::new_v7(at, random).unwrap())
}

pub(crate) fn mc_username() -> impl Strategy<Value = McUsername> {
    "[A-Za-z0-9_]{3,16}".prop_map(|name| McUsername::try_from(name.as_str()).unwrap())
}

pub(crate) fn server_address() -> impl Strategy<Value = ServerAddress> {
    prop_oneof![
        "[a-z][a-z0-9-]{0,8}(\\.[a-z][a-z0-9]{0,6}){0,2}(:[1-9][0-9]{0,3})?",
        "192\\.0\\.2\\.[1-9][0-9]?(:25565)?",
        Just("[2001:db8::1]:25566".to_owned()),
        Just("2001:db8::7".to_owned()),
    ]
    .prop_filter_map("a valid address", |text| {
        ServerAddress::try_from(text.as_str()).ok()
    })
}

pub(crate) fn chat_message() -> impl Strategy<Value = ChatMessage> {
    prop_oneof![
        "[A-Za-z0-9 !?.,]{1,60}",
        "/[a-z]{1,10}( [a-z0-9]{1,10}){0,3}",
        "[\\PC&&[^§]]{1,40}",
    ]
    .prop_filter_map("a valid message", |text| {
        ChatMessage::try_from(text.as_str()).ok()
    })
}

/// A chat rate, weighted toward the ranges' edges (ADR-0020, S3).
pub(crate) fn chat_rate() -> impl Strategy<Value = fleet_core::chat::ChatRate> {
    let interval = prop_oneof![Just(1_u32), Just(60), 1..=60_u32];
    let burst = prop_oneof![Just(1_u32), Just(8), 1..=8_u32];
    (interval, burst)
        .prop_map(|(interval, burst)| fleet_core::chat::ChatRate::try_new(interval, burst).unwrap())
}

pub(crate) fn conflict_texts() -> impl Strategy<Value = ConflictTexts> {
    vec("[A-Za-z0-9 .!]{1,40}", 0..=4).prop_map(|texts| ConflictTexts::try_new(&texts).unwrap())
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        (-180.0_f32..=180.0, -90.0_f32..=90.0).prop_map(|(yaw, pitch)| Action::Look { yaw, pitch }),
        (0.0_f32..=180.0, 0.0_f32..=90.0)
            .prop_map(|(max_yaw, max_pitch)| Action::RotateRandom { max_yaw, max_pitch }),
        Just(Action::Jump),
        any::<bool>().prop_map(|on| Action::Sneak { on }),
        Just(Action::SwingArm),
        Just(Action::UseItem),
        any::<bool>().prop_map(|on| Action::HoldUse { on }),
        Just(Action::AttackFacingEntity),
        (0..=8_u8).prop_map(|slot| Action::SelectHotbarSlot {
            slot: HotbarSlot::try_from(slot).unwrap()
        }),
        chat_message().prop_map(|message| Action::SendChat { message }),
    ]
}

fn schedule() -> impl Strategy<Value = Schedule> {
    prop_oneof![
        Just(Schedule::AtStart),
        (250..=86_400_000_u64, 0..=86_400_000_u64).prop_map(|(interval, jitter)| Schedule::Every {
            interval: Duration::from_millis(interval),
            jitter: Duration::from_millis(jitter),
        }),
        Just(Schedule::OnRespawn),
    ]
}

/// A message a scripted mode declares: fixed, or with an integer, a decimal
/// or a word.
fn declared_message(n: usize) -> impl Strategy<Value = MessageDraft> {
    prop_oneof![
        Just(MessageDraft {
            key: format!("fixed_{n}"),
            text: "/spawn".to_owned(),
            values: Vec::new(),
        }),
        (-1000..=0_i64, 1..=1_000_000_i64).prop_map(move |(min, max)| MessageDraft {
            key: format!("count_{n}"),
            text: "/give AfkMain {count}".to_owned(),
            values: vec![ValueDraft {
                name: "count".to_owned(),
                kind: ValueKind::Integer { min, max },
            }],
        }),
        (0..=100_u32, 1..=4_u8).prop_map(move |(min, places)| MessageDraft {
            key: format!("pay_{n}"),
            text: "/pay AfkMain {amount}".to_owned(),
            values: vec![ValueDraft {
                name: "amount".to_owned(),
                kind: ValueKind::Decimal {
                    min: f64::from(min),
                    max: 1_000_000.0,
                    places,
                },
            }],
        }),
        Just(MessageDraft {
            key: format!("home_{n}"),
            text: "/home {name}".to_owned(),
            values: vec![ValueDraft {
                name: "name".to_owned(),
                kind: ValueKind::Word {
                    options: vec!["farm".to_owned(), "base".to_owned()],
                },
            }],
        }),
    ]
}

/// A chat pattern a scripted mode declares, of any capture's kind.
fn declared_pattern(n: usize) -> impl Strategy<Value = PatternDraft> {
    let when = prop_oneof![Just(When::Reply), Just(When::AnyTime)];
    let capture = prop_oneof![
        (0..=10_i64).prop_map(|min| CaptureKind::Integer { min, max: 64 }),
        (
            0..=10_u32,
            prop_oneof![Just(DecimalMark::Point), Just(DecimalMark::Comma)]
        )
            .prop_map(|(min, decimal_mark)| CaptureKind::Amount {
                min: f64::from(min),
                max: 1_000_000_000.0,
                decimal_mark,
            }),
        Just(CaptureKind::Word {
            options: vec!["sold".to_owned(), "bought".to_owned()],
        }),
    ];
    (when, capture).prop_map(move |(when, kind)| PatternDraft {
        key: format!("pattern_{n}"),
        when,
        text: "Balance: ${value}".to_owned(),
        captures: vec![CaptureDraft {
            name: "value".to_owned(),
            kind,
        }],
    })
}

/// A sign a scripted mode declares: fixed lines, or a price alone on its
/// line.
fn declared_sign(n: usize) -> impl Strategy<Value = SignDraft> {
    prop_oneof![
        Just(SignDraft {
            key: format!("note_{n}"),
            lines: vec!["afk".to_owned(), String::new(), "bot".to_owned()],
            values: Vec::new(),
        }),
        (1..=1000_i64).prop_map(move |min| SignDraft {
            key: format!("price_{n}"),
            lines: vec!["{price}".to_owned()],
            values: vec![ValueDraft {
                name: "price".to_owned(),
                kind: ValueKind::Integer {
                    min,
                    max: 1_000_000,
                },
            }],
        }),
    ]
}

/// A valid scripted mode's body: a script of printable text and line breaks,
/// any leash, tool guard and list of blocks, up to three messages and two
/// patterns of every kind, and at level 2 up to two signs.
pub(crate) fn script() -> impl Strategy<Value = Script> {
    let blocks = proptest::sample::subsequence(
        vec![
            "minecraft:cobblestone",
            "minecraft:stone",
            "minecraft:oak_log",
            "minecraft:melon",
        ],
        0..=4,
    );
    (
        "[ -~\n\t]{0,200}",
        1..=64_u8,
        0..=100_u8,
        blocks,
        0..=3_usize,
        0..=2_usize,
        prop::option::of(0..=2_usize),
    )
        .prop_flat_map(
            |(source, leash, tool_guard, blocks, messages, patterns, signs)| {
                let messages: Vec<_> = (0..messages).map(declared_message).collect();
                let patterns: Vec<_> = (0..patterns).map(declared_pattern).collect();
                // No count of signs is level 1, which declares none.
                let level = if signs.is_some() { 2 } else { 1 };
                let signs: Vec<_> = (0..signs.unwrap_or(0)).map(declared_sign).collect();
                (
                    Just((source, leash, tool_guard, blocks, level)),
                    messages,
                    patterns,
                    signs,
                )
            },
        )
        .prop_map(
            |((source, leash, tool_guard, blocks, level), messages, patterns, signs)| {
                Script::try_from(ScriptDraft {
                    api: level,
                    source,
                    leash,
                    tool_guard,
                    block_guard: blocks.into_iter().map(str::to_owned).collect(),
                    messages,
                    patterns,
                    signs,
                })
                .unwrap()
            },
        )
}

/// A valid mode: random steps that pass `ModeDraft::validate`, the presets,
/// and scripted modes.
pub(crate) fn mode() -> impl Strategy<Value = ModeDefinition> {
    prop_oneof![
        Just(ModeDefinition::afk()),
        Just(ModeDefinition::farm()),
        script().prop_map(ModeDefinition::scripted),
        vec(
            (action(), schedule(), 1..=100_u8).prop_map(|(action, schedule, probability)| Step {
                action,
                schedule,
                probability,
            }),
            0..=4,
        )
        .prop_filter_map("a valid mode", |steps| ModeDraft { steps }.validate().ok()),
    ]
}

pub(crate) fn bot_account() -> impl Strategy<Value = BotAccount> {
    prop_oneof![
        mc_username().prop_map(BotAccount::Offline),
        account_id().prop_map(BotAccount::Online),
    ]
}

pub(crate) fn bot_spec() -> impl Strategy<Value = BotSpec> {
    (
        bot_id(),
        bot_account(),
        server_address(),
        mode(),
        prop_oneof![
            Just(DesiredRunState::Running),
            Just(DesiredRunState::Stopped)
        ],
        conflict_texts(),
        chat_rate(),
        chat_formats(),
    )
        .prop_map(
            |(id, account, server, mode, desired, conflict_texts, chat_rate, chat_formats)| {
                BotSpec {
                    id,
                    account,
                    server,
                    mode,
                    desired,
                    conflict_texts,
                    chat_rate,
                    chat_formats,
                }
            },
        )
}

/// A server's chat formats: up to three valid ones, as written.
pub(crate) fn chat_formats() -> impl Strategy<Value = ChatFormats> {
    proptest::sample::subsequence(
        vec![
            "<{sender}> ",
            "[{any}] {sender}: ",
            "{sender} whispers to you: ",
        ],
        0..=3,
    )
    .prop_map(|formats| ChatFormats::try_new(&formats).unwrap())
}

/// A script's status as an agent reports it: its texts as the reader would
/// leave them, cleaned.
pub(crate) fn script_state() -> impl Strategy<Value = ScriptState> {
    let problem = (
        select(ProblemKind::ALL.to_vec()),
        prop::option::of(select(Hook::ALL.to_vec())),
        prop::option::of((1..=u32::MAX, 1..=u32::MAX)),
        "[ -~]{0,200}",
    )
        .prop_map(|(kind, hook, place, message)| ScriptProblem {
            kind,
            hook,
            place: place.and_then(|(line, column)| ScriptPlace::new(line, column)),
            message: ReportText::clean(&message),
        });
    prop_oneof![
        Just(ScriptState::Running),
        problem.clone().prop_map(ScriptState::Stopped),
        problem.prop_map(ScriptState::Refused),
        Just(ScriptState::Crashed),
    ]
}

/// A line of a script's log as an agent reports it.
pub(crate) fn script_log_line() -> impl Strategy<Value = ScriptLogLine> {
    (select(LogKind::ALL.to_vec()), "[ -~]{0,200}").prop_map(|(kind, text)| ScriptLogLine {
        kind,
        text: ReportText::clean(&text),
    })
}

fn attempt() -> impl Strategy<Value = NonZeroU32> {
    prop_oneof![1..=5_u32, 1..=u32::MAX].prop_map(|n| NonZeroU32::new(n).unwrap())
}

pub(crate) fn pause_reason() -> impl Strategy<Value = PauseReason> {
    Just(PauseReason::Conflict {
        kind: ConflictKind::DuplicateLogin,
    })
}

pub(crate) fn fail_reason() -> impl Strategy<Value = FailReason> {
    prop_oneof![
        select(vec![
            PermanentKind::Banned,
            PermanentKind::NotWhitelisted,
            PermanentKind::WrongVersion,
            PermanentKind::AccountBanned,
            PermanentKind::MultiplayerDisabled,
        ])
        .prop_map(|kind| FailReason::Permanent { kind }),
        Just(FailReason::Auth),
        Just(FailReason::SessionDenied),
        Just(FailReason::CrashLoop),
    ]
}

pub(crate) fn bot_state() -> impl Strategy<Value = BotState> {
    prop_oneof![
        Just(BotState::Stopped),
        (attempt(), any::<bool>())
            .prop_map(|(attempt, fresh)| BotState::AwaitingSession { attempt, fresh }),
        (attempt(), any::<bool>()).prop_map(|(attempt, auth_retried)| BotState::Connecting {
            attempt,
            auth_retried
        }),
        (time(), attempt()).prop_map(|(since, attempt)| BotState::Online { since, attempt }),
        (attempt(), any::<bool>())
            .prop_map(|(attempt, fresh)| BotState::Backoff { attempt, fresh }),
        pause_reason().prop_map(|reason| BotState::Paused { reason }),
        fail_reason().prop_map(|reason| BotState::Failed { reason }),
        any::<bool>().prop_map(|restart| BotState::Stopping { restart }),
    ]
}

pub(crate) fn sticky_state() -> impl Strategy<Value = StickyState> {
    prop_oneof![
        pause_reason().prop_map(StickyState::Paused),
        fail_reason().prop_map(StickyState::Failed),
    ]
}

pub(crate) fn bot_notification() -> impl Strategy<Value = BotNotification> {
    prop_oneof![
        pause_reason().prop_map(|reason| BotNotification::Paused { reason }),
        fail_reason().prop_map(|reason| BotNotification::Failed { reason }),
    ]
}

/// Any text, including what the sanitizer strips: `§`, controls, bidi and
/// invisible characters, line breaks, and text over the caps.
pub(crate) fn raw_text() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        "[a-z §\n\t\u{202E}\u{200B}\u{7}]{0,40}",
        "[a-z]{1000,1100}",
    ]
}

pub(crate) fn disconnect_reason() -> impl Strategy<Value = DisconnectReason> {
    prop_oneof![
        (
            prop_oneof![
                Just(None),
                "[a-z0-9._-]{1,40}".prop_map(Some),
                any::<String>().prop_map(Some),
            ],
            raw_text(),
        )
            .prop_map(|(key, message)| DisconnectReason::kicked(key.as_deref(), &message)),
        Just(DisconnectReason::ConnectionClosed),
        select(vec![
            ConnectFailure::Refused,
            ConnectFailure::TimedOut,
            ConnectFailure::Unresolvable,
            ConnectFailure::HostUnavailable,
            ConnectFailure::Other,
            ConnectFailure::Blocked,
        ])
        .prop_map(|failure| DisconnectReason::ConnectFailed { failure }),
        Just(DisconnectReason::AuthRejected),
        select(vec![
            AccountRestriction::Banned,
            AccountRestriction::MultiplayerDisabled
        ])
        .prop_map(|restriction| DisconnectReason::AccountRestricted { restriction }),
        select(vec![
            SessionServerFailure::Unreachable,
            SessionServerFailure::RateLimited,
            SessionServerFailure::TimedOut,
            SessionServerFailure::Unexpected,
        ])
        .prop_map(|failure| DisconnectReason::SessionServerFailed { failure }),
        Just(DisconnectReason::SessionCrashed),
        Just(DisconnectReason::WatchdogTimeout),
        Just(DisconnectReason::LivenessTimeout),
        Just(DisconnectReason::RespawnFailed),
    ]
}

fn uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

pub(crate) fn incoming_chat() -> impl Strategy<Value = IncomingChat> {
    prop_oneof![
        raw_text().prop_map(|text| IncomingChat::system(&text)),
        (
            select(vec![
                PlayerChatKind::Chat,
                PlayerChatKind::Emote,
                PlayerChatKind::Whisper,
                PlayerChatKind::Announcement,
            ]),
            raw_text(),
            proptest::option::of(uuid()),
            raw_text(),
        )
            .prop_map(|(kind, name, uuid, text)| IncomingChat::player(kind, &name, uuid, &text)),
    ]
}

pub(crate) fn snapshot() -> impl Strategy<Value = BotSnapshot> {
    (
        bot_id(),
        bot_state(),
        time(),
        proptest::option::of(disconnect_reason()),
    )
        .prop_map(|(bot_id, state, since, last_disconnect)| BotSnapshot {
            bot_id,
            state,
            since,
            last_disconnect,
        })
}

/// A token as Minecraft's are: visible ASCII.
pub(crate) fn token() -> impl Strategy<Value = String> {
    prop_oneof!["[!-~]{1,64}", "[A-Za-z0-9._-]{300,400}"]
}

pub(crate) fn session_credentials() -> impl Strategy<Value = SessionCredentials> {
    prop_oneof![
        mc_username().prop_map(|username| SessionCredentials::Offline { username }),
        (
            mc_username(),
            uuid().prop_filter("not nil", |uuid| !uuid.is_nil()),
            token(),
            time()
        )
            .prop_map(
                |(username, uuid, token, expires_at)| SessionCredentials::Online {
                    username,
                    uuid,
                    access_token: SecretString::from(token),
                    expires_at,
                }
            ),
    ]
}

pub(crate) fn session_request_id() -> impl Strategy<Value = SessionRequestId> {
    (1..=u64::MAX).prop_map(|n| SessionRequestId::new(NonZeroU64::new(n).unwrap()))
}

pub(crate) fn chat_failed_reason() -> impl Strategy<Value = ChatFailedReason> {
    select(vec![
        ChatFailedReason::Disconnected,
        ChatFailedReason::ChatUnavailable,
        ChatFailedReason::TimedOut,
        ChatFailedReason::SessionBusy,
        ChatFailedReason::NotInWorld,
        ChatFailedReason::NotOnline,
        ChatFailedReason::QueueFull,
        ChatFailedReason::RateLimited,
        ChatFailedReason::UnknownBot,
        ChatFailedReason::AgentBusy,
        ChatFailedReason::ShuttingDown,
    ])
}

fn bot_event_kind() -> impl Strategy<Value = BotEventKind> {
    prop_oneof![
        Just(BotEventKind::Died),
        incoming_chat().prop_map(BotEventKind::ChatReceived),
        chat_request_id().prop_map(|request| BotEventKind::ChatSent { request }),
        (chat_request_id(), chat_failed_reason())
            .prop_map(|(request, reason)| BotEventKind::ChatFailed { request, reason }),
        chat_message().prop_map(BotEventKind::ModeChatSent),
        bot_notification().prop_map(BotEventKind::Alert),
        Just(BotEventKind::Removed),
        script_state().prop_map(BotEventKind::ScriptStatus),
        script_log_line().prop_map(BotEventKind::ScriptLog),
    ]
}

/// Snapshots of distinct bots, as a hello carries them.
fn running() -> impl Strategy<Value = Vec<BotSnapshot>> {
    vec(snapshot(), 0..=4).prop_map(|mut snapshots| {
        snapshots.sort_by_key(|snapshot| snapshot.bot_id);
        snapshots.dedup_by_key(|snapshot| snapshot.bot_id);
        snapshots
    })
}

pub(crate) fn hello() -> impl Strategy<Value = Hello> {
    (
        "[A-Za-z0-9][A-Za-z0-9._-]{0,20}",
        "[0-9]{1,2}\\.[0-9]{1,2}\\.[0-9]{1,2}(-rc\\.[0-9])?",
        any::<u32>(),
        1..=1000_u32,
        running(),
        any::<u32>(),
    )
        .prop_map(
            |(name, version, protocol_version, capacity, running, script_api)| Hello {
                name: name.parse().unwrap(),
                agent_version: version.parse().unwrap(),
                protocol_version,
                capacity: AgentCapacity::try_from(capacity).unwrap(),
                running,
                script_api,
            },
        )
}

/// An item as a script reads it, without its data components: what crosses
/// in a screen's answer.
fn seen_item() -> impl Strategy<Value = Item> {
    (
        select(vec![
            "minecraft:potion",
            "minecraft:stone",
            "minecraft:diamond_sword",
            "minecraft:enchanted_book",
        ]),
        1..=64_u32,
        "[ -~]{0,80}",
        vec("[ -~]{0,40}", 0..=20),
        prop::option::of(0..=2000_u32),
        vec(
            (
                select(vec!["minecraft:sharpness", "minecraft:mending"]),
                -5..=300_i32,
            ),
            0..=3,
        ),
    )
        .prop_map(|(id, count, name, lore, durability, enchantments)| {
            Item::new(
                Identifier::try_from(id).unwrap(),
                NonZeroU32::new(count).unwrap(),
                ItemData::new(Vec::new()),
            )
            .with_durability(durability)
            .named(&name)
            .with_lore(&lore)
            .with_potion(Some("minecraft:turtle_master"))
            .with_enchantments(enchantments.iter().map(|(id, level)| (*id, *level)))
            .with_stored_enchantments(enchantments.iter().map(|(id, level)| (*id, *level)))
        })
}

/// `count` slots, some of them empty.
fn seen_items(count: u16) -> impl Strategy<Value = Vec<Option<Item>>> {
    vec(prop::option::weighted(0.3, seen_item()), usize::from(count))
}

/// What a bot sees, as it crosses: any inventory, maybe a screen of any
/// kind with its cursor, maybe a sign editor.
pub(crate) fn seen() -> impl Strategy<Value = Seen> {
    let screen = (
        select(ScreenKind::ALL.to_vec()),
        "[ -~]{0,140}",
        prop::option::of(seen_item()),
    )
        .prop_flat_map(|(kind, title, cursor)| {
            (Just((kind, title, cursor)), seen_items(kind.slots()))
        })
        .prop_map(|((kind, title, cursor), items)| Screen::shown(kind, &title, items, cursor));
    let sign = (any::<(i32, i32, i32)>(), any::<bool>()).prop_map(|((x, y, z), front)| SeenSign {
        at: BlockPosition { x, y, z },
        side: if front {
            SignSide::Front
        } else {
            SignSide::Back
        },
    });
    (
        seen_items(46),
        prop::option::of(screen),
        prop::option::of(sign),
    )
        .prop_map(|(inventory, screen, sign)| Seen {
            inventory: BotInventory::new(inventory),
            screen,
            sign,
        })
}

fn screen_request_id() -> impl Strategy<Value = ScreenRequestId> {
    (1..=u64::MAX).prop_map(|n| ScreenRequestId::new(NonZeroU64::new(n).unwrap()))
}

/// A screen request's answer: what the bot sees, or each reason it can't.
pub(crate) fn screen_answer() -> impl Strategy<Value = ScreenAnswer> {
    (
        screen_request_id(),
        bot_id(),
        prop_oneof![
            seen().prop_map(|seen| ScreenOutcome::Seen(Box::new(seen))),
            select(ScreenUnseen::ALL.to_vec()).prop_map(ScreenOutcome::Unseen),
        ],
    )
        .prop_map(|(id, bot_id, outcome)| ScreenAnswer {
            id,
            bot_id,
            outcome,
        })
}

pub(crate) fn agent_message() -> impl Strategy<Value = AgentMessage> {
    prop_oneof![
        hello().prop_map(AgentMessage::Hello),
        Just(AgentMessage::Heartbeat),
        snapshot().prop_map(AgentMessage::Status),
        (bot_id(), time(), bot_event_kind())
            .prop_map(|(bot_id, at, kind)| AgentMessage::Event(BotEvent { bot_id, at, kind })),
        (session_request_id(), bot_id(), any::<bool>()).prop_map(|(id, bot_id, fresh)| {
            AgentMessage::SessionRequest(SessionRequest { id, bot_id, fresh })
        }),
        screen_answer().prop_map(AgentMessage::Screen),
    ]
}

pub(crate) fn assignment() -> impl Strategy<Value = Assignment> {
    (bot_spec(), proptest::option::of(sticky_state()))
        .prop_map(|(spec, restore)| Assignment { spec, restore })
}

pub(crate) fn server_message() -> impl Strategy<Value = ServerMessage> {
    prop_oneof![
        (1_000..=300_000_u64, agent_id()).prop_map(|(millis, agent_id)| ServerMessage::Welcome(
            Welcome {
                heartbeat_interval: Duration::from_millis(millis),
                agent_id,
            }
        )),
        vec(assignment(), 0..=3).prop_map(|mut assignments| {
            assignments.sort_by_key(|assignment| assignment.spec.id);
            assignments.dedup_by_key(|assignment| assignment.spec.id);
            ServerMessage::Reconcile(assignments)
        }),
        assignment().prop_map(ServerMessage::Assign),
        bot_id().prop_map(ServerMessage::Unassign),
        bot_spec().prop_map(ServerMessage::Update),
        (chat_request_id(), bot_id(), chat_message()).prop_map(|(request, bot_id, message)| {
            ServerMessage::SendChat(SendChat {
                request,
                bot_id,
                message,
            })
        }),
        (session_request_id(), bot_id(), session_credentials()).prop_map(
            |(id, bot_id, credentials)| ServerMessage::SessionGrant(SessionGrant {
                id,
                bot_id,
                credentials,
            })
        ),
        (session_request_id(), bot_id(), any::<bool>()).prop_map(|(id, bot_id, retryable)| {
            ServerMessage::SessionDenied(SessionDenied {
                id,
                bot_id,
                retryable,
            })
        }),
        select(REJECT_REASONS.to_vec()).prop_map(ServerMessage::Reject),
        (bot_id(), select(Lifecycle::ALL.to_vec())).prop_map(|(bot_id, command)| {
            ServerMessage::Lifecycle(LifecycleCommand { bot_id, command })
        }),
        (screen_request_id(), bot_id())
            .prop_map(|(id, bot_id)| ServerMessage::ReadScreen(ScreenRequest { id, bot_id })),
    ]
}
