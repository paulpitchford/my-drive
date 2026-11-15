//! Identity: `/me`, `/me/drive` and `/organization`, and the accounts and tenants behind
//! them.
//!
//! Declared by WP-014; WP-023 fills it, documented in `docs/identity.md`. Its routes are
//! already merged into the Graph origin's router, behind the log, fault and token
//! middleware; until WP-023 there are none.

use std::sync::Arc;

use axum::Router;

use crate::server::Shared;

/// The identity routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
}
