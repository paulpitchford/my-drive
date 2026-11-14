//! WP-005 AC1: every example in `docs/protocol/README.md` parses into the matching type and
//! serialises back to equivalent JSON. Also checks that the catalogue covers every method,
//! notification and error code (ADR-0009 R9).

use std::collections::BTreeSet;
use std::path::PathBuf;

use mydrive_ipc::envelope::{ErrorResponse, Message, Notification, Request, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountListResult, DaemonStatusResult, HelloParams, HelloResult, ItemActionResult,
    ItemPathsParams, ItemStatusParams, ItemStatusResult, MethodCall, MountListResult, NoParams,
    StatusChangedParams, StatusSubscribeParams, StatusSubscribeResult, method,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// One labelled JSON example from the catalogue.
#[derive(Debug)]
struct Example {
    label: String,
    line: usize,
    value: Value,
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue() -> Vec<Example> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/README.md");
    let text = std::fs::read_to_string(&path).expect("read docs/protocol/README.md");
    let mut examples = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((n, line)) = lines.next() {
        let Some(label) = line
            .trim()
            .strip_prefix("<!-- example:")
            .and_then(|rest| rest.strip_suffix("-->"))
        else {
            continue;
        };
        let (_, fence) = lines.next().expect("fence after example label");
        assert_eq!(
            fence.trim(),
            "```json",
            "line {}: example label not followed by ```json",
            n + 1
        );
        let mut body = String::new();
        for (_, l) in lines.by_ref() {
            if l.trim() == "```" {
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        let value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("line {}: example is not JSON: {e}", n + 1));
        examples.push(Example {
            label: label.trim().to_owned(),
            line: n + 1,
            value,
        });
    }
    examples
}

/// Decodes `value` as `T` and serialises it back.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn round_trip<T: DeserializeOwned + Serialize>(value: &Value, what: &str) -> Value {
    let typed: T = serde_json::from_value(value.clone()).unwrap_or_else(|e| {
        panic!(
            "{what}: does not parse as {}: {e}",
            std::any::type_name::<T>()
        )
    });
    serde_json::to_value(typed).expect("serialise")
}

/// Typed round trip of a request's params for `method_name`.
fn params_round_trip(method_name: &str, params: Option<&Value>, what: &str) -> Option<Value> {
    let params = params?;
    Some(match method_name {
        method::HELLO => round_trip::<HelloParams>(params, what),
        method::DAEMON_STATUS | method::ACCOUNT_LIST | method::MOUNT_LIST => {
            round_trip::<NoParams>(params, what)
        }
        method::ITEM_STATUS => round_trip::<ItemStatusParams>(params, what),
        method::ITEM_PIN | method::ITEM_UNPIN | method::ITEM_HYDRATE | method::ITEM_DEHYDRATE => {
            round_trip::<ItemPathsParams>(params, what)
        }
        method::STATUS_SUBSCRIBE => round_trip::<StatusSubscribeParams>(params, what),
        other => panic!("{what}: no params type for {other}"),
    })
}

/// Typed round trip of a result for `method_name`.
fn result_round_trip(method_name: &str, result: &Value, what: &str) -> Value {
    match method_name {
        method::HELLO => round_trip::<HelloResult>(result, what),
        method::DAEMON_STATUS => round_trip::<DaemonStatusResult>(result, what),
        method::ACCOUNT_LIST => round_trip::<AccountListResult>(result, what),
        method::MOUNT_LIST => round_trip::<MountListResult>(result, what),
        method::ITEM_STATUS => round_trip::<ItemStatusResult>(result, what),
        method::ITEM_PIN | method::ITEM_UNPIN | method::ITEM_HYDRATE | method::ITEM_DEHYDRATE => {
            round_trip::<ItemActionResult>(result, what)
        }
        method::STATUS_SUBSCRIBE => round_trip::<StatusSubscribeResult>(result, what),
        other => panic!("{what}: no result type for {other}"),
    }
}

/// Every error object (`code` + `message`) anywhere in `value`.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn collect_codes(value: &Value, codes: &mut BTreeSet<i64>) {
    match value {
        Value::Object(map) => {
            if let (Some(code), Some(Value::String(_))) = (map.get("code"), map.get("message")) {
                let error: RpcError = serde_json::from_value(value.clone()).expect("error object");
                assert_eq!(&serde_json::to_value(&error).unwrap(), value);
                codes.insert(code.as_i64().expect("integer code"));
            }
            map.values().for_each(|v| collect_codes(v, codes));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_codes(v, codes)),
        _ => {}
    }
}

#[test]
fn every_catalogue_example_round_trips() {
    let examples = catalogue();
    assert!(
        examples.len() > 20,
        "found only {} examples",
        examples.len()
    );

    let mut requested = BTreeSet::new();
    let mut answered = BTreeSet::new();
    let mut notified = BTreeSet::new();
    let mut codes = BTreeSet::new();
    let mut expected_error: Option<(usize, Request, RpcError)> = None;

    for example in &examples {
        let what = format!("example at line {} ({})", example.line, example.label);
        let message = Message::from_value(example.value.clone())
            .unwrap_or_else(|e| panic!("{what}: not a JSON-RPC message: {e}"));
        // The envelope itself serialises back unchanged.
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            example.value,
            "{what}"
        );
        collect_codes(&example.value, &mut codes);

        let mut words = example.label.split_whitespace();
        match (words.next(), words.next(), message) {
            (Some("request"), None, Message::Request(req)) => {
                let call = MethodCall::from_request(&req)
                    .unwrap_or_else(|e| panic!("{what}: request rejected: {e}"));
                assert_eq!(call.method(), req.method);
                let back = params_round_trip(&req.method, req.params.as_ref(), &what);
                assert_eq!(back.as_ref(), req.params.as_ref(), "{what}: params");
                requested.insert(req.method.clone());
            }
            (Some("request"), Some("rejected"), Message::Request(req)) => {
                let err = MethodCall::from_request(&req)
                    .err()
                    .unwrap_or_else(|| panic!("{what}: request was accepted"));
                expected_error = Some((example.line, req, err));
            }
            (Some("response"), Some(method_name), Message::Success(resp)) => {
                let back = result_round_trip(method_name, &resp.result, &what);
                assert_eq!(back, resp.result, "{what}: result");
                let _: SuccessResponse = serde_json::from_value(example.value.clone()).unwrap();
                answered.insert(method_name.to_owned());
            }
            (Some("error"), None, Message::Error(resp)) => {
                let _: ErrorResponse = serde_json::from_value(example.value.clone()).unwrap();
                if let Some((line, req, produced)) = expected_error.take() {
                    assert_eq!(
                        resp.id,
                        Some(req.id),
                        "{what}: id of request at line {line}"
                    );
                    assert_eq!(
                        resp.error, produced,
                        "{what}: error for request at line {line}"
                    );
                }
            }
            (Some("notification"), None, Message::Notification(note)) => {
                let _: Notification = serde_json::from_value(example.value.clone()).unwrap();
                assert_eq!(note.method, method::STATUS_CHANGED, "{what}");
                let params = note.params.as_ref().expect("params");
                assert_eq!(
                    &round_trip::<StatusChangedParams>(params, &what),
                    params,
                    "{what}"
                );
                notified.insert(note.method.clone());
            }
            (_, _, other) => panic!("{what}: label does not match message {other:?}"),
        }
        assert!(
            expected_error.is_none() || example.label.starts_with("request rejected"),
            "{what}: a rejected request must be followed directly by its error example"
        );
    }
    assert!(
        expected_error.is_none(),
        "last rejected request has no error example"
    );

    let all: BTreeSet<String> = method::ALL.iter().map(|m| (*m).to_owned()).collect();
    assert_eq!(requested, all, "every method has a request example");
    assert_eq!(answered, all, "every method has a response example");
    let notes: BTreeSet<String> = method::NOTIFICATIONS
        .iter()
        .map(|m| (*m).to_owned())
        .collect();
    assert_eq!(notified, notes, "every notification has an example");
    let all_codes: BTreeSet<i64> = ErrorCode::ALL.iter().map(|c| c.code()).collect();
    assert_eq!(codes, all_codes, "every error code has an example");
}

/// ADR-0009 R9: every method and every error code is documented in the catalogue's tables.
#[test]
fn catalogue_tables_name_every_method_and_code() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/README.md");
    let text = std::fs::read_to_string(path).expect("read docs/protocol/README.md");
    for m in method::ALL.iter().chain(method::NOTIFICATIONS) {
        assert!(
            text.contains(&format!("| [`{m}`]")),
            "{m} missing from the method tables"
        );
    }
    for c in ErrorCode::ALL {
        let row = format!("| {} | `{}` |", c.code(), c.name());
        assert!(text.contains(&row), "error table row missing: {row}");
    }
}
