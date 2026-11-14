//! Fixtures shared by the store's integration tests.

#![allow(dead_code, unreachable_pub, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{Account, Item, ItemKind, Mount, Queries, Store, Timestamp};
use tempfile::TempDir;

pub const DRIVE: &str = "drive-a";
pub const ROOT: &str = "root";
pub const MOUNT: &str = "m1";
pub const ACCOUNT: &str = "acct-1";

/// A temporary directory holding `state.db`.
pub struct Fixture {
    pub dir: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().join("state.db")
    }

    pub fn open(&self) -> Store {
        Store::open(self.path()).unwrap()
    }

    /// Opens the store with one account, one mount of `DRIVE` rooted at `ROOT`, and the
    /// root folder item.
    pub fn open_with_mount(&self) -> Store {
        let store = self.open();
        store.upsert_account(&account()).unwrap();
        store
            .insert_mount(&mount(MOUNT, ROOT, "/home/u/OneDrive"))
            .unwrap();
        store.upsert_item(&folder(ROOT, None, "root")).unwrap();
        store
    }
}

pub fn raw(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

pub fn account() -> Account {
    Account {
        account_id: AccountId::new(ACCOUNT).unwrap(),
        tenant_id: None,
    }
}

pub fn mount(id: &str, root: &str, point: &str) -> Mount {
    Mount {
        mount_id: MountId::new(id).unwrap(),
        account_id: AccountId::new(ACCOUNT).unwrap(),
        drive_id: DriveId::new(DRIVE).unwrap(),
        root_item_id: ItemId::new(root).unwrap(),
        mount_point: PathBuf::from(point),
        delta_token: None,
        read_only: false,
        blocked_reason: None,
    }
}

pub fn mount_id() -> MountId {
    MountId::new(MOUNT).unwrap()
}

pub fn key(item: &str) -> ItemKey {
    ItemKey::new(DriveId::new(DRIVE).unwrap(), ItemId::new(item).unwrap())
}

pub fn file(id: &str, parent: Option<&str>, name: &str, size: u64) -> Item {
    Item {
        key: key(id),
        parent_item_id: parent.map(|p| ItemId::new(p).unwrap()),
        name: name.to_owned(),
        kind: ItemKind::File,
        size,
        created: Timestamp::from_unix_seconds(1_700_000_000),
        modified: Timestamp::from_unix_seconds(1_700_000_100),
        etag: format!("etag-{id}"),
        ctag: None,
        server_hash: None,
        local_hash: None,
        state: ItemState {
            hydration: Hydration::Placeholder,
            pin: Pin::Inherit,
            sync: SyncState::InSync,
        },
        last_error: None,
    }
}

pub fn folder(id: &str, parent: Option<&str>, name: &str) -> Item {
    Item {
        kind: ItemKind::Folder,
        size: 0,
        state: ItemState {
            hydration: Hydration::Full,
            pin: Pin::Inherit,
            sync: SyncState::InSync,
        },
        ..file(id, parent, name, 0)
    }
}
