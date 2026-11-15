//! Builds `state-v1.db`, the schema version 1 fixture for the version 2 migration test
//! (WP-017 AC1), through the released version 1 API of `mydrive-store` and nothing else.
//!
//! This file does not compile against the current store: it is the generator, kept with
//! the fixture it produced. `generate.sh` copies it into a checkout of the version 1 code
//! (commit 4673830) as an integration test and runs it there. Every row is written by a
//! version 1 store operation; no SQL is written by hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{
    Account, ByteRange, Item, ItemKind, Mount, Queries, RemoteMetadata, Store, Timestamp,
};

const PERSONAL_DRIVE: &str = "0123456789abcdef";
const WORK_DRIVE: &str = "b!workdrive";

fn key(drive: &str, id: &str) -> ItemKey {
    ItemKey::new(DriveId::new(drive).unwrap(), ItemId::new(id).unwrap())
}

fn hash(b: u8) -> QuickXorHash {
    QuickXorHash::from_bytes([b; 20])
}

fn file(drive: &str, id: &str, parent: &str, name: &str, size: u64) -> Item {
    Item {
        key: key(drive, id),
        parent_item_id: Some(ItemId::new(parent).unwrap()),
        name: name.to_owned(),
        kind: ItemKind::File,
        size,
        created: Timestamp::from_unix_seconds(1_700_000_000),
        modified: Timestamp::from_unix_seconds(1_700_000_100),
        etag: Some(format!("etag-{id}")),
        ctag: None,
        server_hash: None,
        local_hash: None,
        state: ItemState {
            hydration: Hydration::Placeholder,
            pin: Pin::Inherit,
            sync: SyncState::InSync,
        },
        last_error: None,
        is_provisional: false,
        local_dirty: false,
        local_generation: 0,
        remote_changed: false,
        last_remote: None,
    }
}

fn folder(drive: &str, id: &str, parent: Option<&str>, name: &str) -> Item {
    Item {
        parent_item_id: parent.map(|p| ItemId::new(p).unwrap()),
        kind: ItemKind::Folder,
        size: 0,
        state: ItemState {
            hydration: Hydration::Full,
            pin: Pin::Inherit,
            sync: SyncState::InSync,
        },
        ..file(drive, id, "unused", name, 0)
    }
}

fn remote(item: &Item) -> RemoteMetadata {
    RemoteMetadata {
        key: item.key.clone(),
        parent_item_id: item.parent_item_id.clone(),
        name: item.name.clone(),
        kind: item.kind,
        size: item.size,
        created: item.created,
        modified: item.modified,
        etag: item.etag.clone(),
        ctag: item.ctag.clone(),
        server_hash: item.server_hash,
    }
}

fn make_full(store: &Store, k: &ItemKey) {
    let item = store.item(k).unwrap().unwrap();
    if item.size > 0 {
        store
            .record_hydrated_range(k, ByteRange::new(0, item.size).unwrap())
            .unwrap();
    }
    store.set_hydration(k, Hydration::Full).unwrap();
}

#[test]
#[ignore = "run by generate.sh against the version 1 code only"]
fn generate_v1_fixture() {
    let out = PathBuf::from(std::env::var("MYDRIVE_V1_FIXTURE_OUT").unwrap());
    assert!(!out.exists(), "refusing to overwrite {}", out.display());
    let store = Store::open(&out).unwrap();
    assert_eq!(mydrive_store::SCHEMA_VERSION, 1);

    // Accounts: personal (no tenant) and work (with a tenant).
    let personal = AccountId::new("acct-personal").unwrap();
    let work = AccountId::new("acct-work").unwrap();
    store
        .upsert_account(&Account {
            account_id: personal.clone(),
            tenant_id: None,
        })
        .unwrap();
    store
        .upsert_account(&Account {
            account_id: work.clone(),
            tenant_id: Some(TenantId::new("00000000-0000-0000-0000-000000000001").unwrap()),
        })
        .unwrap();

    // Mounts: a personal one with a live token; a work library that is read-only and
    // blocked, with a token; and one with no token yet.
    store
        .insert_mount(&Mount {
            mount_id: MountId::new("personal").unwrap(),
            account_id: personal.clone(),
            drive_id: DriveId::new(PERSONAL_DRIVE).unwrap(),
            root_item_id: ItemId::new("root-p").unwrap(),
            mount_point: PathBuf::from("/home/user/OneDrive"),
            delta_token: None,
            read_only: false,
            blocked_reason: None,
        })
        .unwrap();
    store
        .set_delta_token(
            &MountId::new("personal").unwrap(),
            Some("https://graph.example/delta?token=personal-v1"),
        )
        .unwrap();
    store
        .insert_mount(&Mount {
            mount_id: MountId::new("work-docs").unwrap(),
            account_id: work.clone(),
            drive_id: DriveId::new(WORK_DRIVE).unwrap(),
            root_item_id: ItemId::new("root-w").unwrap(),
            mount_point: PathBuf::from("/home/user/Example/Site - Documents"),
            delta_token: Some("work-token-v1".to_owned()),
            read_only: true,
            blocked_reason: Some("unmanaged device policy".to_owned()),
        })
        .unwrap();
    store
        .insert_mount(&Mount {
            mount_id: MountId::new("work-new").unwrap(),
            account_id: work,
            drive_id: DriveId::new(WORK_DRIVE).unwrap(),
            root_item_id: ItemId::new("folder-w").unwrap(),
            mount_point: PathBuf::from("/home/user/Example/Other"),
            delta_token: None,
            read_only: false,
            blocked_reason: None,
        })
        .unwrap();

    let p = PERSONAL_DRIVE;
    let mut root = folder(p, "root-p", None, "root");
    root.state.pin = Pin::Unpinned;
    store.insert_item(&root).unwrap();
    store
        .insert_item(&folder(p, "docs", Some("root-p"), "Documents"))
        .unwrap();
    store
        .insert_item(&folder(p, "deep", Some("docs"), "Ünïcødé – 日本"))
        .unwrap();

    // Clean files in every hydration, sync state and pin.
    let hydrations = [Hydration::Placeholder, Hydration::Partial, Hydration::Full];
    let syncs = [
        SyncState::InSync,
        SyncState::PendingDownload,
        SyncState::PendingUpload,
        SyncState::Uploading,
        SyncState::Conflict,
        SyncState::Error,
        SyncState::Blocked,
    ];
    let pins = [Pin::Inherit, Pin::Pinned, Pin::Unpinned];
    let mut n = 0u8;
    for h in hydrations {
        for s in syncs {
            n += 1;
            let id = format!("clean-{h}-{s}");
            let mut item = file(p, &id, "docs", &format!("{id}.txt"), 100);
            item.server_hash = Some(hash(n));
            if n % 2 == 0 {
                item.ctag = Some(format!("ctag-{id}"));
            }
            if n % 3 == 0 {
                item.local_hash = Some(hash(n.wrapping_add(100)));
            }
            if s == SyncState::Error {
                item.last_error = Some(format!("error on {id}"));
            }
            item.state.pin = pins[usize::from(n) % 3];
            store.insert_item(&item).unwrap();
            match h {
                Hydration::Placeholder => {}
                Hydration::Partial => {
                    store
                        .record_hydrated_range(&item.key, ByteRange::new(0, 10).unwrap())
                        .unwrap();
                    store
                        .record_hydrated_range(&item.key, ByteRange::new(40, 70).unwrap())
                        .unwrap();
                    store.set_hydration(&item.key, Hydration::Partial).unwrap();
                }
                Hydration::Full => make_full(&store, &item.key),
            }
            if s != SyncState::InSync {
                store.set_sync(&item.key, SyncState::InSync, s).unwrap();
            }
        }
    }

    // An empty file, fully local with no ranges, and a name longer than 255 bytes.
    let mut empty = file(p, "empty", "root-p", "empty.txt", 0);
    empty.state.hydration = Hydration::Full;
    store.insert_item(&empty).unwrap();
    store
        .insert_item(&file(p, "long", "deep", &"n".repeat(300), 5))
        .unwrap();

    // Dirty files in every sync state a dirty item can have, several generations.
    for (i, s) in [
        SyncState::PendingUpload,
        SyncState::Uploading,
        SyncState::Conflict,
        SyncState::Error,
        SyncState::Blocked,
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("dirty-{s}");
        let mut item = file(p, &id, "docs", &format!("{id}.bin"), 200);
        item.server_hash = Some(hash(50));
        store.insert_item(&item).unwrap();
        make_full(&store, &item.key);
        store.mark_local_change(&item.key).unwrap();
        for _ in 0..i {
            store.mark_local_change(&item.key).unwrap();
        }
        store.set_local_size(&item.key, 250 + i as u64).unwrap();
        if s != SyncState::PendingUpload {
            store
                .set_sync(&item.key, SyncState::PendingUpload, s)
                .unwrap();
        }
    }

    // A dirty file the server changed meanwhile: remote_changed with a recorded identity.
    let mut rc = file(p, "dirty-remote-changed", "docs", "both.bin", 80);
    rc.server_hash = Some(hash(60));
    rc.ctag = Some("ctag-rc-1".to_owned());
    store.insert_item(&rc).unwrap();
    make_full(&store, &rc.key);
    store.mark_local_change(&rc.key).unwrap();
    let applied = store
        .apply_remote_metadata(&RemoteMetadata {
            etag: Some("etag-rc-2".to_owned()),
            ctag: Some("ctag-rc-2".to_owned()),
            server_hash: Some(hash(61)),
            size: 90,
            name: "both renamed.bin".to_owned(),
            ..remote(&rc)
        })
        .unwrap();
    assert!(matches!(
        applied,
        mydrive_store::RemoteApplied::ContentChangedUnderLocalState { .. }
    ));

    // A clean file whose upload completed after the server moved on: pending-download
    // with remote_changed kept.
    let mut moved = file(p, "uploaded-moved-on", "docs", "moved.bin", 30);
    moved.server_hash = Some(hash(70));
    store.insert_item(&moved).unwrap();
    make_full(&store, &moved.key);
    let generation = store.mark_local_change(&moved.key).unwrap();
    store
        .apply_remote_metadata(&RemoteMetadata {
            etag: Some("etag-other-client".to_owned()),
            server_hash: Some(hash(72)),
            ..remote(&moved)
        })
        .unwrap();
    let done = store
        .complete_upload(
            &moved.key,
            generation,
            hash(71),
            &RemoteMetadata {
                etag: Some("etag-ours".to_owned()),
                server_hash: Some(hash(71)),
                ..remote(&moved)
            },
            None,
        )
        .unwrap();
    assert_eq!(
        done,
        mydrive_store::UploadCompleted::UploadedButRemoteMovedOn
    );

    // A file uploaded and synced: local_hash set, generation kept.
    let mut synced = file(p, "uploaded-synced", "docs", "synced.bin", 40);
    synced.server_hash = Some(hash(80));
    store.insert_item(&synced).unwrap();
    make_full(&store, &synced.key);
    let generation = store.mark_local_change(&synced.key).unwrap();
    store
        .complete_upload(
            &synced.key,
            generation,
            hash(81),
            &RemoteMetadata {
                etag: Some("etag-synced-2".to_owned()),
                server_hash: Some(hash(81)),
                ..remote(&synced)
            },
            None,
        )
        .unwrap();

    // Provisional items: a folder with a provisional child, and a provisional file.
    let mut pdir = folder(p, "tmp-dir", Some("root-p"), "New folder");
    pdir.is_provisional = true;
    pdir.local_dirty = true;
    pdir.local_generation = 1;
    pdir.etag = None;
    pdir.state.sync = SyncState::PendingUpload;
    store.insert_item(&pdir).unwrap();
    let mut pfile = file(p, "tmp-file", "tmp-dir", "draft.txt", 12);
    pfile.is_provisional = true;
    pfile.local_dirty = true;
    pfile.local_generation = 1;
    pfile.etag = None;
    pfile.state.hydration = Hydration::Full;
    pfile.state.sync = SyncState::PendingUpload;
    store.insert_item(&pfile).unwrap();

    // The work drive: its root, a folder (the root of the second mount) and a file.
    let w = WORK_DRIVE;
    store
        .insert_item(&folder(w, "root-w", None, "root"))
        .unwrap();
    store
        .insert_item(&folder(w, "folder-w", Some("root-w"), "Other"))
        .unwrap();
    let mut wf = file(w, "work-file", "folder-w", "report.docx", 1000);
    wf.ctag = Some("ctag-work".to_owned());
    store.insert_item(&wf).unwrap();
    store
        .record_hydrated_range(&wf.key, ByteRange::new(0, 300).unwrap())
        .unwrap();
    store.set_hydration(&wf.key, Hydration::Partial).unwrap();

    drop(store);
}
