//! What a player's lines arrive as, on a real server (Plan.md P15.32;
//! ADR-0037, C2, C3, C9).
//!
//! A script's chat patterns read only the server's own lines. This
//! scenario has a second player try to put a reply's exact text,
//! `Balance: $500`, in front of the bot in every way a player has on a
//! vanilla server, and holds each line the bot receives to fleet-core's
//! two guards as they are: whether the line is a candidate at all
//! (`candidate_line`), and whether the pattern fits it (`ChatPattern`).
//!
//! | The line | Arrives as | A candidate | Fits the pattern |
//! |---|---|---|---|
//! | The player's chat | a player's chat | no | — |
//! | The player's whisper | a player's whisper | no | — |
//! | The player's `/me` | a player's emote | no | — |
//! | The death line of an item renamed to the text | a system line | **yes** | no: the text is inside the line |
//! | The same with line breaks around the text | a system line of three lines | no | — |
//! | The server's own `tellraw` | a system line | yes | yes |
//!
//! So a death line is stopped by the pattern's anchoring, not by the
//! candidate rule, and one with a line break by the candidate rule.

use fleet_core::chat::{ChatFormats, ChatKind, ChatMessage, ChatSender, IncomingChat};
use fleet_core::mc::{SessionEvent, SessionHandle as _};
use fleet_core::net::TargetPolicy;
use fleet_core::script::{
    CaptureDraft, CaptureKind, Captured, ChatPattern, DecimalMark, PatternDraft, When,
    candidate_line,
};
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};

use crate::harness::{Mode, Server, bot, connect, offline, wait_for, within};

const READER: &str = "AfkBot1";
const PLAYER: &str = "AfkBot2";
/// The reply's exact text, which the reader's pattern fits.
const REPLY: &str = "Balance: $500";

/// The balance example's reply pattern.
fn pattern() -> ChatPattern {
    ChatPattern::try_from(PatternDraft {
        key: "balance".to_owned(),
        when: When::Reply,
        text: "Balance: ${amount}".to_owned(),
        captures: vec![CaptureDraft {
            name: "amount".to_owned(),
            kind: CaptureKind::Amount {
                min: 0.0,
                max: 1_000_000_000.0,
                decimal_mark: DecimalMark::Point,
            },
        }],
    })
    .expect("the balance example's pattern")
}

/// The next chat the reader receives that `wanted` picks.
async fn next_line(
    events: &mut McEvents,
    what: &str,
    wanted: impl Fn(&IncomingChat) -> bool,
) -> IncomingChat {
    let event = wait_for(
        events,
        what,
        |event| matches!(event, SessionEvent::Chat(chat) if wanted(chat)),
    )
    .await;
    let SessionEvent::Chat(chat) = event else {
        unreachable!("only chat is picked");
    };
    eprintln!(
        "MEASURED {what}: {:?} from {:?}: {:?}",
        chat.kind(),
        chat.sender().map(ChatSender::name),
        chat.text()
    );
    chat
}

/// What the two guards make of `chat`: the line it is a candidate with,
/// and the values the pattern reads from it.
fn judged(chat: &IncomingChat) -> (Option<&str>, Option<Vec<Captured>>) {
    let line = candidate_line(chat, &ChatFormats::default());
    let values = line
        .and_then(|line| pattern().matches(line))
        .map(|values| values.into_iter().map(|(_, value)| value).collect());
    (line, values)
}

fn message(text: &str) -> ChatMessage {
    text.parse().expect("a chat message")
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_chat_lines_scenario() {
    // The container is published on 127.0.0.1, which only the offline
    // scenario checks (ADR-0019, S69).
    let connector =
        AzaleaConnector::new(&McConfig::default()).with_targets(TargetPolicy::Unchecked);
    let server = Server::start(Mode::Offline).await;
    let (reader, mut lines) = connect(&connector, &server, bot(1), offline(READER)).await;
    wait_for(&mut lines, "the reader's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    let (player, mut players) = connect(&connector, &server, bot(2), offline(PLAYER)).await;
    wait_for(&mut players, "the player's join", |event| {
        *event == SessionEvent::Joined
    })
    .await;
    // The pattern does fit the text, read alone.
    assert_eq!(
        pattern()
            .matches(REPLY)
            .map(|values| values.into_iter().map(|(_, value)| value).collect()),
        Some(vec![Captured::Amount(500.0)])
    );

    // The player's join line is the server's own: a candidate, and no fit.
    let joined = next_line(&mut lines, "the join line", |chat| {
        chat.text().contains(PLAYER)
    })
    .await;
    assert_eq!(joined.kind(), ChatKind::System);
    assert_eq!(
        judged(&joined),
        (Some("AfkBot2 joined the game"), None),
        "{joined:?}"
    );

    // Chat, a whisper and `/me`: a player's messages, never candidates.
    for (what, sent, kind) in [
        ("the player's chat", REPLY.to_owned(), ChatKind::Chat),
        (
            "the player's whisper",
            format!("/msg {READER} {REPLY}"),
            ChatKind::Whisper,
        ),
        ("the player's /me", format!("/me {REPLY}"), ChatKind::Emote),
    ] {
        assert_eq!(
            within(what, player.send_chat(message(&sent))).await,
            Ok(()),
            "{what}"
        );
        let line = next_line(&mut lines, what, |chat| chat.text().contains(REPLY)).await;
        assert_eq!(line.kind(), kind, "{what}");
        assert_eq!(line.sender().map(ChatSender::name), Some(PLAYER), "{what}");
        assert_eq!(line.text(), REPLY, "{what}");
        assert_eq!(judged(&line), (None, None), "{what}");
    }

    the_death_lines_of_a_renamed_item(&server, &player, &mut players, &mut lines).await;

    // The server's own line is read.
    server.rcon(&format!("tellraw @a \"{REPLY}\"")).await;
    let own = next_line(&mut lines, "the server's own line", |chat| {
        chat.text() == REPLY
    })
    .await;
    assert_eq!(own.kind(), ChatKind::System);
    assert_eq!(own.sender(), None);
    assert_eq!(
        judged(&own),
        (Some(REPLY), Some(vec![Captured::Amount(500.0)])),
        "{own:?}"
    );

    within("disconnect", player.disconnect()).await;
    within("disconnect", reader.disconnect()).await;
}

/// The player dies to an item renamed to the reply's text, then to one
/// with line breaks around it.
async fn the_death_lines_of_a_renamed_item(
    server: &Server,
    player: &McSession,
    players: &mut McEvents,
    lines: &mut McEvents,
) {
    // The player dies to an item renamed to the text. An armour stand
    // holds it, and the death line names the item as it would a player's.
    // A peaceful world lets nothing that isn't a player hurt one ("Target
    // is invulnerable to the given damage type"), so the world is easy
    // from here on; it's morning, and no monster comes before the end.
    server.rcon("difficulty easy").await;
    let died_to = |tag: &str, name: &str| {
        [
            format!(
                "summon minecraft:armor_stand 0.5 -60 3.5 {{Tags:[\"{tag}\"],equipment:{{mainhand:{{id:\"minecraft:stick\",components:{{\"minecraft:custom_name\":\"{name}\"}}}}}}}}"
            ),
            format!("damage {PLAYER} 1000 minecraft:mob_attack by @e[tag={tag},limit=1]"),
        ]
    };
    for command in died_to("renamed", REPLY) {
        server.rcon(&command).await;
    }
    let death = next_line(lines, "the death line of a renamed item", |chat| {
        chat.text().contains(REPLY)
    })
    .await;
    assert_eq!(death.kind(), ChatKind::System);
    assert_eq!(death.sender(), None);
    // A system line, one line, whole: a candidate by ADR-0037's C2. The
    // renamed text is inside it, so the pattern, which must fit a whole
    // line, doesn't fit (C3).
    assert_eq!(
        judged(&death),
        (
            Some("AfkBot2 was slain by Armor Stand using [Balance: $500]"),
            None
        ),
        "{death:?}"
    );

    // The same with line breaks around the text, as an operator can name
    // an item: one of the message's lines is the reply's text exactly.
    wait_for(players, "the player's death", |event| {
        *event == SessionEvent::Died
    })
    .await;
    assert_eq!(within("the respawn", player.respawn()).await, Ok(()));
    wait_for(players, "the player's respawn", |event| {
        *event == SessionEvent::Respawned
    })
    .await;
    for command in died_to("broken", &format!("a\\n{REPLY}\\nb")) {
        server.rcon(&command).await;
    }
    let broken = next_line(
        lines,
        "the death line of an item with line breaks",
        |chat| chat.text().contains(REPLY) && chat.text() != death.text(),
    )
    .await;
    assert_eq!(broken.kind(), ChatKind::System);
    assert_eq!(
        broken.text(),
        "AfkBot2 was slain by Armor Stand using [a\nBalance: $500\nb]"
    );
    assert!(broken.text().lines().any(|line| line == REPLY));
    // A message with a line break is no candidate, not even its lines.
    assert_eq!(judged(&broken), (None, None), "{broken:?}");
}
