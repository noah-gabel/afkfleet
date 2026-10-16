//! The enrollment bundle, read from stdin only (Plan.md P10.3; ADR-0019,
//! S15): never a flag, an environment variable or a file, where it would
//! linger in a shell's history, a process listing or a disk.
//!
//! - **A terminal is refused:** typing the bundle would echo it, and a
//!   script that forgot the pipe would hang. It's piped in.
//! - **At most [`MAX_INPUT_BYTES`]** are read, into a fixed buffer that's
//!   wiped when it's dropped; more is refused.
//! - **One trailing line break** (`\n` or `\r\n`, as PowerShell's pipe
//!   writes it) is removed; anything else must be exactly the bundle.

use std::io::{self, Read};

use fleet_core::enrollment::EnrollmentBundle;
use zeroize::Zeroizing;

/// The most stdin may hold: a bundle and a line break fit many times over.
pub const MAX_INPUT_BYTES: usize = 1024;

/// Why stdin holds no bundle. No message quotes what was read.
#[derive(Debug, thiserror::Error)]
pub enum BundleInputError {
    /// stdin is a terminal.
    #[error(
        "stdin is a terminal: pipe the bundle in, as in `$bundle | afkfleet-agent enroll --config …`"
    )]
    Terminal,
    /// stdin couldn't be read.
    #[error("stdin can't be read")]
    Read(#[source] io::Error),
    /// stdin holds more than [`MAX_INPUT_BYTES`].
    #[error("stdin holds more than 1 KiB, so it isn't an enrollment bundle")]
    TooLong,
    /// stdin isn't one bundle.
    #[error(
        "stdin isn't an enrollment bundle: one line, the token, a dot and the CA's fingerprint"
    )]
    Malformed,
}

/// The bundle `input` holds; `is_terminal` says whether it's a terminal.
///
/// # Errors
/// [`BundleInputError`].
pub fn read_bundle(
    mut input: impl Read,
    is_terminal: bool,
) -> Result<EnrollmentBundle, BundleInputError> {
    if is_terminal {
        return Err(BundleInputError::Terminal);
    }
    // One byte more than allowed, so a longer input shows itself.
    let mut buffer = Zeroizing::new([0_u8; MAX_INPUT_BYTES + 1]);
    let mut len = 0;
    while let Some(rest) = buffer.get_mut(len..).filter(|rest| !rest.is_empty()) {
        match input.read(rest) {
            Ok(0) => break,
            Ok(read) => len += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(BundleInputError::Read(error)),
        }
    }
    if len > MAX_INPUT_BYTES {
        return Err(BundleInputError::TooLong);
    }
    let read = buffer.get(..len).ok_or(BundleInputError::TooLong)?;
    let text = core::str::from_utf8(read).map_err(|_| BundleInputError::Malformed)?;
    let line = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    EnrollmentBundle::try_from(line).map_err(|_| BundleInputError::Malformed)
}

#[cfg(test)]
mod tests {
    use fleet_core::pki::CertFingerprint;
    use secrecy::{ExposeSecret as _, SecretString};

    use super::*;

    fn bundle() -> EnrollmentBundle {
        EnrollmentBundle::new(
            SecretString::from(format!("afk_et_{}", "A".repeat(43))),
            CertFingerprint::from_digest([0xAB; 32]),
        )
        .unwrap()
    }

    fn text() -> String {
        bundle().to_text().expose_secret().to_owned()
    }

    #[test]
    fn a_piped_bundle_is_read_with_or_without_its_line_break() {
        for input in [text(), format!("{}\n", text()), format!("{}\r\n", text())] {
            let read = read_bundle(input.as_bytes(), false).unwrap();

            assert_eq!(read.to_text().expose_secret(), text().as_str(), "{input:?}");
        }
    }

    #[test]
    fn a_terminal_is_refused_before_anything_is_read() {
        let refused = read_bundle(text().as_bytes(), true);

        assert!(
            matches!(refused, Err(BundleInputError::Terminal)),
            "{refused:?}"
        );
    }

    #[test]
    fn more_than_1_kib_is_refused() {
        let input = format!("{}\n{}", text(), " ".repeat(MAX_INPUT_BYTES));

        let refused = read_bundle(input.as_bytes(), false);

        assert!(
            matches!(refused, Err(BundleInputError::TooLong)),
            "{refused:?}"
        );
    }

    #[test]
    fn anything_but_one_bundle_is_refused_without_being_quoted() {
        for input in [
            String::new(),
            "\n".to_owned(),
            format!("{}\n\n", text()),
            format!(" {}", text()),
            format!("{}\n{}\n", text(), text()),
            "afk_et_secret.not-a-fingerprint".to_owned(),
            format!("{}\u{FEFF}", text()),
        ] {
            let refused = read_bundle(input.as_bytes(), false).unwrap_err();

            assert!(
                matches!(refused, BundleInputError::Malformed),
                "{input:?}: {refused:?}"
            );
            assert!(!refused.to_string().contains("afk_et_"), "{refused}");
        }
    }

    #[test]
    fn a_read_error_is_reported() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("the pipe broke"))
            }
        }

        let refused = read_bundle(Broken, false);

        assert!(
            matches!(refused, Err(BundleInputError::Read(_))),
            "{refused:?}"
        );
    }
}
