//! The JSON-RPC error object and the protocol's error-code table.
//!
//! The integer codes are part of the protocol: once released they never change meaning
//! (ADR-0009 "Compatibility"). Codes from the JSON-RPC 2.0 specification keep their standard
//! values; the protocol's own codes are positive, from 1001, outside the range JSON-RPC
//! reserves (-32768 to -32000). Error codes are an open set (ADR-0019): [`RpcError`] keeps
//! the raw integer, and `data` is kept as raw JSON read through tolerant accessors, so an
//! unexpected code or data shape never fails the response that carries it.

use std::fmt;

use mydrive_core::state::DehydrateRefusal;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::values::{BlockedReason, InvalidParamsReason, ReadOnlyReason, RefusalReason, WireSync};

/// Every error code this build defines, with its stable integer value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// JSON-RPC: the line was not valid JSON (or not valid UTF-8).
    ParseError,
    /// JSON-RPC: not a valid request object (including a batch array), or the line was over
    /// the limit.
    InvalidRequest,
    /// JSON-RPC: no such method.
    MethodNotFound,
    /// JSON-RPC: the parameters are malformed, including a relative path (WP-005 AC6).
    InvalidParams,
    /// JSON-RPC: an internal daemon error.
    Internal,
    /// The daemon is still starting; retry later (ADR-0019 R5).
    NotReady,
    /// The path is under a mount point but no such item is known.
    UnknownPath,
    /// The path is not under any mount point ("not managed" in F11).
    NotInAMount,
    /// The request was understood and refused; `data.reason` says why.
    Refused,
    /// The account that owns the item must sign in again.
    NeedsSignIn,
    /// Organisation policy refuses access to the item or its mount.
    BlockedByPolicy,
    /// The item or its mount is read-only.
    ReadOnly,
    /// The request needs the server and the daemon is offline.
    Offline,
    /// `hello` named no protocol version the daemon supports; `data.supported_versions`
    /// lists the ones it does (ADR-0009 R4).
    UnsupportedVersion,
    /// A request other than `hello` before `hello` completed on this connection. A client
    /// bug: retrying does not help (ADR-0019 R5).
    HelloRequired,
}

impl ErrorCode {
    /// Every code, in table order.
    pub const ALL: &'static [Self] = &[
        Self::ParseError,
        Self::InvalidRequest,
        Self::MethodNotFound,
        Self::InvalidParams,
        Self::Internal,
        Self::NotReady,
        Self::UnknownPath,
        Self::NotInAMount,
        Self::Refused,
        Self::NeedsSignIn,
        Self::BlockedByPolicy,
        Self::ReadOnly,
        Self::Offline,
        Self::UnsupportedVersion,
        Self::HelloRequired,
    ];

    /// The integer sent in `error.code`.
    #[must_use]
    pub const fn code(self) -> i64 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::Internal => -32603,
            Self::NotReady => 1001,
            Self::UnknownPath => 1002,
            Self::NotInAMount => 1003,
            Self::Refused => 1004,
            Self::NeedsSignIn => 1005,
            Self::BlockedByPolicy => 1006,
            Self::ReadOnly => 1007,
            Self::Offline => 1008,
            Self::UnsupportedVersion => 1009,
            Self::HelloRequired => 1010,
        }
    }

    /// The code's name in kebab-case, as used in `docs/protocol/README.md` and CLI output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ParseError => "parse-error",
            Self::InvalidRequest => "invalid-request",
            Self::MethodNotFound => "method-not-found",
            Self::InvalidParams => "invalid-params",
            Self::Internal => "internal",
            Self::NotReady => "not-ready",
            Self::UnknownPath => "unknown-path",
            Self::NotInAMount => "not-in-a-mount",
            Self::Refused => "refused",
            Self::NeedsSignIn => "needs-sign-in",
            Self::BlockedByPolicy => "blocked-by-policy",
            Self::ReadOnly => "read-only",
            Self::Offline => "offline",
            Self::UnsupportedVersion => "unsupported-version",
            Self::HelloRequired => "hello-required",
        }
    }

    /// The code with this integer value, if this build defines one.
    #[must_use]
    pub fn from_code(code: i64) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.code() == code)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The longest human-readable `message` the daemon sends, in bytes (ADR-0020 "Every message
/// is bounded").
pub const MAX_MESSAGE_BYTES: usize = 1024;

/// Truncates `message` to at most [`MAX_MESSAGE_BYTES`] bytes on a character boundary,
/// marking a truncation with `…`.
#[must_use]
pub fn cap_message(message: &str) -> String {
    truncate_to(message, MAX_MESSAGE_BYTES)
}

/// Serialises a human-readable message capped at [`MAX_MESSAGE_BYTES`].
pub(crate) fn serialize_capped<S: serde::Serializer>(
    message: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&cap_message(message))
}

/// Serialises an optional human-readable message capped at [`MAX_MESSAGE_BYTES`].
pub(crate) fn serialize_capped_opt<S: serde::Serializer>(
    message: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match message {
        Some(m) => serializer.serialize_some(&cap_message(m)),
        None => serializer.serialize_none(),
    }
}

/// Truncates `text` to at most `max` bytes on a character boundary, ending a truncated text
/// with `…` when there is room for it.
pub(crate) fn truncate_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    const ELLIPSIS: &str = "…";
    let room = if max >= ELLIPSIS.len() {
        max - ELLIPSIS.len()
    } else {
        max
    };
    let mut end = room;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    if max >= ELLIPSIS.len() {
        out.push_str(ELLIPSIS);
    }
    out
}

/// Structured detail for building `error.data`. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorData {
    /// A reason code: a [`RefusalReason`] for `refused`, a [`BlockedReason`] for
    /// `blocked-by-policy`, a [`ReadOnlyReason`] for `read-only` (ADR-0020).
    pub reason: Option<String>,
    /// For `refused` with reason `not-in-sync`: the item's sync state.
    pub sync: Option<WireSync>,
    /// For `unsupported-version`: the protocol major versions the daemon supports.
    pub supported_versions: Option<Vec<u32>>,
}

impl ErrorData {
    fn into_value(self) -> Option<Value> {
        let mut map = Map::new();
        if let Some(reason) = self.reason {
            map.insert("reason".into(), Value::String(reason));
        }
        if let Some(sync) = self.sync {
            map.insert("sync".into(), Value::String(sync.as_str().to_owned()));
        }
        if let Some(versions) = self.supported_versions {
            map.insert("supported_versions".into(), Value::from(versions));
        }
        (!map.is_empty()).then_some(Value::Object(map))
    }

    fn reason(reason: &str) -> Self {
        Self {
            reason: Some(reason.to_owned()),
            ..Self::default()
        }
    }
}

/// A JSON-RPC 2.0 error object: `{"code": ..., "message": ..., "data": ...}`.
///
/// `message` is for people and may change; programs act on `code` and `data` (ADR-0019 R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} (error {code})")]
pub struct RpcError {
    /// The integer code; see [`ErrorCode`].
    pub code: i64,
    /// A human-readable description that says what failed and why. Capped at
    /// [`MAX_MESSAGE_BYTES`] when built and again when serialised, so no message longer than
    /// that is ever sent.
    #[serde(serialize_with = "serialize_capped")]
    pub message: String,
    /// Structured detail as received. Read it with the accessors ([`RpcError::reason_code`]
    /// and the typed ones), which return `None` for a missing or unexpected shape instead of
    /// failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with a code and message and no data. The message is capped at
    /// [`MAX_MESSAGE_BYTES`].
    #[must_use]
    pub fn new(code: ErrorCode, message: impl AsRef<str>) -> Self {
        Self {
            code: code.code(),
            message: cap_message(message.as_ref()),
            data: None,
        }
    }

    /// Attaches structured detail. Empty detail is dropped so it is not sent as `{}`.
    #[must_use]
    pub fn with_data(mut self, data: ErrorData) -> Self {
        self.data = data.into_value();
        self
    }

    /// The known code, or `None` for a code this build does not define.
    #[must_use]
    pub fn kind(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
    }

    fn data_field<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value = self.data.as_ref()?.get(key)?;
        T::deserialize(value).ok()
    }

    /// `data.reason` as a raw code, if present and a string.
    #[must_use]
    pub fn reason_code(&self) -> Option<String> {
        self.data_field("reason")
    }

    /// `data.reason` of a `refused` error.
    #[must_use]
    pub fn refusal_reason(&self) -> Option<RefusalReason> {
        (self.kind() == Some(ErrorCode::Refused))
            .then(|| self.data_field("reason"))
            .flatten()
    }

    /// `data.reason` of a `blocked-by-policy` error.
    #[must_use]
    pub fn blocked_reason(&self) -> Option<BlockedReason> {
        (self.kind() == Some(ErrorCode::BlockedByPolicy))
            .then(|| self.data_field("reason"))
            .flatten()
    }

    /// `data.reason` of a `read-only` error.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<ReadOnlyReason> {
        (self.kind() == Some(ErrorCode::ReadOnly))
            .then(|| self.data_field("reason"))
            .flatten()
    }

    /// `data.reason` of an `invalid-params` error.
    #[must_use]
    pub fn invalid_params_reason(&self) -> Option<InvalidParamsReason> {
        (self.kind() == Some(ErrorCode::InvalidParams))
            .then(|| self.data_field("reason"))
            .flatten()
    }

    /// `invalid-params` for a cursor this daemon did not issue for these parameters, with
    /// reason `restart-listing`.
    #[must_use]
    pub fn restart_listing(method: &str, detail: impl fmt::Display) -> Self {
        Self::invalid_params(method, detail).with_data(ErrorData::reason(
            InvalidParamsReason::RestartListing.as_str(),
        ))
    }

    /// `data.sync`, if present and a string. Unknown sync values are kept (ADR-0020 R7).
    #[must_use]
    pub fn sync(&self) -> Option<WireSync> {
        self.data_field("sync")
    }

    /// `data.supported_versions`, if present and a list of integers.
    #[must_use]
    pub fn supported_versions(&self) -> Option<Vec<u32>> {
        self.data_field("supported_versions")
    }

    /// `method-not-found` for `method`.
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            ErrorCode::MethodNotFound,
            format!("method {method:?} is not part of this protocol version"),
        )
    }

    /// `invalid-params` for `method`, saying what was wrong.
    #[must_use]
    pub fn invalid_params(method: &str, detail: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::InvalidParams,
            format!("invalid params for {method}: {detail}"),
        )
    }

    /// `refused` with a reason.
    #[must_use]
    pub fn refused(reason: &RefusalReason, message: impl AsRef<str>) -> Self {
        Self::new(ErrorCode::Refused, message).with_data(ErrorData::reason(reason.as_str()))
    }

    /// `blocked-by-policy` with a reason (ADR-0020).
    #[must_use]
    pub fn blocked_by_policy(reason: &BlockedReason, message: impl AsRef<str>) -> Self {
        Self::new(ErrorCode::BlockedByPolicy, message).with_data(ErrorData::reason(reason.as_str()))
    }

    /// `read-only` with a reason (ADR-0020).
    #[must_use]
    pub fn read_only(reason: &ReadOnlyReason, message: impl AsRef<str>) -> Self {
        Self::new(ErrorCode::ReadOnly, message).with_data(ErrorData::reason(reason.as_str()))
    }

    /// `unsupported-version`, listing the versions the daemon supports (ADR-0009 R4).
    #[must_use]
    pub fn unsupported_version(requested: &[u32], supported: &[u32]) -> Self {
        Self::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "no protocol version in common: client offered {requested:?}, \
                 daemon supports {supported:?}"
            ),
        )
        .with_data(ErrorData {
            supported_versions: Some(supported.to_vec()),
            ..ErrorData::default()
        })
    }
}

impl From<DehydrateRefusal> for RpcError {
    fn from(refusal: DehydrateRefusal) -> Self {
        let message = refusal.to_string();
        match refusal {
            DehydrateRefusal::NotInSync(sync) => {
                Self::new(ErrorCode::Refused, message).with_data(ErrorData {
                    reason: Some(RefusalReason::NotInSync.as_str().to_owned()),
                    sync: Some(sync.into()),
                    ..ErrorData::default()
                })
            }
            DehydrateRefusal::Pinned => Self::refused(&RefusalReason::Pinned, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;
    use mydrive_core::state::Sync as SyncState;

    #[test]
    fn codes_and_names_are_unique_and_round_trip() {
        let codes: HashSet<i64> = ErrorCode::ALL.iter().map(|c| c.code()).collect();
        let names: HashSet<&str> = ErrorCode::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(codes.len(), ErrorCode::ALL.len());
        assert_eq!(names.len(), ErrorCode::ALL.len());
        for &c in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_code(c.code()), Some(c));
        }
        assert_eq!(ErrorCode::from_code(42), None);
    }

    /// The codes are protocol: changing one breaks every released client.
    #[test]
    fn codes_are_stable() {
        let table: Vec<(&str, i64)> = ErrorCode::ALL
            .iter()
            .map(|c| (c.name(), c.code()))
            .collect();
        assert_eq!(
            table,
            [
                ("parse-error", -32700),
                ("invalid-request", -32600),
                ("method-not-found", -32601),
                ("invalid-params", -32602),
                ("internal", -32603),
                ("not-ready", 1001),
                ("unknown-path", 1002),
                ("not-in-a-mount", 1003),
                ("refused", 1004),
                ("needs-sign-in", 1005),
                ("blocked-by-policy", 1006),
                ("read-only", 1007),
                ("offline", 1008),
                ("unsupported-version", 1009),
                ("hello-required", 1010),
            ]
        );
    }

    /// ADR-0019 R4: an unknown error code and an unknown reason parse and are preserved.
    #[test]
    fn unknown_codes_and_reasons_survive_parsing() {
        let input = json!({"code":1004,"message":"new","data":{"reason":"brand-new","extra":1}});
        let err: RpcError = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(
            err.refusal_reason(),
            Some(RefusalReason::Other("brand-new".into()))
        );
        assert_eq!(serde_json::to_value(&err).unwrap(), input);
        let input = json!({"code":1999,"message":"new"});
        let err: RpcError = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(err.kind(), None);
        assert_eq!(serde_json::to_value(&err).unwrap(), input);
    }

    /// An unexpected `data` shape does not fail the error; the accessors return `None`.
    #[test]
    fn unexpected_data_shapes_are_tolerated() {
        for data in [
            json!("text"),
            json!([1, 2]),
            json!({"reason": 5, "sync": 7, "supported_versions": "1"}),
        ] {
            let input = json!({"code": 1004, "message": "m", "data": data});
            let err: RpcError = serde_json::from_value(input.clone()).unwrap();
            assert_eq!(err.refusal_reason(), None);
            assert_eq!(err.reason_code(), None);
            assert_eq!(err.sync(), None);
            assert_eq!(err.supported_versions(), None);
            assert_eq!(serde_json::to_value(&err).unwrap(), input);
        }
    }

    /// ADR-0019 R6: a refusal is a kebab-case code in `data.reason`, separate from the message.
    #[test]
    fn dehydrate_refusal_maps_to_refused_with_sync_state() {
        let err = RpcError::from(DehydrateRefusal::NotInSync(SyncState::PendingUpload));
        assert_eq!(err.kind(), Some(ErrorCode::Refused));
        assert_eq!(
            err.data,
            Some(json!({"reason": "not-in-sync", "sync": "pending-upload"}))
        );
        assert_eq!(err.refusal_reason(), Some(RefusalReason::NotInSync));
        assert_eq!(err.sync(), Some(WireSync::PendingUpload));
        let err = RpcError::from(DehydrateRefusal::Pinned);
        assert_eq!(err.refusal_reason(), Some(RefusalReason::Pinned));
    }

    /// ADR-0019 R6 (as extended by ADR-0020): `blocked-by-policy` and `read-only` carry a
    /// reason code, read with the accessor for that code only.
    #[test]
    fn policy_and_read_only_errors_carry_reason_codes() {
        let err = RpcError::blocked_by_policy(&BlockedReason::OrganisationPolicy, "no");
        assert_eq!(err.code, 1006);
        assert_eq!(
            err.blocked_reason(),
            Some(BlockedReason::OrganisationPolicy)
        );
        assert_eq!(err.refusal_reason(), None);
        let err = RpcError::read_only(&ReadOnlyReason::LibraryReadOnly, "no");
        assert_eq!(err.code, 1007);
        assert_eq!(
            err.read_only_reason(),
            Some(ReadOnlyReason::LibraryReadOnly)
        );
        assert_eq!(err.data, Some(json!({"reason": "library-read-only"})));
    }

    /// ADR-0020 R4: messages are capped at 1 KiB on a character boundary.
    #[test]
    fn messages_are_capped_on_a_char_boundary() {
        assert_eq!(cap_message("short"), "short");
        let exact = "a".repeat(MAX_MESSAGE_BYTES);
        assert_eq!(cap_message(&exact), exact);
        for filler in ["a", "é", "€", "𝄞"] {
            let long = filler.repeat(MAX_MESSAGE_BYTES + 1);
            let capped = cap_message(&long);
            assert!(capped.len() <= MAX_MESSAGE_BYTES, "{filler}");
            assert!(capped.ends_with('…'));
            assert!(
                capped.len() > MAX_MESSAGE_BYTES - 8,
                "{filler}: {}",
                capped.len()
            );
        }
        let err = RpcError::new(ErrorCode::Internal, "x".repeat(5_000));
        assert!(err.message.len() <= MAX_MESSAGE_BYTES);
        // A message set directly is still capped on the wire.
        let mut err = err;
        err.message = "y".repeat(5_000);
        let v = serde_json::to_value(&err).unwrap();
        assert!(v["message"].as_str().unwrap().len() <= MAX_MESSAGE_BYTES);
        assert_eq!(truncate_to("abc", 0), "");
        assert_eq!(truncate_to("abcdef", 2), "ab");
    }

    #[test]
    fn empty_data_is_not_sent() {
        let err = RpcError::new(ErrorCode::Internal, "x").with_data(ErrorData::default());
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"code": -32603, "message": "x"})
        );
    }
}
