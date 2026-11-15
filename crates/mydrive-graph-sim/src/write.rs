//! The write side: simple upload, create folder, update item and delete item, with
//! `If-Match`, conflict behaviour, locks, the quota and SharePoint's post-upload rewrite.
//!
//! Documented in `docs/write.md`, which gives the source of each behaviour. The state
//! lives in `model`; every write goes through the same `Drive` methods as the control
//! API, so a change made here and the same change made "as if another device did it"
//! look the same in delta. [`commit_upload`] is the one place a completed upload enters
//! the model; the upload-session package (WP-020) calls it too.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, Query, State as AxState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post, put};
use mydrive_core::ids::{DriveId, ItemId};
use serde_json::{Map, Value};

use crate::content::download_url;
use crate::json::{View, item_json};
use crate::model::{Drive, DriveType, FileTimes, Item, State};
use crate::server::{Shared, bad_request, find_drive, graph_error, item_id, not_found};
use crate::time::format_timestamp;
use crate::{GraphSim, SimError};

/// `@microsoft.graph.conflictBehavior`: what a create does when the name is taken.
///
/// The driveItem resource lists the three values and says "the default for PUT is
/// *replace*".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConflictBehavior {
    /// Refuse with `409 nameAlreadyExists`.
    Fail,
    /// Replace the existing file's content (simple upload); not simulated for create
    /// folder, which answers `400`.
    Replace,
    /// Create the item under a free name, `<stem> <n>.<ext>`.
    Rename,
}

impl ConflictBehavior {
    /// Parses the wire value, ignoring ASCII case (the case rule is the simulator's
    /// choice, unverified). Upload sessions (WP-020) use it for the session's item body.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "fail" => Some(Self::Fail),
            "replace" => Some(Self::Replace),
            "rename" => Some(Self::Rename),
            _ => None,
        }
    }
}

/// The status a successful upload that replaced an existing file answers.
///
/// driveitem-put-content's example for "Updating an existing file" shows `201 Created`;
/// plain HTTP would say `200 OK`. Which one Graph sends is unverified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplaceStatus {
    /// `200 OK`.
    Ok,
    /// `201 Created`, as the documented example shows.
    Created,
}

impl ReplaceStatus {
    fn status(self) -> StatusCode {
        match self {
            Self::Ok => StatusCode::OK,
            Self::Created => StatusCode::CREATED,
        }
    }
}

/// When a SharePoint library rewrites an uploaded file ("property promotion"): the
/// stored content, size and hash then differ from what was uploaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RewriteMode {
    /// No rewriting.
    Off,
    /// The rewrite is part of the upload: the upload response already reports the
    /// rewritten size and hash, and the change log holds one change.
    InResponse,
    /// The rewrite follows the upload as a separate change: the upload response reports
    /// the uploaded bytes and their eTag, and the rewritten state (with a newer eTag and
    /// cTag) appears only in later reads and delta.
    Later,
}

/// What the simulator appends to a file it rewrites. Any change of content would do;
/// appending keeps the uploaded bytes visible to a test that looks.
const REWRITE_SUFFIX: &[u8] = b"\n<!-- rewritten by mydrive-graph-sim after upload -->\n";

/// The query parameter and body property that carry the conflict behaviour.
const CONFLICT_BEHAVIOR: &str = "@microsoft.graph.conflictBehavior";

/// The write routes of the Graph origin.
///
/// The create-by-name upload, `items/{parent-id}:/{filename}:/content`, is routed as
/// `items/{item}/{name}/content` with the colons checked in the handler, because a path
/// segment cannot be matched by part. The parameter is named `item` like every other
/// route at that position.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
        .route("/v1.0/drives/{drive}/items/{item}/content", put(put_by_id))
        .route(
            "/v1.0/drives/{drive}/items/{item}/{name}/content",
            put(put_by_name),
        )
        .route(
            "/v1.0/drives/{drive}/items/{item}/children",
            post(create_folder),
        )
        .route(
            "/v1.0/drives/{drive}/root/children",
            post(create_folder_in_root),
        )
        .route(
            "/v1.0/drives/{drive}/items/{item}",
            patch(update_item).delete(delete_item),
        )
}

// ---- refusals --------------------------------------------------------------------------

/// `409 nameAlreadyExists`: the name is taken and the conflict behaviour is `fail`.
pub(crate) fn name_conflict() -> Response {
    graph_error(
        StatusCode::CONFLICT,
        "nameAlreadyExists",
        "The specified item name already exists.",
    )
}

/// `412 resourceModified`: an `If-Match` value matched neither eTag nor cTag.
pub(crate) fn precondition_failed() -> Response {
    graph_error(
        StatusCode::PRECONDITION_FAILED,
        "resourceModified",
        "The resource has changed since the caller last read it (eTag mismatch).",
    )
}

/// `423 resourceLocked`: the item is locked (`GraphSim::set_locked`).
pub(crate) fn locked() -> Response {
    graph_error(
        StatusCode::LOCKED,
        "resourceLocked",
        "The resource that is being accessed is locked.",
    )
}

/// `507 quotaLimitReached`: the write would take the drive past its quota.
pub(crate) fn quota_exceeded() -> Response {
    graph_error(
        StatusCode::INSUFFICIENT_STORAGE,
        "quotaLimitReached",
        "The user has reached their quota limit.",
    )
}

fn too_large() -> Refusal {
    Refusal {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "invalidRequest",
        message: "The request size exceeds the maximum limit for a simple upload.",
    }
}

/// The response for a control-model refusal. The model checks before it changes
/// anything, so after any of these nothing has changed.
fn sim_error_response(error: &SimError) -> Response {
    match error {
        SimError::UnknownDrive { .. } | SimError::UnknownItem { .. } => not_found(),
        SimError::NameConflict { .. } => name_conflict(),
        SimError::NotAFolder { .. }
        | SimError::NotAFile { .. }
        | SimError::RootItem { .. }
        | SimError::InvalidName { .. }
        | SimError::MoveIntoSelf { .. } => bad_request(&error.to_string()),
        SimError::Bind { .. } | SimError::Id(_) => graph_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "generalException",
            "The simulator failed internally.",
        ),
    }
}

/// A refused request: what [`graph_error`] renders, small enough to return in a
/// `Result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl Refusal {
    /// `400 invalidRequest` with `message`.
    pub(crate) fn bad(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalidRequest",
            message,
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        graph_error(self.status, self.code, self.message)
    }
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

// ---- request parts ---------------------------------------------------------------------

/// The `If-Match` header, if sent. Only the first `If-Match` header line is read; a
/// second one is ignored. A value that is not visible ASCII is kept as a value no tag can
/// equal, so it fails the precondition rather than being ignored.
pub(crate) fn if_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .map(|value| value.to_str().unwrap_or("\u{0}").to_owned())
}

/// Whether an `If-Match` value matches the item: `*`, the item's eTag or cTag exactly as
/// served, or a list of quoted tags (RFC 9110) holding one of them. Update and delete
/// document "the eTag (or cTag)". Tags contain commas, so the list is split by quotes,
/// not by commas.
pub(crate) fn precondition_holds(value: &str, item: &Item) -> bool {
    let (etag, ctag) = (item.etag(), item.ctag());
    let value = value.trim();
    if value == "*" || value == etag || value == ctag {
        return true;
    }
    let mut rest = value;
    loop {
        rest = rest.trim_start_matches([' ', '\t', ',']);
        // If-Match compares strongly: a weak tag never matches (RFC 9110 13.1.1).
        let (weak, tag) = match rest.strip_prefix("W/") {
            Some(tag) => (true, tag),
            None => (false, rest),
        };
        let Some(inner) = tag.strip_prefix('"') else {
            return false;
        };
        let Some(end) = inner.find('"') else {
            return false;
        };
        let quoted = &tag[..end + 2];
        if !weak && (quoted == etag || quoted == ctag) {
            return true;
        }
        rest = &inner[end + 1..];
    }
}

/// Names the model refuses: empty, or holding `/`.
fn usable_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/')
}

/// The conflict behaviour from the query string, if any. The driveItem resource says it
/// "should be included in the URL instead of the body". An unknown value is a `400`.
pub(crate) fn conflict_in_query(
    query: &HashMap<String, String>,
) -> Result<Option<ConflictBehavior>, Refusal> {
    match query.get(CONFLICT_BEHAVIOR) {
        None => Ok(None),
        Some(value) => ConflictBehavior::parse(value)
            .map(Some)
            .ok_or_else(|| Refusal::bad("Invalid @microsoft.graph.conflictBehavior value.")),
    }
}

/// The conflict behaviour from a JSON request body's `@microsoft.graph.conflictBehavior`
/// property, if any. Anything but a known string value is a `400`.
pub(crate) fn conflict_in_body(
    fields: &Map<String, Value>,
) -> Result<Option<ConflictBehavior>, Refusal> {
    match fields.get(CONFLICT_BEHAVIOR) {
        None => Ok(None),
        Some(Value::String(value)) => ConflictBehavior::parse(value)
            .map(Some)
            .ok_or_else(|| Refusal::bad("Invalid @microsoft.graph.conflictBehavior value.")),
        Some(_) => Err(Refusal::bad(
            "Invalid @microsoft.graph.conflictBehavior value.",
        )),
    }
}

/// Seconds since the Unix epoch from an ISO 8601 date-time such as
/// `2026-03-21T20:01:37Z`, `2026-03-21T20:01:37.123Z` or `2026-03-21T22:01:37+02:00`.
/// Fractions of a second are dropped: Graph keeps whole seconds.
pub(crate) fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes.get(10) != Some(&b'T') {
        return None;
    }
    let field = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let separators = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if separators.iter().any(|(i, c)| bytes.get(*i) != Some(c))
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut rest = text.get(19..)?;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = fraction.get(digits..)?;
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first() {
                Some(b'+') => 1,
                Some(b'-') => -1,
                _ => return None,
            };
            let digits = |range: std::ops::Range<usize>| -> Option<i64> {
                let part = rest.get(range)?;
                if part.bytes().all(|b| b.is_ascii_digit()) {
                    part.parse().ok()
                } else {
                    None
                }
            };
            let (hh, mm) = (digits(1..3)?, digits(4..6)?);
            if rest.len() != 6 || rest.as_bytes().get(3) != Some(&b':') || hh > 23 || mm > 59 {
                return None;
            }
            sign * (hh * 3600 + mm * 60)
        }
    };
    let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    // A day that does not exist in its month (2026-02-30) comes back as another date.
    if format_timestamp(local).get(..19) != text.get(..19) {
        return None;
    }
    Some(local - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date: Howard Hinnant's
/// `days_from_civil`, the inverse of the one `time` uses.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads a `fileSystemInfo` object from a request body. Absent means no supplied times;
/// `lastAccessedDateTime` is ignored. Anything unreadable answers `400`.
pub(crate) fn parse_file_system_info(value: Option<&Value>) -> Result<FileTimes, Refusal> {
    let Some(value) = value else {
        return Ok(FileTimes::default());
    };
    let Some(info) = value.as_object() else {
        return Err(Refusal::bad("fileSystemInfo must be an object."));
    };
    let time = |key: &str| -> Result<Option<i64>, Refusal> {
        match info.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => parse_timestamp(text)
                .map(Some)
                .ok_or_else(|| Refusal::bad("Invalid date-time in fileSystemInfo.")),
            Some(_) => Err(Refusal::bad("Invalid date-time in fileSystemInfo.")),
        }
    };
    Ok(FileTimes {
        created: time("createdDateTime")?,
        modified: time("lastModifiedDateTime")?,
    })
}

/// A request body that must be a JSON object; anything else is a `400`.
pub(crate) fn json_object(body: &[u8]) -> Result<Map<String, Value>, Refusal> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(Refusal::bad("The request body must be a JSON object.")),
    }
}

/// A free name for `name` in `folder`: `<stem> <n>.<ext>` with the smallest `n` from 1.
/// The pattern Graph uses is unverified. One of the first `children + 1` candidates is
/// always free.
fn free_name(drive: &Drive, folder: &ItemId, name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let limit = drive.child_count(folder) + 1;
    for n in 1..=limit {
        let candidate = format!("{stem} {n}{ext}");
        if drive.child_named(folder, &candidate).is_none() {
            return candidate;
        }
    }
    format!("{stem} {}{ext}", limit + 1)
}

/// Whether, and when, uploading a file named `name` to `drive` is rewritten. Only
/// document libraries rewrite (rclone: "not Personal or OneDrive for Business").
fn rewrite_for(drive: &Drive, name: &str) -> Option<RewriteMode> {
    if drive.drive_type != DriveType::DocumentLibrary || drive.options.rewrite == RewriteMode::Off {
        return None;
    }
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    drive
        .options
        .rewrite_extensions
        .iter()
        .any(|listed| listed.eq_ignore_ascii_case(&ext))
        .then_some(drive.options.rewrite)
}

fn rewritten(content: &Bytes) -> Bytes {
    let mut out = Vec::with_capacity(content.len() + REWRITE_SUFFIX.len());
    out.extend_from_slice(content);
    out.extend_from_slice(REWRITE_SUFFIX);
    Bytes::from(out)
}

/// Renders a live item (or a snapshot of one) for a write response, as get-item would.
fn render(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    item: &ItemId,
    snapshot: Option<&Item>,
) -> Option<Value> {
    let url = download_url(shared, state, drive, item);
    let d = state.drives.get(drive)?;
    let current = d.live(item).ok()?;
    Some(item_json(
        d,
        snapshot.unwrap_or(current),
        View::Item,
        url.as_deref(),
    ))
}

// ---- the commit ------------------------------------------------------------------------

/// Where a completed upload goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadTarget {
    /// Replace the content of this existing file (`items/{id}/content`,
    /// `items/{id}/createUploadSession`).
    Existing(ItemId),
    /// A file called `name` in the folder `parent` (`items/{parent}:/{name}:/...`). If
    /// the name is taken, the conflict behaviour decides.
    New {
        /// The folder.
        parent: ItemId,
        /// The file name, already percent-decoded.
        name: String,
    },
}

/// Everything [`commit_upload`] needs. The caller decides each value; nothing here is
/// read from a request.
#[derive(Debug, Clone)]
pub(crate) struct UploadCommit {
    /// The drive, as stored (not as the client spelled it).
    pub(crate) drive: DriveId,
    /// Where the content goes.
    pub(crate) target: UploadTarget,
    /// Every byte uploaded.
    pub(crate) content: Bytes,
    /// The `If-Match` value to check, or `None` to check nothing. A route that does not
    /// honour `If-Match` passes `None`. With a value, a create (no existing item at the
    /// target) fails the precondition, as RFC 9110 has it.
    pub(crate) if_match: Option<String>,
    /// What to do when a [`UploadTarget::New`] name is taken.
    pub(crate) conflict: ConflictBehavior,
    /// `fileSystemInfo` times the client supplied, already whole seconds.
    pub(crate) file_times: FileTimes,
}

/// Commits a completed upload to the model and returns the response for it: `201
/// Created` with the new driveItem, or the replaced one with the status
/// [`DriveOptions::replace_status`](crate::DriveOptions) names (default `201`).
///
/// Refusals change nothing at all and answer, in this order of checking: `404
/// itemNotFound` (drive, item or parent), `400 invalidRequest` (a folder as the target,
/// a parent that is a file, an unusable name, conflict behaviour `replace` onto a folder
/// answers `409` instead), `409 nameAlreadyExists` (behaviour `fail`), `423
/// resourceLocked` (the file being replaced is locked), `412 resourceModified` (the
/// `If-Match` value matches neither eTag nor cTag) and `507 quotaLimitReached` (the
/// drive's live files would exceed [`DriveOptions::quota_bytes`](crate::DriveOptions)).
///
/// The stored hash is the quickXorHash, the one hash the model keeps for every drive
/// type. On a document library, a file whose extension is listed is rewritten according
/// to [`RewriteMode`]; content, size and hash always describe the same stored bytes, and
/// under [`RewriteMode::Later`] the response describes the uploaded version. The quota is
/// checked against the size that is stored at commit: the rewritten size under
/// [`RewriteMode::InResponse`], the uploaded size under [`RewriteMode::Later`] (whose
/// rewrite follows and may leave the drive above its quota).
///
/// The caller holds the state lock; `shared` is needed only for the download URL in the
/// response. The body size limit of a simple upload is the caller's business, not this
/// function's.
pub(crate) fn commit_upload(shared: &Shared, state: &mut State, commit: UploadCommit) -> Response {
    let now = state.clock;
    let UploadCommit {
        drive,
        target,
        content,
        if_match,
        conflict,
        file_times,
    } = commit;
    let Some(d) = state.drives.get_mut(&drive) else {
        return not_found();
    };
    // What the upload will do, checked against the tree before anything changes.
    let (replace, parent, name) = match target {
        UploadTarget::Existing(id) => match d.live(&id) {
            Ok(item) if item.is_folder() => {
                return bad_request("Only items with the file facet can take content.");
            }
            Ok(item) => {
                let name = item.name.clone();
                (Some(id), None, name)
            }
            Err(_) => return not_found(),
        },
        UploadTarget::New { parent, name } => {
            match d.live(&parent) {
                Ok(folder) if folder.is_folder() => {}
                Ok(_) => return bad_request("The parent item is not a folder."),
                Err(_) => return not_found(),
            }
            if !usable_name(&name) {
                return bad_request("The name is empty or contains '/'.");
            }
            match d.child_named(&parent, &name).cloned() {
                None => (None, Some(parent), name),
                Some(existing) => match conflict {
                    ConflictBehavior::Fail => return name_conflict(),
                    ConflictBehavior::Replace => match d.live(&existing) {
                        Ok(item) if !item.is_folder() => {
                            let name = item.name.clone();
                            (Some(existing), None, name)
                        }
                        _ => return name_conflict(),
                    },
                    ConflictBehavior::Rename => {
                        let free = free_name(d, &parent, &name);
                        (None, Some(parent), free)
                    }
                },
            }
        }
    };
    if let Some(id) = &replace
        && d.is_locked(id)
    {
        return locked();
    }
    if let Some(value) = &if_match {
        let holds = replace
            .as_ref()
            .and_then(|id| d.live(id).ok())
            .is_some_and(|item| precondition_holds(value, item));
        if !holds {
            return precondition_failed();
        }
    }
    let rewrite = rewrite_for(d, &name);
    let stored = if rewrite == Some(RewriteMode::InResponse) {
        rewritten(&content)
    } else {
        content.clone()
    };
    let old_size = replace
        .as_ref()
        .and_then(|id| d.live(id).ok())
        .map_or(0, |item| item.size);
    let used = d
        .used_bytes()
        .saturating_sub(old_size)
        .saturating_add(stored.len() as u64);
    if used > d.options.quota_bytes {
        return quota_exceeded();
    }
    let committed = match (replace, parent) {
        (Some(id), _) => d
            .replace_content(&id, stored, now, file_times)
            .map(|()| (id, false)),
        (None, Some(parent)) => d
            .create_with_times(&parent, &name, Drive::file_kind(stored), now, file_times)
            .map(|id| (id, true)),
        (None, None) => return not_found(),
    };
    let (id, created) = match committed {
        Ok(done) => done,
        Err(error) => return sim_error_response(&error),
    };
    // The later rewrite is the server's own change after the upload; the response still
    // describes what was uploaded.
    let snapshot = if rewrite == Some(RewriteMode::Later) {
        let before = d.live(&id).ok().cloned();
        if let Err(error) = d.rewrite_content(&id, rewritten(&content)) {
            return sim_error_response(&error);
        }
        before
    } else {
        None
    };
    let status = if created {
        StatusCode::CREATED
    } else {
        d.options.replace_status.status()
    };
    match render(shared, state, &drive, &id, snapshot.as_ref()) {
        Some(body) => json_response(status, body),
        None => not_found(),
    }
}

// ---- handlers --------------------------------------------------------------------------

/// The drive's stored ID and simple-upload size limit, read under a short lock so the
/// body is not read while the state is held.
fn upload_limit(shared: &Shared, raw_drive: &str) -> Option<(DriveId, u64)> {
    let state = shared.lock();
    find_drive(&state, raw_drive).map(|d| (d.id.clone(), d.options.simple_upload_max_bytes))
}

/// Reads a simple upload's body, refusing with `413` anything over `limit`: at once if
/// `Content-Length` says so, otherwise as soon as more than `limit` bytes arrive.
async fn read_upload(headers: &HeaderMap, body: Body, limit: u64) -> Result<Bytes, Refusal> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit) {
        return Err(too_large());
    }
    let cap = usize::try_from(limit).unwrap_or(usize::MAX);
    to_bytes(body, cap).await.map_err(|_| too_large())
}

/// A simple upload already read, committed under the lock. `If-Match` is passed on only
/// where [`DriveOptions::simple_replace_honours_if_match`](crate::DriveOptions) says so.
fn commit_simple(
    shared: &Shared,
    drive: DriveId,
    target: UploadTarget,
    content: Bytes,
    conflict: ConflictBehavior,
    headers: &HeaderMap,
) -> Response {
    let mut state = shared.lock();
    let honours = state
        .drives
        .get(&drive)
        .is_some_and(|d| d.options.simple_replace_honours_if_match);
    let commit = UploadCommit {
        drive,
        target,
        content,
        if_match: if honours { if_match(headers) } else { None },
        conflict,
        file_times: FileTimes::default(),
    };
    commit_upload(shared, &mut state, commit)
}

/// `PUT /drives/{d}/items/{id}/content`: replace a file's content.
async fn put_by_id(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (Some((drive, limit)), Some(item)) = (upload_limit(&shared, &drive), item_id(&item)) else {
        return not_found();
    };
    let content = match read_upload(&headers, body, limit).await {
        Ok(content) => content,
        Err(refusal) => return refusal.into_response(),
    };
    commit_simple(
        &shared,
        drive,
        UploadTarget::Existing(item),
        content,
        ConflictBehavior::Replace,
        &headers,
    )
}

/// `PUT /drives/{d}/items/{parent}:/{name}:/content`: upload a new file, or replace one
/// of that name as the conflict behaviour (default `replace`) says.
async fn put_by_name(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, parent, name)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (Some(parent), Some(name)) = (parent.strip_suffix(':'), name.strip_suffix(':')) else {
        return not_found();
    };
    let (Some((drive, limit)), Some(parent)) = (upload_limit(&shared, &drive), item_id(parent))
    else {
        return not_found();
    };
    let conflict = match conflict_in_query(&query) {
        Ok(conflict) => conflict.unwrap_or(ConflictBehavior::Replace),
        Err(refusal) => return refusal.into_response(),
    };
    let content = match read_upload(&headers, body, limit).await {
        Ok(content) => content,
        Err(refusal) => return refusal.into_response(),
    };
    let target = UploadTarget::New {
        parent,
        name: name.to_owned(),
    };
    commit_simple(&shared, drive, target, content, conflict, &headers)
}

async fn create_folder(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, parent)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let Some(parent) = item_id(&parent) else {
        return not_found();
    };
    create_folder_in(&shared, &drive, Some(parent), &query, &body)
}

async fn create_folder_in_root(
    AxState(shared): AxState<Arc<Shared>>,
    Path(drive): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    create_folder_in(&shared, &drive, None, &query, &body)
}

/// `POST .../children`: create a folder. `parent` `None` is the root.
fn create_folder_in(
    shared: &Shared,
    raw_drive: &str,
    parent: Option<ItemId>,
    query: &HashMap<String, String>,
    body: &[u8],
) -> Response {
    let mut state = shared.lock();
    let now = state.clock;
    let Some((drive, root, default)) = find_drive(&state, raw_drive).map(|d| {
        (
            d.id.clone(),
            d.root.clone(),
            d.options.folder_conflict_default,
        )
    }) else {
        return not_found();
    };
    let parent = parent.unwrap_or(root);
    let fields = match json_object(body) {
        Ok(fields) => fields,
        Err(refusal) => return refusal.into_response(),
    };
    let Some(Value::String(name)) = fields.get("name") else {
        return bad_request("The request body needs a name.");
    };
    if !fields.get("folder").is_some_and(Value::is_object) {
        return bad_request("Only folders can be created here; the body needs a folder facet.");
    }
    let in_body = match conflict_in_body(&fields) {
        Ok(in_body) => in_body,
        Err(refusal) => return refusal.into_response(),
    };
    let conflict = match conflict_in_query(query) {
        Ok(in_query) => in_query.or(in_body).unwrap_or(default),
        Err(refusal) => return refusal.into_response(),
    };
    let times = match parse_file_system_info(fields.get("fileSystemInfo")) {
        Ok(times) => times,
        Err(refusal) => return refusal.into_response(),
    };
    let Some(d) = state.drives.get_mut(&drive) else {
        return not_found();
    };
    match d.live(&parent) {
        Ok(folder) if folder.is_folder() => {}
        Ok(_) => return bad_request("The parent item is not a folder."),
        Err(_) => return not_found(),
    }
    if !usable_name(name) {
        return bad_request("The name is empty or contains '/'.");
    }
    let name = if d.child_named(&parent, name).is_some() {
        match conflict {
            ConflictBehavior::Fail => return name_conflict(),
            ConflictBehavior::Rename => free_name(d, &parent, name),
            ConflictBehavior::Replace => {
                return bad_request(
                    "Conflict behaviour replace is not simulated for create folder.",
                );
            }
        }
    } else {
        name.clone()
    };
    let id = match d.create_with_times(&parent, &name, crate::model::ItemKind::Folder, now, times) {
        Ok(id) => id,
        Err(error) => return sim_error_response(&error),
    };
    match render(shared, &mut state, &drive, &id, None) {
        Some(body) => json_response(StatusCode::CREATED, body),
        None => not_found(),
    }
}

/// The parts of a PATCH body the simulator acts on.
struct Update {
    name: Option<String>,
    parent: Option<ItemId>,
    times: FileTimes,
}

fn parse_update(fields: &Map<String, Value>, drive: &DriveId) -> Result<Update, Refusal> {
    let name = match fields.get("name") {
        None => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => return Err(Refusal::bad("name must be a string.")),
    };
    let parent = match fields.get("parentReference") {
        None => None,
        Some(Value::Object(reference)) => {
            if let Some(other) = reference.get("driveId") {
                let same = other
                    .as_str()
                    .and_then(|raw| DriveId::new(raw).ok())
                    .is_some_and(|id| id == *drive);
                if !same {
                    return Err(Refusal::bad(
                        "Moving an item to another drive is not simulated.",
                    ));
                }
            }
            match reference.get("id").and_then(Value::as_str).map(ItemId::new) {
                Some(Ok(id)) => Some(id),
                _ => return Err(Refusal::bad("parentReference needs an id.")),
            }
        }
        Some(_) => return Err(Refusal::bad("parentReference must be an object.")),
    };
    let times = parse_file_system_info(fields.get("fileSystemInfo"))?;
    Ok(Update {
        name,
        parent,
        times,
    })
}

/// `PATCH /drives/{d}/items/{id}`: rename, move and/or set `fileSystemInfo`, as one
/// change. Other properties are ignored, except `@microsoft.graph.conflictBehavior`, in
/// the query string or the body, which answers `400`: what Graph does with it on update
/// is unknown, and it may replace the item in the way. A simulator that answered a
/// harmless `409` instead would hide a destructive client bug.
async fn update_item(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut state = shared.lock();
    let now = state.clock;
    let (Some(drive), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    let Some(d) = state.drives.get_mut(&drive) else {
        return not_found();
    };
    if d.live(&item).is_err() {
        return not_found();
    }
    let fields = match json_object(&body) {
        Ok(fields) => fields,
        Err(refusal) => return refusal.into_response(),
    };
    if query.contains_key(CONFLICT_BEHAVIOR) || fields.contains_key(CONFLICT_BEHAVIOR) {
        return bad_request(
            "@microsoft.graph.conflictBehavior on update is not simulated: Graph may replace \
             the item in the way.",
        );
    }
    let update = match parse_update(&fields, &drive) {
        Ok(update) => update,
        Err(refusal) => return refusal.into_response(),
    };
    if d.is_locked(&item) {
        return locked();
    }
    if let Some(value) = if_match(&headers)
        && !d.live(&item).is_ok_and(|i| precondition_holds(&value, i))
    {
        return precondition_failed();
    }
    let changes =
        update.name.is_some() || update.parent.is_some() || update.times != FileTimes::default();
    if changes
        && let Err(error) = d.relocate(
            &item,
            update.name.as_deref(),
            update.parent.as_ref(),
            update.times,
            now,
        )
    {
        return sim_error_response(&error);
    }
    match render(&shared, &mut state, &drive, &item, None) {
        Some(body) => json_response(StatusCode::OK, body),
        None => not_found(),
    }
}

/// `DELETE /drives/{d}/items/{id}`: the item and everything below it go to the recycle
/// bin (here: they are gone, and delta reports them).
async fn delete_item(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut state = shared.lock();
    let (Some(drive), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    let Some(d) = state.drives.get_mut(&drive) else {
        return not_found();
    };
    if d.live(&item).is_err() {
        return not_found();
    }
    if item == d.root {
        return sim_error_response(&SimError::RootItem { drive });
    }
    if d.locked_within(&item) {
        return locked();
    }
    if let Some(value) = if_match(&headers)
        && !d.live(&item).is_ok_and(|i| precondition_holds(&value, i))
    {
        return precondition_failed();
    }
    match d.delete(&item) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => sim_error_response(&error),
    }
}

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
    /// Locks or unlocks an item, as a checkout or a co-authoring session elsewhere would.
    /// While an item is locked, replacing its content, updating it and deleting it (or a
    /// folder above it) answer `423 Locked` and change nothing. The control API's own
    /// changes ignore locks.
    ///
    /// # Errors
    ///
    /// An unknown drive, or an unknown or deleted item.
    pub fn set_locked(&self, drive: &DriveId, item: &ItemId, locked: bool) -> Result<(), SimError> {
        self.shared
            .lock()
            .drive_mut(drive)?
            .set_locked(item, locked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DriveOptions, ItemKind};

    #[test]
    fn timestamps_parse_to_whole_seconds() {
        assert_eq!(parse_timestamp("2016-03-21T20:01:37Z"), Some(1_458_590_497));
        assert_eq!(
            parse_timestamp("2016-03-21T20:01:37.999Z"),
            Some(1_458_590_497)
        );
        assert_eq!(
            parse_timestamp("2016-03-21T22:01:37+02:00"),
            Some(1_458_590_497)
        );
        assert_eq!(
            parse_timestamp("2016-03-21T19:01:37.5-01:00"),
            Some(1_458_590_497)
        );
        assert_eq!(parse_timestamp("2000-02-29T00:00:00Z"), Some(951_782_400));
        for bad in [
            "2026-02-30T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:00+0100",
            "2026-01-01T00:00:00-+1:00",
            "2026-01-01T00:00:00+01:+0",
            "not a date",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }

    /// A body over the limit is refused even without `Content-Length`, and one at the
    /// limit is read whole.
    #[test]
    fn upload_body_limit_holds_without_content_length() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let none = HeaderMap::new();
        let over = runtime.block_on(read_upload(&none, Body::from("123456789"), 8));
        assert_eq!(over, Err(too_large()));
        let at = runtime.block_on(read_upload(&none, Body::from("12345678"), 8));
        assert_eq!(at, Ok(Bytes::from_static(b"12345678")));
        let mut declared = HeaderMap::new();
        declared.insert(header::CONTENT_LENGTH, "9".parse().unwrap());
        let early = runtime.block_on(read_upload(&declared, Body::from("1"), 8));
        assert_eq!(early, Err(too_large()));
    }

    fn shared_with(drive_type: DriveType, options: DriveOptions) -> (Shared, DriveId, ItemId) {
        let mut state = State::new("k".to_owned());
        let drive = state.add_drive(drive_type, options).unwrap();
        let root = state.drive(&drive).unwrap().root.clone();
        let shared = Shared::new(state, "http://g/v1.0".to_owned(), "http://d".to_owned());
        (shared, drive, root)
    }

    fn body_of(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let bytes = runtime
            .block_on(to_bytes(response.into_body(), usize::MAX))
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn new_file(root: &ItemId, name: &str) -> UploadTarget {
        UploadTarget::New {
            parent: root.clone(),
            name: name.to_owned(),
        }
    }

    /// The commit WP-020 calls keeps supplied `fileSystemInfo` times and honours
    /// `If-Match` whenever it is given one.
    #[test]
    fn commit_upload_keeps_times_and_checks_if_match() {
        let (shared, drive, root) = shared_with(DriveType::Business, DriveOptions::default());
        let times = FileTimes {
            created: parse_timestamp("2020-01-02T03:04:05.678Z"),
            modified: parse_timestamp("2021-01-02T03:04:05Z"),
        };
        let commit = UploadCommit {
            drive: drive.clone(),
            target: new_file(&root, "a.bin"),
            content: Bytes::from_static(b"abc"),
            if_match: None,
            conflict: ConflictBehavior::Fail,
            file_times: times,
        };
        let (status, item) = body_of(commit_upload(&shared, &mut shared.lock(), commit));
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            item["fileSystemInfo"]["createdDateTime"],
            "2020-01-02T03:04:05Z"
        );
        assert_eq!(
            item["fileSystemInfo"]["lastModifiedDateTime"],
            "2021-01-02T03:04:05Z"
        );
        let id = ItemId::new(item["id"].as_str().unwrap()).unwrap();

        let stale = UploadCommit {
            drive: drive.clone(),
            target: UploadTarget::Existing(id.clone()),
            content: Bytes::from_static(b"other"),
            if_match: Some("\"{nope},1\"".to_owned()),
            conflict: ConflictBehavior::Fail,
            file_times: FileTimes::default(),
        };
        let (status, body) = body_of(commit_upload(&shared, &mut shared.lock(), stale));
        assert_eq!(status, StatusCode::PRECONDITION_FAILED);
        assert_eq!(body["error"]["code"], "resourceModified");
        let size = shared.lock().drive(&drive).unwrap().live(&id).unwrap().size;
        assert_eq!(size, 3);

        let current = UploadCommit {
            drive,
            target: UploadTarget::Existing(id),
            content: Bytes::from_static(b"other"),
            if_match: item["eTag"].as_str().map(str::to_owned),
            conflict: ConflictBehavior::Fail,
            file_times: FileTimes::default(),
        };
        let (status, replaced) = body_of(commit_upload(&shared, &mut shared.lock(), current));
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(replaced["size"], 5);
        assert_ne!(replaced["eTag"], item["eTag"]);
    }

    /// Conflict behaviour `rename` picks `<stem> <n>.<ext>`, compared without regard to
    /// case.
    #[test]
    fn rename_picks_a_free_name() {
        let (shared, drive, root) = shared_with(DriveType::Personal, DriveOptions::default());
        for name in ["Report.txt", "report 1.TXT"] {
            let commit = UploadCommit {
                drive: drive.clone(),
                target: new_file(&root, name),
                content: Bytes::new(),
                if_match: None,
                conflict: ConflictBehavior::Fail,
                file_times: FileTimes::default(),
            };
            assert_eq!(
                commit_upload(&shared, &mut shared.lock(), commit).status(),
                StatusCode::CREATED
            );
        }
        let commit = UploadCommit {
            drive: drive.clone(),
            target: new_file(&root, "REPORT.txt"),
            content: Bytes::new(),
            if_match: None,
            conflict: ConflictBehavior::Rename,
            file_times: FileTimes::default(),
        };
        let (status, item) = body_of(commit_upload(&shared, &mut shared.lock(), commit));
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["name"], "REPORT 2.txt");
        let state = shared.lock();
        let d = state.drive(&drive).unwrap();
        assert_eq!(free_name(d, &root, "noext"), "noext 1");
        assert_eq!(free_name(d, &root, ".hidden"), ".hidden 1");
        assert!(matches!(
            d.live(d.child_named(&root, "report 2.TXT").unwrap())
                .unwrap()
                .kind,
            ItemKind::File { .. }
        ));
    }
}
