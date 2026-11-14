# FUSE in CI

Can VFS tests mount a real FUSE filesystem in pull-request CI? ADR-0017 (layer 3) assumes
they can and leaves the answer to M0. This page records the answer and how the smoke test
that checks it is set up. Work package: WP-010.

## Findings

| Environment | Result | Observed |
|---|---|---|
| Developer machine (Arch-based, Linux 7.2, fuse3 3.18, unprivileged user) | Mounts, lists, reads and unmounts; no mount left behind | 2026-11-14 |
| GitHub-hosted runner (`ubuntu-latest`) | Mounts, lists, reads and unmounts; both tests pass | 2026-11-14 |

**Answer: GitHub-hosted runners can mount FUSE unprivileged.** VFS tests (ADR-0017 layer 3)
can run in pull-request CI on `ubuntu-latest` with no extra setup.

Observed in CI run 43600396871 (pull request #2), from the job logs:

- Runner image `ubuntu-26.04`, version 20261111.196.1 (Ubuntu 26.04.1 LTS), Azure, kernel
  `7.0.0-1019-azure`.
- Tests run as user `runner` (uid 1001), not root.
- `/dev/fuse` is present as `crw-rw-rw- root root 10, 229`.
- `/usr/bin/fusermount3` is present and setuid root, version 3.18.2. `fuse3` 3.18.2-1 is
  preinstalled in the image: `apt-get install fuse3` reported "already the newest version",
  so no install step is needed.
- Both tests passed, in two jobs: in the general "Format, lint, test" job
  (`cargo test --workspace --locked`, no apt install) and in a separate FUSE job that ran
  with `MYDRIVE_REQUIRE_FUSE=1` and `--nocapture`, which printed no `SKIPPED` line.
- That separate job failed at its final leftover-mount check, and not because of a mount:
  `findmnt -t <type>` exits 0 even when nothing matches (also true of util-linux 2.42.4
  locally), so `if findmnt -t fuse.mydrive-smoke` always took the error branch while
  printing no rows. The check now reads `/proc/self/mountinfo` instead, and the separate
  job has been folded into the general one (see below).

If a future runner image drops FUSE, the test fails rather than skips (see Skips) and the
alternatives at the end of this page apply.

## The test

`crates/mydrive-vfs/tests/fuse_smoke.rs` holds a read-only filesystem that exists only in the
test: a root directory containing one file with fixed content. Two tests use it.

- `mount_list_read_and_unmount` mounts it on a temporary directory, checks the mount is in
  `/proc/self/mountinfo`, lists the root, stats and reads the file, checks a write open is
  refused, unmounts, and checks the mount is gone and the mount point is empty.
- `panic_while_mounted_leaves_no_mount` mounts, opens and reads the file, panics, and checks
  after the panic that no mount is left.

How it works:

- `fuser` is built without its `libfuse` feature, so nothing links libfuse. It mounts by
  running the setuid `fusermount3` helper and receiving the `/dev/fuse` descriptor from it.
  `fuser` and `tempfile` are dev-dependencies of `mydrive-vfs` only.
- The mount is unprivileged and has no `allow_other`, as ADR-0005 requires of real mounts.
- Each mount sits in a guard that unmounts on drop, so a failing assertion or a panic still
  unmounts as the stack unwinds. The guard drops the session without waiting for its thread
  (so a busy session cannot hang the test) and, if the mount is still listed afterwards, runs
  `fusermount3 -u -z` as a last resort.
- `fuser::spawn_mount` returns only after the FUSE handshake, so the tests need no sleeps.
- A process killed with `SIGKILL` cannot run any destructor. A mount left that way shows as
  "Transport endpoint is not connected"; remove it with `fusermount3 -u -z <path>`.

### Skips

When `/dev/fuse` cannot be opened for reading and writing, or `fusermount3` is not on `PATH`,
each test prints a line starting `SKIPPED` to stderr (visible even when output is captured)
and passes. With `MYDRIVE_REQUIRE_FUSE=1` set, the same condition fails the test instead.

## The CI job

The smoke test runs in one place: the `rust` job ("Format, lint, test") in
`.github/workflows/ci.yml`, as part of `cargo test --workspace --locked`.

- The job sets `MYDRIVE_REQUIRE_FUSE=1`, so if the runner cannot mount, the test fails
  instead of skipping.
- Nothing is installed: the runner image already has `/dev/fuse` and `fusermount3`.
- A final step, which runs even if earlier steps failed, greps `/proc/self/mountinfo` for a
  `fuse.mydrive-smoke` filesystem and fails the job if one is left. It does not use
  `findmnt -t`, because that exits 0 when nothing matches.

## Running it locally

You need `/dev/fuse` and `fusermount3` (on Arch, `pacman -S fuse3`; on Debian and Ubuntu,
`apt install fuse3`). No libfuse headers are needed.

```sh
just test                                    # runs it with everything else
mise exec -- cargo test -p mydrive-vfs --test fuse_smoke -- --nocapture
MYDRIVE_REQUIRE_FUSE=1 mise exec -- cargo test -p mydrive-vfs --test fuse_smoke
```

The panic test prints a panic message marked "expected; part of the test". That is not a
failure. Afterwards, `grep ' - fuse\.mydrive-smoke ' /proc/self/mountinfo` should print
nothing.

## If hosted runners stop being able to mount

Not needed today. Options, roughly in order of preference, should a future runner image
lose FUSE support:

1. **Run the VFS jobs in a container with FUSE access** (`--device /dev/fuse --cap-add
   SYS_ADMIN`). This only helps if the host runner exposes `/dev/fuse`.
2. **A self-hosted runner** for VFS and crash tests (ADR-0017 layers 3 and 4). Keep it off
   pull requests from forks, since a self-hosted runner executes untrusted code.
3. **A VM-based job** (for example a QEMU guest started inside the job) that has its own
   kernel and FUSE module. Slower, but works where the host blocks FUSE.
4. **Run VFS tests outside pull-request CI**, nightly or before merge on a developer
   machine, and record that in ADR-0017 through a superseding ADR.

Any of these changes the test strategy, so it goes to the lead as an ADR change, not as a
workaround in this job.
