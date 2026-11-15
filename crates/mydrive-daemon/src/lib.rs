//! The my-drive daemon as a library, so tests can run it in-process.
//!
//! The `mydrived` binary is a thin entry point over this crate. The daemon maps protocol
//! methods to calls on the engine facade and facade events to notifications; it reaches the
//! store and Microsoft Graph only through the engine (ADR-0023). Declared by WP-014; WP-042
//! fills this crate, keeping `anyhow` out of every signature here (open question Q41). See
//! `docs/architecture/overview.md` for what the daemon owns and must not do.

#![forbid(unsafe_code)]
