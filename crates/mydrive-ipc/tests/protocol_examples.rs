//! WP-005 AC1: every example in `docs/protocol/README.md` parses into the matching type and
//! serialises back to equivalent JSON. Also checks that the catalogue and the code agree on
//! the methods, error codes and value sets (ADR-0009 R9, ADR-0019 R7, ADR-0020 R7, R8).

use std::collections::BTreeSet;
use std::path::PathBuf;

use mydrive_core::state::{EffectivePin, Pin, display_status, effective_pin};
use mydrive_ipc::envelope::{ErrorResponse, Message, Notification, Request, SuccessResponse};
use mydrive_ipc::error::{ErrorCode, RpcError};
use mydrive_ipc::methods::{
    AccountInfo, AccountListResult, DaemonStatusResult, HelloParams, HelloResult, ItemActionResult,
    ItemInfo, ItemPathsParams, ItemStatusParams, ItemStatusResult, ListParams, MethodCall,
    MountInfo, MountListResult, NoParams, StatusChangedParams, StatusSubscribeParams,
    StatusSubscribeResult, method,
};
use mydrive_ipc::values::{
    BlockedReason, InvalidParamsReason, ReadOnlyReason, RefusalReason, VALUE_SETS,
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
fn crate_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue_text() -> String {
    crate_file("../../docs/protocol/README.md")
}

/// Whether a line opens a fenced block tagged as JSON, in any fence style or case.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn is_json_fence(line: &str) -> bool {
    let t = line.trim().to_ascii_lowercase();
    ["```", "~~~"].iter().any(|fence| {
        t.strip_prefix(fence)
            .is_some_and(|rest| rest.trim().starts_with("json"))
    })
}

/// The fence marker and language tag of a line that opens a fenced block, if it does.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn fence_opener(line: &str) -> Option<(&'static str, String)> {
    let t = line.trim();
    ["```", "~~~"].into_iter().find_map(|fence| {
        t.strip_prefix(fence)
            .map(|tag| (fence, tag.trim().to_owned()))
    })
}

/// The labelled examples. Fails if any JSON block is not directly preceded by a label, if a
/// label is not followed by exactly "```json", or if a fenced block with no language tag
/// holds a JSON object or array (an example that would escape the checks).
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn parse_examples(text: &str) -> Vec<Example> {
    let lines: Vec<&str> = text.lines().collect();
    let mut examples = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        assert!(
            !is_json_fence(line),
            "line {}: JSON block without an example label",
            i + 1
        );
        // Any other fenced block: skip its body, so its lines are never read as labels or
        // fences, but refuse an untagged block whose content is JSON.
        if let Some((fence, tag)) = fence_opener(line) {
            let mut body = String::new();
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim() != fence {
                body.push_str(lines[j]);
                body.push('\n');
                j += 1;
            }
            if tag.is_empty() {
                let parsed = serde_json::from_str::<Value>(&body);
                assert!(
                    !matches!(parsed, Ok(Value::Object(_) | Value::Array(_))),
                    "line {}: untagged fenced block holds JSON; label it as an example",
                    i + 1
                );
            }
            i = j + 1;
            continue;
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

/// The method names in the Methods section's tables. Every table row there must link.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn method_table_names(text: &str) -> BTreeSet<String> {
    let section = text
        .split("\n## Methods")
        .nth(1)
        .and_then(|s| s.split("\n### ").next())
        .expect("a Methods section");
    let mut names = BTreeSet::new();
    for line in section.lines().filter(|l| l.starts_with('|')) {
        let first = line.split('|').nth(1).unwrap_or("").trim();
        if first == "Method" || first == "Notification" || first.starts_with("---") {
            continue;
        }
        let name = first
            .strip_prefix("[`")
            .and_then(|rest| rest.split_once("`]("))
            .map(|(name, _)| name)
            .unwrap_or_else(|| panic!("method table row without a link: {line}"));
        names.insert(name.to_owned());
    }
    names
}

#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn catalogue() -> Vec<Example> {
    parse_examples(&catalogue_text())
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
        method::DAEMON_STATUS => round_trip::<NoParams>(params, what),
        method::ACCOUNT_LIST | method::MOUNT_LIST => round_trip::<ListParams>(params, what),
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

/// The typed reading of an example: what the Rust types make of it, re-serialised.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn typed_reading(label: &str, value: &Value, what: &str) -> Value {
    let message = Message::from_value(value.clone())
        .unwrap_or_else(|e| panic!("{what}: not a JSON-RPC message: {e}"));
    let mut words = label.split_whitespace();
    match (words.next(), words.next(), message) {
        (Some("request"), None, Message::Request(req)) => {
            let call = MethodCall::from_request(&req)
                .unwrap_or_else(|e| panic!("{what}: request rejected: {e}"));
            json!({
                "id": req.id,
                "method": call.method(),
                "params": params_round_trip(&req.method, req.params.as_ref(), what),
            })
        }
        (Some("request"), Some("rejected"), Message::Request(req)) => {
            let err = MethodCall::from_request(&req)
                .err()
                .unwrap_or_else(|| panic!("{what}: request was accepted"));
            json!({"id": req.id, "rejected": err.code})
        }
        (Some("response"), Some(method_name), Message::Success(resp)) => {
            json!({"id": resp.id, "result": result_round_trip(method_name, &resp.result, what)})
        }
        (Some("error"), None, Message::Error(resp)) => serde_json::to_value(resp).unwrap(),
        (Some("notification"), None, Message::Notification(note)) => {
            let params = note.params.unwrap_or(Value::Null);
            json!({
                "method": note.method,
                "params": round_trip::<StatusChangedParams>(&params, what),
            })
        }
        (_, _, other) => panic!("{what}: label does not match message {other:?}"),
    }
}

/// Adds an unknown field to every object in `value`, except inside error `data`, which is
/// kept as raw JSON by design.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn inject_unknown_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key != "data" {
                    inject_unknown_fields(child);
                }
            }
            map.insert("zz_unknown_field".into(), json!({"nested": [1, "x"]}));
        }
        Value::Array(items) => items.iter_mut().for_each(inject_unknown_fields),
        _ => {}
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
                check_reason_codes(&error, what);
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

/// ADR-0019 R6 and ADR-0020: refusals, policy blocks and read-only errors carry a known
/// reason code in every example.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn check_reason_codes(error: &RpcError, what: &str) {
    let known =
        |reason: Option<String>, set: &[&str]| reason.is_some_and(|r| set.contains(&r.as_str()));
    match error.kind() {
        Some(ErrorCode::Refused) => {
            assert!(known(error.reason_code(), RefusalReason::KNOWN), "{what}");
        }
        Some(ErrorCode::BlockedByPolicy) => {
            assert!(known(error.reason_code(), BlockedReason::KNOWN), "{what}");
        }
        Some(ErrorCode::ReadOnly) => {
            assert!(known(error.reason_code(), ReadOnlyReason::KNOWN), "{what}");
        }
        Some(ErrorCode::InvalidParams) => {
            if let Some(reason) = error.reason_code() {
                assert!(
                    InvalidParamsReason::KNOWN.contains(&reason.as_str()),
                    "{what}"
                );
            }
        }
        _ => {}
    }
}

/// The example's display status and effective pin are what `mydrive-core` derives from its
/// axes.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn assert_status_consistent(info: &ItemInfo, what: &str) {
    let state = info
        .state()
        .unwrap_or_else(|| panic!("{what}: examples use known axis values"));
    let inherited = match info.effective_pin.to_core() {
        Some(EffectivePin::Pinned) => Pin::Pinned,
        Some(EffectivePin::Unpinned) => Pin::Unpinned,
        None => panic!("{what}: examples use known effective pins"),
    };
    let effective = effective_pin(state.pin, [inherited]);
    assert_eq!(
        info.effective_pin,
        effective.into(),
        "{what}: effective_pin contradicts pin"
    );
    assert_eq!(
        info.status,
        display_status(&state, effective),
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
    let mut top_level = BTreeSet::new();
    let mut per_path = BTreeSet::new();
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
        // Error objects at the top of an error response are top-level; every other error
        // object in an example (in an entry, an action result or a skipped item) is per path.
        if let Message::Error(resp) = &message {
            top_level.insert(resp.error.code);
            codes.insert(resp.error.code);
            check_reason_codes(&resp.error, &what);
        } else {
            let mut nested = BTreeSet::new();
            inspect(&example.value, &mut nested, &mut items, &what);
            codes.extend(&nested);
            per_path.extend(nested);
        }

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

    // Each code appears only where the error table's "Where" column says it can.
    let places = error_table_places(&catalogue_text());
    for code in &top_level {
        let place = places.get(code).map(String::as_str);
        assert!(
            matches!(place, Some("top level" | "both")),
            "code {code} appears at the top level, but the table says {place:?}"
        );
    }
    for code in &per_path {
        let place = places.get(code).map(String::as_str);
        assert!(
            matches!(place, Some("per path" | "both")),
            "code {code} appears per path, but the table says {place:?}"
        );
    }
}

/// The error table's "Where" column, by code.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn error_table_places(text: &str) -> std::collections::BTreeMap<i64, String> {
    text.lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            let code = cells.get(1)?.parse::<i64>().ok()?;
            Some((code, (*cells.get(5)?).to_owned()))
        })
        .collect()
}

/// Every error code says where it can appear.
#[test]
fn error_table_says_where_each_code_appears() {
    let places = error_table_places(&catalogue_text());
    for code in ErrorCode::ALL {
        let place = places.get(&code.code()).map(String::as_str);
        assert!(
            matches!(place, Some("top level" | "per path" | "both")),
            "{}: Where is {place:?}",
            code.name()
        );
    }
}

/// ADR-0009 R5, WP-005 AC5: unknown fields are ignored by every wire type. Each example is
/// re-read with an unknown field added to every object at every level; the types must read
/// exactly what they read without it.
#[test]
fn unknown_fields_at_every_level_are_ignored() {
    for example in catalogue() {
        let what = format!("example at line {} ({})", example.line, example.label);
        let plain = typed_reading(&example.label, &example.value, &what);
        let mut injected = example.value.clone();
        inject_unknown_fields(&mut injected);
        assert_ne!(injected, example.value);
        let with_unknown = typed_reading(&example.label, &injected, &format!("{what}, injected"));
        assert_eq!(
            with_unknown, plain,
            "{what}: an unknown field changed the reading"
        );
    }
}

/// ADR-0009 R9: the method tables and the error table name exactly what the code has.
#[test]
fn catalogue_tables_match_the_code() {
    let text = catalogue_text();
    let documented = method_table_names(&text);
    let code: BTreeSet<String> = method::ALL
        .iter()
        .chain(method::NOTIFICATIONS)
        .map(|m| (*m).to_owned())
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

/// The catalogue parser itself rejects the shapes that would let an example escape the test.
#[test]
fn catalogue_parser_rejects_unchecked_blocks_and_rows() {
    for fence in ["```json", "~~~json", "```JSON", "~~~ JSON", "  ```Json  "] {
        let text = format!("intro\n{fence}\n{{}}\n```\n");
        let result = std::panic::catch_unwind(|| parse_examples(&text));
        assert!(result.is_err(), "unlabelled {fence:?} block was accepted");
    }
    let labelled = "<!-- example: error -->\n~~~json\n{}\n~~~\n";
    assert!(std::panic::catch_unwind(|| parse_examples(labelled)).is_err());
    let ok = "<!-- example: error -->\n```json\n{}\n```\n";
    assert_eq!(parse_examples(ok).len(), 1);

    // An untagged fence holding JSON escapes the checks, so it is refused.
    for json in [
        "{\"jsonrpc\": \"2.0\", \"id\": 1, \"result\": {}}",
        "[1, 2]",
        "{\n  \"a\": 1\n}",
    ] {
        for fence in ["```", "~~~"] {
            let text = format!("intro\n{fence}\n{json}\n{fence}\n");
            let result = std::panic::catch_unwind(|| parse_examples(&text));
            assert!(
                result.is_err(),
                "untagged {fence} block with {json:?} was accepted"
            );
        }
    }
    // Untagged blocks that are not JSON, and tagged blocks in other languages, are fine; a
    // label or JSON fence inside them is not read as one.
    let other = "```\nplain text\n```\n```sh\n{ echo; }\n<!-- example: error -->\n```\n\
                 ```python\nprint([1])\n```\n";
    assert!(parse_examples(other).is_empty());

    let unlinked = "\n## Methods\n\n| Method | Does |\n|---|---|\n| `hello` | x |\n\n### hello\n";
    assert!(std::panic::catch_unwind(|| method_table_names(unlinked)).is_err());
    let linked =
        "\n## Methods\n\n| Method | Does |\n|---|---|\n| [`hello`](#hello) | x |\n\n### x\n";
    assert_eq!(
        method_table_names(linked),
        BTreeSet::from(["hello".to_owned()])
    );
}

/// ADR-0020 R8: the catalogue says `refresh` covers each listed path and everything below.
#[test]
fn refresh_is_documented_as_covering_the_subtree() {
    let text = catalogue_text();
    let section = text
        .split("\n### status.changed")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("a status.changed section");
    assert!(
        section.contains("that path and everything below it"),
        "status.changed must say refresh covers the whole subtree"
    );
}

/// The text of the catalogue's bullet that starts with `start`, up to the next bullet.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn bullet(text: &str, start: &str) -> String {
    let from = text
        .find(&format!("- {start}"))
        .unwrap_or_else(|| panic!("no bullet starting {start:?}"));
    let rest = &text[from + 2..];
    let end = rest.find("\n- ").unwrap_or(rest.len());
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes one raw line the way the daemon does: framing, then the envelope.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn decode_raw(line: &str) -> Result<Result<Message, ErrorResponse>, String> {
    let mut decoder = mydrive_ipc::codec::LineDecoder::new();
    decoder.push(line.as_bytes());
    decoder.push(b"\n");
    let value = decoder
        .next_message()
        .expect("one line")
        .map_err(|e| e.to_string())?;
    Ok(Message::from_value(value).map_err(|e| e.to_response()))
}

/// The catalogue lists the refused numeric forms of a request `id`, and each one it lists is
/// refused as documented: `invalid-request` with `"id": null`, or a parse error for forms
/// that are not JSON numbers.
#[test]
fn documented_refused_id_forms_are_refused() {
    let ids = bullet(&catalogue_text(), "**Request IDs**");
    let refused = [
        "1.5",
        "1.0",
        "0.0",
        "1e3",
        "1E0",
        "-0",
        "9223372036854775808",
        "-9223372036854775809",
    ];
    let not_json = ["01", "+1", "0x1", "NaN"];
    for form in refused.iter().chain(&not_json) {
        assert!(
            ids.contains(&format!("`{form}`")),
            "the Request IDs bullet does not list `{form}`"
        );
    }
    let line = |id: &str| format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"daemon.status"}}"#);
    for form in refused {
        let response = decode_raw(&line(form))
            .unwrap_or_else(|e| panic!("{form}: not even JSON: {e}"))
            .expect_err(form);
        assert_eq!(response.id, None, "{form}");
        assert_eq!(
            response.error.kind(),
            Some(ErrorCode::InvalidRequest),
            "{form}"
        );
    }
    for form in not_json {
        assert!(decode_raw(&line(form)).is_err(), "{form}: parsed as JSON");
    }
    for ok in ["-9223372036854775808", "9223372036854775807"] {
        assert!(ids.contains(ok), "the bullet states the range ({ok})");
        assert!(matches!(decode_raw(&line(ok)), Ok(Ok(Message::Request(_)))));
    }
}

/// The catalogue documents duplicate keys in a request, and the daemon's decoding does what
/// it says: the last occurrence wins, at every level.
#[test]
fn duplicate_keys_are_documented_and_the_last_wins() {
    let text = bullet(&catalogue_text(), "**Duplicate keys.**");
    assert!(
        text.contains("the daemon reads the last occurrence"),
        "{text}"
    );
    let line = r#"{"jsonrpc":"2.0","id":1,"id":2,"method":"item.pin","params":{"paths":["/m/a"],"paths":["/m/b"]}}"#;
    let Ok(Ok(Message::Request(request))) = decode_raw(line) else {
        panic!("a request with repeated keys was not read as a request");
    };
    assert_eq!(request.id, mydrive_ipc::envelope::Id::Number(2));
    assert_eq!(
        MethodCall::from_request(&request).unwrap(),
        MethodCall::ItemPin(ItemPathsParams {
            paths: vec![mydrive_ipc::path::ProtocolPath::new("/m/b").unwrap()],
        })
    );
}

/// `seq`, `skipped_count` and `bytes_freed` are documented as integers a client may receive
/// above 2^53, and the types carry such values exactly. `seq` is documented as optional, with
/// the tie rule stated, and the types accept its absence.
#[test]
fn large_integers_and_optional_seq_are_documented() {
    let text = catalogue_text();
    let large = bullet(&text, "**Large integers.**");
    for needle in [
        "`seq`, `skipped_count` and `bytes_freed` are unsigned 64-bit",
        "above 2^53",
    ] {
        assert!(large.contains(needle), "{needle:?} missing from: {large}");
    }
    let changed = text
        .split("\n### status.changed")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("a status.changed section");
    let changed = changed.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in [
        "`seq` (optional)",
        "On equal `seq`, a notification's state wins over a page's.",
        "`seq` is optional on both pages and notifications.",
    ] {
        assert!(
            changed.contains(needle),
            "status.changed must say {needle:?}"
        );
    }

    // Above 2^53, through text, without loss.
    let big = u64::MAX;
    let page: ItemStatusResult =
        serde_json::from_str(&format!(r#"{{"seq":{big},"entries":[]}}"#)).unwrap();
    assert_eq!(page.seq, Some(big));
    let note: StatusChangedParams =
        serde_json::from_str(&format!(r#"{{"seq":{big},"entries":[],"refresh":[]}}"#)).unwrap();
    assert_eq!(note.seq, Some(big));
    let action: ItemActionResult = serde_json::from_str(&format!(
        r#"{{"entries":[{{"path":"/m/a","skipped_count":{big},"bytes_freed":{big}}}]}}"#
    ))
    .unwrap();
    assert!(
        serde_json::to_string(&action)
            .unwrap()
            .contains(&big.to_string())
    );
    // `seq` may be absent from a page and from a notification.
    let page: ItemStatusResult = serde_json::from_str(r#"{"entries":[]}"#).unwrap();
    assert_eq!(page.seq, None);
    let note: StatusChangedParams = serde_json::from_str(r#"{"entries":[],"refresh":[]}"#).unwrap();
    assert_eq!(note.seq, None);
}

/// The catalogue says the shell loop streams entries, so a consumer must check its exit
/// status (`readme_examples.rs` shows that it does print a partial listing before failing).
#[test]
fn shell_loop_is_documented_as_streaming() {
    let text = catalogue_text();
    let intro = text
        .split("A cursor loop in shell")
        .nth(1)
        .and_then(|s| s.split("```sh").next())
        .expect("the shell loop's introduction");
    let intro = intro.split_whitespace().collect::<Vec<_>>().join(" ");
    for needle in ["It streams", "must check its exit status"] {
        assert!(intro.contains(needle), "{needle:?} missing from: {intro}");
    }
}

const UNKNOWN: &str = "zz-unknown-value";

/// Sets `field` of `base` to `value` and reports whether the containing type still parses
/// and keeps the value (open), or fails (closed).
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

/// An error with `data` set, parsed, for the `*.data.*` probes.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn error_with(code: ErrorCode, data: Value) -> RpcError {
    serde_json::from_value(json!({"code": code.code(), "message": "m", "data": data})).unwrap()
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
    let reason = json!({"reason": UNKNOWN});
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
        "refused.data.reason" => {
            error_with(ErrorCode::Refused, reason).refusal_reason()
                == Some(RefusalReason::Other(UNKNOWN.into()))
        }
        "blocked-by-policy.data.reason" => {
            error_with(ErrorCode::BlockedByPolicy, reason).blocked_reason()
                == Some(BlockedReason::Other(UNKNOWN.into()))
        }
        "read-only.data.reason" => {
            error_with(ErrorCode::ReadOnly, reason).read_only_reason()
                == Some(ReadOnlyReason::Other(UNKNOWN.into()))
        }
        "invalid-params.data.reason" => {
            error_with(ErrorCode::InvalidParams, reason).invalid_params_reason()
                == Some(InvalidParamsReason::Other(UNKNOWN.into()))
        }
        "refused.data.sync" => error_with(ErrorCode::Refused, json!({"sync": UNKNOWN}))
            .sync()
            .is_some_and(|s| s.as_str() == UNKNOWN),
        "hello.capabilities" => probe::<HelloResult>(
            json!({"protocol_version": 1, "capabilities": [],
                   "limits": {"max_line_bytes": 1, "max_action_paths": 1,
                              "max_action_path_bytes": 1, "max_status_paths": 1,
                              "max_status_path_bytes": 1}}),
            "capabilities",
            json!([UNKNOWN]),
        ),
        other => panic!("value-set field {other:?} has no probe; add one"),
    }
}

/// Every enumerated type in this crate's sources: the `open_enum!` invocations and any enum
/// deriving serde. Envelope enums that are not value sets are excluded by name.
#[cfg(test)] // helpers count as test code for clippy's allow-unwrap-in-tests
fn enumerated_wire_types() -> BTreeSet<String> {
    const NOT_VALUE_SETS: &[&str] = &["Id", "Message"];
    let mut found = BTreeSet::new();
    for file in [
        "codec.rs",
        "envelope.rs",
        "error.rs",
        "methods.rs",
        "paging.rs",
        "path.rs",
        "values.rs",
        "lib.rs",
    ] {
        let source = crate_file(&format!("src/{file}"));
        let lines: Vec<&str> = source.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("pub enum ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let macro_form = rest[name.len()..].trim_start().starts_with(", kind =");
            let serde_derive = lines[i.saturating_sub(4)..i]
                .iter()
                .any(|l| l.contains("derive(") && l.contains("Serialize"));
            if (macro_form || serde_derive) && !NOT_VALUE_SETS.contains(&name.as_str()) {
                found.insert(name);
            }
        }
    }
    found
}

/// ADR-0019 R7, ADR-0020 R7: every enumerated field is marked closed or open in the
/// catalogue, the marking matches the types, and the list cannot drift: every enumerated
/// wire type is registered in `VALUE_SETS`, and every registered field has a catalogue row,
/// in both directions.
#[test]
fn value_set_marking_matches_the_types() {
    // Every enumerated wire type in the sources is registered, and every registered enum
    // type exists (`DisplayStatus` comes from mydrive-core; i64 and String are not enums).
    let in_sources = enumerated_wire_types();
    let registered: BTreeSet<String> = VALUE_SETS
        .iter()
        .map(|s| s.type_name.to_owned())
        .filter(|t| !["DisplayStatus", "i64", "String"].contains(&t.as_str()))
        .collect();
    assert_eq!(
        in_sources, registered,
        "enumerated wire types and VALUE_SETS differ; register the type and add a catalogue row"
    );

    let text = catalogue_text();
    let section = text
        .split("## Value sets")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("a Value sets section");
    let mut rows = BTreeSet::new();
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
        let registered = VALUE_SETS
            .iter()
            .find(|s| s.fields.contains(&field))
            .unwrap_or_else(|| panic!("catalogue row {field} is not in VALUE_SETS"));
        assert_eq!(
            open, registered.open,
            "{field}: catalogue and VALUE_SETS disagree"
        );
        assert_eq!(
            accepts_unknown(field),
            open,
            "{field} is marked {set} but its type disagrees"
        );
        if open {
            let fallback = cells.get(4).copied().unwrap_or("");
            assert!(
                !fallback.is_empty() && fallback != "—",
                "{field}: open set without a client fallback"
            );
        }
        if let Some(known) = registered.known {
            let listed: BTreeSet<String> = cells
                .get(3)
                .copied()
                .unwrap_or("")
                .split(',')
                .map(|v| v.trim().trim_matches('`').to_owned())
                .filter(|v| !v.is_empty())
                .collect();
            let expected: BTreeSet<String> = known.iter().map(|s| (*s).to_owned()).collect();
            assert_eq!(
                listed, expected,
                "{field}: listed values differ from the type"
            );
        }
        assert!(rows.insert(field.to_owned()), "{field} is listed twice");
    }
    let fields: BTreeSet<String> = VALUE_SETS
        .iter()
        .flat_map(|s| s.fields.iter().map(|f| (*f).to_owned()))
        .collect();
    assert_eq!(
        rows, fields,
        "every registered field has exactly one catalogue row"
    );
    let closed: Vec<&str> = VALUE_SETS
        .iter()
        .filter(|s| !s.open)
        .map(|s| s.type_name)
        .collect();
    assert_eq!(
        closed,
        ["DisplayStatus"],
        "ADR-0020: only the display status is closed"
    );
}
