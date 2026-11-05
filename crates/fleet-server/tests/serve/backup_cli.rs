//! `afkfleet-server backup` (Plan.md P12.3; ADR-0022, S5), through the real
//! binary: one backup into `[database] backup_dir`, its path alone on
//! stdout, and nothing written when it can't be.

use std::path::{Path, PathBuf};

use fleet_server::cli::backup::EXIT_CODES;

use crate::support::{
    closed_port, config_body, finish, path_arg, test_dir, toml_path, write_config,
};

/// `dir`'s config with `[database]`'s `extra` keys after its path.
fn config_with(dir: &Path, extra: &str) -> PathBuf {
    let body = config_body(dir, closed_port()).replace("[log]", &format!("{extra}[log]"));
    write_config(dir, &body)
}

/// `dir`'s config with `backup_dir` set to `dir/backups`, which it creates,
/// and `extra` keys after it.
fn with_backups(dir: &Path, extra: &str) -> PathBuf {
    let backups = dir.join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    config_with(
        dir,
        &format!("backup_dir = {}\n{extra}", toml_path(&backups)),
    )
}

/// Migrates the database `config` names.
fn migrated(config: &Path) {
    let finished = finish(&["migrate", "--config", path_arg(config)]);
    assert_eq!(finished.status.code(), Some(0), "{}", finished.stderr);
}

/// The names in `dir`.
fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn backup_s_help_lists_its_exit_codes() {
    let finished = finish(&["backup", "--help"]);

    assert_eq!(finished.status.code(), Some(0));
    assert!(finished.stdout.contains(EXIT_CODES), "{}", finished.stdout);
}

#[test]
fn backup_writes_one_backup_and_prints_only_its_path() {
    let dir = test_dir("backup_once");
    let config = with_backups(&dir, "");
    migrated(&config);

    let finished = finish(&["backup", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(0), "{}", finished.stderr);
    let printed: Vec<&str> = finished.stdout.lines().collect();
    assert_eq!(printed.len(), 1, "{}", finished.stdout);
    let file = PathBuf::from(printed[0]);
    assert!(file.is_absolute());
    assert_eq!(file.parent().unwrap(), dir.join("backups"));
    assert!(file.is_file());
    let name = file.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("afkfleet-backup-")
            && file.extension() == Some(std::ffi::OsStr::new("db")),
        "{name}"
    );
    assert_eq!(names(&dir.join("backups")), [name]);
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: backup written, "),
        "{}",
        finished.stderr
    );
}

#[test]
fn backup_without_backup_dir_writes_nothing() {
    let dir = test_dir("backup_off");
    let config = config_with(&dir, "");
    migrated(&config);

    let finished = finish(&["backup", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: backups are off: [database] backup_dir isn't set"),
        "{}",
        finished.stderr
    );
}

#[test]
fn backup_never_creates_the_database() {
    let dir = test_dir("backup_no_database");
    let config = with_backups(&dir, "");

    let finished = finish(&["backup", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: the database can't be backed up: "),
        "{}",
        finished.stderr
    );
    assert!(!dir.join("afkfleet.db").exists());
    assert_eq!(names(&dir.join("backups")), [] as [&str; 0]);
}

#[test]
fn backup_into_a_folder_that_isnt_there_writes_nothing() {
    let dir = test_dir("backup_no_folder");
    let config = with_backups(&dir, "");
    migrated(&config);
    std::fs::remove_dir(dir.join("backups")).unwrap();

    let finished = finish(&["backup", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(1));
    assert_eq!(finished.stdout, "");
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: the backups' folder can't be used: "),
        "{}",
        finished.stderr
    );
    assert!(!dir.join("backups").exists());
}

/// An older backup another process holds open can't be removed on Windows,
/// so the prune fails: the new backup stays, its path on stdout, exit 3.
#[cfg(windows)]
#[test]
fn a_backup_whose_prune_fails_exits_3_and_still_prints_its_path() {
    use std::os::windows::fs::OpenOptionsExt as _;

    let dir = test_dir("backup_unpruned");
    let config = with_backups(&dir, "backup_keep = 1\n");
    migrated(&config);
    let older = dir
        .join("backups")
        .join("afkfleet-backup-20200101T000000Z.db");
    std::fs::write(&older, "").unwrap();
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&older)
        .unwrap();

    let finished = finish(&["backup", "--config", path_arg(&config)]);

    assert_eq!(finished.status.code(), Some(3), "{}", finished.stderr);
    let file = PathBuf::from(finished.stdout.trim_end());
    assert!(file.is_file());
    assert!(older.exists());
    assert!(
        finished
            .stderr
            .contains("afkfleet-server: removing older backups failed: "),
        "{}",
        finished.stderr
    );
}
