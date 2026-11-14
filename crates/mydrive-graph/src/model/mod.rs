//! Graph response types: drive, drive item and delta page.
//!
//! Responses are read field by field from parsed JSON, accepting unknown fields and
//! treating everything Graph may omit as optional: no cTag on business drives, no hashes
//! on some files, no name or size on deleted items, no `parentReference.path` in delta,
//! and the root item's `parentReference` without an `id`. At this boundary drive IDs are
//! normalised through [`DriveId`], item IDs are kept exactly as received, timestamps are
//! truncated to whole seconds (ADR-0011 R5) and quickXorHash is decoded. Only
//! `quickXorHash` is read from `hashes`; `sha1Hash`, `crc32Hash` and `sha256Hash` are
//! ignored (ADR-0011 R4).
//!
//! One bad item never fails a delta page, so it cannot stall a mount's delta forever:
//!
//! - A quickXorHash or timestamp that cannot be read is left `None`, and the item carries
//!   an [`ItemWarning`] saying so. A missing hash must never be read as "unchanged".
//! - An item that has an ID but whose shape cannot be used (for example an empty parent
//!   ID, or a field of the wrong type) becomes a [`DeltaEntry::Problem`] in its place in
//!   the feed, with its ID, whether it carried the `deleted` facet, and the reason.
//! - Ambiguity about what an item *is* is never resolved silently: a facet (`deleted`,
//!   `file`, `folder`, `package`, `root`, `shared`, `remoteItem`) present with a value
//!   that is not an object (`null` included), more than one of `file`, `folder` and
//!   `package`, any key repeated in the item's object, or a blank parent ID, makes the
//!   entry a problem, so the engine refetches the item.
//! - Only a page that is not JSON, lacks its next or delta link (or has an empty or
//!   repeated one), or has an entry without an unambiguous, non-blank ID fails as a
//!   whole. Duplicated keys are detected while parsing, not resolved by "last wins".
//!
//! Every reason and message here names a field and a fixed problem; none repeats a value
//! from the response, so nothing the server sends (names, URLs, tokens) reaches an error
//! or a log line through this module.
//!
//! `@microsoft.graph.downloadUrl` is never read: the client downloads through `/content`
//! (ADR-0011 R10), and a pre-authenticated URL kept in a value could leak through `Debug`.

mod time;

use std::fmt;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

pub use time::{Timestamp, TimestampError};

/// The kind of a drive, from `driveType`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DriveType {
    /// A consumer OneDrive (`personal`).
    Personal,
    /// A OneDrive for Business (`business`).
    Business,
    /// A SharePoint document library (`documentLibrary`).
    DocumentLibrary,
    /// A value this client does not know, kept as received.
    Other(String),
}

impl DriveType {
    fn from_wire(value: &str) -> Self {
        match value {
            "personal" => Self::Personal,
            "business" => Self::Business,
            "documentLibrary" => Self::DocumentLibrary,
            _ => Self::Other(value.to_owned()),
        }
    }
}

/// A drive's storage quota, as far as Graph reports it. The values are signed: Graph
/// documents them as `Int64`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quota {
    /// Total space in bytes.
    pub total: Option<i64>,
    /// Space used in bytes.
    pub used: Option<i64>,
    /// Space remaining in bytes.
    pub remaining: Option<i64>,
    /// Space used by items in the recycle bin, in bytes.
    pub deleted: Option<i64>,
    /// Graph's quota state, for example `normal` or `exceeded`.
    pub state: Option<String>,
}

/// A drive resource, from `GET /drives/{id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// The drive ID, lowercased (ADR-0007 R1).
    pub id: DriveId,
    /// The drive type, if reported.
    pub drive_type: Option<DriveType>,
    /// The drive's display name, if reported.
    pub name: Option<String>,
    /// The quota, if reported as an object. A quota field that cannot be read is `None`
    /// rather than failing the whole drive.
    pub quota: Option<Quota>,
}

/// Where an item sits: its drive and parent folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentReference {
    /// The drive the parent is in, lowercased (ADR-0007 R1).
    pub drive_id: Option<DriveId>,
    /// The type of that drive.
    pub drive_type: Option<DriveType>,
    /// The parent folder's item ID. Absent on the root item; its absence on any other
    /// item does **not** make that item the root.
    pub id: Option<ItemId>,
    /// The parent's percent-encoded path. Delta never includes it, and it must never be
    /// used to identify an item (ADR-0011 R1); it is informational only.
    pub path: Option<String>,
}

/// What an item is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    /// A file (the `file` facet).
    File(FileFacet),
    /// A folder (the `folder` facet).
    Folder(FolderFacet),
    /// A package (the `package` facet), for example a OneNote notebook. Graph treats it
    /// as one item although it may look like a folder; it is neither a plain file nor a
    /// plain folder.
    Package(PackageFacet),
    /// None of those facets was present, for example a deleted item whose facets Graph
    /// omitted, or a shortcut described only by `remoteItem`. Never materialise it as an
    /// empty file or an empty folder.
    Unknown,
}

/// The `file` facet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFacet {
    /// The MIME type Graph reports.
    pub mime_type: Option<String>,
    /// The quickXorHash, the only hash guaranteed on both account types (ADR-0011 R4).
    /// Absent when Graph does not report one (for example on some empty files) or when it
    /// could not be read (then the item has an [`ItemWarning`]); absence must never be
    /// read as "unchanged".
    pub quick_xor_hash: Option<QuickXorHash>,
}

/// The `folder` facet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderFacet {
    /// The number of direct children, if reported.
    pub child_count: Option<u64>,
}

/// The `package` facet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageFacet {
    /// The package type, for example `oneNote`.
    pub package_type: Option<String>,
}

/// The `fileSystemInfo` facet: the times the client that wrote the item reported, which
/// can differ from the service's `createdDateTime` and `lastModifiedDateTime`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSystemInfo {
    /// `fileSystemInfo.createdDateTime`, whole seconds.
    pub created: Option<Timestamp>,
    /// `fileSystemInfo.lastModifiedDateTime`, whole seconds.
    pub last_modified: Option<Timestamp>,
}

/// The `remoteItem` facet: an item that lives in another drive, such as the target of a
/// shortcut (ADR-0015).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteItem {
    /// The remote item's ID, exactly as received.
    pub id: ItemId,
    /// The remote item's name, if reported.
    pub name: Option<String>,
    /// The remote item's size, if reported.
    pub size: Option<u64>,
    /// File, folder, package or unknown.
    pub kind: ItemKind,
    /// The remote item's drive and parent.
    pub parent: Option<ParentReference>,
}

/// A field of an item that was present but could not be read, and was left `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemWarning {
    /// The JSON field, for example `file.hashes.quickXorHash`.
    pub field: &'static str,
    /// Why it was not used.
    pub problem: &'static str,
}

impl fmt::Display for ItemWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.field, self.problem)
    }
}

/// A drive item, from get-item or a delta page.
///
/// Content equality is decided by [`etag`](Self::etag), [`size`](Self::size) and the
/// quickXorHash (ADR-0011 R4). [`ctag`](Self::ctag) is kept because the store records it,
/// but it is absent on business delta and must not be used for that decision.
///
/// What the engine must not assume from what is missing:
///
/// - An [`ItemKind::Unknown`] item is never materialised as an empty file (or folder).
/// - A non-root item without a parent ID (`parent` absent or its `id` absent) is not the
///   root and is not in the root; only [`root`](Self::root) marks the root.
/// - A file without a [`size`](Self::size) has an unknown size, not size 0: fetch it with
///   get-item before downloading, since a download needs the expected size.
/// - A missing hash or timestamp (see [`warnings`](Self::warnings)) is unknown, not
///   unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveItem {
    /// The item ID, exactly as received: never case-folded or otherwise changed.
    pub id: ItemId,
    /// The name. Business delta omits it on deleted items.
    pub name: Option<String>,
    /// The eTag, opaque.
    pub etag: Option<String>,
    /// The cTag, opaque. Absent on business delta and on folders.
    pub ctag: Option<String>,
    /// The size in bytes. Personal delta omits it on deleted items.
    pub size: Option<u64>,
    /// `createdDateTime`, whole seconds.
    pub created: Option<Timestamp>,
    /// `lastModifiedDateTime`, whole seconds.
    pub last_modified: Option<Timestamp>,
    /// The `fileSystemInfo` facet, if present.
    pub file_system_info: Option<FileSystemInfo>,
    /// The parent reference.
    pub parent: Option<ParentReference>,
    /// File, folder, package or unknown.
    pub kind: ItemKind,
    /// Whether the item carries the `deleted` facet.
    pub deleted: bool,
    /// Whether the item carries the `root` facet: it is the drive's root folder.
    pub root: bool,
    /// Whether the item carries the `shared` facet.
    pub shared: bool,
    /// The `remoteItem` facet, if present.
    pub remote_item: Option<RemoteItem>,
    /// Fields that were present but unreadable, and are `None` above.
    pub warnings: Vec<ItemWarning>,
}

impl DriveItem {
    /// The quickXorHash, if this is a file and Graph reported a readable one.
    #[must_use]
    pub fn quick_xor_hash(&self) -> Option<&QuickXorHash> {
        match &self.kind {
            ItemKind::File(file) => file.quick_xor_hash.as_ref(),
            _ => None,
        }
    }

    /// Whether the item has the `folder` facet.
    #[must_use]
    pub fn is_folder(&self) -> bool {
        matches!(self.kind, ItemKind::Folder(_))
    }

    /// Whether the item has the `file` facet.
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self.kind, ItemKind::File(_))
    }
}

/// An entry of a delta page that has an ID but could not be turned into a [`DriveItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemProblem {
    /// The item's ID, exactly as received.
    pub id: ItemId,
    /// Whether the entry had a `deleted` key, whatever its value: a delete may have been
    /// reported even though the rest of the entry is unusable. Refetch the item.
    pub deleted: bool,
    /// Why it could not be used: a field name and a fixed problem, never a value.
    pub reason: String,
}

/// One entry of a delta page, in feed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaEntry {
    /// A usable item.
    Item(Box<DriveItem>),
    /// An entry with an ID that could not be used.
    Problem(ItemProblem),
}

impl DeltaEntry {
    /// The entry's item ID.
    #[must_use]
    pub fn id(&self) -> &ItemId {
        match self {
            Self::Item(item) => &item.id,
            Self::Problem(problem) => &problem.id,
        }
    }
}

/// An opaque delta link: an `@odata.nextLink` or `@odata.deltaLink` exactly as Graph sent
/// it, or a `Location` restart link from a `410 Gone`.
///
/// It can be stored and handed back to
/// [`GraphClient::delta_from_link`](crate::client::GraphClient::delta_from_link) later.
/// The client sends the bearer token with it only if it points at the client's own Graph
/// base URL. A delta link carries a change token, not a credential.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeltaLink(String);

impl DeltaLink {
    /// Wraps a link, for example one read back from the store.
    #[must_use]
    pub fn new(link: impl Into<String>) -> Self {
        Self(link.into())
    }

    /// The link as received.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a delta page ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaContinuation {
    /// `@odata.nextLink`: more pages of this round follow; fetch this link next.
    NextLink(DeltaLink),
    /// `@odata.deltaLink`: this round is complete; save the link and use it for the next
    /// round of changes.
    DeltaLink(DeltaLink),
}

/// One page of a delta feed.
///
/// `entries` keeps every entry in the order Graph sent it, repeats included, usable or
/// not: the same item may appear more than once, and only its last occurrence may be
/// applied (ADR-0011 R2), which needs the whole round, not one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaPage {
    /// The entries, in feed order.
    pub entries: Vec<DeltaEntry>,
    /// The next link or the delta link.
    pub continuation: DeltaContinuation,
}

impl DeltaPage {
    /// The usable items, in feed order.
    pub fn items(&self) -> impl Iterator<Item = &DriveItem> {
        self.entries.iter().filter_map(|e| match e {
            DeltaEntry::Item(item) => Some(item.as_ref()),
            DeltaEntry::Problem(_) => None,
        })
    }

    /// The unusable entries, in feed order.
    pub fn problems(&self) -> impl Iterator<Item = &ItemProblem> {
        self.entries.iter().filter_map(|e| match e {
            DeltaEntry::Problem(problem) => Some(problem),
            DeltaEntry::Item(_) => None,
        })
    }
}

/// Why a response body could not be turned into a model value. Messages name fields and
/// fixed problems only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The body is not JSON of the expected shape.
    #[error("the body is not the expected JSON: {0}")]
    Shape(&'static str),
    /// An identifier is missing, empty or not a string.
    #[error("the {field} is missing, empty or not a string")]
    MissingId {
        /// Which field.
        field: &'static str,
    },
    /// An item has an ID but cannot be used.
    #[error("item {id} cannot be used: {reason}")]
    Item {
        /// The item.
        id: ItemId,
        /// Why: a field name and a fixed problem.
        reason: String,
    },
    /// A delta page carries neither a next link nor a delta link, or both, or a link
    /// that is not a string.
    #[error("the delta page carries {0}")]
    Continuation(&'static str),
}

// ---- strict JSON tree -------------------------------------------------------------------

/// A parsed JSON value that keeps every key of an object, duplicates included, so a
/// duplicated key is detected instead of silently resolved (serde_json keeps the last).
enum J {
    Null,
    Bool(bool),
    Num(serde_json::Number),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

struct JVisitor;

impl<'de> Visitor<'de> for JVisitor {
    type Value = J;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<J, E> {
        Ok(J::Bool(v))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<J, E> {
        Ok(J::Num(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<J, E> {
        Ok(J::Num(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<J, E> {
        serde_json::Number::from_f64(v)
            .map(J::Num)
            .ok_or_else(|| E::custom("a number that is not finite"))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<J, E> {
        Ok(J::Str(v.to_owned()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<J, E> {
        Ok(J::Str(v))
    }
    fn visit_unit<E: de::Error>(self) -> Result<J, E> {
        Ok(J::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<J, E> {
        Ok(J::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<J, D::Error> {
        J::deserialize(d)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<J, A::Error> {
        let mut out = Vec::new();
        while let Some(element) = seq.next_element()? {
            out.push(element);
        }
        Ok(J::Arr(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<J, A::Error> {
        let mut out = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, J>()? {
            out.push((key, value));
        }
        Ok(J::Obj(out))
    }
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(JVisitor)
    }
}

/// Keys whose duplication is reported by name; any other duplicated key is reported as
/// "a key", since key names are server strings.
const NAMED_KEYS: &[&str] = &[
    "id",
    "name",
    "size",
    "eTag",
    "cTag",
    "deleted",
    "file",
    "folder",
    "package",
    "root",
    "shared",
    "remoteItem",
    "parentReference",
    "driveId",
    "hashes",
    "quickXorHash",
    "createdDateTime",
    "lastModifiedDateTime",
    "fileSystemInfo",
];

impl J {
    /// The first key that appears twice in an object anywhere in this value.
    fn duplicate_key(&self) -> Option<&'static str> {
        match self {
            Self::Arr(items) => items.iter().find_map(Self::duplicate_key),
            Self::Obj(pairs) => {
                for (i, (key, _)) in pairs.iter().enumerate() {
                    if pairs[..i].iter().any(|(k, _)| k == key) {
                        return Some(
                            NAMED_KEYS
                                .iter()
                                .find(|named| **named == key)
                                .copied()
                                .unwrap_or("a key"),
                        );
                    }
                }
                pairs.iter().find_map(|(_, v)| v.duplicate_key())
            }
            _ => None,
        }
    }

    /// The value in `serde_json` form. Only called once duplicates are ruled out or do not
    /// matter.
    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(b),
            Self::Num(n) => Value::Number(n),
            Self::Str(s) => Value::String(s),
            Self::Arr(items) => Value::Array(items.into_iter().map(Self::into_value).collect()),
            Self::Obj(pairs) => Value::Object(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k, v.into_value()))
                    .collect(),
            ),
        }
    }
}

fn parse_json(body: &[u8]) -> Result<J, ModelError> {
    serde_json::from_slice(body).map_err(|_| ModelError::Shape("the body is not valid JSON"))
}

/// The one string value of `key` in an object's pairs: `None` if absent, an error if it
/// appears with different values, is not a string, or is blank.
fn unique_id(pairs: &[(String, J)], key: &str) -> Result<Option<String>, ()> {
    let mut found: Option<&str> = None;
    for (k, v) in pairs {
        if k != key {
            continue;
        }
        let J::Str(s) = v else { return Err(()) };
        if s.trim().is_empty() || found.is_some_and(|f| f != s) {
            return Err(());
        }
        found = Some(s);
    }
    Ok(found.map(str::to_owned))
}

// ---- field readers ----------------------------------------------------------------------

type Obj = Map<String, Value>;

/// A field's value, treating JSON `null` as absent. Not for facets: see [`facet`].
fn get<'a>(obj: &'a Obj, key: &str) -> Option<&'a Value> {
    obj.get(key).filter(|v| !v.is_null())
}

fn opt_str(obj: &Obj, key: &str, path: &'static str) -> Result<Option<String>, String> {
    match get(obj, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("{path} is not a string")),
    }
}

fn opt_u64(obj: &Obj, key: &str, path: &'static str) -> Result<Option<u64>, String> {
    match get(obj, key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{path} is not a non-negative integer")),
    }
}

fn opt_obj<'a>(obj: &'a Obj, key: &str, path: &'static str) -> Result<Option<&'a Obj>, String> {
    match get(obj, key) {
        None => Ok(None),
        Some(Value::Object(o)) => Ok(Some(o)),
        Some(_) => Err(format!("{path} is not an object")),
    }
}

/// A facet: absent, or an object. Present with any other value, `null` included, it is
/// ambiguous what the item is, so the item is unusable.
fn facet<'a>(obj: &'a Obj, key: &str, path: &'static str) -> Result<Option<&'a Obj>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::Object(o)) => Ok(Some(o)),
        Some(_) => Err(format!("{path} is present but not an object")),
    }
}

/// An identifier field: absent, or a string that is not blank.
fn opt_id(obj: &Obj, key: &str, path: &'static str) -> Result<Option<String>, String> {
    match opt_str(obj, key, path)? {
        Some(s) if s.trim().is_empty() => Err(format!("{path} is empty or blank")),
        other => Ok(other),
    }
}

/// Collects warnings while one item is converted.
struct Warnings(Vec<ItemWarning>);

impl Warnings {
    fn timestamp(&mut self, obj: &Obj, key: &str, field: &'static str) -> Option<Timestamp> {
        let parsed = match get(obj, key)? {
            Value::String(text) => Timestamp::parse_rfc3339(text).ok(),
            _ => None,
        };
        if parsed.is_none() {
            self.0.push(ItemWarning {
                field,
                problem: "is not an RFC 3339 date-time",
            });
        }
        parsed
    }

    fn hash(&mut self, obj: &Obj, field: &'static str) -> Option<QuickXorHash> {
        let parsed = match get(obj, "quickXorHash")? {
            Value::String(text) => QuickXorHash::from_base64(text).ok(),
            _ => None,
        };
        if parsed.is_none() {
            self.0.push(ItemWarning {
                field,
                problem: "is not 20 bytes of base64",
            });
        }
        parsed
    }
}

/// The JSON paths used for a parent reference, which differ under `remoteItem`.
struct ParentPaths {
    drive_id: &'static str,
    drive_type: &'static str,
    id: &'static str,
    path: &'static str,
}

const ITEM_PARENT: ParentPaths = ParentPaths {
    drive_id: "parentReference.driveId",
    drive_type: "parentReference.driveType",
    id: "parentReference.id",
    path: "parentReference.path",
};

const REMOTE_PARENT: ParentPaths = ParentPaths {
    drive_id: "remoteItem.parentReference.driveId",
    drive_type: "remoteItem.parentReference.driveType",
    id: "remoteItem.parentReference.id",
    path: "remoteItem.parentReference.path",
};

fn parent(obj: &Obj, paths: &ParentPaths) -> Result<ParentReference, String> {
    let drive_id = opt_id(obj, "driveId", paths.drive_id)?
        .map(|d| DriveId::new(d).map_err(|_| format!("{} is empty", paths.drive_id)))
        .transpose()?;
    let id = opt_id(obj, "id", paths.id)?
        .map(|i| ItemId::new(i).map_err(|_| format!("{} is empty", paths.id)))
        .transpose()?;
    Ok(ParentReference {
        drive_id,
        drive_type: opt_str(obj, "driveType", paths.drive_type)?
            .as_deref()
            .map(DriveType::from_wire),
        id,
        path: opt_str(obj, "path", paths.path)?,
    })
}

/// The JSON paths used for facets, which differ under `remoteItem`.
struct KindPaths {
    file: &'static str,
    mime: &'static str,
    hashes: &'static str,
    hash: &'static str,
    folder: &'static str,
    child_count: &'static str,
    package: &'static str,
    package_type: &'static str,
    several: &'static str,
}

const ITEM_KIND: KindPaths = KindPaths {
    file: "file",
    mime: "file.mimeType",
    hashes: "file.hashes",
    hash: "file.hashes.quickXorHash",
    folder: "folder",
    child_count: "folder.childCount",
    package: "package",
    package_type: "package.type",
    several: "more than one of file, folder and package is present",
};

const REMOTE_KIND: KindPaths = KindPaths {
    file: "remoteItem.file",
    mime: "remoteItem.file.mimeType",
    hashes: "remoteItem.file.hashes",
    hash: "remoteItem.file.hashes.quickXorHash",
    folder: "remoteItem.folder",
    child_count: "remoteItem.folder.childCount",
    package: "remoteItem.package",
    package_type: "remoteItem.package.type",
    several: "more than one of remoteItem.file, remoteItem.folder and remoteItem.package is present",
};

fn kind(obj: &Obj, paths: &KindPaths, w: &mut Warnings) -> Result<ItemKind, String> {
    let package = facet(obj, "package", paths.package)?;
    let folder = facet(obj, "folder", paths.folder)?;
    let file = facet(obj, "file", paths.file)?;
    let present = [package.is_some(), folder.is_some(), file.is_some()];
    if present.iter().filter(|p| **p).count() > 1 {
        return Err(paths.several.to_owned());
    }
    if let Some(package) = package {
        return Ok(ItemKind::Package(PackageFacet {
            package_type: opt_str(package, "type", paths.package_type)?,
        }));
    }
    if let Some(folder) = folder {
        return Ok(ItemKind::Folder(FolderFacet {
            child_count: opt_u64(folder, "childCount", paths.child_count)?,
        }));
    }
    let Some(file) = file else {
        return Ok(ItemKind::Unknown);
    };
    let quick_xor_hash = match opt_obj(file, "hashes", paths.hashes)? {
        Some(hashes) => w.hash(hashes, paths.hash),
        None => None,
    };
    Ok(ItemKind::File(FileFacet {
        mime_type: opt_str(file, "mimeType", paths.mime)?,
        quick_xor_hash,
    }))
}

fn remote(obj: &Obj, w: &mut Warnings) -> Result<RemoteItem, String> {
    let id =
        opt_id(obj, "id", "remoteItem.id")?.ok_or_else(|| "remoteItem.id is missing".to_owned())?;
    let id = ItemId::new(id).map_err(|_| "remoteItem.id is empty".to_owned())?;
    Ok(RemoteItem {
        kind: kind(obj, &REMOTE_KIND, w)?,
        name: opt_str(obj, "name", "remoteItem.name")?,
        size: opt_u64(obj, "size", "remoteItem.size")?,
        parent: opt_obj(obj, "parentReference", "remoteItem.parentReference")?
            .map(|p| parent(p, &REMOTE_PARENT))
            .transpose()?,
        id,
    })
}

fn item_body(obj: &Obj, id: ItemId) -> Result<DriveItem, String> {
    let mut w = Warnings(Vec::new());
    let deleted = facet(obj, "deleted", "deleted")?.is_some();
    let root = facet(obj, "root", "root")?.is_some();
    let shared = facet(obj, "shared", "shared")?.is_some();
    let remote_item = facet(obj, "remoteItem", "remoteItem")?
        .map(|r| remote(r, &mut w))
        .transpose()?;
    let kind = kind(obj, &ITEM_KIND, &mut w)?;
    let created = w.timestamp(obj, "createdDateTime", "createdDateTime");
    let last_modified = w.timestamp(obj, "lastModifiedDateTime", "lastModifiedDateTime");
    let file_system_info =
        opt_obj(obj, "fileSystemInfo", "fileSystemInfo")?.map(|fs| FileSystemInfo {
            created: w.timestamp(fs, "createdDateTime", "fileSystemInfo.createdDateTime"),
            last_modified: w.timestamp(
                fs,
                "lastModifiedDateTime",
                "fileSystemInfo.lastModifiedDateTime",
            ),
        });
    let parent = opt_obj(obj, "parentReference", "parentReference")?
        .map(|p| parent(p, &ITEM_PARENT))
        .transpose()?;
    Ok(DriveItem {
        id,
        name: opt_str(obj, "name", "name")?,
        etag: opt_str(obj, "eTag", "eTag")?,
        ctag: opt_str(obj, "cTag", "cTag")?,
        size: opt_u64(obj, "size", "size")?,
        created,
        last_modified,
        file_system_info,
        parent,
        kind,
        deleted,
        root,
        shared,
        remote_item,
        warnings: w.0,
    })
}

/// Why one item could not be parsed.
enum ItemFailure {
    /// No unambiguous, usable ID: the page cannot be trusted.
    NoId,
    /// An ID, but the rest cannot be used.
    Problem(ItemProblem),
}

/// Parses one item. Its ID is read first, so every later failure can name it. The ID
/// must be a non-blank string, and if `id` is repeated every occurrence must agree.
fn parse_item(j: J) -> Result<DriveItem, ItemFailure> {
    let J::Obj(pairs) = j else {
        return Err(ItemFailure::NoId);
    };
    let id = unique_id(&pairs, "id")
        .ok()
        .flatten()
        .and_then(|raw| ItemId::new(raw).ok())
        .ok_or(ItemFailure::NoId)?;
    let deleted_key = pairs.iter().any(|(k, _)| k == "deleted");
    let problem = |reason: String| {
        ItemFailure::Problem(ItemProblem {
            id: id.clone(),
            deleted: deleted_key,
            reason,
        })
    };
    let whole = J::Obj(pairs);
    if let Some(key) = whole.duplicate_key() {
        return Err(problem(format!("{key} appears more than once")));
    }
    let value = whole.into_value();
    let Value::Object(obj) = &value else {
        return Err(ItemFailure::NoId);
    };
    item_body(obj, id.clone()).map_err(problem)
}

impl Drive {
    /// Parses a drive resource.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not a JSON object or its ID is unusable (missing,
    /// blank, or repeated with different values). Other fields of the wrong type are left
    /// `None`.
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        let J::Obj(pairs) = parse_json(body)? else {
            return Err(ModelError::Shape("the drive is not an object"));
        };
        let missing = ModelError::MissingId { field: "drive id" };
        let id = unique_id(&pairs, "id")
            .ok()
            .flatten()
            .and_then(|raw| DriveId::new(raw).ok())
            .ok_or(missing)?;
        let value = J::Obj(pairs).into_value();
        let Value::Object(obj) = &value else {
            return Err(ModelError::Shape("the drive is not an object"));
        };
        let text = |key| get(obj, key).and_then(Value::as_str);
        let quota = get(obj, "quota").and_then(Value::as_object).map(|q| {
            let num = |key| get(q, key).and_then(Value::as_i64);
            Quota {
                total: num("total"),
                used: num("used"),
                remaining: num("remaining"),
                deleted: num("deleted"),
                state: get(q, "state").and_then(Value::as_str).map(str::to_owned),
            }
        });
        Ok(Self {
            id,
            drive_type: text("driveType").map(DriveType::from_wire),
            name: text("name").map(str::to_owned),
            quota,
        })
    }
}

impl DriveItem {
    /// Parses a drive item resource.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not JSON, has no usable ID, or the item cannot be
    /// used (including an ambiguous shape). Unreadable hashes and timestamps are
    /// warnings, not errors.
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        parse_item(parse_json(body)?).map_err(|failure| match failure {
            ItemFailure::NoId => ModelError::MissingId { field: "item id" },
            ItemFailure::Problem(p) => ModelError::Item {
                id: p.id,
                reason: p.reason,
            },
        })
    }
}

/// A page-level field: exactly once at most.
fn single<'a>(pairs: &'a [(String, J)], key: &str) -> Result<Option<&'a J>, ModelError> {
    let mut found = None;
    for (k, v) in pairs {
        if k == key {
            if found.is_some() {
                return Err(ModelError::Shape("a page-level key appears more than once"));
            }
            found = Some(v);
        }
    }
    Ok(found)
}

fn link(pairs: &[(String, J)], key: &str) -> Result<Option<DeltaLink>, ModelError> {
    match single(pairs, key)? {
        None | Some(J::Null) => Ok(None),
        Some(J::Str(s)) if s.trim().is_empty() => Err(ModelError::Continuation("an empty link")),
        Some(J::Str(s)) => Ok(Some(DeltaLink(s.clone()))),
        Some(_) => Err(ModelError::Continuation("a link that is not a string")),
    }
}

impl DeltaPage {
    /// Parses a delta page.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not a delta page, an entry has no usable ID (missing,
    /// blank, or repeated with different values), a page-level key is repeated, or the
    /// page does not carry exactly one non-empty `@odata.nextLink` or `@odata.deltaLink`.
    /// Other problems with single entries become [`DeltaEntry::Problem`].
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        let J::Obj(mut pairs) = parse_json(body)? else {
            return Err(ModelError::Shape("the delta page is not an object"));
        };
        let continuation = match (
            link(&pairs, "@odata.nextLink")?,
            link(&pairs, "@odata.deltaLink")?,
        ) {
            (Some(next), None) => DeltaContinuation::NextLink(next),
            (None, Some(delta)) => DeltaContinuation::DeltaLink(delta),
            (None, None) => {
                return Err(ModelError::Continuation(
                    "neither @odata.nextLink nor @odata.deltaLink",
                ));
            }
            (Some(_), Some(_)) => {
                return Err(ModelError::Continuation(
                    "both @odata.nextLink and @odata.deltaLink",
                ));
            }
        };
        let not_array = ModelError::Shape("the delta page's value is not an array");
        if !matches!(single(&pairs, "value")?, Some(J::Arr(_))) {
            return Err(not_array);
        }
        let values = pairs
            .iter_mut()
            .find(|(k, _)| k == "value")
            .map(|(_, v)| std::mem::replace(v, J::Null));
        let Some(J::Arr(values)) = values else {
            return Err(not_array);
        };
        let mut entries = Vec::with_capacity(values.len());
        for value in values {
            entries.push(match parse_item(value) {
                Ok(item) => DeltaEntry::Item(Box::new(item)),
                Err(ItemFailure::Problem(problem)) => DeltaEntry::Problem(problem),
                Err(ItemFailure::NoId) => {
                    return Err(ModelError::MissingId {
                        field: "id of a delta entry",
                    });
                }
            });
        }
        Ok(Self {
            entries,
            continuation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "aGVsbG8gd29ybGQgMTIzNDU2Nzg=";

    fn item(json: &str) -> DriveItem {
        DriveItem::from_json(json.as_bytes()).unwrap()
    }

    /// AC9: unknown fields are ignored and every optional field may be absent.
    #[test]
    fn ac9_minimal_item_and_unknown_fields_parse() {
        let minimal = item(r#"{"id":"01ABC"}"#);
        assert_eq!(minimal.id.as_str(), "01ABC");
        assert_eq!(minimal.kind, ItemKind::Unknown);
        assert!(!minimal.deleted && !minimal.root && !minimal.shared);
        assert!(minimal.parent.is_none() && minimal.etag.is_none() && minimal.size.is_none());
        assert!(minimal.warnings.is_empty() && minimal.file_system_info.is_none());

        let extra = item(
            r#"{"id":"x","futureFacet":{"a":[1,2]},"webUrl":"https://example.invalid/x",
                "createdBy":{"user":{"displayName":"Someone"}},"file":{"newThing":true},
                "parentReference":{"siteId":"s","driveId":"D","unknown":1},"name":null}"#,
        );
        assert!(extra.is_file());
        assert_eq!(extra.quick_xor_hash(), None);
        assert_eq!(extra.name, None, "null is absent");
        assert_eq!(
            extra.parent.unwrap().drive_id.unwrap().as_str(),
            "d",
            "drive IDs go through DriveId"
        );
    }

    /// A `parentReference` with no `driveId` parses, with the drive ID absent.
    #[test]
    fn parent_reference_without_drive_id_parses() {
        let parsed = item(r#"{"id":"x","parentReference":{"id":"p","path":"/drive/root:"}}"#);
        let parent = parsed.parent.unwrap();
        assert_eq!(parent.drive_id, None);
        assert_eq!(parent.id.unwrap().as_str(), "p");
        let empty = item(r#"{"id":"x","parentReference":{}}"#);
        assert_eq!(empty.parent.unwrap(), ParentReference::default());
    }

    /// AC9: item IDs are kept exactly as received, drive IDs are lowercased (ADR-0007 R1).
    #[test]
    fn ac9_item_ids_exact_drive_ids_normalised() {
        let parsed = item(
            r#"{"id":"ABC123!105","parentReference":{"driveId":"ABC123","id":"ABC123!Ab"},
                "remoteItem":{"id":"F00D!7","parentReference":{"driveId":"F00D"}}}"#,
        );
        assert_eq!(parsed.id.as_str(), "ABC123!105");
        let parent = parsed.parent.unwrap();
        assert_eq!(parent.id.unwrap().as_str(), "ABC123!Ab");
        assert_eq!(parent.drive_id.unwrap().as_str(), "abc123");
        let remote = parsed.remote_item.unwrap();
        assert_eq!(remote.id.as_str(), "F00D!7");
        assert_eq!(remote.parent.unwrap().drive_id.unwrap().as_str(), "f00d");

        let drive = Drive::from_json(br#"{"id":"B!MixedCase","driveType":"business"}"#).unwrap();
        assert_eq!(drive.id.as_str(), "b!mixedcase");
        assert_eq!(drive.drive_type, Some(DriveType::Business));
        assert!(drive.quota.is_none());
    }

    /// The `deleted` facet, the root item and a `remoteItem` shortcut target.
    #[test]
    fn deleted_root_and_remote_item_facets() {
        let business_delete =
            item(r#"{"id":"01X","size":0,"deleted":{},"parentReference":{"driveId":"b!x"}}"#);
        assert!(business_delete.deleted);
        assert!(business_delete.name.is_none());
        let personal_delete =
            item(r#"{"id":"A!1","name":"n","deleted":{"state":"deleted"},"folder":{}}"#);
        assert!(personal_delete.deleted && personal_delete.is_folder());
        assert!(personal_delete.size.is_none());

        let root = item(
            r#"{"id":"A!101","root":{},"folder":{"childCount":3},"parentReference":{"driveId":"A"}}"#,
        );
        assert!(root.root);
        assert_eq!(
            root.kind,
            ItemKind::Folder(FolderFacet {
                child_count: Some(3)
            })
        );
        assert!(root.parent.unwrap().id.is_none());

        let shortcut = item(
            r#"{"id":"A!9","name":"Team","remoteItem":{"id":"01R","name":"Team","size":10,
                "folder":{"childCount":1},"parentReference":{"driveId":"b!Z","driveType":"documentLibrary"}}}"#,
        );
        assert_eq!(shortcut.kind, ItemKind::Unknown);
        let remote = shortcut.remote_item.unwrap();
        assert!(matches!(remote.kind, ItemKind::Folder(_)));
        assert_eq!(
            remote.parent.unwrap().drive_type,
            Some(DriveType::DocumentLibrary)
        );
    }

    /// Ambiguity about what an item is never resolves silently: each of these entries
    /// becomes a problem carrying its ID and whether a `deleted` key was present, so the
    /// engine refetches the item.
    #[test]
    fn ambiguous_items_become_problems() {
        for (entry, deleted_key) in [
            (r#"{"id":"x","deleted":null}"#, true),
            (r#"{"id":"x","deleted":{},"deleted":null}"#, true),
            (r#"{"id":"x","deleted":5}"#, true),
            (r#"{"id":"x","id":"x","file":{}}"#, false),
            (r#"{"id":"x","file":{},"folder":{}}"#, false),
            (r#"{"id":"x","package":{},"folder":{}}"#, false),
            (r#"{"id":"x","file":null}"#, false),
            (r#"{"id":"x","folder":[3]}"#, false),
            (r#"{"id":"x","file":[null,null]}"#, false),
            (r#"{"id":"x","package":"oneNote"}"#, false),
            (r#"{"id":"x","root":5}"#, false),
            (r#"{"id":"x","shared":true}"#, false),
            (r#"{"id":"x","remoteItem":null}"#, false),
            (
                r#"{"id":"x","remoteItem":{"id":"r","file":{},"folder":{}}}"#,
                false,
            ),
            (r#"{"id":"x","remoteItem":{"id":" "}}"#, false),
            (r#"{"id":"x","parentReference":{"id":" "}}"#, false),
            (r#"{"id":"x","parentReference":{"driveId":"  "}}"#, false),
            (r#"{"id":"x","size":1,"size":2}"#, false),
            (r#"{"id":"x","name":"a","name":"b"}"#, false),
            (r#"{"id":"x","parentReference":{"id":"a","id":"b"}}"#, false),
            (r#"{"id":"x","unknownKey":1,"unknownKey":2}"#, false),
        ] {
            let body = format!(r#"{{"value":[{entry}],"@odata.deltaLink":"d"}}"#);
            let page = DeltaPage::from_json(body.as_bytes()).unwrap();
            let problems: Vec<_> = page.problems().collect();
            assert_eq!(problems.len(), 1, "{entry}");
            assert_eq!(problems[0].id.as_str(), "x", "{entry}");
            assert_eq!(problems[0].deleted, deleted_key, "{entry}");
            assert!(DriveItem::from_json(entry.as_bytes()).is_err(), "{entry}");
        }
        let dup = DeltaPage::from_json(
            br#"{"value":[{"id":"x","unknownKey":1,"unknownKey":2}],"@odata.deltaLink":"d"}"#,
        )
        .unwrap();
        assert_eq!(
            dup.problems().next().unwrap().reason,
            "a key appears more than once",
            "an unknown key's name is not echoed"
        );
    }

    /// The page fails as a whole when an entry's ID is unusable or ambiguous, or a link is
    /// empty or repeated.
    #[test]
    fn unusable_ids_and_links_fail_the_page() {
        for body in [
            r#"{"value":[{"id":" "}],"@odata.deltaLink":"d"}"#,
            r#"{"value":[{"id":""}],"@odata.deltaLink":"d"}"#,
            r#"{"value":[{"id":"a","id":"b"}],"@odata.deltaLink":"d"}"#,
            r#"{"value":[{"id":"a","id":7}],"@odata.deltaLink":"d"}"#,
            r#"{"value":[],"@odata.deltaLink":""}"#,
            r#"{"value":[],"@odata.nextLink":"  "}"#,
            r#"{"value":[],"@odata.deltaLink":"d","@odata.deltaLink":"e"}"#,
            r#"{"value":[],"value":[],"@odata.deltaLink":"d"}"#,
        ] {
            assert!(DeltaPage::from_json(body.as_bytes()).is_err(), "{body}");
        }
        assert!(DriveItem::from_json(br#"{"id":"a","id":"b"}"#).is_err());
        assert!(Drive::from_json(br#"{"id":"a","id":"b"}"#).is_err());
        assert!(Drive::from_json(br#"{"id":"  "}"#).is_err());
    }

    /// The `package` facet (OneNote notebooks) is its own kind.
    #[test]
    fn package_facet_is_its_own_kind() {
        let notebook = item(r#"{"id":"A!5","package":{"type":"oneNote"}}"#);
        assert_eq!(
            notebook.kind,
            ItemKind::Package(PackageFacet {
                package_type: Some("oneNote".to_owned())
            })
        );
        assert!(!notebook.is_folder() && !notebook.is_file());
    }

    /// `fileSystemInfo` times are kept separately from the service times.
    #[test]
    fn file_system_info_times_are_separate() {
        let parsed = item(
            r#"{"id":"x","createdDateTime":"2020-01-01T00:00:00Z",
                "fileSystemInfo":{"createdDateTime":"2019-01-01T00:00:00.5Z",
                "lastModifiedDateTime":"2019-06-01T00:00:00Z"}}"#,
        );
        let fs = parsed.file_system_info.unwrap();
        assert_eq!(fs.created.unwrap().to_string(), "2019-01-01T00:00:00Z");
        assert_eq!(
            fs.last_modified.unwrap().to_string(),
            "2019-06-01T00:00:00Z"
        );
        assert_eq!(parsed.created.unwrap().to_string(), "2020-01-01T00:00:00Z");
        assert_eq!(parsed.last_modified, None);
    }

    /// ADR-0011 R4: only quickXorHash is read; sha1, crc32 and sha256 hashes are ignored,
    /// and the model has nowhere to put them. An unreadable quickXorHash is `None` with a
    /// warning, never a made-up value.
    #[test]
    fn adr_0011_r4_only_quick_xor_hash_is_read() {
        let only_others = item(
            r#"{"id":"x","file":{"hashes":{"sha1Hash":"AA","sha256Hash":"BB","crc32Hash":"CC"}}}"#,
        );
        assert_eq!(only_others.quick_xor_hash(), None);
        assert!(only_others.warnings.is_empty());
        let both = item(&format!(
            r#"{{"id":"x","file":{{"hashes":{{"sha256Hash":"BB","quickXorHash":"{HASH}"}}}}}}"#
        ));
        assert_eq!(both.quick_xor_hash().unwrap().to_base64(), HASH);
        for bad in [r#""!!""#, "7"] {
            let parsed = item(&format!(
                r#"{{"id":"x","file":{{"hashes":{{"quickXorHash":{bad}}}}}}}"#
            ));
            assert!(parsed.is_file());
            assert_eq!(parsed.quick_xor_hash(), None);
            assert_eq!(parsed.warnings.len(), 1);
            assert_eq!(parsed.warnings[0].field, "file.hashes.quickXorHash");
        }
    }

    /// ADR-0011 R5: timestamps are whole seconds when they leave the model; an
    /// unreadable one is `None` with a warning.
    #[test]
    fn adr_0011_r5_item_timestamps_are_whole_seconds() {
        let parsed = item(
            r#"{"id":"x","createdDateTime":"2016-03-21T20:01:37.987Z",
                "lastModifiedDateTime":"2016-03-21T20:01:38Z"}"#,
        );
        assert_eq!(parsed.created.unwrap().as_unix_seconds(), 1_458_590_497);
        assert_eq!(
            parsed.last_modified.unwrap().as_unix_seconds(),
            1_458_590_498
        );
        let bad = item(r#"{"id":"x","lastModifiedDateTime":"yesterday","createdDateTime":5}"#);
        assert_eq!(bad.last_modified, None);
        assert_eq!(bad.created, None);
        assert_eq!(bad.warnings.len(), 2);
    }

    #[test]
    fn items_without_a_readable_id_are_rejected() {
        for body in [r#"{"id":""}"#, r#"{"name":"no id"}"#, r#"{"id":7}"#, "[]"] {
            assert!(DriveItem::from_json(body.as_bytes()).is_err(), "{body}");
        }
        assert!(Drive::from_json(br#"{"id":""}"#).is_err());
        assert!(matches!(
            DriveItem::from_json(br#"{"id":"x","parentReference":{"id":""}}"#),
            Err(ModelError::Item { .. })
        ));
    }

    /// Quota values are signed, and a quota that cannot be read is left out rather than
    /// failing the drive.
    #[test]
    fn quota_is_signed_and_optional() {
        let drive =
            Drive::from_json(br#"{"id":"d","quota":{"total":10,"remaining":-5,"used":15}}"#)
                .unwrap();
        let quota = drive.quota.unwrap();
        assert_eq!(quota.remaining, Some(-5));
        assert_eq!(quota.used, Some(15));
        let odd = Drive::from_json(br#"{"id":"d","quota":{"total":"lots"}}"#).unwrap();
        assert_eq!(odd.quota.unwrap().total, None);
        let odd = Drive::from_json(br#"{"id":"d","quota":7,"name":3}"#).unwrap();
        assert!(odd.quota.is_none() && odd.name.is_none());
    }

    #[test]
    fn delta_page_needs_exactly_one_link() {
        let next = DeltaPage::from_json(br#"{"value":[],"@odata.nextLink":"n"}"#).unwrap();
        assert_eq!(
            next.continuation,
            DeltaContinuation::NextLink(DeltaLink::new("n"))
        );
        let done = DeltaPage::from_json(
            br#"{"@odata.context":"c","value":[{"id":"a"},{"id":"a"}],"@odata.deltaLink":"d"}"#,
        )
        .unwrap();
        assert_eq!(done.entries.len(), 2, "repeats are kept for ADR-0011 R2");
        for bad in [
            &br#"{"value":[]}"#[..],
            br#"{"value":[],"@odata.nextLink":"n","@odata.deltaLink":"d"}"#,
            br#"{"value":[],"@odata.deltaLink":5}"#,
            br#"{"value":{},"@odata.deltaLink":"d"}"#,
            b"not json",
        ] {
            assert!(DeltaPage::from_json(bad).is_err());
        }
    }

    /// One bad item does not fail the page: unreadable hashes and timestamps are
    /// warnings, an unusable entry with an ID is a problem in its place in the feed (with
    /// its `deleted` facet noted), and only an entry without a readable ID fails the page.
    #[test]
    fn one_bad_item_does_not_fail_the_page() {
        let page = DeltaPage::from_json(
            br#"{"value":[
                {"id":"good"},
                {"id":"badhash","file":{"hashes":{"quickXorHash":"nope"}},"lastModifiedDateTime":"x"},
                {"id":"noparent","parentReference":{"id":""},"deleted":{}},
                {"id":"wrongtype","size":"big"},
                {"id":"good"},
                {"id":"good2","remoteItem":{"id":""}}
            ],"@odata.deltaLink":"d"}"#,
        )
        .unwrap();
        let order: Vec<_> = page
            .entries
            .iter()
            .map(|e| (e.id().as_str(), matches!(e, DeltaEntry::Item(_))))
            .collect();
        assert_eq!(
            order,
            [
                ("good", true),
                ("badhash", true),
                ("noparent", false),
                ("wrongtype", false),
                ("good", true),
                ("good2", false)
            ]
        );
        assert_eq!(page.items().nth(1).unwrap().warnings.len(), 2);
        let problems: Vec<_> = page
            .problems()
            .map(|p| (p.id.as_str(), p.deleted))
            .collect();
        assert_eq!(
            problems,
            [("noparent", true), ("wrongtype", false), ("good2", false)]
        );
        assert!(page.problems().all(|p| !p.reason.is_empty()));

        assert!(matches!(
            DeltaPage::from_json(br#"{"value":[{"id":"a"},{"name":"x"}],"@odata.deltaLink":"d"}"#),
            Err(ModelError::MissingId { .. })
        ));
    }

    /// Reasons and messages name the field and never repeat a value from the response.
    #[test]
    fn reasons_never_echo_values() {
        let secret = "https://h.invalid/x?access_token=SECRET\nfake log line";
        let json = serde_json::json!({
            "value": [
                {"id": "a", "size": secret},
                {"id": "b", "name": {"x": secret}},
                {"id": "c", "parentReference": secret},
                {"id": "d", "remoteItem": {"id": secret.len()}},
                {"id": "e", "file": {"mimeType": [secret]}},
            ],
            "@odata.deltaLink": "d"
        });
        let page = DeltaPage::from_json(json.to_string().as_bytes()).unwrap();
        assert_eq!(page.problems().count(), 5);
        let reasons: Vec<_> = page.problems().map(|p| p.reason.clone()).collect();
        assert_eq!(reasons[0], "size is not a non-negative integer");
        for reason in &reasons {
            assert!(
                !reason.contains("SECRET") && !reason.contains('\n'),
                "{reason}"
            );
        }
        let err = DriveItem::from_json(
            serde_json::json!({"id": "a", "eTag": [secret]})
                .to_string()
                .as_bytes(),
        )
        .unwrap_err();
        assert!(!err.to_string().contains("SECRET"), "{err}");
        let err = DeltaPage::from_json(format!("{{\"x\": \"{secret}").as_bytes()).unwrap_err();
        assert!(!err.to_string().contains("SECRET"), "{err}");
    }

    /// The pre-authenticated `@microsoft.graph.downloadUrl` is not kept, so it cannot
    /// reach `Debug` output.
    #[test]
    fn download_url_annotation_is_not_kept() {
        let parsed = item(
            r#"{"id":"x","file":{},"@microsoft.graph.downloadUrl":"https://h.invalid/d?tempauth=SECRET"}"#,
        );
        assert!(!format!("{parsed:?}").contains("SECRET"));
    }
}
