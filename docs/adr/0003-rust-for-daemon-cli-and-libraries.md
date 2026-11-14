# ADR-0003: Rust for the daemon, CLI and libraries; working name my-drive

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none (see `docs/architecture/overview.md`)

## Context

The research did not compare implementation languages; the README lists the language as
undecided, noting that Rust, Go and C++/Qt all have precedent. This ADR records the project
lead's judgement. The facts below inform it but do not settle it.

- Data safety is the first principle (overview, Principles 1). Write-path corruption is the
  leading failure class in onedriver's tracker: large files destroyed, KeePassXC databases
  corrupted on save, a corruption regression in v0.14.0
  ([kernel_ondemand_mechanisms.md, section 4](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/kernel_ondemand_mechanisms.md);
  [onedriver issues index](https://issues.ecosyste.ms/hosts/GitHub/repositories/jstaf%2Fonedriver/issues?page=2)).
  Memory safety does not prevent logic errors in a write path, but removes one class of
  corruption bug.
- Rust prior art exists for exactly this product: twodrive (MIT; Cargo workspace with a mock
  backend, daemon, CLI, Nautilus extension) and konedrive (GPL-3.0-or-later; daemon serving a
  FUSE mount over a backing directory)
  ([existing_clients.md, Q5](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/existing_clients.md)).
  Both are alpha, so they are references, not foundations.
- Go prior art is stronger: rclone (MIT) has the best sparse, range-tracked VFS cache and a
  mature OneDrive backend; onedriver (GPL-3.0) is Go
  ([rclone mount docs](https://rclone.org/commands/rclone_mount/), [rclone OneDrive docs](https://rclone.org/onedrive/)).
- The `fuser` crate is "a rewrite of libfuse rather than bindings", so it must implement
  kernel transport changes such as FUSE-over-io_uring itself rather than inheriting them from
  libfuse ([LWN 1085618](https://lwn.net/Articles/1085618/)). Whether `fuser` supports
  io_uring or passthrough as of November 2026 was not found
  ([kernel_ondemand_mechanisms.md, section 1 gaps](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/kernel_ondemand_mechanisms.md)).
  Speaking the FUSE protocol natively avoids the libfuse2 packaging trap seen with pCloud's
  AppImage ([pCloud](https://www.pcloud.com/how-to-install-pcloud-drive-linux)).
- Omarchy's own apps are C++ and Qt Quick, which makes Qt6/QML the natural UI toolkit
  ([file_manager_integration.md, section 4](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/file_manager_integration.md)).

The product name is undecided; a distinct name with a "for OneDrive" descriptor is the
conservative choice ([report, Graph section](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#graph-permits-the-client-but-sets-two-near-term-deadlines)).

## Decision

The daemon, the CLI and all library crates are written in Rust.

- **Toolchain.** Stable Rust, pinned to an exact version in `rust-toolchain.toml`. Edition
  2024.
- **Libraries.** Async runtime: `tokio`. FUSE: `fuser`. SQLite: `rusqlite`. HTTP: `reqwest`
  with rustls (no OpenSSL). D-Bus (Secret Service, notifications, tray): `zbus`.
  Serialization: `serde`. Logging and tracing: `tracing`. Errors: `thiserror` in library
  crates; `anyhow` only in binary crates.
- **Unsafe code.** Every crate has `#![forbid(unsafe_code)]`, except `mydrive-vfs`. In
  `mydrive-vfs` each `unsafe` block carries a `// SAFETY:` comment explaining why it is sound.
- **Panics.** `unwrap()` and `expect()` are not used outside tests.
- **Build output.** The daemon and CLI each build as a single self-contained binary.
- **Other components.** The settings and activity UI uses Qt6/QML. File-manager adapters use
  whatever their host requires (Python for Nautilus/Nemo/Caja, C++ for Dolphin, Lua for
  yazi, shell or any language for the Omarchy Files plugin).
- **Working name.** Until a product name is chosen: crates are named `mydrive-*`, binaries are
  `mydrive` (CLI) and `mydrived` (daemon), and the extended-attribute namespace is
  `user.mydrive.`. Renaming is a mechanical change and is tracked here.

## Alternatives considered

**Go.** The strongest alternative. rclone and onedriver are Go, and rclone's VFS cache and
OneDrive backend could be lifted almost directly under MIT. Initial development would likely
be faster (the lead's judgement, not measured). Not chosen because the lead weights
compile-time memory and concurrency safety above reuse for a client whose differentiator is
not corrupting data, and because borrowing rclone's design is still possible in Rust.

**C++ with Qt.** Matches Omarchy's own apps and would let the UI and daemon share a
toolkit. Not chosen because it gives weaker safety guarantees in the daemon, where the risk
lies.

**D.** The language of abraunegg/onedrive, which would ease porting its Graph handling. Not
chosen: the research notes that D limits the contributor pool
([existing_clients.md, Q5 inferences](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/existing_clients.md)),
and porting a mirror-sync engine does not give us an on-demand design.

## Consequences

- Rust prior art (twodrive, konedrive) and rclone's design can be studied; rclone code must
  be ported, not linked.
- `fuser` speaks the kernel protocol itself, so we depend on its maintainers, or our own
  patches, for new FUSE transport features. Current plans do not need them (ADR-0005 rejects
  passthrough and io_uring because both need root).
- Initial development is expected to be slower than in Go.
- The UI and some adapters are in other languages, so the IPC protocol (ADR-0009) must be
  easy to use outside Rust.
- `mydrive-vfs` is the one crate where unsafe code may appear and gets the closest review.
- **Open concern:** the research did not compare languages, so this decision has no
  research backing either way.

## Rules for implementers

- **R1.** `rust-toolchain.toml` pins an exact stable version; CI builds with it.
- **R2.** Every crate's `Cargo.toml` sets `edition = "2024"`.
- **R3.** Every crate root except `mydrive-vfs` contains `#![forbid(unsafe_code)]`; CI fails
  if one is missing.
- **R4.** Every `unsafe` block in `mydrive-vfs` is immediately preceded by a `// SAFETY:`
  comment (enforced with Clippy's `undocumented_unsafe_blocks` lint).
- **R5.** Clippy's `unwrap_used` and `expect_used` lints are denied for non-test code in
  every crate.
- **R6.** Library crates do not depend on `anyhow`; their public error types are defined with
  `thiserror`.
- **R7.** HTTP uses `reqwest` with rustls; no crate in the dependency tree links OpenSSL.
- **R8.** Crate names start with `mydrive-`; xattr names start with `user.mydrive.`; no other
  spelling of the working name appears in identifiers.

## Revisit when

- `fuser` stops being maintained or cannot support a kernel feature we come to need.
- A product name is chosen (rename only; the language decision stands).
- Development speed proves to be the binding constraint on reaching M2.
