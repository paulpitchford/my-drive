//! Bounded results: pages of `item.status`, action results and change notifications that
//! each fit one line (ADR-0020).
//!
//! # Cursors
//!
//! Cursors are keyset cursors (ADR-0020 R1 to R3). A directory cursor names the last child
//! returned; the next page starts at the first child whose name sorts after it, by bytes. A
//! batch cursor names a position in the request's list of paths. Either kind also carries a
//! binding: a hash of the directory, or of the whole batch, so a cursor used with other
//! parameters is refused with `invalid-params`. Cursors hold no daemon state and never go
//! stale. To clients they are opaque, non-empty strings; this module accepts only cursors in
//! the exact form it issues.
//!
//! The directory pager reads children through [`DirectorySource`], which the daemon backs
//! with a store query ("children of this folder with name > X, ordered by name").

use std::collections::VecDeque;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::codec::MAX_LINE_BYTES;
use crate::envelope::{Id, Notification, SuccessResponse};
use crate::error::{ErrorCode, RpcError, truncate_to};
use crate::methods::{
    ActionOutcome, ItemActionEntry, ItemActionResult, ItemStatusEntry, ItemStatusResult,
    SkippedItem, StatusChangedParams, method,
};
use crate::path::ProtocolPath;

/// How many children the directory pager asks its source for at a time.
const CHUNK: usize = 256;

/// Bytes `,"next_cursor":""` adds to a result around the cursor text itself.
const CURSOR_FIELD_BYTES: usize = r#","next_cursor":"""#.len();

/// Reads a directory's children in name order, for the directory pager.
///
/// The daemon implements this over the state store's (parent, name) index; tests implement
/// it over a sorted map.
pub trait DirectorySource {
    /// Up to `limit` children whose names sort strictly after `after` (every child when
    /// `after` is `None`), in ascending byte order of name. Each entry's path is the
    /// directory's path joined with the child's name.
    ///
    /// # Errors
    ///
    /// Any error reading the children; it is returned to the client as is.
    fn children_after(
        &mut self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ItemStatusEntry>, RpcError>;
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2)
        || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// A 128-bit binding of a cursor to its request.
fn binding(kind: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    for part in parts {
        hasher.update([0]);
        hasher.update(part.as_bytes());
    }
    to_hex(&hasher.finalize()[..16])
}

fn directory_binding(directory: &ProtocolPath) -> String {
    binding("directory", &[directory.as_str()])
}

fn batch_binding(paths: &[ProtocolPath]) -> String {
    let parts: Vec<&str> = paths.iter().map(ProtocolPath::as_str).collect();
    binding("paths", &parts)
}

/// A directory cursor; `bound` is [`directory_binding`] of the directory.
fn directory_cursor(bound: &str, name: &str) -> String {
    format!("d1.{bound}.{}", to_hex(name.as_bytes()))
}

/// A batch cursor; `bound` is [`batch_binding`] of the batch.
fn batch_cursor(bound: &str, index: usize) -> String {
    format!("p1.{bound}.{index}")
}

fn foreign_cursor(cursor: &str) -> RpcError {
    RpcError::invalid_params(
        method::ITEM_STATUS,
        format!(
            "cursor {cursor:?} was not issued for these parameters; repeat the call with the \
             same parameters and the cursor from its result, or start again without a cursor"
        ),
    )
}

/// The name after which a directory page starts, from a cursor issued for `directory`.
fn decode_directory_cursor(bound: &str, cursor: &str) -> Result<String, RpcError> {
    let decoded = cursor
        .strip_prefix("d1.")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(b, _)| *b == bound)
        .and_then(|(_, hex)| from_hex(hex))
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|name| valid_name(name));
    match decoded {
        // Only the exact form this module issues is accepted.
        Some(name) if directory_cursor(bound, &name) == cursor => Ok(name),
        _ => Err(foreign_cursor(cursor)),
    }
}

/// The batch index a page starts at, from a cursor issued for a batch of `len` paths.
fn decode_batch_cursor(bound: &str, len: usize, cursor: &str) -> Result<usize, RpcError> {
    let decoded = cursor
        .strip_prefix("p1.")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(b, _)| *b == bound)
        .and_then(|(_, index)| index.parse::<usize>().ok())
        // A cursor is issued only while entries remain, after at least one.
        .filter(|&i| i >= 1 && i < len);
    match decoded {
        Some(index) if batch_cursor(bound, index) == cursor => Ok(index),
        _ => Err(foreign_cursor(cursor)),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// The child's name, checking that `entry` is a direct child of `directory`.
fn child_name<'a>(directory: &ProtocolPath, entry: &'a ItemStatusEntry) -> Option<&'a str> {
    let dir = directory.as_str().trim_end_matches('/');
    let rest = entry.path.as_str().strip_prefix(dir)?.strip_prefix('/')?;
    valid_name(rest).then_some(rest)
}

fn internal(context: &str, e: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorCode::Internal, format!("{context}: {e}"))
}

fn json_len<T: Serialize + ?Sized>(value: &T) -> Result<usize, RpcError> {
    serde_json::to_vec(value)
        .map(|v| v.len())
        .map_err(|e| internal("cannot serialise a protocol message", e))
}

fn response_len(id: &Id, result: &impl Serialize) -> Result<usize, RpcError> {
    let response = SuccessResponse::new(id.clone(), result)
        .map_err(|e| internal("cannot serialise a protocol message", e))?;
    json_len(&response)
}

/// Fills one `item.status` page from a stream of entries. Exact byte accounting: `base` is
/// the encoded response with no entries and no cursor.
struct PageFill {
    base: usize,
    used: usize,
    entries: Vec<ItemStatusEntry>,
}

impl PageFill {
    /// Adds `entry` if the page still fits with it, reserving room for the cursor that would
    /// follow it when more entries may come. Returns false (adding nothing) otherwise.
    fn try_add(
        &mut self,
        entry: &ItemStatusEntry,
        cursor_if_more: Option<&str>,
    ) -> Result<bool, RpcError> {
        let len = json_len(entry)? + usize::from(!self.entries.is_empty());
        let reserve = cursor_if_more.map_or(0, |c| CURSOR_FIELD_BYTES + c.len());
        if self.base + self.used + len + reserve > MAX_LINE_BYTES {
            return Ok(false);
        }
        self.used += len;
        self.entries.push(entry.clone());
        Ok(true)
    }
}

fn too_large(entry: &ItemStatusEntry) -> RpcError {
    RpcError::new(
        ErrorCode::Internal,
        format!(
            "the status entry for {} is too large to send in one {MAX_LINE_BYTES}-byte line",
            entry.path
        ),
    )
}

/// Encodes the finished page and confirms the line limit (ADR-0020 R4).
fn checked(id: &Id, page: ItemStatusResult) -> Result<ItemStatusResult, RpcError> {
    let total = response_len(id, &page)?;
    if total > MAX_LINE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!("item.status page is {total} bytes, over the {MAX_LINE_BYTES}-byte limit"),
        ));
    }
    Ok(page)
}

/// One page of `item.status` for a directory (ADR-0020 R1 to R4).
///
/// `directory_entry` is the directory's own entry, repeated on every page. Children come from
/// `source` in byte order of name, starting after the cursor's name. The page holds as many
/// children as fit in one line with the response for request `id`; `next_cursor` is set
/// exactly when more children remain, and a page is never empty while more remain.
///
/// # Errors
///
/// `invalid-params` for a cursor not issued for this directory; the source's error;
/// `internal` if the source breaks its contract (order, or a path not directly under the
/// directory) or a single child cannot fit in a line.
pub fn directory_page(
    directory: &ProtocolPath,
    directory_entry: &ItemStatusEntry,
    source: &mut impl DirectorySource,
    cursor: Option<&str>,
    id: &Id,
) -> Result<ItemStatusResult, RpcError> {
    let bound = directory_binding(directory);
    let mut last = cursor
        .map(|c| decode_directory_cursor(&bound, c))
        .transpose()?;
    let mut page = ItemStatusResult {
        directory: Some(directory_entry.clone()),
        entries: Vec::new(),
        next_cursor: None,
    };
    let mut fill = PageFill {
        base: response_len(id, &page)?,
        used: 0,
        entries: Vec::new(),
    };
    let mut buffer: VecDeque<ItemStatusEntry> = VecDeque::new();
    let mut exhausted = false;
    let mut fetched_after = last.clone();
    loop {
        if buffer.len() < 2 && !exhausted {
            let batch = source.children_after(fetched_after.as_deref(), CHUNK)?;
            exhausted = batch.len() < CHUNK;
            for entry in batch {
                let name = child_name(directory, &entry).ok_or_else(|| {
                    internal(
                        "directory source returned an entry that is not a child",
                        &entry.path,
                    )
                })?;
                if fetched_after.as_deref().is_some_and(|after| name <= after) {
                    return Err(internal(
                        "directory source returned children out of name order",
                        &entry.path,
                    ));
                }
                fetched_after = Some(name.to_owned());
                buffer.push_back(entry);
            }
        }
        let Some(entry) = buffer.pop_front() else {
            break;
        };
        let more = !buffer.is_empty() || !exhausted;
        let name = child_name(directory, &entry).unwrap_or_default().to_owned();
        let cursor_after = more.then(|| directory_cursor(&bound, &name));
        if fill.try_add(&entry, cursor_after.as_deref())? {
            last = Some(name);
            continue;
        }
        if fill.entries.is_empty() {
            return Err(too_large(&entry));
        }
        page.next_cursor = last.as_deref().map(|n| directory_cursor(&bound, n));
        break;
    }
    page.entries = fill.entries;
    checked(id, page)
}

/// One page of `item.status` for a batch of paths, in request order (ADR-0020 R3, R4).
///
/// `lookup` gives the entry for one path; it is called only for the paths on this page.
///
/// # Errors
///
/// `invalid-params` for a cursor not issued for this batch; `internal` if a single entry
/// cannot fit in a line.
pub fn batch_page(
    paths: &[ProtocolPath],
    cursor: Option<&str>,
    id: &Id,
    mut lookup: impl FnMut(&ProtocolPath) -> ItemStatusEntry,
) -> Result<ItemStatusResult, RpcError> {
    let bound = batch_binding(paths);
    let start = match cursor {
        None => 0,
        Some(c) => decode_batch_cursor(&bound, paths.len(), c)?,
    };
    let mut page = ItemStatusResult {
        directory: None,
        entries: Vec::new(),
        next_cursor: None,
    };
    let mut fill = PageFill {
        base: response_len(id, &page)?,
        used: 0,
        entries: Vec::new(),
    };
    for (index, path) in paths.iter().enumerate().skip(start) {
        let entry = lookup(path);
        let more = index + 1 < paths.len();
        let cursor_after = more.then(|| batch_cursor(&bound, index + 1));
        if fill.try_add(&entry, cursor_after.as_deref())? {
            continue;
        }
        if fill.entries.is_empty() {
            return Err(too_large(&entry));
        }
        page.next_cursor = Some(batch_cursor(&bound, index));
        break;
    }
    page.entries = fill.entries;
    checked(id, page)
}

/// What the daemon did for one requested path, before the result is bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReport {
    /// The requested path.
    pub path: ProtocolPath,
    /// Refused, or accepted with the skipped descendants the daemon knows of.
    pub outcome: ActionOutcome,
}

/// Builds an action result that fits one line with the response for request `id`
/// (ADR-0020 R4, R6).
///
/// Every requested path keeps its entry. `skipped` becomes a sample, filled round-robin
/// across the paths with as many skipped descendants as fit, possibly none; `skipped_count`
/// is kept exact (at least the number of skipped items supplied). Messages are capped at
/// 1 KiB, and shortened further only if the entries without samples would not fit.
///
/// # Errors
///
/// `internal` if even the entries with empty messages cannot fit, which the request limits
/// of `hello` rule out.
pub fn action_result(id: &Id, reports: Vec<ActionReport>) -> Result<ItemActionResult, RpcError> {
    let mut samples: Vec<VecDeque<SkippedItem>> = Vec::with_capacity(reports.len());
    let mut entries = Vec::with_capacity(reports.len());
    for report in reports {
        let (outcome, sample) = match report.outcome {
            ActionOutcome::Refused(error) => (ActionOutcome::Refused(capped(error, 1024)), None),
            ActionOutcome::Accepted {
                skipped,
                skipped_count,
                bytes_freed,
            } => {
                let count = skipped_count.max(skipped.len() as u64);
                let sample = skipped
                    .into_iter()
                    .map(|s| SkippedItem {
                        path: s.path,
                        error: capped(s.error, 1024),
                    })
                    .collect();
                (
                    ActionOutcome::Accepted {
                        skipped: Vec::new(),
                        skipped_count: count,
                        bytes_freed,
                    },
                    Some(sample),
                )
            }
        };
        samples.push(sample.unwrap_or_default());
        entries.push(ItemActionEntry {
            path: report.path,
            outcome,
        });
    }

    let mut result = ItemActionResult { entries };
    let mut base = response_len(id, &result)?;
    for limit in [128, 0] {
        if base <= MAX_LINE_BYTES {
            break;
        }
        for entry in &mut result.entries {
            if let ActionOutcome::Refused(error) = &mut entry.outcome {
                *error = capped(error.clone(), limit);
            }
        }
        base = response_len(id, &result)?;
    }
    if base > MAX_LINE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!("an action result of {base} bytes cannot fit in one line"),
        ));
    }

    // Round-robin, so one large folder does not take the whole line.
    let mut used = base;
    let mut progressed = true;
    while progressed {
        progressed = false;
        for (entry, sample) in result.entries.iter_mut().zip(samples.iter_mut()) {
            let ActionOutcome::Accepted { skipped, .. } = &mut entry.outcome else {
                continue;
            };
            let Some(item) = sample.pop_front() else {
                continue;
            };
            // `"skipped":[` + `],` around the first item, `,` before later ones.
            let extra = if skipped.is_empty() { 13 } else { 1 };
            let len = json_len(&item)? + extra;
            if used + len <= MAX_LINE_BYTES {
                used += len;
                skipped.push(item);
                progressed = true;
            } else {
                // Keep trying smaller items from this path in later rounds.
                progressed |= !sample.is_empty();
            }
        }
    }

    let total = response_len(id, &result)?;
    if total > MAX_LINE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!("action result is {total} bytes, over the {MAX_LINE_BYTES}-byte limit"),
        ));
    }
    Ok(result)
}

fn capped(mut error: RpcError, max: usize) -> RpcError {
    error.message = truncate_to(&error.message, max);
    error
}

/// Splits one change into `status.changed` notifications that each fit one line
/// (ADR-0020 R4).
///
/// Entries and `refresh` paths keep their order across the notifications. An entry too large
/// for a line on its own is replaced by a `refresh` of its path. Returns nothing when there
/// is nothing to send.
///
/// # Errors
///
/// `internal` if serialisation fails.
pub fn status_changed_notifications(
    entries: Vec<ItemStatusEntry>,
    refresh: Vec<ProtocolPath>,
) -> Result<Vec<Notification>, RpcError> {
    let empty = |e: Vec<ItemStatusEntry>, r: Vec<ProtocolPath>| StatusChangedParams {
        entries: e,
        refresh: r,
    };
    let notification = |params: &StatusChangedParams| {
        Notification::new(method::STATUS_CHANGED, params)
            .map_err(|e| internal("cannot serialise status.changed", e))
    };
    let base = json_len(&notification(&empty(Vec::new(), Vec::new()))?)?;

    let mut out = Vec::new();
    let mut current = empty(Vec::new(), Vec::new());
    let mut used = base;

    let mut refresh: VecDeque<ProtocolPath> = refresh.into();
    for entry in entries {
        let len = json_len(&entry)?;
        if base + len > MAX_LINE_BYTES {
            refresh.push_back(entry.path);
            continue;
        }
        let len = len + usize::from(!current.entries.is_empty());
        if used + len > MAX_LINE_BYTES {
            out.push(notification(&current)?);
            current = empty(Vec::new(), Vec::new());
            used = base;
            let len = json_len(&entry)?;
            used += len;
        } else {
            used += len;
        }
        current.entries.push(entry);
    }
    for path in refresh {
        let len = json_len(&path)? + usize::from(!current.refresh.is_empty());
        if used + len > MAX_LINE_BYTES {
            out.push(notification(&current)?);
            current = empty(Vec::new(), Vec::new());
            used = base + json_len(&path)?;
        } else {
            used += len;
        }
        current.refresh.push(path);
    }
    if !current.entries.is_empty() || !current.refresh.is_empty() {
        out.push(notification(&current)?);
    }
    for n in &out {
        let len = json_len(n)?;
        if len > MAX_LINE_BYTES {
            return Err(RpcError::new(
                ErrorCode::Internal,
                format!("status.changed is {len} bytes, over the {MAX_LINE_BYTES}-byte limit"),
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use mydrive_core::state::{EffectivePin, Hydration, ItemState, Modifiers, Pin, Sync};
    use serde_json::Value;

    use super::*;
    use crate::codec::encode_line;
    use crate::methods::ItemInfo;
    use crate::values::{ItemReason, RefusalReason};

    fn info() -> ItemInfo {
        let state = ItemState {
            hydration: Hydration::Placeholder,
            pin: Pin::Inherit,
            sync: Sync::InSync,
        };
        ItemInfo::new(state, EffectivePin::Unpinned, Modifiers::default())
    }

    fn entry(path: &str) -> ItemStatusEntry {
        ItemStatusEntry::known(ProtocolPath::new(path).unwrap(), info())
    }

    /// An entry whose message pads it out, for exact sizing.
    fn padded(path: &str, pad: usize) -> ItemStatusEntry {
        let mut item = info();
        item.reason = Some(ItemReason::DownloadFailed);
        item.message = Some("m".repeat(pad));
        ItemStatusEntry::known(ProtocolPath::new(path).unwrap(), item)
    }

    /// A directory held in a sorted map, as the store's (parent, name) index would serve it.
    struct MapDir {
        dir: String,
        children: BTreeMap<String, ItemStatusEntry>,
    }

    impl MapDir {
        fn new(dir: &str, names: impl IntoIterator<Item = String>) -> Self {
            let mut d = Self {
                dir: dir.to_owned(),
                children: BTreeMap::new(),
            };
            for name in names {
                d.add(&name);
            }
            d
        }

        fn path(&self, name: &str) -> String {
            format!("{}/{name}", self.dir)
        }

        fn add(&mut self, name: &str) {
            let e = entry(&self.path(name));
            self.children.insert(name.to_owned(), e);
        }

        fn add_entry(&mut self, name: &str, e: ItemStatusEntry) {
            self.children.insert(name.to_owned(), e);
        }

        fn remove(&mut self, name: &str) {
            self.children.remove(name);
        }
    }

    impl DirectorySource for MapDir {
        fn children_after(
            &mut self,
            after: Option<&str>,
            limit: usize,
        ) -> Result<Vec<ItemStatusEntry>, RpcError> {
            use std::ops::Bound;
            let lower = after.map_or(Bound::Unbounded, |a| Bound::Excluded(a.to_owned()));
            Ok(self
                .children
                .range((lower, Bound::Unbounded))
                .take(limit)
                .map(|(_, e)| e.clone())
                .collect())
        }
    }

    fn dir_path(d: &MapDir) -> ProtocolPath {
        ProtocolPath::new(d.dir.clone()).unwrap()
    }

    fn line_len(id: &Id, page: &ItemStatusResult) -> usize {
        let line = encode_line(&SuccessResponse::new(id.clone(), page).unwrap())
            .expect("every page encodes within the line limit");
        line.len() - 1
    }

    /// Runs a cursor loop, calling `between` after each page. Returns every entry received
    /// and the page count, checking ADR-0020 R2 to R4 on every page.
    fn fetch_all(
        d: &mut MapDir,
        id: &Id,
        mut between: impl FnMut(&mut MapDir, usize),
    ) -> (Vec<ItemStatusEntry>, usize) {
        let dir = dir_path(d);
        let dir_entry = entry(&d.dir);
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = directory_page(&dir, &dir_entry, d, cursor.as_deref(), id).unwrap();
            pages += 1;
            assert!(line_len(id, &page) <= MAX_LINE_BYTES);
            out.extend(page.entries.iter().cloned());
            match page.next_cursor {
                Some(next) => {
                    assert!(!next.is_empty());
                    assert!(!page.entries.is_empty(), "empty page while more remain");
                    cursor = Some(next);
                    between(d, pages);
                }
                None => return (out, pages),
            }
        }
    }

    fn names(entries: &[ItemStatusEntry], d: &MapDir) -> Vec<String> {
        entries
            .iter()
            .map(|e| e.path.as_str()[d.dir.len() + 1..].to_owned())
            .collect()
    }

    /// ADR-0020 R2 (and R4): 50,000 children are delivered in full, in byte order of name,
    /// with no page empty while more remain and no entry missing or repeated.
    #[test]
    fn fifty_thousand_children_in_name_order() {
        let mut d = MapDir::new(
            "/home/user/OneDrive/Pictures/2026",
            (0..50_000).map(|i| format!("IMG_{i:05}.jpg")),
        );
        let per_entry = json_len(d.children.values().next().unwrap()).unwrap();
        assert!(per_entry > 150, "realistic entries ({per_entry} bytes)");
        let id = Id::String("a-long-request-identifier-0123456789".into());
        let (got, pages) = fetch_all(&mut d, &id, |_, _| {});
        assert!(pages > 1);
        let expected: Vec<ItemStatusEntry> = d.children.values().cloned().collect();
        assert_eq!(got, expected);
    }

    /// Name order is byte order, not locale order.
    #[test]
    fn name_order_is_byte_order() {
        let mut d = MapDir::new("/m", ["b", "B", "a", "é", "Z", "10", "9"].map(String::from));
        let (got, _) = fetch_all(&mut d, &Id::Number(1), |_, _| {});
        assert_eq!(names(&got, &d), ["10", "9", "B", "Z", "a", "b", "é"]);
    }

    /// ADR-0020 R1: the reviewer's scenario. 10,000 children; after page one the first child
    /// is deleted. With an index cursor an unchanged file was skipped for ever.
    #[test]
    fn deleting_before_the_cursor_skips_nothing() {
        let initial: Vec<String> = (0..10_000).map(|i| format!("f{i:05}")).collect();
        let mut d = MapDir::new("/m/dir", initial.clone());
        let (got, pages) = fetch_all(&mut d, &Id::Number(7), |d, page| {
            if page == 1 {
                d.remove("f00000");
            }
        });
        assert!(pages > 1);
        let got = names(&got, &d);
        let unique: BTreeSet<&String> = got.iter().collect();
        assert_eq!(unique.len(), got.len(), "an entry was repeated");
        assert_eq!(
            got, initial,
            "every file present at the start was delivered once"
        );
    }

    /// ADR-0020 R1: whatever is added, removed or renamed before or after the cursor
    /// between pages, every entry that exists unchanged throughout comes exactly once.
    #[test]
    fn changes_between_pages_never_skip_or_repeat_unchanged_entries() {
        let initial: Vec<String> = (0..20_000).map(|i| format!("f{:05}", i * 2)).collect();
        // Each mutation runs between every pair of pages; it touches names before and after
        // any plausible cursor position.
        type Mutation = fn(&mut MapDir, usize) -> Vec<String>;
        let mutations: [(&str, Mutation); 6] = [
            ("delete before", |d, p| {
                let n = format!("f{:05}", (p - 1) * 2);
                d.remove(&n);
                vec![n]
            }),
            ("delete after", |d, p| {
                let n = format!("f{:05}", 39_998 - p * 2);
                d.remove(&n);
                vec![n]
            }),
            ("add before", |d, p| {
                let n = format!("f{:05}", p * 2 + 1);
                d.add(&n);
                vec![n]
            }),
            ("add after", |d, p| {
                let n = format!("f{:05}", 39_997 - p * 2);
                d.add(&n);
                vec![n]
            }),
            ("rename before to after", |d, p| {
                let from = format!("f{:05}", p * 2);
                let to = format!("z-renamed-{p}");
                d.remove(&from);
                d.add(&to);
                vec![from, to]
            }),
            ("rename after to before", |d, p| {
                let from = format!("f{:05}", 39_990 - p * 2);
                let to = format!("a-renamed-{p}");
                d.remove(&from);
                d.add(&to);
                vec![from, to]
            }),
        ];
        for (label, mutate) in mutations {
            let mut d = MapDir::new("/m/dir", initial.clone());
            let mut touched: BTreeSet<String> = BTreeSet::new();
            let (got, pages) = fetch_all(&mut d, &Id::Number(1), |d, p| {
                touched.extend(mutate(d, p));
            });
            assert!(pages > 2, "{label}: needs several pages");
            let got = names(&got, &d);
            let unique: BTreeSet<&String> = got.iter().collect();
            assert_eq!(unique.len(), got.len(), "{label}: an entry was repeated");
            for name in &initial {
                if !touched.contains(name) {
                    assert!(
                        unique.contains(name),
                        "{label}: unchanged {name} was skipped"
                    );
                }
            }
            assert!(
                got.windows(2).all(|w| w[0] < w[1]),
                "{label}: not in name order"
            );
        }
    }

    /// ADR-0020 R3: a cursor is refused with invalid-params when used with another directory
    /// or batch, and anything the daemon could not have issued is refused.
    #[test]
    fn foreign_and_forged_cursors_are_invalid_params() {
        let mut a = MapDir::new("/m/a", (0..20_000).map(|i| format!("n{i:05}")));
        let mut b = MapDir::new("/m/b", (0..20_000).map(|i| format!("n{i:05}")));
        let id = Id::Number(1);
        let page = directory_page(&dir_path(&a), &entry("/m/a"), &mut a, None, &id).unwrap();
        let cursor = page.next_cursor.unwrap();
        let err =
            directory_page(&dir_path(&b), &entry("/m/b"), &mut b, Some(&cursor), &id).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));

        let bound = directory_binding(&dir_path(&a));
        let forged = [
            String::new(),
            "x".into(),
            "0".into(),
            "d1.".into(),
            format!("d1.{bound}"),
            format!("d1.{bound}."),
            format!("d1.{bound}.6"),
            format!("d1.{bound}.6G"),
            format!("d1.{bound}.4E"),
            format!("d1.{bound}.2f"),
            format!("d1.{bound}.2e2e"),
            format!("d1.{bound}.ff"),
            format!("d1.{}.6e", bound.to_uppercase()),
            format!("d2.{bound}.6e"),
            format!(" {cursor}"),
            format!("{cursor}."),
        ];
        for bad in &forged {
            let err =
                directory_page(&dir_path(&a), &entry("/m/a"), &mut a, Some(bad), &id).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad:?}");
        }
        // The real cursor still works.
        assert!(directory_page(&dir_path(&a), &entry("/m/a"), &mut a, Some(&cursor), &id).is_ok());

        let batch: Vec<ProtocolPath> = (0..10_000)
            .map(|i| ProtocolPath::new(format!("/m/a/n{i:05}")).unwrap())
            .collect();
        let lookup = |p: &ProtocolPath| entry(p.as_str());
        let page = batch_page(&batch, None, &id, lookup).unwrap();
        let cursor = page.next_cursor.unwrap();
        let other: Vec<ProtocolPath> = batch.iter().rev().cloned().collect();
        let err = batch_page(&other, Some(&cursor), &id, lookup).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
        let bound = batch_binding(&batch);
        for bad in [
            format!("p1.{bound}.0"),
            format!("p1.{bound}.10000"),
            format!("p1.{bound}.01"),
            format!("p1.{bound}.+1"),
            format!("p1.{bound}.x"),
        ] {
            let err = batch_page(&batch, Some(&bad), &id, lookup).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
        }
        // A directory cursor is not a batch cursor, nor the reverse.
        let dir_cursor = directory_page(&dir_path(&a), &entry("/m/a"), &mut a, None, &id)
            .unwrap()
            .next_cursor
            .unwrap();
        assert!(batch_page(&batch, Some(&dir_cursor), &id, lookup).is_err());
    }

    /// A batch of paths pages in request order, each path exactly once.
    #[test]
    fn batches_page_in_request_order() {
        let batch: Vec<ProtocolPath> = (0..20_000)
            .rev()
            .map(|i| ProtocolPath::new(format!("/m/p{i:05}")).unwrap())
            .collect();
        let id = Id::Number(3);
        let mut got = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = batch_page(&batch, cursor.as_deref(), &id, |p| entry(p.as_str())).unwrap();
            pages += 1;
            assert!(line_len(&id, &page) <= MAX_LINE_BYTES);
            got.extend(page.entries.into_iter().map(|e| e.path));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert!(pages > 1);
        assert_eq!(got, batch);
    }

    /// Sizing is exact: a page that lands exactly on the line limit is sent whole, and one
    /// byte more moves the last entry to the next page.
    #[test]
    fn page_sizing_is_exact_at_the_limit() {
        let id = Id::Number(1);
        // Two children; the second is padded so the single page is exactly the limit.
        let mut d = MapDir::new("/m", ["a".to_owned()]);
        d.add_entry("b", padded("/m/b", 0));
        let dir = dir_path(&d);
        let dir_entry = entry("/m");
        let page = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        let shortfall = MAX_LINE_BYTES - line_len(&id, &page);
        d.add_entry("b", padded("/m/b", shortfall));
        let page = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        assert_eq!(line_len(&id, &page), MAX_LINE_BYTES, "exactly at the limit");
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.next_cursor, None);

        // One byte over: "b" moves to a second page, and the first page carries a cursor.
        d.add_entry("b", padded("/m/b", shortfall + 1));
        let page = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        assert_eq!(page.entries.len(), 1);
        let next = page.next_cursor.clone().unwrap();
        let page2 = directory_page(&dir, &dir_entry, &mut d, Some(&next), &id).unwrap();
        assert_eq!(page2.entries.len(), 1);
        assert_eq!(page2.next_cursor, None);

        // With a third child after "b", the cursor must fit too: exactly at the limit.
        d.add("c");
        d.add_entry("b", padded("/m/b", 0));
        let probe = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        assert_eq!(probe.entries.len(), 3);
        // Size "b" so that [a, b] plus the cursor after "b" is exactly the limit.
        let two = ItemStatusResult {
            directory: Some(dir_entry.clone()),
            entries: vec![entry("/m/a"), padded("/m/b", 0)],
            next_cursor: Some(directory_cursor(&directory_binding(&dir), "b")),
        };
        let pad = MAX_LINE_BYTES - line_len(&id, &two);
        d.add_entry("b", padded("/m/b", pad));
        let page = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        assert_eq!(line_len(&id, &page), MAX_LINE_BYTES);
        assert_eq!(page.entries.len(), 2);
        assert!(page.next_cursor.is_some());
        d.add_entry("b", padded("/m/b", pad + 1));
        let page = directory_page(&dir, &dir_entry, &mut d, None, &id).unwrap();
        assert_eq!(page.entries.len(), 1);
    }

    /// The cursor form is pinned, and matches the one shown in `docs/protocol/README.md`.
    #[test]
    fn cursor_form_is_stable() {
        let dir = ProtocolPath::new("/home/user/OneDrive/Pictures/2026").unwrap();
        assert_eq!(
            directory_cursor(&directory_binding(&dir), "IMG_2002.jpg"),
            "d1.c8ab48c736ffc35c4d7698ae57bb450d.494d475f323030322e6a7067"
        );
        let bound = directory_binding(&dir);
        assert_eq!(
            decode_directory_cursor(&bound, &directory_cursor(&bound, "é x")).unwrap(),
            "é x"
        );
    }

    #[test]
    fn source_contract_violations_are_internal_errors() {
        struct Bad(Vec<ItemStatusEntry>);
        impl DirectorySource for Bad {
            fn children_after(
                &mut self,
                _: Option<&str>,
                _: usize,
            ) -> Result<Vec<ItemStatusEntry>, RpcError> {
                Ok(std::mem::take(&mut self.0))
            }
        }
        let dir = ProtocolPath::new("/m").unwrap();
        for children in [
            vec![entry("/m/b"), entry("/m/a")],
            vec![entry("/other/a")],
            vec![entry("/m/a/b")],
        ] {
            let err = directory_page(&dir, &entry("/m"), &mut Bad(children), None, &Id::Number(1))
                .unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::Internal));
        }
    }

    fn refused(reason: RefusalReason, message: &str) -> RpcError {
        RpcError::refused(&reason, message)
    }

    /// ADR-0020 R4, R6: the worst case. 100 paths of 4,096 bytes, each a folder with
    /// thousands of skipped descendants with long paths and long messages: the result fits
    /// one line, every path keeps its entry, and `skipped_count` stays exact.
    #[test]
    fn worst_case_action_result_fits_one_line() {
        let id = Id::String("x".repeat(64));
        let reports: Vec<ActionReport> = (0..100)
            .map(|i| {
                let path = format!("/{i:03}{}", "a".repeat(4092));
                let skipped: Vec<SkippedItem> = (0..500)
                    .map(|j| SkippedItem {
                        path: ProtocolPath::new(format!("/{i:03}/{j:04}{}", "b".repeat(500)))
                            .unwrap(),
                        error: refused(RefusalReason::NotInSync, &"é".repeat(2_000)),
                    })
                    .collect();
                ActionReport {
                    path: ProtocolPath::new(path).unwrap(),
                    outcome: ActionOutcome::Accepted {
                        skipped,
                        skipped_count: 7_000,
                        bytes_freed: Some(u64::MAX),
                    },
                }
            })
            .collect();
        let result = action_result(&id, reports).unwrap();
        let response = SuccessResponse::new(id.clone(), &result).unwrap();
        let line = encode_line(&response).expect("fits one line");
        assert!(line.len() - 1 <= MAX_LINE_BYTES);
        assert_eq!(result.entries.len(), 100);
        let mut sampled = 0;
        for entry in &result.entries {
            let ActionOutcome::Accepted {
                skipped,
                skipped_count,
                ..
            } = &entry.outcome
            else {
                panic!("accepted entry changed kind");
            };
            assert_eq!(*skipped_count, 7_000);
            assert!(skipped.len() as u64 <= *skipped_count);
            for s in skipped {
                assert!(s.error.message.len() <= crate::error::MAX_MESSAGE_BYTES);
            }
            sampled += skipped.len();
        }
        assert!(sampled > 0, "some samples fit");
        // Round-robin: the sample is spread over the paths, not taken by the first one.
        let first = match &result.entries[0].outcome {
            ActionOutcome::Accepted { skipped, .. } => skipped.len(),
            ActionOutcome::Refused(_) => 0,
        };
        assert!(first <= sampled / 100 + 1);
        // The wire form re-parses.
        let back: ItemActionResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(back, result);
    }

    /// ADR-0020 R4: refused paths whose messages escape badly are shortened until the result
    /// fits, keeping one entry per path.
    #[test]
    fn refusal_messages_shrink_to_fit() {
        let id = Id::Number(1);
        let reports: Vec<ActionReport> = (0..100)
            .map(|i| ActionReport {
                // Within the 512 KiB path budget.
                path: ProtocolPath::new(format!("/{i:03}{}", "a".repeat(4092))).unwrap(),
                outcome: ActionOutcome::Refused(refused(
                    RefusalReason::InUse,
                    &"\u{1}".repeat(1_024),
                )),
            })
            .collect();
        let result = action_result(&id, reports).unwrap();
        let line = encode_line(&SuccessResponse::new(id, &result).unwrap()).unwrap();
        assert!(line.len() - 1 <= MAX_LINE_BYTES);
        assert_eq!(result.entries.len(), 100);
        assert!(result.entries.iter().all(|e| e.error().is_some()));
    }

    #[test]
    fn small_action_results_keep_every_skipped_item() {
        let skipped: Vec<SkippedItem> = (0..3)
            .map(|j| SkippedItem {
                path: ProtocolPath::new(format!("/m/f/{j}")).unwrap(),
                error: refused(RefusalReason::InUse, "open"),
            })
            .collect();
        let report = ActionReport {
            path: ProtocolPath::new("/m/f").unwrap(),
            outcome: ActionOutcome::Accepted {
                skipped: skipped.clone(),
                skipped_count: 0,
                bytes_freed: Some(10),
            },
        };
        let result = action_result(&Id::Number(1), vec![report]).unwrap();
        assert_eq!(
            result.entries[0].outcome,
            ActionOutcome::Accepted {
                skipped,
                skipped_count: 3,
                bytes_freed: Some(10)
            }
        );
    }

    /// ADR-0020 R4: a large change is split across notifications, each within the limit,
    /// with every entry and refresh path delivered once and in order.
    #[test]
    fn large_changes_split_across_notifications() {
        let entries: Vec<ItemStatusEntry> = (0..20_000)
            .map(|i| entry(&format!("/m/dir/f{i:05}")))
            .collect();
        let refresh: Vec<ProtocolPath> = (0..300)
            .map(|i| ProtocolPath::new(format!("/m/r{i}/{}", "q".repeat(4000))).unwrap())
            .collect();
        let notes = status_changed_notifications(entries.clone(), refresh.clone()).unwrap();
        assert!(notes.len() > 2);
        let mut got_entries = Vec::new();
        let mut got_refresh = Vec::new();
        for n in &notes {
            let line = encode_line(n).unwrap();
            assert!(line.len() - 1 <= MAX_LINE_BYTES);
            let params: StatusChangedParams =
                serde_json::from_value(n.params.clone().unwrap_or(Value::Null)).unwrap();
            got_entries.extend(params.entries);
            got_refresh.extend(params.refresh);
        }
        assert_eq!(got_entries, entries);
        assert_eq!(got_refresh, refresh);
        assert!(
            status_changed_notifications(Vec::new(), Vec::new())
                .unwrap()
                .is_empty()
        );
    }
}
