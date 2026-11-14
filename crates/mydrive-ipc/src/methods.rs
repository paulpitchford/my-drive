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
    AccountKind, AccountState, BlockedReason, ItemReason, MountState, WireEffectivePin,
    WireHydration, WireModifiers, WirePin, WireSync,
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
    ];

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Decodes action parameters and enforces [`Limits::V1`] (ADR-0020 R5): a request over a
/// limit is refused as a whole, before anything is done.
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
    /// subscribed connection.
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
    /// on success, moves to [`Phase::Ready`].
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
            "blocked_reason": "solar-flare", "read_only": false
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
            "read_only": false
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
