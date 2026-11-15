//! The engine facade: the `Engine` type with its view and event types. It is the only way
//! the daemon causes or observes anything in the store, the backing directories, the token
//! store or Microsoft Graph (ADR-0023).
//!
//! Declared by WP-014; WP-029 fills it.
