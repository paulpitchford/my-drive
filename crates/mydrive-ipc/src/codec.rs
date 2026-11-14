//! Newline-delimited JSON framing (ADR-0009 "Framing", R2).
//!
//! Each message is one JSON document on one line, terminated by `\n`, at most
//! [`MAX_LINE_BYTES`] long (not counting the `\n`). [`LineDecoder`] is sans-I/O: feed it bytes
//! as they arrive, in chunks of any size, and take complete messages out. It never merges two
//! lines and never splits one, whatever the read boundaries (WP-005 AC2). A bad line yields
//! one typed error and decoding continues with the next line (AC3).
//!
//! [`FrameReader`] and [`write_message`] adapt it to blocking [`std::io`] streams; an async
//! server can drive [`LineDecoder`] directly.

use std::collections::VecDeque;
use std::io::{self, Read, Write};

use serde::Serialize;
use serde_json::Value;

use crate::error::{ErrorCode, RpcError};

/// The longest line accepted or sent, in bytes, excluding the terminating `\n`: 1 MiB.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// A line that could not be decoded. `line` is the 1-based line number on the stream.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The line is longer than [`MAX_LINE_BYTES`]. Its bytes were discarded up to and
    /// including the next `\n`.
    #[error("line {line} is longer than the {limit}-byte limit and was discarded")]
    TooLong {
        /// The line number.
        line: u64,
        /// The limit it exceeded.
        limit: usize,
    },
    /// The line is not valid UTF-8.
    #[error("line {line} is not valid UTF-8 (valid up to byte {valid_up_to})")]
    InvalidUtf8 {
        /// The line number.
        line: u64,
        /// Bytes before the first invalid sequence.
        valid_up_to: usize,
    },
    /// The line is UTF-8 but not one valid JSON document.
    #[error("line {line} is not valid JSON: {source}")]
    InvalidJson {
        /// The line number.
        line: u64,
        /// The parser's error.
        #[source]
        source: serde_json::Error,
    },
    /// The stream ended in the middle of a line (bytes after the last `\n`).
    #[error("stream ended inside line {line} after {bytes} bytes without a newline")]
    Truncated {
        /// The line number.
        line: u64,
        /// How many bytes of the unfinished line were received.
        bytes: usize,
    },
}

impl FrameError {
    /// The JSON-RPC error to answer with (sent with `"id": null`): `parse-error` for bad
    /// UTF-8 or JSON, `invalid-request` for an oversize or truncated line.
    #[must_use]
    pub fn to_rpc_error(&self) -> RpcError {
        let code = match self {
            Self::InvalidUtf8 { .. } | Self::InvalidJson { .. } => ErrorCode::ParseError,
            Self::TooLong { .. } | Self::Truncated { .. } => ErrorCode::InvalidRequest,
        };
        RpcError::new(code, self.to_string())
    }
}

/// Splits a byte stream into JSON messages, one per line.
///
/// Memory is bounded by [`MAX_LINE_BYTES`] plus the size of one pushed chunk: an oversize
/// line is dropped as it arrives rather than buffered. Blank lines (empty or only JSON
/// whitespace) are skipped without output.
#[derive(Debug, Default)]
pub struct LineDecoder {
    /// Bytes of the current, unfinished line.
    buf: Vec<u8>,
    /// True while skipping the rest of an oversize line.
    discarding: bool,
    /// Lines finished so far; the current line's number is `lines + 1`.
    lines: u64,
    /// Decoded messages and errors not yet taken.
    ready: VecDeque<Result<Value, FrameError>>,
}

impl LineDecoder {
    /// A decoder at the start of a stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes read from the stream. Decoded results become available from
    /// [`LineDecoder::next_message`].
    pub fn push(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            if self.discarding {
                match newline {
                    // The oversize line's error was already queued.
                    None => return,
                    Some(i) => {
                        self.discarding = false;
                        self.lines += 1;
                        bytes = &bytes[i + 1..];
                    }
                }
                continue;
            }
            match newline {
                Some(i) => {
                    let line = self.lines + 1;
                    if self.buf.len() + i > MAX_LINE_BYTES {
                        self.ready.push_back(Err(FrameError::TooLong {
                            line,
                            limit: MAX_LINE_BYTES,
                        }));
                    } else {
                        self.buf.extend_from_slice(&bytes[..i]);
                        if let Some(result) = parse_line(&self.buf, line) {
                            self.ready.push_back(result);
                        }
                    }
                    self.buf.clear();
                    self.lines += 1;
                    bytes = &bytes[i + 1..];
                }
                None => {
                    if self.buf.len() + bytes.len() > MAX_LINE_BYTES {
                        self.ready.push_back(Err(FrameError::TooLong {
                            line: self.lines + 1,
                            limit: MAX_LINE_BYTES,
                        }));
                        self.buf.clear();
                        self.discarding = true;
                    } else {
                        self.buf.extend_from_slice(bytes);
                    }
                    return;
                }
            }
        }
    }

    /// The next decoded message or line error, in stream order, if one is complete.
    pub fn next_message(&mut self) -> Option<Result<Value, FrameError>> {
        self.ready.pop_front()
    }

    /// Call at end of stream, after taking every queued result. Returns
    /// [`FrameError::Truncated`] if the stream stopped inside a line, so a cut-off message is
    /// reported rather than silently lost. An oversize line in progress was already reported.
    pub fn finish(&mut self) -> Option<FrameError> {
        if self.buf.is_empty() {
            return None;
        }
        let err = FrameError::Truncated {
            line: self.lines + 1,
            bytes: self.buf.len(),
        };
        self.buf.clear();
        Some(err)
    }
}

/// Decodes one complete line; `None` for a blank line.
fn parse_line(bytes: &[u8], line: u64) -> Option<Result<Value, FrameError>> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            return Some(Err(FrameError::InvalidUtf8 {
                line,
                valid_up_to: e.valid_up_to(),
            }));
        }
    };
    if text.trim_matches([' ', '\t', '\r']).is_empty() {
        return None;
    }
    Some(serde_json::from_str(text).map_err(|source| FrameError::InvalidJson { line, source }))
}

/// A message that cannot be sent.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// Serialising the message failed.
    #[error("cannot serialise message: {0}")]
    Serialise(#[from] serde_json::Error),
    /// The serialised message is longer than [`MAX_LINE_BYTES`]; the peer would reject it.
    #[error("message is {len} bytes, over the {limit}-byte line limit")]
    TooLong {
        /// The serialised length.
        len: usize,
        /// The limit.
        limit: usize,
    },
}

/// Serialises `message` as one line: compact JSON followed by `\n`.
///
/// Compact JSON never contains a raw newline (newlines inside strings are escaped), so the
/// result is exactly one line (ADR-0009 R2).
///
/// # Errors
///
/// [`EncodeError`] if serialisation fails or the line would exceed [`MAX_LINE_BYTES`].
pub fn encode_line<T: Serialize + ?Sized>(message: &T) -> Result<Vec<u8>, EncodeError> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() > MAX_LINE_BYTES {
        return Err(EncodeError::TooLong {
            len: line.len(),
            limit: MAX_LINE_BYTES,
        });
    }
    line.push(b'\n');
    Ok(line)
}

/// Writes `message` to `writer` as one line and flushes.
///
/// # Errors
///
/// An [`io::Error`] if encoding or writing fails; an encoding failure has kind
/// [`io::ErrorKind::InvalidInput`] and nothing is written.
pub fn write_message<W: Write, T: Serialize + ?Sized>(
    writer: &mut W,
    message: &T,
) -> io::Result<()> {
    let line = encode_line(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    writer.write_all(&line)?;
    writer.flush()
}

/// A failure reading a message from a stream.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The underlying read failed.
    #[error("cannot read from the connection: {0}")]
    Io(#[from] io::Error),
    /// A line could not be decoded. Reading can continue with the next line.
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// Reads messages from a blocking byte stream.
#[derive(Debug)]
pub struct FrameReader<R> {
    reader: R,
    decoder: LineDecoder,
    eof: bool,
}

impl<R: Read> FrameReader<R> {
    /// Wraps a reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            decoder: LineDecoder::new(),
            eof: false,
        }
    }

    /// The next message; `Ok(None)` at a clean end of stream.
    ///
    /// # Errors
    ///
    /// [`ReadError::Frame`] for a bad line (call again for the next one), including a
    /// truncated last line; [`ReadError::Io`] if the read fails.
    pub fn read_message(&mut self) -> Result<Option<Value>, ReadError> {
        let mut chunk = [0_u8; 8192];
        loop {
            if let Some(result) = self.decoder.next_message() {
                return result.map(Some).map_err(ReadError::from);
            }
            if self.eof {
                return match self.decoder.finish() {
                    Some(err) => Err(err.into()),
                    None => Ok(None),
                };
            }
            match self.reader.read(&mut chunk) {
                Ok(0) => self.eof = true,
                Ok(n) => self.decoder.push(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The wrapped reader.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn drain(decoder: &mut LineDecoder) -> Vec<Result<Value, FrameError>> {
        std::iter::from_fn(|| decoder.next_message()).collect()
    }

    fn values(results: Vec<Result<Value, FrameError>>) -> Vec<Value> {
        results.into_iter().map(Result::unwrap).collect()
    }

    #[test]
    fn decodes_lines_in_one_chunk_and_skips_blank_lines() {
        let mut d = LineDecoder::new();
        d.push(b"{\"a\":1}\n\n  \r\n[2]\r\n\"x\"\n");
        assert_eq!(
            values(drain(&mut d)),
            [json!({"a":1}), json!([2]), json!("x")]
        );
        assert!(d.finish().is_none());
    }

    #[test]
    fn byte_at_a_time_delivery() {
        let mut d = LineDecoder::new();
        let mut out = Vec::new();
        for b in b"{\"k\":\"v\\n\"}\n{}\n" {
            d.push(std::slice::from_ref(b));
            out.extend(drain(&mut d));
        }
        assert_eq!(values(out), [json!({"k":"v\n"}), json!({})]);
    }

    /// WP-005 AC3: an oversize line is a typed error and the next line still decodes, whether
    /// the line arrives in one chunk or in many.
    #[test]
    fn oversize_line_then_stream_continues() {
        let big = vec![b'x'; MAX_LINE_BYTES + 1];
        for chunk in [usize::MAX, 4096, 1] {
            let mut stream = Vec::new();
            stream.extend_from_slice(b"{\"before\":true}\n");
            stream.extend_from_slice(&big);
            stream.extend_from_slice(b"\n{\"after\":true}\n");
            let mut d = LineDecoder::new();
            let mut out = Vec::new();
            for part in stream.chunks(chunk.min(stream.len())) {
                d.push(part);
                out.extend(drain(&mut d));
            }
            assert_eq!(out.len(), 3, "chunk size {chunk}");
            assert_eq!(out[0].as_ref().unwrap(), &json!({"before": true}));
            assert!(
                matches!(
                    out[1],
                    Err(FrameError::TooLong {
                        line: 2,
                        limit: MAX_LINE_BYTES
                    })
                ),
                "chunk size {chunk}: {:?}",
                out[1]
            );
            assert_eq!(out[2].as_ref().unwrap(), &json!({"after": true}));
            assert!(d.finish().is_none());
        }
    }

    #[test]
    fn line_of_exactly_the_limit_is_accepted() {
        // A JSON string of MAX_LINE_BYTES bytes including its quotes.
        let mut line = vec![b'"'];
        line.extend(std::iter::repeat_n(b'a', MAX_LINE_BYTES - 2));
        line.extend_from_slice(b"\"\n");
        let mut d = LineDecoder::new();
        d.push(&line);
        let out = drain(&mut d);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
    }

    #[test]
    fn invalid_utf8_and_invalid_json_are_typed_and_recoverable() {
        let mut d = LineDecoder::new();
        d.push(b"{\"a\":\"\xff\"}\n{not json}\n{}{}\n{\"ok\":1}\n");
        let out = drain(&mut d);
        assert!(matches!(
            out[0],
            Err(FrameError::InvalidUtf8 {
                line: 1,
                valid_up_to: 6
            })
        ));
        assert!(matches!(
            out[1],
            Err(FrameError::InvalidJson { line: 2, .. })
        ));
        // Two documents on one line are not one JSON document.
        assert!(matches!(
            out[2],
            Err(FrameError::InvalidJson { line: 3, .. })
        ));
        assert_eq!(out[3].as_ref().unwrap(), &json!({"ok": 1}));
        assert_eq!(
            out[0].as_ref().unwrap_err().to_rpc_error().kind(),
            Some(ErrorCode::ParseError)
        );
    }

    #[test]
    fn truncated_last_line_is_reported() {
        let mut d = LineDecoder::new();
        d.push(b"{}\n{\"half\":");
        assert_eq!(values(drain(&mut d)), [json!({})]);
        assert!(matches!(
            d.finish(),
            Some(FrameError::Truncated { line: 2, bytes: 8 })
        ));
    }

    /// ADR-0009 R2: an encoded message is one line of JSON ending in `\n`, even when its
    /// strings contain newlines.
    #[test]
    fn encoded_message_is_a_single_line() {
        let msg = json!({"jsonrpc":"2.0","method":"m","params":{"s":"a\nb\r\nc\u{2028}"}});
        let line = encode_line(&msg).unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        let mut d = LineDecoder::new();
        d.push(&line);
        assert_eq!(values(drain(&mut d)), [msg]);
    }

    #[test]
    fn encoding_refuses_oversize_messages() {
        let msg = "x".repeat(MAX_LINE_BYTES);
        assert!(matches!(
            encode_line(&msg),
            Err(EncodeError::TooLong { .. })
        ));
    }

    #[test]
    fn frame_reader_over_std_io() {
        let data = b"{\"a\":1}\nnope\n{\"b\":2}\n{\"c\"".to_vec();
        let mut r = FrameReader::new(std::io::Cursor::new(data));
        assert_eq!(r.read_message().unwrap(), Some(json!({"a":1})));
        assert!(matches!(
            r.read_message(),
            Err(ReadError::Frame(FrameError::InvalidJson { .. }))
        ));
        assert_eq!(r.read_message().unwrap(), Some(json!({"b":2})));
        assert!(matches!(
            r.read_message(),
            Err(ReadError::Frame(FrameError::Truncated { .. }))
        ));
        assert!(r.read_message().unwrap().is_none());

        let mut out = Vec::new();
        write_message(&mut out, &json!({"z": "\n"})).unwrap();
        assert_eq!(out, b"{\"z\":\"\\n\"}\n");
    }

    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            // Any Unicode, including newlines, carriage returns and quotes.
            any::<String>().prop_map(Value::String),
            "[\n\r\"\\\\ a-z]{0,8}".prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::btree_map(any::<String>(), inner, 0..6)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// WP-005 AC2: whatever the read boundaries, every message comes out intact, in
        /// order, with none lost, split or merged.
        #[test]
        fn messages_survive_arbitrary_split_points(
            messages in prop::collection::vec(arb_json(), 0..12),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..24),
        ) {
            let mut stream = Vec::new();
            for m in &messages {
                stream.extend(encode_line(m).unwrap());
            }
            let mut points: Vec<usize> =
                cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
            points.push(0);
            points.push(stream.len());
            points.sort_unstable();
            points.dedup();

            let mut d = LineDecoder::new();
            let mut out = Vec::new();
            for pair in points.windows(2) {
                d.push(&stream[pair[0]..pair[1]]);
                out.extend(drain(&mut d));
            }
            prop_assert!(d.finish().is_none());
            let out: Vec<Value> = out.into_iter().map(Result::unwrap).collect();
            prop_assert_eq!(out, messages);
        }
    }
}
