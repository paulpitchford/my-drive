//! Row types the store reads and writes.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey, MountId, TenantId};
use mydrive_core::names::AccountKind;
use mydrive_core::state::ItemState;

use crate::error::StoreError;

/// Defines a fieldless enum stored as text, with its stored forms, `as_str`, `Display` and
/// `FromStr`, so every string conversion of the enum shares one table. A unit test keeps
/// each SQL `CHECK` list equal to `WIRE_FORMS`.
macro_rules! stored_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident, kind = $kind:literal {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];

            /// Every stored form, in declaration order.
            pub const WIRE_FORMS: &'static [&'static str] = &[ $( $wire, )+ ];

            /// The form stored in the store's column: lowercase kebab-case.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $wire, )+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $wire => Ok(Self::$variant), )+
                    other => Err(format!(
                        "cannot parse {other:?} as a {}: expected one of {:?}",
                        $kind,
                        Self::WIRE_FORMS
                    )),
                }
            }
        }
    };
}

/// One signed-in Microsoft identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The account's identity.
    pub account_id: AccountId,
    /// The organisation a work account belongs to; `None` for a personal account.
    pub tenant_id: Option<TenantId>,
    /// Personal or work. Stored as `personal` or `work`.
    pub kind: AccountKind,
    /// The sign-in name, if known. Not empty.
    pub username: Option<String>,
    /// The name to show for the account, if known. Not empty.
    pub display_name: Option<String>,
    /// The organisation's name, for a work account, if known. Not empty.
    pub org_name: Option<String>,
    /// The refresh token was rejected: every mount of the account stops syncing until the
    /// user signs in again (overview, "Account and mount states").
    pub needs_sign_in: bool,
    /// The tenant refuses `#` and `%` in names, so name validation must refuse them too
    /// (`mydrive_core::names::NameContext::hash_and_percent_allowed`).
    pub refuses_hash_and_percent: bool,
}

impl Account {
    /// The account's state as the protocol reports it.
    #[must_use]
    pub const fn state(&self) -> AccountState {
        if self.needs_sign_in {
            AccountState::NeedsSignIn
        } else {
            AccountState::SignedIn
        }
    }
}

stored_enum! {
    /// An account's state (overview, "Account and mount states"). Not stored: it is
    /// [`Account::needs_sign_in`].
    pub enum AccountState, kind = "account state" {
        /// The account's tokens work.
        SignedIn => "signed-in",
        /// The refresh token was rejected.
        NeedsSignIn => "needs-sign-in",
    }
}

/// The stored forms of [`AccountKind`], in the order of the `accounts.kind` CHECK list.
#[cfg(test)]
pub(crate) const ACCOUNT_KIND_FORMS: &[&str] = &["personal", "work"];

/// The stored form of an [`AccountKind`].
pub(crate) const fn account_kind_str(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Personal => "personal",
        AccountKind::Work => "work",
    }
}

/// Parses the stored form of an [`AccountKind`].
pub(crate) fn parse_account_kind(s: &str) -> Result<AccountKind, String> {
    match s {
        "personal" => Ok(AccountKind::Personal),
        "work" => Ok(AccountKind::Work),
        other => Err(format!(
            "cannot parse {other:?} as an account kind: expected \"personal\" or \"work\""
        )),
    }
}

stored_enum! {
    /// The kind of drive a mount shows, as Graph's `driveType` reports it.
    pub enum DriveType, kind = "drive type" {
        /// A personal OneDrive (`personal`).
        Personal => "personal",
        /// A OneDrive for Business (`business`).
        Business => "business",
        /// A SharePoint document library (`documentLibrary`).
        DocumentLibrary => "document-library",
    }
}

stored_enum! {
    /// Why a mount is blocked. Mirrors the protocol's open set `mount.blocked_reason`.
    pub enum BlockedReason, kind = "blocked reason" {
        /// Organisation policy refuses access, for example from an unmanaged device.
        OrganisationPolicy => "organisation-policy",
        /// The tenant refuses the account altogether.
        TenantRefused => "tenant-refused",
    }
}

stored_enum! {
    /// Why a mount is read-only. Mirrors the protocol's `read-only.data.reason`.
    pub enum ReadOnlyReason, kind = "read-only reason" {
        /// The library itself is read-only.
        LibraryReadOnly => "library-read-only",
        /// The account may read the library but not write to it.
        NoWritePermission => "no-write-permission",
    }
}

stored_enum! {
    /// Which kind of delta run is in progress on a mount (ADR-0022).
    pub enum DeltaRunKind, kind = "delta run" {
        /// The first enumeration of a mount, from no token.
        Initial => "initial",
        /// A run from the live token.
        Incremental => "incremental",
        /// A `410 Gone` resync with `resyncChangesApplyDifferences` (ADR-0011 R3).
        ResyncApply => "resync-apply",
        /// A `410 Gone` resync with `resyncChangesUploadDifferences` (ADR-0011 R3).
        ResyncUpload => "resync-upload",
    }
}

impl DeltaRunKind {
    /// Whether the run is a `410 Gone` resync. The kind is stored so that a restart repeats
    /// a resync in full and the mount reports `resyncing` meanwhile.
    #[must_use]
    pub const fn is_resync(self) -> bool {
        matches!(self, Self::ResyncApply | Self::ResyncUpload)
    }
}

stored_enum! {
    /// A mount's state (overview, "Account and mount states"). Never stored: it is derived
    /// from the mount's facts by [`derive_mount_state`].
    pub enum MountState, kind = "mount state" {
        /// Syncing normally.
        Active => "active",
        /// Paused by the user.
        Paused => "paused",
        /// Organisation policy blocks the mount; it carries a reason.
        Blocked => "blocked",
        /// The first enumeration, or a `410 Gone` resync, has not been applied yet.
        Resyncing => "resyncing",
    }
}

/// The facts a mount's state is derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MountStateFacts {
    /// A blocked reason is recorded.
    pub blocked: bool,
    /// The user paused the mount.
    pub paused: bool,
    /// The mount has a live delta token.
    pub has_live_token: bool,
    /// The delta run in progress, if any.
    pub run: Option<DeltaRunKind>,
}

/// Derives a mount's state from its facts, by this precedence: `blocked` if a blocked
/// reason is recorded; else `paused` if the user paused it; else `resyncing` if it has no
/// live token (its first enumeration has not been applied) or the run in progress is a
/// resync; else `active`.
///
/// `paused_until` does not enter: it is when the engine resumes the mount, and the mount is
/// paused until the engine clears the pause.
#[must_use]
pub const fn derive_mount_state(facts: MountStateFacts) -> MountState {
    if facts.blocked {
        return MountState::Blocked;
    }
    if facts.paused {
        return MountState::Paused;
    }
    let resync = match facts.run {
        Some(run) => run.is_resync(),
        None => false,
    };
    if !facts.has_live_token || resync {
        return MountState::Resyncing;
    }
    MountState::Active
}

/// A drive, or a folder within one, attached to a local mount point (ADR-0007).
///
/// The fields mirror the `mounts` columns. [`insert_mount`](crate::Queries::insert_mount)
/// refuses a combination the table's constraints forbid with
/// [`StoreError::InvalidMount`]; the other mount operations take typed arguments.
///
/// `Debug` redacts the delta token and both run links, which carry tokens and must never
/// reach a log.
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
    /// The kind of drive. Required for a new mount; `None` only for a mount of a work
    /// account added by schema version 1, which never recorded it.
    pub drive_type: Option<DriveType>,
    /// The drive's or library's name, if known. Not empty.
    pub display_name: Option<String>,
    /// Whether the mount refuses local writes.
    pub read_only: bool,
    /// Why the mount is read-only, when known. Only with `read_only`.
    pub read_only_reason: Option<ReadOnlyReason>,
    /// The user paused syncing.
    pub paused: bool,
    /// When the engine resumes a paused mount; `None` to stay paused. Only with `paused`.
    pub paused_until: Option<Timestamp>,
    /// Why organisation policy blocks the mount; `None` when it is not blocked.
    pub blocked_reason: Option<BlockedReason>,
    /// Text for people about the block. Only with `blocked_reason`. Not empty.
    pub blocked_message: Option<String>,
    /// The live delta token, the only one a new run may start from (ADR-0007 R2). Changed
    /// only by [`finish_delta_run`](crate::Queries::finish_delta_run) (ADR-0022 R4).
    pub delta_token: Option<String>,
    /// The delta run in progress, if any.
    pub delta_run: Option<DeltaRunKind>,
    /// Where the run in progress continues: the last staged page's next link.
    pub delta_next_link: Option<String>,
    /// The delta link of a run whose staged set is complete and awaits apply.
    pub delta_staged_link: Option<String>,
}

impl Mount {
    /// The facts [`derive_mount_state`] reads.
    #[must_use]
    pub const fn state_facts(&self) -> MountStateFacts {
        MountStateFacts {
            blocked: self.blocked_reason.is_some(),
            paused: self.paused,
            has_live_token: self.delta_token.is_some(),
            run: self.delta_run,
        }
    }

    /// The mount's derived state.
    #[must_use]
    pub const fn state(&self) -> MountState {
        derive_mount_state(self.state_facts())
    }
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redact = |v: &Option<String>| v.as_ref().map(|_| "<redacted>");
        f.debug_struct("Mount")
            .field("mount_id", &self.mount_id)
            .field("account_id", &self.account_id)
            .field("drive_id", &self.drive_id)
            .field("root_item_id", &self.root_item_id)
            .field("mount_point", &self.mount_point)
            .field("drive_type", &self.drive_type)
            .field("display_name", &self.display_name)
            .field("read_only", &self.read_only)
            .field("read_only_reason", &self.read_only_reason)
            .field("paused", &self.paused)
            .field("paused_until", &self.paused_until)
            .field("blocked_reason", &self.blocked_reason)
            .field("blocked_message", &self.blocked_message)
            .field("delta_token", &redact(&self.delta_token))
            .field("delta_run", &self.delta_run)
            .field("delta_next_link", &redact(&self.delta_next_link))
            .field("delta_staged_link", &redact(&self.delta_staged_link))
            .finish()
    }
}

/// Which mount constraint an argument would break. Carried by
/// [`StoreError::InvalidMount`]; part of the error contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MountRule {
    /// A next link or staged link without a delta run.
    LinkWithoutRun,
    /// A next link and a staged link at once.
    BothLinks,
    /// A blocked message without a blocked reason.
    BlockedMessageWithoutReason,
    /// A paused-until time on a mount that is not paused.
    PausedUntilWithoutPause,
    /// A read-only reason on a mount that is not read-only.
    ReadOnlyReasonWithoutReadOnly,
    /// A new mount must record its drive type.
    DriveTypeRequired,
    /// A new mount has no live token and no run links: the token is written only by
    /// finishing a run (ADR-0022 R4), the links only by staging.
    NewMountHasDeltaState,
    /// A new mount's run, if any, is its first enumeration (`initial`).
    NewMountRunNotInitial,
    /// An incremental run starts from the live token, so it needs one.
    IncrementalWithoutToken,
    /// An initial run is the first enumeration, so the mount has no live token.
    InitialWithToken,
    /// A text field (display name, blocked message, delta link) is empty.
    EmptyText,
}

impl fmt::Display for MountRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LinkWithoutRun => "a next or staged delta link needs a delta run in progress",
            Self::BothLinks => "a mount cannot hold a next link and a staged delta link at once",
            Self::BlockedMessageWithoutReason => "a blocked message needs a blocked reason",
            Self::PausedUntilWithoutPause => "a paused-until time needs the mount to be paused",
            Self::ReadOnlyReasonWithoutReadOnly => "a read-only reason needs a read-only mount",
            Self::DriveTypeRequired => "a new mount must record its drive type",
            Self::NewMountHasDeltaState => {
                "a new mount has no delta token or run links; only staging and finishing a run \
                 write them"
            }
            Self::NewMountRunNotInitial => "a new mount's delta run can only be the initial one",
            Self::IncrementalWithoutToken => "an incremental run needs a live delta token",
            Self::InitialWithToken => "an initial run is only for a mount with no live token",
            Self::EmptyText => "a text field is empty",
        })
    }
}

/// Whether a mount is paused, for [`set_mount_paused`](crate::Queries::set_mount_paused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPause {
    /// Not paused.
    Running,
    /// Paused until the user resumes it.
    Paused,
    /// Paused; the engine resumes it at this time.
    PausedUntil(Timestamp),
}

/// Why a mount is blocked, for [`set_mount_blocked`](crate::Queries::set_mount_blocked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountBlock {
    /// The reason code.
    pub reason: BlockedReason,
    /// Text for people, if any. Not empty.
    pub message: Option<String>,
}

/// Whether a mount accepts local writes, for
/// [`set_mount_read_only`](crate::Queries::set_mount_read_only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountAccess {
    /// Local writes are accepted.
    ReadWrite,
    /// Local writes are refused, for this reason if known.
    ReadOnly(Option<ReadOnlyReason>),
}

/// What [`remove_mount`](crate::Queries::remove_mount) may do about items whose local
/// changes have not reached the server. There is no default: the caller states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOnlyContent {
    /// Refuse while any item of the mount is dirty, naming them.
    Refuse,
    /// The caller has written the dirty items' bytes elsewhere (into the mount-point
    /// directory, ADR-0015 R8) and the user confirmed that they stay only on this
    /// computer. Accepted only if exactly `dirty_items` items are dirty now, the count the
    /// user confirmed, so a change made after the confirmation is refused.
    KeptElsewhere {
        /// How many dirty items the user confirmed.
        dirty_items: u64,
    },
}

/// What [`remove_mount`](crate::Queries::remove_mount) removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountRemoved {
    /// Item rows removed.
    pub items: u64,
    /// Hydrated-range rows removed.
    pub ranges: u64,
    /// Staged delta entries removed.
    pub staged: u64,
}

/// The most dirty items a refused [`remove_mount`](crate::Queries::remove_mount) names. The
/// error carries the full count beside them.
pub const DIRTY_SAMPLE_LIMIT: usize = 20;

/// A stable local handle for an item row (ADR-0021): assigned when the row is created,
/// unique in the store, never changed for the life of the row, and never reused after the
/// row is removed. It names the item's content file, `mounts/<mount_id>/content/<local_id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(u64);

impl LocalId {
    /// Wraps a stored value, which is at least 1.
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The numeric value, at least 1.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One page of a folder's children, from
/// [`children_after`](crate::Queries::children_after).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildrenPage {
    /// The children, in byte order of name, then of item ID.
    pub items: Vec<Item>,
    /// More children follow the last one returned.
    pub more: bool,
}

/// Whether an item is a file, a folder or a package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// A file: has content and hydrated ranges.
    File,
    /// A folder: has children and never has hydrated ranges.
    Folder,
    /// A package (Graph's `package` facet, for example a OneNote notebook). The store
    /// treats it as having no content of its own: it never has hydrated ranges.
    Package,
}

impl ItemKind {
    /// The form stored in the `kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
            Self::Package => "package",
        }
    }

    /// Whether items of this kind have content: bytes, hydrated ranges, a content file.
    /// Only files do.
    #[must_use]
    pub const fn has_content(self) -> bool {
        matches!(self, Self::File)
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
            "package" => Ok(Self::Package),
            other => Err(format!(
                "cannot parse {other:?} as an item kind: expected \"file\", \"folder\" or \
                 \"package\""
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
    /// While the item is dirty: the last content identity the server reported for it.
    /// When the item first becomes dirty this is the identity of the version the local
    /// change is based on; each remote report on the dirty item then replaces it.
    /// `None` for a clean item.
    pub last_remote: Option<RemoteIdentity>,
}

/// What identifies one version of a file's content on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteIdentity {
    /// The eTag of that version.
    pub etag: Option<String>,
    /// The cTag of that version, if the server sent one.
    pub ctag: Option<String>,
    /// The quickXorHash of that version, if the server sent one.
    pub hash: Option<QuickXorHash>,
    /// The size of that version in bytes.
    pub size: u64,
}

impl RemoteIdentity {
    /// Whether `self` and `other` name the same content: the hashes and sizes are equal
    /// when either side has a hash; otherwise the cTags and sizes when both have a cTag
    /// (a rename changes the eTag but not the cTag); otherwise the eTags and sizes.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        if self.size != other.size {
            return false;
        }
        if self.hash.is_some() || other.hash.is_some() {
            return self.hash == other.hash;
        }
        match (&self.ctag, &other.ctag) {
            (Some(a), Some(b)) => a == b,
            _ => self.etag == other.etag,
        }
    }
}

impl RemoteMetadata {
    /// The content identity this report describes.
    #[must_use]
    pub fn identity(&self) -> RemoteIdentity {
        RemoteIdentity {
            etag: self.etag.clone(),
            ctag: self.ctag.clone(),
            hash: self.server_hash,
            size: self.size,
        }
    }
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
    /// The uploaded version is still the local version and the server has nothing newer:
    /// the item is clean and `in-sync`.
    Synced,
    /// The uploaded version is still the local version, so the item is clean, but the
    /// server reported a different version after our upload (another client changed it).
    /// The item is `pending-download` with `remote_changed` kept: the engine fetches the
    /// current metadata and applies it.
    UploadedButRemoteMovedOn,
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
    /// The server has confirmed a delete the user made locally of the named item. Only
    /// that item is exempt, and only if its `local_generation` still equals
    /// `expected_generation`; any other dirty item in a recursive delete is still refused.
    ServerConfirmedLocalDelete {
        /// The named item's generation when the user deleted it.
        expected_generation: u64,
    },
    /// The named item was created locally and never uploaded (it is provisional), and the
    /// user deleted it. Only that item is exempt, with the same generation check; any
    /// other dirty item in a recursive delete is still refused.
    DiscardNeverUploaded {
        /// The named item's generation when the user deleted it.
        expected_generation: u64,
    },
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

// ------------------------------------------------------------------ delta staging

stored_enum! {
    /// What a staged delta entry describes, from the item's Graph facets.
    pub enum StagedFacet, kind = "staged facet" {
        /// The `file` facet.
        File => "file",
        /// The `folder` facet.
        Folder => "folder",
        /// The `package` facet (for example a OneNote notebook).
        Package => "package",
        /// The `remoteItem` facet: a shortcut to an item in another drive.
        RemoteItem => "remote-item",
    }
}

/// The server's description of an item in a staged delta entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedItem {
    /// The parent folder; `None` only for the drive root.
    pub parent_item_id: Option<ItemId>,
    /// The item's name. Validated as for [`Item::name`].
    pub name: String,
    /// What the item is.
    pub facet: StagedFacet,
    /// Size in bytes.
    pub size: u64,
    /// When the item was created.
    pub created: Timestamp,
    /// When the item was last modified.
    pub modified: Timestamp,
    /// The server's eTag.
    pub etag: Option<String>,
    /// The server's cTag, if sent.
    pub ctag: Option<String>,
    /// The quickXorHash, if sent.
    pub server_hash: Option<QuickXorHash>,
}

/// Why a staged entry cannot be applied as it stands (ADR-0022 R6). The engine resolves it
/// during apply by fetching the item, or records an error on it; it never guesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedProblem {
    /// The Graph client could not use the entry. The text says why.
    Reported(String),
    /// Set by the store when the run is complete: following parents through the staged
    /// set never reaches an item outside it (a parent cycle, or below one).
    ParentCycle,
    /// Set by the store when the run is complete: the parent is neither staged nor stored.
    MissingParent,
    /// Set by the store when the run is complete: the parent is staged as deleted while
    /// this entry is not.
    ParentDeleted,
}

impl StagedProblem {
    /// The form stored in `delta_staged.problem`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Reported(_) => "reported",
            Self::ParentCycle => "parent-cycle",
            Self::MissingParent => "missing-parent",
            Self::ParentDeleted => "parent-deleted",
        }
    }

    /// Every stored form, in the order of the `delta_staged.problem` CHECK list.
    pub const WIRE_FORMS: &'static [&'static str] = &[
        "reported",
        "parent-cycle",
        "missing-parent",
        "parent-deleted",
    ];
}

/// What a staged entry says about its item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedChange {
    /// The item exists with this description.
    Present(StagedItem),
    /// The item was deleted (the `deleted` facet).
    Deleted,
    /// The entry cannot be applied as it stands. `item` holds what was known: `None` for a
    /// problem the Graph client reported without a usable description, the entry's own
    /// description for a problem the store found.
    Problem {
        /// Why.
        problem: StagedProblem,
        /// What was known about the item.
        item: Option<StagedItem>,
    },
}

/// One entry of a delta page, as the engine stages it (ADR-0022).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    /// The item, in the mount's drive.
    pub item_id: ItemId,
    /// The entry carries Graph's `root` facet: it is the drive root, which has no parent.
    pub is_root: bool,
    /// What the entry says.
    pub change: StagedChange,
}

/// A staged entry as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// Order of staging within the run: a later occurrence of the item has a larger value.
    pub seq: u64,
    /// The entry: the last occurrence of the item in the run (ADR-0022 R2).
    pub entry: StagedEntry,
}

/// The link a delta page ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageLink {
    /// More pages follow at this `@odata.nextLink`.
    Next(String),
    /// The last page: this `@odata.deltaLink` becomes the live token when the run is
    /// finished.
    Delta(String),
}

/// Where a walk of the staged set continues, from [`StagedPage::next`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCursor {
    pub(crate) order: i64,
    pub(crate) item_id: ItemId,
}

/// One page of the staged set, in apply order: parents before children, deletes last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPage {
    /// The entries.
    pub entries: Vec<Staged>,
    /// Where the next page starts; `None` when this is the last.
    pub next: Option<StagedCursor>,
}

/// One page of the items the staged set does not contain, for a resync's anti-join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstagedPage {
    /// The items, in byte order of item ID.
    pub items: Vec<Item>,
    /// The item ID to pass as `after` for the next page; `None` when this is the last.
    pub next: Option<ItemId>,
}
