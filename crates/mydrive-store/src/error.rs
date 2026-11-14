//! The store's error type.

use std::path::PathBuf;

use mydrive_core::ids::{AccountId, ItemId, ItemKey, MountId};

use crate::types::ByteRange;

/// Why a store operation failed. Every variant says what was being done and to what.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("state store: {action}: {source}")]
    Sqlite {
        /// What the store was doing, including the item or mount concerned.
        action: String,
        /// The SQLite error.
        #[source]
        source: rusqlite::Error,
    },

    /// The database was written by a newer my-drive. Nothing was changed.
    #[error(
        "cannot open state store {path}: its schema version is {found}, this build understands \
         up to {supported}; the store was not modified"
    )]
    SchemaTooNew {
        /// The database file.
        path: PathBuf,
        /// The schema version found in the file.
        found: i64,
        /// The newest schema version this build can use.
        supported: i64,
    },

    /// The file has tables but no schema version, so it is not a my-drive state store.
    /// Nothing was changed.
    #[error(
        "cannot open state store {path}: it holds tables but no my-drive schema version, so it \
         is not a my-drive state store; the file was not modified"
    )]
    NotAStateStore {
        /// The database file.
        path: PathBuf,
    },

    /// SQLite would not switch the database to WAL mode (ADR-0006 R2).
    #[error("cannot open state store {path}: journal mode is {mode:?}, expected \"wal\"")]
    NotWal {
        /// The database file.
        path: PathBuf,
        /// The journal mode SQLite reported.
        mode: String,
    },

    /// A path given to the store is not valid UTF-8.
    #[error("state store: {what} {path:?} is not valid UTF-8")]
    NonUtf8Path {
        /// Which path, for example `"mount point of mount m1"`.
        what: String,
        /// The path.
        path: PathBuf,
    },

    /// A column held a value the store cannot interpret. The database is damaged or was
    /// edited by hand.
    #[error("state store: {what} holds {value:?}, which is not valid: {reason}")]
    Corrupt {
        /// The row and column concerned.
        what: String,
        /// The stored value.
        value: String,
        /// Why it is not valid.
        reason: String,
    },

    /// No item has this key.
    #[error("state store: {action}: item {item_id} in drive {drive_id} is not in the store",
        drive_id = .key.drive_id, item_id = .key.item_id)]
    ItemNotFound {
        /// What was being done.
        action: &'static str,
        /// The item looked for.
        key: ItemKey,
    },

    /// No mount has this ID.
    #[error("state store: {action}: mount {mount_id} is not in the store")]
    MountNotFound {
        /// What was being done.
        action: &'static str,
        /// The mount looked for.
        mount_id: MountId,
    },

    /// No account has this ID.
    #[error("state store: {action}: account {account_id} is not in the store")]
    AccountNotFound {
        /// What was being done.
        action: &'static str,
        /// The account looked for.
        account_id: AccountId,
    },

    /// A mount with this ID already exists.
    #[error("state store: cannot add mount {mount_id}: a mount with that ID already exists")]
    MountExists {
        /// The mount ID that is taken.
        mount_id: MountId,
    },

    /// Another mount already has this root (ADR-0007 R4).
    #[error(
        "state store: cannot add mount {mount_id}: its root item {item_id} in drive {drive_id} \
         is already mounted as {existing}",
        drive_id = .root.drive_id, item_id = .root.item_id
    )]
    MountRootTaken {
        /// The mount being added.
        mount_id: MountId,
        /// The root it asked for.
        root: ItemKey,
        /// The mount that already has that root.
        existing: MountId,
    },

    /// An item's name would make its path ambiguous.
    #[error("state store: cannot store item {item_id} in drive {drive_id}: name {name:?} {reason}",
        drive_id = .key.drive_id, item_id = .key.item_id)]
    InvalidName {
        /// The item.
        key: ItemKey,
        /// The rejected name.
        name: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// An item's size does not fit the store's 63-bit integer column.
    #[error("state store: cannot store item {item_id} in drive {drive_id}: size {size} is larger \
             than {max}", drive_id = .key.drive_id, item_id = .key.item_id, max = i64::MAX)]
    SizeTooLarge {
        /// The item.
        key: ItemKey,
        /// The rejected size.
        size: u64,
    },

    /// An upsert would leave hydrated ranges that the item can no longer hold, because the
    /// new size is smaller than the ranges or the item became a folder. Clear the ranges
    /// first, in the same transaction.
    #[error(
        "state store: cannot update item {item_id} in drive {drive_id} to a {kind} of size \
         {size}: its hydrated ranges reach byte {covered_end}; clear them first",
        drive_id = .key.drive_id, item_id = .key.item_id
    )]
    RangesWouldExceedItem {
        /// The item.
        key: ItemKey,
        /// The kind the upsert asked for.
        kind: crate::types::ItemKind,
        /// The size the upsert asked for.
        size: u64,
        /// One past the last hydrated byte.
        covered_end: u64,
    },

    /// `end` is before `start`.
    #[error("invalid byte range: end {end} is before start {start}")]
    InvalidRange {
        /// The requested start.
        start: u64,
        /// The requested end.
        end: u64,
    },

    /// A byte range reaches past the end of the item.
    #[error("state store: {action}: range {range} is outside item {item_id} in drive \
             {drive_id}, which is {size} bytes", drive_id = .key.drive_id, item_id = .key.item_id)]
    RangeOutOfBounds {
        /// What was being done.
        action: &'static str,
        /// The item.
        key: ItemKey,
        /// The requested range.
        range: ByteRange,
        /// The item's size.
        size: u64,
    },

    /// Hydrated ranges were asked of a folder.
    #[error("state store: {action}: item {item_id} in drive {drive_id} is a folder, which has \
             no content ranges", drive_id = .key.drive_id, item_id = .key.item_id)]
    NotAFile {
        /// What was being done.
        action: &'static str,
        /// The item.
        key: ItemKey,
    },

    /// A non-recursive delete was asked of a folder that has children (WP-006 AC5).
    #[error(
        "state store: refusing to delete item {item_id} in drive {drive_id}: it has {children} \
         children; ask for a recursive delete explicitly",
        drive_id = .key.drive_id, item_id = .key.item_id
    )]
    HasChildren {
        /// The item.
        key: ItemKey,
        /// How many direct children it has.
        children: u64,
    },

    /// The item belongs to a different drive than the mount.
    #[error("state store: cannot resolve the path of item {item_id} in drive {drive_id} within \
             mount {mount_id}: the item is not in that mount's drive",
             drive_id = .key.drive_id, item_id = .key.item_id)]
    WrongDrive {
        /// The item.
        key: ItemKey,
        /// The mount.
        mount_id: MountId,
    },

    /// Walking the parent chain reached the drive root without passing the mount's root, so
    /// the item is outside the mount.
    #[error("state store: cannot resolve the path of item {item_id} in drive {drive_id} within \
             mount {mount_id}: the item is not below the mount's root",
             drive_id = .key.drive_id, item_id = .key.item_id)]
    NotInMount {
        /// The item.
        key: ItemKey,
        /// The mount.
        mount_id: MountId,
    },

    /// Walking the parent chain reached a parent that is not in the store.
    #[error("state store: cannot resolve the path of item {item_id} in drive {drive_id}: its \
             ancestor {missing} is not in the store",
             drive_id = .key.drive_id, item_id = .key.item_id)]
    MissingAncestor {
        /// The item.
        key: ItemKey,
        /// The parent that is missing.
        missing: ItemId,
    },

    /// The parent chain loops (WP-006 AC8).
    #[error("state store: cannot resolve the path of item {item_id} in drive {drive_id}: its \
             parent chain loops back to {repeated}",
             drive_id = .key.drive_id, item_id = .key.item_id)]
    ParentCycle {
        /// The item.
        key: ItemKey,
        /// The first item seen twice.
        repeated: ItemId,
    },
}

/// Adds what was being done to a SQLite error. The description is built only on failure.
pub(crate) trait SqlContext<T> {
    fn ctx<S: Into<String>>(self, action: impl FnOnce() -> S) -> Result<T, StoreError>;
}

impl<T> SqlContext<T> for Result<T, rusqlite::Error> {
    fn ctx<S: Into<String>>(self, action: impl FnOnce() -> S) -> Result<T, StoreError> {
        self.map_err(|source| StoreError::Sqlite {
            action: action().into(),
            source,
        })
    }
}
