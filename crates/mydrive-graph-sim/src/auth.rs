//! Bearer tokens on the Graph origin: the middleware that checks them and the control API
//! that adds and revokes them.
//!
//! The fake authority that issues tokens is a separate feature (`authority`). Documented in
//! `docs/server.md`.

use std::sync::Arc;

use axum::extract::{Request, State as AxState};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::GraphSim;
use crate::server::{Shared, graph_error};

pub(crate) async fn require_bearer(
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

impl GraphSim {
    /// A bearer token this simulator accepts, unique to it.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Makes another bearer token valid.
    pub fn add_token(&self, token: impl Into<String>) {
        self.shared.lock().tokens.insert(token.into());
    }

    /// Makes a bearer token invalid; later requests carrying it get 401.
    pub fn revoke_token(&self, token: &str) {
        self.shared.lock().tokens.remove(token);
    }
}
