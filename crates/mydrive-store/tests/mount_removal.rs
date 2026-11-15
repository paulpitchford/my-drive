//! Removing a mount's rows (WP-017 AC8, ADR-0015 R8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    DRIVE, Fixture, ROOT, file, folder, key, make_dirty, mount, mount_id, provisional, raw,
};
use mydrive_core::ids::{DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::Sync as SyncState;
use mydrive_store::{
    DIRTY_SAMPLE_LIMIT, DeltaRunKind, LocalOnlyContent, Mount, MountRemoved, PageLink, Queries,
    Store, StoreError,
};

const OTHER_DRIVE: &str = "drive-b";

/// Every row in `items` and `hydrated_ranges` of `drive`, through a second connection.
fn drive_rows(fx: &Fixture, drive: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let conn = raw(&fx.path());
    let mut out = Vec::new();
    for sql in [
        "SELECT * FROM items WHERE drive_id = ?1 ORDER BY item_id",
        "SELECT * FROM hydrated_ranges WHERE drive_id = ?1 ORDER BY item_id, range_start",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let mut q = stmt.query([drive]).unwrap();
        while let Some(row) = q.next().unwrap() {
            out.push((0..n).map(|i| row.get(i).unwrap()).collect());
        }
    }
    out
}

/// The fixture's mount with 30 dirty files and some clean ones, plus a second mount of
/// another drive with a dirty file of its own.
fn two_drives(fx: &Fixture) -> Store {
    let store = fx.open_with_mount();
    store
        .insert_item(&folder("docs", Some(ROOT), "docs"))
        .unwrap();
    for n in 0..30 {
        let id = format!("d{n:02}");
        store
            .insert_item(&file(&id, Some("docs"), &id, 10))
            .unwrap();
        make_dirty(&store, &key(&id), SyncState::PendingUpload);
    }
    store
        .insert_item(&file("clean", Some(ROOT), "clean", 5))
        .unwrap();
    let other = Mount {
        drive_id: DriveId::new(OTHER_DRIVE).unwrap(),
        ..mount("m2", "root-b", "/home/u/B")
    };
    store.insert_mount(&other).unwrap();
    let other_key =
        |id: &str| ItemKey::new(DriveId::new(OTHER_DRIVE).unwrap(), ItemId::new(id).unwrap());
    let mut root_b = folder("root-b", None, "root");
    root_b.key = other_key("root-b");
    store.insert_item(&root_b).unwrap();
    let mut b = file("b", Some("root-b"), "b", 10);
    b.key = other_key("b");
    store.insert_item(&b).unwrap();
    make_dirty(&store, &b.key, SyncState::PendingUpload);
    store
}

/// WP-017 AC8: removal is refused while any item of the mount is dirty. The error names
/// them, bounded: the full count, and at most `DIRTY_SAMPLE_LIMIT` keys in key order.
/// Nothing changes, in either drive.
#[test]
fn ac8_removal_is_refused_naming_the_dirty_items_boundedly() {
    let fx = Fixture::new();
    let store = two_drives(&fx);
    let before = (drive_rows(&fx, DRIVE), drive_rows(&fx, OTHER_DRIVE));
    let err = store
        .remove_mount(&mount_id(), LocalOnlyContent::Refuse)
        .unwrap_err();
    let StoreError::MountHasDirtyItems {
        count, ref sample, ..
    } = err
    else {
        panic!("{err}");
    };
    assert_eq!(count, 30);
    assert_eq!(sample.len(), DIRTY_SAMPLE_LIMIT);
    let expected: Vec<ItemKey> = (0..DIRTY_SAMPLE_LIMIT)
        .map(|n| key(&format!("d{n:02}")))
        .collect();
    assert_eq!(sample, &expected);
    let text = err.to_string();
    assert!(
        text.contains("30 item(s)") && text.contains("d00"),
        "{text}"
    );
    assert!(!text.contains("d29"), "the message is bounded: {text}");
    assert_eq!(
        (drive_rows(&fx, DRIVE), drive_rows(&fx, OTHER_DRIVE)),
        before
    );
    assert!(store.mount(&mount_id()).unwrap().is_some());
}

/// WP-017 AC8: "kept elsewhere" is accepted only with the number of dirty items the user
/// confirmed; a different number means something changed since, and is refused.
#[test]
fn ac8_kept_elsewhere_must_match_the_current_dirty_count() {
    let fx = Fixture::new();
    let store = two_drives(&fx);
    let before = drive_rows(&fx, DRIVE);
    for wrong in [0, 29, 31] {
        let err = store
            .remove_mount(
                &mount_id(),
                LocalOnlyContent::KeptElsewhere { dirty_items: wrong },
            )
            .unwrap_err();
        assert!(
            matches!(err, StoreError::DirtyCountChanged { confirmed, actual: 30, .. } if confirmed == wrong),
            "{err}"
        );
        assert_eq!(drive_rows(&fx, DRIVE), before);
    }
}

/// WP-017 AC8 and ADR-0015 R8: a confirmed removal removes the mount, its staged entries
/// and its drive's items and ranges, and never changes a row of another drive.
#[test]
fn adr_0015_r8_removal_removes_only_the_mounts_rows() {
    let fx = Fixture::new();
    let store = two_drives(&fx);
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[mydrive_store::StagedEntry {
                item_id: ItemId::new("x").unwrap(),
                is_root: false,
                change: mydrive_store::StagedChange::Deleted,
            }],
            &PageLink::Next("n".to_owned()),
        )
        .unwrap();
    let other_before = drive_rows(&fx, OTHER_DRIVE);
    let other_mount = store.mount(&MountId::new("m2").unwrap()).unwrap();
    let removed = store
        .remove_mount(
            &mount_id(),
            LocalOnlyContent::KeptElsewhere { dirty_items: 30 },
        )
        .unwrap();
    assert_eq!(
        removed,
        MountRemoved {
            items: 1 + 1 + 30 + 1,
            ranges: 30,
            staged: 1,
        }
    );
    assert_eq!(store.mount(&mount_id()).unwrap(), None);
    assert!(drive_rows(&fx, DRIVE).is_empty());
    assert_eq!(drive_rows(&fx, OTHER_DRIVE), other_before);
    assert_eq!(
        store.mount(&MountId::new("m2").unwrap()).unwrap(),
        other_mount
    );
    let staged: i64 = raw(&fx.path())
        .query_row("SELECT COUNT(*) FROM delta_staged", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0);

    // A clean mount needs no confirmation.
    let err = store
        .remove_mount(&MountId::new("m2").unwrap(), LocalOnlyContent::Refuse)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::MountHasDirtyItems { count: 1, .. }),
        "{err}"
    );
}

/// A mount without dirty items is removed with `Refuse`; adding the drive again gives new
/// local IDs, never the removed rows' (ADR-0021 R1).
#[test]
fn a_clean_mount_is_removed_and_its_local_ids_are_not_reused() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let mut ids = vec![store.local_id(&key(ROOT)).unwrap()];
    for n in 0..3 {
        ids.push(
            store
                .insert_item(&file(&format!("f{n}"), Some(ROOT), &format!("f{n}"), 1))
                .unwrap(),
        );
    }
    store
        .remove_mount(&mount_id(), LocalOnlyContent::Refuse)
        .unwrap();
    drop(store);
    let store = fx.open_with_mount();
    let root = store.local_id(&key(ROOT)).unwrap();
    assert!(ids.iter().all(|old| root > *old), "{root} after {ids:?}");
}

/// Removing a mount whose drive another mount also shows would remove that mount's
/// items: refused, naming it.
#[test]
fn removal_is_refused_while_another_mount_shows_the_drive() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_mount(&mount("m-folder", "some-folder", "/home/u/Folder"))
        .unwrap();
    let err = store
        .remove_mount(&mount_id(), LocalOnlyContent::Refuse)
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::DriveSharedByMounts { other, count: 1, .. } if other.as_str() == "m-folder"),
        "{err}"
    );
    assert!(store.item(&key(ROOT)).unwrap().is_some());
    let err = store
        .remove_mount(&MountId::new("absent").unwrap(), LocalOnlyContent::Refuse)
        .unwrap_err();
    assert!(matches!(err, StoreError::MountNotFound { .. }), "{err}");
}

/// A provisional item (never uploaded) is dirty, so it blocks removal like any other.
#[test]
fn a_provisional_item_blocks_removal() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_item(&provisional("tmp", Some(ROOT), "new", 3))
        .unwrap();
    let err = store
        .remove_mount(&mount_id(), LocalOnlyContent::Refuse)
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::MountHasDirtyItems { count: 1, sample, .. } if sample == &[key("tmp")]),
        "{err}"
    );
}
