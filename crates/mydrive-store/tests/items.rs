//! Items: keys, columns, setters, remote metadata, re-keying, paths and deletes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Fixture, ROOT, file, folder, key, mount_id, raw, remote};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{
    ByteRange, DeleteMode, Item, ItemKind, PendingChanges, Queries, RemoteApplied, Store,
    StoreError,
};

const REFUSE: PendingChanges = PendingChanges::Refuse;

fn hash(b: u8) -> Option<QuickXorHash> {
    Some(QuickXorHash::from_bytes([b; 20]))
}

fn r(start: u64, end: u64) -> ByteRange {
    ByteRange::new(start, end).unwrap()
}

/// Every column survives a round trip, including the nullable ones and a provisional item
/// with no eTag.
#[test]
fn insert_and_fetch_round_trip_every_column() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f1", Some(ROOT), "report.docx", 1234);
    item.ctag = Some("ctag-1".to_owned());
    item.server_hash = hash(7);
    item.local_hash = hash(9);
    item.state = ItemState {
        hydration: Hydration::Partial,
        pin: Pin::Pinned,
        sync: SyncState::PendingUpload,
    };
    item.last_error = Some("quota full".to_owned());
    store.insert_item(&item).unwrap();
    assert_eq!(store.item(&item.key).unwrap(), Some(item.clone()));

    let mut local = file("local-1", Some(ROOT), "new.txt", 0);
    local.etag = None;
    local.is_provisional = true;
    local.state.sync = SyncState::PendingUpload;
    store.insert_item(&local).unwrap();
    assert_eq!(store.item(&local.key).unwrap(), Some(local));
    assert_eq!(store.item(&key("absent")).unwrap(), None);
}

#[test]
fn insert_refuses_an_existing_key_and_changes_nothing() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = file("f", Some(ROOT), "f", 1);
    store.insert_item(&item).unwrap();
    let err = store
        .insert_item(&file("f", Some(ROOT), "other", 2))
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemExists { .. }), "{err}");
    assert_eq!(store.item(&item.key).unwrap(), Some(item));
}

/// The narrow setters change their own column and nothing else.
#[test]
fn setters_change_only_their_column() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f", Some(ROOT), "f", 10);
    store.insert_item(&item).unwrap();

    store.set_pin(&item.key, Pin::Pinned).unwrap();
    item.state.pin = Pin::Pinned;
    store.set_hydration(&item.key, Hydration::Full).unwrap();
    item.state.hydration = Hydration::Full;
    store.set_hashes(&item.key, hash(1), hash(2)).unwrap();
    item.local_hash = hash(1);
    item.server_hash = hash(2);
    store.set_last_error(&item.key, Some("boom")).unwrap();
    item.last_error = Some("boom".to_owned());
    store
        .set_sync(&item.key, SyncState::InSync, SyncState::PendingUpload)
        .unwrap();
    item.state.sync = SyncState::PendingUpload;
    store.set_local_size(&item.key, 20).unwrap();
    item.size = 20;
    assert_eq!(store.item(&item.key).unwrap(), Some(item));

    let absent = key("absent");
    assert!(matches!(
        store.set_pin(&absent, Pin::Pinned).unwrap_err(),
        StoreError::ItemNotFound { .. }
    ));
    assert!(matches!(
        store.set_hashes(&absent, None, None).unwrap_err(),
        StoreError::ItemNotFound { .. }
    ));
}

/// `set_sync` is a compare-and-set: a stale expectation is refused with the stored value
/// and changes nothing.
#[test]
fn set_sync_compares_before_setting() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 1)).unwrap();
    let err = store
        .set_sync(&key("f"), SyncState::Uploading, SyncState::InSync)
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::SyncMismatch {
                expected: SyncState::Uploading,
                actual: SyncState::InSync,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(
        store.item(&key("f")).unwrap().unwrap().state.sync,
        SyncState::InSync
    );
    let err = store
        .set_sync(&key("x"), SyncState::InSync, SyncState::Error)
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
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
        .insert_item(&file("a", Some(ROOT), "same.txt", 1))
        .unwrap();
    store
        .insert_item(&file("b", Some(ROOT), "same.txt", 2))
        .unwrap();
    assert_eq!(store.item(&key("a")).unwrap().unwrap().size, 1);
    assert_eq!(store.item(&key("b")).unwrap().unwrap().size, 2);

    let other = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new("a").unwrap());
    let mut in_b = file("a", None, "elsewhere", 3);
    in_b.key = other.clone();
    store.insert_item(&in_b).unwrap();
    assert_eq!(store.item(&other).unwrap().unwrap().size, 3);
    assert_eq!(store.item(&key("a")).unwrap().unwrap().size, 1);
}

/// WP-006 AC7, ADR-0006 R5, ADR-0008 R1: the three axes are three columns holding
/// `mydrive-core`'s wire forms, for every value of every axis.
#[test]
fn ac7_adr_0006_r5_state_axes_are_three_columns_of_core_wire_forms() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut expected = Vec::new();
    for &hydration in Hydration::ALL {
        for &pin in Pin::ALL {
            for &sync in SyncState::ALL {
                let n = expected.len();
                let mut item = file(&format!("i{n}"), Some(ROOT), &format!("f{n}"), 10);
                item.state = ItemState {
                    hydration,
                    pin,
                    sync,
                };
                store.insert_item(&item).unwrap();
                assert_eq!(store.item(&item.key).unwrap().unwrap().state, item.state);
                expected.push((
                    format!("i{n}"),
                    (
                        hydration.as_str().to_owned(),
                        pin.as_str().to_owned(),
                        sync.as_str().to_owned(),
                    ),
                ));
            }
        }
    }
    drop(store);

    let conn = raw(&fx.path());
    let mut stmt = conn
        .prepare("SELECT hydration, pin, sync FROM items WHERE item_id = ?1")
        .unwrap();
    for (id, want) in expected {
        let row: (String, String, String) = stmt
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(row, want);
    }
    // The schema refuses anything that is not a wire form, such as a display status.
    let refused = conn.execute("UPDATE items SET sync = 'syncing' WHERE item_id = 'i1'", []);
    assert!(refused.is_err());
}

/// Lead ruling: `kind` has no SQL CHECK, so a new kind needs no table rebuild; the Rust
/// type still refuses a value it does not know when the row is read.
#[test]
fn kind_is_validated_in_rust_not_sql() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 1)).unwrap();
    drop(store);
    raw(&fx.path())
        .execute("UPDATE items SET kind = 'package' WHERE item_id = 'f'", [])
        .unwrap();
    let store = fx.open();
    let err = store.item(&key("f")).unwrap_err();
    assert!(matches!(err, StoreError::Corrupt { .. }), "{err}");
}

/// ADR-0007 R1 at the store: a drive ID column only ever holds lowercase.
#[test]
fn drive_id_is_stored_lowercase() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("x", None, "x", 0);
    item.key = ItemKey::new(DriveId::new("MiXeD").unwrap(), ItemId::new("x").unwrap());
    store.insert_item(&item).unwrap();
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
            .insert_item(&file("f", Some(ROOT), bad, 0))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::InvalidName { .. }),
            "{bad:?}: {err}"
        );
    }
    let good = file("f", Some(ROOT), "f", 0);
    store.insert_item(&good).unwrap();
    let mut renamed = remote(&good);
    renamed.name = "a/b".to_owned();
    assert!(matches!(
        store.apply_remote_metadata(&renamed).unwrap_err(),
        StoreError::InvalidName { .. }
    ));
    // Long names that Linux cannot hold are stored (F02 keeps them with sync Error).
    store
        .insert_item(&file("g", Some(ROOT), &"n".repeat(400), 0))
        .unwrap();
}

#[test]
fn sizes_beyond_i64_are_refused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .insert_item(&file("f", Some(ROOT), "huge", u64::MAX))
        .unwrap_err();
    assert!(matches!(err, StoreError::SizeTooLarge { .. }), "{err}");
}

#[test]
fn children_lists_direct_children_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&folder("d", Some(ROOT), "docs")).unwrap();
    store
        .insert_item(&file("b", Some("d"), "b.txt", 1))
        .unwrap();
    store
        .insert_item(&file("a", Some("d"), "a.txt", 1))
        .unwrap();
    store
        .insert_item(&file("deep", Some("a"), "deeper", 1))
        .unwrap();
    store
        .insert_item(&file("top", Some(ROOT), "top.txt", 1))
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
    store.insert_item(&pending).unwrap();
    let mut pending_b = pending.clone();
    pending_b.key = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new("p").unwrap());
    store.insert_item(&pending_b).unwrap();
    store.insert_item(&file("q", Some(ROOT), "q", 1)).unwrap();

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

// ------------------------------------------------------------ remote metadata

/// A hydrated file with known hashes and local state, used by the remote-metadata tests.
fn hydrated(store: &Store, sync: SyncState) -> Item {
    let mut item = file("f", Some(ROOT), "f.txt", 100);
    item.server_hash = hash(1);
    item.local_hash = hash(1);
    item.state = ItemState {
        hydration: Hydration::Partial,
        pin: Pin::Pinned,
        sync,
    };
    item.last_error = Some("old".to_owned());
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 80)).unwrap();
    item
}

/// Lead ruling 3: unchanged content writes every server-owned column and nothing local.
#[test]
fn remote_metadata_without_content_change_writes_server_columns_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::PendingUpload);
    let mut update = remote(&item);
    update.name = "renamed.txt".to_owned();
    update.etag = Some("etag-new".to_owned());
    update.ctag = Some("ctag-new".to_owned());

    assert_eq!(
        store.apply_remote_metadata(&update).unwrap(),
        RemoteApplied::ContentUnchanged
    );
    let stored = store.item(&item.key).unwrap().unwrap();
    assert_eq!(stored.name, "renamed.txt");
    assert_eq!(stored.etag.as_deref(), Some("etag-new"));
    assert_eq!(stored.ctag.as_deref(), Some("ctag-new"));
    assert_eq!(stored.state, item.state, "local state untouched");
    assert_eq!(stored.local_hash, item.local_hash);
    assert_eq!(stored.last_error, item.last_error);
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 80)]);
}

/// Lead rulings 3 and 4: a content change on an item with nothing pending clears the
/// ranges and sets hydration to placeholder in the same operation, so ranges never
/// describe a version the hashes do not. Local columns other than hydration are kept.
#[test]
fn remote_content_change_on_an_idle_item_drops_its_ranges() {
    for sync in [SyncState::InSync, SyncState::PendingDownload] {
        let fx = Fixture::new();
        let store = fx.open_with_mount();
        let item = hydrated(&store, sync);
        let mut update = remote(&item);
        update.server_hash = hash(2);
        update.size = 10; // smaller than the old ranges: nothing is stranded
        update.etag = Some("etag-2".to_owned());

        assert_eq!(
            store.apply_remote_metadata(&update).unwrap(),
            RemoteApplied::ContentReplaced { ranges_cleared: 1 },
            "{sync}"
        );
        let stored = store.item(&item.key).unwrap().unwrap();
        assert_eq!(stored.server_hash, hash(2));
        assert_eq!(stored.size, 10);
        assert_eq!(stored.state.hydration, Hydration::Placeholder);
        assert_eq!(stored.state.pin, Pin::Pinned);
        assert_eq!(stored.state.sync, sync);
        assert_eq!(stored.local_hash, item.local_hash);
        assert!(store.hydrated_ranges(&item.key).unwrap().is_empty());
    }
}

/// Lead ruling 3: a content change under a pending local change keeps ranges, local state
/// and the content columns (so the stale eTag still makes the upload fail with 412), takes
/// the new parent and name, and tells the caller.
#[test]
fn remote_content_change_under_local_state_keeps_everything_and_reports() {
    for sync in [
        SyncState::PendingUpload,
        SyncState::Uploading,
        SyncState::Conflict,
        SyncState::Error,
        SyncState::Blocked,
    ] {
        let fx = Fixture::new();
        let store = fx.open_with_mount();
        store.insert_item(&folder("d", Some(ROOT), "d")).unwrap();
        let item = hydrated(&store, sync);
        let mut update = remote(&item);
        update.server_hash = hash(2);
        update.size = 10;
        update.etag = Some("etag-2".to_owned());
        update.name = "moved.txt".to_owned();
        update.parent_item_id = Some(ItemId::new("d").unwrap());

        assert_eq!(
            store.apply_remote_metadata(&update).unwrap(),
            RemoteApplied::ContentChangedUnderLocalState { sync },
        );
        let stored = store.item(&item.key).unwrap().unwrap();
        let expected = Item {
            name: "moved.txt".to_owned(),
            parent_item_id: Some(ItemId::new("d").unwrap()),
            ..item.clone()
        };
        assert_eq!(stored, expected, "{sync}");
        assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 80)]);
    }
}

/// Without hashes on either side, a different eTag or size is a content change.
#[test]
fn without_hashes_etag_and_size_identify_the_content() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = file("f", Some(ROOT), "f", 100);
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 10)).unwrap();

    assert_eq!(
        store.apply_remote_metadata(&remote(&item)).unwrap(),
        RemoteApplied::ContentUnchanged
    );
    let mut update = remote(&item);
    update.etag = Some("etag-other".to_owned());
    assert_eq!(
        store.apply_remote_metadata(&update).unwrap(),
        RemoteApplied::ContentReplaced { ranges_cleared: 1 }
    );
}

/// Lead ruling 3: a folder with children cannot become a file; an empty one can.
#[test]
fn folder_with_children_cannot_become_a_file() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let dir = folder("d", Some(ROOT), "d");
    store.insert_item(&dir).unwrap();
    store.insert_item(&file("c", Some("d"), "c", 1)).unwrap();
    let mut as_file = remote(&dir);
    as_file.kind = ItemKind::File;
    let err = store.apply_remote_metadata(&as_file).unwrap_err();
    assert!(
        matches!(err, StoreError::FolderHasChildren { children: 1, .. }),
        "{err}"
    );
    assert_eq!(store.item(&dir.key).unwrap(), Some(dir.clone()));

    store
        .delete_item(&key("c"), DeleteMode::ItemOnly, REFUSE)
        .unwrap();
    assert_eq!(
        store.apply_remote_metadata(&as_file).unwrap(),
        RemoteApplied::ContentReplaced { ranges_cleared: 0 }
    );
    let stored = store.item(&dir.key).unwrap().unwrap();
    assert_eq!(
        (stored.kind, stored.state.hydration),
        (ItemKind::File, Hydration::Placeholder)
    );
}

#[test]
fn remote_metadata_for_an_unknown_item_is_refused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .apply_remote_metadata(&remote(&file("new", Some(ROOT), "n", 1)))
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
}

/// Lead ruling 3: on the local-write path, a size shrink that would strand ranges is
/// refused and changes nothing; clearing the ranges first, in one transaction, works.
#[test]
fn local_size_shrink_refuses_to_strand_ranges() {
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    store.record_hydrated_range(&key("f"), r(40, 80)).unwrap();

    let err = store.set_local_size(&key("f"), 60).unwrap_err();
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
    assert_eq!(store.item(&key("f")).unwrap().unwrap().size, 100);
    store.set_local_size(&key("f"), 80).unwrap();

    let tx = store.transaction().unwrap();
    tx.clear_hydrated_ranges(&key("f")).unwrap();
    tx.set_local_size(&key("f"), 60).unwrap();
    tx.commit().unwrap();
    assert_eq!(store.item(&key("f")).unwrap().unwrap().size, 60);
}

// ------------------------------------------------------------------ re-keying

/// Lead ruling on provisional items: re-keying moves the item, its children's parent
/// links and its ranges together, and clears `is_provisional`.
#[test]
fn rekey_moves_item_children_and_ranges() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut dir = folder("tmp-1", Some(ROOT), "new folder");
    dir.is_provisional = true;
    dir.etag = None;
    store.insert_item(&dir).unwrap();
    let mut child = file("tmp-2", Some("tmp-1"), "a.txt", 10);
    child.is_provisional = true;
    store.insert_item(&child).unwrap();
    store.record_hydrated_range(&child.key, r(0, 10)).unwrap();

    store
        .rekey_item(&child.key, &ItemId::new("srv-2").unwrap())
        .unwrap();
    store
        .rekey_item(&dir.key, &ItemId::new("srv-1").unwrap())
        .unwrap();

    assert_eq!(store.item(&key("tmp-1")).unwrap(), None);
    assert_eq!(store.item(&key("tmp-2")).unwrap(), None);
    let new_dir = store.item(&key("srv-1")).unwrap().unwrap();
    assert!(!new_dir.is_provisional);
    let new_child = store.item(&key("srv-2")).unwrap().unwrap();
    assert!(!new_child.is_provisional);
    assert_eq!(new_child.parent_item_id.unwrap().as_str(), "srv-1");
    assert_eq!(store.hydrated_ranges(&key("srv-2")).unwrap(), [r(0, 10)]);
    assert_eq!(path_of(&store, "srv-2"), "new folder/a.txt");
    drop(store);
    let orphans: i64 = raw(&fx.path())
        .query_row(
            "SELECT COUNT(*) FROM hydrated_ranges WHERE item_id LIKE 'tmp-%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
}

#[test]
fn rekey_refusals_change_nothing() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut tmp = file("tmp", Some(ROOT), "t", 10);
    tmp.is_provisional = true;
    store.insert_item(&tmp).unwrap();
    store
        .insert_item(&file("taken", Some(ROOT), "x", 1))
        .unwrap();

    let err = store
        .rekey_item(&tmp.key, &ItemId::new("taken").unwrap())
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemExists { .. }), "{err}");
    assert_eq!(store.item(&tmp.key).unwrap(), Some(tmp));

    let err = store
        .rekey_item(&key("taken"), &ItemId::new("new").unwrap())
        .unwrap_err();
    assert!(matches!(err, StoreError::NotProvisional { .. }), "{err}");
    let err = store
        .rekey_item(&key("absent"), &ItemId::new("new").unwrap())
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
}

// ---------------------------------------------------------------------- paths

fn tree(store: &impl Queries) {
    // root / docs / work / plan.txt
    store
        .insert_item(&folder("docs", Some(ROOT), "docs"))
        .unwrap();
    store
        .insert_item(&folder("work", Some("docs"), "work"))
        .unwrap();
    store
        .insert_item(&file("plan", Some("work"), "plan.txt", 100))
        .unwrap();
    store
        .insert_item(&folder("other", Some(ROOT), "other"))
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

    let mut docs = remote(&store.item(&key("docs")).unwrap().unwrap());
    docs.name = "papers".to_owned();
    docs.parent_item_id = Some(ItemId::new("other").unwrap());
    store.apply_remote_metadata(&docs).unwrap();

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
        "SELECT item_id, COALESCE(parent_item_id, '') || '|' || name || '|' || \
         COALESCE(etag, '') || '|' || sync || '|' || hydration FROM items ORDER BY item_id",
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
    store.insert_item(&folder("a", Some("b"), "a")).unwrap();
    store.insert_item(&folder("b", Some("a"), "b")).unwrap();
    store
        .insert_item(&file("leaf", Some("a"), "leaf", 0))
        .unwrap();
    store
        .insert_item(&folder("self", Some("self"), "self"))
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
        .insert_item(&file("orphan", Some("never-seen"), "o", 0))
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

// -------------------------------------------------------------------- deletes

fn range_rows(fx: &Fixture) -> i64 {
    raw(&fx.path())
        .query_row("SELECT COUNT(*) FROM hydrated_ranges", [], |r| r.get(0))
        .unwrap()
}

/// WP-006 AC5: deleting an item removes its hydrated ranges.
#[test]
fn ac5_delete_removes_the_items_ranges() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    store.record_hydrated_range(&key("f"), r(0, 50)).unwrap();
    assert_eq!(
        store
            .delete_item(&key("f"), DeleteMode::ItemOnly, REFUSE)
            .unwrap(),
        1
    );
    assert_eq!(store.item(&key("f")).unwrap(), None);
    assert_eq!(range_rows(&fx), 0);
}

/// WP-006 AC5: a folder with children is not deleted unless the caller asks for a
/// recursive delete, and the refusal changes nothing.
#[test]
fn ac5_deleting_a_folder_with_children_is_refused_unless_recursive() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .record_hydrated_range(&key("plan"), r(10, 20))
        .unwrap();
    let before = row_snapshot(&fx);

    let err = store
        .delete_item(&key("docs"), DeleteMode::ItemOnly, REFUSE)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::HasChildren { children: 1, .. }),
        "{err}"
    );
    assert_eq!(row_snapshot(&fx), before, "refused delete changed nothing");
    assert_eq!(store.hydrated_ranges(&key("plan")).unwrap().len(), 1);

    assert_eq!(
        store
            .delete_item(&key("other"), DeleteMode::ItemOnly, REFUSE)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .delete_item(&key("docs"), DeleteMode::Recursive, REFUSE)
            .unwrap(),
        3
    );
    for gone in ["docs", "work", "plan"] {
        assert_eq!(store.item(&key(gone)).unwrap(), None, "{gone}");
    }
    assert!(store.item(&key(ROOT)).unwrap().is_some());
    assert_eq!(range_rows(&fx), 0);
}

/// Review finding (blocking): deleting an item with local changes not on the server is
/// refused, for a single item and for any descendant of a recursive delete. The error
/// names every offending key, and nothing changes.
#[test]
fn delete_refuses_items_with_pending_local_changes() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .insert_item(&file("c1", Some("work"), "c1", 1))
        .unwrap();
    store.record_hydrated_range(&key("plan"), r(0, 10)).unwrap();
    store
        .set_sync(&key("plan"), SyncState::InSync, SyncState::Uploading)
        .unwrap();
    store
        .set_sync(&key("c1"), SyncState::InSync, SyncState::Conflict)
        .unwrap();
    let before = row_snapshot(&fx);

    let err = store
        .delete_item(&key("docs"), DeleteMode::Recursive, REFUSE)
        .unwrap_err();
    match &err {
        StoreError::HasPendingLocalChanges { pending, .. } => {
            assert_eq!(pending, &[key("c1"), key("plan")]);
        }
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains("plan") && err.to_string().contains("c1"));

    for sync in [
        SyncState::PendingUpload,
        SyncState::Uploading,
        SyncState::Conflict,
    ] {
        store
            .insert_item(&{
                let mut f = file("single", Some(ROOT), "s", 1);
                f.state.sync = sync;
                f
            })
            .unwrap();
        let err = store
            .delete_item(&key("single"), DeleteMode::ItemOnly, REFUSE)
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::HasPendingLocalChanges { pending, .. } if pending == &[key("single")]),
            "{sync}: {err}"
        );
        store
            .delete_item(
                &key("single"),
                DeleteMode::ItemOnly,
                PendingChanges::ServerConfirmedLocalDelete,
            )
            .unwrap();
    }
    assert_eq!(row_snapshot(&fx), before, "refused deletes changed nothing");
    assert_eq!(store.hydrated_ranges(&key("plan")).unwrap(), [r(0, 10)]);
}

/// The explicit opt-in deletes a subtree with pending local changes.
#[test]
fn server_confirmed_local_delete_removes_pending_items() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .set_sync(&key("plan"), SyncState::InSync, SyncState::PendingUpload)
        .unwrap();
    store.record_hydrated_range(&key("plan"), r(0, 10)).unwrap();
    assert_eq!(
        store
            .delete_item(
                &key("docs"),
                DeleteMode::Recursive,
                PendingChanges::ServerConfirmedLocalDelete
            )
            .unwrap(),
        3
    );
    assert_eq!(store.item(&key("plan")).unwrap(), None);
    assert_eq!(range_rows(&fx), 0);
}

/// A recursive delete terminates on a parent cycle and only removes the subtree.
#[test]
fn recursive_delete_terminates_on_a_cycle() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&folder("a", Some("b"), "a")).unwrap();
    store.insert_item(&folder("b", Some("a"), "b")).unwrap();
    store
        .insert_item(&file("keep", Some(ROOT), "keep", 0))
        .unwrap();
    assert_eq!(
        store
            .delete_item(&key("a"), DeleteMode::Recursive, REFUSE)
            .unwrap(),
        2
    );
    assert!(store.item(&key("keep")).unwrap().is_some());
}

#[test]
fn delete_of_unknown_item_is_an_error() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let err = store
        .delete_item(&key("absent"), DeleteMode::Recursive, REFUSE)
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
}
