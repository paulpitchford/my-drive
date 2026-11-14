//! SQLite state store: schema, migrations and queries.
//!
//! The store is the source of truth for everything the daemon knows: accounts, mounts,
//! items, their three state axes and which byte ranges of each file are local (ADR-0006).
//! It decides nothing about when state changes; that is the engine's job.
//!
//! - [`Store::open`] opens the database in WAL mode with `synchronous=FULL` and foreign
//!   keys on, and applies forward-only migrations.
//! - Every operation is a method of the [`Queries`] trait, implemented by both [`Store`]
//!   (each call is atomic on its own) and [`Transaction`] (calls become visible together
//!   on [`Transaction::commit`], and not at all if it is dropped).
//! - Items are addressed only by [`ItemKey`](mydrive_core::ids::ItemKey). Paths are derived
//!   on demand by [`Queries::resolve_path`] and never stored.
//!
//! `docs/architecture/store-schema.md` documents the tables, columns and invariants. See
//! `docs/architecture/overview.md` for what this crate owns and must not do.

#![forbid(unsafe_code)]

mod error;
mod ops;
mod schema;
mod types;

use std::cell::Cell;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

pub use crate::error::StoreError;
pub use crate::ops::Queries;
pub use crate::schema::{APPLICATION_ID, SCHEMA_VERSION};
pub use crate::types::{
    Account, ByteRange, DeleteMode, Item, ItemKind, Mount, MountPath, PendingChanges,
    RemoteApplied, RemoteIdentity, RemoteMetadata, Timestamp, UploadCompleted,
};

use crate::error::SqlContext;

/// An open state store. Owns the daemon's single connection to `state.db`.
///
/// Only `mydrived` opens the store, and within it only the engine writes (ADR-0006 R1).
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Opens the store at `path`, creating it if it does not exist, and migrates it to
    /// [`SCHEMA_VERSION`].
    ///
    /// # Errors
    ///
    /// [`StoreError::SchemaTooNew`] if the file was written by a newer build, and
    /// [`StoreError::NotAStateStore`] if it is not SQLite or not a my-drive store; in both
    /// cases the file is not modified. [`StoreError::NotWal`] if WAL mode is unavailable,
    /// and [`StoreError::Sqlite`] for any other failure.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let conn = schema::open(path)?;
        Ok(Self {
            conn,
            path: path.to_owned(),
        })
    }

    /// The database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Starts a write transaction. Its changes become visible, all together, only on
    /// [`Transaction::commit`]; dropping it, or the process dying first, discards them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the write lock cannot be taken.
    pub fn transaction(&mut self) -> Result<Transaction<'_>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .ctx(|| "begin a transaction")?;
        Ok(Transaction {
            tx,
            aborted: Cell::new(false),
        })
    }
}

/// A batch of store operations applied atomically, for example one delta page and the
/// token that follows it.
///
/// Dropping a transaction without calling [`commit`](Self::commit) rolls it back.
///
/// An operation that the store refuses (a constraint, a missing item) changes nothing and
/// leaves the transaction usable. If SQLite itself rolls the transaction back (after
/// `SQLITE_FULL`, an I/O error, out of memory), every later call, including
/// [`commit`](Self::commit), fails with [`StoreError::TransactionAborted`]: otherwise later
/// statements would run outside any transaction and commit one by one.
#[derive(Debug)]
pub struct Transaction<'s> {
    tx: rusqlite::Transaction<'s>,
    aborted: Cell<bool>,
}

impl Transaction<'_> {
    /// Makes every change in the transaction durable and visible.
    ///
    /// # Errors
    ///
    /// [`StoreError::TransactionAborted`] if SQLite already rolled it back, and
    /// [`StoreError::Sqlite`] if the commit fails; in both cases nothing in the
    /// transaction is applied.
    pub fn commit(self) -> Result<(), StoreError> {
        self.check_live()?;
        self.tx.commit().ctx(|| "commit a transaction")
    }

    /// Discards every change in the transaction. Dropping it does the same.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if SQLite reports a failure while rolling back.
    pub fn rollback(self) -> Result<(), StoreError> {
        if self.tx.is_autocommit() {
            // SQLite already rolled back; there is nothing left to undo.
            return Ok(());
        }
        self.tx.rollback().ctx(|| "roll back a transaction")
    }

    /// Fails, and stays failed, once SQLite has ended the transaction by itself.
    fn check_live(&self) -> Result<(), StoreError> {
        if self.aborted.get() || self.tx.is_autocommit() {
            self.aborted.set(true);
            return Err(StoreError::TransactionAborted { cause: None });
        }
        Ok(())
    }
}

impl ops::sealed::Connected for Store {
    fn run<T, F>(&self, _: ops::sealed::Token, op: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError>,
    {
        op(&self.conn)
    }
}

impl ops::sealed::Connected for Transaction<'_> {
    fn run<T, F>(&self, _: ops::sealed::Token, op: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError>,
    {
        self.check_live()?;
        let result = op(&self.tx);
        if self.tx.is_autocommit() {
            self.aborted.set(true);
            return Err(StoreError::TransactionAborted {
                cause: result.err().map(Box::new),
            });
        }
        result
    }
}

impl Queries for Store {}
impl Queries for Transaction<'_> {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use mydrive_core::ids::{AccountId, DriveId, ItemId, ItemKey};
    use mydrive_core::state::{Hydration, ItemState, Pin, Sync as SyncState};

    use super::*;

    fn account(n: usize) -> Account {
        Account {
            account_id: AccountId::new(format!("acct-{n}")).unwrap(),
            tenant_id: None,
        }
    }

    fn big_item(n: usize) -> Item {
        Item {
            key: ItemKey::new(
                DriveId::new("d").unwrap(),
                ItemId::new(format!("i{n}")).unwrap(),
            ),
            parent_item_id: None,
            name: format!("{n}-{}", "x".repeat(2000)),
            kind: ItemKind::File,
            size: 1,
            created: Timestamp::from_unix_seconds(0),
            modified: Timestamp::from_unix_seconds(0),
            etag: None,
            ctag: None,
            server_hash: None,
            local_hash: None,
            state: ItemState {
                hydration: Hydration::Placeholder,
                pin: Pin::Inherit,
                sync: SyncState::InSync,
            },
            last_error: None,
            is_provisional: false,
            local_dirty: false,
            local_generation: 0,
            remote_changed: false,
            last_remote: None,
        }
    }

    /// Review finding (blocking): after SQLite rolls a transaction back by itself, further
    /// calls on the `Transaction` must not commit on their own. The database is capped with
    /// `max_page_count` so a batch fails with `SQLITE_FULL`; then more calls are made, the
    /// transaction is dropped and the store reopened: nothing from the batch is present.
    #[test]
    fn sqlite_full_aborts_the_transaction_and_nothing_from_the_batch_persists() {
        let (path, _dir) = full_store_scenario(|tx| {
            let commit = tx.commit();
            assert!(
                matches!(commit, Err(StoreError::TransactionAborted { .. })),
                "{commit:?}"
            );
        });
        let store = Store::open(&path).unwrap();
        assert_eq!(store.account(&account(0).account_id).unwrap(), None);
        assert_eq!(store.account(&account(1).account_id).unwrap(), None);
        assert_eq!(store.item(&big_item(0).key).unwrap(), None);
    }

    /// The same, with the transaction dropped instead of committed.
    #[test]
    fn sqlite_full_then_drop_leaves_nothing_from_the_batch() {
        let (path, _dir) = full_store_scenario(|tx| drop(tx));
        let store = Store::open(&path).unwrap();
        assert_eq!(store.account(&account(0).account_id).unwrap(), None);
        assert_eq!(store.account(&account(1).account_id).unwrap(), None);
        assert_eq!(store.item(&big_item(0).key).unwrap(), None);
    }

    /// Fills a capped store inside one transaction until SQLite rolls it back, checks that
    /// further calls are refused, then hands the transaction to `finish`.
    fn full_store_scenario(
        finish: impl FnOnce(Transaction<'_>),
    ) -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).unwrap();
        let pages: i64 = store
            .conn
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        store
            .conn
            .pragma_update(None, "max_page_count", pages + 4)
            .unwrap();

        let tx = store.transaction().unwrap();
        tx.upsert_account(&account(0)).unwrap();
        let mut failure = None;
        for n in 0..200 {
            if let Err(e) = tx.insert_item(&big_item(n)) {
                failure = Some(e);
                break;
            }
        }
        let failure = failure.unwrap();
        let full = matches!(
            &failure,
            StoreError::TransactionAborted { cause: Some(cause) }
                if matches!(**cause, StoreError::Sqlite { ref source, .. }
                    if source.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull))
        );
        assert!(full, "the batch failed with SQLITE_FULL: {failure}");
        assert!(
            tx.tx.is_autocommit(),
            "SQLite rolled the transaction back itself"
        );

        // The reviewer's scenario: carry on after the failure. Each call is refused, so
        // none of them can commit on its own.
        let after = tx.upsert_account(&account(1));
        assert!(
            matches!(after, Err(StoreError::TransactionAborted { cause: None })),
            "{after:?}"
        );
        let read = tx.account(&account(0).account_id);
        assert!(
            matches!(read, Err(StoreError::TransactionAborted { .. })),
            "{read:?}"
        );
        finish(tx);
        drop(store);
        (path, dir)
    }
}
