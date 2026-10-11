//! [`XChaChaVault`]: the vault, and [`VaultKeys`], its keys as loaded from
//! their files (Plan.md §7.5, P7.4, ADR-0016).
//!
//! # The envelope, version 1
//! `0x01 | key_id (u32, big-endian) | nonce (24 bytes) | ciphertext + tag (16
//! bytes)`, stored as a BLOB. XChaCha20-Poly1305 with a nonce from the
//! [`SecureRandom`] port; the key ID names the key that sealed it, so old
//! keys keep opening their envelopes until `vault rotate-key` (P9) has
//! re-encrypted everything.
//!
//! The AAD binds the header, the record and the purpose: `0x01 ‖ key_id (4) ‖
//! record UUID (16 bytes) ‖ purpose name`. The fixed widths come first and
//! the purpose last, so no two inputs share an AAD, and changing any header
//! byte fails authentication. A committed golden envelope pins the format:
//! a real change means version 2, never a changed version 1.
//!
//! # Key files
//! Each holds one 32-byte key as standard base64 with padding (44
//! characters, what `openssl rand -base64 32` prints), read through
//! fleet-startup's secret-file loader. The decoded key is wiped wherever it
//! was copied, and two key IDs with the same key are refused: a "rotation"
//! to an old key would leave a possibly leaked key in use.

use core::fmt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD;
use base64::{DecodeSliceError, Engine as _};
use chacha20poly1305::aead::{Aead as _, Payload};
use chacha20poly1305::{KeyInit as _, XChaCha20Poly1305, XNonce};
use fleet_core::system::{SecureRandom, random_bytes};
use fleet_startup::secret_file::{self, SecretFileError};
use secrecy::{ExposeSecret as _, ExposeSecretMut as _, SecretBox};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::config::VaultConfig;
use crate::ports::vault::{Vault, VaultError, VaultPurpose};

/// The envelope's version byte.
pub const VERSION_1: u8 = 0x01;
/// What an envelope adds to its plaintext: the version, the key ID, the
/// nonce and the tag.
pub const OVERHEAD: usize = HEADER_BYTES + NONCE_BYTES + 16;

/// A key's length: one XChaCha20-Poly1305 key.
const KEY_BYTES: usize = 32;
/// The version byte and the key ID.
const HEADER_BYTES: usize = 1 + 4;
/// `XChaCha20`'s extended nonce.
const NONCE_BYTES: usize = 24;

/// Why the vault's keys couldn't be loaded. No message shows any part of a
/// key file.
#[derive(Debug, thiserror::Error)]
pub enum VaultKeyError {
    /// The file can't be used (fleet-startup's loader). The source names the
    /// file and the reason.
    #[error("vault key {key_id}")]
    File {
        /// The key ID it's configured for.
        key_id: u32,
        /// The loader's error, naming the file.
        #[source]
        source: SecretFileError,
    },
    /// The file starts with a byte-order mark, which Windows editors and
    /// Windows PowerShell 5.1's `>` add.
    #[error(
        "vault key {key_id}: the file {} starts with a byte-order mark; save it as plain text without BOM",
        path.display()
    )]
    ByteOrderMark {
        /// The key ID it's configured for.
        key_id: u32,
        /// The file.
        path: PathBuf,
    },
    /// The file isn't standard base64 with padding.
    #[error("vault key {key_id}: the file {} isn't standard base64 with padding", path.display())]
    NotBase64 {
        /// The key ID it's configured for.
        key_id: u32,
        /// The file.
        path: PathBuf,
    },
    /// The file doesn't decode to exactly 32 bytes.
    #[error("vault key {key_id}: the file {} doesn't hold exactly 32 bytes", path.display())]
    WrongLength {
        /// The key ID it's configured for.
        key_id: u32,
        /// The file.
        path: PathBuf,
    },
    /// Two key IDs hold the same key.
    #[error("vault keys {first} and {second} are the same key; every key ID needs its own")]
    Duplicate {
        /// The lower key ID.
        first: u32,
        /// The higher key ID.
        second: u32,
    },
    /// The active key ID has no file.
    #[error("the active vault key {key_id} has no key file")]
    ActiveMissing {
        /// The active key ID.
        key_id: u32,
    },
}

/// The vault's keys, loaded and checked: each a 32-byte key in a
/// [`SecretBox`], and the active one's ID.
pub struct VaultKeys {
    keys: BTreeMap<u32, SecretBox<[u8; KEY_BYTES]>>,
    active: u32,
}

impl fmt::Debug for VaultKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultKeys")
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .field("active", &self.active)
            .finish()
    }
}

impl VaultKeys {
    /// Reads and checks every key file `config` lists. `serve` calls it
    /// before its runtime starts, so a bad key stops startup.
    ///
    /// # Errors
    /// [`VaultKeyError`] for the first file that can't be used, a key that
    /// appears twice, or an active key without a file.
    pub fn load(config: &VaultConfig) -> Result<Self, VaultKeyError> {
        let mut keys = BTreeMap::new();
        for (&key_id, path) in &config.key_files {
            let text =
                secret_file::read(path).map_err(|source| VaultKeyError::File { key_id, source })?;
            keys.insert(key_id, decode(key_id, path, text.expose_secret())?);
        }
        if !keys.contains_key(&config.active_key_id) {
            return Err(VaultKeyError::ActiveMissing {
                key_id: config.active_key_id,
            });
        }
        let entries: Vec<_> = keys.iter().collect();
        for (index, (first, key)) in entries.iter().enumerate() {
            for (second, other) in entries.iter().skip(index.saturating_add(1)) {
                if bool::from(key.expose_secret().ct_eq(other.expose_secret())) {
                    return Err(VaultKeyError::Duplicate {
                        first: **first,
                        second: **second,
                    });
                }
            }
        }
        Ok(Self {
            keys,
            active: config.active_key_id,
        })
    }

    /// The active key's ID.
    #[must_use]
    pub const fn active(&self) -> u32 {
        self.active
    }
}

/// The byte-order marks an editor may put in front: UTF-8's, and UTF-16's in
/// either byte order.
const BYTE_ORDER_MARKS: [&[u8]; 3] = [&[0xEF, 0xBB, 0xBF], &[0xFF, 0xFE], &[0xFE, 0xFF]];

/// Decodes a key file's `text` straight into the key's [`SecretBox`], so the
/// key is never copied anywhere else; on an error the box is dropped, and
/// wiped.
///
/// The characters come first: a byte-order mark gets its own error, since
/// it's invisible, and any byte outside the standard alphabet and `=` is
/// "not base64", whatever the length. base64 itself would check the length
/// first and call a too-long text with a stray space a wrong length. Then
/// `decode_slice` decides the padding, the trailing bits and the length: a
/// key that would need more than 32 bytes comes back as
/// `OutputSliceTooSmall`, a wrong length like a shorter one.
fn decode(
    key_id: u32,
    path: &Path,
    text: &[u8],
) -> Result<SecretBox<[u8; KEY_BYTES]>, VaultKeyError> {
    if BYTE_ORDER_MARKS.iter().any(|mark| text.starts_with(mark)) {
        return Err(VaultKeyError::ByteOrderMark {
            key_id,
            path: path.to_owned(),
        });
    }
    let standard = |byte: &u8| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=');
    if !text.iter().all(standard) {
        return Err(VaultKeyError::NotBase64 {
            key_id,
            path: path.to_owned(),
        });
    }
    let mut key = SecretBox::new(Box::new([0_u8; KEY_BYTES]));
    match STANDARD.decode_slice(text, key.expose_secret_mut()) {
        Ok(KEY_BYTES) => Ok(key),
        Ok(_) | Err(DecodeSliceError::OutputSliceTooSmall) => Err(VaultKeyError::WrongLength {
            key_id,
            path: path.to_owned(),
        }),
        Err(DecodeSliceError::DecodeError(_)) => Err(VaultKeyError::NotBase64 {
            key_id,
            path: path.to_owned(),
        }),
    }
}

/// The vault: XChaCha20-Poly1305 with every configured key, sealing with the
/// active one.
pub struct XChaChaVault {
    ciphers: BTreeMap<u32, XChaCha20Poly1305>,
    active: u32,
    random: Arc<dyn SecureRandom>,
}

impl fmt::Debug for XChaChaVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XChaChaVault")
            .field("key_ids", &self.ciphers.keys().collect::<Vec<_>>())
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl XChaChaVault {
    /// The vault over `keys`, drawing nonces from `random`. Each key goes
    /// into its cipher, which wipes it when the vault is dropped, and the
    /// loaded keys are dropped (and wiped) here.
    #[must_use]
    pub fn new(keys: VaultKeys, random: Arc<dyn SecureRandom>) -> Self {
        let ciphers = keys
            .keys
            .into_iter()
            .map(|(key_id, key)| (key_id, XChaCha20Poly1305::new(key.expose_secret().into())))
            .collect();
        Self {
            ciphers,
            active: keys.active,
            random,
        }
    }
}

/// The AAD: the header, the record and the purpose, fixed widths first.
fn aad(key_id: u32, record: Uuid, purpose: VaultPurpose) -> Vec<u8> {
    let name = purpose.name().as_bytes();
    let mut aad = Vec::with_capacity(HEADER_BYTES + 16 + name.len());
    aad.push(VERSION_1);
    aad.extend_from_slice(&key_id.to_be_bytes());
    aad.extend_from_slice(record.as_bytes());
    aad.extend_from_slice(name);
    aad
}

impl Vault for XChaChaVault {
    fn seal(
        &self,
        purpose: VaultPurpose,
        record: Uuid,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, VaultError> {
        let cipher = self
            .ciphers
            .get(&self.active)
            .ok_or(VaultError::UnknownKey {
                key_id: self.active,
            })?;
        let nonce = random_bytes::<NONCE_BYTES>(self.random.as_ref())?;
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad(self.active, record, purpose),
                },
            )
            .map_err(|_| VaultError::TooLarge)?;
        let mut envelope = Vec::with_capacity(OVERHEAD + plaintext.len());
        envelope.push(VERSION_1);
        envelope.extend_from_slice(&self.active.to_be_bytes());
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&sealed);
        Ok(envelope)
    }

    fn open(
        &self,
        purpose: VaultPurpose,
        record: Uuid,
        envelope: &[u8],
    ) -> Result<SecretBox<Vec<u8>>, VaultError> {
        if envelope.len() < OVERHEAD {
            return Err(VaultError::Malformed);
        }
        let (header, rest) = envelope
            .split_at_checked(HEADER_BYTES)
            .ok_or(VaultError::Malformed)?;
        let (nonce, sealed) = rest
            .split_at_checked(NONCE_BYTES)
            .ok_or(VaultError::Malformed)?;
        let (&version, key_id) = header.split_first().ok_or(VaultError::Malformed)?;
        if version != VERSION_1 {
            return Err(VaultError::Malformed);
        }
        let key_id = u32::from_be_bytes(key_id.try_into().map_err(|_| VaultError::Malformed)?);
        let nonce: [u8; NONCE_BYTES] = nonce.try_into().map_err(|_| VaultError::Malformed)?;
        let cipher = self
            .ciphers
            .get(&key_id)
            .ok_or(VaultError::UnknownKey { key_id })?;
        let plaintext = cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: &aad(key_id, record, purpose),
                },
            )
            .map_err(|_| VaultError::Rejected)?;
        Ok(SecretBox::new(Box::new(plaintext)))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE};
    use fleet_core::system::RandomError;
    use fleet_testkit::system::SeededRandom;
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    /// The golden version 1 envelope: `seal` of [`GOLDEN_PLAINTEXT`] for
    /// [`golden_record`] as a TOTP secret, under key 7 = `[0x42; 32]`. Made
    /// once and never regenerated: if this test fails, the format changed,
    /// and a change means version 2 with its own golden envelope.
    const GOLDEN_ENVELOPE: &str = "01000000076cbab308ea9afd64d0e9f94c39cf751a6f2bfacb8beb10b074eb268b4eba20573f2fdc295fc9651d9e727dce74760b04818bddff79aeadd8e80a";
    const GOLDEN_KEY_ID: u32 = 7;
    const GOLDEN_KEY: [u8; 32] = [0x42; 32];
    const GOLDEN_PLAINTEXT: &[u8] = b"golden totp secret";

    fn golden_record() -> Uuid {
        "0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b".parse().unwrap()
    }

    /// `bytes` encoded by `engine`; base64 is built without `alloc`, so
    /// through a fixed buffer.
    fn encode(engine: &impl Engine, bytes: &[u8]) -> String {
        let mut buffer = [0_u8; 128];
        let len = engine.encode_slice(bytes, &mut buffer).unwrap();
        String::from_utf8(buffer[..len].to_vec()).unwrap()
    }

    fn record(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    /// Writes key files into a fresh folder and loads them.
    struct Keys {
        dir: TempDir,
        files: BTreeMap<u32, PathBuf>,
    }

    impl Keys {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                files: BTreeMap::new(),
            }
        }

        /// Adds key `id` holding `text` exactly.
        fn text(self, id: u32, text: &str) -> Self {
            self.bytes(id, text.as_bytes())
        }

        /// Adds key `id` holding `bytes` exactly.
        fn bytes(mut self, id: u32, bytes: &[u8]) -> Self {
            let path = self.dir.path().join(format!("vault_key_{id}"));
            fs::write(&path, bytes).unwrap();
            owner_only(&path);
            self.files.insert(id, path);
            self
        }

        /// Adds key `id` holding `key`, as the docs say: standard base64
        /// with padding and a line break.
        fn key(self, id: u32, key: &[u8]) -> Self {
            let text = format!("{}\n", encode(&STANDARD, key));
            self.text(id, &text)
        }

        fn config(&self, active: u32) -> VaultConfig {
            VaultConfig {
                key_files: self.files.clone(),
                active_key_id: active,
            }
        }

        fn load(&self, active: u32) -> Result<VaultKeys, VaultKeyError> {
            VaultKeys::load(&self.config(active))
        }
    }

    #[cfg(unix)]
    fn owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn owner_only(_path: &Path) {}

    fn vault(keys: &Keys, active: u32, random: &SeededRandom) -> XChaChaVault {
        XChaChaVault::new(keys.load(active).unwrap(), Arc::new(random.clone()))
    }

    fn golden_vault() -> XChaChaVault {
        vault(
            &Keys::new().key(GOLDEN_KEY_ID, &GOLDEN_KEY),
            GOLDEN_KEY_ID,
            &SeededRandom::new(1),
        )
    }

    // --- The format ------------------------------------------------------------

    #[test]
    fn the_golden_version_1_envelope_still_opens() {
        let opened = golden_vault()
            .open(
                VaultPurpose::TotpSecret,
                golden_record(),
                &hex(GOLDEN_ENVELOPE),
            )
            .unwrap();

        assert_eq!(opened.expose_secret().as_slice(), GOLDEN_PLAINTEXT);
    }

    #[test]
    fn an_envelope_is_the_version_the_key_id_a_nonce_and_the_ciphertext() {
        let vault = golden_vault();

        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"twenty bytes secret!")
            .unwrap();

        assert_eq!(envelope.first(), Some(&VERSION_1));
        assert_eq!(
            envelope.get(1..5),
            Some(GOLDEN_KEY_ID.to_be_bytes().as_slice())
        );
        assert_eq!(envelope.len(), OVERHEAD + 20);
        assert_eq!(OVERHEAD, 45);
    }

    #[test]
    fn two_seals_of_the_same_input_differ() {
        let vault = golden_vault();

        let first = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"same")
            .unwrap();
        let second = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"same")
            .unwrap();

        assert_ne!(first, second);
    }

    /// Each purpose's name, written out. A match without a wildcard: a new
    /// purpose fails to compile here until its name is pinned.
    const fn pinned_name(purpose: VaultPurpose) -> &'static str {
        match purpose {
            VaultPurpose::TotpSecret => "totp_secret",
        }
    }

    #[rstest]
    #[case::totp_secret(VaultPurpose::TotpSecret)]
    fn every_purpose_keeps_its_name(#[case] purpose: VaultPurpose) {
        assert_eq!(purpose.name(), pinned_name(purpose));
    }

    // --- Sealing and opening ------------------------------------------------------

    #[rstest]
    #[case::empty(b"".as_slice())]
    #[case::totp_secret(b"twenty bytes secret!")]
    #[case::large(&[7; 4096])]
    fn what_is_sealed_opens_again(#[case] plaintext: &[u8]) {
        let vault = golden_vault();

        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), plaintext)
            .unwrap();
        let opened = vault
            .open(VaultPurpose::TotpSecret, record(1), &envelope)
            .unwrap();

        assert_eq!(opened.expose_secret().as_slice(), plaintext);
    }

    #[test]
    fn the_plaintext_never_appears_in_the_envelope() {
        let vault = golden_vault();

        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"plaintext-marker")
            .unwrap();

        assert!(
            !envelope
                .windows(b"plaintext-marker".len())
                .any(|window| window == b"plaintext-marker")
        );
    }

    #[test]
    fn every_changed_byte_is_refused() {
        let vault = golden_vault();
        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"twenty bytes secret!")
            .unwrap();
        assert_eq!(envelope.len(), OVERHEAD + 20, "every byte is tried");

        for at in 0..envelope.len() {
            let mut changed = envelope.clone();
            changed[at] ^= 0x01;

            let result = vault.open(VaultPurpose::TotpSecret, record(1), &changed);

            match at {
                0 => assert!(matches!(result, Err(VaultError::Malformed)), "byte {at}"),
                1..5 => assert!(
                    matches!(result, Err(VaultError::UnknownKey { .. })),
                    "byte {at}"
                ),
                _ => assert!(matches!(result, Err(VaultError::Rejected)), "byte {at}"),
            }
        }
    }

    #[test]
    fn a_wrong_key_with_the_same_id_is_rejected() {
        let sealer = golden_vault();
        let other = vault(
            &Keys::new().key(GOLDEN_KEY_ID, &[0x43; 32]),
            GOLDEN_KEY_ID,
            &SeededRandom::new(1),
        );
        let envelope = sealer
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        let result = other.open(VaultPurpose::TotpSecret, record(1), &envelope);

        assert!(matches!(result, Err(VaultError::Rejected)), "{result:?}");
    }

    #[test]
    fn another_record_is_rejected() {
        let vault = golden_vault();
        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        let result = vault.open(VaultPurpose::TotpSecret, record(2), &envelope);

        assert!(matches!(result, Err(VaultError::Rejected)), "{result:?}");
    }

    #[test]
    fn a_key_id_no_file_holds_is_unknown() {
        let sealer = vault(&Keys::new().key(2, &[2; 32]), 2, &SeededRandom::new(1));
        let opener = vault(&Keys::new().key(1, &[1; 32]), 1, &SeededRandom::new(1));
        let envelope = sealer
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        let result = opener.open(VaultPurpose::TotpSecret, record(1), &envelope);

        assert!(
            matches!(result, Err(VaultError::UnknownKey { key_id: 2 })),
            "{result:?}"
        );
    }

    #[test]
    fn an_older_key_still_opens_its_envelopes_and_new_seals_use_the_active_key() {
        let keys = Keys::new().key(1, &[1; 32]).key(2, &[2; 32]);
        let old = vault(&keys, 1, &SeededRandom::new(1));
        let current = vault(&keys, 2, &SeededRandom::new(2));
        let sealed_before = old
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        let opened = current
            .open(VaultPurpose::TotpSecret, record(1), &sealed_before)
            .unwrap();
        let sealed_now = current
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        assert_eq!(opened.expose_secret().as_slice(), b"secret");
        assert_eq!(sealed_now.get(1..5), Some(2_u32.to_be_bytes().as_slice()));
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one_short(OVERHEAD - 1)]
    fn a_short_envelope_is_malformed(#[case] len: usize) {
        let vault = golden_vault();
        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"")
            .unwrap();

        let result = vault.open(VaultPurpose::TotpSecret, record(1), &envelope[..len]);

        assert!(matches!(result, Err(VaultError::Malformed)), "{result:?}");
    }

    #[test]
    fn an_unknown_version_is_malformed() {
        let vault = golden_vault();
        let mut envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();
        envelope[0] = 2;

        let result = vault.open(VaultPurpose::TotpSecret, record(1), &envelope);

        assert!(matches!(result, Err(VaultError::Malformed)), "{result:?}");
    }

    #[test]
    fn a_failing_random_source_fails_the_seal() {
        let random = SeededRandom::new(1);
        let vault = vault(&Keys::new().key(1, &[1; 32]), 1, &random);
        random.fail_next(RandomError::Unavailable);

        let result = vault.seal(VaultPurpose::TotpSecret, record(1), b"secret");

        assert!(
            matches!(result, Err(VaultError::Random(RandomError::Unavailable))),
            "{result:?}"
        );
    }

    #[test]
    fn debug_shows_key_ids_but_no_key() {
        let keys = Keys::new().key(1, &[0xAB; 32]);
        let loaded = keys.load(1).unwrap();

        let shown = format!(
            "{loaded:?} {:?}",
            XChaChaVault::new(keys.load(1).unwrap(), Arc::new(SeededRandom::new(1)))
        );

        assert!(shown.contains("active"), "{shown}");
        assert!(!shown.contains("171"), "{shown}");
        assert!(!shown.to_lowercase().contains("abab"), "{shown}");
        assert!(!shown.contains(&encode(&STANDARD, &[0xAB; 32])), "{shown}");
    }

    // --- Loading the keys ------------------------------------------------------------

    #[test]
    fn a_key_file_of_standard_base64_loads() {
        let keys = Keys::new().key(3, &[9; 32]).key(5, &[8; 32]);

        let loaded = keys.load(5).unwrap();

        assert_eq!(loaded.active(), 5);
        assert_eq!(loaded.keys.len(), 2);
        assert_eq!(
            loaded.keys.get(&3).map(|key| *key.expose_secret()),
            Some([9; 32])
        );
    }

    /// 32 bytes whose standard encoding contains both `+` and `/`.
    const AWKWARD: [u8; 32] = [0xFB; 32];

    #[rstest]
    #[case::url_safe_alphabet(encode(&URL_SAFE, &AWKWARD))]
    #[case::no_padding(encode(&STANDARD_NO_PAD, &[9; 32]))]
    #[case::space_inside(format!("{} {}", &encode(&STANDARD, &[9; 32])[..20], &encode(&STANDARD, &[9; 32])[20..]))]
    #[case::not_base64_at_all("this is no key at all".to_owned())]
    fn anything_but_standard_padded_base64_is_refused(#[case] text: String) {
        assert!(
            encode(&STANDARD, &AWKWARD).contains('+') && encode(&STANDARD, &AWKWARD).contains('/')
        );
        let keys = Keys::new().text(1, &text);

        let result = keys.load(1);

        assert!(
            matches!(result, Err(VaultKeyError::NotBase64 { key_id: 1, .. })),
            "{result:?}"
        );
    }

    #[rstest]
    #[case::thirty_one_bytes(31)]
    #[case::thirty_three_bytes(33)]
    #[case::sixteen_bytes(16)]
    #[case::sixty_four_bytes(64)]
    fn a_key_of_another_length_is_refused_as_the_wrong_length(#[case] len: usize) {
        let keys = Keys::new().key(1, &vec![9; len]);

        let result = keys.load(1);

        assert!(
            matches!(result, Err(VaultKeyError::WrongLength { key_id: 1, .. })),
            "{result:?}"
        );
    }

    /// Windows editors and Windows PowerShell 5.1's `>` add a byte-order mark;
    /// it's invisible, so it gets an error that names it.
    #[rstest]
    #[case::utf8(&[0xEF, 0xBB, 0xBF])]
    #[case::utf16_little_endian(&[0xFF, 0xFE])]
    #[case::utf16_big_endian(&[0xFE, 0xFF])]
    fn a_byte_order_mark_gets_its_own_error(#[case] mark: &[u8]) {
        let mut content = mark.to_vec();
        content.extend_from_slice(encode(&STANDARD, &[9; 32]).as_bytes());
        content.push(b'\n');
        let keys = Keys::new().bytes(1, &content);

        let error = keys.load(1).unwrap_err();

        assert!(
            matches!(error, VaultKeyError::ByteOrderMark { key_id: 1, .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("byte-order mark"), "{error}");
    }

    #[test]
    fn two_ids_with_the_same_key_are_refused() {
        let keys = Keys::new()
            .key(1, &[5; 32])
            .key(4, &[6; 32])
            .key(9, &[5; 32]);

        let result = keys.load(1);

        assert!(
            matches!(
                result,
                Err(VaultKeyError::Duplicate {
                    first: 1,
                    second: 9
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn an_active_key_without_a_file_is_refused() {
        let keys = Keys::new().key(1, &[5; 32]);

        let result = keys.load(2);

        assert!(
            matches!(result, Err(VaultKeyError::ActiveMissing { key_id: 2 })),
            "{result:?}"
        );
    }

    #[test]
    fn a_missing_file_names_its_key_and_path() {
        let keys = Keys::new().key(1, &[5; 32]);
        let mut config = keys.config(1);
        config.key_files.insert(2, keys.dir.path().join("missing"));

        let error = VaultKeys::load(&config).unwrap_err();

        assert!(
            matches!(error, VaultKeyError::File { key_id: 2, .. }),
            "{error:?}"
        );
        // A log line joins the chain: the key, the file and the reason, once
        // each.
        let shown = crate::http::error::chain(&error);
        assert!(
            shown.starts_with(&format!(
                "vault key 2: the secret file {} can't be read: ",
                keys.dir.path().join("missing").display()
            )),
            "{shown}"
        );
        assert_eq!(shown.matches("can't be read").count(), 1, "{shown}");
    }

    #[test]
    fn errors_never_show_a_key_file_s_text() {
        let text = encode(&STANDARD, &[0xCD; 31]);
        let keys = Keys::new().text(1, &format!("{text}\n"));

        let error = keys.load(1).unwrap_err();

        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(&text), "{shown}");
        assert!(!shown.contains(&text[..12]), "{shown}");
    }
}
