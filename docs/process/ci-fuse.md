# FUSE in CI

Can VFS tests mount a real FUSE filesystem in pull-request CI? ADR-0017 (layer 3) assumes
they can and leaves the answer to M0. This page records the answer and how the smoke test
that checks it is set up. Work package: WP-010.

## Findings

| Environment | Result | Observed |
|---|---|---|
| Developer machine (Arch-based, Linux 7.2, fuse3 3.18, unprivileged user) | Mounts, lists, reads and unmounts; no mount left behind | 2026-11-14 |
| GitHub-hosted runner (`ubuntu-latest`) | **Pending.** The `FUSE smoke test` job has not run yet. | — |

The hosted-runner row is to be filled in from the first CI run of the `FUSE smoke test` job,
as observed fact: the run link, the runner image version from the job log, and whether the
test passed. If it fails, record the error the test printed and choose from the
alternatives below.

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

The `fuse` job ("FUSE smoke test") in `.github/workflows/ci.yml`:

1. installs `fuse3` with apt, which provides `fusermount3`;
2. prints the kernel version, the user, and the state of `/dev/fuse` and `fusermount3`, so a
   failure can be diagnosed from the log;
3. runs `cargo test -p mydrive-vfs --test fuse_smoke --locked -- --nocapture` with
   `MYDRIVE_REQUIRE_FUSE=1`, so a skip is a failure;
4. always checks with `findmnt` that no `fuse.mydrive-smoke` mount is left.

The `rust` job also runs the smoke test as part of `cargo test --workspace`, without
`MYDRIVE_REQUIRE_FUSE`. There it runs or skips depending on what the runner image has
installed; the `fuse` job is the one that decides whether FUSE works in CI.

## Running it locally

You need `/dev/fuse` and `fusermount3` (on Arch, `pacman -S fuse3`; on Debian and Ubuntu,
`apt install fuse3`). No libfuse headers are needed.

```sh
just test                                    # runs it with everything else
mise exec -- cargo test -p mydrive-vfs --test fuse_smoke -- --nocapture
MYDRIVE_REQUIRE_FUSE=1 mise exec -- cargo test -p mydrive-vfs --test fuse_smoke
```

The panic test prints a panic message marked "expected; part of the test". That is not a
failure. Afterwards, `findmnt -t fuse.mydrive-smoke` should print nothing.

## If hosted runners cannot mount

Options, roughly in order of preference, should the hosted-runner result be negative:

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
