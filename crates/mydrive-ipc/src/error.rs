//! The JSON-RPC error object and the protocol's error-code table.
//!
//! The integer codes are part of the protocol: once released they never change meaning
//! (ADR-0009 "Compatibility"). Codes from the JSON-RPC 2.0 specification keep their standard
//! values; the protocol's own codes are in the 1000s, outside the range JSON-RPC reserves
//! (-32768 to -32000).

use std::fmt;

use mydrive_core::state::{DehydrateRefusal, Sync as SyncState};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Every error code the protocol defines, with its stable integer value.
///
/// An [`RpcError`] keeps the raw integer, so a client can still handle an error whose code
/// was added in a later minor revision; [`RpcError::kind`] maps it back when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// JSON-RPC: the line was not valid JSON (or not valid UTF-8).
    ParseError,
    /// JSON-RPC: the JSON was not a valid request object, or the line was over the limit.
    InvalidRequest,
    /// JSON-RPC: no such method.
    MethodNotFound,
    /// JSON-RPC: the parameters are malformed, including a relative path (WP-005 AC6).
    InvalidParams,
    /// JSON-RPC: an internal daemon error.
    Internal,
    /// The daemon cannot serve the request yet: `hello` has not completed on this connection,
    /// or the daemon is still starting.
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
        }
    }

    /// The code with this integer value, if the protocol defines one.
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

/// Why a [`ErrorCode::Refused`] request was refused.
///
/// The wire form is a kebab-case string. A reason this build does not know is kept as
/// [`RefusalReason::Other`] rather than failing the whole response, so a newer daemon can add
/// reasons without breaking older clients.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// The item's sync state is not `in-sync`; `data.sync` names it. Dehydrating would risk
    /// content that is not on the server (ADR-0008 R5, R7).
    NotInSync,
    /// The item's effective pin is `pinned` (engine-level dehydrate, ADR-0008 R5).
    Pinned,
    /// The file has open handles (F06 step 9).
    InUse,
    /// The item changed on the server or is gone; delta must apply first (F06).
    RemoteChanged,
    /// The content does not fit on the backing filesystem (F06).
    InsufficientSpace,
    /// A reason this build does not know.
    Other(String),
}

impl RefusalReason {
    /// The wire form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::NotInSync => "not-in-sync",
            Self::Pinned => "pinned",
            Self::InUse => "in-use",
            Self::RemoteChanged => "remote-changed",
            Self::InsufficientSpace => "insufficient-space",
            Self::Other(other) => other,
        }
    }

    /// Parses a wire form; unknown strings become [`RefusalReason::Other`].
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        match value {
            "not-in-sync" => Self::NotInSync,
            "pinned" => Self::Pinned,
            "in-use" => Self::InUse,
            "remote-changed" => Self::RemoteChanged,
            "insufficient-space" => Self::InsufficientSpace,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for RefusalReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RefusalReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ReasonVisitor;

        impl Visitor<'_> for ReasonVisitor {
            type Value = RefusalReason;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a refusal reason string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<RefusalReason, E> {
                Ok(RefusalReason::from_wire(v))
            }
        }

        deserializer.deserialize_str(ReasonVisitor)
    }
}

/// Structured detail in `error.data`. Every field is optional; unknown fields are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorData {
    /// For `refused`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<RefusalReason>,
    /// For `refused` with reason `not-in-sync`: the item's sync state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncState>,
    /// For `unsupported-version`: the protocol major versions the daemon supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_versions: Option<Vec<u32>>,
}

impl ErrorData {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A JSON-RPC 2.0 error object: `{"code": ..., "message": ..., "data": ...}`.
///
/// `message` is for people and may change; programs act on `code` and `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} (error {code})")]
pub struct RpcError {
    /// The integer code; see [`ErrorCode`].
    pub code: i64,
    /// A human-readable description that says what failed and why.
    pub message: String,
    /// Structured detail, when the code defines any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

impl RpcError {
    /// An error with a code and message and no data.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    /// Attaches structured detail. Empty detail is dropped so it is not sent as `{}`.
    #[must_use]
    pub fn with_data(mut self, data: ErrorData) -> Self {
        self.data = if data.is_empty() { None } else { Some(data) };
        self
    }

    /// The known code, or `None` for a code this build does not define.
    #[must_use]
    pub fn kind(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
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
    pub fn refused(reason: RefusalReason, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Refused, message).with_data(ErrorData {
            reason: Some(reason),
            ..ErrorData::default()
        })
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
                    reason: Some(RefusalReason::NotInSync),
                    sync: Some(sync),
                    ..ErrorData::default()
                })
            }
            DehydrateRefusal::Pinned => Self::refused(RefusalReason::Pinned, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

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
            ]
        );
    }

    #[test]
    fn unknown_codes_and_reasons_survive_parsing() {
        let err: RpcError = serde_json::from_str(
            r#"{"code":1999,"message":"new","data":{"reason":"brand-new","extra":1}}"#,
        )
        .unwrap();
        assert_eq!(err.kind(), None);
        let data = err.data.unwrap();
        assert_eq!(data.reason, Some(RefusalReason::Other("brand-new".into())));
        assert_eq!(
            serde_json::to_value(&data).unwrap(),
            serde_json::json!({"reason": "brand-new"})
        );
    }

    #[test]
    fn dehydrate_refusal_maps_to_refused_with_sync_state() {
        let err = RpcError::from(DehydrateRefusal::NotInSync(SyncState::PendingUpload));
        assert_eq!(err.kind(), Some(ErrorCode::Refused));
        assert_eq!(
            serde_json::to_value(&err.data).unwrap(),
            serde_json::json!({"reason": "not-in-sync", "sync": "pending-upload"})
        );
        let err = RpcError::from(DehydrateRefusal::Pinned);
        assert_eq!(err.data.unwrap().reason, Some(RefusalReason::Pinned));
    }

    #[test]
    fn empty_data_is_not_sent() {
        let err = RpcError::new(ErrorCode::Internal, "x").with_data(ErrorData::default());
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            serde_json::json!({"code": -32603, "message": "x"})
        );
    }
}
