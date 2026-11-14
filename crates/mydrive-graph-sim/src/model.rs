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

/// How an item changed several times since a delta token appears in the feed.
///
/// Graph documents both that "the delta feed shows the latest state for each item, not
/// each change" and that "the same item may appear more than once in a delta feed ... use
/// the last occurrence". When and how often repeats happen is not documented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeltaRepeats {
    /// Once per change since the token, each occurrence showing the item's state when its
    /// page is served. Deliberately harsher than Graph usually is, to exercise a client's
    /// last-occurrence rule (ADR-0011 R2).
    EveryChange,
    /// Once, at the position of its latest change: the "latest state for each item" model.
    LatestStateOnce,
}

/// What delta reports when a folder with contents is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderDeletes {
    /// Only the folder appears with the `deleted` facet; the client must work out that
    /// everything below it is gone.
    FolderOnly,
    /// The folder and then every descendant appear with the `deleted` facet.
    EveryDescendant,
}

/// Per-drive behaviour switches. Set them with `GraphSim::add_drive_with` or
/// `GraphSim::set_options`.
///
/// Each default is what Microsoft documents. Where the documentation is silent or the
/// behaviour is unverified, the default is the case that is harder for a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveOptions {
    /// How content is downloaded. Default [`DownloadMode::Redirect`], as documented today.
    pub download: DownloadMode,
    /// What happens to `Range`. Default [`RangeMode::Honoured`], the documented normal case.
    pub range: RangeMode,
    /// Items per delta page; 0 is treated as 1. Default 200 (not documented for delta;
    /// the documented children page size).
    pub delta_page_size: usize,
    /// Items per children page when the request has no `$top`; 0 is treated as 1.
    /// Default 200, the documented default page size.
    pub children_page_size: usize,
    /// Repeats in token-based delta. Default [`DeltaRepeats::EveryChange`], the harder
    /// case. A token-less enumeration always shows each item's current state once.
    pub delta_repeats: DeltaRepeats,
    /// What a folder delete reports. Unverified; default [`FolderDeletes::FolderOnly`],
    /// the harder case.
    pub folder_deletes: FolderDeletes,
    /// Whether get-item and list-children include `parentReference.path`. Default `true`,
    /// as Graph does; delta never includes it.
    pub parent_path_outside_delta: bool,
    /// Whether personal-drive folders carry a cTag. Default `false`: the driveItem
    /// resource says cTag "isn't returned if the item is a folder".
    pub personal_folder_ctag: bool,
    /// Whether content responses carry the item's eTag in an `ETag` header. Unverified
    /// (gate G1); default `false`, the harder case.
    pub content_etag_header: bool,
    /// Whether zero-byte files report a quickXorHash. Unverified; default `false`, the
    /// harder case.
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
            folder_deletes: FolderDeletes::FolderOnly,
            parent_path_outside_delta: true,
            personal_folder_ctag: false,
            content_etag_header: false,
            hash_empty_files: false,
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
    /// A file's length, or the total of a folder's live contents.
    pub(crate) size: u64,
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

/// Where a delta enumeration has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeltaCursor {
    /// Changes up to and including this sequence number have been served.
    pub(crate) after: u64,
    /// For a token-less enumeration, the end of the change log when it started. Changes
    /// up to here are served as the current state of each live item, once; later ones as
    /// ordinary changes.
    pub(crate) snapshot: Option<u64>,
}

/// A page of delta results, as item IDs in feed order.
#[derive(Debug)]
pub(crate) struct DeltaPage {
    pub(crate) items: Vec<ItemId>,
    /// Where the next page continues.
    pub(crate) next: DeltaCursor,
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
    /// Live children of each live folder.
    children: HashMap<ItemId, HashSet<ItemId>>,
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
            children: HashMap::new(),
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
        let size = match &kind {
            ItemKind::File { content, .. } => content.len() as u64,
            ItemKind::Folder => 0,
        };
        let item = Item {
            id: id.clone(),
            name,
            parent: parent.clone(),
            kind,
            created: now,
            modified: now,
            size,
            etag_version: 1,
            ctag_version: 1,
            guid,
            deleted: false,
            last_seq: 0,
        };
        if item.is_folder() {
            self.children.insert(id.clone(), HashSet::new());
        }
        self.items.insert(id.clone(), item);
        if let Some(parent) = &parent {
            self.children
                .entry(parent.clone())
                .or_default()
                .insert(id.clone());
            self.adjust_sizes(parent, size, true);
        }
        self.record(&id);
        Ok(id)
    }

    /// Adds `amount` to (or takes it from) the size of `folder` and each of its ancestors.
    fn adjust_sizes(&mut self, folder: &ItemId, amount: u64, add: bool) {
        let mut cursor = Some(folder.clone());
        while let Some(id) = cursor {
            cursor = match self.items.get_mut(&id) {
                Some(item) => {
                    item.size = if add {
                        item.size.saturating_add(amount)
                    } else {
                        item.size.saturating_sub(amount)
                    };
                    item.parent.clone()
                }
                None => None,
            };
        }
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

    /// How many live children `folder` has.
    pub(crate) fn child_count(&self, folder: &ItemId) -> usize {
        self.children.get(folder).map_or(0, HashSet::len)
    }

    /// Live children of `folder`, sorted by name without regard to case, then by ID.
    pub(crate) fn children(&self, folder: &ItemId) -> Vec<&Item> {
        let mut children: Vec<&Item> = self
            .children
            .get(folder)
            .into_iter()
            .flatten()
            .filter_map(|id| self.items.get(id))
            .collect();
        children.sort_by_cached_key(|item| (item.name.to_lowercase(), item.id.clone()));
        children
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
            .children
            .get(parent)
            .into_iter()
            .flatten()
            .filter(|id| Some(*id) != except)
            .filter_map(|id| self.items.get(id))
            .any(|child| child.name.to_lowercase() == lower);
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
        let item = self.live(id)?;
        if item.is_folder() {
            return Err(SimError::NotAFile {
                drive: self.id.clone(),
                item: id.clone(),
            });
        }
        let (old_size, parent) = (item.size, item.parent.clone());
        let new_size = content.len() as u64;
        let kind = Self::file_kind(content);
        if let Some(item) = self.items.get_mut(id) {
            item.kind = kind;
            item.size = new_size;
            item.modified = now;
            item.etag_version += 1;
            item.ctag_version += 1;
        }
        if let Some(parent) = &parent {
            self.adjust_sizes(parent, old_size, false);
            self.adjust_sizes(parent, new_size, true);
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
        let item = self.live(id)?;
        let (name, size, old_parent) = (item.name.clone(), item.size, item.parent.clone());
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
        if let Some(old) = &old_parent {
            if let Some(siblings) = self.children.get_mut(old) {
                siblings.remove(id);
            }
            self.adjust_sizes(old, size, false);
        }
        self.children
            .entry(new_parent.clone())
            .or_default()
            .insert(id.clone());
        self.adjust_sizes(new_parent, size, true);
        if let Some(item) = self.items.get_mut(id) {
            item.parent = Some(new_parent.clone());
            item.etag_version += 1;
        }
        self.record(id);
        Ok(())
    }

    /// Deletes an item and everything below it. Delta reports the item, and its
    /// descendants after it only under [`FolderDeletes::EveryDescendant`].
    fn delete(&mut self, id: &ItemId) -> Result<(), SimError> {
        self.not_root(id)?;
        let item = self.live(id)?;
        let (size, parent) = (item.size, item.parent.clone());
        let mut order = Vec::new();
        let mut queue = VecDeque::from([id.clone()]);
        while let Some(current) = queue.pop_front() {
            if let Some(children) = self.children.remove(&current) {
                let mut children: Vec<ItemId> = children.into_iter().collect();
                children.sort();
                queue.extend(children);
            }
            order.push(current);
        }
        for current in &order {
            if let Some(item) = self.items.get_mut(current) {
                item.deleted = true;
                item.etag_version += 1;
            }
        }
        if let Some(parent) = &parent {
            if let Some(siblings) = self.children.get_mut(parent) {
                siblings.remove(id);
            }
            self.adjust_sizes(parent, size, false);
        }
        match self.options.folder_deletes {
            FolderDeletes::FolderOnly => self.record(id),
            FolderDeletes::EveryDescendant => {
                for current in &order {
                    self.record(current);
                }
            }
        }
        Ok(())
    }

    /// A token-less enumeration starting now.
    pub(crate) fn initial_cursor(&self) -> DeltaCursor {
        DeltaCursor {
            after: 0,
            snapshot: Some(self.seq),
        }
    }

    /// Whether a cursor decoded from a client's link can belong to this drive.
    pub(crate) fn cursor_is_valid(&self, cursor: DeltaCursor) -> bool {
        cursor.after <= self.seq && cursor.snapshot.is_none_or(|s| s <= self.seq)
    }

    /// The next page of the feed from `cursor`.
    pub(crate) fn delta_page(&self, cursor: DeltaCursor) -> DeltaPage {
        let page_size = self.options.delta_page_size.max(1);
        let start = self
            .changes
            .partition_point(|(seq, _)| *seq <= cursor.after);
        let mut items = Vec::new();
        let mut after = cursor.after;
        for (seq, id) in &self.changes[start..] {
            if items.len() == page_size {
                return DeltaPage {
                    items,
                    next: DeltaCursor {
                        after,
                        snapshot: cursor.snapshot,
                    },
                    last: false,
                };
            }
            after = *seq;
            let Some(item) = self.items.get(id) else {
                continue;
            };
            let latest = item.last_seq == *seq;
            let include = match cursor.snapshot {
                // The current state: each live item once, at its latest change.
                Some(snapshot) if *seq <= snapshot => latest && !item.deleted,
                _ => latest || self.options.delta_repeats == DeltaRepeats::EveryChange,
            };
            if include {
                items.push(id.clone());
            }
        }
        DeltaPage {
            items,
            next: DeltaCursor {
                after: self.seq,
                snapshot: None,
            },
            last: true,
        }
    }

    /// The current end of the change log.
    #[cfg(test)]
    pub(crate) fn latest_seq(&self) -> u64 {
        self.seq
    }
}

/// How many pre-authenticated download keys stay valid at once.
pub(crate) const MAX_DOWNLOAD_KEYS: usize = 1024;

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
    /// Issue order of `download_keys`, oldest first, so the oldest can expire.
    key_order: VecDeque<String>,
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
            key_order: VecDeque::new(),
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

    /// Issues a fresh pre-authenticated download key for an item. Only the newest
    /// [`MAX_DOWNLOAD_KEYS`] keys stay valid; an older one has expired and gets 401.
    pub(crate) fn issue_download_key(&mut self, drive: &DriveId, item: &ItemId) -> String {
        self.next_key += 1;
        let key = format!("{}{:08x}", self.key_prefix, self.next_key);
        self.download_keys
            .insert(key.clone(), (drive.clone(), item.clone()));
        self.key_order.push_back(key.clone());
        while self.key_order.len() > MAX_DOWNLOAD_KEYS {
            if let Some(oldest) = self.key_order.pop_front() {
                self.download_keys.remove(&oldest);
            }
        }
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
    fn sizes_and_child_counts_follow_changes() {
        let mut d = drive(DriveOptions::default());
        let root = d.root.clone();
        let a = d.create(&root, "a", ItemKind::Folder, START_SECS).unwrap();
        let b = d.create(&root, "b", ItemKind::Folder, START_SECS).unwrap();
        let five = Drive::file_kind(Bytes::from_static(b"12345"));
        let f = d.create(&a, "f", five, START_SECS).unwrap();
        let size = |d: &Drive, id: &ItemId| d.live(id).unwrap().size;
        assert_eq!((size(&d, &root), size(&d, &a), size(&d, &b)), (5, 5, 0));
        d.modify_file(&f, Bytes::from_static(b"12"), START_SECS)
            .unwrap();
        assert_eq!((size(&d, &root), size(&d, &a)), (2, 2));
        d.move_to(&f, &b).unwrap();
        assert_eq!((size(&d, &root), size(&d, &a), size(&d, &b)), (2, 0, 2));
        assert_eq!((d.child_count(&a), d.child_count(&b)), (0, 1));
        d.delete(&b).unwrap();
        assert_eq!((size(&d, &root), d.child_count(&root)), (0, 1));
        assert!(d.live(&f).is_err());
    }

    #[test]
    fn download_keys_are_bounded() {
        let mut state = State::new("k".to_owned());
        let drive = state
            .add_drive(DriveType::Personal, DriveOptions::default())
            .unwrap();
        let root = state.drive(&drive).unwrap().root.clone();
        let first = state.issue_download_key(&drive, &root);
        for _ in 0..MAX_DOWNLOAD_KEYS {
            state.issue_download_key(&drive, &root);
        }
        assert!(state.resolve_download_key(&first).is_none());
        assert_eq!(state.download_keys.len(), MAX_DOWNLOAD_KEYS);
    }

    #[test]
    fn latest_state_once_skips_earlier_occurrences() {
        let mut d = drive(DriveOptions {
            delta_repeats: DeltaRepeats::LatestStateOnce,
            ..DriveOptions::default()
        });
        let root = d.root.clone();
        let file = d
            .create(&root, "a", Drive::file_kind(Bytes::new()), START_SECS)
            .unwrap();
        let after = d.latest_seq();
        d.rename(&file, "b").unwrap();
        d.rename(&file, "c").unwrap();
        let page = d.delta_page(DeltaCursor {
            after,
            snapshot: None,
        });
        assert!(page.last);
        assert_eq!(page.items, vec![file]);
    }
}
