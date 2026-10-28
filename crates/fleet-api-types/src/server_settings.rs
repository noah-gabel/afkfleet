//! The servers' settings (Plan.md P11.9, Appendix B; ADR-0020, S74–S78):
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

use fleet_core::disconnect::{ConflictTexts, ConflictTextsError};
use garde::{Error, Path, Report, Validate};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::openapi::schema::ArrayBuilder;
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema as _, ToSchema};

use crate::bots::{SERVER_ADDRESS_MAX_CHARS, valid_server_address};
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

/// The most servers with settings: fleet-core's `MAX_SERVER_SETTINGS`.
pub const SERVER_SETTINGS_MAX: u32 = fleet_core::server_settings::MAX_SERVER_SETTINGS;

/// The field message of a server that has settings already, which only the
/// server can tell.
pub const SERVER_TAKEN: &str = "already has settings";

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
    /// account: 1 to 16, each 1 to 1024 characters, line breaks allowed.
    #[garde(dive)]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = conflict_texts_schema)]
    pub conflict_texts: ConflictTextsInput,
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
    #[garde(dive)]
    #[ts(as = "Vec<String>")]
    #[schema(schema_with = conflict_texts_schema)]
    pub conflict_texts: ConflictTextsInput,
}

/// A request's conflict texts. On the wire it's the plain list of strings;
/// its rules are fleet-core's, checked by [`ConflictTextsInput::texts`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConflictTextsInput(pub Vec<String>);

/// Why a request's conflict texts were refused, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictTextsProblem {
    /// The entry it's at; `None` for the list itself.
    pub index: Option<usize>,
    /// The field message.
    pub message: &'static str,
}

impl ConflictTextsProblem {
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
    /// The texts, when fleet-core takes them and at least one is given.
    ///
    /// # Errors
    /// The first [`ConflictTextsProblem`]: an empty list, too many entries,
    /// or an entry that's empty, too long or holds a character no kick
    /// message can.
    pub fn texts(&self) -> Result<ConflictTexts, ConflictTextsProblem> {
        if self.0.is_empty() {
            return Err(ConflictTextsProblem {
                index: None,
                message: "must hold at least one text",
            });
        }
        ConflictTexts::try_new(&self.0).map_err(problem_of)
    }
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
const fn problem_of(error: ConflictTextsError) -> ConflictTextsProblem {
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
    ConflictTextsProblem { index, message }
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
        let request = create(json!({ "server": "mc.example.com", "conflict_texts": entries }));

        assert_eq!(errors(&request), []);
        assert!(request.conflict_texts.texts().is_ok());
    }

    #[rstest]
    #[case::none(vec![], "conflict_texts", "must hold at least one text")]
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
        let request = create(json!({ "server": "mc.example.com", "conflict_texts": entries }));

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
        let request = create(json!({ "server": server, "conflict_texts": ["One"] }));

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
        }));

        assert!(read.is_err());
    }

    /// Every field message reads the same after a `BoundedText`'s cleaning,
    /// so the app shows it as the server wrote it (U4).
    #[test]
    fn every_message_comes_through_the_cleaning_unchanged() {
        let messages = [
            SERVER_TAKEN,
            FORBIDDEN,
            "must hold at least one text",
            "must hold at most 16 texts",
            "must not be empty",
            "must be at most 1024 characters",
        ];

        for message in messages {
            assert_eq!(BoundedText::<256>::new(message).as_str(), message);
        }
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
