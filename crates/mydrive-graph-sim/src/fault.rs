//! Scripted faults: the next request that matches gets a chosen failure (ADR-0017 R5).

use std::time::Duration;

use axum::http::{Method, StatusCode};

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
                retry_after: Some(7)
            }
        );
        let delay = Fault::delay(Duration::from_secs(1)).retry_after(7);
        assert_eq!(delay.action(), &FaultAction::Delay(Duration::from_secs(1)));
    }
}
