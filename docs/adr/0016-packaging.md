# ADR-0016: Packaging: native packages, AUR first; no Flatpak, snap or AppImage for the daemon or adapters

- **Status:** Accepted
- **Date:** 2026-11-14
- **Depends on gates:** none

## Context

File-manager integration and FUSE both need the host system:

- Sandboxed builds of the Nextcloud client (snap, Flatpak, AppImage) lose overlay icons and
  context menus in Nautilus, Caja, Nemo and Dolphin, because the host's extension directories
  are outside the sandbox
  ([Nextcloud forum](https://help.nextcloud.com/t/sandboxed-nextcloud-desktop-client-for-linux-and-system-integration/36627)).
  Nextcloud's Flatpak lacks the FUSE-based virtual files feature its AppImage has
  ([nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668),
  [flathub issue 273](https://github.com/flathub/com.nextcloud.desktopclient.nextcloud/issues/273)).
- snap-confine fails with a permission error when a bind-mount source is inside a user FUSE
  filesystem, and Firefox as a snap could not save to gvfs FUSE mounts
  ([snapcraft forum](https://forum.snapcraft.io/t/can-interfaces-define-per-user-bind-mounts/501/18),
  [Mozilla bug 1774556](https://bugzilla.mozilla.org/show_bug.cgi?id=1774556)).
- Flatpak apps see little of the host by default
  ([Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)).
- pCloud's AppImage needs libfuse2 installed by hand on Ubuntu 24.04+
  ([pCloud](https://www.pcloud.com/how-to-install-pcloud-drive-linux)). The Rust `fuser` crate
  is a rewrite of libfuse, not bindings
  ([LWN 1085618](https://lwn.net/Articles/1085618/)); it needs only `fusermount3` at runtime
  (research note `kernel_ondemand_mechanisms.md`, section 4).
- Unprivileged mounts go through the setuid `fusermount3` helper
  ([kernel FUSE docs](https://docs.kernel.org/filesystems/fuse/fuse.html)).
- Compiled file-manager plugins are tied to the host's toolkit generation (Nautilus API 3 vs 4,
  KF5 vs KF6), so they ship best as separate packages built against the distro's libraries
  ([openSUSE 1221923](https://lists.opensuse.org/archives/list/bugs@lists.opensuse.org/thread/MUO5Z6HLHN3L2K5ZNBSQXDANE4RGXVBZ)).
- On Omarchy 4.5.0, `fuse3` is installed, gnome-keyring provides the Secret Service, and
  Hyprland runs inside a systemd user session, so a systemd user unit works for the daemon
  (research note `file_manager_integration.md`, section 5, local inspection).

## Decision

The daemon, CLI and file-manager adapters ship only as native packages. AUR comes first, then
deb and rpm (milestone M5).

AUR packages:

- `mydrive`: `mydrived`, the `mydrive` CLI and the systemd user unit.
- `mydrive-folio`: the Files plugin (ADR-0014).
- Later, one package per further adapter.

We do not ship Flatpak, snap or AppImage builds of the daemon or the adapters.

Runtime dependencies are `fuse3` (for `fusermount3`) and a Secret Service provider. The daemon
uses `fuser`, which speaks the FUSE protocol directly, so there is no libfuse2 dependency.

Release builds use `cargo build --release --locked`.

Versions follow semantic versioning. Each release is a signed git tag. Before 1.0 the IPC
protocol and on-disk formats (state store, xattrs, config) may change between releases; every
such change ships with a migration, and IPC changes are negotiated through `hello` (ADR-0009).

The daemon runs as a systemd user unit. The package does not enable it; post-install
instructions tell the user to run `systemctl --user enable --now` for the unit.

## Alternatives considered

**Flatpak for everything.** One build for all distros, but it loses file-manager integration
and has FUSE trouble, as Nextcloud's Flatpak shows. Rejected for the daemon and adapters.

**AppImage.** No install step, but cannot install file-manager plugins into host directories
and has a history of libfuse2 breakage. Rejected.

**snap.** Confinement fails on user FUSE mounts. Rejected.

**Auto-enabling the user unit on install.** Starts a network daemon in every user session
without consent and before any account exists. Rejected.

## Consequences

- We maintain one PKGBUILD per package now, and deb and rpm recipes later.
- Users of immutable distributions get no supported route until deb/rpm or layered packages
  are worked out; that was not researched.
- The settings UI (`mydrive-ui`) is not covered by this ADR; a sandboxed build of it alone is
  not ruled out, though it would still need the daemon natively.
- `--locked` builds require `Cargo.lock` to be committed and kept current.
- An Omarchy-repository package for `omarchy pkg add` is a possible later step, not decided
  here.

## Rules for implementers

- **R1.** The `mydrive` package installs `mydrived`, `mydrive` and the systemd user unit, and
  depends on `fuse3` and a Secret Service provider.
- **R2.** No package's install script enables or starts the user unit.
- **R3.** No binary links against libfuse2; `fusermount3` from `fuse3` is the only FUSE runtime
  dependency.
- **R4.** Release artefacts are built with `cargo build --release --locked`.
- **R5.** Each adapter is in its own package, and `mydrive` does not depend on any adapter package.
- **R6.** Every change to the state store schema or on-disk format ships with a migration that a
  test exercises from the previous release's format.
- **R7.** Releases are cut only from signed git tags whose version matches the crate versions.

## Revisit when

- Flatpak or snap gain a supported way to install host file-manager extensions and FUSE mounts.
- A distribution we target cannot satisfy the runtime dependencies.
- We reach 1.0 and freeze the IPC protocol and on-disk formats.
