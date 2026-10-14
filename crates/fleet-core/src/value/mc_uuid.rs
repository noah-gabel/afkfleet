//! [`McUuid`]: a Minecraft profile's UUID.

use core::fmt;
use core::str::FromStr;

use uuid::Uuid;

/// Why a UUID or a text isn't a valid [`McUuid`]. Neither variant carries
/// the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McUuidError {
    /// The nil UUID, which no Minecraft profile has.
    #[error("the nil UUID isn't a Minecraft profile")]
    Nil,
    /// The text isn't a UUID in lowercase hyphenated form.
    #[error("a Minecraft profile's UUID is written in lowercase hyphenated form")]
    Malformed,
}

/// A Minecraft profile's UUID: any UUID but the nil one, of any version
/// (Microsoft accounts' profiles are version 4).
///
/// It's written and read only in its lowercase hyphenated form, the form
/// `mc_accounts.mc_uuid` stores, so one profile has exactly one text.
/// Microsoft's profile API answers it without hyphens; turning that into an
/// `McUuid` is the adapter's job. Its own type keeps it apart from the
/// server's record IDs, which are UUIDs too.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct McUuid(Uuid);

impl McUuid {
    /// The UUID itself.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl TryFrom<Uuid> for McUuid {
    type Error = McUuidError;

    fn try_from(uuid: Uuid) -> Result<Self, McUuidError> {
        if uuid.is_nil() {
            return Err(McUuidError::Nil);
        }
        Ok(Self(uuid))
    }
}

impl From<McUuid> for Uuid {
    fn from(uuid: McUuid) -> Self {
        uuid.0
    }
}

impl FromStr for McUuid {
    type Err = McUuidError;

    /// Reads exactly the lowercase hyphenated form: uuid's parser also takes
    /// the simple, braced and urn forms and either case, so the text must
    /// equal the parsed UUID written back.
    fn from_str(text: &str) -> Result<Self, McUuidError> {
        let uuid = Uuid::try_parse(text).map_err(|_| McUuidError::Malformed)?;
        let mut buffer = Uuid::encode_buffer();
        if uuid.hyphenated().encode_lower(&mut buffer) != text {
            return Err(McUuidError::Malformed);
        }
        Self::try_from(uuid)
    }
}

impl fmt::Display for McUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = Uuid::encode_buffer();
        f.write_str(self.0.hyphenated().encode_lower(&mut buffer))
    }
}

impl fmt::Debug for McUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "McUuid({self})")
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::version_4("069a79f4-44e9-4726-a5be-fca90e38aaf5")]
    #[case::version_7("0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b")]
    #[case::max("ffffffff-ffff-ffff-ffff-ffffffffffff")]
    fn reads_and_writes_the_lowercase_hyphenated_form(#[case] text: &str) {
        let uuid: McUuid = text.parse().unwrap();

        assert_eq!(uuid.to_string(), text);
        assert_eq!(format!("{uuid:?}"), format!("McUuid({text})"));
        assert_eq!(Uuid::from(uuid), Uuid::parse_str(text).unwrap());
        assert_eq!(uuid.as_uuid(), &Uuid::parse_str(text).unwrap());
    }

    #[test]
    fn the_nil_uuid_is_refused() {
        assert_eq!(McUuid::try_from(Uuid::nil()), Err(McUuidError::Nil));
        assert_eq!(
            "00000000-0000-0000-0000-000000000000".parse::<McUuid>(),
            Err(McUuidError::Nil)
        );
    }

    #[test]
    fn any_other_uuid_is_accepted() {
        let uuid = Uuid::from_u128(1);

        assert_eq!(McUuid::try_from(uuid).map(Uuid::from), Ok(uuid));
    }

    #[rstest]
    #[case::uppercase("069A79F4-44E9-4726-A5BE-FCA90E38AAF5")]
    #[case::mixed_case("069a79f4-44e9-4726-a5be-fcA90e38aaf5")]
    #[case::simple("069a79f444e94726a5befca90e38aaf5")]
    #[case::braced("{069a79f4-44e9-4726-a5be-fca90e38aaf5}")]
    #[case::urn("urn:uuid:069a79f4-44e9-4726-a5be-fca90e38aaf5")]
    #[case::one_short("069a79f4-44e9-4726-a5be-fca90e38aaf")]
    #[case::one_long("069a79f4-44e9-4726-a5be-fca90e38aaf50")]
    #[case::not_hex("069a79f4-44e9-4726-a5be-fca90e38aazz")]
    #[case::padded(" 069a79f4-44e9-4726-a5be-fca90e38aaf5")]
    #[case::empty("")]
    fn any_other_spelling_is_malformed(#[case] text: &str) {
        assert_eq!(text.parse::<McUuid>(), Err(McUuidError::Malformed));
    }

    proptest! {
        #[test]
        fn parsing_never_panics(text in any::<String>()) {
            let _ = text.parse::<McUuid>();
        }

        #[test]
        fn every_other_uuid_round_trips_through_its_text(bits in 1_u128..) {
            let uuid = McUuid::try_from(Uuid::from_u128(bits)).unwrap();

            prop_assert_eq!(uuid.to_string().parse::<McUuid>(), Ok(uuid));
        }
    }
}
