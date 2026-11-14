//! Domain types, state model and pure logic for my-drive.
//!
//! This crate does no I/O. See `docs/architecture/overview.md` for what it owns.

#![forbid(unsafe_code)]

pub mod hash;
pub mod ids;
pub mod names;
pub mod state;
