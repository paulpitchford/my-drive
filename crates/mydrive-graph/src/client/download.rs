//! A content download in progress, and what it covers.

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

/// What a download response actually carries. Never assume it is what was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// `206 Partial Content`: bytes `start..=end` of the file, as the server's
    /// `Content-Range` confirmed. `start` is the requested start; `end` may be earlier
    /// than requested when the file is shorter.
    Range {
        /// The first byte.
        start: u64,
        /// The last byte, inclusive.
        end: u64,
        /// The file's total length, if the server stated it.
        total: Option<u64>,
    },
    /// `200 OK`: the whole file from offset 0, whether or not a range was requested; a
    /// server may ignore `Range` (ADR-0011 R11).
    WholeFile {
        /// The announced length, if any. Compare it with the expected item size before
        /// trusting the content.
        length: Option<u64>,
    },
}

impl Coverage {
    /// Whether this is the whole file from offset 0.
    #[must_use]
    pub fn is_whole_file(&self) -> bool {
        matches!(self, Self::WholeFile { .. })
    }
}

/// A download whose head has arrived. Read the body with [`Download::chunk`].
///
/// The body is checked against its announced length: if it ends early,
/// [`Download::chunk`] fails with [`GraphErrorKind::Truncated`] (or a transport error),
/// never `Ok(None)`. Bytes already returned were real bytes of the stated coverage, but
/// the download as a whole has failed.
pub struct Download {
    operation: Operation,
    coverage: Coverage,
    etag: Option<String>,
    expected: Option<u64>,
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

impl fmt::Debug for Download {
    // The response is left out: its `Debug` shows the URL, which may be pre-authenticated.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("operation", &self.operation)
            .field("coverage", &self.coverage)
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

impl Download {
    pub(super) fn new(
        operation: Operation,
        response: Response,
        requested: Option<ByteRange>,
    ) -> Result<Self, GraphError> {
        let fail = |op: Operation, reason: &str| {
            Err(GraphError::new(
                op,
                GraphErrorKind::Malformed(reason.into()),
            ))
        };
        let headers = response.headers();
        let length = match content_length(headers) {
            Ok(length) => length,
            Err(kind) => return Err(GraphError::new(operation, kind)),
        };
        let etag = headers
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let (coverage, expected) = match response.status() {
            StatusCode::OK => (Coverage::WholeFile { length }, length),
            StatusCode::PARTIAL_CONTENT => {
                let Some(requested) = requested else {
                    return fail(operation, "206 Partial Content to a request without Range");
                };
                let Some((start, end, total)) = content_range(headers) else {
                    return fail(operation, "206 without a single-range Content-Range");
                };
                // Anything but the requested start, or bytes past the requested end, is
                // not what was asked for and must not be reported as it.
                if start != requested.start()
                    || end < start
                    || end > requested.end_inclusive()
                    || total.is_some_and(|t| end >= t)
                {
                    return fail(operation, "206 for a different range than requested");
                }
                let span = end - start + 1;
                if length.is_some_and(|n| n != span) {
                    return fail(operation, "206 whose Content-Length differs from its range");
                }
                (Coverage::Range { start, end, total }, Some(span))
            }
            _ => return fail(operation, "a download response that is neither 200 nor 206"),
        };
        tracing::debug!(
            operation = %operation,
            status = response.status().as_u16(),
            whole_file = coverage.is_whole_file(),
            range_requested = requested.is_some(),
            "download started"
        );
        Ok(Self {
            operation,
            coverage,
            etag,
            expected,
            received: 0,
            state: ReadState::Reading,
            response,
        })
    }

    /// What this response carries.
    #[must_use]
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// The response's `ETag` header, if any. Whether Graph sends one is part of gate G1.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// The number of body bytes this response must deliver, if known.
    #[must_use]
    pub fn expected_len(&self) -> Option<u64> {
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
    /// [`GraphErrorKind::Truncated`] if the body ends before its announced length,
    /// [`GraphErrorKind::Transport`] if the connection fails otherwise, and
    /// [`GraphErrorKind::Malformed`] if more bytes arrive than announced. After an error,
    /// every later call fails too.
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
        let result = self.response.chunk().await;
        let short = |received: u64, expected: Option<u64>| {
            expected
                .filter(|&e| received < e)
                .map(|expected| GraphErrorKind::Truncated { expected, received })
        };
        match result {
            Ok(Some(chunk)) => {
                self.received += chunk.len() as u64;
                if self.expected.is_some_and(|e| self.received > e) {
                    return Err(self.fail(GraphErrorKind::Malformed(
                        "more body bytes than announced".into(),
                    )));
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
    /// As for [`Download::chunk`].
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
}
