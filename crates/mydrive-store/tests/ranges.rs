//! Hydrated byte ranges (WP-006 AC3, ADR-0006 R6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::{Fixture, ROOT, file, folder, key};
use mydrive_store::{ByteRange, Queries, StoreError};
use proptest::prelude::*;

fn r(start: u64, end: u64) -> ByteRange {
    ByteRange::new(start, end).unwrap()
}

#[test]
fn ac3_adjacent_and_overlapping_ranges_merge() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    let k = key("f");

    store.record_hydrated_range(&k, r(10, 20)).unwrap();
    store.record_hydrated_range(&k, r(30, 40)).unwrap();
    assert_eq!(store.hydrated_ranges(&k).unwrap(), [r(10, 20), r(30, 40)]);

    // Adjacent on the left: merges.
    store.record_hydrated_range(&k, r(5, 10)).unwrap();
    assert_eq!(store.hydrated_ranges(&k).unwrap(), [r(5, 20), r(30, 40)]);

    // Bridging both: one range.
    store.record_hydrated_range(&k, r(15, 35)).unwrap();
    assert_eq!(store.hydrated_ranges(&k).unwrap(), [r(5, 40)]);

    // Contained: no change.
    store.record_hydrated_range(&k, r(6, 7)).unwrap();
    assert_eq!(store.hydrated_ranges(&k).unwrap(), [r(5, 40)]);

    // Out of order (later range first, earlier range second) and up to the last byte.
    store.record_hydrated_range(&k, r(90, 100)).unwrap();
    store.record_hydrated_range(&k, r(0, 2)).unwrap();
    assert_eq!(
        store.hydrated_ranges(&k).unwrap(),
        [r(0, 2), r(5, 40), r(90, 100)]
    );
}

#[test]
fn ac3_range_is_present_only_when_every_byte_is_covered() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    let k = key("f");
    store.record_hydrated_range(&k, r(10, 20)).unwrap();
    store.record_hydrated_range(&k, r(21, 30)).unwrap();

    assert!(store.is_range_hydrated(&k, r(10, 20)).unwrap());
    assert!(store.is_range_hydrated(&k, r(12, 18)).unwrap());
    assert!(!store.is_range_hydrated(&k, r(9, 20)).unwrap());
    assert!(!store.is_range_hydrated(&k, r(10, 21)).unwrap());
    // Spans the one-byte gap at 20.
    assert!(!store.is_range_hydrated(&k, r(15, 25)).unwrap());
    assert!(!store.is_range_hydrated(&k, r(50, 60)).unwrap());
}

#[test]
fn ac3_zero_length_ranges_are_handled() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 10)).unwrap();
    store
        .insert_item(&file("empty", Some(ROOT), "e", 0))
        .unwrap();
    let k = key("f");

    store.record_hydrated_range(&k, r(5, 5)).unwrap();
    assert!(
        store.hydrated_ranges(&k).unwrap().is_empty(),
        "nothing recorded"
    );
    assert!(
        store.is_range_hydrated(&k, r(5, 5)).unwrap(),
        "no bytes asked for"
    );
    assert!(store.is_range_hydrated(&k, r(10, 10)).unwrap());

    // An empty file has no bytes to record and every empty query is present.
    store.record_hydrated_range(&key("empty"), r(0, 0)).unwrap();
    assert!(store.is_range_hydrated(&key("empty"), r(0, 0)).unwrap());
}

#[test]
fn ac3_reversed_range_cannot_be_built() {
    assert!(matches!(
        ByteRange::new(10, 5),
        Err(StoreError::InvalidRange { start: 10, end: 5 })
    ));
}

#[test]
fn ac3_ranges_cannot_exceed_the_item_size() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    let k = key("f");

    let err = store.record_hydrated_range(&k, r(90, 101)).unwrap_err();
    assert!(
        matches!(err, StoreError::RangeOutOfBounds { size: 100, .. }),
        "{err}"
    );
    assert!(store.hydrated_ranges(&k).unwrap().is_empty());
    let err = store.is_range_hydrated(&k, r(100, 101)).unwrap_err();
    assert!(matches!(err, StoreError::RangeOutOfBounds { .. }), "{err}");
    let err = store.record_hydrated_range(&k, r(200, 200)).unwrap_err();
    assert!(matches!(err, StoreError::RangeOutOfBounds { .. }), "{err}");
}

#[test]
fn ranges_are_refused_for_folders_and_unknown_items() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&folder("d", Some(ROOT), "d")).unwrap();
    let err = store.record_hydrated_range(&key("d"), r(0, 0)).unwrap_err();
    assert!(matches!(err, StoreError::NotAFile { .. }), "{err}");
    let err = store.record_hydrated_range(&key("x"), r(0, 1)).unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
    let err = store.clear_hydrated_ranges(&key("x")).unwrap_err();
    assert!(matches!(err, StoreError::ItemNotFound { .. }), "{err}");
}

#[test]
fn clear_removes_every_range_of_one_item_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("f", Some(ROOT), "f", 100)).unwrap();
    store.insert_item(&file("g", Some(ROOT), "g", 100)).unwrap();
    store.record_hydrated_range(&key("f"), r(0, 10)).unwrap();
    store.record_hydrated_range(&key("f"), r(50, 60)).unwrap();
    store.record_hydrated_range(&key("g"), r(0, 10)).unwrap();

    assert_eq!(store.clear_hydrated_ranges(&key("f")).unwrap(), 2);
    assert!(store.hydrated_ranges(&key("f")).unwrap().is_empty());
    assert!(!store.is_range_hydrated(&key("f"), r(0, 1)).unwrap());
    assert_eq!(store.hydrated_ranges(&key("g")).unwrap(), [r(0, 10)]);
}

/// ADR-0006 R6: the hydrated-range map is read from the store. A backing file whose bytes
/// are all allocated does not make a range present; only a recorded range does, and it
/// survives reopening.
#[test]
fn adr_0006_r6_hydrated_ranges_come_from_the_store_only() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_item(&file("f", Some(ROOT), "f", 4096))
        .unwrap();
    // A fully written (no holes) file next to the store has no bearing on the answer.
    std::fs::write(fx.dir.path().join("f"), vec![1u8; 4096]).unwrap();
    assert!(!store.is_range_hydrated(&key("f"), r(0, 4096)).unwrap());

    store.record_hydrated_range(&key("f"), r(0, 4096)).unwrap();
    drop(store);
    let store = fx.open();
    assert!(store.is_range_hydrated(&key("f"), r(0, 4096)).unwrap());
}

#[derive(Debug, Clone)]
enum Op {
    Record(u64, u64),
    Query(u64, u64),
    Clear,
}

fn op(limit: u64) -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..=limit, 0..=limit).prop_map(|(a, b)| Op::Record(a, b)),
        4 => (0..=limit, 0..=limit).prop_map(|(a, b)| Op::Query(a, b)),
        1 => Just(Op::Clear),
    ]
}

/// The maximal runs of consecutive bytes in `bytes`, as ranges.
fn runs(bytes: &BTreeSet<u64>) -> Vec<ByteRange> {
    let mut out: Vec<ByteRange> = Vec::new();
    for &b in bytes {
        match out.last_mut() {
            Some(last) if last.end() == b => *last = ByteRange::new(last.start(), b + 1).unwrap(),
            _ => out.push(ByteRange::new(b, b + 1).unwrap()),
        }
    }
    out
}

proptest! {
    /// WP-006 AC3 required property test: the store agrees with a set-of-bytes model for
    /// any sequence of records, queries and clears, including zero-length, reversed and
    /// out-of-bounds inputs, and always holds the merged form.
    #[test]
    fn ac3_ranges_match_a_set_of_bytes_model(
        size in 0u64..48,
        ops in proptest::collection::vec(op(56), 1..40),
    ) {
        let fx = Fixture::new();
        let mut store = fx.open_with_mount();
        let tx = store.transaction().unwrap();
        tx.insert_item(&file("f", Some(ROOT), "f", size)).unwrap();
        let k = key("f");
        let mut model: BTreeSet<u64> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Clear => {
                    tx.clear_hydrated_ranges(&k).unwrap();
                    model.clear();
                }
                Op::Record(a, b) | Op::Query(a, b) => {
                    let is_record = matches!(op, Op::Record(..));
                    let range = match ByteRange::new(a, b) {
                        Ok(range) => range,
                        Err(err) => {
                            prop_assert!(a > b);
                            let invalid = matches!(err, StoreError::InvalidRange { .. });
                            prop_assert!(invalid, "{}", err);
                            continue;
                        }
                    };
                    let result = if is_record {
                        tx.record_hydrated_range(&k, range).map(|()| None)
                    } else {
                        tx.is_range_hydrated(&k, range).map(Some)
                    };
                    if b > size {
                        let refused = matches!(result, Err(StoreError::RangeOutOfBounds { .. }));
                        prop_assert!(refused, "{:?}", result);
                    } else if is_record {
                        prop_assert!(result.is_ok(), "{:?}", result);
                        model.extend(a..b);
                    } else {
                        let expected = (a..b).all(|byte| model.contains(&byte));
                        prop_assert_eq!(result.unwrap(), Some(expected));
                    }
                }
            }
            prop_assert_eq!(tx.hydrated_ranges(&k).unwrap(), runs(&model));
        }
    }
}
