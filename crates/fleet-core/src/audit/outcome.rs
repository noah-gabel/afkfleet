//! [`AuditOutcome`]: how an audited attempt ended.

use core::fmt;
use core::str::FromStr;

use super::AuditError;

/// How an audited attempt ended, stored in `audit_log.outcome`. A closed set:
/// the column's CHECK lists exactly these names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditOutcome {
    /// It happened.
    Success,
    /// It was refused on its merits: a wrong password, an expired invite, a
    /// reused refresh token.
    Failure,
    /// `authorize()` refused it (a 403 or 404).
    Denied,
}

impl AuditOutcome {
    /// Every outcome, for the tests that keep Rust and the SQL CHECK in sync.
    pub const ALL: [Self; 3] = [Self::Success, Self::Failure, Self::Denied];

    /// Returns the stable name stored in the `outcome` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
        }
    }
}

impl FromStr for AuditOutcome {
    type Err = AuditError;

    fn from_str(name: &str) -> Result<Self, AuditError> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == name)
            .ok_or(AuditError::UnknownOutcome)
    }
}

impl fmt::Display for AuditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::success(AuditOutcome::Success, "success")]
    #[case::failure(AuditOutcome::Failure, "failure")]
    #[case::denied(AuditOutcome::Denied, "denied")]
    fn the_stored_names_are_pinned(#[case] outcome: AuditOutcome, #[case] name: &str) {
        assert_eq!(outcome.as_str(), name);
        assert_eq!(outcome.to_string(), name);
        assert_eq!(name.parse::<AuditOutcome>(), Ok(outcome));
    }

    #[rstest]
    #[case::empty("")]
    #[case::uppercase("Success")]
    #[case::other("error")]
    fn an_unknown_name_is_refused(#[case] name: &str) {
        assert_eq!(
            name.parse::<AuditOutcome>(),
            Err(AuditError::UnknownOutcome)
        );
    }
}
