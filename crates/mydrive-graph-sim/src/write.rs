//! The write side: simple upload, create folder, update item and delete item, with
//! `If-Match` and conflict behaviour.
//!
//! Declared by WP-014; WP-019 fills it, documented in `docs/write.md`. Its routes are
//! already merged into the Graph origin's router, behind the log, fault and token
//! middleware; until WP-019 there are none.

use std::sync::Arc;

use axum::Router;

use crate::server::Shared;

/// The write routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
}
