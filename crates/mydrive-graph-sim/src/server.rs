//! The HTTP side: the shared state, the two origins' routers and the response helpers
//! every feature uses.
//!
//! Each origin's router is composed from per-feature routers, so a feature adds its routes
//! in its own file. The middleware lives with its feature: the request log in `log`,
//! scripted faults in `fault`, the bearer-token check in `auth`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use mydrive_core::ids::{DriveId, ItemId};
use serde_json::Value;

use crate::json::error_json;
use crate::log::Origin;
use crate::model::{Drive, State};
use crate::{auth, content, delta, discovery, fault, identity, items, log, session, write};

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

/// The state of the log and fault middleware: the shared state and the origin a request
/// arrived on.
pub(crate) type Ctx = (Arc<Shared>, Origin);

/// The router for the Graph origin, composed from the features' routers.
pub(crate) fn graph_router(shared: Arc<Shared>) -> Router {
    let ctx: Ctx = (shared.clone(), Origin::Graph);
    Router::new()
        .merge(items::routes())
        .merge(delta::routes())
        .merge(content::graph_routes())
        .merge(write::routes())
        .merge(session::routes())
        .merge(identity::routes())
        .merge(discovery::routes())
        .fallback(no_route)
        .layer(from_fn_with_state(shared.clone(), auth::require_bearer))
        .layer(from_fn_with_state(ctx.clone(), fault::apply_fault))
        .layer(from_fn_with_state(ctx, log::log_request))
        .with_state(shared)
}

/// The router for the pre-authenticated download origin. It checks no bearer token.
pub(crate) fn download_router(shared: Arc<Shared>) -> Router {
    let ctx: Ctx = (shared.clone(), Origin::Download);
    Router::new()
        .merge(content::download_routes())
        .fallback(no_route)
        .layer(from_fn_with_state(ctx.clone(), fault::apply_fault))
        .layer(from_fn_with_state(ctx, log::log_request))
        .with_state(shared)
}

// ---- helpers ---------------------------------------------------------------------------

pub(crate) fn graph_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, axum::Json(error_json(code, message))).into_response()
}

pub(crate) fn not_found() -> Response {
    graph_error(
        StatusCode::NOT_FOUND,
        "itemNotFound",
        "The resource could not be found.",
    )
}

pub(crate) fn bad_request(message: &str) -> Response {
    graph_error(StatusCode::BAD_REQUEST, "invalidRequest", message)
}

async fn no_route() -> Response {
    not_found()
}

pub(crate) fn ok_json(value: Value) -> Response {
    (StatusCode::OK, axum::Json(value)).into_response()
}

/// Finds a drive by the ID in the URL. Drive IDs compare without regard to ASCII case.
pub(crate) fn find_drive<'s>(state: &'s State, raw: &str) -> Option<&'s Drive> {
    let id = DriveId::new(raw).ok()?;
    state.drives.get(&id)
}

pub(crate) fn item_id(raw: &str) -> Option<ItemId> {
    ItemId::new(raw).ok()
}

#[cfg(test)]
mod tests {
    // The functions these tests cover moved to their feature files (WP-014); the tests
    // stay here so their names do not change.
    use crate::content::{RangeRequest, parse_range};
    use crate::delta::{decode_token, encode_token};
    use crate::model::DeltaCursor;

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
        for cursor in [
            DeltaCursor {
                after: 0,
                snapshot: None,
            },
            DeltaCursor {
                after: 12_345,
                snapshot: None,
            },
            DeltaCursor {
                after: 3,
                snapshot: Some(9),
            },
        ] {
            assert_eq!(decode_token(&encode_token(cursor)), Some(cursor));
        }
        assert_eq!(decode_token("garbage"), None);
        assert_eq!(decode_token("sim1-x"), None);
    }
}
