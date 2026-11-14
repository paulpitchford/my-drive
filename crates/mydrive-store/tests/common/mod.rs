//! Fixtures shared by the store's integration tests.

#![allow(dead_code, unreachable_pub, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use mydrive_store::{Account, Item, ItemKind, Mount, Queries, RemoteMetadata, Store, Timestamp};
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
        store.insert_item(&folder(ROOT, None, "root")).unwrap();
        store
    }
}

/// Kills this process with SIGKILL: no destructors, no rollback, no clean close, and no
/// core dump. Exits normally only if the kill failed, which makes the parent's check fail.
pub fn kill_self() -> ! {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &std::process::id().to_string()])
        .status();
    std::process::exit(0);
}

/// Re-runs `test` (in the current test binary) as a child process with `env` set to
/// `value`, and checks that it was killed by SIGKILL.
pub fn run_child_until_killed(test: &str, env: &str, value: &str) {
    use std::os::unix::process::ExitStatusExt;
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(env, value)
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "the child must be killed, not exit: {status}"
    );
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

/// A file created locally and not yet uploaded: provisional, dirty (generation 1),
/// pending upload, no eTag. The only kind of dirty item `insert_item` accepts.
pub fn provisional(id: &str, parent: Option<&str>, name: &str, size: u64) -> Item {
    let mut item = file(id, parent, name, size);
    item.is_provisional = true;
    item.local_dirty = true;
    item.local_generation = 1;
    item.etag = None;
    item.state.hydration = Hydration::Full;
    item.state.sync = SyncState::PendingUpload;
    item
}

/// Sync states a dirty item can be in (never `in-sync` or `pending-download`).
pub const DIRTY_SYNC_STATES: [SyncState; 5] = [
    SyncState::PendingUpload,
    SyncState::Uploading,
    SyncState::Conflict,
    SyncState::Error,
    SyncState::Blocked,
];

/// Hydrates a stored clean file fully, as the engine does before a local change.
pub fn make_full(store: &impl Queries, key: &ItemKey) {
    let item = store.item(key).unwrap().unwrap();
    if item.kind == ItemKind::File && item.size > 0 {
        store
            .record_hydrated_range(key, mydrive_store::ByteRange::new(0, item.size).unwrap())
            .unwrap();
    }
    store.set_hydration(key, Hydration::Full).unwrap();
}

/// Makes a stored clean item dirty through the API (hydrating it first, as the engine
/// must) and moves it to `sync`.
pub fn make_dirty(store: &impl Queries, key: &ItemKey, sync: SyncState) {
    make_full(store, key);
    store.mark_local_change(key).unwrap();
    if sync != SyncState::PendingUpload {
        store.set_sync(key, SyncState::PendingUpload, sync).unwrap();
    }
}

/// The server-owned part of `item`, as a delta would report it unchanged.
pub fn remote(item: &Item) -> RemoteMetadata {
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
