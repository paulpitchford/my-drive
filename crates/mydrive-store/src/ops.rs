//! The typed operations, shared by [`Store`](crate::Store) and
//! [`Transaction`](crate::Transaction).
//!
//! Every operation that runs more than one statement runs inside its own savepoint, so it
//! is atomic whether it is called on its own or inside a caller's transaction. When the
//! store refuses an operation, or one of its statements fails a constraint, the savepoint
//! undoes the operation's earlier statements and the caller's transaction stays usable.
//! That does not hold when SQLite rolls the whole transaction back by itself (for example
//! `SQLITE_FULL` or an I/O error): [`Transaction`](crate::Transaction) then refuses every
//! further call with [`StoreError::TransactionAborted`], and the batch must be retried from
//! the start.

use std::collections::HashSet;
use std::path::PathBuf;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{SqlContext, StoreError};
use crate::types::{
    Account, ByteRange, DeleteMode, Item, ItemKind, Mount, MountPath, PendingChanges,
    RemoteApplied, RemoteIdentity, RemoteMetadata, Timestamp, UploadCompleted,
};

pub(crate) mod sealed {
    use crate::StoreError;

    /// Proof of being inside this crate; nothing outside can build one.
    #[derive(Debug, Clone, Copy)]
    pub struct Token(pub(super) ());

    /// Gives [`Queries`](super::Queries) its connection without exposing it, and lets a
    /// transaction check its own state around every operation.
    pub trait Connected {
        /// Runs one operation on the connection.
        fn run<T, F>(&self, token: Token, op: F) -> Result<T, StoreError>
        where
            F: FnOnce(&rusqlite::Connection) -> Result<T, StoreError>;
    }
}

const ITEM_COLUMNS: &str = "drive_id, item_id, parent_item_id, name, kind, size, created_at, \
     modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error, \
     is_provisional, local_dirty, local_generation, remote_changed, remote_etag, remote_ctag, \
     remote_hash, remote_size";

/// The columns `insert_item` writes; the `remote_*` columns start `NULL`.
const INSERT_COLUMNS: &str = "drive_id, item_id, parent_item_id, name, kind, size, created_at, \
     modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error, \
     is_provisional, local_dirty, local_generation, remote_changed";

const MOUNT_COLUMNS: &str = "mount_id, account_id, drive_id, root_item_id, mount_point, \
     delta_token, read_only, blocked_reason";

/// Every typed operation on the store.
///
/// Implemented by [`Store`](crate::Store), where each call is atomic on its own, and by
/// [`Transaction`](crate::Transaction), where calls take effect together on commit. The
/// trait is sealed: it cannot be implemented outside this crate.
///
/// Items are addressed only by [`ItemKey`]; no operation takes a path as an item's
/// identity (WP-006 AC1, ADR-0007 R6).
pub trait Queries: sealed::Connected {
    // ---------------------------------------------------------------- accounts

    /// Inserts the account, or replaces the stored one with the same ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the write fails.
    fn upsert_account(&self, account: &Account) -> Result<(), StoreError> {
        run(self, |c| {
            c.execute(
                "INSERT INTO accounts (account_id, tenant_id) VALUES (?1, ?2) \
                 ON CONFLICT (account_id) DO UPDATE SET tenant_id = excluded.tenant_id",
                params![
                    account.account_id.as_str(),
                    account.tenant_id.as_ref().map(TenantId::as_str)
                ],
            )
            .ctx(|| format!("store account {}", account.account_id))?;
            Ok(())
        })
    }

    /// Fetches an account by ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row holds
    /// an invalid identifier.
    fn account(&self, account_id: &AccountId) -> Result<Option<Account>, StoreError> {
        run(self, |c| {
            let raw: Option<(String, Option<String>)> = c
                .query_row(
                    "SELECT account_id, tenant_id FROM accounts WHERE account_id = ?1",
                    [account_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .ctx(|| format!("read account {account_id}"))?;
            raw.map(|(id, tenant)| {
                let what = format!("account {id}");
                Ok(Account {
                    account_id: parse_id(&what, "account_id", id.clone(), AccountId::new)?,
                    tenant_id: tenant
                        .map(|t| parse_id(&what, "tenant_id", t, TenantId::new))
                        .transpose()?,
                })
            })
            .transpose()
        })
    }

    // ------------------------------------------------------------------ mounts

    /// Adds a mount.
    ///
    /// The mount point is not a key and is not checked here: refusing a mount point that
    /// is in use, nested or overlapping is the engine's check (overview, "Rulings on
    /// details"), and ADR-0007 R6 forbids paths as keys.
    ///
    /// # Errors
    ///
    /// [`StoreError::AccountNotFound`] if its account is not stored,
    /// [`StoreError::MountExists`] if the mount ID is taken, [`StoreError::MountRootTaken`]
    /// if another mount has the same root (`drive_id`, `item_id`) (ADR-0007 R4),
    /// [`StoreError::NonUtf8Path`] if the mount point is not UTF-8.
    fn insert_mount(&self, mount: &Mount) -> Result<(), StoreError> {
        run(self, |c| atomic(c, |c| insert_mount(c, mount)))
    }

    /// Fetches a mount by ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row is
    /// invalid.
    fn mount(&self, mount_id: &MountId) -> Result<Option<Mount>, StoreError> {
        run(self, |c| {
            c.query_row(
                &format!("SELECT {MOUNT_COLUMNS} FROM mounts WHERE mount_id = ?1"),
                [mount_id.as_str()],
                RawMount::from_row,
            )
            .optional()
            .ctx(|| format!("read mount {mount_id}"))?
            .map(RawMount::into_mount)
            .transpose()
        })
    }

    /// Lists every mount, ordered by mount ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn mounts(&self) -> Result<Vec<Mount>, StoreError> {
        run(self, |c| {
            let mut stmt = c
                .prepare_cached(&format!(
                    "SELECT {MOUNT_COLUMNS} FROM mounts ORDER BY mount_id"
                ))
                .ctx(|| "list mounts")?;
            let raws = stmt
                .query_map([], RawMount::from_row)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .ctx(|| "list mounts")?;
            raws.into_iter().map(RawMount::into_mount).collect()
        })
    }

    /// Reads a mount's delta token (ADR-0007 R2). `None` means no token has been saved.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`] if the mount is not stored.
    fn delta_token(&self, mount_id: &MountId) -> Result<Option<String>, StoreError> {
        run(self, |c| {
            c.query_row(
                "SELECT delta_token FROM mounts WHERE mount_id = ?1",
                [mount_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ctx(|| format!("read the delta token of mount {mount_id}"))?
            .ok_or_else(|| StoreError::MountNotFound {
                action: "read the delta token",
                mount_id: mount_id.clone(),
            })
        })
    }

    /// Replaces a mount's delta token; `None` clears it, so the next delta enumerates the
    /// whole drive. The token is never logged.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`] if the mount is not stored.
    fn set_delta_token(&self, mount_id: &MountId, token: Option<&str>) -> Result<(), StoreError> {
        run(self, |c| {
            update_mount(c, mount_id, "delta_token", token, "write the delta token")
        })
    }

    /// Sets why organisation policy blocks a mount; `None` clears the block.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`] if the mount is not stored.
    fn set_mount_blocked_reason(
        &self,
        mount_id: &MountId,
        reason: Option<&str>,
    ) -> Result<(), StoreError> {
        run(self, |c| {
            update_mount(
                c,
                mount_id,
                "blocked_reason",
                reason,
                "write the blocked reason",
            )
        })
    }

    // ------------------------------------------------------------------- items

    /// Inserts a new item with every column as given: an item first seen in a delta, or
    /// one created locally (`is_provisional`, with a provisional `item_id`).
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemExists`] if the key is taken, [`StoreError::InvalidName`] if the
    /// name is empty, `.`, `..`, or contains `/` or NUL, [`StoreError::SizeTooLarge`] if
    /// the size exceeds `i64::MAX`.
    fn insert_item(&self, item: &Item) -> Result<(), StoreError> {
        run(self, |c| atomic(c, |c| insert_item(c, item)))
    }

    /// Applies what the server reports about an existing item, writing only server-owned
    /// columns: parent, name, kind, size, timestamps, eTag, cTag and server hash. Local
    /// hash, pin, sync and last error are never written here.
    ///
    /// The content identity has changed when the server hash differs; where neither side
    /// has a hash, when the cTag differs (if both have one; a rename changes the eTag but
    /// not the cTag), else the eTag, or the size differs; a kind change also counts.
    ///
    /// - Dirty item (`local_dirty`): only parent and name are written. Nothing describing
    ///   content changes, so size and ranges keep describing the local content and the old
    ///   eTag still makes an upload meet 412. If the content identity differs, the durable
    ///   `remote_changed` flag is set and the result is
    ///   [`RemoteApplied::ContentChangedUnderLocalState`], so the engine runs conflict
    ///   handling, now or after a restart.
    /// - Clean item, unchanged content: every server-owned column is written and
    ///   `remote_changed` cleared.
    /// - Clean item, changed content: as above, and the hydrated ranges are cleared and
    ///   hydration set to `placeholder` (`full` for a folder) in the same operation, so the
    ///   ranges never describe another version than the hashes.
    ///
    /// A move or rename is one row update; every descendant's resolved path follows
    /// (WP-006 AC4).
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`] if the item is not stored (use
    /// [`insert_item`](Self::insert_item)); [`StoreError::FolderHasChildren`] if a folder
    /// with children would become a file; [`StoreError::InvalidName`];
    /// [`StoreError::SizeTooLarge`].
    fn apply_remote_metadata(&self, remote: &RemoteMetadata) -> Result<RemoteApplied, StoreError> {
        run(self, |c| atomic(c, |c| apply_remote_metadata(c, remote)))
    }

    /// Sets the sync state to `new` if it is currently `expected` (compare-and-set).
    ///
    /// A dirty item cannot be set `in-sync` or `pending-download`: only
    /// [`complete_upload`](Self::complete_upload) or
    /// [`discard_local_changes`](Self::discard_local_changes) end a local change.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::LocalChanges`] as above, and
    /// [`StoreError::SyncMismatch`] with the stored value if it is not `expected`;
    /// nothing changes.
    fn set_sync(
        &self,
        key: &ItemKey,
        expected: SyncState,
        new: SyncState,
    ) -> Result<(), StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let item = load_item(c, key, "set the sync state")?;
                if item.local_dirty && matches!(new, SyncState::InSync | SyncState::PendingDownload)
                {
                    return Err(StoreError::LocalChanges {
                        action: "set in sync or pending download",
                        key: key.clone(),
                    });
                }
                // The server has a version not yet applied; only applying fresh metadata
                // clears this (fourth review, finding 3).
                if item.remote_changed && new == SyncState::InSync {
                    return Err(StoreError::RemoteChanged {
                        action: "set in sync",
                        key: key.clone(),
                    });
                }
                let changed = c
                    .execute(
                        "UPDATE items SET sync = ?4 \
                         WHERE drive_id = ?1 AND item_id = ?2 AND sync = ?3",
                        params![
                            key.drive_id.as_str(),
                            key.item_id.as_str(),
                            expected.as_str(),
                            new.as_str()
                        ],
                    )
                    .ctx(|| describe("set the sync state of", key))?;
                if changed == 1 {
                    return Ok(());
                }
                let actual = load_item(c, key, "set the sync state")?.state.sync;
                Err(StoreError::SyncMismatch {
                    key: key.clone(),
                    expected,
                    actual,
                    new,
                })
            })
        })
    }

    /// Sets the item's own pin.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`].
    fn set_pin(&self, key: &ItemKey, pin: Pin) -> Result<(), StoreError> {
        run(self, |c| {
            update_item(c, key, "pin", pin.as_str(), "set the pin")
        })
    }

    /// Sets the hydration state. Moving a dirty item to `placeholder` or `partial` is
    /// refused: that would claim its unsynced local bytes are gone. Use
    /// [`dehydrate`](Self::dehydrate) to release content.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::LocalChanges`].
    fn set_hydration(&self, key: &ItemKey, hydration: Hydration) -> Result<(), StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let item = load_item(c, key, "set the hydration")?;
                if item.local_dirty && hydration != Hydration::Full {
                    return Err(StoreError::LocalChanges {
                        action: "lower the hydration of",
                        key: key.clone(),
                    });
                }
                update_item(c, key, "hydration", hydration.as_str(), "set the hydration")
            })
        })
    }

    /// Records, durably, that a local change to the item's content is about to be made:
    /// sets `local_dirty` and bumps `local_generation`, returning the new generation. Call
    /// it, and commit, before the change touches the backing file (ADR-0012 R3). An item
    /// that was `in-sync` or `pending-download` becomes `pending-upload` in the same
    /// statement, so a dirty item is never `in-sync` and restart recovery always finds it.
    /// On the clean-to-dirty transition the base version's identity is recorded as the
    /// last remote identity.
    ///
    /// A dirty file is always fully local, so a file must be `full` with ranges covering
    /// `[0, size)` before it can be marked: the engine hydrates first (ADR-0012 R1). The
    /// one exception is [`truncate_local`](Self::truncate_local) to 0 (`O_TRUNC`).
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotFullyLocal`]. A refusal changes
    /// nothing.
    fn mark_local_change(&self, key: &ItemKey) -> Result<u64, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let item = load_item(c, key, "mark a local change")?;
                require_fully_local(c, &item, "mark a local change of")?;
                mark_local_change(c, key)
            })
        })
    }

    /// Records a local write that changes the size of a fully local file: marks a local
    /// change (as [`mark_local_change`](Self::mark_local_change)), sets the size and makes
    /// the ranges exactly `[0, size)`. Growing extends the range, because the grown bytes
    /// are local by definition; shrinking trims it. Returns the new generation.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`],
    /// [`StoreError::NotFullyLocal`], [`StoreError::SizeTooLarge`]. A refusal changes
    /// nothing.
    fn set_local_size(&self, key: &ItemKey, size: u64) -> Result<u64, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let stored = to_i64_checked(size, key)?;
                file_size(c, key, "set the size")?;
                let item = load_item(c, key, "set the size")?;
                require_fully_local(c, &item, "set the local size of")?;
                let generation = mark_local_change(c, key)?;
                set_size(c, key, stored)?;
                set_whole_file_range(c, key, stored)?;
                Ok(generation)
            })
        })
    }

    /// Records a local truncate (or extension) to `size`, returning the new generation.
    ///
    /// - `size == 0` (`O_TRUNC`, which needs no download, ADR-0012 R2) works on any file:
    ///   it clears the ranges, sets hydration `full` and size 0, and marks the change.
    /// - Any other size needs a fully local file, as
    ///   [`set_local_size`](Self::set_local_size), and leaves the ranges exactly
    ///   `[0, size)`.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`],
    /// [`StoreError::NotFullyLocal`] (non-zero size only), [`StoreError::SizeTooLarge`].
    /// A refusal changes nothing.
    fn truncate_local(&self, key: &ItemKey, size: u64) -> Result<u64, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let stored = to_i64_checked(size, key)?;
                file_size(c, key, "truncate")?;
                let item = load_item(c, key, "truncate")?;
                if size > 0 {
                    require_fully_local(c, &item, "truncate")?;
                } else {
                    // Nothing of the old content is needed any more: the empty file is
                    // fully local. Done before the mark so the dirty CHECK holds.
                    clear_ranges(c, key)?;
                    update_item(c, key, "hydration", Hydration::Full.as_str(), "truncate")?;
                }
                let generation = mark_local_change(c, key)?;
                set_size(c, key, stored)?;
                set_whole_file_range(c, key, stored)?;
                Ok(generation)
            })
        })
    }

    /// Records a finished upload, in one operation:
    ///
    /// 1. Only a dirty item can complete an upload; a replayed completion is refused.
    /// 2. A provisional item takes the server's ID (`new_item_id`), as
    ///    [`rekey_item`](Self::rekey_item).
    /// 3. `uploaded_local_hash` becomes `local_hash`, and the server's eTag, cTag, hash,
    ///    parent and name from `response` are written.
    /// 4. The server has "moved on" if a remote report seen while dirty
    ///    (`remote_changed`) names different content from `response`; if it names the
    ///    same content, that report was our own upload echoing back.
    /// 5. If `local_generation` still equals `expected_generation`, our current bytes are
    ///    on the server: the size and timestamps are written, ranges trimmed to the server
    ///    size, and the item becomes clean. It is `in-sync` ([`UploadCompleted::Synced`]),
    ///    or, if the server moved on, `pending-download` with `remote_changed` kept
    ///    ([`UploadCompleted::UploadedButRemoteMovedOn`]).
    /// 6. Otherwise the item changed locally while uploading: size, timestamps and ranges
    ///    keep describing the local content, and the item stays dirty and
    ///    `pending-upload` ([`UploadCompleted::LocalChangedSince`]), with `remote_changed`
    ///    kept if the server moved on.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`]; [`StoreError::NotDirty`] for an item with no local
    /// changes; [`StoreError::UploadMismatch`] if a provisional item
    /// gets no server ID, a non-provisional one gets a different one, or `response`
    /// names another item; [`StoreError::ItemExists`] if the server ID is already stored
    /// (see "Recovery" in `docs/architecture/store-schema.md`); [`StoreError::InvalidName`],
    /// [`StoreError::SizeTooLarge`]. A refusal changes nothing.
    fn complete_upload(
        &self,
        key: &ItemKey,
        expected_generation: u64,
        uploaded_local_hash: QuickXorHash,
        response: &RemoteMetadata,
        new_item_id: Option<&ItemId>,
    ) -> Result<UploadCompleted, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                complete_upload(
                    c,
                    key,
                    expected_generation,
                    uploaded_local_hash,
                    response,
                    new_item_id,
                )
            })
        })
    }

    /// Throws away the item's unsynced local content, explicitly, and makes it a clean
    /// placeholder of the server's current version, in one operation: `current` is that
    /// version's metadata, written as for a clean item (size, timestamps, eTag, cTag,
    /// server hash); `local_dirty`, `remote_changed` and the recorded remote identity are
    /// cleared, the ranges cleared, hydration set to `placeholder` (`full` for a folder)
    /// and sync to `in-sync`. There is no way to discard without the server's metadata, so
    /// a local size can never be left on a placeholder. The caller must also discard the
    /// bytes. A provisional item has no server version; delete it with
    /// [`PendingChanges::DiscardNeverUploaded`] instead.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::ProvisionalItem`],
    /// [`StoreError::UploadMismatch`] if `current` describes another item,
    /// [`StoreError::InvalidName`], [`StoreError::FolderHasChildren`],
    /// [`StoreError::SizeTooLarge`]. A refusal changes nothing.
    fn discard_local_changes(
        &self,
        key: &ItemKey,
        current: &RemoteMetadata,
    ) -> Result<(), StoreError> {
        run(self, |c| {
            atomic(c, |c| discard_local_changes(c, key, current))
        })
    }

    /// Every dirty item, in any drive and whatever its sync state, ordered by key. This is
    /// the restart-recovery listing: a blocked or errored mount cannot hide a dirty item
    /// from it. Backed by the partial index `items_by_dirty`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn dirty_items(&self) -> Result<Vec<Item>, StoreError> {
        run(self, |c| {
            query_items(
                c,
                &format!(
                    "SELECT {ITEM_COLUMNS} FROM items WHERE local_dirty = 1 \
                     ORDER BY drive_id, item_id"
                ),
                &[],
                || "list dirty items",
            )
        })
    }

    /// Records a local rename or move: writes only parent and name, with the same name
    /// validation as everywhere else. Works on provisional items.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::InvalidName`].
    fn move_item_local(
        &self,
        key: &ItemKey,
        new_parent: Option<&ItemId>,
        new_name: &str,
    ) -> Result<(), StoreError> {
        run(self, |c| {
            check_name(key, new_name)?;
            let changed = c
                .execute(
                    "UPDATE items SET parent_item_id = ?3, name = ?4 \
                     WHERE drive_id = ?1 AND item_id = ?2",
                    params![
                        key.drive_id.as_str(),
                        key.item_id.as_str(),
                        new_parent.map(ItemId::as_str),
                        new_name
                    ],
                )
                .ctx(|| describe("move", key))?;
            found(changed, key, "move an item")
        })
    }

    /// Sets or clears the item's last error.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`].
    fn set_last_error(&self, key: &ItemKey, error: Option<&str>) -> Result<(), StoreError> {
        run(self, |c| {
            let changed = c
                .execute(
                    "UPDATE items SET last_error = ?3 WHERE drive_id = ?1 AND item_id = ?2",
                    params![key.drive_id.as_str(), key.item_id.as_str(), error],
                )
                .ctx(|| describe("set the last error of", key))?;
            found(changed, key, "set the last error")
        })
    }

    /// Gives a provisional item its server ID: changes its `item_id`, its children's
    /// `parent_item_id` and its hydrated ranges together, and clears `is_provisional`.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotProvisional`] for an item that
    /// already has a server ID, and [`StoreError::ItemExists`] if the new key is taken.
    fn rekey_item(&self, old: &ItemKey, new_item_id: &ItemId) -> Result<(), StoreError> {
        run(self, |c| atomic(c, |c| rekey_item(c, old, new_item_id)))
    }

    /// Fetches an item by key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row is
    /// invalid.
    fn item(&self, key: &ItemKey) -> Result<Option<Item>, StoreError> {
        run(self, |c| fetch_item(c, key))
    }

    /// Lists the items whose parent is `parent`, ordered by name then item ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn children(&self, parent: &ItemKey) -> Result<Vec<Item>, StoreError> {
        run(self, |c| {
            query_items(
                c,
                &format!(
                    "SELECT {ITEM_COLUMNS} FROM items \
                     WHERE drive_id = ?1 AND parent_item_id = ?2 ORDER BY name, item_id"
                ),
                &[parent.drive_id.as_str(), parent.item_id.as_str()],
                || describe("list the children of", parent),
            )
        })
    }

    /// Lists every item, in any drive, whose sync state is `sync`, ordered by key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn items_with_sync(&self, sync: SyncState) -> Result<Vec<Item>, StoreError> {
        run(self, |c| {
            query_items(
                c,
                &format!(
                    "SELECT {ITEM_COLUMNS} FROM items WHERE sync = ?1 \
                     ORDER BY drive_id, item_id"
                ),
                &[sync.as_str()],
                || format!("list items with sync state {sync}"),
            )
        })
    }

    /// Deletes an item and its hydrated ranges, and returns how many items were deleted.
    ///
    /// With [`DeleteMode::ItemOnly`] an item that has children is refused. With
    /// [`DeleteMode::Recursive`] every descendant goes too, with its ranges.
    ///
    /// With [`PendingChanges::Refuse`], a delete that would remove any dirty item
    /// (`local_dirty`), whatever its sync state, is refused, naming every such item.
    /// [`PendingChanges::ServerConfirmedLocalDelete`] (the server confirmed a delete the
    /// user made locally) and [`PendingChanges::DiscardNeverUploaded`] (a provisional item
    /// the user deleted) exempt only the named item, and only if its generation still
    /// matches; any other dirty item in the delete is still refused and named. A delete
    /// that would remove a mount's root item is always refused. Deleting rows never touches
    /// a backing directory.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::HasChildren`] for a non-recursive
    /// delete of an item with children, [`StoreError::MountRoot`],
    /// [`StoreError::HasPendingLocalChanges`], [`StoreError::GenerationMismatch`],
    /// [`StoreError::ProvisionalItem`] (a confirmed server delete of an item the server
    /// never had), [`StoreError::NotProvisional`] (`DiscardNeverUploaded` on an item the
    /// server knows). A refused delete changes nothing.
    fn delete_item(
        &self,
        key: &ItemKey,
        mode: DeleteMode,
        pending: PendingChanges,
    ) -> Result<u64, StoreError> {
        run(self, |c| atomic(c, |c| delete_item(c, key, mode, pending)))
    }

    /// Resolves an item's path within a mount by walking its parents up to the mount's
    /// root item. The mount root resolves to the empty path.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`], [`StoreError::ItemNotFound`];
    /// [`StoreError::WrongDrive`] if the item is in another drive;
    /// [`StoreError::NotInMount`] if the chain reaches the drive root without passing the
    /// mount root; [`StoreError::MissingAncestor`] if a parent is not stored;
    /// [`StoreError::ParentCycle`] if the chain loops (WP-006 AC8).
    fn resolve_path(&self, mount_id: &MountId, key: &ItemKey) -> Result<MountPath, StoreError> {
        run(self, |c| atomic(c, |c| resolve_path(c, mount_id, key)))
    }

    // --------------------------------------------------------- hydrated ranges

    /// Records that `range` of a file is durable in its backing file, merging it with any
    /// recorded range it overlaps or touches. An empty range changes nothing.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`] for a folder, and
    /// [`StoreError::RangeOutOfBounds`] if the range ends past the item's size.
    fn record_hydrated_range(&self, key: &ItemKey, range: ByteRange) -> Result<(), StoreError> {
        run(self, |c| atomic(c, |c| record_range(c, key, range)))
    }

    /// Whether every byte of `range` is recorded as hydrated. An empty range is trivially
    /// present. The answer comes from the store only, never from the backing file's holes
    /// (ADR-0006 R6).
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`] for a folder, and
    /// [`StoreError::RangeOutOfBounds`] if the range ends past the item's size.
    fn is_range_hydrated(&self, key: &ItemKey, range: ByteRange) -> Result<bool, StoreError> {
        run(self, |c| atomic(c, |c| is_range_hydrated(c, key, range)))
    }

    /// The recorded hydrated ranges of a file, in order. They never overlap or touch.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`] for a folder.
    fn hydrated_ranges(&self, key: &ItemKey) -> Result<Vec<ByteRange>, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                file_size(c, key, "list hydrated ranges")?;
                ranges_of(c, key)
            })
        })
    }

    /// Forgets every hydrated range of a clean item, for example when its backing file was
    /// found damaged. Returns how many ranges were removed. Refused on a dirty item, whose
    /// ranges are the record of unsynced local bytes.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::LocalChanges`].
    fn clear_hydrated_ranges(&self, key: &ItemKey) -> Result<u64, StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let item = load_item(c, key, "clear hydrated ranges")?;
                if item.local_dirty {
                    return Err(StoreError::LocalChanges {
                        action: "clear the hydrated ranges of",
                        key: key.clone(),
                    });
                }
                clear_ranges(c, key)
            })
        })
    }

    /// Releases a file's local content (dehydrate): clears its ranges and sets hydration
    /// to `placeholder`, together. Requires the item to be clean and `in-sync`. Effective
    /// pin is the engine's check (ADR-0008 R5). The caller punches the holes afterwards.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`], [`StoreError::LocalChanges`]
    /// for a dirty item, [`StoreError::NotInSync`] for any sync state but `in-sync`. A
    /// refusal changes nothing.
    fn dehydrate(&self, key: &ItemKey) -> Result<(), StoreError> {
        run(self, |c| {
            atomic(c, |c| {
                let item = load_item(c, key, "dehydrate")?;
                if item.kind == ItemKind::Folder {
                    return Err(StoreError::NotAFile {
                        action: "dehydrate",
                        key: key.clone(),
                    });
                }
                if item.local_dirty {
                    return Err(StoreError::LocalChanges {
                        action: "dehydrate",
                        key: key.clone(),
                    });
                }
                if item.state.sync != SyncState::InSync {
                    return Err(StoreError::NotInSync {
                        key: key.clone(),
                        sync: item.state.sync,
                    });
                }
                clear_ranges(c, key)?;
                update_item(
                    c,
                    key,
                    "hydration",
                    Hydration::Placeholder.as_str(),
                    "dehydrate",
                )
            })
        })
    }
}

// ------------------------------------------------------------- implementations

fn run<Q, T, F>(q: &Q, op: F) -> Result<T, StoreError>
where
    Q: Queries + ?Sized,
    F: FnOnce(&Connection) -> Result<T, StoreError>,
{
    q.run(sealed::Token(()), op)
}

fn insert_mount(c: &Connection, mount: &Mount) -> Result<(), StoreError> {
    let mount_point = mount
        .mount_point
        .to_str()
        .ok_or_else(|| StoreError::NonUtf8Path {
            what: format!("mount point of mount {}", mount.mount_id),
            path: mount.mount_point.clone(),
        })?;
    let account_known: bool = c
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM accounts WHERE account_id = ?1)",
            [mount.account_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("look up account {}", mount.account_id))?;
    if !account_known {
        return Err(StoreError::AccountNotFound {
            action: "add a mount",
            account_id: mount.account_id.clone(),
        });
    }
    let taken: bool = c
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM mounts WHERE mount_id = ?1)",
            [mount.mount_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| format!("look up mount {}", mount.mount_id))?;
    if taken {
        return Err(StoreError::MountExists {
            mount_id: mount.mount_id.clone(),
        });
    }
    let same_root: Option<String> = c
        .query_row(
            "SELECT mount_id FROM mounts WHERE drive_id = ?1 AND root_item_id = ?2",
            [mount.drive_id.as_str(), mount.root_item_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .ctx(|| {
            format!(
                "check whether the root of mount {} is mounted",
                mount.mount_id
            )
        })?;
    if let Some(existing) = same_root {
        return Err(StoreError::MountRootTaken {
            mount_id: mount.mount_id.clone(),
            root: ItemKey::new(mount.drive_id.clone(), mount.root_item_id.clone()),
            existing: parse_id("mount row", "mount_id", existing, MountId::new)?,
        });
    }
    c.execute(
        &format!("INSERT INTO mounts ({MOUNT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
        params![
            mount.mount_id.as_str(),
            mount.account_id.as_str(),
            mount.drive_id.as_str(),
            mount.root_item_id.as_str(),
            mount_point,
            mount.delta_token.as_deref(),
            mount.read_only,
            mount.blocked_reason.as_deref(),
        ],
    )
    .ctx(|| format!("insert mount {}", mount.mount_id))?;
    Ok(())
}

fn update_mount(
    c: &Connection,
    mount_id: &MountId,
    column: &str,
    value: Option<&str>,
    action: &'static str,
) -> Result<(), StoreError> {
    let changed = c
        .execute(
            &format!("UPDATE mounts SET {column} = ?2 WHERE mount_id = ?1"),
            params![mount_id.as_str(), value],
        )
        .ctx(|| format!("{action} of mount {mount_id}"))?;
    if changed == 0 {
        return Err(StoreError::MountNotFound {
            action,
            mount_id: mount_id.clone(),
        });
    }
    Ok(())
}

fn update_item(
    c: &Connection,
    key: &ItemKey,
    column: &str,
    value: &str,
    action: &'static str,
) -> Result<(), StoreError> {
    let changed = c
        .execute(
            &format!("UPDATE items SET {column} = ?3 WHERE drive_id = ?1 AND item_id = ?2"),
            params![key.drive_id.as_str(), key.item_id.as_str(), value],
        )
        .ctx(|| describe(action, key))?;
    found(changed, key, action)
}

fn found(changed: usize, key: &ItemKey, action: &'static str) -> Result<(), StoreError> {
    if changed == 0 {
        return Err(StoreError::ItemNotFound {
            action,
            key: key.clone(),
        });
    }
    Ok(())
}

fn check_name(key: &ItemKey, name: &str) -> Result<(), StoreError> {
    let reason = if name.is_empty() {
        "is empty"
    } else if name == "." || name == ".." {
        "is a relative path component"
    } else if name.contains('/') {
        "contains '/'"
    } else if name.contains('\0') {
        "contains NUL"
    } else {
        return Ok(());
    };
    Err(StoreError::InvalidName {
        key: key.clone(),
        name: name.to_owned(),
        reason,
    })
}

fn to_i64_checked(size: u64, key: &ItemKey) -> Result<i64, StoreError> {
    i64::try_from(size).map_err(|_| StoreError::SizeTooLarge {
        key: key.clone(),
        size,
    })
}

fn insert_item(c: &Connection, item: &Item) -> Result<(), StoreError> {
    let key = &item.key;
    check_name(key, &item.name)?;
    let size = to_i64_checked(item.size, key)?;
    let invalid = |reason| StoreError::InvalidItem {
        key: key.clone(),
        reason,
    };
    // A new item is either known to the server and clean, or created locally: then it is
    // provisional, dirty and pending upload (F6). A dirty item has no in-sync state (F3).
    if item.is_provisional && !(item.local_dirty && item.state.sync == SyncState::PendingUpload) {
        return Err(invalid(
            "a provisional item must be inserted dirty and pending-upload",
        ));
    }
    if item.local_dirty && !item.is_provisional {
        return Err(invalid(
            "only a provisional item can be inserted dirty; mark local changes instead",
        ));
    }
    if item.remote_changed || item.last_remote.is_some() {
        return Err(invalid(
            "a new item has no remote change or remote identity recorded",
        ));
    }
    if item.local_dirty && item.kind == ItemKind::File && item.state.hydration != Hydration::Full {
        return Err(invalid(
            "a file created locally is fully local: its hydration must be full",
        ));
    }
    if item_exists(c, key)? {
        return Err(StoreError::ItemExists {
            action: "insert an item",
            key: key.clone(),
        });
    }
    c.execute(
        &format!(
            "INSERT INTO items ({INSERT_COLUMNS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
              ?18, ?19, ?20)"
        ),
        params![
            key.drive_id.as_str(),
            key.item_id.as_str(),
            item.parent_item_id.as_ref().map(ItemId::as_str),
            item.name,
            item.kind.as_str(),
            size,
            item.created.as_unix_seconds(),
            item.modified.as_unix_seconds(),
            item.etag.as_deref(),
            item.ctag.as_deref(),
            item.server_hash.as_ref().map(QuickXorHash::to_base64),
            item.local_hash.as_ref().map(QuickXorHash::to_base64),
            item.state.hydration.as_str(),
            item.state.pin.as_str(),
            item.state.sync.as_str(),
            item.last_error.as_deref(),
            item.is_provisional,
            item.local_dirty,
            generation_to_i64(item.local_generation, key)?,
            item.remote_changed,
        ],
    )
    .ctx(|| describe("insert", key))?;
    if item.local_dirty && item.kind == ItemKind::File {
        // Its bytes exist only here: the whole file is local.
        set_whole_file_range(c, key, size)?;
    }
    Ok(())
}

fn generation_to_i64(generation: u64, key: &ItemKey) -> Result<i64, StoreError> {
    i64::try_from(generation).map_err(|_| StoreError::Corrupt {
        what: describe("local generation of", key),
        value: generation.to_string(),
        reason: "larger than i64::MAX".to_owned(),
    })
}

/// The content identity the stored server columns of `item` describe. Meaningful as the
/// server version only while the item is clean; while dirty, `size` is the local size and
/// `last_remote` is used instead.
fn base_identity(item: &Item) -> RemoteIdentity {
    RemoteIdentity {
        etag: item.etag.clone(),
        ctag: item.ctag.clone(),
        hash: item.server_hash,
        size: item.size,
    }
}

/// Whether a report names different content from `known` (the version the store last
/// knew of). A kind change always counts; folders have no content identity.
fn content_changed(old: &Item, known: &RemoteIdentity, new: &RemoteMetadata) -> bool {
    if old.kind != new.kind {
        return true;
    }
    old.kind == ItemKind::File && !known.same_content(&new.identity())
}

fn mark_local_change(c: &Connection, key: &ItemKey) -> Result<u64, StoreError> {
    // On the clean -> dirty transition the base version's identity (server columns and the
    // size, which is still the server's) is recorded as the last remote identity, and an
    // `in-sync` or `pending-download` item becomes `pending-upload`, so a restart finds it
    // whatever happens next (F3). Every right-hand side reads the row as it was.
    let generation: Option<i64> = c
        .query_row(
            "UPDATE items SET local_dirty = 1, local_generation = local_generation + 1, \
             sync = CASE WHEN sync IN ('in-sync', 'pending-download') \
                    THEN 'pending-upload' ELSE sync END, \
             remote_etag = CASE WHEN local_dirty = 0 THEN etag ELSE remote_etag END, \
             remote_ctag = CASE WHEN local_dirty = 0 THEN ctag ELSE remote_ctag END, \
             remote_hash = CASE WHEN local_dirty = 0 THEN server_hash ELSE remote_hash END, \
             remote_size = CASE WHEN local_dirty = 0 THEN size ELSE remote_size END \
             WHERE drive_id = ?1 AND item_id = ?2 RETURNING local_generation",
            [key.drive_id.as_str(), key.item_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .ctx(|| describe("mark a local change of", key))?;
    let generation = generation.ok_or_else(|| StoreError::ItemNotFound {
        action: "mark a local change",
        key: key.clone(),
    })?;
    to_u64(generation, "items", "local_generation", key)
}

fn set_size(c: &Connection, key: &ItemKey, size: i64) -> Result<(), StoreError> {
    c.execute(
        "UPDATE items SET size = ?3 WHERE drive_id = ?1 AND item_id = ?2",
        params![key.drive_id.as_str(), key.item_id.as_str(), size],
    )
    .ctx(|| describe("set the size of", key))?;
    Ok(())
}

/// Refuses a file that is not fully local: hydration `full` and ranges covering every
/// byte. Folders pass.
fn require_fully_local(
    c: &Connection,
    item: &Item,
    action: &'static str,
) -> Result<(), StoreError> {
    if item.kind == ItemKind::Folder {
        return Ok(());
    }
    let whole = ByteRange::new(0, item.size)?;
    if item.state.hydration != Hydration::Full || !is_range_hydrated(c, &item.key, whole)? {
        return Err(StoreError::NotFullyLocal {
            action,
            key: item.key.clone(),
            hydration: item.state.hydration,
        });
    }
    Ok(())
}

/// Makes a file's ranges exactly `[0, size)`: a dirty file is fully local.
fn set_whole_file_range(c: &Connection, key: &ItemKey, size: i64) -> Result<(), StoreError> {
    clear_ranges(c, key)?;
    if size > 0 {
        c.execute(
            "INSERT INTO hydrated_ranges (drive_id, item_id, range_start, range_end) \
             VALUES (?1, ?2, 0, ?3)",
            params![key.drive_id.as_str(), key.item_id.as_str(), size],
        )
        .ctx(|| describe("record the whole of", key))?;
    }
    Ok(())
}

/// Removes recorded bytes at or past `size`. Merged ranges stay merged.
fn trim_ranges(c: &Connection, key: &ItemKey, size: i64) -> Result<(), StoreError> {
    let ids = [key.drive_id.as_str(), key.item_id.as_str()];
    c.execute(
        "DELETE FROM hydrated_ranges WHERE drive_id = ?1 AND item_id = ?2 AND range_start >= ?3",
        params![ids[0], ids[1], size],
    )
    .ctx(|| describe("trim the hydrated ranges of", key))?;
    c.execute(
        "UPDATE hydrated_ranges SET range_end = ?3 \
         WHERE drive_id = ?1 AND item_id = ?2 AND range_end > ?3",
        params![ids[0], ids[1], size],
    )
    .ctx(|| describe("trim the hydrated ranges of", key))?;
    Ok(())
}

fn complete_upload(
    c: &Connection,
    key: &ItemKey,
    expected_generation: u64,
    uploaded_local_hash: QuickXorHash,
    response: &RemoteMetadata,
    new_item_id: Option<&ItemId>,
) -> Result<UploadCompleted, StoreError> {
    let item = load_item(c, key, "complete an upload")?;
    if !item.local_dirty {
        // A replayed completion would overwrite newer server identity (F4).
        return Err(StoreError::NotDirty { key: key.clone() });
    }
    let mismatch = |reason: &str| StoreError::UploadMismatch {
        key: key.clone(),
        reason: reason.to_owned(),
    };
    let final_key = match (item.is_provisional, new_item_id) {
        (true, Some(id)) => {
            rekey_item(c, key, id)?;
            ItemKey::new(key.drive_id.clone(), id.clone())
        }
        (true, None) => {
            return Err(mismatch(
                "the item is provisional and the response gave no server item ID",
            ));
        }
        (false, Some(id)) if id != &key.item_id => {
            return Err(mismatch(
                "the item already has a server ID and the response gave another",
            ));
        }
        (false, _) => key.clone(),
    };
    if response.key != final_key {
        return Err(mismatch("the response describes a different item"));
    }
    check_name(&final_key, &response.name)?;
    let size = to_i64_checked(response.size, &final_key)?;
    let ids = [final_key.drive_id.as_str(), final_key.item_id.as_str()];

    // F1: a remote report seen while dirty that is not our own upload echoing back means
    // another client changed the file after our upload was accepted.
    let uploaded = response.identity();
    let moved_on = item.remote_changed
        && item
            .last_remote
            .as_ref()
            .is_none_or(|seen| !seen.same_content(&uploaded));
    // An echo: the delta already reported our uploaded content, possibly with newer
    // metadata (for example renamed, with a later eTag). Keep that newer parent, name, eTag
    // and cTag rather than the older upload response (fourth review, finding 2).
    let echo = match (&item.last_remote, item.remote_changed && !moved_on) {
        (Some(seen), true) => Some(seen),
        _ => None,
    };
    let parent = match echo {
        Some(_) => item.parent_item_id.as_ref().map(ItemId::as_str),
        None => response.parent_item_id.as_ref().map(ItemId::as_str),
    };
    let name = echo.map_or(response.name.as_str(), |_| item.name.as_str());
    let etag = echo.map_or(response.etag.as_deref(), |seen| seen.etag.as_deref());
    let ctag = echo.map_or(response.ctag.as_deref(), |seen| seen.ctag.as_deref());

    if item.local_generation != expected_generation {
        // Still dirty: the uploaded version becomes the base (its eTag is the next
        // If-Match), and size, timestamps and ranges keep describing the local content.
        let last_remote = if moved_on || echo.is_some() {
            item.last_remote.clone()
        } else {
            Some(uploaded)
        };
        c.execute(
            "UPDATE items SET parent_item_id = ?3, name = ?4, etag = ?5, ctag = ?6, \
             server_hash = ?7, local_hash = ?8, sync = ?9, remote_changed = ?10, \
             remote_etag = ?11, remote_ctag = ?12, remote_hash = ?13, remote_size = ?14 \
             WHERE drive_id = ?1 AND item_id = ?2",
            params![
                ids[0],
                ids[1],
                parent,
                name,
                etag,
                ctag,
                response.server_hash.as_ref().map(QuickXorHash::to_base64),
                uploaded_local_hash.to_base64(),
                SyncState::PendingUpload.as_str(),
                moved_on,
                last_remote.as_ref().and_then(|r| r.etag.as_deref()),
                last_remote.as_ref().and_then(|r| r.ctag.as_deref()),
                last_remote
                    .as_ref()
                    .and_then(|r| r.hash.as_ref().map(QuickXorHash::to_base64)),
                last_remote
                    .as_ref()
                    .map(|r| to_i64_checked(r.size, &final_key))
                    .transpose()?,
            ],
        )
        .ctx(|| describe("record the upload of", &final_key))?;
        return Ok(UploadCompleted::LocalChangedSince {
            generation: item.local_generation,
        });
    }

    // Our current bytes reached the server: the item is clean. If the server moved on
    // since, it is not in sync: `pending-download`, `remote_changed` kept.
    let sync = if moved_on {
        SyncState::PendingDownload
    } else {
        SyncState::InSync
    };
    c.execute(
        "UPDATE items SET parent_item_id = ?3, name = ?4, etag = ?5, ctag = ?6, \
         server_hash = ?7, local_hash = ?8, size = ?9, created_at = ?10, modified_at = ?11, \
         local_dirty = 0, sync = ?12, remote_changed = ?13, remote_etag = NULL, \
         remote_ctag = NULL, remote_hash = NULL, remote_size = NULL \
         WHERE drive_id = ?1 AND item_id = ?2",
        params![
            ids[0],
            ids[1],
            parent,
            name,
            etag,
            ctag,
            response.server_hash.as_ref().map(QuickXorHash::to_base64),
            uploaded_local_hash.to_base64(),
            size,
            response.created.as_unix_seconds(),
            response.modified.as_unix_seconds(),
            sync.as_str(),
            moved_on,
        ],
    )
    .ctx(|| describe("record the upload of", &final_key))?;
    trim_ranges(c, &final_key, size)?;
    Ok(if moved_on {
        UploadCompleted::UploadedButRemoteMovedOn
    } else {
        UploadCompleted::Synced
    })
}

fn apply_remote_metadata(
    c: &Connection,
    remote: &RemoteMetadata,
) -> Result<RemoteApplied, StoreError> {
    let key = &remote.key;
    check_name(key, &remote.name)?;
    to_i64_checked(remote.size, key)?;
    let old = load_item(c, key, "apply remote metadata")?;
    check_folder_to_file(c, &old, remote)?;
    let ids = [key.drive_id.as_str(), key.item_id.as_str()];

    if old.local_dirty {
        // Unsynced local content: write nothing that describes content, so size and
        // ranges keep describing the local bytes and the old eTag keeps the 412 check.
        // Compare with, then record, the last remote identity seen (F1, F9).
        let known = old
            .last_remote
            .clone()
            .unwrap_or_else(|| base_identity(&old));
        let changed = content_changed(&old, &known, remote);
        let reported = remote.identity();
        c.execute(
            "UPDATE items SET parent_item_id = ?3, name = ?4, \
             remote_changed = MAX(remote_changed, ?5), remote_etag = ?6, remote_ctag = ?7, \
             remote_hash = ?8, remote_size = ?9 WHERE drive_id = ?1 AND item_id = ?2",
            params![
                ids[0],
                ids[1],
                remote.parent_item_id.as_ref().map(ItemId::as_str),
                remote.name,
                changed,
                reported.etag.as_deref(),
                reported.ctag.as_deref(),
                reported.hash.as_ref().map(QuickXorHash::to_base64),
                to_i64_checked(reported.size, key)?,
            ],
        )
        .ctx(|| describe("apply a remote move of", key))?;
        return Ok(if changed {
            RemoteApplied::ContentChangedUnderLocalState {
                sync: old.state.sync,
            }
        } else {
            RemoteApplied::ContentUnchanged
        });
    }

    let replaced = content_changed(&old, &base_identity(&old), remote);
    write_server_columns(c, key, remote)?;
    if !replaced {
        return Ok(RemoteApplied::ContentUnchanged);
    }
    let ranges_cleared = release_content(c, key, remote.kind)?;
    Ok(RemoteApplied::ContentReplaced { ranges_cleared })
}

fn check_folder_to_file(
    c: &Connection,
    old: &Item,
    remote: &RemoteMetadata,
) -> Result<(), StoreError> {
    if old.kind == ItemKind::Folder && remote.kind == ItemKind::File {
        let children = child_count(c, &old.key)?;
        if children > 0 {
            return Err(StoreError::FolderHasChildren {
                key: old.key.clone(),
                children,
            });
        }
    }
    Ok(())
}

/// Writes every server-owned column of a clean item and clears `remote_changed`.
fn write_server_columns(
    c: &Connection,
    key: &ItemKey,
    remote: &RemoteMetadata,
) -> Result<(), StoreError> {
    c.execute(
        "UPDATE items SET parent_item_id = ?3, name = ?4, kind = ?5, size = ?6, \
         created_at = ?7, modified_at = ?8, etag = ?9, ctag = ?10, server_hash = ?11, \
         remote_changed = 0 WHERE drive_id = ?1 AND item_id = ?2",
        params![
            key.drive_id.as_str(),
            key.item_id.as_str(),
            remote.parent_item_id.as_ref().map(ItemId::as_str),
            remote.name,
            remote.kind.as_str(),
            to_i64_checked(remote.size, key)?,
            remote.created.as_unix_seconds(),
            remote.modified.as_unix_seconds(),
            remote.etag.as_deref(),
            remote.ctag.as_deref(),
            remote.server_hash.as_ref().map(QuickXorHash::to_base64),
        ],
    )
    .ctx(|| describe("apply remote metadata to", key))?;
    Ok(())
}

/// Clears a clean item's ranges and sets hydration to `placeholder` (`full` for a folder).
fn release_content(c: &Connection, key: &ItemKey, kind: ItemKind) -> Result<u64, StoreError> {
    let ranges_cleared = clear_ranges(c, key)?;
    let hydration = match kind {
        ItemKind::File => Hydration::Placeholder,
        ItemKind::Folder => Hydration::Full,
    };
    update_item(
        c,
        key,
        "hydration",
        hydration.as_str(),
        "reset the hydration",
    )?;
    Ok(ranges_cleared)
}

fn discard_local_changes(
    c: &Connection,
    key: &ItemKey,
    current: &RemoteMetadata,
) -> Result<(), StoreError> {
    let item = load_item(c, key, "discard local changes")?;
    if item.is_provisional {
        return Err(StoreError::ProvisionalItem {
            action: "discard the local changes of",
            key: key.clone(),
        });
    }
    if &current.key != key {
        return Err(StoreError::UploadMismatch {
            key: key.clone(),
            reason: "the server metadata describes a different item".to_owned(),
        });
    }
    // Demonstrably stale metadata: the server has reported a newer version
    // (`remote_changed`) and `current` is still the version the stored server columns
    // describe. Their size is not compared, since a dirty item's size is local.
    if item.remote_changed {
        let base = RemoteIdentity {
            size: current.size,
            ..base_identity(&item)
        };
        if base.same_content(&current.identity()) {
            return Err(StoreError::RemoteChanged {
                action: "discard, with stale server metadata, the local changes of",
                key: key.clone(),
            });
        }
    }
    check_name(key, &current.name)?;
    check_folder_to_file(c, &item, current)?;
    // Clean first, in one statement, so the CHECKs hold; then the item takes the
    // server's current version exactly as a clean item would (F2).
    c.execute(
        "UPDATE items SET local_dirty = 0, sync = ?3, remote_changed = 0, remote_etag = NULL, \
         remote_ctag = NULL, remote_hash = NULL, remote_size = NULL \
         WHERE drive_id = ?1 AND item_id = ?2",
        params![
            key.drive_id.as_str(),
            key.item_id.as_str(),
            SyncState::InSync.as_str()
        ],
    )
    .ctx(|| describe("discard the local changes of", key))?;
    write_server_columns(c, key, current)?;
    release_content(c, key, current.kind)?;
    Ok(())
}

fn rekey_item(c: &Connection, old: &ItemKey, new_item_id: &ItemId) -> Result<(), StoreError> {
    let item = load_item(c, old, "re-key an item")?;
    if !item.is_provisional {
        return Err(StoreError::NotProvisional { key: old.clone() });
    }
    let new = ItemKey::new(old.drive_id.clone(), new_item_id.clone());
    if item_exists(c, &new)? {
        return Err(StoreError::ItemExists {
            action: "re-key an item",
            key: new,
        });
    }
    // Hydrated ranges follow through the foreign key's ON UPDATE CASCADE.
    c.execute(
        "UPDATE items SET item_id = ?3, is_provisional = 0 \
         WHERE drive_id = ?1 AND item_id = ?2",
        params![
            old.drive_id.as_str(),
            old.item_id.as_str(),
            new_item_id.as_str()
        ],
    )
    .ctx(|| describe("re-key", old))?;
    c.execute(
        "UPDATE items SET parent_item_id = ?3 WHERE drive_id = ?1 AND parent_item_id = ?2",
        params![
            old.drive_id.as_str(),
            old.item_id.as_str(),
            new_item_id.as_str()
        ],
    )
    .ctx(|| describe("re-parent the children of", old))?;
    Ok(())
}

/// The recursive set of `?2` and its descendants in drive `?1`. UNION, not UNION ALL, so
/// a parent cycle cannot make it loop forever.
const SUBTREE: &str = "WITH RECURSIVE subtree (item_id) AS ( \
     SELECT ?2 UNION \
     SELECT i.item_id FROM items AS i JOIN subtree AS s \
     ON i.drive_id = ?1 AND i.parent_item_id = s.item_id) ";

fn delete_item(
    c: &Connection,
    key: &ItemKey,
    mode: DeleteMode,
    pending: PendingChanges,
) -> Result<u64, StoreError> {
    let item = load_item(c, key, "delete an item")?;
    // Which item, if any, the caller vouches for (F5): only the named one, and only if it
    // has not changed locally since the caller read its generation.
    let exempt_generation = match pending {
        PendingChanges::Refuse => None,
        PendingChanges::ServerConfirmedLocalDelete {
            expected_generation,
        } => {
            if item.is_provisional {
                return Err(StoreError::ProvisionalItem {
                    action: "confirm a server delete of",
                    key: key.clone(),
                });
            }
            Some(expected_generation)
        }
        PendingChanges::DiscardNeverUploaded {
            expected_generation,
        } => {
            if !item.is_provisional {
                return Err(StoreError::NotProvisional { key: key.clone() });
            }
            Some(expected_generation)
        }
    };
    if let Some(expected) = exempt_generation
        && item.local_generation != expected
    {
        return Err(StoreError::GenerationMismatch {
            key: key.clone(),
            expected,
            actual: item.local_generation,
        });
    }
    let ids = [key.drive_id.as_str(), key.item_id.as_str()];
    if mode == DeleteMode::ItemOnly {
        let children = child_count(c, key)?;
        if children > 0 {
            return Err(StoreError::HasChildren {
                key: key.clone(),
                children,
            });
        }
    }
    let root_of: Option<(String, String)> = c
        .query_row(
            &format!(
                "{SUBTREE} SELECT mount_id, root_item_id FROM mounts WHERE drive_id = ?1 \
                 AND root_item_id IN (SELECT item_id FROM subtree) \
                 ORDER BY mount_id LIMIT 1"
            ),
            ids,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ctx(|| describe("look for mount roots below", key))?;
    if let Some((mount_id, root)) = root_of {
        return Err(StoreError::MountRoot {
            key: ItemKey::new(
                key.drive_id.clone(),
                parse_id("mounts row", "root_item_id", root, ItemId::new)?,
            ),
            mount_id: parse_id("mounts row", "mount_id", mount_id, MountId::new)?,
        });
    }
    {
        let mut stmt = c
            .prepare(&format!(
                "{SUBTREE} SELECT item_id FROM items WHERE drive_id = ?1 \
                 AND item_id IN (SELECT item_id FROM subtree) AND local_dirty = 1 \
                 AND NOT (item_id = ?2 AND ?3) ORDER BY item_id"
            ))
            .ctx(|| describe("look for pending local changes below", key))?;
        let blocking: Vec<String> = stmt
            .query_map(
                params![ids[0], ids[1], exempt_generation.is_some()],
                |row| row.get(0),
            )
            .and_then(Iterator::collect)
            .ctx(|| describe("look for pending local changes below", key))?;
        if !blocking.is_empty() {
            let pending = blocking
                .into_iter()
                .map(|id| {
                    parse_id("items row", "item_id", id, ItemId::new)
                        .map(|item_id| ItemKey::new(key.drive_id.clone(), item_id))
                })
                .collect::<Result<_, _>>()?;
            return Err(StoreError::HasPendingLocalChanges {
                key: key.clone(),
                pending,
            });
        }
    }
    c.execute(
        &format!(
            "{SUBTREE} DELETE FROM hydrated_ranges WHERE drive_id = ?1 \
             AND item_id IN (SELECT item_id FROM subtree)"
        ),
        ids,
    )
    .ctx(|| describe("delete the hydrated ranges below", key))?;
    let deleted = c
        .execute(
            &format!(
                "{SUBTREE} DELETE FROM items WHERE drive_id = ?1 \
                 AND item_id IN (SELECT item_id FROM subtree)"
            ),
            ids,
        )
        .ctx(|| describe("delete", key))?;
    Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
}

fn resolve_path(
    c: &Connection,
    mount_id: &MountId,
    key: &ItemKey,
) -> Result<MountPath, StoreError> {
    let (drive_id, root): (String, String) = c
        .query_row(
            "SELECT drive_id, root_item_id FROM mounts WHERE mount_id = ?1",
            [mount_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ctx(|| format!("read mount {mount_id}"))?
        .ok_or_else(|| StoreError::MountNotFound {
            action: "resolve a path",
            mount_id: mount_id.clone(),
        })?;
    if drive_id != key.drive_id.as_str() {
        return Err(StoreError::WrongDrive {
            key: key.clone(),
            mount_id: mount_id.clone(),
        });
    }
    if !item_exists(c, key)? {
        return Err(StoreError::ItemNotFound {
            action: "resolve a path",
            key: key.clone(),
        });
    }
    let mut stmt = c
        .prepare_cached(
            "SELECT parent_item_id, name FROM items WHERE drive_id = ?1 AND item_id = ?2",
        )
        .ctx(|| describe("resolve the path of", key))?;
    let mut visited: HashSet<String> = HashSet::new();
    let mut names = Vec::new();
    let mut current = key.item_id.as_str().to_owned();
    while current != root {
        if !visited.insert(current.clone()) {
            return Err(StoreError::ParentCycle {
                key: key.clone(),
                repeated: parse_id("items row", "item_id", current, ItemId::new)?,
            });
        }
        let row: Option<(Option<String>, String)> = stmt
            .query_row([drive_id.as_str(), current.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .ctx(|| describe("resolve the path of", key))?;
        let Some((parent, name)) = row else {
            return Err(StoreError::MissingAncestor {
                key: key.clone(),
                missing: parse_id("items row", "parent_item_id", current, ItemId::new)?,
            });
        };
        names.push(name);
        match parent {
            Some(parent) => current = parent,
            None => {
                return Err(StoreError::NotInMount {
                    key: key.clone(),
                    mount_id: mount_id.clone(),
                });
            }
        }
    }
    names.reverse();
    Ok(MountPath::new(names))
}

fn record_range(c: &Connection, key: &ItemKey, range: ByteRange) -> Result<(), StoreError> {
    file_size_checked(c, key, range, "record a hydrated range")?;
    // A dirty file is already fully local; the local-write operations keep its ranges
    // exactly [0, size). A separate record here could only describe a download landing
    // over unsynced bytes.
    if load_item(c, key, "record a hydrated range")?.local_dirty {
        return Err(StoreError::LocalChanges {
            action: "record a hydrated range of",
            key: key.clone(),
        });
    }
    if range.is_empty() {
        return Ok(());
    }
    let start = to_i64(range.start());
    let end = to_i64(range.end());
    let ids = [key.drive_id.as_str(), key.item_id.as_str()];
    // Ranges are stored merged: no two overlap or touch. Every stored range that overlaps
    // or touches the new one is replaced by their union.
    let (merged_start, merged_end): (Option<i64>, Option<i64>) = c
        .query_row(
            "SELECT MIN(range_start), MAX(range_end) FROM hydrated_ranges \
             WHERE drive_id = ?1 AND item_id = ?2 AND range_start <= ?4 AND range_end >= ?3",
            params![ids[0], ids[1], start, end],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ctx(|| describe("read the hydrated ranges of", key))?;
    let merged_start = merged_start.map_or(start, |s| s.min(start));
    let merged_end = merged_end.map_or(end, |e| e.max(end));
    c.execute(
        "DELETE FROM hydrated_ranges WHERE drive_id = ?1 AND item_id = ?2 \
         AND range_start <= ?4 AND range_end >= ?3",
        params![ids[0], ids[1], start, end],
    )
    .ctx(|| describe("merge the hydrated ranges of", key))?;
    c.execute(
        "INSERT INTO hydrated_ranges (drive_id, item_id, range_start, range_end) \
         VALUES (?1, ?2, ?3, ?4)",
        params![ids[0], ids[1], merged_start, merged_end],
    )
    .ctx(|| describe("record a hydrated range of", key))?;
    Ok(())
}

fn is_range_hydrated(c: &Connection, key: &ItemKey, range: ByteRange) -> Result<bool, StoreError> {
    file_size_checked(c, key, range, "check a hydrated range")?;
    if range.is_empty() {
        return Ok(true);
    }
    // Stored ranges never overlap or touch, so one stored range must hold the whole
    // query: the last one starting at or before the query's start.
    let covering_end: Option<i64> = c
        .query_row(
            "SELECT range_end FROM hydrated_ranges \
             WHERE drive_id = ?1 AND item_id = ?2 AND range_start <= ?3 \
             ORDER BY range_start DESC LIMIT 1",
            params![
                key.drive_id.as_str(),
                key.item_id.as_str(),
                to_i64(range.start())
            ],
            |row| row.get(0),
        )
        .optional()
        .ctx(|| describe("check the hydrated ranges of", key))?;
    Ok(covering_end.is_some_and(|end| end >= to_i64(range.end())))
}

fn ranges_of(c: &Connection, key: &ItemKey) -> Result<Vec<ByteRange>, StoreError> {
    let mut stmt = c
        .prepare_cached(
            "SELECT range_start, range_end FROM hydrated_ranges \
             WHERE drive_id = ?1 AND item_id = ?2 ORDER BY range_start",
        )
        .ctx(|| describe("list the hydrated ranges of", key))?;
    let raw: Vec<(i64, i64)> = stmt
        .query_map([key.drive_id.as_str(), key.item_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .and_then(Iterator::collect)
        .ctx(|| describe("list the hydrated ranges of", key))?;
    raw.into_iter()
        .map(|(s, e)| {
            let start = to_u64(s, "hydrated_ranges", "range_start", key)?;
            let end = to_u64(e, "hydrated_ranges", "range_end", key)?;
            ByteRange::new(start, end).map_err(|_| StoreError::Corrupt {
                what: describe("hydrated_ranges row of", key),
                value: format!("{s}..{e}"),
                reason: "end is before start".to_owned(),
            })
        })
        .collect()
}

fn clear_ranges(c: &Connection, key: &ItemKey) -> Result<u64, StoreError> {
    let removed = c
        .execute(
            "DELETE FROM hydrated_ranges WHERE drive_id = ?1 AND item_id = ?2",
            [key.drive_id.as_str(), key.item_id.as_str()],
        )
        .ctx(|| describe("clear the hydrated ranges of", key))?;
    Ok(u64::try_from(removed).unwrap_or(u64::MAX))
}

fn child_count(c: &Connection, key: &ItemKey) -> Result<u64, StoreError> {
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM items WHERE drive_id = ?1 AND parent_item_id = ?2",
            [key.drive_id.as_str(), key.item_id.as_str()],
            |row| row.get(0),
        )
        .ctx(|| describe("count the children of", key))?;
    to_u64(n, "items", "COUNT(*)", key)
}

// ------------------------------------------------------------------- helpers

/// Runs `f` inside a savepoint: all of its statements take effect, or none do.
///
/// Works both on a bare connection, where the savepoint is its own transaction and
/// releasing it commits, and inside a caller's transaction, where it nests.
fn atomic<T>(
    c: &Connection,
    f: impl FnOnce(&Connection) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let savepoint = Savepoint::begin(c)?;
    let value = f(c)?;
    savepoint.release()?;
    Ok(value)
}

/// Rolls back to its savepoint when dropped unless released, so an early return or a
/// panic in an operation never leaves half of it applied.
struct Savepoint<'c> {
    conn: &'c Connection,
    released: bool,
}

impl<'c> Savepoint<'c> {
    fn begin(conn: &'c Connection) -> Result<Self, StoreError> {
        conn.execute_batch("SAVEPOINT mydrive_op")
            .ctx(|| "begin a savepoint")?;
        Ok(Self {
            conn,
            released: false,
        })
    }

    fn release(mut self) -> Result<(), StoreError> {
        self.released = true;
        if let Err(source) = self.conn.execute_batch("RELEASE mydrive_op") {
            // The release (a commit, outside a transaction) failed and the savepoint may
            // still be open: undo it rather than leave a transaction hanging.
            self.released = false;
            return Err(StoreError::Sqlite {
                action: "release a savepoint".to_owned(),
                source,
            });
        }
        Ok(())
    }
}

impl Drop for Savepoint<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let Err(error) = self
            .conn
            .execute_batch("ROLLBACK TO mydrive_op; RELEASE mydrive_op")
        else {
            return;
        };
        // If SQLite already rolled the whole transaction back (after an I/O error, for
        // example) the savepoint is gone and there is nothing left to undo. Otherwise part
        // of this operation may still be applied inside an open transaction: roll the
        // whole transaction back, so the caller's `Transaction` sees autocommit and refuses
        // every further call (`TransactionAborted`) instead of committing half of it.
        if !self.conn.is_autocommit() {
            tracing::warn!(%error, "rolling back a store savepoint failed; aborting the transaction");
            if let Err(error) = self.conn.execute_batch("ROLLBACK") {
                tracing::error!(%error, "rolling back the store transaction failed");
            }
        }
    }
}

fn describe(action: &str, key: &ItemKey) -> String {
    format!("{action} item {} in drive {}", key.item_id, key.drive_id)
}

/// Converts a value already checked to fit (sizes are at most `i64::MAX`).
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64, table: &str, column: &str, key: &ItemKey) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt {
        what: format!("{} {column}", describe(&format!("{table} row of"), key)),
        value: value.to_string(),
        reason: "negative".to_owned(),
    })
}

fn item_exists(c: &Connection, key: &ItemKey) -> Result<bool, StoreError> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM items WHERE drive_id = ?1 AND item_id = ?2)",
        [key.drive_id.as_str(), key.item_id.as_str()],
        |row| row.get(0),
    )
    .ctx(|| describe("look up", key))
}

fn fetch_item(c: &Connection, key: &ItemKey) -> Result<Option<Item>, StoreError> {
    c.query_row(
        &format!("SELECT {ITEM_COLUMNS} FROM items WHERE drive_id = ?1 AND item_id = ?2"),
        [key.drive_id.as_str(), key.item_id.as_str()],
        RawItem::from_row,
    )
    .optional()
    .ctx(|| describe("read", key))?
    .map(RawItem::into_item)
    .transpose()
}

fn load_item(c: &Connection, key: &ItemKey, action: &'static str) -> Result<Item, StoreError> {
    fetch_item(c, key)?.ok_or_else(|| StoreError::ItemNotFound {
        action,
        key: key.clone(),
    })
}

/// The size of a file, refusing missing items and folders.
fn file_size(c: &Connection, key: &ItemKey, action: &'static str) -> Result<u64, StoreError> {
    let item = load_item(c, key, action)?;
    if item.kind == ItemKind::Folder {
        return Err(StoreError::NotAFile {
            action,
            key: key.clone(),
        });
    }
    Ok(item.size)
}

/// As [`file_size`], and refuses a range that ends past the end of the file.
fn file_size_checked(
    c: &Connection,
    key: &ItemKey,
    range: ByteRange,
    action: &'static str,
) -> Result<u64, StoreError> {
    let size = file_size(c, key, action)?;
    if range.end() > size {
        return Err(StoreError::RangeOutOfBounds {
            action,
            key: key.clone(),
            range,
            size,
        });
    }
    Ok(size)
}

fn query_items<S: Into<String>>(
    c: &Connection,
    sql: &str,
    params: &[&str],
    action: impl Fn() -> S,
) -> Result<Vec<Item>, StoreError> {
    let mut stmt = c.prepare_cached(sql).ctx(&action)?;
    let raws: Vec<RawItem> = stmt
        .query_map(rusqlite::params_from_iter(params), RawItem::from_row)
        .and_then(Iterator::collect)
        .ctx(&action)?;
    raws.into_iter().map(RawItem::into_item).collect()
}

fn parse_id<T, E: std::fmt::Display>(
    what: &str,
    column: &str,
    value: String,
    build: impl FnOnce(String) -> Result<T, E>,
) -> Result<T, StoreError> {
    build(value.clone()).map_err(|e| StoreError::Corrupt {
        what: format!("{what} {column}"),
        value,
        reason: e.to_string(),
    })
}

fn parse_state<T: std::str::FromStr<Err = mydrive_core::state::ParseStateError>>(
    value: String,
    column: &str,
    key: &ItemKey,
) -> Result<T, StoreError> {
    value.parse().map_err(
        |e: mydrive_core::state::ParseStateError| StoreError::Corrupt {
            what: format!("{} {column}", describe("items row of", key)),
            value,
            reason: e.to_string(),
        },
    )
}

fn parse_hash(
    value: Option<String>,
    column: &str,
    key: &ItemKey,
) -> Result<Option<QuickXorHash>, StoreError> {
    value
        .map(|v| {
            QuickXorHash::from_base64(&v).map_err(|e| StoreError::Corrupt {
                what: format!("{} {column}", describe("items row of", key)),
                value: v,
                reason: e.to_string(),
            })
        })
        .transpose()
}

/// An `items` row as SQLite returns it, before validation.
struct RawItem {
    drive_id: String,
    item_id: String,
    parent_item_id: Option<String>,
    name: String,
    kind: String,
    size: i64,
    created_at: i64,
    modified_at: i64,
    etag: Option<String>,
    ctag: Option<String>,
    server_hash: Option<String>,
    local_hash: Option<String>,
    hydration: String,
    pin: String,
    sync: String,
    last_error: Option<String>,
    is_provisional: bool,
    local_dirty: bool,
    local_generation: i64,
    remote_changed: bool,
    remote_etag: Option<String>,
    remote_ctag: Option<String>,
    remote_hash: Option<String>,
    remote_size: Option<i64>,
}

impl RawItem {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            drive_id: row.get(0)?,
            item_id: row.get(1)?,
            parent_item_id: row.get(2)?,
            name: row.get(3)?,
            kind: row.get(4)?,
            size: row.get(5)?,
            created_at: row.get(6)?,
            modified_at: row.get(7)?,
            etag: row.get(8)?,
            ctag: row.get(9)?,
            server_hash: row.get(10)?,
            local_hash: row.get(11)?,
            hydration: row.get(12)?,
            pin: row.get(13)?,
            sync: row.get(14)?,
            last_error: row.get(15)?,
            is_provisional: row.get(16)?,
            local_dirty: row.get(17)?,
            local_generation: row.get(18)?,
            remote_changed: row.get(19)?,
            remote_etag: row.get(20)?,
            remote_ctag: row.get(21)?,
            remote_hash: row.get(22)?,
            remote_size: row.get(23)?,
        })
    }

    fn into_item(self) -> Result<Item, StoreError> {
        let what = format!("items row {} in drive {}", self.item_id, self.drive_id);
        let key = ItemKey::new(
            parse_id(&what, "drive_id", self.drive_id, DriveId::new)?,
            parse_id(&what, "item_id", self.item_id, ItemId::new)?,
        );
        let kind = self.kind.parse().map_err(|reason| StoreError::Corrupt {
            what: format!("{what} kind"),
            value: self.kind.clone(),
            reason,
        })?;
        Ok(Item {
            parent_item_id: self
                .parent_item_id
                .map(|p| parse_id(&what, "parent_item_id", p, ItemId::new))
                .transpose()?,
            name: self.name,
            kind,
            size: to_u64(self.size, "items", "size", &key)?,
            created: Timestamp::from_unix_seconds(self.created_at),
            modified: Timestamp::from_unix_seconds(self.modified_at),
            etag: self.etag,
            ctag: self.ctag,
            server_hash: parse_hash(self.server_hash, "server_hash", &key)?,
            local_hash: parse_hash(self.local_hash, "local_hash", &key)?,
            state: ItemState {
                hydration: parse_state::<Hydration>(self.hydration, "hydration", &key)?,
                pin: parse_state::<Pin>(self.pin, "pin", &key)?,
                sync: parse_state::<SyncState>(self.sync, "sync", &key)?,
            },
            last_error: self.last_error,
            is_provisional: self.is_provisional,
            local_dirty: self.local_dirty,
            local_generation: to_u64(self.local_generation, "items", "local_generation", &key)?,
            remote_changed: self.remote_changed,
            last_remote: match self.remote_size {
                None => None,
                Some(size) => Some(RemoteIdentity {
                    etag: self.remote_etag,
                    ctag: self.remote_ctag,
                    hash: parse_hash(self.remote_hash, "remote_hash", &key)?,
                    size: to_u64(size, "items", "remote_size", &key)?,
                }),
            },
            key,
        })
    }
}

/// A `mounts` row as SQLite returns it, before validation.
struct RawMount {
    mount_id: String,
    account_id: String,
    drive_id: String,
    root_item_id: String,
    mount_point: String,
    delta_token: Option<String>,
    read_only: bool,
    blocked_reason: Option<String>,
}

impl RawMount {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            mount_id: row.get(0)?,
            account_id: row.get(1)?,
            drive_id: row.get(2)?,
            root_item_id: row.get(3)?,
            mount_point: row.get(4)?,
            delta_token: row.get(5)?,
            read_only: row.get(6)?,
            blocked_reason: row.get(7)?,
        })
    }

    fn into_mount(self) -> Result<Mount, StoreError> {
        let what = format!("mounts row {}", self.mount_id);
        Ok(Mount {
            mount_id: parse_id(&what, "mount_id", self.mount_id.clone(), MountId::new)?,
            account_id: parse_id(&what, "account_id", self.account_id, AccountId::new)?,
            drive_id: parse_id(&what, "drive_id", self.drive_id, DriveId::new)?,
            root_item_id: parse_id(&what, "root_item_id", self.root_item_id, ItemId::new)?,
            mount_point: PathBuf::from(self.mount_point),
            delta_token: self.delta_token,
            read_only: self.read_only,
            blocked_reason: self.blocked_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap()
    }

    fn failing_op(c: &Connection) -> Result<(), StoreError> {
        atomic(c, |c| {
            c.execute("INSERT INTO t VALUES (1)", []).ctx(|| "insert")?;
            // A second statement fails after the first has run.
            c.execute("INSERT INTO missing VALUES (1)", [])
                .ctx(|| "insert")?;
            Ok(())
        })
    }

    /// An operation that fails part way (here a statement error, not a SQLite-initiated
    /// rollback) leaves none of its statements applied, on a bare connection and inside a
    /// transaction that then commits.
    #[test]
    fn atomic_rolls_back_a_partly_applied_operation() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();

        assert!(failing_op(&c).is_err());
        assert_eq!(count(&c), 0);
        assert!(c.is_autocommit(), "no transaction left open");

        let tx = c.transaction().unwrap();
        tx.execute("INSERT INTO t VALUES (0)", []).unwrap();
        assert!(failing_op(&tx).is_err());
        tx.commit().unwrap();
        assert_eq!(
            count(&c),
            1,
            "only the caller's own statement was committed"
        );
    }

    /// Review N3: when rolling back to the savepoint itself fails while a transaction is
    /// open, the whole transaction is rolled back, so the connection is in autocommit and
    /// `Transaction` refuses every further call (`TransactionAborted`) rather than
    /// committing half an operation.
    #[test]
    fn failed_savepoint_rollback_aborts_the_transaction() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        let tx = c.transaction().unwrap();
        tx.execute("INSERT INTO t VALUES (0)", []).unwrap();
        let result: Result<(), StoreError> = atomic(&tx, |c| {
            c.execute("INSERT INTO t VALUES (1)", []).ctx(|| "insert")?;
            // Make the savepoint disappear, so ROLLBACK TO cannot undo the insert.
            c.execute_batch("RELEASE mydrive_op").ctx(|| "release")?;
            c.execute("INSERT INTO missing VALUES (1)", [])
                .ctx(|| "insert")?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(tx.is_autocommit(), "the whole transaction was rolled back");
        drop(tx);
        assert_eq!(count(&c), 0, "nothing from the transaction survived");
    }
}
