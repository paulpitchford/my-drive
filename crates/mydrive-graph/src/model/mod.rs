//! Graph response types: drive, drive item and delta page.
//!
//! Each type is parsed from a private wire shape that accepts unknown fields and treats
//! everything Graph may omit as optional: no cTag on business drives, no hashes on some
//! files, no name or size on deleted items, no `parentReference.path` in delta, and the
//! root item's `parentReference` without an `id`. At this boundary drive IDs are normalised
//! through [`DriveId`], item IDs are kept exactly as received, timestamps are truncated to
//! whole seconds (ADR-0011 R5) and quickXorHash is decoded. Only `quickXorHash` is read
//! from `hashes`; `sha1Hash`, `crc32Hash` and `sha256Hash` are ignored (ADR-0011 R4).
//!
//! `@microsoft.graph.downloadUrl` is never read: the client downloads through `/content`
//! (ADR-0011 R10), and a pre-authenticated URL kept in a value could leak through `Debug`.

mod time;

use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId};
use serde::Deserialize;
use serde::de::IgnoredAny;

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
    fn from_wire(value: String) -> Self {
        match value.as_str() {
            "personal" => Self::Personal,
            "business" => Self::Business,
            "documentLibrary" => Self::DocumentLibrary,
            _ => Self::Other(value),
        }
    }
}

/// A drive's storage quota, as far as Graph reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quota {
    /// Total space in bytes.
    pub total: Option<u64>,
    /// Space used in bytes.
    pub used: Option<u64>,
    /// Space remaining in bytes.
    pub remaining: Option<u64>,
    /// Space used by items in the recycle bin, in bytes.
    pub deleted: Option<u64>,
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
    /// The quota, if reported.
    pub quota: Option<Quota>,
}

/// Where an item sits: its drive and parent folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentReference {
    /// The drive the parent is in, lowercased (ADR-0007 R1).
    pub drive_id: Option<DriveId>,
    /// The type of that drive.
    pub drive_type: Option<DriveType>,
    /// The parent folder's item ID. Absent on the root item.
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
    /// Neither facet was present, for example a deleted item whose facets Graph omitted,
    /// a OneNote package, or a shortcut described only by `remoteItem`.
    Unknown,
}

/// The `file` facet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFacet {
    /// The MIME type Graph reports.
    pub mime_type: Option<String>,
    /// The quickXorHash, the only hash guaranteed on both account types (ADR-0011 R4).
    /// Absent when Graph does not report one, which happens (for example on some empty
    /// files); absence must never be read as "unchanged".
    pub quick_xor_hash: Option<QuickXorHash>,
}

/// The `folder` facet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderFacet {
    /// The number of direct children, if reported.
    pub child_count: Option<u64>,
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
    /// File, folder or unknown.
    pub kind: ItemKind,
    /// The remote item's drive and parent.
    pub parent: Option<ParentReference>,
}

/// A drive item, from get-item or a delta page.
///
/// Content equality is decided by [`etag`](Self::etag), [`size`](Self::size) and the
/// quickXorHash (ADR-0011 R4). [`ctag`](Self::ctag) is kept because the store records it,
/// but it is absent on business delta and must not be used for that decision.
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
    /// The parent reference.
    pub parent: Option<ParentReference>,
    /// File, folder or unknown.
    pub kind: ItemKind,
    /// Whether the item carries the `deleted` facet.
    pub deleted: bool,
    /// Whether the item carries the `root` facet: it is the drive's root folder.
    pub root: bool,
    /// Whether the item carries the `shared` facet.
    pub shared: bool,
    /// The `remoteItem` facet, if present.
    pub remote_item: Option<RemoteItem>,
}

impl DriveItem {
    /// The quickXorHash, if this is a file and Graph reported one.
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
/// `items` keeps every entry in the order Graph sent it, repeats included: the same item
/// may appear more than once, and only its last occurrence may be applied (ADR-0011 R2),
/// which needs the whole round, not one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaPage {
    /// The items, in feed order.
    pub items: Vec<DriveItem>,
    /// The next link or the delta link.
    pub continuation: DeltaContinuation,
}

/// Why a response body could not be turned into a model value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// The body is not JSON of the expected shape. The reason is serde's, which names a
    /// field or position but not the body.
    #[error("the JSON does not have the expected shape: {0}")]
    Shape(String),
    /// An identifier was empty.
    #[error("the {field} is empty")]
    EmptyId {
        /// Which field.
        field: &'static str,
    },
    /// A timestamp is not RFC 3339.
    #[error("{field} is not an RFC 3339 date-time")]
    Timestamp {
        /// Which field.
        field: &'static str,
    },
    /// A quickXorHash is not 20 bytes of base64.
    #[error("the quickXorHash of item {item} is not 20 bytes of base64")]
    Hash {
        /// The item it belongs to.
        item: ItemId,
    },
    /// A delta page carries neither a next link nor a delta link, or both.
    #[error("the delta page carries {0}")]
    Continuation(&'static str),
}

// ---- wire shapes ------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireQuota {
    total: Option<u64>,
    used: Option<u64>,
    remaining: Option<u64>,
    deleted: Option<u64>,
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireDrive {
    id: String,
    drive_type: Option<String>,
    name: Option<String>,
    quota: Option<WireQuota>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireParent {
    drive_id: Option<String>,
    drive_type: Option<String>,
    id: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireHashes {
    quick_xor_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFile {
    mime_type: Option<String>,
    hashes: Option<WireHashes>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireFolder {
    child_count: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRemote {
    id: String,
    name: Option<String>,
    size: Option<u64>,
    file: Option<WireFile>,
    folder: Option<WireFolder>,
    parent_reference: Option<WireParent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireItem {
    id: String,
    name: Option<String>,
    #[serde(rename = "eTag")]
    etag: Option<String>,
    #[serde(rename = "cTag")]
    ctag: Option<String>,
    size: Option<u64>,
    created_date_time: Option<String>,
    last_modified_date_time: Option<String>,
    parent_reference: Option<WireParent>,
    file: Option<WireFile>,
    folder: Option<WireFolder>,
    deleted: Option<IgnoredAny>,
    root: Option<IgnoredAny>,
    shared: Option<IgnoredAny>,
    remote_item: Option<WireRemote>,
}

#[derive(Deserialize)]
struct WireDeltaPage {
    value: Vec<WireItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

// ---- conversion -------------------------------------------------------------------------

fn shape(e: &serde_json::Error) -> ModelError {
    ModelError::Shape(e.to_string())
}

fn drive_id(raw: String, field: &'static str) -> Result<DriveId, ModelError> {
    DriveId::new(raw).map_err(|_| ModelError::EmptyId { field })
}

fn item_id(raw: String, field: &'static str) -> Result<ItemId, ModelError> {
    ItemId::new(raw).map_err(|_| ModelError::EmptyId { field })
}

fn timestamp(raw: Option<String>, field: &'static str) -> Result<Option<Timestamp>, ModelError> {
    raw.map(|text| Timestamp::parse_rfc3339(&text).map_err(|_| ModelError::Timestamp { field }))
        .transpose()
}

impl From<WireQuota> for Quota {
    fn from(w: WireQuota) -> Self {
        Self {
            total: w.total,
            used: w.used,
            remaining: w.remaining,
            deleted: w.deleted,
            state: w.state,
        }
    }
}

impl WireParent {
    fn convert(self) -> Result<ParentReference, ModelError> {
        Ok(ParentReference {
            drive_id: self
                .drive_id
                .map(|d| drive_id(d, "parentReference.driveId"))
                .transpose()?,
            drive_type: self.drive_type.map(DriveType::from_wire),
            id: self
                .id
                .map(|i| item_id(i, "parentReference.id"))
                .transpose()?,
            path: self.path,
        })
    }
}

fn kind(
    file: Option<WireFile>,
    folder: Option<WireFolder>,
    item: &ItemId,
) -> Result<ItemKind, ModelError> {
    if let Some(folder) = folder {
        return Ok(ItemKind::Folder(FolderFacet {
            child_count: folder.child_count,
        }));
    }
    let Some(file) = file else {
        return Ok(ItemKind::Unknown);
    };
    let quick_xor_hash = file
        .hashes
        .and_then(|h| h.quick_xor_hash)
        .map(|encoded| {
            QuickXorHash::from_base64(&encoded).map_err(|_| ModelError::Hash { item: item.clone() })
        })
        .transpose()?;
    Ok(ItemKind::File(FileFacet {
        mime_type: file.mime_type,
        quick_xor_hash,
    }))
}

impl WireRemote {
    fn convert(self) -> Result<RemoteItem, ModelError> {
        let id = item_id(self.id, "remoteItem.id")?;
        let kind = kind(self.file, self.folder, &id)?;
        Ok(RemoteItem {
            kind,
            name: self.name,
            size: self.size,
            parent: self.parent_reference.map(WireParent::convert).transpose()?,
            id,
        })
    }
}

impl WireItem {
    fn convert(self) -> Result<DriveItem, ModelError> {
        let id = item_id(self.id, "item id")?;
        let kind = kind(self.file, self.folder, &id)?;
        Ok(DriveItem {
            kind,
            name: self.name,
            etag: self.etag,
            ctag: self.ctag,
            size: self.size,
            created: timestamp(self.created_date_time, "createdDateTime")?,
            last_modified: timestamp(self.last_modified_date_time, "lastModifiedDateTime")?,
            parent: self.parent_reference.map(WireParent::convert).transpose()?,
            deleted: self.deleted.is_some(),
            root: self.root.is_some(),
            shared: self.shared.is_some(),
            remote_item: self.remote_item.map(WireRemote::convert).transpose()?,
            id,
        })
    }
}

impl Drive {
    /// Parses a drive resource.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not a drive resource.
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        let wire: WireDrive = serde_json::from_slice(body).map_err(|e| shape(&e))?;
        Ok(Self {
            id: drive_id(wire.id, "drive id")?,
            drive_type: wire.drive_type.map(DriveType::from_wire),
            name: wire.name,
            quota: wire.quota.map(Quota::from),
        })
    }
}

impl DriveItem {
    /// Parses a drive item resource.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not a drive item.
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        let wire: WireItem = serde_json::from_slice(body).map_err(|e| shape(&e))?;
        wire.convert()
    }
}

impl DeltaPage {
    /// Parses a delta page.
    ///
    /// # Errors
    ///
    /// [`ModelError`] if the body is not a delta page, an item in it is malformed, or the
    /// page does not carry exactly one of `@odata.nextLink` and `@odata.deltaLink`.
    pub fn from_json(body: &[u8]) -> Result<Self, ModelError> {
        let wire: WireDeltaPage = serde_json::from_slice(body).map_err(|e| shape(&e))?;
        let continuation = match (wire.next_link, wire.delta_link) {
            (Some(next), None) => DeltaContinuation::NextLink(DeltaLink(next)),
            (None, Some(delta)) => DeltaContinuation::DeltaLink(DeltaLink(delta)),
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
        let items = wire
            .value
            .into_iter()
            .map(WireItem::convert)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            items,
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

        let extra = item(
            r#"{"id":"x","futureFacet":{"a":[1,2]},"webUrl":"https://example.invalid/x",
                "createdBy":{"user":{"displayName":"Someone"}},"file":{"newThing":true},
                "parentReference":{"siteId":"s","driveId":"D","unknown":1}}"#,
        );
        assert!(extra.is_file());
        assert_eq!(extra.quick_xor_hash(), None);
        assert_eq!(
            extra.parent.unwrap().drive_id.unwrap().as_str(),
            "d",
            "drive IDs go through DriveId"
        );
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

    /// ADR-0011 R4: only quickXorHash is read; sha1, crc32 and sha256 hashes are ignored,
    /// and the model has nowhere to put them.
    #[test]
    fn adr_0011_r4_only_quick_xor_hash_is_read() {
        let only_others = item(
            r#"{"id":"x","file":{"hashes":{"sha1Hash":"AA","sha256Hash":"BB","crc32Hash":"CC"}}}"#,
        );
        assert_eq!(only_others.quick_xor_hash(), None);
        let both = item(&format!(
            r#"{{"id":"x","file":{{"hashes":{{"sha256Hash":"BB","quickXorHash":"{HASH}"}}}}}}"#
        ));
        assert_eq!(both.quick_xor_hash().unwrap().to_base64(), HASH);
        let bad = DriveItem::from_json(br#"{"id":"x","file":{"hashes":{"quickXorHash":"!!"}}}"#);
        assert!(matches!(bad, Err(ModelError::Hash { .. })));
    }

    /// ADR-0011 R5: timestamps are whole seconds when they leave the model.
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
        assert!(matches!(
            DriveItem::from_json(br#"{"id":"x","lastModifiedDateTime":"yesterday"}"#),
            Err(ModelError::Timestamp { .. })
        ));
    }

    #[test]
    fn empty_ids_are_rejected() {
        assert!(matches!(
            DriveItem::from_json(br#"{"id":""}"#),
            Err(ModelError::EmptyId { .. })
        ));
        assert!(DriveItem::from_json(br#"{"name":"no id"}"#).is_err());
        assert!(Drive::from_json(br#"{"id":""}"#).is_err());
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
        assert_eq!(done.items.len(), 2, "repeats are kept for ADR-0011 R2");
        assert!(DeltaPage::from_json(br#"{"value":[]}"#).is_err());
        assert!(
            DeltaPage::from_json(br#"{"value":[],"@odata.nextLink":"n","@odata.deltaLink":"d"}"#)
                .is_err()
        );
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
