//! Upload sessions: creating one, fragments, status, cancelling and expiry.
//!
//! Declared by WP-014; WP-020 fills it, documented in `docs/session.md`. Its routes are
//! already merged into the Graph origin's router, behind the log, fault and token
//! middleware; until WP-020 there are none. Where the upload URL itself is served is
//! WP-020's decision.

use std::sync::Arc;

use axum::Router;

use crate::server::Shared;

/// The upload-session routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
}
