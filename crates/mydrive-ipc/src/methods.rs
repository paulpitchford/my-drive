//! The version 1 method catalogue: names, parameters and results (ADR-0009, ADR-0019).
//!
//! `docs/protocol/README.md` documents every type here with a JSON example; a test parses
//! each example into these types (WP-005 AC1). The JSON is kept plain for adapters written in
//! Python, shell and Lua: snake_case field names, strings for enums, no tagged-enum shapes.
//!
//! Item state uses the `mydrive-core` types directly (WP-005 AC4), so the wire strings of
//! the closed sets are the ones `mydrive-core` defines and the display status is the one its
//! single derivation function computes (ADR-0008 R2, R3). Open sets are in
//! [`crate::values`].

use mydrive_core::ids::{AccountId, DriveId, MountId, TenantId};
use mydrive_core::state::{
    DisplayStatus, EffectivePin, ItemState, Modifiers, Pin, display_status, effective_pin,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{Id, Request};
use crate::error::{ErrorCode, RpcError, cap_message, serialize_capped_opt};
use crate::path::ProtocolPath;
use crate::values::{
    AccountKind, AccountState, BlockedReason, CAPABILITY_IMPORT_EXPORT, DriveType, ItemReason,
    LoginMode, MountState, MountTargetKind, TransferMode, WireEffectivePin, WireHydration,
    WireModifiers, WirePin, WireSync,
};

/// The protocol major version this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Every protocol major version this build supports, oldest first.
pub const SUPPORTED_VERSIONS: &[u32] = &[PROTOCOL_VERSION];

/// Method and notification names.
pub mod method {
    /// Handshake; the first request on every connection.
    pub const HELLO: &str = "hello";
    /// Daemon version and readiness.
    pub const DAEMON_STATUS: &str = "daemon.status";
    /// Signed-in accounts.
    pub const ACCOUNT_LIST: &str = "account.list";
    /// Mounts.
    pub const MOUNT_LIST: &str = "mount.list";
    /// State of a batch of paths, or of one directory and its children.
    pub const ITEM_STATUS: &str = "item.status";
    /// Always keep on this device.
    pub const ITEM_PIN: &str = "item.pin";
    /// Clear the pin (back to `inherit`).
    pub const ITEM_UNPIN: &str = "item.unpin";
    /// Download now without pinning.
    pub const ITEM_HYDRATE: &str = "item.hydrate";
    /// Free up space.
    pub const ITEM_DEHYDRATE: &str = "item.dehydrate";
    /// Turn this connection into a change stream.
    pub const STATUS_SUBSCRIBE: &str = "status.subscribe";
    /// Notification sent on a subscribed connection when item state changes.
    pub const STATUS_CHANGED: &str = "status.changed";
    /// Begin signing an account in.
    pub const ACCOUNT_LOGIN_START: &str = "account.login.start";
    /// Complete a sign-in begun with `account.login.start`.
    pub const ACCOUNT_LOGIN_FINISH: &str = "account.login.finish";
    /// Sign an account out.
    pub const ACCOUNT_LOGOUT: &str = "account.logout";
    /// Show what adding a mount would mount, without changing anything.
    pub const MOUNT_ADD_PREVIEW: &str = "mount.add.preview";
    /// Add the mount a preview showed.
    pub const MOUNT_ADD_CONFIRM: &str = "mount.add.confirm";
    /// Stop syncing a mount. Never deletes remote content.
    pub const MOUNT_REMOVE: &str = "mount.remove";
    /// Pause syncing.
    pub const MOUNT_PAUSE: &str = "mount.pause";
    /// Resume syncing.
    pub const MOUNT_RESUME: &str = "mount.resume";
    /// Copy a local file into a mount (capability `item-import-export`).
    pub const ITEM_IMPORT: &str = "item.import";
    /// Copy a file out of a mount (capability `item-import-export`).
    pub const ITEM_EXPORT: &str = "item.export";

    /// Every request method in protocol version 1.
    pub const ALL: &[&str] = &[
        HELLO,
        DAEMON_STATUS,
        ACCOUNT_LIST,
        MOUNT_LIST,
        ITEM_STATUS,
        ITEM_PIN,
        ITEM_UNPIN,
        ITEM_HYDRATE,
        ITEM_DEHYDRATE,
        STATUS_SUBSCRIBE,
        ACCOUNT_LOGIN_START,
        ACCOUNT_LOGIN_FINISH,
        ACCOUNT_LOGOUT,
        MOUNT_ADD_PREVIEW,
        MOUNT_ADD_CONFIRM,
        MOUNT_REMOVE,
        MOUNT_PAUSE,
        MOUNT_RESUME,
        ITEM_IMPORT,
        ITEM_EXPORT,
    ];

    /// The methods offered only while `hello` advertises
    /// [`crate::values::CAPABILITY_IMPORT_EXPORT`].
    pub const IMPORT_EXPORT: &[&str] = &[ITEM_IMPORT, ITEM_EXPORT];

    /// The methods whose parameters may hold a credential or a sharing link. Their
    /// `invalid-params` messages never repeat a parameter value (ADR-0010 R6).
    pub const REDACTED: &[&str] = &[ACCOUNT_LOGIN_FINISH, MOUNT_ADD_PREVIEW, MOUNT_ADD_CONFIRM];

    /// Every notification method in protocol version 1.
    pub const NOTIFICATIONS: &[&str] = &[STATUS_CHANGED];
}

/// Parameters for methods that take none. Accepts `{}`, an absent `params`, or an object
/// with fields this version does not know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoParams {}

/// `hello` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    /// A short name for the client, for logs (for example `mydrive-cli`).
    pub client_name: String,
    /// Protocol major versions the client supports.
    pub protocol_versions: Vec<u32>,
}

/// `hello` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    /// The chosen protocol major version.
    pub protocol_version: u32,
    /// Optional features the daemon offers. An open set: clients ignore capabilities they do
    /// not know (ADR-0009 R5, ADR-0019). Version 1 defines none.
    pub capabilities: Vec<String>,
    /// Size limits the client must respect (ADR-0020 R5).
    pub limits: Limits,
}

/// The longest line, in either direction, in protocol version 1.
pub const MAX_LINE_BYTES: u64 = crate::codec::MAX_LINE_BYTES as u64;

/// The most paths one action request may carry in protocol version 1 (ADR-0020 R5).
pub const MAX_ACTION_PATHS: u32 = 100;

/// The most bytes the paths of one action request may take, encoded as JSON strings. Half
/// the line, so that one result entry per path always fits in the response line.
pub const MAX_ACTION_PATH_BYTES: u64 = MAX_LINE_BYTES / 2;

/// The most paths one `item.status` batch may carry in protocol version 1.
pub const MAX_STATUS_PATHS: u32 = 10_000;

/// The most bytes the paths of one `item.status` batch may take, encoded as JSON strings.
/// Half the line, so the request plus the longest batch cursor, the longest ID and the
/// separators always fits in one line (ADR-0020 R4).
pub const MAX_STATUS_PATH_BYTES: u64 = MAX_LINE_BYTES / 2;

/// Limits returned by `hello` (ADR-0020 R5). New limits may be added as fields; a limit
/// added in a later version may only constrain something new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// The longest line, in bytes, excluding the `\n`.
    pub max_line_bytes: u64,
    /// The most paths in one `item.pin`, `item.unpin`, `item.hydrate` or `item.dehydrate`.
    pub max_action_paths: u32,
    /// The most bytes those paths may take together, each counted in its minimal JSON
    /// string encoding.
    pub max_action_path_bytes: u64,
    /// The most paths in one `item.status` batch.
    pub max_status_paths: u32,
    /// The most bytes those paths may take together, counted as for actions.
    pub max_status_path_bytes: u64,
}

impl Limits {
    /// The limits of protocol version 1.
    pub const V1: Self = Self {
        max_line_bytes: MAX_LINE_BYTES,
        max_action_paths: MAX_ACTION_PATHS,
        max_action_path_bytes: MAX_ACTION_PATH_BYTES,
        max_status_paths: MAX_STATUS_PATHS,
        max_status_path_bytes: MAX_STATUS_PATH_BYTES,
    };
}

/// Parameters of `account.list` and `mount.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListParams {
    /// The `next_cursor` of the previous page; absent for the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Chooses the highest protocol major version both sides support (ADR-0009 R4).
///
/// # Errors
///
/// `unsupported-version`, listing `daemon_supported`, if there is none in common.
pub fn negotiate(daemon_supported: &[u32], client_supported: &[u32]) -> Result<u32, RpcError> {
    daemon_supported
        .iter()
        .copied()
        .filter(|v| client_supported.contains(v))
        .max()
        .ok_or_else(|| RpcError::unsupported_version(client_supported, daemon_supported))
}

/// `daemon.status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusResult {
    /// The daemon's release version.
    pub daemon_version: String,
    /// Protocol major versions the daemon supports.
    pub protocol_versions: Vec<u32>,
    /// False while the daemon is starting; other requests then get `not-ready`.
    pub ready: bool,
}

/// One account in `account.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// The account's ID.
    pub account_id: AccountId,
    /// Personal or work. Open set.
    pub kind: AccountKind,
    /// The tenant, for a work account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<TenantId>,
    /// Signed in, or needing sign-in. Open set.
    pub state: AccountState,
    /// The name the account signs in with, as Graph reports it (`userPrincipalName`, often an
    /// email address). For display; never a credential. Capped at 1 KiB like every message.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_capped_opt"
    )]
    pub user_name: Option<String>,
    /// The person's name, as Graph reports it. For display; capped at 1 KiB.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_capped_opt"
    )]
    pub display_name: Option<String>,
}

/// `account.list` result: one page, in byte order of `account_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountListResult {
    /// This page's accounts.
    pub accounts: Vec<AccountInfo>,
    /// Present while more accounts remain; see `item.status` for the cursor contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One mount in `mount.list`.
///
/// `paused` is always present and `paused_until` only with `paused` true: a mount with
/// `paused_until` and `paused` false fails to deserialise rather than being read one way or
/// the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MountInfoWire")]
pub struct MountInfo {
    /// The mount's ID.
    pub mount_id: MountId,
    /// The account it belongs to.
    pub account_id: AccountId,
    /// The drive it mounts.
    pub drive_id: DriveId,
    /// Where the user sees it. Never the backing directory (ADR-0009 R8).
    pub mount_point: ProtocolPath,
    /// Its state. Open set.
    pub state: MountState,
    /// Why it is blocked, as a code, when `state` is `blocked` (ADR-0019 R6). Open set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    /// A human-readable explanation of the block, for display only. Capped at 1 KiB on the
    /// wire, like every message.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_capped_opt"
    )]
    pub blocked_message: Option<String>,
    /// True for a read-only library.
    pub read_only: bool,
    /// The drive's type, when known. Open set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive_type: Option<DriveType>,
    /// How many items of the drive the daemon knows: while the mount is `resyncing` for its
    /// first enumeration, the items listed so far. Absent when not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<u64>,
    /// True whenever the mount is paused, whatever `state` shows: a paused mount that is also
    /// blocked shows `blocked` but still has `paused` true.
    pub paused: bool,
    /// When the mount's pause ends, in whole seconds since the Unix epoch (UTC), as in the
    /// `mount.pause` result. Only with `paused` true; absent for a pause until
    /// `mount.resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<u64>,
}

#[derive(Deserialize)]
struct MountInfoWire {
    mount_id: MountId,
    account_id: AccountId,
    drive_id: DriveId,
    mount_point: ProtocolPath,
    state: MountState,
    #[serde(default)]
    blocked_reason: Option<BlockedReason>,
    #[serde(default)]
    blocked_message: Option<String>,
    read_only: bool,
    #[serde(default)]
    drive_type: Option<DriveType>,
    #[serde(default)]
    item_count: Option<u64>,
    paused: bool,
    #[serde(default)]
    paused_until: Option<u64>,
}

impl TryFrom<MountInfoWire> for MountInfo {
    type Error = String;

    fn try_from(wire: MountInfoWire) -> Result<Self, String> {
        if wire.paused_until.is_some() && !wire.paused {
            return Err(format!(
                "mount {} has paused_until but paused is false",
                wire.mount_id
            ));
        }
        Ok(Self {
            mount_id: wire.mount_id,
            account_id: wire.account_id,
            drive_id: wire.drive_id,
            mount_point: wire.mount_point,
            state: wire.state,
            blocked_reason: wire.blocked_reason,
            blocked_message: wire.blocked_message,
            read_only: wire.read_only,
            drive_type: wire.drive_type,
            item_count: wire.item_count,
            paused: wire.paused,
            paused_until: wire.paused_until,
        })
    }
}

/// `mount.list` result: one page, in byte order of `mount_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountListResult {
    /// This page's mounts.
    pub mounts: Vec<MountInfo>,
    /// Present while more mounts remain; see `item.status` for the cursor contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// What `item.status` asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemStatusTarget {
    /// The state of each path, in request order.
    Paths(Vec<ProtocolPath>),
    /// The state of a directory and every child, in name order.
    Directory(ProtocolPath),
}

/// `item.status` parameters.
///
/// On the wire: an object with exactly one of `paths` or `directory`, and optionally
/// `cursor` (ADR-0019 R1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ItemStatusWire", into = "ItemStatusWire")]
pub struct ItemStatusParams {
    /// What to report on.
    pub target: ItemStatusTarget,
    /// The `next_cursor` of the previous page; absent for the first page.
    pub cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ItemStatusWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paths: Option<Vec<ProtocolPath>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<ProtocolPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

impl TryFrom<ItemStatusWire> for ItemStatusParams {
    type Error = String;

    fn try_from(wire: ItemStatusWire) -> Result<Self, String> {
        let target = match (wire.paths, wire.directory) {
            (Some(paths), None) => ItemStatusTarget::Paths(paths),
            (None, Some(directory)) => ItemStatusTarget::Directory(directory),
            _ => return Err("give exactly one of \"paths\" or \"directory\"".into()),
        };
        Ok(Self {
            target,
            cursor: wire.cursor,
        })
    }
}

impl From<ItemStatusParams> for ItemStatusWire {
    fn from(params: ItemStatusParams) -> Self {
        let (paths, directory) = match params.target {
            ItemStatusTarget::Paths(paths) => (Some(paths), None),
            ItemStatusTarget::Directory(directory) => (None, Some(directory)),
        };
        Self {
            paths,
            directory,
            cursor: params.cursor,
        }
    }
}

/// One item's state, as the daemon reports it (WP-005 AC4).
///
/// Clients draw from `status` and `modifiers` and never derive a status from the axes
/// (ADR-0014 R5); the axes are included for clients that want detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemInfo {
    /// The display status, from `mydrive_core::state::display_status`. The only closed set.
    pub status: DisplayStatus,
    /// The `read-only` and `shortcut` indicators, as a list. Open set.
    pub modifiers: WireModifiers,
    /// Hydration axis. Open on the wire (ADR-0020).
    pub hydration: WireHydration,
    /// Pin axis, as set on the item itself. Open on the wire.
    pub pin: WirePin,
    /// The pin after inheritance. Open on the wire.
    pub effective_pin: WireEffectivePin,
    /// Sync axis. Open on the wire.
    pub sync: WireSync,
    /// Why the status is `error` or `blocked`, as a code (ADR-0019 R6). Open set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ItemReason>,
    /// A human-readable explanation to go with `reason`, for display only. Capped at 1 KiB.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_capped_opt"
    )]
    pub message: Option<String>,
}

impl ItemInfo {
    /// Builds the report for an item, deriving the display status with `mydrive-core`'s one
    /// derivation function.
    ///
    /// `inherited` is the item's resolved pin. As in `mydrive-core`, the item's own explicit
    /// pin wins over it, so `effective_pin` and `status` always agree with `pin`.
    #[must_use]
    pub fn new(state: ItemState, inherited: EffectivePin, modifiers: Modifiers) -> Self {
        let inherited_pin = match inherited {
            EffectivePin::Pinned => Pin::Pinned,
            EffectivePin::Unpinned => Pin::Unpinned,
        };
        let effective = effective_pin(state.pin, [inherited_pin]);
        Self {
            status: display_status(&state, effective),
            modifiers: modifiers.into(),
            hydration: state.hydration.into(),
            pin: state.pin.into(),
            effective_pin: effective.into(),
            sync: state.sync.into(),
            reason: None,
            message: None,
        }
    }

    /// Adds the reason code and message for an item in `error` or `blocked`. The message is
    /// capped at 1 KiB.
    #[must_use]
    pub fn with_reason(mut self, reason: ItemReason, message: impl AsRef<str>) -> Self {
        self.reason = Some(reason);
        self.message = Some(cap_message(message.as_ref()));
        self
    }

    /// The three axes as `mydrive-core` values, or `None` if any of them is a value this
    /// build does not know.
    #[must_use]
    pub fn state(&self) -> Option<ItemState> {
        Some(ItemState {
            hydration: self.hydration.to_core()?,
            pin: self.pin.to_core()?,
            sync: self.sync.to_core()?,
        })
    }
}

/// What is known about one path: its state, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryOutcome {
    /// The path names a known item.
    Item(ItemInfo),
    /// There is no state for this path: `not-in-a-mount` outside every mount,
    /// `unknown-path` in a mount with no such item.
    Error(RpcError),
}

/// The state of one path.
///
/// On the wire: `{"path": ..., "item": {...}}` or `{"path": ..., "error": {...}}`. Exactly
/// one of `item` and `error` must be present; anything else fails to deserialise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EntryWire", into = "EntryWire")]
pub struct ItemStatusEntry {
    /// The path asked about, or the child's path for a directory listing.
    pub path: ProtocolPath,
    /// The item's state or the error.
    pub outcome: EntryOutcome,
}

#[derive(Serialize, Deserialize)]
struct EntryWire {
    path: ProtocolPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<ItemInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

impl TryFrom<EntryWire> for ItemStatusEntry {
    type Error = String;

    fn try_from(wire: EntryWire) -> Result<Self, String> {
        let outcome = match (wire.item, wire.error) {
            (Some(item), None) => EntryOutcome::Item(item),
            (None, Some(error)) => EntryOutcome::Error(error),
            _ => {
                return Err(format!(
                    "status entry for {} must have exactly one of \"item\" or \"error\"",
                    wire.path
                ));
            }
        };
        Ok(Self {
            path: wire.path,
            outcome,
        })
    }
}

impl From<ItemStatusEntry> for EntryWire {
    fn from(entry: ItemStatusEntry) -> Self {
        let (item, error) = match entry.outcome {
            EntryOutcome::Item(item) => (Some(item), None),
            EntryOutcome::Error(error) => (None, Some(error)),
        };
        Self {
            path: entry.path,
            item,
            error,
        }
    }
}

impl ItemStatusEntry {
    /// An entry for a known item.
    #[must_use]
    pub fn known(path: ProtocolPath, item: ItemInfo) -> Self {
        Self {
            path,
            outcome: EntryOutcome::Item(item),
        }
    }

    /// An entry for a path with no state.
    #[must_use]
    pub fn failed(path: ProtocolPath, error: RpcError) -> Self {
        Self {
            path,
            outcome: EntryOutcome::Error(error),
        }
    }

    /// The item's state, if known.
    #[must_use]
    pub fn item(&self) -> Option<&ItemInfo> {
        match &self.outcome {
            EntryOutcome::Item(item) => Some(item),
            EntryOutcome::Error(_) => None,
        }
    }

    /// The error, if there is no state.
    #[must_use]
    pub fn error(&self) -> Option<&RpcError> {
        match &self.outcome {
            EntryOutcome::Item(_) => None,
            EntryOutcome::Error(error) => Some(error),
        }
    }
}

/// `item.status` result: one page (ADR-0019 R1, R3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemStatusResult {
    /// For a `directory` request, the directory itself (on every page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<ItemStatusEntry>,
    /// The daemon-wide change sequence number this page was read at. A client holding state
    /// for an entry keeps whichever came with the higher `seq`, from a page or from
    /// `status.changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// This page's entries: for `paths`, in request order; for `directory`, the children in
    /// name order.
    pub entries: Vec<ItemStatusEntry>,
    /// Present when more entries remain: repeat the call with the same parameters plus this
    /// cursor. Absent on the last page. The listing is complete only then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Parameters of `item.pin`, `item.unpin`, `item.hydrate` and `item.dehydrate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPathsParams {
    /// The items to act on.
    pub paths: Vec<ProtocolPath>,
}

/// A descendant of a requested folder that the action did not apply to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedItem {
    /// The descendant's path.
    pub path: ProtocolPath,
    /// Why it was skipped. It is unchanged.
    pub error: RpcError,
}

/// What happened for one requested path of an item action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Not accepted for this path at all; nothing changed. Has no `skipped`.
    Refused(RpcError),
    /// Accepted, possibly with descendants skipped.
    Accepted {
        /// A sample of the skipped descendants: at most `skipped_count` of them, as many as
        /// fit in the line, possibly none (ADR-0020 R6).
        skipped: Vec<SkippedItem>,
        /// The exact number of skipped descendants.
        skipped_count: u64,
        /// For `item.dehydrate`: local bytes released.
        bytes_freed: Option<u64>,
    },
}

/// The outcome for one requested path of an item action.
///
/// On the wire: `path`, and either `error` alone, or any of `skipped`, `skipped_count`
/// (present whenever anything was skipped) and `bytes_freed`. An entry with `error` and
/// `skipped`, or with more `skipped` entries than `skipped_count`, fails to deserialise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ActionEntryWire", into = "ActionEntryWire")]
pub struct ItemActionEntry {
    /// The path acted on.
    pub path: ProtocolPath,
    /// What happened.
    pub outcome: ActionOutcome,
}

impl ItemActionEntry {
    /// An accepted path with nothing skipped.
    #[must_use]
    pub fn accepted(path: ProtocolPath) -> Self {
        Self {
            path,
            outcome: ActionOutcome::Accepted {
                skipped: Vec::new(),
                skipped_count: 0,
                bytes_freed: None,
            },
        }
    }

    /// A refused path.
    #[must_use]
    pub fn refused(path: ProtocolPath, error: RpcError) -> Self {
        Self {
            path,
            outcome: ActionOutcome::Refused(error),
        }
    }

    /// The error, if the path was refused.
    #[must_use]
    pub fn error(&self) -> Option<&RpcError> {
        match &self.outcome {
            ActionOutcome::Refused(error) => Some(error),
            ActionOutcome::Accepted { .. } => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ActionEntryWire {
    path: ProtocolPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "null_as_default"
    )]
    skipped: Vec<SkippedItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skipped_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_freed: Option<u64>,
}

impl TryFrom<ActionEntryWire> for ItemActionEntry {
    type Error = String;

    fn try_from(wire: ActionEntryWire) -> Result<Self, String> {
        let outcome = match wire.error {
            Some(error) => {
                if !wire.skipped.is_empty()
                    || wire.skipped_count.is_some()
                    || wire.bytes_freed.is_some()
                {
                    return Err(format!(
                        "action entry for {} has \"error\" and also skipped or freed items",
                        wire.path
                    ));
                }
                ActionOutcome::Refused(error)
            }
            None => {
                let skipped_count = wire.skipped_count.unwrap_or(0);
                if (wire.skipped.len() as u64) > skipped_count {
                    return Err(format!(
                        "action entry for {} lists {} skipped items but skipped_count is {}",
                        wire.path,
                        wire.skipped.len(),
                        skipped_count
                    ));
                }
                ActionOutcome::Accepted {
                    skipped: wire.skipped,
                    skipped_count,
                    bytes_freed: wire.bytes_freed,
                }
            }
        };
        Ok(Self {
            path: wire.path,
            outcome,
        })
    }
}

impl From<ItemActionEntry> for ActionEntryWire {
    fn from(entry: ItemActionEntry) -> Self {
        match entry.outcome {
            ActionOutcome::Refused(error) => Self {
                path: entry.path,
                error: Some(error),
                skipped: Vec::new(),
                skipped_count: None,
                bytes_freed: None,
            },
            ActionOutcome::Accepted {
                skipped,
                skipped_count,
                bytes_freed,
            } => Self {
                path: entry.path,
                error: None,
                skipped,
                skipped_count: (skipped_count > 0).then_some(skipped_count),
                bytes_freed,
            },
        }
    }
}

/// Result of `item.pin`, `item.unpin`, `item.hydrate` and `item.dehydrate`: one entry per
/// requested path, in request order.
///
/// `item.dehydrate` is decided per file before the response is sent. `item.pin` and
/// `item.hydrate` are accepted when recorded; the downloads that follow are reported through
/// `status.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemActionResult {
    /// Per-path outcomes.
    pub entries: Vec<ItemActionEntry>,
}

/// `status.subscribe` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSubscribeParams {
    /// Mount points or folders to watch, each with its whole subtree. Empty, absent or null
    /// means every mount.
    #[serde(default, deserialize_with = "null_as_default")]
    pub paths: Vec<ProtocolPath>,
}

/// Deserialises `null` as the type's default, so a null optional field means the same as an
/// absent one.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// `status.subscribe` result: an empty object. From then on the connection carries
/// notifications only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSubscribeResult {}

/// `status.changed` notification parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusChangedParams {
    /// The daemon-wide change sequence number of this change; see [`ItemStatusResult::seq`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The new state of each changed item. An entry with error `unknown-path` means the item
    /// is gone.
    #[serde(default, deserialize_with = "null_as_default")]
    pub entries: Vec<ItemStatusEntry>,
    /// Paths whose state changed too much to enumerate (for example a renamed folder with
    /// many descendants). For each one the client discards what it has cached for that path
    /// and everything below it, and re-requests what it is showing (ADR-0020 R8).
    #[serde(default, deserialize_with = "null_as_default")]
    pub refresh: Vec<ProtocolPath>,
}

// ---------------------------------------------------------------------------------------
// Methods added for M1 (WP-016): sign-in, mounts, pause, import and export.
// ---------------------------------------------------------------------------------------

/// The longest [`AttemptId`], in bytes. Shorter than the shortest PKCE verifier (43
/// characters, RFC 7636), and far shorter than any authorisation code or token, so an attempt
/// ID cannot carry one (ADR-0010 R6).
pub const MAX_ATTEMPT_ID_BYTES: usize = 40;

/// The longest authorisation URL `account.login.start` returns, in bytes.
pub const MAX_AUTHORIZATION_URL_BYTES: usize = 4096;

/// The longest redirect address `account.login.finish` accepts, in bytes.
pub const MAX_PASTED_URL_BYTES: usize = 8192;

/// The longest sharing link `mount.add.preview` and `mount.add.confirm` accept, in bytes.
pub const MAX_SHARING_LINK_BYTES: usize = 4096;

/// The longest confirmation token, in bytes. Like [`MAX_ATTEMPT_ID_BYTES`], too short to hold
/// a verifier, code or token.
pub const MAX_CONFIRMATION_TOKEN_BYTES: usize = 40;

/// The longest pause `mount.pause` accepts, in seconds: 7 days. A longer pause is a pause with
/// no duration, ended by `mount.resume`.
pub const MAX_PAUSE_SECS: u64 = 7 * 24 * 60 * 60;

/// Query parameters an authorisation URL must never carry: each is a credential or would make
/// one travel to the browser (ADR-0010 R6). Compared case-insensitively.
const FORBIDDEN_URL_PARAMS: &[&str] = &[
    "code",
    "code_verifier",
    "client_secret",
    "client_assertion",
    "assertion",
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "password",
];

/// Why a sign-in or mount-target value was refused. The message never repeats the value,
/// because the value may be a credential or a sharing link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValueRejected(&'static str);

impl ValueRejected {
    /// What was wrong, without the value.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.0
    }
}

/// Implements `serde` for a validated string newtype through its `new` constructor.
macro_rules! validated_string_serde {
    ($ty:ident) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

const ATTEMPT_ID_RULE: &str = "an attempt ID is 1 to 40 characters from a-z, 0-9 and '-'";

/// A sign-in attempt, as `account.login.start` names it: 1 to [`MAX_ATTEMPT_ID_BYTES`]
/// characters from `a-z`, `0-9` and `-`. Opaque to clients.
///
/// The length and alphabet are a structural guard: no PKCE verifier, authorisation code or
/// token fits, so an implementation cannot hand one out as an attempt ID (ADR-0010 R6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttemptId(String);

impl AttemptId {
    /// Checks and wraps an attempt ID.
    ///
    /// # Errors
    ///
    /// [`ValueRejected`] if it is empty, too long or has a character outside the alphabet.
    /// The error does not repeat the value.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueRejected> {
        let value = value.into();
        let ok = !value.is_empty()
            && value.len() <= MAX_ATTEMPT_ID_BYTES
            && value
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'));
        if ok {
            Ok(Self(value))
        } else {
            Err(ValueRejected(ATTEMPT_ID_RULE))
        }
    }

    /// The ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

validated_string_serde!(AttemptId);

/// The URL a client opens to sign in, from `account.login.start`. It is the only sign-in
/// datum a client receives (WP-016 AC3).
///
/// It carries the PKCE `code_challenge` (with method `S256`, ADR-0010 R1) and `state`, never
/// the verifier: the challenge is a SHA-256 digest of the verifier, from which the verifier
/// cannot be recovered. With method `plain` the challenge would *be* the verifier, so a URL
/// without `code_challenge_method=S256` is refused here. The constructor also refuses what
/// can be checked without knowing the authority:
///
/// - anything but `https://`, or `http://` to a loopback host (the simulator's authority in
///   tests);
/// - user information in the authority (`user:pass@host`);
/// - a character outside printable ASCII, or a space;
/// - a fragment;
/// - a query parameter named like a credential (`code`, `code_verifier`, `client_secret`,
///   `access_token`, `refresh_token`, `id_token`, `token`, `password`, `assertion`,
///   `client_assertion`), a parameter name that is percent-encoded or empty, or a repeated
///   parameter;
/// - a `code_challenge` that is not 43 base64url characters (the S256 digest), or more than
///   [`MAX_AUTHORIZATION_URL_BYTES`] bytes in all.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthorizationUrl(String);

impl AuthorizationUrl {
    /// Checks and wraps an authorisation URL.
    ///
    /// # Errors
    ///
    /// [`ValueRejected`] naming the rule broken; the error does not repeat the URL.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueRejected> {
        let value = value.into();
        match authorization_url_problem(&value) {
            None => Ok(Self(value)),
            Some(problem) => Err(ValueRejected(problem)),
        }
    }

    /// The URL as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

validated_string_serde!(AuthorizationUrl);

const PKCE_RULE: &str = "an authorisation URL carries a PKCE code_challenge with method S256";

fn authorization_url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() || url.len() > MAX_AUTHORIZATION_URL_BYTES {
        return Some("an authorisation URL is 1 to 4096 bytes");
    }
    if !url.bytes().all(|b| b.is_ascii_graphic()) {
        return Some("an authorisation URL is printable ASCII with no spaces");
    }
    if url.contains('#') {
        return Some("an authorisation URL has no fragment");
    }
    let (rest, secure) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, true)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, false)
    } else {
        return Some("an authorisation URL starts with https://");
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Some("an authorisation URL names a host and no user information");
    }
    if !secure && !is_loopback_authority(authority) {
        return Some("an authorisation URL uses https://, or http:// only to a loopback host");
    }
    let Some((_, query)) = rest.split_once('?') else {
        return Some(PKCE_RULE);
    };
    let mut seen: Vec<String> = Vec::new();
    let mut challenge_ok = false;
    let mut method_ok = false;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let plain = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.');
        if name.is_empty() || !name.bytes().all(plain) {
            return Some("an authorisation URL's parameter names are plain, not percent-encoded");
        }
        let lower = name.to_ascii_lowercase();
        if FORBIDDEN_URL_PARAMS.contains(&lower.as_str()) {
            return Some("an authorisation URL never carries a code, verifier, secret or token");
        }
        if seen.contains(&lower) {
            return Some("an authorisation URL names each parameter once");
        }
        match lower.as_str() {
            "code_challenge" => {
                challenge_ok = value.len() == 43
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
            }
            "code_challenge_method" => method_ok = value == "S256",
            _ => {}
        }
        seen.push(lower);
    }
    if challenge_ok && method_ok {
        None
    } else {
        Some(PKCE_RULE)
    }
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// True for an empty, over-long value or one with whitespace or a control character.
fn opaque_text_bad(value: &str, max: usize) -> bool {
    value.is_empty()
        || value.len() > max
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
}

const PASTED_RULE: &str =
    "the pasted address must be 1 to 8192 bytes with no spaces or control characters";

/// The address the browser ended on, pasted by the user for a headless sign-in
/// (`account.login.finish`). It holds the authorisation code, so it is a credential.
///
/// It travels only from client to daemon. No result, notification or error carries it, and
/// its `Debug` form is redacted, so it cannot reach a log by accident (WP-016 AC4,
/// ADR-0010 R6). Checked here only for size and for characters no URL contains: 1 to
/// [`MAX_PASTED_URL_BYTES`] bytes with no whitespace or control character. It is never
/// trimmed or repaired; whether it is a redirect to the attempt's URI with the right `state`
/// is the daemon's check.
#[derive(Clone, PartialEq, Eq)]
pub struct PastedRedirect(String);

impl PastedRedirect {
    /// Checks and wraps a pasted address.
    ///
    /// # Errors
    ///
    /// [`ValueRejected`]; the error does not repeat the address.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueRejected> {
        let value = value.into();
        if opaque_text_bad(&value, MAX_PASTED_URL_BYTES) {
            Err(ValueRejected(PASTED_RULE))
        } else {
            Ok(Self(value))
        }
    }

    /// The address. Named so that every use of the credential is visible in review.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PastedRedirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PastedRedirect(<redacted>)")
    }
}

validated_string_serde!(PastedRedirect);

const LINK_RULE: &str = "a sharing link is an https:// address of at most 4096 bytes with no \
                         spaces or control characters";

/// A sharing link given to `mount.add.preview` and `mount.add.confirm`. Whoever holds a
/// link may be able to open what it points to, so it is treated like a credential: no result,
/// notification or error repeats it, the confirmation token does not encode it readably, and
/// its `Debug` form is redacted.
///
/// Checked here for form only: `https://` (the scheme in any case) and a host, at most
/// [`MAX_SHARING_LINK_BYTES`] bytes, no whitespace or control character. It is never trimmed
/// or repaired.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SharingLink(String);

impl SharingLink {
    /// Checks and wraps a sharing link.
    ///
    /// # Errors
    ///
    /// [`ValueRejected`]; the error does not repeat the link.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueRejected> {
        let value = value.into();
        let scheme_ok = value
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"));
        let host_ok = value
            .get(8..)
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'));
        if opaque_text_bad(&value, MAX_SHARING_LINK_BYTES) || !scheme_ok || !host_ok {
            Err(ValueRejected(LINK_RULE))
        } else {
            Ok(Self(value))
        }
    }

    /// The link. Named so that every use of it is visible in review.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SharingLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharingLink(<redacted>)")
    }
}

validated_string_serde!(SharingLink);

/// The token `mount.add.preview` returns and `mount.add.confirm` takes. Opaque: clients never
/// parse, build or change it. At most [`MAX_CONFIRMATION_TOKEN_BYTES`] characters from `a-z`,
/// `0-9` and `.`. Issued and checked by [`crate::paging::ConfirmationLedger`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConfirmationToken(String);

impl ConfirmationToken {
    /// Checks and wraps a token.
    ///
    /// # Errors
    ///
    /// [`ValueRejected`] if it is empty, too long or has another character.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueRejected> {
        let value = value.into();
        let ok = !value.is_empty()
            && value.len() <= MAX_CONFIRMATION_TOKEN_BYTES
            && value
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.'));
        if ok {
            Ok(Self(value))
        } else {
            Err(ValueRejected(
                "a confirmation token is 1 to 40 characters from a-z, 0-9 and '.'",
            ))
        }
    }

    /// The token as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

validated_string_serde!(ConfirmationToken);

/// A path on the local filesystem, outside every mount: the source of `item.import` and the
/// destination of `item.export`.
///
/// Spelled by the same rule as a [`ProtocolPath`]: absolute, canonical (no `.` or `..`
/// component, no empty component, no trailing slash other than `/` itself), UTF-8, no NUL
/// byte, at most 4,096 bytes. A path that breaks the rule is refused with `invalid-params`,
/// never repaired (WP-016 AC8). The rule is lexical: this type cannot see symbolic links.
/// The daemon resolves the path and refuses one whose resolved location is under a mount
/// point or the daemon's data directory (`local-file-error`, reason `inside-mount`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalPath(String);

impl LocalPath {
    /// Checks and wraps a local path.
    ///
    /// # Errors
    ///
    /// [`crate::path::PathError`] for a path that breaks the protocol path rule.
    pub fn new(path: impl Into<String>) -> Result<Self, crate::path::PathError> {
        ProtocolPath::new(path).map(|p| Self(p.as_str().to_owned()))
    }

    /// The path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LocalPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

validated_string_serde!(LocalPath);

/// Refuses a value of an open request set that this build does not know. A request is never
/// given a default in place of a value it did not understand.
fn known_value(value: &impl std::fmt::Display, field: &str, known: &[&str]) -> Result<(), String> {
    let text = value.to_string();
    if known.contains(&text.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "unknown {field} {:?}; this daemon accepts {}",
            crate::error::truncate_to(&text, 64),
            known.join(", ")
        ))
    }
}

/// `account.login.start` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LoginStartWire", into = "LoginStartWire")]
pub struct LoginStartParams {
    /// How to sign in. Never [`LoginMode::Other`]: an unknown mode is refused when decoded.
    pub mode: LoginMode,
}

#[derive(Serialize, Deserialize)]
struct LoginStartWire {
    mode: LoginMode,
}

impl TryFrom<LoginStartWire> for LoginStartParams {
    type Error = String;

    fn try_from(wire: LoginStartWire) -> Result<Self, String> {
        known_value(&wire.mode, "mode", LoginMode::KNOWN)?;
        Ok(Self { mode: wire.mode })
    }
}

impl From<LoginStartParams> for LoginStartWire {
    fn from(params: LoginStartParams) -> Self {
        Self { mode: params.mode }
    }
}

/// `account.login.start` result: the attempt and the URL to open. Nothing else about the
/// sign-in reaches the client (ADR-0010 R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStartResult {
    /// Names the attempt in `account.login.finish`.
    pub attempt_id: AttemptId,
    /// The URL to open in a browser.
    pub authorization_url: AuthorizationUrl,
}

/// `account.login.finish` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginFinishParams {
    /// The attempt from `account.login.start`.
    pub attempt_id: AttemptId,
    /// For a headless attempt, the address the browser ended on; absent for a browser
    /// attempt. A credential: never repeated in any response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<PastedRedirect>,
}

/// The mount `account.login.finish` proposes for the account's own drive. Nothing is created;
/// the client asks the user, then calls `mount.add.preview` with target `own-drive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedMount {
    /// The account's own drive.
    pub drive_id: DriveId,
    /// Its type. Open set.
    pub drive_type: DriveType,
    /// The default mount point: `~/OneDrive` or `~/OneDrive - <Org>`.
    pub mount_point: ProtocolPath,
}

/// `account.login.finish` result: who signed in. It creates no mount (overview "Sign-in never
/// creates a mount").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginFinishResult {
    /// The signed-in account, with `user_name` so the client can show it before asking.
    pub account: AccountInfo,
    /// The proposed mount for the own drive; absent when that drive is already mounted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_mount: Option<ProposedMount>,
}

/// `account.logout` parameters. There is no parameter that could delete anything, local or
/// remote (WP-016 AC7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoutParams {
    /// The account to sign out.
    pub account_id: AccountId,
    /// True to report what signing out would find, changing nothing.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "null_as_default"
    )]
    pub dry_run: bool,
}

/// `account.logout` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogoutResult {
    /// The account.
    pub account_id: AccountId,
    /// True when the account was signed out; false for a dry run.
    pub signed_out: bool,
    /// Files in the account's mounts with changes not yet on the server. They are kept and
    /// upload after the account signs in again (F01 "Sign-out").
    pub unsynced_files: u64,
}

/// What a mount is to be made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountSource {
    /// The account's own drive.
    OwnDrive,
    /// A drive named by ID.
    Drive(DriveId),
    /// The drive a sharing link points into. M1 mounts the drive root, even for a link to a
    /// folder inside it (overview "M1 mounts drive roots only").
    SharingLink(SharingLink),
}

/// The target of `mount.add.preview` and `mount.add.confirm`: which account, and what to
/// mount with it.
///
/// On the wire: `account_id`, `target` (`own-drive`, `drive-id` or `sharing-link`), and
/// `drive_id` exactly when the target is `drive-id`, `sharing_link` exactly when it is
/// `sharing-link`. Any other combination, and an unknown target, is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountTarget {
    /// The account whose access is used.
    pub account_id: AccountId,
    /// What to mount.
    pub source: MountSource,
}

#[derive(Serialize, Deserialize)]
struct MountTargetWire {
    account_id: AccountId,
    target: MountTargetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drive_id: Option<DriveId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sharing_link: Option<SharingLink>,
}

impl MountTargetWire {
    fn into_target(self) -> Result<MountTarget, String> {
        let source = match (&self.target, self.drive_id, self.sharing_link) {
            (MountTargetKind::OwnDrive, None, None) => MountSource::OwnDrive,
            (MountTargetKind::DriveId, Some(drive), None) => MountSource::Drive(drive),
            (MountTargetKind::SharingLink, None, Some(link)) => MountSource::SharingLink(link),
            _ => {
                return Err("give target own-drive alone, drive-id with drive_id, or \
                            sharing-link with sharing_link"
                    .into());
            }
        };
        Ok(MountTarget {
            account_id: self.account_id,
            source,
        })
    }

    fn from_target(target: MountTarget) -> Self {
        let (kind, drive_id, sharing_link) = match target.source {
            MountSource::OwnDrive => (MountTargetKind::OwnDrive, None, None),
            MountSource::Drive(d) => (MountTargetKind::DriveId, Some(d), None),
            MountSource::SharingLink(l) => (MountTargetKind::SharingLink, None, Some(l)),
        };
        Self {
            account_id: target.account_id,
            target: kind,
            drive_id,
            sharing_link,
        }
    }
}

/// `mount.add.preview` parameters. The preview never changes the user's access (ADR-0015 R2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MountTargetWire", into = "MountTargetWire")]
pub struct MountAddPreviewParams {
    /// What to preview.
    pub target: MountTarget,
}

impl TryFrom<MountTargetWire> for MountAddPreviewParams {
    type Error = String;

    fn try_from(wire: MountTargetWire) -> Result<Self, String> {
        wire.into_target().map(|target| Self { target })
    }
}

impl From<MountAddPreviewParams> for MountTargetWire {
    fn from(params: MountAddPreviewParams) -> Self {
        Self::from_target(params.target)
    }
}

/// `mount.add.preview` result: what confirming would mount. Nothing is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountAddPreviewResult {
    /// Pass this to `mount.add.confirm` with the same target parameters.
    pub confirmation_token: ConfirmationToken,
    /// The account.
    pub account_id: AccountId,
    /// The drive that would be mounted (its root, in M1).
    pub drive_id: DriveId,
    /// Its type. Open set.
    pub drive_type: DriveType,
    /// The drive's name as Graph reports it (for a library, the library's name). For display
    /// only; capped at 1 KiB like every message.
    #[serde(serialize_with = "crate::error::serialize_capped")]
    pub drive_name: String,
    /// For a library, the site's name as Graph reports it. For display only; capped at
    /// 1 KiB.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_capped_opt"
    )]
    pub site_name: Option<String>,
    /// How many items the drive holds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<u64>,
    /// True when the mount would be read-only.
    pub read_only: bool,
    /// The proposed mount point.
    pub mount_point: ProtocolPath,
}

/// `mount.add.confirm` parameters: the preview's target parameters again, the token, and
/// optionally another mount point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MountAddConfirmWire", into = "MountAddConfirmWire")]
pub struct MountAddConfirmParams {
    /// The same target as the preview; a token used with another target is refused.
    pub target: MountTarget,
    /// The preview's token. Single use.
    pub confirmation_token: ConfirmationToken,
    /// A mount point other than the proposed one.
    pub mount_point: Option<ProtocolPath>,
}

#[derive(Serialize, Deserialize)]
struct MountAddConfirmWire {
    account_id: AccountId,
    target: MountTargetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drive_id: Option<DriveId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sharing_link: Option<SharingLink>,
    confirmation_token: ConfirmationToken,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mount_point: Option<ProtocolPath>,
}

impl TryFrom<MountAddConfirmWire> for MountAddConfirmParams {
    type Error = String;

    fn try_from(wire: MountAddConfirmWire) -> Result<Self, String> {
        let target = MountTargetWire {
            account_id: wire.account_id,
            target: wire.target,
            drive_id: wire.drive_id,
            sharing_link: wire.sharing_link,
        }
        .into_target()?;
        Ok(Self {
            target,
            confirmation_token: wire.confirmation_token,
            mount_point: wire.mount_point,
        })
    }
}

impl From<MountAddConfirmParams> for MountAddConfirmWire {
    fn from(params: MountAddConfirmParams) -> Self {
        let target = MountTargetWire::from_target(params.target);
        Self {
            account_id: target.account_id,
            target: target.target,
            drive_id: target.drive_id,
            sharing_link: target.sharing_link,
            confirmation_token: params.confirmation_token,
            mount_point: params.mount_point,
        }
    }
}

/// `mount.add.confirm` result: the new mount, in state `resyncing` until its first full
/// delta has been applied (overview "First enumeration").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountAddConfirmResult {
    /// The new mount.
    pub mount: MountInfo,
}

/// `mount.remove` parameters. There is no parameter that could delete remote content:
/// removing a mount never deletes anything on the server (ADR-0015 R8, WP-016 AC7).
///
/// A mount with files whose changes are not yet on the server is not removed unless
/// `keep_unsynced` is true: the daemon answers `refused` with reason `unsynced-changes`,
/// counting and naming them ([`RpcError::unsynced_changes`]), so the user can confirm that
/// those files will stay only on this computer (F12 step 11). Absent, `null` and `false` all
/// mean "refuse". A dry run never refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRemoveParams {
    /// The mount to remove.
    pub mount_id: MountId,
    /// True to report what removal would find, changing nothing. Never refused for unsynced
    /// changes.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "null_as_default"
    )]
    pub dry_run: bool,
    /// True when the user has confirmed that files with changes not yet on the server will
    /// stay only on this computer: removal then treats them like every other local file
    /// (F12 step 11) and they are never uploaded. False refuses removal while there are any.
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "null_as_default"
    )]
    pub keep_unsynced: bool,
}

/// `mount.remove` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRemoveResult {
    /// The mount.
    pub mount_id: MountId,
    /// True when the mount was removed; false for a dry run.
    pub removed: bool,
    /// Where the local copies are (or would be) kept: the mount point, now a plain directory
    /// (ADR-0021).
    pub mount_point: ProtocolPath,
    /// Fully local files whose content is (or would be) kept at the mount point.
    pub kept_files: u64,
    /// Files whose changes are not yet on the server. For a dry run, how many a removal
    /// without `keep_unsynced` would refuse over; after a removal, how many were kept only on
    /// this computer (only possible with `keep_unsynced`).
    pub unsynced_files: u64,
}

/// `mount.pause` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MountPauseWire", into = "MountPauseWire")]
pub struct MountPauseParams {
    /// The mount to pause; absent for every mount.
    pub mount_id: Option<MountId>,
    /// How long, 1 to [`MAX_PAUSE_SECS`] seconds; absent to pause until `mount.resume`.
    pub duration_secs: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct MountPauseWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mount_id: Option<MountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_secs: Option<u64>,
}

impl TryFrom<MountPauseWire> for MountPauseParams {
    type Error = String;

    fn try_from(wire: MountPauseWire) -> Result<Self, String> {
        if let Some(secs) = wire.duration_secs.filter(|&s| s == 0 || s > MAX_PAUSE_SECS) {
            return Err(format!(
                "duration_secs is {secs}; give 1 to {MAX_PAUSE_SECS} seconds, or leave it out \
                 to pause until resumed"
            ));
        }
        Ok(Self {
            mount_id: wire.mount_id,
            duration_secs: wire.duration_secs,
        })
    }
}

impl From<MountPauseParams> for MountPauseWire {
    fn from(params: MountPauseParams) -> Self {
        Self {
            mount_id: params.mount_id,
            duration_secs: params.duration_secs,
        }
    }
}

/// `mount.pause` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPauseResult {
    /// How many mounts this request paused.
    pub paused_mounts: u64,
    /// When the pause ends, in whole seconds since the Unix epoch (UTC); absent for a pause
    /// until `mount.resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<u64>,
}

/// `mount.resume` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountResumeParams {
    /// The mount to resume; absent for every mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<MountId>,
}

/// `mount.resume` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountResumeResult {
    /// How many paused mounts this request resumed.
    pub resumed_mounts: u64,
}

/// `item.import` parameters: copy a local file into a mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ImportWire", into = "ImportWire")]
pub struct ItemImportParams {
    /// The local file to copy.
    pub source: LocalPath,
    /// The file's path in the mount. Its parent folder must exist.
    pub destination: ProtocolPath,
    /// `create` for a new file, `replace` to replace the existing file. Never
    /// [`TransferMode::Other`].
    pub mode: TransferMode,
}

#[derive(Serialize, Deserialize)]
struct ImportWire {
    source: LocalPath,
    destination: ProtocolPath,
    mode: TransferMode,
}

impl TryFrom<ImportWire> for ItemImportParams {
    type Error = String;

    fn try_from(wire: ImportWire) -> Result<Self, String> {
        known_value(&wire.mode, "mode", TransferMode::KNOWN)?;
        Ok(Self {
            source: wire.source,
            destination: wire.destination,
            mode: wire.mode,
        })
    }
}

impl From<ItemImportParams> for ImportWire {
    fn from(params: ItemImportParams) -> Self {
        Self {
            source: params.source,
            destination: params.destination,
            mode: params.mode,
        }
    }
}

/// `item.import` result: the item, which now has local changes to upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemImportResult {
    /// The destination.
    pub path: ProtocolPath,
    /// Its state, normally sync `pending-upload`.
    pub item: ItemInfo,
    /// The bytes copied in.
    pub bytes_copied: u64,
}

/// `item.export` parameters: copy a file out of a mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ExportWire", into = "ExportWire")]
pub struct ItemExportParams {
    /// The file in the mount.
    pub source: ProtocolPath,
    /// The local path to write.
    pub destination: LocalPath,
    /// `create` when nothing may exist at the destination, `replace` to replace what does.
    /// Never [`TransferMode::Other`].
    pub mode: TransferMode,
}

#[derive(Serialize, Deserialize)]
struct ExportWire {
    source: ProtocolPath,
    destination: LocalPath,
    mode: TransferMode,
}

impl TryFrom<ExportWire> for ItemExportParams {
    type Error = String;

    fn try_from(wire: ExportWire) -> Result<Self, String> {
        known_value(&wire.mode, "mode", TransferMode::KNOWN)?;
        Ok(Self {
            source: wire.source,
            destination: wire.destination,
            mode: wire.mode,
        })
    }
}

impl From<ItemExportParams> for ExportWire {
    fn from(params: ItemExportParams) -> Self {
        Self {
            source: params.source,
            destination: params.destination,
            mode: params.mode,
        }
    }
}

/// `item.export` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemExportResult {
    /// The file in the mount.
    pub path: ProtocolPath,
    /// Where it was written.
    pub destination: LocalPath,
    /// The bytes copied out.
    pub bytes_copied: u64,
}

/// A request decoded into its typed parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodCall {
    /// `hello`.
    Hello(HelloParams),
    /// `daemon.status`.
    DaemonStatus,
    /// `account.list`.
    AccountList(ListParams),
    /// `mount.list`.
    MountList(ListParams),
    /// `item.status`.
    ItemStatus(ItemStatusParams),
    /// `item.pin`.
    ItemPin(ItemPathsParams),
    /// `item.unpin`.
    ItemUnpin(ItemPathsParams),
    /// `item.hydrate`.
    ItemHydrate(ItemPathsParams),
    /// `item.dehydrate`: free up space. As the user-facing action, it sets the pin to
    /// `unpinned` and then dehydrates, and refuses anything not `in-sync` before changing the
    /// pin (overview, "Rulings on details"; F06).
    ItemDehydrate(ItemPathsParams),
    /// `status.subscribe`.
    StatusSubscribe(StatusSubscribeParams),
    /// `account.login.start`.
    AccountLoginStart(LoginStartParams),
    /// `account.login.finish`.
    AccountLoginFinish(LoginFinishParams),
    /// `account.logout`.
    AccountLogout(LogoutParams),
    /// `mount.add.preview`.
    MountAddPreview(MountAddPreviewParams),
    /// `mount.add.confirm`.
    MountAddConfirm(MountAddConfirmParams),
    /// `mount.remove`.
    MountRemove(MountRemoveParams),
    /// `mount.pause`.
    MountPause(MountPauseParams),
    /// `mount.resume`.
    MountResume(MountResumeParams),
    /// `item.import`.
    ItemImport(ItemImportParams),
    /// `item.export`.
    ItemExport(ItemExportParams),
}

impl MethodCall {
    /// Decodes a method name and parameters.
    ///
    /// # Errors
    ///
    /// `method-not-found` for a method not in this version (WP-005 AC5), `invalid-params` for
    /// parameters of the wrong shape, including any relative path (AC6) and positional
    /// (array) parameters. Unknown parameter fields are ignored.
    pub fn parse(method: &str, params: Option<&Value>) -> Result<Self, RpcError> {
        let call = match method {
            method::HELLO => Self::Hello(decode(method, params)?),
            method::DAEMON_STATUS => {
                decode::<NoParams>(method, params).map(|_| Self::DaemonStatus)?
            }
            method::ACCOUNT_LIST => Self::AccountList(decode(method, params)?),
            method::MOUNT_LIST => Self::MountList(decode(method, params)?),
            method::ITEM_STATUS => Self::ItemStatus(decode_status(method, params)?),
            method::ITEM_PIN => Self::ItemPin(decode_action(method, params)?),
            method::ITEM_UNPIN => Self::ItemUnpin(decode_action(method, params)?),
            method::ITEM_HYDRATE => Self::ItemHydrate(decode_action(method, params)?),
            method::ITEM_DEHYDRATE => Self::ItemDehydrate(decode_action(method, params)?),
            method::STATUS_SUBSCRIBE => Self::StatusSubscribe(decode(method, params)?),
            method::ACCOUNT_LOGIN_START => Self::AccountLoginStart(decode(method, params)?),
            method::ACCOUNT_LOGIN_FINISH => {
                Self::AccountLoginFinish(decode_redacted(method, params, FINISH_SHAPE)?)
            }
            method::ACCOUNT_LOGOUT => Self::AccountLogout(decode(method, params)?),
            method::MOUNT_ADD_PREVIEW => {
                Self::MountAddPreview(decode_redacted(method, params, TARGET_SHAPE)?)
            }
            method::MOUNT_ADD_CONFIRM => {
                Self::MountAddConfirm(decode_redacted(method, params, CONFIRM_SHAPE)?)
            }
            method::MOUNT_REMOVE => Self::MountRemove(decode(method, params)?),
            method::MOUNT_PAUSE => Self::MountPause(decode(method, params)?),
            method::MOUNT_RESUME => Self::MountResume(decode(method, params)?),
            method::ITEM_IMPORT => Self::ItemImport(decode(method, params)?),
            method::ITEM_EXPORT => Self::ItemExport(decode(method, params)?),
            other => return Err(RpcError::method_not_found(other)),
        };
        Ok(call)
    }

    /// Decodes a request's method and parameters; see [`MethodCall::parse`].
    ///
    /// # Errors
    ///
    /// As for [`MethodCall::parse`].
    pub fn from_request(request: &Request) -> Result<Self, RpcError> {
        Self::parse(&request.method, request.params.as_ref())
    }

    /// The method name.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        match self {
            Self::Hello(_) => method::HELLO,
            Self::DaemonStatus => method::DAEMON_STATUS,
            Self::AccountList(_) => method::ACCOUNT_LIST,
            Self::MountList(_) => method::MOUNT_LIST,
            Self::ItemStatus(_) => method::ITEM_STATUS,
            Self::ItemPin(_) => method::ITEM_PIN,
            Self::ItemUnpin(_) => method::ITEM_UNPIN,
            Self::ItemHydrate(_) => method::ITEM_HYDRATE,
            Self::ItemDehydrate(_) => method::ITEM_DEHYDRATE,
            Self::StatusSubscribe(_) => method::STATUS_SUBSCRIBE,
            Self::AccountLoginStart(_) => method::ACCOUNT_LOGIN_START,
            Self::AccountLoginFinish(_) => method::ACCOUNT_LOGIN_FINISH,
            Self::AccountLogout(_) => method::ACCOUNT_LOGOUT,
            Self::MountAddPreview(_) => method::MOUNT_ADD_PREVIEW,
            Self::MountAddConfirm(_) => method::MOUNT_ADD_CONFIRM,
            Self::MountRemove(_) => method::MOUNT_REMOVE,
            Self::MountPause(_) => method::MOUNT_PAUSE,
            Self::MountResume(_) => method::MOUNT_RESUME,
            Self::ItemImport(_) => method::ITEM_IMPORT,
            Self::ItemExport(_) => method::ITEM_EXPORT,
        }
    }

    /// The parameters as JSON; `None` for methods that take none.
    ///
    /// # Errors
    ///
    /// The `serde_json` error if serialisation fails.
    pub fn params(&self) -> Result<Option<Value>, serde_json::Error> {
        let value = match self {
            Self::Hello(p) => serde_json::to_value(p)?,
            Self::DaemonStatus => return Ok(None),
            Self::AccountList(p) | Self::MountList(p) => serde_json::to_value(p)?,
            Self::ItemStatus(p) => serde_json::to_value(p)?,
            Self::ItemPin(p)
            | Self::ItemUnpin(p)
            | Self::ItemHydrate(p)
            | Self::ItemDehydrate(p) => serde_json::to_value(p)?,
            Self::StatusSubscribe(p) => serde_json::to_value(p)?,
            Self::AccountLoginStart(p) => serde_json::to_value(p)?,
            Self::AccountLoginFinish(p) => serde_json::to_value(p)?,
            Self::AccountLogout(p) => serde_json::to_value(p)?,
            Self::MountAddPreview(p) => serde_json::to_value(p)?,
            Self::MountAddConfirm(p) => serde_json::to_value(p)?,
            Self::MountRemove(p) => serde_json::to_value(p)?,
            Self::MountPause(p) => serde_json::to_value(p)?,
            Self::MountResume(p) => serde_json::to_value(p)?,
            Self::ItemImport(p) => serde_json::to_value(p)?,
            Self::ItemExport(p) => serde_json::to_value(p)?,
        };
        Ok(Some(value))
    }

    /// A request carrying this call.
    ///
    /// # Errors
    ///
    /// The `serde_json` error if serialisation fails.
    pub fn to_request(&self, id: impl Into<Id>) -> Result<Request, serde_json::Error> {
        Ok(Request {
            jsonrpc: crate::envelope::JsonRpcVersion,
            id: id.into(),
            method: self.method().to_owned(),
            params: self.params()?,
        })
    }
}

/// Decodes `item.status` parameters and enforces the batch limits of [`Limits::V1`], sized so
/// that the request plus the longest cursor always fits in one line (ADR-0020 R4).
fn decode_status(method: &str, params: Option<&Value>) -> Result<ItemStatusParams, RpcError> {
    let params: ItemStatusParams = decode(method, params)?;
    if let ItemStatusTarget::Paths(paths) = &params.target {
        if paths.len() > MAX_STATUS_PATHS as usize {
            return Err(RpcError::invalid_params(
                method,
                format!(
                    "{} paths given, the limit is {MAX_STATUS_PATHS}; split the batch",
                    paths.len()
                ),
            ));
        }
        let bytes = encoded_path_bytes(paths);
        if bytes > MAX_STATUS_PATH_BYTES {
            return Err(RpcError::invalid_params(
                method,
                format!(
                    "the paths take {bytes} bytes, the limit is {MAX_STATUS_PATH_BYTES}; split \
                     the batch"
                ),
            ));
        }
    }
    Ok(params)
}

/// Decodes action parameters and enforces [`Limits::V1`] (ADR-0020 R5): a request over a
/// limit is refused as a whole, before anything is done.
fn decode_action(method: &str, params: Option<&Value>) -> Result<ItemPathsParams, RpcError> {
    let params: ItemPathsParams = decode(method, params)?;
    if params.paths.len() > MAX_ACTION_PATHS as usize {
        return Err(RpcError::invalid_params(
            method,
            format!(
                "{} paths given, the limit is {MAX_ACTION_PATHS}; split the request",
                params.paths.len()
            ),
        ));
    }
    let bytes = encoded_path_bytes(&params.paths);
    if bytes > MAX_ACTION_PATH_BYTES {
        return Err(RpcError::invalid_params(
            method,
            format!(
                "the paths take {bytes} bytes, the limit is {MAX_ACTION_PATH_BYTES}; split the \
                 request"
            ),
        ));
    }
    Ok(params)
}

/// The bytes `paths` take in their minimal JSON string encoding (as `serde_json` writes them,
/// quotes included): the measure the path-byte limits use. A client counting its own
/// encoding never counts less.
#[must_use]
pub fn encoded_path_bytes(paths: &[ProtocolPath]) -> u64 {
    paths
        .iter()
        .map(|p| serde_json::to_string(p.as_str()).map_or(0, |s| s.len() as u64))
        .sum()
}

const FINISH_SHAPE: &str =
    "attempt_id (an attempt ID) and, for a headless attempt, redirect_url (the pasted address)";

const TARGET_SHAPE: &str = "account_id, and target own-drive alone, drive-id with drive_id, or \
                            sharing-link with sharing_link (an https:// address)";

const CONFIRM_SHAPE: &str = "the preview's account_id, target and drive_id or sharing_link, \
                             confirmation_token, and optionally mount_point (an absolute, \
                             canonical path)";

/// Decodes the parameters of a method in [`method::REDACTED`]. On failure the message says
/// what shape was expected and repeats nothing from the request: `serde` errors can quote a
/// value, and here a value may be a pasted redirect address (holding an authorisation code) or
/// a sharing link (ADR-0010 R6, WP-016 AC4).
fn decode_redacted<P: DeserializeOwned>(
    method: &str,
    params: Option<&Value>,
    expected: &str,
) -> Result<P, RpcError> {
    decode(method, params).map_err(|_| {
        RpcError::invalid_params(
            method,
            format!(
                "expected an object with {expected}; the values given are not repeated here \
                 because they may hold a credential or a sharing link"
            ),
        )
    })
}

fn decode<P: DeserializeOwned>(method: &str, params: Option<&Value>) -> Result<P, RpcError> {
    let empty = Value::Object(serde_json::Map::new());
    let params = match params {
        None | Some(Value::Null) => &empty,
        Some(Value::Array(_)) => {
            return Err(RpcError::invalid_params(
                method,
                "parameters must be passed by name, as an object",
            ));
        }
        Some(other) => other,
    };
    P::deserialize(params).map_err(|e| RpcError::invalid_params(method, e))
}

/// Where a connection is in its life (ADR-0009 R3, overview "Change stream").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing but `hello` is accepted.
    AwaitingHello,
    /// `hello` succeeded with this protocol version.
    Ready(u32),
    /// `status.subscribe` succeeded; the connection carries notifications only.
    Subscribed(u32),
}

/// Per-connection gatekeeping that a server applies before dispatching a request.
///
/// Pure state: it decides whether a call may run and records the outcome of `hello` and
/// `status.subscribe`. A refused call does not change the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    phase: Phase,
    /// Whether `hello` advertised [`CAPABILITY_IMPORT_EXPORT`].
    import_export: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A new connection, awaiting `hello`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Phase::AwaitingHello,
            import_export: false,
        }
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether `call` may run now.
    ///
    /// # Errors
    ///
    /// `hello-required` for anything but `hello` before `hello` has succeeded (ADR-0009 R3,
    /// ADR-0019 R5); `invalid-request` for a second `hello`, and for any request on a
    /// subscribed connection; `method-not-found` for `item.import` and `item.export` when
    /// `hello` did not advertise [`CAPABILITY_IMPORT_EXPORT`].
    pub fn admit(&self, call: &MethodCall) -> Result<(), RpcError> {
        match (self.phase, call) {
            (Phase::AwaitingHello, MethodCall::Hello(_)) => Ok(()),
            (Phase::AwaitingHello, other) => Err(RpcError::new(
                ErrorCode::HelloRequired,
                format!(
                    "{} refused: the first request on a connection must be hello",
                    other.method()
                ),
            )),
            (Phase::Ready(_), MethodCall::Hello(_)) => Err(RpcError::new(
                ErrorCode::InvalidRequest,
                "hello refused: this connection has already completed hello",
            )),
            (Phase::Ready(_), MethodCall::ItemImport(_) | MethodCall::ItemExport(_))
                if !self.import_export =>
            {
                Err(RpcError::new(
                    ErrorCode::MethodNotFound,
                    format!(
                        "method {:?} is not offered by this daemon: hello did not advertise \
                         the capability {CAPABILITY_IMPORT_EXPORT}",
                        call.method()
                    ),
                ))
            }
            (Phase::Ready(_), _) => Ok(()),
            (Phase::Subscribed(_), other) => Err(RpcError::new(
                ErrorCode::InvalidRequest,
                format!(
                    "{} refused: this connection is subscribed and carries notifications only; \
                     open a second connection for requests",
                    other.method()
                ),
            )),
        }
    }

    /// Runs `hello` for this connection: negotiates a version against `daemon_supported` and,
    /// on success, moves to [`Phase::Ready`]. `capabilities` is what the result advertises;
    /// the connection admits `item.import` and `item.export` only if it includes
    /// [`CAPABILITY_IMPORT_EXPORT`].
    ///
    /// # Errors
    ///
    /// As [`Session::admit`] for a repeated `hello`, or `unsupported-version` (ADR-0009 R4);
    /// on error the phase is unchanged.
    pub fn hello(
        &mut self,
        params: &HelloParams,
        daemon_supported: &[u32],
        capabilities: &[&str],
    ) -> Result<HelloResult, RpcError> {
        self.admit(&MethodCall::Hello(params.clone()))?;
        let version = negotiate(daemon_supported, &params.protocol_versions)?;
        self.phase = Phase::Ready(version);
        self.import_export = capabilities.contains(&CAPABILITY_IMPORT_EXPORT);
        Ok(HelloResult {
            protocol_version: version,
            capabilities: capabilities.iter().map(|&c| c.to_owned()).collect(),
            limits: Limits::V1,
        })
    }

    /// Records a successful `status.subscribe`. Has no effect unless the phase is
    /// [`Phase::Ready`].
    pub fn subscribed(&mut self) {
        if let Phase::Ready(version) = self.phase {
            self.phase = Phase::Subscribed(version);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use mydrive_core::state::{Hydration, Sync as SyncState};

    use super::*;
    use crate::values::{Modifier, RefusalReason};

    fn p(s: &str) -> ProtocolPath {
        ProtocolPath::new(s).unwrap()
    }

    fn hello(versions: &[u32]) -> HelloParams {
        HelloParams {
            client_name: "test".into(),
            protocol_versions: versions.to_vec(),
        }
    }

    fn paths(list: &[&str]) -> ItemPathsParams {
        ItemPathsParams {
            paths: list.iter().map(|s| p(s)).collect(),
        }
    }

    const LINK: &str = "https://example.sharepoint.com/:f:/s/Finance/EjK3pQ2-planted-link";

    fn acct() -> AccountId {
        AccountId::new("acct-work-1").unwrap()
    }

    fn target(source: MountSource) -> MountTarget {
        MountTarget {
            account_id: acct(),
            source,
        }
    }

    fn link_target() -> MountTarget {
        target(MountSource::SharingLink(SharingLink::new(LINK).unwrap()))
    }

    /// One call of each method added by WP-016, in `method::ALL` order.
    fn new_calls() -> Vec<MethodCall> {
        vec![
            MethodCall::AccountLoginStart(LoginStartParams {
                mode: LoginMode::Headless,
            }),
            MethodCall::AccountLoginFinish(LoginFinishParams {
                attempt_id: AttemptId::new("attempt-1").unwrap(),
                redirect_url: Some(
                    PastedRedirect::new("https://login.example/nativeclient?code=c&state=s")
                        .unwrap(),
                ),
            }),
            MethodCall::AccountLogout(LogoutParams {
                account_id: acct(),
                dry_run: true,
            }),
            MethodCall::MountAddPreview(MountAddPreviewParams {
                target: link_target(),
            }),
            MethodCall::MountAddConfirm(MountAddConfirmParams {
                target: target(MountSource::Drive(DriveId::new("b!abc").unwrap())),
                confirmation_token: ConfirmationToken::new("c1.00.00").unwrap(),
                mount_point: Some(p("/home/u/Example/Site - Documents")),
            }),
            MethodCall::MountRemove(MountRemoveParams {
                mount_id: MountId::new("personal").unwrap(),
                dry_run: false,
                keep_unsynced: true,
            }),
            MethodCall::MountPause(MountPauseParams {
                mount_id: None,
                duration_secs: Some(7_200),
            }),
            MethodCall::MountResume(MountResumeParams {
                mount_id: Some(MountId::new("personal").unwrap()),
            }),
            MethodCall::ItemImport(ItemImportParams {
                source: LocalPath::new("/home/u/Downloads/report.pdf").unwrap(),
                destination: p("/home/u/OneDrive/Documents/report.pdf"),
                mode: TransferMode::Create,
            }),
            MethodCall::ItemExport(ItemExportParams {
                source: p("/home/u/OneDrive/Documents/report.pdf"),
                destination: LocalPath::new("/home/u/Desktop/report.pdf").unwrap(),
                mode: TransferMode::Replace,
            }),
        ]
    }

    fn ready_session(capabilities: &[&str]) -> Session {
        let mut session = Session::new();
        session
            .hello(&hello(&[1]), SUPPORTED_VERSIONS, capabilities)
            .unwrap();
        session
    }

    /// WP-016 AC1, ADR-0009 R3, ADR-0019 R5: every new method is in the method list, parses,
    /// and is refused before `hello` with `hello-required`, with no effect on the session.
    #[test]
    fn adr_0019_r5_new_methods_need_hello_first() {
        let session = Session::new();
        for call in new_calls() {
            assert!(method::ALL.contains(&call.method()), "{}", call.method());
            let req = call.to_request(5).unwrap();
            assert_eq!(MethodCall::from_request(&req).unwrap(), call);
            let err = session.admit(&call).unwrap_err();
            assert_eq!(
                err.kind(),
                Some(ErrorCode::HelloRequired),
                "{}",
                call.method()
            );
            assert_eq!(session.phase(), Phase::AwaitingHello);
        }
        let session = ready_session(&[CAPABILITY_IMPORT_EXPORT]);
        for call in new_calls() {
            assert!(session.admit(&call).is_ok(), "{}", call.method());
        }
    }

    /// `item.import` and `item.export` are admitted only while `hello` advertised their
    /// capability; otherwise they get `method-not-found`, as a method that does not exist.
    #[test]
    fn import_and_export_need_their_capability() {
        let without = ready_session(&[]);
        let with = ready_session(&[CAPABILITY_IMPORT_EXPORT]);
        let transfers: Vec<MethodCall> = new_calls()
            .into_iter()
            .filter(|c| method::IMPORT_EXPORT.contains(&c.method()))
            .collect();
        assert_eq!(transfers.len(), 2);
        for call in &transfers {
            let err = without.admit(call).unwrap_err();
            assert_eq!(
                err.kind(),
                Some(ErrorCode::MethodNotFound),
                "{}",
                call.method()
            );
            assert!(
                err.message.contains(CAPABILITY_IMPORT_EXPORT),
                "{}",
                err.message
            );
            assert!(with.admit(call).is_ok());
        }
        // Other methods do not depend on the capability.
        assert!(
            without
                .admit(&MethodCall::MountResume(MountResumeParams::default()))
                .is_ok()
        );
        let result = Session::new()
            .hello(
                &hello(&[1]),
                SUPPORTED_VERSIONS,
                &[CAPABILITY_IMPORT_EXPORT],
            )
            .unwrap();
        assert_eq!(result.capabilities, [CAPABILITY_IMPORT_EXPORT]);
    }

    /// Leniency never guesses: an unknown mode, target kind or transfer mode in a request is
    /// `invalid-params`, never replaced by a default; so are missing required values and
    /// fields that do not match the target kind.
    #[test]
    fn unknown_or_mismatched_request_values_are_invalid_params() {
        let cases = [
            (method::ACCOUNT_LOGIN_START, json!({"mode": "device-code"})),
            (method::ACCOUNT_LOGIN_START, json!({"mode": "Browser"})),
            (method::ACCOUNT_LOGIN_START, json!({})),
            (method::ACCOUNT_LOGIN_START, json!({"mode": null})),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "teams-channel"}),
            ),
            (method::MOUNT_ADD_PREVIEW, json!({"account_id": "a"})),
            (method::MOUNT_ADD_PREVIEW, json!({"target": "own-drive"})),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "own-drive", "drive_id": "d"}),
            ),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "drive-id"}),
            ),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "drive-id", "sharing_link": LINK}),
            ),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "sharing-link", "sharing_link": LINK,
                       "drive_id": "d"}),
            ),
            (
                method::MOUNT_ADD_PREVIEW,
                json!({"account_id": "a", "target": "sharing-link",
                       "sharing_link": "http://example.com/x"}),
            ),
            (
                method::MOUNT_ADD_CONFIRM,
                json!({"account_id": "a", "target": "own-drive"}),
            ),
            (
                method::MOUNT_ADD_CONFIRM,
                json!({"account_id": "a", "target": "own-drive", "confirmation_token": ""}),
            ),
            (
                method::MOUNT_ADD_CONFIRM,
                json!({"account_id": "a", "target": "own-drive",
                       "confirmation_token": "c1.0.0", "mount_point": "~/OneDrive"}),
            ),
            (
                method::ITEM_IMPORT,
                json!({"source": "/a", "destination": "/m/a", "mode": "merge"}),
            ),
            (
                method::ITEM_IMPORT,
                json!({"source": "/a", "destination": "/m/a"}),
            ),
            (
                method::ITEM_EXPORT,
                json!({"source": "/m/a", "destination": "/a", "mode": "overwrite"}),
            ),
            (
                method::ITEM_EXPORT,
                json!({"source": "/m/a", "destination": "/a", "mode": ""}),
            ),
            (method::ACCOUNT_LOGOUT, json!({})),
            (
                method::ACCOUNT_LOGOUT,
                json!({"account_id": "a", "dry_run": "yes"}),
            ),
            (method::MOUNT_REMOVE, json!({"mount_id": "Not A Mount Id"})),
            (method::MOUNT_REMOVE, json!({"mount_id": "m", "dry_run": 1})),
            (method::MOUNT_PAUSE, json!({"mount_id": "UPPER"})),
            (method::MOUNT_RESUME, json!({"mount_id": ""})),
            (method::ACCOUNT_LOGIN_FINISH, json!({})),
            (
                method::ACCOUNT_LOGIN_FINISH,
                json!({"attempt_id": "Attempt_1"}),
            ),
        ];
        for (m, params) in cases {
            let err = MethodCall::parse(m, Some(&params)).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m} {params}");
            assert!(err.message.contains(m), "{}", err.message);
        }
        // Positional parameters are refused for the new methods too.
        for m in [method::MOUNT_PAUSE, method::ACCOUNT_LOGIN_FINISH] {
            let err = MethodCall::parse(m, Some(&json!(["x"]))).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m}");
        }
        // The open sets keep the unknown value on their own; only the request refuses it.
        assert_eq!(
            LoginMode::from_wire("device-code"),
            LoginMode::Other("device-code".into())
        );
        let err = MethodCall::parse(
            method::ACCOUNT_LOGIN_START,
            Some(&json!({"mode": "device-code"})),
        )
        .unwrap_err();
        assert!(err.message.contains("browser, headless"), "{}", err.message);
    }

    /// `mount.pause` takes 1 to `MAX_PAUSE_SECS` seconds, or no duration; and absent `mount_id`
    /// means every mount, for pause and resume alike.
    #[test]
    fn pause_duration_is_bounded() {
        for ok in [
            json!({}),
            json!({"duration_secs": 1}),
            json!({"duration_secs": MAX_PAUSE_SECS}),
            json!({"duration_secs": null, "mount_id": null}),
        ] {
            let call = MethodCall::parse(method::MOUNT_PAUSE, Some(&ok)).unwrap();
            let MethodCall::MountPause(p) = call else {
                panic!("not a pause");
            };
            assert_eq!(p.mount_id, None, "{ok}");
        }
        assert_eq!(MAX_PAUSE_SECS, 604_800);
        for bad in [
            json!({"duration_secs": 0}),
            json!({"duration_secs": MAX_PAUSE_SECS + 1}),
            json!({"duration_secs": -1}),
            json!({"duration_secs": 1.5}),
            json!({"duration_secs": "2h"}),
            json!({"duration_secs": u64::MAX}),
        ] {
            let err = MethodCall::parse(method::MOUNT_PAUSE, Some(&bad)).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
        }
        let call = MethodCall::parse(method::MOUNT_RESUME, None).unwrap();
        assert_eq!(call, MethodCall::MountResume(MountResumeParams::default()));
    }

    /// WP-016 AC8, ADR-0009 R8: the local side of `item.import` and `item.export` is spelled
    /// by the protocol path rule: absolute and canonical. A relative or non-canonical path is
    /// `invalid-params`, never repaired; so is the mount side.
    #[test]
    fn local_paths_are_absolute_and_canonical() {
        let bad_paths = [
            "",
            "report.pdf",
            "./report.pdf",
            "~/Downloads/report.pdf",
            "Downloads/../report.pdf",
            "/home/u/Downloads/../.ssh/id_ed25519",
            "/home/u/./Downloads/report.pdf",
            "/home/u//Downloads/report.pdf",
            "/home/u/Downloads/",
            "/home/u/Downloads/.",
            "/home/u/Downloads/..",
            "/home/u/a\0b",
        ];
        let too_long = format!("/{}", "a".repeat(crate::path::MAX_PATH_BYTES));
        for bad in bad_paths.iter().copied().chain([too_long.as_str()]) {
            assert!(LocalPath::new(bad).is_err(), "{bad:?}");
            let cases = [
                (
                    method::ITEM_IMPORT,
                    json!({"source": bad, "destination": "/m/a", "mode": "create"}),
                ),
                (
                    method::ITEM_EXPORT,
                    json!({"source": "/m/a", "destination": bad, "mode": "create"}),
                ),
                (
                    method::ITEM_IMPORT,
                    json!({"source": "/a", "destination": bad, "mode": "create"}),
                ),
                (
                    method::ITEM_EXPORT,
                    json!({"source": bad, "destination": "/a", "mode": "create"}),
                ),
            ];
            for (m, params) in cases {
                let err = MethodCall::parse(m, Some(&params)).unwrap_err();
                assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m} {bad:?}");
            }
        }
        // Names that only start with dots, spaces and non-ASCII are ordinary names, kept as
        // given.
        for ok in ["/home/u/.config/x", "/home/u/My Files/ü...txt", "/"] {
            assert_eq!(LocalPath::new(ok).unwrap().as_str(), ok);
        }
    }

    /// WP-016 AC7: `account.logout` and `mount.remove` have no parameter that could delete
    /// remote content. Logout takes exactly the ID and `dry_run`; removal takes the ID,
    /// `dry_run` and `keep_unsynced`, which only keeps local files. A field asking for a
    /// delete is an unknown field: ignored, changing nothing in the decoded request.
    #[test]
    fn logout_and_remove_have_no_delete_parameter() {
        let logout =
            MethodCall::parse(method::ACCOUNT_LOGOUT, Some(&json!({"account_id": "a"}))).unwrap();
        let remove =
            MethodCall::parse(method::MOUNT_REMOVE, Some(&json!({"mount_id": "m"}))).unwrap();
        for extra in [
            "delete_remote",
            "delete_local",
            "purge",
            "force",
            "recursive",
        ] {
            let with = MethodCall::parse(
                method::ACCOUNT_LOGOUT,
                Some(&json!({"account_id": "a", (extra): true})),
            )
            .unwrap();
            assert_eq!(with, logout, "{extra}");
            let with = MethodCall::parse(
                method::MOUNT_REMOVE,
                Some(&json!({"mount_id": "m", (extra): true})),
            )
            .unwrap();
            assert_eq!(with, remove, "{extra}");
        }
        let keys = |call: &MethodCall| {
            let mut v: Vec<String> = call
                .params()
                .unwrap()
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            v.sort();
            v
        };
        let logout = MethodCall::AccountLogout(LogoutParams {
            account_id: acct(),
            dry_run: true,
        });
        assert_eq!(keys(&logout), ["account_id", "dry_run"]);
        let remove = MethodCall::MountRemove(MountRemoveParams {
            mount_id: MountId::new("m").unwrap(),
            dry_run: true,
            keep_unsynced: true,
        });
        assert_eq!(keys(&remove), ["dry_run", "keep_unsynced", "mount_id"]);
        // A dry run is explicit; absent and null mean "do it", never something destructive.
        let MethodCall::MountRemove(p) = MethodCall::parse(
            method::MOUNT_REMOVE,
            Some(&json!({"mount_id": "m", "dry_run": null})),
        )
        .unwrap() else {
            panic!("not a remove");
        };
        assert!(!p.dry_run);
        // Both results report the unsynced count, so a client can warn first.
        let r: LogoutResult = serde_json::from_value(
            json!({"account_id": "a", "signed_out": false, "unsynced_files": 3}),
        )
        .unwrap();
        assert_eq!(r.unsynced_files, 3);
        let r: MountRemoveResult = serde_json::from_value(json!({
            "mount_id": "m", "removed": false, "mount_point": "/home/u/OneDrive",
            "kept_files": 140, "unsynced_files": 2
        }))
        .unwrap();
        assert_eq!(r.unsynced_files, 2);
    }

    /// F12 step 11, WP-016 AC7 ruling: removal refuses over unsynced changes unless the
    /// request carries `keep_unsynced: true`. Absent, `null` and `false` all mean refuse; any
    /// other value is `invalid-params`, never read as a confirmation.
    #[test]
    fn f12_step_11_removal_refuses_unsynced_changes_by_default() {
        let parse = |params: Value| match MethodCall::parse(method::MOUNT_REMOVE, Some(&params)) {
            Ok(MethodCall::MountRemove(p)) => Ok(p),
            Ok(other) => panic!("not a remove: {other:?}"),
            Err(e) => Err(e),
        };
        for refuse in [
            json!({"mount_id": "m"}),
            json!({"mount_id": "m", "keep_unsynced": null}),
            json!({"mount_id": "m", "keep_unsynced": false}),
        ] {
            assert!(!parse(refuse.clone()).unwrap().keep_unsynced, "{refuse}");
        }
        assert!(
            parse(json!({"mount_id": "m", "keep_unsynced": true}))
                .unwrap()
                .keep_unsynced
        );
        for bad in [
            json!("true"),
            json!(1),
            json!("yes"),
            json!([true]),
            json!({}),
        ] {
            let err = parse(json!({"mount_id": "m", "keep_unsynced": bad})).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
        }
        // The refusal: `refused`, reason `unsynced-changes`, the full count and a sample.
        let mount = MountId::new("example-site-documents").unwrap();
        let files: Vec<ProtocolPath> = (0..50)
            .map(|i| p(&format!("/home/u/Example/Site - Documents/f{i}.docx")))
            .collect();
        let err = RpcError::unsynced_changes(&mount, 50, files.clone());
        assert_eq!(err.kind(), Some(ErrorCode::Refused));
        assert_eq!(err.refusal_reason(), Some(RefusalReason::UnsyncedChanges));
        assert_eq!(err.unsynced_files(), Some(50));
        let named = err.unsynced_paths().unwrap();
        assert_eq!(named, files[..crate::error::MAX_UNSYNCED_SAMPLE_PATHS]);
        assert!(err.message.contains("keep_unsynced"), "{}", err.message);
        // The error survives the wire unchanged.
        let back: RpcError = serde_json::from_value(serde_json::to_value(&err).unwrap()).unwrap();
        assert_eq!(back, err);
        // Few files: all named; none offered: an empty list, with the count.
        let err = RpcError::unsynced_changes(&mount, 2, files[..2].to_vec());
        assert_eq!(err.unsynced_paths().unwrap(), files[..2]);
        let err = RpcError::unsynced_changes(&mount, 7, Vec::new());
        assert_eq!(err.unsynced_paths(), Some(Vec::new()));
        assert_eq!(err.unsynced_files(), Some(7));
    }

    /// `mount.list` tells all four combinations of blocked or not and paused or not apart
    /// (`state` shows `blocked` over `paused`, `paused` is separate), and paused with an end
    /// from paused until resumed. `paused_until` with `paused` false is refused, not guessed.
    #[test]
    fn paused_is_separate_from_state() {
        let mount = |state: &str, paused: bool, until: Option<u64>| {
            let mut v = json!({"mount_id": "m", "account_id": "a", "drive_id": "d",
                               "mount_point": "/m", "state": state, "read_only": false,
                               "paused": paused});
            if state == "blocked" {
                v["blocked_reason"] = json!("organisation-policy");
            }
            if let Some(t) = until {
                v["paused_until"] = json!(t);
            }
            v
        };
        let cases = [
            ("active", false, None),
            ("paused", true, None),
            ("paused", true, Some(1_794_790_800)),
            ("blocked", false, None),
            ("blocked", true, None),
            ("blocked", true, Some(1_794_790_800)),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (state, paused, until) in cases {
            let v = mount(state, paused, until);
            let info: MountInfo = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(serde_json::to_value(&info).unwrap(), v, "{v}");
            let blocked = info.state == MountState::Blocked;
            assert!(
                seen.insert((blocked, info.paused, info.paused_until)),
                "{v}"
            );
        }
        // Each of the four blocked/paused combinations was read distinctly.
        let combos: std::collections::BTreeSet<(bool, bool)> =
            seen.iter().map(|(b, p, _)| (*b, *p)).collect();
        assert_eq!(combos.len(), 4);
        // `paused_until` without `paused`, and a missing `paused`, do not parse.
        for bad in [
            mount("active", false, Some(1)),
            mount("blocked", false, Some(1)),
        ] {
            let err = serde_json::from_value::<MountInfo>(bad).unwrap_err();
            assert!(err.to_string().contains("paused is false"), "{err}");
        }
        let mut missing = mount("active", false, None);
        missing.as_object_mut().unwrap().remove("paused");
        assert!(serde_json::from_value::<MountInfo>(missing).is_err());
        // The end has the representation of the `mount.pause` result.
        let pause: MountPauseResult =
            serde_json::from_value(json!({"paused_mounts": 1, "paused_until": 1_794_790_800_u64}))
                .unwrap();
        let info: MountInfo =
            serde_json::from_value(mount("paused", true, Some(1_794_790_800))).unwrap();
        assert_eq!(pause.paused_until, info.paused_until);
    }

    /// A test challenge: 43 base64url characters, as an S256 digest is.
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn auth_url(extra: &str) -> String {
        format!(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=x&\
             response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53211&state=st4te&\
             code_challenge={CHALLENGE}&code_challenge_method=S256{extra}"
        )
    }

    /// ADR-0010 R1, R6: the authorisation URL must carry an S256 challenge (with `plain` the
    /// challenge would be the verifier) and no parameter named like a credential.
    #[test]
    fn adr_0010_r1_authorization_url_carries_s256_and_no_credential() {
        assert!(AuthorizationUrl::new(auth_url("")).is_ok());
        assert!(AuthorizationUrl::new(auth_url("&prompt=select_account")).is_ok());
        // The simulator's authority, on loopback over plain HTTP.
        for host in ["127.0.0.1:4100", "localhost:4100", "[::1]:4100"] {
            let url = auth_url("").replace(
                "https://login.microsoftonline.com",
                &format!("http://{host}"),
            );
            assert!(AuthorizationUrl::new(url).is_ok(), "{host}");
        }
        let bad = [
            auth_url("").replace("code_challenge_method=S256", "code_challenge_method=plain"),
            auth_url("").replace("&code_challenge_method=S256", ""),
            auth_url("").replace(&format!("code_challenge={CHALLENGE}&"), ""),
            auth_url("").replace(CHALLENGE, "short"),
            auth_url("").replace(CHALLENGE, &format!("{CHALLENGE}A")),
            auth_url("&code_verifier=dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            auth_url("&CODE_VERIFIER=x"),
            auth_url("&code=M.C507_BAY.2.U.planted-code"),
            auth_url("&client_secret=s"),
            auth_url("&access_token=a"),
            auth_url("&refresh_token=r"),
            auth_url("&id_token=i"),
            auth_url("&token=t"),
            auth_url("&password=p"),
            auth_url("&code%5Fverifier=x"),
            auth_url("&=x"),
            auth_url("&state=again"),
            auth_url("#code=x"),
            auth_url(" "),
            auth_url("&login_hint=\u{e9}"),
            auth_url("").replace("https://", "http://"),
            auth_url("").replace("https://", "https://user:pass@"),
            auth_url("").replace("https://", "ftp://"),
            "https://".into(),
            "https:///authorize?code_challenge_method=S256".into(),
            auth_url(&"&x=y".repeat(MAX_AUTHORIZATION_URL_BYTES / 4)),
            String::new(),
        ];
        for url in bad {
            let err = AuthorizationUrl::new(url.clone()).unwrap_err();
            assert!(!err.to_string().contains(&url) || url.is_empty(), "{err}");
            let as_json = serde_json::to_value(&url).unwrap();
            assert!(
                serde_json::from_value::<AuthorizationUrl>(as_json).is_err(),
                "{url}"
            );
        }
    }

    /// ADR-0010 R6: an attempt ID and a confirmation token are too short, and in too small an
    /// alphabet, to carry a PKCE verifier (43 to 128 characters), a code or a token.
    #[test]
    fn adr_0010_r6_ids_cannot_carry_a_verifier() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let lower_verifier = "dbjftjez4cvp-mb92k27uhbuju1p1r-ww1gfwfoejxk";
        assert_eq!(verifier.len(), 43);
        for secret in [verifier, lower_verifier] {
            assert!(AttemptId::new(secret).is_err());
            assert!(ConfirmationToken::new(secret).is_err());
        }
        assert!(AttemptId::new("a".repeat(MAX_ATTEMPT_ID_BYTES)).is_ok());
        assert!(AttemptId::new("a".repeat(MAX_ATTEMPT_ID_BYTES + 1)).is_err());
        for bad in ["", "A", "a_b", "a.b", "a b", "é"] {
            assert!(AttemptId::new(bad).is_err(), "{bad:?}");
        }
        assert!(ConfirmationToken::new("c1.0123456789abcdef.0123456789abcdef").is_ok());
        for bad in ["", "C1.00", "c1-00", "c1.00.00 ", "c1.00/00"] {
            assert!(ConfirmationToken::new(bad).is_err(), "{bad:?}");
        }
    }

    /// The pasted address and the sharing link never show in `Debug` output, so a log of a
    /// decoded request cannot leak them; their errors do not repeat them either.
    #[test]
    fn credentials_are_redacted_in_debug_and_errors() {
        let pasted = "https://login.example/nativeclient?code=PLANTED-CODE&state=s";
        let call = MethodCall::AccountLoginFinish(LoginFinishParams {
            attempt_id: AttemptId::new("a1").unwrap(),
            redirect_url: Some(PastedRedirect::new(pasted).unwrap()),
        });
        let debug = format!("{call:?}");
        assert!(!debug.contains("PLANTED-CODE"), "{debug}");
        assert!(debug.contains("<redacted>"));
        let call = MethodCall::MountAddPreview(MountAddPreviewParams {
            target: link_target(),
        });
        let debug = format!("{call:?}");
        assert!(!debug.contains("planted-link"), "{debug}");
        for bad in [
            format!("{pasted} "),
            format!("{pasted}\n"),
            "x".repeat(MAX_PASTED_URL_BYTES + 1),
        ] {
            let err = PastedRedirect::new(bad.clone()).unwrap_err();
            assert!(!err.to_string().contains("PLANTED"), "{err}");
        }
        for bad in [
            format!("http://{}", &LINK[8..]),
            format!("{LINK} x"),
            "https://".into(),
            "https:///x".into(),
            format!("{LINK}{}", "x".repeat(MAX_SHARING_LINK_BYTES)),
        ] {
            let err = SharingLink::new(bad).unwrap_err();
            assert!(!err.to_string().contains("planted"), "{err}");
        }
        assert!(SharingLink::new(LINK.replace("https", "HTTPS")).is_ok());
    }

    /// WP-005 AC5: an unknown method gets JSON-RPC "method not found".
    #[test]
    fn unknown_method_is_method_not_found() {
        for m in ["item.delete", "activity.list", "", "HELLO"] {
            let err = MethodCall::parse(m, None).unwrap_err();
            assert_eq!(err.code, -32601, "{m}");
            assert_eq!(err.kind(), Some(ErrorCode::MethodNotFound));
        }
    }

    /// WP-005 AC5: unknown parameter fields are ignored.
    #[test]
    fn unknown_param_fields_are_ignored() {
        let call = MethodCall::parse(
            method::ITEM_PIN,
            Some(&json!({"paths": ["/home/u/OneDrive/a"], "recursive": true})),
        )
        .unwrap();
        assert_eq!(call, MethodCall::ItemPin(paths(&["/home/u/OneDrive/a"])));
        let call = MethodCall::parse(method::DAEMON_STATUS, Some(&json!({"verbose": 1}))).unwrap();
        assert_eq!(call, MethodCall::DaemonStatus);
    }

    /// WP-005 AC6 and ADR-0009 R8: a relative path anywhere in the parameters is rejected
    /// with invalid-params.
    #[test]
    fn relative_paths_are_invalid_params() {
        let cases = [
            (
                method::ITEM_STATUS,
                json!({"paths": ["/ok", "OneDrive/a.txt"]}),
            ),
            (method::ITEM_STATUS, json!({"directory": "OneDrive"})),
            (method::ITEM_PIN, json!({"paths": ["a.txt"]})),
            (method::ITEM_UNPIN, json!({"paths": ["./a"]})),
            (method::ITEM_HYDRATE, json!({"paths": [""]})),
            (method::ITEM_DEHYDRATE, json!({"paths": ["~/OneDrive/a"]})),
            (method::STATUS_SUBSCRIBE, json!({"paths": ["OneDrive"]})),
            (
                method::ITEM_PIN,
                json!({"paths": ["/home/u/OneDrive/../.local/share/x"]}),
            ),
        ];
        for (m, params) in cases {
            let err = MethodCall::parse(m, Some(&params)).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m} {params}");
            assert!(err.message.contains(m), "{}", err.message);
        }
    }

    #[test]
    fn malformed_params_are_invalid_params() {
        let cases = [
            (method::HELLO, None),
            (
                method::HELLO,
                Some(json!({"client_name": "x", "protocol_versions": [-1]})),
            ),
            (method::ITEM_STATUS, Some(json!({}))),
            (method::ITEM_STATUS, Some(json!({"cursor": "0"}))),
            (
                method::ITEM_STATUS,
                Some(json!({"paths": ["/a"], "directory": "/a"})),
            ),
            (method::ITEM_PIN, Some(json!(["/a"]))),
            (method::ITEM_PIN, Some(json!({"paths": "/a"}))),
        ];
        for (m, params) in cases {
            let err = MethodCall::parse(m, params.as_ref()).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m} {params:?}");
        }
    }

    #[test]
    fn calls_round_trip_through_requests() {
        let calls = [
            MethodCall::Hello(hello(&[1])),
            MethodCall::DaemonStatus,
            MethodCall::AccountList(ListParams::default()),
            MethodCall::MountList(ListParams {
                cursor: Some("m1.6d31".into()),
            }),
            MethodCall::ItemStatus(ItemStatusParams {
                target: ItemStatusTarget::Paths(vec![p("/a"), p("/b")]),
                cursor: None,
            }),
            MethodCall::ItemStatus(ItemStatusParams {
                target: ItemStatusTarget::Directory(p("/a")),
                cursor: Some("17".into()),
            }),
            MethodCall::ItemPin(paths(&["/a"])),
            MethodCall::ItemUnpin(paths(&["/a"])),
            MethodCall::ItemHydrate(paths(&["/a"])),
            MethodCall::ItemDehydrate(paths(&["/a"])),
            MethodCall::StatusSubscribe(StatusSubscribeParams::default()),
        ];
        let calls = calls.into_iter().chain(new_calls());
        let mut seen = Vec::new();
        for call in calls {
            let req = call.to_request(1).unwrap();
            assert_eq!(MethodCall::from_request(&req).unwrap(), call);
            seen.push(call.method());
        }
        seen.dedup();
        assert_eq!(seen, method::ALL);
    }

    /// ADR-0009 R4: no version in common gets an error listing the daemon's versions.
    #[test]
    fn hello_with_no_common_version_lists_supported_versions() {
        assert_eq!(negotiate(&[1, 2], &[2, 1, 7]), Ok(2));
        assert_eq!(negotiate(SUPPORTED_VERSIONS, &[1]), Ok(PROTOCOL_VERSION));
        let err = negotiate(SUPPORTED_VERSIONS, &[2, 3]).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::UnsupportedVersion));
        assert_eq!(err.supported_versions(), Some(vec![1]));
        let mut session = Session::new();
        let err = session
            .hello(&hello(&[]), SUPPORTED_VERSIONS, &[])
            .unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::UnsupportedVersion));
        assert_eq!(session.phase(), Phase::AwaitingHello);
    }

    /// ADR-0009 R3 and ADR-0019 R5: a request other than hello before hello gets
    /// `hello-required` (never `not-ready`) and has no effect.
    #[test]
    fn requests_before_hello_are_refused_without_effect() {
        let mut session = Session::new();
        let calls = [
            MethodCall::DaemonStatus,
            MethodCall::ItemPin(paths(&["/a"])),
            MethodCall::StatusSubscribe(StatusSubscribeParams::default()),
        ];
        for call in &calls {
            let err = session.admit(call).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::HelloRequired));
            assert_eq!(err.code, 1010);
            assert_eq!(session.phase(), Phase::AwaitingHello);
        }
        // A subscribe recorded before hello is ignored too.
        session.subscribed();
        assert_eq!(session.phase(), Phase::AwaitingHello);

        let result = session
            .hello(&hello(&[1]), SUPPORTED_VERSIONS, &[])
            .unwrap();
        assert_eq!(result.protocol_version, 1);
        assert_eq!(session.phase(), Phase::Ready(1));
        for call in &calls {
            assert!(session.admit(call).is_ok());
        }
        let err = session
            .hello(&hello(&[1]), SUPPORTED_VERSIONS, &[])
            .unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidRequest));
    }

    /// Overview "Change stream": a subscribed connection carries notifications only.
    #[test]
    fn subscribed_connection_refuses_requests() {
        let mut session = Session::new();
        session
            .hello(&hello(&[1]), SUPPORTED_VERSIONS, &[])
            .unwrap();
        session.subscribed();
        assert_eq!(session.phase(), Phase::Subscribed(1));
        let err = session
            .admit(&MethodCall::MountList(ListParams::default()))
            .unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidRequest));
    }

    /// WP-005 AC4: item state uses the core axes, effective pin and display status with the
    /// core wire strings, and the status comes from core's derivation.
    #[test]
    fn item_info_uses_core_state_and_wire_forms() {
        for &hydration in Hydration::ALL {
            for &pin in Pin::ALL {
                for &sync in SyncState::ALL {
                    for &inherited in EffectivePin::ALL {
                        let state = ItemState {
                            hydration,
                            pin,
                            sync,
                        };
                        let modifiers = Modifiers {
                            read_only: true,
                            shortcut: false,
                        };
                        let info = ItemInfo::new(state, inherited, modifiers);
                        let effective = effective_pin(
                            pin,
                            [match inherited {
                                EffectivePin::Pinned => Pin::Pinned,
                                EffectivePin::Unpinned => Pin::Unpinned,
                            }],
                        );
                        assert_eq!(info.status, display_status(&state, effective));
                        assert_eq!(info.effective_pin, effective.into());
                        assert_eq!(info.state(), Some(state));
                        let v = serde_json::to_value(&info).unwrap();
                        assert_eq!(v["status"], info.status.as_str());
                        assert_eq!(v["hydration"], hydration.as_str());
                        assert_eq!(v["pin"], pin.as_str());
                        assert_eq!(v["effective_pin"], effective.as_str());
                        assert_eq!(v["sync"], sync.as_str());
                        assert_eq!(v["modifiers"], json!(["read-only"]));
                        let back: ItemInfo = serde_json::from_value(v).unwrap();
                        assert_eq!(back, info);
                    }
                }
            }
        }
    }

    fn info_json() -> Value {
        json!({
            "status": "local", "modifiers": [], "hydration": "full", "pin": "inherit",
            "effective_pin": "unpinned", "sync": "in-sync"
        })
    }

    /// ADR-0009 R5: clients ignore unknown result fields and capabilities.
    #[test]
    fn results_with_extra_fields_still_decode() {
        let hello: HelloResult = serde_json::from_value(json!({
            "protocol_version": 1, "capabilities": ["future-thing"], "motd": "hi",
            "limits": {"max_line_bytes": 1, "max_action_paths": 1, "max_action_path_bytes": 1,
                       "max_status_paths": 1, "max_status_path_bytes": 1, "max_new_thing": 3}
        }))
        .unwrap();
        assert_eq!(hello.capabilities, ["future-thing"]);
        let mut item = info_json();
        item["size_on_disk"] = json!(12);
        let status: ItemStatusResult = serde_json::from_value(json!({
            "entries": [{"path": "/home/u/OneDrive/a.txt", "item": item, "etag": "x"}],
            "extra": null
        }))
        .unwrap();
        assert_eq!(status.entries.len(), 1);
    }

    /// ADR-0019 R4: open sets inside a response accept unknown values without failing it;
    /// closed sets reject them.
    #[test]
    fn open_values_in_responses_do_not_fail_them() {
        let mounts: MountListResult = serde_json::from_value(json!({"mounts": [{
            "mount_id": "m1", "account_id": "a", "drive_id": "d",
            "mount_point": "/home/u/OneDrive", "state": "hibernating",
            "blocked_reason": "solar-flare", "read_only": false, "paused": false
        }]}))
        .unwrap();
        assert_eq!(
            mounts.mounts[0].state,
            MountState::Other("hibernating".into())
        );
        assert_eq!(
            mounts.mounts[0].blocked_reason,
            Some(BlockedReason::Other("solar-flare".into()))
        );
        let accounts: AccountListResult = serde_json::from_value(json!({"accounts": [{
            "account_id": "a", "kind": "guest", "state": "locked"
        }]}))
        .unwrap();
        assert_eq!(
            accounts.accounts[0].kind,
            AccountKind::Other("guest".into())
        );
        let mut item = info_json();
        item["modifiers"] = json!(["shortcut", "encrypted"]);
        item["reason"] = json!("cosmic-ray");
        let info: ItemInfo = serde_json::from_value(item.clone()).unwrap();
        assert_eq!(
            info.modifiers.0,
            [Modifier::Shortcut, Modifier::Other("encrypted".into())]
        );
        assert_eq!(serde_json::to_value(&info).unwrap(), item);

        let mut item = info_json();
        item["status"] = json!("sparkly");
        assert!(
            serde_json::from_value::<ItemInfo>(item).is_err(),
            "the closed display status accepted an unknown value"
        );
    }

    /// ADR-0020 R7: an unknown axis value is kept and does not fail the response; the
    /// display status still rejects an unknown value.
    #[test]
    fn unknown_axis_values_are_kept() {
        for (field, value) in [
            ("hydration", "half"),
            ("pin", "excluded"),
            ("effective_pin", "excluded"),
            ("sync", "paused-upload"),
        ] {
            let mut item = info_json();
            item[field] = json!(value);
            let result: ItemStatusResult = serde_json::from_value(
                json!({"entries": [{"path": "/home/u/OneDrive/a", "item": item.clone()}]}),
            )
            .unwrap_or_else(|e| panic!("unknown {field} failed the response: {e}"));
            let info = result.entries[0].item().unwrap();
            assert_eq!(serde_json::to_value(info).unwrap(), item);
            if field != "effective_pin" {
                assert_eq!(info.state(), None, "{field}");
            }
        }
    }

    /// ADR-0020 R5: `hello` returns the limits, and an action request over a limit is
    /// refused with invalid-params before anything is done.
    #[test]
    fn hello_returns_limits_and_oversize_actions_are_refused() {
        let mut session = Session::new();
        let result = session
            .hello(&hello(&[1]), SUPPORTED_VERSIONS, &[])
            .unwrap();
        assert_eq!(
            serde_json::to_value(result.limits).unwrap(),
            json!({"max_line_bytes": 1_048_576, "max_action_paths": 100,
                   "max_action_path_bytes": 524_288, "max_status_paths": 10_000,
                   "max_status_path_bytes": 524_288})
        );
        let at_limit: Vec<String> = (0..100).map(|i| format!("/m/f{i}")).collect();
        for m in [
            method::ITEM_PIN,
            method::ITEM_UNPIN,
            method::ITEM_HYDRATE,
            method::ITEM_DEHYDRATE,
        ] {
            assert!(MethodCall::parse(m, Some(&json!({"paths": at_limit}))).is_ok());
            let mut over = at_limit.clone();
            over.push("/m/one-more".into());
            let err = MethodCall::parse(m, Some(&json!({"paths": over}))).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{m}");
            assert!(err.message.contains("limit is 100"), "{}", err.message);
        }
        // 100 paths of 4,096 bytes fit the byte limit; paths full of quotes do not.
        let long: Vec<String> = (0..100)
            .map(|i| format!("/{i:03}{}", "a".repeat(4092)))
            .collect();
        assert!(MethodCall::parse(method::ITEM_PIN, Some(&json!({"paths": long}))).is_ok());
        let quoted: Vec<String> = (0..100)
            .map(|i| format!("/{i:03}{}", "\"".repeat(4092)))
            .collect();
        let err = MethodCall::parse(method::ITEM_PIN, Some(&json!({"paths": quoted}))).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
    }

    /// Exactly one of `item` and `error` per status entry.
    #[test]
    fn status_entry_needs_exactly_one_outcome() {
        let path = "/home/u/OneDrive/a";
        let error = json!({"code": 1002, "message": "gone"});
        let ok = json!({"path": path, "item": info_json()});
        assert!(serde_json::from_value::<ItemStatusEntry>(ok).is_ok());
        let ok = json!({"path": path, "error": error});
        assert!(serde_json::from_value::<ItemStatusEntry>(ok).is_ok());
        for bad in [
            json!({"path": path}),
            json!({"path": path, "item": info_json(), "error": error}),
        ] {
            let err = serde_json::from_value::<ItemStatusEntry>(bad).unwrap_err();
            assert!(err.to_string().contains("exactly one"), "{err}");
        }
    }

    #[test]
    fn item_reason_is_a_code_with_a_message() {
        let state = ItemState {
            hydration: Hydration::Full,
            pin: Pin::Inherit,
            sync: SyncState::Error,
        };
        let info = ItemInfo::new(state, EffectivePin::Unpinned, Modifiers::default())
            .with_reason(ItemReason::StorageFull, "OneDrive is full");
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["status"], "error");
        assert_eq!(v["reason"], "storage-full");
        assert_eq!(v["message"], "OneDrive is full");
    }

    #[test]
    fn action_entry_optional_fields_are_omitted_when_empty() {
        let entry = ItemActionEntry::accepted(p("/a"));
        assert_eq!(serde_json::to_value(&entry).unwrap(), json!({"path": "/a"}));
        let entry =
            ItemActionEntry::refused(p("/a"), RpcError::refused(&RefusalReason::InUse, "open"));
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            json!({"path": "/a", "error": {"code": 1004, "message": "open",
                   "data": {"reason": "in-use"}}})
        );
    }

    /// ADR-0020 R4: an `item.status` batch is limited so that the same request plus the
    /// longest cursor still fits in one line; the probe that once broke a cursor loop (74,893
    /// paths) is now refused up front.
    #[test]
    fn status_batches_are_limited_so_the_cursor_request_fits() {
        let at_limit: Vec<String> = (0..MAX_STATUS_PATHS).map(|i| format!("/m/{i}")).collect();
        let call =
            MethodCall::parse(method::ITEM_STATUS, Some(&json!({"paths": at_limit}))).unwrap();
        assert!(matches!(call, MethodCall::ItemStatus(_)));
        let probe: Vec<String> = (0..74_893).map(|i| format!("/m/{i}")).collect();
        let err =
            MethodCall::parse(method::ITEM_STATUS, Some(&json!({"paths": probe}))).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));

        // The heaviest batch the byte limit admits: 128 paths of 4,096 quote characters
        // would exceed it, so fill exactly up to the limit with plain characters instead.
        let per = 4_096;
        let count = usize::try_from(MAX_STATUS_PATH_BYTES).unwrap() / (per + 2);
        let heavy: Vec<String> = (0..count)
            .map(|i| format!("/{i:04}{}", "a".repeat(per - 5)))
            .collect();
        let params =
            json!({"paths": heavy, "cursor": format!("p1.{}.{}", "f".repeat(32), u64::MAX)});
        let call = MethodCall::parse(method::ITEM_STATUS, Some(&params)).unwrap();
        let worst_id = Id::String("\u{1}".repeat(crate::envelope::MAX_ID_BYTES));
        let line = crate::codec::encode_line(&call.to_request(worst_id).unwrap())
            .expect("the request with the longest cursor and ID fits one line");
        assert!(line.len() - 1 <= crate::codec::MAX_LINE_BYTES);
        let mut over = heavy.clone();
        over.push(format!("/x{}", "a".repeat(per - 2)));
        let err =
            MethodCall::parse(method::ITEM_STATUS, Some(&json!({"paths": over}))).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
    }

    /// ADR-0020 R5: the `item.status` path-count limit holds on its own. 10,001 one-byte
    /// paths take 30,003 bytes, far under the byte limit, so only the count can refuse them.
    #[test]
    fn adr_0020_r5_status_path_count_limit_alone() {
        let at_limit = vec!["/"; MAX_STATUS_PATHS as usize];
        let over = vec!["/"; MAX_STATUS_PATHS as usize + 1];
        let over_paths: Vec<ProtocolPath> = over.iter().map(|s| p(s)).collect();
        assert!(
            encoded_path_bytes(&over_paths) < MAX_STATUS_PATH_BYTES / 10,
            "the batch must be well within the byte limit"
        );
        let call =
            MethodCall::parse(method::ITEM_STATUS, Some(&json!({"paths": at_limit}))).unwrap();
        assert!(matches!(call, MethodCall::ItemStatus(_)));
        let err =
            MethodCall::parse(method::ITEM_STATUS, Some(&json!({"paths": over}))).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
        assert!(
            err.message
                .contains("10001 paths given, the limit is 10000"),
            "refused by the count, not the bytes: {}",
            err.message
        );
    }

    /// A `null` optional field means the same as an absent one, everywhere.
    #[test]
    fn null_optional_fields_mean_absent() {
        let sub: StatusSubscribeParams = serde_json::from_value(json!({"paths": null})).unwrap();
        assert!(sub.paths.is_empty());
        let changed: StatusChangedParams =
            serde_json::from_value(json!({"entries": null, "refresh": null, "seq": null})).unwrap();
        assert_eq!(changed, StatusChangedParams::default());
        let entry: ItemActionEntry = serde_json::from_value(
            json!({"path": "/a", "skipped": null, "skipped_count": null, "bytes_freed": null}),
        )
        .unwrap();
        assert_eq!(entry, ItemActionEntry::accepted(p("/a")));
        let status: ItemStatusParams =
            serde_json::from_value(json!({"directory": "/a", "paths": null, "cursor": null}))
                .unwrap();
        assert_eq!(status.cursor, None);
        let list: ListParams = serde_json::from_value(json!({"cursor": null})).unwrap();
        assert_eq!(list, ListParams::default());
        let mut item = info_json();
        item["reason"] = Value::Null;
        item["message"] = Value::Null;
        assert!(serde_json::from_value::<ItemInfo>(item).is_ok());
        let mount: MountInfo = serde_json::from_value(json!({
            "mount_id": "m", "account_id": "a", "drive_id": "d", "mount_point": "/m",
            "state": "active", "blocked_reason": null, "blocked_message": null,
            "read_only": false, "paused": false
        }))
        .unwrap();
        assert_eq!(mount.blocked_message, None);
    }

    /// ADR-0020 R6: an entry with `error` has no `skipped`, and `skipped` never holds more
    /// than `skipped_count`.
    #[test]
    fn action_entry_shape_is_enforced() {
        let error = json!({"code": 1004, "message": "no"});
        let skipped = json!([{"path": "/a/b", "error": error}]);
        let ok = json!({"path": "/a", "skipped": skipped, "skipped_count": 7000});
        let entry: ItemActionEntry = serde_json::from_value(ok.clone()).unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), ok);
        let ok = json!({"path": "/a", "skipped_count": 7000});
        assert!(serde_json::from_value::<ItemActionEntry>(ok).is_ok());
        for bad in [
            json!({"path": "/a", "error": error, "skipped": skipped}),
            json!({"path": "/a", "error": error, "skipped_count": 1}),
            json!({"path": "/a", "error": error, "bytes_freed": 1}),
            json!({"path": "/a", "skipped": skipped}),
            json!({"path": "/a", "skipped": skipped, "skipped_count": 0}),
        ] {
            assert!(
                serde_json::from_value::<ItemActionEntry>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }
}
