//! FUSE filesystem over a backing directory.
//!
//! See `docs/architecture/overview.md` for what this crate owns and must not do.
//!
//! This is the only crate where `unsafe` may be allowed, block by block, each with a
//! `SAFETY:` comment (ADR-0003).

#![deny(unsafe_code)]
