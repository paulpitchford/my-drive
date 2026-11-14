//! The HTTP side: routes, handlers and the log, fault and auth middleware.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State as AxState};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use mydrive_core::ids::{DriveId, ItemId};
use serde_json::{Value, json};

use crate::conn::ConnTap;
use crate::fault::FaultAction;
use crate::json::{View, drive_json, error_json, item_json, percent_encode};
use crate::log::{LOGGED_HEADERS, LoggedRequest, Origin};
use crate::model::{DownloadMode, Drive, DriveOptions, ItemKind, RangeMode, State};

/// State shared by the handlers and the control API.
#[derive(Debug)]
pub(crate) struct Shared {
    state: Mutex<State>,
    /// `http://127.0.0.1:<port>/v1.0`, the Graph base URL.
    pub(crate) graph_base: String,
    /// `http://127.0.0.1:<port>`, the pre-authenticated download origin.
    pub(crate) download_base: String,
}

impl Shared {
    pub(crate) fn new(state: State, graph_base: String, download_base: String) -> Self {
        Self {
            state: Mutex::new(state),
            graph_base,
            download_base,
        }
    }

    /// Locks the state. A panic while it was held (only possible in a test's own code
    /// path) leaves it usable: every mutation is applied whole or not at all.
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Ctx = (Arc<Shared>, Origin);

/// The router for the Graph origin.
pub(crate) fn graph_router(shared: Arc<Shared>) -> Router {
    let ctx: Ctx = (shared.clone(), Origin::Graph);
    Router::new()
        .route("/v1.0/drives/{drive}", get(get_drive))
        .route("/v1.0/drives/{drive}/root", get(get_root))
        .route(
            "/v1.0/drives/{drive}/root/children",
            get(list_root_children),
        )
        .route("/v1.0/drives/{drive}/root/delta", get(delta))
        .route("/v1.0/drives/{drive}/items/{item}", get(get_item))
        .route(
            "/v1.0/drives/{drive}/items/{item}/children",
            get(list_children),
        )
        .route(
            "/v1.0/drives/{drive}/items/{item}/content",
            get(get_content),
        )
        .fallback(no_route)
        .layer(from_fn_with_state(shared.clone(), require_bearer))
        .layer(from_fn_with_state(ctx.clone(), apply_fault))
        .layer(from_fn_with_state(ctx, log_request))
        .with_state(shared)
}

/// The router for the pre-authenticated download origin. It checks no bearer token.
pub(crate) fn download_router(shared: Arc<Shared>) -> Router {
    let ctx: Ctx = (shared.clone(), Origin::Download);
    Router::new()
        .route("/download", get(pre_authenticated_download))
        .fallback(no_route)
        .layer(from_fn_with_state(ctx.clone(), apply_fault))
        .layer(from_fn_with_state(ctx, log_request))
        .with_state(shared)
}

// ---- middleware ------------------------------------------------------------------------

async fn log_request(
    AxState((shared, origin)): AxState<Ctx>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().map(str::to_owned);
    let headers = LOGGED_HEADERS
        .iter()
        .filter_map(|name| {
            req.headers().get(*name).map(|value| {
                let text = value.to_str().unwrap_or("<non-ascii>");
                ((*name).to_owned(), text.to_owned())
            })
        })
        .collect();
    let response = next.run(req).await;
    shared.lock().log.push(LoggedRequest {
        origin,
        method,
        path,
        query,
        headers,
        status: response.status().as_u16(),
    });
    response
}

async fn apply_fault(AxState((shared, _)): AxState<Ctx>, req: Request, next: Next) -> Response {
    let fault = shared.lock().take_fault(req.method(), req.uri().path());
    let Some(fault) = fault else {
        return next.run(req).await;
    };
    match fault.action {
        FaultAction::Status {
            status,
            retry_after,
        } => {
            let mut response = graph_error(status, default_code(status), "Scripted fault.");
            if let Some(secs) = retry_after {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(secs));
            }
            response
        }
        FaultAction::Delay(delay) => {
            tokio::time::sleep(delay).await;
            next.run(req).await
        }
        FaultAction::TruncateBody { keep_bytes } => {
            let tap = req.extensions().get::<ConnectInfo<ConnTap>>().cloned();
            let (parts, body) = next.run(req).await.into_parts();
            let Ok(full) = to_bytes(body, usize::MAX).await else {
                return graph_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "generalException",
                    "The simulator could not buffer a body to truncate.",
                );
            };
            // The head announces the whole length; the connection then lets only
            // `keep_bytes` of the body through and closes (see `conn`). A body no longer
            // than that is sent whole.
            if keep_bytes < full.len()
                && let Some(ConnectInfo(tap)) = tap
            {
                tap.truncate_next_response(keep_bytes);
            }
            Response::from_parts(parts, Body::from(full))
        }
    }
}

async fn require_bearer(
    AxState(shared): AxState<Arc<Shared>>,
    req: Request,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        });
    let known = token.is_some_and(|t| shared.lock().tokens.contains(t));
    if known {
        next.run(req).await
    } else {
        graph_error(
            StatusCode::UNAUTHORIZED,
            "InvalidAuthenticationToken",
            "Access token is empty or not valid.",
        )
    }
}

// ---- helpers ---------------------------------------------------------------------------

/// The Graph error code a scripted status fault carries.
fn default_code(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalidRequest",
        401 => "InvalidAuthenticationToken",
        403 => "accessDenied",
        404 => "itemNotFound",
        409 => "nameAlreadyExists",
        416 => "invalidRange",
        429 => "activityLimitReached",
        503 => "serviceNotAvailable",
        _ => "generalException",
    }
}

fn graph_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, axum::Json(error_json(code, message))).into_response()
}

fn not_found() -> Response {
    graph_error(
        StatusCode::NOT_FOUND,
        "itemNotFound",
        "The resource could not be found.",
    )
}

fn bad_request(message: &str) -> Response {
    graph_error(StatusCode::BAD_REQUEST, "invalidRequest", message)
}

async fn no_route() -> Response {
    not_found()
}

fn ok_json(value: Value) -> Response {
    (StatusCode::OK, axum::Json(value)).into_response()
}

/// Finds a drive by the ID in the URL. Drive IDs compare without regard to ASCII case.
fn find_drive<'s>(state: &'s State, raw: &str) -> Option<&'s Drive> {
    let id = DriveId::new(raw).ok()?;
    state.drives.get(&id)
}

fn item_id(raw: &str) -> Option<ItemId> {
    ItemId::new(raw).ok()
}

/// The `@microsoft.graph.downloadUrl` for a file, issued only in redirect mode.
fn download_url(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    item: &ItemId,
) -> Option<String> {
    let d = state.drives.get(drive)?;
    let file = d.live(item).ok().is_some_and(|i| !i.is_folder());
    if !file || d.options.download != DownloadMode::Redirect {
        return None;
    }
    let key = state.issue_download_key(drive, item);
    Some(format!("{}/download?tempauth={key}", shared.download_base))
}

/// Renders a live item for get-item or list-children.
fn render_item(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    item: &ItemId,
) -> Option<Value> {
    let url = download_url(shared, state, drive, item);
    let d = state.drives.get(drive)?;
    let i = d.live(item).ok()?;
    Some(item_json(d, i, View::Item, url.as_deref()))
}

// ---- handlers --------------------------------------------------------------------------

async fn get_drive(AxState(shared): AxState<Arc<Shared>>, Path(drive): Path<String>) -> Response {
    let state = shared.lock();
    match find_drive(&state, &drive) {
        Some(d) => ok_json(drive_json(d)),
        None => not_found(),
    }
}

async fn get_root(AxState(shared): AxState<Arc<Shared>>, Path(drive): Path<String>) -> Response {
    let mut state = shared.lock();
    let Some((id, root)) = find_drive(&state, &drive).map(|d| (d.id.clone(), d.root.clone()))
    else {
        return not_found();
    };
    render_item(&shared, &mut state, &id, &root).map_or_else(not_found, ok_json)
}

async fn get_item(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
) -> Response {
    let mut state = shared.lock();
    let (Some(id), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    render_item(&shared, &mut state, &id, &item).map_or_else(not_found, ok_json)
}

async fn list_root_children(
    AxState(shared): AxState<Arc<Shared>>,
    Path(drive): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = shared.lock();
    let Some((id, root)) = find_drive(&state, &drive).map(|d| (d.id.clone(), d.root.clone()))
    else {
        return not_found();
    };
    children_page(&shared, &mut state, &id, &root, &query)
}

async fn list_children(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = shared.lock();
    let (Some(id), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    children_page(&shared, &mut state, &id, &item, &query)
}

/// One page of a folder's children. `$top` sets the page size; the nextLink carries
/// `$skiptoken`, the offset of the next page.
fn children_page(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    folder: &ItemId,
    query: &HashMap<String, String>,
) -> Response {
    let Some(d) = state.drives.get(drive) else {
        return not_found();
    };
    match d.live(folder) {
        Ok(item) if item.is_folder() => {}
        Ok(_) => return bad_request("The item is not a folder."),
        Err(_) => return not_found(),
    }
    let page_size = match query.get("$top") {
        Some(top) => match top.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => return bad_request("Invalid $top value."),
        },
        None => d.options.children_page_size.max(1),
    };
    let offset = match query.get("$skiptoken") {
        Some(skip) => match skip.parse::<usize>() {
            Ok(n) => n,
            Err(_) => return bad_request("Invalid $skiptoken value."),
        },
        None => 0,
    };
    let ids: Vec<ItemId> = d
        .children(folder)
        .into_iter()
        .map(|i| i.id.clone())
        .collect();
    let page: Vec<ItemId> = ids.iter().skip(offset).take(page_size).cloned().collect();
    let next = offset + page.len();
    let mut value = Vec::with_capacity(page.len());
    for id in &page {
        if let Some(json) = render_item(shared, state, drive, id) {
            value.push(json);
        }
    }
    let mut body = json!({ "value": value });
    if next < ids.len() {
        body["@odata.nextLink"] = json!(format!(
            "{}/drives/{}/items/{}/children?$top={page_size}&$skiptoken={next}",
            shared.graph_base,
            percent_encode(drive.as_str()),
            percent_encode(folder.as_str())
        ));
    }
    ok_json(body)
}

/// Delta tokens: `sim` and the change sequence number in hex. Opaque to clients.
fn encode_token(seq: u64) -> String {
    format!("sim{seq:x}")
}

fn decode_token(token: &str) -> Option<u64> {
    u64::from_str_radix(token.strip_prefix("sim")?, 16).ok()
}

async fn delta(
    AxState(shared): AxState<Arc<Shared>>,
    Path(drive): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let state = shared.lock();
    let Some(d) = find_drive(&state, &drive) else {
        return not_found();
    };
    let after = match query.get("token") {
        None => 0,
        Some(token) => match decode_token(token) {
            Some(seq) if seq <= d.latest_seq() => seq,
            _ => return bad_request("Invalid delta token."),
        },
    };
    let page = d.delta_page(after);
    let value: Vec<Value> = page
        .items
        .iter()
        .filter_map(|id| d.items.get(id))
        .map(|item| item_json(d, item, View::Delta, None))
        .collect();
    let link = format!(
        "{}/drives/{}/root/delta?token={}",
        shared.graph_base,
        percent_encode(d.id.as_str()),
        encode_token(page.resume_after)
    );
    let key = if page.last {
        "@odata.deltaLink"
    } else {
        "@odata.nextLink"
    };
    let mut body = json!({ "value": value });
    body[key] = json!(link);
    ok_json(body)
}

async fn get_content(
    AxState(shared): AxState<Arc<Shared>>,
    Path((drive, item)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut state = shared.lock();
    let (Some(id), Some(item)) = (
        find_drive(&state, &drive).map(|d| d.id.clone()),
        item_id(&item),
    ) else {
        return not_found();
    };
    let Some(d) = state.drives.get(&id) else {
        return not_found();
    };
    let Ok(file) = d.live(&item) else {
        return not_found();
    };
    let ItemKind::File { content, .. } = &file.kind else {
        return bad_request("Only items with the file facet can be downloaded.");
    };
    match d.options.download {
        DownloadMode::Direct => serve_content(content.clone(), &file.etag(), &d.options, &headers),
        DownloadMode::Redirect => redirect_to_download(&shared, &mut state, &id, &item),
    }
}

/// `302 Found` to a fresh pre-authenticated URL. `Range` on `/content` is not honoured
/// here: the download documentation says it must be sent to the download URL.
fn redirect_to_download(
    shared: &Shared,
    state: &mut State,
    drive: &DriveId,
    item: &ItemId,
) -> Response {
    let key = state.issue_download_key(drive, item);
    let location = format!("{}/download?tempauth={key}", shared.download_base);
    match HeaderValue::from_str(&location) {
        Ok(value) => (StatusCode::FOUND, [(header::LOCATION, value)]).into_response(),
        Err(_) => graph_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "generalException",
            "The simulator built an invalid Location.",
        ),
    }
}

async fn pre_authenticated_download(
    AxState(shared): AxState<Arc<Shared>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let state = shared.lock();
    let Some((drive, item)) = query
        .get("tempauth")
        .and_then(|key| state.resolve_download_key(key))
    else {
        return graph_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "The pre-authenticated URL is not valid.",
        );
    };
    let Some(d) = state.drives.get(drive) else {
        return not_found();
    };
    match d.live(item) {
        Ok(file) => match &file.kind {
            ItemKind::File { content, .. } => {
                serve_content(content.clone(), &file.etag(), &d.options, &headers)
            }
            ItemKind::Folder => not_found(),
        },
        Err(_) => not_found(),
    }
}

/// A byte range request, already checked against the content length.
enum RangeRequest {
    /// No usable `Range`: send everything.
    Whole,
    /// Send bytes `start..=end`.
    Part { start: usize, end: usize },
    /// The range lies outside the content.
    Unsatisfiable,
}

/// Parses a single `bytes=` range. Anything else, including several ranges, is treated
/// as no range, which RFC 9110 allows a server to do.
fn parse_range(value: &str, len: usize) -> RangeRequest {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return RangeRequest::Whole;
    };
    if spec.contains(',') {
        return RangeRequest::Whole;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Whole;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // Suffix range: the last N bytes.
        let Ok(n) = last.parse::<usize>() else {
            return RangeRequest::Whole;
        };
        if n == 0 || len == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Part {
            start: len.saturating_sub(n),
            end: len - 1,
        };
    }
    let Ok(start) = first.parse::<usize>() else {
        return RangeRequest::Whole;
    };
    let end = if last.is_empty() {
        usize::MAX
    } else {
        match last.parse::<usize>() {
            Ok(end) if end >= start => end,
            _ => return RangeRequest::Whole,
        }
    };
    if start >= len {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Part {
        start,
        end: end.min(len - 1),
    }
}

fn serve_content(
    content: Bytes,
    etag: &str,
    options: &DriveOptions,
    headers: &HeaderMap,
) -> Response {
    let len = content.len();
    let range = match (options.range, headers.get(header::RANGE)) {
        (RangeMode::Honoured, Some(value)) => value
            .to_str()
            .map_or(RangeRequest::Whole, |v| parse_range(v, len)),
        _ => RangeRequest::Whole,
    };
    let mut response = match range {
        RangeRequest::Whole => (StatusCode::OK, content).into_response(),
        RangeRequest::Part { start, end } => {
            let mut response =
                (StatusCode::PARTIAL_CONTENT, content.slice(start..=end)).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            response
        }
        RangeRequest::Unsatisfiable => {
            let mut response = graph_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "invalidRange",
                "The requested range is not satisfiable.",
            );
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{len}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
    };
    let out = response.headers_mut();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if options.range == RangeMode::Honoured {
        out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }
    if options.content_etag_header
        && let Ok(value) = HeaderValue::from_str(etag)
    {
        out.insert(header::ETAG, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(value: &str, len: usize) -> Option<(usize, usize)> {
        match parse_range(value, len) {
            RangeRequest::Part { start, end } => Some((start, end)),
            _ => None,
        }
    }

    #[test]
    fn parses_single_ranges() {
        assert_eq!(part("bytes=0-1023", 2048), Some((0, 1023)));
        assert_eq!(part("bytes=10-", 20), Some((10, 19)));
        assert_eq!(part("bytes=-5", 20), Some((15, 19)));
        assert_eq!(part("bytes=-50", 20), Some((0, 19)));
        assert_eq!(part("bytes=5-500", 20), Some((5, 19)));
    }

    #[test]
    fn unusable_ranges_mean_whole_body_or_416() {
        assert!(matches!(
            parse_range("bytes=0-1,4-5", 20),
            RangeRequest::Whole
        ));
        assert!(matches!(parse_range("items=0-1", 20), RangeRequest::Whole));
        assert!(matches!(parse_range("bytes=5-1", 20), RangeRequest::Whole));
        assert!(matches!(
            parse_range("bytes=20-", 20),
            RangeRequest::Unsatisfiable
        ));
        assert!(matches!(
            parse_range("bytes=-0", 20),
            RangeRequest::Unsatisfiable
        ));
        assert!(matches!(
            parse_range("bytes=0-", 0),
            RangeRequest::Unsatisfiable
        ));
    }

    #[test]
    fn delta_tokens_round_trip() {
        assert_eq!(decode_token(&encode_token(0)), Some(0));
        assert_eq!(decode_token(&encode_token(12_345)), Some(12_345));
        assert_eq!(decode_token("garbage"), None);
    }
}
