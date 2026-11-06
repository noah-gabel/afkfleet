//! The backups' SFTP account, proven in a throwaway container (Plan.md
//! P12.2; ADR-0022, S15, U14): `deploy/host/sftp-backups.sh` installed into
//! a Debian 13 sshd built from the builder image the images pin, then the
//! account tried from the same container's ssh client with a key made
//! there and thrown away with it. `just test-stack` runs it, and so does
//! the Stack workflow; it never runs on a real host.
//!
//! `slow_sftp_backups_account` checks, in order:
//! - **every refusal leaves sshd untouched:** an `sshd_config` without the
//!   drop-ins' `Include`, a chroot folder others may write or root doesn't
//!   own, a backups' folder that isn't 65532's with mode 700, a taken name,
//!   uid, group name or gid, a drop-in that exists, an earlier `Match all`
//!   or `AllowUsers` that would win, a drop-in `sshd -t` fails, settings
//!   that change for everyone else, a key file of two lines, one ssh-keygen
//!   can't read or one with a quote or a backslash; after each, every file
//!   under /etc/ssh is byte-identical and there's no account;
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

/// Every file under /etc/ssh with its hash, and whether the account and its
/// group exist: what a refusal must leave as it was.
fn sshd_state() -> String {
    exec_ok(
        "cd /etc/ssh && find . -type f | sort | xargs sha256sum; \
         grep -c '^afkfleet-backup:' /etc/passwd || true; \
         grep -c '^afkfleet-backup:' /etc/group || true",
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

/// What runs after every refusal case, once its state is compared: whatever
/// a script that didn't refuse left (the drop-in, the key file, the account,
/// the group) goes, so the next case starts from the same host.
const RESET: &str = "rm -f /etc/ssh/sshd_config.d/10-afkfleet-backup.conf \
                     /etc/ssh/afkfleet-backup.authorized_keys; \
                     userdel afkfleet-backup 2>/dev/null; groupdel afkfleet-backup 2>/dev/null; true";

/// A refused install: its extra arguments, what prepares the host and what
/// undoes that, and a part of the reason it must give.
type Refusal = (&'static str, &'static str, &'static str, &'static str);

/// A stand-in for sshd, first on root's PATH, that calls the real one but
/// answers `sshd -t` with an error once the drop-in exists: a drop-in that
/// fails the check only after it was written.
const SSHD_FAILS_THE_DROPIN: &str = "printf '#!/bin/sh\\n\
    if [ \"$1\" = -t ] && [ -e /etc/ssh/sshd_config.d/10-afkfleet-backup.conf ]; \
    then echo bad >&2; exit 255; fi\\nexec /usr/sbin/sshd \"$@\"\\n' \
    > /usr/local/sbin/sshd && chmod 755 /usr/local/sbin/sshd";

/// A stand-in for sshd whose plain `sshd -T` prints one setting more once
/// the drop-in exists: a drop-in that would change everyone's settings.
const SSHD_CHANGES_FOR_EVERYONE: &str = "printf '#!/bin/sh\\n\
    if [ $# -eq 1 ] && [ \"$1\" = -T ] && [ -e /etc/ssh/sshd_config.d/10-afkfleet-backup.conf ]; \
    then /usr/sbin/sshd -T; echo maxsessions 99; exit 0; fi\\nexec /usr/sbin/sshd \"$@\"\\n' \
    > /usr/local/sbin/sshd && chmod 755 /usr/local/sbin/sshd";

/// Every refusal: each must exit 1 with its reason and leave sshd's files,
/// the accounts and the groups as they were.
const REFUSALS: [Refusal; 27] = [
    // A tool that's missing is found before anything is written, never
    // exit 127 halfway: groupadd, or anything to reload sshd with (Debian's
    // openssh-server brings a real systemctl beside the test's stub).
    (
        "",
        "mv /usr/sbin/groupadd /root/groupadd.off",
        "mv /root/groupadd.off /usr/sbin/groupadd",
        "groupadd",
    ),
    (
        "",
        "mv /usr/local/sbin/systemctl /root/systemctl.off && mv /usr/bin/systemctl /root/systemctl.real",
        "mv /root/systemctl.real /usr/bin/systemctl && mv /root/systemctl.off /usr/local/sbin/systemctl",
        "to reload sshd",
    ),
    // A step that fails after the first write: everything this run added
    // goes again (the drop-in, the key file, the group, the account). The
    // real systemctl fails here, with no systemd running.
    (
        "",
        "mv /usr/local/sbin/systemctl /root/systemctl.off",
        "mv /root/systemctl.off /usr/local/sbin/systemctl",
        "couldn't be reloaded",
    ),
    (
        "",
        "printf '#!/bin/sh\\nexit 1\\n' > /usr/local/sbin/useradd && chmod 755 /usr/local/sbin/useradd",
        "rm /usr/local/sbin/useradd",
        "useradd",
    ),
    (
        "",
        "cp /etc/ssh/sshd_config /root/sshd_config && sed -i '/^Include/d' /etc/ssh/sshd_config",
        "cp /root/sshd_config /etc/ssh/sshd_config",
        "must include /etc/ssh/sshd_config.d/*.conf",
    ),
    (
        "",
        "chmod 775 /srv/afkfleet-backups",
        "chmod 755 /srv/afkfleet-backups",
        "writable by root alone",
    ),
    // root_only's owner half: root's mode, another owner (PR #97's review).
    (
        "",
        "chown 65532 /srv/afkfleet-backups",
        "chown 0 /srv/afkfleet-backups",
        "must belong to root",
    ),
    // The backups' folder itself: the server's user's, owner-only.
    (
        "",
        "chmod 750 /srv/afkfleet-backups/backups",
        "chmod 700 /srv/afkfleet-backups/backups",
        "must belong to 65532:65532 with mode 700",
    ),
    (
        "",
        "chown 65532:0 /srv/afkfleet-backups/backups",
        "chown 65532:65532 /srv/afkfleet-backups/backups",
        "must belong to 65532:65532 with mode 700",
    ),
    // stat's %a prints 0002 as 2.
    (
        "",
        "chmod 0002 /srv/afkfleet-backups",
        "chmod 755 /srv/afkfleet-backups",
        "writable by root alone",
    ),
    (
        "",
        "useradd --uid 65532 --no-create-home someone",
        "userdel someone",
        "is taken",
    ),
    // taken's other halves: the name, the group's name, the gid. A script
    // that didn't refuse may have removed the prepared one itself, so the
    // undo tolerates its absence; the state comparison still sees the rest.
    (
        "",
        "useradd --uid 4000 --no-user-group --no-create-home afkfleet-backup",
        "userdel afkfleet-backup 2>/dev/null; true",
        "is taken",
    ),
    (
        "",
        "groupadd --gid 4001 afkfleet-backup",
        "groupdel afkfleet-backup 2>/dev/null; true",
        "is taken",
    ),
    (
        "",
        "groupadd --gid 65532 somegroup",
        "groupdel somegroup",
        "is taken",
    ),
    (
        "",
        "touch /etc/ssh/sshd_config.d/10-afkfleet-backup.conf",
        "rm /etc/ssh/sshd_config.d/10-afkfleet-backup.conf",
        "exists already",
    ),
    (
        "",
        "printf 'Match all\\n\\tAllowTcpForwarding yes\\n' > /etc/ssh/sshd_config.d/05-earlier.conf",
        "rm /etc/ssh/sshd_config.d/05-earlier.conf",
        "an earlier setting wins",
    ),
    // An earlier AllowUsers that leaves the account out: it could never log in.
    (
        "",
        "printf 'AllowUsers root\\n' > /etc/ssh/sshd_config.d/05-allow.conf",
        "rm /etc/ssh/sshd_config.d/05-allow.conf",
        "leaves the account out",
    ),
    // The checks after the drop-in is written: `sshd -t`, and sshd's
    // settings for everyone else unchanged.
    (
        "",
        SSHD_FAILS_THE_DROPIN,
        "rm /usr/local/sbin/sshd",
        "the drop-in fails sshd -t",
    ),
    (
        "",
        SSHD_CHANGES_FOR_EVERYONE,
        "rm /usr/local/sbin/sshd",
        "for everyone else",
    ),
    (
        "",
        "cp /root/pull.pub /root/one.pub && cat /root/one.pub >> /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "one public key",
    ),
    // An option would come through behind `restrict`.
    (
        "",
        "cp /root/pull.pub /root/one.pub && sed -i 's/^/cert-authority /' /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "one public key",
    ),
    // A line with the right type that isn't a key, and a key whose comment
    // holds a quote or a backslash (PR #97's review).
    (
        "",
        "cp /root/pull.pub /root/one.pub && printf 'ssh-ed25519 AAAAnotakey comment\\n' > /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "ssh-keygen can read",
    ),
    (
        "",
        "cp /root/pull.pub /root/one.pub && sed -i 's/$/ \"quoted\"/' /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "a character a key line can't",
    ),
    (
        "",
        "cp /root/pull.pub /root/one.pub && sed -i 's/$/ back\\\\slash/' /root/pull.pub",
        "cp /root/one.pub /root/pull.pub",
        "a character a key line can't",
    ),
    // A chroot of / (dirname //backups is /), and . and .. parts that lead
    // to the real folder.
    (
        "--backups //backups",
        "install -d -o 65532 -g 65532 -m 700 /backups",
        "rmdir /backups",
        "the backups' folder must be",
    ),
    (
        "--backups /srv/./afkfleet-backups/backups",
        "true",
        "true",
        "the backups' folder must be",
    ),
    (
        "--backups /srv/../srv/afkfleet-backups/backups",
        "true",
        "true",
        "the backups' folder must be",
    ),
];

/// One refused install: `None` when it refused as it must, else what went
/// wrong. The host's state is compared before [`RESET`] runs.
fn refused((args, prepare, undo, reason): Refusal) -> Option<String> {
    let before = sshd_state();
    exec_ok(prepare);
    let install = exec(&format!(
        "sh /root/sftp-backups.sh install --key /root/pull.pub {args}"
    ));
    exec_ok(undo);
    let after = sshd_state();
    exec_ok(RESET);

    let mut problems = Vec::new();
    if install.code != Some(1) {
        problems.push(format!("exit {:?}", install.code));
    }
    if !install.stderr.contains(reason) {
        problems.push(format!("stderr: {}", install.stderr.trim()));
    }
    if after != before {
        problems.push("the host changed".to_owned());
    }
    (!problems.is_empty()).then(|| format!("[{args}] {reason}: {}", problems.join("; ")))
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

    // Checked at the end, with the stuck uninstall's, so a run shows them all.
    let mut failed: Vec<String> = REFUSALS.into_iter().filter_map(refused).collect();

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

    // An uninstall whose userdel fails (the account logged in, say) says
    // so and removes nothing, rather than half of it.
    let installed = sshd_state();
    exec_ok(
        "printf '#!/bin/sh\\necho \"userdel: user afkfleet-backup is currently used\" >&2\\nexit 8\\n' \
         > /usr/local/sbin/userdel && chmod 755 /usr/local/sbin/userdel",
    );
    let stuck = exec("sh /root/sftp-backups.sh uninstall");
    exec_ok("rm /usr/local/sbin/userdel");
    if stuck.code != Some(1) || !stuck.stderr.contains("userdel") {
        failed.push(format!(
            "uninstall with a failing userdel: exit {:?}: {}",
            stuck.code,
            stuck.stderr.trim()
        ));
    }
    if sshd_state() != installed {
        failed.push("uninstall with a failing userdel removed a part".to_owned());
    }

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
    assert!(failed.is_empty(), "{failed:#?}");
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
