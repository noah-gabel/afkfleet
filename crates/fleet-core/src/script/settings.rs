//! [`Script`]: a scripted mode's body, its source with the settings and
//! declarations beside it.

use serde::{Deserialize, Serialize};

use super::facts::{Identifier, IdentifierError};
use super::pattern::{ChatPattern, ChatPatterns, ChatPatternsError, PatternDraft, PatternError};
use super::source::{ScriptSource, ScriptSourceError};
use super::template::{MessageDraft, MessageTemplate, TemplateError};

/// Why a number isn't a [`LeashRadius`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the leash is {blocks} blocks; it must be {min} to {max}", min = LeashRadius::MIN, max = LeashRadius::MAX)]
pub struct LeashRadiusError {
    /// The radius asked for, in blocks.
    pub blocks: u8,
}

/// How far a scripted bot walks from where it was put, in blocks: 1 to 64,
/// 8 unless the mode says otherwise. There is no "off" (ADR-0036, D10; the
/// user's answer 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct LeashRadius(u8);

impl LeashRadius {
    /// The shortest leash, in blocks.
    pub const MIN: u8 = 1;
    /// The longest leash, in blocks: well inside a server's view distance.
    pub const MAX: u8 = 64;
    /// The leash of a mode that sets none: 8 blocks.
    pub const DEFAULT: Self = Self(8);

    /// The radius, in blocks.
    #[must_use]
    pub const fn blocks(self) -> u8 {
        self.0
    }
}

impl Default for LeashRadius {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u8> for LeashRadius {
    type Error = LeashRadiusError;

    fn try_from(blocks: u8) -> Result<Self, LeashRadiusError> {
        if (Self::MIN..=Self::MAX).contains(&blocks) {
            Ok(Self(blocks))
        } else {
            Err(LeashRadiusError { blocks })
        }
    }
}

impl From<LeashRadius> for u8 {
    fn from(radius: LeashRadius) -> Self {
        radius.0
    }
}

/// Why a number isn't a [`ToolGuard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the tool guard is {uses} uses; it must be 0 to {max}", max = ToolGuard::MAX)]
pub struct ToolGuardError {
    /// The number of uses asked for.
    pub uses: u8,
}

/// How many uses a tool keeps when a scripted bot stops mining with it:
/// 2 unless the mode says otherwise, 0 to mine a tool to its end
/// (ADR-0036, D11; the user's answer 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct ToolGuard(u8);

impl ToolGuard {
    /// The most uses a guard keeps.
    pub const MAX: u8 = 100;
    /// The guard of a mode that sets none: on, at 2 uses left.
    pub const DEFAULT: Self = Self(2);
    /// No guard: a tool is mined until it breaks.
    pub const OFF: Self = Self(0);

    /// How many uses the guard keeps; 0 for none.
    #[must_use]
    pub const fn uses_left(self) -> u8 {
        self.0
    }

    /// Whether the guard is off.
    #[must_use]
    pub const fn is_off(self) -> bool {
        self.0 == 0
    }
}

impl Default for ToolGuard {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u8> for ToolGuard {
    type Error = ToolGuardError;

    fn try_from(uses: u8) -> Result<Self, ToolGuardError> {
        if uses <= Self::MAX {
            Ok(Self(uses))
        } else {
            Err(ToolGuardError { uses })
        }
    }
}

impl From<ToolGuard> for u8 {
    fn from(guard: ToolGuard) -> Self {
        guard.0
    }
}

/// Why a list of blocks isn't a [`BlockGuard`]. `block` is a place in the
/// list, from 0; no error holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockGuardError {
    /// The list is longer than [`BlockGuard::MAX_BLOCKS`].
    #[error("the block guard lists {count} blocks; the limit is {max}", max = BlockGuard::MAX_BLOCKS)]
    TooMany {
        /// How many it lists.
        count: usize,
    },
    /// An entry isn't a registry identifier.
    #[error("block {block} of the block guard: {reason}")]
    Block {
        /// The entry's place.
        block: usize,
        /// Why it isn't one.
        reason: IdentifierError,
    },
    /// A block is listed twice, with or without its namespace.
    #[error("block {block} of the block guard is listed before")]
    Duplicate {
        /// The second one's place.
        block: usize,
    },
}

/// The blocks a held left button mines: up to [`BlockGuard::MAX_BLOCKS`]
/// registry identifiers, or none for any block, as in vanilla (ADR-0042's
/// amendment, G13; the user's answer to PR #126).
///
/// A held left button goes on to whatever the crosshair lands on once its
/// block is gone. At a farm that can be the wall that holds the lava, so a
/// mode says what its script mines: on any other block a hold pauses, and
/// a new hold is refused. The floor guard and the [`ToolGuard`] hold
/// beside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct BlockGuard(Vec<Identifier>);

impl BlockGuard {
    /// The most blocks a mode lists.
    pub const MAX_BLOCKS: usize = 16;
    /// No list: any block is mined.
    pub const ANY: Self = Self(Vec::new());

    /// The listed blocks, in the order they were listed.
    #[must_use]
    pub fn blocks(&self) -> &[Identifier] {
        &self.0
    }

    /// Whether the guard is off: nothing is listed, so any block is mined.
    #[must_use]
    pub fn is_off(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<String>> for BlockGuard {
    type Error = BlockGuardError;

    /// Checks a list: how many, then each entry in order (an identifier,
    /// not listed before). Text without a namespace is in `minecraft`, as
    /// everywhere, so `cobblestone` and `minecraft:cobblestone` are one
    /// block.
    fn try_from(blocks: Vec<String>) -> Result<Self, BlockGuardError> {
        let count = blocks.len();
        if count > Self::MAX_BLOCKS {
            return Err(BlockGuardError::TooMany { count });
        }
        let mut listed: Vec<Identifier> = Vec::with_capacity(count);
        for (block, name) in blocks.iter().enumerate() {
            let identifier = Identifier::try_from(name.as_str())
                .map_err(|reason| BlockGuardError::Block { block, reason })?;
            if listed.contains(&identifier) {
                return Err(BlockGuardError::Duplicate { block });
            }
            listed.push(identifier);
        }
        Ok(Self(listed))
    }
}

impl From<BlockGuard> for Vec<String> {
    fn from(guard: BlockGuard) -> Self {
        guard
            .0
            .iter()
            .map(|block| block.as_str().to_owned())
            .collect()
    }
}

/// Why a number isn't a [`ScriptApiLevel`] this version knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("script API level {level} isn't known here; the newest is {newest}", newest = ScriptApiLevel::NEWEST.get())]
pub struct ScriptApiLevelError {
    /// The level asked for.
    pub level: u8,
}

/// The level of the script API a mode was validated at, and the level an
/// agent runs (ADR-0036, D13).
///
/// A script needs an agent at its level or above, so the server never
/// sends a scripted mode to an agent below it. Phase 15's API is level 1;
/// Phase 16's inventory and menus make it 2. A level this version doesn't
/// know is refused: a stored mode from a newer version fails to load
/// instead of running in part (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct ScriptApiLevel(u8);

impl ScriptApiLevel {
    /// Phase 15's API: keys, mouse, look, hotbar, vehicles, chat, timers,
    /// facts.
    pub const V1: Self = Self(1);
    /// The newest level this version knows.
    pub const NEWEST: Self = Self::V1;

    /// The level's number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ScriptApiLevel {
    type Error = ScriptApiLevelError;

    fn try_from(level: u8) -> Result<Self, ScriptApiLevelError> {
        if (Self::V1.0..=Self::NEWEST.0).contains(&level) {
            Ok(Self(level))
        } else {
            Err(ScriptApiLevelError { level })
        }
    }
}

impl From<ScriptApiLevel> for u8 {
    fn from(level: ScriptApiLevel) -> Self {
        level.0
    }
}

/// A scripted mode's body as written, before [`Script::try_from`] has
/// checked it.
///
/// Its JSON is `{"api":…,"source":…,"leash":…,"tool_guard":…,
/// "block_guard":[…],"messages":[…],"patterns":[…]}`. Every field is required, so a mode
/// always says what it runs under; unknown fields are ignored (ADR-0010).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptDraft {
    /// The API level the script was validated at.
    pub api: u8,
    /// The script's text.
    pub source: String,
    /// The leash's radius, in blocks.
    pub leash: u8,
    /// How many uses the tool guard keeps.
    pub tool_guard: u8,
    /// The blocks a held left button mines; none for any block.
    pub block_guard: Vec<String>,
    /// The messages the script may send.
    pub messages: Vec<MessageDraft>,
    /// The chat lines the script reads.
    pub patterns: Vec<PatternDraft>,
}

/// Why a [`ScriptDraft`] isn't a scripted mode's body.
///
/// `message` and `pattern` are places in their lists, from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// The API level isn't one this version knows.
    #[error(transparent)]
    Api(#[from] ScriptApiLevelError),
    /// The source isn't a [`ScriptSource`].
    #[error(transparent)]
    Source(#[from] ScriptSourceError),
    /// The leash isn't a [`LeashRadius`].
    #[error(transparent)]
    Leash(#[from] LeashRadiusError),
    /// The tool guard isn't a [`ToolGuard`].
    #[error(transparent)]
    ToolGuard(#[from] ToolGuardError),
    /// The block guard isn't a [`BlockGuard`].
    #[error(transparent)]
    BlockGuard(#[from] BlockGuardError),
    /// There are more than [`Script::MAX_MESSAGES`] declared messages.
    #[error("the mode declares {count} messages; the limit is {max}", max = Script::MAX_MESSAGES)]
    TooManyMessages {
        /// How many there are.
        count: usize,
    },
    /// A declared message isn't a [`MessageTemplate`].
    #[error("message {message}: {reason}")]
    Message {
        /// The message's place.
        message: usize,
        /// Why it isn't one.
        reason: TemplateError,
    },
    /// Two declared messages have the same key.
    #[error("message {message} has the key of an earlier one")]
    DuplicateMessage {
        /// The second one's place.
        message: usize,
    },
    /// A chat pattern isn't a [`ChatPattern`].
    #[error("chat pattern {pattern}: {reason}")]
    Pattern {
        /// The pattern's place.
        pattern: usize,
        /// Why it isn't one.
        reason: PatternError,
    },
    /// The chat patterns aren't a list a mode may declare.
    #[error(transparent)]
    Patterns(#[from] ChatPatternsError),
}

/// A scripted mode's body: the script's source, the API level it was
/// validated at, and what the mode sets and declares beside it (ADR-0036,
/// D3, D8, D10, D11, D13; ADR-0037).
///
/// - **The leash, the tool guard and the block guard** are the mode's, not
///   the script's: a script can't loosen them.
/// - **The declared messages** are every text the script can send, and
///   **the chat patterns** every line it reads. A message's key and a
///   pattern's key are told apart by where a script uses them, so a
///   command and its reply may share one (`balance`).
///
/// This type says nothing about whether the source is a valid program:
/// `fleet-script` compiles it against these settings when the mode is
/// saved, and again on the agent. It reads and writes the JSON of a
/// [`ScriptDraft`], validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ScriptDraft", into = "ScriptDraft")]
pub struct Script {
    api: ScriptApiLevel,
    source: ScriptSource,
    leash: LeashRadius,
    tool_guard: ToolGuard,
    block_guard: BlockGuard,
    messages: Vec<MessageTemplate>,
    patterns: ChatPatterns,
}

impl Script {
    /// The most messages a mode declares.
    pub const MAX_MESSAGES: usize = 16;

    /// The API level the script was validated at.
    #[must_use]
    pub const fn api(&self) -> ScriptApiLevel {
        self.api
    }

    /// The script's text.
    #[must_use]
    pub const fn source(&self) -> &ScriptSource {
        &self.source
    }

    /// The leash's radius.
    #[must_use]
    pub const fn leash(&self) -> LeashRadius {
        self.leash
    }

    /// The tool guard.
    #[must_use]
    pub const fn tool_guard(&self) -> ToolGuard {
        self.tool_guard
    }

    /// The block guard: the blocks a held left button mines.
    #[must_use]
    pub const fn block_guard(&self) -> &BlockGuard {
        &self.block_guard
    }

    /// The declared messages, in the order they were declared.
    #[must_use]
    pub fn messages(&self) -> &[MessageTemplate] {
        &self.messages
    }

    /// The declared message with this key.
    #[must_use]
    pub fn message(&self, key: &str) -> Option<&MessageTemplate> {
        self.messages
            .iter()
            .find(|message| message.key().as_str() == key)
    }

    /// The chat patterns.
    #[must_use]
    pub const fn patterns(&self) -> &ChatPatterns {
        &self.patterns
    }

    /// The names of the commands among the declared messages, in declared
    /// order, for the allowlist check: what
    /// [`MessageTemplate::command_name`] gives for each.
    pub fn command_names(&self) -> impl Iterator<Item = &str> {
        self.messages
            .iter()
            .filter_map(MessageTemplate::command_name)
    }
}

impl TryFrom<ScriptDraft> for Script {
    type Error = ScriptError;

    /// Checks a scripted mode's body.
    ///
    /// The checks run in this order, the first problem winning: the API
    /// level, the source, the leash, the tool guard, the block guard; the
    /// messages (how
    /// many, then each in order: itself, a key used before); the patterns
    /// (each in order, then the list).
    fn try_from(draft: ScriptDraft) -> Result<Self, ScriptError> {
        let api = ScriptApiLevel::try_from(draft.api)?;
        let source = ScriptSource::try_from(draft.source)?;
        let leash = LeashRadius::try_from(draft.leash)?;
        let tool_guard = ToolGuard::try_from(draft.tool_guard)?;
        let block_guard = BlockGuard::try_from(draft.block_guard)?;

        let count = draft.messages.len();
        if count > Self::MAX_MESSAGES {
            return Err(ScriptError::TooManyMessages { count });
        }
        let mut messages: Vec<MessageTemplate> = Vec::with_capacity(count);
        for (message, draft) in draft.messages.into_iter().enumerate() {
            let template = MessageTemplate::try_from(draft)
                .map_err(|reason| ScriptError::Message { message, reason })?;
            if messages
                .iter()
                .any(|earlier| earlier.key() == template.key())
            {
                return Err(ScriptError::DuplicateMessage { message });
            }
            messages.push(template);
        }

        let patterns = draft
            .patterns
            .into_iter()
            .enumerate()
            .map(|(pattern, draft)| {
                ChatPattern::try_from(draft)
                    .map_err(|reason| ScriptError::Pattern { pattern, reason })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let patterns = ChatPatterns::try_new(patterns)?;

        Ok(Self {
            api,
            source,
            leash,
            tool_guard,
            block_guard,
            messages,
            patterns,
        })
    }
}

impl From<Script> for ScriptDraft {
    fn from(script: Script) -> Self {
        Self {
            api: script.api.get(),
            source: script.source.into(),
            leash: script.leash.blocks(),
            tool_guard: script.tool_guard.uses_left(),
            block_guard: script.block_guard.into(),
            messages: script.messages.into_iter().map(Into::into).collect(),
            patterns: script
                .patterns
                .iter()
                .cloned()
                .map(PatternDraft::from)
                .collect(),
        }
    }
}

/// A draft with nothing declared, for the tests of this crate.
#[cfg(test)]
pub(crate) fn draft_of(source: &str) -> ScriptDraft {
    ScriptDraft {
        api: 1,
        source: source.to_owned(),
        leash: 8,
        tool_guard: 2,
        block_guard: Vec::new(),
        messages: Vec::new(),
        patterns: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{
        CaptureDraft, CaptureKind, DecimalMark, IdentError, ValueDraft, ValueKind, When,
    };
    use rstest::rstest;

    fn message(key: &str, text: &str) -> MessageDraft {
        MessageDraft {
            key: key.to_owned(),
            text: text.to_owned(),
            values: Vec::new(),
        }
    }

    fn pattern(key: &str, text: &str) -> PatternDraft {
        PatternDraft {
            key: key.to_owned(),
            when: When::Reply,
            text: text.to_owned(),
            captures: Vec::new(),
        }
    }

    /// The reference's balance example (docs/scripting.md).
    fn balance() -> ScriptDraft {
        ScriptDraft {
            messages: vec![
                message("balance", "/balance"),
                MessageDraft {
                    key: "pay".to_owned(),
                    text: "/pay AfkMain {amount}".to_owned(),
                    values: vec![ValueDraft {
                        name: "amount".to_owned(),
                        kind: ValueKind::Decimal {
                            min: 1.0,
                            max: 1_000_000.0,
                            places: 2,
                        },
                    }],
                },
                message("thanks", "thank you"),
            ],
            patterns: vec![PatternDraft {
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
            }],
            ..draft_of("fn on_join(first) {\n    chat.send(\"balance\");\n}\n")
        }
    }

    // --- The leash, the tool guard, the level ---

    #[rstest]
    #[case::the_shortest(1)]
    #[case::the_default(8)]
    #[case::the_longest(64)]
    fn a_leash_is_1_to_64_blocks(#[case] blocks: u8) {
        let radius = LeashRadius::try_from(blocks).unwrap();

        assert_eq!(radius.blocks(), blocks);
        assert_eq!(u8::from(radius), blocks);
    }

    #[rstest]
    #[case::off(0)]
    #[case::too_long(65)]
    #[case::far_too_long(255)]
    fn a_leash_is_never_off_and_never_longer(#[case] blocks: u8) {
        assert_eq!(
            LeashRadius::try_from(blocks),
            Err(LeashRadiusError { blocks })
        );
    }

    #[test]
    fn the_leash_is_8_blocks_unless_set() {
        assert_eq!(LeashRadius::default().blocks(), 8);
        assert_eq!(LeashRadius::DEFAULT, LeashRadius::try_from(8).unwrap());
        assert_eq!((LeashRadius::MIN, LeashRadius::MAX), (1, 64));
        assert_eq!(
            LeashRadiusError { blocks: 0 }.to_string(),
            "the leash is 0 blocks; it must be 1 to 64"
        );
    }

    #[rstest]
    #[case::off(0, true)]
    #[case::the_default(2, false)]
    #[case::the_most(100, false)]
    fn a_tool_guard_keeps_0_to_100_uses(#[case] uses: u8, #[case] off: bool) {
        let guard = ToolGuard::try_from(uses).unwrap();

        assert_eq!(guard.uses_left(), uses);
        assert_eq!(guard.is_off(), off);
        assert_eq!(u8::from(guard), uses);
    }

    #[test]
    fn a_tool_guard_keeps_no_more_than_100_uses() {
        assert_eq!(ToolGuard::try_from(101), Err(ToolGuardError { uses: 101 }));
        assert_eq!(
            ToolGuardError { uses: 101 }.to_string(),
            "the tool guard is 101 uses; it must be 0 to 100"
        );
    }

    #[test]
    fn the_tool_guard_is_on_at_2_uses_unless_set() {
        assert_eq!(ToolGuard::default().uses_left(), 2);
        assert!(!ToolGuard::default().is_off());
        assert_eq!(ToolGuard::OFF, ToolGuard::try_from(0).unwrap());
    }

    #[test]
    fn phase_15s_api_is_level_1_and_no_other_is_known() {
        assert_eq!(ScriptApiLevel::try_from(1), Ok(ScriptApiLevel::V1));
        assert_eq!(ScriptApiLevel::NEWEST.get(), 1);
        for level in [0, 2, 255] {
            assert_eq!(
                ScriptApiLevel::try_from(level),
                Err(ScriptApiLevelError { level })
            );
        }
        assert_eq!(
            ScriptApiLevelError { level: 2 }.to_string(),
            "script API level 2 isn't known here; the newest is 1"
        );
    }

    #[test]
    fn the_numbers_read_and_write_as_plain_numbers() {
        assert_eq!(serde_json::to_string(&LeashRadius::DEFAULT).unwrap(), "8");
        assert_eq!(serde_json::to_string(&ToolGuard::OFF).unwrap(), "0");
        assert_eq!(serde_json::to_string(&ScriptApiLevel::V1).unwrap(), "1");
        assert!(serde_json::from_str::<LeashRadius>("0").is_err());
        assert!(serde_json::from_str::<ToolGuard>("101").is_err());
        assert!(serde_json::from_str::<ScriptApiLevel>("2").is_err());
        assert!(serde_json::from_str::<LeashRadius>("-1").is_err());
    }

    // --- The block guard ---

    fn blocks(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_block_guard_lists_its_blocks_with_their_namespace() {
        let guard = BlockGuard::try_from(blocks(&["cobblestone", "lobby:ore"])).unwrap();

        let listed: Vec<&str> = guard.blocks().iter().map(Identifier::as_str).collect();
        assert_eq!(listed, ["minecraft:cobblestone", "lobby:ore"]);
        assert!(!guard.is_off());
        assert_eq!(
            Vec::<String>::from(guard),
            blocks(&["minecraft:cobblestone", "lobby:ore"])
        );
    }

    #[test]
    fn a_block_guard_that_lists_nothing_is_off() {
        let guard = BlockGuard::try_from(Vec::new()).unwrap();

        assert!(guard.is_off());
        assert_eq!(guard, BlockGuard::ANY);
        assert_eq!(guard, BlockGuard::default());
        assert_eq!(guard.blocks(), []);
    }

    #[test]
    fn a_block_guard_lists_up_to_16_blocks() {
        let names = |count: usize| (0..count).map(|n| format!("block_{n}")).collect::<Vec<_>>();

        assert_eq!(BlockGuard::try_from(names(16)).unwrap().blocks().len(), 16);
        assert_eq!(
            BlockGuard::try_from(names(17)),
            Err(BlockGuardError::TooMany { count: 17 })
        );
        assert_eq!(BlockGuard::MAX_BLOCKS, 16);
    }

    #[rstest]
    #[case::text_that_is_no_identifier(
        &["stone", "Not A Block"],
        BlockGuardError::Block { block: 1, reason: IdentifierError::InvalidChar { index: 0 } }
    )]
    #[case::an_empty_entry(
        &[""],
        BlockGuardError::Block { block: 0, reason: IdentifierError::Empty }
    )]
    #[case::a_block_listed_twice(
        &["stone", "dirt", "stone"],
        BlockGuardError::Duplicate { block: 2 }
    )]
    #[case::a_block_listed_with_and_without_its_namespace(
        &["cobblestone", "minecraft:cobblestone"],
        BlockGuardError::Duplicate { block: 1 }
    )]
    fn refuses_a_list_that_is_no_block_guard(
        #[case] names: &[&str],
        #[case] error: BlockGuardError,
    ) {
        assert_eq!(BlockGuard::try_from(blocks(names)), Err(error));
    }

    #[test]
    fn the_lists_length_is_judged_before_its_entries() {
        let mut names = vec![String::new(); 17];
        names.push("Not A Block".to_owned());

        assert_eq!(
            BlockGuard::try_from(names),
            Err(BlockGuardError::TooMany { count: 18 })
        );
    }

    #[test]
    fn a_block_guards_errors_say_where_and_never_what() {
        assert_eq!(
            BlockGuardError::TooMany { count: 17 }.to_string(),
            "the block guard lists 17 blocks; the limit is 16"
        );
        assert_eq!(
            BlockGuardError::Block {
                block: 1,
                reason: IdentifierError::InvalidChar { index: 0 }
            }
            .to_string(),
            "block 1 of the block guard: the identifier holds a character it may not, at position 0"
        );
        assert_eq!(
            BlockGuardError::Duplicate { block: 2 }.to_string(),
            "block 2 of the block guard is listed before"
        );
    }

    #[test]
    fn a_block_guard_reads_and_writes_as_a_list_of_names() {
        let guard = BlockGuard::try_from(blocks(&["cobblestone"])).unwrap();

        let json = serde_json::to_string(&guard).unwrap();

        assert_eq!(json, r#"["minecraft:cobblestone"]"#);
        assert_eq!(serde_json::from_str::<BlockGuard>(&json).unwrap(), guard);
        assert_eq!(
            serde_json::from_str::<BlockGuard>(r#"["cobblestone"]"#).unwrap(),
            guard
        );
        assert!(serde_json::from_str::<BlockGuard>(r#"["a","a"]"#).is_err());
        assert!(serde_json::from_str::<BlockGuard>("[1]").is_err());
    }

    // --- The body ---

    #[test]
    fn a_script_keeps_what_was_set_and_declared() {
        let script = Script::try_from(ScriptDraft {
            leash: 16,
            tool_guard: 0,
            block_guard: blocks(&["cobblestone", "stone"]),
            ..balance()
        })
        .unwrap();

        assert_eq!(script.api(), ScriptApiLevel::V1);
        assert!(script.source().as_str().starts_with("fn on_join(first)"));
        assert_eq!(script.leash().blocks(), 16);
        assert!(script.tool_guard().is_off());
        assert_eq!(script.block_guard().blocks().len(), 2);
        let keys: Vec<&str> = script
            .messages()
            .iter()
            .map(|message| message.key().as_str())
            .collect();
        assert_eq!(keys, ["balance", "pay", "thanks"]);
        assert_eq!(script.patterns().len(), 1);
        assert!(script.patterns().get("balance").is_some());
    }

    #[test]
    fn a_script_with_nothing_declared_sends_and_reads_nothing() {
        let script = Script::try_from(draft_of("")).unwrap();

        assert_eq!(script.messages(), []);
        assert!(script.patterns().is_empty());
        assert_eq!(script.command_names().count(), 0);
        assert_eq!(script.message("balance"), None);
        assert_eq!(script.leash(), LeashRadius::DEFAULT);
        assert_eq!(script.tool_guard(), ToolGuard::DEFAULT);
        assert!(script.block_guard().is_off());
    }

    #[test]
    fn a_declared_message_is_found_by_its_key() {
        let script = Script::try_from(balance()).unwrap();

        assert_eq!(
            script.message("pay").map(MessageTemplate::text),
            Some("/pay AfkMain {amount}")
        );
        assert_eq!(script.message("Pay"), None);
        assert_eq!(script.message("amount"), None);
    }

    /// What the allowlist judges: the declared commands' names, and no
    /// plain chat.
    #[test]
    fn the_declared_commands_are_named_in_declared_order() {
        let script = Script::try_from(balance()).unwrap();

        let names: Vec<&str> = script.command_names().collect();

        assert_eq!(names, ["balance", "pay"]);
    }

    #[test]
    fn a_message_and_a_pattern_may_share_a_key() {
        let script = Script::try_from(balance()).unwrap();

        assert!(script.message("balance").is_some());
        assert!(script.patterns().get("balance").is_some());
    }

    #[test]
    fn a_mode_declares_up_to_16_messages() {
        let messages = |count: usize| {
            (0..count)
                .map(|n| message(&format!("m{n}"), "hello"))
                .collect::<Vec<_>>()
        };

        let sixteen = ScriptDraft {
            messages: messages(16),
            ..draft_of("")
        };
        assert_eq!(Script::try_from(sixteen).unwrap().messages().len(), 16);

        let seventeen = ScriptDraft {
            messages: messages(17),
            ..draft_of("")
        };
        assert_eq!(
            Script::try_from(seventeen),
            Err(ScriptError::TooManyMessages { count: 17 })
        );
    }

    #[rstest]
    #[case::an_unknown_level(
        ScriptDraft { api: 2, ..draft_of("") },
        ScriptError::Api(ScriptApiLevelError { level: 2 })
    )]
    #[case::a_source_that_isnt_one(
        draft_of("a\u{7}"),
        ScriptError::Source(ScriptSourceError::ForbiddenChar { line: 1, column: 2 })
    )]
    #[case::no_leash(
        ScriptDraft { leash: 0, ..draft_of("") },
        ScriptError::Leash(LeashRadiusError { blocks: 0 })
    )]
    #[case::a_tool_guard_too_large(
        ScriptDraft { tool_guard: 200, ..draft_of("") },
        ScriptError::ToolGuard(ToolGuardError { uses: 200 })
    )]
    #[case::a_block_guard_that_isnt_one(
        ScriptDraft { block_guard: vec!["stone".to_owned(), "stone".to_owned()], ..draft_of("") },
        ScriptError::BlockGuard(BlockGuardError::Duplicate { block: 1 })
    )]
    #[case::a_message_that_isnt_one(
        ScriptDraft { messages: vec![message("ok", "hello"), message("Bad", "hello")], ..draft_of("") },
        ScriptError::Message { message: 1, reason: TemplateError::Key(IdentError::InvalidChar { index: 0 }) }
    )]
    #[case::two_messages_with_one_key(
        ScriptDraft {
            messages: vec![message("a", "hello"), message("b", "hello"), message("a", "bye")],
            ..draft_of("")
        },
        ScriptError::DuplicateMessage { message: 2 }
    )]
    #[case::a_pattern_that_isnt_one(
        ScriptDraft { patterns: vec![pattern("ok", "Welcome back"), pattern("p", "x")], ..draft_of("") },
        ScriptError::Pattern { pattern: 1, reason: PatternError::TooLittleText { chars: 1 } }
    )]
    #[case::two_patterns_with_one_key(
        ScriptDraft {
            patterns: vec![pattern("a", "Welcome back"), pattern("a", "Good bye")],
            ..draft_of("")
        },
        ScriptError::Patterns(ChatPatternsError::DuplicateKey { pattern: 1 })
    )]
    #[case::nine_patterns(
        ScriptDraft {
            patterns: (0..9).map(|n| pattern(&format!("p{n}"), "Welcome back")).collect(),
            ..draft_of("")
        },
        ScriptError::Patterns(ChatPatternsError::TooMany { count: 9 })
    )]
    fn refuses_a_body_that_isnt_one(#[case] draft: ScriptDraft, #[case] error: ScriptError) {
        assert_eq!(Script::try_from(draft), Err(error));
    }

    #[rstest]
    #[case::the_level_before_the_source(
        ScriptDraft { api: 0, leash: 0, ..draft_of("\u{7}") },
        ScriptError::Api(ScriptApiLevelError { level: 0 })
    )]
    #[case::the_source_before_the_leash(
        ScriptDraft { leash: 0, tool_guard: 200, ..draft_of("\u{7}") },
        ScriptError::Source(ScriptSourceError::ForbiddenChar { line: 1, column: 1 })
    )]
    #[case::the_leash_before_the_tool_guard(
        ScriptDraft { leash: 0, tool_guard: 200, ..draft_of("") },
        ScriptError::Leash(LeashRadiusError { blocks: 0 })
    )]
    #[case::the_tool_guard_before_the_block_guard(
        ScriptDraft { tool_guard: 200, block_guard: vec![String::new()], ..draft_of("") },
        ScriptError::ToolGuard(ToolGuardError { uses: 200 })
    )]
    #[case::the_block_guard_before_a_message(
        ScriptDraft { block_guard: vec![String::new()], messages: vec![message("Bad", "x")], ..draft_of("") },
        ScriptError::BlockGuard(BlockGuardError::Block { block: 0, reason: IdentifierError::Empty })
    )]
    #[case::the_messages_count_before_any_message(
        ScriptDraft {
            messages: (0..17).map(|_| message("Bad", "x")).collect(),
            ..draft_of("")
        },
        ScriptError::TooManyMessages { count: 17 }
    )]
    #[case::a_message_before_a_pattern(
        ScriptDraft {
            messages: vec![message("a", "x"), message("a", "y")],
            patterns: vec![pattern("p", "x")],
            ..draft_of("")
        },
        ScriptError::DuplicateMessage { message: 1 }
    )]
    #[case::a_pattern_before_the_list(
        ScriptDraft {
            patterns: (0..9).map(|_| pattern("p", "x")).collect(),
            ..draft_of("")
        },
        ScriptError::Pattern { pattern: 0, reason: PatternError::TooLittleText { chars: 1 } }
    )]
    fn the_first_problem_wins(#[case] draft: ScriptDraft, #[case] error: ScriptError) {
        assert_eq!(Script::try_from(draft), Err(error));
    }

    #[test]
    fn errors_say_where_and_never_what() {
        assert_eq!(
            ScriptError::Message {
                message: 3,
                reason: TemplateError::Empty
            }
            .to_string(),
            "message 3: the message is empty"
        );
        assert_eq!(
            ScriptError::DuplicateMessage { message: 2 }.to_string(),
            "message 2 has the key of an earlier one"
        );
        assert_eq!(
            ScriptError::TooManyMessages { count: 17 }.to_string(),
            "the mode declares 17 messages; the limit is 16"
        );
        assert_eq!(
            ScriptError::Leash(LeashRadiusError { blocks: 0 }).to_string(),
            "the leash is 0 blocks; it must be 1 to 64"
        );
    }

    // --- JSON ---

    #[test]
    fn a_body_reads_back_from_its_json_as_itself() {
        let listed = ScriptDraft {
            block_guard: blocks(&["minecraft:cobblestone"]),
            ..balance()
        };
        let script = Script::try_from(listed.clone()).unwrap();
        assert_eq!(ScriptDraft::from(script), listed);
        let script = Script::try_from(balance()).unwrap();

        let json = serde_json::to_string(&script).unwrap();

        assert_eq!(serde_json::from_str::<Script>(&json).unwrap(), script);
        assert_eq!(ScriptDraft::from(script), balance());
    }

    #[rstest]
    #[case::no_level(
        r#"{"source":"","leash":8,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::no_source(
        r#"{"api":1,"leash":8,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::no_leash(
        r#"{"api":1,"source":"","tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::no_tool_guard(
        r#"{"api":1,"source":"","leash":8,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::no_block_guard(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"messages":[],"patterns":[]}"#
    )]
    #[case::a_block_guard_as_text(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"block_guard":"stone","messages":[],"patterns":[]}"#
    )]
    #[case::a_block_that_is_no_identifier(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"block_guard":["Stone"],"messages":[],"patterns":[]}"#
    )]
    #[case::no_messages(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"block_guard":[],"patterns":[]}"#
    )]
    #[case::no_patterns(
        r#"{"api":1,"source":"","leash":8,"tool_guard":2,"block_guard":[],"messages":[]}"#
    )]
    #[case::a_newer_level(
        r#"{"api":2,"source":"","leash":8,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::a_leash_that_is_off(
        r#"{"api":1,"source":"","leash":0,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::a_leash_as_text(
        r#"{"api":1,"source":"","leash":"off","tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    #[case::a_source_as_a_list(
        r#"{"api":1,"source":[],"leash":8,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[]}"#
    )]
    fn refuses_json_that_isnt_a_body(#[case] json: &str) {
        assert!(serde_json::from_str::<Script>(json).is_err(), "{json}");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"api":1,"source":"","leash":8,"tool_guard":2,"block_guard":[],"messages":[],"patterns":[],"notes":"x"}"#;

        assert_eq!(
            serde_json::from_str::<Script>(json).unwrap(),
            Script::try_from(draft_of("")).unwrap()
        );
    }
}
