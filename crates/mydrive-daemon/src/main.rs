//! The my-drive daemon.
//!
//! See `docs/architecture/overview.md` for what this crate owns and must not do.

#![forbid(unsafe_code)]

fn main() {
    println!("mydrived {}", env!("CARGO_PKG_VERSION"));
}
