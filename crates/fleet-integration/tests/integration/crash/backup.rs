//! A kill during a backup (Plan.md P13.5; ADR-0022, ADR-0033): the
//! `backup` command, whose steps are the ones serve's own schedule runs,
//! killed while its file is being written. What it leaves is the temporary
//! dot file, which never counts as a backup: the next one is due at once,
//! finishes, and passes what `restore` checks; a day later a backup removes
//! what the kill left.

use core::time::Duration;

use fleet_server::infra::sqlite::backup::list;
use tokio_util::sync::CancellationToken;

use super::child::Trip;
use super::{after_the_fault, assert_restorable, assert_sound, back_up_in_child};
use crate::harness::server::{BACKUPS, ServerOptions};
use crate::harness::world::World;

/// The end of a backup's name while it's being written
/// (`.afkfleet-backup-<stamp>.db.partial`).
const PARTIAL: &str = ".partial";

#[tokio::test]
async fn a_backup_killed_while_it_s_written_never_counts_and_the_next_one_finishes() {
    let options = ServerOptions {
        backups: true,
        ..ServerOptions::default()
    };
    let mut world = World::new(options).await;
    world.owner.bot("AfkBot1").await;
    let backups = world.server.config().database.path.with_file_name(BACKUPS);
    world.server.stop().await;
    let mut child = back_up_in_child(
        &world,
        options,
        Trip::Appears {
            dir: backups.clone(),
            suffix: PARTIAL.to_owned(),
        },
    )
    .await;

    // The temporary file is there, made empty before the copy starts: the
    // kill lands while the copy, its sync or its check runs.
    child.says("TRIPPED").await;
    let finished = child.said("FINISHED");
    child.kill(&world);

    assert!(
        !finished,
        "the backup finished before its kill: this machine wrote it within the kill's own delay"
    );
    let left = list(&backups).await.unwrap();
    assert_eq!(left.finished, [], "a killed backup was published");
    assert_eq!(left.partial.len(), 1, "{left:?}");
    assert_sound(&world.server.config().database).await;
    // serve's own check of the folder takes it as it is.
    world.server.start_again().await;
    let service = world.server.backups();
    let due = service.due().await.unwrap();
    assert!(due.now, "a half-written backup doesn't count");
    assert_eq!(due.newest, None);

    // The next backup finishes the job.
    let taken = service.take(&CancellationToken::new()).await.unwrap();

    assert_eq!(taken.prune_failed.as_ref().map(ToString::to_string), None);
    let now = list(&backups).await.unwrap();
    assert_eq!(now.finished, [taken.written.name]);
    assert_eq!(now.partial, left.partial, "it's kept for a day, for a look");
    assert_restorable(world.server.config(), &taken.written.file).await;
    assert!(!service.is_due().await.unwrap());
    // A day later, a backup removes what the kill left, its journal too.
    world.server.clock().advance(Duration::from_hours(25));
    let later = service.take(&CancellationToken::new()).await.unwrap();
    let cleaned = list(&backups).await.unwrap();
    assert_eq!(cleaned.partial, []);
    assert_eq!(cleaned.partial_journals, []);
    assert_eq!(cleaned.finished.len(), 2);
    assert_ne!(later.written.name, taken.written.name);
    after_the_fault(&mut world).await;
    world.end(Vec::new()).await;
}
