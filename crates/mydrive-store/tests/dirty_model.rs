//! Fourth review, blocking: a dirty file is always fully local. Property test over the
//! local-write operations.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Fixture, ROOT, file, key, remote};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::state::Hydration;
use mydrive_store::{ByteRange, Queries, RemoteMetadata, StoreError};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Mark,
    SetSize(u64),
    Truncate(u64),
    Record(u64, u64),
    Lower,
    CompleteUpload,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Mark),
        (0u64..80).prop_map(Op::SetSize),
        (0u64..80).prop_map(Op::Truncate),
        (0u64..80, 0u64..80).prop_map(|(a, b)| Op::Record(a.min(b), a.max(b))),
        Just(Op::Lower),
        Just(Op::CompleteUpload),
    ]
}

/// The starting file: a placeholder, a partly hydrated file, or a fully local one.
#[derive(Debug, Clone, Copy)]
enum Start {
    Placeholder,
    Partial,
    Full,
}

proptest! {
    /// For any sequence of local-write operations from any starting hydration, whenever
    /// the file is dirty it is `full` and its ranges are exactly `[0, size)`, and a refused
    /// operation changes nothing.
    #[test]
    fn a_dirty_file_is_always_fully_local(
        size in 0u64..60,
        start in prop_oneof![Just(Start::Placeholder), Just(Start::Partial), Just(Start::Full)],
        ops in proptest::collection::vec(op(), 1..25),
    ) {
        let fx = Fixture::new();
        let mut store = fx.open_with_mount();
        let tx = store.transaction().unwrap();
        let k = key("f");
        let mut item = file("f", Some(ROOT), "f", size);
        item.state.hydration = match start {
            Start::Placeholder => Hydration::Placeholder,
            Start::Partial => Hydration::Partial,
            Start::Full => Hydration::Full,
        };
        tx.insert_item(&item).unwrap();
        match start {
            Start::Placeholder => {}
            Start::Partial if size > 1 => {
                tx.record_hydrated_range(&k, ByteRange::new(0, size / 2).unwrap()).unwrap();
            }
            Start::Partial => {}
            Start::Full if size > 0 => {
                tx.record_hydrated_range(&k, ByteRange::new(0, size).unwrap()).unwrap();
            }
            Start::Full => {}
        }

        for op in ops {
            let before = (tx.item(&k).unwrap(), tx.hydrated_ranges(&k).unwrap());
            let result: Result<(), StoreError> = match op {
                Op::Mark => tx.mark_local_change(&k).map(drop),
                Op::SetSize(n) => tx.set_local_size(&k, n).map(drop),
                Op::Truncate(n) => tx.truncate_local(&k, n).map(drop),
                Op::Record(a, b) => tx.record_hydrated_range(&k, ByteRange::new(a, b).unwrap()),
                Op::Lower => tx.set_hydration(&k, Hydration::Partial),
                Op::CompleteUpload => {
                    let current = tx.item(&k).unwrap().unwrap();
                    let response = RemoteMetadata {
                        etag: Some(format!("etag-{}", current.local_generation)),
                        server_hash: Some(QuickXorHash::from_bytes([7; 20])),
                        ..remote(&current)
                    };
                    tx.complete_upload(
                        &k,
                        current.local_generation,
                        QuickXorHash::from_bytes([7; 20]),
                        &response,
                        None,
                    )
                    .map(drop)
                }
            };
            let now = tx.item(&k).unwrap().unwrap();
            let ranges = tx.hydrated_ranges(&k).unwrap();
            if result.is_err() {
                prop_assert_eq!(&(Some(now.clone()), ranges.clone()), &before, "{:?}", op);
            }
            if now.local_dirty {
                prop_assert_eq!(now.state.hydration, Hydration::Full, "{:?}", op);
                let whole: Vec<ByteRange> = if now.size == 0 {
                    vec![]
                } else {
                    vec![ByteRange::new(0, now.size).unwrap()]
                };
                prop_assert_eq!(ranges, whole, "{:?}", op);
            }
        }
    }
}
