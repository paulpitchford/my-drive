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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The server's eTag for the item.
    pub etag: String,
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
