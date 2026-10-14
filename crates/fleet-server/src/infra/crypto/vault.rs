//! [`XChaChaVault`]: the vault, and [`VaultKeys`], its keys as loaded from
//! their files (Plan.md §7.5, P7.4, ADR-0016).
//!
//! # The envelope, version 1
//! `0x01 | key_id (u32, big-endian) | nonce (24 bytes) | ciphertext + tag (16
//! bytes)`, stored as a BLOB; the port defines its header
//! ([`crate::ports::vault::key_id_of`]). XChaCha20-Poly1305 with a nonce
//! from the [`SecureRandom`] port; the key ID names the key that sealed it,
//! so old keys keep opening their envelopes until `vault rotate-key` (P9.2)
//! has re-encrypted them under the active key.
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

use super::attempt_keys::hmac_sha256;
use crate::config::VaultConfig;
use crate::ports::vault::{
    HEADER_BYTES, OVERHEAD, VERSION_1, Vault, VaultError, VaultPurpose, key_id_of,
};

/// A key's length: one XChaCha20-Poly1305 key.
const KEY_BYTES: usize = 32;
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
        Self::from_keys(keys, config.active_key_id)
    }

    /// The keys `keys`, by ID, with `active` as the active one, checked as
    /// [`VaultKeys::load`] checks the ones it read: the active key must be
    /// there, and no key may appear under two IDs. `load` reads the files;
    /// this takes keys that are already in memory, such as a test's.
    ///
    /// # Errors
    /// [`VaultKeyError::ActiveMissing`] or [`VaultKeyError::Duplicate`].
    pub fn from_keys(
        keys: BTreeMap<u32, SecretBox<[u8; KEY_BYTES]>>,
        active: u32,
    ) -> Result<Self, VaultKeyError> {
        if !keys.contains_key(&active) {
            return Err(VaultKeyError::ActiveMissing { key_id: active });
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
        Ok(Self { keys, active })
    }

    /// The active key's ID.
    #[must_use]
    pub const fn active(&self) -> u32 {
        self.active
    }

    /// The key for `purpose`, derived from the active key as
    /// HMAC-SHA256(active key, the purpose's label) (ADR-0016). It's never
    /// stored: `serve` derives it at startup, so rotating the active key
    /// changes it.
    ///
    /// # Errors
    /// [`VaultKeyError::ActiveMissing`] when the active key has no key,
    /// which [`VaultKeys::load`] already refuses.
    pub fn derive_key(&self, purpose: DerivedKey) -> Result<SecretBox<[u8; 32]>, VaultKeyError> {
        let active = self
            .keys
            .get(&self.active)
            .ok_or(VaultKeyError::ActiveMissing {
                key_id: self.active,
            })?;
        let mut derived = SecretBox::new(Box::new([0_u8; 32]));
        hmac_sha256(
            active.expose_secret(),
            purpose.label().as_bytes(),
            derived.expose_secret_mut(),
        );
        Ok(derived)
    }
}

/// A key the vault's keys derive for another use (ADR-0016). Each label is
/// fixed: renaming one would change every key derived from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedKey {
    /// The key of the `login_attempts` rows' HMAC (P7.10).
    LoginAttempts,
}

impl DerivedKey {
    /// The label the key is derived from.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LoginAttempts => "afkfleet login_attempts v1",
        }
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
    fn active_key_id(&self) -> u32 {
        self.active
    }

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
        let key_id = key_id_of(envelope)?;
        let (nonce, sealed) = envelope
            .get(HEADER_BYTES..)
            .and_then(|rest| rest.split_at_checked(NONCE_BYTES))
            .ok_or(VaultError::Malformed)?;
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
            .map_err(|_| VaultError::Rejected { key_id })?;
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

    /// The golden version 1 envelope of a Microsoft refresh token: `seal` of
    /// [`GOLDEN_REFRESH_PLAINTEXT`] for [`golden_account`] under key 7 =
    /// `[0x42; 32]`. Made once (P9.2) and never regenerated, like
    /// [`GOLDEN_ENVELOPE`]: it pins the purpose's name in the AAD.
    const GOLDEN_REFRESH_ENVELOPE: &str = "0100000007611830d3641a68f94a690dcc25d1f4b0dac948325ac18f6d35531b76929b54f6634e96fc88b51531ce27496bd20475f181e26e2e39e975164790720bfb55c5";
    const GOLDEN_REFRESH_PLAINTEXT: &[u8] = b"golden ms refresh token";

    fn golden_record() -> Uuid {
        "0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6b".parse().unwrap()
    }

    fn golden_account() -> Uuid {
        "0192a3b4-c5d6-7e8f-9a0b-1c2d3e4f5a6c".parse().unwrap()
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

    fn in_memory(id: u32, byte: u8) -> (u32, SecretBox<[u8; KEY_BYTES]>) {
        (id, SecretBox::new(Box::new([byte; KEY_BYTES])))
    }

    #[test]
    fn keys_in_memory_make_the_same_vault_as_loaded_ones() {
        let keys = VaultKeys::from_keys(
            BTreeMap::from([in_memory(GOLDEN_KEY_ID, 0x42)]),
            GOLDEN_KEY_ID,
        )
        .unwrap();
        let vault = XChaChaVault::new(keys, Arc::new(SeededRandom::new(1)));

        let opened = vault
            .open(
                VaultPurpose::TotpSecret,
                golden_record(),
                &hex(GOLDEN_ENVELOPE),
            )
            .unwrap();

        assert_eq!(opened.expose_secret().as_slice(), GOLDEN_PLAINTEXT);
    }

    #[test]
    fn keys_in_memory_are_checked_like_loaded_ones() {
        let missing = VaultKeys::from_keys(BTreeMap::from([in_memory(1, 1)]), 2);
        let twice = VaultKeys::from_keys(BTreeMap::from([in_memory(1, 7), in_memory(2, 7)]), 1);

        assert!(
            matches!(missing, Err(VaultKeyError::ActiveMissing { key_id: 2 })),
            "{missing:?}"
        );
        assert!(
            matches!(
                twice,
                Err(VaultKeyError::Duplicate {
                    first: 1,
                    second: 2
                })
            ),
            "{twice:?}"
        );
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
    fn the_golden_refresh_token_envelope_still_opens() {
        let opened = golden_vault()
            .open(
                VaultPurpose::MsRefreshToken,
                golden_account(),
                &hex(GOLDEN_REFRESH_ENVELOPE),
            )
            .unwrap();

        assert_eq!(opened.expose_secret().as_slice(), GOLDEN_REFRESH_PLAINTEXT);
    }

    #[test]
    fn the_vault_names_its_active_key() {
        let keys = Keys::new().key(1, &[1; 32]).key(2, &[2; 32]);

        assert_eq!(vault(&keys, 2, &SeededRandom::new(1)).active_key_id(), 2);
        assert_eq!(golden_vault().active_key_id(), GOLDEN_KEY_ID);
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
            VaultPurpose::MsRefreshToken => "ms_refresh",
        }
    }

    #[rstest]
    #[case::totp_secret(VaultPurpose::TotpSecret)]
    #[case::ms_refresh(VaultPurpose::MsRefreshToken)]
    fn every_purpose_keeps_its_name(#[case] purpose: VaultPurpose) {
        assert_eq!(purpose.name(), pinned_name(purpose));
    }

    #[rstest]
    #[case::totp_secret_as_refresh_token(VaultPurpose::TotpSecret, VaultPurpose::MsRefreshToken)]
    #[case::refresh_token_as_totp_secret(VaultPurpose::MsRefreshToken, VaultPurpose::TotpSecret)]
    fn a_secret_never_opens_for_another_purpose(
        #[case] sealed_for: VaultPurpose,
        #[case] opened_as: VaultPurpose,
    ) {
        let vault = golden_vault();
        // The same record on both sides: only the purpose differs.
        let envelope = vault.seal(sealed_for, record(1), b"secret").unwrap();

        let result = vault.open(opened_as, record(1), &envelope);

        assert!(
            matches!(
                result,
                Err(VaultError::Rejected {
                    key_id: GOLDEN_KEY_ID
                })
            ),
            "{result:?}"
        );
    }

    /// P9.2's swap test at the vault: a refresh token copied into another
    /// account's row doesn't open there.
    #[test]
    fn a_refresh_token_moved_to_another_account_is_rejected() {
        let vault = golden_vault();
        let first = vault
            .seal(VaultPurpose::MsRefreshToken, record(1), b"first token")
            .unwrap();
        let second = vault
            .seal(VaultPurpose::MsRefreshToken, record(2), b"second token")
            .unwrap();

        let first_in_second = vault.open(VaultPurpose::MsRefreshToken, record(2), &first);
        let second_in_first = vault.open(VaultPurpose::MsRefreshToken, record(1), &second);

        assert!(
            matches!(first_in_second, Err(VaultError::Rejected { .. })),
            "{first_in_second:?}"
        );
        assert!(
            matches!(second_in_first, Err(VaultError::Rejected { .. })),
            "{second_in_first:?}"
        );
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
                _ => assert!(
                    matches!(result, Err(VaultError::Rejected { .. })),
                    "byte {at}"
                ),
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

        assert!(
            matches!(
                result,
                Err(VaultError::Rejected {
                    key_id: GOLDEN_KEY_ID
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn another_record_is_rejected() {
        let vault = golden_vault();
        let envelope = vault
            .seal(VaultPurpose::TotpSecret, record(1), b"secret")
            .unwrap();

        let result = vault.open(VaultPurpose::TotpSecret, record(2), &envelope);

        assert!(
            matches!(
                result,
                Err(VaultError::Rejected {
                    key_id: GOLDEN_KEY_ID
                })
            ),
            "{result:?}"
        );
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

    /// HMAC-SHA256(`[0x42; 32]`, `"afkfleet login_attempts v1"`), computed with
    /// Python's `hmac` and Node's `crypto`.
    const DERIVED_LOGIN_ATTEMPTS: &str =
        "b1712d4846d39fd7e27355abfc46370f5074be7dc4174ba670db7da2186194e0";

    #[test]
    fn the_lockout_key_is_the_hmac_of_its_label_under_the_active_key() {
        let keys = Keys::new().key(1, &[5; 32]).key(2, &[0x42; 32]);

        let derived = keys
            .load(2)
            .unwrap()
            .derive_key(DerivedKey::LoginAttempts)
            .unwrap();

        assert_eq!(
            derived.expose_secret().as_slice(),
            hex(DERIVED_LOGIN_ATTEMPTS)
        );
    }

    #[test]
    fn another_active_key_derives_another_lockout_key() {
        let keys = Keys::new().key(1, &[5; 32]).key(2, &[0x42; 32]);

        let first = keys.load(1).unwrap().derive_key(DerivedKey::LoginAttempts);
        let second = keys.load(2).unwrap().derive_key(DerivedKey::LoginAttempts);

        assert_ne!(
            first.unwrap().expose_secret(),
            second.unwrap().expose_secret()
        );
    }

    #[test]
    fn the_derived_keys_labels_are_pinned() {
        assert_eq!(
            DerivedKey::LoginAttempts.label(),
            "afkfleet login_attempts v1"
        );
    }

    #[test]
    fn deriving_without_the_active_key_is_refused() {
        let mut keys = Keys::new().key(1, &[5; 32]).load(1).unwrap();
        keys.active = 9;

        let result = keys.derive_key(DerivedKey::LoginAttempts);

        assert!(
            matches!(result, Err(VaultKeyError::ActiveMissing { key_id: 9 })),
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
