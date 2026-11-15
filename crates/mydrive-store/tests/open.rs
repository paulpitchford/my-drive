//! Opening the store: connection settings, migrations and refusals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::{Fixture, kill_self, raw, run_child_until_killed};
use mydrive_store::{APPLICATION_ID, Queries, SCHEMA_VERSION, Store, StoreError};

/// ADR-0006 R2: the database is opened with `journal_mode=WAL`.
#[test]
fn adr_0006_r2_store_is_opened_in_wal_mode() {
    let fx = Fixture::new();
    let store = fx.open();
    drop(store);
    // The journal mode is persistent, so a fresh connection sees it.
    let mode: String = raw(&fx.path())
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

/// ADR-0006 R3 and WP-006 required test: create at version 0, migrate, verify schema.
#[test]
fn adr_0006_r3_migrating_from_version_0_yields_the_current_schema() {
    let fx = Fixture::new();
    {
        // An empty database at user_version 0, as a first daemon start finds it.
        let conn = raw(&fx.path());
        conn.execute_batch("VACUUM").unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0);
    }
    drop(fx.open());

    let conn = raw(&fx.path());
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(schema(&conn), expected_schema());

    let indexes: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'index' AND sql IS NOT NULL \
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        indexes,
        [
            "delta_staged_by_order",
            "delta_staged_by_parent",
            "items_by_dirty",
            "items_by_parent_name",
            "items_by_sync",
        ]
    );

    let range_fks: Vec<String> = conn
        .prepare(
            "SELECT \"table\" || '.' || \"from\" || '=' || \"to\" || ' update:' || on_update \
             || ' delete:' || on_delete FROM pragma_foreign_key_list('hydrated_ranges')",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        range_fks,
        [
            "items.drive_id=drive_id update:CASCADE delete:NO ACTION",
            "items.item_id=item_id update:CASCADE delete:NO ACTION",
        ]
    );

    let app_id: i64 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, APPLICATION_ID);

    // Lead ruling: no SQL CHECK on `kind`; the state columns keep theirs.
    let items_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'items'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!items_sql.contains("kind IN"), "{items_sql}");
    for column in ["hydration IN", "pin IN", "sync IN"] {
        assert!(items_sql.contains(column), "{column}");
    }
}

/// ADR-0006 R3: opening an already-current store applies nothing and keeps its data.
#[test]
fn adr_0006_r3_reopening_a_current_store_keeps_data() {
    let fx = Fixture::new();
    drop(fx.open_with_mount());
    let store = fx.open();
    assert!(store.item(&common::key(common::ROOT)).unwrap().is_some());
    assert_eq!(store.mounts().unwrap().len(), 1);
}

/// WP-006 AC6: a store with a higher schema version is refused and the file is unchanged.
#[test]
fn ac6_newer_schema_is_refused_without_modifying_the_file() {
    let fx = Fixture::new();
    {
        let conn = raw(&fx.path());
        conn.execute_batch(&format!("CREATE TABLE future (x INTEGER); PRAGMA user_version = 99; PRAGMA application_id = {APPLICATION_ID};"))
            .unwrap();
    }
    let before = fs::read(fx.path()).unwrap();
    let files_before = dir_listing(&fx);

    let err = Store::open(fx.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::SchemaTooNew { found: 99, supported, .. } if supported == SCHEMA_VERSION),
        "{err}"
    );

    assert_eq!(fs::read(fx.path()).unwrap(), before);
    assert_eq!(
        dir_listing(&fx),
        files_before,
        "no -wal or -shm file was created"
    );
    let mode: String = raw(&fx.path())
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete", "the journal mode was not switched to WAL");
}

/// WP-006 AC6, for a newer store already in WAL mode, as a later my-drive would leave it:
/// the main file is not checkpointed or otherwise written.
#[test]
fn ac6_newer_wal_schema_is_refused_without_modifying_the_file() {
    let fx = Fixture::new();
    {
        let conn = raw(&fx.path());
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch(&format!("CREATE TABLE future (x INTEGER); PRAGMA user_version = 7; PRAGMA application_id = {APPLICATION_ID};"))
            .unwrap();
    }
    let before = fs::read(fx.path()).unwrap();

    let err = Store::open(fx.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::SchemaTooNew { found: 7, .. }),
        "{err}"
    );
    assert_eq!(fs::read(fx.path()).unwrap(), before);
}

/// A non-empty SQLite file without a schema version is not ours: refused, unchanged.
#[test]
fn foreign_database_is_refused_without_modifying_it() {
    let fx = Fixture::new();
    {
        let conn = raw(&fx.path());
        conn.execute_batch("CREATE TABLE someone_elses (x INTEGER);")
            .unwrap();
    }
    let before = fs::read(fx.path()).unwrap();
    let err = Store::open(fx.path()).unwrap_err();
    assert!(matches!(err, StoreError::NotAStateStore { .. }), "{err}");
    assert_eq!(fs::read(fx.path()).unwrap(), before);
}

/// Lead ruling 5: a file that is not SQLite at all is `NotAStateStore`, names the file,
/// and is left byte-identical.
#[test]
fn non_sqlite_file_is_refused_without_modifying_it() {
    let fx = Fixture::new();
    let bytes = b"this is not a database, just some bytes........".repeat(100);
    fs::write(fx.path(), &bytes).unwrap();
    let files_before = dir_listing(&fx);
    let err = Store::open(fx.path()).unwrap_err();
    assert!(matches!(err, StoreError::NotAStateStore { .. }), "{err}");
    assert!(err.to_string().contains("state.db"), "{err}");
    assert_eq!(fs::read(fx.path()).unwrap(), bytes);
    assert_eq!(dir_listing(&fx), files_before);
}

/// Lead ruling 5: a SQLite file with another application ID is `NotAStateStore` and is
/// left byte-identical, even when its user_version looks like ours.
#[test]
fn wrong_application_id_is_refused_without_modifying_it() {
    let fx = Fixture::new();
    {
        let conn = raw(&fx.path());
        conn.execute_batch(
            "CREATE TABLE t (x INTEGER); PRAGMA application_id = 42; PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let before = fs::read(fx.path()).unwrap();
    let err = Store::open(fx.path()).unwrap_err();
    assert!(
        matches!(&err, StoreError::NotAStateStore { reason, .. } if reason.contains("0x2a")),
        "{err}"
    );
    assert_eq!(fs::read(fx.path()).unwrap(), before);
}

/// Set in the child of `ac6_newer_store_killed_with_wal_frames_is_left_byte_identical`.
const NEWER_CHILD_ENV: &str = "MYDRIVE_STORE_NEWER_SCHEMA_CHILD";

/// WP-006 AC6, exercising the read-only probe: a newer my-drive was killed with
/// uncheckpointed frames still in its WAL. Opening it with this build must leave the main
/// database file byte-identical. Without the probe, the read-write connection would
/// checkpoint the WAL into the main file when it closes.
#[test]
fn ac6_newer_store_killed_with_wal_frames_is_left_byte_identical() {
    if let Ok(db) = std::env::var(NEWER_CHILD_ENV) {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {};
             CREATE TABLE future (x TEXT);",
            SCHEMA_VERSION + 1
        ))
        .unwrap();
        for i in 0..200 {
            conn.execute("INSERT INTO future VALUES (?1)", [format!("{i:0>500}")])
                .unwrap();
        }
        kill_self();
    }

    let fx = Fixture::new();
    run_child_until_killed(
        "ac6_newer_store_killed_with_wal_frames_is_left_byte_identical",
        NEWER_CHILD_ENV,
        &fx.path().display().to_string(),
    );
    let wal = fx.dir.path().join("state.db-wal");
    assert!(
        fs::metadata(&wal).unwrap().len() > 0,
        "the WAL holds uncheckpointed frames"
    );
    let before = fs::read(fx.path()).unwrap();

    let err = Store::open(fx.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::SchemaTooNew { found, .. } if found == SCHEMA_VERSION + 1),
        "{err}"
    );
    assert_eq!(
        fs::read(fx.path()).unwrap(),
        before,
        "main file not checkpointed"
    );
    assert!(fs::metadata(&wal).unwrap().len() > 0, "WAL left in place");
}

/// Column name, declared type, NOT NULL and primary-key position for every table.
type Columns = Vec<(String, String, bool, i64)>;

fn schema(conn: &rusqlite::Connection) -> BTreeMap<String, Columns> {
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|t| {
            let cols = conn
                .prepare(
                    "SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid",
                )
                .unwrap()
                .query_map([&t], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            (t, cols)
        })
        .collect()
}

fn expected_schema() -> BTreeMap<String, Columns> {
    fn cols(spec: &[(&str, &str, bool, i64)]) -> Columns {
        spec.iter()
            .map(|(n, t, nn, pk)| ((*n).to_owned(), (*t).to_owned(), *nn, *pk))
            .collect()
    }
    BTreeMap::from([
        (
            "accounts".to_owned(),
            cols(&[
                ("account_id", "TEXT", true, 1),
                ("tenant_id", "TEXT", false, 0),
                ("kind", "TEXT", true, 0),
                ("username", "TEXT", false, 0),
                ("display_name", "TEXT", false, 0),
                ("org_name", "TEXT", false, 0),
                ("needs_sign_in", "INTEGER", true, 0),
                ("hash_percent_refused", "INTEGER", true, 0),
            ]),
        ),
        (
            "delta_staged".to_owned(),
            cols(&[
                ("mount_id", "TEXT", true, 1),
                ("item_id", "TEXT", true, 2),
                ("seq", "INTEGER", true, 0),
                ("deleted", "INTEGER", true, 0),
                ("problem", "TEXT", false, 0),
                ("problem_detail", "TEXT", false, 0),
                ("is_root", "INTEGER", true, 0),
                ("facet", "TEXT", false, 0),
                ("parent_item_id", "TEXT", false, 0),
                ("name", "TEXT", false, 0),
                ("size", "INTEGER", false, 0),
                ("created_at", "INTEGER", false, 0),
                ("modified_at", "INTEGER", false, 0),
                ("etag", "TEXT", false, 0),
                ("ctag", "TEXT", false, 0),
                ("server_hash", "TEXT", false, 0),
                ("apply_order", "INTEGER", false, 0),
            ]),
        ),
        (
            "hydrated_ranges".to_owned(),
            cols(&[
                ("drive_id", "TEXT", true, 1),
                ("item_id", "TEXT", true, 2),
                ("range_start", "INTEGER", true, 3),
                ("range_end", "INTEGER", true, 0),
            ]),
        ),
        (
            "items".to_owned(),
            cols(&[
                ("drive_id", "TEXT", true, 1),
                ("item_id", "TEXT", true, 2),
                ("parent_item_id", "TEXT", false, 0),
                ("name", "TEXT", true, 0),
                ("kind", "TEXT", true, 0),
                ("size", "INTEGER", true, 0),
                ("created_at", "INTEGER", true, 0),
                ("modified_at", "INTEGER", true, 0),
                ("etag", "TEXT", false, 0),
                ("ctag", "TEXT", false, 0),
                ("server_hash", "TEXT", false, 0),
                ("local_hash", "TEXT", false, 0),
                ("hydration", "TEXT", true, 0),
                ("pin", "TEXT", true, 0),
                ("sync", "TEXT", true, 0),
                ("last_error", "TEXT", false, 0),
                ("is_provisional", "INTEGER", true, 0),
                ("local_dirty", "INTEGER", true, 0),
                ("local_generation", "INTEGER", true, 0),
                ("remote_changed", "INTEGER", true, 0),
                ("remote_etag", "TEXT", false, 0),
                ("remote_ctag", "TEXT", false, 0),
                ("remote_hash", "TEXT", false, 0),
                ("remote_size", "INTEGER", false, 0),
                ("local_id", "INTEGER", true, 0),
            ]),
        ),
        (
            "local_id_allocator".to_owned(),
            cols(&[
                ("singleton", "INTEGER", true, 1),
                ("next_local_id", "INTEGER", true, 0),
            ]),
        ),
        (
            "mounts".to_owned(),
            cols(&[
                ("mount_id", "TEXT", true, 1),
                ("account_id", "TEXT", true, 0),
                ("drive_id", "TEXT", true, 0),
                ("root_item_id", "TEXT", true, 0),
                ("mount_point", "TEXT", true, 0),
                ("drive_type", "TEXT", false, 0),
                ("display_name", "TEXT", false, 0),
                ("read_only", "INTEGER", true, 0),
                ("read_only_reason", "TEXT", false, 0),
                ("paused", "INTEGER", true, 0),
                ("paused_until", "INTEGER", false, 0),
                ("blocked_reason", "TEXT", false, 0),
                ("blocked_message", "TEXT", false, 0),
                ("delta_token", "TEXT", false, 0),
                ("delta_run", "TEXT", false, 0),
                ("delta_next_link", "TEXT", false, 0),
                ("delta_staged_link", "TEXT", false, 0),
            ]),
        ),
    ])
}

fn dir_listing(fx: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fx.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
