//! Typed IDs for the domain's entities, and [`RequestId`], which names one HTTP
//! request to the server rather than an entity.
//!
//! Every entity has its own ID type, so a [`BotId`] can't be passed where a
//! [`UserId`] is expected:
//!
//! ```compile_fail,E0308
//! use fleet_core::id::{BotId, UserId};
//!
//! fn owner_of(bot: BotId) -> UserId {
//!     bot
//! }
//! ```
//!
//! All IDs wrap a version 7 UUID, which sorts by creation time. `fleet-core`
//! never reads a clock or the OS's randomness, so an ID is minted from data the
//! caller passes in: the creation time and 10 random bytes (the server takes them
//! from the OS's secure random source). Parsing and deserializing accept only
//! version 7 UUIDs, and IDs serialize to the plain hyphenated string
//! (ADR-0010).

use core::fmt;
use core::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Builder, Uuid, Variant, Version};

/// Why a value isn't a valid ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The text isn't a UUID.
    #[error("not a UUID")]
    Malformed,
    /// The UUID isn't a version 7 UUID with the standard variant.
    #[error("not a version 7 UUID")]
    NotV7,
    /// The creation time is before 1970 or beyond the 48-bit millisecond range of
    /// a version 7 UUID (the year 10889).
    #[error("creation time outside the range of a version 7 UUID")]
    TimestampOutOfRange,
}

/// The first millisecond a version 7 UUID can't hold: its timestamp has 48 bits.
const V7_MILLIS_LIMIT: u64 = 1 << 48;

/// Builds a version 7 UUID from its parts.
///
/// uuid's builder silently drops the bits above 48, so out-of-range times are
/// rejected here instead.
fn v7_from_parts(created_at: DateTime<Utc>, random: &[u8; 10]) -> Result<Uuid, IdError> {
    let millis =
        u64::try_from(created_at.timestamp_millis()).map_err(|_| IdError::TimestampOutOfRange)?;
    if millis >= V7_MILLIS_LIMIT {
        return Err(IdError::TimestampOutOfRange);
    }
    Ok(Builder::from_unix_timestamp_millis(millis, random).into_uuid())
}

/// Accepts only version 7 UUIDs with the standard (RFC 9562) variant.
fn check_v7(uuid: Uuid) -> Result<Uuid, IdError> {
    if uuid.get_version() == Some(Version::SortRand) && uuid.get_variant() == Variant::RFC4122 {
        Ok(uuid)
    } else {
        Err(IdError::NotV7)
    }
}

/// Parses the text form of a UUID and checks that it's version 7.
///
/// uuid's own error is dropped on purpose: its message quotes the input.
fn parse_v7(text: &str) -> Result<Uuid, IdError> {
    Uuid::try_parse(text)
        .map_err(|_| IdError::Malformed)
        .and_then(check_v7)
}

/// An ID type that is minted from a creation time and 10 random bytes, so
/// [`crate::system::mint`] can make any of them from the server's ports
/// (ADR-0015). Every ID type here implements it.
pub trait V7Id: Sized {
    /// Mints an ID from its creation time and 10 random bytes.
    ///
    /// # Errors
    /// [`IdError::TimestampOutOfRange`] if `created_at` is before 1970 or
    /// beyond the range of a version 7 UUID.
    fn new_v7(created_at: DateTime<Utc>, random: [u8; 10]) -> Result<Self, IdError>;
}

/// Defines one ID newtype with its constructors, conversions and formatting.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "Uuid", into = "Uuid")]
        pub struct $name(Uuid);

        impl $name {
            /// Mints an ID from its creation time and 10 random bytes.
            ///
            /// The caller supplies the randomness, so this stays pure; the
            /// server takes the bytes from the OS's secure random source.
            ///
            /// # Errors
            /// [`IdError::TimestampOutOfRange`] if `created_at` is before 1970 or
            /// beyond the range of a version 7 UUID.
            pub fn new_v7(created_at: DateTime<Utc>, random: [u8; 10]) -> Result<Self, IdError> {
                v7_from_parts(created_at, &random).map(Self)
            }

            /// Returns the wrapped UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl V7Id for $name {
            fn new_v7(created_at: DateTime<Utc>, random: [u8; 10]) -> Result<Self, IdError> {
                Self::new_v7(created_at, random)
            }
        }

        impl TryFrom<Uuid> for $name {
            type Error = IdError;

            fn try_from(uuid: Uuid) -> Result<Self, IdError> {
                check_v7(uuid).map(Self)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(text: &str) -> Result<Self, IdError> {
                parse_v7(text).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0.hyphenated())
            }
        }
    };
}

define_id!(
    /// Identifies a user of the app.
    UserId
);
define_id!(
    /// Identifies a linked Minecraft account.
    AccountId
);
define_id!(
    /// Identifies a bot (one per Minecraft account).
    BotId
);
define_id!(
    /// Identifies an agent that runs bots.
    AgentId
);
define_id!(
    /// Identifies a mode (built-in or custom).
    ModeId
);
define_id!(
    /// Identifies one HTTP request to the server: minted by its request-ID
    /// layer for every request, sent back in `x-request-id` and in every error
    /// body, and logged with the request. It names no entity.
    RequestId
);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    const FIXED_MILLIS: i64 = 1_700_000_000_000;
    const FIXED_RANDOM: [u8; 10] = [0xAB; 10];
    /// `FIXED_MILLIS` and `FIXED_RANDOM` as a v7 UUID, worked out by hand from
    /// RFC 9562: 48 bits of time, version 7, 12 random bits, variant `10`, 62
    /// random bits.
    const FIXED_TEXT: &str = "018bcfe5-6800-7bab-abab-abababababab";
    /// A valid version 4 UUID.
    const V4_TEXT: &str = "550e8400-e29b-41d4-a716-446655440000";
    const MAX_V7_MILLIS: i64 = (1 << 48) - 1;

    fn at(millis: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(millis).unwrap()
    }

    fn fixed_bot_id() -> BotId {
        BotId::new_v7(at(FIXED_MILLIS), FIXED_RANDOM).unwrap()
    }

    #[test]
    fn display_is_lowercase_hyphenated() {
        assert_eq!(fixed_bot_id().to_string(), FIXED_TEXT);
    }

    #[test]
    fn debug_names_the_type() {
        assert_eq!(
            format!("{:?}", fixed_bot_id()),
            format!("BotId({FIXED_TEXT})")
        );
    }

    #[test]
    fn new_v7_encodes_millis() {
        let id = fixed_bot_id();

        let (secs, nanos) = id.as_uuid().get_timestamp().unwrap().to_unix();
        let millis = secs * 1000 + u64::from(nanos / 1_000_000);
        assert_eq!(millis, u64::try_from(FIXED_MILLIS).unwrap());
        assert_eq!(id.as_uuid().get_version(), Some(uuid::Version::SortRand));
    }

    #[rstest]
    #[case::before_the_epoch(-1)]
    #[case::beyond_48_bits(1 << 48)]
    fn new_v7_rejects_out_of_range_timestamps(#[case] millis: i64) {
        assert_eq!(
            BotId::new_v7(at(millis), FIXED_RANDOM),
            Err(IdError::TimestampOutOfRange)
        );
    }

    #[rstest]
    #[case::the_epoch(0)]
    #[case::the_last_48_bit_millisecond(MAX_V7_MILLIS)]
    fn new_v7_accepts_the_range_limits(#[case] millis: i64) {
        assert!(BotId::new_v7(at(millis), FIXED_RANDOM).is_ok());
    }

    #[test]
    fn parse_accepts_the_display_form() {
        assert_eq!(FIXED_TEXT.parse::<BotId>(), Ok(fixed_bot_id()));
    }

    #[rstest]
    #[case::empty("")]
    #[case::word("not-a-uuid")]
    #[case::too_short("018bcfe5-6800-7bab-abab-ababababab")]
    #[case::too_long("018bcfe5-6800-7bab-abab-abababababab0")]
    #[case::non_hex("018bcfe5-6800-7bab-abab-abababababag")]
    #[case::control_char("018bcfe5-6800-7bab-abab-ababababab\n")]
    fn parse_rejects_malformed(#[case] text: &str) {
        assert_eq!(text.parse::<BotId>(), Err(IdError::Malformed));
    }

    #[rstest]
    #[case::nil("00000000-0000-0000-0000-000000000000")]
    #[case::max("ffffffff-ffff-ffff-ffff-ffffffffffff")]
    #[case::v4(V4_TEXT)]
    #[case::v7_with_a_non_standard_variant("018bcfe5-6800-7bab-2bab-abababababab")]
    fn parse_rejects_anything_but_v7(#[case] text: &str) {
        assert_eq!(text.parse::<BotId>(), Err(IdError::NotV7));
    }

    #[test]
    fn try_from_uuid_rejects_v4() {
        let v4 = Uuid::parse_str(V4_TEXT).unwrap();
        assert_eq!(BotId::try_from(v4), Err(IdError::NotV7));
    }

    #[test]
    fn converts_back_into_the_uuid() {
        let id = fixed_bot_id();
        assert_eq!(Uuid::from(id), *id.as_uuid());
    }

    #[test]
    fn deserialize_accepts_v7() {
        let json = format!("\"{FIXED_TEXT}\"");
        assert_eq!(
            serde_json::from_str::<BotId>(&json).unwrap(),
            fixed_bot_id()
        );
    }

    #[test]
    fn deserialize_rejects_non_v7() {
        let json = format!("\"{V4_TEXT}\"");
        assert!(serde_json::from_str::<BotId>(&json).is_err());
    }

    /// Mints, prints, parses and serializes one ID type.
    fn assert_round_trips<T>(mint: fn(DateTime<Utc>, [u8; 10]) -> Result<T, IdError>)
    where
        T: Copy + FromStr<Err = IdError> + fmt::Display + fmt::Debug + PartialEq + Serialize,
        T: for<'de> Deserialize<'de>,
    {
        let id = mint(at(FIXED_MILLIS), FIXED_RANDOM).unwrap();
        assert_eq!(id.to_string().parse::<T>(), Ok(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{FIXED_TEXT}\""));
        assert_eq!(serde_json::from_str::<T>(&json).unwrap(), id);
    }

    /// Mints one ID type through [`V7Id`] and through its own constructor.
    fn assert_trait_mints_like_the_constructor<T>(
        constructor: fn(DateTime<Utc>, [u8; 10]) -> Result<T, IdError>,
    ) where
        T: V7Id + fmt::Debug + PartialEq,
    {
        assert_eq!(
            <T as V7Id>::new_v7(at(FIXED_MILLIS), FIXED_RANDOM),
            constructor(at(FIXED_MILLIS), FIXED_RANDOM)
        );
        assert_eq!(
            <T as V7Id>::new_v7(at(-1), FIXED_RANDOM),
            Err(IdError::TimestampOutOfRange)
        );
    }

    #[test]
    fn every_id_type_mints_through_the_trait() {
        assert_trait_mints_like_the_constructor(UserId::new_v7);
        assert_trait_mints_like_the_constructor(AccountId::new_v7);
        assert_trait_mints_like_the_constructor(BotId::new_v7);
        assert_trait_mints_like_the_constructor(AgentId::new_v7);
        assert_trait_mints_like_the_constructor(ModeId::new_v7);
        assert_trait_mints_like_the_constructor(RequestId::new_v7);
    }

    #[test]
    fn every_id_type_round_trips() {
        assert_round_trips(UserId::new_v7);
        assert_round_trips(AccountId::new_v7);
        assert_round_trips(BotId::new_v7);
        assert_round_trips(AgentId::new_v7);
        assert_round_trips(ModeId::new_v7);
        assert_round_trips(RequestId::new_v7);
    }

    #[test]
    fn serializes_as_plain_uuid_strings() {
        #[derive(Serialize)]
        struct AllIds {
            user: UserId,
            account: AccountId,
            bot: BotId,
            agent: AgentId,
            mode: ModeId,
        }
        let t = at(FIXED_MILLIS);
        let ids = AllIds {
            user: UserId::new_v7(t, [0x01; 10]).unwrap(),
            account: AccountId::new_v7(t, [0x02; 10]).unwrap(),
            bot: BotId::new_v7(t, [0x03; 10]).unwrap(),
            agent: AgentId::new_v7(t, [0x04; 10]).unwrap(),
            mode: ModeId::new_v7(t, [0x05; 10]).unwrap(),
        };

        insta::assert_json_snapshot!("serialized_ids", ids);
    }

    proptest! {
        #[test]
        fn parse_round_trips_display(millis in 0..=MAX_V7_MILLIS, random in any::<[u8; 10]>()) {
            let id = BotId::new_v7(at(millis), random).unwrap();
            prop_assert_eq!(id.to_string().parse::<BotId>(), Ok(id));
        }

        #[test]
        fn parse_never_panics(text in any::<String>()) {
            let _ = text.parse::<BotId>();
        }

        #[test]
        fn ids_sort_by_creation_time(
            earlier in 0..MAX_V7_MILLIS,
            gap in 1..=1_000_000_i64,
            random_a in any::<[u8; 10]>(),
            random_b in any::<[u8; 10]>(),
        ) {
            let later = (earlier + gap).min(MAX_V7_MILLIS);
            prop_assume!(later > earlier);
            let first = BotId::new_v7(at(earlier), random_a).unwrap();
            let second = BotId::new_v7(at(later), random_b).unwrap();
            prop_assert!(first < second);
        }
    }
}
