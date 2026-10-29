//! [`ChatFormats`]: a Minecraft server's chat formats, which read a
//! display-only sender out of the system lines its plugins send (ADR-0020,
//! S79, S80, U59).

use crate::chat::{ChatKind, IncomingChat};
use crate::text::{self, LineBreaks};
use crate::value::{McUsername, is_name_char};

/// The most chat formats a server holds (S80).
pub const MAX_CHAT_FORMATS: usize = 8;

/// The placeholder for the sender's name.
const SENDER: &str = "{sender}";
/// The placeholder for a part that changes.
const ANY: &str = "{any}";
/// The most `{any}` a format holds.
const MAX_WILDCARDS: usize = 2;

/// The most characters of a received text a match reads: every literal
/// character a format can hold, each `{any}` at its longest, and a name run
/// up to the character that makes it too long.
pub const READ_MAX: usize = ChatFormats::MAX_LEN - SENDER.len()
    + MAX_WILDCARDS * ChatFormats::MAX_ANY
    + McUsername::MAX_LEN
    + 1;

/// Why entries can't form [`ChatFormats`].
///
/// `index` is the format's place in the list, counting from 0, and `position`
/// a character's place in it, counted in characters. The format's text is
/// never included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatFormatsError {
    /// The list has more than [`MAX_CHAT_FORMATS`] formats.
    #[error("the list has {count} chat formats; the limit is {max}", max = MAX_CHAT_FORMATS)]
    TooMany {
        /// How many formats it has.
        count: usize,
    },
    /// The format is empty.
    #[error("chat format {index} is empty")]
    Empty {
        /// The format's place in the list, from 0.
        index: usize,
    },
    /// The format is longer than [`ChatFormats::MAX_LEN`] characters as
    /// written.
    #[error("chat format {index} is {chars} characters long; the limit is {max}", max = ChatFormats::MAX_LEN)]
    TooLong {
        /// The format's place in the list, from 0.
        index: usize,
        /// Its length in characters, as written.
        chars: usize,
    },
    /// The format holds a character received chat never has: a line break,
    /// another control character, `§`, a text-direction mark or an invisible
    /// character. Received text is sanitized, so the format could never fit.
    #[error(
        "chat format {index} holds a character received chat never has, at position {position}"
    )]
    ForbiddenChar {
        /// The format's place in the list, from 0.
        index: usize,
        /// The character's place in it.
        position: usize,
    },
    /// The format holds a brace that isn't part of `{sender}`, `{any}`, `{{`
    /// or `}}` (U59).
    #[error(
        "chat format {index} holds a brace that isn't part of a placeholder or a doubled brace, at position {position}"
    )]
    UnknownPlaceholder {
        /// The format's place in the list, from 0.
        index: usize,
        /// The brace's place in it.
        position: usize,
    },
    /// The format holds a second `{sender}`.
    #[error("chat format {index} holds a second sender, at position {position}")]
    SecondPlaceholder {
        /// The format's place in the list, from 0.
        index: usize,
        /// The second `{sender}`'s place in it.
        position: usize,
    },
    /// The format holds `{any}` after `{sender}`.
    #[error("chat format {index} holds a changing part after the sender, at position {position}")]
    WildcardAfterSender {
        /// The format's place in the list, from 0.
        index: usize,
        /// The `{any}`'s place in it.
        position: usize,
    },
    /// The format holds a third `{any}`.
    #[error("chat format {index} holds a third changing part, at position {position}")]
    TooManyWildcards {
        /// The format's place in the list, from 0.
        index: usize,
        /// The third `{any}`'s place in it.
        position: usize,
    },
    /// An `{any}` isn't followed by text: the format ends or another
    /// placeholder comes right after it.
    #[error("chat format {index} has no text right after the changing part at position {position}")]
    WildcardWithoutLiteral {
        /// The format's place in the list, from 0.
        index: usize,
        /// The `{any}`'s place in it.
        position: usize,
    },
    /// The format holds no `{sender}`.
    #[error("chat format {index} holds no sender")]
    MissingPlaceholder {
        /// The format's place in the list, from 0.
        index: usize,
    },
    /// Nothing follows `{sender}`.
    #[error("chat format {index} has no text after the sender")]
    NoSeparator {
        /// The format's place in the list, from 0.
        index: usize,
    },
    /// The text after `{sender}` starts with a name character, so a name
    /// couldn't end there.
    #[error(
        "chat format {index}'s text after the sender starts with a name character, at position {position}"
    )]
    SeparatorStartsWithNameChar {
        /// The format's place in the list, from 0.
        index: usize,
        /// That character's place in it.
        position: usize,
    },
}

/// A server's chat formats, in order, duplicates kept (S80).
///
/// On many servers, player chat arrives as system text, such as
/// `[Clan] Name : hello`, whose sender azalea can only guess. A format is a
/// template of that line's start: literal text, one `{sender}` where the name
/// is, and up to two `{any}` for parts that change, such as a clan tag. A
/// server holds at most [`MAX_CHAT_FORMATS`] of them, and the first that fits
/// a line names it. The name is display only: it never becomes a line's
/// sender, never decides who sees or owns anything, and the line's text stays
/// whole. A player whose clan tag holds `] Admin : ` can make "Admin" the
/// parsed sender, which the app marks as read by a format (S80's accepted
/// residual).
///
/// **The rules a format follows** (S80, U59):
/// - at most [`ChatFormats::MAX_LEN`] characters as written;
/// - exactly one `{sender}`;
/// - at most two `{any}`, each before `{sender}` and followed by text;
/// - text after `{sender}` that doesn't start with a name character
///   (`A`–`Z`, `a`–`z`, `0`–`9`, `_`), so the name ends where it should;
/// - one line, without a character received chat never has;
/// - braces reserved: `{{` and `}}` are a literal brace, and any other `{`
///   or `}` is refused, so a typo such as `{Sender}` is caught and a later
///   name such as `{clan}` can't change what a stored format means.
///
/// **Matching** is anchored at the start of the received text. Literal text
/// is compared exactly. An `{any}` takes 0 to [`ChatFormats::MAX_ANY`]
/// characters up to the first place the text after it starts, never across a
/// line break and never tried again further on. The sender is the longest run
/// of name characters, which must be a valid [`McUsername`] and be followed by
/// the text after `{sender}`. Matching reads at most [`READ_MAX`] characters
/// of the text's start, so its work is bounded whatever the line, and nothing
/// past them changes its answer. It never indexes and never panics.
///
/// The default list is empty, which reads no sender from any line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatFormats {
    formats: Vec<Format>,
}

/// One format: as written, and the steps a match takes before the name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Format {
    written: String,
    head: Vec<Step>,
    separator: String,
}

/// A step a match takes before the name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// This text, exactly.
    Literal(String),
    /// Up to [`ChatFormats::MAX_ANY`] characters, then this text.
    Any(String),
}

/// What [`ChatFormats::matched`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatMatch {
    format: usize,
    sender: McUsername,
}

impl FormatMatch {
    /// The format that fitted: its place in the list, from 0.
    #[must_use]
    pub const fn format(&self) -> usize {
        self.format
    }

    /// The name it read.
    #[must_use]
    pub const fn sender(&self) -> &McUsername {
        &self.sender
    }
}

impl ChatFormats {
    /// The longest format, in characters as written: `{sender}` counts 8,
    /// and `{{` 2.
    pub const MAX_LEN: usize = 64;

    /// The most characters an `{any}` takes.
    pub const MAX_ANY: usize = 32;

    /// Builds the list from `entries`, taken as written, in order,
    /// duplicates kept.
    ///
    /// # Errors
    /// [`ChatFormatsError::TooMany`] for more than [`MAX_CHAT_FORMATS`]
    /// entries; otherwise the first invalid entry's error. Within one entry
    /// the checks run in this order: empty, length, a forbidden character,
    /// then from left to right, the first problem by position winning (a
    /// brace, a second `{sender}`, an `{any}` after `{sender}`, a third
    /// `{any}`, an `{any}` without text after it, text after `{sender}` that
    /// starts with a name character), then what's missing at the end (no
    /// `{sender}`, no text after it).
    pub fn try_new<S: AsRef<str>>(entries: &[S]) -> Result<Self, ChatFormatsError> {
        if entries.len() > MAX_CHAT_FORMATS {
            return Err(ChatFormatsError::TooMany {
                count: entries.len(),
            });
        }
        let formats = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| Format::parse(index, entry.as_ref()))
            .collect::<Result<_, _>>()?;
        Ok(Self { formats })
    }

    /// The formats as written, in order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.formats.iter().map(|format| format.written.as_str())
    }

    /// How many formats there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.formats.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formats.is_empty()
    }

    /// The name the first fitting format reads from `chat`, a system line;
    /// `None` when none fits, or for any other kind of line.
    #[must_use]
    pub fn sender_of(&self, chat: &IncomingChat) -> Option<McUsername> {
        self.matched(chat).map(|found| found.sender)
    }

    /// Which format fits `chat` first, a system line, and the name it reads;
    /// `None` when none fits, or for any other kind of line.
    #[must_use]
    pub fn matched(&self, chat: &IncomingChat) -> Option<FormatMatch> {
        if chat.kind() != ChatKind::System {
            return None;
        }
        let head = head(chat.text());
        self.formats.iter().enumerate().find_map(|(format, each)| {
            each.sender_in(head)
                .map(|sender| FormatMatch { format, sender })
        })
    }
}

/// `text`'s first [`READ_MAX`] characters.
fn head(text: &str) -> &str {
    text.char_indices()
        .nth(READ_MAX)
        .and_then(|(end, _)| text.get(..end))
        .unwrap_or(text)
}

/// Where a scan of one format is.
struct Scan {
    index: usize,
    head: Vec<Step>,
    literal: String,
    /// An `{any}` not yet followed by text, and where it is.
    pending_any: Option<usize>,
    wildcards: usize,
    sender: bool,
    separator: String,
}

impl Scan {
    /// A literal character at `position`.
    fn literal(&mut self, c: char, position: usize) -> Result<(), ChatFormatsError> {
        self.pending_any = None;
        if self.sender {
            if self.separator.is_empty() && is_name_char(c) {
                return Err(ChatFormatsError::SeparatorStartsWithNameChar {
                    index: self.index,
                    position,
                });
            }
            self.separator.push(c);
        } else {
            self.literal.push(c);
        }
        Ok(())
    }

    /// An `{any}` without text after it, if one waits.
    fn unfollowed(&self) -> Result<(), ChatFormatsError> {
        self.pending_any.map_or(Ok(()), |position| {
            Err(ChatFormatsError::WildcardWithoutLiteral {
                index: self.index,
                position,
            })
        })
    }

    /// The text gathered since the last placeholder, as the step it ends.
    fn close_literal(&mut self) {
        let literal = core::mem::take(&mut self.literal);
        match self.head.last_mut() {
            Some(Step::Any(until)) if until.is_empty() => *until = literal,
            _ if literal.is_empty() => {}
            _ => self.head.push(Step::Literal(literal)),
        }
    }

    /// `{sender}` at `position`.
    fn sender(&mut self, position: usize) -> Result<(), ChatFormatsError> {
        self.unfollowed()?;
        if self.sender {
            return Err(ChatFormatsError::SecondPlaceholder {
                index: self.index,
                position,
            });
        }
        self.close_literal();
        self.sender = true;
        Ok(())
    }

    /// `{any}` at `position`.
    fn any(&mut self, position: usize) -> Result<(), ChatFormatsError> {
        self.unfollowed()?;
        let error = if self.sender {
            Some(ChatFormatsError::WildcardAfterSender {
                index: self.index,
                position,
            })
        } else if self.wildcards == MAX_WILDCARDS {
            Some(ChatFormatsError::TooManyWildcards {
                index: self.index,
                position,
            })
        } else {
            None
        };
        if let Some(error) = error {
            return Err(error);
        }
        self.close_literal();
        self.head.push(Step::Any(String::new()));
        self.wildcards += 1;
        self.pending_any = Some(position);
        Ok(())
    }
}

impl Format {
    /// Format `index`, `written`; see [`ChatFormats::try_new`].
    fn parse(index: usize, written: &str) -> Result<Self, ChatFormatsError> {
        if written.is_empty() {
            return Err(ChatFormatsError::Empty { index });
        }
        let chars = written.chars().count();
        if chars > ChatFormats::MAX_LEN {
            return Err(ChatFormatsError::TooLong { index, chars });
        }
        if let Some(position) = text::first_stripped(written, LineBreaks::Strip) {
            return Err(ChatFormatsError::ForbiddenChar { index, position });
        }
        let mut scan = Scan {
            index,
            head: Vec::new(),
            literal: String::new(),
            pending_any: None,
            wildcards: 0,
            sender: false,
            separator: String::new(),
        };
        let mut rest = written;
        let mut position = 0;
        while !rest.is_empty() {
            let (taken, after) = if let Some(after) = rest.strip_prefix("{{") {
                scan.literal('{', position)?;
                (2, after)
            } else if let Some(after) = rest.strip_prefix("}}") {
                scan.literal('}', position)?;
                (2, after)
            } else if let Some(after) = rest.strip_prefix(SENDER) {
                scan.sender(position)?;
                (SENDER.len(), after)
            } else if let Some(after) = rest.strip_prefix(ANY) {
                scan.any(position)?;
                (ANY.len(), after)
            } else {
                let mut chars = rest.chars();
                match chars.next() {
                    Some('{' | '}') | None => {
                        scan.unfollowed()?;
                        return Err(ChatFormatsError::UnknownPlaceholder { index, position });
                    }
                    Some(c) => scan.literal(c, position)?,
                }
                (1, chars.as_str())
            };
            rest = after;
            position += taken;
        }
        scan.unfollowed()?;
        if !scan.sender {
            return Err(ChatFormatsError::MissingPlaceholder { index });
        }
        if scan.separator.is_empty() {
            return Err(ChatFormatsError::NoSeparator { index });
        }
        Ok(Self {
            written: written.to_owned(),
            head: scan.head,
            separator: scan.separator,
        })
    }

    /// The name this format reads from `text`'s start, if it fits.
    fn sender_in(&self, text: &str) -> Option<McUsername> {
        let mut rest = text;
        for step in &self.head {
            rest = match step {
                Step::Literal(literal) => rest.strip_prefix(literal.as_str())?,
                Step::Any(until) => after_any(rest, until)?,
            };
        }
        let run = rest
            .chars()
            .take(McUsername::MAX_LEN + 1)
            .take_while(|c| is_name_char(*c))
            .count();
        if run > McUsername::MAX_LEN {
            return None;
        }
        // Name characters are ASCII: as many bytes as characters.
        let name = McUsername::try_from(rest.get(..run)?).ok()?;
        rest.get(run..)?
            .starts_with(self.separator.as_str())
            .then_some(name)
    }
}

/// What follows the first place `until` starts within
/// [`ChatFormats::MAX_ANY`] characters of `text`'s start, never across a
/// line break.
fn after_any<'t>(text: &'t str, until: &str) -> Option<&'t str> {
    let mut rest = text;
    for _ in 0..=ChatFormats::MAX_ANY {
        if let Some(after) = rest.strip_prefix(until) {
            return Some(after);
        }
        let mut chars = rest.chars();
        match chars.next() {
            None | Some('\n') => return None,
            Some(_) => rest = chars.as_str(),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::PlayerChatKind;
    use proptest::prelude::*;
    use rstest::rstest;

    /// The user's example (S80): a clan tag that differs per player, and
    /// players without one.
    const USERS: [&str; 2] = ["[{any}] {sender} : ", "{sender} : "];

    fn formats(entries: &[&str]) -> ChatFormats {
        ChatFormats::try_new(entries).unwrap()
    }

    /// The sender the formats read from a system line `text`.
    fn sender(entries: &[&str], text: &str) -> Option<String> {
        formats(entries)
            .sender_of(&IncomingChat::system(text))
            .map(String::from)
    }

    #[rstest]
    #[case::empty("", ChatFormatsError::Empty { index: 0 })]
    #[case::too_long(&"x".repeat(65), ChatFormatsError::TooLong { index: 0, chars: 65 })]
    #[case::line_break("{sender}\n: ", ChatFormatsError::ForbiddenChar { index: 0, position: 8 })]
    #[case::formatting_code("§a{sender} : ", ChatFormatsError::ForbiddenChar { index: 0, position: 0 })]
    #[case::text_direction("\u{202E}{sender} : ", ChatFormatsError::ForbiddenChar { index: 0, position: 0 })]
    #[case::invisible("{sender}\u{200B}: ", ChatFormatsError::ForbiddenChar { index: 0, position: 8 })]
    #[case::wrong_case("{Sender} : ", ChatFormatsError::UnknownPlaceholder { index: 0, position: 0 })]
    #[case::unknown_name("[{clan}] {sender} : ", ChatFormatsError::UnknownPlaceholder { index: 0, position: 1 })]
    #[case::unclosed("{sender : ", ChatFormatsError::UnknownPlaceholder { index: 0, position: 0 })]
    #[case::lone_closing("{sender} : }", ChatFormatsError::UnknownPlaceholder { index: 0, position: 11 })]
    #[case::escape_then_lone("{{sender} : ", ChatFormatsError::UnknownPlaceholder { index: 0, position: 8 })]
    #[case::second_sender("{sender} {sender} : ", ChatFormatsError::SecondPlaceholder { index: 0, position: 9 })]
    #[case::any_after_sender("{sender} : {any}", ChatFormatsError::WildcardAfterSender { index: 0, position: 11 })]
    #[case::third_any("{any}a{any}b{any}c{sender} : ", ChatFormatsError::TooManyWildcards { index: 0, position: 12 })]
    #[case::any_then_sender("{any}{sender} : ", ChatFormatsError::WildcardWithoutLiteral { index: 0, position: 0 })]
    #[case::any_then_any("{any}{any}x{sender} : ", ChatFormatsError::WildcardWithoutLiteral { index: 0, position: 0 })]
    #[case::any_at_the_end("[{any}", ChatFormatsError::WildcardWithoutLiteral { index: 0, position: 1 })]
    #[case::no_sender("[{any}] : ", ChatFormatsError::MissingPlaceholder { index: 0 })]
    #[case::nothing_after_sender("[{any}] {sender}", ChatFormatsError::NoSeparator { index: 0 })]
    #[case::letter_after_sender("{sender}x : ", ChatFormatsError::SeparatorStartsWithNameChar { index: 0, position: 8 })]
    #[case::digit_after_sender("{sender}7: ", ChatFormatsError::SeparatorStartsWithNameChar { index: 0, position: 8 })]
    #[case::underscore_after_sender("{sender}_ : ", ChatFormatsError::SeparatorStartsWithNameChar { index: 0, position: 8 })]
    fn a_format_is_refused_with_its_problem_and_where(
        #[case] format: &str,
        #[case] error: ChatFormatsError,
    ) {
        assert_eq!(ChatFormats::try_new(&[format]), Err(error));
    }

    #[rstest]
    #[case::length_before_characters(&format!("\n{}", "x".repeat(64)), ChatFormatsError::TooLong { index: 0, chars: 65 })]
    #[case::characters_before_braces("{Sender}\n", ChatFormatsError::ForbiddenChar { index: 0, position: 8 })]
    #[case::separator_before_a_later_brace("{sender}x {Sender}", ChatFormatsError::SeparatorStartsWithNameChar { index: 0, position: 8 })]
    #[case::unfollowed_any_before_a_later_brace("{any}{x}", ChatFormatsError::WildcardWithoutLiteral { index: 0, position: 0 })]
    #[case::brace_before_a_later_second_sender("{x}{sender}{sender}", ChatFormatsError::UnknownPlaceholder { index: 0, position: 0 })]
    #[case::second_sender_before_a_missing_separator("{sender}{sender}", ChatFormatsError::SecondPlaceholder { index: 0, position: 8 })]
    fn the_first_problem_decides_in_the_documented_order(
        #[case] format: &str,
        #[case] error: ChatFormatsError,
    ) {
        assert_eq!(ChatFormats::try_new(&[format]), Err(error));
    }

    #[rstest]
    #[case::users_clan("[{any}] {sender} : ")]
    #[case::users_plain("{sender} : ")]
    #[case::escaped_braces("{{{sender}}} : ")]
    #[case::escaped_after_sender("{sender}}} ")]
    #[case::two_wildcards("{any} | {any}» {sender}: ")]
    #[case::sender_first("{sender}: ")]
    #[case::longest(&format!("{{sender}}: {}", "x".repeat(54)))]
    fn a_valid_format_is_taken_as_written(#[case] format: &str) {
        let taken = formats(&[format]);

        assert_eq!(taken.iter().collect::<Vec<_>>(), [format]);
    }

    #[test]
    fn eight_formats_are_taken_in_order_with_duplicates_and_a_ninth_is_too_many() {
        let eight = ["{sender} : "; 8];
        let mut nine = eight.to_vec();
        nine.push("<{sender}> ");

        let taken = formats(&eight);

        assert_eq!(taken.len(), 8);
        assert_eq!(taken.iter().collect::<Vec<_>>(), eight);
        assert_eq!(
            ChatFormats::try_new(&nine),
            Err(ChatFormatsError::TooMany { count: 9 })
        );
        assert_eq!(MAX_CHAT_FORMATS, 8);
    }

    #[test]
    fn the_first_invalid_format_names_its_place_in_the_list() {
        assert_eq!(
            ChatFormats::try_new(&["{sender} : ", "{Sender} : ", ""]),
            Err(ChatFormatsError::UnknownPlaceholder {
                index: 1,
                position: 0
            })
        );
    }

    #[test]
    fn no_formats_read_no_sender() {
        let none = ChatFormats::default();

        assert!(none.is_empty());
        assert_eq!(ChatFormats::try_new::<&str>(&[]), Ok(none.clone()));
        assert_eq!(none.sender_of(&IncomingChat::system("Notch : hi")), None);
    }

    #[test]
    fn errors_never_include_the_format() {
        let secret = "hunter2hunter2";
        let cases = [
            format!("{secret}{{Sender}}"),
            format!("{secret}\n"),
            format!("{secret}{{any}}"),
            format!("{{sender}}{secret}"),
            secret.repeat(5),
        ];

        for case in cases {
            let error = ChatFormats::try_new(&[case]).unwrap_err();
            assert!(!error.to_string().contains(secret), "{error}");
        }
    }

    #[rstest]
    #[case::clan("[Clan] Notch : hello", Some("Notch"), Some(0))]
    #[case::no_clan("Notch : hello", Some("Notch"), Some(1))]
    #[case::empty_clan("[] Notch : hi", Some("Notch"), Some(0))]
    #[case::not_a_chat_line("Server restarting soon", None, None)]
    #[case::spoof("[x] Admin : hi", Some("Admin"), Some(0))]
    #[case::formatting_codes_gone("[§cRed§r] Notch : hi", Some("Notch"), Some(0))]
    fn the_users_formats_read_the_sender(
        #[case] text: &str,
        #[case] expected: Option<&str>,
        #[case] format: Option<usize>,
    ) {
        let found = formats(&USERS).matched(&IncomingChat::system(text));

        assert_eq!(
            found.as_ref().map(|found| found.sender().as_str()),
            expected
        );
        assert_eq!(found.map(|found| found.format()), format);
    }

    #[test]
    fn escaped_braces_match_single_ones() {
        assert_eq!(
            sender(&["{{{sender}}} : "], "{Notch} : hi"),
            Some("Notch".to_owned())
        );
        assert_eq!(sender(&["{{{sender}}} : "], "{{Notch}} : hi"), None);
    }

    #[test]
    fn any_takes_up_to_thirty_two_characters() {
        let at_most = format!("[{}] Notch : hi", "c".repeat(32));
        let past_it = format!("[{}] Notch : hi", "c".repeat(33));

        assert_eq!(sender(&USERS[..1], &at_most), Some("Notch".to_owned()));
        assert_eq!(sender(&USERS[..1], &past_it), None);
    }

    #[test]
    fn any_stops_at_the_first_place_its_text_starts_and_never_tries_again() {
        assert_eq!(sender(&USERS[..1], "[a] b] Notch : hi"), None);
        assert_eq!(
            sender(&["{any}| {any}» {sender}: "], "x| y» Notch: hi"),
            Some("Notch".to_owned())
        );
    }

    #[test]
    fn any_never_crosses_a_line_break() {
        assert_eq!(sender(&USERS[..1], "[a\nb] Notch : hi"), None);
        assert_eq!(sender(&USERS, "Notch : one\ntwo"), Some("Notch".to_owned()));
    }

    #[rstest]
    #[case::shortest("abc", true)]
    #[case::longest("A234567890123456", true)]
    #[case::too_short("ab", false)]
    #[case::too_long("A2345678901234567", false)]
    fn the_sender_is_a_valid_minecraft_name(#[case] name: &str, #[case] read: bool) {
        let found = sender(&USERS[1..], &format!("{name} : hi"));

        assert_eq!(found.is_some(), read, "{name}");
    }

    #[test]
    fn the_text_after_the_sender_must_follow_it_exactly() {
        assert_eq!(sender(&USERS[1..], "Notch: hi"), None);
        assert_eq!(sender(&USERS[1..], "Notch-x : hi"), None);
        assert_eq!(
            sender(&USERS[1..], "Notch_ : hi"),
            Some("Notch_".to_owned())
        );
    }

    #[test]
    fn literal_text_is_compared_exactly() {
        assert_eq!(
            sender(&["[{any}] {sender} SAYS "], "[x] Notch says hi"),
            None
        );
        assert_eq!(sender(&["<{sender}> "], " <Notch> hi"), None);
    }

    #[test]
    fn the_first_format_that_fits_wins() {
        let found = formats(&["<{sender}> ", "{sender} : ", "{sender} : "])
            .matched(&IncomingChat::system("Notch : hi"))
            .unwrap();

        assert_eq!(found.format(), 1);
    }

    #[rstest]
    #[case::chat(PlayerChatKind::Chat)]
    #[case::emote(PlayerChatKind::Emote)]
    #[case::whisper(PlayerChatKind::Whisper)]
    #[case::announcement(PlayerChatKind::Announcement)]
    fn only_a_system_line_is_read(#[case] kind: PlayerChatKind) {
        let line = IncomingChat::player(kind, "Steve", None, "Notch : hi");

        assert_eq!(formats(&USERS).sender_of(&line), None);
        assert_eq!(formats(&USERS).matched(&line), None);
    }

    #[test]
    fn a_match_reads_at_most_the_texts_first_characters() {
        let long = "ü".repeat(200);

        assert_eq!(READ_MAX, 137);
        assert_eq!(head(&long), "ü".repeat(READ_MAX));
        assert_eq!(head("Notch : hi"), "Notch : hi");
        assert_eq!(head(""), "");
    }

    /// Characters for a format's literal text: no name characters, so a name
    /// ends where the text after it starts; braces included, escaped.
    const LITERAL: &str = "[ ]<>:|{}-!#.,»";

    fn literal(max: usize) -> impl Strategy<Value = String> {
        proptest::collection::vec(
            proptest::sample::select(LITERAL.chars().collect::<Vec<_>>()),
            1..=max,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    fn escaped(text: &str) -> String {
        text.replace('{', "{{").replace('}', "}}")
    }

    /// A format with an optional prefix, up to two `{any}` each with its
    /// text, the sender and the text after it; a line it fits, with fills of
    /// letters (never in a literal), and the name it reads.
    fn fitting() -> impl Strategy<Value = (String, String, String)> {
        (
            proptest::option::of(literal(5)),
            proptest::collection::vec(("[a-z]{0,32}", literal(5)), 0..=2),
            "[A-Za-z0-9_]{3,16}",
            literal(5),
            any::<String>(),
        )
            .prop_map(|(prefix, wildcards, name, separator, tail)| {
                let mut format = String::new();
                let mut line = String::new();
                if let Some(prefix) = prefix {
                    format.push_str(&escaped(&prefix));
                    line.push_str(&prefix);
                }
                for (fill, until) in wildcards {
                    format.push_str(ANY);
                    format.push_str(&escaped(&until));
                    line.push_str(&fill);
                    line.push_str(&until);
                }
                format.push_str(SENDER);
                format.push_str(&escaped(&separator));
                line.push_str(&name);
                line.push_str(&separator);
                line.push_str(&tail);
                (format, line, name)
            })
    }

    proptest! {
        #[test]
        fn try_new_never_panics_and_names_a_place_in_the_list(
            entries in proptest::collection::vec(any::<String>(), 0..10),
        ) {
            if let Err(error) = ChatFormats::try_new(&entries) {
                let index = match error {
                    ChatFormatsError::TooMany { .. } => None,
                    ChatFormatsError::Empty { index }
                    | ChatFormatsError::TooLong { index, .. }
                    | ChatFormatsError::ForbiddenChar { index, .. }
                    | ChatFormatsError::UnknownPlaceholder { index, .. }
                    | ChatFormatsError::SecondPlaceholder { index, .. }
                    | ChatFormatsError::WildcardAfterSender { index, .. }
                    | ChatFormatsError::TooManyWildcards { index, .. }
                    | ChatFormatsError::WildcardWithoutLiteral { index, .. }
                    | ChatFormatsError::MissingPlaceholder { index }
                    | ChatFormatsError::NoSeparator { index }
                    | ChatFormatsError::SeparatorStartsWithNameChar { index, .. } => Some(index),
                };
                prop_assert!(index.is_none_or(|index| index < entries.len()));
            }
        }

        #[test]
        fn a_generated_format_reads_the_name_back_from_its_line(
            (format, line, name) in fitting(),
        ) {
            let taken = ChatFormats::try_new(&[format.as_str()]).unwrap();
            prop_assert_eq!(taken.iter().collect::<Vec<_>>(), [format.as_str()]);

            let found = taken.matched(&IncomingChat::system(&line)).unwrap();

            prop_assert_eq!(found.sender().as_str(), name.as_str());
            prop_assert_eq!(found.format(), 0);
        }

        #[test]
        fn a_match_is_total_and_reads_a_valid_name_from_the_texts_start(
            (format, line, _) in fitting(),
            other in any::<String>(),
            fitting_line in any::<bool>(),
        ) {
            let taken = ChatFormats::try_new(&[format.as_str(), "{sender} : "]).unwrap();
            let chat = IncomingChat::system(if fitting_line { &line } else { &other });

            if let Some(name) = taken.sender_of(&chat) {
                prop_assert!(head(chat.text()).contains(name.as_str()));
            }
        }

        #[test]
        fn text_past_the_first_characters_never_changes_the_answer(
            (format, line, _) in fitting(),
            other in any::<String>(),
            starts_fitting in any::<bool>(),
            fitting_extra in any::<bool>(),
        ) {
            let taken = ChatFormats::try_new(&[format.as_str()]).unwrap();
            // A start that fits, or one that doesn't, then the fitting line
            // again or anything: past the start, nothing may change the answer.
            let fits = if starts_fitting { IncomingChat::system(&line).text().to_owned() } else { String::new() };
            let start: String = fits
                .chars()
                .chain(core::iter::repeat('x'))
                .take(READ_MAX)
                .collect();
            let extra = if fitting_extra { &line } else { &other };

            prop_assert_eq!(
                taken.sender_of(&IncomingChat::system(&start)),
                taken.sender_of(&IncomingChat::system(&format!("{start}{extra}")))
            );
        }
    }
}
