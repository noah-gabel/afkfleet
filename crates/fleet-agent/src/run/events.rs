//! The fleet's events in the agent's log (ADR-0014).
//!
//! In standalone mode nothing else consumes the fleet's events. One task
//! logs the chat the bots receive and the chat their modes send, and how
//! many events it missed when it fell behind, all at `debug`: chat is
//! logged only at `debug` (CLAUDE.md). The actors already log every state
//! change, so nothing else is logged here.
//!
//! Chat text is untrusted. It's already sanitized, but it keeps its line
//! breaks, so it's recorded as a plain string field, which both formats
//! escape, never with `%`, which would write it raw.

use fleet_core::chat::ChatSender;
use fleet_runtime::{FleetEvent, FleetEventKind};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Logs the chat in `events` until `cancel` fires or the fleet's event
/// channel closes.
pub(crate) async fn log_fleet_events(
    mut events: broadcast::Receiver<FleetEvent>,
    cancel: CancellationToken,
) {
    loop {
        let received = tokio::select! {
            biased;
            // Both branches are cancel-safe: `cancelled` and `recv` lose
            // nothing when the other one wins.
            () = cancel.cancelled() => return,
            received = events.recv() => received,
        };
        match received {
            Ok(event) => log(&event),
            Err(RecvError::Lagged(skipped)) => {
                debug!(skipped, "the event log fell behind; events were skipped");
            }
            Err(RecvError::Closed) => return,
        }
    }
}

/// Logs one event if it's chat.
fn log(event: &FleetEvent) {
    match &event.kind {
        FleetEventKind::ChatReceived(chat) => debug!(
            bot_id = %event.bot_id,
            kind = %chat.kind(),
            sender = chat.sender().map(ChatSender::name),
            text = chat.text(),
            truncated = chat.is_truncated(),
            "the bot received chat"
        ),
        FleetEventKind::ModeChatSent { message } => debug!(
            bot_id = %event.bot_id,
            text = message.as_str(),
            "the bot's mode sent chat"
        ),
        FleetEventKind::StateChanged(_)
        | FleetEventKind::Died
        | FleetEventKind::ChatSent { .. }
        | FleetEventKind::ChatFailed { .. }
        | FleetEventKind::Alert(_)
        | FleetEventKind::Removed => {}
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use chrono::DateTime;
    use fleet_core::bot::{BotSnapshot, BotState};
    use fleet_core::chat::{IncomingChat, PlayerChatKind};
    use fleet_core::id::BotId;
    use fleet_runtime::FleetEventKind;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;
    use crate::config::{LogConfig, LogFilter, LogFormat};
    use crate::telemetry::capture::{Capture, json_on_this_thread};

    const BOT: &str = "018bcfe5-6800-7bab-abab-abababababab";

    fn event(kind: FleetEventKind) -> FleetEvent {
        FleetEvent {
            bot_id: BOT.parse().unwrap(),
            at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            kind,
        }
    }

    fn chat(text: &str) -> FleetEvent {
        event(FleetEventKind::ChatReceived(IncomingChat::system(text)))
    }

    /// Logs everything already sent on a channel of `capacity`, then ends
    /// as the channel closes.
    async fn log_all(capacity: usize, events: Vec<FleetEvent>) {
        let (sender, receiver) = broadcast::channel(capacity);
        for event in events {
            sender.send(event).unwrap();
        }
        drop(sender);
        tokio::time::timeout(
            Duration::from_secs(1),
            log_fleet_events(receiver, CancellationToken::new()),
        )
        .await
        .expect("the logger should end when the channel closes");
    }

    #[tokio::test]
    async fn received_chat_is_logged_at_debug_with_its_kind_sender_and_text() {
        let (capture, _guard) = json_on_this_thread();
        let whisper =
            IncomingChat::player(PlayerChatKind::Whisper, "Steve", None, "meet me at spawn");

        log_all(16, vec![event(FleetEventKind::ChatReceived(whisper))]).await;

        let lines = capture.lines_with("the bot received chat");
        assert_eq!(lines.len(), 1, "{}", capture.text());
        let line = &lines[0];
        assert_eq!(line["level"], "DEBUG");
        assert_eq!(line["bot_id"], BOT);
        assert_eq!(line["kind"], "whisper");
        assert_eq!(line["sender"], "Steve");
        assert_eq!(line["text"], "meet me at spawn");
        assert_eq!(line["truncated"], false);
    }

    #[tokio::test]
    async fn a_system_message_has_no_sender() {
        let (capture, _guard) = json_on_this_thread();

        log_all(16, vec![chat("Server restarts in 5 minutes")]).await;

        let line = &capture.lines_with("the bot received chat")[0];
        assert_eq!(line["kind"], "system");
        assert!(line.get("sender").is_none());
    }

    #[tokio::test]
    async fn mode_chat_is_logged_at_debug_as_text() {
        let (capture, _guard) = json_on_this_thread();
        let message = "hello".parse().unwrap();

        log_all(16, vec![event(FleetEventKind::ModeChatSent { message })]).await;

        let lines = capture.lines_with("the bot's mode sent chat");
        assert_eq!(lines.len(), 1, "{}", capture.text());
        assert_eq!(lines[0]["level"], "DEBUG");
        assert_eq!(lines[0]["bot_id"], BOT);
        assert_eq!(lines[0]["text"], "hello");
    }

    #[tokio::test]
    async fn falling_behind_is_logged_at_debug_with_the_skipped_count() {
        let (capture, _guard) = json_on_this_thread();

        log_all(1, vec![chat("one"), chat("two"), chat("three")]).await;

        let lagged = capture.lines_with("the event log fell behind; events were skipped");
        assert_eq!(lagged.len(), 1, "{}", capture.text());
        assert_eq!(lagged[0]["level"], "DEBUG");
        assert_eq!(lagged[0]["skipped"], 2);
        let texts: Vec<_> = capture
            .lines_with("the bot received chat")
            .iter()
            .map(|line| line["text"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(texts, ["three"]);
    }

    #[tokio::test]
    async fn other_events_are_not_logged() {
        let (capture, _guard) = json_on_this_thread();
        let snapshot = BotSnapshot {
            bot_id: BOT.parse::<BotId>().unwrap(),
            state: BotState::Stopped,
            since: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            last_disconnect: None,
        };

        log_all(
            16,
            vec![
                event(FleetEventKind::StateChanged(snapshot)),
                event(FleetEventKind::Died),
                event(FleetEventKind::Removed),
            ],
        )
        .await;

        assert_eq!(capture.text(), "");
    }

    #[tokio::test]
    async fn the_logger_ends_when_cancelled() {
        let (sender, receiver) = broadcast::channel::<FleetEvent>(16);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let ended =
            tokio::time::timeout(Duration::from_secs(1), log_fleet_events(receiver, cancel)).await;

        assert!(ended.is_ok());
        drop(sender);
    }

    #[tokio::test]
    async fn a_chat_line_break_stays_on_one_pretty_line() {
        let capture = Capture::default();
        let config = LogConfig {
            format: LogFormat::Pretty,
            filter: LogFilter::try_from("debug").unwrap(),
        };
        let subscriber = tracing_subscriber::registry().with(crate::telemetry::layer(
            &config,
            false,
            capture.clone(),
        ));
        let _guard = tracing::subscriber::set_default(subscriber);

        log_all(16, vec![chat("first line\nINFO forged line")]).await;

        let text = capture.text();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains(r"first line\nINFO forged line"), "{text}");
    }
}
