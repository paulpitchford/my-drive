//! WP-005 AC1: every example in `docs/protocol/README.md` parses into the matching type and
//! serialises back to equivalent JSON. Also checks that the catalogue and the code agree on
//! the methods, error codes and value sets (ADR-0009 R9, ADR-0019 R7).

use std::collections::BTreeSet;
use std::path::PathBuf;

use mydrive_core::state::{
    DisplayStatus, EffectivePin, Hydration, Pin, Sync as SyncState, display_status, effective_pin,
};
use mydrive_ipc::envelope::{ErrorResponse, Message, Notification, Request, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountInfo, AccountListResult, DaemonStatusResult, HelloParams, HelloResult, ItemActionResult,
    ItemInfo, ItemPathsParams, ItemStatusParams, ItemStatusResult, MethodCall, MountInfo,
    MountListResult, NoParams, StatusChangedParams, StatusSubscribeParams, StatusSubscribeResult,
    method,
};
use mydrive_ipc::values::{
    AccountKind, AccountState, BlockedReason, ItemReason, Modifier, MountState, RefusalReason,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// One labelled JSON example from the catalogue.
#[derive(Debug)]
struct Example {
    label: String,
    line: usize,
    value: Value,
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol/README.md");
    std::fs::read_to_string(&path).expect("read docs/protocol/README.md")
}

/// The labelled examples. Fails if any ```json block is not directly preceded by a label.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue() -> Vec<Example> {
    let text = catalogue_text();
    let lines: Vec<&str> = text.lines().collect();
    let mut examples = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("```json") {
            panic!("line {}: ```json block without an example label", i + 1);
        }
        let Some(label) = line
            .strip_prefix("<!-- example:")
            .and_then(|rest| rest.strip_suffix("-->"))
        else {
            i += 1;
            continue;
        };
        assert_eq!(
            lines.get(i + 1).map(|l| l.trim()),
            Some("```json"),
            "line {}: example label not followed by ```json",
            i + 1
        );
        let mut body = String::new();
        let mut j = i + 2;
        while j < lines.len() && lines[j].trim() != "```" {
            body.push_str(lines[j]);
            body.push('\n');
            j += 1;
        }
        let value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("line {}: example is not JSON: {e}", i + 1));
        examples.push(Example {
            label: label.trim().to_owned(),
            line: i + 1,
            value,
        });
        i = j + 1;
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
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
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
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
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

/// Walks `value`, collecting error codes and checking every item's display status.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn inspect(value: &Value, codes: &mut BTreeSet<i64>, items: &mut usize, what: &str) {
    match value {
        Value::Object(map) => {
            if let (Some(code), Some(Value::String(_))) = (map.get("code"), map.get("message")) {
                let error: RpcError = serde_json::from_value(value.clone()).expect("error object");
                assert_eq!(&serde_json::to_value(&error).unwrap(), value, "{what}");
                codes.insert(code.as_i64().expect("integer code"));
            }
            if map.contains_key("effective_pin") {
                let info: ItemInfo = serde_json::from_value(value.clone())
                    .unwrap_or_else(|e| panic!("{what}: item info: {e}"));
                assert_status_consistent(&info, what);
                *items += 1;
            }
            map.values().for_each(|v| inspect(v, codes, items, what));
        }
        Value::Array(list) => list.iter().for_each(|v| inspect(v, codes, items, what)),
        _ => {}
    }
}

/// The example's display status and effective pin are what `mydrive-core` derives from its
/// axes.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_status_consistent(info: &ItemInfo, what: &str) {
    let inherited = match info.effective_pin {
        EffectivePin::Pinned => Pin::Pinned,
        EffectivePin::Unpinned => Pin::Unpinned,
    };
    let effective = effective_pin(info.pin, [inherited]);
    assert_eq!(
        info.effective_pin, effective,
        "{what}: effective_pin contradicts pin"
    );
    assert_eq!(
        info.status,
        display_status(&info.state(), effective),
        "{what}: status is not the one derived from the axes"
    );
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
    let mut items = 0;
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
        inspect(&example.value, &mut codes, &mut items, &what);

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
                continue;
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
            expected_error.is_none(),
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
    assert!(items >= 8, "only {items} item states in the examples");
}

/// ADR-0009 R9: the method tables and the error table name exactly what the code has.
#[test]
fn catalogue_tables_match_the_code() {
    let text = catalogue_text();
    let documented: BTreeSet<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("| [`"))
        .filter_map(|l| l.split('`').next())
        .collect();
    let code: BTreeSet<&str> = method::ALL
        .iter()
        .chain(method::NOTIFICATIONS)
        .copied()
        .collect();
    assert_eq!(documented, code, "method tables differ from the code");

    let rows: BTreeSet<(i64, String)> = text
        .lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            let code = cells.get(1)?.parse::<i64>().ok()?;
            Some((code, cells.get(2)?.trim_matches('`').to_owned()))
        })
        .collect();
    let table: BTreeSet<(i64, String)> = ErrorCode::ALL
        .iter()
        .map(|c| (c.code(), c.name().to_owned()))
        .collect();
    assert_eq!(rows, table, "error table differs from the code");
}

const UNKNOWN: &str = "zz-unknown-value";

/// Sets `field` of `base` to the unknown value and reports whether the containing type still
/// parses and keeps the value (open), or fails (closed).
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn probe<T: DeserializeOwned + Serialize>(mut base: Value, field: &str, value: Value) -> bool {
    base[field] = value.clone();
    match serde_json::from_value::<T>(base) {
        Ok(parsed) => {
            let back = serde_json::to_value(parsed).unwrap();
            assert_eq!(
                back[field], value,
                "open value of {field} was not preserved"
            );
            true
        }
        Err(_) => false,
    }
}

/// Whether the type behind a catalogue value-set row accepts and preserves an unknown value.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn accepts_unknown(field: &str) -> bool {
    let item = json!({
        "status": "local", "modifiers": [], "hydration": "full", "pin": "inherit",
        "effective_pin": "unpinned", "sync": "in-sync"
    });
    let mount = json!({
        "mount_id": "m", "account_id": "a", "drive_id": "d", "mount_point": "/m",
        "state": "active", "read_only": false
    });
    let account = json!({"account_id": "a", "kind": "personal", "state": "signed-in"});
    let unknown = Value::String(UNKNOWN.into());
    match field {
        "item.status" | "item.hydration" | "item.pin" | "item.effective_pin" | "item.sync"
        | "item.reason" => {
            let key = field.trim_start_matches("item.");
            probe::<ItemInfo>(item, key, unknown)
        }
        "item.modifiers" => probe::<ItemInfo>(item, "modifiers", json!(["read-only", UNKNOWN])),
        "mount.state" | "mount.blocked_reason" => {
            probe::<MountInfo>(mount, field.trim_start_matches("mount."), unknown)
        }
        "account.state" | "account.kind" => {
            probe::<AccountInfo>(account, field.trim_start_matches("account."), unknown)
        }
        "error.code" => {
            let err: RpcError =
                serde_json::from_value(json!({"code": 4242, "message": "m"})).unwrap();
            err.kind().is_none() && err.code == 4242
        }
        "error.data.reason" => {
            let err: RpcError = serde_json::from_value(
                json!({"code": 1004, "message": "m", "data": {"reason": UNKNOWN}}),
            )
            .unwrap();
            err.reason() == Some(RefusalReason::Other(UNKNOWN.into()))
        }
        "error.data.sync" => {
            let err: RpcError = serde_json::from_value(
                json!({"code": 1004, "message": "m", "data": {"sync": UNKNOWN}}),
            )
            .unwrap();
            err.sync().is_some()
        }
        "hello.capabilities" => probe::<HelloResult>(
            json!({"protocol_version": 1, "capabilities": []}),
            "capabilities",
            json!([UNKNOWN]),
        ),
        other => panic!("value-set row {other:?} has no probe; add one"),
    }
}

/// ADR-0019 R7 (and R4): every enumerated field is marked closed or open in the catalogue,
/// and the marking matches the types: open types accept and keep an unknown value, closed
/// types reject it.
#[test]
fn value_set_marking_matches_the_types() {
    let text = catalogue_text();
    let section = text
        .split("## Value sets")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("a Value sets section");
    let mut marked = BTreeSet::new();
    for line in section.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let (Some(field), Some(set)) = (cells.get(1), cells.get(2)) else {
            continue;
        };
        let Some(field) = field.strip_prefix('`').and_then(|f| f.strip_suffix('`')) else {
            continue;
        };
        let open = match *set {
            "open" => true,
            "closed" => false,
            other => panic!("{field}: set must be open or closed, not {other:?}"),
        };
        if open {
            let fallback = cells.get(4).copied().unwrap_or("");
            assert!(
                !fallback.is_empty() && fallback != "—",
                "{field}: open set without a client fallback"
            );
        }
        assert_eq!(
            accepts_unknown(field),
            open,
            "{field} is marked {set} but its type disagrees"
        );
        marked.insert(field.to_owned());
    }
    let expected: BTreeSet<String> = [
        "item.status",
        "item.hydration",
        "item.pin",
        "item.effective_pin",
        "item.sync",
        "item.modifiers",
        "item.reason",
        "mount.state",
        "mount.blocked_reason",
        "account.state",
        "account.kind",
        "error.code",
        "error.data.reason",
        "error.data.sync",
        "hello.capabilities",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(marked, expected, "every enumerated field is marked once");

    // The closed sets' listed values are exactly the core wire forms.
    let values_of = |field: &str| -> BTreeSet<String> {
        section
            .lines()
            .find(|l| l.starts_with(&format!("| `{field}` |")))
            .and_then(|l| l.split('|').nth(3))
            .map(|cell| {
                cell.split(',')
                    .map(|v| v.trim().trim_matches('`').to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let wire =
        |forms: &[&str]| -> BTreeSet<String> { forms.iter().map(|s| (*s).to_owned()).collect() };
    assert_eq!(values_of("item.status"), wire(DisplayStatus::WIRE_FORMS));
    assert_eq!(values_of("item.hydration"), wire(Hydration::WIRE_FORMS));
    assert_eq!(values_of("item.pin"), wire(Pin::WIRE_FORMS));
    assert_eq!(
        values_of("item.effective_pin"),
        wire(EffectivePin::WIRE_FORMS)
    );
    assert_eq!(values_of("item.sync"), wire(SyncState::WIRE_FORMS));
    // The open sets' listed values are exactly the ones this build knows.
    assert_eq!(values_of("item.modifiers"), wire(Modifier::KNOWN));
    assert_eq!(values_of("item.reason"), wire(ItemReason::KNOWN));
    assert_eq!(values_of("mount.state"), wire(MountState::KNOWN));
    assert_eq!(
        values_of("mount.blocked_reason"),
        wire(BlockedReason::KNOWN)
    );
    assert_eq!(values_of("account.state"), wire(AccountState::KNOWN));
    assert_eq!(values_of("account.kind"), wire(AccountKind::KNOWN));
    assert_eq!(values_of("error.data.reason"), wire(RefusalReason::KNOWN));
}
