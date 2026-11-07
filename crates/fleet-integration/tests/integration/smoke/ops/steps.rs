//! The runbook's operations on a [`Stack`] (ADR-0022, S25, S35–S43): each
//! the way an operator runs it. On CI's runners (`Setup::Script`) that's
//! `deploy/setup.sh` itself, with `sudo -n` exactly where the runbook says
//! `sudo`; elsewhere (`Setup::Steps`, a Windows PC) the same compose calls
//! the script makes, since PowerShell has no POSIX sh. The nightly Script
//! runs catch any drift between the two.
//!
//! The keys' copy never touches a file on a PC: in Steps mode it's the tar
//! streams `docker compose cp … -` writes, held in memory and fed back with
//! `docker cp -a`, so their owners and modes come through as the server's
//! image made them.

use std::path::{Path, PathBuf};

use fleet_testkit::docker::{Finished, finish};

use super::super::checks::{stamp_in, tar_files};
use super::super::stack::{QUICK, Root, SERVER_CONFIG, SLOW, Setup, Stack};

/// The keys' copy, as `keys-backup` left it.
#[derive(Debug)]
pub(crate) enum KeysCopy {
    /// Script mode: the root-only folder `keys-backup` printed.
    Folder(PathBuf),
    /// Steps mode: the `keys` and `ca` volumes' tar streams.
    Tar { keys: Vec<u8>, ca: Vec<u8> },
}

/// A folder `sudo` made outside the stack's copy (the keys' copy under
/// `/root`, CI alone), removed with `sudo` when dropped: only that path.
#[derive(Debug)]
pub(crate) struct RootFolder(pub(crate) PathBuf);

impl Drop for RootFolder {
    fn drop(&mut self) {
        let mut command = std::process::Command::new("sudo");
        command.args(["-n", "rm", "-rf", "--"]).arg(&self.0);
        let _ = finish(command, None, QUICK);
    }
}

/// Now in UTC as a stamp, `YYYYMMDDTHHMMSSZ`.
fn stamp_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// `docker <args>` in `dir` with `stdin` on its standard input, which must
/// succeed.
fn docker_with_stdin(dir: &Path, args: &[&str], stdin: &[u8]) -> Finished {
    let mut command = std::process::Command::new("docker");
    command.current_dir(dir).args(args);
    let finished = finish(command, Some(stdin), SLOW).unwrap();
    assert!(
        finished.success,
        "docker {args:?} failed: {}",
        finished.stderr
    );
    finished
}

impl Stack {
    /// `sh setup.sh check`: it reads and changes nothing, so Steps mode has
    /// nothing to do.
    pub(crate) fn check(&mut self) {
        if self.setup == Setup::Script {
            let out = self.setup_sh_ok(&["check"], Root::No);
            assert!(out.contains("is installed here"), "{out}");
        }
    }

    /// `sh setup.sh update` (S36): a backup with the running server, the
    /// pull, the server first, then the rest.
    pub(crate) fn update(&mut self) {
        match self.setup {
            Setup::Script => {
                let out = self.setup_sh_ok(&["update"], Root::No);
                assert!(out.contains("afkfleet is updated"), "{out}");
            }
            Setup::Steps => {
                self.backup();
                self.ok(&["pull"], SLOW);
                self.ok(
                    &[
                        "up",
                        "--detach",
                        "--wait",
                        "--wait-timeout",
                        "180",
                        "server",
                    ],
                    SLOW,
                );
                self.start();
            }
        }
    }

    /// Every service up and healthy: the command `restore` prints.
    pub(crate) fn start(&mut self) {
        self.ok(&["up", "--detach", "--wait", "--wait-timeout", "180"], SLOW);
    }

    /// A backup now, with the running server; its file's name.
    pub(crate) fn backup(&mut self) -> String {
        let path = self.ok(
            &[
                "exec",
                "-T",
                "server",
                "/usr/local/bin/afkfleet-server",
                "backup",
                "--config",
                SERVER_CONFIG,
            ],
            QUICK,
        );
        let path = path.trim();
        path.strip_prefix("/backups/")
            .unwrap_or_else(|| panic!("not a backup's path: {path}"))
            .to_owned()
    }

    /// The server's command `args` in the `maintenance` service, which
    /// must succeed; its stdout.
    pub(crate) fn maintenance(&mut self, args: &[&str]) -> String {
        let mut full = vec!["run", "--rm", "-T", "maintenance"];
        full.extend_from_slice(args);
        full.extend(["--config", SERVER_CONFIG]);
        self.ok(&full, SLOW)
    }

    /// `sh setup.sh restore NAME` after a crash, which ends with the server
    /// stopped and the start command printed (S40); the stamp the old files
    /// were moved aside under.
    pub(crate) fn restore(&mut self, name: &str) -> String {
        let (out, stamp) = self.restore_steps(name);
        if let Some(out) = out {
            assert!(out.contains("docker compose up --detach --wait"), "{out}");
            return stamp_in(&out).unwrap_or_else(|| panic!("no stamp in:\n{out}"));
        }
        stamp
    }

    /// `sh setup.sh restore NAME` on a new host, with no server container:
    /// it ends with init as the next step, never another start (PR #99's
    /// review).
    pub(crate) fn restore_on_a_new_host(&mut self, name: &str) {
        if let (Some(out), _) = self.restore_steps(name) {
            assert!(out.contains("sudo sh setup.sh init"), "{out}");
            assert!(!out.contains("docker compose up"), "{out}");
        }
    }

    /// Restore's steps: setup.sh's own output in Script mode, or the same
    /// compose calls in Steps mode; and Steps mode's stamp.
    fn restore_steps(&mut self, name: &str) -> (Option<String>, String) {
        match self.setup {
            Setup::Script => (
                Some(self.setup_sh_ok(&["restore", name], Root::No)),
                String::new(),
            ),
            Setup::Steps => {
                self.ok(&["run", "--rm", "-T", "maintenance", "--version"], SLOW);
                self.ok(&["stop", "agent", "server"], SLOW);
                let stamp = stamp_now();
                self.ok(
                    &["run", "--rm", "-T", "files", "restore-in", &stamp, name],
                    SLOW,
                );
                self.maintenance(&["migrate"]);
                self.ok(&["run", "--rm", "-T", "files", "finished", &stamp], SLOW);
                self.maintenance(&["vault", "status"]);
                self.maintenance(&["agent", "list"]);
                (None, stamp)
            }
        }
    }

    /// `ls -la` of a volume `folder` (`data`, `tls`) through the `files`
    /// service's image, which mounts it.
    pub(crate) fn listing(&mut self, folder: &str) -> String {
        self.ok(
            &[
                "run",
                "--rm",
                "-T",
                "--entrypoint",
                "ls",
                "files",
                "-la",
                &format!("/vol/{folder}"),
            ],
            QUICK,
        )
    }

    /// `sh setup.sh forget-aside STAMP` with the server healthy (S40).
    pub(crate) fn forget_aside(&mut self, stamp: &str) {
        match self.setup {
            Setup::Script => {
                self.setup_sh_ok(&["forget-aside", stamp], Root::No);
            }
            Setup::Steps => {
                self.ok(&["run", "--rm", "-T", "files", "forget", stamp], SLOW);
            }
        }
    }

    /// `sh setup.sh renew-cert` (S41) with `names` beside `server`; the
    /// stamp the old pair was moved aside under.
    pub(crate) fn renew_cert(&mut self, names: &[&str]) -> String {
        match self.setup {
            Setup::Script => {
                let out = self.setup_sh_ok(&["renew-cert"], Root::No);
                stamp_in(&out).unwrap_or_else(|| panic!("no stamp in:\n{out}"))
            }
            Setup::Steps => {
                self.ok(&["stop", "server"], SLOW);
                let stamp = stamp_now();
                self.ok(&["run", "--rm", "-T", "files", "tls-aside", &stamp], SLOW);
                let mut pki = vec![
                    "run",
                    "--rm",
                    "-T",
                    "setup",
                    "pki",
                    "server-cert",
                    "--config",
                    SERVER_CONFIG,
                    "--dns",
                    "server",
                ];
                for name in names {
                    pki.extend(["--dns", name]);
                }
                self.ok(&pki, SLOW);
                self.ok(&["run", "--rm", "-T", "files", "finished", &stamp], SLOW);
                self.start();
                stamp
            }
        }
    }

    /// Adds `line` to the stack's `.env`, as an operator edits it.
    pub(crate) fn add_to_env(&mut self, line: &str) {
        let path = self.dir().join(".env");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(line);
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    /// The vault key's rotation, as the runbook's commands (S42): a backup,
    /// key 2 named in `.env` and made, then active, the secrets sealed
    /// again with the server stopped, the status, the start; the keys' copy
    /// is the caller's next step.
    pub(crate) fn rotate_key(&mut self) {
        self.backup();
        self.add_to_env("AFKFLEET_SERVER__VAULT__MASTER_KEY_FILES__2=/keys/vault-key-2");
        self.ok(
            &[
                "run",
                "--rm",
                "-T",
                "setup",
                "vault",
                "create-key",
                "--config",
                SERVER_CONFIG,
                "--id",
                "2",
            ],
            SLOW,
        );
        self.add_to_env("AFKFLEET_SERVER__VAULT__ACTIVE_KEY_ID=2");
        self.ok(&["stop", "agent", "server"], SLOW);
        self.maintenance(&["vault", "rotate-key"]);
        let status = self.maintenance(&["vault", "status"]);
        assert!(status.contains('2'), "{status}");
        self.start();
    }

    /// `sudo sh setup.sh keys-backup` (S39): the copy of the `keys` and `ca`
    /// volumes.
    pub(crate) fn keys_backup(&mut self) -> (KeysCopy, Option<RootFolder>) {
        match self.setup {
            Setup::Script => {
                let out = self.setup_sh_ok(&["keys-backup"], Root::Sudo);
                let folder = out
                    .split_whitespace()
                    .find(|word| word.starts_with("/root/afkfleet-keys-"))
                    .map_or_else(
                        || panic!("no folder in:\n{out}"),
                        |word| word.trim_end_matches([':', '.']),
                    );
                let folder = PathBuf::from(folder);
                (KeysCopy::Folder(folder.clone()), Some(RootFolder(folder)))
            }
            Setup::Steps => {
                let tar = |folder: &str| {
                    let finished = self
                        .compose
                        .run_bytes(&["cp", &format!("server:/{folder}/."), "-"], QUICK)
                        .unwrap();
                    assert!(finished.success, "cp {folder}: {}", finished.stderr);
                    finished.stdout
                };
                let keys = tar("keys");
                let ca = tar("ca");
                (KeysCopy::Tar { keys, ca }, None)
            }
        }
    }

    /// `sudo sh setup.sh keys-restore DIR` on a new host (S39, R30): the
    /// keys and the CA into new volumes as the server's user, then a server
    /// certificate for `names` beside `server`.
    pub(crate) fn keys_restore(&mut self, copy: &KeysCopy, names: &[&str]) {
        match (self.setup, copy) {
            (Setup::Script, KeysCopy::Folder(folder)) => {
                let folder = folder.display().to_string();
                self.setup_sh_ok(&["keys-restore", &folder], Root::Sudo);
            }
            (Setup::Steps, KeysCopy::Tar { keys, ca }) => {
                self.ok(&["--profile", "setup", "create", "setup"], SLOW);
                let id = self.ok(&["ps", "--all", "--quiet", "setup"], QUICK);
                let id = id.trim().to_owned();
                assert!(!id.is_empty(), "setup wasn't created");
                let dir = self.dir().to_path_buf();
                docker_with_stdin(&dir, &["cp", "-a", "-", &format!("{id}:/keys")], keys);
                docker_with_stdin(&dir, &["cp", "-a", "-", &format!("{id}:/ca")], ca);
                self.ok(&["rm", "--force", "setup"], QUICK);
                let mut pki = vec![
                    "run",
                    "--rm",
                    "-T",
                    "setup",
                    "pki",
                    "server-cert",
                    "--config",
                    SERVER_CONFIG,
                    "--dns",
                    "server",
                ];
                for name in names {
                    pki.extend(["--dns", name]);
                }
                self.ok(&pki, SLOW);
            }
            (setup, copy) => panic!("{setup:?} can't restore {copy:?}"),
        }
    }

    /// The backup `name` of `from` put into this stack's backups, as the
    /// server's user, owner-only: the runbook's `sudo install -o 65532 -g
    /// 65532 -m 600` on CI, a tar stream into a created server container
    /// elsewhere.
    pub(crate) fn put_backup(&mut self, from: &Path, name: &str, from_compose: &Stack) {
        match self.setup {
            Setup::Script => {
                let mut command = std::process::Command::new("sudo");
                command
                    .args(["-n", "install", "-o", "65532", "-g", "65532", "-m", "600"])
                    .arg(from.join(name))
                    .arg(self.dir().join("srv").join("backups").join(name));
                let finished = finish(command, None, QUICK).unwrap();
                assert!(finished.success, "{}", finished.stderr);
            }
            Setup::Steps => {
                let tar = from_compose
                    .compose
                    .run_bytes(&["cp", &format!("server:/backups/{name}"), "-"], QUICK)
                    .unwrap();
                assert!(tar.success, "{}", tar.stderr);
                assert_eq!(tar_files(&tar.stdout).len(), 1);
                self.ok(&["create", "server"], SLOW);
                let id = self.ok(&["ps", "--all", "--quiet", "server"], QUICK);
                let dir = self.dir().to_path_buf();
                docker_with_stdin(
                    &dir,
                    &["cp", "-a", "-", &format!("{}:/backups", id.trim())],
                    &tar.stdout,
                );
                self.ok(&["rm", "--force", "server"], QUICK);
            }
        }
    }

    /// The uninstall (S47): the project's containers, networks and volumes
    /// by its label, never a prune; then nothing with its label is left.
    pub(crate) fn uninstall(&mut self) {
        self.ok(&["down", "--volumes", "--remove-orphans"], SLOW);
        let project = self.compose.project().to_owned();
        let label = format!("label=com.docker.compose.project={project}");
        for kind in [
            &["ps", "--all", "--quiet", "--filter"][..],
            &["volume", "ls", "--quiet", "--filter"][..],
            &["network", "ls", "--quiet", "--filter"][..],
        ] {
            let mut args = kind.to_vec();
            args.push(&label);
            let left = self.docker(&args);
            assert_eq!(left.stdout.trim(), "", "{args:?} left something");
        }
    }
}
