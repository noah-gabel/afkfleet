//! [`AuditTarget`]: what an audit entry touched, and [`RecordedTarget`], how
//! a stored target reads back.

use uuid::Uuid;

use super::AuditError;
use super::name::{self, Dots};
use crate::id::{AccountId, AgentId, BotId, ModeId, SessionId, UserId};

/// What an entry touched, stored as `target_type` and `target_id`. Each
/// variant holds its typed ID, so the type and the ID can never disagree.
/// Later phases add variants when their ID types arrive (invites in P7's
/// group E).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditTarget {
    /// A user.
    User(UserId),
    /// A linked Minecraft account.
    Account(AccountId),
    /// A bot.
    Bot(BotId),
    /// An agent.
    Agent(AgentId),
    /// A mode.
    Mode(ModeId),
    /// A session: one login's token family (P7, group D).
    Session(SessionId),
}

impl AuditTarget {
    /// Returns the stable name stored in the `target_type` column.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::Account(_) => "account",
            Self::Bot(_) => "bot",
            Self::Agent(_) => "agent",
            Self::Mode(_) => "mode",
            Self::Session(_) => "session",
        }
    }

    /// Returns the ID stored in the `target_id` column.
    #[must_use]
    pub fn id(&self) -> Uuid {
        match self {
            Self::User(id) => *id.as_uuid(),
            Self::Account(id) => *id.as_uuid(),
            Self::Bot(id) => *id.as_uuid(),
            Self::Agent(id) => *id.as_uuid(),
            Self::Mode(id) => *id.as_uuid(),
            Self::Session(id) => *id.as_uuid(),
        }
    }
}

/// A stored target type this version may not know: a name of `a`–`z`,
/// `0`–`9` and `_`, at most [`TargetKind::MAX_LEN`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetKind(String);

impl TargetKind {
    /// The longest name, in characters.
    pub const MAX_LEN: usize = 64;

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for TargetKind {
    type Error = AuditError;

    fn try_from(text: &str) -> Result<Self, AuditError> {
        name::check(text, Self::MAX_LEN, Dots::Forbidden)?;
        Ok(Self(text.to_owned()))
    }
}

/// A stored target as it reads back: a known [`AuditTarget`], or a type only
/// a newer version knows, kept with its ID instead of failing the list. Code
/// can't record an unrecognized target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedTarget {
    /// A target type this version knows.
    Known(AuditTarget),
    /// A target type this version doesn't know.
    Unrecognized {
        /// The stored type name.
        kind: TargetKind,
        /// The stored ID.
        id: Uuid,
    },
}

impl RecordedTarget {
    /// Reads a stored `target_type` and `target_id`.
    ///
    /// # Errors
    /// [`AuditError::TargetId`] when a known type's ID isn't a version 7
    /// UUID, and the name errors when `kind` isn't a valid name.
    pub fn from_parts(kind: &str, id: Uuid) -> Result<Self, AuditError> {
        let known = match kind {
            "user" => UserId::try_from(id).map(AuditTarget::User),
            "account" => AccountId::try_from(id).map(AuditTarget::Account),
            "bot" => BotId::try_from(id).map(AuditTarget::Bot),
            "agent" => AgentId::try_from(id).map(AuditTarget::Agent),
            "mode" => ModeId::try_from(id).map(AuditTarget::Mode),
            "session" => SessionId::try_from(id).map(AuditTarget::Session),
            _ => return TargetKind::try_from(kind).map(|kind| Self::Unrecognized { kind, id }),
        };
        known.map(Self::Known).map_err(|_| AuditError::TargetId)
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use rstest::rstest;

    use super::*;

    fn bytes() -> ([u8; 10], chrono::DateTime<chrono::Utc>) {
        (
            [7; 10],
            DateTime::from_timestamp_millis(1_800_000_000_000).unwrap(),
        )
    }

    fn every_target() -> [AuditTarget; 6] {
        let (random, at) = bytes();
        [
            AuditTarget::User(UserId::new_v7(at, random).unwrap()),
            AuditTarget::Account(AccountId::new_v7(at, random).unwrap()),
            AuditTarget::Bot(BotId::new_v7(at, random).unwrap()),
            AuditTarget::Agent(AgentId::new_v7(at, random).unwrap()),
            AuditTarget::Mode(ModeId::new_v7(at, random).unwrap()),
            AuditTarget::Session(SessionId::new_v7(at, random).unwrap()),
        ]
    }

    #[test]
    fn the_stored_type_names_are_pinned() {
        let kinds = every_target().map(|target| target.kind());

        assert_eq!(
            kinds,
            ["user", "account", "bot", "agent", "mode", "session"]
        );
    }

    #[test]
    fn every_target_reads_back_as_itself() {
        for target in every_target() {
            assert_eq!(
                RecordedTarget::from_parts(target.kind(), target.id()),
                Ok(RecordedTarget::Known(target)),
                "{target:?}"
            );
        }
    }

    #[test]
    fn the_id_is_the_typed_ids_uuid() {
        let (random, at) = bytes();
        let user = UserId::new_v7(at, random).unwrap();

        assert_eq!(AuditTarget::User(user).id(), *user.as_uuid());
    }

    #[test]
    fn an_unknown_type_reads_back_as_unrecognized() {
        let id = Uuid::from_u128(0x018b_cfe5_6800_7bab_abab_abab_abab_abab);

        let target = RecordedTarget::from_parts("nonexistent_kind", id).unwrap();

        assert_eq!(
            target,
            RecordedTarget::Unrecognized {
                kind: TargetKind::try_from("nonexistent_kind").unwrap(),
                id,
            }
        );
    }

    #[test]
    fn a_known_type_with_a_non_v7_id_is_an_error() {
        let v4 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        assert_eq!(
            RecordedTarget::from_parts("user", v4),
            Err(AuditError::TargetId)
        );
    }

    #[rstest]
    #[case::empty("", AuditError::NameLength { len: 0, max: 64 })]
    #[case::dotted("user.session", AuditError::NameChar { index: 4 })]
    #[case::uppercase("User", AuditError::NameChar { index: 0 })]
    fn an_invalid_type_name_is_an_error(#[case] kind: &str, #[case] expected: AuditError) {
        assert_eq!(RecordedTarget::from_parts(kind, Uuid::nil()), Err(expected));
    }
}
