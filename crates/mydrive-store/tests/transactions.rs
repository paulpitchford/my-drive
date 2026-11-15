//! Transactions: all or nothing, across drops, reopens and a killed process (WP-006 AC2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use common::{Fixture, ROOT, file, key, kill_self, mount_id, raw, run_child_until_killed};
use mydrive_store::{
    ByteRange, DeleteMode, DeltaRunKind, PageLink, PendingChanges, Queries, Store,
};

/// The store with its mount in an initial delta run, so a batch can record a next link.
fn open_running(fx: &Fixture) -> Store {
    let store = fx.open_with_mount();
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
        .unwrap();
    store
}

/// The mount's recorded next link.
fn next_link(store: &Store) -> Option<String> {
    store.mount(&mount_id()).unwrap().unwrap().delta_next_link
}

/// Set in the child process of `ac2_killed_at_any_point_leaves_exactly_the_committed_state`:
/// `<db path>|<kill point>`.
const CHILD_ENV: &str = "MYDRIVE_STORE_CRASH_CHILD";
const BATCH: usize = 3000;

/// Part of one delta page: items `prefix{from..to}`, with hydrated ranges on every tenth.
fn apply_items(tx: &impl Queries, prefix: &str, from: usize, to: usize) {
    // Long names make the batch larger than SQLite's page cache, so uncommitted pages are
    // spilled into the WAL file before the transaction ends.
    let padding = "x".repeat(1000);
    for i in from..to {
        let item = file(
            &format!("{prefix}{i}"),
            Some(ROOT),
            &format!("{prefix}{i}{padding}"),
            100,
        );
        tx.insert_item(&item).unwrap();
        if i % 10 == 0 {
            tx.record_hydrated_range(&item.key, ByteRange::new(0, 30).unwrap())
                .unwrap();
            tx.record_hydrated_range(&item.key, ByteRange::new(30, 60).unwrap())
                .unwrap();
            tx.record_hydrated_range(&item.key, ByteRange::new(80, 100).unwrap())
                .unwrap();
        }
    }
}

fn apply_page(tx: &impl Queries, prefix: &str, token: &str) {
    apply_items(tx, prefix, 0, BATCH);
    tx.stage_delta_page(&mount_id(), &[], &PageLink::Next(token.to_owned()))
        .unwrap();
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
    let mut store = open_running(&fx);
    let tx = store.transaction().unwrap();
    apply_page(&tx, "c", "token-1");
    tx.commit().unwrap();
    assert_eq!(count_prefix(&store, "c"), BATCH);
    drop(store);

    let store = fx.open();
    assert_eq!(count_prefix(&store, "c"), BATCH);
    assert_eq!(next_link(&store).as_deref(), Some("token-1"));
}

#[test]
fn ac2_dropped_transaction_leaves_nothing() {
    let fx = Fixture::new();
    let mut store = open_running(&fx);
    store
        .stage_delta_page(&mount_id(), &[], &PageLink::Next("token-0".to_owned()))
        .unwrap();
    {
        let tx = store.transaction().unwrap();
        apply_page(&tx, "d", "token-1");
        // Visible inside the transaction...
        assert!(tx.item(&key("d0")).unwrap().is_some());
        // ...and dropped without commit.
    }
    assert_eq!(count_prefix(&store, "d"), 0);
    assert_eq!(next_link(&store).as_deref(), Some("token-0"));
    drop(store);

    let store = fx.open();
    assert_eq!(count_prefix(&store, "d"), 0);
    assert_eq!(next_link(&store).as_deref(), Some("token-0"));
}

#[test]
fn ac2_explicit_rollback_leaves_nothing() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let tx = store.transaction().unwrap();
    tx.insert_item(&file("r", Some(ROOT), "r", 1)).unwrap();
    tx.rollback().unwrap();
    assert_eq!(store.item(&key("r")).unwrap(), None);
}

/// An operation the store refuses (a constraint, a missing item) leaves no partial effect
/// and leaves the transaction usable, so the rest of it can still commit. This is not true
/// after SQLite rolls the transaction back itself; see the `TransactionAborted` tests in
/// the crate's unit tests.
#[test]
fn refused_operation_inside_a_transaction_leaves_it_usable() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let tx = store.transaction().unwrap();
    tx.insert_item(&file("ok", Some(ROOT), "ok", 1)).unwrap();
    assert!(tx.insert_item(&file("bad", Some(ROOT), "a/b", 1)).is_err());
    assert!(
        tx.delete_item(&key(ROOT), DeleteMode::ItemOnly, PendingChanges::Refuse)
            .is_err()
    );
    tx.commit().unwrap();
    assert!(store.item(&key("ok")).unwrap().is_some());
    assert!(store.item(&key("bad")).unwrap().is_none());
    assert!(store.item(&key(ROOT)).unwrap().is_some());
}

const KILL_POINTS: [&str; 4] = ["after-begin", "mid-batch", "before-commit", "after-commit"];

/// WP-006 AC2: a process killed at any point leaves exactly what it had committed: the
/// child commits batch 1, then is killed with SIGKILL right after beginning batch 2,
/// half way through it, just before its commit, or just after it. After each, the store
/// reopens with `PRAGMA integrity_check` clean and every hydrated range merged and within
/// its item.
#[test]
fn ac2_killed_at_any_point_leaves_exactly_the_committed_state() {
    if let Ok(arg) = std::env::var(CHILD_ENV) {
        let (db, point) = arg.split_once('|').unwrap();
        crash_child(Path::new(db), point);
    }

    for point in KILL_POINTS {
        let fx = Fixture::new();
        drop(open_running(&fx));
        run_child_until_killed(
            "ac2_killed_at_any_point_leaves_exactly_the_committed_state",
            CHILD_ENV,
            &format!("{}|{point}", fx.path().display()),
        );
        assert!(
            fx.dir.path().join(format!("reached-{point}")).exists(),
            "{point}: the child reached the kill point"
        );
        if point != "after-begin" {
            assert!(
                fx.dir.path().join("state.db-wal").exists(),
                "{point}: batch 2 reached the WAL file, so recovery is really exercised"
            );
        }

        let integrity: String = raw(&fx.path())
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "{point}");

        let store = fx.open();
        let second_committed = point == "after-commit";
        assert_eq!(count_prefix(&store, "first"), BATCH, "{point}");
        assert_eq!(
            count_prefix(&store, "second"),
            if second_committed { BATCH } else { 0 },
            "{point}"
        );
        assert_eq!(
            next_link(&store).as_deref(),
            Some(if second_committed {
                "token-second"
            } else {
                "token-first"
            }),
            "{point}"
        );
        // Every item with ranges recorded touching pieces holds exactly the merged form.
        assert_eq!(
            assert_ranges_merged_and_in_bounds(&store),
            [vec![
                ByteRange::new(0, 60).unwrap(),
                ByteRange::new(80, 100).unwrap()
            ]],
            "{point}"
        );
    }
}

/// Set in the child of `killed_during_a_local_change_and_upload_never_loses_the_dirty_mark`.
const UPLOAD_CHILD_ENV: &str = "MYDRIVE_STORE_UPLOAD_CRASH_CHILD";

const UPLOAD_KILL_POINTS: [&str; 5] = [
    "mid-mark",
    "after-mark",
    "after-write",
    "mid-complete",
    "after-complete",
];

fn hash(b: u8) -> mydrive_core::hash::QuickXorHash {
    mydrive_core::hash::QuickXorHash::from_bytes([b; 20])
}

/// Second review: a process killed anywhere in mark_local_change, local writes and
/// complete_upload never leaves an item that is clean while its local bytes differ from
/// the server. Only a committed `complete_upload` clears the dirty mark. After each kill
/// the store passes `PRAGMA integrity_check` and its ranges are merged and in bounds.
#[test]
fn killed_during_a_local_change_and_upload_never_loses_the_dirty_mark() {
    use mydrive_core::state::{Hydration, Sync as SyncState};

    if let Ok(arg) = std::env::var(UPLOAD_CHILD_ENV) {
        let (db, point) = arg.split_once('|').unwrap();
        upload_crash_child(Path::new(db), point);
    }

    for point in UPLOAD_KILL_POINTS {
        let fx = Fixture::new();
        {
            let store = fx.open_with_mount();
            let mut item = file("f", Some(ROOT), "f", 100);
            item.server_hash = Some(hash(1));
            item.state.hydration = Hydration::Full;
            store.insert_item(&item).unwrap();
            store
                .record_hydrated_range(&item.key, ByteRange::new(0, 100).unwrap())
                .unwrap();
        }
        run_child_until_killed(
            "killed_during_a_local_change_and_upload_never_loses_the_dirty_mark",
            UPLOAD_CHILD_ENV,
            &format!("{}|{point}", fx.path().display()),
        );
        assert!(
            fx.dir.path().join(format!("reached-{point}")).exists(),
            "{point}: the child reached the kill point"
        );
        let integrity: String = raw(&fx.path())
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "{point}");

        let store = fx.open();
        let item = store.item(&key("f")).unwrap().unwrap();
        let ranges = store.hydrated_ranges(&key("f")).unwrap();
        let got = (
            item.local_dirty,
            item.local_generation,
            item.state.sync,
            item.size,
            item.etag.as_deref(),
            ranges.clone(),
        );
        let r = |s, e| ByteRange::new(s, e).unwrap();
        let expected = match point {
            // The mark never committed, so nothing was written to the backing file.
            "mid-mark" => (
                false,
                0,
                SyncState::InSync,
                100,
                Some("etag-f"),
                vec![r(0, 100)],
            ),
            "after-mark" => (
                true,
                1,
                SyncState::PendingUpload,
                100,
                Some("etag-f"),
                vec![r(0, 100)],
            ),
            "after-write" => (
                true,
                2,
                SyncState::PendingUpload,
                150,
                Some("etag-f"),
                vec![r(0, 150)],
            ),
            "mid-complete" => (
                true,
                2,
                SyncState::Uploading,
                150,
                Some("etag-f"),
                vec![r(0, 150)],
            ),
            "after-complete" => (
                false,
                2,
                SyncState::InSync,
                150,
                Some("etag-up"),
                vec![r(0, 150)],
            ),
            other => panic!("{other}"),
        };
        assert_eq!(got, expected, "{point}");
        if point == "after-complete" {
            assert_eq!(item.local_hash, Some(hash(9)));
        }
        // The restart-recovery listing finds the item exactly when it is dirty (F3).
        let recovered: Vec<_> = store
            .dirty_items()
            .unwrap()
            .into_iter()
            .map(|i| i.key)
            .collect();
        let want: &[mydrive_core::ids::ItemKey] = if item.local_dirty { &[key("f")] } else { &[] };
        assert_eq!(recovered, want, "{point}");
        assert_ranges_merged_and_in_bounds(&store);
    }
}

fn upload_crash_child(db: &Path, point: &str) -> ! {
    use mydrive_core::state::Sync as SyncState;

    let reached = |p: &str| {
        std::fs::write(db.with_file_name(format!("reached-{p}")), b"").unwrap();
    };
    let kill_at = |p: &str| {
        if p == point {
            reached(p);
            kill_self();
        }
    };
    let mut store = Store::open(db).unwrap();
    let k = key("f");

    let tx = store.transaction().unwrap();
    // The mark itself moves the item to pending-upload (F3); no separate set_sync.
    tx.mark_local_change(&k).unwrap();
    kill_at("mid-mark");
    tx.commit().unwrap();
    kill_at("after-mark");

    // The write itself goes to the backing file; the store records its effect.
    let tx = store.transaction().unwrap();
    // Growing a fully local file records the new bytes itself.
    let generation = tx.set_local_size(&k, 150).unwrap();
    tx.commit().unwrap();
    kill_at("after-write");

    store
        .set_sync(&k, SyncState::PendingUpload, SyncState::Uploading)
        .unwrap();
    let item = store.item(&k).unwrap().unwrap();
    let response = mydrive_store::RemoteMetadata {
        etag: Some("etag-up".to_owned()),
        server_hash: Some(hash(9)),
        size: 150,
        ..common::remote(&item)
    };
    let tx = store.transaction().unwrap();
    tx.complete_upload(&k, generation, hash(9), &response, None)
        .unwrap();
    kill_at("mid-complete");
    tx.commit().unwrap();
    reached("after-complete");
    kill_self();
}

/// Every file under the root has merged, in-bounds ranges. Returns the distinct range
/// lists seen, for callers that expect a particular shape.
fn assert_ranges_merged_and_in_bounds(store: &Store) -> Vec<Vec<ByteRange>> {
    let mut seen: Vec<Vec<ByteRange>> = Vec::new();
    for item in store.children(&key(ROOT)).unwrap() {
        let ranges = store.hydrated_ranges(&item.key).unwrap();
        for pair in ranges.windows(2) {
            assert!(pair[0].end() < pair[1].start(), "{:?} not merged", item.key);
        }
        if let Some(last) = ranges.last() {
            assert!(last.end() <= item.size, "{:?} out of bounds", item.key);
        }
        if !ranges.is_empty() && !seen.contains(&ranges) {
            seen.push(ranges);
        }
    }
    seen
}

fn crash_child(db: &Path, point: &str) -> ! {
    let reached = |p: &str| std::fs::write(db.with_file_name(format!("reached-{p}")), b"").unwrap();
    let mut store = Store::open(db).unwrap();
    let tx = store.transaction().unwrap();
    apply_page(&tx, "first", "token-first");
    tx.commit().unwrap();

    let tx = store.transaction().unwrap();
    if point == "after-begin" {
        reached(point);
        kill_self();
    }
    apply_items(&tx, "second", 0, BATCH / 2);
    if point == "mid-batch" {
        reached(point);
        kill_self();
    }
    apply_items(&tx, "second", BATCH / 2, BATCH);
    tx.stage_delta_page(&mount_id(), &[], &PageLink::Next("token-second".to_owned()))
        .unwrap();
    if point == "before-commit" {
        reached(point);
        kill_self();
    }
    tx.commit().unwrap();
    reached(point);
    kill_self();
}

/// Set in the child of `adr_0022_killed_anywhere_in_a_delta_run_leaves_it_in_progress_or_finished`.
const RUN_CHILD_ENV: &str = "MYDRIVE_STORE_RUN_CRASH_CHILD";
/// Entries per staged page; long names push the pages past SQLite's cache into the WAL.
const RUN_PAGE: usize = 1500;

const RUN_KILL_POINTS: [&str; 10] = [
    "mid-page",
    "after-page",
    "mid-last-page",
    "after-last-page",
    "mid-apply",
    "after-apply",
    "mid-finish",
    "after-finish",
    "mid-remove",
    "after-remove",
];

fn run_page(prefix: &str) -> Vec<mydrive_store::StagedEntry> {
    let padding = "y".repeat(800);
    (0..RUN_PAGE)
        .map(|i| mydrive_store::StagedEntry {
            item_id: mydrive_core::ids::ItemId::new(format!("{prefix}{i}")).unwrap(),
            is_root: false,
            change: mydrive_store::StagedChange::Present(mydrive_store::StagedItem {
                parent_item_id: Some(mydrive_core::ids::ItemId::new(ROOT).unwrap()),
                name: format!("{prefix}{i}{padding}"),
                facet: mydrive_store::StagedFacet::File,
                size: 7,
                created: mydrive_store::Timestamp::from_unix_seconds(1),
                modified: mydrive_store::Timestamp::from_unix_seconds(2),
                etag: Some(format!("e-{prefix}{i}")),
                ctag: None,
                server_hash: None,
            }),
        })
        .collect()
}

/// WP-017 AC6 and AC10: a process killed anywhere in a delta run (staging a page, the
/// last page, applying the staged set, finishing the run) or in removing the mount
/// afterwards leaves the run still in progress with its staged set intact, or the run
/// finished, never anything between. The live token moves only with the finish
/// (ADR-0022 R1, R4). After each kill the store passes `PRAGMA integrity_check` and
/// `PRAGMA foreign_key_check`.
#[test]
fn adr_0022_killed_anywhere_in_a_delta_run_leaves_it_in_progress_or_finished() {
    if let Ok(arg) = std::env::var(RUN_CHILD_ENV) {
        let (db, point) = arg.split_once('|').unwrap();
        run_crash_child(Path::new(db), point);
    }

    for point in RUN_KILL_POINTS {
        let fx = Fixture::new();
        drop(fx.open_with_mount());
        run_child_until_killed(
            "adr_0022_killed_anywhere_in_a_delta_run_leaves_it_in_progress_or_finished",
            RUN_CHILD_ENV,
            &format!("{}|{point}", fx.path().display()),
        );
        assert!(
            fx.dir.path().join(format!("reached-{point}")).exists(),
            "{point}: the child reached the kill point"
        );
        let conn = raw(&fx.path());
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(conn);
        assert_eq!((integrity.as_str(), violations), ("ok", 0), "{point}");

        let store = fx.open();
        let applied = count_prefix(&store, "p");
        let Some(m) = store.mount(&mount_id()).unwrap() else {
            assert_eq!(point, "after-remove");
            assert_eq!(store.item(&key(ROOT)).unwrap(), None);
            assert_eq!(applied, 0);
            continue;
        };
        let staged = store.staged_count(&mount_id()).unwrap();
        let got = (
            m.delta_token.as_deref(),
            m.delta_run,
            m.delta_next_link.as_deref(),
            m.delta_staged_link.as_deref(),
            staged,
            applied,
        );
        let page = RUN_PAGE as u64;
        let initial = Some(DeltaRunKind::Initial);
        let expected = match point {
            // The run began (its own commit), but no page committed.
            "mid-page" => (None, initial, None, None, 0, 0),
            "after-page" | "mid-last-page" => (None, initial, Some("next-1"), None, page, 0),
            // The staged set is complete; the apply's transaction never committed.
            "after-last-page" | "mid-apply" => (None, initial, None, Some("delta-1"), 2 * page, 0),
            // Applied, not finished: still in progress, with the staged set intact.
            "after-apply" | "mid-finish" => {
                (None, initial, None, Some("delta-1"), 2 * page, 2 * RUN_PAGE)
            }
            "after-finish" | "mid-remove" => (Some("delta-1"), None, None, None, 0, 2 * RUN_PAGE),
            other => panic!("{other}"),
        };
        assert_eq!(got, expected, "{point}");
        if m.delta_staged_link.is_some() {
            // The complete staged set walks in full after the restart.
            let mut walked = 0;
            let mut cursor = None;
            loop {
                let page = store
                    .staged_entries(&mount_id(), cursor.as_ref(), common::limit(500))
                    .unwrap();
                walked += page.entries.len();
                match page.next {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
            assert_eq!(walked, 2 * RUN_PAGE, "{point}");
        }
    }
}

fn run_crash_child(db: &Path, point: &str) -> ! {
    let reached = |p: &str| std::fs::write(db.with_file_name(format!("reached-{p}")), b"").unwrap();
    let kill_at = |p: &str| {
        if p == point {
            reached(p);
            kill_self();
        }
    };
    let mut store = Store::open(db).unwrap();
    let m = mount_id();
    store.begin_delta_run(&m, DeltaRunKind::Initial).unwrap();

    let tx = store.transaction().unwrap();
    tx.stage_delta_page(&m, &run_page("p1-"), &PageLink::Next("next-1".to_owned()))
        .unwrap();
    kill_at("mid-page");
    tx.commit().unwrap();
    kill_at("after-page");

    let tx = store.transaction().unwrap();
    tx.stage_delta_page(&m, &run_page("p2-"), &PageLink::Delta("delta-1".to_owned()))
        .unwrap();
    kill_at("mid-last-page");
    tx.commit().unwrap();
    kill_at("after-last-page");

    // Apply: walk the staged set and insert each item, in one transaction.
    let tx = store.transaction().unwrap();
    let mut cursor = None;
    loop {
        let page = tx
            .staged_entries(&m, cursor.as_ref(), common::limit(400))
            .unwrap();
        for s in &page.entries {
            if let mydrive_store::StagedChange::Present(it) = &s.entry.change {
                let mut item = file(s.entry.item_id.as_str(), Some(ROOT), &it.name, it.size);
                item.etag.clone_from(&it.etag);
                tx.insert_item(&item).unwrap();
            }
        }
        match page.next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    kill_at("mid-apply");
    tx.commit().unwrap();
    kill_at("after-apply");

    let tx = store.transaction().unwrap();
    tx.finish_delta_run(&m).unwrap();
    kill_at("mid-finish");
    tx.commit().unwrap();
    kill_at("after-finish");

    let tx = store.transaction().unwrap();
    tx.remove_mount(&m, mydrive_store::LocalOnlyContent::Refuse)
        .unwrap();
    kill_at("mid-remove");
    tx.commit().unwrap();
    reached("after-remove");
    kill_self();
}
