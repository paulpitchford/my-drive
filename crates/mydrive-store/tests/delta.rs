//! Delta runs: staging, apply order, the resync anti-join and finishing (ADR-0022).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{Fixture, ROOT, file, finish_run_with_token, folder, key, limit, mount_id, raw};
use mydrive_core::hash::QuickXorHash;
use mydrive_core::ids::{DriveId, ItemId, ItemKey};
use mydrive_store::{
    DeltaRunKind, PageLink, Queries, Staged, StagedChange, StagedEntry, StagedFacet, StagedItem,
    StagedProblem, Store, StoreError, Timestamp,
};

fn id(s: &str) -> ItemId {
    ItemId::new(s).unwrap()
}

fn item(parent: Option<&str>, name: &str, facet: StagedFacet) -> StagedItem {
    StagedItem {
        parent_item_id: parent.map(id),
        name: name.to_owned(),
        facet,
        size: 10,
        created: Timestamp::from_unix_seconds(1_700_000_000),
        modified: Timestamp::from_unix_seconds(1_700_000_100),
        etag: Some(format!("etag-{name}")),
        ctag: None,
        server_hash: Some(QuickXorHash::from_bytes([3; 20])),
    }
}

fn present(item_id: &str, parent: &str, name: &str) -> StagedEntry {
    StagedEntry {
        item_id: id(item_id),
        is_root: false,
        change: StagedChange::Present(item(Some(parent), name, StagedFacet::File)),
    }
}

fn dir(item_id: &str, parent: &str) -> StagedEntry {
    StagedEntry {
        item_id: id(item_id),
        is_root: false,
        change: StagedChange::Present(item(Some(parent), item_id, StagedFacet::Folder)),
    }
}

fn drive_root(item_id: &str) -> StagedEntry {
    StagedEntry {
        item_id: id(item_id),
        is_root: true,
        change: StagedChange::Present(item(None, "root", StagedFacet::Folder)),
    }
}

fn deleted(item_id: &str) -> StagedEntry {
    StagedEntry {
        item_id: id(item_id),
        is_root: false,
        change: StagedChange::Deleted,
    }
}

fn next(link: &str) -> PageLink {
    PageLink::Next(link.to_owned())
}

fn delta(link: &str) -> PageLink {
    PageLink::Delta(link.to_owned())
}

/// The whole staged set, walked `page` entries at a time.
fn walk(store: &impl Queries, page: usize) -> Vec<Staged> {
    let mut out = Vec::new();
    let mut cursor = None;
    loop {
        let p = store
            .staged_entries(&mount_id(), cursor.as_ref(), limit(page))
            .unwrap();
        assert!(p.entries.len() <= page, "a page never exceeds its limit");
        if p.next.is_some() {
            assert!(!p.entries.is_empty(), "no empty page while more remain");
        }
        out.extend(p.entries);
        match p.next {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
}

fn running(fx: &Fixture) -> Store {
    let store = fx.open_with_mount();
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
        .unwrap();
    store
}

/// Every row of `items`, `hydrated_ranges` and `local_id_allocator`, through a second
/// connection.
fn item_tables(fx: &Fixture) -> Vec<Vec<rusqlite::types::Value>> {
    let conn = raw(&fx.path());
    let mut rows = Vec::new();
    for sql in [
        "SELECT * FROM items ORDER BY drive_id, item_id",
        "SELECT * FROM hydrated_ranges ORDER BY drive_id, item_id, range_start",
        "SELECT * FROM local_id_allocator",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let mut q = stmt.query([]).unwrap();
        while let Some(row) = q.next().unwrap() {
            rows.push((0..n).map(|i| row.get(i).unwrap()).collect());
        }
    }
    rows
}

fn links(store: &Store) -> (Option<String>, Option<String>, Option<String>) {
    let m = store.mount(&mount_id()).unwrap().unwrap();
    (m.delta_token, m.delta_next_link, m.delta_staged_link)
}

/// ADR-0022 R1 (WP-017 AC5): a page's entries and its next link are committed together;
/// a dropped transaction leaves neither, and a committed one leaves both, across a reopen.
#[test]
fn adr_0022_r1_a_page_and_its_next_link_commit_together_or_not_at_all() {
    let fx = Fixture::new();
    let mut store = running(&fx);
    {
        let tx = store.transaction().unwrap();
        tx.stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n1"))
            .unwrap();
        assert_eq!(tx.staged_count(&mount_id()).unwrap(), 1);
        // Dropped without commit.
    }
    assert_eq!(store.staged_count(&mount_id()).unwrap(), 0);
    assert_eq!(links(&store), (None, None, None));

    let tx = store.transaction().unwrap();
    tx.stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n1"))
        .unwrap();
    tx.commit().unwrap();
    drop(store);
    let store = fx.open();
    assert_eq!(store.staged_count(&mount_id()).unwrap(), 1);
    assert_eq!(links(&store), (None, Some("n1".to_owned()), None));
}

/// ADR-0022 R1: a page with an entry the table cannot hold is refused whole: none of its
/// entries is staged and the next link does not move.
#[test]
fn adr_0022_r1_a_refused_page_stages_nothing() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n1"))
        .unwrap();
    let bad_pages: [(Vec<StagedEntry>, &str); 4] = [
        (
            vec![present("b", ROOT, "b"), present("c", ROOT, "a/b")],
            "InvalidName",
        ),
        (
            vec![
                present("b", ROOT, "b"),
                StagedEntry {
                    is_root: true,
                    ..deleted("r")
                },
            ],
            "InvalidStagedEntry",
        ),
        (
            vec![StagedEntry {
                item_id: id("p"),
                is_root: false,
                change: StagedChange::Problem {
                    problem: StagedProblem::ParentCycle,
                    item: None,
                },
            }],
            "InvalidStagedEntry",
        ),
        (
            vec![StagedEntry {
                item_id: id("q"),
                is_root: false,
                change: StagedChange::Present(item(None, "q", StagedFacet::File)),
            }],
            "InvalidStagedEntry",
        ),
    ];
    for (page, expected) in bad_pages {
        let err = store
            .stage_delta_page(&mount_id(), &page, &next("n2"))
            .unwrap_err();
        assert!(format!("{err:?}").starts_with(expected), "{err:?}");
        assert_eq!(store.staged_count(&mount_id()).unwrap(), 1);
        assert_eq!(links(&store).1.as_deref(), Some("n1"));
    }
}

/// ADR-0022 R2 (WP-017 AC5): staging the same item twice in a run, on two pages or twice
/// on one page, keeps one entry: the later one, with the later sequence number.
#[test]
fn adr_0022_r2_a_later_entry_for_an_item_replaces_the_earlier() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .stage_delta_page(
            &mount_id(),
            &[present("a", ROOT, "first"), present("b", ROOT, "b")],
            &next("n1"),
        )
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[
                present("a", ROOT, "second"),
                deleted("b"),
                present("b", ROOT, "back"),
            ],
            &next("n2"),
        )
        .unwrap();
    store
        .stage_delta_page(&mount_id(), &[deleted("a")], &delta("d"))
        .unwrap();
    assert_eq!(store.staged_count(&mount_id()).unwrap(), 2);
    let all = walk(&store, 10);
    let by_id: BTreeMap<&str, &Staged> =
        all.iter().map(|s| (s.entry.item_id.as_str(), s)).collect();
    assert_eq!(by_id["a"].entry.change, StagedChange::Deleted);
    assert_eq!(
        by_id["b"].entry.change,
        StagedChange::Present(item(Some(ROOT), "back", StagedFacet::File))
    );
    assert!(
        by_id["a"].seq > by_id["b"].seq,
        "a's last occurrence came last"
    );
}

/// ADR-0022 R3 (WP-017 AC5): staging changes no item row, no range and no local ID, even
/// for entries that describe stored items, delete them or are problems, and walking and
/// listing the complete set change nothing either.
#[test]
fn adr_0022_r3_staging_changes_no_item_row() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .insert_item(&folder("docs", Some(ROOT), "docs"))
        .unwrap();
    store
        .insert_item(&file("f", Some("docs"), "f", 100))
        .unwrap();
    store
        .record_hydrated_range(&key("f"), mydrive_store::ByteRange::new(0, 50).unwrap())
        .unwrap();
    let before = item_tables(&fx);
    store
        .stage_delta_page(
            &mount_id(),
            &[
                present("f", ROOT, "renamed"),
                deleted("docs"),
                present("new", "docs", "new"),
            ],
            &next("n1"),
        )
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[StagedEntry {
                item_id: id("odd"),
                is_root: false,
                change: StagedChange::Problem {
                    problem: StagedProblem::Reported("no name".to_owned()),
                    item: None,
                },
            }],
            &delta("d"),
        )
        .unwrap();
    assert_eq!(item_tables(&fx), before);
    walk(&store, 1);
    store.unstaged_items(&mount_id(), None, limit(10)).unwrap();
    assert_eq!(item_tables(&fx), before);
}

/// ADR-0022 R4 (WP-017 AC6): finishing a run sets the live token to the staged delta link,
/// clears the run state and removes the staged rows, together. Beginning a run and
/// staging pages never change the live token, and finishing is refused until the delta
/// link is staged.
#[test]
fn adr_0022_r4_only_finishing_a_complete_run_changes_the_live_token() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    finish_run_with_token(&store, &mount_id(), "live-1");
    assert_eq!(links(&store), (Some("live-1".to_owned()), None, None));

    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Incremental)
        .unwrap();
    assert_eq!(links(&store).0.as_deref(), Some("live-1"));
    let err = store.finish_delta_run(&mount_id()).unwrap_err();
    assert!(
        matches!(err, StoreError::DeltaRunNotComplete { .. }),
        "{err}"
    );
    store
        .stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n1"))
        .unwrap();
    assert_eq!(links(&store).0.as_deref(), Some("live-1"));
    let err = store.finish_delta_run(&mount_id()).unwrap_err();
    assert!(
        matches!(err, StoreError::DeltaRunNotComplete { .. }),
        "{err}"
    );
    store
        .stage_delta_page(&mount_id(), &[present("b", ROOT, "b")], &delta("live-2"))
        .unwrap();
    assert_eq!(
        links(&store),
        (Some("live-1".to_owned()), None, Some("live-2".to_owned()))
    );

    assert_eq!(store.finish_delta_run(&mount_id()).unwrap(), 2);
    let m = store.mount(&mount_id()).unwrap().unwrap();
    assert_eq!(
        (
            m.delta_token.as_deref(),
            m.delta_run,
            m.delta_next_link,
            m.delta_staged_link
        ),
        (Some("live-2"), None, None, None)
    );
    assert_eq!(store.staged_count(&mount_id()).unwrap(), 0);
    let err = store.finish_delta_run(&mount_id()).unwrap_err();
    assert!(matches!(err, StoreError::NoDeltaRun { .. }), "{err}");
}

/// The overview's ruling: a run that restarts from the beginning discards its staged set
/// first; the live token is untouched.
#[test]
fn beginning_a_run_discards_the_previous_staged_set() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    finish_run_with_token(&store, &mount_id(), "live");
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Incremental)
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[present("a", ROOT, "a"), present("b", ROOT, "b")],
            &delta("d"),
        )
        .unwrap();
    let discarded = store
        .begin_delta_run(&mount_id(), DeltaRunKind::ResyncApply)
        .unwrap();
    assert_eq!(discarded, 2);
    let m = store.mount(&mount_id()).unwrap().unwrap();
    assert_eq!(
        (m.delta_token.as_deref(), m.delta_run, m.delta_staged_link),
        (Some("live"), Some(DeltaRunKind::ResyncApply), None)
    );
    assert_eq!(store.staged_count(&mount_id()).unwrap(), 0);
}

/// Position of each item in the walk.
fn positions(all: &[Staged]) -> BTreeMap<String, usize> {
    all.iter()
        .enumerate()
        .map(|(i, s)| (s.entry.item_id.as_str().to_owned(), i))
        .collect()
}

fn problem_of(all: &[Staged], item: &str) -> Option<StagedProblem> {
    all.iter()
        .find(|s| s.entry.item_id.as_str() == item)
        .and_then(|s| match &s.entry.change {
            StagedChange::Problem { problem, .. } => Some(problem.clone()),
            _ => None,
        })
}

/// WP-017 "walk the staged set parents first": staged children-first over several pages,
/// the walk puts the drive root in the first level, every entry after its staged parent, and the
/// deletes last; paging by any size returns every entry exactly once.
#[test]
fn the_staged_set_is_walked_parents_first_and_deletes_last() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store
        .insert_item(&folder("old", Some(ROOT), "old"))
        .unwrap();
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
        .unwrap();
    // Deepest first, across pages, with deletes mixed in.
    store
        .stage_delta_page(
            &mount_id(),
            &[
                present("f3", "d3", "f3"),
                deleted("gone1"),
                dir("d3", "d2"),
                present("f2", "d2", "f2"),
            ],
            &next("n1"),
        )
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[
                dir("d2", "d1"),
                deleted("gone2"),
                present("in-old", "old", "x"),
            ],
            &next("n2"),
        )
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[dir("d1", ROOT), drive_root(ROOT)],
            &delta("d"),
        )
        .unwrap();

    for page in [1, 2, 3, 100] {
        let all = walk(&store, page);
        assert_eq!(all.len(), 9, "page size {page}");
        let ids: BTreeSet<&str> = all.iter().map(|s| s.entry.item_id.as_str()).collect();
        assert_eq!(ids.len(), 9, "each entry once");
        let at = positions(&all);
        // The drive root has no parent: it is in the first level, before its children.
        assert!(at[ROOT] <= 1, "root and in-old make the first level");
        for (child, parent) in [
            ("d1", ROOT),
            ("d2", "d1"),
            ("d3", "d2"),
            ("f2", "d2"),
            ("f3", "d3"),
        ] {
            assert!(at[parent] < at[child], "{parent} before {child}");
        }
        let first_delete = at["gone1"].min(at["gone2"]);
        assert!(
            all[first_delete..]
                .iter()
                .all(|s| s.entry.change == StagedChange::Deleted)
        );
        assert_eq!(first_delete, 7);
        // Nothing usable was marked: the parent of `in-old` is stored.
        assert!(
            all.iter()
                .all(|s| !matches!(s.entry.change, StagedChange::Problem { .. }))
        );
    }
}

/// WP-017, lead's point: a parent cycle inside the staged set (two entries that are each
/// other's parent, an entry that is its own parent, and one hanging below a cycle) does
/// not loop and is not dropped: each such entry is walked once, marked `ParentCycle`, after
/// every other entry that is not a delete and before the deletes. Its description is kept,
/// for the fetch that resolves it.
#[test]
fn a_parent_cycle_in_the_staged_set_is_walked_once_as_a_problem() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .stage_delta_page(
            &mount_id(),
            &[
                dir("a", "b"),
                dir("b", "a"),
                dir("self", "self"),
                present("below", "a", "below"),
                present("fine", ROOT, "fine"),
                deleted("gone"),
            ],
            &delta("d"),
        )
        .unwrap();
    let all = walk(&store, 2);
    assert_eq!(all.len(), 6);
    let at = positions(&all);
    for cyclic in ["a", "b", "self", "below"] {
        assert_eq!(
            problem_of(&all, cyclic),
            Some(StagedProblem::ParentCycle),
            "{cyclic}"
        );
        assert!(
            at["fine"] < at[cyclic] && at[cyclic] < at["gone"],
            "{cyclic}"
        );
    }
    let kept = all
        .iter()
        .find(|s| s.entry.item_id.as_str() == "below")
        .unwrap();
    assert_eq!(
        kept.entry.change,
        StagedChange::Problem {
            problem: StagedProblem::ParentCycle,
            item: Some(item(Some("a"), "below", StagedFacet::File)),
        }
    );
    assert_eq!(problem_of(&all, "fine"), None);
}

/// WP-017, lead's point: a staged child whose parent is neither staged nor stored is
/// walked and marked `MissingParent`; one whose parent is staged as deleted is marked
/// `ParentDeleted`. The mount's own root is exempt from the first, since its parent may lie
/// outside the mount. A problem the Graph client reported is kept as it was.
#[test]
fn unusable_parents_surface_as_problems() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .insert_item(&folder("stored", Some(ROOT), "stored"))
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[
                present("orphan", "nowhere", "orphan"),
                present("ok", "stored", "ok"),
                deleted("dead"),
                present("under-dead", "dead", "u"),
                dir(ROOT, "outside-the-mount"),
                StagedEntry {
                    item_id: id("reported"),
                    is_root: false,
                    change: StagedChange::Problem {
                        problem: StagedProblem::Reported("malformed entry".to_owned()),
                        item: None,
                    },
                },
            ],
            &delta("d"),
        )
        .unwrap();
    let all = walk(&store, 10);
    assert_eq!(all.len(), 6);
    assert_eq!(
        problem_of(&all, "orphan"),
        Some(StagedProblem::MissingParent)
    );
    assert_eq!(
        problem_of(&all, "under-dead"),
        Some(StagedProblem::ParentDeleted)
    );
    assert_eq!(problem_of(&all, "ok"), None);
    assert_eq!(problem_of(&all, ROOT), None);
    assert_eq!(
        problem_of(&all, "reported"),
        Some(StagedProblem::Reported("malformed entry".to_owned()))
    );
}

/// WP-017, lead's point (invariants are table constraints): staged rows exist only for
/// the run in progress, enforced by the table, not only by the operations. Whatever
/// writes the database, a staged row needs its mount to have that run in progress; the
/// run cannot end or change kind while its staged rows remain; removing the mount removes
/// them.
#[test]
fn staged_rows_belong_to_the_run_in_progress_by_a_foreign_key() {
    let fx = Fixture::new();
    let store = running(&fx);
    store
        .stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n"))
        .unwrap();
    drop(store);
    let conn = raw(&fx.path());
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    let refused = |sql: &str| {
        let err = conn.execute_batch(sql).unwrap_err().to_string();
        assert!(err.contains("FOREIGN KEY constraint failed"), "{sql}: {err}");
    };
    let m = mount_id();
    let m = m.as_str();
    // A row for a run the mount does not have.
    refused(&format!(
        "INSERT INTO delta_staged (mount_id, run, item_id, seq, deleted, is_root) \
         VALUES ('{m}', 'incremental', 'x', 99, 1, 0)"
    ));
    // Ending the run, or changing its kind, with rows still staged.
    refused(&format!(
        "UPDATE mounts SET delta_run = NULL, delta_next_link = NULL WHERE mount_id = '{m}'"
    ));
    refused(&format!(
        "UPDATE mounts SET delta_run = 'resync-apply' WHERE mount_id = '{m}'"
    ));
    let staged: i64 = conn
        .query_row("SELECT COUNT(*) FROM delta_staged", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 1);
    // Removing the mount (after its items, which the store does first) cascades.
    conn.execute_batch(&format!(
        "DELETE FROM hydrated_ranges; DELETE FROM items; DELETE FROM mounts WHERE mount_id = '{m}'"
    ))
    .unwrap();
    let staged: i64 = conn
        .query_row("SELECT COUNT(*) FROM delta_staged", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0);
}

/// The walk, the anti-join and finishing need a complete staged set, and a run.
#[test]
fn walking_listing_and_finishing_need_a_complete_run() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    let no_run = |e: StoreError| matches!(e, StoreError::NoDeltaRun { .. });
    assert!(no_run(
        store
            .staged_entries(&mount_id(), None, limit(1))
            .unwrap_err()
    ));
    assert!(no_run(
        store
            .unstaged_items(&mount_id(), None, limit(1))
            .unwrap_err()
    ));
    assert!(no_run(store.finish_delta_run(&mount_id()).unwrap_err()));
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::Initial)
        .unwrap();
    store
        .stage_delta_page(&mount_id(), &[present("a", ROOT, "a")], &next("n"))
        .unwrap();
    let partial = |e: StoreError| matches!(e, StoreError::DeltaRunNotComplete { .. });
    assert!(partial(
        store
            .staged_entries(&mount_id(), None, limit(1))
            .unwrap_err()
    ));
    assert!(partial(
        store
            .unstaged_items(&mount_id(), None, limit(1))
            .unwrap_err()
    ));
    assert!(partial(store.finish_delta_run(&mount_id()).unwrap_err()));
}

/// F05's resync anti-join: the items of the mount's drive that the complete staged set
/// does not contain, paged by item ID; an item staged as deleted is contained; another
/// drive's items are never listed.
#[test]
fn unstaged_items_is_the_resync_anti_join() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for n in 0..7 {
        store
            .insert_item(&file(&format!("i{n}"), Some(ROOT), &format!("i{n}"), 1))
            .unwrap();
    }
    let mut other = file("i5", None, "elsewhere", 1);
    other.key = ItemKey::new(DriveId::new("drive-b").unwrap(), id("i5"));
    store.insert_item(&other).unwrap();
    finish_run_with_token(&store, &mount_id(), "live");
    store
        .begin_delta_run(&mount_id(), DeltaRunKind::ResyncApply)
        .unwrap();
    store
        .stage_delta_page(
            &mount_id(),
            &[drive_root(ROOT), present("i1", ROOT, "i1"), deleted("i3")],
            &delta("d"),
        )
        .unwrap();
    let mut seen = Vec::new();
    let mut after: Option<ItemId> = None;
    loop {
        let page = store
            .unstaged_items(&mount_id(), after.as_ref(), limit(2))
            .unwrap();
        assert!(page.items.len() <= 2);
        seen.extend(page.items.iter().map(|i| i.key.clone()));
        match page.next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    let expected: Vec<ItemKey> = ["i0", "i2", "i4", "i5", "i6"]
        .iter()
        .map(|s| key(s))
        .collect();
    assert_eq!(seen, expected);
}

/// WP-017, lead's point: walking a staged set of 500,000 entries reads it one page at a
/// time (every page at most its limit, so memory follows the page size, not the set), and
/// delivers every entry exactly once with each parent before its children.
#[test]
fn a_500000_entry_staged_set_is_walked_in_bounded_pages() {
    const FOLDERS: usize = 100;
    const FILES_PER_FOLDER: usize = 4_999;
    const PAGE: usize = 1_000;
    let fx = Fixture::new();
    let mut store = running(&fx);
    // Staged children first, as delta may deliver them; one transaction for the test's
    // speed (each page is still its own savepoint).
    {
        let tx = store.transaction().unwrap();
        let mut page = Vec::with_capacity(PAGE);
        let mut n = 0;
        for f in 0..FOLDERS {
            for i in 0..FILES_PER_FOLDER {
                page.push(present(
                    &format!("f{f}-{i}"),
                    &format!("d{f}"),
                    &format!("f{i}"),
                ));
                if page.len() == PAGE {
                    tx.stage_delta_page(&mount_id(), &page, &next(&format!("n{n}")))
                        .unwrap();
                    n += 1;
                    page.clear();
                }
            }
        }
        for f in 0..FOLDERS {
            page.push(dir(&format!("d{f}"), ROOT));
        }
        page.push(drive_root(ROOT));
        tx.stage_delta_page(&mount_id(), &page, &delta("d"))
            .unwrap();
        tx.commit().unwrap();
    }
    let total = FOLDERS * FILES_PER_FOLDER + FOLDERS + 1;
    assert_eq!(total, 500_001);
    assert_eq!(
        store.staged_count(&mount_id()).unwrap(),
        u64::try_from(total).unwrap()
    );

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let p = store
            .staged_entries(&mount_id(), cursor.as_ref(), limit(PAGE))
            .unwrap();
        pages += 1;
        assert!(p.entries.len() <= PAGE);
        for s in &p.entries {
            if let StagedChange::Present(it) = &s.entry.change {
                if let Some(parent) = &it.parent_item_id {
                    assert!(
                        seen.contains(parent.as_str()),
                        "{} before its parent {parent}",
                        s.entry.item_id
                    );
                }
            } else {
                panic!("unexpected {s:?}");
            }
            assert!(seen.insert(s.entry.item_id.as_str().to_owned()));
        }
        match p.next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(seen.len(), total);
    assert_eq!(pages, total.div_ceil(PAGE));
}
