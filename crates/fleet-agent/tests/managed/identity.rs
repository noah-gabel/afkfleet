//! The managed agent's identity at start (Plan.md P10.7; ADR-0019, S68):
//! the three files read and checked, every refusal naming its files and the
//! fix, never their content.

use core::time::Duration;
use fleet_agent::config::{LogConfig, LogFilter, LogFormat};
use fleet_agent::enroll::certificate::CertificateProblem;
use fleet_agent::enroll::pin::fingerprint;
use fleet_agent::managed::identity::{IdentityError, IdentityFile, ManagedIdentity};
use fleet_agent::telemetry::layer;
use fleet_testkit::log_buffer::LogBuffer;
use rustls::pki_types::UnixTime;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::pki::{ALWAYS, TestCa, Validity, agent_id, agent_key, identity_files, test_dir};

const URL: &str = "https://127.0.0.1:7443";

/// 2026-01-01 to 2026-06-01.
const SPRING: Validity = Validity {
    from: (2026, 1, 1),
    until: (2026, 6, 1),
};

/// A time in seconds since the epoch.
fn at(secs: u64) -> UnixTime {
    UnixTime::since_unix_epoch(Duration::from_secs(secs))
}

/// 2026-03-01, inside SPRING.
const MARCH: u64 = 1_772_323_200;

/// The body line of a PEM: what a leak would show.
fn body(pem: &str) -> &str {
    pem.lines().nth(1).unwrap()
}

fn shown(error: &IdentityError) -> String {
    format!("{error} {error:?}")
}

#[test]
fn an_enrolled_identity_loads_and_shows_only_its_fingerprints() {
    let dir = test_dir("identity", "loads");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let pem = ca.agent_certificate(&key, agent_id(1), ALWAYS);
    let plane = identity_files(&dir, URL, &ca, &key, &pem);

    let identity = ManagedIdentity::load(&plane, at(MARCH)).unwrap();

    let certificate_der = rustls::pki_types::pem::PemObject::from_pem_slice(pem.as_bytes())
        .map(|der: rustls::pki_types::CertificateDer<'static>| der.to_vec())
        .unwrap();
    assert_eq!(
        identity.fingerprints(),
        (fingerprint(&certificate_der), fingerprint(&ca.der))
    );
    let debug = format!("{identity:?}");
    assert!(!debug.contains(body(&key.to_pem())), "{debug}");
    assert!(!debug.contains("PRIVATE"), "{debug}");
}

#[rstest::rstest]
#[case::the_ca(IdentityFile::Ca, "fleet_ca.crt", "pki init")]
#[case::the_key(IdentityFile::Key, "agent.key", "afkfleet-agent enroll")]
#[case::the_certificate(IdentityFile::Certificate, "agent.crt", "afkfleet-agent enroll")]
fn a_missing_file_names_itself_and_the_fix(
    #[case] file: IdentityFile,
    #[case] name: &str,
    #[case] fix: &str,
) {
    let dir = test_dir("identity", &format!("missing_{name}"));
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), ALWAYS),
    );
    std::fs::remove_file(dir.join(name)).unwrap();

    let refused = ManagedIdentity::load(&plane, at(MARCH)).unwrap_err();

    assert!(
        matches!(&refused, IdentityError::File { file: found, .. } if *found == file),
        "{refused:?}"
    );
    let text = refused.to_string();
    assert!(
        text.contains(&dir.join(name).display().to_string()),
        "{text}"
    );
    assert!(text.contains(fix), "{text}");
}

#[test]
fn a_ca_file_with_two_certificates_is_refused_by_its_name() {
    let dir = test_dir("identity", "two_cas");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), ALWAYS),
    );
    crate::pki::write_owner_only(
        &plane.ca_cert_file,
        format!("{}{}", ca.pem, TestCa::new(2).pem).as_bytes(),
    );

    let refused = ManagedIdentity::load(&plane, at(MARCH)).unwrap_err();

    assert!(
        matches!(refused, IdentityError::CaNotOneCertificate { .. }),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("fleet_ca.crt"), "{refused}");
}

#[test]
fn a_key_file_that_isnt_one_ed25519_key_is_refused_without_its_content() {
    let dir = test_dir("identity", "bad_key");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), ALWAYS),
    );
    let two = format!(
        "{}{}",
        key.to_pem().as_str(),
        agent_key(2).to_pem().as_str()
    );
    crate::pki::write_owner_only(&plane.key_file, two.as_bytes());

    let refused = ManagedIdentity::load(&plane, at(MARCH)).unwrap_err();

    assert!(matches!(refused, IdentityError::Key { .. }), "{refused:?}");
    let text = shown(&refused);
    assert!(text.contains("agent.key"), "{text}");
    assert!(!text.contains(body(&key.to_pem())), "{text}");
}

/// Loads an identity whose certificate is `pem`, of the CA of seed 1 and
/// key 1, at `now`.
fn refusal(name: &str, pem: impl FnOnce(&TestCa) -> String, now: u64) -> IdentityError {
    let dir = test_dir("identity", name);
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(&dir, URL, &ca, &key, &pem(&ca));
    ManagedIdentity::load(&plane, at(now)).unwrap_err()
}

#[test]
fn another_cas_certificate_names_the_certificate_and_the_ca() {
    let refused = refusal(
        "another_ca",
        |_| TestCa::new(2).agent_certificate(&agent_key(1), agent_id(1), ALWAYS),
        MARCH,
    );

    assert!(
        matches!(
            refused,
            IdentityError::Certificate {
                problem: CertificateProblem::NotFromTheCa,
                ..
            }
        ),
        "{refused:?}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("agent.crt") && text.contains("fleet_ca.crt"),
        "{text}"
    );
    assert!(text.contains("enroll it again"), "{text}");
}

#[test]
fn another_keys_certificate_names_the_certificate_and_the_key() {
    let refused = refusal(
        "another_key",
        |ca| ca.agent_certificate(&agent_key(2), agent_id(1), ALWAYS),
        MARCH,
    );

    assert!(
        matches!(
            refused,
            IdentityError::Certificate {
                problem: CertificateProblem::NotOurKey,
                ..
            }
        ),
        "{refused:?}"
    );
    let text = refused.to_string();
    assert!(
        text.contains("agent.crt") && text.contains("agent.key"),
        "{text}"
    );
}

#[test]
fn an_expired_certificate_says_to_enroll_again_or_fix_the_clock() {
    // 2026-07-01.
    let refused = refusal(
        "expired",
        |ca| ca.agent_certificate(&agent_key(1), agent_id(1), SPRING),
        1_782_864_000,
    );

    assert!(
        matches!(
            refused,
            IdentityError::Certificate {
                problem: CertificateProblem::Expired { .. },
                ..
            }
        ),
        "{refused:?}"
    );
    let text = refused.to_string();
    assert!(text.contains("2026-06-01"), "{text}");
    assert!(
        text.contains("enroll it again") && text.contains("NTP"),
        "{text}"
    );
}

#[test]
fn a_certificate_not_valid_yet_says_to_fix_the_clock() {
    // 2025-12-01.
    let refused = refusal(
        "not_yet",
        |ca| ca.agent_certificate(&agent_key(1), agent_id(1), SPRING),
        1_764_547_200,
    );

    assert!(
        matches!(
            refused,
            IdentityError::Certificate {
                problem: CertificateProblem::NotYetValid,
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("NTP"), "{refused}");
}

#[cfg(unix)]
#[test]
fn a_key_others_may_read_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = test_dir("identity", "open_key");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), ALWAYS),
    );
    std::fs::set_permissions(&plane.key_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let refused = ManagedIdentity::load(&plane, at(MARCH)).unwrap_err();

    assert!(
        matches!(
            refused,
            IdentityError::File {
                file: IdentityFile::Key,
                ..
            }
        ),
        "{refused:?}"
    );
}

#[test]
fn the_kept_certificate_expires_on_the_runs_clock() {
    let dir = test_dir("identity", "kept");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), SPRING),
    );
    let identity = ManagedIdentity::load(&plane, at(MARCH)).unwrap();

    let (_tls, kept) = identity.into_tls("localhost".to_owned());

    assert_eq!(kept.validity(at(MARCH)), Ok(()));
    assert!(
        matches!(
            kept.validity(at(1_782_864_000)),
            Err(CertificateProblem::Expired { .. })
        ),
        "after its end"
    );
}

#[test]
fn the_start_line_shows_both_fingerprints_and_no_key() {
    let dir = test_dir("identity", "logged");
    let ca = TestCa::new(1);
    let key = agent_key(1);
    let plane = identity_files(
        &dir,
        URL,
        &ca,
        &key,
        &ca.agent_certificate(&key, agent_id(1), ALWAYS),
    );
    let identity = ManagedIdentity::load(&plane, at(MARCH)).unwrap();
    let logs = LogBuffer::default();
    let config = LogConfig {
        format: LogFormat::Json,
        filter: LogFilter::try_from("debug").unwrap(),
    };
    let subscriber = tracing_subscriber::registry().with(layer(&config, false, logs.clone()));

    tracing::subscriber::with_default(subscriber, || identity.log_loaded());

    let lines = logs.lines_with("the agent's identity is loaded").unwrap();
    assert_eq!(lines.len(), 1, "{}", logs.text());
    let (certificate, authority) = identity.fingerprints();
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["certificate_sha256"], certificate.to_string());
    assert_eq!(lines[0]["ca_sha256"], authority.to_string());
    assert!(
        !logs.text().contains(body(&key.to_pem())),
        "{}",
        logs.text()
    );
}

#[test]
fn a_server_certificate_in_the_agents_place_is_refused() {
    let refused = refusal("server_certificate", |ca| ca.server_certificate(9).0, MARCH);

    assert!(
        matches!(
            refused,
            IdentityError::Certificate {
                problem: CertificateProblem::NotFromTheCa,
                ..
            }
        ),
        "{refused:?}"
    );
}
