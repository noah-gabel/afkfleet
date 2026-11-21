//! The examples: a complete script for each kind of mode the reference
//! shows (ADR-0036, D4; ADR-0037).
//!
//! Each lives once, as a file in `scripts/`. This crate's tests compile
//! each and run it against a fake world and hold the reference's text
//! equal to the file; the editor offers them as starter templates.

use fleet_core::script::{
    CaptureDraft, CaptureKind, DecimalMark, LeashRadius, MessageDraft, PatternDraft,
    ScriptApiLevel, ScriptDraft, ToolGuard, ValueDraft, ValueKind, When,
};

/// A complete scripted mode to start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Example {
    /// A short name for it, as a mode's key would be: `afk_farm`.
    pub id: &'static str,
    /// Its heading in the reference.
    pub title: &'static str,
    /// The script.
    pub source: &'static str,
    declarations: Declarations,
    /// The blocks the example's mode lists for mining.
    block_guard: &'static [&'static str],
}

/// What an example's mode declares beside its script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declarations {
    /// Nothing.
    None,
    /// The balance example's two messages and its reply pattern.
    Balance,
}

impl Example {
    /// The example as a scripted mode's body, as written: the script with
    /// the default leash and tool guard, the blocks it mines, and what the
    /// example declares.
    #[must_use]
    pub fn draft(&self) -> ScriptDraft {
        let (messages, patterns) = match self.declarations {
            Declarations::None => (Vec::new(), Vec::new()),
            Declarations::Balance => balance(),
        };
        ScriptDraft {
            api: ScriptApiLevel::V1.get(),
            source: self.source.to_owned(),
            leash: LeashRadius::DEFAULT.blocks(),
            tool_guard: ToolGuard::DEFAULT.uses_left(),
            block_guard: self
                .block_guard
                .iter()
                .map(|block| (*block).to_owned())
                .collect(),
            messages,
            patterns,
        }
    }
}

/// The balance example's declarations: `/balance`, `/pay AfkMain {amount}`
/// and the reply `Balance: ${amount}`. `AfkMain` is a placeholder for the
/// author's own main account.
fn balance() -> (Vec<MessageDraft>, Vec<PatternDraft>) {
    let messages = vec![
        MessageDraft {
            key: "balance".to_owned(),
            text: "/balance".to_owned(),
            values: Vec::new(),
        },
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
    ];
    let patterns = vec![PatternDraft {
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
    }];
    (messages, patterns)
}

/// Every example, in the reference's order.
pub const EXAMPLES: [Example; 4] = [
    Example {
        id: "afk_farm",
        title: "An AFK farm",
        source: include_str!("../scripts/afk_farm.rhai"),
        declarations: Declarations::None,
        block_guard: &[],
    },
    Example {
        id: "minecart_farm",
        title: "The minecart farm",
        source: include_str!("../scripts/minecart_farm.rhai"),
        declarations: Declarations::None,
        block_guard: &[],
    },
    Example {
        id: "cobblestone_generator",
        title: "A cobblestone generator",
        source: include_str!("../scripts/cobblestone_generator.rhai"),
        declarations: Declarations::None,
        // The wall behind the generator's block is never mined.
        block_guard: &["cobblestone"],
    },
    Example {
        id: "balance",
        title: "Paying out a balance",
        source: include_str!("../scripts/balance.rhai"),
        declarations: Declarations::Balance,
        block_guard: &[],
    },
];
