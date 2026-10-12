//! What the app needs before its window opens (Plan.md P8.4, ADR-0017): the
//! aws-lc-rs TLS provider, which `main` installs before any client is built,
//! and the errors that keep the app from starting.

use thiserror::Error;

/// Why the app couldn't start. No variant holds a path's or a store's
/// content.
#[derive(Debug, Error)]
pub enum StartError {
    /// Another TLS crypto provider was installed first.
    #[error("a TLS crypto provider is already installed")]
    CryptoProvider,
    /// The Windows Credential Manager can't be opened.
    #[error("the Windows Credential Manager can't be opened")]
    Keychain,
    /// Tauri couldn't build or run the app, or find its data folder.
    #[error("the app can't start")]
    Tauri(#[from] tauri::Error),
}

/// Installs aws-lc-rs as the process's TLS crypto provider, the only one the
/// workspace allows (CLAUDE.md, security rule 9).
///
/// # Errors
/// [`StartError::CryptoProvider`] when a provider is already installed.
pub fn install_crypto_provider() -> Result<(), StartError> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| StartError::CryptoProvider)
}

#[cfg(test)]
mod tests {
    use super::*;

    // nextest runs each test in its own process, so this one installs first.
    #[test]
    fn the_provider_installs_once_and_a_second_install_fails() {
        let first = install_crypto_provider();
        let second = install_crypto_provider();

        assert!(first.is_ok(), "{first:?}");
        assert!(
            matches!(second, Err(StartError::CryptoProvider)),
            "{second:?}"
        );
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn errors_have_fixed_texts() {
        assert_eq!(
            StartError::CryptoProvider.to_string(),
            "a TLS crypto provider is already installed"
        );
        assert_eq!(
            StartError::Keychain.to_string(),
            "the Windows Credential Manager can't be opened"
        );
    }
}
