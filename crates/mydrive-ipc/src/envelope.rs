//! JSON-RPC 2.0 envelopes: request, notification, success response and error response.
//!
//! Parameters and results are carried as [`serde_json::Value`] here; the typed catalogue in
//! [`crate::methods`] converts them. Unknown fields at any level are ignored (ADR-0009 R5,
//! WP-005 AC5): no type in this crate uses `deny_unknown_fields`.

use std::fmt;

use serde::de::{self, DeserializeOwned, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::{ErrorCode, RpcError};

/// The `"jsonrpc": "2.0"` member. Serialises as `"2.0"` and accepts nothing else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct JsonRpcVersion;

/// The only JSON-RPC version string accepted.
pub const JSONRPC_VERSION: &str = "2.0";

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(JSONRPC_VERSION)
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct VersionVisitor;

        impl Visitor<'_> for VersionVisitor {
            type Value = JsonRpcVersion;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("the string \"2.0\"")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<JsonRpcVersion, E> {
                if v == JSONRPC_VERSION {
                    Ok(JsonRpcVersion)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
        }

        deserializer.deserialize_str(VersionVisitor)
    }
}

/// The longest string request ID accepted, in UTF-8 bytes. IDs are bounded so that every
/// response that echoes one stays within the line limit (ADR-0020 R4).
pub const MAX_ID_BYTES: usize = 128;

/// A request ID: an integer in `i64` range, or a string of at most [`MAX_ID_BYTES`] bytes,
/// chosen by the client and echoed in the response. Anything else is refused with
/// `invalid-request`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum Id {
    /// An integer ID.
    Number(i64),
    /// A string ID.
    String(String),
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "an integer in i64 range or a string of at most {MAX_ID_BYTES} bytes"
                )
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Id, E> {
                Ok(Id::Number(v))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Id, E> {
                i64::try_from(v)
                    .map(Id::Number)
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(v), &self))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Id, E> {
                if v.len() <= MAX_ID_BYTES {
                    Ok(Id::String(v.to_owned()))
                } else {
                    Err(E::invalid_length(v.len(), &self))
                }
            }
        }

        deserializer.deserialize_any(IdVisitor)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::String(s) => write!(f, "{s:?}"),
        }
    }
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Self::Number(n)
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

/// A request: a method call that expects a response with the same `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Echoed in the response.
    pub id: Id,
    /// The method name, for example `item.status`.
    pub method: String,
    /// Named parameters. Absent for methods that take none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A notification: a method call with no `id` and no response. The daemon sends
/// `status.changed` this way on a subscribed connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The notification method, for example `status.changed`.
    pub method: String,
    /// Named parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A successful response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuccessResponse {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The ID of the request this answers.
    pub id: Id,
    /// The method's result.
    pub result: Value,
}

/// An error response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The ID of the request this answers, or `null` when it could not be read (for example
    /// after a parse error). Always present on the wire, as JSON-RPC requires.
    pub id: Option<Id>,
    /// What went wrong.
    pub error: RpcError,
}

/// Any message that can arrive on a connection.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Message {
    /// A method call expecting a response.
    Request(Request),
    /// A method call with no response.
    Notification(Notification),
    /// A successful response.
    Success(SuccessResponse),
    /// An error response.
    Error(ErrorResponse),
}

/// A JSON value that is not a valid JSON-RPC 2.0 message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid JSON-RPC 2.0 message: {reason}")]
pub struct EnvelopeError {
    /// The request ID, when one could be read, so the error response can carry it.
    pub id: Option<Id>,
    /// What was wrong.
    pub reason: String,
}

impl EnvelopeError {
    /// The `invalid-request` error response to send back.
    #[must_use]
    pub fn to_response(&self) -> ErrorResponse {
        ErrorResponse::new(
            self.id.clone(),
            RpcError::new(ErrorCode::InvalidRequest, self.to_string()),
        )
    }
}

impl Request {
    /// Builds a request, serialising `params`.
    ///
    /// # Errors
    ///
    /// The `serde_json` error if `params` cannot be serialised.
    pub fn new<P: Serialize>(
        id: impl Into<Id>,
        method: impl Into<String>,
        params: Option<&P>,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            method: method.into(),
            params: params.map(serde_json::to_value).transpose()?,
        })
    }
}

impl Notification {
    /// Builds a notification, serialising `params`.
    ///
    /// # Errors
    ///
    /// The `serde_json` error if `params` cannot be serialised.
    pub fn new<P: Serialize>(
        method: impl Into<String>,
        params: &P,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params: Some(serde_json::to_value(params)?),
        })
    }
}

impl SuccessResponse {
    /// Builds a success response, serialising `result`.
    ///
    /// # Errors
    ///
    /// The `serde_json` error if `result` cannot be serialised.
    pub fn new<R: Serialize>(id: Id, result: &R) -> Result<Self, serde_json::Error> {
        Ok(Self {
            jsonrpc: JsonRpcVersion,
            id,
            result: serde_json::to_value(result)?,
        })
    }

    /// Decodes the result as `R`. Unknown result fields are ignored (ADR-0009 R5).
    ///
    /// # Errors
    ///
    /// The `serde_json` error if the result does not have the shape of `R`.
    pub fn result_as<R: DeserializeOwned>(&self) -> Result<R, serde_json::Error> {
        R::deserialize(&self.result)
    }
}

impl ErrorResponse {
    /// Builds an error response.
    #[must_use]
    pub fn new(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            error,
        }
    }
}

impl Message {
    /// Classifies and checks a parsed JSON value as a JSON-RPC 2.0 message.
    ///
    /// Objects with `method` are requests (with `id`) or notifications (without); objects
    /// with `result` or `error` are responses. Unknown members are ignored.
    ///
    /// # Errors
    ///
    /// [`EnvelopeError`] if the value is not a valid message; its `id` is set when the
    /// value carried a readable one.
    pub fn from_value(value: Value) -> Result<Self, EnvelopeError> {
        let Value::Object(object) = value else {
            return Err(EnvelopeError {
                id: None,
                reason: "a message must be a JSON object".into(),
            });
        };
        let id = object.get("id").and_then(|v| Id::deserialize(v).ok());
        let fail = |reason: String| EnvelopeError {
            id: id.clone(),
            reason,
        };
        match object.get("jsonrpc") {
            Some(Value::String(v)) if v == JSONRPC_VERSION => {}
            _ => return Err(fail("\"jsonrpc\" must be \"2.0\"".into())),
        }
        // A null `params` is treated as absent, like every other optional field.
        if let Some(params) = object.get("params")
            && !matches!(params, Value::Object(_) | Value::Array(_) | Value::Null)
        {
            return Err(fail("\"params\" must be an object or an array".into()));
        }
        let has = |key: &str| object.contains_key(key);
        let message = if has("method") {
            if has("id") {
                if id.is_none() {
                    return Err(fail(format!(
                        "\"id\" must be an integer in i64 range or a string of at most \
                         {MAX_ID_BYTES} bytes"
                    )));
                }
                decode(object, &fail).map(Self::Request)?
            } else {
                decode(object, &fail).map(Self::Notification)?
            }
        } else if has("result") {
            decode(object, &fail).map(Self::Success)?
        } else if has("error") {
            decode(object, &fail).map(Self::Error)?
        } else {
            return Err(fail(
                "a message needs \"method\", \"result\" or \"error\"".into(),
            ));
        };
        Ok(message)
    }
}

fn decode<T: DeserializeOwned>(
    object: Map<String, Value>,
    fail: &impl Fn(String) -> EnvelopeError,
) -> Result<T, EnvelopeError> {
    serde_json::from_value(Value::Object(object)).map_err(|e| fail(e.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(v: Value) -> Result<Message, EnvelopeError> {
        Message::from_value(v)
    }

    #[test]
    fn classifies_each_kind() {
        let req = parse(json!({"jsonrpc":"2.0","id":1,"method":"daemon.status"})).unwrap();
        assert!(matches!(req, Message::Request(ref r) if r.id == Id::Number(1)));
        let note = parse(json!({"jsonrpc":"2.0","method":"status.changed","params":{}})).unwrap();
        assert!(matches!(note, Message::Notification(_)));
        let ok = parse(json!({"jsonrpc":"2.0","id":"a","result":{}})).unwrap();
        assert!(matches!(ok, Message::Success(ref r) if r.id == Id::String("a".into())));
        let err = parse(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"x"}}))
            .unwrap();
        assert!(matches!(err, Message::Error(ref e) if e.id.is_none()));
    }

    /// WP-005 AC5: unknown fields in incoming messages are ignored.
    #[test]
    fn unknown_envelope_fields_are_ignored() {
        let v = json!({"jsonrpc":"2.0","id":7,"method":"mount.list","trace":"x","extra":{"a":1}});
        let Message::Request(req) = parse(v).unwrap() else {
            panic!("not a request");
        };
        assert_eq!(req.method, "mount.list");
        let v = json!({"jsonrpc":"2.0","id":7,"result":{"mounts":[]},"server_time":3});
        assert!(matches!(parse(v).unwrap(), Message::Success(_)));
    }

    /// ADR-0009 R2: every message is valid JSON-RPC 2.0; anything else is refused with the
    /// request's ID where it can be read.
    #[test]
    fn invalid_envelopes_are_refused() {
        let cases = [
            json!([1, 2]),
            json!({"id":1,"method":"hello"}),
            json!({"jsonrpc":"1.0","id":1,"method":"hello"}),
            json!({"jsonrpc":"2.0","id":1.5,"method":"hello"}),
            json!({"jsonrpc":"2.0","id":null,"method":"hello"}),
            json!({"jsonrpc":"2.0","id":1,"method":3}),
            json!({"jsonrpc":"2.0","id":1,"method":"hello","params":"x"}),
            json!({"jsonrpc":"2.0","id":1}),
        ];
        for case in cases {
            let err = parse(case.clone()).unwrap_err();
            let resp = err.to_response();
            assert_eq!(resp.error.kind(), Some(ErrorCode::InvalidRequest), "{case}");
        }
        let err = parse(json!({"jsonrpc":"1.0","id":9,"method":"hello"})).unwrap_err();
        assert_eq!(err.id, Some(Id::Number(9)));
    }

    /// ADR-0020 R4: request IDs are bounded, so no response echoing one can exceed the line.
    #[test]
    fn ids_are_bounded() {
        let longest = "é".repeat(MAX_ID_BYTES / 2);
        for ok in [json!(i64::MIN), json!(i64::MAX), json!(longest), json!("")] {
            let msg = json!({"jsonrpc":"2.0","id":ok,"method":"daemon.status"});
            assert!(parse(msg).is_ok(), "{ok}");
        }
        for bad in [
            json!(u64::MAX),
            json!(1e3),
            json!("x".repeat(MAX_ID_BYTES + 1)),
            json!([1]),
            json!({"a": 1}),
            json!(true),
        ] {
            let msg = json!({"jsonrpc":"2.0","id":bad,"method":"daemon.status"});
            let err = parse(msg).unwrap_err();
            assert_eq!(err.id, None, "{bad}");
            assert_eq!(
                err.to_response().error.kind(),
                Some(ErrorCode::InvalidRequest)
            );
            let resp = json!({"jsonrpc":"2.0","id":bad,"result":{}});
            assert!(parse(resp).is_err(), "response with id {bad}");
        }
        // A null `params` is treated as absent.
        let Message::Request(req) =
            parse(json!({"jsonrpc":"2.0","id":1,"method":"mount.list","params":null})).unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(req.params, None);
    }

    #[test]
    fn error_response_always_carries_id() {
        let resp = ErrorResponse::new(None, RpcError::new(ErrorCode::ParseError, "bad"));
        assert_eq!(
            serde_json::to_value(&resp).unwrap(),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}})
        );
    }

    #[test]
    fn messages_serialise_back_to_their_input() {
        let inputs = [
            json!({"jsonrpc":"2.0","id":1,"method":"daemon.status"}),
            json!({"jsonrpc":"2.0","id":"x","method":"item.pin","params":{"paths":["/a"]}}),
            json!({"jsonrpc":"2.0","method":"status.changed","params":{"entries":[]}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"mounts":[]}}),
            json!({"jsonrpc":"2.0","id":2,"error":{"code":1004,"message":"no","data":{"reason":"pinned"}}}),
        ];
        for input in inputs {
            let msg = parse(input.clone()).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), input);
        }
    }
}
