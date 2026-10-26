//! The global lock and the per-transaction in-flight locks (vision §5.5, D23, D24).
//!
//! Liveness is proven only by held flocks. A flock belongs to one open file description
//! of one inode, and the kernel releases it when the holder exits: "free" proves the
//! holder is gone, "held" proves it is alive. The global lock file is therefore
//! immutable: created once, never truncated away, renamed or unlinked by any code path,
//! because a re-created file is a new inode and a second, independent lock. The holder
//! info written into it is for diagnosis only.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::scope::{Access, OwnedFolders, Refusal, ScopeTable, Writer};
use super::session::WriteSession;
use super::{FsxError, ensure_dir_all, failpoint, sys};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::{LOCK_ACQUIRE_TIMEOUT, PLAN_MAX_BYTES};
use crate::model::TxId;
use crate::state::paths::Paths;

/// Who holds the global lock, as written by the holder. Diagnostic only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HolderInfo {
    pub pid: u32,
    pub command: String,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
}

/// What a reader learns from probing the global lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    Free,
    /// Held by a live process; `holder` is its own description, if readable.
    Held {
        holder: Option<HolderInfo>,
    },
    /// The lock file does not exist yet (nothing has ever mutated).
    Missing,
}

/// The global lock (namespace).
pub struct GlobalLock;

/// Holds the global lock until dropped.
#[derive(Debug)]
pub struct GlobalLockGuard {
    _file: File,
    acquired_at: Instant,
}

impl GlobalLockGuard {
    /// How long the lock has been held; the engine targets < 50 ms per commit.
    pub fn held_for(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// Releases the lock now (dropping the guard does the same).
    pub fn release(self) {}
}

impl GlobalLock {
    /// Acquires the global lock, waiting up to `LOCK_ACQUIRE_TIMEOUT`.
    ///
    /// Not re-entrant: a second acquire in the same process waits on itself until the
    /// timeout. Pass the guard down instead of acquiring again.
    pub fn acquire(paths: &Paths, command: &str) -> Result<GlobalLockGuard> {
        Self::acquire_with_timeout(paths, command, LOCK_ACQUIRE_TIMEOUT)
    }

    /// Acquires the global lock, waiting up to `timeout`; then `locked` (exit 5,
    /// retryable) with the holder in `details`. The fix says to retry; the command
    /// layer replaces it with the caller's exact argv.
    pub fn acquire_with_timeout(
        paths: &Paths,
        command: &str,
        timeout: Duration,
    ) -> Result<GlobalLockGuard> {
        let path = paths.lock_file();
        let io =
            |err: &io::Error| Error::io(&path, err, Suggestion::command(["omaddons", "status"]));
        if let Some(dir) = path.parent() {
            sys::create_dir_all(dir).map_err(|err| io(&err))?;
        }
        // Read, write, create; never truncate. The file is opened, never replaced.
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(&path)
            .map_err(|err| io(&err))?;
        let started = Instant::now();
        let mut pause = Duration::from_millis(5);
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() >= timeout {
                        return Err(locked(&path, timeout));
                    }
                    std::thread::sleep(pause.min(timeout.saturating_sub(started.elapsed())));
                    pause = (pause * 2).min(Duration::from_millis(50));
                }
                Err(TryLockError::Error(err)) => return Err(io(&err)),
            }
        }
        let acquired_at = Instant::now();
        failpoint::hit("fsx.lock.acquired").map_err(|err| io(&err))?;
        write_holder(&mut file, command).map_err(|err| io(&err))?;
        failpoint::hit("fsx.lock.holder_written").map_err(|err| io(&err))?;
        Ok(GlobalLockGuard {
            _file: file,
            acquired_at,
        })
    }
}

/// Overwrites the holder info in place through the locked handle (same inode).
fn write_holder(file: &mut File, command: &str) -> io::Result<()> {
    let info = HolderInfo {
        pid: std::process::id(),
        command: command.to_owned(),
        started_at: Timestamp::now(),
    };
    let mut line = serde_json::to_vec(&info).map_err(io::Error::other)?;
    line.push(b'\n');
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&line)
}

fn read_holder(path: &Path) -> Option<HolderInfo> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim()).ok()
}

fn locked(path: &Path, timeout: Duration) -> Error {
    let holder = read_holder(path);
    let who = holder.as_ref().map_or_else(
        || "another omaddons process".to_owned(),
        |h| format!("omaddons {} (pid {})", h.command, h.pid),
    );
    Error::new(
        ErrorCode::Locked,
        format!(
            "{who} held the lock for more than {} ms",
            timeout.as_millis()
        ),
        Suggestion::text("wait for the other command to finish, then retry"),
    )
    .details(serde_json::json!({ "holder": holder }))
}

/// Probes the global lock without creating or writing anything: what readers use to
/// choose between `journal_pending` (free) and `tx_in_progress` (held).
pub fn probe_global(paths: &Paths) -> Result<LockState> {
    let path = paths.lock_file();
    let file = match File::open(&path) {
        Ok(file) => file,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(LockState::Missing),
        Err(err) => {
            return Err(Error::io(
                &path,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    match file.try_lock_shared() {
        Ok(()) => {
            drop(file.unlock());
            Ok(LockState::Free)
        }
        Err(TryLockError::WouldBlock) => Ok(LockState::Held {
            holder: read_holder(&path),
        }),
        Err(TryLockError::Error(err)) => Err(Error::io(
            &path,
            &err,
            Suggestion::command(["omaddons", "status"]),
        )),
    }
}

/// A transaction's liveness lock, held from planning until cleanup. Stage dirs, temp
/// blobs and plans are orphaned only when their transaction's lock is free or missing.
/// A transaction's lock lives in the state directory; a lock that guards only cache
/// scratch ([`InflightLock::create_scratch`]) lives in the cache, so planning, release
/// checks and dry runs leave the state directory alone.
#[derive(Debug)]
pub struct InflightLock {
    _file: File,
    tx: TxId,
}

impl InflightLock {
    /// Creates `<state>/inflight/<tx>.lock` (it must not exist) and holds it
    /// exclusively.
    pub fn create(paths: &Paths, tx: &TxId) -> Result<Self> {
        Self::create_at(&paths.inflight_dir(), paths.inflight_lock(tx), tx)
    }

    /// Creates `<cache>/inflight/<tx>.lock` for work that writes only cache scratch
    /// (`<cache>/tmp/<tx>`): the sweeps honour it like a transaction's lock.
    pub fn create_scratch(paths: &Paths, tx: &TxId) -> Result<Self> {
        Self::create_at(
            &paths.cache_inflight_dir(),
            paths.cache_inflight_lock(tx),
            tx,
        )
    }

    fn create_at(dir: &Path, path: PathBuf, tx: &TxId) -> Result<Self> {
        let io =
            |err: &io::Error| Error::io(&path, err, Suggestion::command(["omaddons", "status"]));
        sys::create_dir_all(dir).map_err(|err| io(&err))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
            .map_err(|err| io(&err))?;
        // Waits, never fails, on a holder: the file is new, so only a sweep's liveness
        // probe can hold it, and only for a moment.
        file.lock().map_err(|err| io(&err))?;
        Ok(Self {
            _file: file,
            tx: *tx,
        })
    }

    pub fn tx(&self) -> TxId {
        self.tx
    }

    /// Whether some live process holds `tx`'s lock, in the state directory or the cache.
    /// A missing file is not live.
    pub fn is_live(paths: &Paths, tx: &TxId) -> Result<bool> {
        Ok(Self::held(paths.inflight_lock(tx))? || Self::held(paths.cache_inflight_lock(tx))?)
    }

    fn held(path: PathBuf) -> Result<bool> {
        let file = match File::open(&path) {
            Ok(file) => file,
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(Error::io(
                    &path,
                    &err,
                    Suggestion::command(["omaddons", "status"]),
                ));
            }
        };
        match file.try_lock() {
            Ok(()) => {
                drop(file.unlock());
                Ok(false)
            }
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(err)) => Err(Error::io(
                &path,
                &err,
                Suggestion::command(["omaddons", "status"]),
            )),
        }
    }
}

/// A `flock` on a directory itself (M3-D48), through an `O_RDONLY | O_DIRECTORY` file
/// descriptor: no lock file is created, so a cache wipe cannot break exclusion, and the
/// kernel releases it when the holder exits, so a crashed reader never pins a backup.
/// Readers of a backup hold its directory shared; prune and the automatic-backup
/// scheduler take theirs exclusively, without waiting. Lock order: directory locks are
/// taken before the global lock and never waited on while holding it.
#[derive(Debug)]
pub struct DirLock {
    file: File,
}

impl DirLock {
    /// Holds `dir` shared, waiting while an exclusive holder has it. A symlink is
    /// refused.
    pub fn shared(dir: &Path) -> io::Result<Self> {
        let file = open_dir(dir)?;
        file.lock_shared()?;
        Ok(Self { file })
    }

    /// Holds `dir` exclusively when nobody else holds it; `None` when somebody does.
    pub fn try_exclusive(dir: &Path) -> io::Result<Option<Self>> {
        let file = open_dir(dir)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err),
        }
    }

    /// Whether `path` is still the directory this lock holds (same device and inode): a
    /// reader learns that its backup was moved or deleted under it.
    pub fn same_dir(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let (Ok(held), Ok(now)) = (self.file.metadata(), fs::symlink_metadata(path)) else {
            return false;
        };
        now.is_dir() && held.dev() == now.dev() && held.ino() == now.ino()
    }
}

/// `dir` opened for locking: a directory, never through a symlink.
fn open_dir(dir: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(dir, flags, Mode::empty())?))
}

/// A named advisory lock on a cache file: the plan store's admission lock
/// (`<cache>/plans/.lock`) and an applier's plan lease (`<cache>/plans/<id>.lease`, D5).
/// Never the global lock: work under it blocks no other command. The file is opened, or
/// created when missing, never created exclusively and never truncated away, so a crashed
/// holder leaves nothing that blocks: the kernel releases its flock. The holder writes
/// `{pid, command, started_at}` through the locked handle, for diagnosis only; nothing
/// trusts it unless the lock is held.
pub struct Lease;

/// Holds a lease until dropped.
#[derive(Debug)]
pub struct LeaseGuard {
    _file: File,
    path: PathBuf,
    acquired_at: Instant,
}

/// Why a lease was not taken.
#[derive(Debug)]
pub enum LeaseError {
    /// Held by another process for the whole wait.
    Busy {
        holder: Option<HolderInfo>,
        waited: Duration,
    },
    /// Not a lease this writer may take: outside the cache scope, or a symlink.
    Refused(String),
    Io(io::Error),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy { holder, waited } => {
                let who = holder.as_ref().map_or_else(
                    || "another omaddons process".to_owned(),
                    |h| format!("omaddons {} (pid {})", h.command, h.pid),
                );
                write!(f, "{who} held it for {} ms", waited.as_millis())
            }
            Self::Refused(why) => f.write_str(why),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Lease {
    /// Takes the lease at `path`, waiting at most `timeout` while another holder has it.
    pub fn acquire(
        scopes: &ScopeTable,
        path: &Path,
        command: &str,
        timeout: Duration,
    ) -> std::result::Result<LeaseGuard, LeaseError> {
        let (file, path) = open_lease(scopes, path)?;
        let started = Instant::now();
        let mut pause = Duration::from_millis(5);
        loop {
            match file.try_lock() {
                Ok(()) => return hold(file, path, command),
                Err(TryLockError::WouldBlock) => {
                    let waited = started.elapsed();
                    if waited >= timeout {
                        return Err(LeaseError::Busy {
                            holder: Self::holder(&path),
                            waited,
                        });
                    }
                    std::thread::sleep(pause.min(timeout.saturating_sub(waited)));
                    pause = (pause * 2).min(Duration::from_millis(50));
                }
                Err(TryLockError::Error(err)) => return Err(LeaseError::Io(err)),
            }
        }
    }

    /// Takes the lease at `path` when nobody holds it; `None` when somebody does.
    pub fn try_acquire(
        scopes: &ScopeTable,
        path: &Path,
        command: &str,
    ) -> std::result::Result<Option<LeaseGuard>, LeaseError> {
        let (file, path) = open_lease(scopes, path)?;
        match file.try_lock() {
            Ok(()) => hold(file, path, command).map(Some),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(LeaseError::Io(err)),
        }
    }

    /// What the lease's last holder wrote, read without following a symlink and at most
    /// `PLAN_MAX_BYTES`; trust it only while the lease is held, as when `acquire` reports
    /// [`LeaseError::Busy`].
    pub fn holder(path: &Path) -> Option<HolderInfo> {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(rustix::fs::open(path, flags, Mode::empty()).ok()?);
        let mut bytes = Vec::new();
        file.take(PLAN_MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > PLAN_MAX_BYTES {
            return None;
        }
        serde_json::from_slice(bytes.trim_ascii()).ok()
    }
}

impl LeaseGuard {
    /// How long the lease has been held.
    pub fn held_for(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// The lease file, canonical.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Releases it now (dropping does the same).
    pub fn release(self) {}
}

/// The lease file at `path`, opened (created 0600 when missing) after its location is
/// checked: an absolute, normalized path under the cache that a `Derived` writer may
/// write, never through a symlink. Nothing is created for a path refused.
fn open_lease(
    scopes: &ScopeTable,
    path: &Path,
) -> std::result::Result<(File, PathBuf), LeaseError> {
    use rustix::fs::{Mode, OFlags};
    let refuse = |why: &str| LeaseError::Refused(format!("{}: {why}", path.display()));
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(refuse("not a normalized absolute path"));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refuse("it names no file"));
    };
    let Some(parent) = super::prune::canonical_lenient(parent) else {
        return Err(refuse("its location cannot be resolved"));
    };
    if !parent.starts_with(scopes.cache_dir()) {
        return Err(refuse("a lease lives in the cache"));
    }
    let canonical = parent.join(name);
    scopes
        .check(
            Writer::Derived,
            &OwnedFolders::default(),
            &canonical,
            Access::Write,
        )
        .map_err(|refusal| match refusal {
            Refusal::ScopeDenied(why) | Refusal::Protected(why) => LeaseError::Refused(why),
        })?;
    let mut session = WriteSession::untracked(Writer::Derived, scopes, "lease");
    ensure_dir_all(&mut session, &parent).map_err(|err| match err {
        FsxError::Io { source, .. } => LeaseError::Io(source),
        other => LeaseError::Refused(other.to_string()),
    })?;
    // Read, write, create; never truncate, never through a symlink (ELOOP).
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file = match rustix::fs::open(&canonical, flags, Mode::RUSR | Mode::WUSR) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::LOOP) => return Err(refuse("a symlink is never a lease")),
        Err(err) => return Err(LeaseError::Io(err.into())),
    };
    if !file.metadata().map_err(LeaseError::Io)?.is_file() {
        return Err(refuse("not a regular file"));
    }
    Ok((file, canonical))
}

/// The guard of a lease just locked, its holder line written.
fn hold(
    mut file: File,
    path: PathBuf,
    command: &str,
) -> std::result::Result<LeaseGuard, LeaseError> {
    let acquired_at = Instant::now();
    write_holder(&mut file, command).map_err(LeaseError::Io)?;
    Ok(LeaseGuard {
        _file: file,
        path,
        acquired_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths::with_roots(
            dir,
            dir.join(".config"),
            dir.join(".local/state"),
            dir.join(".cache"),
        )
    }

    /// Polls `free` for up to 5 s. A lock this process released can stay held for a
    /// moment: a child another test spawns holds a copy of every descriptor until it
    /// execs, and a flock lasts until the last copy of its open file description closes.
    fn freed(mut free: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !free() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn holder_info_round_trips() {
        let info = HolderInfo {
            pid: 42,
            command: "install".into(),
            started_at: "2026-10-14T21:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(
            json,
            r#"{"pid":42,"command":"install","started_at":"2026-10-14T21:00:00Z"}"#
        );
        assert_eq!(serde_json::from_str::<HolderInfo>(&json).unwrap(), info);
    }

    #[test]
    fn probing_a_missing_lock_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        assert_eq!(probe_global(&p).unwrap(), LockState::Missing);
        assert!(!p.state_dir().exists());
    }

    #[test]
    fn acquire_writes_holder_info_and_release_frees() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let guard = GlobalLock::acquire(&p, "install").unwrap();
        let holder = read_holder(&p.lock_file()).unwrap();
        assert_eq!(
            (holder.pid, holder.command.as_str()),
            (std::process::id(), "install")
        );
        // A separate open file description sees the lock as held.
        assert!(matches!(
            probe_global(&p).unwrap(),
            LockState::Held { holder: Some(_) }
        ));
        assert!(guard.held_for() < Duration::from_secs(5));
        guard.release();
        assert!(freed(|| probe_global(&p).unwrap() == LockState::Free));
    }

    #[test]
    fn inflight_locks_are_live_while_held() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let tx = TxId::new();
        assert!(!InflightLock::is_live(&p, &tx).unwrap());
        let lock = InflightLock::create(&p, &tx).unwrap();
        assert_eq!(lock.tx(), tx);
        assert!(InflightLock::is_live(&p, &tx).unwrap());
        assert!(
            InflightLock::create(&p, &tx).is_err(),
            "a tx lock is created once"
        );
        drop(lock);
        assert!(freed(|| !InflightLock::is_live(&p, &tx).unwrap()));
    }

    #[test]
    fn no_code_path_truncates_renames_or_unlinks_the_lock_file() {
        use crate::source_scan::{Literals, production_code, sources};

        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut referencing = Vec::new();
        for (path, text) in sources(&src) {
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            if code.contains("lock_file") {
                referencing.push(
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .to_path_buf(),
                );
            }
        }
        referencing.sort();
        let expected: Vec<std::path::PathBuf> =
            ["src/fsx/lock.rs", "src/fsx/scope.rs", "src/state/paths.rs"]
                .iter()
                .map(Into::into)
                .collect();
        assert_eq!(
            referencing, expected,
            "only the lock, the deny-list and the path table name the lock file"
        );
        let code = production_code(include_str!("lock.rs"), Literals::Blank).unwrap();
        for forbidden in [
            ".truncate(true)",
            "remove_file",
            "rename(",
            "rename_with",
            "prune(",
        ] {
            assert!(
                !code.contains(forbidden),
                "src/fsx/lock.rs uses {forbidden}"
            );
        }
    }

    #[test]
    fn a_shared_dir_lock_blocks_an_exclusive_one_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup");
        fs::create_dir(&backup).unwrap();
        let reader = DirLock::shared(&backup).unwrap();
        let second = DirLock::shared(&backup).unwrap();
        assert!(DirLock::try_exclusive(&backup).unwrap().is_none());
        drop(reader);
        assert!(
            DirLock::try_exclusive(&backup).unwrap().is_none(),
            "the second reader still holds it"
        );
        drop(second);
        let mut held = None;
        assert!(freed(|| {
            held = DirLock::try_exclusive(&backup).unwrap();
            held.is_some()
        }));
        assert!(DirLock::try_exclusive(&backup).unwrap().is_none());
        drop(held);
        assert!(freed(|| DirLock::try_exclusive(&backup).unwrap().is_some()));
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no lock file");
        assert_eq!(fs::read_dir(&backup).unwrap().count(), 0);
    }

    #[test]
    fn same_dir_sees_a_moved_or_deleted_dir_and_symlinks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup");
        fs::create_dir(&backup).unwrap();
        let lock = DirLock::shared(&backup).unwrap();
        assert!(lock.same_dir(&backup));
        let moved = dir.path().join("moved");
        fs::rename(&backup, &moved).unwrap();
        assert!(!lock.same_dir(&backup), "gone from its path");
        assert!(lock.same_dir(&moved));
        fs::create_dir(&backup).unwrap();
        assert!(!lock.same_dir(&backup), "another dir at the path");
        fs::remove_dir(&moved).unwrap();
        assert!(!lock.same_dir(&moved), "deleted");

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&backup, &link).unwrap();
        assert!(DirLock::shared(&link).is_err());
        assert!(DirLock::try_exclusive(&dir.path().join("missing")).is_err());
        let file = dir.path().join("file");
        fs::write(&file, "").unwrap();
        assert!(DirLock::try_exclusive(&file).is_err(), "not a directory");
    }

    #[test]
    fn a_holder_that_dies_frees_the_dir_lock() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup");
        fs::create_dir(&backup).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "fsx::lock::tests::child_holds_a_dir_lock",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_FSX_TEST_PATH", dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = dir.path().join("ready");
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the child never locked"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            DirLock::try_exclusive(&backup).unwrap().is_none(),
            "held by the child"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            DirLock::try_exclusive(&backup).unwrap().is_some(),
            "freed by its death"
        );
    }

    #[test]
    #[ignore = "child process for a_holder_that_dies_frees_the_dir_lock"]
    fn child_holds_a_dir_lock() {
        let Some(dir) = std::env::var_os("OMADDONS_FSX_TEST_PATH").map(PathBuf::from) else {
            return;
        };
        let _lock = DirLock::shared(&dir.join("backup")).unwrap();
        fs::write(dir.join("ready"), "").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    // ---- Leases (M4-D24, M4-D61) -------------------------------------------------------

    const PLAN: &str = "01JB8Q2VA00000000000000000";

    /// Paths under `dir` made canonical, and the scope table of a machine with no client.
    fn scoped(dir: &Path) -> (Paths, ScopeTable) {
        let p = paths(&fs::canonicalize(dir).unwrap());
        let scopes = ScopeTable::new(&p, &[], None).unwrap();
        (p, scopes)
    }

    #[test]
    fn a_lease_is_created_and_reused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let (p, scopes) = scoped(dir.path());
        let path = p.plans_lock();
        assert!(!path.exists());
        let guard = Lease::acquire(&scopes, &path, "install", Duration::from_secs(1)).unwrap();
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "created owner-only"
        );
        let holder = Lease::holder(&path).unwrap();
        assert_eq!(
            (holder.pid, holder.command.as_str()),
            (std::process::id(), "install")
        );
        assert_eq!(guard.path(), path);
        assert!(guard.held_for() < Duration::from_secs(5));
        guard.release();
        let again = Lease::acquire(&scopes, &path, "update", Duration::from_secs(5)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            meta.ino(),
            "the same file, reused"
        );
        assert_eq!(Lease::holder(&path).unwrap().command, "update");
        drop(again);
    }

    #[test]
    fn a_held_lease_times_out_with_its_holder() {
        let dir = tempfile::tempdir().unwrap();
        let (p, scopes) = scoped(dir.path());
        let path = p.plan_lease(&PLAN.parse().unwrap());
        let _held = Lease::acquire(&scopes, &path, "apply", Duration::from_secs(1)).unwrap();
        let wait = Duration::from_millis(50);
        let result = std::thread::scope(|s| {
            s.spawn(|| Lease::acquire(&scopes, &path, "apply", wait))
                .join()
                .unwrap()
        });
        assert!(
            matches!(&result, Err(LeaseError::Busy { .. })),
            "{result:?}"
        );
        if let Err(LeaseError::Busy { holder, waited }) = result {
            assert_eq!(holder.map(|h| h.pid), Some(std::process::id()));
            assert!(waited >= wait, "{waited:?}");
        }
    }

    #[test]
    fn try_acquire_returns_none_while_held() {
        let dir = tempfile::tempdir().unwrap();
        let (p, scopes) = scoped(dir.path());
        let path = p.plan_lease(&PLAN.parse().unwrap());
        let held = Lease::try_acquire(&scopes, &path, "apply").unwrap();
        assert!(held.is_some());
        assert!(
            Lease::try_acquire(&scopes, &path, "apply")
                .unwrap()
                .is_none()
        );
        drop(held);
        assert!(freed(|| Lease::try_acquire(&scopes, &path, "apply")
            .unwrap()
            .is_some()));
    }

    #[test]
    fn a_lease_holder_that_dies_frees_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (p, scopes) = scoped(&root);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "fsx::lock::tests::child_holds_a_lease",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("OMADDONS_FSX_TEST_PATH", &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = root.join("ready");
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the child never took the lease"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let path = p.plans_lock();
        assert!(
            Lease::try_acquire(&scopes, &path, "apply")
                .unwrap()
                .is_none(),
            "held by the child"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            freed(|| Lease::try_acquire(&scopes, &path, "apply")
                .unwrap()
                .is_some()),
            "freed by its death"
        );
    }

    #[test]
    #[ignore = "child process for a_lease_holder_that_dies_frees_it"]
    fn child_holds_a_lease() {
        let Some(dir) = std::env::var_os("OMADDONS_FSX_TEST_PATH").map(PathBuf::from) else {
            return;
        };
        let (p, scopes) = scoped(&dir);
        let _lease =
            Lease::acquire(&scopes, &p.plans_lock(), "apply", Duration::from_secs(5)).unwrap();
        fs::write(dir.join("ready"), "").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn a_lease_outside_the_cache_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (p, scopes) = scoped(&root);
        let plans = p.plans_dir();
        fs::create_dir_all(&plans).unwrap();
        let file = root.join("file");
        fs::write(&file, "keep").unwrap();
        let missing = root.join("missing");
        let to_file = plans.join(format!("{PLAN}.lease"));
        std::os::unix::fs::symlink(&file, &to_file).unwrap();
        let to_missing = plans.join("01JB8Q2VA00000000000000001.lease");
        std::os::unix::fs::symlink(&missing, &to_missing).unwrap();
        let rows = [
            ("a lock in <state>", p.state_dir().join("x.lock")),
            ("the global lock file", p.lock_file()),
            ("a relative path", PathBuf::from("plans/x.lease")),
            ("a path with ..", plans.join("../x.lease")),
            ("a symlink to a file", to_file),
            ("a symlink to a missing file", to_missing),
        ];
        for (row, path) in rows {
            let result = Lease::acquire(&scopes, &path, "apply", Duration::from_millis(10));
            assert!(
                matches!(&result, Err(LeaseError::Refused(_))),
                "{row}: expected refused, got {result:?}"
            );
        }
        assert!(!p.state_dir().exists(), "nothing created in <state>");
        assert!(!p.cache_dir().join("x.lease").exists());
        assert!(!missing.exists(), "a link's missing target is not created");
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
    }
}
