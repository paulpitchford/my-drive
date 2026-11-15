//! Bounded results: pages of `item.status`, `mount.list` and `account.list`, and action
//! results and change notifications that each fit one line (ADR-0020). Also the confirmation
//! tokens of `mount.add.preview`, which are bound to their request the way cursors are
//! ([`ConfirmationLedger`]).
//!
//! # Cursors
//!
//! Cursors are keyset cursors (ADR-0020 R1 to R3). A directory cursor names the last child
//! returned; the next page starts at the first child whose name sorts after it, by bytes.
//! Mount and account cursors work the same way over `mount_id` and `account_id`. A batch
//! cursor names a position in the request's list of paths. Directory and batch cursors also
//! carry a binding: a hash of the directory path, or of the whole batch, so a cursor used
//! with other parameters is refused with `invalid-params`. Cursors hold no daemon state and
//! never go stale. To clients they are opaque, non-empty strings; this module accepts only
//! cursors in the exact form and version it issues, and refuses anything else with
//! `invalid-params` and reason `restart-listing`.
//!
//! The longest cursors: a directory cursor is 36 + 2 × (name bytes) bytes, so at most 546
//! for a 255-byte name; a batch cursor at most 56 bytes; a mount cursor at most 131 bytes;
//! an account cursor 3 + 2 × (account ID bytes).
//!
//! The pagers read through [`KeysetSource`], which the daemon backs with store queries
//! ("children of this folder with name > X, ordered by name").

use std::collections::VecDeque;

use mydrive_core::ids::MountId;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::codec::MAX_LINE_BYTES;
use crate::envelope::{Id, Notification, SuccessResponse};
use crate::error::{ErrorCode, RpcError, truncate_to};
use crate::methods::{
    AccountInfo, AccountListResult, ActionOutcome, ConfirmationToken, ItemActionEntry,
    ItemActionResult, ItemStatusEntry, ItemStatusResult, MountInfo, MountListResult, MountSource,
    MountTarget, SkippedItem, StatusChangedParams, method,
};
use crate::path::ProtocolPath;
use crate::values::MountTargetKind;

/// How many rows the pagers ask a source for at a time.
const CHUNK: usize = 256;

/// Bytes `,"next_cursor":""` adds to a result around the cursor text itself.
const CURSOR_FIELD_BYTES: usize = r#","next_cursor":"""#.len();

/// Reads rows in ascending byte order of their key, for the keyset pagers.
///
/// The key is a directory child's name, a mount's `mount_id` or an account's `account_id`.
/// The daemon implements this over the state store's indexes; tests implement it over a
/// sorted map.
pub trait KeysetSource<T> {
    /// Up to `limit` rows whose keys sort strictly after `after` (every row when `after` is
    /// `None`), in ascending byte order of key. Returning fewer than `limit` rows does not
    /// mean the end: the pager asks again, and only an empty batch ends the listing. For a
    /// directory, each entry's path is the directory's path joined with the child's name.
    ///
    /// # Errors
    ///
    /// Any error reading the rows; it is returned to the client as is.
    fn rows_after(&mut self, after: Option<&str>, limit: usize) -> Result<Vec<T>, RpcError>;
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty()
        || !text.len().is_multiple_of(2)
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

/// One kind of keyset cursor: `<prefix><hex of the key>`, where the prefix names the kind,
/// the version and, for directories, the binding.
struct KeyCursor {
    prefix: String,
}

impl KeyCursor {
    fn directory(directory: &ProtocolPath) -> Self {
        Self {
            prefix: format!("d1.{}.", directory_binding(directory)),
        }
    }

    fn mounts() -> Self {
        Self {
            prefix: "m1.".to_owned(),
        }
    }

    fn accounts() -> Self {
        Self {
            prefix: "a1.".to_owned(),
        }
    }

    fn encode(&self, key: &str) -> String {
        format!("{}{}", self.prefix, to_hex(key.as_bytes()))
    }

    /// The key a cursor names, accepting only the exact form [`KeyCursor::encode`] issues.
    fn decode(&self, method: &str, cursor: &str) -> Result<String, RpcError> {
        cursor
            .strip_prefix(self.prefix.as_str())
            .and_then(from_hex)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|key| self.encode(key) == cursor)
            .ok_or_else(|| foreign_cursor(method, cursor))
    }
}

/// A batch cursor; `bound` is [`batch_binding`] of the batch.
fn batch_cursor(bound: &str, index: usize) -> String {
    format!("p1.{bound}.{index}")
}

fn foreign_cursor(method: &str, cursor: &str) -> RpcError {
    RpcError::restart_listing(
        method,
        format!(
            "cursor {:?} was not issued for these parameters, or by this version; start \
             the listing again from the first page",
            truncate_to(cursor, 64)
        ),
    )
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
        _ => Err(foreign_cursor(method::ITEM_STATUS, cursor)),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// The child's name, checking that `entry` is a direct child of `directory`.
fn child_name(directory: &ProtocolPath, entry: &ItemStatusEntry) -> Option<String> {
    let dir = directory.as_str().trim_end_matches('/');
    let rest = entry.path.as_str().strip_prefix(dir)?.strip_prefix('/')?;
    valid_name(rest).then(|| rest.to_owned())
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

/// Fills one page from a stream of rows. Exact byte accounting: `base` is the encoded
/// response with no rows and no cursor, and the list is the last field before
/// `next_cursor`.
struct PageFill<T> {
    base: usize,
    used: usize,
    rows: Vec<T>,
}

impl<T: Serialize> PageFill<T> {
    fn new(base: usize) -> Self {
        Self {
            base,
            used: 0,
            rows: Vec::new(),
        }
    }

    /// Adds `row` if the page still fits with it, reserving room for the cursor that would
    /// follow it when more rows may come. Returns false (adding nothing) otherwise.
    fn try_add(&mut self, row: T, cursor_if_more: Option<&str>) -> Result<Option<T>, RpcError> {
        let len = json_len(&row)? + usize::from(!self.rows.is_empty());
        let reserve = cursor_if_more.map_or(0, |c| CURSOR_FIELD_BYTES + c.len());
        if self.base + self.used + len + reserve > MAX_LINE_BYTES {
            return Ok(Some(row));
        }
        self.used += len;
        self.rows.push(row);
        Ok(None)
    }
}

fn too_large(what: &str) -> RpcError {
    RpcError::new(
        ErrorCode::Internal,
        format!("the entry for {what} is too large to send in one {MAX_LINE_BYTES}-byte line"),
    )
}

/// Encodes the finished page and confirms the line limit (ADR-0020 R4).
fn checked<R: Serialize>(id: &Id, page: R) -> Result<R, RpcError> {
    let total = response_len(id, &page)?;
    if total > MAX_LINE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Internal,
            format!("a result page is {total} bytes, over the {MAX_LINE_BYTES}-byte limit"),
        ));
    }
    Ok(page)
}

/// The keyset loop shared by every keyed listing: fills a page from `source` after `start`,
/// looking one row ahead so a cursor is reserved only when more rows really follow, and
/// ending only on an empty batch from the source.
fn keyset_fill<T: Serialize>(
    base: usize,
    source: &mut impl KeysetSource<T>,
    start: Option<String>,
    key_of: impl Fn(&T) -> Option<String>,
    cursor: &KeyCursor,
) -> Result<(Vec<T>, Option<String>), RpcError> {
    let mut fill = PageFill::new(base);
    let mut buffer: VecDeque<(String, T)> = VecDeque::new();
    let mut exhausted = false;
    let mut fetched_after = start;
    let mut last: Option<String> = None;
    loop {
        // Keep one row of lookahead, so we know whether more follow the current row.
        while buffer.len() < 2 && !exhausted {
            let batch = source.rows_after(fetched_after.as_deref(), CHUNK)?;
            exhausted = batch.is_empty();
            for row in batch {
                let key = key_of(&row)
                    .ok_or_else(|| internal("source returned a row with an invalid key", ""))?;
                if fetched_after
                    .as_deref()
                    .is_some_and(|after| key.as_str() <= after)
                {
                    return Err(internal("source returned rows out of key order", &key));
                }
                fetched_after = Some(key.clone());
                buffer.push_back((key, row));
            }
        }
        let Some((key, row)) = buffer.pop_front() else {
            return Ok((fill.rows, None));
        };
        let more = !buffer.is_empty() || !exhausted;
        let cursor_after = more.then(|| cursor.encode(&key));
        match fill.try_add(row, cursor_after.as_deref())? {
            None => last = Some(key),
            Some(_) if fill.rows.is_empty() => return Err(too_large(&key)),
            Some(_) => {
                let next = last.as_deref().map(|k| cursor.encode(k));
                return Ok((fill.rows, next));
            }
        }
    }
}

/// One page of `item.status` for a directory (ADR-0020 R1 to R4).
///
/// `directory_entry` is the directory's own entry, repeated on every page; `seq` is the
/// change sequence number the page was read at. Children come from `source` in byte order of
/// name, starting after the cursor's name. The page holds as many children as fit in one
/// line with the response for request `id`; `next_cursor` is set exactly when more children
/// remain, and a page is never empty while more remain. The cursor is bound to the
/// directory's path.
///
/// # Errors
///
/// `invalid-params` with reason `restart-listing` for a cursor not issued for this
/// directory; the source's error; `internal` if the source breaks its contract (order, or a
/// path not directly under the directory) or a single child cannot fit in a line.
pub fn directory_page(
    directory: &ProtocolPath,
    directory_entry: &ItemStatusEntry,
    source: &mut impl KeysetSource<ItemStatusEntry>,
    cursor: Option<&str>,
    seq: Option<u64>,
    id: &Id,
) -> Result<ItemStatusResult, RpcError> {
    let keys = KeyCursor::directory(directory);
    let start = cursor
        .map(|c| keys.decode(method::ITEM_STATUS, c))
        .transpose()?;
    if start.as_deref().is_some_and(|name| !valid_name(name)) {
        return Err(foreign_cursor(method::ITEM_STATUS, cursor.unwrap_or("")));
    }
    let mut page = ItemStatusResult {
        directory: Some(directory_entry.clone()),
        seq,
        entries: Vec::new(),
        next_cursor: None,
    };
    let base = response_len(id, &page)?;
    let (entries, next) = keyset_fill(base, source, start, |e| child_name(directory, e), &keys)?;
    page.entries = entries;
    page.next_cursor = next;
    checked(id, page)
}

/// One page of `item.status` for a batch of paths, in request order (ADR-0020 R3, R4).
///
/// `lookup` gives the entry for one path; it is called only for the paths on this page.
///
/// # Errors
///
/// `invalid-params` with reason `restart-listing` for a cursor not issued for this batch;
/// `internal` if a single entry cannot fit in a line.
pub fn batch_page(
    paths: &[ProtocolPath],
    cursor: Option<&str>,
    seq: Option<u64>,
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
        seq,
        entries: Vec::new(),
        next_cursor: None,
    };
    let mut fill = PageFill::new(response_len(id, &page)?);
    for (index, path) in paths.iter().enumerate().skip(start) {
        let more = index + 1 < paths.len();
        let cursor_after = more.then(|| batch_cursor(&bound, index + 1));
        if fill
            .try_add(lookup(path), cursor_after.as_deref())?
            .is_some()
        {
            if fill.rows.is_empty() {
                return Err(too_large(path.as_str()));
            }
            page.next_cursor = Some(batch_cursor(&bound, index));
            break;
        }
    }
    page.entries = fill.rows;
    checked(id, page)
}

/// One page of `mount.list`, in byte order of `mount_id` (ADR-0020 R4).
///
/// # Errors
///
/// `invalid-params` with reason `restart-listing` for a cursor this daemon did not issue;
/// the source's error; `internal` if the source breaks key order or one mount cannot fit.
pub fn mount_page(
    source: &mut impl KeysetSource<MountInfo>,
    cursor: Option<&str>,
    id: &Id,
) -> Result<MountListResult, RpcError> {
    let keys = KeyCursor::mounts();
    let start = cursor
        .map(|c| keys.decode(method::MOUNT_LIST, c))
        .transpose()?;
    // A cursor naming a key no mount can have was not issued by this daemon.
    if start
        .as_deref()
        .is_some_and(|key| MountId::new(key).is_err())
    {
        return Err(foreign_cursor(method::MOUNT_LIST, cursor.unwrap_or("")));
    }
    let mut page = MountListResult {
        mounts: Vec::new(),
        next_cursor: None,
    };
    let base = response_len(id, &page)?;
    let key = |m: &MountInfo| Some(m.mount_id.as_str().to_owned());
    let (mounts, next) = keyset_fill(base, source, start, key, &keys)?;
    page.mounts = mounts;
    page.next_cursor = next;
    checked(id, page)
}

/// One page of `account.list`, in byte order of `account_id` (ADR-0020 R4).
///
/// # Errors
///
/// As for [`mount_page`].
pub fn account_page(
    source: &mut impl KeysetSource<AccountInfo>,
    cursor: Option<&str>,
    id: &Id,
) -> Result<AccountListResult, RpcError> {
    let keys = KeyCursor::accounts();
    let start = cursor
        .map(|c| keys.decode(method::ACCOUNT_LIST, c))
        .transpose()?;
    let mut page = AccountListResult {
        accounts: Vec::new(),
        next_cursor: None,
    };
    let base = response_len(id, &page)?;
    let key = |a: &AccountInfo| Some(a.account_id.as_str().to_owned());
    let (accounts, next) = keyset_fill(base, source, start, key, &keys)?;
    page.accounts = accounts;
    page.next_cursor = next;
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

/// Confirms that a single (unpaged) result fits one line with the response for request `id`
/// (ADR-0020 R4), returning it unchanged.
///
/// Every text field a result carries for display is capped at 1 KiB, but the identifiers
/// inside it (`account_id`, `tenant_id`, `drive_id`) have no protocol length limit. A result
/// that would not fit is never sent; the daemon answers `internal` instead.
///
/// # Errors
///
/// `internal` if the encoded response is over the line limit, or cannot be encoded.
pub fn fit_result<R: Serialize>(id: &Id, result: R) -> Result<R, RpcError> {
    checked(id, result)
}

/// How long a preview's confirmation token stays valid, in seconds.
pub const PREVIEW_LIFETIME_SECS: u64 = 600;

/// The most previews awaiting confirmation at once. Issuing one more forgets the oldest, whose
/// token is then refused with `restart-preview`.
pub const MAX_PENDING_PREVIEWS: usize = 32;

struct PendingPreview<T> {
    nonce: u64,
    binding: String,
    issued_at: u64,
    payload: T,
}

/// The previews awaiting `mount.add.confirm` (WP-016 AC6).
///
/// [`ConfirmationLedger::issue`] gives a token bound to the preview's target;
/// [`ConfirmationLedger::redeem`] accepts it once, with the same target, within
/// [`PREVIEW_LIFETIME_SECS`]. Anything else is `invalid-params` with reason
/// `restart-preview`: a token used twice, with another target, after it expired, after the
/// ledger forgot it, or one this ledger did not issue (including one from before a daemon
/// restart).
///
/// A token is `c1.` + a 16-hex-digit sequence number + `.` + 16 hex digits of a SHA-256
/// binding over the number and the target. It holds no daemon state and nothing readable of
/// the target: a sharing link enters only the hash (ADR-0010 R6). It is not a secret: it only
/// stops a confirmation from attaching something the user was not shown.
///
/// `payload` is whatever the engine needs to finish the mount (the resolved drive); `now_secs`
/// comes from the daemon's clock, so the ledger itself reads no clock.
pub struct ConfirmationLedger<T> {
    next: u64,
    pending: VecDeque<PendingPreview<T>>,
}

impl<T> std::fmt::Debug for ConfirmationLedger<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmationLedger")
            .field("next", &self.next)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl<T> Default for ConfirmationLedger<T> {
    fn default() -> Self {
        Self::new()
    }
}

fn target_binding(nonce: u64, target: &MountTarget) -> String {
    let nonce = format!("{nonce:016x}");
    let (kind, value) = match &target.source {
        MountSource::OwnDrive => (MountTargetKind::OwnDrive, ""),
        MountSource::Drive(drive) => (MountTargetKind::DriveId, drive.as_str()),
        MountSource::SharingLink(link) => (MountTargetKind::SharingLink, link.expose()),
    };
    let mut bound = binding(
        "mount-target",
        &[&nonce, target.account_id.as_str(), kind.as_str(), value],
    );
    bound.truncate(16);
    bound
}

fn confirmation_token(nonce: u64, binding: &str) -> Result<ConfirmationToken, RpcError> {
    ConfirmationToken::new(format!("c1.{nonce:016x}.{binding}"))
        .map_err(|e| internal("cannot build a confirmation token", e))
}

/// The sequence number and binding a token names, accepting only the exact form issued.
fn parse_token(token: &ConfirmationToken) -> Option<(u64, &str)> {
    let rest = token.as_str().strip_prefix("c1.")?;
    let (nonce, bound) = rest.split_once('.')?;
    let lower_hex =
        |s: &str| s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !lower_hex(nonce) || !lower_hex(bound) {
        return None;
    }
    u64::from_str_radix(nonce, 16).ok().map(|n| (n, bound))
}

impl<T> ConfirmationLedger<T> {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: 0,
            pending: VecDeque::new(),
        }
    }

    /// How many previews await confirmation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// True when no preview awaits confirmation.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn forget_expired(&mut self, now_secs: u64) {
        self.pending
            .retain(|p| now_secs.saturating_sub(p.issued_at) < PREVIEW_LIFETIME_SECS);
    }

    /// Records a preview of `target` and returns its token.
    ///
    /// # Errors
    ///
    /// `internal` only if the token cannot be built, which its fixed form rules out.
    pub fn issue(
        &mut self,
        target: &MountTarget,
        payload: T,
        now_secs: u64,
    ) -> Result<ConfirmationToken, RpcError> {
        self.forget_expired(now_secs);
        while self.pending.len() >= MAX_PENDING_PREVIEWS {
            self.pending.pop_front();
        }
        let nonce = self.next;
        self.next = self.next.wrapping_add(1);
        let binding = target_binding(nonce, target);
        let token = confirmation_token(nonce, &binding)?;
        self.pending.push_back(PendingPreview {
            nonce,
            binding,
            issued_at: now_secs,
            payload,
        });
        Ok(token)
    }

    /// Accepts `token` for `target` once, returning the preview's payload.
    ///
    /// A token offered with another target is refused and stays valid for its own target,
    /// since nothing happened. A redeemed or expired token is forgotten.
    ///
    /// # Errors
    ///
    /// `invalid-params` with reason `restart-preview` for anything but an outstanding token
    /// issued for `target`. The error repeats neither the token nor the target.
    pub fn redeem(
        &mut self,
        token: &ConfirmationToken,
        target: &MountTarget,
        now_secs: u64,
    ) -> Result<T, RpcError> {
        self.forget_expired(now_secs);
        let refused = || RpcError::restart_preview(method::MOUNT_ADD_CONFIRM);
        let (nonce, bound) = parse_token(token).ok_or_else(refused)?;
        let index = self
            .pending
            .iter()
            .position(|p| p.nonce == nonce && p.binding == bound)
            .ok_or_else(refused)?;
        if target_binding(nonce, target) != bound {
            return Err(refused());
        }
        self.pending
            .remove(index)
            .map(|p| p.payload)
            .ok_or_else(refused)
    }
}

/// Splits one change, at change sequence number `seq`, into `status.changed` notifications
/// that each fit one line
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
    seq: Option<u64>,
) -> Result<Vec<Notification>, RpcError> {
    let empty = |e: Vec<ItemStatusEntry>, r: Vec<ProtocolPath>| StatusChangedParams {
        seq,
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
    use crate::values::RefusalReason;

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
    /// Messages are capped, so pad with an unknown modifier, an open value of any length that
    /// grows the encoding byte for byte.
    fn padded(path: &str, pad: usize) -> ItemStatusEntry {
        let mut item = info();
        item.modifiers =
            crate::values::WireModifiers(vec![crate::values::Modifier::Other("m".repeat(pad))]);
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

    impl KeysetSource<ItemStatusEntry> for MapDir {
        fn rows_after(
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
            let page = directory_page(&dir, &dir_entry, d, cursor.as_deref(), None, id).unwrap();
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
        let page = directory_page(&dir_path(&a), &entry("/m/a"), &mut a, None, None, &id).unwrap();
        let cursor = page.next_cursor.unwrap();
        let err = directory_page(
            &dir_path(&b),
            &entry("/m/b"),
            &mut b,
            Some(&cursor),
            None,
            &id,
        )
        .unwrap_err();
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
            let err = directory_page(&dir_path(&a), &entry("/m/a"), &mut a, Some(bad), None, &id)
                .unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad:?}");
        }
        // The real cursor still works.
        assert!(
            directory_page(
                &dir_path(&a),
                &entry("/m/a"),
                &mut a,
                Some(&cursor),
                None,
                &id
            )
            .is_ok()
        );

        let batch: Vec<ProtocolPath> = (0..10_000)
            .map(|i| ProtocolPath::new(format!("/m/a/n{i:05}")).unwrap())
            .collect();
        let lookup = |p: &ProtocolPath| entry(p.as_str());
        let page = batch_page(&batch, None, None, &id, lookup).unwrap();
        let cursor = page.next_cursor.unwrap();
        let other: Vec<ProtocolPath> = batch.iter().rev().cloned().collect();
        let err = batch_page(&other, Some(&cursor), None, &id, lookup).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
        let bound = batch_binding(&batch);
        for bad in [
            format!("p1.{bound}.0"),
            format!("p1.{bound}.10000"),
            format!("p1.{bound}.01"),
            format!("p1.{bound}.+1"),
            format!("p1.{bound}.x"),
        ] {
            let err = batch_page(&batch, Some(&bad), None, &id, lookup).unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
        }
        // A directory cursor is not a batch cursor, nor the reverse.
        let dir_cursor = directory_page(&dir_path(&a), &entry("/m/a"), &mut a, None, None, &id)
            .unwrap()
            .next_cursor
            .unwrap();
        assert!(batch_page(&batch, Some(&dir_cursor), None, &id, lookup).is_err());
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
            let page =
                batch_page(&batch, cursor.as_deref(), None, &id, |p| entry(p.as_str())).unwrap();
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
        let page = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        let shortfall = MAX_LINE_BYTES - line_len(&id, &page);
        d.add_entry("b", padded("/m/b", shortfall));
        let page = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        assert_eq!(line_len(&id, &page), MAX_LINE_BYTES, "exactly at the limit");
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.next_cursor, None);

        // One byte over: "b" moves to a second page, and the first page carries a cursor.
        d.add_entry("b", padded("/m/b", shortfall + 1));
        let page = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        assert_eq!(page.entries.len(), 1);
        let next = page.next_cursor.clone().unwrap();
        let page2 = directory_page(&dir, &dir_entry, &mut d, Some(&next), None, &id).unwrap();
        assert_eq!(page2.entries.len(), 1);
        assert_eq!(page2.next_cursor, None);

        // With a third child after "b", the cursor must fit too: exactly at the limit.
        d.add("c");
        d.add_entry("b", padded("/m/b", 0));
        let probe = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        assert_eq!(probe.entries.len(), 3);
        // Size "b" so that [a, b] plus the cursor after "b" is exactly the limit.
        let two = ItemStatusResult {
            directory: Some(dir_entry.clone()),
            seq: None,
            entries: vec![entry("/m/a"), padded("/m/b", 0)],
            next_cursor: Some(KeyCursor::directory(&dir).encode("b")),
        };
        let pad = MAX_LINE_BYTES - line_len(&id, &two);
        d.add_entry("b", padded("/m/b", pad));
        let page = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        assert_eq!(line_len(&id, &page), MAX_LINE_BYTES);
        assert_eq!(page.entries.len(), 2);
        assert!(page.next_cursor.is_some());
        d.add_entry("b", padded("/m/b", pad + 1));
        let page = directory_page(&dir, &dir_entry, &mut d, None, None, &id).unwrap();
        assert_eq!(page.entries.len(), 1);
    }

    /// The cursor form is pinned, and matches the one shown in `docs/protocol/README.md`.
    #[test]
    fn cursor_form_is_stable() {
        let dir = ProtocolPath::new("/home/user/OneDrive/Pictures/2026").unwrap();
        assert_eq!(
            KeyCursor::directory(&dir).encode("IMG_2002.jpg"),
            "d1.c8ab48c736ffc35c4d7698ae57bb450d.494d475f323030322e6a7067"
        );
        assert_eq!(
            KeyCursor::mounts().encode("example-site-documents"),
            "m1.6578616d706c652d736974652d646f63756d656e7473"
        );
        // Longest cursors, as documented: 36 + 2 × name bytes, 56 for a batch, 131 for a
        // mount.
        assert_eq!(
            KeyCursor::directory(&dir).encode(&"n".repeat(255)).len(),
            546
        );
        assert_eq!(batch_cursor(&"f".repeat(32), usize::MAX).len(), 56);
        assert_eq!(KeyCursor::mounts().encode(&"m".repeat(64)).len(), 131);
        // The catalogue's sizing assumptions: a batch of at most 10,000 paths gives cursors of
        // at most 40 bytes, and an account cursor is 3 + 2 × the account ID's bytes.
        let last_index = crate::methods::MAX_STATUS_PATHS as usize - 1;
        assert_eq!(batch_cursor(&"f".repeat(32), last_index).len(), 40);
        assert_eq!(KeyCursor::accounts().encode(&"é".repeat(100)).len(), 403);
        let keys = KeyCursor::directory(&dir);
        assert_eq!(
            keys.decode(method::ITEM_STATUS, &keys.encode("é x"))
                .unwrap(),
            "é x"
        );
    }

    #[test]
    fn source_contract_violations_are_internal_errors() {
        struct Bad(Vec<ItemStatusEntry>);
        impl KeysetSource<ItemStatusEntry> for Bad {
            fn rows_after(
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
            let err = directory_page(
                &dir,
                &entry("/m"),
                &mut Bad(children),
                None,
                None,
                &Id::Number(1),
            )
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
    /// fits, keeping one entry per path. The verifier's worst case: 100 escape-heavy paths
    /// filling the action path-byte limit, every one refused with a 1 KiB message of
    /// characters JSON escapes six-fold. With 1 KiB messages the result is over the line, so
    /// the shrinking really runs.
    #[test]
    fn refusal_messages_shrink_to_fit() {
        let id = Id::String("\u{1}".repeat(crate::envelope::MAX_ID_BYTES));
        // Each path is "/NNN" plus quotes, sized so the 100 paths together take as many
        // encoded bytes as the limit allows.
        let per_path = usize::try_from(crate::methods::MAX_ACTION_PATH_BYTES).unwrap() / 100;
        let quotes = (per_path - 2 - 4) / 2;
        let paths: Vec<ProtocolPath> = (0..100)
            .map(|i| ProtocolPath::new(format!("/{i:03}{}", "\"".repeat(quotes))).unwrap())
            .collect();
        let path_bytes = crate::methods::encoded_path_bytes(&paths);
        assert!(path_bytes <= crate::methods::MAX_ACTION_PATH_BYTES);
        assert!(path_bytes > crate::methods::MAX_ACTION_PATH_BYTES - 1_000);
        // The request itself is within the hello limits.
        let params = serde_json::json!({"paths": paths});
        assert!(crate::methods::MethodCall::parse(method::ITEM_DEHYDRATE, Some(&params)).is_ok());

        let reports: Vec<ActionReport> = paths
            .iter()
            .map(|path| ActionReport {
                path: path.clone(),
                outcome: ActionOutcome::Refused(refused(
                    RefusalReason::InUse,
                    &"\u{1}".repeat(1_024),
                )),
            })
            .collect();
        // Without shrinking, the result would not fit.
        let unshrunk = ItemActionResult {
            entries: reports
                .iter()
                .map(|r| ItemActionEntry {
                    path: r.path.clone(),
                    outcome: r.outcome.clone(),
                })
                .collect(),
        };
        assert!(response_len(&id, &unshrunk).unwrap() > MAX_LINE_BYTES);

        let result = action_result(&id, reports).unwrap();
        let line = encode_line(&SuccessResponse::new(id, &result).unwrap()).unwrap();
        assert!(line.len() - 1 <= MAX_LINE_BYTES);
        assert_eq!(result.entries.len(), 100);
        for (entry, path) in result.entries.iter().zip(&paths) {
            assert_eq!(&entry.path, path, "each path keeps its own entry, in order");
            let error = entry.error().expect("each refusal is kept");
            assert_eq!(error.refusal_reason(), Some(RefusalReason::InUse));
        }
    }

    /// ADR-0020 R4: an entry too large for one `status.changed` line is replaced by a
    /// `refresh` of its path, and the other entries are still delivered.
    #[test]
    fn oversize_change_entries_become_refresh() {
        let small = entry("/m/dir/small");
        let mut item = info();
        // An unknown modifier of 2 MiB: an open value, so nothing caps it.
        item.modifiers = crate::values::WireModifiers(vec![crate::values::Modifier::Other(
            "x".repeat(2 * MAX_LINE_BYTES),
        )]);
        let huge_path = ProtocolPath::new("/m/dir/huge").unwrap();
        let huge = ItemStatusEntry::known(huge_path.clone(), item);
        let notes = status_changed_notifications(
            vec![small.clone(), huge, small.clone()],
            Vec::new(),
            Some(9),
        )
        .unwrap();
        let mut entries = Vec::new();
        let mut refresh = Vec::new();
        for n in &notes {
            assert!(encode_line(n).unwrap().len() - 1 <= MAX_LINE_BYTES);
            let params: StatusChangedParams =
                serde_json::from_value(n.params.clone().unwrap_or(Value::Null)).unwrap();
            assert_eq!(params.seq, Some(9));
            entries.extend(params.entries);
            refresh.extend(params.refresh);
        }
        assert_eq!(entries, [small.clone(), small]);
        assert_eq!(refresh, [huge_path]);
    }

    /// ADR-0019 R3: `next_cursor` is absent on the last page and present on every earlier
    /// page, for directory, batch, mount and account listings.
    #[test]
    fn adr_0019_r3_next_cursor_is_absent_only_on_the_last_page() {
        let id = Id::Number(1);
        // Directory.
        let mut d = MapDir::new("/m/dir", (0..30_000).map(|i| format!("f{i:05}")));
        let total = d.children.len();
        let dir = dir_path(&d);
        let dir_entry = entry("/m/dir");
        let (mut delivered, mut cursor, mut pages) = (0, None::<String>, 0);
        loop {
            let page =
                directory_page(&dir, &dir_entry, &mut d, cursor.as_deref(), None, &id).unwrap();
            pages += 1;
            delivered += page.entries.len();
            if delivered < total {
                assert!(
                    page.next_cursor.is_some(),
                    "directory page {pages}: cursor missing"
                );
                cursor = page.next_cursor;
            } else {
                assert_eq!(page.next_cursor, None, "directory: cursor on the last page");
                break;
            }
        }
        assert!(pages > 2);

        // Batch.
        let batch: Vec<ProtocolPath> = (0..20_000)
            .map(|i| ProtocolPath::new(format!("/m/p{i:05}")).unwrap())
            .collect();
        let (mut delivered, mut cursor, mut pages) = (0, None::<String>, 0);
        loop {
            let page =
                batch_page(&batch, cursor.as_deref(), None, &id, |p| entry(p.as_str())).unwrap();
            pages += 1;
            delivered += page.entries.len();
            if delivered < batch.len() {
                assert!(
                    page.next_cursor.is_some(),
                    "batch page {pages}: cursor missing"
                );
                cursor = page.next_cursor;
            } else {
                assert_eq!(page.next_cursor, None, "batch: cursor on the last page");
                break;
            }
        }
        assert!(pages > 1);

        // Mounts.
        let mounts: Vec<MountInfo> = (0..300).map(worst_mount).collect();
        let total = mounts.len();
        let mut source = MountSource(mounts);
        let (mut delivered, mut cursor, mut pages) = (0, None::<String>, 0);
        loop {
            let page = mount_page(&mut source, cursor.as_deref(), &id).unwrap();
            pages += 1;
            delivered += page.mounts.len();
            if delivered < total {
                assert!(
                    page.next_cursor.is_some(),
                    "mount page {pages}: cursor missing"
                );
                cursor = page.next_cursor;
            } else {
                assert_eq!(page.next_cursor, None, "mounts: cursor on the last page");
                break;
            }
        }
        assert!(pages > 1);

        // A listing that fits one page has no cursor at all.
        let mut small = MapDir::new("/m/s", ["a".to_owned(), "b".to_owned()]);
        let small_dir = dir_path(&small);
        let page = directory_page(&small_dir, &entry("/m/s"), &mut small, None, None, &id).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.next_cursor, None);
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
        let notes =
            status_changed_notifications(entries.clone(), refresh.clone(), Some(7)).unwrap();
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
            status_changed_notifications(Vec::new(), Vec::new(), None)
                .unwrap()
                .is_empty()
        );
    }

    /// A source that returns at most `cap` rows per call, as a store with a page limit would.
    struct Capped<'a> {
        inner: &'a mut MapDir,
        cap: usize,
    }

    impl KeysetSource<ItemStatusEntry> for Capped<'_> {
        fn rows_after(
            &mut self,
            after: Option<&str>,
            limit: usize,
        ) -> Result<Vec<ItemStatusEntry>, RpcError> {
            self.inner.rows_after(after, limit.min(self.cap))
        }
    }

    /// A short batch from the source is not the end of the listing: only an empty batch is.
    #[test]
    fn short_batches_from_the_source_do_not_end_the_listing() {
        let mut d = MapDir::new("/m/dir", (0..1_000).map(|i| format!("f{i:04}")));
        let expected: Vec<ItemStatusEntry> = d.children.values().cloned().collect();
        let dir = dir_path(&d);
        let dir_entry = entry("/m/dir");
        for cap in [1, 7, 100] {
            let mut source = Capped { inner: &mut d, cap };
            let page = directory_page(&dir, &dir_entry, &mut source, None, Some(3), &Id::Number(1))
                .unwrap();
            assert_eq!(page.entries, expected, "cap {cap}");
            assert_eq!(page.next_cursor, None);
            assert_eq!(page.seq, Some(3));
        }
    }

    /// Every refused cursor, including one of an unknown version, says to restart the
    /// listing.
    #[test]
    fn refused_cursors_say_restart_listing() {
        let mut d = MapDir::new("/m/a", (0..10).map(|i| format!("n{i}")));
        let dir = dir_path(&d);
        let keys = KeyCursor::directory(&dir);
        let good = keys.encode("n3");
        let old_version = good.replacen("d1.", "d0.", 1);
        let future = good.replacen("d1.", "d9.", 1);
        for bad in [old_version, future, "x".into(), "m1.6e33".into()] {
            let err = directory_page(
                &dir,
                &entry("/m/a"),
                &mut d,
                Some(&bad),
                None,
                &Id::Number(1),
            )
            .unwrap_err();
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{bad}");
            assert_eq!(
                err.invalid_params_reason(),
                Some(crate::values::InvalidParamsReason::RestartListing),
                "{bad}"
            );
        }
        assert!(
            directory_page(
                &dir,
                &entry("/m/a"),
                &mut d,
                Some(&good),
                None,
                &Id::Number(1)
            )
            .is_ok()
        );
        let err =
            mount_page(&mut MountSource(Vec::new()), Some("m0.6d"), &Id::Number(1)).unwrap_err();
        assert_eq!(err.reason_code().as_deref(), Some("restart-listing"));
    }

    /// ADR-0020 R3: a forged mount or account cursor naming a key that cannot exist (the
    /// empty ID, which neither `MountId` nor `AccountId` admits, or a key that breaks a
    /// `MountId` rule) is refused with `restart-listing`, and the source is never asked for
    /// rows after it.
    #[test]
    fn adr_0020_r3_forged_list_cursors_naming_impossible_keys_are_refused() {
        struct NeverRead;
        impl KeysetSource<MountInfo> for NeverRead {
            fn rows_after(
                &mut self,
                after: Option<&str>,
                _: usize,
            ) -> Result<Vec<MountInfo>, RpcError> {
                panic!("source read after a forged mount cursor ({after:?})");
            }
        }
        impl KeysetSource<AccountInfo> for NeverRead {
            fn rows_after(
                &mut self,
                after: Option<&str>,
                _: usize,
            ) -> Result<Vec<AccountInfo>, RpcError> {
                panic!("source read after a forged account cursor ({after:?})");
            }
        }
        let id = Id::Number(1);
        let check = |err: RpcError, cursor: &str| {
            assert_eq!(err.kind(), Some(ErrorCode::InvalidParams), "{cursor:?}");
            assert_eq!(
                err.reason_code().as_deref(),
                Some("restart-listing"),
                "{cursor:?}"
            );
        };
        for cursor in ["m1.", "m1"] {
            check(
                mount_page(&mut NeverRead, Some(cursor), &id).unwrap_err(),
                cursor,
            );
        }
        // Well-formed cursors whose key breaks a `MountId` rule: uppercase, a leading `-`,
        // longer than 64 characters, a `/`, a non-ASCII character.
        let too_long = "z".repeat(65);
        for key in ["UPPER", "-lead", too_long.as_str(), "a/b", "é"] {
            assert!(MountId::new(key).is_err(), "{key:?} must be impossible");
            let cursor = KeyCursor::mounts().encode(key);
            check(
                mount_page(&mut NeverRead, Some(&cursor), &id).unwrap_err(),
                &cursor,
            );
        }
        // The longest valid key still decodes, so the check refuses only impossible keys.
        let longest = KeyCursor::mounts().encode(&"z".repeat(64));
        assert!(mount_page(&mut MountSource(Vec::new()), Some(&longest), &id).is_ok());
        // Every non-empty string is a possible `AccountId`, so the empty key is the only
        // impossible account cursor key.
        for cursor in ["a1.", "a1"] {
            check(
                account_page(&mut NeverRead, Some(cursor), &id).unwrap_err(),
                cursor,
            );
        }
    }

    struct MountSource(Vec<MountInfo>);

    impl KeysetSource<MountInfo> for MountSource {
        fn rows_after(
            &mut self,
            after: Option<&str>,
            limit: usize,
        ) -> Result<Vec<MountInfo>, RpcError> {
            Ok(self
                .0
                .iter()
                .filter(|m| after.is_none_or(|a| m.mount_id.as_str() > a))
                .take(limit)
                .cloned()
                .collect())
        }
    }

    struct AccountSource(Vec<AccountInfo>);

    impl KeysetSource<AccountInfo> for AccountSource {
        fn rows_after(
            &mut self,
            after: Option<&str>,
            limit: usize,
        ) -> Result<Vec<AccountInfo>, RpcError> {
            Ok(self
                .0
                .iter()
                .filter(|a| after.is_none_or(|k| a.account_id.as_str() > k))
                .take(limit)
                .cloned()
                .collect())
        }
    }

    fn mount_target(source: crate::methods::MountSource) -> MountTarget {
        MountTarget {
            account_id: mydrive_core::ids::AccountId::new("acct-work-1").unwrap(),
            source,
        }
    }

    const LINK: &str = "https://example.sharepoint.com/:f:/s/Finance/EjK3pQ2-planted-link";

    fn link(text: &str) -> MountTarget {
        mount_target(crate::methods::MountSource::SharingLink(
            crate::methods::SharingLink::new(text).unwrap(),
        ))
    }

    fn drive(id: &str) -> MountTarget {
        mount_target(crate::methods::MountSource::Drive(
            mydrive_core::ids::DriveId::new(id).unwrap(),
        ))
    }

    fn is_restart_preview(err: &RpcError) -> bool {
        err.kind() == Some(ErrorCode::InvalidParams)
            && err.invalid_params_reason()
                == Some(crate::values::InvalidParamsReason::RestartPreview)
    }

    /// WP-016 AC6: a confirmation token is accepted once, for the target it was issued for;
    /// used twice, or with a different target, it is refused with `invalid-params`.
    #[test]
    fn confirmation_tokens_are_single_use_and_bound_to_their_target() {
        let mut ledger = ConfirmationLedger::new();
        let own = mount_target(crate::methods::MountSource::OwnDrive);
        let token = ledger.issue(&link(LINK), "finance", 100).unwrap();
        // Another target of every kind, another account, another link: refused, and the
        // token stays valid for its own target.
        let other_account = MountTarget {
            account_id: mydrive_core::ids::AccountId::new("acct-work-2").unwrap(),
            ..link(LINK)
        };
        for wrong in [
            own.clone(),
            drive("b!finance"),
            link(&format!("{LINK}x")),
            other_account,
        ] {
            let err = ledger.redeem(&token, &wrong, 101).unwrap_err();
            assert!(is_restart_preview(&err), "{err}");
        }
        assert_eq!(ledger.redeem(&token, &link(LINK), 102), Ok("finance"));
        // Twice: refused.
        let err = ledger.redeem(&token, &link(LINK), 103).unwrap_err();
        assert!(is_restart_preview(&err), "{err}");
        assert!(ledger.is_empty());

        // Two previews of the same target get different tokens, each usable once.
        let a = ledger.issue(&own, "a", 0).unwrap();
        let b = ledger.issue(&own, "b", 0).unwrap();
        assert_ne!(a, b);
        assert_eq!(ledger.redeem(&b, &own, 1), Ok("b"));
        assert_eq!(ledger.redeem(&a, &own, 1), Ok("a"));
    }

    /// WP-016 AC6: the token is opaque and does not encode the sharing link readably: it is
    /// short, of a fixed form, and holds none of the link's text.
    #[test]
    fn confirmation_tokens_do_not_encode_the_link() {
        let mut ledger = ConfirmationLedger::new();
        let token = ledger.issue(&link(LINK), (), 0).unwrap();
        let text = token.as_str();
        assert_eq!(text.len(), 36, "{text}");
        assert!(text.starts_with("c1.0000000000000000."), "{text}");
        let hex_link = to_hex(LINK.as_bytes());
        for needle in ["planted", "sharepoint", "Finance", &hex_link[..16]] {
            assert!(!text.contains(needle), "{text} contains {needle}");
        }
        assert!(text.len() <= crate::methods::MAX_CONFIRMATION_TOKEN_BYTES);
    }

    /// A forged, altered or foreign token, an expired one, and one forgotten because too many
    /// previews followed it, are all refused with `restart-preview`.
    #[test]
    fn stale_forged_and_forgotten_tokens_are_refused() {
        let own = mount_target(crate::methods::MountSource::OwnDrive);
        let mut ledger = ConfirmationLedger::new();
        let token = ledger.issue(&own, 1, 1_000).unwrap();
        let text = token.as_str().to_owned();
        let mut forged = vec![
            text.replacen("c1.", "c2.", 1),
            text.replacen("c1.", "C1.", 1),
            format!("{text}0"),
            text[..text.len() - 1].to_owned(),
            "c1.0000000000000001".to_owned(),
            "x".to_owned(),
        ];
        // Flip each hex digit of the binding in turn.
        for i in 20..36 {
            let mut bytes = text.clone().into_bytes();
            bytes[i] = if bytes[i] == b'0' { b'1' } else { b'0' };
            forged.push(String::from_utf8(bytes).unwrap());
        }
        for bad in forged {
            let Ok(bad) = ConfirmationToken::new(bad.clone()) else {
                continue;
            };
            let err = ledger.redeem(&bad, &own, 1_001).unwrap_err();
            assert!(is_restart_preview(&err), "{bad:?}");
        }
        // A fresh ledger (a restarted daemon) did not issue it.
        let mut restarted: ConfirmationLedger<i32> = ConfirmationLedger::new();
        assert!(is_restart_preview(
            &restarted.redeem(&token, &own, 1_001).unwrap_err()
        ));
        // Expired: refused at the lifetime, accepted just before it.
        let mut ledger = ConfirmationLedger::new();
        let token = ledger.issue(&own, 2, 1_000).unwrap();
        let late = 1_000 + PREVIEW_LIFETIME_SECS;
        assert!(is_restart_preview(
            &ledger.redeem(&token, &own, late).unwrap_err()
        ));
        let token = ledger.issue(&own, 3, 1_000).unwrap();
        assert_eq!(ledger.redeem(&token, &own, late - 1), Ok(3));
        // Forgotten: the oldest preview goes when the ledger is full.
        let mut ledger = ConfirmationLedger::new();
        let first = ledger.issue(&own, 0, 0).unwrap();
        for i in 1..=MAX_PENDING_PREVIEWS {
            ledger.issue(&own, i, 0).unwrap();
        }
        assert_eq!(ledger.len(), MAX_PENDING_PREVIEWS);
        assert!(is_restart_preview(
            &ledger.redeem(&first, &own, 0).unwrap_err()
        ));
    }

    /// The refusal repeats neither the token nor the target, which may be a sharing link.
    #[test]
    fn confirmation_refusals_repeat_nothing() {
        let mut ledger = ConfirmationLedger::new();
        let token = ledger.issue(&link(LINK), (), 0).unwrap();
        let err = ledger
            .redeem(&token, &link(&format!("{LINK}-other")), 0)
            .unwrap_err();
        let text = serde_json::to_string(&err).unwrap();
        assert!(!text.contains("planted"), "{text}");
        assert!(!text.contains(token.as_str()), "{text}");
        assert!(!format!("{ledger:?}").contains("planted"));
    }

    /// ADR-0020 R4: a single result that cannot fit one line is refused with `internal`
    /// rather than sent.
    #[test]
    fn oversize_single_results_are_internal_errors() {
        let mut mount = worst_mount(1);
        let id = Id::Number(1);
        assert!(fit_result(&id, mount.clone()).is_ok());
        mount.drive_id = mydrive_core::ids::DriveId::new("d".repeat(MAX_LINE_BYTES)).unwrap();
        let err = fit_result(&id, mount).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorCode::Internal));
    }

    fn worst_mount(i: usize) -> MountInfo {
        use mydrive_core::ids::{AccountId, DriveId, MountId};
        MountInfo {
            mount_id: MountId::new(format!("{i:06}{}", "m".repeat(58))).unwrap(),
            account_id: AccountId::new(format!("acct-{i}-{}", "\u{1}".repeat(200))).unwrap(),
            drive_id: DriveId::new(format!("b!{}", "\"".repeat(2_000))).unwrap(),
            mount_point: ProtocolPath::new(format!("/{i:06}{}", "\"".repeat(4_089))).unwrap(),
            state: crate::values::MountState::Blocked,
            blocked_reason: Some(crate::values::BlockedReason::OrganisationPolicy),
            blocked_message: Some("\u{1}".repeat(100_000)),
            read_only: true,
            drive_type: Some(crate::values::DriveType::DocumentLibrary),
            item_count: Some(u64::MAX),
            paused_until: Some(u64::MAX),
        }
    }

    /// ADR-0020 R4: `mount.list` is paginated, and `blocked_message` is capped, so 300 mounts
    /// with worst-case fields come back in pages that each fit one line.
    #[test]
    fn mount_list_pages_fit_one_line() {
        let mounts: Vec<MountInfo> = (0..300).map(worst_mount).collect();
        let mut source = MountSource(mounts.clone());
        let id = Id::String("\u{1}".repeat(crate::envelope::MAX_ID_BYTES));
        let mut got = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = mount_page(&mut source, cursor.as_deref(), &id).unwrap();
            pages += 1;
            let line = encode_line(&SuccessResponse::new(id.clone(), &page).unwrap()).unwrap();
            assert!(line.len() - 1 <= MAX_LINE_BYTES);
            for m in &page.mounts {
                let v = serde_json::to_value(m).unwrap();
                assert!(v["blocked_message"].as_str().unwrap().len() <= 1024);
            }
            got.extend(page.mounts.into_iter().map(|m| m.mount_id));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert!(pages > 1, "300 worst-case mounts need several pages");
        let expected: Vec<_> = mounts.into_iter().map(|m| m.mount_id).collect();
        assert_eq!(got, expected);
    }

    /// ADR-0020 R4: `account.list` is paginated the same way.
    #[test]
    fn account_list_pages_fit_one_line() {
        use mydrive_core::ids::{AccountId, TenantId};
        let accounts: Vec<AccountInfo> = (0..2_000)
            .map(|i| AccountInfo {
                account_id: AccountId::new(format!("{i:05}{}", "a".repeat(400))).unwrap(),
                kind: crate::values::AccountKind::Work,
                tenant_id: Some(TenantId::new("\"".repeat(400)).unwrap()),
                state: crate::values::AccountState::NeedsSignIn,
                user_name: Some(format!("user{i}@example.com")),
                display_name: Some("Example User".into()),
            })
            .collect();
        let mut source = AccountSource(accounts.clone());
        let id = Id::Number(i64::MIN);
        let mut got = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = account_page(&mut source, cursor.as_deref(), &id).unwrap();
            pages += 1;
            let line = encode_line(&SuccessResponse::new(id.clone(), &page).unwrap()).unwrap();
            assert!(line.len() - 1 <= MAX_LINE_BYTES);
            got.extend(page.accounts);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert!(pages > 1);
        assert_eq!(got, accounts);
    }
}
