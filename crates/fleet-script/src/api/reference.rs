//! The script reference's tables, written from the one description of the
//! API (Plan.md P15.28; ADR-0036, D4; ADR-0046).
//!
//! `docs/scripting.md` marks each table that comes from the description:
//!
//! ```text
//! <!-- generated table: keys -->
//! | Member | What it is or does |
//! …
//! <!-- end of generated table -->
//! ```
//!
//! [`with_tables`] writes every marked table anew from [`API`] and
//! [`reason_meaning`], and leaves every other line as it is: `just gen` writes
//! the reference with it, and a test holds the reference equal to what it
//! writes, so neither the reference nor the editor names a member the
//! engine doesn't have.

use core::fmt::Write as _;

use fleet_core::script::Refusal;

use super::describe::{API, MemberKind, Object, reason_meaning};

/// The line that starts a generated table, before its name.
const START: &str = "<!-- generated table: ";
/// The end of that line.
const START_END: &str = " -->";
/// The line that ends a generated table.
const END: &str = "<!-- end of generated table -->";
/// The name of the reasons' table.
const REASONS: &str = "reasons";

/// Why the reference's tables couldn't be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceError {
    /// A marked table names neither an object of the API nor `reasons`.
    #[error(
        "the generated table on line {line} names {name:?}, which the description doesn't have"
    )]
    Unknown {
        /// The marker's line, from 1.
        line: usize,
        /// The name it gives.
        name: String,
    },
    /// A marked table has no end.
    #[error("the generated table on line {line} has no end marker")]
    Unended {
        /// The marker's line, from 1.
        line: usize,
    },
}

/// `text` with every marked table written anew from the description; see
/// the module comment.
///
/// # Errors
/// [`ReferenceError`] for a marker that names no table, or has no end.
pub fn with_tables(text: &str) -> Result<String, ReferenceError> {
    let mut written = String::with_capacity(text.len());
    let mut lines = text.split_inclusive('\n').enumerate();
    while let Some((index, line)) = lines.next() {
        written.push_str(line);
        let Some(name) = line
            .trim_end()
            .strip_prefix(START)
            .and_then(|rest| rest.strip_suffix(START_END))
        else {
            continue;
        };
        let table = table(name).ok_or_else(|| ReferenceError::Unknown {
            line: index.saturating_add(1),
            name: name.to_owned(),
        })?;
        written.push_str(&table);
        // What stood between the markers goes; the end marker stays.
        let end =
            lines
                .find(|(_, line)| line.trim_end() == END)
                .ok_or(ReferenceError::Unended {
                    line: index.saturating_add(1),
                })?;
        written.push_str(end.1);
    }
    Ok(written)
}

/// The table called `name`: an object's members, the hooks, or the
/// reasons.
#[must_use]
pub fn table(name: &str) -> Option<String> {
    if name == REASONS {
        return Some(reasons());
    }
    API.iter().find(|object| object.name == name).map(members)
}

/// An object's members: each written as a script writes it, with its
/// summary.
fn members(object: &Object) -> String {
    let heading = match object.name {
        "hooks" => "| Hook | When it runs |",
        "input" => "| On a key or a button | What it is or does |",
        _ => "| Member | What it is or does |",
    };
    let mut table = format!("{heading}\n|---|---|\n");
    for member in object.members {
        let written = match (object.name, member.kind) {
            (_, MemberKind::Hook) | ("input", _) => member.signature.to_owned(),
            (name, _) => format!("{name}.{}", member.signature),
        };
        // Writing into a String never fails.
        let _ = writeln!(table, "| `{written}` | {} |", cell(member.summary));
    }
    table
}

/// The reasons an action's result can carry, with what each means.
fn reasons() -> String {
    let mut table = "| Reason | Meaning |\n|---|---|\n".to_owned();
    for reason in Refusal::ALL {
        let _ = writeln!(
            table,
            "| `{}` | {} |",
            reason.as_str(),
            cell(reason_meaning(reason))
        );
    }
    table
}

/// `text` as a table cell: a summary's own `|` would end it.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marked_table_is_written_anew_and_the_rest_stays() {
        let text = "Before.\n<!-- generated table: vehicle -->\nold\n<!-- end of generated table -->\nAfter.\n";

        let written = with_tables(text).unwrap();

        assert_eq!(
            written,
            "Before.\n<!-- generated table: vehicle -->\n| Member | What it is or does |\n|---|---|\n| `vehicle.mount(kind)` | Get into the nearest free vehicle of that kind within reach: \"minecart\". |\n| `vehicle.dismount()` | Get out. |\n<!-- end of generated table -->\nAfter.\n"
        );
        assert_eq!(with_tables(&written).unwrap(), written);
    }

    #[test]
    fn hooks_inputs_and_reasons_are_written_as_a_script_reads_them() {
        let hooks = table("hooks").unwrap();
        let input = table("input").unwrap();
        let reasons = table("reasons").unwrap();

        assert!(hooks.contains("| `fn on_join(first)` | The bot is in the world after connecting"));
        assert!(input.contains("| `hold_for(ms)` | Press, and let go by itself"));
        assert!(reasons.starts_with("| Reason | Meaning |\n|---|---|\n| `too_soon` |"));
        assert_eq!(reasons.lines().count(), 2 + Refusal::ALL.len());
    }

    #[test]
    fn a_marker_without_a_table_or_an_end_is_refused_at_its_line() {
        assert_eq!(
            with_tables("x\n<!-- generated table: nope -->\n<!-- end of generated table -->\n"),
            Err(ReferenceError::Unknown {
                line: 2,
                name: "nope".to_owned()
            })
        );
        assert_eq!(
            with_tables("<!-- generated table: keys -->\nold\n"),
            Err(ReferenceError::Unended { line: 1 })
        );
    }

    #[test]
    fn a_text_without_markers_is_left_as_it_is() {
        let text = "# Title\n\n| a | b |\n|---|---|\n";

        assert_eq!(with_tables(text).unwrap(), text);
    }
}
