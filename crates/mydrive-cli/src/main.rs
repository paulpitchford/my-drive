//! The my-drive command-line client.
//!
//! See `docs/architecture/overview.md` for what this crate owns and must not do.

#![forbid(unsafe_code)]

fn main() {
    println!("mydrive {}", env!("CARGO_PKG_VERSION"));
}
