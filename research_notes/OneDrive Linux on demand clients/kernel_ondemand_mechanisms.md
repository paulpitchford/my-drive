# Linux kernel / filesystem mechanisms for "Files On-Demand" placeholder files (state: November 2026)

Researcher notes. Conventions: "VERIFIED" = read in a source this session; "UNVERIFIED/background" = from the researcher's prior knowledge and not re-confirmed against a source this session (kept out of Cited Findings, listed under Inferences or Gaps). The target machine reports `Linux 7.2.15-1-omarchy` (local `uname`, not a web source), so every kernel feature discussed below up to 7.2 is version-wise available on the first target; 7.3 was described as "in development" by Phoronix at the time of its article.

---

## 1. FUSE (libfuse3, fuser, go-fuse, cgofuse): how on-demand cloud filesystems use it, performance, passthrough, io_uring, hang/offline behaviour, unprivileged mounts, sandboxes, inotify

### Takeaway
FUSE remains the only mechanism that (a) works unprivileged via setuid `fusermount3`, (b) is independent of the backing filesystem, and (c) is what every shipping Linux on-demand product actually uses (rclone, onedriver, pCloud, SeaDrive, and since Nextcloud Desktop 35 the OpenCloud "openvfs" FUSE layer). Its two newest accelerators are of limited help to an unprivileged per-user daemon: FUSE passthrough still requires CAP_SYS_ADMIN, and FUSE-over-io_uring needs the `enable_uring` module parameter switched on by root; the hung-daemon problem is mitigated but not removed by the opt-in request timeouts added in 6.15.

### Cited Findings

**Unprivileged mounting and access rules**
- `mount()` is privileged, so "a helper program (fusermount) is needed", installed setuid root; fusermount adds `nosuid` and `nodev` to non-privileged mounts and only permits a mount if the owner can fully modify the mountpoint. — [kernel FUSE overview](https://docs.kernel.org/filesystems/fuse/fuse.html)
- By default only the mounting user's processes can access the mount (ptrace-style check); `allow_other` is root-only unless `user_allow_other` is set in the fuse config; `allow_sys_admin_access` exists for superuser access. Both relaxations are described as exposing the system to information leaks and DoS. — [kernel FUSE overview](https://docs.kernel.org/filesystems/fuse/fuse.html)
- On Ubuntu, rclone documents that a mount may fail with "Permission denied" from fusermount3 because of newer AppArmor restrictions (workaround given: `sudo aa-disable /usr/bin/fusermount3`). — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- systemd runs mount units without PATH/HOME; rclone falls back to `/bin:/usr/bin` to find `fusermount`/`fusermount3`; `Type=notify` makes the unit "started" only once the mountpoint is set up. — [rclone mount docs](https://rclone.org/commands/rclone_mount/)

**Hung daemon / offline behaviour**
- Interrupt semantics: if a request has not yet been sent to userspace, a fatal signal dequeues it and returns immediately; if it already reached userspace the kernel queues an INTERRUPT request, which the daemon "may ignore" or honour by replying EINTR to the original request. — [kernel FUSE overview](https://docs.kernel.org/filesystems/fuse/fuse.html)
- Hang detection and recovery: `/sys/fs/fuse/connections/<n>/waiting` non-zero with no activity means "the filesystem is hung or deadlocked"; writing to the `abort` file aborts the connection ("Most powerful method, always works"); `umount -f` is an alternative. — [kernel FUSE overview](https://docs.kernel.org/filesystems/fuse/fuse.html)
- FUSE has "forced" requests that do an unkillable wait in `request_wait_answer()`; these are released when the daemon goes away or the connection is aborted via fusectl; this "does wreak havoc in containers". — [LKML 2022 thread](https://lkml.org/lkml/2022/6/23/1607)
- Linux 6.15 added `fs.fuse.default_request_timeout` and `fs.fuse.max_request_timeout` sysctls (Joanne Koong). If the server does not reply within the timeout the connection is aborted. Both default to 0 (no timeout); existing servers are unaffected unless they opt in; upper bound 65535; a request may run roughly 15 s past the configured maximum because of the implementation. — [fuse update for 6.15 pull](https://lkml.rescloud.iu.edu/2504.0/00453.html); [commit 5e17b5c7 mirror](https://tangled.org/tjh.dev/kernel/commit/5e17b5c71729d8ce936c83a579ed45f65efcb456)
- Note the granularity: the 6.15 timeout aborts the whole connection, i.e. it is a "daemon is dead" safety net, not a per-read "return EIO because we are offline" mechanism. — [commit 5e17b5c7 mirror](https://tangled.org/tjh.dev/kernel/commit/5e17b5c71729d8ce936c83a579ed45f65efcb456)
- Nextcloud maintainer mgallien's objection (2021): "FUSE is not a good solution when network is involved" because the normal file API cannot cope with network errors. — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- rclone's own framing: "File systems expect things to be 100% reliable, whereas cloud storage systems are a long way from 100% reliable"; without a local cache "if an upload fails it can't be retried". — [rclone mount docs](https://rclone.org/commands/rclone_mount/)

**FUSE passthrough**
- Passthrough lets read/write/splice/mmap on a FUSE file go directly to a backing file; requires `CONFIG_FUSE_PASSTHROUGH` and negotiation in FUSE_INIT; the backing file is registered with the `FUSE_DEV_IOC_BACKING_OPEN` ioctl on the `/dev/fuse` fd. — [kernel fuse-passthrough docs](https://docs.kernel.org/filesystems/fuse/fuse-passthrough.html)
- "Setting up passthrough functionality currently requires the FUSE daemon to possess the CAP_SYS_ADMIN capability"; reasons given are dependency loops at shutdown/unmount (compared to LOOP_SET_FD), backing files invisible to lsof, and not counted against RLIMIT_NOFILE. Described as a temporary safeguard, but the requirement is still present in the 6.17 and 7.x documentation. — [kernel fuse-passthrough docs 6.17](https://kernel.org/doc/html/v6.17/filesystems/fuse-passthrough.html); [7.2 doc source](https://docs.kernel.org/7.2/_sources/filesystems/fuse/fuse-passthrough.rst.txt)

**FUSE over io_uring**
- Purpose: "increase fuse performance, by reducing kernel/userspace context switches"; early series reported xfstests generic/522 at ~2400 s on /dev/fuse vs ~1600 s patched (author caveat: run under ASAN/leak detection). — [linux-fsdevel RFC v2 thread](https://spinics.net/lists/linux-fsdevel/msg272677.html)
- LWN covered Bernd Schubert's v9 series in January 2025. — [LWN 1004358](https://lwn.net/Articles/1004358/)
- libfuse: enabled with `-o io_uring`, default queue depth 8; kernel side needs the fuse module parameter `enable_uring` turned on. — [libfuse README.fuse-io-uring](https://raw.githubusercontent.com/libfuse/libfuse/master/doc/README.fuse-io-uring)
- A Rust crate (`fractal-fuse`) describes itself as targeting "Linux 6.14+" for the io_uring transport. — [fractal-fuse on docs.rs](https://docs.rs/crate/fractal-fuse/latest/source/src/dispatch.rs)
- LSFMM+BPF 2026 (Zagreb; LWN article dated 3 Aug 2026): libfuse sets up eight entries per ring, each defaulting to the 8 MB maximum payload (switchable to 4 MB), wasting memory on small metadata requests; competing proposals from Bernd Schubert (single ring, sized payload pools) and Joanne Koong (multiple rings/buffer sizes); synchronous handling in libfuse causes head-of-line blocking; Schubert plans coroutine support in libfuse; session ended without a decision. — [LWN 1085618](https://lwn.net/Articles/1085618/)
- Linux 7.3 (in development at time of the article) adds io_uring buffer pools and zero-copy to the FUSE io_uring transport, reported as "nice performance improvements". — [Phoronix: Linux 7.3 FUSE](https://phoronix.com/news/Linux-7.3-FUSE)
- LWN commenters noted direct (non-libfuse) implementations such as the Rust `fuser` crate ("a rewrite of libfuse rather than bindings") must implement transport changes themselves. — [LWN 1085618](https://lwn.net/Articles/1085618/)

**Change notification (inotify) on FUSE**
- libfuse's entry-invalidation notify makes the kernel drop the dentry and re-ask the filesystem on next lookup/readdir; if it returns -ENOSYS the kernel is too old and the server must rely on short cache timeouts. — [libfuse notify_inval_entry example](https://sources.debian.org/data/main/f/fuse/2.9.9-9/doc/html/notify__inval__entry_8c_source.html)
- A 2021–2022 RFC "Inotify support in FUSE and virtiofs" proposed letting the FUSE kernel module receive remote inotify events; search found no evidence it was merged. — [LKML RFC](https://lkml.iu.edu/hypermail/linux/kernel/2110.3/03416.html)
- fanotify(7) states generally that for network filesystems remote events are not reported. — [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)
- rclone picks up remote changes only by polling (`--poll-interval`, default 1m, backend-dependent) or when `--dir-cache-time` (default 5m) expires; its docs do not mention inotify at all. — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- Same problem class on macOS: an rclone mount polled Drive and updated its database but changes were not pushed to FSEvents. — [rclone forum](https://forum.rclone.org/t/does-rclone-on-osx-publish-to-fsevents/8131)

**Sandbox visibility**
- xdg-document-portal itself is a FUSE filesystem (under the user runtime dir, `/run/user/<uid>/doc`) used to expose individual files to sandboxed apps. — [xdg-desktop-portal: Documents & FUSE](https://flatpak.github.io/xdg-desktop-portal/docs/documents-and-fuse.html)
- KDE's kio-fuse experience: FUSE mounts under a randomly named runtime directory make "working with flatpaks difficult"; users work around it with explicit `flatpak run --filesystem=xdg-run/kio-fuse-XXXX`. — [KDE bug 434917](https://bugs.kde.org/show_bug.cgi?id=434917)
- Snap: snap-confine setting up the mount namespace as root gets a permission error when a bind-mount source is inside a user FUSE filesystem (no `allow_root`/`allow_other`), whereas the same case works with Flatpak; Firefox-as-snap could not save to gvfs/SMB FUSE mount points. — [snapcraft forum](https://forum.snapcraft.io/t/can-interfaces-define-per-user-bind-mounts/501/18); [Mozilla bug 1774556](https://bugzilla.mozilla.org/show_bug.cgi?id=1774556)
- The Nextcloud Flatpak (as of Nov 2026) does not ship the FUSE-based VFS feature while the AppImage does; a Flathub request for VFS support is open. — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668); [flathub issue 273](https://github.com/flathub/com.nextcloud.desktopclient.nextcloud/issues/273)

**How on-demand filesystems use FUSE (patterns)**
- rclone `--vfs-cache-mode full`: all reads/writes buffered to disk in sparse cache files; "rclone will keep track of which bits of the files it has downloaded"; warns FAT/exFAT lack sparse support ("Rclone will perform very badly"); chunked ranged reads (`--vfs-read-chunk-size`, default 128M; parallel `--vfs-read-chunk-streams`); LRU eviction by `--vfs-cache-max-size`/`--vfs-cache-max-age` (default 1h), open files cannot be evicted; writes uploaded only on close, pending uploads resumed on next start. — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- openvfs (OpenCloud, used by Nextcloud 35): FUSE3 layer run against the synced directory, proxies `open()`/`readdir()`; "most files occupy zero bytes on disk"; pin state stored in xattrs; on `open()` of a non-local file it asks the desktop client (socket, `-s <socket-file>`) to download and blocks until the transfer finishes; no root needed; GPL-3.0; derived from LoggedFS; self-described "experimental". — [opencloud-eu/openvfs](https://github.com/opencloud-eu/openvfs)
- onedriver: "not a sync client - files are fetched on-demand and cached locally. Only files you actually use will be downloaded" (v0.15.0 man page, Jan 2026). — [onedriver(1), Debian](https://dyn.manpages.debian.org/testing/onedriver/onedriver.1.en.html)

### Inferences
- For an unprivileged per-user daemon, the realistic FUSE configuration in Nov 2026 is: classic `/dev/fuse` transport (or io_uring only where root has set `enable_uring`), kernel page cache for hydrated data, and no passthrough. Passthrough is only reachable via a small privileged helper (file capability `cap_sys_admin` on a helper binary, or a system service) — an architectural cost that should be optional, not required.
- Because passthrough is unavailable unprivileged, a design that keeps hydrated files as real files on ext4/btrfs/xfs and serves them through FUSE pays a FUSE round-trip (or page-cache hit) for every read of already-local data; the alternative that avoids this entirely is a non-FUSE design (section 2/3).
- The "blocked process" failure mode has three distinct layers the client must handle itself: (1) daemon alive but network down — the daemon must reply with an errno quickly (EIO/ENETUNREACH/etc.) or honour INTERRUPT; nothing in the kernel does this automatically; (2) daemon hung — only the 6.15 opt-in timeout or a fusectl abort helps, and both kill the whole mount; (3) daemon crashed — mountpoint becomes "Transport endpoint is not connected" until remounted (background knowledge, unverified this session).
- File managers and editors watching a FUSE mount with inotify only see changes made through that mount's local VFS path; remote-originated changes need either the daemon to perform a visible local operation or the file manager to re-list. (UNVERIFIED/background: `FUSE_NOTIFY_INVAL_ENTRY`/`FUSE_NOTIFY_DELETE` semantic differences regarding fsnotify events were not confirmed; one crate comment hints the delete notify exists "for deletes where applications may be watching".) A design in which the sync directory is a real directory on a real filesystem (stub files, or the "overmount a real dir" approach where the daemon writes through to the backing dir) gets native inotify for free for events that the daemon applies via the mount.
- go-fuse vs fuser vs libfuse3 vs cgofuse: this session found no comparative benchmark. The only hard differentiator found is that non-libfuse implementations (fuser, go-fuse) do not automatically inherit libfuse's io_uring/coroutine work.

### Gaps
- Exact merge release of FUSE passthrough (commonly cited as 6.9) and FUSE-over-io_uring (commonly cited as 6.14) were not confirmed from a primary release note this session; the 6.14 figure is corroborated only indirectly (fractal-fuse "Linux 6.14+", LWN Jan 2025 v9 coverage). Treat both version numbers as "very likely, not primary-verified".
- No source found on whether fuser / go-fuse support io_uring transport or passthrough ioctls as of Nov 2026.
- No quantitative benchmark found for FUSE vs native read throughput on current kernels, nor passthrough vs io_uring comparison.
- Status of FUSE BPF (Android-origin) and of the "backing inode / metadata passthrough" work upstream: nothing found; a 7.2 merge note only says io_uring and passthrough got their own MAINTAINERS entries.
- Whether Flatpak apps with `--filesystem=home` can see a FUSE mount located inside `$HOME` was not confirmed from a primary source (background expectation: yes, since it is the same user and the host mount is bind-propagated; portal-mediated access adds a second FUSE layer).
- `FOPEN_KEEP_CACHE`, `FOPEN_DIRECT_IO`, `writeback_cache`, entry/attr timeout tuning, and `fuse_lowlevel_notify_inval_inode/store/retrieve` specifics were not re-verified this session.

---

## 2. fanotify pre-content events (FAN_PRE_ACCESS / HSM hooks)

### Takeaway
FAN_PRE_ACCESS was merged in Linux 6.14-rc1 and is purpose-built for exactly this use case (sparse stub files filled on first read, including via mmap page faults) on ext4, btrfs and xfs — but it requires `FAN_CLASS_PRE_CONTENT`, which requires CAP_SYS_ADMIN, so it cannot be used by a plain unprivileged per-user daemon. Directory population (populate-on-lookup/readdir) was explicitly left out of the 6.14 merge; follow-up patches exist (April 2025 onward) but their merge status could not be confirmed.

### Cited Findings

**What was merged and when**
- The pre-content ("HSM") events feature "was merged to v6.14-rc1"; LTP test `fanotify24` covers FAN_PRE_ACCESS and FAN_DENY_ERRNO. — [LTP list, Amir Goldstein, Feb 2025](https://lists.linux.it/pipermail/ltp/2025-February/042129.html)
- The pull `fsnotify_hsm_for_v6.14-rc1` (Jan Kara) "introduces a new fsnotify event (FS_PRE_ACCESS) that gets generated before a file contents is accessed"; ~21 commits including "fanotify: introduce FAN_PRE_ACCESS permission event" and "fanotify: allow to set errno in FAN_DENY permission response". — [merge commit 8883957b mirror](https://mirrors.lzu.edu.cn/git/linux-nf/-/commit/8883957b3c9de2087fb6cf9691c1188cccf1ac9c)
- It slipped: SUSE's LPC 2024 slides had it staged for 6.12; Linus rejected an earlier pull, after which the series was respun with an `FMODE_NOTIFY_PERM` file flag (cheap check in the common no-watcher case) and per-superblock enablement. — [LPC slides](https://lpc.events/event/18/contributions/1717/attachments/1648/3404/fanotify.pdf); [LWN 998968 (v9 cover letter)](https://lwn.net/Articles/998968/)
- Man page: FAN_PRE_ACCESS "(since Linux 6.14)" creates an event "before read or write access to a file range, that provides an opportunity for the event listener to modify the content of the file before access to the content in the specified range"; "an fanotify file descriptor created with FAN_CLASS_PRE_CONTENT is required"; each event carries a `FAN_EVENT_INFO_TYPE_RANGE` record with `offset` and `count`. — [fanotify_mark(2)](https://man7.org/linux/man-pages/man2/fanotify_mark.2.html); [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)
- Error responses: a FAN_CLASS_PRE_CONTENT listener can deny with a chosen errno via `FAN_DENY_ERRNO(...)`; only EPERM, EIO, EBUSY, ETXTBSY, EAGAIN, ENOSPC, EDQUOT are allowed. — [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)
  - CONFLICT: the man page text says FAN_DENY_ERRNO is "since Linux 6.13", while the kernel merge that contains "allow to set errno in FAN_DENY permission response" is the 6.14-rc1 HSM pull. — [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html) vs [merge commit 8883957b](https://mirrors.lzu.edu.cn/git/linux-nf/-/commit/8883957b3c9de2087fb6cf9691c1188cccf1ac9c). Treat 6.14 as the safe minimum.
- Permission handling requires `CONFIG_FANOTIFY_ACCESS_PERMISSIONS`. — [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)

**Filesystems, mmap, restrictions**
- Validated/enabled filesystems: btrfs, xfs, ext4. bcachefs patch was dropped in v6 "after concerns that we were doing something wrong". Enablement is via an `FS_ALLOW_HSM`-style flag, moved to per-superblock; patch "fs: enable pre-content events on supported file systems". — [LWN 998968](https://lwn.net/Articles/998968/)
- mmap is covered: page-fault hooks in mm/filemap.c, mm/memory.c, mm/readahead.c fire only when a page has to be added; ext4/xfs DAX fault hooks added. — [LWN 998968](https://lwn.net/Articles/998968/)
- Trade-offs for watched files: readahead disabled; huge faults disabled ("entirely because it's untested"); btrfs defrag disabled. — [LWN 998968](https://lwn.net/Articles/998968/)
- "Data consistency relies on the daemon running": the design has a daemon watching the whole filesystem for FAN_PRE_ACCESS (and a planned FAN_PRE_MODIFY). — [LPC slides](https://lpc.events/event/18/contributions/1717/attachments/1648/3404/fanotify.pdf)
- A later (2025) change added a watchdog for permission events (hung listener detection). — [fs/notify git log, v6.18.y](https://git.radix-linux.su/kernel/linux.git/log/fs/notify?h=v6.18.9)

**Directories**
- In the 6.14 merge, "pre-content events are not currently generated for directories", and a mask combining pre-content events with directory-only flags is rejected. — [commit 4f8afa33](https://github.molgen.mpg.de/mariux64/linux/commit/4f8afa33817a6420398d1c177c6e220a05081f51)
- April 2025: Amir Goldstein posted "fanotify: allow creating FAN_PRE_ACCESS events on directories" (readdir generates pre-access events only if the listener opts in with FAN_ONDIR; removes the -EINVAL rejection); an "[RFC PATCH v2 1/3]" of the same followed. — [patchwork](https://patchwork.kernel.org/project/linux-fsdevel/patch/20250402062707.1637811-1-amir73il@gmail.com); [spinics v1](https://spinics.net/lists/linux-fsdevel/msg298003.html); [spinics RFC v2](https://spinics.net/lists/linux-fsdevel/msg302904.html)
- A kernel header comment (as surfaced in a search snippet of a later tree) says FAN_PRE_ACCESS "is only allowed with class FAN_CLASS_PRE_CONTENT and only allowed on regular files and dirs" — suggesting directory support did land at some point, but the exact tree/version could not be pinned. — [include/linux/fanotify.h log](https://git.zx2c4.com/wireguard-linux/log/include/linux/fanotify.h?showmsg=1)

**Privileges**
- fanotify_init(2): FAN_CLASS_PRE_CONTENT and FAN_CLASS_CONTENT — "Use of this flag requires the CAP_SYS_ADMIN capability." — [fanotify_init(2)](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)
- Unprivileged fanotify (since 5.13 / 5.10.220): "The user cannot request to use either notification classes FAN_CLASS_CONTENT or FAN_CLASS_PRE_CONTENT. This means that user cannot request permission events."; limited to inode marks, must use file-handle reporting (FAN_REPORT_FID), no mount/filesystem marks, no PID of the event originator. — [fanotify_init(2)](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)
- FAN_REPORT_FID cannot be combined with FAN_CLASS_CONTENT/PRE_CONTENT (EINVAL). — [fanotify_init(2)](https://man7.org/linux/man-pages/man2/fanotify_init.2.html)
- May 2025 work (Goldstein/Kara) adds limited mount/filesystem marks inside user namespaces, requiring CAP_SYS_ADMIN in the userns that created the group or owns the mntns — notification-class only, not pre-content. — [linux-api RFC](https://spinics.net/lists/linux-api/msg55627.html)
- No patch enabling FAN_CLASS_PRE_CONTENT for unprivileged users / user namespaces was found (searched Nov 2026).

**Known users / maturity**
- Josef Bacik: Meta ran these patches in production "for almost a year" (excluding the page-fault patch) with an internal utility for on-demand package fetching, with "pretty significant" savings; page-fault hooks needed for on-demand range fetching of multi-gigabyte executables (faster container start). — [LWN 998968](https://lwn.net/Articles/998968/)
- Proof-of-concept tools from Bacik: `populate` (creates stub files/dirs from a source tree), `remote-fetch` (fills content on access), `mmap-validate`. Kernel build in a stub tree: 10m6s vs 9m49s native (~17 s overhead); 125 MB vs 1.6 GB on disk after `make mrproper`. — [LWN 998968](https://lwn.net/Articles/998968/)
- Authors: Amir Goldstein (11 patches), Josef Bacik (6), Jan Kara (1 + maintainer), Al Viro (1). — [LWN 998968](https://lwn.net/Articles/998968/)

### Inferences
- How an HSM-style OneDrive daemon would use it: create each cloud file as a sparse file of the correct `st_size` (truncate, no blocks) with real mtime and an xattr holding item-id/eTag; place a FAN_PRE_ACCESS mark (inode marks on dehydrated files, or a filesystem/mount mark filtered by path); on an event, read the RANGE record, download that byte range (Graph supports ranged GETs), `pwrite` it into the file through the event fd (the listener's own writes do not recurse because the event fd is opened with FMODE_NONOTIFY — background knowledge), then respond FAN_ALLOW; when offline respond `FAN_DENY_ERRNO(EIO)` (ENETUNREACH/ENODATA are not in the allowed list). Once fully hydrated, remove the inode mark so subsequent access has zero overhead.
- This gives properties FUSE cannot: hydrated files are ordinary files on ext4/btrfs/xfs with native I/O speed, native inotify, no mountpoint, no sandbox-visibility issues, and the directory survives the daemon not running. The corresponding hazard is the one in the LPC slides: if the daemon is not running, a dehydrated file reads as zeros with no error — silent corruption for any reader (cp, backup, tar). A FUSE mount fails closed (ENOTCONN); a sparse-stub tree fails open. Mitigations would be needed (e.g. persistent marks do not exist; a boot-time system service must be up before user sessions; or stubs made unreadable `chmod 000` until the daemon marks them — speculative).
- Because CAP_SYS_ADMIN is mandatory, the only way to use this from a per-user client is a privileged system helper (root systemd service or a `cap_sys_admin` file-capability binary) that owns the fanotify group and forwards events to the user daemon over a socket. That is packaging-heavy, hostile to Flatpak/AppImage distribution, and a security-review burden (CAP_SYS_ADMIN listener receives open fds for arbitrary files on a filesystem mark).
- Without settled pre-dir-content support, directory trees must be fully materialised as stubs up front (one inode per cloud item). For large OneDrive/SharePoint libraries (hundreds of thousands of items) that costs inodes and initial enumeration time but is workable; FUSE can instead populate lazily on `lookup`/`readdir`.
- Filesystem coverage (ext4/btrfs/xfs) matches the stated targets but excludes bcachefs, f2fs, ZFS, NFS home directories, eCryptfs/fscrypt-on-other-fs etc. (only the three were stated as enabled in the 6.14 series; later additions unverified).
- Verdict: technically the closest Linux analogue to the Windows cfapi filter driver, but in Nov 2026 it is a root-only building block with one known large user (Meta, internal) and no known desktop sync client using it. Suitable as an optional "accelerated backend" later, not as the primary architecture for an unprivileged cross-distro client.

### Gaps
- Could not confirm whether/when FAN_PRE_ACCESS-on-directories (April 2025 patch / RFC v2) was merged, nor whether true pre-dir-content events (lookup/create/populate-directory hooks, sometimes discussed as FAN_PRE_DIR_*/FAN_PRE_MODIFY) exist upstream by 7.2. lore.kernel.org, patchwork and spinics fetches were blocked (403 / bot protection / connection refused). The term `FAN_PRE_DIR_ACCESS` returned no results at all.
- FAN_PRE_MODIFY status unknown (appeared in LPC 2024 design slides; not in current man pages fetched).
- Which mark types FAN_PRE_ACCESS accepts (inode vs mount vs filesystem) is not stated in fanotify_mark(2) as fetched.
- Whether additional filesystems gained `FS_ALLOW_HSM`-style enablement after 6.14 (e.g. bcachefs, f2fs, overlayfs) is unverified.
- No public open-source HSM daemon built on FAN_PRE_ACCESS (beyond Bacik's PoC and Amir Goldstein's earlier "httpdirfs"-style demos, the latter being background knowledge) was found; Amir Goldstein's HSM wiki/roadmap was not fetched.
- Behaviour of the permission-event watchdog (added 2025) — timeout values and what happens to blocked readers — not verified.

---

## 3. Other approaches (stub files + xattrs, symlinks, overlayfs, cachefiles on-demand/EROFS, userfaultfd, LD_PRELOAD, NFS/9p loopback, ublk, fs-specific features, eBPF/LSM, FUSE BPF): which are practical?

### Takeaway
Outside FUSE and fanotify pre-content, the only approach with real-world deployment for desktop sync is suffix stub files (Nextcloud `.nextcloud`, ownCloud `.owncloud`), which needs no kernel support and no privileges but is not transparent to applications. The kernel's cachefiles on-demand mode is container-image-specific, root-only, and its sole in-kernel consumer (EROFS over fscache) appears to be on its way out.

### Cited Findings
- **Suffix stub files:** on Linux, Nextcloud's unsynchronised virtual files have the suffix `.nextcloud`; enabled via `showExperimentalOptions=true` in `[General]` of `nextcloud.cfg`; maintainers called it "not considered stable enough for daily usage". ownCloud's wiki (2019): on macOS and Linux virtual files "appear as tiny files with a '.owncloud' extension". — [openSUSE README.vfs.md](https://src.opensuse.org/pool/nextcloud-desktop/src/branch/devel/README.vfs.md); [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668); [ownCloud wiki: Virtual Files](https://gh.r-mj.com/owncloud/client/wiki/Virtual-Files)
- Users object that suffix placeholders do not work from the shell or non-integrated apps (need a helper to "open" the placeholder). — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- **Custom xattr read by apps:** Nextcloud's 2021 plan was "to register a custom xattr and read that when opening files", which "would still need support from the reading program"; commenters noted "there is no universal xattr support so many tools and programs seem to strip them on save". — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- **cachefiles on-demand mode:** merged for 5.19 together with EROFS-over-fscache; requires `CONFIG_CACHEFILES_ONDEMAND`; on cache miss the kernel sends a request to a user daemon, which is "responsible for fetching data and then writing to the cache file"; reference users are a cachefilesd fork and the Nydus container image service. — [erofs 5.19-rc1 pull](https://lkml.rescloud.iu.edu/2205.2/08343.html); [cachefiles docs v5.19](https://kernel.org/doc/html/v5.19/filesystems/caching/cachefiles.html); [cachefiles.rst 7.1](https://docs.kernel.org/7.1/_sources/filesystems/caching/cachefiles.rst.txt)
- A later patch submission states "EROFS over fscache is now removed" and cleans up the netfs/fscache/cachefiles on-demand code (seen on syzbot CI; mainline status unconfirmed). — [syzbot CI patch page](https://ci.syzbot.org/patches/49d53e5f-e10a-4200-af71-4ac8783a0359)
- **bcachefs:** pre-content hook support for bcachefs was tested then dropped from the fanotify series. — [LWN 998968](https://lwn.net/Articles/998968/)
- **FUSE passthrough-style stacking as "overlay":** openvfs is run *against the synced directory* (derived from LoggedFS, a passthrough-logging FUSE fs), i.e. real files + xattrs on the backing fs, with a FUSE layer interposed for `open()`. — [opencloud-eu/openvfs](https://github.com/opencloud-eu/openvfs)

### Inferences
Practicality matrix (researcher judgement; rows other than those cited above rest on background knowledge and are UNVERIFIED this session):

| Mechanism | Transparent to apps | Unprivileged | Any backing fs | Verdict for a OneDrive client |
|---|---|---|---|---|
| FUSE (own namespace, cache elsewhere) | yes | yes (fusermount3) | yes | Practical; proven (rclone, onedriver, pCloud, SeaDrive) |
| FUSE stacked over real sync dir (openvfs/LoggedFS style; sparse/stub + xattrs on backing fs) | yes | yes | needs user xattrs (+ sparse for size-preserving stubs) | Practical; newest production approach (Nextcloud 35/OpenCloud) |
| fanotify FAN_PRE_ACCESS + sparse files | yes (incl. mmap) | no (CAP_SYS_ADMIN) | ext4/btrfs/xfs only | Best semantics, root helper required; optional backend |
| Suffix stub files (`.nextcloud`) | no | yes | yes | Fallback only; poor UX, breaks CLI |
| Symlinks to a FUSE/cache location | partially | yes | yes | Fragile (dangling links, apps replace link on save, wrong size/mtime via lstat) |
| overlayfs | n/a | needs userns/root | — | No on-demand hook; lower layer would still need FUSE — adds nothing |
| cachefiles on-demand / EROFS | read-only images | no (root, /dev/cachefiles) | — | Not applicable (immutable images; being removed) |
| userfaultfd | only for mappings the process itself registers | — | — | Not applicable to other processes' file I/O |
| LD_PRELOAD shim | no (static binaries, Go, direct syscalls, Flatpak) | yes | yes | Not viable |
| Loopback NFS/9p/SMB server | yes | mount needs root | yes | Worse than FUSE on every axis on Linux (root mount, hard-mount hangs); macOS-only trick |
| ublk / NBD block device | yes | no | fixed fs image | Wrong abstraction (block-level, no per-file hydration state) |
| eBPF LSM hooks | can deny, cannot sleep to fetch/fill content | no (CAP_BPF/CAP_SYS_ADMIN) | — | Only useful as a deny/filter adjunct (e.g. per-process blocking), not for hydration |
| FUSE BPF | — | — | — | Not upstream as far as found; ignore |
| btrfs/xfs/bcachefs native HSM features | — | — | — | None exist upstream that provide per-file recall (XFS DMAPI was never mainlined — background) |

- The "FUSE stacked over the real sync directory" pattern deserves special attention: metadata and hydrated data live as ordinary files on the user's filesystem (so the state survives daemon crashes and is inspectable with `getfattr`), and FUSE is only the interposition layer. It is also the natural seam for later swapping the interposition layer to fanotify pre-content (same on-disk format: sparse file + xattrs) when privileges are available.

### Gaps
- No primary sources were fetched for: userfaultfd scope, LD_PRELOAD limits, ublk, loopback NFS/9p, eBPF LSM sleepability, FUSE BPF upstream status. These rows are background knowledge and should be labelled as such in any report.
- Mainline status of the "EROFS over fscache removed" cleanup (which kernel release) not confirmed.
- openvfs's exact on-disk representation (sparse full-size file vs zero-length + `user.openvfs.realsize`) is only known from a third-party disassembly note (see section 4); INTEGRATION.md was not read.

---

## 4. How existing products implement on-demand on Linux; lessons and pain points

### Takeaway
Every Linux product that offers transparent on-demand files does it with FUSE; vendors without a FUSE layer (Dropbox, classic Nextcloud/ownCloud) simply do not offer transparent on-demand on Linux. The notable 2025–2026 development is OpenCloud's openvfs (FUSE + xattr pin states + socket to the sync client), which Nextcloud integrated in Desktop 35 (autumn 2026) — closing a five-year-old "VFS on Linux" issue.

### Cited Findings
- **Nextcloud Desktop (history):** 2021 position — Linux VFS "needs to have a proper support on KDE/Gnome first"; order of work Windows, then macOS, then Linux; "Linux doesn't have a standardized API for this like macOS and Windows do". — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668); [Nextcloud forum 2026](https://help.nextcloud.com/t/linux-desktop-client-doesnt-support-virtual-files/244807/10)
- **Nextcloud Desktop (current):** 22 Sep 2026 mgallien: "this is now done and available in master branch and future releases" via PR #10635 "feat(linux/vfs): integrate openvfs linux virtual files plugin"; issue closed as completed. Users report the option appearing in 35.0.x; AppImage (35+) includes it, Flatpak does not; requires `showExperimentalOptions=true` and `isVfsEnabled=true` plus per-folder enablement; seeing `.nextcloud` suffixes means the old placeholder backend is active, not openvfs. A separate issue reports "no vfs menu on linux, version 35.0.0". — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668); [PR #10635](https://github.com/nextcloud/desktop/pull/10635); [issue #11077](https://github.com/nextcloud/desktop/issues/11077); [flathub issue 273](https://github.com/flathub/com.nextcloud.desktopclient.nextcloud/issues/273)
- PR #10635's description noted that initial sync worked but "proper communication between the OpenVFS FUSE daemon and the desktop client" still needed investigation at the time of writing; later commits add Flatpak/AppImage build changes and make the plugin default on Linux. — [PR #10635](https://github.com/nextcloud/desktop/pull/10635)
- **openvfs (OpenCloud):** "A Virtual Filesystem Layer for cloud storages for the free desktop"; FUSE3; pin state in xattrs; blocks `open()` until the client has downloaded; unprivileged; files-on-demand concept proposed by OpenCloud in April 2025; based on LoggedFS; GPL-3.0; "experimental". — [opencloud-eu/openvfs](https://github.com/opencloud-eu/openvfs)
- Third-party reverse-engineering notes (not runtime-tested by their author) report xattrs `user.openvfs.data` and `user.openvfs.realsize`, a hydration socket at `/run/user/1000/Nextcloud/socket`, and that manually writing the PinState xattr is not a verified way to dehydrate. — [thunar-nextcloud-plugin-vfs issue 1](https://github.com/imsoka/thunar-nextcloud-plugin-vfs/issues/1); [issue 3](https://github.com/imsoka/thunar-nextcloud-plugin-vfs/issues/3)
- **ownCloud:** suffix `.owncloud` virtual files on Linux (2019 wiki); an ownCloud FUSE prototype (OCFFS) was mentioned in 2021. — [ownCloud wiki](https://gh.r-mj.com/owncloud/client/wiki/Virtual-Files); [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- **OneDrive, onedriver (jstaf, Go/FUSE):** on-demand FUSE filesystem, not a sync client; v0.15.0 man page Jan 2026. Reported issues include: files ≥100 MB "destroyed if manipulated" (#235), corruption when copying/accessing (#222, closed), KeePassXC databases corrupted on save (#217), mtime not updated on write (#201), missing files/folders (#215), Nautilus "glitchy typing performance" in an onedriver folder (#378), request to prune disk/memory caches (#50). A 2026 user report: caching is weak, browsing folders re-fetches from the cloud. — [onedriver(1)](https://dyn.manpages.debian.org/testing/onedriver/onedriver.1.en.html); [onedriver issues index](https://issues.ecosyste.ms/hosts/GitHub/repositories/jstaf%2Fonedriver/issues?page=2); [user thread](https://yomotherboard.com/question/looking-for-a-onedrive-client-for-linux-with-on-demand-download-and-caching)
- **OneDrive, abraunegg/onedrive (D):** a sync client; its README lists "Support for Windows 'On-Demand' functionality so file is only downloaded when accessed locally" under what's missing. — [abraunegg/onedrive README mirror](https://de.github.com/abraunegg/onedrive)
- Microsoft Q&A (Jan 2026) recommends abraunegg for sync and `rclone mount` for on-demand access ("not a sync; relies on the VFS cache and different reliability semantics"). — [Microsoft Q&A](https://learn.microsoft.com/en-ca/answers/questions/5691471/onedrive-download-on-linux-fail)
- **rclone:** FUSE mount with VFS cache modes off/minimal/writes/full; details in section 1. — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- **Dropbox:** community/staff forum replies over several years state Smart Sync / online-only files are Windows and macOS only; Linux gets selective sync, which "won't show the files at all". — [Dropbox forum thread](https://www.dropboxforum.com/t5/Dropbox-ideas/When-will-Smart-Sync-work-on-linux/idi-p/262171)
- **pCloud:** pCloud Drive on Linux is a FUSE virtual drive distributed as an AppImage; on Ubuntu 24.04+ users must install `libfuse2t64` manually (libfuse2 dependency); pclsync suggested as a reference implementation. — [pCloud install page](https://www.pcloud.com/how-to-install-pcloud-drive-linux); [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- **Seafile SeaDrive:** Linux drive client distributed only as AppImage since 3.0.12 (distro packages deprecated); a user reports it "works rather well, but only on a limited number of Linux distros". — [Seafile docs](https://help.seafile.com/drive_client/drive_client_for_linux); [Nextcloud forum](https://help.nextcloud.com/t/virtual-drive-for-linux/155906)
- **Wusel:** third-party Nextcloud virtual filesystem in Rust mounted over FUSE. — [Nextcloud forum 2026](https://help.nextcloud.com/t/hows-virtual-drive-support-in-2026-on-linux/242950)

### Inferences
- Lessons to carry into a new OneDrive client: (1) data-integrity bugs in the write path (large files, save-via-rename patterns used by KeePassXC/office apps, mtime handling) are the dominant complaint class for FUSE cloud filesystems — the write path should always land in a durable local file first and upload asynchronously (rclone "writes/full" model); (2) directory metadata must be cached persistently and served offline — re-fetching listings on browse is a top UX complaint; (3) AppImage + libfuse2 is a recurrent packaging failure — link libfuse3 or speak the FUSE protocol natively (fuser/go-fuse do not need libfuse at runtime except `fusermount3`); (4) Flatpak distribution of the client itself conflicts with running a FUSE daemon (Nextcloud's Flatpak lacks VFS).
- The Nextcloud/OpenCloud split of "FUSE interposer process" + "sync engine process" connected by a Unix socket in `$XDG_RUNTIME_DIR` is a de-facto emerging convention on the free desktop; a OneDrive client could either reuse openvfs (GPL-3.0 — licence implications) or mirror its xattr/pin-state contract to benefit from file-manager plugins being written for it (e.g. the Thunar plugin).

### Gaps
- No reliable sources found this session for: Synology Drive Client (Linux on-demand support), Infomaniak kDrive ("Lite Sync" on Linux), MEGA (MEGAsync/MEGAcmd FUSE mounts), Tresorit, Insync, google-drive-ocamlfuse, gcsf. Searches returned nothing relevant; these need separate lookup. (Background, UNVERIFIED: Synology and kDrive on-demand are Windows/macOS only; MEGAcmd added `fuse-add` FUSE mounts on Linux around 2024–2025; google-drive-ocamlfuse and gcsf are plain FUSE mounts; Insync has no placeholder files; Tresorit Drive is not offered on Linux.)
- Dropbox finding rests on forum posts, not current official documentation.
- The merged state/details of Nextcloud PR #10635 (default-on? which package formats?) are inconsistent across sources; the search tool also reported inconsistent dates. Distribution packages (e.g. Arch `nextcloud-client`) may not yet build the openvfs plugin — one Omarchy user reported still getting the old placeholder backend on 35.0.x.
- openvfs kernel requirements, use of passthrough, and its open issue list were not read.
- Whether ownCloud/OpenCloud desktop clients ship openvfs by default was not confirmed.

---

## 5. Reference semantics: Windows Cloud Files API and macOS File Provider; states a Linux implementation must replicate

### Takeaway
Both platforms separate two orthogonal axes that a Linux client must also model: *hydration state* (is content local: placeholder / partial / full) and *pin policy* (user intent: always keep / free up / unspecified), plus an *in-sync* flag and per-item error status; both also let the OS itself dehydrate/evict unpinned, in-sync items without the provider's involvement.

### Cited Findings
**Windows cfapi**
- `CF_PIN_STATE` values: UNSPECIFIED, PINNED, UNPINNED, EXCLUDED, INHERIT. Setting PINNED tells the sync provider to fetch content asynchronously; UNPINNED tells it to dehydrate asynchronously; EXCLUDED means the provider never syncs the item (only the provider can set it); INHERIT re-evaluates from the parent as if moved. Any application, not just the provider, may set pin state. — [CF_PIN_STATE](https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ne-cfapi-cf_pin_state)
- Dehydration fails if the file is pinned, and requires the file to be in sync. `CfUpdatePlaceholder` flags can mark a placeholder always-full (dehydration then fails) or allow partial. — [CfUpdatePlaceholder](https://learn.microsoft.com/windows/win32/api/cfapi/nf-cfapi-cfupdateplaceholder)
- `CF_PLACEHOLDER_STATE` flags include PLACEHOLDER, SYNC_ROOT, IN_SYNC, PARTIAL, PARTIALLY_ON_DISK (content not fully present locally). — [CF_PLACEHOLDER_STATE](https://learn.microsoft.com/windows/win32/api/cfapi/ne-cfapi-cf_placeholder_state)
- Hydration policy is per sync root (primary policy + modifiers); modifier AUTO_DEHYDRATION_ALLOWED lets the platform dehydrate in-sync placeholders without the provider. — [CF_SYNC_POLICIES](https://learn.microsoft.com/windows/win32/api/cfapi/ns-cfapi-cf_sync_policies)
- Overall model (sync root registration, placeholders, callbacks) documented in "Build a cloud file sync engine". — [Microsoft Learn](https://learn.microsoft.com/windows/win32/cfapi/build-a-cloud-file-sync-engine)

**macOS File Provider (NSFileProviderReplicatedExtension)**
- Items are dataless or materialized; when a user opens a dataless file "the system calls fetchContents(for:version:request:completionHandler:) and saves the file's content"; the system can convert materialized items back to dataless to free space, except items with pending changes not yet synced. — [Apple: Synchronizing the File Provider Extension](https://developer.apple.com/documentation/fileprovider/synchronizing-the-file-provider-extension)
- "The system will evict local files automatically and without involving your file provider extension when there is an urgent need for disk space." — [WWDC 2021 session 10182](https://developer.apple.com/videos/play/wwdc2021/10182)
- Provider-initiated eviction via `NSFileProviderManager.evictItem(withIdentifier:)`; it can fail and success must be checked. — [Apple developer forums](https://developer.apple.com/forums/thread/831371)
- Partial/ranged fetch exists (`NSFileProviderPartialContentFetching`). — [Apple developer forums](https://developer.apple.com/forums/thread/812794)

### Inferences
Minimum state model for a Linux implementation (synthesis):

| User-visible state | Hydration | Pin | In sync | Notes |
|---|---|---|---|---|
| Online-only | none (placeholder) | unspecified or unpinned | yes | size/mtime real; 0 blocks |
| Partially available | partial ranges | unspecified | yes | from ranged reads/streaming; show as online-only or "partially" |
| Locally available | full | unspecified | yes | eligible for automatic eviction (LRU / free-space pressure) |
| Always available | full (or hydrating) | pinned | yes | never auto-evicted; pin on folder inherited by children |
| Syncing (download) | hydrating | any | n/a | progress, cancellable |
| Syncing (upload) / modified | full | any | no | must never be dehydrated until in sync |
| Excluded | local only | excluded | n/a | never uploaded (temp files, lock files) |
| Error | any | any | no | per-item error code + message (quota, path too long, conflict, auth, offline) |

- Invariants worth copying: dehydrate only if in-sync and not pinned; pin state inherits from parent; pin changes are *requests* processed asynchronously by the engine; hydration is ranged where possible; the "OS" (here: the daemon's own eviction policy) may dehydrate unpinned in-sync files under space pressure.
- Linux has no platform component for sync-root registration or shell status icons; equivalents must be provided per file manager (Nautilus/Dolphin/Thunar extensions reading xattrs or talking to the daemon socket) — hence the value of a stable xattr contract.

### Gaps
- Full cfapi callback list (FETCH_DATA, VALIDATE_DATA, CANCEL_FETCH_DATA, FETCH_PLACEHOLDERS, NOTIFY_DEHYDRATE, etc.), hydration policy enum values (PARTIAL/PROGRESSIVE/FULL/ALWAYS_FULL) and population policy were not fetched this session (background knowledge only).
- Apple's content-policy options for "keep downloaded" (introduced in later macOS releases) and exact materialization flags were not verified.
- The OneDrive-specific mapping of these states to its UI icons was not researched here (adjacent researcher scope).

---

## 6. Persisting and exposing hydration/pin state; bad interactions (du/df, backup, indexers, thumbnailers, grep -r) and mitigations

### Takeaway
Current practice is converging on `user.*` xattrs on the item itself as the externally visible state contract (openvfs), backed by the sync engine's own database as source of truth; sparse-file block accounting is used for cached content (rclone). Evidence on indexers is that the mainstream desktop indexers already refuse FUSE/network mounts by default (Baloo hard-refuses; LocalSearch appears to skip non-local mounts), so the larger real-world risks are thumbnailers, recursive content tools (grep, backup, antivirus) and file-manager previews — for which no kernel-level mitigation exists and the daemon must implement policy itself.

### Cited Findings
- openvfs stores sync state as a "pin state" in xattrs on each file; third-party notes report names `user.openvfs.data` and `user.openvfs.realsize`. — [opencloud-eu/openvfs](https://github.com/opencloud-eu/openvfs); [thunar plugin notes](https://github.com/imsoka/thunar-nextcloud-plugin-vfs/issues/1)
- xattrs are fragile as the *only* store: "no universal xattr support so many tools and programs seem to strip them on save". — [nextcloud/desktop #3668](https://github.com/nextcloud/desktop/issues/3668)
- rclone full cache mode uses sparse files and separately "keep[s] track of which bits of the files it has downloaded"; files report full size while occupying only downloaded ranges; sparse-unsupported filesystems (FAT/exFAT) perform "very badly". — [rclone mount docs](https://rclone.org/commands/rclone_mount/)
- Bacik's fanotify PoC shows the du effect of sparse stubs: a 1.6 GB tree occupies 125 MB as an HSM stub tree. — [LWN 998968](https://lwn.net/Articles/998968/)
- Baloo (KDE) "refuses to index FUSE network mounts (e.g. rclone), with no opt-in override"; the folder appears configured but is never crawled. — [KDE Discuss](https://discuss.kde.org/t/baloo-refuses-to-index-fuse-network-mounts-e-g-rclone-with-no-opt-in-override/50530)
- LocalSearch (GNOME, formerly Tracker Miners) on Debian 13 did not index an SMB share mounted inside an indexed folder; suggested workaround was explicit `localsearch index --add --recursive`. — [StackExchange mirror](https://backiee.wasmer.app/https_unix_stackexchange_com/questions/801533/debian-13-localsearch-indexing-service-is-not-indexing-smb-share-on-a-synolog)
- rclone users who want content search over a mount are told to `rclone sync` everything locally instead; content-search/grep over a mount downloads data. — [rclone forum: search indexer and mount](https://forum.rclone.org/t/search-indexer-and-rclone-mount/20795); [rclone forum: file content search](https://forum.rclone.org/t/rclone-mount-file-content-search/26265)
- Error codes available to a fanotify-based implementation when offline are limited to EPERM, EIO, EBUSY, ETXTBSY, EAGAIN, ENOSPC, EDQUOT; a FUSE daemon can return any errno. — [fanotify(7)](https://man7.org/linux/man-pages/man7/fanotify.7.html)
- Unprivileged fanotify listeners do not receive the PID that generated an event; FUSE requests carry the caller's pid/uid in the request header (`fuse_in_header`), which is what per-process policies key on. — [fanotify_init(2)](https://man7.org/linux/man-pages/man2/fanotify_init.2.html) (first clause; second clause is background knowledge)
- Nautilus was reported to become glitchy inside an onedriver FUSE folder. — [onedriver issue index (#378)](https://issues.ecosyste.ms/hosts/GitHub/repositories/jstaf%2Fonedriver/issues?page=2)

### Inferences
- Recommended persistence layering (synthesis): (1) engine SQLite DB = source of truth (item id, eTag/cTag, parent, remote size/mtime, hydration ranges, pin state, sync status, error); (2) `user.*` xattrs mirrored onto the backing file / exposed virtually through FUSE `getxattr` = public read interface for file-manager extensions and scripts, and public write interface for pin requests (`setfattr -n user.<client>.pin -v pinned`); (3) sparse file extents (`SEEK_HOLE`/`SEEK_DATA`, `st_blocks`) = physical cache, never trusted alone because holes are indistinguishable from legitimately zero regions and filesystems may allocate/merge extents. With FUSE the daemon can synthesise xattrs without the backing fs supporting them at all.
- `st_size` should be the remote size and `st_blocks` should reflect locally present bytes, so `ls -l` shows true size while `du` shows actual local usage (the Windows "size" vs "size on disk" distinction); `du --apparent-size` then shows cloud size. `statfs` (df) on a FUSE mount should report something sensible — either the cloud quota or the cache filesystem's free space; applications check free space before writing, so reporting cloud quota remaining is usually the least surprising.
- Backup tools (rsync, borg, restic, tar, Timeshift, btrfs send for the cache dir) will hydrate everything when reading through a FUSE mount, or silently back up zeros/holes when reading sparse stubs directly (fanotify design with daemon down, or backing-dir access behind a stacked FUSE). Mitigations: put the mount/cache under a path with a `CACHEDIR.TAG`/`.nobackup` marker; document exclusion; for stacked designs keep the backing directory private (mode 0700, hidden location) so only the FUSE view is reachable.
- Mitigation techniques available to a FUSE daemon (design options, largely unverified against existing implementations this session): per-request caller identification (pid → `/proc/<pid>/exe`/comm) with deny/allow lists so known crawlers (thumbnailers, `localsearch-extract`, `baloo_file_extractor`, `updatedb`, antivirus, `grep`, `rg`) get EIO/EACCES or zero-length reads on dehydrated files instead of triggering downloads — this is what Windows calls "hydration by known apps blocked" and macOS does via its dataless-fault policy for certain processes; a rate/volume circuit-breaker ("process X has triggered N hydrations in T seconds — pause and notify user"); honouring `O_NONBLOCK`/`RWF_NOWAIT` by returning EAGAIN is not something regular-file readers expect and is unlikely to help; when offline, fail `open()` or the first `read()` quickly with EIO/ENETDOWN-class errors rather than blocking; expose a mount option/xattr to mark the filesystem as remote so well-behaved tools skip it (Baloo already does for FUSE).
- Thumbnails: serve thumbnails for dehydrated items from Microsoft Graph's thumbnail endpoint into the freedesktop thumbnail cache (`~/.cache/thumbnails`, keyed by URI+mtime) proactively or via a file-manager extension, so thumbnailers never need file content; combine with denying known thumbnailer processes on dehydrated files. (Design inference; no existing Linux client was found doing this.)
- PID-based policy is unavailable in the unprivileged-fanotify world and only partially reliable in FUSE (PID is 0 for requests from other PID namespaces — e.g. Flatpak/bubblewrap sandboxes — background knowledge), so sandboxed apps cannot be reliably classified by name.

### Gaps
- No primary source found confirming exactly how LocalSearch/Tracker decides to skip FUSE or remote mounts, or whether GNOME's thumbnailer (via GIO `G_FILE_ATTRIBUTE_FILESYSTEM_REMOTE` / "preview on local files only" setting) skips a FUSE mount in `$HOME`. The commonly cited behaviour (Nautilus "Show thumbnails: Files on this computer only" keys off GIO's remote-filesystem detection, which for FUSE depends on the fs subtype) is UNVERIFIED.
- No source found documenting any Linux cloud client implementing process allow/deny lists for hydration, or populating the freedesktop thumbnail cache from server-side thumbnails. These remain design proposals, not observed practice.
- Did not verify how `cp --sparse`, rsync, or btrfs/xfs reflink and `fallocate(PUNCH_HOLE)` behave for dehydration (hole punching to dehydrate in place is the obvious mechanism; background knowledge only).
- No data found on plocate/updatedb defaults for FUSE (`PRUNEFS` typically includes `fuse.*` subtypes — unverified for the current Arch default).
- Whether `user.*` xattrs written through openvfs are stored on the backing file or synthesised was not confirmed.
