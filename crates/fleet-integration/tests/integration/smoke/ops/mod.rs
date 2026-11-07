//! The runbook's operations on the production project (Plan.md P12.6;
//! ADR-0022, S25, S35–S46): what an operator runs after the install, each
//! command as the runbook gives it, against both images built here.
//! `just test-stack` runs them beside the smoke tests, and the Stack
//! workflow nightly, where `deploy/setup.sh` itself runs (`steps`).
//!
//! - **A restore after a crash** (`afkfleet-ops-crash`, the host path): the
//!   install with the backups' folder left to `sudo sh setup.sh init`, the
//!   Owner, `check`, `update`; a write, a backup, another write; the server
//!   killed; `restore` moves the database and its `-wal` aside, never
//!   opened, and ends stopped; `files put-back` undoes it, and a second
//!   `restore` does it again; the start it prints brings back the first
//!   write alone; `forget-aside` removes exactly the aside and undone files;
//!   then `renew-cert`, the vault key's rotation, the first look at the
//!   services (`ps`, the agent's last lines), incident response and the
//!   uninstall by label, with nothing of the project left.
//! - **A new host from the keys' copy** (`afkfleet-ops-old-host`, then
//!   `-new-host`): on the old host the install, the Owner, a write, a backup
//!   and `keys-backup`, then it stops; on the new one `keys-restore`, the
//!   backup put in place, `restore`, the old local agent disabled, `init`
//!   on the restored state. The Owner logs in with the same authenticator,
//!   the write is there, the CA is the same bytes, a new local agent comes
//!   online, the key files are the server's user's, owner-only, and nothing
//!   of the copy reaches any output. On CI a remote agent
//!   (`afkfleet-ops-remote`), updated in its own folder with `setup.sh`
//!   first, moves with the server and comes back online
//!   with its old ID, never enrolling again.
//!
//! Every wait is bounded and wakes on an event: compose's `--wait`, or a
//! service's next log line (S19's form).

mod steps;

use core::time::Duration;

use fleet_api_types::accounts::CreateOfflineAccountRequest;
use fleet_api_types::agents::{AgentResponse, AgentStatus};
use fleet_api_types::{PageQuery, SecretText};
use fleet_client::Trust;
use fleet_testkit::docker::Compose;
use tempfile::TempDir;

use self::steps::KeysCopy;
use super::checks::{agents, tar_files};
use super::stack::{
    AGENT_CONFIG, AGENT_IMAGE, ProxyPath, QUICK, Root, SERVER_CONFIG, SLOW, Setup, Spec, Stack,
    repo,
};
use super::{Owner, create_owner, draw_password};

/// The name remote agents dial, and the host port their gRPC port is
/// published on (CI's runners alone).
const GRPC: (&str, u16) = ("ops-grpc.test", 17443);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_ops_restore_after_a_crash() {
    let mut stack = Stack::prepare_as(Spec {
        path: ProxyPath::Host,
        project: "afkfleet-ops-crash",
        net: 40,
        make_backups: false,
        ops: true,
        grpc: None,
    });
    stack.set_up_as(Root::Sudo);
    let password = draw_password();
    stack.transcript.forbid(&password);
    create_owner(&mut stack, &password);
    let mut owner = Owner::first(&url(&mut stack), Trust::System, &password, &mut stack).await;

    stack.check();
    // The update recreates nothing here (the same images), so the session stays.
    stack.update();

    // A write before the backup, the backup, a write after it; then a crash.
    account(&owner, "OpsBefore").await;
    let backup = stack.backup();
    account(&owner, "OpsAfter").await;
    stack.ok(&["kill", "server"], QUICK);

    let undone = stack.restore(&backup);
    assert_restored(&mut stack, &undone);
    // Undone with the runbook's command for a refused restore: the crash's
    // database and its -wal back in place, the restored one set apart.
    stack.ok(&["run", "--rm", "-T", "files", "put-back", &undone], SLOW);
    let data = stack.listing("data");
    assert!(
        data.contains(&format!("afkfleet.db.undone-{undone}")),
        "{data}"
    );
    assert!(
        data.lines().any(|line| line.ends_with(" afkfleet.db-wal")),
        "the crash's -wal isn't back:\n{data}"
    );

    let stamp = stack.restore(&backup);
    assert_ne!(stamp, undone);
    assert_restored(&mut stack, &stamp);
    stack.start();
    let at = url(&mut stack);
    owner.again(&at, Trust::System, &stack).await;
    let names = account_names(&owner).await;
    assert!(names.iter().any(|name| name == "OpsBefore"), "{names:?}");
    assert!(!names.iter().any(|name| name == "OpsAfter"), "{names:?}");

    let backups_before = backups_hash(&mut stack, &backup);
    stack.forget_aside(&stamp);
    stack.forget_aside(&undone);
    let data = stack.listing("data");
    assert!(
        !data.contains(".aside-") && !data.contains(".undone-"),
        "{data}"
    );
    assert_eq!(backups_hash(&mut stack, &backup), backups_before);

    let renewed = stack.renew_cert(&[]);
    let tls = stack.listing("tls");
    assert!(
        tls.contains(&format!("server.crt.aside-{renewed}")),
        "{tls}"
    );
    let at = url(&mut stack);
    owner.again(&at, Trust::System, &stack).await;
    let local = until_online(&mut stack, &owner, "local").await;

    stack.rotate_key();
    let at = url(&mut stack);
    owner.again(&at, Trust::System, &stack).await;

    first_look(&mut stack);
    incident_response(&mut stack, &local);
    stack.uninstall();
    stack.transcript.assert_clean();
}

/// After `restore` NAME under `stamp`, with the server stopped: the crash's
/// `-wal` moved aside with its database, never opened, and no live `-wal`
/// left beside the restored one.
fn assert_restored(stack: &mut Stack, stamp: &str) {
    let data = stack.listing("data");
    assert!(
        data.contains(&format!("afkfleet.db-wal.aside-{stamp}")),
        "the crash's -wal wasn't moved aside:\n{data}"
    );
    assert!(
        !data.lines().any(|line| line.ends_with(" afkfleet.db-wal")),
        "a live -wal is left beside the restored database:\n{data}"
    );
}

/// The runbook's first look at a service that won't start or keeps
/// restarting: every service's state, then the agent's last 50 lines.
fn first_look(stack: &mut Stack) {
    let states = stack.ok(&["ps"], QUICK);
    assert!(states.contains("(healthy)"), "{states}");
    let logs = stack.ok(&["logs", "--tail", "50", "agent"], QUICK);
    let lines = logs.lines().count();
    assert!((1..=50).contains(&lines), "{lines} lines:\n{logs}");
}

/// Incident response, as the runbook's commands: every agent listed beside
/// the running server; public dialling off through `.env` (U10); the agent
/// `local` disabled for good and its bots released, then stopped, since its
/// restarts would be refused.
fn incident_response(stack: &mut Stack, local: &AgentResponse) {
    let list = stack.ok(
        &[
            "exec",
            "-T",
            "server",
            "/usr/local/bin/afkfleet-server",
            "agent",
            "list",
            "--config",
            SERVER_CONFIG,
        ],
        QUICK,
    );
    let listed = agents(&list).unwrap_or_else(|| panic!("{list}"));
    assert!(
        listed.iter().any(|agent| agent.id == local.id.to_string()),
        "{list}"
    );
    stack.add_to_env("AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS=false");
    stack.ok(&["up", "--detach", "agent"], SLOW);
    let id = stack.container("agent");
    let env = stack.docker(&[
        "inspect",
        "--format",
        "{{range .Config.Env}}{{println .}}{{end}}",
        &id,
    ]);
    assert!(
        env.stdout
            .lines()
            .any(|line| line == "AFKFLEET_AGENT__CONTROL_PLANE__PUBLIC_TARGETS=false"),
        "{}",
        env.stdout
    );
    for command in ["disable", "release"] {
        stack.ok(
            &[
                "exec",
                "-T",
                "server",
                "/usr/local/bin/afkfleet-server",
                "agent",
                command,
                "--config",
                SERVER_CONFIG,
                "--id",
                &local.id.to_string(),
            ],
            QUICK,
        );
    }
    stack.ok(&["stop", "agent"], SLOW);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_ops_new_host_from_the_keys_copy() {
    let script = Setup::from_env() == Setup::Script;
    let grpc = script.then_some(GRPC);
    let names: &[&str] = if script { &[GRPC.0] } else { &[] };
    let mut old = Stack::prepare_as(Spec {
        path: ProxyPath::Host,
        project: "afkfleet-ops-old-host",
        net: 50,
        make_backups: true,
        ops: true,
        grpc,
    });
    old.set_up_with(Root::No, names);
    let password = draw_password();
    old.transcript.forbid(&password);
    create_owner(&mut old, &password);
    let mut owner = Owner::first(&url(&mut old), Trust::System, &password, &mut old).await;
    let old_local = until_online(&mut old, &owner, "local").await;
    let remote = if script {
        let remote = Remote::enroll(&mut old);
        let online = until_online(&mut old, &owner, Remote::NAME).await;
        // The runbook's update in a remote agent's folder (PR #99's review):
        // no backup, the agent's image, the agent started again.
        remote.update(&mut old);
        let again = until_online(&mut old, &owner, Remote::NAME).await;
        assert_eq!(again.id, online.id, "the update enrolled the agent again");
        Some((online, remote))
    } else {
        None
    };
    account(&owner, "OpsMoved").await;
    let backup = old.backup();
    let (copy, _copy_folder) = old.keys_backup();
    let old_ca = file_of(&old, "ca", "ca.crt");
    forbid_the_keys(&mut old, &copy);
    old.ok(&["stop"], SLOW);

    let mut new = Stack::prepare_as(Spec {
        path: ProxyPath::Host,
        project: "afkfleet-ops-new-host",
        net: 60,
        // keys-restore makes the backups' folder, as init would, before the
        // runbook copies the backup into it.
        make_backups: false,
        ops: true,
        grpc,
    });
    forbid_the_keys(&mut new, &copy);
    new.keys_restore(&copy, names);
    new.put_backup(&old.dir().join("srv").join("backups"), &backup, &old);
    new.restore_on_a_new_host(&backup);
    let list = new.maintenance(&["agent", "list"]);
    let old_agent = agents(&list)
        .unwrap_or_else(|| panic!("{list}"))
        .into_iter()
        .find(|agent| agent.name == "local" && agent.state == "enabled")
        .unwrap_or_else(|| panic!("the old local agent isn't listed:\n{list}"));
    assert_eq!(old_agent.id, old_local.id.to_string());
    new.maintenance(&["agent", "disable", "--id", &old_agent.id]);
    new.set_up_restored(Root::No);

    let at = url(&mut new);
    owner.again(&at, Trust::System, &new).await;
    let names_now = account_names(&owner).await;
    assert!(
        names_now.iter().any(|name| name == "OpsMoved"),
        "{names_now:?}"
    );
    assert_eq!(file_of(&new, "ca", "ca.crt"), old_ca, "the CA changed");
    let new_local = until_online(&mut new, &owner, "local").await;
    assert_ne!(new_local.id, old_local.id, "the new host's agent is new");
    if let Some((old_remote, _remote)) = &remote {
        let moved = until_online(&mut new, &owner, Remote::NAME).await;
        assert_eq!(moved.id, old_remote.id, "the remote agent enrolled again");
    }
    for (folder, file) in [
        ("keys", "vault-key-1"),
        ("keys", "ca.key"),
        ("ca", "ca.crt"),
    ] {
        let (mode, uid, gid) = header_of(&new, folder, file);
        assert_eq!(
            (mode & 0o777, uid, gid),
            (0o600, 65532, 65532),
            "{folder}/{file}"
        );
    }
    old.transcript.assert_clean();
    new.transcript.assert_clean();
}

/// The server's API on the host path, as the test is the host's proxy.
fn url(stack: &mut Stack) -> String {
    let port = stack.port(None, "server", "8080");
    format!("http://127.0.0.1:{port}")
}

/// An offline account named `name`, made by the Owner.
async fn account(owner: &Owner, name: &str) {
    owner
        .client
        .create_offline_account(&CreateOfflineAccountRequest {
            name: SecretText::new(name.to_owned()),
        })
        .await
        .unwrap();
}

/// Every account's name the Owner sees.
async fn account_names(owner: &Owner) -> Vec<String> {
    owner
        .client
        .accounts(&PageQuery {
            before: None,
            limit: Some(100),
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|account| account.name.as_str().to_owned())
        .collect()
}

/// The agent `name`, once the Owner sees it online: woken at each of the
/// server's log lines, within [`SLOW`].
async fn until_online(stack: &mut Stack, owner: &Owner, name: &str) -> AgentResponse {
    let follower = stack.compose.follow("server").unwrap();
    let started = std::time::Instant::now();
    loop {
        let page = owner
            .client
            .agents(&PageQuery {
                before: None,
                limit: Some(100),
            })
            .await
            .unwrap();
        if let Some(found) = page.items.into_iter().find(|agent| {
            agent.name.as_str() == name && agent.status.known() == Some(AgentStatus::Online)
        }) {
            return found;
        }
        assert!(started.elapsed() < SLOW, "{name} never came online");
        follower.wait(Duration::from_secs(2));
    }
}

/// The backup `name`'s SHA-256, read through the `probe` service.
fn backups_hash(stack: &mut Stack, name: &str) -> String {
    stack.ok(
        &[
            "run",
            "--rm",
            "--entrypoint",
            "sha256sum",
            "probe",
            &format!("/backups/{name}"),
        ],
        QUICK,
    )
}

/// `file`'s bytes in the server's volume `folder`, through `docker compose
/// cp`'s tar stream (the server's container, running or stopped).
fn file_of(stack: &Stack, folder: &str, file: &str) -> Vec<u8> {
    tar_of(stack, folder, file)
        .into_iter()
        .next()
        .map_or_else(|| panic!("no {folder}/{file}"), |(_, data)| data)
}

/// `file`'s mode, owner and group in the server's volume `folder`, as its
/// tar header records them.
fn header_of(stack: &Stack, folder: &str, file: &str) -> (u32, u32, u32) {
    let finished = stack
        .compose
        .run_bytes(&["cp", &format!("server:/{folder}/{file}"), "-"], QUICK)
        .unwrap();
    assert!(finished.success, "{}", finished.stderr);
    let head = finished.stdout.get(..512).unwrap();
    let octal = |offset: usize, length: usize| {
        let field = head.get(offset..offset + length).unwrap();
        let text = String::from_utf8_lossy(field);
        u32::from_str_radix(text.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap()
    };
    (octal(100, 8), octal(108, 8), octal(116, 8))
}

/// The files of `folder`/`file` in the server's container, as a tar.
fn tar_of(stack: &Stack, folder: &str, file: &str) -> Vec<(String, Vec<u8>)> {
    let finished = stack
        .compose
        .run_bytes(&["cp", &format!("server:/{folder}/{file}"), "-"], QUICK)
        .unwrap();
    assert!(finished.success, "{}", finished.stderr);
    tar_files(&finished.stdout)
}

/// Every key's text forbidden in `stack`'s outputs: the copy never reaches a
/// terminal (U41, S39).
fn forbid_the_keys(stack: &mut Stack, copy: &KeysCopy) {
    let KeysCopy::Tar { keys, .. } = copy else {
        // The folder is root's alone: its files are checked by setup.sh's
        // tests; the transcript holds only what setup.sh printed.
        return;
    };
    for (name, data) in tar_files(keys) {
        let text = String::from_utf8_lossy(&data);
        // A line of the key itself, never a PEM header any certificate shares.
        if let Some(line) = text
            .lines()
            .find(|line| line.len() > 20 && !line.starts_with("-----"))
        {
            stack.transcript.forbid(line.trim());
        } else {
            panic!("{name} holds no line to look for");
        }
    }
}

/// A remote agent on another compose project, on CI's runners alone: the
/// server's CA copied to its folder as the runbook says (65532, 600), its
/// name and URL in its `.env`, host networking so it dials the server's
/// published gRPC port as a remote host would; `setup.sh` beside them, for
/// the runbook's update in a remote agent's folder.
struct Remote {
    _cleanup: fleet_testkit::docker::Cleanup,
    dir: TempDir,
}

impl Remote {
    const NAME: &str = "ops-remote";

    /// Enrolled with `server`'s stack and running.
    fn enroll(server: &mut Stack) -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("prod")).unwrap();
        for file in ["compose.agent.yaml", "prod/agent.remote.toml", "setup.sh"] {
            std::fs::copy(repo().join("deploy").join(file), dir.path().join(file)).unwrap();
        }
        // The runbook's copy of the CA's certificate, on the server's host.
        let target = dir.path().join("ca.crt");
        server.ok(
            &["cp", "server:/ca/ca.crt", target.to_str().unwrap()],
            QUICK,
        );
        for args in [
            &["-n", "chown", "65532:65532"][..],
            &["-n", "chmod", "600"][..],
        ] {
            let mut command = std::process::Command::new("sudo");
            command.args(args).arg(dir.path().join("ca.crt"));
            let finished = fleet_testkit::docker::finish(command, None, QUICK).unwrap();
            assert!(finished.success, "{}", finished.stderr);
        }
        std::fs::write(
            dir.path().join(".env"),
            format!(
                "COMPOSE_PROJECT_NAME=afkfleet-ops-remote\nCOMPOSE_PATH_SEPARATOR=:\nCOMPOSE_FILE=compose.agent.yaml:compose.override.yaml\nAFKFLEET_AGENT_IMAGE={AGENT_IMAGE}\nAFKFLEET_AGENT__NAME={}\nAFKFLEET_AGENT__CONTROL_PLANE__URL=https://{}:{}\nAFKFLEET_CA_CERT_FILE=./ca.crt\n",
                Self::NAME,
                GRPC.0,
                GRPC.1
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("compose.override.yaml"),
            format!(
                "# The ops test's remote agent: the host's network, so it dials the published port.\nservices:\n  agent:\n    pull_policy: never\n    network_mode: host\n    extra_hosts:\n      - \"{}:127.0.0.1\"\n",
                GRPC.0
            ),
        )
        .unwrap();
        let compose = Compose::new(
            dir.path(),
            "afkfleet-ops-remote",
            &["compose.agent.yaml", "compose.override.yaml"],
        );
        let _ = compose.down(SLOW);
        let token = server
            .compose
            .run(
                &[
                    "exec",
                    "-T",
                    "server",
                    "/usr/local/bin/afkfleet-server",
                    "agent",
                    "create-enrollment-token",
                    "--config",
                    SERVER_CONFIG,
                ],
                QUICK,
            )
            .unwrap();
        assert!(token.success, "{}", token.stderr);
        server.transcript.forbid(token.stdout.trim());
        let enrolled = compose
            .run_with_stdin(
                &[
                    "run",
                    "--rm",
                    "-T",
                    "--no-deps",
                    "agent",
                    "enroll",
                    "--config",
                    AGENT_CONFIG,
                ],
                token.stdout.as_bytes(),
                QUICK,
            )
            .unwrap();
        server.transcript.keep(&enrolled);
        assert!(enrolled.success, "{}", enrolled.stderr);
        let up = compose
            .run(&["up", "--detach", "--wait", "--wait-timeout", "180"], SLOW)
            .unwrap();
        assert!(up.success, "{}", up.stderr);
        Self {
            _cleanup: compose.cleanup(SLOW),
            dir,
        }
    }

    /// `sh setup.sh update` in its folder, as the runbook's Agents on other
    /// hosts gives it: no backup, the agent's image pulled (none here: the
    /// test's override never pulls) and the agent started.
    fn update(&self, server: &mut Stack) {
        let mut command = std::process::Command::new("sh");
        command
            .args(["setup.sh", "update"])
            .current_dir(self.dir.path());
        let finished = fleet_testkit::docker::finish(command, None, SLOW).unwrap();
        server.transcript.keep(&finished);
        assert!(finished.success, "{}", finished.stderr);
        assert!(
            finished.stdout.contains("ok: the agent is updated"),
            "{}",
            finished.stdout
        );
    }
}
