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
pub const SCHEMA_VERSION: i64 = 2;

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
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: V1,
    },
    Migration {
        version: 2,
        sql: V2,
    },
];

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

/// Schema v2 (WP-017): account and mount facts, the local ID (ADR-0021), delta staging
/// (ADR-0022) and listing by name.
///
/// - `accounts` gains columns by `ADD COLUMN`. `kind` follows from version 1's meaning of
///   `tenant_id` (`NULL` for a personal account).
/// - `mounts` is rebuilt so its cross-column rules are table CHECKs. A version 1 blocked
///   reason (free text) becomes the code `organisation-policy` with the text as its
///   message; a personal account's drive type is `personal`, a work account's is not known
///   and stays `NULL`. Tokens are copied unchanged; no run is in progress.
/// - `items` is rebuilt, because `ADD COLUMN` cannot add a `NOT NULL` column without a
///   constant default or a `UNIQUE` column (proved by a unit test). Every version 1 column is
///   copied unchanged; each row gets a `local_id`, 1, 2, ... in key order, and the
///   allocator starts after the largest. `kind` keeps having no CHECK; the dirty-file CHECK
///   is now written for every kind without content. `items_by_parent` is replaced by
///   `items_by_parent_name`, which serves the same lookups and orders children by name.
/// - `delta_staged` and `local_id_allocator` are new. A staged row names the run it
///   belongs to, and (`mount_id`, `run`) references (`mount_id`, `delta_run`) of `mounts`:
///   staged rows can exist only while their mount has that run in progress, and the run
///   cannot end or change kind until they are removed.
///
/// The CHECK lists are the stored forms of the store's enums as of this migration; a test
/// keeps them equal to each enum's `WIRE_FORMS`.
const V2: &str = r"
ALTER TABLE accounts ADD COLUMN kind TEXT NOT NULL DEFAULT 'work'
    CHECK (kind IN ('personal', 'work'));
UPDATE accounts SET kind = CASE WHEN tenant_id IS NULL THEN 'personal' ELSE 'work' END;
ALTER TABLE accounts ADD COLUMN username TEXT CHECK (username IS NULL OR username <> '');
ALTER TABLE accounts ADD COLUMN display_name TEXT
    CHECK (display_name IS NULL OR display_name <> '');
ALTER TABLE accounts ADD COLUMN org_name TEXT CHECK (org_name IS NULL OR org_name <> '');
ALTER TABLE accounts ADD COLUMN needs_sign_in INTEGER NOT NULL DEFAULT 0
    CHECK (needs_sign_in IN (0, 1));
ALTER TABLE accounts ADD COLUMN hash_percent_refused INTEGER NOT NULL DEFAULT 0
    CHECK (hash_percent_refused IN (0, 1));

CREATE TABLE mounts_v2 (
    mount_id          TEXT PRIMARY KEY NOT NULL CHECK (mount_id <> ''),
    account_id        TEXT NOT NULL REFERENCES accounts (account_id),
    drive_id          TEXT NOT NULL CHECK (drive_id <> '' AND drive_id = lower(drive_id)),
    root_item_id      TEXT NOT NULL CHECK (root_item_id <> ''),
    mount_point       TEXT NOT NULL CHECK (mount_point <> ''),
    drive_type        TEXT CHECK (drive_type IS NULL
                          OR drive_type IN ('personal', 'business', 'document-library')),
    display_name      TEXT CHECK (display_name IS NULL OR display_name <> ''),
    read_only         INTEGER NOT NULL CHECK (read_only IN (0, 1)),
    read_only_reason  TEXT CHECK (read_only_reason IS NULL
                          OR read_only_reason IN ('library-read-only', 'no-write-permission')),
    paused            INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    paused_until      INTEGER,
    blocked_reason    TEXT CHECK (blocked_reason IS NULL
                          OR blocked_reason IN ('organisation-policy', 'tenant-refused')),
    blocked_message   TEXT CHECK (blocked_message IS NULL OR blocked_message <> ''),
    delta_token       TEXT,
    delta_run         TEXT CHECK (delta_run IS NULL
                          OR delta_run IN ('initial', 'incremental', 'resync-apply',
                                           'resync-upload')),
    delta_next_link   TEXT CHECK (delta_next_link IS NULL OR delta_next_link <> ''),
    delta_staged_link TEXT CHECK (delta_staged_link IS NULL OR delta_staged_link <> ''),
    UNIQUE (drive_id, root_item_id),
    UNIQUE (mount_id, delta_run),
    CHECK (delta_run IS NOT NULL OR (delta_next_link IS NULL AND delta_staged_link IS NULL)),
    CHECK (delta_next_link IS NULL OR delta_staged_link IS NULL),
    CHECK (blocked_message IS NULL OR blocked_reason IS NOT NULL),
    CHECK (paused = 1 OR paused_until IS NULL),
    CHECK (read_only = 1 OR read_only_reason IS NULL),
    CHECK (delta_run IS NULL OR delta_run <> 'incremental' OR delta_token IS NOT NULL),
    CHECK (delta_run IS NULL OR delta_run <> 'initial' OR delta_token IS NULL)
) STRICT;

INSERT INTO mounts_v2 (mount_id, account_id, drive_id, root_item_id, mount_point, drive_type,
    display_name, read_only, read_only_reason, paused, paused_until, blocked_reason,
    blocked_message, delta_token, delta_run, delta_next_link, delta_staged_link)
SELECT m.mount_id, m.account_id, m.drive_id, m.root_item_id, m.mount_point,
    CASE WHEN a.kind = 'personal' THEN 'personal' ELSE NULL END,
    NULL, m.read_only, NULL, 0, NULL,
    CASE WHEN m.blocked_reason IS NULL THEN NULL ELSE 'organisation-policy' END,
    NULLIF(m.blocked_reason, ''), m.delta_token, NULL, NULL, NULL
FROM mounts AS m LEFT JOIN accounts AS a ON a.account_id = m.account_id;

DROP TABLE mounts;
ALTER TABLE mounts_v2 RENAME TO mounts;

CREATE TABLE items_v2 (
    drive_id       TEXT NOT NULL CHECK (drive_id <> '' AND drive_id = lower(drive_id)),
    item_id        TEXT NOT NULL CHECK (item_id <> ''),
    parent_item_id TEXT CHECK (parent_item_id IS NULL OR parent_item_id <> ''),
    name           TEXT NOT NULL COLLATE BINARY CHECK (name <> ''),
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
    local_id       INTEGER NOT NULL UNIQUE CHECK (local_id >= 1),
    PRIMARY KEY (drive_id, item_id),
    CHECK (is_provisional = 0 OR local_dirty = 1),
    CHECK (local_dirty = 0 OR sync NOT IN ('in-sync', 'pending-download')),
    CHECK (local_dirty = 1 OR remote_size IS NULL),
    CHECK (local_dirty = 0 OR kind <> 'file' OR hydration = 'full'),
    CHECK (remote_changed = 0 OR sync <> 'in-sync')
) STRICT, WITHOUT ROWID;

INSERT INTO items_v2 (drive_id, item_id, parent_item_id, name, kind, size, created_at,
    modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error,
    is_provisional, local_dirty, local_generation, remote_changed, remote_etag, remote_ctag,
    remote_hash, remote_size, local_id)
SELECT drive_id, item_id, parent_item_id, name, kind, size, created_at,
    modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error,
    is_provisional, local_dirty, local_generation, remote_changed, remote_etag, remote_ctag,
    remote_hash, remote_size, row_number() OVER (ORDER BY drive_id, item_id)
FROM items;

DROP TABLE items;
ALTER TABLE items_v2 RENAME TO items;

CREATE INDEX items_by_parent_name ON items (drive_id, parent_item_id, name);
CREATE INDEX items_by_sync ON items (sync);
CREATE INDEX items_by_dirty ON items (local_dirty) WHERE local_dirty = 1;

CREATE TABLE local_id_allocator (
    singleton     INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    next_local_id INTEGER NOT NULL CHECK (next_local_id >= 1)
) STRICT;
INSERT INTO local_id_allocator (singleton, next_local_id)
    SELECT 1, COALESCE(MAX(local_id), 0) + 1 FROM items;

CREATE TABLE delta_staged (
    mount_id       TEXT NOT NULL,
    run            TEXT NOT NULL,
    item_id        TEXT NOT NULL CHECK (item_id <> ''),
    seq            INTEGER NOT NULL CHECK (seq >= 1),
    deleted        INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    problem        TEXT CHECK (problem IS NULL OR problem IN ('reported', 'parent-cycle',
                       'missing-parent', 'parent-deleted')),
    problem_detail TEXT,
    is_root        INTEGER NOT NULL CHECK (is_root IN (0, 1)),
    facet          TEXT CHECK (facet IS NULL OR facet <> ''),
    parent_item_id TEXT CHECK (parent_item_id IS NULL OR parent_item_id <> ''),
    name           TEXT COLLATE BINARY CHECK (name IS NULL OR name <> ''),
    size           INTEGER CHECK (size IS NULL OR size >= 0),
    created_at     INTEGER,
    modified_at    INTEGER,
    etag           TEXT,
    ctag           TEXT,
    server_hash    TEXT,
    apply_order    INTEGER CHECK (apply_order IS NULL OR apply_order >= 0),
    PRIMARY KEY (mount_id, item_id),
    UNIQUE (mount_id, seq),
    FOREIGN KEY (mount_id, run) REFERENCES mounts (mount_id, delta_run) ON DELETE CASCADE,
    CHECK (deleted = 0 OR (problem IS NULL AND is_root = 0 AND facet IS NULL
        AND parent_item_id IS NULL AND name IS NULL AND size IS NULL AND created_at IS NULL
        AND modified_at IS NULL AND etag IS NULL AND ctag IS NULL AND server_hash IS NULL)),
    CHECK (problem IS NOT NULL OR deleted = 1 OR (facet IS NOT NULL AND name IS NOT NULL
        AND size IS NOT NULL AND created_at IS NOT NULL AND modified_at IS NOT NULL)),
    CHECK (problem IS NOT NULL OR deleted = 1 OR is_root = 1 OR parent_item_id IS NOT NULL),
    CHECK (is_root = 0 OR parent_item_id IS NULL),
    CHECK ((problem IS 'reported') = (problem_detail IS NOT NULL)),
    CHECK ((name IS NULL) = (facet IS NULL))
) STRICT, WITHOUT ROWID;

CREATE INDEX delta_staged_by_parent ON delta_staged (mount_id, parent_item_id);
CREATE INDEX delta_staged_by_order ON delta_staged (mount_id, apply_order, item_id);
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
    migrate(&mut conn, path, migrations())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .ctx(|| "enable foreign keys")?;
    Ok(conn)
}

/// The migrations [`open`] applies: always [`MIGRATIONS`], except in this crate's unit
/// tests, which may substitute a longer list on their own thread to run a later
/// migration through the real `Store::open` path (see `tests::MigrationsOverride`).
fn migrations() -> &'static [Migration] {
    #[cfg(test)]
    if let Some(list) = tests::MIGRATIONS_OVERRIDE.with(std::cell::Cell::get) {
        return list;
    }
    MIGRATIONS
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
        #[cfg(test)]
        tests::before_migration(&tx, migration.version);
        tx.execute_batch(migration.sql)
            .ctx(|| format!("apply migration {}", migration.version))?;
        tx.pragma_update(None, "user_version", migration.version)
            .ctx(|| format!("record schema version {}", migration.version))?;
        #[cfg(test)]
        tests::after_migration(&tx, migration.version);
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

    /// The state CHECK lists in migrations 1 and 2 (which rebuilds `items`) hold exactly
    /// `mydrive-core`'s wire forms, so a value core can produce is never refused and
    /// nothing else is accepted (WP-006 AC7).
    #[test]
    fn state_check_lists_match_core_wire_forms() {
        use mydrive_core::state::{Hydration, Pin, Sync as SyncState};
        for sql in [V1, V2] {
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
                let normalised: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let expected = format!("CHECK ({column} IN ({list}))");
                assert!(normalised.contains(&expected), "missing {expected}");
            }
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

    /// A current store holding one item with one hydrated range, closed again.
    fn store_with_an_item(path: &Path) {
        let conn = open(path).unwrap();
        conn.execute_batch(
            "INSERT INTO items (drive_id, item_id, name, kind, size, created_at, modified_at, \
             hydration, pin, sync, local_id) VALUES ('d', 'i', 'n', 'file', 10, 0, 0, \
             'partial', 'inherit', 'in-sync', 1);
             INSERT INTO hydrated_ranges VALUES ('d', 'i', 0, 5);",
        )
        .unwrap();
    }

    /// Every migration this build has, then `extra` as the next one.
    fn with_next(extra: &'static str) -> Vec<Migration> {
        let mut list: Vec<Migration> = MIGRATIONS
            .iter()
            .map(|m| Migration {
                version: m.version,
                sql: m.sql,
            })
            .collect();
        list.push(Migration {
            version: SCHEMA_VERSION + 1,
            sql: extra,
        });
        list
    }

    /// The 12-step table rebuild SQLite documents, applied to the current `items`: a new
    /// first column.
    fn rebuild_items_migration() -> &'static str {
        let start = V2.find("CREATE TABLE items_v2 (").unwrap();
        let end = V2[start..].find(") STRICT, WITHOUT ROWID;").unwrap() + start;
        let body = &V2[start + "CREATE TABLE items_v2 (".len()..end];
        let sql = format!(
            "CREATE TABLE items_new (extra TEXT, {body}) STRICT, WITHOUT ROWID;
             INSERT INTO items_new SELECT NULL, * FROM items;
             DROP TABLE items;
             ALTER TABLE items_new RENAME TO items;
             CREATE INDEX items_by_parent_name ON items (drive_id, parent_item_id, name);
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
        store_with_an_item(&path);

        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        migrate(&mut conn, &path, &with_next(rebuild_items_migration())).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        let (extra, size, local_id): (Option<String>, i64, i64) = conn
            .query_row(
                "SELECT extra, size, local_id FROM items WHERE item_id = 'i'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((extra, size, local_id), (None, 10, 1));
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

    thread_local! {
        /// Read by [`migrations`]; set only through [`MigrationsOverride`].
        pub(super) static MIGRATIONS_OVERRIDE: std::cell::Cell<Option<&'static [Migration]>> =
            const { std::cell::Cell::new(None) };

        /// Called by [`migrate`] before and after each migration's statements (the second
        /// time before the commit); set only by the kill test below. The flag says which.
        static MIGRATION_HOOK: std::cell::Cell<Option<MigrationHook>> =
            const { std::cell::Cell::new(None) };
    }

    /// A hook called with the connection, the migration's version and whether it has run.
    type MigrationHook = fn(&Connection, i64, bool);

    /// The hook [`migrate`] calls before a migration in this crate's unit tests.
    pub(super) fn before_migration(conn: &Connection, version: i64) {
        if let Some(hook) = MIGRATION_HOOK.with(std::cell::Cell::get) {
            hook(conn, version, false);
        }
    }

    /// The hook [`migrate`] calls after a migration in this crate's unit tests.
    pub(super) fn after_migration(conn: &Connection, version: i64) {
        if let Some(hook) = MIGRATION_HOOK.with(std::cell::Cell::get) {
            hook(conn, version, true);
        }
    }

    /// While alive, [`open`] on this thread applies `list` instead of [`MIGRATIONS`].
    struct MigrationsOverride;

    impl MigrationsOverride {
        fn set(list: Vec<Migration>) -> Self {
            let list: &'static [Migration] = Box::leak(list.into_boxed_slice());
            MIGRATIONS_OVERRIDE.with(|c| c.set(Some(list)));
            Self
        }
    }

    impl Drop for MigrationsOverride {
        fn drop(&mut self) {
            MIGRATIONS_OVERRIDE.with(|c| c.set(None));
        }
    }

    /// WP-013: a table-rebuild migration run through the real `Store::open` path, so the
    /// step that turns foreign keys off before migrating (and on again after) is covered.
    /// With foreign keys on, the rebuild's `DROP TABLE items` would fail on the hydrated
    /// range that references it; with them left off afterwards, the cascade check at the
    /// end would fail.
    #[test]
    fn a_table_rebuild_migration_runs_through_store_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        store_with_an_item(&path);

        let _override = MigrationsOverride::set(with_next(rebuild_items_migration()));
        let store = crate::Store::open(&path).unwrap();
        let conn = &store.conn;

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        let extra: Option<String> = conn
            .query_row("SELECT extra FROM items WHERE item_id = 'i'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(extra, None);
        let key = mydrive_core::ids::ItemKey::new(
            mydrive_core::ids::DriveId::new("d").unwrap(),
            mydrive_core::ids::ItemId::new("i").unwrap(),
        );
        assert_eq!(
            crate::Queries::hydrated_ranges(&store, &key).unwrap(),
            [crate::ByteRange::new(0, 5).unwrap()]
        );
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        // Foreign keys are on again on the store's connection: the cascade works.
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
        store_with_an_item(&path);

        let mut conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        let err = migrate(&mut conn, &path, &with_next("DELETE FROM items;")).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::MigrationForeignKeys { version, violations: 1, .. }
                    if version == SCHEMA_VERSION + 1
            ),
            "{err}"
        );
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        let items: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!((version, items), (SCHEMA_VERSION, 1));
    }

    /// WP-017 Notes: the properties migration 2 gives `local_id` (NOT NULL, UNIQUE) cannot
    /// be added by `ALTER TABLE ADD COLUMN`, which is why it rebuilds `items`.
    #[test]
    fn local_id_needs_a_rebuild_not_add_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch(
            "INSERT INTO items (drive_id, item_id, name, kind, size, created_at, modified_at, \
             hydration, pin, sync) VALUES ('d', 'i', 'n', 'file', 1, 0, 0, 'placeholder', \
             'inherit', 'in-sync');",
        )
        .unwrap();
        for (sql, why) in [
            (
                "ALTER TABLE items ADD COLUMN local_id INTEGER NOT NULL",
                "NOT NULL column with default value NULL",
            ),
            (
                "ALTER TABLE items ADD COLUMN local_id INTEGER UNIQUE",
                "UNIQUE column",
            ),
        ] {
            let err = conn.execute_batch(sql).unwrap_err().to_string();
            assert!(err.contains(why), "{sql}: {err}");
        }
        // A constant default would satisfy NOT NULL but give every existing row the same
        // value, which a unique index then refuses.
        conn.execute_batch("ALTER TABLE items ADD COLUMN local_id INTEGER NOT NULL DEFAULT 0")
            .unwrap();
        conn.execute_batch(
            "INSERT INTO items (drive_id, item_id, name, kind, size, created_at, modified_at, \
             hydration, pin, sync) VALUES ('d', 'j', 'n', 'file', 1, 0, 0, 'placeholder', \
             'inherit', 'in-sync');",
        )
        .unwrap();
        let err = conn
            .execute_batch("CREATE UNIQUE INDEX u ON items (local_id)")
            .unwrap_err()
            .to_string();
        assert!(err.contains("UNIQUE constraint failed"), "{err}");
    }

    /// The fixture written by the version 1 code (`tests/fixtures/v1`).
    fn v1_fixture_copy(dir: &Path) -> std::path::PathBuf {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v1/state-v1.db");
        let path = dir.join("state.db");
        std::fs::copy(fixture, &path).unwrap();
        path
    }

    /// Every row of every version 1 table, read through a plain connection.
    fn v1_rows(path: &Path) -> Vec<Vec<rusqlite::types::Value>> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut rows = Vec::new();
        for sql in [
            "SELECT account_id, tenant_id FROM accounts ORDER BY account_id",
            "SELECT mount_id, account_id, drive_id, root_item_id, mount_point, delta_token, \
             read_only FROM mounts ORDER BY mount_id",
            "SELECT drive_id, item_id, parent_item_id, name, kind, size, created_at, \
             modified_at, etag, ctag, server_hash, local_hash, hydration, pin, sync, last_error, \
             is_provisional, local_dirty, local_generation, remote_changed, remote_etag, \
             remote_ctag, remote_hash, remote_size FROM items ORDER BY drive_id, item_id",
            "SELECT * FROM hydrated_ranges ORDER BY drive_id, item_id, range_start",
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

    /// Set in the child of `killed_mid_migration_leaves_a_valid_version_1_store`.
    const MIGRATION_CHILD_ENV: &str = "MYDRIVE_STORE_MIGRATION_CRASH_CHILD";

    /// WP-017 AC1, crash safety: the process is killed with SIGKILL after migration 2's
    /// statements and its `user_version` update have run, before the commit. The database
    /// is then a valid version 1 store, byte-for-byte the same rows as before, and opens
    /// and migrates normally afterwards.
    #[test]
    fn killed_mid_migration_leaves_a_valid_version_1_store() {
        use std::os::unix::process::ExitStatusExt;

        if let Ok(db) = std::env::var(MIGRATION_CHILD_ENV) {
            MIGRATION_HOOK.with(|c| {
                c.set(Some(|conn: &Connection, version: i64, done: bool| {
                    if version != 2 {
                        return;
                    }
                    if !done {
                        // A tiny page cache, so the migration's pages spill into the WAL
                        // before the transaction ends and recovery has frames to discard.
                        conn.pragma_update(None, "cache_size", 1).unwrap();
                        return;
                    }
                    let db = std::env::var(MIGRATION_CHILD_ENV).unwrap();
                    std::fs::write(Path::new(&db).with_file_name("reached"), b"").unwrap();
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &std::process::id().to_string()])
                        .status();
                    std::process::exit(0);
                }));
            });
            let _ = crate::Store::open(&db);
            std::process::exit(0);
        }

        let dir = tempfile::tempdir().unwrap();
        let path = v1_fixture_copy(dir.path());
        let before = v1_rows(&path);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "schema::tests::killed_mid_migration_leaves_a_valid_version_1_store",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MIGRATION_CHILD_ENV, &path)
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(9),
            "the child must be killed: {status}"
        );
        assert!(
            dir.path().join("reached").exists(),
            "the child reached the hook"
        );
        let wal = std::fs::metadata(dir.path().join("state.db-wal")).map_or(0, |m| m.len());
        assert!(
            wal > 0,
            "the migration's pages reached the WAL file before the kill"
        );

        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        let new_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name IN \
                 ('delta_staged', 'local_id_allocator', 'items_v2', 'mounts_v2')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(
            (version, integrity.as_str(), violations, new_tables),
            (1, "ok", 0, 0)
        );
        assert_eq!(v1_rows(&path), before, "every version 1 row is unchanged");

        // And it migrates normally afterwards.
        let store = crate::Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    /// Every CHECK list migration 2 adds holds exactly the stored forms of the Rust type
    /// that reads the column, so a value the store writes is never refused and nothing else
    /// is accepted.
    #[test]
    fn v2_check_lists_match_the_store_enums() {
        use crate::types::{
            ACCOUNT_KIND_FORMS, BlockedReason, DeltaRunKind, DriveType, ReadOnlyReason,
            StagedProblem,
        };
        let normalised: String = V2.split_whitespace().collect::<Vec<_>>().join(" ");
        for (column, forms, nullable) in [
            ("kind", ACCOUNT_KIND_FORMS, false),
            ("drive_type", DriveType::WIRE_FORMS, true),
            ("read_only_reason", ReadOnlyReason::WIRE_FORMS, true),
            ("blocked_reason", BlockedReason::WIRE_FORMS, true),
            ("delta_run", DeltaRunKind::WIRE_FORMS, true),
            ("problem", StagedProblem::WIRE_FORMS, true),
        ] {
            let list = forms
                .iter()
                .map(|f| format!("'{f}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let expected = if nullable {
                format!("CHECK ({column} IS NULL OR {column} IN ({list}))")
            } else {
                format!("CHECK ({column} IN ({list}))")
            };
            assert!(normalised.contains(&expected), "missing {expected}");
        }
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
