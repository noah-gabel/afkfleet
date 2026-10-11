//! [`SvgQr`]: the enrollment's QR code as an SVG (Plan.md P7.7, ADR-0016).
//!
//! qrcodegen encodes the text (error correction Medium) into its grid of
//! modules; this writer draws the grid itself, so no image codec joins the
//! tree. The document holds exactly two elements inside the `<svg>`: a
//! white `<path>` that covers the whole `viewBox` (a transparent code on a
//! dark screen can't be scanned) and one black `<path>` of unit squares,
//! one per dark module, offset by the 4-module quiet zone. Nothing else: no
//! text, links, scripts, styles or external references. It's sent as a
//! `data:image/svg+xml;base64,…` URI, which the app shows only through an
//! `<img>`.
//!
//! The text holds the TOTP secret, so the drawing does too: every buffer
//! here is wiped, and the result is a [`SecretString`].

use core::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use qrcodegen::{QrCode, QrCodeEcc};
use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroizing;

use crate::ports::qr::{QrCodes, QrError};

/// The quiet zone around the code, in modules (ISO/IEC 18004's 4).
const QUIET_ZONE: i32 = 4;

/// What a data URI of an SVG starts with.
const DATA_URI_PREFIX: &str = "data:image/svg+xml;base64,";

/// QR codes as SVG data URIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvgQr;

impl SvgQr {
    /// The adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl QrCodes for SvgQr {
    fn svg_data_uri(&self, text: &SecretString) -> Result<SecretString, QrError> {
        let svg = svg(text.expose_secret())?;
        data_uri(&svg)
    }
}

/// `text` as a QR code in an SVG document.
fn svg(text: &str) -> Result<Zeroizing<String>, QrError> {
    let code = QrCode::encode_text(text, QrCodeEcc::Medium).map_err(|_| QrError)?;
    let modules = code.size();
    let side = modules.saturating_add(QUIET_ZONE.saturating_mul(2));
    let mut svg = Zeroizing::new(String::new());
    write!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><path fill="#fff" d="M0 0h{side}v{side}H0z"/><path fill="#000" d=""##
    )
    .map_err(|_| QrError)?;
    for y in 0..modules {
        for x in 0..modules {
            if code.get_module(x, y) {
                write!(
                    svg,
                    "M{} {}h1v1h-1z",
                    x.saturating_add(QUIET_ZONE),
                    y.saturating_add(QUIET_ZONE)
                )
                .map_err(|_| QrError)?;
            }
        }
    }
    svg.push_str(r#""/></svg>"#);
    Ok(svg)
}

/// `svg` as a base64 `data:` URI, written once into a buffer of its exact
/// length.
fn data_uri(svg: &str) -> Result<SecretString, QrError> {
    let encoded_len = base64::encoded_len(svg.len(), true).ok_or(QrError)?;
    let mut uri = Zeroizing::new(vec![
        0_u8;
        DATA_URI_PREFIX.len().saturating_add(encoded_len)
    ]);
    let (prefix, encoded) = uri
        .split_at_mut_checked(DATA_URI_PREFIX.len())
        .ok_or(QrError)?;
    prefix.copy_from_slice(DATA_URI_PREFIX.as_bytes());
    let written = STANDARD
        .encode_slice(svg.as_bytes(), encoded)
        .map_err(|_| QrError)?;
    if written != encoded_len {
        return Err(QrError);
    }
    let text = core::str::from_utf8(uri.as_slice()).map_err(|_| QrError)?;
    Ok(SecretString::from(text))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fleet_api_types::mfa::QR_SVG_MAX_CHARS;

    use super::*;

    /// The test URI: RFC 6238's secret for the user `afkbot1`.
    const URI: &str =
        "otpauth://totp/afkfleet:afkbot1?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=afkfleet";

    /// The SVG inside a data URI. base64 is built without `alloc`, so it
    /// decodes into a buffer of the encoded length, then cut.
    fn decode(uri: &SecretString) -> String {
        let encoded = uri.expose_secret().strip_prefix(DATA_URI_PREFIX).unwrap();
        let mut bytes = vec![0_u8; encoded.len()];
        let len = STANDARD.decode_slice(encoded, &mut bytes).unwrap();
        bytes.truncate(len);
        String::from_utf8(bytes).unwrap()
    }

    fn drawn(text: &str) -> String {
        decode(&SvgQr.svg_data_uri(&SecretString::from(text)).unwrap())
    }

    /// The black path's squares, as module positions.
    fn squares(svg: &str) -> Vec<(i32, i32)> {
        let start = svg.find(r##"<path fill="#000" d=""##).unwrap() + 21;
        let path = &svg[start..svg[start..].find('"').unwrap() + start];
        path.split('M')
            .filter(|square| !square.is_empty())
            .map(|square| {
                let (position, rest) = square.split_once('h').unwrap();
                assert_eq!(rest, "1v1h-1z", "{square}");
                let (x, y) = position.split_once(' ').unwrap();
                (x.parse().unwrap(), y.parse().unwrap())
            })
            .collect()
    }

    #[test]
    fn the_test_uris_svg_is_pinned() {
        insta::assert_snapshot!(drawn(URI));
    }

    #[test]
    fn every_dark_module_is_one_square_and_nothing_else_is() {
        let code = QrCode::encode_text(URI, QrCodeEcc::Medium).unwrap();
        let dark: BTreeSet<(i32, i32)> = (0..code.size())
            .flat_map(|y| (0..code.size()).map(move |x| (x, y)))
            .filter(|(x, y)| code.get_module(*x, *y))
            .map(|(x, y)| (x + QUIET_ZONE, y + QUIET_ZONE))
            .collect();

        let squares = squares(&drawn(URI));

        assert_eq!(squares.len(), dark.len(), "one square per dark module");
        assert_eq!(squares.into_iter().collect::<BTreeSet<_>>(), dark);
    }

    #[test]
    fn the_view_box_is_the_code_and_its_quiet_zone() {
        let code = QrCode::encode_text(URI, QrCodeEcc::Medium).unwrap();
        let side = code.size() + 8;

        let svg = drawn(URI);

        assert!(
            svg.contains(&format!(r#"viewBox="0 0 {side} {side}""#)),
            "{svg}"
        );
        assert!(
            svg.contains(&format!(
                r##"<path fill="#fff" d="M0 0h{side}v{side}H0z"/>"##
            )),
            "{svg}"
        );
    }

    /// Every tag's name and attribute names, in order.
    fn tags(svg: &str) -> Vec<(String, Vec<String>)> {
        svg.split('<')
            .skip(1)
            .map(|tag| {
                let tag = tag.split('>').next().unwrap().trim_end_matches('/');
                let mut parts = tag.split(' ');
                let name = parts.next().unwrap().to_owned();
                let attributes = tag
                    .split('"')
                    .step_by(2)
                    .filter_map(|part| part.trim().rsplit(' ').next())
                    .filter_map(|attribute| attribute.strip_suffix('='))
                    .map(str::to_owned)
                    .collect();
                (name, attributes)
            })
            .collect()
    }

    #[test]
    fn the_document_holds_only_the_allowed_elements_and_attributes() {
        let svg = drawn(URI);

        assert_eq!(
            tags(&svg),
            [
                (
                    "svg".to_owned(),
                    vec![
                        "xmlns".to_owned(),
                        "viewBox".to_owned(),
                        "shape-rendering".to_owned()
                    ]
                ),
                ("path".to_owned(), vec!["fill".to_owned(), "d".to_owned()]),
                ("path".to_owned(), vec!["fill".to_owned(), "d".to_owned()]),
                ("/svg".to_owned(), vec![]),
            ]
        );
        for forbidden in ["script", "href", "style", "url(", "text", "<a", "foreign"] {
            assert!(!svg.contains(forbidden), "{forbidden}");
        }
        assert_eq!(svg.matches("http").count(), 1, "only the namespace");
    }

    #[test]
    fn the_longest_usernames_qr_fits_its_cap() {
        let longest = format!(
            "otpauth://totp/afkfleet:{}?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=afkfleet",
            "a".repeat(32)
        );

        let uri = SvgQr.svg_data_uri(&SecretString::from(longest)).unwrap();

        assert!(
            uri.expose_secret().chars().count() <= QR_SVG_MAX_CHARS,
            "{}",
            uri.expose_secret().len()
        );
    }

    #[test]
    fn a_text_too_long_for_any_qr_code_is_refused() {
        let text = SecretString::from("a".repeat(4_000));

        assert_eq!(SvgQr.svg_data_uri(&text).err(), Some(QrError));
    }
}
