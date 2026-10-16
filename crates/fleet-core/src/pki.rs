//! The control plane's PKI values, shared by the server and the agent
//! (Plan.md P10.2, P10.3; ADR-0019).
//!
//! - **Every key is Ed25519, made from 32 random bytes.** The server draws
//!   them through [`SecureRandom`](crate::system::SecureRandom), the agent
//!   from the OS. [`ed25519_pkcs8`] wraps them as PKCS#8 v1 (RFC 8410), the
//!   form rcgen loads, so no crypto library ever draws a key from its own
//!   random number generator (security rule 4).
//! - **[`CertFingerprint`]** is the SHA-256 of a certificate's DER, written
//!   as `openssl x509 -noout -fingerprint -sha256` prints it. An enrollment
//!   bundle pins the CA certificate's, and the agent checks its CA file
//!   against it before it sends anything; the server keys each agent
//!   certificate's row by its own (`agent_certs`). Hashing happens outside
//!   core, which only holds the value.

use core::fmt;
use core::str::FromStr;

use secrecy::SecretBox;

/// The length of an Ed25519 seed, the whole private key (RFC 8032).
pub const ED25519_SEED_LEN: usize = 32;

/// The length of an Ed25519 key as PKCS#8 v1: a fixed 16-byte prefix and the seed.
pub const ED25519_PKCS8_LEN: usize = 48;

/// What comes before the seed in every Ed25519 PKCS#8 v1 key (RFC 8410 §7):
/// a `SEQUENCE` of version 0, the algorithm `id-Ed25519` (1.3.101.112) and an
/// `OCTET STRING` holding the seed as an `OCTET STRING`. Every length in it is
/// fixed, so the whole key is this prefix and the seed.
const PKCS8_V1_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// The Ed25519 private key with this `seed`, as PKCS#8 v1 DER (RFC 8410).
///
/// Any 32 bytes are a valid Ed25519 seed, so this can't fail. The bytes are
/// written once into a buffer of exactly their size, which is wiped when the
/// result is dropped.
#[must_use]
pub fn ed25519_pkcs8(seed: &[u8; ED25519_SEED_LEN]) -> SecretBox<Vec<u8>> {
    let mut der = Vec::with_capacity(ED25519_PKCS8_LEN);
    der.extend_from_slice(&PKCS8_V1_PREFIX);
    der.extend_from_slice(seed);
    // Boxing moves only the vector's pointer, never its bytes.
    SecretBox::new(Box::new(der))
}

/// Why text isn't a [`CertFingerprint`]. Neither variant holds the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CertFingerprintError {
    /// The text isn't 95 characters long: 32 pairs of hex digits joined by
    /// colons.
    #[error("a certificate fingerprint has {expected} characters, not {len}", expected = CertFingerprint::TEXT_LEN)]
    WrongLength {
        /// The text's length in characters.
        len: usize,
    },
    /// A character that isn't an uppercase hex digit where one belongs, or
    /// isn't a colon between two pairs.
    #[error(
        "a certificate fingerprint is 32 pairs of uppercase hex digits joined by colons (position {index})"
    )]
    InvalidChar {
        /// The character's position, counted in characters.
        index: usize,
    },
}

/// The SHA-256 of a certificate's DER: the CA certificate's is the pin an
/// enrollment bundle carries (P10.3), an agent certificate's the key of its
/// row in `agent_certs` (P10.2).
///
/// Its text is `openssl x509 -noout -fingerprint -sha256`'s form: 32 pairs of
/// uppercase hex digits joined by colons, such as `17:25:5B:…:3C`. Parsing
/// takes exactly that form and nothing else, so one fingerprint has one text.
/// It isn't secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CertFingerprint([u8; 32]);

impl CertFingerprint {
    /// The length of the text: 32 pairs and 31 colons.
    pub const TEXT_LEN: usize = 95;

    /// The fingerprint of a certificate whose SHA-256 is `digest`.
    #[must_use]
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// The SHA-256 digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl TryFrom<&str> for CertFingerprint {
    type Error = CertFingerprintError;

    fn try_from(text: &str) -> Result<Self, CertFingerprintError> {
        let len = text.chars().count();
        if len != Self::TEXT_LEN {
            return Err(CertFingerprintError::WrongLength { len });
        }
        // Every third character is a colon; the others are uppercase hex.
        if let Some(index) = text.chars().enumerate().position(|(index, c)| {
            if index % 3 == 2 {
                c != ':'
            } else {
                !matches!(c, '0'..='9' | 'A'..='F')
            }
        }) {
            return Err(CertFingerprintError::InvalidChar { index });
        }
        // Only ASCII is left, so each pair and its colon are three bytes.
        let mut digest = [0; 32];
        for (byte, pair) in digest.iter_mut().zip(text.as_bytes().chunks(3)) {
            let (Some(&high), Some(&low)) = (pair.first(), pair.get(1)) else {
                return Err(CertFingerprintError::WrongLength { len });
            };
            *byte = (hex_value(high) << 4) | hex_value(low);
        }
        Ok(Self(digest))
    }
}

/// The value of an uppercase hex digit, which the caller has checked.
const fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'A'..=b'F' => digit - b'A' + 10,
        _ => 0,
    }
}

/// The uppercase hex digit for a value below 16.
fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'A' + (value - 10)),
    }
}

impl FromStr for CertFingerprint {
    type Err = CertFingerprintError;

    fn from_str(text: &str) -> Result<Self, CertFingerprintError> {
        Self::try_from(text)
    }
}

impl fmt::Display for CertFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(":")?;
            }
            write!(f, "{}{}", hex_digit(byte >> 4), hex_digit(byte & 0x0f))?;
        }
        Ok(())
    }
}

impl fmt::Debug for CertFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertFingerprint({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;
    use secrecy::ExposeSecret as _;

    /// RFC 8410 §10.3's example private key, `MC4CAQAwBQYDK2VwBCIEINTu…`,
    /// decoded: the prefix and its seed.
    const RFC_8410_KEY: [u8; 48] = [
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20, 0xd4, 0xee, 0x72, 0xdb, 0xf9, 0x13, 0x58, 0x4a, 0xd5, 0xb6, 0xd8, 0xf1, 0xf7, 0x69,
        0xf8, 0xad, 0x3a, 0xfe, 0x7c, 0x28, 0xcb, 0xf1, 0xd4, 0xfb, 0xe0, 0x97, 0xa8, 0x8f, 0x44,
        0x75, 0x58, 0x42,
    ];

    /// The seed of [`RFC_8410_KEY`].
    fn rfc_8410_seed() -> [u8; 32] {
        let mut seed = [0; 32];
        seed.copy_from_slice(&RFC_8410_KEY[16..]);
        seed
    }

    /// The digest 00, 01, …, 1F.
    fn counting() -> [u8; 32] {
        core::array::from_fn(|i| u8::try_from(i).unwrap())
    }

    const COUNTING_TEXT: &str = "00:01:02:03:04:05:06:07:08:09:0A:0B:0C:0D:0E:0F:10:11:12:13:14:15:16:17:18:19:1A:1B:1C:1D:1E:1F";

    #[test]
    fn the_seed_wrapped_is_rfc_8410_s_example_key() {
        let key = ed25519_pkcs8(&rfc_8410_seed());

        assert_eq!(key.expose_secret().as_slice(), RFC_8410_KEY.as_slice());
    }

    #[test]
    fn the_key_holds_exactly_its_length() {
        let key = ed25519_pkcs8(&[7; 32]);

        assert_eq!(key.expose_secret().len(), ED25519_PKCS8_LEN);
        assert_eq!(key.expose_secret().capacity(), ED25519_PKCS8_LEN);
        assert_eq!(&key.expose_secret()[16..], [7; 32].as_slice());
    }

    #[test]
    fn a_fingerprint_is_written_as_openssl_prints_it() {
        let fingerprint = CertFingerprint::from_digest(counting());

        assert_eq!(fingerprint.to_string(), COUNTING_TEXT);
        assert_eq!(fingerprint.to_string().len(), CertFingerprint::TEXT_LEN);
        assert_eq!(
            format!("{fingerprint:?}"),
            format!("CertFingerprint({COUNTING_TEXT})")
        );
    }

    #[test]
    fn a_fingerprint_is_read_back_from_its_text() {
        let fingerprint: CertFingerprint = COUNTING_TEXT.parse().unwrap();

        assert_eq!(fingerprint.as_bytes(), &counting());
        assert_eq!(fingerprint, CertFingerprint::from_digest(counting()));
    }

    #[rstest]
    #[case::empty("", CertFingerprintError::WrongLength { len: 0 })]
    #[case::one_pair_short(&COUNTING_TEXT[3..], CertFingerprintError::WrongLength { len: 92 })]
    #[case::trailing_colon(&format!("{COUNTING_TEXT}:"), CertFingerprintError::WrongLength { len: 96 })]
    #[case::no_colons("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F", CertFingerprintError::WrongLength { len: 64 })]
    #[case::lowercase(&COUNTING_TEXT.replace('A', "a"), CertFingerprintError::InvalidChar { index: 31 })]
    #[case::not_hex(&COUNTING_TEXT.replacen("00", "0G", 1), CertFingerprintError::InvalidChar { index: 1 })]
    #[case::dash_for_colon(&COUNTING_TEXT.replacen(':', "-", 1), CertFingerprintError::InvalidChar { index: 2 })]
    #[case::colon_for_digit(&COUNTING_TEXT.replacen("01", ":1", 1), CertFingerprintError::InvalidChar { index: 3 })]
    #[case::non_ascii(&COUNTING_TEXT.replacen('0', "Ä", 1), CertFingerprintError::InvalidChar { index: 0 })]
    fn anything_else_is_refused(#[case] text: &str, #[case] error: CertFingerprintError) {
        assert_eq!(CertFingerprint::try_from(text), Err(error));
    }

    #[test]
    fn errors_have_fixed_messages() {
        assert_eq!(
            CertFingerprintError::WrongLength { len: 7 }.to_string(),
            "a certificate fingerprint has 95 characters, not 7"
        );
        assert_eq!(
            CertFingerprintError::InvalidChar { index: 4 }.to_string(),
            "a certificate fingerprint is 32 pairs of uppercase hex digits joined by colons (position 4)"
        );
    }

    proptest! {
        #[test]
        fn every_digest_round_trips_through_its_text(digest in any::<[u8; 32]>()) {
            let fingerprint = CertFingerprint::from_digest(digest);
            prop_assert_eq!(fingerprint.to_string().parse::<CertFingerprint>(), Ok(fingerprint));
        }

        #[test]
        fn parsing_never_panics(text in any::<String>()) {
            let _ = CertFingerprint::try_from(text.as_str());
        }

        #[test]
        fn every_seed_is_wrapped_behind_the_prefix(seed in any::<[u8; 32]>()) {
            let key = ed25519_pkcs8(&seed);
            prop_assert_eq!(&key.expose_secret()[..16], PKCS8_V1_PREFIX.as_slice());
            prop_assert_eq!(&key.expose_secret()[16..], seed.as_slice());
        }
    }
}
