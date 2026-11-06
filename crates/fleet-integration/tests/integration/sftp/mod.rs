//! The backups' SFTP account, proven in a throwaway container (Plan.md
//! P12.2; ADR-0022, S15, U14): `deploy/host/sftp-backups.sh` installed into
//! a Debian 13 sshd built from the builder image the images pin, then the
//! account tried from the same container's ssh client with a key made
//! there and thrown away with it. `just test-stack` runs it, and so does
//! the Stack workflow; it never runs on a real host.
//!
//! `slow_sftp_backups_account` checks, in order:
//! - **every refusal leaves sshd untouched:** an `sshd_config` without the
//!   drop-ins' `Include`, a chroot folder others may write, a taken uid, a
//!   drop-in that exists, an earlier `Match all` that would win, a key file
//!   of two lines; after each, every file under /etc/ssh is byte-identical
//!   and there's no account;
//! - **the account reads:** `ls` lists the backups and `get` copies one,
//!   byte for byte, and the chroot shows nothing but the backups' folder;
//! - **the account changes nothing:** `put`, `rm`, `rename`, `mkdir`,
//!   `rmdir`, `ln -s`, `ln`, `chmod` and `chown` each fail, and every file's
//!   bytes, owner, mode and time are as before;
//! - **no shell, no TTY, no forwarding, no password:** a command prints no
//!   uid, `-tt` gets no PTY, `-W` and `-R` are refused, and the server
//!   offers only `publickey`;
//! - **uninstall** removes it all: `sshd -T` prints what it printed before
//!   the install, and the key no longer logs in.
//!
//! The container has no network: sshd and the client meet on its loopback.
//! Without Docker, `the_test_images_base_is_the_images_builder` keeps the
//! test image on the builder image the Dockerfiles pin (U14).

use core::time::Duration;
use std::path::{Path, PathBuf};

use fleet_testkit::docker::{Finished, docker};
use fleet_testkit::dockerfile::{from_image, only, stage};

/// The test-only image.
const IMAGE: &str = "afkfleet-sftp-test:local";
/// The container, the test's own.
const CONTAINER: &str = "afkfleet-sftp-test";
/// How long one quick call may take.
const QUICK: Duration = Duration::from_secs(60);
/// How long the image's build may take (apt downloads the first time).
const BUILD: Duration = Duration::from_secs(480);

/// The backups' folder inside the container, and its chroot.
const BACKUPS: &str = "/srv/afkfleet-backups/backups";
/// The two backups the account must read.
const ONE: &str = "afkfleet-backup-20261101T000000Z.db";
const TWO: &str = "afkfleet-backup-20261102T000000Z.db";

/// The repository's root.
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Removes the container when dropped, also when the test panics.
struct Container;

impl Drop for Container {
    fn drop(&mut self) {
        let _ = docker(&repo(), &["rm", "--force", CONTAINER], QUICK);
    }
}

/// `docker <args>`, which must succeed.
fn ok(args: &[&str], deadline: Duration) -> Finished {
    let finished = docker(&repo(), args, deadline).unwrap();
    assert!(
        finished.success,
        "docker {args:?} failed:\n{}\n{}",
        finished.stdout, finished.stderr
    );
    finished
}

/// `script` in the container as root, however it ends.
fn exec(script: &str) -> Finished {
    docker(&repo(), &["exec", CONTAINER, "sh", "-c", script], QUICK).unwrap()
}

/// `script` in the container as root, which must succeed; its stdout.
fn exec_ok(script: &str) -> String {
    let finished = exec(script);
    assert!(
        finished.success,
        "{script} failed:\n{}\n{}",
        finished.stdout, finished.stderr
    );
    finished.stdout
}

/// Every file under /etc/ssh with its hash, and whether the account
/// exists: what a refusal must leave as it was.
fn sshd_state() -> String {
    exec_ok(
        "cd /etc/ssh && find . -type f | sort | xargs sha256sum; \
         grep -c '^afkfleet-backup:' /etc/passwd || true",
    )
}

/// The backups' files: names, bytes, owners (by number), modes and times.
fn backups_state() -> String {
    exec_ok(&format!(
        "cd {BACKUPS} && ls -lan --time-style=full-iso && sha256sum {ONE} {TWO}"
    ))
}

/// ssh's options for every client call: the key, no prompt, no host check
/// (the container is thrown away).
const SSH: &str = "-i /root/pull -o BatchMode=yes -o StrictHostKeyChecking=no \
                   -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10";

/// One sftp batch as the account; how it ended.
fn sftp(batch: &str) -> Finished {
    exec(&format!(
        "printf '%s\\n' '{batch}' > /root/batch && sftp {SSH} -b /root/batch afkfleet-backup@127.0.0.1"
    ))
}

/// One refused install, run by `prepare` and undone by `undo`: exit 1, and
/// nothing under /etc/ssh changed.
fn refused(prepare: &str, undo: &str, reason: &str) {
    let before = sshd_state();
    exec_ok(prepare);
    let install = exec("sh /root/sftp-backups.sh install --key /root/pull.pub");
    exec_ok(undo);

    assert_eq!(install.code, Some(1), "{reason}: {}", install.stderr);
    assert!(
        install.stderr.contains(reason),
        "{reason}: {}",
        install.stderr
    );
    assert_eq!(sshd_state(), before, "{reason}: sshd's files changed");
}

#[test]
fn slow_sftp_backups_account() {
    let context = repo().join("crates/fleet-integration/tests/integration/sftp");
    let context = context.to_str().unwrap();
    ok(
        &[
            "build",
            "--tag",
            IMAGE,
            "--file",
            &format!("{context}/sshd.Dockerfile"),
            context,
        ],
        BUILD,
    );
    let _ = docker(&repo(), &["rm", "--force", CONTAINER], QUICK);
    let _container = Container;
    ok(
        &[
            "run",
            "--detach",
            "--network",
            "none",
            "--name",
            CONTAINER,
            IMAGE,
        ],
        QUICK,
    );
    ok(
        &[
            "cp",
            "deploy/host/sftp-backups.sh",
            &format!("{CONTAINER}:/root/sftp-backups.sh"),
        ],
        QUICK,
    );
    exec_ok(&format!(
        "set -e; \
         install -d -o root -g root -m 755 /srv/afkfleet-backups; \
         install -d -o 65532 -g 65532 -m 700 {BACKUPS}; \
         printf 'backup one' > {BACKUPS}/{ONE}; printf 'backup two' > {BACKUPS}/{TWO}; \
         printf 'partial' > {BACKUPS}/.{TWO}.partial; \
         chown 65532:65532 {BACKUPS}/*.db {BACKUPS}/.*.partial; chmod 600 {BACKUPS}/*.db {BACKUPS}/.*.partial; \
         ssh-keygen -q -t ed25519 -N '' -f /root/pull; \
         printf '#!/bin/sh\\nkill -HUP 1\\n' > /usr/local/sbin/systemctl; chmod 755 /usr/local/sbin/systemctl"
    ));
    let global_before = exec_ok("sshd -T");

    refused(
        "cp /etc/ssh/sshd_config /root/sshd_config && sed -i '/^Include/d' /etc/ssh/sshd_config",
        "cp /root/sshd_config /etc/ssh/sshd_config",
        "must include /etc/ssh/sshd_config.d/*.conf",
    );
    refused(
        "chmod 775 /srv/afkfleet-backups",
        "chmod 755 /srv/afkfleet-backups",
        "writable by root alone",
    );
    refused(
        "useradd --uid 65532 --no-create-home someone",
        "userdel someone",
        "is taken",
    );
    refused(
        "touch /etc/ssh/sshd_config.d/10-afkfleet-backup.conf",
        "rm /etc/ssh/sshd_config.d/10-afkfleet-backup.conf",
        "exists already",
    );
    refused(
        "printf 'Match all\\n\\tAllowTcpForwarding yes\\n' > /etc/ssh/sshd_config.d/05-earlier.conf",
        "rm /etc/ssh/sshd_config.d/05-earlier.conf",
        "an earlier setting wins",
    );
    refused(
        "cp /root/pull.pub /root/one.pub && cat /root/one.pub >> /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "one public key",
    );

    exec_ok("sh /root/sftp-backups.sh install --key /root/pull.pub");
    let files_before = backups_state();

    let listed = sftp("ls -la");
    let got = sftp(&format!("get {ONE} /root/got"));
    let root = sftp("ls /");
    let outside = sftp("get /etc/passwd /root/passwd");
    assert!(listed.success, "{}", listed.stderr);
    assert!(
        listed.stdout.contains(ONE) && listed.stdout.contains(TWO),
        "{}",
        listed.stdout
    );
    assert!(got.success, "{}", got.stderr);
    assert_eq!(exec_ok("cat /root/got"), "backup one");
    assert!(root.success, "{}", root.stderr);
    assert!(
        root.stdout
            .lines()
            .filter(|line| !line.starts_with("sftp>"))
            .all(|line| line.trim() == "/backups"),
        "{}",
        root.stdout
    );
    assert!(!outside.success, "the chroot let /etc/passwd out");

    for change in [
        format!("put /etc/hostname {BACKUPS}/new"),
        "put /etc/hostname new".to_owned(),
        format!("rm {ONE}"),
        format!("rename {ONE} moved.db"),
        "mkdir folder".to_owned(),
        "rmdir /backups".to_owned(),
        format!("ln -s {ONE} link"),
        format!("ln {ONE} hard"),
        format!("chmod 644 {ONE}"),
        format!("chown 0 {ONE}"),
    ] {
        let tried = sftp(&change);
        assert!(!tried.success, "`{change}` worked");
    }
    assert_eq!(
        backups_state(),
        files_before,
        "a refused change changed something"
    );

    let shell = exec(&format!("ssh {SSH} afkfleet-backup@127.0.0.1 id"));
    assert!(
        !shell.stdout.contains("uid="),
        "a command ran: {}",
        shell.stdout
    );
    let tty = exec(&format!("ssh {SSH} -tt afkfleet-backup@127.0.0.1 id"));
    assert!(
        tty.stderr.contains("PTY allocation request failed"),
        "{}",
        tty.stderr
    );
    let local = exec(&format!(
        "ssh {SSH} -W 127.0.0.1:22 afkfleet-backup@127.0.0.1 < /dev/null"
    ));
    assert!(!local.success, "-W forwarded: {}", local.stderr);
    let remote = exec(&format!(
        "timeout 20 ssh {SSH} -N -o ExitOnForwardFailure=yes -R 0:127.0.0.1:22 afkfleet-backup@127.0.0.1"
    ));
    assert!(!remote.success, "-R forwarded");
    assert!(
        remote.stderr.contains("forwarding failed"),
        "{}",
        remote.stderr
    );
    let methods = exec(&format!(
        "ssh {SSH} -v -o PubkeyAuthentication=no afkfleet-backup@127.0.0.1 true"
    ));
    assert!(!methods.success);
    assert!(
        methods
            .stderr
            .contains("Authentications that can continue: publickey\n")
            || methods
                .stderr
                .contains("Authentications that can continue: publickey\r\n"),
        "{}",
        methods.stderr
    );

    exec_ok("sh /root/sftp-backups.sh uninstall");

    assert_eq!(exec_ok("sshd -T"), global_before);
    assert_eq!(
        exec_ok("grep -c '^afkfleet-backup:' /etc/passwd || true").trim(),
        "0"
    );
    assert!(!sftp("ls").success, "the key still logs in");
    assert_eq!(
        backups_state(),
        files_before,
        "uninstall touched the backups"
    );
}

/// The test image's base is the builder image both Dockerfiles pin, so the
/// test brings no image of its own to pin and scan (U14).
#[test]
fn the_test_images_base_is_the_images_builder() {
    let server = include_str!("../../../../../deploy/docker/server.Dockerfile");
    let chef = stage(server, "chef").unwrap();
    let pinned = from_image(chef.first().unwrap()).unwrap();
    let test = only(include_str!("sshd.Dockerfile"), "FROM").unwrap();

    assert_eq!(from_image(&test), Some(pinned));
}
