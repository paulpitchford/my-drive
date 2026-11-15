//! Content download: `/content` on the Graph origin, the redirect to a pre-authenticated
//! URL, the download origin that serves it, and byte ranges.
//!
//! Documented in `docs/content.md`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State as AxState};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use mydrive_core::ids::{DriveId, ItemId};

use crate::model::{DownloadMode, DriveOptions, ItemKind, RangeMode, State};
use crate::server::{Shared, bad_request, find_drive, graph_error, item_id, not_found};

/// The content route of the Graph origin.
pub(crate) fn graph_routes() -> Router<Arc<Shared>> {
    Router::new().route(
        "/v1.0/drives/{drive}/items/{item}/content",
        get(get_content),
    )
}

/// The route of the pre-authenticated download origin.
pub(crate) fn download_routes() -> Router<Arc<Shared>> {
    Router::new().route("/download", get(pre_authenticated_download))
}

/// The `@microsoft.graph.downloadUrl` for a file, issued only in redirect mode.
pub(crate) fn download_url(
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
pub(crate) enum RangeRequest {
    /// No usable `Range`: send everything.
    Whole,
    /// Send bytes `start..=end`.
    Part { start: usize, end: usize },
    /// The range lies outside the content.
    Unsatisfiable,
}

/// Parses a single `bytes=` range. Anything else, including several ranges, is treated
/// as no range, which RFC 9110 allows a server to do.
pub(crate) fn parse_range(value: &str, len: usize) -> RangeRequest {
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
