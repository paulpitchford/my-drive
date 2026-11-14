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

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

pub use crate::error::StoreError;
pub use crate::ops::Queries;
pub use crate::schema::SCHEMA_VERSION;
pub use crate::types::{
    Account, ByteRange, DeleteMode, Item, ItemKind, Mount, MountPath, Timestamp,
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
    /// [`StoreError::NotAStateStore`] if it is some other SQLite database; in both cases
    /// the file is not modified. [`StoreError::NotWal`] if WAL mode is unavailable, and
    /// [`StoreError::Sqlite`] for any other failure.
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
        Ok(Transaction { tx })
    }
}

/// A batch of store operations applied atomically, for example one delta page and the
/// token that follows it.
///
/// Dropping a transaction without calling [`commit`](Self::commit) rolls it back.
#[derive(Debug)]
pub struct Transaction<'s> {
    tx: rusqlite::Transaction<'s>,
}

impl Transaction<'_> {
    /// Makes every change in the transaction durable and visible.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if the commit fails; nothing in the transaction is applied.
    pub fn commit(self) -> Result<(), StoreError> {
        self.tx.commit().ctx(|| "commit a transaction")
    }

    /// Discards every change in the transaction. Dropping it does the same.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if SQLite reports a failure while rolling back.
    pub fn rollback(self) -> Result<(), StoreError> {
        self.tx.rollback().ctx(|| "roll back a transaction")
    }
}

impl ops::sealed::Connected for Store {
    fn connection(&self, _: ops::sealed::Token) -> &Connection {
        &self.conn
    }
}

impl ops::sealed::Connected for Transaction<'_> {
    fn connection(&self, _: ops::sealed::Token) -> &Connection {
        &self.tx
    }
}

impl Queries for Store {}
impl Queries for Transaction<'_> {}
