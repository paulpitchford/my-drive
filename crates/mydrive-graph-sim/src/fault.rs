//! Scripted faults: the next request that matches gets a chosen failure (ADR-0017 R5).
//!
//! The fault types, the middleware that applies them on both origins and the control API
//! that queues them. Documented in `docs/faults.md`.

use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Request, State as AxState};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::GraphSim;
use crate::conn::ConnTap;
use crate::server::{Ctx, graph_error};

/// One scripted fault. It applies to exactly one request, the first one after it was
/// queued that matches, and is then removed from the queue.
///
/// Faults are matched in the order they were queued, so a script of faults gives the same
/// results on every run for the same sequence of requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub(crate) method: Option<Method>,
    pub(crate) path_contains: Option<String>,
    pub(crate) action: FaultAction,
}

/// What a fault does to the request it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaultAction {
    /// Answer with this status and a Graph error body instead of handling the request.
    Status {
        /// The status to return.
        status: StatusCode,
        /// The value of a `Retry-After` header in seconds, if one is sent.
        retry_after: Option<u64>,
        /// The Graph error `code` in the body. `None` uses the code the simulator uses
        /// for that status elsewhere (see the README).
        code: Option<String>,
        /// The value of a `Location` header, if one is sent. The documented `410 Gone`
        /// resync responses carry the link to restart from here.
        location: Option<String>,
    },
    /// Wait this long, then handle the request normally.
    Delay(Duration),
    /// Handle the request normally, but send only the first `keep_bytes` bytes of the body
    /// while still announcing the full `Content-Length`, then close the connection.
    TruncateBody {
        /// How many body bytes are sent before the connection is closed.
        keep_bytes: usize,
    },
}

impl Fault {
    /// A fault that answers with `status` and a Graph error body.
    #[must_use]
    pub fn status(status: StatusCode) -> Self {
        Self::new(FaultAction::Status {
            status,
            retry_after: None,
            code: None,
            location: None,
        })
    }

    /// A fault that delays the request by `delay` before handling it.
    #[must_use]
    pub fn delay(delay: Duration) -> Self {
        Self::new(FaultAction::Delay(delay))
    }

    /// A fault that cuts the response body off after `keep_bytes` bytes.
    #[must_use]
    pub fn truncate_body(keep_bytes: usize) -> Self {
        Self::new(FaultAction::TruncateBody { keep_bytes })
    }

    fn new(action: FaultAction) -> Self {
        Self {
            method: None,
            path_contains: None,
            action,
        }
    }

    /// Adds a `Retry-After: <secs>` header. Has no effect on a fault that is not a status.
    #[must_use]
    pub fn retry_after(mut self, secs: u64) -> Self {
        if let FaultAction::Status { retry_after, .. } = &mut self.action {
            *retry_after = Some(secs);
        }
        self
    }

    /// Sets the Graph error `code` the body carries, for example
    /// `resyncChangesApplyDifferences`. Has no effect on a fault that is not a status.
    #[must_use]
    pub fn code(mut self, value: impl Into<String>) -> Self {
        if let FaultAction::Status { code, .. } = &mut self.action {
            *code = Some(value.into());
        }
        self
    }

    /// Adds a `Location: <value>` header. Has no effect on a fault that is not a status.
    #[must_use]
    pub fn location(mut self, value: impl Into<String>) -> Self {
        if let FaultAction::Status { location, .. } = &mut self.action {
            *location = Some(value.into());
        }
        self
    }

    /// Matches only requests with this method. Without it any method matches.
    #[must_use]
    pub fn on_method(mut self, method: Method) -> Self {
        self.method = Some(method);
        self
    }

    /// Matches only requests whose path (without the query string) contains `fragment`.
    /// Without it any path matches.
    #[must_use]
    pub fn on_path(mut self, fragment: impl Into<String>) -> Self {
        self.path_contains = Some(fragment.into());
        self
    }

    /// What this fault does.
    #[must_use]
    pub fn action(&self) -> &FaultAction {
        &self.action
    }

    pub(crate) fn matches(&self, method: &Method, path: &str) -> bool {
        self.method.as_ref().is_none_or(|m| m == method)
            && self
                .path_contains
                .as_deref()
                .is_none_or(|fragment| path.contains(fragment))
    }
}

// ---- middleware ------------------------------------------------------------------------

pub(crate) async fn apply_fault(
    AxState((shared, _)): AxState<Ctx>,
    req: Request,
    next: Next,
) -> Response {
    let fault = shared.lock().take_fault(req.method(), req.uri().path());
    let Some(fault) = fault else {
        return next.run(req).await;
    };
    match fault.action {
        FaultAction::Status {
            status,
            retry_after,
            code,
            location,
        } => {
            let code = code.as_deref().unwrap_or_else(|| default_code(status));
            let mut response = graph_error(status, code, "Scripted fault.");
            if let Some(secs) = retry_after {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(secs));
            }
            if let Some(location) = location {
                match HeaderValue::from_str(&location) {
                    Ok(value) => {
                        response.headers_mut().insert(header::LOCATION, value);
                    }
                    Err(_) => {
                        return graph_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "generalException",
                            "The scripted fault's Location is not a valid header value.",
                        );
                    }
                }
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

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
    /// Queues a fault. It applies to the next request that matches it, then is removed.
    pub fn push_fault(&self, fault: Fault) {
        self.shared.lock().faults.push_back(fault);
    }

    /// How many queued faults have not yet matched a request.
    #[must_use]
    pub fn pending_faults(&self) -> usize {
        self.shared.lock().faults.len()
    }

    /// Removes every queued fault.
    pub fn clear_faults(&self) {
        self.shared.lock().faults.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_uses_method_and_path() {
        let fault = Fault::status(StatusCode::SERVICE_UNAVAILABLE)
            .on_method(Method::GET)
            .on_path("/delta");
        assert!(fault.matches(&Method::GET, "/v1.0/drives/d/root/delta"));
        assert!(!fault.matches(&Method::POST, "/v1.0/drives/d/root/delta"));
        assert!(!fault.matches(&Method::GET, "/v1.0/drives/d"));
        assert!(Fault::delay(Duration::ZERO).matches(&Method::PUT, "/anything"));
    }

    #[test]
    fn retry_after_only_applies_to_status_faults() {
        let status = Fault::status(StatusCode::TOO_MANY_REQUESTS).retry_after(7);
        assert_eq!(
            status.action(),
            &FaultAction::Status {
                status: StatusCode::TOO_MANY_REQUESTS,
                retry_after: Some(7),
                code: None,
                location: None,
            }
        );
        let delay = Fault::delay(Duration::from_secs(1)).retry_after(7);
        assert_eq!(delay.action(), &FaultAction::Delay(Duration::from_secs(1)));
    }

    #[test]
    fn code_and_location_only_apply_to_status_faults() {
        let gone = Fault::status(StatusCode::GONE)
            .code("resyncChangesApplyDifferences")
            .location("http://127.0.0.1:1/v1.0/drives/d/root/delta?token=x");
        assert_eq!(
            gone.action(),
            &FaultAction::Status {
                status: StatusCode::GONE,
                retry_after: None,
                code: Some("resyncChangesApplyDifferences".to_owned()),
                location: Some("http://127.0.0.1:1/v1.0/drives/d/root/delta?token=x".to_owned()),
            }
        );
        let truncate = Fault::truncate_body(3).code("x").location("y");
        assert_eq!(
            truncate.action(),
            &FaultAction::TruncateBody { keep_bytes: 3 }
        );
    }
}
