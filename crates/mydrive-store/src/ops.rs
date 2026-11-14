//! The typed operations, shared by [`Store`](crate::Store) and
//! [`Transaction`](crate::Transaction).
//!
//! Every operation that runs more than one statement runs inside its own savepoint, so it
//! is atomic whether it is called on its own or inside a caller's transaction: if it fails
//! part way, none of its statements take effect, and a caller who carries on and commits
//! cannot commit half of it.

use std::collections::HashSet;
use std::path::PathBuf;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{SqlContext, StoreError};
use crate::types::{Account, ByteRange, DeleteMode, Item, ItemKind, Mount, MountPath, Timestamp};

pub(crate) mod sealed {
    /// Proof of being inside this crate; nothing outside can build one.
    #[derive(Debug, Clone, Copy)]
    pub struct Token(pub(super) ());

    /// Gives [`Queries`](super::Queries) its connection without exposing it.
    pub trait Connected {
        /// The connection to run statements on.
        fn connection(&self, token: Token) -> &rusqlite::Connection;
    }
}

const ITEM_COLUMNS: &str = "drive_id, item_id, parent_item_id, name, kind, size, created_at, \
     modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error";

const MOUNT_COLUMNS: &str = "mount_id, account_id, drive_id, root_item_id, mount_point, delta_token, read_only, \
     blocked_reason";

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
        conn(self)
            .execute(
                "INSERT INTO accounts (account_id, tenant_id) VALUES (?1, ?2) \
                 ON CONFLICT (account_id) DO UPDATE SET tenant_id = excluded.tenant_id",
                params![
                    account.account_id.as_str(),
                    account.tenant_id.as_ref().map(TenantId::as_str)
                ],
            )
            .ctx(|| format!("store account {}", account.account_id))?;
        Ok(())
    }

    /// Fetches an account by ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row holds
    /// an invalid identifier.
    fn account(&self, account_id: &AccountId) -> Result<Option<Account>, StoreError> {
        let raw: Option<(String, Option<String>)> = conn(self)
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
    }

    // ------------------------------------------------------------------ mounts

    /// Adds a mount.
    ///
    /// # Errors
    ///
    /// [`StoreError::AccountNotFound`] if its account is not stored,
    /// [`StoreError::MountExists`] if the mount ID is taken, [`StoreError::MountRootTaken`]
    /// if another mount has the same root (`drive_id`, `item_id`) (ADR-0007 R4),
    /// [`StoreError::NonUtf8Path`] if the mount point is not UTF-8.
    ///
    /// The mount point is not a key and is not checked here: refusing a mount point that
    /// is in use, nested or overlapping is the engine's check (overview, "Rulings on
    /// details"), and ADR-0007 R6 forbids paths as keys.
    fn insert_mount(&self, mount: &Mount) -> Result<(), StoreError> {
        atomic(conn(self), |c| {
            let mount_point =
                mount
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
            if mount_exists(c, &mount.mount_id)? {
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
                &format!(
                    "INSERT INTO mounts ({MOUNT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
                ),
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
        })
    }

    /// Fetches a mount by ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row is
    /// invalid.
    fn mount(&self, mount_id: &MountId) -> Result<Option<Mount>, StoreError> {
        conn(self)
            .query_row(
                &format!("SELECT {MOUNT_COLUMNS} FROM mounts WHERE mount_id = ?1"),
                [mount_id.as_str()],
                RawMount::from_row,
            )
            .optional()
            .ctx(|| format!("read mount {mount_id}"))?
            .map(RawMount::into_mount)
            .transpose()
    }

    /// Lists every mount, ordered by mount ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn mounts(&self) -> Result<Vec<Mount>, StoreError> {
        let c = conn(self);
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
    }

    /// Reads a mount's delta token (ADR-0007 R2). `None` means no token has been saved.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`] if the mount is not stored.
    fn delta_token(&self, mount_id: &MountId) -> Result<Option<String>, StoreError> {
        conn(self)
            .query_row(
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
    }

    /// Replaces a mount's delta token; `None` clears it, so the next delta enumerates the
    /// whole drive. The token is never logged.
    ///
    /// # Errors
    ///
    /// [`StoreError::MountNotFound`] if the mount is not stored.
    fn set_delta_token(&self, mount_id: &MountId, token: Option<&str>) -> Result<(), StoreError> {
        let changed = conn(self)
            .execute(
                "UPDATE mounts SET delta_token = ?2 WHERE mount_id = ?1",
                params![mount_id.as_str(), token],
            )
            .ctx(|| format!("write the delta token of mount {mount_id}"))?;
        if changed == 0 {
            return Err(StoreError::MountNotFound {
                action: "write the delta token",
                mount_id: mount_id.clone(),
            });
        }
        Ok(())
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
        let changed = conn(self)
            .execute(
                "UPDATE mounts SET blocked_reason = ?2 WHERE mount_id = ?1",
                params![mount_id.as_str(), reason],
            )
            .ctx(|| format!("write the blocked reason of mount {mount_id}"))?;
        if changed == 0 {
            return Err(StoreError::MountNotFound {
                action: "write the blocked reason",
                mount_id: mount_id.clone(),
            });
        }
        Ok(())
    }

    // ------------------------------------------------------------------- items

    /// Inserts the item, or replaces every column of the stored item with the same key.
    ///
    /// Moving or renaming an item is an upsert with a new parent or name: one row changes,
    /// and every descendant's resolved path follows (WP-006 AC4).
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidName`] if the name is empty, `.`, `..`, or contains `/` or NUL;
    /// [`StoreError::SizeTooLarge`] if the size exceeds `i64::MAX`;
    /// [`StoreError::RangesWouldExceedItem`] if the item has hydrated ranges that the new
    /// kind or size cannot hold (clear them first).
    fn upsert_item(&self, item: &Item) -> Result<(), StoreError> {
        let key = &item.key;
        if let Some(reason) = name_problem(&item.name) {
            return Err(StoreError::InvalidName {
                key: key.clone(),
                name: item.name.clone(),
                reason,
            });
        }
        let size = i64::try_from(item.size).map_err(|_| StoreError::SizeTooLarge {
            key: key.clone(),
            size: item.size,
        })?;
        atomic(conn(self), |c| {
            let covered_end: Option<i64> = c
                .query_row(
                    "SELECT MAX(range_end) FROM hydrated_ranges \
                     WHERE drive_id = ?1 AND item_id = ?2",
                    [key.drive_id.as_str(), key.item_id.as_str()],
                    |row| row.get(0),
                )
                .ctx(|| describe("read the hydrated ranges of", key))?;
            if let Some(end) = covered_end
                && (item.kind == ItemKind::Folder || end > size)
            {
                return Err(StoreError::RangesWouldExceedItem {
                    key: key.clone(),
                    kind: item.kind,
                    size: item.size,
                    covered_end: to_u64(end, "hydrated_ranges", "range_end", key)?,
                });
            }
            c.execute(
                &format!(
                    "INSERT INTO items ({ITEM_COLUMNS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
                     ON CONFLICT (drive_id, item_id) DO UPDATE SET \
                     parent_item_id = excluded.parent_item_id, name = excluded.name, \
                     kind = excluded.kind, size = excluded.size, \
                     created_at = excluded.created_at, modified_at = excluded.modified_at, \
                     etag = excluded.etag, ctag = excluded.ctag, \
                     server_hash = excluded.server_hash, local_hash = excluded.local_hash, \
                     hydration = excluded.hydration, pin = excluded.pin, sync = excluded.sync, \
                     last_error = excluded.last_error"
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
                    item.etag,
                    item.ctag.as_deref(),
                    item.server_hash.as_ref().map(QuickXorHash::to_base64),
                    item.local_hash.as_ref().map(QuickXorHash::to_base64),
                    item.state.hydration.as_str(),
                    item.state.pin.as_str(),
                    item.state.sync.as_str(),
                    item.last_error.as_deref(),
                ],
            )
            .ctx(|| describe("store", key))?;
            Ok(())
        })
    }

    /// Fetches an item by key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if the row is
    /// invalid.
    fn item(&self, key: &ItemKey) -> Result<Option<Item>, StoreError> {
        conn(self)
            .query_row(
                &format!("SELECT {ITEM_COLUMNS} FROM items WHERE drive_id = ?1 AND item_id = ?2"),
                [key.drive_id.as_str(), key.item_id.as_str()],
                RawItem::from_row,
            )
            .optional()
            .ctx(|| describe("read", key))?
            .map(RawItem::into_item)
            .transpose()
    }

    /// Lists the items whose parent is `parent`, ordered by name then item ID.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn children(&self, parent: &ItemKey) -> Result<Vec<Item>, StoreError> {
        query_items(
            conn(self),
            &format!(
                "SELECT {ITEM_COLUMNS} FROM items WHERE drive_id = ?1 AND parent_item_id = ?2 \
                 ORDER BY name, item_id"
            ),
            &[parent.drive_id.as_str(), parent.item_id.as_str()],
            || describe("list the children of", parent),
        )
    }

    /// Lists every item, in any drive, whose sync state is `sync`, ordered by key.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the read fails, [`StoreError::Corrupt`] if a row is
    /// invalid.
    fn items_with_sync(&self, sync: SyncState) -> Result<Vec<Item>, StoreError> {
        query_items(
            conn(self),
            &format!("SELECT {ITEM_COLUMNS} FROM items WHERE sync = ?1 ORDER BY drive_id, item_id"),
            &[sync.as_str()],
            || format!("list items with sync state {sync}"),
        )
    }

    /// Deletes an item and its hydrated ranges, and returns how many items were deleted.
    ///
    /// With [`DeleteMode::ItemOnly`] an item that has children is refused. With
    /// [`DeleteMode::Recursive`] every descendant goes too, with its ranges. Deleting rows
    /// never touches the backing directory; whether a delete is allowed for an item with
    /// pending local changes is the engine's decision.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`] if the item is not stored, [`StoreError::HasChildren`]
    /// for a non-recursive delete of an item with children.
    fn delete_item(&self, key: &ItemKey, mode: DeleteMode) -> Result<u64, StoreError> {
        atomic(conn(self), |c| {
            if !item_exists(c, key)? {
                return Err(StoreError::ItemNotFound {
                    action: "delete an item",
                    key: key.clone(),
                });
            }
            let ids = [key.drive_id.as_str(), key.item_id.as_str()];
            let deleted = match mode {
                DeleteMode::ItemOnly => {
                    let children: i64 = c
                        .query_row(
                            "SELECT COUNT(*) FROM items \
                             WHERE drive_id = ?1 AND parent_item_id = ?2",
                            ids,
                            |row| row.get(0),
                        )
                        .ctx(|| describe("count the children of", key))?;
                    if children > 0 {
                        return Err(StoreError::HasChildren {
                            key: key.clone(),
                            children: to_u64(children, "items", "COUNT(*)", key)?,
                        });
                    }
                    c.execute(
                        "DELETE FROM hydrated_ranges WHERE drive_id = ?1 AND item_id = ?2",
                        ids,
                    )
                    .ctx(|| describe("delete the hydrated ranges of", key))?;
                    c.execute(
                        "DELETE FROM items WHERE drive_id = ?1 AND item_id = ?2",
                        ids,
                    )
                    .ctx(|| describe("delete", key))?
                }
                DeleteMode::Recursive => {
                    // UNION, not UNION ALL: a parent cycle cannot make this loop forever.
                    const SUBTREE: &str = "WITH RECURSIVE subtree (item_id) AS ( \
                         SELECT ?2 UNION \
                         SELECT i.item_id FROM items AS i JOIN subtree AS s \
                         ON i.drive_id = ?1 AND i.parent_item_id = s.item_id) ";
                    c.execute(
                        &format!(
                            "{SUBTREE} DELETE FROM hydrated_ranges WHERE drive_id = ?1 \
                             AND item_id IN (SELECT item_id FROM subtree)"
                        ),
                        ids,
                    )
                    .ctx(|| describe("delete the hydrated ranges below", key))?;
                    c.execute(
                        &format!(
                            "{SUBTREE} DELETE FROM items WHERE drive_id = ?1 \
                             AND item_id IN (SELECT item_id FROM subtree)"
                        ),
                        ids,
                    )
                    .ctx(|| describe("delete the subtree of", key))?
                }
            };
            Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
        })
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
        atomic(conn(self), |c| {
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
        })
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
        const ACTION: &str = "record a hydrated range";
        atomic(conn(self), |c| {
            file_size_checked(c, key, range, ACTION)?;
            if range.is_empty() {
                return Ok(());
            }
            let start = to_i64(range.start());
            let end = to_i64(range.end());
            let ids = [key.drive_id.as_str(), key.item_id.as_str()];
            // Ranges are stored merged: no two overlap or touch. Every stored range that
            // overlaps or touches the new one is replaced by their union.
            let (merged_start, merged_end): (Option<i64>, Option<i64>) = c
                .query_row(
                    "SELECT MIN(range_start), MAX(range_end) FROM hydrated_ranges \
                     WHERE drive_id = ?1 AND item_id = ?2 \
                     AND range_start <= ?4 AND range_end >= ?3",
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
        })
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
        atomic(conn(self), |c| {
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
        })
    }

    /// The recorded hydrated ranges of a file, in order. They never overlap or touch.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`], [`StoreError::NotAFile`] for a folder.
    fn hydrated_ranges(&self, key: &ItemKey) -> Result<Vec<ByteRange>, StoreError> {
        atomic(conn(self), |c| {
            file_size(c, key, "list hydrated ranges")?;
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
        })
    }

    /// Forgets every hydrated range of an item, for example when its content is released
    /// or replaced. Returns how many ranges were removed.
    ///
    /// # Errors
    ///
    /// [`StoreError::ItemNotFound`] if the item is not stored.
    fn clear_hydrated_ranges(&self, key: &ItemKey) -> Result<u64, StoreError> {
        atomic(conn(self), |c| {
            if !item_exists(c, key)? {
                return Err(StoreError::ItemNotFound {
                    action: "clear hydrated ranges",
                    key: key.clone(),
                });
            }
            let removed = c
                .execute(
                    "DELETE FROM hydrated_ranges WHERE drive_id = ?1 AND item_id = ?2",
                    [key.drive_id.as_str(), key.item_id.as_str()],
                )
                .ctx(|| describe("clear the hydrated ranges of", key))?;
            Ok(u64::try_from(removed).unwrap_or(u64::MAX))
        })
    }
}

// ------------------------------------------------------------------- helpers

fn conn<Q: Queries + ?Sized>(q: &Q) -> &Connection {
    q.connection(sealed::Token(()))
}

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
        if !self.released {
            // If SQLite already rolled the whole transaction back (after an I/O error, for
            // example) the savepoint is gone and this fails; there is nothing left to undo.
            if let Err(error) = self
                .conn
                .execute_batch("ROLLBACK TO mydrive_op; RELEASE mydrive_op")
            {
                tracing::debug!(%error, "rolling back a store savepoint failed");
            }
        }
    }
}

fn describe(action: &str, key: &ItemKey) -> String {
    format!("{action} item {} in drive {}", key.item_id, key.drive_id)
}

/// Why a name would make a resolved path ambiguous, if it would.
fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("is empty")
    } else if name == "." || name == ".." {
        Some("is a relative path component")
    } else if name.contains('/') {
        Some("contains '/'")
    } else if name.contains('\0') {
        Some("contains NUL")
    } else {
        None
    }
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

fn mount_exists(c: &Connection, mount_id: &MountId) -> Result<bool, StoreError> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM mounts WHERE mount_id = ?1)",
        [mount_id.as_str()],
        |row| row.get(0),
    )
    .ctx(|| format!("look up mount {mount_id}"))
}

/// The size of a file, refusing missing items and folders.
fn file_size(c: &Connection, key: &ItemKey, action: &'static str) -> Result<u64, StoreError> {
    let row: Option<(String, i64)> = c
        .query_row(
            "SELECT kind, size FROM items WHERE drive_id = ?1 AND item_id = ?2",
            [key.drive_id.as_str(), key.item_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ctx(|| describe("look up", key))?;
    let Some((kind, size)) = row else {
        return Err(StoreError::ItemNotFound {
            action,
            key: key.clone(),
        });
    };
    if parse_kind(kind, key)? == ItemKind::Folder {
        return Err(StoreError::NotAFile {
            action,
            key: key.clone(),
        });
    }
    to_u64(size, "items", "size", key)
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

fn parse_kind(value: String, key: &ItemKey) -> Result<ItemKind, StoreError> {
    value.parse().map_err(|reason| StoreError::Corrupt {
        what: describe("items row of", key) + " kind",
        value,
        reason,
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
    etag: String,
    ctag: Option<String>,
    server_hash: Option<String>,
    local_hash: Option<String>,
    hydration: String,
    pin: String,
    sync: String,
    last_error: Option<String>,
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
        })
    }

    fn into_item(self) -> Result<Item, StoreError> {
        let what = format!("items row {} in drive {}", self.item_id, self.drive_id);
        let key = ItemKey::new(
            parse_id(&what, "drive_id", self.drive_id, DriveId::new)?,
            parse_id(&what, "item_id", self.item_id, ItemId::new)?,
        );
        Ok(Item {
            parent_item_id: self
                .parent_item_id
                .map(|p| parse_id(&what, "parent_item_id", p, ItemId::new))
                .transpose()?,
            name: self.name,
            kind: parse_kind(self.kind, &key)?,
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

    /// An operation that fails part way leaves none of its statements applied, on a bare
    /// connection and inside a transaction that then commits.
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
}
