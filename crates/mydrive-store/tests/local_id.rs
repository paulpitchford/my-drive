//! The local ID (ADR-0021): assigned once, unique, never changed, never reused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{
    Fixture, ROOT, file, folder, key, kill_self, make_full, provisional, raw, remote,
    run_child_until_killed,
};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::ItemId;
use mydrive_store::{DeleteMode, LocalId, PendingChanges, Queries, RemoteMetadata, Store};

fn ids_in_store(path: &Path) -> BTreeSet<i64> {
    let conn = raw(path);
    let mut stmt = conn.prepare("SELECT local_id FROM items").unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// ADR-0021 R1 (WP-017 AC2): across several opens of the store, items are inserted and
/// removed (including the one holding the largest ID, then every item); a new item never
/// receives an ID any earlier row held, and VACUUM does not reset the sequence.
#[test]
fn adr_0021_r1_local_ids_are_never_reused_across_opens_deletes_and_vacuum() {
    let fx = Fixture::new();
    // Every ID handed out, in order. Each must be new and larger than all before it: the
    // allocator only counts up.
    let mut ever: Vec<LocalId> = Vec::new();
    let fresh = |id: LocalId, ever: &mut Vec<LocalId>| {
        assert!(!ever.contains(&id), "local ID {id} handed out twice");
        assert!(ever.iter().all(|e| id > *e), "local ID {id} went backwards");
        ever.push(id);
    };

    let store = fx.open_with_mount();
    fresh(store.local_id(&key(ROOT)).unwrap(), &mut ever);
    for n in 0..5 {
        fresh(
            store
                .insert_item(&file(&format!("a{n}"), Some(ROOT), &format!("a{n}"), 1))
                .unwrap(),
            &mut ever,
        );
    }
    // Remove the row with the largest ID, then reopen.
    let last = key("a4");
    store
        .delete_item(&last, DeleteMode::ItemOnly, PendingChanges::Refuse)
        .unwrap();
    drop(store);

    let store = fx.open();
    fresh(
        store.insert_item(&file("b0", Some(ROOT), "b0", 1)).unwrap(),
        &mut ever,
    );
    // Re-inserting the same key is a new row with a new ID.
    fresh(
        store.insert_item(&file("a4", Some(ROOT), "a4", 1)).unwrap(),
        &mut ever,
    );
    // Remove every item, reopen: the sequence does not restart.
    store
        .delete_item(&key(ROOT), DeleteMode::Recursive, PendingChanges::Refuse)
        .unwrap_err(); // the mount root is refused...
    for id in ["a0", "a1", "a2", "a3", "a4", "b0"] {
        store
            .delete_item(&key(id), DeleteMode::ItemOnly, PendingChanges::Refuse)
            .unwrap();
    }
    raw(&fx.path())
        .execute_batch("DELETE FROM mounts; DELETE FROM items;")
        .unwrap(); // ...so the last rows go by hand: the table is empty.
    assert!(ids_in_store(&fx.path()).is_empty());
    drop(store);

    let store = fx.open();
    fresh(
        store.insert_item(&folder(ROOT, None, "root")).unwrap(),
        &mut ever,
    );
    drop(store);
    raw(&fx.path()).execute_batch("VACUUM").unwrap();
    let store = fx.open();
    for n in 0..3 {
        fresh(
            store
                .insert_item(&file(&format!("c{n}"), Some(ROOT), &format!("c{n}"), 1))
                .unwrap(),
            &mut ever,
        );
    }
    assert_eq!(ever.len(), 1 + 5 + 2 + 1 + 3);
}

/// ADR-0021 R1: the table refuses a duplicate, missing or non-positive local ID
/// whatever writes the row.
#[test]
fn adr_0021_r1_local_id_is_unique_and_not_null_in_the_table() {
    let fx = Fixture::new();
    drop(fx.open_with_mount());
    let conn = raw(&fx.path());
    let insert = |id: &str, local_id: &str| {
        conn.execute_batch(&format!(
            "INSERT INTO items (drive_id, item_id, name, kind, size, created_at, modified_at, \
             hydration, pin, sync, local_id) VALUES ('drive-a', '{id}', 'n', 'file', 0, 0, 0, \
             'full', 'inherit', 'in-sync', {local_id})"
        ))
    };
    let root_id: i64 = conn
        .query_row("SELECT local_id FROM items", [], |r| r.get(0))
        .unwrap();
    for (value, why) in [
        (root_id.to_string(), "UNIQUE constraint failed"),
        ("NULL".to_owned(), "NOT NULL constraint failed"),
        ("0".to_owned(), "CHECK constraint failed"),
    ] {
        let err = insert("x", &value).unwrap_err().to_string();
        assert!(err.contains(why), "{value}: {err}");
    }
}

/// ADR-0021 R2 (WP-017 AC2): `rekey_item`, `complete_upload` with a new server ID and
/// `move_item_local` leave the local ID unchanged, as do the other operations on the row.
#[test]
fn adr_0021_r2_local_id_survives_rekey_upload_and_local_moves() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_item(&folder("dir", Some(ROOT), "dir"))
        .unwrap();

    // rekey_item.
    let a = store
        .insert_item(&provisional("tmp-a", Some(ROOT), "a", 3))
        .unwrap();
    store
        .rekey_item(&key("tmp-a"), &ItemId::new("srv-a").unwrap())
        .unwrap();
    assert_eq!(store.local_id(&key("srv-a")).unwrap(), a);

    // complete_upload with a new server ID.
    let b_item = provisional("tmp-b", Some(ROOT), "b", 4);
    let b = store.insert_item(&b_item).unwrap();
    let response = RemoteMetadata {
        key: key("srv-b"),
        etag: Some("e".to_owned()),
        ..remote(&b_item)
    };
    store
        .complete_upload(
            &key("tmp-b"),
            1,
            QuickXorHash::from_bytes([1; 20]),
            &response,
            Some(&ItemId::new("srv-b").unwrap()),
        )
        .unwrap();
    assert_eq!(store.local_id(&key("srv-b")).unwrap(), b);

    // move_item_local, then the other row operations.
    store
        .move_item_local(&key("srv-b"), Some(&ItemId::new("dir").unwrap()), "moved")
        .unwrap();
    assert_eq!(store.local_id(&key("srv-b")).unwrap(), b);
    let c_item = file("c", Some(ROOT), "c", 10);
    let c = store.insert_item(&c_item).unwrap();
    make_full(&store, &key("c"));
    store.mark_local_change(&key("c")).unwrap();
    store.set_local_size(&key("c"), 20).unwrap();
    store
        .apply_remote_metadata(&RemoteMetadata {
            name: "renamed".to_owned(),
            ..remote(&c_item)
        })
        .unwrap();
    store
        .discard_local_changes(&key("c"), &remote(&c_item))
        .unwrap();
    assert_eq!(store.local_id(&key("c")).unwrap(), c);
    assert!(matches!(
        store.local_id(&key("tmp-a")).unwrap_err(),
        mydrive_store::StoreError::ItemNotFound { .. }
    ));
}

/// Set in the child of `adr_0021_r1_a_crash_between_allocation_and_commit_reuses_no_held_id`.
const CHILD_ENV: &str = "MYDRIVE_STORE_LOCAL_ID_CRASH_CHILD";

/// ADR-0021 R1: the process is killed after an insert allocated its local ID and wrote
/// its row, before the commit. Neither survives: the allocator is where it was, so the next
/// insert may take that number, which no row ever held. Every ID a committed row held stays
/// unused.
#[test]
fn adr_0021_r1_a_crash_between_allocation_and_commit_reuses_no_held_id() {
    if let Ok(db) = std::env::var(CHILD_ENV) {
        let mut store = Store::open(&db).unwrap();
        let tx = store.transaction().unwrap();
        let id = tx
            .insert_item(&file("lost", Some(ROOT), "lost", 1))
            .unwrap();
        std::fs::write(Path::new(&db).with_file_name("allocated"), id.to_string()).unwrap();
        kill_self();
    }

    let fx = Fixture::new();
    let held: BTreeSet<LocalId> = {
        let store = fx.open_with_mount();
        let a = store.insert_item(&file("a", Some(ROOT), "a", 1)).unwrap();
        let b = store.insert_item(&file("b", Some(ROOT), "b", 1)).unwrap();
        store
            .delete_item(&key("b"), DeleteMode::ItemOnly, PendingChanges::Refuse)
            .unwrap();
        [store.local_id(&key(ROOT)).unwrap(), a, b].into()
    };
    run_child_until_killed(
        "adr_0021_r1_a_crash_between_allocation_and_commit_reuses_no_held_id",
        CHILD_ENV,
        &fx.path().display().to_string(),
    );
    let allocated: u64 = std::fs::read_to_string(fx.dir.path().join("allocated"))
        .unwrap()
        .parse()
        .unwrap();
    let store = fx.open();
    assert_eq!(store.item(&key("lost")).unwrap(), None);
    let next = store
        .insert_item(&file("next", Some(ROOT), "next", 1))
        .unwrap();
    assert!(!held.contains(&next), "{next} was held by a committed row");
    assert!(held.iter().all(|h| next > *h));
    assert_eq!(
        next.get(),
        allocated,
        "the rolled-back number was never held"
    );
}
