//! The user's real-account check (ADR-0011): a session with a real Minecraft
//! token joins a local online-mode server and sends signed chat, then `/me`,
//! which fleet-mc refuses before sending, since azalea can't sign a
//! command's message (ADR-0020, S2; see the step). Neither the token
//! nor a chat-signing private key may reach a log, except where azalea-auth
//! logs the certificate response at TRACE (an accepted risk that P5.2 caps).
//!
//! The session's steps only record their results. The teardown and the log
//! checks always run, and failed steps are reported after them.
//!
//! **It needs the user's real credentials, so only the user runs it**, with
//! `just test-real-account`; the AI never runs it or the `manual` profile.
//! The account file, `secrets/p1.8-account.txt`, comes from the archived
//! spike's `fetch-token` (`spikes/azalea/README.md`), right before running:
//! it holds no expiry. Nothing the test prints shows the file's contents.
//! Errors name only a line number and the key expected there, and a failed
//! wait names only the step and the kinds of events the session sent.

use core::fmt;
use std::{fs, io};

use fleet_core::chat::ChatKind;
use fleet_core::disconnect::DisconnectReason;
use fleet_core::mc::{
    SessionCredentials, SessionError, SessionEvent, SessionEvents, SessionHandle,
};
use fleet_core::net::TargetPolicy;
use fleet_mc::{AzaleaConnector, McConfig, McEvents, McSession};
use fleet_testkit::log_capture;
use secrecy::{ExposeSecret as _, SecretString};
use uuid::Uuid;

use crate::harness::{Mode, Server, WITHIN, any_expiry, bot, connect, is_chat, within};

/// The account file, as `fetch-token` writes it.
const ACCOUNT_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../secrets/p1.8-account.txt"
);
/// The account file's path in the repository, for messages.
const ACCOUNT_FILE_NAME: &str = "secrets/p1.8-account.txt";
/// The chat the bot sends, signed.
const CHAT: &str = "hello from the afkfleet real-account check";
/// The `/me` command the bot tries, and the text its echo would have.
const EMOTE: &str = "/me waves from the afkfleet real-account check";
const EMOTE_TEXT: &str = "waves from the afkfleet real-account check";
/// The signed chat sent after `/me`, whose echo comes after any answer to
/// `/me` had it gone out.
const AFTER_EMOTE: &str = "still here after /me, from the afkfleet real-account check";
/// The translation key of a server's answer to an unsigned command that
/// needs signed arguments.
const REJECTED_COMMAND: &str = "chat.disabled.invalid_command_signature";
/// What every PEM private key's header contains.
const PRIVATE_KEY: &str = "PRIVATE KEY";
/// Where azalea-auth 0.16.0 logs the certificate response, chat-signing
/// private key included, at TRACE (`certs.rs`, `fetch_certificates`).
const CERTS_TARGET: &str = "azalea_auth::certs";

/// A line of the account file, in the order `fetch-token` writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    /// Line 1: the account's name.
    Name,
    /// Line 2: the account's UUID.
    Uuid,
    /// Line 3: the Minecraft access token.
    Token,
}

impl Key {
    /// The key as the file spells it.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Uuid => "uuid",
            Self::Token => "token",
        }
    }

    /// What its value must be.
    const fn value(self) -> &'static str {
        match self {
            Self::Name => "a Minecraft name",
            Self::Uuid => "a UUID",
            Self::Token => "a token",
        }
    }
}

/// Why the account file can't be used. No variant holds any of its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountFileError {
    /// The file couldn't be read.
    Unreadable(io::ErrorKind),
    /// Line `line` is missing, or isn't `key=` and a valid value.
    Line { line: usize, key: Key },
    /// Line `line`, after the token, isn't empty.
    Extra { line: usize },
}

impl fmt::Display for AccountFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(kind) => write!(
                f,
                "can't read {ACCOUNT_FILE_NAME} ({kind}); create it with the spike's fetch-token right before running (spikes/azalea/README.md)"
            ),
            Self::Line { line, key } => write!(
                f,
                "{ACCOUNT_FILE_NAME}, line {line}: expected `{}=` and {}",
                key.as_str(),
                key.value()
            ),
            Self::Extra { line } => write!(
                f,
                "{ACCOUNT_FILE_NAME}, line {line}: expected nothing after the token"
            ),
        }
    }
}

/// The online credentials in the text of an account file: `name=`, `uuid=`
/// and `token=` lines, in that order. CRLF line ends and trailing empty
/// lines are accepted, and values are trimmed.
pub(crate) fn parse_account(text: &str) -> Result<SessionCredentials, AccountFileError> {
    let mut lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line));
    let mut value = |key: Key, line: usize| {
        lines
            .next()
            .and_then(|(_, text)| text.strip_prefix(key.as_str())?.strip_prefix('='))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(AccountFileError::Line { line, key })
    };
    let username = value(Key::Name, 1)?
        .parse()
        .map_err(|_| AccountFileError::Line {
            line: 1,
            key: Key::Name,
        })?;
    let uuid = Uuid::parse_str(value(Key::Uuid, 2)?).map_err(|_| AccountFileError::Line {
        line: 2,
        key: Key::Uuid,
    })?;
    let access_token = SecretString::from(value(Key::Token, 3)?.to_owned());
    if let Some((line, _)) = lines.find(|(_, text)| !text.trim().is_empty()) {
        return Err(AccountFileError::Extra { line });
    }
    Ok(SessionCredentials::Online {
        username,
        uuid,
        access_token,
        expires_at: any_expiry(),
    })
}

/// Reads the account file straight into a secret, and parses it.
fn read_account() -> Result<SessionCredentials, AccountFileError> {
    let text = fs::read_to_string(ACCOUNT_FILE)
        .map(SecretString::from)
        .map_err(|error| AccountFileError::Unreadable(error.kind()))?;
    parse_account(text.expose_secret())
}

/// What an event is, without its contents: no chat text, no names.
fn kind(event: &SessionEvent) -> String {
    match event {
        SessionEvent::Joined => "Joined".to_owned(),
        SessionEvent::Chat(_) => "Chat".to_owned(),
        SessionEvent::Died => "Died".to_owned(),
        SessionEvent::Respawned => "Respawned".to_owned(),
        SessionEvent::Disconnected(reason) => format!("Disconnected({:?})", reason.classify()),
        SessionEvent::ConnectionFailed(failure) => format!("ConnectionFailed({failure:?})"),
    }
}

/// Waits for an event that satisfies `wanted`, skipping others, and returns
/// it. A failure names only `what` and the kinds of the events the session
/// sent, and a rejected token says it may have expired.
async fn wait_quietly(
    events: &mut McEvents,
    what: &str,
    wanted: impl Fn(&SessionEvent) -> bool,
) -> Result<SessionEvent, String> {
    let mut seen = Vec::new();
    let mut rejected = false;
    let found = tokio::time::timeout(WITHIN, async {
        while let Some(event) = events.next().await {
            if wanted(&event) {
                return Some(event);
            }
            rejected |= event == SessionEvent::Disconnected(DisconnectReason::AuthRejected);
            seen.push(kind(&event));
        }
        None
    })
    .await;
    if rejected {
        return Err(format!(
            "the session server rejected the token before {what}. It may have expired: run the spike's fetch-token again right before this test (spikes/azalea/README.md)"
        ));
    }
    match found {
        Ok(Some(event)) => Ok(event),
        Ok(None) => Err(format!("the session ended before {what}; it sent {seen:?}")),
        Err(_) => Err(format!(
            "{what} didn't happen within {WITHIN:?}; the session sent {seen:?}"
        )),
    }
}

/// Sends `text` and waits for its echo: chat of `kind` with the account,
/// `name`, as its sender and `echo` as its text.
async fn echoed(
    session: &McSession,
    events: &mut McEvents,
    what: &str,
    (text, kind, echo): (&str, ChatKind, &str),
    name: &str,
) -> Result<(), String> {
    let sent = session.send_chat(text.parse().unwrap()).await;
    if let Err(error) = sent {
        return Err(format!("sending {what} failed: {error}"));
    }
    wait_quietly(events, &format!("{what}'s echo"), |event| {
        is_chat(event, kind, Some(name), echo)
    })
    .await
    .map(drop)
}

/// Tries `/me` and expects fleet-mc to refuse it before sending (ADR-0020,
/// S2): the server enforces secure chat, azalea signs no command, and the
/// server would reject its message. Then a signed chat line must echo before
/// any rejection of `/me` or its echo: the server answers in order, so either
/// would come first had `/me` gone out. The bot stays up.
async fn emote_refused(
    session: &McSession,
    events: &mut McEvents,
    name: &str,
) -> Result<(), String> {
    let tried = session.send_chat(EMOTE.parse().unwrap()).await;
    if let Err(error) = session.send_chat(AFTER_EMOTE.parse().unwrap()).await {
        return Err(format!("sending the chat after /me failed: {error}"));
    }
    let rejection = rejection();
    let first = wait_quietly(events, "the chat after /me", |event| {
        is_chat(event, ChatKind::Emote, Some(name), EMOTE_TEXT)
            || is_chat(event, ChatKind::System, None, rejection)
            || is_chat(event, ChatKind::Chat, Some(name), AFTER_EMOTE)
    })
    .await?;
    emote_verdict(tried, &first, name)
}

/// The server's answer to an unsigned command that needs signed arguments,
/// as fleet-mc renders it: with azalea-language, or the key where it has
/// none.
fn rejection() -> &'static str {
    azalea_language::get(REJECTED_COMMAND).unwrap_or(REJECTED_COMMAND)
}

/// The `/me` step's verdict: `send_chat` refused it as `tried`, and the
/// first event the step waited for, `first`, is the later chat's echo, never
/// the server's rejection or the emote's echo.
fn emote_verdict(
    tried: Result<(), SessionError>,
    first: &SessionEvent,
    name: &str,
) -> Result<(), String> {
    match tried {
        Err(SessionError::ChatUnavailable) => {}
        Ok(()) => {
            return Err(
                "fleet-mc sent /me; it should refuse a command with a message where chat must be signed (ADR-0020, S2)"
                    .to_owned(),
            );
        }
        Err(error) => return Err(format!("trying /me failed otherwise: {error}")),
    }
    if is_chat(first, ChatKind::Emote, Some(name), EMOTE_TEXT) {
        return Err(
            "the emote's echo arrived, so /me went out signed: azalea may sign commands now; update this step and ADR-0020's S2"
                .to_owned(),
        );
    }
    if is_chat(first, ChatKind::System, None, rejection()) {
        return Err("the server rejected /me, so it went out unsigned".to_owned());
    }
    Ok(())
}

/// What the session's steps found. Each failure names only the step and
/// kinds of events, so the steps can fail without hiding the log checks.
struct Outcome {
    join: Result<(), String>,
    chat: Result<(), String>,
    emote: Result<(), String>,
}

/// Joins, sends signed chat, then tries `/me`, which fleet-mc refuses.
/// Nothing here fails the test: the
/// teardown and the log checks must run whatever happens (the user's
/// decision, after a failed chat once skipped them).
async fn session_steps(session: &McSession, events: &mut McEvents, name: &str) -> Outcome {
    let not_tried = || Err("not tried: the bot didn't join".to_owned());
    // 1. The session server accepts the real token, and the bot joins.
    let join = wait_quietly(events, "the join", |event| *event == SessionEvent::Joined)
        .await
        .map(drop);
    if join.is_err() {
        return Outcome {
            join,
            chat: not_tried(),
            emote: not_tried(),
        };
    }
    // 2. Signed chat goes through: the server enforces secure profiles and
    // echoes it with the account as its sender. `send_chat` waits for
    // azalea's chat-signing session first.
    let chat = echoed(
        session,
        events,
        "the signed chat",
        (CHAT, ChatKind::Chat, CHAT),
        name,
    )
    .await;
    // 3. `/me` is refused before it's sent (see `emote_refused`). Its result
    // is checked last.
    let emote = emote_refused(session, events, name).await;
    Outcome { join, chat, emote }
}

#[tokio::test]
async fn manual_real_account_scenario() {
    let credentials = read_account().unwrap_or_else(|error| panic!("{error}"));
    let SessionCredentials::Online {
        username,
        access_token,
        ..
    } = &credentials
    else {
        panic!("the account file gave offline credentials");
    };
    let name = username.as_str().to_owned();
    let token = access_token.clone();
    log_capture::install().unwrap();
    let server = Server::start(Mode::Online).await;
    // The container is published on 127.0.0.1, which only the offline
    // scenario checks (ADR-0019, S69).
    let connector =
        AzaleaConnector::new(&McConfig::default()).with_targets(TargetPolicy::Unchecked);

    // 1 to 3. The session's steps, which record their results.
    let (session, mut events) = connect(&connector, &server, bot(1), credentials).await;
    let outcome = session_steps(&session, &mut events, &name).await;

    // 4. The teardown leaves nothing behind.
    within("the teardown", session.disconnect()).await;
    assert_eq!(connector.pool().live_threads(), 0);

    // 5. The token never reached a log, at any level of any target. A leak
    // names only targets and counts.
    log_capture::check_absent(token.expose_secret()).unwrap();

    // 6. A chat-signing private key reached a log only where azalea-auth
    // 0.16.0 logs the certificate response at TRACE, which P5.2 caps.
    if let Err(leak) = log_capture::check_absent(PRIVATE_KEY) {
        assert!(
            leak.hits.keys().all(|target| target == CERTS_TARGET),
            "a private key reached a log outside {CERTS_TARGET}: {leak}"
        );
    }

    // 7. Only now, the session's steps.
    let failures: Vec<String> = [
        ("join", outcome.join),
        ("signed chat", outcome.chat),
        ("/me", outcome.emote),
    ]
    .into_iter()
    .filter_map(|(step, result)| result.err().map(|failure| format!("{step}: {failure}")))
    .collect();
    assert!(
        failures.is_empty(),
        "the log checks passed, but steps failed: {failures:#?}"
    );
}

mod tests {
    use fleet_core::chat::IncomingChat;
    use rstest::rstest;

    use super::*;

    // --- The `/me` step's verdict (ADR-0020, S2), without an account ---

    fn chat(kind: ChatKind, sender: Option<&str>, text: &str) -> SessionEvent {
        let sender = sender.map(|name| (name, None));
        SessionEvent::Chat(IncomingChat::from_parts(kind, sender, text, false).unwrap())
    }

    #[test]
    fn a_refused_emote_followed_by_the_later_chat_passes() {
        let first = chat(ChatKind::Chat, Some(NAME), AFTER_EMOTE);

        let verdict = emote_verdict(Err(SessionError::ChatUnavailable), &first, NAME);

        assert_eq!(verdict, Ok(()));
    }

    #[rstest]
    #[case::sent(Ok(()), chat(ChatKind::Chat, Some(NAME), AFTER_EMOTE))]
    #[case::failed_otherwise(
        Err(SessionError::Closed),
        chat(ChatKind::Chat, Some(NAME), AFTER_EMOTE)
    )]
    #[case::the_emote_echoed(
        Err(SessionError::ChatUnavailable),
        chat(ChatKind::Emote, Some(NAME), EMOTE_TEXT)
    )]
    #[case::the_server_rejected_it(
        Err(SessionError::ChatUnavailable),
        chat(ChatKind::System, None, rejection())
    )]
    fn an_emote_that_went_out_or_failed_otherwise_fails_the_step(
        #[case] tried: Result<(), SessionError>,
        #[case] first: SessionEvent,
    ) {
        assert!(emote_verdict(tried, &first, NAME).is_err());
    }

    const NAME: &str = "AfkBot1";
    const UUID: &str = "01234567-89ab-4def-8123-456789abcdef";
    const TOKEN: &str = "afkfleet-account-file-marker-7c19d4";

    fn file(lines: &[&str]) -> String {
        lines.iter().flat_map(|line| [*line, "\n"]).collect()
    }

    fn valid() -> String {
        file(&[
            &format!("name={NAME}"),
            &format!("uuid={UUID}"),
            &format!("token={TOKEN}"),
        ])
    }

    fn assert_placeholders(credentials: &SessionCredentials) {
        let SessionCredentials::Online {
            username,
            uuid,
            access_token,
            ..
        } = credentials
        else {
            panic!("expected online credentials, got {credentials:?}");
        };
        assert_eq!(username.as_str(), NAME);
        assert_eq!(*uuid, Uuid::parse_str(UUID).unwrap());
        assert_eq!(access_token.expose_secret(), TOKEN);
    }

    #[test]
    fn a_file_from_fetch_token_gives_online_credentials() {
        let credentials = parse_account(&valid()).unwrap();

        assert_placeholders(&credentials);
    }

    #[test]
    fn crlf_line_ends_trailing_empty_lines_and_spaces_are_accepted() {
        let text = format!("name={NAME} \r\nuuid= {UUID}\r\ntoken={TOKEN}\r\n\r\n\n");

        let credentials = parse_account(&text).unwrap();

        assert_placeholders(&credentials);
    }

    #[rstest]
    #[case::empty_file(String::new(), AccountFileError::Line { line: 1, key: Key::Name })]
    #[case::only_a_name(file(&[&format!("name={NAME}")]), AccountFileError::Line { line: 2, key: Key::Uuid })]
    #[case::no_token(file(&[&format!("name={NAME}"), &format!("uuid={UUID}")]), AccountFileError::Line { line: 3, key: Key::Token })]
    #[case::wrong_order(file(&[&format!("uuid={UUID}"), &format!("name={NAME}"), &format!("token={TOKEN}")]), AccountFileError::Line { line: 1, key: Key::Name })]
    #[case::invalid_name(file(&["name=Afk Bot", &format!("uuid={UUID}"), &format!("token={TOKEN}")]), AccountFileError::Line { line: 1, key: Key::Name })]
    #[case::invalid_uuid(file(&[&format!("name={NAME}"), "uuid=not-a-uuid", &format!("token={TOKEN}")]), AccountFileError::Line { line: 2, key: Key::Uuid })]
    #[case::empty_token(file(&[&format!("name={NAME}"), &format!("uuid={UUID}"), "token="]), AccountFileError::Line { line: 3, key: Key::Token })]
    #[case::no_equals_sign(file(&[&format!("name {NAME}"), &format!("uuid={UUID}"), &format!("token={TOKEN}")]), AccountFileError::Line { line: 1, key: Key::Name })]
    #[case::a_line_after_the_token(format!("{}\nextra={TOKEN}\n", valid()), AccountFileError::Extra { line: 5 })]
    fn a_malformed_file_names_the_line_and_the_expected_key(
        #[case] text: String,
        #[case] error: AccountFileError,
    ) {
        assert_eq!(parse_account(&text).unwrap_err(), error);
    }

    #[rstest]
    #[case::line(AccountFileError::Line { line: 2, key: Key::Uuid }, "secrets/p1.8-account.txt, line 2: expected `uuid=` and a UUID")]
    #[case::extra(AccountFileError::Extra { line: 4 }, "secrets/p1.8-account.txt, line 4: expected nothing after the token")]
    #[case::unreadable(
        AccountFileError::Unreadable(io::ErrorKind::NotFound),
        "can't read secrets/p1.8-account.txt (entity not found); create it with the spike's fetch-token right before running (spikes/azalea/README.md)"
    )]
    fn errors_have_fixed_messages(#[case] error: AccountFileError, #[case] message: &str) {
        assert_eq!(error.to_string(), message);
    }

    /// Every malformed file below holds the real-looking values, and no
    /// error may show any of them.
    #[test]
    fn errors_never_show_the_files_contents() {
        let malformed = [
            format!("name={NAME}\nuuid={TOKEN}\ntoken={UUID}\n"),
            format!("token={TOKEN}\nname={NAME}\nuuid={UUID}\n"),
            format!("name={NAME}\nuuid={UUID}\ntoken={TOKEN}\n{TOKEN}\n"),
            format!("name={NAME}={TOKEN}\nuuid={UUID}\ntoken={TOKEN}\n"),
        ];
        for text in malformed {
            let error = parse_account(&text).unwrap_err();

            let shown = format!("{error} {error:?}");
            for secret in [NAME, UUID, TOKEN] {
                assert!(!shown.contains(secret), "an error showed file contents");
            }
        }
    }
}
