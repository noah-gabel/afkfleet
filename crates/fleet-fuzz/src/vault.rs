//! The target over the vault's envelopes (§7.5): the parser every stored
//! Microsoft refresh token and TOTP secret goes through.

use core::sync::atomic::{AtomicU8, Ordering};
use std::collections::BTreeMap;
use std::sync::Arc;

use fleet_core::system::{RandomError, SecureRandom};
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::ports::vault::{Vault, VaultError, VaultPurpose, key_id_of};
use secrecy::{ExposeSecret as _, SecretBox};
use uuid::Uuid;

/// The record every envelope here is sealed for.
const RECORD: Uuid = Uuid::from_u128(0x0189_0000_0000_7000_8000_0000_0000_0001);

/// Another record.
const OTHER_RECORD: Uuid = Uuid::from_u128(0x0189_0000_0000_7000_8000_0000_0000_0002);

/// Nonces that count up: never secure, and never used outside this target,
/// whose vault seals nothing real.
#[derive(Debug, Default)]
struct Counting(AtomicU8);

impl SecureRandom for Counting {
    fn fill(&self, dest: &mut [u8]) -> Result<(), RandomError> {
        dest.fill(self.0.fetch_add(1, Ordering::Relaxed));
        Ok(())
    }
}

/// A vault with one fixed key, 1.
fn vault() -> Option<XChaChaVault> {
    let keys = VaultKeys::from_keys(
        BTreeMap::from([(1, SecretBox::new(Box::new([0x42; 32])))]),
        1,
    )
    .ok()?;
    Some(XChaChaVault::new(keys, Arc::new(Counting::default())))
}

/// The vault over any bytes as an envelope, and over a sealed envelope with
/// one byte changed: no panic; nothing the vault didn't seal opens;
/// [`key_id_of`] and [`Vault::open`] call the same envelopes malformed; and
/// a sealed secret opens only unchanged, for its own record and purpose.
///
/// The first two bytes choose the byte to change and what to change it by;
/// the rest is the secret.
///
/// # Panics
/// When a property breaks.
pub fn vault_envelope(data: &[u8]) {
    let vault = vault();
    assert!(vault.is_some(), "the target's vault builds");
    let Some(vault) = vault else { return };

    let opened = vault.open(VaultPurpose::TotpSecret, RECORD, data);
    assert!(opened.is_err(), "bytes the vault never sealed opened");
    assert_eq!(
        key_id_of(data).is_err(),
        matches!(opened, Err(VaultError::Malformed)),
        "the header's reader and the vault disagree about a malformed envelope"
    );

    let (&[at, by], secret) = data.split_first_chunk::<2>().unwrap_or((&[0, 0], data));
    let Ok(sealed) = vault.seal(VaultPurpose::TotpSecret, RECORD, secret) else {
        return;
    };
    let whole = vault.open(VaultPurpose::TotpSecret, RECORD, &sealed);
    assert!(
        whole.is_ok_and(|opened| opened.expose_secret() == secret),
        "a sealed secret doesn't open as itself"
    );
    assert!(
        vault
            .open(VaultPurpose::TotpSecret, OTHER_RECORD, &sealed)
            .is_err(),
        "a sealed secret opened for another record"
    );
    assert!(
        vault
            .open(VaultPurpose::MsRefreshToken, RECORD, &sealed)
            .is_err(),
        "a sealed secret opened for another purpose"
    );
    let mut changed = sealed;
    let place = usize::from(at) % changed.len().max(1);
    if let Some(byte) = changed.get_mut(place) {
        // Never by 0, which would change nothing.
        *byte ^= by | 1;
    }
    assert!(
        vault
            .open(VaultPurpose::TotpSecret, RECORD, &changed)
            .is_err(),
        "a sealed secret with a changed byte opened"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_targets_nonces_count_up() {
        let random = Counting::default();
        let mut first = [9; 3];
        let mut second = [9; 2];

        random.fill(&mut first).unwrap();
        random.fill(&mut second).unwrap();

        assert_eq!(first, [0, 0, 0]);
        assert_eq!(second, [1, 1]);
    }

    #[test]
    fn the_targets_vault_seals_with_key_1() {
        assert_eq!(vault().unwrap().active_key_id(), 1);
    }
}
