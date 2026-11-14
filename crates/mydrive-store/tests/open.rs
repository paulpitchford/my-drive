//! Opening the store: connection settings, migrations and refusals.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::{Fixture, raw};
use mydrive_store::{Queries, SCHEMA_VERSION, Store, StoreError};

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
    assert_eq!(indexes, ["items_by_parent", "items_by_sync"]);

    let range_fks: Vec<(String, String, String)> = conn
        .prepare(
            "SELECT \"table\", \"from\", \"to\" FROM pragma_foreign_key_list('hydrated_ranges')",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        range_fks,
        [
            (
                "items".to_owned(),
                "drive_id".to_owned(),
                "drive_id".to_owned()
            ),
            (
                "items".to_owned(),
                "item_id".to_owned(),
                "item_id".to_owned()
            ),
        ]
    );
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
        conn.execute_batch("CREATE TABLE future (x INTEGER); PRAGMA user_version = 99;")
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
        conn.execute_batch("CREATE TABLE future (x INTEGER); PRAGMA user_version = 7;")
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

/// A file that is not SQLite at all is refused with an error naming the file.
#[test]
fn non_sqlite_file_is_refused() {
    let fx = Fixture::new();
    fs::write(
        fx.path(),
        b"this is not a database, just some bytes........",
    )
    .unwrap();
    let err = Store::open(fx.path()).unwrap_err();
    assert!(err.to_string().contains("state.db"), "{err}");
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
                ("etag", "TEXT", true, 0),
                ("ctag", "TEXT", false, 0),
                ("server_hash", "TEXT", false, 0),
                ("local_hash", "TEXT", false, 0),
                ("hydration", "TEXT", true, 0),
                ("pin", "TEXT", true, 0),
                ("sync", "TEXT", true, 0),
                ("last_error", "TEXT", false, 0),
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
                ("delta_token", "TEXT", false, 0),
                ("read_only", "INTEGER", true, 0),
                ("blocked_reason", "TEXT", false, 0),
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
