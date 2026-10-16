//! Reading the PKI's PEM files: each holds exactly one section.
//!
//! rustls's PEM reader (rustls-pki-types) reads every section it knows
//! (`CERTIFICATE`, `PRIVATE KEY`, `CERTIFICATE REQUEST`, …). A file whose
//! sections aren't exactly one of the expected kind is refused, and the
//! reader's own errors are never passed on: they can quote a line of the
//! file, which may be a key.

use rustls::pki_types::pem::{SectionKind, SliceIter};
use zeroize::Zeroizing;

/// The DER of the one section in `pem`, when there's exactly one section the
/// reader knows and it's of `kind`. Its bytes are wiped when dropped.
pub(super) fn single_section(pem: &[u8], kind: SectionKind) -> Option<Zeroizing<Vec<u8>>> {
    let mut sections = SliceIter::<(SectionKind, Vec<u8>)>::new(pem);
    let (found, der) = sections.next()?.ok()?;
    let der = Zeroizing::new(der);
    // A second section of any kind, or a broken one, refuses the file.
    (found == kind && sections.next().is_none()).then_some(der)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n";
    const KEY: &str = "-----BEGIN PRIVATE KEY-----\nAwQF\n-----END PRIVATE KEY-----\n";

    #[test]
    fn the_one_section_of_its_kind_is_read() {
        let der = single_section(CERT.as_bytes(), SectionKind::Certificate).unwrap();

        assert_eq!(der.as_slice(), [0, 1, 2].as_slice());
    }

    #[test]
    fn text_around_the_section_is_ignored() {
        let pem = format!("a comment\n{KEY}trailing\n");

        let der = single_section(pem.as_bytes(), SectionKind::PrivateKey).unwrap();

        assert_eq!(der.as_slice(), [3, 4, 5].as_slice());
    }

    #[test]
    fn anything_but_exactly_one_section_of_its_kind_is_refused() {
        for pem in [
            String::new(),
            "no section".to_owned(),
            KEY.to_owned(),
            format!("{CERT}{CERT}"),
            format!("{CERT}{KEY}"),
            "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n".to_owned(),
            "-----BEGIN CERTIFICATE-----\nAAEC\n".to_owned(),
        ] {
            assert!(
                single_section(pem.as_bytes(), SectionKind::Certificate).is_none(),
                "{pem}"
            );
        }
    }
}
