//! IPC protocol types, server and client.
//!
//! The protocol is newline-delimited JSON-RPC 2.0 over the daemon's Unix socket (ADR-0009).
//! This crate defines it as types, so the daemon, CLI and adapters share one contract:
//!
//! - [`envelope`]: JSON-RPC requests, notifications and responses.
//! - [`codec`]: one message per line, at most 1 MiB, robust to any read boundaries.
//! - [`methods`]: the version 1 catalogue, the `hello` negotiation and per-connection
//!   gatekeeping.
//! - [`paging`]: splitting `item.status` results into pages that fit one line (ADR-0019).
//! - [`values`]: the open value sets (ADR-0019).
//! - [`error`]: the error object and the stable error-code table.
//! - [`path`]: absolute protocol paths.
//!
//! The written catalogue, with an example of every message, is `docs/protocol/README.md`.
//! The socket server and client come later (M1). See `docs/architecture/overview.md` for what
//! this crate owns and must not do.

#![forbid(unsafe_code)]

pub mod codec;
pub mod envelope;
pub mod error;
pub mod methods;
pub mod paging;
pub mod path;
pub mod values;
