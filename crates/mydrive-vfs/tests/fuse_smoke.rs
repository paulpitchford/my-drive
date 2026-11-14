//! FUSE smoke test (WP-010).
//!
//! Proves that this environment can mount a FUSE filesystem unprivileged, read through it and
//! unmount it again, before any real filesystem code exists. The filesystem here is a fixed,
//! read-only tree that lives only in this test: a root directory holding one file.
//!
//! The test is skipped, not failed, when `/dev/fuse` or `fusermount3` is unavailable. Set
//! `MYDRIVE_REQUIRE_FUSE=1` to turn that skip into a failure; CI does this so a runner that
//! cannot mount cannot pass silently. See `docs/process/ci-fuse.md`.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation,
    INodeNo, LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    Request,
};

/// Environment variable that turns "FUSE unavailable" from a skip into a failure.
const REQUIRE_ENV: &str = "MYDRIVE_REQUIRE_FUSE";

const FILE_NAME: &str = "hello.txt";
const FILE_CONTENT: &[u8] = b"my-drive FUSE smoke test\n";
const FILE_INO: INodeNo = INodeNo(2);
const TTL: Duration = Duration::from_secs(1);

/// A read-only filesystem with one directory (the root) holding one file.
struct SmokeFs {
    uid: u32,
    gid: u32,
}

impl SmokeFs {
    fn attr(&self, ino: INodeNo) -> Option<FileAttr> {
        let (kind, perm, nlink, size) = if ino == INodeNo::ROOT {
            (FileType::Directory, 0o555, 2, 0)
        } else if ino == FILE_INO {
            (FileType::RegularFile, 0o444, 1, FILE_CONTENT.len() as u64)
        } else {
            return None;
        };
        Some(FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
}

impl Filesystem for SmokeFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.attr(FILE_INO) {
            Some(attr) if parent == INodeNo::ROOT && name == FILE_NAME => {
                reply.entry(&TTL, &attr, Generation(0));
            }
            _ => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr(ino) {
            Some(attr) => reply.attr(&TTL, &attr),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        if ino != FILE_INO {
            reply.error(Errno::ENOENT);
            return;
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(FILE_CONTENT.len());
        let end = start
            .saturating_add(usize::try_from(size).unwrap_or(usize::MAX))
            .min(FILE_CONTENT.len());
        reply.data(&FILE_CONTENT[start..end]);
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if ino != INodeNo::ROOT {
            reply.error(Errno::ENOTDIR);
            return;
        }
        let entries = [
            (INodeNo::ROOT, FileType::Directory, "."),
            (INodeNo::ROOT, FileType::Directory, ".."),
            (FILE_INO, FileType::RegularFile, FILE_NAME),
        ];
        let skip = usize::try_from(offset).unwrap_or(usize::MAX);
        for (index, (entry_ino, kind, name)) in entries.into_iter().enumerate().skip(skip) {
            // The offset handed back is that of the next entry.
            if reply.add(entry_ino, (index + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

/// Why FUSE cannot be used here, or `None` if it can.
fn fuse_unavailable_reason() -> Option<String> {
    if let Err(err) = OpenOptions::new().read(true).write(true).open("/dev/fuse") {
        return Some(format!(
            "cannot open /dev/fuse for reading and writing: {err}"
        ));
    }
    if find_on_path("fusermount3").is_none() {
        return Some("fusermount3 was not found on PATH".to_owned());
    }
    None
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Returns true if the test should run. Skips loudly, or fails if `MYDRIVE_REQUIRE_FUSE=1`.
fn fuse_available_or_skip(test: &str) -> bool {
    let Some(reason) = fuse_unavailable_reason() else {
        return true;
    };
    if std::env::var_os(REQUIRE_ENV).is_some_and(|value| value == "1") {
        panic!("{test}: {REQUIRE_ENV}=1 but FUSE is unavailable: {reason}");
    }
    // Written to the real stderr, not through `eprintln!`, so the skip shows even when the
    // test harness captures output.
    let _ = writeln!(
        io::stderr().lock(),
        "SKIPPED {test}: {reason}. Install fuse3 or set {REQUIRE_ENV}=1 to make this a failure."
    );
    false
}

/// Undoes the escaping of spaces, tabs, newlines and backslashes in `/proc/self/mountinfo`.
fn unescape_mountinfo(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).and_then(|digits| {
            std::str::from_utf8(digits)
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        });
        match (bytes[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether anything is mounted at `mountpoint` (which must be canonical).
fn is_mounted(mountpoint: &Path) -> io::Result<bool> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    Ok(mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .any(|field| Path::new(&unescape_mountinfo(field)) == mountpoint))
}

/// A mounted [`SmokeFs`]. Unmounts on drop, including while a panic unwinds, so a failing
/// test leaves no mount behind.
struct MountGuard {
    session: Option<BackgroundSession>,
    mountpoint: PathBuf,
}

impl MountGuard {
    fn mount(mountpoint: &Path) -> io::Result<MountGuard> {
        let metadata = fs::metadata(mountpoint)?;
        let fs = SmokeFs {
            uid: metadata.uid(),
            gid: metadata.gid(),
        };
        let mut config = Config::default();
        // Read-only, and deliberately without allow_other (ADR-0005).
        config.mount_options = vec![
            MountOption::RO,
            MountOption::FSName("mydrive-smoke".to_owned()),
            MountOption::Subtype("mydrive-smoke".to_owned()),
        ];
        // `spawn_mount` returns only after the FUSE handshake, so the mount is ready.
        let session = fuser::spawn_mount(fs, mountpoint, &config).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("mount FUSE at {}: {err}", mountpoint.display()),
            )
        })?;
        Ok(MountGuard {
            session: Some(session),
            mountpoint: mountpoint.to_path_buf(),
        })
    }

    /// Unmounts and waits for the session thread, reporting any error from either.
    fn unmount(mut self) -> io::Result<()> {
        match self.session.take() {
            Some(session) => session.umount_and_join(),
            None => Ok(()),
        }
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        // Dropping the session unmounts through fusermount3 without joining its thread, so a
        // panic here cannot hang on a session that is still busy.
        drop(self.session.take());
        // If mountinfo cannot be read, assume the worst and try the lazy unmount anyway.
        if is_mounted(&self.mountpoint).unwrap_or(true) {
            // Last resort: a lazy unmount detaches it even if something still holds it open.
            let _ = Command::new("fusermount3")
                .arg("-u")
                .arg("-z")
                .arg("--")
                .arg(&self.mountpoint)
                .status();
        }
    }
}

fn temp_mountpoint() -> io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().canonicalize()?;
    Ok((dir, path))
}

/// Mounts unprivileged on a temporary directory, lists it, reads the file, unmounts cleanly.
#[test]
fn mount_list_read_and_unmount() {
    if !fuse_available_or_skip("mount_list_read_and_unmount") {
        return;
    }
    let (_dir, mountpoint) = temp_mountpoint().expect("create temporary mount point");
    assert!(
        !is_mounted(&mountpoint).expect("read mountinfo"),
        "mount point already in use"
    );

    let guard = MountGuard::mount(&mountpoint).expect("mount");
    assert!(
        is_mounted(&mountpoint).expect("read mountinfo"),
        "mount is not in /proc/self/mountinfo"
    );

    let names: Vec<_> = fs::read_dir(&mountpoint)
        .expect("list mount root")
        .map(|entry| entry.expect("read directory entry").file_name())
        .collect();
    assert_eq!(names, [FILE_NAME]);

    let file_path = mountpoint.join(FILE_NAME);
    let metadata = fs::metadata(&file_path).expect("stat file");
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), FILE_CONTENT.len() as u64);

    let mut content = Vec::new();
    fs::File::open(&file_path)
        .expect("open file")
        .read_to_end(&mut content)
        .expect("read file");
    assert_eq!(content, FILE_CONTENT);

    let write = OpenOptions::new().write(true).open(&file_path);
    assert!(write.is_err(), "read-only mount accepted a write open");

    guard.unmount().expect("unmount cleanly");
    assert!(
        !is_mounted(&mountpoint).expect("read mountinfo"),
        "mount left behind after unmount"
    );
    let leftover = fs::read_dir(&mountpoint)
        .expect("list mount point after unmount")
        .count();
    assert_eq!(leftover, 0, "mount point is not empty after unmount");
}

/// A test that panics while the filesystem is mounted, with a file open, leaves no mount.
#[test]
fn panic_while_mounted_leaves_no_mount() {
    if !fuse_available_or_skip("panic_while_mounted_leaves_no_mount") {
        return;
    }
    let (_dir, mountpoint) = temp_mountpoint().expect("create temporary mount point");

    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = MountGuard::mount(&mountpoint).expect("mount");
        let mut file = fs::File::open(mountpoint.join(FILE_NAME)).expect("open file");
        let mut first = [0_u8; 4];
        file.read_exact(&mut first).expect("read file");
        assert!(is_mounted(&mountpoint).expect("read mountinfo"));
        panic!("intentional panic while mounted (expected; part of the test)");
    }));

    assert!(outcome.is_err(), "the closure was meant to panic");
    assert!(
        !is_mounted(&mountpoint).expect("read mountinfo"),
        "panic left a mount behind"
    );
}
