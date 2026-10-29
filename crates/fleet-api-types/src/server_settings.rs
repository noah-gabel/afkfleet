//! The servers' settings (Plan.md P11.9, Appendix B; ADR-0020, S74–S80):
//! what every bot on one Minecraft server shares, whoever's bot it is.
//! Admins and the Owner read them and, after a step-up, write them (U54).
//!
//! A request's server is any address a bot could be given; the answer
//! names it as its key, the host and a port that's always written (S74).
//! The conflict texts follow fleet-core's `ConflictTexts`: refused, never
//! repaired, each problem at its own path (`conflict_texts[3]`), and
//! duplicates counting once (S6, S77). An answer gives each text as its
//! lines, since a [`BoundedText`] shows a line break as ` | `: joined by
//! line breaks again, the lines are the text exactly.
//!
//! The chat formats follow fleet-core's `ChatFormats` the same way, kept as
//! written, in order, duplicates kept (S80, U59). Every request names them
//! and every answer holds them, an empty list when there are none (R135),
//! so an older client is refused rather than clearing formats it never
//! showed. A row holds at least one text unless it holds a format (S78,
//! R140). The preview ([`PreviewChatFormatsRequest`]) asks which sender
//! unsaved formats read from a line (U58, R134).
//!
//! A page holds at most [`SERVER_SETTINGS_PAGE_MAX`] rows, by default and at
//! most ([`ServerSettingsPageQuery`]): a row at its caps answers about 64 KiB,
//! so ten fit fleet-client's 1 MiB answer cap (PR #79's review).

use fleet_core::chat::{ChatFormats, ChatFormatsError, IncomingChat, MAX_CHAT_FORMATS};
use fleet_core::disconnect::{ConflictTexts, ConflictTextsError};
use garde::{Error, Path, Report, Validate};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::ArrayBuilder;
use utoipa::openapi::{RefOr, Schema};
use utoipa::{IntoParams, PartialSchema as _, ToSchema};

use crate::bots::{SERVER_ADDRESS_MAX_CHARS, valid_server_address};
use crate::chat::CHAT_SENDER_MAX_CHARS;
use crate::ids::{ServerSettingsId, id_schema};
use crate::int::integer_schema;
use crate::text::BoundedText;
use crate::time::Timestamp;

/// The most conflict texts a server holds: fleet-core's
/// `ConflictTexts::MAX_ENTRIES`.
pub const CONFLICT_TEXTS_MAX: usize = ConflictTexts::MAX_ENTRIES;

/// The characters of one conflict text, and so of each of its lines:
/// fleet-core's `ConflictTexts::MAX_LEN`, a kick message's cap.
pub const CONFLICT_TEXT_MAX_CHARS: usize = ConflictTexts::MAX_LEN;

/// The most chat formats a server holds: fleet-core's `MAX_CHAT_FORMATS`.
pub const CHAT_FORMATS_MAX: usize = MAX_CHAT_FORMATS;

/// The characters of one chat format as written: fleet-core's
/// `ChatFormats::MAX_LEN`.
pub const CHAT_FORMAT_MAX_CHARS: usize = ChatFormats::MAX_LEN;

/// The characters of a preview's line: what a received line keeps,
/// fleet-core's `IncomingChat::MAX_TEXT_LEN`.
pub const CHAT_PREVIEW_LINE_MAX_CHARS: usize = IncomingChat::MAX_TEXT_LEN;

/// The highest place a format has in its list, counted from 0.
const FORMAT_INDEX_MAX: u32 = 7;

const _: () = assert!(FORMAT_INDEX_MAX as usize + 1 == CHAT_FORMATS_MAX);

/// The field message of a row that would hold nothing (S78, R140).
pub const TEXTS_UNLESS_FORMATS: &str = "must hold at least one text, unless a chat format is set";

/// The field message of a preview's line past its cap.
pub const LINE_TOO_LONG: &str = "must be at most 1024 characters";

/// The most servers with settings: fleet-core's `MAX_SERVER_SETTINGS`.
pub const SERVER_SETTINGS_MAX: u32 = fleet_core::server_settings::MAX_SERVER_SETTINGS;

/// The field message of a server that has settings already, which only the
/// server can tell.
pub const SERVER_TAKEN: &str = "already has settings";

/// The most rows one page of the servers' settings holds, and how many it
/// holds when the query names no `limit`: [`MAX_SERVER_SETTINGS`] rows are
/// then at most this many pages.
///
/// [`MAX_SERVER_SETTINGS`]: fleet_core::server_settings::MAX_SERVER_SETTINGS
pub const SERVER_SETTINGS_PAGE_MAX: u32 = 10;

/// `GET /server-settings?before=<id>&limit=<n>`: which page of the servers'
/// settings to answer, newest first. Like [`crate::PageQuery`], with pages of
/// at most [`SERVER_SETTINGS_PAGE_MAX`] rows, so a page of rows at their caps
/// fits a client's answer cap. A parameter that isn't one of these two, or
/// one given twice, is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct ServerSettingsPageQuery {
    /// Only settings older than these: the `next` of the page before.
    #[garde(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<ServerSettingsId>,
    /// How many settings at most: 1 to [`SERVER_SETTINGS_PAGE_MAX`].
    #[garde(range(min = 1, max = SERVER_SETTINGS_PAGE_MAX))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl ServerSettingsPageQuery {
    /// The page size: `limit`, or [`SERVER_SETTINGS_PAGE_MAX`].
    #[must_use]
    pub fn limit_or_default(&self) -> u32 {
        self.limit.unwrap_or(SERVER_SETTINGS_PAGE_MAX)
    }
}

impl IntoParams for ServerSettingsPageQuery {
    fn into_params(_parameter_in_provider: impl Fn() -> Option<ParameterIn>) -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("before")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some(
                    "Only settings older than this ID: the `next` of the page before.",
                ))
                .schema(Some(id_schema()))
                .build(),
            ParameterBuilder::new()
                .name("limit")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some("How many settings at most; 10 by default."))
                .schema(Some(integer_schema::<1, SERVER_SETTINGS_PAGE_MAX>()))
                .build(),
        ]
    }
}

/// `POST /server-settings`: a server's new settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateServerSettingsRequest {
    /// The server: a host name or an IP address, and an optional port, as
    /// a bot's server is given (`mc.example.com`, `192.0.2.10:25566`). An
    /// address without a port is the same server as the one with 25565.
    #[garde(custom(valid_server_address))]
    #[schema(schema_with = BoundedText::<SERVER_ADDRESS_MAX_CHARS>::schema)]
    pub server: String,
    /// The kick texts that count as someone logging in with a bot's
    /// account: up to 16, each 1 to 1024 characters, line breaks allowed;
    /// at least one unless `chat_formats` holds one.
    #[garde(dive, custom(texts_unless_formats(&self.chat_formats)))]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = conflict_texts_schema)]
    pub conflict_texts: ConflictTextsInput,
    /// The chat formats, in the order they're tried: up to 8, each at most
    /// 64 characters as written, with one `{sender}`, up to two `{any}`
    /// before it and literal braces written `{{` and `}}`. Always given;
    /// `[]` for none.
    #[garde(dive)]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = chat_formats_schema)]
    pub chat_formats: ChatFormatsInput,
}

/// `PUT /server-settings/{id}`: the settings as they should be from now on,
/// and the version the caller last read (ADR-0020, S77).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateServerSettingsRequest {
    /// The `version` the caller last read: settings changed since are 409
    /// `server_settings_changed`.
    #[garde(range(min = 1))]
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>)]
    pub version: u32,
    /// The server, under `CreateServerSettingsRequest`'s rules: another
    /// server moves the settings there.
    #[garde(custom(valid_server_address))]
    #[schema(schema_with = BoundedText::<SERVER_ADDRESS_MAX_CHARS>::schema)]
    pub server: String,
    /// The conflict texts, under `CreateServerSettingsRequest`'s rules.
    #[garde(dive, custom(texts_unless_formats(&self.chat_formats)))]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = conflict_texts_schema)]
    pub conflict_texts: ConflictTextsInput,
    /// The chat formats, under `CreateServerSettingsRequest`'s rules.
    #[garde(dive)]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = chat_formats_schema)]
    pub chat_formats: ChatFormatsInput,
}

/// `POST /server-settings/chat-format-preview`: which sender unsaved chat
/// formats read from a line (ADR-0020, U58, R134).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Validate, TS, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewChatFormatsRequest {
    /// The formats, under `CreateServerSettingsRequest`'s rules.
    #[garde(dive)]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = chat_formats_schema)]
    pub chat_formats: ChatFormatsInput,
    /// A line as the server sends it, at most 1024 characters. It stands in
    /// for untrusted server text, so it's cleaned as received chat is: a
    /// character received chat never holds is dropped, not refused.
    #[garde(custom(valid_preview_line))]
    #[schema(schema_with = BoundedText::<CHAT_PREVIEW_LINE_MAX_CHARS>::schema)]
    pub line: String,
}

/// What the preview's formats read from its line: nothing when none fits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct PreviewChatFormatsResponse {
    /// The name the first fitting format read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = BoundedText::<CHAT_SENDER_MAX_CHARS>::schema, nullable = false)]
    pub sender: Option<BoundedText<CHAT_SENDER_MAX_CHARS>>,
    /// That format's place in the list, counted from 0 as field paths are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    #[schema(schema_with = integer_schema::<0, FORMAT_INDEX_MAX>, nullable = false)]
    pub format: Option<u32>,
}

/// A request's conflict texts. On the wire it's the plain list of strings;
/// its rules are fleet-core's, checked by [`ConflictTextsInput::texts`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConflictTextsInput(pub Vec<String>);

/// Why a request's list, its conflict texts or its chat formats, was
/// refused, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListProblem {
    /// The entry it's at; `None` for the list itself.
    pub index: Option<usize>,
    /// The field message.
    pub message: &'static str,
}

impl ListProblem {
    /// The problem's path under `texts`, the request's own path for its
    /// list: `conflict_texts`, or `conflict_texts[3]`.
    #[must_use]
    pub fn path(&self, texts: &Path) -> Path {
        match self.index {
            Some(index) => texts.join(index),
            None => texts.clone(),
        }
    }
}

impl ConflictTextsInput {
    /// The texts, when fleet-core takes them. Whether a row may hold none is
    /// the request's rule, with its formats (R140).
    ///
    /// # Errors
    /// The first [`ListProblem`]: too many entries, or an entry
    /// that's empty, too long or holds a character no kick message can.
    pub fn texts(&self) -> Result<ConflictTexts, ListProblem> {
        ConflictTexts::try_new(&self.0).map_err(problem_of)
    }
}

/// Whether a row with these `texts` and `formats` holds something: a text,
/// or a format (S78, R140).
#[must_use]
pub fn holds_something(texts: &ConflictTextsInput, formats: &ChatFormatsInput) -> bool {
    !texts.0.is_empty() || !formats.0.is_empty()
}

/// The garde rule of a request's texts beside its `formats`: none is
/// refused only while there's no format either (R140).
fn texts_unless_formats(
    formats: &ChatFormatsInput,
) -> impl FnOnce(&ConflictTextsInput, &()) -> garde::Result + '_ {
    move |texts, _context| {
        if holds_something(texts, formats) {
            Ok(())
        } else {
            Err(Error::new(TEXTS_UNLESS_FORMATS))
        }
    }
}

/// The garde rule of a preview's line: at most
/// [`CHAT_PREVIEW_LINE_MAX_CHARS`] characters.
fn valid_preview_line<C>(line: &str, _context: &C) -> garde::Result {
    if line.chars().count() <= CHAT_PREVIEW_LINE_MAX_CHARS {
        Ok(())
    } else {
        Err(Error::new(LINE_TOO_LONG))
    }
}

/// A request's chat formats. On the wire it's the plain list of strings;
/// its rules are fleet-core's, checked by [`ChatFormatsInput::formats`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatFormatsInput(pub Vec<String>);

impl ChatFormatsInput {
    /// The formats, when fleet-core takes them.
    ///
    /// # Errors
    /// The first [`ListProblem`] of the list: too many formats, or
    /// the first format that breaks a rule, at its own place.
    pub fn formats(&self) -> Result<ChatFormats, ListProblem> {
        ChatFormats::try_new(&self.0).map_err(format_problem_of)
    }
}

/// garde can't express fleet-core's rules, so [`ChatFormatsInput::formats`]
/// checks them and reports the first one broken at its own path.
impl Validate for ChatFormatsInput {
    type Context = ();

    fn validate_into(&self, _context: &(), parent: &mut dyn FnMut() -> Path, report: &mut Report) {
        if let Err(problem) = self.formats() {
            report.append(problem.path(&parent()), Error::new(problem.message));
        }
    }
}

/// The field message of each way a request's chat formats can be refused
/// (the texts approved at H4's planning; U59's for a brace).
const fn format_problem_of(error: ChatFormatsError) -> ListProblem {
    let (index, message) = match error {
        ChatFormatsError::TooMany { .. } => (None, "must hold at most 8 formats"),
        ChatFormatsError::Empty { index } => (Some(index), "must not be empty"),
        ChatFormatsError::TooLong { index, .. } => (Some(index), "must be at most 64 characters"),
        ChatFormatsError::ForbiddenChar { index, .. } => (
            Some(index),
            "must be one line, without control characters, formatting codes, text-direction marks or invisible characters",
        ),
        ChatFormatsError::UnknownPlaceholder { index, .. } => (
            Some(index),
            "can hold only {sender} and {any}; write a brace as {{ or }}",
        ),
        ChatFormatsError::SecondPlaceholder { index, .. }
        | ChatFormatsError::MissingPlaceholder { index } => {
            (Some(index), "must hold {sender} exactly once")
        }
        ChatFormatsError::TooManyWildcards { index, .. } => {
            (Some(index), "can hold {any} at most twice")
        }
        ChatFormatsError::WildcardAfterSender { index, .. } => {
            (Some(index), "can hold {any} only before {sender}")
        }
        ChatFormatsError::WildcardWithoutLiteral { index, .. } => {
            (Some(index), "must have text right after each {any}")
        }
        ChatFormatsError::NoSeparator { index }
        | ChatFormatsError::SeparatorStartsWithNameChar { index, .. } => (
            Some(index),
            "must have text after {sender} that doesn't start with a letter, a digit or _",
        ),
    };
    ListProblem { index, message }
}

/// garde can't express fleet-core's rules, so [`ConflictTextsInput::texts`]
/// checks them and reports the first one broken at its own path.
impl Validate for ConflictTextsInput {
    type Context = ();

    fn validate_into(&self, _context: &(), parent: &mut dyn FnMut() -> Path, report: &mut Report) {
        if let Err(problem) = self.texts() {
            report.append(problem.path(&parent()), Error::new(problem.message));
        }
    }
}

/// The field message of each way a request's conflict texts can be refused.
const fn problem_of(error: ConflictTextsError) -> ListProblem {
    let (index, message) = match error {
        ConflictTextsError::TooMany { .. } => (None, "must hold at most 16 texts"),
        ConflictTextsError::Empty { index } => (Some(index), "must not be empty"),
        ConflictTextsError::ForbiddenChar { index, .. } => (
            Some(index),
            "can't hold control characters other than line breaks, formatting codes, text-direction marks or invisible characters",
        ),
        ConflictTextsError::TooLong { index, .. } => {
            (Some(index), "must be at most 1024 characters")
        }
    };
    ListProblem { index, message }
}

/// One conflict text in an answer: its lines, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ConflictTextResponse {
    /// The text's lines: joined by line breaks, they're the text exactly.
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = conflict_text_lines_schema)]
    pub lines: Vec<BoundedText<CONFLICT_TEXT_MAX_CHARS>>,
}

impl ConflictTextResponse {
    /// Every text of `texts`, in the set's order, each as its lines.
    #[must_use]
    pub fn list(texts: &ConflictTexts) -> Vec<Self> {
        texts
            .iter()
            .map(|text| Self {
                lines: text.split('\n').map(BoundedText::new).collect(),
            })
            .collect()
    }
}

/// One server's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ServerSettingsResponse {
    /// The settings' ID.
    #[ts(type = "string")]
    #[schema(schema_with = id_schema)]
    pub id: ServerSettingsId,
    /// The server, as its key: the host and a port that's always written
    /// (`mc.example.com:25565`, `[2001:db8::1]:25565`).
    #[ts(type = "string")]
    #[schema(schema_with = BoundedText::<SERVER_ADDRESS_MAX_CHARS>::schema)]
    pub server: BoundedText<SERVER_ADDRESS_MAX_CHARS>,
    /// The conflict texts, in a fixed order, duplicates counted once.
    pub conflict_texts: Vec<ConflictTextResponse>,
    /// The chat formats as written, in the order they're tried; `[]` when
    /// there are none.
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = chat_formats_schema)]
    pub chat_formats: Vec<BoundedText<CHAT_FORMAT_MAX_CHARS>>,
    /// 1 when they were created, one more on every change.
    #[schema(schema_with = integer_schema::<1, { u32::MAX }>)]
    pub version: u32,
    /// When they were created.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub created_at: Timestamp,
    /// When they last changed.
    #[ts(type = "string")]
    #[schema(schema_with = Timestamp::schema)]
    pub updated_at: Timestamp,
}

/// `GET /server-settings`: one page of the servers' settings, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, ToSchema)]
pub struct ServerSettingsPage {
    /// The settings.
    pub items: Vec<ServerSettingsResponse>,
    /// The `before` of the next page, when one follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    #[schema(schema_with = id_schema, nullable = false)]
    pub next: Option<ServerSettingsId>,
}

/// A request's conflict texts: strings of at most
/// [`CONFLICT_TEXT_MAX_CHARS`] characters.
fn conflict_texts_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(BoundedText::<CONFLICT_TEXT_MAX_CHARS>::schema())
        .into()
}

/// Chat formats: strings of at most [`CHAT_FORMAT_MAX_CHARS`] characters.
fn chat_formats_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(BoundedText::<CHAT_FORMAT_MAX_CHARS>::schema())
        .into()
}

/// A conflict text's lines: strings of at most [`CONFLICT_TEXT_MAX_CHARS`]
/// characters.
fn conflict_text_lines_schema() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(BoundedText::<CONFLICT_TEXT_MAX_CHARS>::schema())
        .into()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn create(json: serde_json::Value) -> CreateServerSettingsRequest {
        serde_json::from_value(json).unwrap()
    }

    /// Each field error of `request` as `(path, message)`.
    fn errors(request: &impl Validate<Context = ()>) -> Vec<(String, String)> {
        request.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    fn texts(count: usize) -> Vec<String> {
        (0..count).map(|n| format!("Text {n}")).collect()
    }

    #[rstest]
    #[case::one(vec!["Already connected".to_owned()])]
    #[case::a_line_break(vec!["You logged in\nfrom another location".to_owned()])]
    #[case::the_most(texts(CONFLICT_TEXTS_MAX))]
    #[case::the_longest(vec!["x".repeat(CONFLICT_TEXT_MAX_CHARS)])]
    #[case::duplicates(vec!["Already connected".to_owned(), "Already connected".to_owned()])]
    fn valid_texts_pass(#[case] entries: Vec<String>) {
        let request = create(
            json!({ "server": "mc.example.com", "conflict_texts": entries, "chat_formats": [] }),
        );

        assert_eq!(errors(&request), []);
        assert!(request.conflict_texts.texts().is_ok());
    }

    #[rstest]
    #[case::too_many(texts(CONFLICT_TEXTS_MAX + 1), "conflict_texts", "must hold at most 16 texts")]
    #[case::empty(vec!["One".to_owned(), String::new()], "conflict_texts[1]", "must not be empty")]
    #[case::too_long(
        vec!["x".repeat(CONFLICT_TEXT_MAX_CHARS + 1)],
        "conflict_texts[0]",
        "must be at most 1024 characters"
    )]
    #[case::a_section_sign(
        vec!["One".to_owned(), "Two".to_owned(), "\u{a7}cThree".to_owned()],
        "conflict_texts[2]",
        FORBIDDEN
    )]
    #[case::a_carriage_return(vec!["One\r\nTwo".to_owned()], "conflict_texts[0]", FORBIDDEN)]
    #[case::a_bidi_control(vec!["One\u{202e}".to_owned()], "conflict_texts[0]", FORBIDDEN)]
    #[case::an_invisible_character(vec!["One\u{200b}".to_owned()], "conflict_texts[0]", FORBIDDEN)]
    fn refused_texts_are_reported_at_their_entry(
        #[case] entries: Vec<String>,
        #[case] path: &str,
        #[case] message: &str,
    ) {
        let request = create(
            json!({ "server": "mc.example.com", "conflict_texts": entries, "chat_formats": [] }),
        );

        assert_eq!(errors(&request), [(path.to_owned(), message.to_owned())]);
        assert!(request.conflict_texts.texts().is_err());
    }

    /// The message of a character no kick message can hold.
    const FORBIDDEN: &str = "can't hold control characters other than line breaks, formatting codes, text-direction marks or invisible characters";

    #[test]
    fn the_first_refused_entry_is_the_one_reported() {
        let request = create(json!({
            "server": "mc.example.com",
            "conflict_texts": ["One", "", "Two\u{202e}"],
            "chat_formats": [],
        }));

        assert_eq!(
            errors(&request),
            [(
                "conflict_texts[1]".to_owned(),
                "must not be empty".to_owned()
            )]
        );
    }

    #[test]
    fn an_update_reports_its_texts_and_its_version_like_a_create() {
        let request: UpdateServerSettingsRequest = serde_json::from_value(json!({
            "version": 0,
            "server": "mc.example.com",
            "conflict_texts": [""],
            "chat_formats": [],
        }))
        .unwrap();

        let mut found = errors(&request);
        found.sort();

        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(
            found[0],
            (
                "conflict_texts[0]".to_owned(),
                "must not be empty".to_owned()
            )
        );
        assert_eq!(found[1].0, "version");
    }

    #[rstest]
    #[case::a_scheme("tcp://mc.example.com", "must be only a host and an optional port")]
    #[case::empty("", "must not be empty")]
    fn a_server_that_isnt_an_address_is_refused_at_server(
        #[case] server: &str,
        #[case] message: &str,
    ) {
        let request =
            create(json!({ "server": server, "conflict_texts": ["One"], "chat_formats": [] }));

        assert_eq!(
            errors(&request),
            [("server".to_owned(), message.to_owned())]
        );
    }

    #[test]
    fn unknown_fields_are_refused() {
        let read = serde_json::from_value::<CreateServerSettingsRequest>(json!({
            "server": "mc.example.com",
            "conflict_texts": ["One"],
            "chat_formats": [],
            "highlights": [],
        }));

        assert!(read.is_err());
    }

    /// R135: a request without its formats is refused as a body of the
    /// wrong shape, so an older client never clears formats it never showed.
    #[test]
    fn a_request_without_its_formats_is_refused() {
        let created = serde_json::from_value::<CreateServerSettingsRequest>(json!({
            "server": "mc.example.com",
            "conflict_texts": ["One"],
        }));
        let edited = serde_json::from_value::<UpdateServerSettingsRequest>(json!({
            "version": 1,
            "server": "mc.example.com",
            "conflict_texts": ["One"],
        }));

        assert!(created.is_err());
        assert!(edited.is_err());
    }

    fn with_formats(texts: &[&str], formats: &[&str]) -> CreateServerSettingsRequest {
        create(
            json!({ "server": "mc.example.com", "conflict_texts": texts, "chat_formats": formats }),
        )
    }

    #[rstest]
    #[case::texts_only(&["One"], &[])]
    #[case::formats_only(&[], &["[{any}] {sender} : "])]
    #[case::both(&["One"], &["{sender} : ", "{sender} : "])]
    #[case::the_most(&["One"], &["{sender} : "; CHAT_FORMATS_MAX])]
    fn a_row_with_a_text_or_a_format_is_valid(#[case] texts: &[&str], #[case] formats: &[&str]) {
        let request = with_formats(texts, formats);

        assert_eq!(errors(&request), []);
        assert!(request.chat_formats.formats().is_ok());
        assert!(holds_something(
            &request.conflict_texts,
            &request.chat_formats
        ));
    }

    /// S78, R140: a row holds at least one text unless it holds a format,
    /// refused at `conflict_texts` in a create and an edit alike.
    #[test]
    fn a_row_with_neither_a_text_nor_a_format_is_refused_at_its_texts() {
        let edited: UpdateServerSettingsRequest = serde_json::from_value(json!({
            "version": 1,
            "server": "mc.example.com",
            "conflict_texts": [],
            "chat_formats": [],
        }))
        .unwrap();
        let expected = [("conflict_texts".to_owned(), TEXTS_UNLESS_FORMATS.to_owned())];

        assert_eq!(errors(&with_formats(&[], &[])), expected);
        assert_eq!(errors(&edited), expected);
    }

    #[rstest]
    #[case::too_many(&["{sender} : "; CHAT_FORMATS_MAX + 1], "chat_formats", "must hold at most 8 formats")]
    #[case::empty(&["{sender} : ", ""], "chat_formats[1]", "must not be empty")]
    #[case::too_long(&["{sender} : xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"], "chat_formats[0]", "must be at most 64 characters")]
    #[case::a_line_break(&["{sender}\n: "], "chat_formats[0]", FORMAT_ONE_LINE)]
    #[case::a_section_sign(&["\u{a7}a{sender} : "], "chat_formats[0]", FORMAT_ONE_LINE)]
    #[case::a_typo(&["{Sender} : "], "chat_formats[0]", "can hold only {sender} and {any}; write a brace as {{ or }}")]
    #[case::a_lone_brace(&["{sender} } "], "chat_formats[0]", "can hold only {sender} and {any}; write a brace as {{ or }}")]
    #[case::no_sender(&["[{any}] : "], "chat_formats[0]", "must hold {sender} exactly once")]
    #[case::two_senders(&["{sender} {sender} : "], "chat_formats[0]", "must hold {sender} exactly once")]
    #[case::three_wildcards(&["{any}a{any}b{any}c{sender} : "], "chat_formats[0]", "can hold {any} at most twice")]
    #[case::a_wildcard_after(&["{sender} : {any}"], "chat_formats[0]", "can hold {any} only before {sender}")]
    #[case::a_bare_wildcard(&["{any}{sender} : "], "chat_formats[0]", "must have text right after each {any}")]
    #[case::nothing_after(&["[{any}] {sender}"], "chat_formats[0]", SEPARATOR)]
    #[case::a_letter_after(&["{sender}x : "], "chat_formats[0]", SEPARATOR)]
    fn refused_formats_are_reported_at_their_entry(
        #[case] formats: &[&str],
        #[case] path: &str,
        #[case] message: &str,
    ) {
        let request = with_formats(&["One"], formats);

        assert_eq!(errors(&request), [(path.to_owned(), message.to_owned())]);
        assert!(request.chat_formats.formats().is_err());
    }

    /// The message of a format with a character received chat never has.
    const FORMAT_ONE_LINE: &str = "must be one line, without control characters, formatting codes, text-direction marks or invisible characters";

    /// The message of a format whose text after `{sender}` is missing or
    /// starts with a name character.
    const SEPARATOR: &str =
        "must have text after {sender} that doesn't start with a letter, a digit or _";

    #[test]
    fn an_edit_reports_its_formats_like_a_create() {
        let request: UpdateServerSettingsRequest = serde_json::from_value(json!({
            "version": 1,
            "server": "mc.example.com",
            "conflict_texts": [],
            "chat_formats": ["{sender} : ", "{Sender} : "],
        }))
        .unwrap();

        assert_eq!(
            errors(&request),
            [(
                "chat_formats[1]".to_owned(),
                "can hold only {sender} and {any}; write a brace as {{ or }}".to_owned()
            )]
        );
    }

    fn preview(json: serde_json::Value) -> PreviewChatFormatsRequest {
        serde_json::from_value(json).unwrap()
    }

    #[rstest]
    #[case::a_line("[Clan] Notch : hello")]
    #[case::empty("")]
    #[case::the_longest(&"x".repeat(CHAT_PREVIEW_LINE_MAX_CHARS))]
    #[case::stripped_characters_dropped_not_refused("\u{a7}cNotch\u{202e} : hi\u{7}")]
    fn a_preview_takes_any_line_up_to_its_cap(#[case] line: &str) {
        let request = preview(json!({ "chat_formats": ["{sender} : "], "line": line }));

        assert_eq!(errors(&request), []);
    }

    #[test]
    fn a_previews_line_past_its_cap_and_its_bad_formats_are_refused_at_their_paths() {
        let request = preview(json!({
            "chat_formats": ["{any}{sender} : "],
            "line": "x".repeat(CHAT_PREVIEW_LINE_MAX_CHARS + 1),
        }));

        let mut found = errors(&request);
        found.sort();

        assert_eq!(
            found,
            [
                (
                    "chat_formats[0]".to_owned(),
                    "must have text right after each {any}".to_owned()
                ),
                ("line".to_owned(), LINE_TOO_LONG.to_owned()),
            ]
        );
    }

    #[test]
    fn a_preview_refuses_an_unknown_field_and_needs_both() {
        let unknown = serde_json::from_value::<PreviewChatFormatsRequest>(json!({
            "chat_formats": [], "line": "", "server": "mc.example.com",
        }));
        let no_line =
            serde_json::from_value::<PreviewChatFormatsRequest>(json!({ "chat_formats": [] }));
        let no_formats = serde_json::from_value::<PreviewChatFormatsRequest>(json!({ "line": "" }));

        assert!(unknown.is_err());
        assert!(no_line.is_err());
        assert!(no_formats.is_err());
    }

    #[test]
    fn a_previews_answer_leaves_out_what_it_lacks() {
        let none = PreviewChatFormatsResponse {
            sender: None,
            format: None,
        };
        let found = PreviewChatFormatsResponse {
            sender: Some(BoundedText::new("Notch")),
            format: Some(0),
        };

        assert_eq!(serde_json::to_value(&none).unwrap(), json!({}));
        assert_eq!(
            serde_json::to_value(&found).unwrap(),
            json!({ "sender": "Notch", "format": 0 })
        );
    }

    /// Every field message reads the same after a `BoundedText`'s cleaning,
    /// so the app shows it as the server wrote it (U4).
    #[test]
    fn every_message_comes_through_the_cleaning_unchanged() {
        let messages = [
            SERVER_TAKEN,
            FORBIDDEN,
            TEXTS_UNLESS_FORMATS,
            LINE_TOO_LONG,
            FORMAT_ONE_LINE,
            SEPARATOR,
            "must hold at most 16 texts",
            "must not be empty",
            "must be at most 1024 characters",
            "must hold at most 8 formats",
            "must be at most 64 characters",
            "can hold only {sender} and {any}; write a brace as {{ or }}",
            "must hold {sender} exactly once",
            "can hold {any} at most twice",
            "can hold {any} only before {sender}",
            "must have text right after each {any}",
        ];

        for message in messages {
            assert_eq!(BoundedText::<256>::new(message).as_str(), message);
        }
    }

    fn page(limit: Option<u32>) -> ServerSettingsPageQuery {
        ServerSettingsPageQuery {
            before: None,
            limit,
        }
    }

    #[rstest]
    #[case::none(None)]
    #[case::the_smallest(Some(1))]
    #[case::the_largest(Some(10))]
    fn a_page_limit_of_1_to_10_is_valid(#[case] limit: Option<u32>) {
        assert_eq!(errors(&page(limit)), []);
    }

    #[rstest]
    #[case::zero(0, "lower than 1")]
    #[case::one_too_many(11, "greater than 10")]
    fn a_page_limit_out_of_range_breaks_its_rule(#[case] limit: u32, #[case] message: &str) {
        assert_eq!(
            errors(&page(Some(limit))),
            [("limit".to_owned(), message.to_owned())]
        );
    }

    #[test]
    fn a_page_holds_ten_rows_by_default() {
        assert_eq!(page(None).limit_or_default(), 10);
        assert_eq!(page(Some(3)).limit_or_default(), 3);
    }

    #[test]
    fn a_page_query_refuses_an_unknown_parameter() {
        let read =
            serde_json::from_value::<ServerSettingsPageQuery>(json!({ "limit": 5, "after": 1 }));

        assert!(read.is_err());
    }

    #[test]
    fn the_page_query_names_its_two_parameters_and_the_bound_of_ten() {
        let params = serde_json::to_value(ServerSettingsPageQuery::into_params(|| None)).unwrap();

        assert_eq!(params[0]["name"], "before");
        assert_eq!(params[0]["in"], "query");
        assert_eq!(params[0]["schema"]["maxLength"], 36);
        assert_eq!(params[1]["name"], "limit");
        assert_eq!(
            params[1]["description"],
            "How many settings at most; 10 by default."
        );
        assert_eq!(params[1]["schema"]["minimum"], 1);
        assert_eq!(params[1]["schema"]["maximum"], 10);
        assert_eq!(params.as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_text_is_answered_as_its_lines_in_the_sets_order() {
        let set =
            ConflictTexts::try_new(&["You logged in\nfrom another location", "Already connected"])
                .unwrap();

        let answered = ConflictTextResponse::list(&set);

        assert_eq!(
            serde_json::to_value(&answered).unwrap(),
            json!([
                { "lines": ["Already connected"] },
                { "lines": ["You logged in", "from another location"] },
            ])
        );
    }

    #[test]
    fn the_lines_joined_again_are_the_text_exactly() {
        let long = "x".repeat(CONFLICT_TEXT_MAX_CHARS);
        let originals = ["a\n\nb", "\nleading", "trailing\n", long.as_str()];
        let set = ConflictTexts::try_new(&originals).unwrap();

        let joined: Vec<String> = ConflictTextResponse::list(&set)
            .iter()
            .map(|text| {
                text.lines
                    .iter()
                    .map(BoundedText::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();

        assert_eq!(joined, set.iter().collect::<Vec<_>>());
    }
}
