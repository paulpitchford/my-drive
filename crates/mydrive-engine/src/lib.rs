//! Sync engine: delta processing, transfers, reconciliation and scheduling.
//!
//! The daemon reaches the store, the backing directories, the token store and Microsoft
//! Graph only through the facade in [`api`] (ADR-0023). The other modules are the engine's
//! parts, declared by WP-014 so each M1 work package owns whole files; each module's
//! documentation names the package that fills it.
//!
//! See `docs/architecture/overview.md` for what this crate owns and must not do.

#![forbid(unsafe_code)]

pub mod accounts;
pub mod api;
pub mod backing;
pub mod clock;
pub mod config;
pub mod conflict;
pub mod db;
pub mod delta;
pub mod error;
pub mod events;
pub mod library;
pub mod local;
pub mod mount;
pub mod policy;
pub mod recovery;
pub mod scheduler;
pub mod transfer;
