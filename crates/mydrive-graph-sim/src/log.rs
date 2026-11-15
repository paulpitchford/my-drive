//! The request log tests assert on: its types, the middleware that records every request
//! on both origins, and the control API that reads and clears it. Documented in
//! `docs/faults.md`.

use axum::extract::{Request, State as AxState};
use axum::middleware::Next;
use axum::response::Response;

use crate::GraphSim;
use crate::server::Ctx;

/// Which listener a request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The Graph API origin, `GraphSim::base_url`.
    Graph,
    /// The separate origin that serves pre-authenticated download URLs, standing in for a
    /// storage host such as `*.files.1drv.com`.
    Download,
}

/// The request headers the log records, lowercase. Others are not kept.
pub const LOGGED_HEADERS: &[&str] = &[
    "authorization",
    "range",
    "if-match",
    "if-none-match",
    "user-agent",
    "prefer",
    "deltaexcludeparent",
];

/// One request the simulator received, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    /// The listener it arrived on.
    pub origin: Origin,
    /// The HTTP method, for example `GET`.
    pub method: String,
    /// The path, without the query string, exactly as sent (still percent-encoded).
    pub path: String,
    /// The query string without the leading `?`, if there was one.
    pub query: Option<String>,
    /// The headers named in [`LOGGED_HEADERS`] that were present, as (lowercase name,
    /// value). A value that is not visible ASCII is recorded as `<non-ascii>`.
    pub headers: Vec<(String, String)>,
    /// The status the simulator answered with, including a scripted fault's status.
    pub status: u16,
}

impl LoggedRequest {
    /// The value of a logged header, by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

pub(crate) async fn log_request(
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
    shared.lock().push_log(LoggedRequest {
        origin,
        method,
        path,
        query,
        headers,
        status: response.status().as_u16(),
    });
    response
}

impl GraphSim {
    /// The requests received so far, in the order the responses were completed. Only the
    /// newest [`MAX_LOGGED_REQUESTS`](crate::MAX_LOGGED_REQUESTS) are kept; older ones are
    /// dropped first. A long test that asserts on the log should call
    /// [`GraphSim::clear_requests`] before the part it checks.
    #[must_use]
    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.shared.lock().log.iter().cloned().collect()
    }

    /// Empties the request log.
    pub fn clear_requests(&self) {
        self.shared.lock().log.clear();
    }
}
