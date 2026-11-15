//! WP-017 AC1: a version 1 store, written by the version 1 code
//! (`tests/fixtures/v1/generate_v1_fixture.rs`), migrates to version 2 with every row and
//! range intact.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::raw;
use mydrive_core::ids::{DriveId, ItemId, ItemKey, MountId};
use mydrive_core::names::AccountKind;
use mydrive_core::state::{Hydration, Pin, Sync as SyncState};
use mydrive_store::{
    BlockedReason, DriveType, ItemKind, MountState, Queries, SCHEMA_VERSION, Store, Timestamp,
};
use rusqlite::types::Value;

type Rows = Vec<Vec<Value>>;

const V1_ACCOUNTS: &str = "SELECT account_id, tenant_id FROM accounts ORDER BY account_id";
const V1_MOUNTS: &str = "SELECT mount_id, account_id, drive_id, root_item_id, mount_point, \
     delta_token, read_only FROM mounts ORDER BY mount_id";
const V1_ITEMS: &str = "SELECT drive_id, item_id, parent_item_id, name, kind, size, \
     created_at, modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, \
     last_error, is_provisional, local_dirty, local_generation, remote_changed, remote_etag, \
     remote_ctag, remote_hash, remote_size FROM items ORDER BY drive_id, item_id";
const V1_RANGES: &str = "SELECT drive_id, item_id, range_start, range_end FROM hydrated_ranges \
     ORDER BY drive_id, item_id, range_start";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1/state-v1.db")
}

/// A copy of the fixture in a temporary directory; the committed file is never opened.
fn copy(dir: &Path) -> PathBuf {
    let path = dir.join("state.db");
    std::fs::copy(fixture(), &path).unwrap();
    path
}

fn rows(conn: &rusqlite::Connection, sql: &str) -> Rows {
    let mut stmt = conn.prepare(sql).unwrap();
    let n = stmt.column_count();
    let mut q = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = q.next().unwrap() {
        out.push((0..n).map(|i| row.get::<_, Value>(i).unwrap()).collect());
    }
    out
}

fn column(rows: &Rows, i: usize) -> impl Iterator<Item = &Value> {
    rows.iter().map(move |r| &r[i])
}

fn text(v: &Value) -> &str {
    match v {
        Value::Text(s) => s,
        other => panic!("not text: {other:?}"),
    }
}

/// The fixture is what the test claims: version 1, built with every state the version 1
/// code can produce. Guards against a regenerated fixture that quietly covers less.
#[test]
fn the_fixture_is_a_version_1_store_covering_every_state() {
    let conn = rusqlite::Connection::open_with_flags(
        fixture(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    let items = rows(&conn, V1_ITEMS);
    let set =
        |i: usize| -> BTreeSet<String> { column(&items, i).map(|v| text(v).to_owned()).collect() };
    let all = |forms: &[&str]| {
        forms
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(set(12), all(Hydration::WIRE_FORMS));
    assert_eq!(set(13), all(Pin::WIRE_FORMS));
    assert_eq!(set(14), all(SyncState::WIRE_FORMS));
    let flag = |i: usize| {
        column(&items, i)
            .filter(|v| **v == Value::Integer(1))
            .count()
    };
    assert!(flag(16) >= 2, "provisional items");
    assert!(flag(17) >= 7, "dirty items");
    assert!(flag(19) >= 2, "remote_changed, dirty and clean");
    assert!(
        column(&items, 23).any(|v| *v != Value::Null),
        "a recorded remote identity"
    );
    assert!(
        column(&items, 18).any(|v| matches!(v, Value::Integer(g) if *g > 1)),
        "several generations"
    );
    assert!(rows(&conn, V1_RANGES).len() >= 20, "hydrated ranges");
    let mounts = rows(&conn, V1_MOUNTS);
    assert_eq!(
        column(&mounts, 5).filter(|v| **v != Value::Null).count(),
        2,
        "tokens"
    );
}

/// WP-017 AC1: every version 1 row and range is intact, bit for bit, after the
/// migration (including the durable facts `local_dirty`, `local_generation`,
/// `remote_changed`, the recorded remote identity and the delta tokens); every item has a
/// distinct `local_id`; `PRAGMA foreign_key_check` is empty.
#[test]
fn ac1_version_1_store_migrates_with_every_row_and_range_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy(dir.path());
    let (accounts, mounts, items, ranges) = {
        let conn = raw(&path);
        (
            rows(&conn, V1_ACCOUNTS),
            rows(&conn, V1_MOUNTS),
            rows(&conn, V1_ITEMS),
            rows(&conn, V1_RANGES),
        )
    };

    let store = Store::open(&path).unwrap();
    drop(store);
    let conn = raw(&path);
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 2);

    assert_eq!(rows(&conn, V1_ACCOUNTS), accounts);
    assert_eq!(rows(&conn, V1_MOUNTS), mounts);
    assert_eq!(rows(&conn, V1_ITEMS), items);
    assert_eq!(rows(&conn, V1_RANGES), ranges);

    let violations = rows(&conn, "PRAGMA foreign_key_check");
    assert!(violations.is_empty(), "{violations:?}");
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");

    // Every item holds a distinct local ID, at least 1, and the allocator is past them all.
    let ids = rows(&conn, "SELECT local_id FROM items");
    let distinct: BTreeSet<i64> = ids
        .iter()
        .map(|r| match r[0] {
            Value::Integer(i) => i,
            ref other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(distinct.len(), items.len());
    assert!(distinct.iter().all(|&i| i >= 1));
    let next: i64 = conn
        .query_row("SELECT next_local_id FROM local_id_allocator", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(next > *distinct.iter().max().unwrap());
    let staged: i64 = conn
        .query_row("SELECT COUNT(*) FROM delta_staged", [], |r| r.get(0))
        .unwrap();
    assert_eq!(staged, 0);
}

/// WP-017 AC1, read through the API: every migrated item reads back, the new facts take
/// the documented values, and the migrated store works.
#[test]
fn ac1_migrated_rows_read_back_through_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy(dir.path());
    let store = Store::open(&path).unwrap();

    // Accounts: kind from version 1's meaning of tenant_id; nothing else known.
    let accounts = store.accounts().unwrap();
    let kinds: Vec<(&str, AccountKind)> = accounts
        .iter()
        .map(|a| (a.account_id.as_str(), a.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("acct-personal", AccountKind::Personal),
            ("acct-work", AccountKind::Work)
        ]
    );
    assert!(
        accounts
            .iter()
            .all(|a| !a.needs_sign_in && !a.refuses_hash_and_percent && a.username.is_none())
    );

    // Mounts: tokens unchanged, the free-text block becomes a code with the text kept as
    // its message, drive type known only for the personal account, no run in progress.
    let personal = store
        .mount(&MountId::new("personal").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        personal.delta_token.as_deref(),
        Some("https://graph.example/delta?token=personal-v1")
    );
    assert_eq!(personal.drive_type, Some(DriveType::Personal));
    assert_eq!(personal.state(), MountState::Active);
    let work = store
        .mount(&MountId::new("work-docs").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(work.delta_token.as_deref(), Some("work-token-v1"));
    assert_eq!(work.blocked_reason, Some(BlockedReason::OrganisationPolicy));
    assert_eq!(
        work.blocked_message.as_deref(),
        Some("unmanaged device policy")
    );
    assert!(work.read_only && work.read_only_reason.is_none());
    assert_eq!(work.drive_type, None);
    assert_eq!(work.state(), MountState::Blocked);
    let new = store
        .mount(&MountId::new("work-new").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!((new.delta_token.as_deref(), new.delta_run), (None, None));
    assert_eq!(new.state(), MountState::Resyncing);
    assert!(!new.paused);

    // Every item reads through the API, and the dirty ones are all still dirty.
    let personal_drive = DriveId::new("0123456789abcdef").unwrap();
    let work_drive = DriveId::new("b!workdrive").unwrap();
    let total = store.item_count(&personal_drive).unwrap() + store.item_count(&work_drive).unwrap();
    assert_eq!(total, 39);
    let dirty = store.dirty_items().unwrap();
    assert_eq!(dirty.len(), 8);
    let rc = dirty
        .iter()
        .find(|i| i.key.item_id.as_str() == "dirty-remote-changed")
        .unwrap();
    assert!(rc.remote_changed && rc.last_remote.is_some());
    let mut ids = BTreeSet::new();
    for item in store.items_with_sync(SyncState::InSync).unwrap() {
        ids.insert(store.local_id(&item.key).unwrap());
    }
    assert!(!ids.is_empty());

    // The migrated store works: a new item gets a local ID no migrated row holds.
    let new_key = ItemKey::new(
        personal_drive.clone(),
        ItemId::new("after-migration").unwrap(),
    );
    let mut item = store
        .item(&ItemKey::new(
            personal_drive.clone(),
            ItemId::new("empty").unwrap(),
        ))
        .unwrap()
        .unwrap();
    item.key = new_key.clone();
    item.name = "after.txt".to_owned();
    let id = store.insert_item(&item).unwrap();
    let conn = raw(&path);
    let max: i64 = conn
        .query_row(
            "SELECT MAX(local_id) FROM items WHERE item_id <> 'after-migration'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(id.get() > u64::try_from(max).unwrap());
    assert_eq!(store.local_id(&new_key).unwrap(), id);
    assert_eq!(item.kind, ItemKind::File);
    assert_eq!(item.created, Timestamp::from_unix_seconds(1_700_000_000));
}

/// Opening the migrated store again applies nothing and changes nothing.
#[test]
fn ac1_reopening_the_migrated_store_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy(dir.path());
    drop(Store::open(&path).unwrap());
    let snapshot = |p: &Path| {
        let conn = raw(p);
        let mut all = Vec::new();
        for t in [
            "accounts",
            "mounts",
            "items",
            "hydrated_ranges",
            "local_id_allocator",
        ] {
            all.extend(rows(&conn, &format!("SELECT * FROM {t} ORDER BY 1, 2")));
        }
        all
    };
    let before = snapshot(&path);
    drop(Store::open(&path).unwrap());
    assert_eq!(snapshot(&path), before);
}
