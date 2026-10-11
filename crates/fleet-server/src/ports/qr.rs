//! QR codes (Plan.md P7.7, ADR-0016): [`QrCodes`] draws a text as a QR
//! code, for the enrollment's otpauth URI.
//!
//! The only implementation is infra's `SvgQr`: an SVG with nothing but the
//! modules, sent as a `data:image/svg+xml;base64,…` URI that the app shows
//! only through an `<img>`. Its text holds the TOTP secret, so the result
//! is a secret too.

use core::fmt;

use secrecy::SecretString;

/// Why a text can't be a QR code: it's longer than the largest QR code
/// holds, which no otpauth URI comes near.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the text is too long for a QR code")]
pub struct QrError;

/// Drawing QR codes. Microseconds of work, so it's synchronous.
pub trait QrCodes: Send + Sync + fmt::Debug {
    /// `text` as a QR code: an SVG `data:` URI.
    ///
    /// # Errors
    /// [`QrError`] for a text too long for any QR code.
    fn svg_data_uri(&self, text: &SecretString) -> Result<SecretString, QrError>;
}
