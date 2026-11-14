//! The typed error every client call returns.
//!
//! No variant carries a token, a response body, an error message from the server or a
//! URL: a pre-authenticated download URL is a credential, and transport errors have their
//! URL removed before they are stored. Graph error codes are kept, because policy
//! detection (gate G2) and resync handling need them.

use std::fmt;
use std::time::Duration;

use mydrive_core::ids::{DriveId, ItemId};

use crate::auth::TokenError;
use crate::model::{DeltaLink, ModelError};

/// What the client was doing when a call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// `GET /drives/{drive}`.
    GetDrive {
        /// The drive.
        drive: DriveId,
    },
    /// `GET /drives/{drive}/root`.
    GetRoot {
        /// The drive.
        drive: DriveId,
    },
    /// `GET /drives/{drive}/items/{item}`.
    GetItem {
        /// The drive.
        drive: DriveId,
        /// The item.
        item: ItemId,
    },
    /// A delta page, from the drive root or from a saved link.
    Delta {
        /// The drive.
        drive: DriveId,
    },
    /// `GET /drives/{drive}/items/{item}/content`, and any redirect it leads to.
    Download {
        /// The drive.
        drive: DriveId,
        /// The item.
        item: ItemId,
    },
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GetDrive { drive } => write!(f, "reading drive {drive}"),
            Self::GetRoot { drive } => write!(f, "reading the root of drive {drive}"),
            Self::GetItem { drive, item } => write!(f, "reading item {item} of drive {drive}"),
            Self::Delta { drive } => write!(f, "fetching a delta page of drive {drive}"),
            Self::Download { drive, item } => {
                write!(f, "downloading item {item} of drive {drive}")
            }
        }
    }
}

/// The Graph error codes of an error response: `error.code` and each nested
/// `innerError.code`, outermost first. Empty if the body had none (or was not JSON).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorCodes(Vec<String>);

impl ErrorCodes {
    pub(crate) fn new(codes: Vec<String>) -> Self {
        Self(codes)
    }

    /// The outermost code, `error.code`.
    #[must_use]
    pub fn primary(&self) -> Option<&str> {
        self.0.first().map(String::as_str)
    }

    /// Every code, outermost first.
    #[must_use]
    pub fn all(&self) -> &[String] {
        &self.0
    }

    /// Whether any level carries `code`.
    #[must_use]
    pub fn contains(&self, code: &str) -> bool {
        self.0.iter().any(|c| c == code)
    }
}

impl fmt::Display for ErrorCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("no error code")
        } else {
            f.write_str(&self.0.join("/"))
        }
    }
}

/// Which `410 Gone` resync Graph asked for (delta documentation, ADR-0011 R3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resync {
    /// `resyncChangesApplyDifferences`: replace local items with the server's versions,
    /// keeping local changes that have not been uploaded.
    ApplyDifferences,
    /// `resyncChangesUploadDifferences`: upload local items the service did not return,
    /// and keep both copies when unsure which is newer.
    UploadDifferences,
    /// A `410` whose code is neither of the documented ones. Handle it at least as
    /// conservatively as [`Resync::UploadDifferences`]: never discard local content.
    Unknown {
        /// The codes the response carried.
        codes: ErrorCodes,
    },
}

/// What went wrong, without the operation.
#[derive(Debug, thiserror::Error)]
pub enum GraphErrorKind {
    /// The token provider could not give a token: needs sign-in, a transient refresh
    /// failure, a closed provider, and so on. No request was retried because of it.
    #[error("no access token: {0}")]
    Token(#[source] TokenError),
    /// Graph answered `401` to the request and again after one forced token refresh.
    #[error("Graph rejected the access token ({codes})")]
    Unauthorized {
        /// The codes of the last `401`.
        codes: ErrorCodes,
    },
    /// The redirected download host answered `401`: the pre-authenticated URL is no
    /// longer valid. Not a token problem, so no refresh is attempted; a new `/content`
    /// request gets a new URL. (A `403` there is [`GraphErrorKind::Forbidden`].)
    #[error("the download URL was refused (HTTP {status})")]
    DownloadUrlRejected {
        /// The status.
        status: u16,
    },
    /// `403 Forbidden`. The codes are kept for policy detection.
    #[error("access denied ({codes})")]
    Forbidden {
        /// The error codes.
        codes: ErrorCodes,
    },
    /// `404 Not Found`.
    #[error("not found ({codes})")]
    NotFound {
        /// The error codes.
        codes: ErrorCodes,
    },
    /// `410 Gone` on delta: the token is no longer valid and the drive must be resynced.
    #[error("delta needs a resync ({resync:?})")]
    ResyncRequired {
        /// Which resync.
        resync: Resync,
        /// Where to restart enumeration, from the `Location` header, when present and on
        /// the client's own Graph origin. Without it, restart from the drive root.
        restart: Option<DeltaLink>,
    },
    /// `429 Too Many Requests` or `503 Service Unavailable`. The client does not wait or
    /// retry; the tenant's scheduler decides (ADR-0011 R6).
    #[error("throttled (HTTP {status}){}", retry_after.map(|d| format!(", retry after {} s", d.as_secs())).unwrap_or_default())]
    Throttled {
        /// `429` or `503`.
        status: u16,
        /// `Retry-After` in seconds, if the response carried a usable one.
        retry_after: Option<Duration>,
    },
    /// Another `5xx`.
    #[error("Graph server error (HTTP {status}, {codes})")]
    ServerError {
        /// The status.
        status: u16,
        /// The error codes.
        codes: ErrorCodes,
    },
    /// Any other status this call does not expect, for example `400`, `409` or `416`.
    #[error("unexpected HTTP {status} ({codes})")]
    UnexpectedStatus {
        /// The status.
        status: u16,
        /// The error codes.
        codes: ErrorCodes,
    },
    /// The request could not be sent, or the connection failed while reading the
    /// response. The error has had its URL removed.
    #[error("transport error: {0}")]
    Transport(#[source] reqwest::Error),
    /// The body ended before the announced length. Never reported as a short read.
    #[error("the response body ended after {received} of {expected} bytes")]
    Truncated {
        /// The length the response announced.
        expected: u64,
        /// The bytes that arrived.
        received: u64,
    },
    /// The response is not what Graph documents for this call.
    #[error("malformed response: {0}")]
    Malformed(String),
    /// A link to fetch does not point at this client's Graph base URL, so the bearer
    /// token is not sent to it.
    #[error("the link does not point at this client's Graph base URL")]
    ForeignLink,
}

impl From<ModelError> for GraphErrorKind {
    fn from(e: ModelError) -> Self {
        Self::Malformed(e.to_string())
    }
}

/// A failed client call: what was being done, and what went wrong.
///
/// Boxed, so a `Result` carrying it stays small.
#[derive(Debug)]
pub struct GraphError(Box<Parts>);

#[derive(Debug)]
struct Parts {
    operation: Operation,
    kind: GraphErrorKind,
}

impl GraphError {
    pub(crate) fn new(operation: Operation, kind: GraphErrorKind) -> Self {
        Self(Box::new(Parts { operation, kind }))
    }

    /// What was being done.
    #[must_use]
    pub fn operation(&self) -> &Operation {
        &self.0.operation
    }

    /// What went wrong.
    #[must_use]
    pub fn kind(&self) -> &GraphErrorKind {
        &self.0.kind
    }

    /// Splits the error into its parts.
    #[must_use]
    pub fn into_parts(self) -> (Operation, GraphErrorKind) {
        let parts = *self.0;
        (parts.operation, parts.kind)
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.0.operation, self.0.kind)
    }
}

impl std::error::Error for GraphError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0.kind)
    }
}
