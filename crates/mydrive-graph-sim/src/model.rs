//! The simulated drives and items, and the change log delta is served from.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use axum::body::Bytes;
use mydrive_core::hash::QuickXorHasher;
use mydrive_core::ids::{DriveId, ItemId};

use crate::error::SimError;
use crate::fault::Fault;
use crate::log::LoggedRequest;
use crate::time::START_SECS;

/// The kind of drive, as Graph's `driveType` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriveType {
    /// A personal OneDrive (`personal`).
    Personal,
    /// A OneDrive for Business (`business`).
    Business,
    /// A SharePoint document library (`documentLibrary`).
    DocumentLibrary,
}

impl DriveType {
    /// The `driveType` string Graph uses.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Business => "business",
            Self::DocumentLibrary => "documentLibrary",
        }
    }

    /// Whether the drive is a consumer OneDrive. Business drives and SharePoint libraries
    /// share the "OneDrive for Business" behaviour in the delta documentation.
    #[must_use]
    pub fn is_personal(self) -> bool {
        self == Self::Personal
    }
}

/// How `GET .../content` serves file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadMode {
    /// `302 Found` to a pre-authenticated URL that needs no token, as Graph documents
    /// today.
    Redirect,
    /// The content itself, from the request that carries the bearer token, as Graph is
    /// announced to behave after pre-authenticated URLs are retired (MC1492958).
    Direct,
}

/// What the server does with a `Range` header on a content request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeMode {
    /// A satisfiable single range gets `206 Partial Content` with exactly those bytes.
    Honoured,
    /// The header is ignored and the whole body comes back with `200 OK`, which the
    /// download documentation says may happen.
    Ignored,
}

/// How often an item changed several times since a token appears in the delta feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeltaRepeats {
    /// Once per change, each time showing its state when the page is served. This is the
    /// "same item may appear more than once" case that clients must handle.
    EveryChange,
    /// Once, at the position of its latest change.
    LastChangeOnly,
}

/// Per-drive behaviour switches. Set them with `GraphSim::add_drive_with` or
/// `GraphSim::set_options`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveOptions {
    /// How content is downloaded. Default: [`DownloadMode::Redirect`].
    pub download: DownloadMode,
    /// What happens to `Range`. Default: [`RangeMode::Honoured`].
    pub range: RangeMode,
    /// Items per delta page; 0 is treated as 1. Default: 200.
    pub delta_page_size: usize,
    /// Items per children page when the request has no `$top`; 0 is treated as 1.
    /// Default: 200, the documented default page size.
    pub children_page_size: usize,
    /// Whether repeated changes repeat in delta. Default: [`DeltaRepeats::EveryChange`].
    pub delta_repeats: DeltaRepeats,
    /// Whether `parentReference.path` is included in get-item and list-children
    /// responses, as real Graph does. Delta never includes it. Default: `false`.
    pub parent_path_outside_delta: bool,
    /// Whether content responses carry the item's eTag in an `ETag` header. Unverified
    /// (gate G1); default `false`.
    pub content_etag_header: bool,
    /// Whether zero-byte files report a quickXorHash. Unverified; default `true`.
    pub hash_empty_files: bool,
}

impl Default for DriveOptions {
    fn default() -> Self {
        Self {
            download: DownloadMode::Redirect,
            range: RangeMode::Honoured,
            delta_page_size: 200,
            children_page_size: 200,
            delta_repeats: DeltaRepeats::EveryChange,
            parent_path_outside_delta: false,
            content_etag_header: false,
            hash_empty_files: true,
        }
    }
}

/// Folder or file, and a file's content.
#[derive(Debug, Clone)]
pub(crate) enum ItemKind {
    Folder,
    File { content: Bytes, quick_xor: String },
}

/// One item, live or deleted. Deleted items stay so delta can report them.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub(crate) id: ItemId,
    pub(crate) name: String,
    pub(crate) parent: Option<ItemId>,
    pub(crate) kind: ItemKind,
    pub(crate) created: i64,
    pub(crate) modified: i64,
    /// Bumped on every change; the number in the eTag.
    pub(crate) etag_version: u64,
    /// Bumped on content changes; the number in the cTag.
    pub(crate) ctag_version: u64,
    /// The GUID-shaped part shared by eTag and cTag.
    pub(crate) guid: String,
    pub(crate) deleted: bool,
    /// Sequence number of the item's latest change.
    pub(crate) last_seq: u64,
}

impl Item {
    pub(crate) fn is_folder(&self) -> bool {
        matches!(self.kind, ItemKind::Folder)
    }

    pub(crate) fn etag(&self) -> String {
        format!("\"{{{}}},{}\"", self.guid, self.etag_version)
    }

    pub(crate) fn ctag(&self) -> String {
        format!("\"c:{{{}}},{}\"", self.guid, self.ctag_version)
    }
}

/// A page of delta results, as item IDs in feed order.
#[derive(Debug)]
pub(crate) struct DeltaPage {
    pub(crate) items: Vec<ItemId>,
    /// The change sequence the next request continues after.
    pub(crate) resume_after: u64,
    /// Whether this is the last page (it then carries a deltaLink).
    pub(crate) last: bool,
}

/// One drive and its items.
#[derive(Debug)]
pub(crate) struct Drive {
    pub(crate) id: DriveId,
    pub(crate) number: u64,
    pub(crate) drive_type: DriveType,
    pub(crate) options: DriveOptions,
    pub(crate) root: ItemId,
    pub(crate) items: HashMap<ItemId, Item>,
    /// Every change, in order: (sequence number, item).
    changes: Vec<(u64, ItemId)>,
    seq: u64,
    next_item: u64,
}

impl Drive {
    fn new(
        number: u64,
        drive_type: DriveType,
        options: DriveOptions,
        now: i64,
    ) -> Result<Self, SimError> {
        let raw = match drive_type {
            // Personal drive IDs are 16 hex digits (research section 4, issue #1902).
            DriveType::Personal => format!("{:016x}", 0x5100_0000_0000_0000_u64 + number),
            // Business and SharePoint drive IDs start `b!` (drive-get example).
            DriveType::Business | DriveType::DocumentLibrary => {
                format!(
                    "b!sim-{}-{number:06}",
                    drive_type.as_wire().to_ascii_lowercase()
                )
            }
        };
        let mut drive = Self {
            id: DriveId::new(raw)?,
            root: ItemId::new("pending-root")?,
            number,
            drive_type,
            options,
            items: HashMap::new(),
            changes: Vec::new(),
            seq: 0,
            next_item: 0,
        };
        drive.root = drive.insert(None, "root".to_owned(), ItemKind::Folder, now)?;
        Ok(drive)
    }

    fn new_item_id(&mut self) -> Result<(ItemId, String), SimError> {
        self.next_item += 1;
        let n = self.next_item;
        let raw = match self.drive_type {
            // Personal item IDs are `<DRIVE ID>!<number>` in upper case.
            DriveType::Personal => format!("{}!{}", self.id.as_str().to_ascii_uppercase(), n),
            // Business and SharePoint item IDs are 34 upper-case characters starting `01`,
            // as in the get-item example.
            DriveType::Business | DriveType::DocumentLibrary => {
                format!("01SIM{:05}{:024}", self.number % 100_000, n)
            }
        };
        let guid = format!("{:08X}-0000-4000-8000-{:012X}", self.number, n);
        Ok((ItemId::new(raw)?, guid))
    }

    fn insert(
        &mut self,
        parent: Option<ItemId>,
        name: String,
        kind: ItemKind,
        now: i64,
    ) -> Result<ItemId, SimError> {
        let (id, guid) = self.new_item_id()?;
        let item = Item {
            id: id.clone(),
            name,
            parent,
            kind,
            created: now,
            modified: now,
            etag_version: 1,
            ctag_version: 1,
            guid,
            deleted: false,
            last_seq: 0,
        };
        self.items.insert(id.clone(), item);
        self.record(&id);
        Ok(id)
    }

    fn record(&mut self, id: &ItemId) {
        self.seq += 1;
        let seq = self.seq;
        self.changes.push((seq, id.clone()));
        if let Some(item) = self.items.get_mut(id) {
            item.last_seq = seq;
        }
    }

    /// The live item with this ID.
    pub(crate) fn live(&self, id: &ItemId) -> Result<&Item, SimError> {
        match self.items.get(id) {
            Some(item) if !item.deleted => Ok(item),
            _ => Err(SimError::UnknownItem {
                drive: self.id.clone(),
                item: id.clone(),
            }),
        }
    }

    fn live_folder(&self, id: &ItemId) -> Result<&Item, SimError> {
        let item = self.live(id)?;
        if item.is_folder() {
            Ok(item)
        } else {
            Err(SimError::NotAFolder {
                drive: self.id.clone(),
                item: id.clone(),
            })
        }
    }

    /// Live children of `folder`, sorted by name without regard to case, then by ID.
    pub(crate) fn children(&self, folder: &ItemId) -> Vec<&Item> {
        let mut children: Vec<&Item> = self
            .items
            .values()
            .filter(|item| !item.deleted && item.parent.as_ref() == Some(folder))
            .collect();
        children.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        children
    }

    /// Total size of a live item: a file's length, or the sum over a folder's descendants.
    pub(crate) fn size(&self, item: &Item) -> u64 {
        match &item.kind {
            ItemKind::File { content, .. } => content.len() as u64,
            ItemKind::Folder => self
                .children(&item.id)
                .into_iter()
                .map(|child| self.size(child))
                .sum(),
        }
    }

    /// Names of the ancestors of `item` below the root, outermost first.
    pub(crate) fn ancestor_names(&self, item: &Item) -> Vec<&str> {
        let mut names = Vec::new();
        let mut next = item.parent.as_ref();
        while let Some(id) = next {
            match self.items.get(id) {
                Some(parent) if parent.parent.is_some() => {
                    names.push(parent.name.as_str());
                    next = parent.parent.as_ref();
                }
                _ => break,
            }
        }
        names.reverse();
        names
    }

    fn check_name(
        &self,
        parent: &ItemId,
        name: &str,
        except: Option<&ItemId>,
    ) -> Result<(), SimError> {
        if name.is_empty() || name.contains('/') {
            return Err(SimError::InvalidName {
                name: name.to_owned(),
            });
        }
        // OneDrive's namespace is case-insensitive (rclone docs, research section 4).
        let lower = name.to_lowercase();
        let clash = self
            .children(parent)
            .into_iter()
            .any(|child| Some(&child.id) != except && child.name.to_lowercase() == lower);
        if clash {
            return Err(SimError::NameConflict {
                drive: self.id.clone(),
                parent: parent.clone(),
                name: name.to_owned(),
            });
        }
        Ok(())
    }

    fn create(
        &mut self,
        parent: &ItemId,
        name: &str,
        kind: ItemKind,
        now: i64,
    ) -> Result<ItemId, SimError> {
        self.live_folder(parent)?;
        self.check_name(parent, name, None)?;
        self.insert(Some(parent.clone()), name.to_owned(), kind, now)
    }

    fn file_kind(content: Bytes) -> ItemKind {
        let mut hasher = QuickXorHasher::new();
        hasher.update(&content);
        ItemKind::File {
            quick_xor: hasher.finalize().to_base64(),
            content,
        }
    }

    fn not_root(&self, id: &ItemId) -> Result<(), SimError> {
        if *id == self.root {
            Err(SimError::RootItem {
                drive: self.id.clone(),
            })
        } else {
            Ok(())
        }
    }

    fn modify_file(&mut self, id: &ItemId, content: Bytes, now: i64) -> Result<(), SimError> {
        if self.live(id)?.is_folder() {
            return Err(SimError::NotAFile {
                drive: self.id.clone(),
                item: id.clone(),
            });
        }
        let kind = Self::file_kind(content);
        if let Some(item) = self.items.get_mut(id) {
            item.kind = kind;
            item.modified = now;
            item.etag_version += 1;
            item.ctag_version += 1;
        }
        self.record(id);
        Ok(())
    }

    fn rename(&mut self, id: &ItemId, name: &str) -> Result<(), SimError> {
        self.not_root(id)?;
        let parent = self.live(id)?.parent.clone();
        if let Some(parent) = &parent {
            self.check_name(parent, name, Some(id))?;
        }
        if let Some(item) = self.items.get_mut(id) {
            item.name = name.to_owned();
            item.etag_version += 1;
        }
        self.record(id);
        Ok(())
    }

    fn move_to(&mut self, id: &ItemId, new_parent: &ItemId) -> Result<(), SimError> {
        self.not_root(id)?;
        let name = self.live(id)?.name.clone();
        self.live_folder(new_parent)?;
        // A folder cannot move into itself or below itself.
        let mut cursor = Some(new_parent.clone());
        while let Some(current) = cursor {
            if current == *id {
                return Err(SimError::MoveIntoSelf {
                    drive: self.id.clone(),
                    item: id.clone(),
                });
            }
            cursor = self
                .items
                .get(&current)
                .and_then(|item| item.parent.clone());
        }
        self.check_name(new_parent, &name, Some(id))?;
        if let Some(item) = self.items.get_mut(id) {
            item.parent = Some(new_parent.clone());
            item.etag_version += 1;
        }
        self.record(id);
        Ok(())
    }

    /// Deletes an item and everything below it. The item's change comes first in the feed
    /// and its descendants' after it, so a client that removes a folder only once it is
    /// empty after the whole feed is applied is exercised.
    fn delete(&mut self, id: &ItemId) -> Result<(), SimError> {
        self.not_root(id)?;
        self.live(id)?;
        let mut order = Vec::new();
        let mut queue = VecDeque::from([id.clone()]);
        while let Some(current) = queue.pop_front() {
            queue.extend(self.children(&current).into_iter().map(|c| c.id.clone()));
            order.push(current);
        }
        for current in &order {
            if let Some(item) = self.items.get_mut(current) {
                item.deleted = true;
                item.etag_version += 1;
            }
        }
        for current in &order {
            self.record(current);
        }
        Ok(())
    }

    /// The next page of changes after sequence number `after`.
    pub(crate) fn delta_page(&self, after: u64) -> DeltaPage {
        let page_size = self.options.delta_page_size.max(1);
        let start = self.changes.partition_point(|(seq, _)| *seq <= after);
        let mut items = Vec::new();
        let mut resume_after = after;
        for (seq, id) in &self.changes[start..] {
            if items.len() == page_size {
                return DeltaPage {
                    items,
                    resume_after,
                    last: false,
                };
            }
            resume_after = *seq;
            let latest = self.items.get(id).map(|item| item.last_seq);
            if self.options.delta_repeats == DeltaRepeats::LastChangeOnly && latest != Some(*seq) {
                continue;
            }
            items.push(id.clone());
        }
        DeltaPage {
            items,
            resume_after: self.seq,
            last: true,
        }
    }

    /// The current end of the change log.
    pub(crate) fn latest_seq(&self) -> u64 {
        self.seq
    }
}

/// Everything one simulator holds.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) clock: i64,
    next_drive: u64,
    pub(crate) drives: BTreeMap<DriveId, Drive>,
    pub(crate) tokens: HashSet<String>,
    pub(crate) faults: VecDeque<Fault>,
    pub(crate) log: Vec<LoggedRequest>,
    download_keys: HashMap<String, (DriveId, ItemId)>,
    next_key: u64,
    /// Distinguishes this simulator's pre-authenticated keys from another's.
    key_prefix: String,
}

impl State {
    pub(crate) fn new(key_prefix: String) -> Self {
        Self {
            clock: START_SECS,
            next_drive: 0,
            drives: BTreeMap::new(),
            tokens: HashSet::new(),
            faults: VecDeque::new(),
            log: Vec::new(),
            download_keys: HashMap::new(),
            next_key: 0,
            key_prefix,
        }
    }

    pub(crate) fn add_drive(
        &mut self,
        drive_type: DriveType,
        options: DriveOptions,
    ) -> Result<DriveId, SimError> {
        self.next_drive += 1;
        let drive = Drive::new(self.next_drive, drive_type, options, self.clock)?;
        let id = drive.id.clone();
        self.drives.insert(id.clone(), drive);
        Ok(id)
    }

    pub(crate) fn drive(&self, id: &DriveId) -> Result<&Drive, SimError> {
        self.drives
            .get(id)
            .ok_or_else(|| SimError::UnknownDrive { drive: id.clone() })
    }

    pub(crate) fn drive_mut(&mut self, id: &DriveId) -> Result<&mut Drive, SimError> {
        self.drives
            .get_mut(id)
            .ok_or_else(|| SimError::UnknownDrive { drive: id.clone() })
    }

    pub(crate) fn create_folder(
        &mut self,
        drive: &DriveId,
        parent: &ItemId,
        name: &str,
    ) -> Result<ItemId, SimError> {
        let now = self.clock;
        self.drive_mut(drive)?
            .create(parent, name, ItemKind::Folder, now)
    }

    pub(crate) fn create_file(
        &mut self,
        drive: &DriveId,
        parent: &ItemId,
        name: &str,
        content: Bytes,
    ) -> Result<ItemId, SimError> {
        let now = self.clock;
        self.drive_mut(drive)?
            .create(parent, name, Drive::file_kind(content), now)
    }

    pub(crate) fn modify_file(
        &mut self,
        drive: &DriveId,
        item: &ItemId,
        content: Bytes,
    ) -> Result<(), SimError> {
        let now = self.clock;
        self.drive_mut(drive)?.modify_file(item, content, now)
    }

    pub(crate) fn rename(
        &mut self,
        drive: &DriveId,
        item: &ItemId,
        name: &str,
    ) -> Result<(), SimError> {
        self.drive_mut(drive)?.rename(item, name)
    }

    pub(crate) fn move_to(
        &mut self,
        drive: &DriveId,
        item: &ItemId,
        new_parent: &ItemId,
    ) -> Result<(), SimError> {
        self.drive_mut(drive)?.move_to(item, new_parent)
    }

    pub(crate) fn delete(&mut self, drive: &DriveId, item: &ItemId) -> Result<(), SimError> {
        self.drive_mut(drive)?.delete(item)
    }

    /// Issues a fresh pre-authenticated download key for an item.
    pub(crate) fn issue_download_key(&mut self, drive: &DriveId, item: &ItemId) -> String {
        self.next_key += 1;
        let key = format!("{}{:08x}", self.key_prefix, self.next_key);
        self.download_keys
            .insert(key.clone(), (drive.clone(), item.clone()));
        key
    }

    pub(crate) fn resolve_download_key(&self, key: &str) -> Option<&(DriveId, ItemId)> {
        self.download_keys.get(key)
    }

    /// Removes and returns the first queued fault that matches.
    pub(crate) fn take_fault(&mut self, method: &axum::http::Method, path: &str) -> Option<Fault> {
        let index = self.faults.iter().position(|f| f.matches(method, path))?;
        self.faults.remove(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(options: DriveOptions) -> Drive {
        Drive::new(1, DriveType::Personal, options, START_SECS).unwrap()
    }

    #[test]
    fn rename_keeps_ctag_and_bumps_etag() {
        let mut d = drive(DriveOptions::default());
        let root = d.root.clone();
        let file = d
            .create(
                &root,
                "a.txt",
                Drive::file_kind(Bytes::from_static(b"x")),
                START_SECS,
            )
            .unwrap();
        let before = d.live(&file).unwrap().clone();
        d.rename(&file, "b.txt").unwrap();
        let after = d.live(&file).unwrap().clone();
        assert_ne!(before.etag(), after.etag());
        assert_eq!(before.ctag(), after.ctag());
        d.modify_file(&file, Bytes::from_static(b"y"), START_SECS)
            .unwrap();
        assert_ne!(after.ctag(), d.live(&file).unwrap().ctag());
    }

    #[test]
    fn names_clash_without_regard_to_case() {
        let mut d = drive(DriveOptions::default());
        let root = d.root.clone();
        d.create(&root, "Hello.doc", ItemKind::Folder, START_SECS)
            .unwrap();
        assert!(matches!(
            d.create(&root, "hello.doc", ItemKind::Folder, START_SECS),
            Err(SimError::NameConflict { .. })
        ));
    }

    #[test]
    fn folder_cannot_move_below_itself() {
        let mut d = drive(DriveOptions::default());
        let root = d.root.clone();
        let a = d.create(&root, "a", ItemKind::Folder, START_SECS).unwrap();
        let b = d.create(&a, "b", ItemKind::Folder, START_SECS).unwrap();
        assert!(matches!(
            d.move_to(&a, &b),
            Err(SimError::MoveIntoSelf { .. })
        ));
        assert!(matches!(
            d.move_to(&a, &a),
            Err(SimError::MoveIntoSelf { .. })
        ));
        assert!(matches!(d.delete(&root), Err(SimError::RootItem { .. })));
    }

    #[test]
    fn last_change_only_skips_earlier_occurrences() {
        let mut d = drive(DriveOptions {
            delta_repeats: DeltaRepeats::LastChangeOnly,
            ..DriveOptions::default()
        });
        let root = d.root.clone();
        let file = d
            .create(&root, "a", Drive::file_kind(Bytes::new()), START_SECS)
            .unwrap();
        d.rename(&file, "b").unwrap();
        let page = d.delta_page(0);
        assert!(page.last);
        assert_eq!(page.items, vec![root, file]);
    }
}
