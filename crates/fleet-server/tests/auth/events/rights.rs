//! The rights a socket judges its events on (ADR-0020, S30, S41, R18):
//! one shared snapshot, read again only for a commit count it predates, by
//! one read at a time, tagged with the count from before its read.

use fleet_server::ports::store::StoreError;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use super::super::support::setup;
use super::{PATIENCE, commit_anything};

#[sqlx::test(migrations = false)]
async fn a_snapshot_new_enough_is_shared_without_another_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    let count = rights.count();

    let first = rights.at_least(count).await.unwrap();
    let again = rights.at_least(count).await.unwrap();
    let older = rights.at_least(0).await.unwrap();

    assert_eq!(first.at(), count);
    assert_eq!((again.at(), older.at()), (count, count));
    assert_eq!(setup.hooked.rights_reads(), 1);
}

#[sqlx::test(migrations = false)]
async fn twenty_askers_after_one_commit_share_one_rebuild(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    rights.at_least(rights.count()).await.unwrap();
    commit_anything(&setup).await;
    let count = rights.count();
    let mut held = setup.hooked.hold_next_rights_read();

    let askers: Vec<_> = (0..20)
        .map(|_| {
            let rights = rights.clone();
            tokio::spawn(async move { rights.at_least(count).await.unwrap().at() })
        })
        .collect();
    held.read(PATIENCE).await;
    held.release();
    let mut seen = Vec::new();
    for asker in askers {
        seen.push(
            tokio::time::timeout(PATIENCE, asker)
                .await
                .unwrap()
                .unwrap(),
        );
    }

    assert!(seen.iter().all(|at| *at >= count), "{seen:?}");
    assert_eq!(
        setup.hooked.rights_reads(),
        2,
        "one at the start, one after"
    );
}

#[sqlx::test(migrations = false)]
async fn a_snapshot_is_tagged_with_the_count_from_before_its_read(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    let before = rights.count();
    let mut held = setup.hooked.hold_next_rights_read();
    let reader = rights.clone();
    let reading = tokio::spawn(async move { reader.at_least(before).await.unwrap() });

    // The read has read the database; a commit lands after it.
    held.read(PATIENCE).await;
    commit_anything(&setup).await;
    held.release();
    let snapshot = tokio::time::timeout(PATIENCE, reading)
        .await
        .unwrap()
        .unwrap();
    let after = rights.count();
    let fresh = rights.at_least(after).await.unwrap();

    assert_eq!(snapshot.at(), before);
    assert!(after > before);
    assert!(fresh.at() >= after);
    assert_eq!(
        setup.hooked.rights_reads(),
        2,
        "the commit made it read again"
    );
}

#[sqlx::test(migrations = false)]
async fn a_read_that_fails_fails_the_ask_and_keeps_no_snapshot(
    _pool: SqlitePoolOptions,
    options: SqliteConnectOptions,
) {
    let setup = setup(options).await;
    let rights = setup.live_events.rights().clone();
    rights.at_least(rights.count()).await.unwrap();
    commit_anything(&setup).await;
    setup.hooked.fail_rights_reads();

    let failed = rights.at_least(rights.count()).await;
    let again = rights.at_least(rights.count()).await;

    assert!(matches!(failed, Err(StoreError::Busy)), "{failed:?}");
    assert!(matches!(again, Err(StoreError::Busy)), "{again:?}");
    assert_eq!(setup.hooked.rights_reads(), 3);
}
