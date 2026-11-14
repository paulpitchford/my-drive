//! Items: keys, columns, setters, remote metadata, re-keying, paths and deletes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    DIRTY_SYNC_STATES, Fixture, ROOT, file, folder, key, make_dirty, make_full, mount_id,
    provisional, raw, remote,
};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{
    ByteRange, DeleteMode, Item, ItemKind, PendingChanges, Queries, RemoteApplied, RemoteIdentity,
    RemoteMetadata, Store, StoreError, UploadCompleted,
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

    let mut local = provisional("local-1", Some(ROOT), "new.txt", 0);
    local.local_generation = 3;
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
    store.set_last_error(&item.key, Some("boom")).unwrap();
    item.last_error = Some("boom".to_owned());
    store
        .set_sync(&item.key, SyncState::InSync, SyncState::PendingUpload)
        .unwrap();
    item.state.sync = SyncState::PendingUpload;
    store.move_item_local(&item.key, None, "moved").unwrap();
    item.parent_item_id = None;
    item.name = "moved".to_owned();
    assert_eq!(store.item(&item.key).unwrap(), Some(item.clone()));

    // Local content changes mark the item dirty and bump the generation; the file is
    // hydrated first, as the engine must.
    store.record_hydrated_range(&item.key, r(0, 10)).unwrap();
    assert_eq!(store.mark_local_change(&item.key).unwrap(), 1);
    assert_eq!(store.set_local_size(&item.key, 20).unwrap(), 2);
    // The base version's identity (size still 10) was recorded when it became dirty.
    item.last_remote = Some(RemoteIdentity {
        etag: item.etag.clone(),
        ctag: None,
        hash: None,
        size: 10,
    });
    item.size = 20;
    item.local_dirty = true;
    item.local_generation = 2;
    assert_eq!(store.item(&item.key).unwrap(), Some(item));

    let absent = key("absent");
    assert!(matches!(
        store.set_pin(&absent, Pin::Pinned).unwrap_err(),
        StoreError::ItemNotFound { .. }
    ));
    assert!(matches!(
        store.mark_local_change(&absent).unwrap_err(),
        StoreError::ItemNotFound { .. }
    ));
    assert!(matches!(
        store.move_item_local(&absent, None, "x").unwrap_err(),
        StoreError::ItemNotFound { .. }
    ));
}

/// Lead ruling 8: a local move writes parent and name only, works on a provisional item,
/// and validates the name.
#[test]
fn local_move_works_on_provisional_items_and_validates_names() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&folder("d", Some(ROOT), "d")).unwrap();
    let tmp = provisional("tmp", Some(ROOT), "draft.txt", 5);
    store.insert_item(&tmp).unwrap();
    store
        .move_item_local(&tmp.key, Some(&ItemId::new("d").unwrap()), "final.txt")
        .unwrap();
    assert_eq!(path_of(&store, "tmp"), "d/final.txt");
    let err = store.move_item_local(&tmp.key, None, "a/b").unwrap_err();
    assert!(matches!(err, StoreError::InvalidName { .. }), "{err}");
    let stored = store.item(&tmp.key).unwrap().unwrap();
    assert_eq!(stored.name, "final.txt");
    assert!(stored.local_dirty && stored.is_provisional);
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
fn hydrated(store: &Store, sync: SyncState, dirty: bool) -> Item {
    let mut item = file("f", Some(ROOT), "f.txt", 100);
    item.server_hash = hash(1);
    item.local_hash = hash(1);
    item.state = ItemState {
        hydration: Hydration::Full,
        pin: Pin::Pinned,
        sync,
    };
    item.last_error = Some("old".to_owned());
    if dirty {
        item.state.sync = SyncState::InSync;
    }
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 100)).unwrap();
    if dirty {
        make_dirty(store, &item.key, sync);
    }
    store.item(&item.key).unwrap().unwrap()
}

/// Second review B1, the reviewer's sequence: in-sync 1000-byte file, the user appends to
/// 2000 bytes (pending), then a remote rename arrives reporting the old size 1000. The
/// row must still say 2000 bytes with ranges to 2000, under the new name.
#[test]
fn review_b1_remote_rename_does_not_shrink_a_locally_grown_file() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f", Some(ROOT), "notes.txt", 1000);
    item.server_hash = hash(1);
    item.state.hydration = Hydration::Full;
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 1000)).unwrap();

    // The user appends 1000 bytes.
    store.mark_local_change(&item.key).unwrap();
    store.set_local_size(&item.key, 2000).unwrap();

    // A remote rename: same content (same hash), old size, new eTag.
    let mut rename = remote(&item);
    rename.name = "renamed.txt".to_owned();
    rename.etag = Some("etag-after-rename".to_owned());
    rename.modified = mydrive_store::Timestamp::from_unix_seconds(1_800_000_000);
    assert_eq!(
        store.apply_remote_metadata(&rename).unwrap(),
        RemoteApplied::ContentUnchanged
    );

    let stored = store.item(&item.key).unwrap().unwrap();
    assert_eq!(stored.name, "renamed.txt");
    assert_eq!(stored.size, 2000, "size still describes the local content");
    assert_eq!(stored.modified, item.modified, "timestamps untouched");
    assert_eq!(stored.etag, item.etag, "old eTag kept for If-Match");
    assert!(stored.local_dirty && !stored.remote_changed);
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 2000)]);
    assert!(store.is_range_hydrated(&item.key, r(0, 2000)).unwrap());
}

/// Lead ruling 3: on a clean item, unchanged content writes every server-owned column and
/// nothing local.
#[test]
fn remote_metadata_without_content_change_writes_server_columns_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::PendingUpload, false);
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
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 100)]);
}

/// Lead rulings 3 and 4: a content change on a clean item, whatever its sync state, clears
/// the ranges and sets hydration to placeholder in the same operation, so ranges never
/// describe a version the hashes do not. Local columns other than hydration are kept.
#[test]
fn remote_content_change_on_a_clean_item_drops_its_ranges() {
    for &sync in SyncState::ALL {
        let fx = Fixture::new();
        let store = fx.open_with_mount();
        let item = hydrated(&store, sync, false);
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

/// Second review B1: a content change on a dirty item, whatever its sync state, keeps
/// ranges, local state and every content column (so the stale eTag still makes the upload
/// fail with 412), takes the new parent and name, sets the durable `remote_changed` flag
/// (it survives a reopen) and tells the caller.
#[test]
fn remote_content_change_on_a_dirty_item_keeps_everything_and_reports() {
    for sync in DIRTY_SYNC_STATES {
        let fx = Fixture::new();
        let store = fx.open_with_mount();
        store.insert_item(&folder("d", Some(ROOT), "d")).unwrap();
        let item = hydrated(&store, sync, true);
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
            remote_changed: true,
            last_remote: Some(update.identity()),
            ..item.clone()
        };
        assert_eq!(stored, expected, "{sync}");
        assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 100)]);
        drop(store);
        assert!(fx.open().item(&item.key).unwrap().unwrap().remote_changed);
    }
}

/// Without hashes, the cTag decides when both sides have one: a rename changes the eTag
/// only and is not a content change.
#[test]
fn without_hashes_the_ctag_is_preferred_over_the_etag() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f", Some(ROOT), "f", 100);
    item.ctag = Some("ctag-1".to_owned());
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 10)).unwrap();

    let mut rename = remote(&item);
    rename.etag = Some("etag-renamed".to_owned());
    rename.name = "g".to_owned();
    assert_eq!(
        store.apply_remote_metadata(&rename).unwrap(),
        RemoteApplied::ContentUnchanged
    );
    let mut edit = rename.clone();
    edit.ctag = Some("ctag-2".to_owned());
    assert_eq!(
        store.apply_remote_metadata(&edit).unwrap(),
        RemoteApplied::ContentReplaced { ranges_cleared: 1 }
    );
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

/// Second review B3 with the fourth-review ruling: `truncate_local` marks the change, sets
/// the size and leaves the ranges exactly `[0, size)`.
#[test]
fn truncate_local_keeps_the_whole_file_range() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    full_file(&store, "f", 100);

    assert_eq!(store.truncate_local(&key("f"), 50).unwrap(), 1);
    let stored = store.item(&key("f")).unwrap().unwrap();
    assert_eq!((stored.size, stored.local_dirty), (50, true));
    assert_eq!(store.hydrated_ranges(&key("f")).unwrap(), [r(0, 50)]);
    assert_eq!(store.truncate_local(&key("f"), 80).unwrap(), 2);
    assert_eq!(store.hydrated_ranges(&key("f")).unwrap(), [r(0, 80)]);
    assert_eq!(store.truncate_local(&key("f"), 0).unwrap(), 3);
    assert!(store.hydrated_ranges(&key("f")).unwrap().is_empty());
    let err = store.truncate_local(&key(ROOT), 0).unwrap_err();
    assert!(matches!(err, StoreError::NotAFile { .. }), "{err}");
}

/// A clean, fully local 100-byte file: hydration `full`, ranges `[0, 100)`.
fn full_file(store: &Store, id: &str, size: u64) -> Item {
    let mut item = file(id, Some(ROOT), id, size);
    item.state.hydration = Hydration::Full;
    store.insert_item(&item).unwrap();
    if size > 0 {
        store.record_hydrated_range(&item.key, r(0, size)).unwrap();
    }
    item
}

/// Fourth review, blocking (a), the reviewer's sequence: a clean placeholder of size 100
/// is truncated to 0 (`O_TRUNC`, no download), then grown to 50 by a write. The file must
/// be fully local, with `[0, 50)` present, so no "hydrate the missing range" can
/// download the old version over the 50 new bytes.
#[test]
fn review_blocking_a_truncate_then_grow_stays_fully_local() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    assert_eq!(
        store.item(&key("f")).unwrap().unwrap().state.hydration,
        Hydration::Placeholder
    );

    store.truncate_local(&key("f"), 0).unwrap();
    let stored = store.item(&key("f")).unwrap().unwrap();
    assert_eq!(
        (stored.size, stored.state.hydration, stored.local_dirty),
        (0, Hydration::Full, true)
    );
    store.set_local_size(&key("f"), 50).unwrap();
    let stored = store.item(&key("f")).unwrap().unwrap();
    assert_eq!((stored.size, stored.state.hydration), (50, Hydration::Full));
    assert!(store.is_range_hydrated(&key("f"), r(0, 50)).unwrap());
    assert_eq!(store.hydrated_ranges(&key("f")).unwrap(), [r(0, 50)]);
}

/// Fourth review, blocking (b), the reviewer's sequence: a full 100-byte file grown to
/// 200 bytes has all 200 bytes recorded as present.
#[test]
fn review_blocking_b_growing_a_full_file_records_the_new_bytes() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    full_file(&store, "f", 100);
    store.set_local_size(&key("f"), 200).unwrap();
    let stored = store.item(&key("f")).unwrap().unwrap();
    assert_eq!(
        (stored.size, stored.state.hydration),
        (200, Hydration::Full)
    );
    assert!(store.is_range_hydrated(&key("f"), r(100, 200)).unwrap());
    assert_eq!(store.hydrated_ranges(&key("f")).unwrap(), [r(0, 200)]);
}

/// Fourth review, blocking: a local change to a file that is not fully local is refused
/// (the engine hydrates first, ADR-0012 R1), and changes nothing; only a truncate to 0
/// works on any file. A dirty file cannot take a separate range record.
#[test]
fn local_changes_need_a_fully_local_file() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    // A placeholder, a partial file, and a "full" file whose ranges do not cover it.
    store.insert_item(&file("p", Some(ROOT), "p", 100)).unwrap();
    let mut partial = file("q", Some(ROOT), "q", 100);
    partial.state.hydration = Hydration::Partial;
    store.insert_item(&partial).unwrap();
    store.record_hydrated_range(&partial.key, r(0, 40)).unwrap();
    let mut lying = file("l", Some(ROOT), "l", 100);
    lying.state.hydration = Hydration::Full;
    store.insert_item(&lying).unwrap();

    for id in ["p", "q", "l"] {
        let before = store.item(&key(id)).unwrap();
        let ranges = store.hydrated_ranges(&key(id)).unwrap();
        let errs = [
            store.mark_local_change(&key(id)).unwrap_err(),
            store.set_local_size(&key(id), 100).unwrap_err(),
            store.set_local_size(&key(id), 150).unwrap_err(),
            store.truncate_local(&key(id), 10).unwrap_err(),
        ];
        for err in errs {
            assert!(
                matches!(err, StoreError::NotFullyLocal { .. }),
                "{id}: {err}"
            );
        }
        assert_eq!(store.item(&key(id)).unwrap(), before, "{id}");
        assert_eq!(store.hydrated_ranges(&key(id)).unwrap(), ranges, "{id}");
    }

    let item = full_file(&store, "f", 100);
    store.mark_local_change(&item.key).unwrap();
    let err = store
        .record_hydrated_range(&item.key, r(0, 10))
        .unwrap_err();
    assert!(matches!(err, StoreError::LocalChanges { .. }), "{err}");
    drop(store);
    let refused = raw(&fx.path()).execute(
        "UPDATE items SET hydration = 'partial' WHERE item_id = 'f'",
        [],
    );
    assert!(refused.is_err(), "the schema CHECK keeps a dirty file full");
}

// ---------------------------------------------------- dehydration and discard

/// Second review B3: dehydration is one atomic operation, refused for a dirty item or one
/// that is not in-sync, and changes nothing when refused.
#[test]
fn dehydrate_is_atomic_and_refused_for_dirty_or_unsynced_items() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::InSync, false);
    store.dehydrate(&item.key).unwrap();
    let stored = store.item(&item.key).unwrap().unwrap();
    assert_eq!(stored.state.hydration, Hydration::Placeholder);
    assert!(store.hydrated_ranges(&item.key).unwrap().is_empty());

    for &sync in SyncState::ALL {
        for dirty in [false, true] {
            if (sync == SyncState::InSync && !dirty)
                || (dirty && !DIRTY_SYNC_STATES.contains(&sync))
            {
                continue;
            }
            let fx = Fixture::new();
            let store = fx.open_with_mount();
            let item = hydrated(&store, sync, dirty);
            let err = store.dehydrate(&item.key).unwrap_err();
            if dirty {
                assert!(matches!(err, StoreError::LocalChanges { .. }), "{err}");
            } else {
                assert!(matches!(err, StoreError::NotInSync { .. }), "{err}");
            }
            assert_eq!(store.item(&item.key).unwrap(), Some(item.clone()));
            assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 100)]);
        }
    }
}

/// Second review B3: on a dirty item, clearing ranges or lowering hydration is refused,
/// since either would claim its unsynced bytes are gone.
#[test]
fn dirty_items_refuse_clearing_ranges_and_lowering_hydration() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::Error, true);
    let err = store.clear_hydrated_ranges(&item.key).unwrap_err();
    assert!(matches!(err, StoreError::LocalChanges { .. }), "{err}");
    for hydration in [Hydration::Placeholder, Hydration::Partial] {
        let err = store.set_hydration(&item.key, hydration).unwrap_err();
        assert!(matches!(err, StoreError::LocalChanges { .. }), "{err}");
    }
    store.set_hydration(&item.key, Hydration::Full).unwrap();
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 100)]);
}

/// Third review F2, the reviewer's sequence: a 100-byte file is truncated to 0 locally
/// and the change is then discarded. The item must come back as a clean placeholder of
/// the server's version (100 bytes), never as an empty in-sync file.
#[test]
fn review_f2_discard_restores_the_server_version_not_the_local_size() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut item = file("f", Some(ROOT), "f", 100);
    item.server_hash = hash(1);
    item.state.hydration = Hydration::Full;
    store.insert_item(&item).unwrap();
    store.record_hydrated_range(&item.key, r(0, 100)).unwrap();
    store.truncate_local(&item.key, 0).unwrap();

    let current = remote(&item);
    store.discard_local_changes(&item.key, &current).unwrap();
    let stored = store.item(&item.key).unwrap().unwrap();
    assert_eq!(stored.size, 100, "the server's size, not the local 0");
    assert_eq!(stored.server_hash, hash(1));
    assert_eq!(stored.state.hydration, Hydration::Placeholder);
    assert_eq!(stored.state.sync, SyncState::InSync);
    assert!(!stored.local_dirty && !stored.remote_changed && stored.last_remote.is_none());
    assert!(store.hydrated_ranges(&item.key).unwrap().is_empty());
    assert!(!store.is_range_hydrated(&item.key, r(0, 1)).unwrap());
}

/// F2: discard needs metadata for the same item, and is refused on a provisional item
/// (it has no server version); nothing changes when refused.
#[test]
fn discard_refusals_change_nothing() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::Conflict, true);
    let mut other = remote(&item);
    other.key = key("other");
    let err = store.discard_local_changes(&item.key, &other).unwrap_err();
    assert!(matches!(err, StoreError::UploadMismatch { .. }), "{err}");
    assert_eq!(store.item(&item.key).unwrap(), Some(item.clone()));

    let tmp = provisional("tmp", Some(ROOT), "t", 5);
    store.insert_item(&tmp).unwrap();
    let err = store
        .discard_local_changes(&tmp.key, &remote(&tmp))
        .unwrap_err();
    assert!(matches!(err, StoreError::ProvisionalItem { .. }), "{err}");
    assert_eq!(store.item(&tmp.key).unwrap(), Some(tmp));
}

// ------------------------------------------------------------------- uploads

fn upload_response(item: &Item, etag: &str, server: u8, size: u64) -> RemoteMetadata {
    RemoteMetadata {
        etag: Some(etag.to_owned()),
        server_hash: hash(server),
        size,
        ..remote(item)
    }
}

/// N1: an upload of the current generation makes the item clean and in-sync and records
/// the response's server columns and the uploaded hash.
#[test]
fn complete_upload_of_the_current_generation_syncs_the_item() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&item.key).unwrap();
    store
        .set_sync(&item.key, SyncState::PendingUpload, SyncState::Uploading)
        .unwrap();
    let response = upload_response(&item, "etag-up", 5, 100);

    assert_eq!(
        store
            .complete_upload(&item.key, generation, hash(4).unwrap(), &response, None)
            .unwrap(),
        UploadCompleted::Synced
    );
    let stored = store.item(&item.key).unwrap().unwrap();
    assert!(!stored.local_dirty && !stored.remote_changed);
    assert_eq!(stored.state.sync, SyncState::InSync);
    assert_eq!(stored.etag.as_deref(), Some("etag-up"));
    assert_eq!((stored.local_hash, stored.server_hash), (hash(4), hash(5)));
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 100)]);
}

/// N1, the ABA case: upload 1 (generation 1) is in flight when the user writes again
/// (generation 2). Completing upload 1 records its eTag and hashes but must not mark
/// version 2 as synced; the item stays dirty and pending. Completing upload 2 then syncs.
#[test]
fn complete_upload_of_an_older_generation_keeps_the_item_dirty() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::InSync, false);
    let first = store.mark_local_change(&item.key).unwrap();
    store
        .set_sync(&item.key, SyncState::PendingUpload, SyncState::Uploading)
        .unwrap();
    // The user appends while upload 1 is in flight.
    let second = store.set_local_size(&item.key, 150).unwrap();
    assert!(second > first);

    let response_1 = upload_response(&item, "etag-v1", 6, 100);
    assert_eq!(
        store
            .complete_upload(&item.key, first, hash(6).unwrap(), &response_1, None)
            .unwrap(),
        UploadCompleted::LocalChangedSince { generation: second }
    );
    let stored = store.item(&item.key).unwrap().unwrap();
    assert!(stored.local_dirty, "version 2 is not on the server");
    assert_eq!(stored.state.sync, SyncState::PendingUpload);
    assert_eq!(stored.size, 150, "size still describes version 2");
    assert_eq!(
        stored.etag.as_deref(),
        Some("etag-v1"),
        "next If-Match is v1"
    );
    assert_eq!(store.hydrated_ranges(&item.key).unwrap(), [r(0, 150)]);
    let err = store
        .delete_item(&item.key, DeleteMode::ItemOnly, REFUSE)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::HasPendingLocalChanges { .. }),
        "{err}"
    );

    let response_2 = upload_response(&item, "etag-v2", 7, 150);
    assert_eq!(
        store
            .complete_upload(&item.key, second, hash(7).unwrap(), &response_2, None)
            .unwrap(),
        UploadCompleted::Synced
    );
    let stored = store.item(&item.key).unwrap().unwrap();
    assert!(!stored.local_dirty);
    assert_eq!(stored.etag.as_deref(), Some("etag-v2"));
}

/// N1: completing the upload of a provisional item gives it the server ID, moving its
/// ranges and children, in the same operation.
#[test]
fn complete_upload_rekeys_a_provisional_item() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let tmp = provisional("tmp-1", Some(ROOT), "new.txt", 10);
    store.insert_item(&tmp).unwrap();
    store
        .set_sync(&tmp.key, SyncState::PendingUpload, SyncState::Uploading)
        .unwrap();
    let tmp = store.item(&tmp.key).unwrap().unwrap();

    let server_id = ItemId::new("srv-1").unwrap();
    let response = RemoteMetadata {
        key: key("srv-1"),
        ..upload_response(&tmp, "etag-1", 8, 10)
    };
    let err = store
        .complete_upload(&tmp.key, 1, hash(8).unwrap(), &response, None)
        .unwrap_err();
    assert!(matches!(err, StoreError::UploadMismatch { .. }), "{err}");
    assert_eq!(store.item(&tmp.key).unwrap(), Some(tmp.clone()));

    assert_eq!(
        store
            .complete_upload(&tmp.key, 1, hash(8).unwrap(), &response, Some(&server_id))
            .unwrap(),
        UploadCompleted::Synced
    );
    assert_eq!(store.item(&tmp.key).unwrap(), None);
    let stored = store.item(&key("srv-1")).unwrap().unwrap();
    assert!(!stored.is_provisional && !stored.local_dirty);
    assert_eq!(store.hydrated_ranges(&key("srv-1")).unwrap(), [r(0, 10)]);
}

/// N4: if the server item arrived (from a delta) before the upload was recorded, the
/// re-key is refused and nothing changes; the documented recovery is to delete the clean
/// server row and complete the upload again.
#[test]
fn complete_upload_refuses_when_the_server_item_is_already_stored() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let tmp = provisional("tmp-1", Some(ROOT), "new.txt", 10);
    store.insert_item(&tmp).unwrap();
    store
        .insert_item(&file("srv-1", Some(ROOT), "new.txt", 10))
        .unwrap();

    let server_id = ItemId::new("srv-1").unwrap();
    let response = RemoteMetadata {
        key: key("srv-1"),
        ..upload_response(&tmp, "etag-1", 8, 10)
    };
    let err = store
        .complete_upload(&tmp.key, 1, hash(8).unwrap(), &response, Some(&server_id))
        .unwrap_err();
    assert!(matches!(err, StoreError::ItemExists { .. }), "{err}");
    assert_eq!(store.item(&tmp.key).unwrap(), Some(tmp.clone()));

    // Recovery (store-schema.md): the server row is clean, so it can be deleted.
    store
        .delete_item(&key("srv-1"), DeleteMode::ItemOnly, REFUSE)
        .unwrap();
    store
        .complete_upload(&tmp.key, 1, hash(8).unwrap(), &response, Some(&server_id))
        .unwrap();
    assert!(!store.item(&key("srv-1")).unwrap().unwrap().local_dirty);
}

/// A provisional folder `tmp-dir` under the root holding a provisional file `tmp-child`
/// with one hydrated range, both stored, and the upload response for the folder naming
/// the server item `srv-dir`.
fn provisional_folder_with_child(store: &Store) -> (Item, Item, RemoteMetadata) {
    let mut dir = provisional("tmp-dir", Some(ROOT), "new folder", 0);
    dir.kind = ItemKind::Folder;
    store.insert_item(&dir).unwrap();
    let child = provisional("tmp-child", Some("tmp-dir"), "a.txt", 10);
    store.insert_item(&child).unwrap();
    assert_eq!(store.hydrated_ranges(&child.key).unwrap(), [r(0, 10)]);
    let response = RemoteMetadata {
        key: key("srv-dir"),
        ..upload_response(&dir, "etag-dir", 3, 0)
    };
    (dir, child, response)
}

/// Every row of `items` and `hydrated_ranges`, read through a second connection, so a
/// test can show that a refused operation left the committed database exactly as it was.
fn snapshot(path: &std::path::Path) -> Vec<Vec<rusqlite::types::Value>> {
    let conn = raw(path);
    let mut rows = Vec::new();
    for sql in [
        "SELECT * FROM items ORDER BY drive_id, item_id",
        "SELECT * FROM hydrated_ranges ORDER BY drive_id, item_id, range_start",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let width = stmt.column_count();
        let table: Vec<Vec<rusqlite::types::Value>> = stmt
            .query_map([], |row| {
                (0..width)
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect()
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.extend(table);
        rows.push(Vec::new()); // separates the two tables
    }
    rows
}

/// The responses `complete_upload` refuses only after it has re-keyed the item: one
/// naming another item, one with an invalid name, one with a size SQLite cannot store.
fn refused_after_rekey(response: &RemoteMetadata) -> [(RemoteMetadata, &'static str); 3] {
    [
        (
            RemoteMetadata {
                key: key("someone-else"),
                ..response.clone()
            },
            "UploadMismatch",
        ),
        (
            RemoteMetadata {
                name: "a/b".to_owned(),
                ..response.clone()
            },
            "InvalidName",
        ),
        (
            RemoteMetadata {
                size: u64::MAX,
                ..response.clone()
            },
            "SizeTooLarge",
        ),
    ]
}

fn assert_refusal(err: &StoreError, expected: &str) {
    let ok = match expected {
        "UploadMismatch" => matches!(err, StoreError::UploadMismatch { .. }),
        "InvalidName" => matches!(err, StoreError::InvalidName { .. }),
        "SizeTooLarge" => matches!(err, StoreError::SizeTooLarge { .. }),
        _ => false,
    };
    assert!(ok, "expected {expected}, got {err}");
}

/// WP-013: `complete_upload` is atomic. A refusal that comes after the re-key step (the
/// response names another item, has an invalid name, or a size too large to store)
/// leaves nothing changed: the provisional item keeps its key, its child keeps its
/// parent link and ranges, and no server row appears.
#[test]
fn complete_upload_refused_after_the_rekey_changes_nothing() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let (dir, child, response) = provisional_folder_with_child(&store);
    let server_id = ItemId::new("srv-dir").unwrap();
    let before = snapshot(&fx.path());

    for (bad, expected) in refused_after_rekey(&response) {
        let err = store
            .complete_upload(&dir.key, 1, hash(3).unwrap(), &bad, Some(&server_id))
            .unwrap_err();
        assert_refusal(&err, expected);
        assert_eq!(snapshot(&fx.path()), before, "after {expected}");
        assert_eq!(store.item(&dir.key).unwrap(), Some(dir.clone()));
        assert_eq!(store.item(&child.key).unwrap(), Some(child.clone()));
        assert_eq!(store.item(&key("srv-dir")).unwrap(), None);
        assert_eq!(store.hydrated_ranges(&child.key).unwrap(), [r(0, 10)]);
    }

    // The same refusals inside a caller's transaction: each is undone on its own, the
    // transaction stays usable, and committing it applies only what succeeded.
    let mut store = store;
    let tx = store.transaction().unwrap();
    for (bad, expected) in refused_after_rekey(&response) {
        let err = tx
            .complete_upload(&dir.key, 1, hash(3).unwrap(), &bad, Some(&server_id))
            .unwrap_err();
        assert_refusal(&err, expected);
        assert_eq!(tx.item(&dir.key).unwrap(), Some(dir.clone()));
        assert_eq!(tx.item(&child.key).unwrap(), Some(child.clone()));
        assert_eq!(tx.item(&key("srv-dir")).unwrap(), None);
    }
    tx.commit().unwrap();
    assert_eq!(snapshot(&fx.path()), before);

    // Nothing was left half-done: the valid response still completes.
    assert_eq!(
        store
            .complete_upload(&dir.key, 1, hash(3).unwrap(), &response, Some(&server_id))
            .unwrap(),
        UploadCompleted::Synced
    );
}

/// WP-013, N1: the re-key inside `complete_upload` moves a child with it. The provisional
/// folder gets the server ID; its child's parent link follows and the child keeps its
/// ranges under its own key; no range or parent link names the provisional ID afterwards.
#[test]
fn complete_upload_rekey_moves_a_child_and_keeps_its_ranges() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let (dir, child, response) = provisional_folder_with_child(&store);
    let server_id = ItemId::new("srv-dir").unwrap();

    assert_eq!(
        store
            .complete_upload(&dir.key, 1, hash(3).unwrap(), &response, Some(&server_id))
            .unwrap(),
        UploadCompleted::Synced
    );
    assert_eq!(store.item(&dir.key).unwrap(), None);
    let new_dir = store.item(&key("srv-dir")).unwrap().unwrap();
    assert!(!new_dir.is_provisional && !new_dir.local_dirty);
    assert_eq!(new_dir.kind, ItemKind::Folder);

    let stored_child = store.item(&child.key).unwrap().unwrap();
    assert_eq!(
        stored_child.parent_item_id.as_ref().map(ItemId::as_str),
        Some("srv-dir")
    );
    assert_eq!(
        Item {
            parent_item_id: Some(server_id.clone()),
            ..child.clone()
        },
        stored_child,
        "only the child's parent link changes"
    );
    assert_eq!(store.hydrated_ranges(&child.key).unwrap(), [r(0, 10)]);
    assert_eq!(store.children(&key("srv-dir")).unwrap(), [stored_child]);
    assert!(store.children(&dir.key).unwrap().is_empty());
    assert_eq!(path_of(&store, "tmp-child"), "new folder/a.txt");

    drop(store);
    let stale: i64 = raw(&fx.path())
        .query_row(
            "SELECT (SELECT COUNT(*) FROM items WHERE parent_item_id = 'tmp-dir') + \
             (SELECT COUNT(*) FROM hydrated_ranges WHERE item_id = 'tmp-dir')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
}

/// Third review F1, the reviewer's sequence: clean V1, local change, uploading; the
/// server accepts our V2; then a delta reports V3 from another client; then our V2
/// upload completes. The item must not end clean and in-sync at V2: our bytes reached the
/// server (clean), but it is `pending-download` with `remote_changed` kept, and the result
/// says so. Dehydration stays refused.
#[test]
fn review_f1_remote_change_during_an_upload_is_not_forgotten() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let v1 = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&v1.key).unwrap();
    store
        .set_sync(&v1.key, SyncState::PendingUpload, SyncState::Uploading)
        .unwrap();
    // The server accepted our V2 (response not yet recorded), then V3 arrives by delta.
    let v2 = upload_response(&v1, "etag-v2", 2, 100);
    let v3 = upload_response(&v1, "etag-v3", 3, 120);
    assert_eq!(
        store.apply_remote_metadata(&v3).unwrap(),
        RemoteApplied::ContentChangedUnderLocalState {
            sync: SyncState::Uploading
        }
    );

    assert_eq!(
        store
            .complete_upload(&v1.key, generation, hash(2).unwrap(), &v2, None)
            .unwrap(),
        UploadCompleted::UploadedButRemoteMovedOn
    );
    let stored = store.item(&v1.key).unwrap().unwrap();
    assert!(!stored.local_dirty, "our bytes reached the server");
    assert!(stored.remote_changed, "the server has moved on: kept");
    assert_eq!(stored.state.sync, SyncState::PendingDownload);
    assert_eq!(stored.etag.as_deref(), Some("etag-v2"));
    let err = store.dehydrate(&v1.key).unwrap_err();
    assert!(matches!(err, StoreError::NotInSync { .. }), "{err}");

    // Applying the current metadata (V3) to the now clean item replaces its content.
    assert_eq!(
        store.apply_remote_metadata(&v3).unwrap(),
        RemoteApplied::ContentReplaced { ranges_cleared: 1 }
    );
    let stored = store.item(&v1.key).unwrap().unwrap();
    assert!(!stored.remote_changed);
    assert_eq!((stored.size, stored.server_hash), (120, hash(3)));
}

/// Fourth review, finding 2, the reviewer's sequence: the delta reports our V2 already
/// renamed on the server (new name, eTag e2b); then `complete_upload` arrives with the
/// older response (old name, eTag e2). The newer name and eTag the store recorded win.
#[test]
fn review_4_2_an_echo_keeps_the_newer_recorded_metadata() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let v1 = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&v1.key).unwrap();
    let response = upload_response(&v1, "e2", 2, 100);
    let renamed_echo = RemoteMetadata {
        name: "renamed.txt".to_owned(),
        etag: Some("e2b".to_owned()),
        ..response.clone()
    };
    store.apply_remote_metadata(&renamed_echo).unwrap();

    assert_eq!(
        store
            .complete_upload(&v1.key, generation, hash(2).unwrap(), &response, None)
            .unwrap(),
        UploadCompleted::Synced
    );
    let stored = store.item(&v1.key).unwrap().unwrap();
    assert_eq!(stored.name, "renamed.txt");
    assert_eq!(stored.etag.as_deref(), Some("e2b"));
    assert!(!stored.local_dirty && !stored.remote_changed);
}

/// Fourth review, finding 3: after `UploadedButRemoteMovedOn`, the item cannot be set
/// in-sync (and so cannot be dehydrated as a stale V2 placeholder) until fresh metadata is
/// applied; applying it clears the flag.
#[test]
fn review_4_3_remote_changed_blocks_in_sync_until_fresh_metadata() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let v1 = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&v1.key).unwrap();
    let v2 = upload_response(&v1, "etag-v2", 2, 100);
    let v3 = upload_response(&v1, "etag-v3", 3, 100);
    store.apply_remote_metadata(&v3).unwrap();
    assert_eq!(
        store
            .complete_upload(&v1.key, generation, hash(2).unwrap(), &v2, None)
            .unwrap(),
        UploadCompleted::UploadedButRemoteMovedOn
    );

    let err = store
        .set_sync(&v1.key, SyncState::PendingDownload, SyncState::InSync)
        .unwrap_err();
    assert!(matches!(err, StoreError::RemoteChanged { .. }), "{err}");
    let err = store.dehydrate(&v1.key).unwrap_err();
    assert!(matches!(err, StoreError::NotInSync { .. }), "{err}");
    drop(store);
    let refused =
        raw(&fx.path()).execute("UPDATE items SET sync = 'in-sync' WHERE item_id = 'f'", []);
    assert!(refused.is_err(), "the schema CHECK refuses it too");

    let store = fx.open();
    store.apply_remote_metadata(&v3).unwrap();
    assert!(!store.item(&v1.key).unwrap().unwrap().remote_changed);
    store
        .set_sync(&v1.key, SyncState::PendingDownload, SyncState::InSync)
        .unwrap();
}

/// Fourth review, finding 4: discard is refused when the server has reported a newer
/// version and the supplied metadata is still the stored base version (stale); fresh
/// metadata is accepted.
#[test]
fn review_4_4_discard_refuses_stale_metadata() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let base = hydrated(&store, SyncState::InSync, false);
    store.mark_local_change(&base.key).unwrap();
    let newer = upload_response(&base, "etag-v3", 3, 120);
    store.apply_remote_metadata(&newer).unwrap();
    let before = store.item(&base.key).unwrap();

    let err = store
        .discard_local_changes(&base.key, &remote(&base))
        .unwrap_err();
    assert!(matches!(err, StoreError::RemoteChanged { .. }), "{err}");
    assert_eq!(store.item(&base.key).unwrap(), before);

    store.discard_local_changes(&base.key, &newer).unwrap();
    let stored = store.item(&base.key).unwrap().unwrap();
    assert_eq!((stored.size, stored.server_hash), (120, hash(3)));
    assert!(!stored.local_dirty && !stored.remote_changed);
}

/// F1: when the delta seen during the upload is our own upload echoing back, completion
/// is `Synced` as usual.
#[test]
fn delta_echo_of_our_own_upload_still_syncs() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let v1 = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&v1.key).unwrap();
    let v2 = upload_response(&v1, "etag-v2", 2, 100);
    // The delta reports V2 (ours) before the upload response is recorded.
    store.apply_remote_metadata(&v2).unwrap();
    assert!(store.item(&v1.key).unwrap().unwrap().remote_changed);

    assert_eq!(
        store
            .complete_upload(&v1.key, generation, hash(2).unwrap(), &v2, None)
            .unwrap(),
        UploadCompleted::Synced
    );
    let stored = store.item(&v1.key).unwrap().unwrap();
    assert!(!stored.local_dirty && !stored.remote_changed);
    assert_eq!(stored.state.sync, SyncState::InSync);
}

/// Third review F4: a replayed completion on a clean item is refused and changes nothing,
/// so it cannot overwrite newer server identity.
#[test]
fn review_f4_replayed_completion_is_refused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::InSync, false);
    let generation = store.mark_local_change(&item.key).unwrap();
    let v2 = upload_response(&item, "etag-v2", 2, 100);
    store
        .complete_upload(&item.key, generation, hash(2).unwrap(), &v2, None)
        .unwrap();
    // A newer version arrives, then the old completion is replayed.
    let v3 = upload_response(&item, "etag-v3", 3, 100);
    store.apply_remote_metadata(&v3).unwrap();
    let before = store.item(&item.key).unwrap();
    let err = store
        .complete_upload(&item.key, generation, hash(2).unwrap(), &v2, None)
        .unwrap_err();
    assert!(matches!(err, StoreError::NotDirty { .. }), "{err}");
    assert_eq!(store.item(&item.key).unwrap(), before);
}

/// Third review F3, the reviewer's sequence: the mark commits and the process dies before
/// any `set_sync`. Recovery must still find the item: the mark itself moved it to
/// `pending-upload`, and `dirty_items` lists it whatever its sync state.
#[test]
fn review_f3_a_marked_item_is_found_by_recovery() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for (id, sync) in [
        ("a", SyncState::InSync),
        ("b", SyncState::PendingDownload),
        ("c", SyncState::Blocked),
        ("d", SyncState::Error),
    ] {
        let mut f = file(id, Some(ROOT), id, 10);
        f.state.sync = sync;
        store.insert_item(&f).unwrap();
        make_full(&store, &f.key);
        store.mark_local_change(&f.key).unwrap();
    }
    drop(store);
    let store = fx.open();
    let pending: Vec<String> = store
        .items_with_sync(SyncState::PendingUpload)
        .unwrap()
        .into_iter()
        .map(|i| i.key.item_id.to_string())
        .collect();
    assert_eq!(
        pending,
        ["a", "b"],
        "in-sync and pending-download became pending-upload"
    );
    let dirty: Vec<(String, SyncState)> = store
        .dirty_items()
        .unwrap()
        .into_iter()
        .map(|i| (i.key.item_id.to_string(), i.state.sync))
        .collect();
    assert_eq!(
        dirty,
        [
            ("a".to_owned(), SyncState::PendingUpload),
            ("b".to_owned(), SyncState::PendingUpload),
            ("c".to_owned(), SyncState::Blocked),
            ("d".to_owned(), SyncState::Error),
        ]
    );
}

/// F3: a dirty item can never be set `in-sync` or `pending-download`, through the API or
/// by SQL.
#[test]
fn a_dirty_item_cannot_be_marked_in_sync() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::PendingUpload, true);
    for target in [SyncState::InSync, SyncState::PendingDownload] {
        let err = store
            .set_sync(&item.key, SyncState::PendingUpload, target)
            .unwrap_err();
        assert!(matches!(err, StoreError::LocalChanges { .. }), "{err}");
    }
    assert_eq!(store.item(&item.key).unwrap(), Some(item));
    drop(store);
    let refused =
        raw(&fx.path()).execute("UPDATE items SET sync = 'in-sync' WHERE item_id = 'f'", []);
    assert!(refused.is_err(), "the schema CHECK refuses it too");
}

/// F3: `dirty_items` is backed by the partial index on the dirty flag.
#[test]
fn dirty_items_uses_the_dirty_index() {
    let fx = Fixture::new();
    drop(fx.open());
    let plan: Vec<String> = raw(&fx.path())
        .prepare("EXPLAIN QUERY PLAN SELECT * FROM items WHERE local_dirty = 1")
        .unwrap()
        .query_map([], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("items_by_dirty")),
        "{plan:?}"
    );
}

/// Third review F6: provisional implies dirty, in the insert path and in the schema.
#[test]
fn review_f6_provisional_implies_dirty() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for bad in [
        Item {
            local_dirty: false,
            ..provisional("p1", Some(ROOT), "p", 1)
        },
        Item {
            state: ItemState {
                sync: SyncState::InSync,
                ..provisional("p2", Some(ROOT), "p", 1).state
            },
            ..provisional("p2", Some(ROOT), "p", 1)
        },
        Item {
            local_dirty: true,
            ..file("p3", Some(ROOT), "p", 1)
        },
    ] {
        let err = store.insert_item(&bad).unwrap_err();
        assert!(matches!(err, StoreError::InvalidItem { .. }), "{err}");
        assert_eq!(store.item(&bad.key).unwrap(), None);
    }
    store
        .insert_item(&provisional("ok", Some(ROOT), "p", 1))
        .unwrap();
    drop(store);
    let refused = raw(&fx.path()).execute(
        "UPDATE items SET local_dirty = 0, sync = 'uploading' WHERE item_id = 'ok'",
        [],
    );
    assert!(
        refused.is_err(),
        "the schema CHECK refuses a clean provisional item"
    );
}

/// Third review F9: on the hash path the size is compared too, for a clean item and for
/// the identity recorded on a dirty one.
#[test]
fn review_f9_the_hash_path_compares_size() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::InSync, false);
    let mut same_hash_other_size = remote(&item);
    same_hash_other_size.size = 90;
    assert_eq!(
        store.apply_remote_metadata(&same_hash_other_size).unwrap(),
        RemoteApplied::ContentReplaced { ranges_cleared: 1 }
    );

    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let item = hydrated(&store, SyncState::PendingUpload, true);
    let mut report = remote(&item);
    report.size = 100; // the base version's size, though the local size may differ
    assert_eq!(
        store.apply_remote_metadata(&report).unwrap(),
        RemoteApplied::ContentUnchanged
    );
    report.size = 90;
    assert_eq!(
        store.apply_remote_metadata(&report).unwrap(),
        RemoteApplied::ContentChangedUnderLocalState {
            sync: SyncState::PendingUpload
        }
    );
}

// ------------------------------------------------------------------ re-keying

/// Lead ruling on provisional items: re-keying moves the item, its children's parent
/// links and its ranges together, and clears `is_provisional`.
#[test]
fn rekey_moves_item_children_and_ranges() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut dir = provisional("tmp-1", Some(ROOT), "new folder", 0);
    dir.kind = ItemKind::Folder;
    dir.state.hydration = Hydration::Full;
    store.insert_item(&dir).unwrap();
    let child = provisional("tmp-2", Some("tmp-1"), "a.txt", 10);
    store.insert_item(&child).unwrap();

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
    let tmp = provisional("tmp", Some(ROOT), "t", 10);
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
    // Second review B2: dirty items in `error` and `blocked` must be protected too.
    make_dirty(&store, &key("plan"), SyncState::Error);
    make_dirty(&store, &key("c1"), SyncState::Blocked);
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

    for sync in DIRTY_SYNC_STATES {
        store
            .insert_item(&file("single", Some(ROOT), "s", 1))
            .unwrap();
        make_dirty(&store, &key("single"), sync);
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
                PendingChanges::ServerConfirmedLocalDelete {
                    expected_generation: 1,
                },
            )
            .unwrap();
    }
    assert_eq!(row_snapshot(&fx), before, "refused deletes changed nothing");
    assert_eq!(store.hydrated_ranges(&key("plan")).unwrap(), [r(0, 100)]);
}

/// The explicit opt-in deletes the named dirty item, with its ranges.
#[test]
fn server_confirmed_local_delete_removes_the_named_item() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    make_full(&store, &key("plan"));
    let generation = store.mark_local_change(&key("plan")).unwrap();
    assert_eq!(
        store
            .delete_item(
                &key("plan"),
                DeleteMode::ItemOnly,
                PendingChanges::ServerConfirmedLocalDelete {
                    expected_generation: generation
                }
            )
            .unwrap(),
        1
    );
    assert_eq!(store.item(&key("plan")).unwrap(), None);
    assert_eq!(range_rows(&fx), 0);
}

/// Third review F5, the reviewer's sequence: the user deletes folder `docs` locally;
/// meanwhile another client moves a file into it, and that file is dirty here. The server
/// confirms the folder delete. A recursive confirmed delete must not take the dirty
/// newcomer with it: it is refused and named, and nothing changes. A confirmation whose
/// generation is stale is refused too.
#[test]
fn review_f5_confirmed_delete_exempts_only_the_named_item() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    // The user's local delete of `docs` is the dirty change being confirmed.
    let generation = store.mark_local_change(&key("docs")).unwrap();
    // A file another client moved into `docs/work`, with local edits not yet uploaded.
    store
        .insert_item(&file("moved-in", Some("work"), "theirs.txt", 10))
        .unwrap();
    make_dirty(&store, &key("moved-in"), SyncState::PendingUpload);
    let before = row_snapshot(&fx);

    let confirmed = PendingChanges::ServerConfirmedLocalDelete {
        expected_generation: generation,
    };
    let err = store
        .delete_item(&key("docs"), DeleteMode::Recursive, confirmed)
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::HasPendingLocalChanges { pending, .. } if pending == &[key("moved-in")]),
        "{err}"
    );
    assert_eq!(row_snapshot(&fx), before);
    assert_eq!(store.hydrated_ranges(&key("moved-in")).unwrap(), [r(0, 10)]);

    let stale = PendingChanges::ServerConfirmedLocalDelete {
        expected_generation: generation + 1,
    };
    let err = store
        .delete_item(&key("docs"), DeleteMode::Recursive, stale)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::GenerationMismatch { .. }),
        "{err}"
    );
    assert_eq!(row_snapshot(&fx), before);
}

/// F2/F6: a never-uploaded item is removed with its own explicitly named mode, which
/// refuses an item the server knows, and a confirmed server delete refuses a provisional
/// item.
#[test]
fn never_uploaded_items_have_their_own_delete_mode() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let tmp = provisional("tmp", Some(ROOT), "t", 5);
    store.insert_item(&tmp).unwrap();
    store.insert_item(&file("srv", Some(ROOT), "s", 5)).unwrap();

    let err = store
        .delete_item(
            &tmp.key,
            DeleteMode::ItemOnly,
            PendingChanges::ServerConfirmedLocalDelete {
                expected_generation: 1,
            },
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::ProvisionalItem { .. }), "{err}");
    let err = store
        .delete_item(
            &key("srv"),
            DeleteMode::ItemOnly,
            PendingChanges::DiscardNeverUploaded {
                expected_generation: 0,
            },
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::NotProvisional { .. }), "{err}");
    assert_eq!(
        store
            .delete_item(
                &tmp.key,
                DeleteMode::ItemOnly,
                PendingChanges::DiscardNeverUploaded {
                    expected_generation: 1
                },
            )
            .unwrap(),
        1
    );
}

/// A clean item is deletable whatever its sync state: the guard keys on `local_dirty`.
#[test]
fn clean_items_delete_in_any_sync_state() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for &sync in SyncState::ALL {
        let mut f = file("clean", Some(ROOT), "c", 1);
        f.state.sync = sync;
        store.insert_item(&f).unwrap();
        store
            .delete_item(&f.key, DeleteMode::ItemOnly, REFUSE)
            .unwrap();
    }
}

/// Second review B2: a mount's root item cannot be deleted, directly or as part of a
/// recursive delete, even in the server-confirmed mode.
#[test]
fn delete_refuses_a_mount_root() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    tree(&store);
    store
        .insert_mount(&common::mount("m2", "work", "/home/u/Work"))
        .unwrap();
    let before = row_snapshot(&fx);
    for (target, mode) in [
        (ROOT, DeleteMode::Recursive),
        ("work", DeleteMode::Recursive),
        ("docs", DeleteMode::Recursive),
    ] {
        for pending in [
            REFUSE,
            PendingChanges::ServerConfirmedLocalDelete {
                expected_generation: 0,
            },
        ] {
            let err = store.delete_item(&key(target), mode, pending).unwrap_err();
            assert!(
                matches!(err, StoreError::MountRoot { .. }),
                "{target}: {err}"
            );
        }
    }
    store
        .insert_item(&folder("leaf-root", Some(ROOT), "lr"))
        .unwrap();
    store
        .insert_mount(&common::mount("m3", "leaf-root", "/home/u/L"))
        .unwrap();
    let err = store
        .delete_item(&key("leaf-root"), DeleteMode::ItemOnly, REFUSE)
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::MountRoot { mount_id, .. } if mount_id.as_str() == "m3"),
        "{err}"
    );
    store
        .delete_item(&key("plan"), DeleteMode::ItemOnly, REFUSE)
        .unwrap();
    let after: Vec<_> = row_snapshot(&fx)
        .into_iter()
        .filter(|(id, _)| id != "leaf-root")
        .collect();
    let expected: Vec<_> = before.into_iter().filter(|(id, _)| id != "plan").collect();
    assert_eq!(after, expected);
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
