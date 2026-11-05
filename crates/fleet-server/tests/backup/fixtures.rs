//! What the backup tests share: a migrated database holding one user, a
//! backups' folder beside it, and ways to read what a folder holds.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fleet_core::audit::{AuditAction, AuditOutcome};
use fleet_core::authz::Role;
use fleet_core::id::UserId;
use fleet_core::value::Username;
use fleet_server::config::DatabaseConfig;
use fleet_server::infra::sqlite::{Database, DatabaseOptions, open, open_migrated};
use fleet_server::ports::audit::NewAuditEntry;
use fleet_server::ports::store::Store;
use fleet_server::ports::users::{NewUser, PasswordHash};
use secrecy::SecretString;
use tempfile::TempDir;

/// A valid Argon2id PHC string (not a real hash).
const PHC: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g";

/// The one user's name, which a backup must hold.
pub(crate) const CANARY: &str = "canaryuser";

pub(crate) fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().to_utc()
}

/// A temporary folder holding the database and its backups' folder.
pub(crate) struct Setup {
    pub(crate) root: TempDir,
    pub(crate) database: PathBuf,
    pub(crate) backups: PathBuf,
    pub(crate) db: Database,
}

/// A migrated database with [`CANARY`] in it, and an empty backups' folder.
pub(crate) async fn setup() -> Setup {
    let root = TempDir::new().unwrap();
    let database = root.path().join("data").join("afkfleet.db");
    let backups = root.path().join("data").join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let db = open(
        &DatabaseConfig {
            path: database.clone(),
            backups: None,
        },
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    let created = at("2027-01-01T00:00:00Z");
    let mut tx = db.write().await.unwrap();
    tx.users()
        .insert(&NewUser {
            id: UserId::new_v7(created, [7; 10]).unwrap(),
            username: Username::try_from(CANARY).unwrap(),
            password_hash: PasswordHash::new(SecretString::from(PHC)).unwrap(),
            role: Role::Owner,
            created_at: created,
        })
        .await
        .unwrap();
    tx.commit(NewAuditEntry::new(
        created,
        AuditAction::try_from("test.backup_setup").unwrap(),
        AuditOutcome::Success,
    ))
    .await
    .unwrap();
    Setup {
        root,
        database,
        backups,
        db,
    }
}

/// The names in `dir`, sorted.
pub(crate) fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Whether `file` holds `text`.
pub(crate) fn holds(file: &Path, text: &str) -> bool {
    std::fs::read(file)
        .unwrap()
        .windows(text.len())
        .any(|window| window == text.as_bytes())
}

/// Copies `backup` alone into a fresh folder and opens it as a migrated
/// database, as a restore would: it must open without migrating.
pub(crate) async fn opens_as_a_database(backup: &Path) -> bool {
    let fresh = TempDir::new().unwrap();
    let copy = fresh.path().join("afkfleet.db");
    std::fs::copy(backup, &copy).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let opened = open_migrated(
        &DatabaseConfig {
            path: copy,
            backups: None,
        },
        DatabaseOptions::default(),
    )
    .await;
    match opened {
        Ok(db) => {
            db.close().await;
            true
        }
        Err(_) => false,
    }
}
