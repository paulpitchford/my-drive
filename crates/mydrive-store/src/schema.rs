//! Opening the database, connection settings and forward-only migrations (ADR-0006 R2, R3).
//!
//! The schema version lives in SQLite's `user_version`, and the file is marked as a
//! my-drive store by `application_id`. Migrations are numbered from 1, each strictly
//! greater than the one before, and there is no down-migration. All pending migrations run
//! in one transaction together with the `user_version` update, so a crash leaves the
//! database either at its old version, untouched, or fully migrated.
//!
//! A migration's SQL is frozen once released: a later change to the schema is a new
//! migration, never an edit to an old one. `docs/architecture/store-schema.md` describes
//! the resulting schema.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};

use crate::error::{SqlContext, StoreError};

/// The newest schema version this build understands.
pub const SCHEMA_VERSION: i64 = 1;

/// The `application_id` that marks a SQLite file as a my-drive state store: ASCII `MYDV`.
pub const APPLICATION_ID: i64 = 0x4D59_4456;

/// How long a statement waits for a lock held by another connection, such as `sqlite3`
/// inspecting the file, before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One forward-only migration.
#[derive(Debug)]
pub(crate) struct Migration {
    /// The `user_version` the database has after this migration.
    pub(crate) version: i64,
    /// The statements to run. Frozen once released.
    pub(crate) sql: &'static str,
}

/// Every migration, in order. `MIGRATIONS.last().version == SCHEMA_VERSION`.
pub(crate) const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: V1,
}];

/// Schema v1. The CHECK lists are the wire forms of `mydrive-core`'s state enums as of
/// this migration; a test keeps them equal to `WIRE_FORMS`. Adding a state value needs a
/// new migration. `kind` has no CHECK, so a new item kind needs no table rebuild; the Rust
/// type validates it. The application ID literal equals [`APPLICATION_ID`] (tested).
const V1: &str = r"
PRAGMA application_id = 1297695830;

CREATE TABLE accounts (
    account_id TEXT PRIMARY KEY NOT NULL CHECK (account_id <> ''),
    tenant_id  TEXT CHECK (tenant_id IS NULL OR tenant_id <> '')
) STRICT;

CREATE TABLE mounts (
    mount_id       TEXT PRIMARY KEY NOT NULL CHECK (mount_id <> ''),
    account_id     TEXT NOT NULL REFERENCES accounts (account_id),
    drive_id       TEXT NOT NULL CHECK (drive_id <> '' AND drive_id = lower(drive_id)),
    root_item_id   TEXT NOT NULL CHECK (root_item_id <> ''),
    mount_point    TEXT NOT NULL CHECK (mount_point <> ''),
    delta_token    TEXT,
    read_only      INTEGER NOT NULL CHECK (read_only IN (0, 1)),
    blocked_reason TEXT,
    UNIQUE (drive_id, root_item_id)
) STRICT;

CREATE TABLE items (
    drive_id       TEXT NOT NULL CHECK (drive_id <> '' AND drive_id = lower(drive_id)),
    item_id        TEXT NOT NULL CHECK (item_id <> ''),
    parent_item_id TEXT CHECK (parent_item_id IS NULL OR parent_item_id <> ''),
    name           TEXT NOT NULL CHECK (name <> ''),
    kind           TEXT NOT NULL,
    size           INTEGER NOT NULL CHECK (size >= 0),
    created_at     INTEGER NOT NULL,
    modified_at    INTEGER NOT NULL,
    etag           TEXT,
    ctag           TEXT,
    server_hash    TEXT,
    local_hash     TEXT,
    hydration      TEXT NOT NULL CHECK (hydration IN ('placeholder', 'partial', 'full')),
    pin            TEXT NOT NULL CHECK (pin IN ('inherit', 'pinned', 'unpinned')),
    sync           TEXT NOT NULL CHECK (sync IN ('in-sync', 'pending-download',
                       'pending-upload', 'uploading', 'conflict', 'error', 'blocked')),
    last_error     TEXT,
    is_provisional INTEGER NOT NULL DEFAULT 0 CHECK (is_provisional IN (0, 1)),
    local_dirty    INTEGER NOT NULL DEFAULT 0 CHECK (local_dirty IN (0, 1)),
    local_generation INTEGER NOT NULL DEFAULT 0 CHECK (local_generation >= 0),
    remote_changed INTEGER NOT NULL DEFAULT 0 CHECK (remote_changed IN (0, 1)),
    remote_etag    TEXT,
    remote_ctag    TEXT,
    remote_hash    TEXT,
    remote_size    INTEGER CHECK (remote_size IS NULL OR remote_size >= 0),
    PRIMARY KEY (drive_id, item_id),
    CHECK (is_provisional = 0 OR local_dirty = 1),
    CHECK (local_dirty = 0 OR sync NOT IN ('in-sync', 'pending-download')),
    CHECK (local_dirty = 1 OR remote_size IS NULL),
    CHECK (local_dirty = 0 OR kind = 'folder' OR hydration = 'full'),
    CHECK (remote_changed = 0 OR sync <> 'in-sync')
) STRICT, WITHOUT ROWID;

CREATE INDEX items_by_parent ON items (drive_id, parent_item_id);
CREATE INDEX items_by_sync ON items (sync);
CREATE INDEX items_by_dirty ON items (local_dirty) WHERE local_dirty = 1;

CREATE TABLE hydrated_ranges (
    drive_id    TEXT NOT NULL,
    item_id     TEXT NOT NULL,
    range_start INTEGER NOT NULL CHECK (range_start >= 0),
    range_end   INTEGER NOT NULL CHECK (range_end > range_start),
    PRIMARY KEY (drive_id, item_id, range_start),
    FOREIGN KEY (drive_id, item_id) REFERENCES items (drive_id, item_id) ON UPDATE CASCADE
) STRICT, WITHOUT ROWID;
";

/// Opens (creating if needed) and migrates the database at `path`.
pub(crate) fn open(path: &Path) -> Result<Connection, StoreError> {
    // Probe an existing file read-only first. A read-write connection to a WAL database
    // checkpoints when it closes, which would write to a file we are about to refuse.
    if path.exists() {
        let probe = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ctx(|| {
            format!(
                "open {} read-only to check its schema version",
                path.display()
            )
        })?;
        check_openable(&probe, path)?;
        probe
            .close()
            .map_err(|(_, source)| source)
            .ctx(|| format!("close the read-only check of {}", path.display()))?;
    }

    let mut conn = Connection::open(path).ctx(|| format!("open state store {}", path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .ctx(|| "set the busy timeout")?;
    // Checked again on the connection we keep, before anything is written.
    check_openable(&conn, path)?;

    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "wal", |row| row.get(0))
        .ctx(|| format!("switch {} to WAL mode", path.display()))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::NotWal {
            path: path.to_owned(),
            mode,
        });
    }
    // FULL: a commit is on disk before it returns, so the engine's durable dirty mark
    // (ADR-0012 R3) survives power loss, not just a process crash.
    conn.pragma_update(None, "synchronous", "FULL")
        .ctx(|| "set synchronous=FULL")?;
    // Foreign keys stay off while migrating, so a migration may rebuild a table (create,
    // copy, drop, rename); `migrate` checks every key before it commits.
    conn.pragma_update(None, "foreign_keys", "OFF")
        .ctx(|| "disable foreign keys for migration")?;
    migrate(&mut conn, path, MIGRATIONS)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .ctx(|| "enable foreign keys")?;
    Ok(conn)
}

fn check_openable(conn: &Connection, path: &Path) -> Result<i64, StoreError> {
    check_openable_up_to(conn, path, SCHEMA_VERSION)
}

/// Refuses a file that is not a my-drive store or is newer than `supported`, and returns
/// its schema version otherwise. Reads only.
fn check_openable_up_to(conn: &Connection, path: &Path, supported: i64) -> Result<i64, StoreError> {
    let not_ours = |reason: &str| StoreError::NotAStateStore {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };
    let read = |pragma: &str| -> Result<i64, StoreError> {
        conn.pragma_query_value(None, pragma, |row| row.get(0))
            .map_err(|e| classify(e, path, pragma))
    };
    let app_id = read("application_id")?;
    let version = read("user_version")?;
    if app_id == 0 && version == 0 {
        let has_objects: bool = conn
            .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_schema)", [], |row| {
                row.get(0)
            })
            .map_err(|e| classify(e, path, "sqlite_schema"))?;
        if has_objects {
            return Err(not_ours("it holds tables but no my-drive application ID"));
        }
        // An empty database: a first start.
        return Ok(0);
    }
    if app_id != APPLICATION_ID {
        return Err(not_ours(&format!("its application ID is {app_id:#x}")));
    }
    if version > supported {
        return Err(StoreError::SchemaTooNew {
            path: path.to_owned(),
            found: version,
            supported,
        });
    }
    if version < 1 {
        return Err(not_ours(&format!("its schema version is {version}")));
    }
    Ok(version)
}

/// A file SQLite cannot read as a database is not a state store; any other error is
/// reported as it is.
fn classify(source: rusqlite::Error, path: &Path, what: &str) -> StoreError {
    if source.sqlite_error_code() == Some(ErrorCode::NotADatabase) {
        StoreError::NotAStateStore {
            path: path.to_owned(),
            reason: "it is not an SQLite database".to_owned(),
        }
    } else {
        StoreError::Sqlite {
            action: format!("read {what} of {}", path.display()),
            source,
        }
    }
}

/// Applies every migration in `migrations` newer than the database, in one transaction.
///
/// The caller turns foreign keys off first (they cannot change inside a transaction), so
/// a migration may rebuild a table. Before committing, `PRAGMA foreign_key_check` must
/// report nothing; otherwise everything is rolled back.
fn migrate(conn: &mut Connection, path: &Path, migrations: &[Migration]) -> Result<(), StoreError> {
    let latest = migrations.last().map_or(0, |m| m.version);
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .ctx(|| "begin the migration transaction")?;
    // Read inside the write lock, so the version cannot change under us.
    let from = check_openable_up_to(&tx, path, latest)?;
    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > from).collect();
    if pending.is_empty() {
        return Ok(());
    }
    for migration in &pending {
        tx.execute_batch(migration.sql)
            .ctx(|| format!("apply migration {}", migration.version))?;
        tx.pragma_update(None, "user_version", migration.version)
            .ctx(|| format!("record schema version {}", migration.version))?;
        let violations = tx
            .prepare("PRAGMA foreign_key_check")
            .and_then(|mut stmt| stmt.query_map([], |_| Ok(()))?.count_rows())
            .ctx(|| format!("check foreign keys after migration {}", migration.version))?;
        if violations > 0 {
            return Err(StoreError::MigrationForeignKeys {
                path: path.to_owned(),
                version: migration.version,
                violations,
            });
        }
    }
    tx.commit()
        .ctx(|| format!("commit migrations from version {from} to {latest}"))?;
    tracing::info!(
        from,
        to = latest,
        path = %path.display(),
        "migrated state store"
    );
    Ok(())
}

/// Counts rows without collecting them.
trait CountRows {
    fn count_rows(self) -> rusqlite::Result<usize>;
}

impl<I: Iterator<Item = rusqlite::Result<()>>> CountRows for I {
    fn count_rows(self) -> rusqlite::Result<usize> {
        let mut n = 0;
        for row in self {
            row?;
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// WP-006 "In scope" and ADR-0006 R2: the connection the store keeps has WAL mode,
    /// `synchronous=FULL` (2) and foreign keys on. The last two are per connection, so they
    /// are read from the connection `open` returns.
    #[test]
    fn opened_connection_has_wal_full_sync_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("state.db")).unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap();
        let fks: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!((mode.as_str(), sync, fks), ("wal", 2, 1));
    }

    /// The state CHECK lists in migration 1 hold exactly `mydrive-core`'s wire forms, so a
    /// value core can produce is never refused and nothing else is accepted (WP-006 AC7).
    #[test]
    fn v1_check_lists_match_core_wire_forms() {
        use mydrive_core::state::{Hydration, Pin, Sync as SyncState};
        for (column, forms) in [
            ("hydration", Hydration::WIRE_FORMS),
            ("pin", Pin::WIRE_FORMS),
            ("sync", SyncState::WIRE_FORMS),
        ] {
            let list = forms
                .iter()
                .map(|f| format!("'{f}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let normalised: String = V1.split_whitespace().collect::<Vec<_>>().join(" ");
            let expected = format!("CHECK ({column} IN ({list}))");
            assert!(normalised.contains(&expected), "missing {expected}");
        }
    }

    /// Migration 1 sets exactly [`APPLICATION_ID`].
    #[test]
    fn v1_sets_the_application_id() {
        assert!(V1.contains(&format!("PRAGMA application_id = {APPLICATION_ID};")));
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("state.db")).unwrap();
        let id: i64 = conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap();
        assert_eq!(id, APPLICATION_ID);
    }

    /// A v1 store holding one item with one hydrated range, closed again.
    fn v1_store_with_an_item(path: &Path) {
        let conn = open(path).unwrap();
        conn.execute_batch(
            "INSERT INTO items (drive_id, item_id, name, kind, size, created_at, modified_at, \
             hydration, pin, sync) VALUES ('d', 'i', 'n', 'file', 10, 0, 0, 'partial', \
             'inherit', 'in-sync');
             INSERT INTO hydrated_ranges VALUES ('d', 'i', 0, 5);",
        )
        .unwrap();
    }

    /// The 12-step table rebuild SQLite documents, applied to `items`: a new first column.
    fn rebuild_items_migration() -> &'static str {
        let start = V1.find("CREATE TABLE items (").unwrap();
        let end = V1[start..].find(") STRICT, WITHOUT ROWID;").unwrap() + start;
        let body = &V1[start + "CREATE TABLE items (".len()..end];
        let sql = format!(
            "CREATE TABLE items_new (extra TEXT, {body}) STRICT, WITHOUT ROWID;
             INSERT INTO items_new SELECT NULL, * FROM items;
             DROP TABLE items;
             ALTER TABLE items_new RENAME TO items;
             CREATE INDEX items_by_parent ON items (drive_id, parent_item_id);
             CREATE INDEX items_by_sync ON items (sync);
             CREATE INDEX items_by_dirty ON items (local_dirty) WHERE local_dirty = 1;"
        );
        Box::leak(sql.into_boxed_str())
    }

    /// Third review: a later migration can rebuild `items` (which `hydrated_ranges`
    /// references) because the runner turns foreign keys off around it, and every key
    /// still holds afterwards.
    #[test]
    fn a_table_rebuild_migration_works_and_keeps_every_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        v1_store_with_an_item(&path);

        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        let migrations = [
            Migration {
                version: 1,
                sql: V1,
            },
            Migration {
                version: 2,
                sql: rebuild_items_migration(),
            },
        ];
        migrate(&mut conn, &path, &migrations).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let (extra, size): (Option<String>, i64) = conn
            .query_row(
                "SELECT extra, size FROM items WHERE item_id = 'i'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((extra, size), (None, 10));
        let ranges: i64 = conn
            .query_row("SELECT COUNT(*) FROM hydrated_ranges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ranges, 1);
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        // The cascade still works after the rebuild.
        conn.execute("UPDATE items SET item_id = 'j' WHERE item_id = 'i'", [])
            .unwrap();
        let moved: String = conn
            .query_row("SELECT item_id FROM hydrated_ranges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(moved, "j");
    }

    /// A migration that leaves a foreign-key violation is rolled back entirely.
    #[test]
    fn a_migration_that_breaks_foreign_keys_is_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        v1_store_with_an_item(&path);

        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        let migrations = [
            Migration {
                version: 1,
                sql: V1,
            },
            Migration {
                version: 2,
                sql: "DELETE FROM items;",
            },
        ];
        let err = migrate(&mut conn, &path, &migrations).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::MigrationForeignKeys {
                    version: 2,
                    violations: 1,
                    ..
                }
            ),
            "{err}"
        );
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        let items: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!((version, items), (1, 1));
    }

    /// The store's own connection has foreign keys on after opening, even though they are
    /// off during migration.
    #[test]
    fn foreign_keys_are_on_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("state.db")).unwrap();
        let fks: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(fks, 1);
    }

    /// ADR-0006 R3: each migration's number is greater than all previous ones, numbering
    /// starts at 1 with no gaps, and the last one is the current schema version.
    #[test]
    fn migrations_are_strictly_increasing_and_end_at_current_version() {
        let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
        let expected: Vec<i64> = (1..=SCHEMA_VERSION).collect();
        assert_eq!(versions, expected);
    }
}
