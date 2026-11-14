//! Row types the store reads and writes.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use mydrive_core::state::ItemState;

use crate::error::StoreError;

/// One signed-in Microsoft identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The account's identity.
    pub account_id: AccountId,
    /// The organisation a work account belongs to; `None` for a personal account.
    pub tenant_id: Option<TenantId>,
}

/// A drive, or a folder within one, attached to a local mount point (ADR-0007).
///
/// `Debug` redacts the delta token, which must never reach a log.
#[derive(Clone, PartialEq, Eq)]
pub struct Mount {
    /// The mount's identity.
    pub mount_id: MountId,
    /// The account whose sign-in the mount uses.
    pub account_id: AccountId,
    /// The drive the mount shows.
    pub drive_id: DriveId,
    /// The item at the top of the mount: the drive root, or a folder within the drive.
    pub root_item_id: ItemId,
    /// Where the user sees the mount. Must be valid UTF-8.
    pub mount_point: PathBuf,
    /// The mount's delta token, if one has been saved (ADR-0007 R2).
    pub delta_token: Option<String>,
    /// Whether the mount refuses local writes.
    pub read_only: bool,
    /// Why organisation policy blocks the mount; `None` when it is not blocked.
    pub blocked_reason: Option<String>,
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mount")
            .field("mount_id", &self.mount_id)
            .field("account_id", &self.account_id)
            .field("drive_id", &self.drive_id)
            .field("root_item_id", &self.root_item_id)
            .field("mount_point", &self.mount_point)
            .field(
                "delta_token",
                &self.delta_token.as_ref().map(|_| "<redacted>"),
            )
            .field("read_only", &self.read_only)
            .field("blocked_reason", &self.blocked_reason)
            .finish()
    }
}

/// Whether an item is a file or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// A file: has content and hydrated ranges.
    File,
    /// A folder: has children and never has hydrated ranges.
    Folder,
}

impl ItemKind {
    /// The form stored in the `kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ItemKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file" => Ok(Self::File),
            "folder" => Ok(Self::Folder),
            other => Err(format!(
                "cannot parse {other:?} as an item kind: expected \"file\" or \"folder\""
            )),
        }
    }
}

/// A point in time in whole seconds since the Unix epoch, UTC.
///
/// Graph timestamps are truncated to whole seconds before they reach the store
/// (ADR-0011 R5), so nothing finer is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Builds a timestamp from seconds since the Unix epoch.
    #[must_use]
    pub const fn from_unix_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub const fn as_unix_seconds(self) -> i64 {
        self.0
    }
}

/// Everything the store holds about one item, keyed by (`drive_id`, `item_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The item's identity. Never a path.
    pub key: ItemKey,
    /// The parent folder in the same drive; `None` for the drive root.
    pub parent_item_id: Option<ItemId>,
    /// The item's name within its parent. Not empty, not `.` or `..`, and free of `/` and
    /// NUL, so a resolved path is unambiguous.
    pub name: String,
    /// File or folder.
    pub kind: ItemKind,
    /// Size in bytes as the server reports it. At most `i64::MAX`.
    pub size: u64,
    /// When the item was created, as the server reports it.
    pub created: Timestamp,
    /// When the item was last modified, as the server reports it.
    pub modified: Timestamp,
    /// The server's eTag for the item; `None` until the server has one (a provisional
    /// item).
    pub etag: Option<String>,
    /// The server's cTag; business delta omits it.
    pub ctag: Option<String>,
    /// The quickXorHash the server reports (ADR-0012 "Two hashes").
    pub server_hash: Option<QuickXorHash>,
    /// The quickXorHash of the bytes we last uploaded (ADR-0012 "Two hashes").
    pub local_hash: Option<QuickXorHash>,
    /// The three state axes, stored in three columns (ADR-0008 R1).
    pub state: ItemState,
    /// The most recent error for the item, for `mydrive activity` and `mydrive status`.
    pub last_error: Option<String>,
    /// The item was created locally and the server has not assigned its ID yet: `item_id`
    /// is a provisional value the engine generated. Cleared by
    /// [`rekey_item`](crate::Queries::rekey_item) and
    /// [`complete_upload`](crate::Queries::complete_upload).
    pub is_provisional: bool,
    /// The backing file holds content that has not reached the server. Every guard against
    /// losing local content keys on this, not on the sync state. Set by
    /// [`mark_local_change`](crate::Queries::mark_local_change); cleared only by
    /// [`complete_upload`](crate::Queries::complete_upload) or
    /// [`discard_local_changes`](crate::Queries::discard_local_changes).
    pub local_dirty: bool,
    /// Bumped on every local content change, so an upload of an older version cannot mark
    /// a newer one as synced.
    pub local_generation: u64,
    /// The server reported different content while the item was dirty, and the server
    /// columns were deliberately not updated: they still describe the version the local
    /// change was based on. Survives a restart, so conflict handling is not forgotten.
    pub remote_changed: bool,
}

/// The server-owned part of an item, as a delta or an upload response reports it.
///
/// [`apply_remote_metadata`](crate::Queries::apply_remote_metadata) writes only these
/// fields; local hash, hydration, pin, sync and last error are never touched by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMetadata {
    /// The item's identity.
    pub key: ItemKey,
    /// The parent folder in the same drive; `None` for the drive root.
    pub parent_item_id: Option<ItemId>,
    /// The item's name within its parent.
    pub name: String,
    /// File or folder.
    pub kind: ItemKind,
    /// Size in bytes.
    pub size: u64,
    /// When the item was created.
    pub created: Timestamp,
    /// When the item was last modified.
    pub modified: Timestamp,
    /// The server's eTag.
    pub etag: Option<String>,
    /// The server's cTag, if the server sent one.
    pub ctag: Option<String>,
    /// The quickXorHash the server reports, if any.
    pub server_hash: Option<QuickXorHash>,
}

/// What [`apply_remote_metadata`](crate::Queries::apply_remote_metadata) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteApplied {
    /// The content identity did not change. On a clean item every server-owned column was
    /// written; on a dirty item only parent and name were.
    ContentUnchanged,
    /// The content changed and the item was clean: every server-owned column was written,
    /// the hydrated ranges were cleared and hydration set to `placeholder` (`full` for a
    /// folder).
    ContentReplaced {
        /// How many hydrated ranges were removed.
        ranges_cleared: u64,
    },
    /// The content changed while the item is dirty. Only parent and name were written;
    /// nothing describing content (kind, size, timestamps, eTag, cTag, server hash),
    /// ranges or local state changed, and the durable `remote_changed` flag was set. The
    /// stale eTag still makes the next upload fail with 412. The engine runs conflict
    /// handling.
    ContentChangedUnderLocalState {
        /// The item's sync state, unchanged.
        sync: mydrive_core::state::Sync,
    },
}

/// What [`complete_upload`](crate::Queries::complete_upload) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadCompleted {
    /// The uploaded version is still the local version: the item is clean and `in-sync`.
    Synced,
    /// The item changed locally after the upload started. The server columns and hashes
    /// now describe the uploaded version (so the next upload's `If-Match` is right), but
    /// the item stays dirty and `pending-upload`.
    LocalChangedSince {
        /// The item's current local generation.
        generation: u64,
    },
}

/// Whether [`delete_item`](crate::Queries::delete_item) may remove items with local
/// changes that have not reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingChanges {
    /// Refuse if the item, or any descendant, is dirty (`local_dirty`).
    Refuse,
    /// The server has confirmed a delete the user made locally, so the pending state is
    /// that delete and removing the rows loses nothing.
    ServerConfirmedLocalDelete,
}

/// A half-open byte range `[start, end)` within a file.
///
/// `start == end` is a valid, empty range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    /// Builds the range `[start, end)`.
    ///
    /// # Errors
    ///
    /// [`StoreError::InvalidRange`] if `end < start`.
    pub fn new(start: u64, end: u64) -> Result<Self, StoreError> {
        if end < start {
            return Err(StoreError::InvalidRange { start, end });
        }
        Ok(Self { start, end })
    }

    /// The first byte in the range.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// One past the last byte in the range.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }

    /// The number of bytes in the range.
    #[must_use]
    pub const fn len(self) -> u64 {
        self.end - self.start
    }

    /// Whether the range holds no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {})", self.start, self.end)
    }
}

/// Whether [`delete_item`](crate::Queries::delete_item) may remove a folder's descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// Delete the item only; refused if it has children.
    ItemOnly,
    /// Delete the item and every descendant, with their hydrated ranges.
    Recursive,
}

/// An item's path relative to its mount point, derived from the parent chain.
///
/// It is computed on demand and is never stored or used as a key (ADR-0006 R4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPath {
    components: Vec<String>,
}

impl MountPath {
    pub(crate) fn new(components: Vec<String>) -> Self {
        Self { components }
    }

    /// The names from the mount root down to the item. Empty for the mount root itself.
    #[must_use]
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// Whether this is the mount root.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// The path as a relative [`PathBuf`]; empty for the mount root.
    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        self.components.iter().collect()
    }
}

impl fmt::Display for MountPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.components.join("/"))
    }
}
