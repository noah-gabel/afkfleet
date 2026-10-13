//! The server profile (Plan.md P8.4, ADR-0017): which server the app talks
//! to, and which certificates it trusts there. Invalid certificates are
//! never accepted.
//!
//! - **Validated** with fleet-client's own rules: [`ServerUrl`] (an origin;
//!   plain http only for a loopback address) and [`CustomCa::from_pem`]
//!   (certificates only, at most 16, each a valid trust anchor).
//! - **Stored** in `server-profile.json` in the app's local data folder, a
//!   small JSON file with a version. A custom CA is stored as canonical PEM,
//!   re-encoded from the certificates `from_pem` parsed ([`CustomCa::to_pem`]),
//!   never as the text the user pasted, and is read back through `from_pem`.
//!   The file is written to a temporary file next to it and renamed over it,
//!   so it's never half-written.
//! - **Read** at startup, at most [`PROFILE_MAX_BYTES`]. A file that can't
//!   be used is [`Loaded::Unreadable`] with a reason that never repeats its
//!   content; the app then starts without a profile.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use fleet_api_types::app::{
    CommandError, CustomCaView, ProfileUnreadable, ServerProfileRequest, ServerProfileView,
};
use fleet_client::{ClientConfig, CustomCa, ServerUrl, Trust};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error;

/// The profile file's name in the app's local data folder.
pub const PROFILE_FILE: &str = "server-profile.json";

/// The most bytes read from the profile file: the URL's and the CA's caps
/// with room for the JSON around them. A larger file is never read in full.
pub const PROFILE_MAX_BYTES: u64 = 128 * 1024;

/// The profile file's format version.
const VERSION: u32 = 1;

/// A validated server profile: the origin and the certificates to trust.
#[derive(Debug, Clone)]
pub struct ServerProfile {
    url: ServerUrl,
    ca: Option<CustomCa>,
}

impl ServerProfile {
    /// The profile `request` describes, checked by fleet-client's rules.
    ///
    /// # Errors
    /// `invalid_server_url` or `invalid_ca`, each with the broken rule in its
    /// fields.
    pub fn parse(request: &ServerProfileRequest) -> Result<Self, CommandError> {
        let url = ServerUrl::parse(&request.url).map_err(error::server_url)?;
        let ca = request
            .ca_pem
            .as_deref()
            .map(CustomCa::from_pem)
            .transpose()
            .map_err(error::ca)?;
        Ok(Self { url, ca })
    }

    /// The server's origin.
    #[must_use]
    pub fn url(&self) -> &ServerUrl {
        &self.url
    }

    /// The client's config for this profile.
    #[must_use]
    pub fn config(&self) -> ClientConfig {
        ClientConfig {
            server: self.url.clone(),
            trust: self.ca.clone().map_or(Trust::System, Trust::CustomCa),
        }
    }

    /// Whether `other` is the same profile: the same origin and the same
    /// certificates.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        self.url == other.url
            && self.ca.as_ref().map(CustomCa::to_pem) == other.ca.as_ref().map(CustomCa::to_pem)
    }

    /// The profile as the UI shows it.
    #[must_use]
    pub fn view(&self) -> ServerProfileView {
        ServerProfileView {
            url: self.url.as_str().to_owned(),
            custom_ca: self.ca.as_ref().map(|ca| CustomCaView {
                certificates: u32::try_from(ca.len()).unwrap_or(u32::MAX),
                fingerprints: ca.fingerprints().iter().map(fingerprint).collect(),
            }),
        }
    }

    /// The profile as its file holds it.
    fn file(&self) -> ProfileFile {
        ProfileFile {
            version: VERSION,
            url: self.url.as_str().to_owned(),
            ca_pem: self.ca.as_ref().map(CustomCa::to_pem),
        }
    }
}

/// A fingerprint as openssl prints it: uppercase hex pairs joined by colons.
#[must_use]
pub fn fingerprint(hash: &[u8; 32]) -> String {
    hash.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// What the profile file holds.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    version: u32,
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ca_pem: Option<String>,
}

/// Only the version, read first, so a newer file is told apart from a
/// broken one.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

/// What the profile file held at startup.
#[derive(Debug)]
pub enum Loaded {
    /// There's no profile file: no server is set up yet.
    NotSet,
    /// The file can't be used.
    Unreadable(ProfileUnreadable),
    /// The profile.
    Set(ServerProfile),
}

/// Reads the profile file at `path`. It runs once at startup, before the
/// window opens, so it reads synchronously.
#[must_use]
pub fn load(path: &Path) -> Loaded {
    let text = match read_capped(path) {
        Ok(Some(text)) => text,
        Ok(None) => return Loaded::NotSet,
        Err(()) => return Loaded::Unreadable(ProfileUnreadable::Io),
    };
    match serde_json::from_slice::<Versioned>(&text) {
        Ok(Versioned { version: VERSION }) => {}
        Ok(Versioned { .. }) => return Loaded::Unreadable(ProfileUnreadable::Version),
        Err(_) => return Loaded::Unreadable(ProfileUnreadable::Json),
    }
    let Ok(file) = serde_json::from_slice::<ProfileFile>(&text) else {
        return Loaded::Unreadable(ProfileUnreadable::Json);
    };
    let Ok(url) = ServerUrl::parse(&file.url) else {
        return Loaded::Unreadable(ProfileUnreadable::Url);
    };
    match file.ca_pem.as_deref().map(CustomCa::from_pem).transpose() {
        Ok(ca) => Loaded::Set(ServerProfile { url, ca }),
        Err(_) => Loaded::Unreadable(ProfileUnreadable::Ca),
    }
}

/// The file's bytes, `None` when there's no file, or `Err` when it can't be
/// read or is larger than [`PROFILE_MAX_BYTES`], which is never read in full.
fn read_capped(path: &Path) -> Result<Option<Vec<u8>>, ()> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    let mut text = Vec::new();
    file.take(PROFILE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut text)
        .map_err(drop)?;
    if u64::try_from(text.len()).map_or(true, |len| len > PROFILE_MAX_BYTES) {
        return Err(());
    }
    Ok(Some(text))
}

/// The profile file couldn't be written.
#[derive(Debug, Error)]
#[error("the server profile can't be written")]
pub struct SaveError(#[source] io::Error);

/// Writes `profile` to `path`, replacing the file atomically, in a blocking
/// thread. Local disk IO has no timeout: it never waits on a network.
///
/// # Errors
/// [`SaveError`] when the folder, the temporary file or the rename fails.
pub async fn save(path: &Path, profile: &ServerProfile) -> Result<(), SaveError> {
    // These hold only strings and a number, which serde_json always writes.
    let text = serde_json::to_vec_pretty(&profile.file()).unwrap_or_default();
    let path = path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || write_atomically(&path, &text))
        .await
        .unwrap_or_else(|_| Err(io::ErrorKind::Interrupted.into()))
        .map_err(SaveError)
}

/// Writes `text` to `path` through a temporary file next to it, renamed
/// over it once it's complete and on disk.
fn write_atomically(path: &Path, text: &[u8]) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = fs::File::create(&temporary)?;
    file.write_all(text)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use rcgen::{CertificateParams, CertifiedIssuer, KeyPair};
    use tempfile::TempDir;

    use super::*;

    /// A self-signed CA certificate as PEM.
    fn ca_pem(name: &str) -> String {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        CertifiedIssuer::self_signed(params, key).unwrap().pem()
    }

    fn request(url: &str, ca_pem: Option<String>) -> ServerProfileRequest {
        ServerProfileRequest {
            url: url.to_owned(),
            ca_pem,
        }
    }

    fn profile(url: &str, ca_pem: Option<String>) -> ServerProfile {
        ServerProfile::parse(&request(url, ca_pem)).unwrap()
    }

    fn file_in(dir: &TempDir) -> PathBuf {
        dir.path().join(PROFILE_FILE)
    }

    fn reason(loaded: &Loaded) -> Option<ProfileUnreadable> {
        match loaded {
            Loaded::Unreadable(reason) => Some(*reason),
            Loaded::NotSet | Loaded::Set(_) => None,
        }
    }

    #[test]
    fn a_valid_request_is_a_profile() {
        let parsed = profile("https://EXAMPLE.com", None);

        assert_eq!(parsed.url().as_str(), "https://example.com/");
        assert!(matches!(parsed.config().trust, Trust::System));
    }

    #[test]
    fn a_profile_with_a_ca_trusts_only_that_ca() {
        let parsed = profile("https://example.com", Some(ca_pem("Test CA")));

        assert!(matches!(parsed.config().trust, Trust::CustomCa(ca) if ca.len() == 1));
    }

    #[test]
    fn a_refused_url_names_its_rule() {
        let refused = ServerProfile::parse(&request("http://192.0.2.10", None)).unwrap_err();

        assert_eq!(
            refused,
            error::server_url(fleet_client::ServerUrlError::PlainHttp)
        );
    }

    #[test]
    fn a_refused_ca_names_its_rule() {
        let refused = ServerProfile::parse(&request(
            "https://example.com",
            Some("just text".to_owned()),
        ))
        .unwrap_err();

        assert_eq!(refused, error::ca(fleet_client::CaError::NoCertificate));
    }

    #[test]
    fn the_same_origin_and_certificates_are_the_same_profile() {
        let ca = ca_pem("Test CA");
        let one = profile("https://example.com", Some(ca.clone()));
        let reformatted = profile(
            "https://EXAMPLE.com:443/",
            Some(format!(
                "subject=CN = Test CA\r\n{}",
                ca.replace('\n', "\r\n")
            )),
        );

        assert!(one.same_as(&reformatted));
        assert!(!one.same_as(&profile("https://example.com", None)));
        assert!(!one.same_as(&profile("https://example.com", Some(ca_pem("Other CA")))));
        assert!(!one.same_as(&profile("https://example.com:8443", Some(ca))));
        assert!(
            profile("https://example.com", None).same_as(&profile("https://example.com/", None))
        );
    }

    #[test]
    fn the_view_shows_the_origin_and_each_certificates_fingerprint() {
        let two = format!("{}{}", ca_pem("Test CA 1"), ca_pem("Test CA 2"));
        let parsed = profile("https://example.com", Some(two));
        let expected: Vec<String> = match parsed.config().trust {
            Trust::CustomCa(ca) => ca.fingerprints().iter().map(fingerprint).collect(),
            Trust::System => Vec::new(),
        };

        let view = parsed.view();

        assert_eq!(view.url, "https://example.com/");
        let ca = view.custom_ca.unwrap();
        assert_eq!(ca.certificates, 2);
        assert_eq!(ca.fingerprints, expected);
        assert_eq!(profile("https://example.com", None).view().custom_ca, None);
    }

    #[test]
    fn a_fingerprint_is_uppercase_hex_pairs_joined_by_colons() {
        let mut hash = [0_u8; 32];
        hash[0] = 0x0A;
        hash[1] = 0xFF;
        hash[31] = 0x3C;

        let shown = fingerprint(&hash);

        assert_eq!(shown.len(), 95);
        assert!(shown.starts_with("0A:FF:00:"), "{shown}");
        assert!(shown.ends_with(":00:3C"), "{shown}");
    }

    #[tokio::test]
    async fn a_saved_profile_loads_back() {
        let dir = TempDir::new().unwrap();
        let saved = profile("https://example.com:8443", Some(ca_pem("Test CA")));

        save(&file_in(&dir), &saved).await.unwrap();
        let loaded = load(&file_in(&dir));

        assert!(
            matches!(&loaded, Loaded::Set(profile) if profile.same_as(&saved)),
            "{loaded:?}"
        );
    }

    #[tokio::test]
    async fn the_file_holds_the_re_encoded_ca_never_the_pasted_text() {
        let dir = TempDir::new().unwrap();
        let ca = ca_pem("Test CA");
        let pasted = format!("subject=CN = Test CA\r\n{}", ca.replace('\n', "\r\n"));

        save(
            &file_in(&dir),
            &profile("https://example.com", Some(pasted)),
        )
        .await
        .unwrap();
        let text = fs::read_to_string(file_in(&dir)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();

        let stored = json["ca_pem"].as_str().unwrap();
        assert_eq!(stored, CustomCa::from_pem(&ca).unwrap().to_pem());
        assert!(!stored.contains("subject") && !stored.contains('\r'));
        assert_eq!(json["version"], 1);
        assert_eq!(json["url"], "https://example.com/");
    }

    #[tokio::test]
    async fn a_profile_without_a_ca_stores_only_the_version_and_the_url() {
        let dir = TempDir::new().unwrap();

        save(&file_in(&dir), &profile("http://127.0.0.1:8080", None))
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(file_in(&dir)).unwrap()).unwrap();

        assert_eq!(
            json,
            serde_json::json!({ "version": 1, "url": "http://127.0.0.1:8080/" })
        );
    }

    #[tokio::test]
    async fn saving_replaces_the_file_and_leaves_no_temporary_file() {
        let dir = TempDir::new().unwrap();
        save(&file_in(&dir), &profile("https://example.com", None))
            .await
            .unwrap();

        save(&file_in(&dir), &profile("https://example.org", None))
            .await
            .unwrap();

        assert!(
            matches!(load(&file_in(&dir)), Loaded::Set(p) if p.url().as_str() == "https://example.org/")
        );
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [PROFILE_FILE]);
    }

    #[tokio::test]
    async fn saving_creates_the_data_folder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("io.example.test").join(PROFILE_FILE);

        save(&path, &profile("https://example.com", None))
            .await
            .unwrap();

        assert!(matches!(load(&path), Loaded::Set(_)));
    }

    #[tokio::test]
    async fn a_save_that_cant_write_fails() {
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("not-a-folder");
        fs::write(&blocker, b"x").unwrap();

        let result = save(
            &blocker.join(PROFILE_FILE),
            &profile("https://example.com", None),
        )
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn no_file_is_no_profile() {
        let dir = TempDir::new().unwrap();

        assert!(matches!(load(&file_in(&dir)), Loaded::NotSet));
    }

    #[rstest::rstest]
    #[case::not_json("{ not json", ProfileUnreadable::Json)]
    #[case::unknown_field(
        r#"{"version":1,"url":"https://example.com/","extra":true}"#,
        ProfileUnreadable::Json
    )]
    #[case::no_url(r#"{"version":1}"#, ProfileUnreadable::Json)]
    #[case::newer(
        r#"{"version":2,"url":"https://example.com/","new":1}"#,
        ProfileUnreadable::Version
    )]
    #[case::refused_url(r#"{"version":1,"url":"http://192.0.2.10/"}"#, ProfileUnreadable::Url)]
    #[case::refused_ca(
        r#"{"version":1,"url":"https://example.com/","ca_pem":"no"}"#,
        ProfileUnreadable::Ca
    )]
    fn a_file_that_cant_be_used_is_unreadable_with_its_reason(
        #[case] content: &str,
        #[case] expected: ProfileUnreadable,
    ) {
        let dir = TempDir::new().unwrap();
        fs::write(file_in(&dir), content).unwrap();

        assert_eq!(reason(&load(&file_in(&dir))), Some(expected));
    }

    #[test]
    fn a_folder_where_the_file_should_be_is_unreadable() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(file_in(&dir)).unwrap();

        assert_eq!(reason(&load(&file_in(&dir))), Some(ProfileUnreadable::Io));
    }

    #[test]
    fn a_file_over_the_cap_is_unreadable_without_being_read_in_full() {
        let dir = TempDir::new().unwrap();
        let cap = usize::try_from(PROFILE_MAX_BYTES).unwrap();
        let padded = format!(
            r#"{{"version":1,"url":"https://example.com/","ca_pem":"{}"}}"#,
            " ".repeat(cap)
        );
        fs::write(file_in(&dir), padded).unwrap();

        assert_eq!(reason(&load(&file_in(&dir))), Some(ProfileUnreadable::Io));
    }

    #[test]
    fn the_request_caps_match_fleet_clients() {
        assert_eq!(fleet_api_types::app::CA_PEM_MAX_BYTES, CustomCa::MAX_BYTES);
        assert!(
            u64::try_from(
                fleet_api_types::app::CA_PEM_MAX_BYTES + fleet_api_types::app::SERVER_URL_MAX_BYTES
            )
            .unwrap()
                < PROFILE_MAX_BYTES
        );
    }
}
