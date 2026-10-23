//! Phase 9's definition of done, automated (Plan.md Phase 9, ADR-0018 group F), with the
//! Microsoft fake over `#[sqlx::test]`'s database file:
//! - **"The DB contains only ciphertext":** after a link, a link again and a
//!   refresh, no token or code is in the database's files, while it's open
//!   (the WAL holds the recent writes) and after it closed.
//! - **"`rotate-key` works on a copy of the DB":** the README's procedure on
//!   a copy with a linked account, run by the real binary; the copy then
//!   works with the new key alone, and the original is untouched.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;

use axum::http::Method;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fleet_core::account::AccountStatus;
use fleet_core::authz::Role;
use fleet_core::id::{AccountId, LinkFlowId};
use fleet_server::app::link_flows::LinkFlowStatus;
use fleet_server::app::session_tokens::{SessionTokenPorts, SessionTokenService};
use fleet_server::config::DatabaseConfig;
use fleet_server::infra::crypto::vault::{VaultKeys, XChaChaVault};
use fleet_server::infra::sqlite::{self, Database, DatabaseOptions};
use fleet_server::ports::accounts::AccountCredential;
use fleet_server::ports::msauth::PollOutcome;
use fleet_server::ports::store::Store as _;
use fleet_server::ports::vault::{Vault as _, VaultPurpose, key_id_of};
use fleet_testkit::system::SeededRandom;
use secrecy::{ExposeSecret as _, SecretBox};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio_util::sync::CancellationToken;

use crate::support::http::{HttpSetup, Person, Seen};
use crate::support::msauth::{Call, Script, device_flow, session, tokens};
use crate::support::service::{DAY, PROFILE, T0, at, bounded};

/// The test harness's vault key (`HttpSetup`'s key 1), and the new one.
const KEY_1: [u8; 32] = [0x21; 32];
const KEY_2: [u8; 32] = [0x42; 32];

/// How long the binary may take.
const DEADLINE: core::time::Duration = core::time::Duration::from_secs(60);

/// Every secret Microsoft's fake hands out in these tests.
const SECRETS: [&str; 13] = [
    "device-code-1",
    "device-code-2",
    "DEVC-0001",
    "DEVC-0002",
    "access-1",
    "access-2",
    "access-3",
    "refresh-1",
    "refresh-2",
    "refresh-3",
    "minecraft-1",
    "minecraft-2",
    "minecraft-3",
];

/// A vault over `key` as key `id` alone.
fn vault(id: u32, key: [u8; 32]) -> XChaChaVault {
    XChaChaVault::new(
        VaultKeys::from_keys(BTreeMap::from([(id, SecretBox::new(Box::new(key)))]), id).unwrap(),
        Arc::new(SeededRandom::new(5)),
    )
}

/// Scripts flow `n`: its start, Microsoft's approval with `access-<n>` and
/// `refresh-<n>`, and the Minecraft session `minecraft-<n>` of [`PROFILE`].
fn script_flow(setup: &HttpSetup, n: u8) {
    setup.fake.script_start(Script::Answer(Ok(device_flow(
        &format!("device-code-{n}"),
        &format!("DEVC-000{n}"),
        at(T0 + 900_000),
    ))));
    setup
        .fake
        .script_poll(Script::Answer(Ok(PollOutcome::Authorized(tokens(
            &format!("access-{n}"),
            &format!("refresh-{n}"),
            at(T0 + 3_600_000),
        )))));
    setup.fake.script_session(Script::Answer(Ok(session(
        &format!("minecraft-{n}"),
        PROFILE,
        "AfkBot1",
        at(T0 + DAY),
    ))));
}

/// The flow a start answered.
fn flow_of(seen: &Seen) -> LinkFlowId {
    assert_eq!(seen.status, 201, "{}", seen.text);
    seen.body["flow_id"].as_str().unwrap().parse().unwrap()
}

/// The flow `id`'s status once it ended; fails the test after 10 seconds.
async fn ended(setup: &HttpSetup, id: LinkFlowId) -> LinkFlowStatus {
    bounded(async {
        loop {
            match setup.link_flows.status(id).map(|view| view.status) {
                Some(LinkFlowStatus::Waiting { .. } | LinkFlowStatus::Finishing) | None => {}
                Some(status) => return status,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

/// The Owner's link of [`PROFILE`] through the routes: active at once.
async fn link(setup: &HttpSetup, owner: &Person) -> AccountId {
    script_flow(setup, 1);
    let flow = flow_of(&setup.start(owner).await);
    let LinkFlowStatus::Linked {
        account, status, ..
    } = ended(setup, flow).await
    else {
        panic!("the link didn't end linked");
    };
    assert_eq!(status, AccountStatus::Active);
    account
}

/// The envelope stored for `account` in `db`.
async fn envelope(db: &Database, account: AccountId) -> Vec<u8> {
    let AccountCredential::Microsoft {
        refresh_envelope, ..
    } = db
        .accounts()
        .find(account)
        .await
        .unwrap()
        .unwrap()
        .credential
    else {
        panic!("an offline account has no envelope");
    };
    refresh_envelope
}

/// `envelope` opened by `vault` for `account`.
fn opened(vault: &XChaChaVault, account: AccountId, envelope: &[u8]) -> String {
    let plaintext = vault
        .open(VaultPurpose::MsRefreshToken, account.into(), envelope)
        .unwrap();
    String::from_utf8(plaintext.expose_secret().clone()).unwrap()
}

/// The session-token service over `db`, sealing and opening with `vault`.
fn tokens_over(setup: &HttpSetup, db: &Database, vault: XChaChaVault) -> SessionTokenService {
    SessionTokenService::new(
        SessionTokenPorts {
            clock: Arc::new(setup.clock.clone()),
            store: Arc::new(db.clone()),
            vault: Arc::new(vault),
            microsoft: setup.fake.clone(),
            live_hints: std::sync::Arc::new(fleet_server::ports::live_hints::NoLiveHints),
        },
        &CancellationToken::new(),
    )
}

/// Fails if any secret is in `file`, its `-wal` or its `-shm`.
fn assert_only_ciphertext(file: &Path, when: &str) {
    let mut read = 0;
    for suffix in ["", "-wal", "-shm"] {
        let mut path = file.to_owned().into_os_string();
        path.push(suffix);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        read += 1;
        for secret in SECRETS {
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()),
                "{secret} is in {}{suffix} {when}",
                file.display()
            );
        }
    }
    assert!(read > 0, "no file of the database to read {when}");
}

#[sqlx::test(migrations = false)]
async fn no_token_reaches_the_database_file_after_a_link_a_link_again_and_a_refresh(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let file = options.get_filename().to_owned();
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Owner).await;
    let account = link(&setup, &owner).await;
    script_flow(&setup, 2);
    let again = setup
        .api(
            Method::POST,
            &format!("/accounts/{account}/relink"),
            &owner,
            None,
        )
        .await;
    assert!(matches!(
        ended(&setup, flow_of(&again)).await,
        LinkFlowStatus::Linked { .. }
    ));
    setup.fake.script_refresh(Script::Answer(Ok(tokens(
        "access-3",
        "refresh-3",
        at(T0 + 3_600_000),
    ))));
    setup.fake.script_session(Script::Answer(Ok(session(
        "minecraft-3",
        PROFILE,
        "AfkBot1",
        at(T0 + DAY),
    ))));
    let service = tokens_over(&setup, &setup.db, vault(1, KEY_1));

    let minted = bounded(service.session(account, false)).await.unwrap();
    assert_only_ciphertext(&file, "while the database is open");
    service.shutdown().await;
    let stored = envelope(&setup.db, account).await;
    setup.db.close().await;

    assert_only_ciphertext(&file, "after it closed");
    assert_eq!(minted.token.expose_secret(), "minecraft-3");
    assert_eq!(opened(&vault(1, KEY_1), account, &stored), "refresh-3");
}

/// A key file holding `key`, readable only by its owner on Unix.
fn key_file(dir: &Path, id: u32, key: [u8; 32]) -> PathBuf {
    let path = dir.join(format!("vault-key-{id}"));
    std::fs::write(&path, format!("{}\n", STANDARD.encode(key))).unwrap();
    owner_only(&path);
    path
}

#[cfg(unix)]
fn owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn owner_only(_path: &Path) {}

/// Runs the server's binary with `args` and none of the test's own
/// `AFKFLEET_SERVER__…` variables, until it exits within the deadline.
fn run(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_afkfleet-server"));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AFKFLEET_SERVER__") {
            command.env_remove(key);
        }
    }
    let mut child = Killed(Some(command.spawn().unwrap()));
    let stdout = drain(child.0.as_mut().unwrap().stdout.take().unwrap());
    let stderr = drain(child.0.as_mut().unwrap().stderr.take().unwrap());
    let stdout = stdout
        .recv_timeout(DEADLINE)
        .expect("the binary should exit before the deadline");
    let stderr = stderr.recv_timeout(DEADLINE).unwrap();
    let status = child.0.take().unwrap().wait().unwrap();
    Output {
        status,
        stdout,
        stderr,
    }
}

/// Kills the child if the test ends first.
struct Killed(Option<Child>);

impl Drop for Killed {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Reads `pipe` to its end on a thread of its own.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    receiver
}

/// `output`'s stdout and stderr, for a failure's message.
fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[sqlx::test(migrations = false)]
async fn rotate_key_moves_a_linked_accounts_token_to_the_new_key_on_a_copy(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let original = options.get_filename().to_owned();
    let setup = HttpSetup::new(options).await;
    let owner = setup.person(1, Role::Owner).await;
    let account = link(&setup, &owner).await;
    setup.db.close().await;
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("afkfleet.db");
    std::fs::copy(&original, &copy).unwrap();
    owner_only(&copy);
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "[database]\npath = '{}'\n[log]\nformat = \"json\"\n[vault]\nmaster_key_files = {{ 1 = '{}', 2 = '{}' }}\nactive_key_id = 2\n",
            copy.display(),
            key_file(dir.path(), 1, KEY_1).display(),
            key_file(dir.path(), 2, KEY_2).display(),
        ),
    )
    .unwrap();
    let config = config.to_str().unwrap();

    let rotated = run(&["vault", "rotate-key", "--config", config]);
    let status = run(&["vault", "status", "--config", config]);

    assert_eq!(rotated.status.code(), Some(0), "{}", text(&rotated));
    assert_eq!(status.status.code(), Some(0), "{}", text(&status));
    let report = String::from_utf8_lossy(&status.stdout);
    assert!(report.contains("key 2 (active)"), "{report}");
    assert!(report.contains("key 1: seals nothing"), "{report}");
    let db = sqlite::open(
        &DatabaseConfig { path: copy.clone() },
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    let sealed = envelope(&db, account).await;
    assert_eq!(key_id_of(&sealed).unwrap(), 2);
    assert_eq!(opened(&vault(2, KEY_2), account, &sealed), "refresh-1");
    assert_eq!(
        db.accounts().find(account).await.unwrap().unwrap().status(),
        AccountStatus::Active
    );
    setup.fake.script_refresh(Script::Answer(Ok(tokens(
        "access-3",
        "refresh-3",
        at(T0 + 3_600_000),
    ))));
    setup.fake.script_session(Script::Answer(Ok(session(
        "minecraft-3",
        PROFILE,
        "AfkBot1",
        at(T0 + DAY),
    ))));
    let service = tokens_over(&setup, &db, vault(2, KEY_2));
    let minted = bounded(service.session(account, false)).await.unwrap();
    service.shutdown().await;
    db.close().await;
    assert_eq!(minted.token.expose_secret(), "minecraft-3");
    assert!(setup.fake.calls().contains(&Call::Refresh {
        refresh_token: "refresh-1".to_owned()
    }));
    // sqlx's test file is 0644, and the server opens only owner-only files on Unix.
    owner_only(&original);
    let before = sqlite::open(
        &DatabaseConfig { path: original },
        DatabaseOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(key_id_of(&envelope(&before, account).await).unwrap(), 1);
    before.close().await;
}
