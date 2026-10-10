//! The rule every name in an audit entry follows: lowercase ASCII letters,
//! digits and `_`, plus `.` between an action's parts.

use super::AuditError;

/// Whether a name may have dots, and must then have two or more parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dots {
    /// An action: `area.event`.
    Required,
    /// A target type or a metadata key: no dots.
    Forbidden,
}

/// Checks `text` against the name rule, with at most `max` characters.
pub(super) fn check(text: &str, max: usize, dots: Dots) -> Result<(), AuditError> {
    let len = text.chars().count();
    if len == 0 || len > max {
        return Err(AuditError::NameLength { len, max });
    }
    let allowed = |c: char| {
        c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || c == '_'
            || (dots == Dots::Required && c == '.')
    };
    if let Some(index) = text.chars().position(|c| !allowed(c)) {
        return Err(AuditError::NameChar { index });
    }
    if dots == Dots::Required && (!text.contains('.') || text.split('.').any(str::is_empty)) {
        return Err(AuditError::ActionShape);
    }
    Ok(())
}
