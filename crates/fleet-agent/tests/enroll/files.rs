//! The identity's files (ADR-0019, S18): nothing is ever replaced, the
//! locks keep two runs apart, and a run that ends early leaves nothing.

use std::fs;
use std::path::{Path, PathBuf};

use fleet_agent::enroll::files::{ClaimError, Claimed, PublishError, temp_path};
use rstest::rstest;

use crate::support::{assert_owner_only, names, test_dir};

/// The key's and the certificate's paths in `dir`.
fn paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join("agent.key"), dir.join("agent.crt"))
}

#[rstest]
#[case::the_key("key")]
#[case::the_certificate("cert")]
fn a_claim_takes_nothing_while_a_file_exists(#[case] which: &str) {
    let dir = test_dir(&format!("claim_exists_{which}"));
    let (key, cert) = paths(&dir);
    let existing = if which == "key" { &key } else { &cert };
    fs::write(existing, "someone else's").unwrap();

    let refused = Claimed::claim(&key, &cert).unwrap_err();

    assert!(
        matches!(&refused, ClaimError::Exists { path } if path == existing),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("never replaced"), "{refused}");
    assert_eq!(fs::read_to_string(existing).unwrap(), "someone else's");
    assert_eq!(names(&dir).len(), 1, "{:?}", names(&dir));
}

#[test]
fn a_folder_at_the_key_counts_as_existing() {
    let dir = test_dir("claim_folder");
    let (key, cert) = paths(&dir);
    fs::create_dir(&key).unwrap();

    let refused = Claimed::claim(&key, &cert).unwrap_err();

    assert!(matches!(refused, ClaimError::Exists { .. }), "{refused:?}");
}

#[cfg(unix)]
#[test]
fn a_dangling_link_at_the_certificate_counts_as_existing() {
    let dir = test_dir("claim_dangling");
    let (key, cert) = paths(&dir);
    std::os::unix::fs::symlink(dir.join("nowhere"), &cert).unwrap();

    let refused = Claimed::claim(&key, &cert).unwrap_err();

    assert!(matches!(refused, ClaimError::Exists { .. }), "{refused:?}");
}

#[test]
fn a_missing_folder_is_refused_and_not_created() {
    let dir = test_dir("claim_no_folder");
    let key = dir.join("missing").join("agent.key");
    let cert = dir.join("agent.crt");

    let refused = Claimed::claim(&key, &cert).unwrap_err();

    assert!(
        matches!(&refused, ClaimError::NoFolder { path } if *path == key),
        "{refused:?}"
    );
    assert_eq!(names(&dir), Vec::<String>::new());
}

#[test]
fn a_lock_left_by_another_run_is_refused_and_kept() {
    let dir = test_dir("claim_locked");
    let (key, cert) = paths(&dir);
    fs::write(temp_path(&cert), "another run's").unwrap();

    let refused = Claimed::claim(&key, &cert).unwrap_err();

    assert!(
        matches!(&refused, ClaimError::Locked { path } if *path == temp_path(&cert)),
        "{refused:?}"
    );
    assert_eq!(
        fs::read_to_string(temp_path(&cert)).unwrap(),
        "another run's"
    );
    assert_eq!(names(&dir), ["agent.crt.tmp"]);
}

#[test]
fn a_claim_locks_both_files_and_a_dropped_one_removes_its_locks() {
    let dir = test_dir("claim_drop");
    let (key, cert) = paths(&dir);

    let claimed = Claimed::claim(&key, &cert).unwrap();
    let again = Claimed::claim(&key, &cert);
    claimed.write_key(b"the key").unwrap();
    let held = names(&dir);
    drop(claimed);

    assert!(matches!(again, Err(ClaimError::Locked { .. })), "{again:?}");
    assert_eq!(held, ["agent.crt.tmp", "agent.key.tmp"]);
    assert_eq!(names(&dir), Vec::<String>::new());
}

#[test]
fn a_published_identity_holds_both_files_owner_only_and_no_lock() {
    let dir = test_dir("publish");
    let (key, cert) = paths(&dir);
    let claimed = Claimed::claim(&key, &cert).unwrap();
    claimed.write_key(b"the key").unwrap();
    assert_owner_only(&temp_path(&key));

    claimed.publish(b"the certificate").unwrap();

    assert_eq!(names(&dir), ["agent.crt", "agent.key"]);
    assert_eq!(fs::read(&key).unwrap(), b"the key");
    assert_eq!(fs::read(&cert).unwrap(), b"the certificate");
    assert_owner_only(&key);
    assert_owner_only(&cert);
}

#[test]
fn a_key_that_appeared_meanwhile_is_kept_and_nothing_is_published() {
    let dir = test_dir("publish_key_appeared");
    let (key, cert) = paths(&dir);
    let claimed = Claimed::claim(&key, &cert).unwrap();
    claimed.write_key(b"the key").unwrap();
    fs::write(&key, "someone else's").unwrap();

    let refused = claimed.publish(b"the certificate").unwrap_err();

    assert!(
        matches!(&refused, PublishError::Link { path, .. } if *path == key),
        "{refused:?}"
    );
    assert_eq!(fs::read_to_string(&key).unwrap(), "someone else's");
    assert_eq!(names(&dir), ["agent.key"]);
}

#[test]
fn a_certificate_that_appeared_meanwhile_is_kept_and_the_key_taken_back() {
    let dir = test_dir("publish_cert_appeared");
    let (key, cert) = paths(&dir);
    let claimed = Claimed::claim(&key, &cert).unwrap();
    claimed.write_key(b"the key").unwrap();
    fs::write(&cert, "someone else's").unwrap();

    let refused = claimed.publish(b"the certificate").unwrap_err();

    assert!(
        matches!(&refused, PublishError::Link { path, .. } if *path == cert),
        "{refused:?}"
    );
    assert_eq!(fs::read_to_string(&cert).unwrap(), "someone else's");
    assert_eq!(names(&dir), ["agent.crt"]);
}
