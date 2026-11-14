//! Transactions: all or nothing, across drops, reopens and a killed process (WP-006 AC2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use common::{Fixture, ROOT, file, key, mount_id};
use mydrive_store::{Queries, Store};

/// Set in the child process of `ac2_killed_before_commit_leaves_nothing`.
const CHILD_ENV: &str = "MYDRIVE_STORE_CRASH_CHILD_DB";
const BATCH: usize = 3000;

/// One delta page: many items plus the token that follows it.
fn apply_page(tx: &impl Queries, prefix: &str, token: &str) {
    // Long names make the batch larger than SQLite's page cache, so uncommitted pages are
    // spilled into the WAL file before the transaction ends.
    let padding = "x".repeat(1000);
    for i in 0..BATCH {
        tx.upsert_item(&file(
            &format!("{prefix}{i}"),
            Some(ROOT),
            &format!("{prefix}{i}{padding}"),
            10,
        ))
        .unwrap();
    }
    tx.set_delta_token(&mount_id(), Some(token)).unwrap();
}

fn count_prefix(store: &Store, prefix: &str) -> usize {
    store
        .children(&key(ROOT))
        .unwrap()
        .iter()
        .filter(|i| i.key.item_id.as_str().starts_with(prefix))
        .count()
}

#[test]
fn ac2_committed_batch_is_fully_visible_after_reopen() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let tx = store.transaction().unwrap();
    apply_page(&tx, "c", "token-1");
    tx.commit().unwrap();
    assert_eq!(count_prefix(&store, "c"), BATCH);
    drop(store);

    let store = fx.open();
    assert_eq!(count_prefix(&store, "c"), BATCH);
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("token-1")
    );
}

#[test]
fn ac2_dropped_transaction_leaves_nothing() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    store.set_delta_token(&mount_id(), Some("token-0")).unwrap();
    {
        let tx = store.transaction().unwrap();
        apply_page(&tx, "d", "token-1");
        // Visible inside the transaction...
        assert!(tx.item(&key("d0")).unwrap().is_some());
        // ...and dropped without commit.
    }
    assert_eq!(count_prefix(&store, "d"), 0);
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("token-0")
    );
    drop(store);

    let store = fx.open();
    assert_eq!(count_prefix(&store, "d"), 0);
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("token-0")
    );
}

#[test]
fn ac2_explicit_rollback_leaves_nothing() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let tx = store.transaction().unwrap();
    tx.upsert_item(&file("r", Some(ROOT), "r", 1)).unwrap();
    tx.rollback().unwrap();
    assert_eq!(store.item(&key("r")).unwrap(), None);
}

/// A failed operation inside a transaction leaves no partial effect, and the rest of the
/// transaction still commits.
#[test]
fn failed_operation_inside_a_transaction_does_not_poison_it() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let tx = store.transaction().unwrap();
    tx.upsert_item(&file("ok", Some(ROOT), "ok", 1)).unwrap();
    assert!(tx.upsert_item(&file("bad", Some(ROOT), "a/b", 1)).is_err());
    assert!(
        tx.delete_item(&key(ROOT), mydrive_store::DeleteMode::ItemOnly)
            .is_err()
    );
    tx.commit().unwrap();
    assert!(store.item(&key("ok")).unwrap().is_some());
    assert!(store.item(&key("bad")).unwrap().is_none());
    assert!(store.item(&key(ROOT)).unwrap().is_some());
}

/// WP-006 AC2: a process killed before commit leaves none of the batch; what it committed
/// earlier survives. The child is this test binary re-run with `CHILD_ENV` set; it is
/// killed with SIGKILL: no destructors, no rollback and no clean close.
#[test]
fn ac2_killed_before_commit_leaves_nothing() {
    if let Ok(db) = std::env::var(CHILD_ENV) {
        crash_child(Path::new(&db));
    }

    let fx = Fixture::new();
    drop(fx.open_with_mount());
    let marker = fx.dir.path().join("reached-abort");

    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ac2_killed_before_commit_leaves_nothing",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, fx.path())
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "the child must be killed, not exit: {status}"
    );
    assert!(
        marker.exists(),
        "the child reached the point of abort with the batch applied"
    );
    assert!(
        fx.dir.path().join("state.db-wal").exists(),
        "the uncommitted batch reached the WAL file, so recovery is really exercised"
    );

    let store = fx.open();
    assert_eq!(count_prefix(&store, "committed"), BATCH);
    assert_eq!(count_prefix(&store, "lost"), 0);
    assert_eq!(
        store.delta_token(&mount_id()).unwrap().as_deref(),
        Some("token-committed")
    );
}

fn crash_child(db: &Path) -> ! {
    let mut store = Store::open(db).unwrap();
    let tx = store.transaction().unwrap();
    apply_page(&tx, "committed", "token-committed");
    tx.commit().unwrap();

    let tx = store.transaction().unwrap();
    apply_page(&tx, "lost", "token-lost");
    std::fs::write(db.with_file_name("reached-abort"), b"").unwrap();
    // SIGKILL rather than abort(), so no core dump is written.
    let _ = Command::new("kill")
        .args(["-KILL", &std::process::id().to_string()])
        .status();
    // Not reached if the kill worked; exiting normally makes the parent's check fail.
    std::process::exit(0);
}
