//! Items: keys, columns, children, sync listing, paths and deletes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Fixture, ROOT, file, folder, key, mount_id, raw};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{ByteRange, DeleteMode, ItemKind, Queries, StoreError};

/// Every column survives a round trip, including the nullable ones set.
#[test]
fn upsert_and_fetch_round_trip_every_column() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f1", Some(ROOT), "report.docx", 1234);
    item.ctag = Some("ctag-1".to_owned());
    item.server_hash = Some(QuickXorHash::from_bytes([7; 20]));
    item.local_hash = Some(QuickXorHash::from_bytes([9; 20]));
    item.state = ItemState {
        hydration: Hydration::Partial,
        pin: Pin::Pinned,
        sync: SyncState::PendingUpload,
    };
    item.last_error = Some("quota full".to_owned());
    store.upsert_item(&item).unwrap();
    assert_eq!(store.item(&item.key).unwrap(), Some(item.clone()));

    // A second upsert replaces every column.
    let mut changed = item.clone();
    changed.ctag = None;
    changed.local_hash = None;
    changed.last_error = None;
    changed.etag = "etag-2".to_owned();
    changed.state.sync = SyncState::InSync;
    store.upsert_item(&changed).unwrap();
    assert_eq!(store.item(&item.key).unwrap(), Some(changed));
    assert_eq!(store.item(&key("absent")).unwrap(), None);
}

/// ADR-0006 R4: every item table is keyed by (`drive_id`, `item_id`), and no table has a
/// path column used as a key.
#[test]
fn adr_0006_r4_item_tables_are_keyed_by_drive_and_item_id() {
    let fx = Fixture::new();
    drop(fx.open());
    let conn = raw(&fx.path());
    for table in ["items", "hydrated_ranges"] {
        let pk: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")
            .unwrap()
            .query_map([table], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(&pk[..2], ["drive_id", "item_id"], "{table}");
    }
    // No primary key or unique index anywhere includes a path-like column.
    let key_columns: Vec<String> = conn
        .prepare(
            "SELECT ii.name FROM sqlite_schema AS t, pragma_index_list(t.name) AS il, \
             pragma_index_info(il.name) AS ii WHERE t.type = 'table' AND il.\"unique\" = 1 \
             UNION SELECT ti.name FROM sqlite_schema AS t, pragma_table_info(t.name) AS ti \
             WHERE t.type = 'table' AND ti.pk > 0",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for column in &key_columns {
        assert!(
            !column.contains("path") && !column.contains("name") && column != "mount_point",
            "{column} is part of a key"
        );
    }
}

/// WP-006 AC1, ADR-0007 R6: two items with the same parent and the same name (the same
/// path) are still two items, addressed by key.
#[test]
fn ac1_items_are_identified_by_key_not_path() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .upsert_item(&file("a", Some(ROOT), "same.txt", 1))
        .unwrap();
    store
        .upsert_item(&file("b", Some(ROOT), "same.txt", 2))
        .unwrap();
    assert_eq!(store.item(&key("a")).unwrap().unwrap().size, 1);
    assert_eq!(store.item(&key("b")).unwrap().unwrap().size, 2);

    // The same item ID in another drive is another item.
    let other = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new("a").unwrap());
    let mut in_b = file("a", None, "elsewhere", 3);
    in_b.key = other.clone();
    store.upsert_item(&in_b).unwrap();
    assert_eq!(store.item(&other).unwrap().unwrap().size, 3);
    assert_eq!(store.item(&key("a")).unwrap().unwrap().size, 1);
}

/// WP-006 AC7, ADR-0006 R5, ADR-0008 R1: the three axes are three columns holding
/// `mydrive-core`'s wire forms, for every value of every axis.
#[test]
fn ac7_adr_0006_r5_state_axes_are_three_columns_of_core_wire_forms() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut n = 0;
    for &hydration in Hydration::ALL {
        for &pin in Pin::ALL {
            for &sync in SyncState::ALL {
                n += 1;
                let mut item = file(&format!("i{n}"), Some(ROOT), &format!("f{n}"), 10);
                item.state = ItemState {
                    hydration,
                    pin,
                    sync,
                };
                store.upsert_item(&item).unwrap();
                assert_eq!(store.item(&item.key).unwrap().unwrap().state, item.state);
            }
        }
    }
    drop(store);

    let conn = raw(&fx.path());
    let mut stmt = conn
        .prepare("SELECT hydration, pin, sync FROM items WHERE item_id = ?1")
        .unwrap();
    let mut n = 0;
    for &hydration in Hydration::ALL {
        for &pin in Pin::ALL {
            for &sync in SyncState::ALL {
                n += 1;
                let row: (String, String, String) = stmt
                    .query_row([format!("i{n}")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap();
                assert_eq!(
                    row,
                    (
                        hydration.as_str().to_owned(),
                        pin.as_str().to_owned(),
                        sync.as_str().to_owned()
                    )
                );
            }
        }
    }
    // The schema refuses anything that is not a wire form, such as a display status.
    let refused = conn.execute("UPDATE items SET sync = 'syncing' WHERE item_id = 'i1'", []);
    assert!(refused.is_err());
}

/// ADR-0007 R1 at the store: a drive ID column only ever holds lowercase.
#[test]
fn drive_id_is_stored_lowercase() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("x", None, "x", 0);
    item.key = ItemKey::new(DriveId::new("MiXeD").unwrap(), ItemId::new("x").unwrap());
    store.upsert_item(&item).unwrap();
    drop(store);
    let conn = raw(&fx.path());
    let stored: String = conn
        .query_row("SELECT drive_id FROM items WHERE item_id = 'x'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, "mixed");
    assert!(
        conn.execute(
            "UPDATE items SET drive_id = 'UPPER' WHERE item_id = 'x'",
            []
        )
        .is_err()
    );
}

#[test]
fn names_that_would_make_a_path_ambiguous_are_refused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for bad in ["", ".", "..", "a/b", "nul\0"] {
        let err = store
            .upsert_item(&file("f", Some(ROOT), bad, 0))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::InvalidName { .. }),
            "{bad:?}: {err}"
        );
    }
    // Long names that Linux cannot hold are stored (F02 keeps them with sync Error).
    store
        .upsert_item(&file("f", Some(ROOT), &"n".repeat(400), 0))
        .unwrap();
}

#[test]
fn sizes_beyond_i64_are_refused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .upsert_item(&file("f", Some(ROOT), "huge", u64::MAX))
        .unwrap_err();
    assert!(matches!(err, StoreError::SizeTooLarge { .. }), "{err}");
}

#[test]
fn children_lists_direct_children_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.upsert_item(&folder("d", Some(ROOT), "docs")).unwrap();
    store
        .upsert_item(&file("b", Some("d"), "b.txt", 1))
        .unwrap();
    store
        .upsert_item(&file("a", Some("d"), "a.txt", 1))
        .unwrap();
    store
        .upsert_item(&file("deep", Some("a"), "deeper", 1))
        .unwrap();
    store
        .upsert_item(&file("top", Some(ROOT), "top.txt", 1))
        .unwrap();

    let names: Vec<String> = store
        .children(&key("d"))
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
    assert!(store.children(&key("b")).unwrap().is_empty());
}

#[test]
fn items_with_sync_lists_matching_items_across_drives() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut pending = file("p", Some(ROOT), "p", 1);
    pending.state.sync = SyncState::PendingUpload;
    store.upsert_item(&pending).unwrap();
    let mut pending_b = pending.clone();
    pending_b.key = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new("p").unwrap());
    store.upsert_item(&pending_b).unwrap();
    store.upsert_item(&file("q", Some(ROOT), "q", 1)).unwrap();

    let keys: Vec<ItemKey> = store
        .items_with_sync(SyncState::PendingUpload)
        .unwrap()
        .into_iter()
        .map(|i| i.key)
        .collect();
    assert_eq!(keys, [key("p"), pending_b.key]);
    assert!(
        store
            .items_with_sync(SyncState::Conflict)
            .unwrap()
            .is_empty()
    );
}

fn tree(store: &impl Queries) {
    // root / docs / work / plan.txt
    store
        .upsert_item(&folder("docs", Some(ROOT), "docs"))
        .unwrap();
    store
        .upsert_item(&folder("work", Some("docs"), "work"))
        .unwrap();
    store
        .upsert_item(&file("plan", Some("work"), "plan.txt", 100))
        .unwrap();
    store
        .upsert_item(&folder("other", Some(ROOT), "other"))
        .unwrap();
}

fn path_of(store: &impl Queries, item: &str) -> String {
    store
        .resolve_path(&mount_id(), &key(item))
        .unwrap()
        .to_string()
}

#[test]
fn resolve_path_walks_parents_to_the_mount_root() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    assert_eq!(path_of(&store, "plan"), "docs/work/plan.txt");
    let root = store.resolve_path(&mount_id(), &key(ROOT)).unwrap();
    assert!(root.is_root());
    assert_eq!(root.to_path_buf(), std::path::PathBuf::new());
}

/// A mount of a folder within a drive resolves paths below that folder, and refuses items
/// outside it.
#[test]
fn resolve_path_in_a_folder_mount() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .insert_mount(&common::mount("m2", "docs", "/home/u/Docs"))
        .unwrap();
    let m2 = MountId::new("m2").unwrap();
    assert_eq!(
        store.resolve_path(&m2, &key("plan")).unwrap().components(),
        ["work", "plan.txt"]
    );
    let err = store.resolve_path(&m2, &key("other")).unwrap_err();
    assert!(matches!(err, StoreError::NotInMount { .. }), "{err}");
}

/// WP-006 AC4: renaming or moving a folder is one row update, and every descendant's path
/// follows.
#[test]
fn ac4_rename_and_move_of_a_folder_is_one_row_update() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    let before = row_snapshot(&fx);

    let mut docs = store.item(&key("docs")).unwrap().unwrap();
    docs.name = "papers".to_owned();
    docs.parent_item_id = Some(ItemId::new("other").unwrap());
    store.upsert_item(&docs).unwrap();

    assert_eq!(path_of(&store, "docs"), "other/papers");
    assert_eq!(path_of(&store, "work"), "other/papers/work");
    assert_eq!(path_of(&store, "plan"), "other/papers/work/plan.txt");

    let after = row_snapshot(&fx);
    let changed: Vec<&String> = before
        .iter()
        .zip(&after)
        .filter(|(b, a)| b != a)
        .map(|(b, _)| &b.0)
        .collect();
    assert_eq!(changed, ["docs"], "only the folder's own row changed");
}

fn row_snapshot(fx: &Fixture) -> Vec<(String, String)> {
    let conn = raw(&fx.path());
    conn.prepare(
        "SELECT item_id, COALESCE(parent_item_id, '') || '|' || name || '|' || etag \
         FROM items ORDER BY item_id",
    )
    .unwrap()
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// WP-006 AC8: a parent cycle ends path resolution with an error instead of looping.
#[test]
fn ac8_parent_cycle_is_reported() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.upsert_item(&folder("a", Some("b"), "a")).unwrap();
    store.upsert_item(&folder("b", Some("a"), "b")).unwrap();
    store
        .upsert_item(&file("leaf", Some("a"), "leaf", 0))
        .unwrap();
    store
        .upsert_item(&folder("self", Some("self"), "self"))
        .unwrap();

    for item in ["leaf", "a", "self"] {
        let err = store.resolve_path(&mount_id(), &key(item)).unwrap_err();
        assert!(
            matches!(err, StoreError::ParentCycle { .. }),
            "{item}: {err}"
        );
    }
}

#[test]
fn resolve_path_reports_missing_ancestors_and_wrong_drive() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .upsert_item(&file("orphan", Some("never-seen"), "o", 0))
        .unwrap();
    let err = store.resolve_path(&mount_id(), &key("orphan")).unwrap_err();
    assert!(
        matches!(&err, StoreError::MissingAncestor { missing, .. } if missing.as_str() == "never-seen"),
        "{err}"
    );

    let other = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new(ROOT).unwrap());
    let err = store.resolve_path(&mount_id(), &other).unwrap_err();
    assert!(matches!(err, StoreError::WrongDrive { .. }), "{err}");

    let err = store.resolve_path(&mount_id(), &key("absent")).unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");

    let err = store
        .resolve_path(&MountId::new("nope").unwrap(), &key(ROOT))
        .unwrap_err();
    assert!(matches!(err, StoreError::MountNotFound { .. }), "{err}");
}

/// WP-006 AC5: deleting an item removes its hydrated ranges.
#[test]
fn ac5_delete_removes_the_items_ranges() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.upsert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    store
        .record_hydrated_range(&key("f"), ByteRange::new(0, 50).unwrap())
        .unwrap();
    assert_eq!(
        store.delete_item(&key("f"), DeleteMode::ItemOnly).unwrap(),
        1
    );
    assert_eq!(store.item(&key("f")).unwrap(), None);
    drop(store);
    let left: i64 = raw(&fx.path())
        .query_row("SELECT COUNT(*) FROM hydrated_ranges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// WP-006 AC5: a folder with children is not deleted unless the caller asks for a
/// recursive delete, and the refusal changes nothing.
#[test]
fn ac5_deleting_a_folder_with_children_is_refused_unless_recursive() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .record_hydrated_range(&key("plan"), ByteRange::new(10, 20).unwrap())
        .unwrap();
    let before = row_snapshot(&fx);

    let err = store
        .delete_item(&key("docs"), DeleteMode::ItemOnly)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::HasChildren { children: 1, .. }),
        "{err}"
    );
    assert_eq!(row_snapshot(&fx), before, "refused delete changed nothing");
    assert_eq!(store.hydrated_ranges(&key("plan")).unwrap().len(), 1);

    // An empty folder needs no recursive flag.
    assert_eq!(
        store
            .delete_item(&key("other"), DeleteMode::ItemOnly)
            .unwrap(),
        1
    );

    assert_eq!(
        store
            .delete_item(&key("docs"), DeleteMode::Recursive)
            .unwrap(),
        3
    );
    for gone in ["docs", "work", "plan"] {
        assert_eq!(store.item(&key(gone)).unwrap(), None, "{gone}");
    }
    assert!(store.item(&key(ROOT)).unwrap().is_some());
    drop(store);
    let left: i64 = raw(&fx.path())
        .query_row("SELECT COUNT(*) FROM hydrated_ranges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// A recursive delete terminates on a parent cycle and only removes the subtree.
#[test]
fn recursive_delete_terminates_on_a_cycle() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.upsert_item(&folder("a", Some("b"), "a")).unwrap();
    store.upsert_item(&folder("b", Some("a"), "b")).unwrap();
    store
        .upsert_item(&file("keep", Some(ROOT), "keep", 0))
        .unwrap();
    assert_eq!(
        store.delete_item(&key("a"), DeleteMode::Recursive).unwrap(),
        2
    );
    assert!(store.item(&key("keep")).unwrap().is_some());
}

#[test]
fn delete_of_unknown_item_is_an_error() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .delete_item(&key("absent"), DeleteMode::Recursive)
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
}

/// Upserting an item to a size or kind its recorded ranges do not fit is refused and
/// changes nothing; clearing the ranges first, in one transaction, works.
#[test]
fn upsert_refuses_to_strand_hydrated_ranges() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let original = file("f", Some(ROOT), "f", 100);
    store.upsert_item(&original).unwrap();
    store
        .record_hydrated_range(&key("f"), ByteRange::new(40, 80).unwrap())
        .unwrap();

    let shrunk = file("f", Some(ROOT), "f", 60);
    let err = store.upsert_item(&shrunk).unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::RangesWouldExceedItem {
                covered_end: 80,
                ..
            }
        ),
        "{err}"
    );
    let as_folder = folder("f", Some(ROOT), "f");
    assert!(matches!(
        store.upsert_item(&as_folder).unwrap_err(),
        StoreError::RangesWouldExceedItem {
            kind: ItemKind::Folder,
            ..
        }
    ));
    assert_eq!(store.item(&key("f")).unwrap(), Some(original));

    // Growing is fine.
    store.upsert_item(&file("f", Some(ROOT), "f", 200)).unwrap();

    let tx = store.transaction().unwrap();
    tx.clear_hydrated_ranges(&key("f")).unwrap();
    tx.upsert_item(&shrunk).unwrap();
    tx.commit().unwrap();
    assert_eq!(store.item(&key("f")).unwrap().unwrap().size, 60);
}
