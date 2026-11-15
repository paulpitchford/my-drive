//! The delta feed: `GET /drives/{drive}/root/delta`, with and without a token.
//!
//! The change log and paging live in `model`; this file is the route and the token
//! format. Moved here from `server` by WP-014; WP-021 extends it (`410 Gone` as a model,
//! ancestors in delta). Documented in `docs/delta.md`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State as AxState};
use axum::response::Response;
use axum::routing::get;
use serde_json::{Value, json};

use crate::json::{View, item_json, percent_encode};
use crate::model::DeltaCursor;
use crate::server::{Shared, bad_request, find_drive, not_found, ok_json};

/// The delta route of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new().route("/v1.0/drives/{drive}/root/delta", get(delta))
}

/// Delta tokens: `sim`, the change sequence number in hex, and for a token-less
/// enumeration still in progress `-` and its snapshot point. Opaque to clients.
pub(crate) fn encode_token(cursor: DeltaCursor) -> String {
    match cursor.snapshot {
        Some(snapshot) => format!("sim{:x}-{snapshot:x}", cursor.after),
        None => format!("sim{:x}", cursor.after),
    }
}

pub(crate) fn decode_token(token: &str) -> Option<DeltaCursor> {
    let rest = token.strip_prefix("sim")?;
    let (after, snapshot) = match rest.split_once('-') {
        Some((after, snapshot)) => (after, Some(u64::from_str_radix(snapshot, 16).ok()?)),
        None => (rest, None),
    };
    Some(DeltaCursor {
        after: u64::from_str_radix(after, 16).ok()?,
        snapshot,
    })
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
    // No token: "enumerates the hierarchy's current state" (delta, parameters).
    let cursor = match query.get("token") {
        None => d.initial_cursor(),
        Some(token) => match decode_token(token) {
            Some(cursor) if d.cursor_is_valid(cursor) => cursor,
            _ => return bad_request("Invalid delta token."),
        },
    };
    let page = d.delta_page(cursor);
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
        encode_token(page.next)
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
