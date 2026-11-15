//! Model-based test of the store's mutating operations.
//!
//! Fourth review, blocking: a dirty file is always fully local. WP-017 AC10 extends the
//! same property test to every operation version 2 adds (local IDs, delta runs, mount
//! facts and mount removal), checked against a model after every step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{Fixture, MOUNT, ROOT, file, folder, key, limit, mount, mount_id, remote};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{ItemId, ItemKey};
use mydrive_core::state::Hydration;
use mydrive_store::{
    BlockedReason, ByteRange, DeleteMode, DeltaRunKind, LocalId, LocalOnlyContent, MountBlock,
    MountPause, PageLink, PendingChanges, Queries, RemoteMetadata, StagedChange, StagedEntry,
    StagedFacet, StagedItem, StoreError, Timestamp, Transaction,
};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    // Version 1: local writes on the file `f`.
    Mark,
    SetSize(u64),
    Truncate(u64),
    Record(u64, u64),
    Lower,
    CompleteUpload,
    // Version 2: rows and local IDs.
    Insert(u8),
    Delete(u8),
    MoveLocal(u8, u8),
    CreateProvisional,
    UploadProvisional,
    // Version 2: delta runs.
    BeginRun(DeltaRunKind),
    Stage(Vec<(u8, u8)>, bool),
    Finish,
    // Version 2: mount facts and removal.
    Pause(bool),
    Block(bool),
    Remove(bool),
}

fn run_kind() -> impl Strategy<Value = DeltaRunKind> {
    prop_oneof![
        Just(DeltaRunKind::Initial),
        Just(DeltaRunKind::Incremental),
        Just(DeltaRunKind::ResyncApply),
        Just(DeltaRunKind::ResyncUpload),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Mark),
        (0u64..80).prop_map(Op::SetSize),
        (0u64..80).prop_map(Op::Truncate),
        (0u64..80, 0u64..80).prop_map(|(a, b)| Op::Record(a.min(b), a.max(b))),
        Just(Op::Lower),
        Just(Op::CompleteUpload),
        (0u8..4).prop_map(Op::Insert),
        (0u8..4).prop_map(Op::Delete),
        (0u8..4, 0u8..3).prop_map(|(a, b)| Op::MoveLocal(a, b)),
        Just(Op::CreateProvisional),
        Just(Op::UploadProvisional),
        run_kind().prop_map(Op::BeginRun),
        (
            proptest::collection::vec((0u8..6, 0u8..3), 0..5),
            any::<bool>()
        )
            .prop_map(|(e, last)| Op::Stage(e, last)),
        Just(Op::Finish),
        any::<bool>().prop_map(Op::Pause),
        any::<bool>().prop_map(Op::Block),
        any::<bool>().prop_map(Op::Remove),
    ]
}

/// The starting file: a placeholder, a partly hydrated file, or a fully local one.
#[derive(Debug, Clone, Copy)]
enum Start {
    Placeholder,
    Partial,
    Full,
}

/// What the model expects of the mount's delta run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RunModel {
    token: Option<String>,
    run: Option<DeltaRunKind>,
    next: Option<String>,
    staged_link: Option<String>,
    /// The last entry staged for each item in the run.
    staged: BTreeMap<String, StagedChange>,
}

/// The model of everything version 2 adds.
#[derive(Debug, Default)]
struct Model {
    run: RunModel,
    /// Every live row's local ID, as first assigned.
    local_ids: BTreeMap<ItemKey, LocalId>,
    /// Every local ID ever handed out.
    ever: BTreeSet<LocalId>,
    links: u32,
    provisional: u32,
}

fn staged_entry(item: u8, what: u8) -> StagedEntry {
    let item_id = ItemId::new(format!("s{item}")).unwrap();
    let change = match what {
        0 => StagedChange::Deleted,
        _ => StagedChange::Present(StagedItem {
            parent_item_id: Some(ItemId::new(ROOT).unwrap()),
            name: format!("s{item}-{what}"),
            facet: if what == 1 {
                StagedFacet::File
            } else {
                StagedFacet::Folder
            },
            size: u64::from(what),
            created: Timestamp::from_unix_seconds(1),
            modified: Timestamp::from_unix_seconds(2),
            etag: Some(format!("e{what}")),
            ctag: None,
            server_hash: None,
        }),
    };
    StagedEntry {
        item_id,
        is_root: false,
        change,
    }
}

/// Every item row visible through the API, with its local ID, for "nothing changed"
/// checks.
fn item_rows(tx: &Transaction<'_>) -> Vec<Row> {
    let mut out = Vec::new();
    let mut queue = vec![key(ROOT)];
    if let Some(root) = tx.item(&key(ROOT)).unwrap() {
        out.push((root, tx.local_id(&key(ROOT)).unwrap(), vec![]));
    } else {
        queue.clear();
    }
    while let Some(parent) = queue.pop() {
        for child in tx.children(&parent).unwrap() {
            let ranges = if child.kind.has_content() {
                tx.hydrated_ranges(&child.key).unwrap()
            } else {
                vec![]
            };
            queue.push(child.key.clone());
            let id = tx.local_id(&child.key).unwrap();
            out.push((child, id, ranges));
        }
    }
    out.sort_by(|a, b| a.0.key.item_id.cmp(&b.0.key.item_id));
    out
}

/// An item row with its local ID and ranges.
type Row = (mydrive_store::Item, LocalId, Vec<ByteRange>);

/// The mount, its staged count and every item row.
type Snapshot = (Option<mydrive_store::Mount>, u64, Vec<Row>);

fn snapshot(tx: &Transaction<'_>) -> Snapshot {
    let staged = if tx.mount(&mount_id()).unwrap().is_some() {
        tx.staged_count(&mount_id()).unwrap()
    } else {
        0
    };
    (tx.mount(&mount_id()).unwrap(), staged, item_rows(tx))
}

/// Applies `op` to the store and the model; returns the store's result.
fn apply(tx: &Transaction<'_>, model: &mut Model, op: &Op) -> Result<(), StoreError> {
    let k = key("f");
    let m = mount_id();
    match op {
        Op::Mark => tx.mark_local_change(&k).map(drop),
        Op::SetSize(n) => tx.set_local_size(&k, *n).map(drop),
        Op::Truncate(n) => tx.truncate_local(&k, *n).map(drop),
        Op::Record(a, b) => tx.record_hydrated_range(&k, ByteRange::new(*a, *b).unwrap()),
        Op::Lower => tx.set_hydration(&k, Hydration::Partial),
        Op::CompleteUpload => {
            let Some(current) = tx.item(&k).unwrap() else {
                return Err(StoreError::NotDirty { key: k });
            };
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
        Op::Insert(n) => {
            let item = file(&format!("x{n}"), Some(ROOT), &format!("x{n}"), 1);
            let id = tx.insert_item(&item)?;
            assert!(model.ever.iter().all(|e| id > *e), "{id} reused or lower");
            model.ever.insert(id);
            model.local_ids.insert(item.key, id);
            Ok(())
        }
        Op::Delete(n) => {
            let k = key(&format!("x{n}"));
            tx.delete_item(&k, DeleteMode::ItemOnly, PendingChanges::Refuse)?;
            model.local_ids.remove(&k);
            Ok(())
        }
        Op::MoveLocal(n, to) => {
            let parent = match to {
                0 => ItemId::new(ROOT).unwrap(),
                _ => ItemId::new("dir").unwrap(),
            };
            tx.move_item_local(&key(&format!("x{n}")), Some(&parent), &format!("moved{to}"))
        }
        Op::CreateProvisional => {
            model.provisional += 1;
            let mut item = file(&format!("tmp{}", model.provisional), Some(ROOT), "new", 2);
            item.is_provisional = true;
            item.local_dirty = true;
            item.local_generation = 1;
            item.etag = None;
            item.state.hydration = Hydration::Full;
            item.state.sync = mydrive_core::state::Sync::PendingUpload;
            let id = tx.insert_item(&item)?;
            assert!(model.ever.iter().all(|e| id > *e), "{id} reused or lower");
            model.ever.insert(id);
            model.local_ids.insert(item.key, id);
            Ok(())
        }
        Op::UploadProvisional => {
            let old = key(&format!("tmp{}", model.provisional));
            let Some(item) = tx.item(&old).unwrap() else {
                return Err(StoreError::NotDirty { key: old });
            };
            let new_id = ItemId::new(format!("srv{}", model.provisional)).unwrap();
            let new = ItemKey::new(old.drive_id.clone(), new_id.clone());
            let response = RemoteMetadata {
                key: new.clone(),
                etag: Some("e".to_owned()),
                ..remote(&item)
            };
            tx.complete_upload(
                &old,
                item.local_generation,
                QuickXorHash::from_bytes([2; 20]),
                &response,
                Some(&new_id),
            )?;
            let id = model.local_ids.remove(&old).unwrap();
            model.local_ids.insert(new, id);
            Ok(())
        }
        Op::BeginRun(kind) => {
            tx.begin_delta_run(&m, *kind)?;
            let r = &mut model.run;
            r.run = Some(*kind);
            r.next = None;
            r.staged_link = None;
            r.staged.clear();
            Ok(())
        }
        Op::Stage(entries, last) => {
            model.links += 1;
            let link = format!("link-{}", model.links);
            let link = if *last {
                PageLink::Delta(link)
            } else {
                PageLink::Next(link)
            };
            let entries: Vec<StagedEntry> =
                entries.iter().map(|(i, w)| staged_entry(*i, *w)).collect();
            let items_before = item_rows(tx);
            let result = tx.stage_delta_page(&m, &entries, &link);
            // ADR-0022 R3: staging never changes an item row, staged or refused.
            assert_eq!(item_rows(tx), items_before, "staging changed an item row");
            result?;
            let r = &mut model.run;
            for e in entries {
                r.staged.insert(e.item_id.as_str().to_owned(), e.change);
            }
            match link {
                PageLink::Next(l) => r.next = Some(l),
                PageLink::Delta(l) => {
                    r.next = None;
                    r.staged_link = Some(l);
                }
            }
            Ok(())
        }
        Op::Finish => {
            tx.finish_delta_run(&m)?;
            let r = &mut model.run;
            r.token = r.staged_link.take();
            r.run = None;
            r.next = None;
            r.staged.clear();
            Ok(())
        }
        Op::Pause(on) => tx.set_mount_paused(
            &m,
            if *on {
                MountPause::PausedUntil(Timestamp::from_unix_seconds(9))
            } else {
                MountPause::Running
            },
        ),
        Op::Block(on) => tx.set_mount_blocked(
            &m,
            on.then(|| MountBlock {
                reason: BlockedReason::OrganisationPolicy,
                message: Some("policy".to_owned()),
            })
            .as_ref(),
        ),
        Op::Remove(kept) => {
            let dirty = tx.dirty_items().unwrap().len();
            let local_only = if *kept {
                LocalOnlyContent::KeptElsewhere {
                    dirty_items: u64::try_from(dirty).unwrap(),
                }
            } else {
                LocalOnlyContent::Refuse
            };
            tx.remove_mount(&m, local_only)?;
            // The engine adds the drive again: a new mount, a new root row, new IDs.
            model.local_ids.clear();
            model.run = RunModel::default();
            tx.insert_mount(&mount(MOUNT, ROOT, "/home/u/OneDrive"))
                .unwrap();
            let id = tx.insert_item(&folder(ROOT, None, "root")).unwrap();
            assert!(
                model.ever.iter().all(|e| id > *e),
                "{id} reused after removal"
            );
            model.ever.insert(id);
            model.local_ids.insert(key(ROOT), id);
            Ok(())
        }
    }
}

/// Every invariant of the model, checked after each step.
fn check(tx: &Transaction<'_>, model: &Model, op: &Op) -> Result<(), TestCaseError> {
    let m = mount_id();
    // A dirty file is always fully local (fourth review).
    if let Some(now) = tx.item(&key("f")).unwrap() {
        let ranges = tx.hydrated_ranges(&key("f")).unwrap();
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
    // ADR-0021 R1, R2: every live row keeps the local ID it was given; IDs are unique.
    let rows = item_rows(tx);
    let mut seen = BTreeSet::new();
    for (item, id, _) in &rows {
        prop_assert!(seen.insert(*id), "duplicate local ID {} ({:?})", id, op);
        if let Some(expected) = model.local_ids.get(&item.key) {
            prop_assert_eq!(
                id,
                expected,
                "local ID of {:?} changed by {:?}",
                item.key,
                op
            );
        }
    }
    // The run: mount columns, staged rows only while a run is in progress, the live
    // token moved only by a finish (ADR-0022 R4).
    let mount = tx.mount(&m).unwrap().unwrap();
    let got = (
        mount.delta_token.clone(),
        mount.delta_run,
        mount.delta_next_link.clone(),
        mount.delta_staged_link.clone(),
    );
    let r = &model.run;
    prop_assert_eq!(
        got,
        (
            r.token.clone(),
            r.run,
            r.next.clone(),
            r.staged_link.clone()
        ),
        "{:?}",
        op
    );
    let count = tx.staged_count(&m).unwrap();
    prop_assert_eq!(count, u64::try_from(r.staged.len()).unwrap(), "{:?}", op);
    if r.run.is_none() {
        prop_assert_eq!(count, 0, "staged rows without a run");
    }
    // ADR-0022 R2: once complete, the walk holds each item's last entry exactly once.
    if r.staged_link.is_some() {
        let mut walked = BTreeMap::new();
        let mut cursor = None;
        loop {
            let page = tx.staged_entries(&m, cursor.as_ref(), limit(2)).unwrap();
            for s in page.entries {
                let id = s.entry.item_id.as_str().to_owned();
                prop_assert!(walked.insert(id, s.entry.change).is_none(), "walked twice");
            }
            match page.next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        prop_assert_eq!(&walked, &r.staged, "{:?}", op);
    }
    Ok(())
}

proptest! {
    /// For any sequence of operations from any starting hydration: whenever the file is
    /// dirty it is `full` with ranges exactly `[0, size)`; a refused operation changes
    /// nothing; and every invariant of the version 2 model holds (WP-017 AC10).
    #[test]
    fn a_dirty_file_is_always_fully_local(
        size in 0u64..60,
        start in prop_oneof![Just(Start::Placeholder), Just(Start::Partial), Just(Start::Full)],
        ops in proptest::collection::vec(op(), 1..30),
    ) {
        let fx = Fixture::new();
        let mut store = fx.open_with_mount();
        let mut model = Model::default();
        let root_id = store.local_id(&key(ROOT)).unwrap();
        model.ever.insert(root_id);
        model.local_ids.insert(key(ROOT), root_id);
        let tx = store.transaction().unwrap();
        let k = key("f");
        let mut item = file("f", Some(ROOT), "f", size);
        item.state.hydration = match start {
            Start::Placeholder => Hydration::Placeholder,
            Start::Partial => Hydration::Partial,
            Start::Full => Hydration::Full,
        };
        for (it, id) in [
            (item, "f"),
            (folder("dir", Some(ROOT), "dir"), "dir"),
        ] {
            let local = tx.insert_item(&it).unwrap();
            model.ever.insert(local);
            model.local_ids.insert(key(id), local);
        }
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
            let before = snapshot(&tx);
            let result = apply(&tx, &mut model, &op);
            if result.is_err() {
                prop_assert_eq!(&snapshot(&tx), &before, "{:?} refused: {:?}", op, result);
            }
            check(&tx, &model, &op)?;
        }
    }
}
