//! Low-level filesystem primitives. Each one is atomic, or reports honestly that it
//! cannot be: flagged renames return [`RenameError::Unsupported`] rather than doing
//! something weaker, and [`clone_file`] says whether it reflinked or copied.
//!
//! These functions decide nothing about scope and log nothing; `mutate()` does. Each
//! hits the failpoint `fsx.<primitive>.before`, and those with two steps also hit
//! `fsx.<primitive>.after` between them.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rustix::fs::{CWD, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use super::failpoint;

/// Prefix of every temp name fsx creates. A crash can orphan one; the doctor (M4)
/// collects them.
pub(crate) const TEMP_PREFIX: &str = ".omaddons-";

/// The default mode of written files; credentials pass 0600.
const DEFAULT_FILE_MODE: u32 = 0o644;

/// How many durability calls this thread has made, for tests that assert staging's
/// one-`syncfs`, no-`fsync` contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncCounts {
    /// `fsync` of files and directories.
    pub fsync: u64,
    pub syncfs: u64,
}

thread_local! {
    static SYNC_COUNTS: std::cell::Cell<SyncCounts> = const {
        std::cell::Cell::new(SyncCounts { fsync: 0, syncfs: 0 })
    };
}

/// This thread's durability calls so far.
pub fn sync_counts() -> SyncCounts {
    SYNC_COUNTS.with(std::cell::Cell::get)
}

fn count(update: impl FnOnce(&mut SyncCounts)) {
    SYNC_COUNTS.with(|cell| {
        let mut counts = cell.get();
        update(&mut counts);
        cell.set(counts);
    });
}

/// `fsync(2)`, counted.
fn fsync(file: &File) -> io::Result<()> {
    count(|c| c.fsync += 1);
    file.sync_all()
}

/// The content of an atomic file write (`Op::WriteFile`).
pub enum Content<'a> {
    Bytes(&'a [u8]),
    /// Streamed into the temp file and hashed on the way (DEC-11).
    Reader(&'a mut dyn Read),
}

/// The SHA-256 and length of what an atomic file write wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteResult {
    pub sha256: [u8; 32],
    pub len: u64,
}

/// How a rename (`Op::Rename`) treats an existing target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameMode {
    /// Replace the target, like `rename(2)`.
    Replace,
    /// Fail if the target exists.
    NoReplace,
    /// Atomically swap the two paths; both must exist.
    Exchange,
}

/// Why a rename did not happen.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// The filesystem does not support `RENAME_NOREPLACE`/`RENAME_EXCHANGE` (`EINVAL`
    /// or `ENOSYS`). The caller falls back to two renames and reports
    /// `atomic_swap: false`.
    #[error("the filesystem does not support atomic exchange or no-replace renames")]
    Unsupported,
    #[error("the rename target already exists")]
    TargetExists,
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<RenameError> for io::Error {
    fn from(err: RenameError) -> Self {
        match err {
            RenameError::Unsupported => io::Error::new(io::ErrorKind::Unsupported, err.to_string()),
            RenameError::TargetExists => {
                io::Error::new(io::ErrorKind::AlreadyExists, err.to_string())
            }
            RenameError::Io(err) => err,
        }
    }
}

/// How a file clone (`Op::CloneFile`) copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneKind {
    /// `FICLONE`: instant and space-sharing.
    Reflink,
    /// A full byte copy, on filesystems without reflinks.
    ByteCopy,
}

/// Whether a write is made durable before it returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Durability {
    /// fsync the file and its directory.
    #[default]
    Immediate,
    /// Skip the fsyncs but keep temp + rename atomicity; the caller makes the whole
    /// filesystem durable later with one `syncfs` (staging only).
    Deferred,
}

/// Replaces `path` atomically: a unique temp file in the same directory, the content,
/// `mode` (default 0644), fsync, rename, then fsync of the directory (the two fsyncs
/// are skipped under [`Durability::Deferred`]). On any error the destination is
/// untouched and the temp file is removed.
pub(crate) fn write_atomic(
    path: &Path,
    content: Content<'_>,
    mode: Option<u32>,
    durability: Durability,
) -> io::Result<WriteResult> {
    failpoint::hit("fsx.write_atomic.before")?;
    let parent = parent_dir(path)?;
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(parent)?;
    let mut writer = HashingWriter::new(temp.as_file_mut());
    match content {
        Content::Bytes(bytes) => writer.write_all(bytes)?,
        Content::Reader(reader) => {
            io::copy(reader, &mut writer)?;
        }
    }
    let result = writer.finish();
    temp.as_file()
        .set_permissions(Permissions::from_mode(mode.unwrap_or(DEFAULT_FILE_MODE)))?;
    if durability == Durability::Immediate {
        fsync(temp.as_file())?;
    }
    failpoint::hit("fsx.write_atomic.after")?;
    temp.persist(path).map_err(|err| err.error)?;
    if durability == Durability::Immediate {
        fsync_dir(parent)?;
    }
    Ok(result)
}

/// Appends `bytes` with `O_APPEND` and fsyncs, creating the file if needed (and then
/// fsyncing its directory). A crash can leave a torn last line; readers tolerate it.
pub(crate) fn append_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    failpoint::hit("fsx.append_durable.before")?;
    let existed = fs::symlink_metadata(path).is_ok();
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(bytes)?;
    fsync(&file)?;
    if !existed {
        fsync_dir(parent_dir(path)?)?;
    }
    Ok(())
}

/// `renameat2(2)` with the given mode. `EXDEV` is a real error, never "unsupported":
/// fsx keeps stage, snapshots and AddOns on one filesystem by construction.
pub(crate) fn rename_with(from: &Path, to: &Path, mode: RenameMode) -> Result<(), RenameError> {
    failpoint::hit("fsx.rename_with.before")?;
    let flags = match mode {
        RenameMode::Replace => RenameFlags::empty(),
        RenameMode::NoReplace => RenameFlags::NOREPLACE,
        RenameMode::Exchange => RenameFlags::EXCHANGE,
    };
    let flagged = mode != RenameMode::Replace;
    if flagged && failpoint::enabled("fsx.renameat2.einval") {
        return Err(RenameError::Unsupported);
    }
    match rustix::fs::renameat_with(CWD, from, CWD, to, flags) {
        Ok(()) => Ok(()),
        Err(Errno::INVAL | Errno::NOSYS) if flagged => Err(RenameError::Unsupported),
        Err(Errno::EXIST) if mode == RenameMode::NoReplace => Err(RenameError::TargetExists),
        Err(errno) => Err(RenameError::Io(errno.into())),
    }
}

/// Copies the regular file `from` to the new path `to` (which must not exist), keeping
/// its mode: a reflink where the filesystem supports one, else a byte copy through a
/// temp file. Refuses anything but a regular file at `from`, symlinks included. The
/// fsyncs are skipped under [`Durability::Deferred`].
pub(crate) fn clone_file(from: &Path, to: &Path, durability: Durability) -> io::Result<CloneKind> {
    failpoint::hit("fsx.clone_file.before")?;
    let meta = fs::symlink_metadata(from)?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", from.display()),
        ));
    }
    let durable = durability == Durability::Immediate;
    let parent = parent_dir(to)?;
    if !failpoint::enabled("fsx.reflink.unsupported") {
        match reflink_copy::reflink(from, to) {
            Ok(()) => {
                if durable {
                    fsync(&File::open(to)?)?;
                    fsync_dir(parent)?;
                }
                return Ok(CloneKind::Reflink);
            }
            Err(err) if reflink_unsupported(&err) => {}
            Err(err) => return Err(err),
        }
    }
    let mut source = File::from(rustix::fs::open(
        from,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(parent)?;
    io::copy(&mut source, temp.as_file_mut())?;
    temp.as_file().set_permissions(meta.permissions())?;
    if durable {
        fsync(temp.as_file())?;
    }
    failpoint::hit("fsx.clone_file.after")?;
    temp.persist_noclobber(to).map_err(|err| err.error)?;
    if durable {
        fsync_dir(parent)?;
    }
    Ok(CloneKind::ByteCopy)
}

/// Errors that mean "this filesystem cannot reflink", as opposed to real failures.
fn reflink_unsupported(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Unsupported
        || matches!(
            Errno::from_io_error(err),
            Some(Errno::OPNOTSUPP | Errno::XDEV | Errno::INVAL | Errno::NOTTY | Errno::NOSYS)
        )
}

/// Points `link` at `target` atomically: a symlink at a unique temp name, renamed over
/// `link`, then fsync of the directory.
pub(crate) fn symlink_atomic(target: &Path, link: &Path) -> io::Result<()> {
    failpoint::hit("fsx.symlink_atomic.before")?;
    let dir = parent_dir(link)?;
    let temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .make_in(dir, |path| std::os::unix::fs::symlink(target, path))?;
    failpoint::hit("fsx.symlink_atomic.after")?;
    rename_with(temp.path(), link, RenameMode::Replace)?;
    // The temp name is gone after the rename; disarm its cleanup guard.
    drop(temp.into_temp_path().keep());
    fsync_dir(dir)
}

/// Makes renames and creations inside `dir` durable.
pub(crate) fn fsync_dir(dir: &Path) -> io::Result<()> {
    failpoint::hit("fsx.fsync_dir.before")?;
    fsync(&File::open(dir)?)
}

/// Creates one directory level, failing if it exists, with `mode` when given.
pub(crate) fn create_dir(path: &Path, mode: Option<u32>) -> io::Result<()> {
    create_dir_with(path, mode, Durability::Immediate)
}

/// [`create_dir`], with the parent fsync skipped under [`Durability::Deferred`].
pub(crate) fn create_dir_with(
    path: &Path,
    mode: Option<u32>,
    durability: Durability,
) -> io::Result<()> {
    failpoint::hit("fsx.create_dir.before")?;
    fs::create_dir(path)?;
    if let Some(mode) = mode {
        fs::set_permissions(path, Permissions::from_mode(mode))?;
    }
    if durability == Durability::Immediate {
        fsync_dir(parent_dir(path)?)?;
    }
    Ok(())
}

/// Creates `dir` and its missing ancestors one level at a time, tolerating directories
/// that appear meanwhile. For fsx's own bookkeeping dirs (backups, action logs, lock
/// dirs) only: callers have checked the location.
pub(crate) fn create_dir_all(dir: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut current = dir;
    while fs::symlink_metadata(current).is_err() {
        missing.push(current);
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    for dir in missing.into_iter().rev() {
        match create_dir(dir, None) {
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() != io::ErrorKind::AlreadyExists => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

/// Makes everything written on the filesystem holding `path` durable (`syncfs(2)`).
pub(crate) fn sync_filesystem(path: &Path) -> io::Result<()> {
    failpoint::hit("fsx.sync_filesystem.before")?;
    count(|c| c.syncfs += 1);
    rustix::fs::syncfs(File::open(path)?).map_err(io::Error::from)
}

/// Sets the mode of `path`, refusing to follow a symlink.
pub(crate) fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    failpoint::hit("fsx.chmod.before")?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to chmod through the symlink {}", path.display()),
        ));
    }
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// The directory containing `path`; `.` for a bare file name.
fn parent_dir(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(parent) => Ok(parent),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", path.display()),
        )),
    }
}

/// Hashes and counts everything written through it.
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    len: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            len: 0,
        }
    }

    fn finish(self) -> WriteResult {
        WriteResult {
            sha256: self.hasher.finalize().into(),
            len: self.len,
        }
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(buf.get(..written).unwrap_or(buf));
        self.len = self.len.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FsKind {
        Btrfs,
        Tmpfs,
        Other,
    }

    fn fs_kind(path: &Path) -> FsKind {
        match rustix::fs::statfs(path).map(|s| i128::from(s.f_type)) {
            Ok(0x9123_683E) => FsKind::Btrfs,
            Ok(0x0102_1994) => FsKind::Tmpfs,
            _ => FsKind::Other,
        }
    }

    /// A tempdir in the system temp dir (tmpfs here) and one under `target/` (btrfs
    /// here), so atomicity is exercised on both kinds of filesystem.
    fn dirs() -> Vec<(TempDir, FsKind)> {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        [
            tempfile::tempdir().unwrap(),
            tempfile::tempdir_in(target).unwrap(),
        ]
        .into_iter()
        .map(|dir| {
            let kind = fs_kind(dir.path());
            (dir, kind)
        })
        .collect()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn temp_leftovers(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(TEMP_PREFIX)
            })
            .collect()
    }

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    /// Re-runs this test binary with one `#[ignore]`d child test selected by name.
    fn run_child(test: &str, failpoints: &str, path: &Path) -> std::process::ExitStatus {
        Command::new(std::env::current_exe().unwrap())
            .args([
                test,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_FAILPOINTS", failpoints)
            .env("OMADDONS_FSX_TEST_PATH", path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
    }

    /// The path a `child_*` test acts on. They are child processes of the tests here,
    /// not checks: run on their own they do nothing.
    fn child_path() -> Option<PathBuf> {
        std::env::var_os("OMADDONS_FSX_TEST_PATH").map(PathBuf::from)
    }

    #[test]
    fn write_atomic_round_trips_with_modes() {
        for (dir, kind) in dirs() {
            for mode in [None, Some(0o600)] {
                let path = dir.path().join("file");
                let result =
                    write_atomic(&path, Content::Bytes(b"hello"), mode, Durability::Immediate)
                        .unwrap();
                assert_eq!(fs::read(&path).unwrap(), b"hello", "{kind:?}");
                assert_eq!(mode_of(&path), mode.unwrap_or(0o644), "{kind:?}");
                assert_eq!(
                    result,
                    WriteResult {
                        sha256: sha256(b"hello"),
                        len: 5
                    }
                );
                assert!(temp_leftovers(dir.path()).is_empty());
            }
        }
    }

    #[test]
    fn write_atomic_streams_and_hashes_reader_content() {
        let data: Vec<u8> = (0..1 << 20).map(|i: u32| (i % 251) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let mut reader = io::Cursor::new(data.clone());
        let result = write_atomic(
            &path,
            Content::Reader(&mut reader),
            None,
            Durability::Immediate,
        )
        .unwrap();
        assert_eq!(result.len, 1 << 20);
        assert_eq!(result.sha256, sha256(&fs::read(&path).unwrap()));
        assert_eq!(result.sha256, sha256(&data));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn write_atomic_failure_before_rename_keeps_the_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"old").unwrap();
        failpoint::with_spec("fsx.write_atomic.after=error", || {
            let err = write_atomic(&path, Content::Bytes(b"new"), None, Durability::Immediate)
                .unwrap_err();
            assert_eq!(err.to_string(), "failpoint fsx.write_atomic.after");
        });
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    // Env-driven failpoints exist only in debug builds.
    #[cfg(debug_assertions)]
    #[test]
    fn write_atomic_abort_before_rename_keeps_the_old_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"old").unwrap();
        let status = run_child(
            "fsx::sys::tests::child_write_atomic",
            "fsx.write_atomic.after=abort",
            &path,
        );
        assert_eq!(
            status.signal(),
            Some(6),
            "child should die by SIGABRT: {status:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"old");
        // A crash may orphan the temp file; that is documented and harmless.
        assert!(temp_leftovers(dir.path()).len() <= 1);
    }

    #[test]
    #[ignore = "child process for write_atomic_abort_before_rename_keeps_the_old_content"]
    fn child_write_atomic() {
        if let Some(path) = child_path() {
            let _ = write_atomic(&path, Content::Bytes(b"new"), None, Durability::Immediate);
        }
    }

    #[test]
    fn concurrent_writers_leave_one_complete_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("file"));
        let barrier = Arc::new(Barrier::new(2));
        let contents = [vec![b'a'; 64 * 1024], vec![b'b'; 32 * 1024]];
        let handles: Vec<_> = contents
            .iter()
            .cloned()
            .map(|content| {
                let (path, barrier) = (Arc::clone(&path), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    write_atomic(&path, Content::Bytes(&content), None, Durability::Immediate)
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        let result = fs::read(&*path).unwrap();
        assert!(
            contents.contains(&result),
            "torn result of {} bytes",
            result.len()
        );
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn rename_modes_on_each_filesystem() {
        for (dir, kind) in dirs() {
            let d = dir.path();
            let (a, b) = (d.join("a"), d.join("b"));
            fs::create_dir(&a).unwrap();
            fs::create_dir(&b).unwrap();
            fs::write(a.join("which"), "a").unwrap();
            fs::write(b.join("which"), "b").unwrap();

            rename_with(&a, &b, RenameMode::Exchange).unwrap();
            assert_eq!(
                fs::read_to_string(a.join("which")).unwrap(),
                "b",
                "{kind:?}"
            );
            assert_eq!(
                fs::read_to_string(b.join("which")).unwrap(),
                "a",
                "{kind:?}"
            );

            let err = rename_with(&a, &b, RenameMode::NoReplace).unwrap_err();
            assert!(matches!(err, RenameError::TargetExists), "{kind:?}: {err}");
            assert_eq!(fs::read_to_string(a.join("which")).unwrap(), "b");

            let c = d.join("c");
            rename_with(&a, &c, RenameMode::NoReplace).unwrap();
            assert!(!a.exists() && c.join("which").exists());

            let (f, g) = (d.join("f"), d.join("g"));
            fs::write(&f, "f").unwrap();
            fs::write(&g, "g").unwrap();
            rename_with(&f, &g, RenameMode::Replace).unwrap();
            assert_eq!(fs::read_to_string(&g).unwrap(), "f");
            assert!(!f.exists());
        }
    }

    #[test]
    fn exdev_is_an_error_not_unsupported() {
        let dirs = dirs();
        let [(tmp, tmp_kind), (target, target_kind)] = [&dirs[0], &dirs[1]].map(|(d, k)| (d, *k));
        if tmp_kind == target_kind {
            eprintln!("skipped: both tempdirs are on {tmp_kind:?}");
            return;
        }
        let from = tmp.path().join("x");
        fs::write(&from, "x").unwrap();
        let err = rename_with(&from, &target.path().join("x"), RenameMode::NoReplace).unwrap_err();
        assert!(matches!(err, RenameError::Io(_)), "{err}");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn einval_failpoint_makes_flagged_renames_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        failpoint::with_spec("fsx.renameat2.einval", || {
            for mode in [RenameMode::Exchange, RenameMode::NoReplace] {
                let err = rename_with(&a, &b, mode).unwrap_err();
                assert!(matches!(err, RenameError::Unsupported), "{mode:?}");
            }
            rename_with(&a, &b, RenameMode::Replace).unwrap();
        });
        assert_eq!(fs::read_to_string(&b).unwrap(), "a");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn einval_failpoint_is_read_from_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let status = run_child(
            "fsx::sys::tests::child_flagged_rename",
            "fsx.renameat2.einval",
            dir.path(),
        );
        assert!(status.success(), "{status:?}");
        // The marker proves the child test ran; a mistyped name would run nothing.
        assert!(dir.path().join("checked").exists());
    }

    #[test]
    #[ignore = "child process for einval_failpoint_is_read_from_the_environment"]
    fn child_flagged_rename() {
        if let Some(dir) = child_path() {
            let (a, b) = (dir.join("a"), dir.join("b"));
            fs::write(&a, "a").unwrap();
            fs::write(&b, "b").unwrap();
            let err = rename_with(&a, &b, RenameMode::Exchange).unwrap_err();
            assert!(matches!(err, RenameError::Unsupported), "{err}");
            fs::write(dir.join("checked"), "").unwrap();
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn clone_file_reflinks_where_it_can() {
        for (dir, kind) in dirs() {
            let from = dir.path().join("src");
            fs::write(&from, b"saved variables").unwrap();
            fs::set_permissions(&from, Permissions::from_mode(0o640)).unwrap();
            let to = dir.path().join("dst");
            let clone = clone_file(&from, &to, Durability::Immediate).unwrap();
            match kind {
                FsKind::Btrfs => assert_eq!(clone, CloneKind::Reflink),
                FsKind::Tmpfs => assert_eq!(clone, CloneKind::ByteCopy),
                FsKind::Other => eprintln!("note: {clone:?} on an unrecognized filesystem"),
            }
            assert_eq!(fs::read(&to).unwrap(), b"saved variables", "{kind:?}");
            assert_eq!(mode_of(&to), 0o640, "{kind:?}");

            let fallback = dir.path().join("fallback");
            let clone = failpoint::with_spec("fsx.reflink.unsupported", || {
                clone_file(&from, &fallback, Durability::Deferred).unwrap()
            });
            assert_eq!(clone, CloneKind::ByteCopy, "{kind:?}");
            assert_eq!(fs::read(&fallback).unwrap(), b"saved variables");
            assert_eq!(mode_of(&fallback), 0o640);

            let err = clone_file(&from, &to, Durability::Immediate).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{kind:?}");
            assert!(temp_leftovers(dir.path()).is_empty());
        }
    }

    #[test]
    fn clone_file_refuses_symlinks_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::write(&real, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        for from in [link.as_path(), dir.path()] {
            let err = clone_file(from, &dir.path().join("out"), Durability::Immediate).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::InvalidInput,
                "{}",
                from.display()
            );
        }
        assert!(!dir.path().join("out").exists());
    }

    #[test]
    fn symlink_atomic_replaces_an_existing_link() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("latest");
        symlink_atomic(Path::new("run-1"), &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("run-1"));
        symlink_atomic(Path::new("run-2"), &link).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("run-2"));
        assert!(temp_leftovers(dir.path()).is_empty());
    }

    #[test]
    fn create_dir_chmod_and_append() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        create_dir(&sub, Some(0o700)).unwrap();
        assert_eq!(mode_of(&sub), 0o700);
        assert_eq!(
            create_dir(&sub, None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );

        let log = sub.join("actions.jsonl");
        append_durable(&log, b"{\"a\":1}\n").unwrap();
        append_durable(&log, b"{\"a\":2}\n").unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "{\"a\":1}\n{\"a\":2}\n");

        chmod(&log, 0o600).unwrap();
        assert_eq!(mode_of(&log), 0o600);
        let link = sub.join("link");
        std::os::unix::fs::symlink(&log, &link).unwrap();
        assert_eq!(
            chmod(&link, 0o644).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(mode_of(&log), 0o600);
    }

    #[test]
    fn bare_file_names_resolve_to_the_current_directory() {
        assert_eq!(parent_dir(Path::new("file")).unwrap(), Path::new("."));
        assert_eq!(parent_dir(Path::new("/a/b")).unwrap(), Path::new("/a"));
        assert!(parent_dir(Path::new("/")).is_err());
    }
}
