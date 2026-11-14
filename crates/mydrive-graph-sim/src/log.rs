//! The request log tests assert on.

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
