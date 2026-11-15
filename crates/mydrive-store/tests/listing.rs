//! Listing a folder by name: `children_after`, `child_by_name`, `item_count`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{Fixture, ROOT, file, folder, key, limit};
use mydrive_core::ids::{DriveId, ItemId, ItemKey};
use mydrive_store::{DeleteMode, PendingChanges, Queries, Store, StoreError};

/// Names chosen to trip every wrong ordering: prefixes of each other, case, non-ASCII
/// (whose UTF-8 sorts after every ASCII byte), a precomposed and a decomposed é, and
/// punctuation that sorts before letters.
const TRICKY: [&str; 14] = [
    "a",
    "a ",
    "a-",
    "aa",
    "ab",
    "a\u{e9}",
    "ae\u{301}",
    "B",
    "b",
    "z",
    "\u{e9}",
    "\u{65e5}",
    "Zebra",
    "~",
];

fn names(items: &[mydrive_store::Item]) -> Vec<String> {
    items.iter().map(|i| i.name.clone()).collect()
}

fn byte_sorted(names: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
    v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    v
}

fn with_tricky_children(fx: &Fixture) -> Store {
    let store = fx.open_with_mount();
    for (n, name) in TRICKY.iter().enumerate() {
        store
            .insert_item(&file(&format!("t{n}"), Some(ROOT), name, 1))
            .unwrap();
    }
    store
}

/// WP-017 AC3: `children_after` returns names strictly after the given one, in byte order
/// of the UTF-8 (not locale, case-folded or normalised order), never fooled by names that
/// are prefixes of each other.
#[test]
fn ac3_children_after_is_strictly_after_in_byte_order() {
    let fx = Fixture::new();
    let store = with_tricky_children(&fx);
    let expected = byte_sorted(&TRICKY);
    let page = store.children_after(&key(ROOT), None, limit(100)).unwrap();
    assert_eq!(names(&page.items), expected);
    assert!(!page.more);

    for (i, after) in expected.iter().enumerate() {
        let page = store
            .children_after(&key(ROOT), Some(after), limit(100))
            .unwrap();
        assert_eq!(names(&page.items), expected[i + 1..], "after {after:?}");
    }
    // A name that is not stored, between two that are.
    let page = store
        .children_after(&key(ROOT), Some("a\u{0}"), limit(100))
        .unwrap();
    assert_eq!(names(&page.items)[0], "a ");
    // Paging one at a time visits every name once, in order.
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .children_after(&key(ROOT), after.as_deref(), limit(1))
            .unwrap();
        seen.extend(names(&page.items));
        after = page.items.last().map(|i| i.name.clone());
        if !page.more {
            break;
        }
    }
    assert_eq!(seen, expected);
}

/// WP-017 AC3: two children with the same name (possible only transiently) are never
/// split across pages, so paging by name skips neither.
#[test]
fn ac3_equal_names_are_never_split_across_pages() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for (id, name) in [
        ("1", "a"),
        ("2", "same"),
        ("3", "same"),
        ("4", "same"),
        ("5", "z"),
    ] {
        store.insert_item(&file(id, Some(ROOT), name, 1)).unwrap();
    }
    let page = store.children_after(&key(ROOT), None, limit(2)).unwrap();
    assert_eq!(names(&page.items), ["a", "same", "same", "same"]);
    assert!(page.more);
    let page = store
        .children_after(&key(ROOT), Some("same"), limit(2))
        .unwrap();
    assert_eq!(names(&page.items), ["z"]);
    assert!(!page.more);
    // A page that ends exactly at the end of a run reports no more when none follow.
    let page = store
        .children_after(&key(ROOT), Some("a"), limit(1))
        .unwrap();
    assert_eq!(names(&page.items), ["same", "same", "same"]);
    assert!(page.more);
}

/// A small deterministic generator, so the test needs no randomness dependency and
/// repeats exactly.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap() % n
    }

    fn name(&mut self) -> String {
        let stem = ["", "a", "aa", "B", "\u{e9}", "z", "\u{65e5}"][self.below(7)];
        format!("{stem}{}", self.next() % 100_000)
    }
}

/// WP-017 AC3, ADR-0020 R1 and R2: a 256-entry paging loop over 50,000 children delivers
/// each child that exists unchanged throughout exactly once, while other children are
/// inserted, renamed and removed between pages; no page is empty while more remain.
#[test]
fn adr_0020_r1_r2_paging_50000_children_under_change_delivers_each_unchanged_child_once() {
    const CHILDREN: usize = 50_000;
    let fx = Fixture::new();
    let mut store = fx.open_with_mount();
    let mut rng = Lcg(17);
    // Every child now, by item ID, with its name.
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    // One transaction for the whole test: each page reads the changes made before it, and
    // the test avoids two fsyncs per page.
    let tx = store.transaction().unwrap();
    for n in 0..CHILDREN {
        let id = format!("c{n}");
        let name = format!("{}-{n}", rng.name());
        tx.insert_item(&file(&id, Some(ROOT), &name, 1)).unwrap();
        current.insert(id, name);
    }
    let mut unchanged: BTreeSet<String> = current.keys().cloned().collect();
    let mut delivered: BTreeMap<String, usize> = BTreeMap::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    let mut added = 0;
    loop {
        let page = tx
            .children_after(&key(ROOT), after.as_deref(), limit(256))
            .unwrap();
        pages += 1;
        if page.more {
            assert!(!page.items.is_empty(), "an empty page while more remain");
        }
        for item in &page.items {
            *delivered
                .entry(item.key.item_id.as_str().to_owned())
                .or_default() += 1;
        }
        after = page.items.last().map(|i| i.name.clone());
        if !page.more {
            break;
        }
        // Between pages: two inserts, a rename and a removal, anywhere in the order.
        for _ in 0..2 {
            let id = format!("new{added}");
            added += 1;
            let name = rng.name();
            tx.insert_item(&file(&id, Some(ROOT), &name, 1)).unwrap();
            current.insert(id, name);
        }
        let ids: Vec<&String> = current.keys().collect();
        let renamed = ids[rng.below(ids.len())].clone();
        let removed = ids[rng.below(ids.len())].clone();
        let new_name = rng.name();
        tx.move_item_local(&key(&renamed), Some(&ItemId::new(ROOT).unwrap()), &new_name)
            .unwrap();
        current.insert(renamed.clone(), new_name);
        unchanged.remove(&renamed);
        if removed != renamed {
            tx.delete_item(&key(&removed), DeleteMode::ItemOnly, PendingChanges::Refuse)
                .unwrap();
            current.remove(&removed);
            unchanged.remove(&removed);
        }
    }
    assert!(pages > CHILDREN / 256, "{pages} pages");
    assert!(unchanged.len() > CHILDREN - 2 * pages);
    for id in &unchanged {
        assert_eq!(delivered.get(id), Some(&1), "child {id} delivered once");
    }
}

/// WP-017 AC4: `child_by_name` is exact and byte-wise: no case folding, no
/// normalisation, no trimming, no prefix match.
#[test]
fn ac4_child_by_name_is_exact_and_byte_wise() {
    let fx = Fixture::new();
    let store = with_tricky_children(&fx);
    for (n, name) in TRICKY.iter().enumerate() {
        let found = store.child_by_name(&key(ROOT), name).unwrap().unwrap();
        assert_eq!(found.key, key(&format!("t{n}")), "{name:?}");
    }
    for absent in [
        "A",
        "a  ",
        "Z",
        "\u{c9}",
        "aé ",
        "",
        "zebra",
        "a\u{e9}\u{301}",
    ] {
        assert_eq!(
            store.child_by_name(&key(ROOT), absent).unwrap(),
            None,
            "{absent:?}"
        );
    }
    // The precomposed and decomposed forms are different children.
    let pre = store.child_by_name(&key(ROOT), "a\u{e9}").unwrap().unwrap();
    let de = store
        .child_by_name(&key(ROOT), "ae\u{301}")
        .unwrap()
        .unwrap();
    assert_ne!(pre.key, de.key);
    // Only direct children of that parent.
    store
        .insert_item(&folder("sub", Some(ROOT), "sub"))
        .unwrap();
    store
        .insert_item(&file("deep", Some("sub"), "only-below", 1))
        .unwrap();
    assert_eq!(store.child_by_name(&key(ROOT), "only-below").unwrap(), None);
    assert!(
        store
            .child_by_name(&key("sub"), "only-below")
            .unwrap()
            .is_some()
    );
}

/// `child_by_name` refuses to pick one of two children with the same name.
#[test]
fn child_by_name_refuses_an_ambiguous_name() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    store.insert_item(&file("x", Some(ROOT), "dup", 1)).unwrap();
    store.insert_item(&file("y", Some(ROOT), "dup", 1)).unwrap();
    let err = store.child_by_name(&key(ROOT), "dup").unwrap_err();
    assert!(
        matches!(err, StoreError::DuplicateName { count: 2, .. }),
        "{err}"
    );
}

/// WP-017 "Listing": the item count is per drive.
#[test]
fn item_count_is_per_drive() {
    let fx = Fixture::new();
    let store = fx.open_with_mount();
    for n in 0..4 {
        store
            .insert_item(&file(&format!("f{n}"), Some(ROOT), &format!("f{n}"), 1))
            .unwrap();
    }
    let mut other = file("o", None, "o", 1);
    other.key = ItemKey::new(DriveId::new("drive-b").unwrap(), ItemId::new("o").unwrap());
    store.insert_item(&other).unwrap();
    assert_eq!(
        store.item_count(&DriveId::new("drive-a").unwrap()).unwrap(),
        5
    );
    assert_eq!(
        store.item_count(&DriveId::new("drive-b").unwrap()).unwrap(),
        1
    );
    assert_eq!(
        store.item_count(&DriveId::new("drive-c").unwrap()).unwrap(),
        0
    );
}
