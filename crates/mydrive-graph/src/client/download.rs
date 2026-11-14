//! A content download in progress, and what it covers.
//!
//! The caller always passes the item's expected size (from its metadata), and every
//! response is checked against it and against the requested range before any byte is
//! handed out:
//!
//! - A content-encoded response (gzip from a proxy, say) is refused: its bytes are not
//!   the file's bytes.
//! - `200` is the whole file from offset 0, and must total exactly the expected size.
//! - `206` must confirm the requested start, end no later than the requested end, and
//!   state the expected size as its total. It may end early only at the last byte of the
//!   file.
//! - The body must deliver exactly the length promised; ending early is
//!   [`GraphErrorKind::Truncated`], never a short success.

use std::fmt;

use reqwest::header::{self, HeaderMap};
use reqwest::{Response, StatusCode};

use super::error::{GraphError, GraphErrorKind, Operation};
use super::{content_length, transport};

/// A byte range to request: `start` to `end`, both inclusive, never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteRange {
    start: u64,
    end: u64,
}

impl ByteRange {
    /// The `len` bytes from `start`. `None` if `len` is 0 or the range would pass
    /// `u64::MAX`.
    #[must_use]
    pub fn new(start: u64, len: u64) -> Option<Self> {
        let end = start.checked_add(len.checked_sub(1)?)?;
        Some(Self { start, end })
    }

    /// The first byte.
    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The last byte, inclusive.
    #[must_use]
    pub fn end_inclusive(&self) -> u64 {
        self.end
    }

    /// The number of bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Always `false`: a range is never empty. Present for clippy's `len` convention.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    pub(super) fn header_value(self) -> String {
        format!("bytes={}-{}", self.start, self.end)
    }
}

/// A download whose head has arrived and been checked. Match on it: what arrived is not
/// necessarily what was requested (ADR-0011 R11).
#[derive(Debug)]
#[must_use = "a download must be matched: it may be the whole file instead of the range"]
pub enum Download {
    /// `206 Partial Content`: bytes `start..=end` of the file. `start` is the requested
    /// start; `end` is the requested end, or the file's last byte if the request went
    /// past it.
    Range {
        /// The first byte.
        start: u64,
        /// The last byte, inclusive.
        end: u64,
        /// The bytes.
        body: Body,
    },
    /// `200 OK`: the whole file from offset 0, whether or not a range was requested; a
    /// server may ignore `Range` (ADR-0011 R11). It totals exactly the expected size.
    WholeFile {
        /// The bytes.
        body: Body,
    },
}

/// The body of a checked download. Read it with [`Body::chunk`].
///
/// If it ends before its promised length, [`Body::chunk`] fails with
/// [`GraphErrorKind::Truncated`] (or a transport error), never `Ok(None)`. Bytes already
/// returned were real bytes of the stated coverage, but the download as a whole has
/// failed.
pub struct Body {
    operation: Operation,
    etag: Option<String>,
    expected: u64,
    whole_file: bool,
    received: u64,
    state: ReadState,
    response: Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    Reading,
    Done,
    Failed,
}

impl fmt::Debug for Body {
    // The response is left out: its `Debug` shows the URL, which may be pre-authenticated.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Body")
            .field("operation", &self.operation)
            .field("expected", &self.expected)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

/// Parses `Content-Range: bytes a-b/total` (or `/*`).
fn content_range(headers: &HeaderMap) -> Option<(u64, u64, Option<u64>)> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let spec = value.trim().strip_prefix("bytes ")?;
    let (span, total) = spec.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let start = first.trim().parse::<u64>().ok()?;
    let end = last.trim().parse::<u64>().ok()?;
    let total = match total.trim() {
        "*" => None,
        n => Some(n.parse::<u64>().ok()?),
    };
    Some((start, end, total))
}

/// Refuses a response whose `Content-Encoding` is anything but absent or `identity`.
pub(super) fn require_identity(headers: &HeaderMap) -> Result<(), GraphErrorKind> {
    let Some(value) = headers.get(header::CONTENT_ENCODING) else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(value.as_bytes());
    if text.trim().eq_ignore_ascii_case("identity") {
        return Ok(());
    }
    let encoding = text
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(64)
        .collect();
    Err(GraphErrorKind::UnsupportedEncoding { encoding })
}

impl Download {
    pub(super) fn new(
        operation: Operation,
        response: Response,
        requested: Option<ByteRange>,
        expected_size: u64,
    ) -> Result<Self, GraphError> {
        match Self::check(&response, requested, expected_size) {
            Ok(coverage) => {
                let etag = response
                    .headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                tracing::debug!(
                    operation = %operation,
                    status = response.status().as_u16(),
                    whole_file = coverage.is_none(),
                    range_requested = requested.is_some(),
                    "download started"
                );
                let (expected, whole_file) = match coverage {
                    Some((start, end)) => (end - start + 1, false),
                    None => (expected_size, true),
                };
                let body = Body {
                    operation,
                    etag,
                    expected,
                    whole_file,
                    received: 0,
                    state: ReadState::Reading,
                    response,
                };
                Ok(match coverage {
                    Some((start, end)) => Self::Range { start, end, body },
                    None => Self::WholeFile { body },
                })
            }
            Err(kind) => Err(GraphError::new(operation, kind)),
        }
    }

    /// Checks the head. `Some((start, end))` for a confirmed range, `None` for the whole
    /// file.
    fn check(
        response: &Response,
        requested: Option<ByteRange>,
        expected_size: u64,
    ) -> Result<Option<(u64, u64)>, GraphErrorKind> {
        let malformed = |reason: &str| Err(GraphErrorKind::Malformed(reason.into()));
        let headers = response.headers();
        require_identity(headers)?;
        let length = content_length(headers)?;
        match response.status() {
            StatusCode::OK => {
                if let Some(announced) = length
                    && announced != expected_size
                {
                    return Err(GraphErrorKind::SizeMismatch {
                        expected: expected_size,
                        reported: Some(announced),
                    });
                }
                Ok(None)
            }
            StatusCode::PARTIAL_CONTENT => {
                let Some(requested) = requested else {
                    return malformed("206 Partial Content to a request without Range");
                };
                let Some((start, end, total)) = content_range(headers) else {
                    return malformed("206 without a single-range Content-Range");
                };
                if total != Some(expected_size) {
                    return Err(GraphErrorKind::SizeMismatch {
                        expected: expected_size,
                        reported: total,
                    });
                }
                // Anything but the requested start, bytes past the requested end, or an
                // early end that is not the end of the file, is not what was asked for
                // and must not be reported as it.
                if start != requested.start()
                    || end < start
                    || end >= expected_size
                    || end > requested.end_inclusive()
                    || (end < requested.end_inclusive() && end + 1 != expected_size)
                {
                    return malformed("206 for a different range than requested");
                }
                if length.is_some_and(|n| n != end - start + 1) {
                    return malformed("206 whose Content-Length differs from its range");
                }
                Ok(Some((start, end)))
            }
            _ => malformed("a download response that is neither 200 nor 206"),
        }
    }
}

impl Body {
    /// The response's `ETag` header, if any. Whether Graph sends one is part of gate G1.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// The number of body bytes this response must deliver.
    #[must_use]
    pub fn expected_len(&self) -> u64 {
        self.expected
    }

    /// The body bytes delivered so far.
    #[must_use]
    pub fn received(&self) -> u64 {
        self.received
    }

    /// What the download was for.
    #[must_use]
    pub fn operation(&self) -> &Operation {
        &self.operation
    }

    /// The next piece of the body, or `Ok(None)` once all of it has arrived.
    ///
    /// # Errors
    ///
    /// [`GraphErrorKind::Truncated`] if the body ends before its promised length,
    /// [`GraphErrorKind::Transport`] if the connection fails otherwise, and
    /// [`GraphErrorKind::SizeMismatch`] (whole file) or [`GraphErrorKind::Malformed`]
    /// (range) if more bytes arrive than promised. After an error, every later call
    /// fails too.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, GraphError> {
        match self.state {
            ReadState::Done => return Ok(None),
            ReadState::Failed => {
                return Err(self.error(GraphErrorKind::Malformed(
                    "the download already failed".into(),
                )));
            }
            ReadState::Reading => {}
        }
        let short = |received: u64, expected: u64| {
            (received < expected).then_some(GraphErrorKind::Truncated { expected, received })
        };
        match self.response.chunk().await {
            Ok(Some(chunk)) => {
                self.received += chunk.len() as u64;
                if self.received > self.expected {
                    let kind = if self.whole_file {
                        GraphErrorKind::SizeMismatch {
                            expected: self.expected,
                            reported: Some(self.received),
                        }
                    } else {
                        GraphErrorKind::Malformed("more body bytes than the range".into())
                    };
                    return Err(self.fail(kind));
                }
                Ok(Some(chunk.into()))
            }
            Ok(None) => {
                if let Some(kind) = short(self.received, self.expected) {
                    return Err(self.fail(kind));
                }
                self.state = ReadState::Done;
                Ok(None)
            }
            Err(e) => {
                let kind = short(self.received, self.expected).unwrap_or_else(|| transport(e));
                Err(self.fail(kind))
            }
        }
    }

    /// Reads the rest of the body into memory.
    ///
    /// # Errors
    ///
    /// As for [`Body::chunk`].
    pub async fn bytes(mut self) -> Result<Vec<u8>, GraphError> {
        let mut out = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    fn fail(&mut self, kind: GraphErrorKind) -> GraphError {
        self.state = ReadState::Failed;
        tracing::debug!(
            operation = %self.operation,
            received = self.received,
            expected = self.expected,
            "download body failed"
        );
        self.error(kind)
    }

    fn error(&self, kind: GraphErrorKind) -> GraphError {
        GraphError::new(self.operation.clone(), kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges() {
        let r = ByteRange::new(10, 5).unwrap();
        assert_eq!((r.start(), r.end_inclusive(), r.len()), (10, 14, 5));
        assert_eq!(r.header_value(), "bytes=10-14");
        assert!(ByteRange::new(0, 0).is_none());
        assert!(ByteRange::new(u64::MAX, 2).is_none());
        assert!(ByteRange::new(u64::MAX, 1).is_some());
    }

    #[test]
    fn parses_content_range() {
        let mut h = HeaderMap::new();
        assert_eq!(content_range(&h), None);
        h.insert(header::CONTENT_RANGE, "bytes 0-9/100".parse().unwrap());
        assert_eq!(content_range(&h), Some((0, 9, Some(100))));
        h.insert(header::CONTENT_RANGE, "bytes 5-9/*".parse().unwrap());
        assert_eq!(content_range(&h), Some((5, 9, None)));
        for bad in ["bytes */100", "items 0-9/100", "bytes 0-9", "bytes a-9/100"] {
            h.insert(header::CONTENT_RANGE, bad.parse().unwrap());
            assert_eq!(content_range(&h), None, "{bad}");
        }
    }

    #[test]
    fn only_identity_encoding_is_accepted() {
        let mut h = HeaderMap::new();
        assert!(require_identity(&h).is_ok());
        h.insert(header::CONTENT_ENCODING, "Identity".parse().unwrap());
        assert!(require_identity(&h).is_ok());
        for bad in ["gzip", "br", "identity, gzip", ""] {
            h.insert(header::CONTENT_ENCODING, bad.parse().unwrap());
            assert!(
                matches!(
                    require_identity(&h),
                    Err(GraphErrorKind::UnsupportedEncoding { .. })
                ),
                "{bad:?}"
            );
        }
    }
}
