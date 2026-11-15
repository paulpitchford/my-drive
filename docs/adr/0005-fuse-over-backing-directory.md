# ADR-0005: On-demand files via FUSE over a separate backing directory

- **Status:** Accepted; backing layout amended by ADR-0021
- **Date:** 2026-11-14
- **Depends on gates:** G1, G3 (see `docs/architecture/overview.md`)

## Context

- FUSE is the only mechanism that mounts unprivileged (through setuid `fusermount3`), works on
  any backing filesystem, and is used by every shipping Linux on-demand product: rclone,
  onedriver, pCloud, SeaDrive, Nextcloud 35
  ([kernel FUSE docs](https://docs.kernel.org/filesystems/fuse/fuse.html);
  [kernel_ondemand_mechanisms.md, sections 1 and 4](../../research_notes/OneDrive%20Linux%20on%20demand%20clients/kernel_ondemand_mechanisms.md)).
- fanotify `FAN_PRE_ACCESS` (Linux 6.14+, ext4/btrfs/xfs) fills sparse files before a range is
  read, including on mmap faults, but needs `CAP_SYS_ADMIN`
  ([fanotify_init(2)](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)), had no
  directory events in the 6.14 merge
  ([commit 4f8afa33](https://github.molgen.mpg.de/mariux64/linux/commit/4f8afa33817a6420398d1c177c6e220a05081f51)),
  and depends on the daemon running: by the research's reading, a stub read while the daemon
  is down returns zeros ([LPC slides](https://lpc.events/event/18/contributions/1717/attachments/1648/3404/fanotify.pdf)).
- FUSE passthrough requires `CAP_SYS_ADMIN`
  ([7.2 docs](https://docs.kernel.org/7.2/_sources/filesystems/fuse/fuse-passthrough.rst.txt));
  FUSE-over-io_uring needs a module parameter set by root
  ([libfuse](https://raw.githubusercontent.com/libfuse/libfuse/master/doc/README.fuse-io-uring)).
- The 6.15 request timeouts are opt-in and abort the whole connection, so the daemon itself
  must answer quickly with an errno when offline
  ([6.15 pull](https://lkml.rescloud.iu.edu/2504.0/00453.html)).
- rclone keeps sparse cache files and tracks which byte ranges are present
  ([rclone mount](https://rclone.org/commands/rclone_mount/)); onedriver holds files in
  memory and is whole-file.
- FUSE mounts in the runtime directory are awkward for Flatpak apps
  ([KDE bug 434917](https://bugs.kde.org/show_bug.cgi?id=434917)), which argues for mount
  points inside the home directory.
- Ranged reads currently go to a pre-authenticated URL; those URLs retire on 2027-04-01, and
  whether `Range` works on authenticated `/content` is unknown (gate G1)
  ([MC1492958 mirror](https://mc.merill.net/message/MC1492958);
  [download docs](https://learn.microsoft.com/en-us/graph/api/driveitem-get-content?view=graph-rest-1.0)).
- abraunegg's draft FUSE design (PR #3600) went on hold on 2026-11-11 for an unstated reason
  (gate G3) ([Issue #757](https://github.com/abraunegg/onedrive/issues/757)).

## Decision

- **Mount.** Each mount (overview vocabulary) is one FUSE filesystem implemented with `fuser`,
  mounted unprivileged through `fusermount3`, without `allow_other`, at a mount point inside
  the user's home directory (overview, On disk).
- **Backing directory.** A mount's content lives in its backing directory,
  `$XDG_DATA_HOME/mydrive/mounts/<mount_id>/`, as sparse files. The mount point and the
  backing directory are different paths; we do not overmount the sync directory as OpenCloud
  openvfs does. When the daemon is not running the mount point is an empty directory, so a
  placeholder can never be read as zeros.
- **No hydration on metadata.** `readdir`, `lookup`, `getattr`/`stat`, `open` and xattr reads
  never trigger hydration. They are answered from the state store.
- **Hydration on read.** Hydration is triggered by `read`, including reads the kernel issues
  for mmap page faults. It fetches the requested byte range plus read-ahead and records the
  hydrated extents in the state store (ADR-0006). If gate G1 fails, the first read of a file
  hydrates the whole file instead.
- **Writes.** A write to a placeholder or partial file first hydrates it fully, unless the
  file was opened with `O_TRUNC`.
- **Failure.** When content cannot be fetched (offline, authentication expired, organisation
  policy block), the read fails with `EIO` once no progress has been made for a bounded time
  (default 30 seconds). The reason is recorded in the daemon's status and surfaced as a
  notification. A read never blocks indefinitely and never returns zeros for missing content.
- **Sizes.** `st_size` is the cloud size; `st_blocks` reflects the bytes held locally.
- **Not used.** FUSE passthrough and FUSE-over-io_uring, because both need root.
- **Later backend.** fanotify pre-content is recorded as a possible later backend behind the
  same `mydrive-core` traits (`Hydrator`), using the same sparse-file format. It is not the
  primary mechanism: it needs `CAP_SYS_ADMIN`, lacks directory events, and reads zeros when
  the daemon is down.

## Alternatives considered

**fanotify pre-content as primary.** Native speed and no mount point, but root-only, no
directory events, and fails open (zeros) when the daemon is down. Kept as a later option.

**Suffix stub files (`.nextcloud` style).** No kernel support needed, but not transparent to
applications or the shell; Nextcloud's maintainers called it not stable enough
([nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)).

**Overmounting the sync directory (openvfs).** State stays visible in the real directory, but
when the FUSE layer is absent the user's path shows the raw sparse files, which read as zeros.

**Whole files in memory (onedriver).** Simple, but poor for large files and no partial
hydration.

**Embedding rclone's VFS.** Proven cache, but no pin or state model, and a Go component
inside a Rust daemon (ADR-0003). Its design is borrowed instead.

## Consequences

- Every application, terminal and file manager gets on-demand files with no adapter.
- Reads of hydrated data still pay a FUSE round trip; no passthrough is available.
- A missing file fails loudly (principle 2).
- Backup tools that read the mount will hydrate everything; the backing directory must be
  kept out of backups (ADR-0013 covers crawler defence).
- **Open concern:** the research recommended FUSE stacked over the real sync directory
  (openvfs pattern) ([report](../../reports/OneDrive%20Linux%20on%20demand%20clients.md#fuse-stacked-over-a-real-directory-is-the-mechanism-to-build-on)).
  This ADR deliberately chooses separate paths instead.
- **Open concern:** if the daemon crashes without unmounting, the mount point returns "Transport
  endpoint is not connected" rather than appearing empty until it is unmounted (background
  knowledge, not verified in the research). This still fails loudly.
- **Open concern:** the report leaned towards as few mount points as the layout allows; one
  FUSE mount per mount means one per library.
- **Open concern:** that FUSE delivers mmap page faults as `read` requests is background
  knowledge, not verified in the research.

## Rules for implementers

- **R1.** No code path reached from `readdir`, `lookup`, `getattr`, `open`, `getxattr` or
  `listxattr` requests content from Graph.
- **R2.** The mount point is never inside `$XDG_DATA_HOME/mydrive/mounts/`, and the backing
  directory is never under a mount point.
- **R3.** With the daemon stopped and the mount cleanly unmounted, the mount point is an empty
  directory.
- **R4.** A read of a range that cannot be fetched returns `EIO` within 30 seconds (default)
  of the last progress, never returns zero bytes in place of content, and leaves a status
  entry and notification naming the reason.
- **R5.** After a ranged read, the store records exactly the hydrated extents; a later read of
  those extents makes no Graph request.
- **R6.** Every HTTP response to a ranged request is checked for status `206` or `200`; a
  `200` is treated as the full body.
- **R7.** A `write` to a placeholder or partial file not opened with `O_TRUNC` hydrates it
  fully before the write is applied.
- **R8.** `st_size` equals the cloud size for every file. A placeholder reports
  `st_blocks = 0`; a partial file reports fewer blocks than a full one of the same size.
- **R9.** The mount is created without `allow_other`, and the daemon never requires root or
  a capability.

## Revisit when

- G1 (Range on authenticated `/content`) or G3 (abraunegg PR #3600) results arrive.
- Unprivileged fanotify pre-content events land upstream.
- FUSE passthrough becomes available without `CAP_SYS_ADMIN`.
