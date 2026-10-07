//! Pure mapping from azalea's `Event` to what it means for the session
//! ([`Mapped`]), before the bridge applies the delivery rules.

use std::io;

use azalea::chat::ChatPacket;
use azalea::protocol::connect::ConnectionError;
use azalea::protocol::packets::game::c_player_chat::DirectChatType;
use azalea::registry::data::{ChatKind as RegistryChatKind, ChatKindKey};
use azalea::registry::{DataRegistry, Holder};
use azalea::{Event, FormattedText};
use fleet_core::chat::{ChatKind, IncomingChat, PlayerChatKind};
use fleet_core::disconnect::{ConnectFailure, DisconnectReason};
use fleet_core::mc::SessionEvent;
use tracing::debug;

use super::render::{render, render_plain};

/// What an azalea event means for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mapped {
    /// A game tick: stamps tick liveness.
    Tick,
    /// A sign of life from the server, such as a `KeepAlive`: stamps packet
    /// liveness.
    Packet,
    /// The bot spawned in the world. The first one is `Joined`.
    Spawn,
    /// The bot died. azalea can report one death twice (ADR-0008 §4).
    Death,
    /// A chat message, already sanitized.
    Chat(IncomingChat),
    /// An action-bar message: a status display, not chat, so it's dropped and
    /// counted (ADR-0011).
    ActionBar,
    /// The session ended.
    Terminal(Terminal),
    /// Nothing the session reports.
    Ignored,
}

/// A terminal session event. The bridge delivers at most one per session:
/// the first one wins, whether azalea reported it or another source injected
/// it (the account's auth result, the connect timeout, `AppExit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Terminal {
    /// The session ended after it connected.
    Disconnected(DisconnectReason),
    /// Connecting failed.
    ConnectionFailed(ConnectFailure),
}

impl From<Terminal> for SessionEvent {
    fn from(terminal: Terminal) -> Self {
        match terminal {
            Terminal::Disconnected(reason) => Self::Disconnected(reason),
            Terminal::ConnectionFailed(failure) => Self::ConnectionFailed(failure),
        }
    }
}

/// Maps one azalea event.
///
/// `Event` is `#[non_exhaustive]`, so a variant that a later azalea adds is
/// ignored and logged at `debug`; every azalea bump re-checks the variants
/// against this mapping (ADR-0003).
pub(crate) fn map_event(event: &Event) -> Mapped {
    match event {
        Event::Tick => Mapped::Tick,
        Event::KeepAlive(_) => Mapped::Packet,
        Event::Spawn => Mapped::Spawn,
        Event::Death(_) => Mapped::Death,
        Event::Chat(packet) => map_chat(packet),
        Event::Disconnect(reason) => {
            Mapped::Terminal(Terminal::Disconnected(disconnect_reason(reason.as_ref())))
        }
        Event::ConnectionFailed(error) => {
            Mapped::Terminal(Terminal::ConnectionFailed(connect_failure(error)))
        }
        Event::Init
        | Event::Login
        | Event::AddPlayer(_)
        | Event::RemovePlayer(_)
        | Event::UpdatePlayer(_)
        | Event::ReceiveChunk(_) => Mapped::Ignored,
        other => {
            debug!(event = ?other, "ignored an azalea event this mapping doesn't know");
            Mapped::Ignored
        }
    }
}

/// Maps a chat packet (ADR-0008 §7, ADR-0011).
///
/// The sender comes only from player packets: for system messages, azalea
/// guesses one from the text, which anyone can fake with `tellraw`. The text
/// is what the vanilla client shows, without the chat-type decoration; a
/// system message keeps its whole text. Both are rendered within the
/// renderer's budget. Text it cut off is marked truncated; a cut-off sender
/// name is only capped, like any long name.
fn map_chat(packet: &ChatPacket) -> Mapped {
    let (kind, sender, text) = match packet {
        ChatPacket::System(system) if system.overlay => return Mapped::ActionBar,
        ChatPacket::System(system) => (ChatKind::System, None, render(&system.content)),
        ChatPacket::Player(player) => (
            player_kind(&player.chat_type.chat_type).into(),
            Some((render(&player.chat_type.name), Some(player.sender))),
            // The unsigned content if the server sent one, else the signed text.
            player
                .unsigned_content
                .as_ref()
                .map_or_else(|| render_plain(&player.body.content), render),
        ),
        ChatPacket::Disguised(disguised) => (
            player_kind(&disguised.chat_type.chat_type).into(),
            Some((render(&disguised.chat_type.name), None)),
            render(&disguised.message),
        ),
    };
    let sender = sender
        .as_ref()
        .map(|(name, uuid)| (name.text.as_str(), *uuid));
    // `from_parts` only refuses a system message with a sender, or a player
    // message without one, and the arms above build neither.
    IncomingChat::from_parts(kind, sender, &text.text, text.stopped)
        .map_or(Mapped::Ignored, Mapped::Chat)
}

/// The kind of a player message, from its chat-type registry id.
///
/// The server assigns the ids. They're read in vanilla's registry order,
/// which azalea's `ChatKindKey::ALL` follows, so a server whose data packs
/// reorder chat types can mislabel a kind; the text and the sender don't
/// depend on it (ADR-0011). Unknown ids and inline chat types count as chat.
fn player_kind(chat_type: &Holder<RegistryChatKind, DirectChatType>) -> PlayerChatKind {
    let Holder::Reference(kind) = chat_type else {
        return PlayerChatKind::Chat;
    };
    let key = usize::try_from(kind.protocol_id())
        .ok()
        .and_then(|id| ChatKindKey::ALL.get(id));
    match key {
        Some(ChatKindKey::EmoteCommand) => PlayerChatKind::Emote,
        Some(ChatKindKey::MsgCommandIncoming) => PlayerChatKind::Whisper,
        Some(ChatKindKey::SayCommand) => PlayerChatKind::Announcement,
        // Chat, the echo of a whisper the bot sent, team chat, and ids beyond
        // the vanilla registry.
        _ => PlayerChatKind::Chat,
    }
}

/// A kick reason, or a closed connection when there's none (ADR-0008 §6).
/// Only a translatable reason has a key; the core classifies by that
/// top-level key, never by the text. The text is only for display, so the
/// renderer's budget cutting it off doesn't change the class.
fn disconnect_reason(reason: Option<&FormattedText>) -> DisconnectReason {
    let Some(reason) = reason else {
        return DisconnectReason::ConnectionClosed;
    };
    let key = match reason {
        FormattedText::Translatable(translatable) => Some(translatable.key.as_str()),
        FormattedText::Text(_) => None,
    };
    DisconnectReason::kicked(key, &render(reason).text)
}

/// Why connecting failed, from the IO error's kind (ADR-0008 §5).
fn connect_failure(error: &ConnectionError) -> ConnectFailure {
    let ConnectionError::Io(error) = error;
    match error.kind() {
        io::ErrorKind::ConnectionRefused => ConnectFailure::Refused,
        io::ErrorKind::TimedOut => ConnectFailure::TimedOut,
        _ => ConnectFailure::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use azalea::core::entity_id::MinecraftEntityId;
    use azalea::core::position::ChunkPos;
    use azalea::protocol::packets::game::c_player_chat::{
        ChatTypeBound, ChatTypeDecoration, FilterMask, PackedLastSeenMessages,
        PackedSignedMessageBody,
    };
    use azalea::protocol::packets::game::{
        ClientboundDisguisedChat, ClientboundPlayerChat, ClientboundPlayerCombatKill,
        ClientboundSystemChat,
    };
    use azalea::protocol::simdnbt::owned::NbtCompound;
    use fleet_core::chat::{ChatKind as FleetChatKind, ChatSender};
    use fleet_core::disconnect::{ConflictKind, DisconnectClass, Kick, PermanentKind};
    use rstest::rstest;

    const PLAYER_UUID: &str = "0123e567-89ab-4def-8123-456789abcdef";

    /// A registry chat type with the protocol id `id`.
    fn kind(id: u32) -> Holder<RegistryChatKind, DirectChatType> {
        Holder::Reference(RegistryChatKind::new_raw(id))
    }

    fn bound(
        chat_type: Holder<RegistryChatKind, DirectChatType>,
        name: FormattedText,
    ) -> ChatTypeBound {
        ChatTypeBound {
            chat_type,
            name,
            target_name: None,
        }
    }

    fn system(text: &str, overlay: bool) -> ChatPacket {
        ChatPacket::System(Arc::new(ClientboundSystemChat {
            content: FormattedText::from(text),
            overlay,
        }))
    }

    fn player(kind_id: u32, name: &str, signed: &str, unsigned: Option<&str>) -> ChatPacket {
        player_named(kind_id, FormattedText::from(name), signed, unsigned)
    }

    fn player_named(
        kind_id: u32,
        name: FormattedText,
        signed: &str,
        unsigned: Option<&str>,
    ) -> ChatPacket {
        ChatPacket::Player(Arc::new(ClientboundPlayerChat {
            global_index: 0,
            sender: PLAYER_UUID.parse().unwrap(),
            index: 0,
            signature: None,
            body: PackedSignedMessageBody {
                content: signed.to_owned(),
                timestamp: 0,
                salt: 0,
                last_seen: PackedLastSeenMessages { entries: vec![] },
            },
            unsigned_content: unsigned.map(FormattedText::from),
            filter_mask: FilterMask::PassThrough,
            chat_type: bound(kind(kind_id), name),
        }))
    }

    fn disguised(
        chat_type: Holder<RegistryChatKind, DirectChatType>,
        name: &str,
        text: &str,
    ) -> ChatPacket {
        ChatPacket::Disguised(Arc::new(ClientboundDisguisedChat {
            message: FormattedText::from(text),
            chat_type: bound(chat_type, FormattedText::from(name)),
        }))
    }

    /// The chat message `packet` maps to.
    fn chat(packet: &ChatPacket) -> IncomingChat {
        match map_event(&Event::Chat(packet.clone())) {
            Mapped::Chat(chat) => chat,
            other => panic!("expected chat, got {other:?}"),
        }
    }

    /// A kick reason as the server sends it: JSON text.
    fn reason(json: &str) -> FormattedText {
        serde_json::from_str(json).unwrap()
    }

    fn disconnected(event: &Event) -> DisconnectReason {
        match map_event(event) {
            Mapped::Terminal(Terminal::Disconnected(reason)) => reason,
            other => panic!("expected a disconnect, got {other:?}"),
        }
    }

    // --- Chat ---

    #[test]
    fn system_chat_has_no_sender_even_when_its_text_names_one() {
        let chat = chat(&system("<AfkBot7> I am not really AfkBot7", false));

        assert_eq!(chat.kind(), FleetChatKind::System);
        assert_eq!(chat.sender(), None);
        assert_eq!(chat.text(), "<AfkBot7> I am not really AfkBot7");
    }

    #[test]
    fn system_chat_keeps_its_whole_text() {
        let chat = chat(&system("[CLAN] AfkBot2 : hello there", false));

        assert_eq!(chat.text(), "[CLAN] AfkBot2 : hello there");
    }

    #[test]
    fn action_bar_messages_are_not_chat() {
        let mapped = map_event(&Event::Chat(system("Mana: 20/20", true)));

        assert_eq!(mapped, Mapped::ActionBar);
    }

    #[test]
    fn player_chat_takes_the_sender_from_the_packet() {
        let chat = chat(&player(0, "AfkBot2", "hello", None));

        assert_eq!(chat.kind(), FleetChatKind::Chat);
        let sender = chat.sender().unwrap();
        assert_eq!(sender.name(), "AfkBot2");
        assert_eq!(sender.uuid(), Some(PLAYER_UUID.parse().unwrap()));
    }

    #[test]
    fn player_chat_shows_the_unsigned_content_when_there_is_one() {
        let chat = chat(&player(
            0,
            "AfkBot2",
            "signed text",
            Some("server's version"),
        ));

        assert_eq!(chat.text(), "server's version");
    }

    #[test]
    fn player_chat_falls_back_to_the_signed_content() {
        let chat = chat(&player(0, "AfkBot2", "signed text", None));

        assert_eq!(chat.text(), "signed text");
    }

    #[test]
    fn player_chat_text_has_no_chat_type_decoration() {
        let chat = chat(&player(0, "AfkBot2", "hello", None));

        assert_eq!(chat.text(), "hello");
    }

    #[test]
    fn disguised_chat_has_a_sender_without_a_uuid() {
        let chat = chat(&disguised(kind(4), "Server", "Hello from the console"));

        assert_eq!(chat.kind(), FleetChatKind::Announcement);
        let sender = chat.sender().unwrap();
        assert_eq!(sender.name(), "Server");
        assert_eq!(sender.uuid(), None);
        assert_eq!(chat.text(), "Hello from the console");
    }

    #[rstest]
    #[case::chat(0, FleetChatKind::Chat)]
    #[case::emote(1, FleetChatKind::Emote)]
    #[case::whisper_incoming(2, FleetChatKind::Whisper)]
    #[case::whisper_echo(3, FleetChatKind::Chat)]
    #[case::say(4, FleetChatKind::Announcement)]
    #[case::team_incoming(5, FleetChatKind::Chat)]
    #[case::team_echo(6, FleetChatKind::Chat)]
    #[case::unknown(7, FleetChatKind::Chat)]
    #[case::far_out(999, FleetChatKind::Chat)]
    fn chat_kinds_follow_the_vanilla_registry_order(
        #[case] id: u32,
        #[case] expected: FleetChatKind,
    ) {
        assert_eq!(chat(&disguised(kind(id), "AfkBot2", "hi")).kind(), expected);
    }

    #[test]
    fn inline_chat_types_count_as_chat() {
        let decoration = ChatTypeDecoration {
            translation_key: "chat.type.emote".to_owned(),
            parameters: vec![],
            style: NbtCompound::default(),
        };
        let inline = Holder::Direct(DirectChatType {
            chat: decoration.clone(),
            narration: decoration,
        });

        assert_eq!(
            chat(&disguised(inline, "AfkBot2", "hi")).kind(),
            FleetChatKind::Chat
        );
    }

    #[test]
    fn chat_text_and_sender_are_sanitized() {
        let chat = chat(&player(0, "Afk\u{7}Bot2", "a§cb\u{202E}c", None));

        assert_eq!(chat.text(), "abc");
        assert_eq!(chat.sender().unwrap().name(), "AfkBot2");
    }

    // --- Disconnects and failed connects ---

    #[rstest]
    #[case::banned(
        r#"{"translate":"multiplayer.disconnect.banned"}"#,
        DisconnectClass::Permanent { kind: PermanentKind::Banned }
    )]
    #[case::duplicate_login(
        r#"{"translate":"multiplayer.disconnect.duplicate_login"}"#,
        DisconnectClass::Conflict { kind: ConflictKind::DuplicateLogin }
    )]
    #[case::kicked(
        r#"{"translate":"multiplayer.disconnect.kicked"}"#,
        DisconnectClass::Transient
    )]
    fn translatable_kick_reasons_keep_their_key(
        #[case] json: &str,
        #[case] class: DisconnectClass,
    ) {
        let reason = disconnected(&Event::Disconnect(Some(reason(json))));

        assert_eq!(reason.classify(), class);
        let DisconnectReason::Kicked(kick) = reason else {
            panic!("expected a kick");
        };
        assert!(json.contains(kick.key().unwrap().as_str()));
    }

    #[rstest]
    #[case::text_component(r#"{"text":"You were kicked by a plugin"}"#)]
    #[case::plain_string(r#""You were kicked by a plugin""#)]
    fn plain_text_kick_reasons_have_no_key(#[case] json: &str) {
        let reason = disconnected(&Event::Disconnect(Some(reason(json))));

        let DisconnectReason::Kicked(kick) = reason else {
            panic!("expected a kick");
        };
        assert_eq!(kick.key(), None);
        assert_eq!(kick.message(), "You were kicked by a plugin");
    }

    #[test]
    fn disconnect_without_a_reason_is_a_closed_connection() {
        assert_eq!(
            disconnected(&Event::Disconnect(None)),
            DisconnectReason::ConnectionClosed
        );
    }

    #[rstest]
    #[case::refused(io::ErrorKind::ConnectionRefused, ConnectFailure::Refused)]
    #[case::timed_out(io::ErrorKind::TimedOut, ConnectFailure::TimedOut)]
    #[case::unreachable(io::ErrorKind::HostUnreachable, ConnectFailure::Other)]
    #[case::reset(io::ErrorKind::ConnectionReset, ConnectFailure::Other)]
    fn failed_connects_map_by_io_error_kind(
        #[case] kind: io::ErrorKind,
        #[case] expected: ConnectFailure,
    ) {
        let error = Arc::new(ConnectionError::Io(io::Error::from(kind)));

        assert_eq!(
            map_event(&Event::ConnectionFailed(error)),
            Mapped::Terminal(Terminal::ConnectionFailed(expected))
        );
    }

    // --- Bounded rendering of server text ---

    /// A translation that repeats its argument four times, nested `depth`
    /// deep around `innermost`: azalea's own rendering grows as 4^depth.
    fn nested(depth: usize, innermost: &str) -> String {
        (0..depth).fold(serde_json::to_string(innermost).unwrap(), |inner, _| {
            format!(r#"{{"translate":"%1$s%1$s%1$s%1$s","with":[{inner}]}}"#)
        })
    }

    fn system_text(content: FormattedText) -> ChatPacket {
        ChatPacket::System(Arc::new(ClientboundSystemChat {
            content,
            overlay: false,
        }))
    }

    #[test]
    fn nested_system_chat_is_cut_off_and_marked_truncated() {
        let chat = chat(&system_text(reason(&nested(12, "x"))));

        assert!(chat.is_truncated());
        assert_eq!(chat.text(), "x".repeat(IncomingChat::MAX_TEXT_LEN));
    }

    #[test]
    fn nested_empty_system_chat_is_marked_truncated() {
        let chat = chat(&system_text(reason(&nested(12, ""))));

        assert!(chat.is_truncated());
        assert_eq!(chat.text(), "");
    }

    #[test]
    fn nested_sender_name_is_capped_without_truncating_the_text() {
        let chat = chat(&player_named(0, reason(&nested(12, "x")), "hello", None));

        assert_eq!(
            chat.sender().unwrap().name(),
            "x".repeat(ChatSender::MAX_NAME_LEN)
        );
        assert_eq!(chat.text(), "hello");
        assert!(!chat.is_truncated());
    }

    #[rstest]
    #[case::cut_off_text("x")]
    #[case::empty_text("")]
    fn nested_args_keep_a_ban_permanent(#[case] innermost: &str) {
        let json = format!(
            r#"{{"translate":"multiplayer.disconnect.banned.reason","with":[{}]}}"#,
            nested(12, innermost)
        );

        let reason = disconnected(&Event::Disconnect(Some(reason(&json))));

        assert_eq!(
            reason.classify(),
            DisconnectClass::Permanent {
                kind: PermanentKind::Banned
            }
        );
        let DisconnectReason::Kicked(kick) = reason else {
            panic!("expected a kick");
        };
        assert!(
            kick.message()
                .starts_with("You are banned from this server.")
        );
        assert!(kick.message().chars().count() <= Kick::MAX_MESSAGE_LEN);
    }

    // --- Other events ---

    #[rstest]
    #[case::tick(Event::Tick, Mapped::Tick)]
    #[case::keep_alive(Event::KeepAlive(42), Mapped::Packet)]
    #[case::spawn(Event::Spawn, Mapped::Spawn)]
    #[case::death_by_health(Event::Death(None), Mapped::Death)]
    #[case::death_by_kill(Event::Death(Some(Arc::new(ClientboundPlayerCombatKill {
        player_id: MinecraftEntityId(1),
        message: FormattedText::from("AfkBot1 was slain"),
    }))), Mapped::Death)]
    #[case::init(Event::Init, Mapped::Ignored)]
    #[case::login(Event::Login, Mapped::Ignored)]
    #[case::chunk(Event::ReceiveChunk(ChunkPos::new(0, 0)), Mapped::Ignored)]
    fn events_map_to_what_they_mean_for_the_session(
        #[case] event: Event,
        #[case] expected: Mapped,
    ) {
        assert_eq!(map_event(&event), expected);
    }

    #[test]
    fn terminal_events_become_session_events() {
        assert_eq!(
            SessionEvent::from(Terminal::Disconnected(DisconnectReason::AuthRejected)),
            SessionEvent::Disconnected(DisconnectReason::AuthRejected)
        );
        assert_eq!(
            SessionEvent::from(Terminal::ConnectionFailed(ConnectFailure::TimedOut)),
            SessionEvent::ConnectionFailed(ConnectFailure::TimedOut)
        );
    }
}
