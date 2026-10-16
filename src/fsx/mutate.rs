//! `mutate()`: the one function that changes the filesystem (vision §5.5, D26).
//!
//! Steps, in the doctor contract's order: (1) normalize the path, run the deny-list,
//! the scope table and the operation's preconditions; (2) before-digest; (3) verbatim
//! backup, verified byte-identical; (4) plan; (5) atomic execution through `fsx::sys`;
//! (6) after-digest; (7) an fsync'd `actions.jsonl` line. Untracked sessions skip
//! steps 2, 3, 6 and 7. Failpoints `fsx.mutate.{checked,backed_up,executed,logged}`
//! sit between the steps.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::failpoint;
use super::op::Op;
use super::scope::{Access, Refusal};
use super::session::{SessionKind, TxState, WriteSession};
use super::sys::{self, CloneKind, Durability, RenameError, RenameMode};
use crate::error::{Error, ErrorCode, Suggestion};
use crate::model::TxId;

/// Why `mutate()` did not change anything, or failed while doing so.
#[derive(Debug, thiserror::Error)]
pub enum FsxError {
    /// Outside every write scope, or on the deny-list (other than a protected folder).
    #[error("{0}")]
    ScopeDenied(String),
    /// Inside a `blizzard_*` AddOns folder.
    #[error("{0}")]
    Protected(String),
    /// The operation's precondition or the caller's `Expect` did not hold.
    #[error("{0}")]
    Precondition(String),
    /// The filesystem cannot do an atomic exchange or no-replace rename; the engine
    /// falls back to two renames under its journal.
    #[error("the filesystem does not support atomic exchange or no-replace renames")]
    Unsupported,
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

impl From<Refusal> for FsxError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ScopeDenied(message) => Self::ScopeDenied(message),
            Refusal::Protected(message) => Self::Protected(message),
        }
    }
}

impl From<FsxError> for Error {
    fn from(err: FsxError) -> Self {
        let status = || Suggestion::command(["omaddons", "status"]);
        match err {
            FsxError::ScopeDenied(message) => Error::new(ErrorCode::ScopeDenied, message, status()),
            FsxError::Protected(message) => Error::new(
                ErrorCode::ProtectedFolder,
                message,
                Suggestion::command(["omaddons", "list", "--protected"]),
            ),
            FsxError::Io { path, source } => Error::io(&path, &source, status()),
            other @ (FsxError::Precondition(_) | FsxError::Unsupported) => {
                Error::new(ErrorCode::Io, other.to_string(), status())
            }
        }
    }
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> FsxError + '_ {
    move |source| FsxError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A compare-and-swap precondition on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Nothing exists at the path.
    Absent,
    /// The path is a regular file with this SHA-256.
    Hash([u8; 32]),
}

/// Options for [`mutate_with`].
#[derive(Debug, Clone, Default)]
pub struct MutateOpts {
    pub expect: Option<Expect>,
    /// The tree digest of a directory at the path before the operation, from the
    /// caller's per-file index, so large folders need not be re-hashed.
    pub tree_before: Option<TreeDigest>,
    /// The same, for what is at the path afterwards (an exchange).
    pub tree_after: Option<TreeDigest>,
    /// [`Durability::Deferred`] is accepted only in untracked sessions, for
    /// `WriteFile`/`CreateDir` under a client's `Interface/.omaddons/stage/`.
    pub durability: Durability,
}

/// A directory digest: SHA-256 over the sorted lines
/// `<relative path>\0<size>\0<sha256 hex>\n` of its regular files. Symlinks and empty
/// directories do not contribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeDigest([u8; 32]);

impl TreeDigest {
    /// From index entries `(relative path with '/', size, sha256)`, in any order.
    pub fn from_entries<P: Into<String>>(
        entries: impl IntoIterator<Item = (P, u64, [u8; 32])>,
    ) -> Self {
        Self::from_hex_entries(
            entries
                .into_iter()
                .map(|(path, size, sha)| (path, size, hex(&sha))),
        )
    }

    /// The same, with the SHA-256s already in lowercase hex (the per-file index form).
    pub fn from_hex_entries<P: Into<String>, H: AsRef<str>>(
        entries: impl IntoIterator<Item = (P, u64, H)>,
    ) -> Self {
        let mut lines: Vec<(String, u64, H)> = entries
            .into_iter()
            .map(|(path, size, sha)| (path.into(), size, sha))
            .collect();
        lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut hasher = Sha256::new();
        for (path, size, sha) in lines {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(size.to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(sha.as_ref().as_bytes());
            hasher.update(b"\n");
        }
        Self(hasher.finalize().into())
    }

    /// Walks `dir` and hashes every regular file under it.
    pub fn compute(dir: &Path) -> io::Result<Self> {
        let mut entries = Vec::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            for entry in fs::read_dir(dir.join(&rel))? {
                let entry = entry?;
                let kind = entry.file_type()?;
                let rel_child = rel.join(entry.file_name());
                if kind.is_dir() {
                    pending.push(rel_child);
                } else if kind.is_file() {
                    let path = dir.join(&rel_child);
                    let size = entry.metadata()?.len();
                    entries.push((
                        rel_child.to_string_lossy().into_owned(),
                        size,
                        hash_file(&path)?,
                    ));
                }
            }
        }
        Ok(Self::from_entries(entries))
    }
}

/// `tree:sha256:<hex>`, the form recorded in `actions.jsonl` and the per-file index.
impl fmt::Display for TreeDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tree:sha256:{}", hex(&self.0))
    }
}

/// The state of a path that an operation changes, recorded before and after it:
/// `sha256:<hex>` (file content), `tree:sha256:<hex>` (directory), `len:<n>` (an
/// append), `mode:<octal>` (chmod) or `link:<target>` (symlink).
#[derive(Debug, Clone, PartialEq, Eq)]
enum StateDigest {
    File([u8; 32]),
    Tree(TreeDigest),
    Len(u64),
    Mode(u32),
    Link(PathBuf),
}

impl fmt::Display for StateDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(sha) => write!(f, "sha256:{}", hex(sha)),
            Self::Tree(tree) => tree.fmt(f),
            Self::Len(len) => write!(f, "len:{len}"),
            Self::Mode(mode) => write!(f, "mode:{mode:o}"),
            Self::Link(target) => write!(f, "link:{}", target.display()),
        }
    }
}

/// One `actions.jsonl` line, also returned by `mutate()`. Paths are stored lossily so a
/// line can always be written. `to` is set for renames; `before_hash`/`after_hash`
/// describe `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionRecord {
    /// 1-based within a transaction; 0 in untracked sessions.
    pub seq: u64,
    pub path: String,
    pub to: Option<String>,
    pub op: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    /// The verbatim backup of overwritten content, if any.
    pub backup: Option<String>,
    pub started_at_ns: u64,
    pub finished_at_ns: u64,
    pub tx: Option<TxId>,
    pub ok: bool,
    /// How a `CloneFile` copied: a reflink or a byte copy. Not logged.
    #[serde(skip)]
    pub clone: Option<CloneKind>,
}

/// What an executed operation reports back.
#[derive(Debug, Clone, Copy, Default)]
struct Executed {
    /// The written content's SHA-256 (`WriteFile`).
    sha256: Option<[u8; 32]>,
    clone: Option<CloneKind>,
}

/// [`mutate_with`] with default options.
pub fn mutate(s: &mut WriteSession<'_>, path: &Path, op: Op<'_>) -> Result<ActionRecord, FsxError> {
    mutate_with(s, path, op, MutateOpts::default())
}

/// Applies `op` to `path` under the session's writer, scopes and recording mode.
pub fn mutate_with(
    s: &mut WriteSession<'_>,
    path: &Path,
    op: Op<'_>,
    opts: MutateOpts,
) -> Result<ActionRecord, FsxError> {
    let started_at_ns = now_ns();
    let wire = op.wire();

    // 1. Scope, op and precondition checks.
    let target = normalize(path, matches!(op, Op::SymlinkAtomic { .. }))?;
    let access = match op {
        Op::CreateDir => Access::CreateDir,
        _ => Access::Write,
    };
    let matched = s.scopes.check(s.writer, &s.owned, &target, access)?;
    let to = match &op {
        Op::Rename { to, .. } => {
            let to = normalize(to, false)?;
            s.scopes.check(s.writer, &s.owned, &to, Access::Write)?;
            Some(to)
        }
        _ => None,
    };
    if opts.durability == Durability::Deferred {
        let deferrable = matches!(
            op,
            Op::WriteFile { .. } | Op::CreateDir | Op::CloneFile { .. }
        ) && matches!(s.kind, SessionKind::Untracked { .. })
            && s.scopes.is_stage(&target);
        if !deferrable {
            return Err(FsxError::Precondition(format!(
                "deferred durability is only for staging writes in untracked sessions, not {wire} on {}",
                target.display()
            )));
        }
    }
    preconditions(&target, to.as_deref(), &op, opts.expect.as_ref())?;
    failpoint::hit("fsx.mutate.checked").map_err(io_err(&target))?;

    let SessionKind::Tx(state) = &mut s.kind else {
        let executed = execute(&target, to.as_deref(), op, opts.durability)?;
        return Ok(ActionRecord {
            seq: 0,
            path: target.to_string_lossy().into_owned(),
            to: to.map(|to| to.to_string_lossy().into_owned()),
            op: wire.to_owned(),
            before_hash: None,
            after_hash: None,
            backup: None,
            started_at_ns,
            finished_at_ns: now_ns(),
            tx: None,
            ok: true,
            clone: executed.clone,
        });
    };
    if !s.scopes.is_internal(&state.actions_log) {
        return Err(FsxError::ScopeDenied(format!(
            "the actions log {} is outside the state and client meta dirs",
            state.actions_log.display()
        )));
    }
    let seq = state.next_seq();

    // 2. Before-digest.
    let before = digest(&target, &op, opts.tree_before).map_err(io_err(&target))?;

    // 3. Verbatim backup of content the operation overwrites.
    let backup_dir = state.snapshot_roots.dir_for(&matched.side, &state.tx);
    if !s.scopes.is_internal(&backup_dir) {
        return Err(FsxError::ScopeDenied(format!(
            "the backup dir {} is outside the state and client meta dirs",
            backup_dir.display()
        )));
    }
    let backup = backup(
        &backup_dir,
        seq,
        &target,
        to.as_deref(),
        &op,
        before.as_ref(),
    )?;
    failpoint::hit("fsx.mutate.backed_up").map_err(io_err(&target))?;

    // 4–5. Execute.
    let tree_after = opts.tree_after;
    let digest_op = DigestOp::of(&op);
    let executed = execute(&target, to.as_deref(), op, opts.durability).and_then(|written| {
        failpoint::hit("fsx.mutate.executed").map_err(io_err(&target))?;
        Ok(written)
    });

    // 6. After-digest. It is audit data only: if it cannot be read, the line still goes
    // out with a null after_hash rather than leaving an executed operation unlogged.
    let after = match &executed {
        Ok(Executed {
            sha256: Some(sha256),
            ..
        }) => Some(StateDigest::File(*sha256)),
        Ok(_) => digest_after(&target, digest_op, tree_after).ok().flatten(),
        Err(_) => None,
    };

    // 7. The actions line, written for failures too.
    let record = ActionRecord {
        seq,
        path: target.to_string_lossy().into_owned(),
        to: to.map(|to| to.to_string_lossy().into_owned()),
        op: wire.to_owned(),
        before_hash: before.map(|d| d.to_string()),
        after_hash: after.map(|d| d.to_string()),
        backup: backup.map(|b| b.to_string_lossy().into_owned()),
        started_at_ns,
        finished_at_ns: now_ns(),
        tx: Some(state.tx),
        ok: executed.is_ok(),
        clone: executed.as_ref().ok().and_then(|e| e.clone),
    };
    log_action(state, &record)?;
    executed?;
    failpoint::hit("fsx.mutate.logged").map_err(io_err(&target))?;
    Ok(record)
}

/// Creates every missing directory of `path`, one scope-checked `CreateDir` at a time,
/// so it can create `Interface/.omaddons/stage/<tx>/` but never an ancestor outside a
/// scope. A directory that appears meanwhile (another process, or this session's own
/// bookkeeping under `<state>`) is skipped.
pub fn ensure_dir_all(
    s: &mut WriteSession<'_>,
    path: &Path,
) -> Result<Vec<ActionRecord>, FsxError> {
    check_shape(path)?;
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(meta) if meta.is_dir() => break,
            Ok(_) => {
                return Err(FsxError::Precondition(format!(
                    "{} exists and is not a directory",
                    current.display()
                )));
            }
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    FsxError::Precondition(format!("no existing ancestor of {}", path.display()))
                })?;
            }
            Err(err) => return Err(io_err(current)(err)),
        }
    }
    let is_dir = |dir: &Path| fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
    let mut created = Vec::new();
    for dir in missing.iter().rev() {
        if is_dir(dir) {
            continue;
        }
        match mutate(s, dir, Op::CreateDir) {
            Ok(record) => created.push(record),
            Err(FsxError::Precondition(_) | FsxError::Io { .. }) if is_dir(dir) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(created)
}

/// Makes everything written on the filesystem holding `path` durable (`syncfs(2)`);
/// the staging bead calls it once before the journal's `prepared` record.
pub fn sync_filesystem(path: &Path) -> Result<(), FsxError> {
    sys::sync_filesystem(path).map_err(io_err(path))
}

/// Absolute, without `..`, with a final component.
fn check_shape(path: &Path) -> Result<(), FsxError> {
    if !path.is_absolute() {
        return Err(FsxError::ScopeDenied(format!(
            "{} is not absolute",
            path.display()
        )));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(FsxError::ScopeDenied(format!(
            "{} contains ..",
            path.display()
        )));
    }
    if path.file_name().is_none() {
        return Err(FsxError::ScopeDenied(format!(
            "{} names no entry",
            path.display()
        )));
    }
    Ok(())
}

/// The canonical parent joined with the final component. The final component must not
/// be an existing symlink unless `allow_symlink` (a `SymlinkAtomic` link path), which
/// keeps developer symlinks in AddOns immutable.
fn normalize(path: &Path, allow_symlink: bool) -> Result<PathBuf, FsxError> {
    check_shape(path)?;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(FsxError::ScopeDenied(format!(
            "{} names no entry",
            path.display()
        )));
    };
    let parent = fs::canonicalize(parent).map_err(|err| {
        // ubs:ignore — an io::ErrorKind, not a secret
        if err.kind() == io::ErrorKind::NotFound {
            FsxError::Precondition(format!("the parent of {} does not exist", path.display()))
        } else {
            io_err(parent)(err)
        }
    })?;
    let target = parent.join(name);
    if !allow_symlink && fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(FsxError::ScopeDenied(format!(
            "{} is a symlink, which fsx never writes through",
            target.display()
        )));
    }
    Ok(target)
}

fn preconditions(
    target: &Path,
    to: Option<&Path>,
    op: &Op<'_>,
    expect: Option<&Expect>,
) -> Result<(), FsxError> {
    let fail = |what: &str| {
        Err(FsxError::Precondition(format!(
            "{}: {what}",
            target.display()
        )))
    };
    let meta = fs::symlink_metadata(target).ok();
    let exists = meta.is_some();
    let is_file = meta.as_ref().is_some_and(fs::Metadata::is_file);
    match op {
        Op::WriteFile { .. } | Op::AppendFile { .. } if exists && !is_file => {
            return fail("exists and is not a regular file");
        }
        Op::CreateDir if exists => return fail("already exists"),
        Op::Rename { mode, .. } => {
            if !exists {
                return fail("does not exist");
            }
            let to_exists = to.is_some_and(|to| fs::symlink_metadata(to).is_ok());
            match mode {
                RenameMode::NoReplace if to_exists => return fail("the rename target exists"),
                RenameMode::Exchange if !to_exists => {
                    return fail("the exchange target does not exist");
                }
                _ => {}
            }
        }
        Op::CloneFile { from } => {
            if !fs::symlink_metadata(from).is_ok_and(|m| m.is_file()) {
                return Err(FsxError::Precondition(format!(
                    "{} is not a regular file",
                    from.display()
                )));
            }
            if exists {
                return fail("already exists");
            }
        }
        Op::Chmod { .. } if !exists => return fail("does not exist"),
        Op::SymlinkAtomic { .. }
            if exists && !meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) =>
        {
            return fail("exists and is not a symlink");
        }
        _ => {}
    }
    match expect {
        Some(Expect::Absent) if exists => fail("expected nothing at this path"),
        Some(Expect::Hash(expected)) => {
            let actual = if is_file {
                Some(hash_file(target).map_err(io_err(target))?)
            } else {
                None
            };
            // ubs:ignore — a content hash for compare-and-swap, not a secret
            if actual.as_ref() == Some(expected) {
                Ok(())
            } else {
                fail("its content changed (hash mismatch)")
            }
        }
        _ => Ok(()),
    }
}

/// What an operation records about its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestOp {
    Content,
    Append,
    Mode,
    Link,
}

impl DigestOp {
    fn of(op: &Op<'_>) -> Self {
        match op {
            Op::AppendFile { .. } => Self::Append,
            Op::Chmod { .. } => Self::Mode,
            Op::SymlinkAtomic { .. } => Self::Link,
            Op::WriteFile { .. } | Op::CreateDir | Op::Rename { .. } | Op::CloneFile { .. } => {
                Self::Content
            }
        }
    }
}

fn digest(path: &Path, op: &Op<'_>, tree: Option<TreeDigest>) -> io::Result<Option<StateDigest>> {
    digest_after(path, DigestOp::of(op), tree)
}

/// The state of `path` as the operation sees it; `None` when nothing is there.
fn digest_after(
    path: &Path,
    op: DigestOp,
    tree: Option<TreeDigest>,
) -> io::Result<Option<StateDigest>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    Ok(Some(match op {
        DigestOp::Append => StateDigest::Len(meta.len()),
        DigestOp::Mode => StateDigest::Mode(meta.permissions().mode() & 0o7777),
        DigestOp::Link | DigestOp::Content if meta.file_type().is_symlink() => {
            StateDigest::Link(fs::read_link(path)?)
        }
        DigestOp::Link => return Ok(None),
        DigestOp::Content if meta.is_dir() => StateDigest::Tree(match tree {
            Some(tree) => tree,
            None => TreeDigest::compute(path)?,
        }),
        DigestOp::Content => StateDigest::File(hash_file(path)?),
    }))
}

/// Clones overwritten content into `<backup_dir>/<seq>` and verifies it byte for byte
/// (by hash): the existing file under `WriteFile`, or the replaced target of a
/// `Rename Replace`. Directory renames need no copy: the engine renames the old folder
/// into its snapshot itself.
fn backup(
    backup_dir: &Path,
    seq: u64,
    target: &Path,
    to: Option<&Path>,
    op: &Op<'_>,
    before: Option<&StateDigest>,
) -> Result<Option<PathBuf>, FsxError> {
    let source = match op {
        Op::WriteFile { .. } => match before {
            Some(StateDigest::File(_)) => target,
            _ => return Ok(None),
        },
        Op::Rename {
            mode: RenameMode::Replace,
            ..
        } => match to {
            Some(to) if fs::symlink_metadata(to).is_ok_and(|m| m.is_file()) => to,
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let expected = hash_file(source).map_err(io_err(source))?;
    sys::create_dir_all(backup_dir).map_err(io_err(backup_dir))?;
    let copy = backup_dir.join(seq.to_string());
    sys::clone_file(source, &copy, Durability::Immediate).map_err(io_err(&copy))?;
    let actual = hash_file(&copy).map_err(io_err(&copy))?;
    // ubs:ignore — verifying a backup's content hash, not a secret
    if actual != expected {
        return Err(io_err(&copy)(io::Error::other(
            "the backup does not match the original",
        )));
    }
    Ok(Some(copy))
}

/// Runs the operation; reports the written content's SHA-256 for `WriteFile` and how
/// a `CloneFile` copied.
fn execute(
    target: &Path,
    to: Option<&Path>,
    op: Op<'_>,
    durability: Durability,
) -> Result<Executed, FsxError> {
    let io = io_err(target);
    let none = |()| Executed::default();
    match op {
        Op::WriteFile { content, mode } => sys::write_atomic(target, content, mode, durability)
            .map(|written| Executed {
                sha256: Some(written.sha256),
                clone: None,
            })
            .map_err(io),
        Op::AppendFile { content } => sys::append_durable(target, content).map(none).map_err(io),
        Op::CreateDir => sys::create_dir_with(target, None, durability)
            .map(none)
            .map_err(io),
        Op::Rename { mode, .. } => {
            let Some(to) = to else {
                return Err(FsxError::Precondition("a rename needs a target".to_owned()));
            };
            match sys::rename_with(target, to, mode) {
                Ok(()) => {}
                Err(RenameError::Unsupported) => return Err(FsxError::Unsupported),
                Err(RenameError::TargetExists) => {
                    return Err(FsxError::Precondition(format!("{} appeared", to.display())));
                }
                Err(RenameError::Io(err)) => return Err(io(err)),
            }
            for dir in [target.parent(), to.parent()].into_iter().flatten() {
                sys::fsync_dir(dir).map_err(io_err(dir))?;
            }
            Ok(Executed::default())
        }
        Op::CloneFile { from } => sys::clone_file(from, target, durability)
            .map(|kind| Executed {
                sha256: None,
                clone: Some(kind),
            })
            .map_err(io),
        Op::Chmod { mode } => sys::chmod(target, mode).map(none).map_err(io),
        Op::SymlinkAtomic {
            target: link_target,
        } => sys::symlink_atomic(link_target, target)
            .map(none)
            .map_err(io),
    }
}

fn log_action(state: &TxState, record: &ActionRecord) -> Result<(), FsxError> {
    let log = &state.actions_log;
    let mut line = serde_json::to_vec(record).map_err(|err| io_err(log)(io::Error::other(err)))?;
    line.push(b'\n');
    if let Some(dir) = log.parent() {
        sys::create_dir_all(dir).map_err(io_err(dir))?;
    }
    sys::append_durable(log, &line).map_err(io_err(log))
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(buf.get(..n).unwrap_or(&buf));
    }
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::scope::{OwnedFolders, ScopeTable, Writer};
    use crate::fsx::session::SnapshotRoots;
    use crate::fsx::sys::Content;
    use crate::model::FolderName;
    use crate::state::paths::Paths;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    const TX: &str = "01JB8Q2VA00000000000000000";

    struct Fixture {
        _dir: TempDir,
        paths: Paths,
        client: PathBuf,
        scopes: ScopeTable,
        tx: TxId,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(dir.path()).unwrap();
            let home = root.join("home");
            let paths = Paths::with_roots(
                &home,
                home.join(".config"),
                home.join(".local/state"),
                home.join(".cache"),
            );
            let client = root.join("games/wow/_retail_");
            for d in [
                "Interface/AddOns/Blizzard_AuctionUI",
                "Interface/AddOns/WeakAuras",
                "Data",
                "WTF/Account/A/SavedVariables",
            ] {
                fs::create_dir_all(client.join(d)).unwrap();
            }
            fs::write(client.join("Wow.exe"), b"MZ").unwrap();
            fs::write(
                client.join("Interface/AddOns/Blizzard_AuctionUI/x.lua"),
                b"--",
            )
            .unwrap();
            fs::write(
                client.join("WTF/Account/A/SavedVariables/WeakAuras.lua"),
                b"WA = {}",
            )
            .unwrap();
            fs::create_dir_all(&home).unwrap();
            let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
            Self {
                _dir: dir,
                paths,
                client,
                scopes,
                tx: TX.parse().unwrap(),
            }
        }

        fn tx_session(&self, writer: Writer) -> WriteSession<'_> {
            WriteSession::tx(
                self.tx,
                writer,
                &self.scopes,
                SnapshotRoots::for_tx(&self.paths, &self.tx),
                self.actions_log(),
            )
        }

        fn actions_log(&self) -> PathBuf {
            self.paths.state_snapshot(&self.tx).join("actions.jsonl")
        }

        fn actions(&self) -> Vec<ActionRecord> {
            fs::read_to_string(self.actions_log())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }

        fn stage(&self) -> PathBuf {
            self.client.join("Interface/.omaddons/stage").join(TX)
        }
    }

    fn write<'a>(bytes: &'a [u8]) -> Op<'a> {
        Op::WriteFile {
            content: Content::Bytes(bytes),
            mode: None,
        }
    }

    fn sha(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    #[test]
    fn every_op_in_a_tx_session_logs_one_line() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        let record = state.join("targets/key.json");

        // `<home>/.local` is created through the XDG-base rule; the session's own action
        // log then creates `<state>/omaddons`, which ensure_dir_all skips.
        let created = ensure_dir_all(&mut s, &state.join("targets")).unwrap();
        assert!(
            created.first().unwrap().path.ends_with("/.local"),
            "{created:?}"
        );
        assert!(
            created.last().unwrap().path.ends_with("/targets"),
            "{created:?}"
        );
        assert!(state.join("targets").is_dir());

        let first = mutate(&mut s, &record, write(b"v1")).unwrap();
        assert_eq!(first.before_hash, None);
        assert_eq!(
            first.after_hash,
            Some(format!("sha256:{}", hex(&sha(b"v1"))))
        );
        assert_eq!(first.backup, None);

        let second = mutate(&mut s, &record, write(b"v2")).unwrap();
        let backup = PathBuf::from(second.backup.clone().unwrap());
        assert_eq!(fs::read(&backup).unwrap(), b"v1", "byte-identical backup");
        assert!(backup.starts_with(f.paths.state_snapshot(&f.tx).join("files")));
        assert_eq!(second.before_hash, first.after_hash);

        let history = state.join("history.jsonl");
        let append = mutate(&mut s, &history, Op::AppendFile { content: b"{}\n" }).unwrap();
        assert_eq!(
            (append.before_hash.as_deref(), append.after_hash.as_deref()),
            (None, Some("len:3"))
        );

        ensure_dir_all(&mut s, &f.stage().join("Plater")).unwrap();
        fs::write(f.stage().join("Plater/Plater.toc"), b"## Interface: 120105").unwrap();
        let addon = f.client.join("Interface/AddOns/Plater");
        let moved = mutate(
            &mut s,
            &f.stage().join("Plater"),
            Op::Rename {
                to: &addon,
                mode: RenameMode::NoReplace,
            },
        )
        .unwrap();
        assert!(
            moved
                .before_hash
                .as_deref()
                .unwrap()
                .starts_with("tree:sha256:")
        );
        assert_eq!(moved.after_hash, None);
        assert_eq!(moved.to.as_deref(), Some(addon.to_str().unwrap()));

        ensure_dir_all(&mut s, &f.stage().join("Plater")).unwrap();
        fs::write(f.stage().join("Plater/Plater.toc"), b"## Interface: 120107").unwrap();
        let swapped = mutate(
            &mut s,
            &f.stage().join("Plater"),
            Op::Rename {
                to: &addon,
                mode: RenameMode::Exchange,
            },
        )
        .unwrap();
        assert_eq!(
            swapped.after_hash, moved.before_hash,
            "the old version is back at the stage path"
        );
        assert_eq!(
            fs::read(addon.join("Plater.toc")).unwrap(),
            b"## Interface: 120107"
        );

        let copy = f
            .client
            .join("Interface/.omaddons/snapshots")
            .join(TX)
            .join("WeakAuras.lua");
        ensure_dir_all(&mut s, copy.parent().unwrap()).unwrap();
        let sv = f.client.join("WTF/Account/A/SavedVariables/WeakAuras.lua");
        let cloned = mutate(&mut s, &copy, Op::CloneFile { from: &sv }).unwrap();
        assert_eq!(
            cloned.after_hash,
            Some(format!("sha256:{}", hex(&sha(b"WA = {}"))))
        );

        let chmod = mutate(&mut s, &record, Op::Chmod { mode: 0o600 }).unwrap();
        assert_eq!(
            (chmod.before_hash.as_deref(), chmod.after_hash.as_deref()),
            (Some("mode:644"), Some("mode:600"))
        );

        let link = state.join("latest");
        let linked = mutate(
            &mut s,
            &link,
            Op::SymlinkAtomic {
                target: Path::new("run-1"),
            },
        )
        .unwrap();
        assert_eq!(
            (linked.before_hash.as_deref(), linked.after_hash.as_deref()),
            (None, Some("link:run-1"))
        );
        let relinked = mutate(
            &mut s,
            &link,
            Op::SymlinkAtomic {
                target: Path::new("run-2"),
            },
        )
        .unwrap();
        assert_eq!(relinked.before_hash.as_deref(), Some("link:run-1"));

        let lines = f.actions();
        let ops: Vec<&str> = lines.iter().map(|r| r.op.as_str()).collect();
        for op in [
            "create_dir",
            "write_file",
            "append_file",
            "rename_noreplace",
            "rename_exchange",
            "clone_file",
            "chmod",
            "symlink_atomic",
        ] {
            assert!(ops.contains(&op), "{op} missing from {ops:?}");
        }
        let seqs: Vec<u64> = lines.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
        // ubs:ignore — a transaction id, not a secret
        assert!(lines.iter().all(|r| r.ok && r.tx == Some(f.tx)));

        let raw = fs::read_to_string(f.actions_log()).unwrap();
        let first_line: serde_json::Value =
            serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        let keys: BTreeSet<&str> = first_line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "seq",
                "path",
                "to",
                "op",
                "before_hash",
                "after_hash",
                "backup",
                "started_at_ns",
                "finished_at_ns",
                "tx",
                "ok",
            ])
        );
    }

    #[test]
    fn rename_replace_backs_up_the_overwritten_target() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        mutate(&mut s, &state.join("a"), write(b"new")).unwrap();
        mutate(&mut s, &state.join("b"), write(b"old")).unwrap();
        let record = mutate(
            &mut s,
            &state.join("a"),
            Op::Rename {
                to: &state.join("b"),
                mode: RenameMode::Replace,
            },
        )
        .unwrap();
        assert_eq!(fs::read(record.backup.unwrap()).unwrap(), b"old");
        assert_eq!(fs::read(state.join("b")).unwrap(), b"new");
    }

    #[test]
    fn expectations_are_compare_and_swap() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let file = state.join("registry.json");
        mutate(&mut s, &file, write(b"v1")).unwrap();

        let stale = MutateOpts {
            expect: Some(Expect::Hash(sha(b"v0"))),
            ..MutateOpts::default()
        };
        let err = mutate_with(&mut s, &file, write(b"v2"), stale).unwrap_err();
        assert!(matches!(err, FsxError::Precondition(_)), "{err}");
        let absent = MutateOpts {
            expect: Some(Expect::Absent),
            ..MutateOpts::default()
        };
        assert!(matches!(
            mutate_with(&mut s, &file, write(b"v2"), absent),
            Err(FsxError::Precondition(_))
        ));
        assert_eq!(fs::read(&file).unwrap(), b"v1");

        let fresh = MutateOpts {
            expect: Some(Expect::Hash(sha(b"v1"))),
            ..MutateOpts::default()
        };
        mutate_with(&mut s, &file, write(b"v2"), fresh).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"v2");
        assert!(matches!(
            mutate(&mut s, &state, Op::CreateDir),
            Err(FsxError::Precondition(_))
        ));
    }

    #[test]
    fn provided_tree_digests_match_computed_ones() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Libs/Ace")).unwrap();
        fs::write(dir.path().join("Foo.toc"), b"## Interface: 120105").unwrap();
        fs::write(dir.path().join("Libs/Ace/Ace.lua"), b"local A").unwrap();
        std::os::unix::fs::symlink("Foo.toc", dir.path().join("link")).unwrap();
        let index = [
            ("Libs/Ace/Ace.lua", 7, sha(b"local A")),
            ("Foo.toc", 20, sha(b"## Interface: 120105")),
        ];
        assert_eq!(
            TreeDigest::compute(dir.path()).unwrap(),
            TreeDigest::from_entries(index)
        );
        let other = [("Foo.toc", 20, sha(b"## Interface: 120105"))];
        assert_ne!(
            TreeDigest::compute(dir.path()).unwrap(),
            TreeDigest::from_entries(other)
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_failed_execution_is_logged_with_ok_false() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let err = failpoint::with_spec("fsx.mutate.executed=error", || {
            mutate(&mut s, &state.join("status.json"), write(b"{}"))
        })
        .unwrap_err();
        assert!(matches!(err, FsxError::Io { .. }), "{err}");
        let last = f.actions().pop().unwrap();
        assert_eq!(
            (last.op.as_str(), last.ok, last.after_hash),
            ("write_file", false, None)
        );
    }

    #[test]
    fn untracked_sessions_record_nothing() {
        let f = Fixture::new();
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "status.json");
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let status = state.join("status.json");
        mutate(&mut s, &status, write(b"{}")).unwrap();
        let record = mutate(&mut s, &status, write(b"{\"v\":2}")).unwrap();
        assert_eq!(
            (record.seq, record.tx, record.backup, record.before_hash),
            (0, None, None, None)
        );
        assert_eq!(fs::read(&status).unwrap(), b"{\"v\":2}");
        assert!(
            !state.join("snapshots").exists(),
            "no actions log or backups"
        );
        assert_eq!(s.untracked_reason(), Some("status.json"));
    }

    #[test]
    fn saved_variables_need_undo_and_ownership() {
        let f = Fixture::new();
        let sv = f.client.join("WTF/Account/A/SavedVariables/WeakAuras.lua");
        let weakauras = FolderName::new("WeakAuras").unwrap();
        let mut undo = f
            .tx_session(Writer::Undo)
            .with_owned_folders(OwnedFolders::new([&weakauras]));
        mutate(&mut undo, &sv, write(b"WA = {restored}")).unwrap();

        let mut not_owned = f.tx_session(Writer::Undo);
        assert!(matches!(
            mutate(&mut not_owned, &sv, write(b"x")),
            Err(FsxError::ScopeDenied(_))
        ));
        let mut mutating = f
            .tx_session(Writer::Mutating)
            .with_owned_folders(OwnedFolders::new([&weakauras]));
        assert!(matches!(
            mutate(&mut mutating, &sv, write(b"x")),
            Err(FsxError::ScopeDenied(_))
        ));
        assert_eq!(fs::read(&sv).unwrap(), b"WA = {restored}");
    }

    #[test]
    fn refused_writes_change_nothing() {
        let f = Fixture::new();
        let root = f
            .client
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        // The lock file's directory exists, so the deny-list (not a missing parent) is
        // what refuses it.
        fs::create_dir_all(f.paths.state_dir()).unwrap();
        fs::write(f.paths.lock_file(), b"").unwrap();
        let before = TreeDigest::compute(&root).unwrap();
        let mut s = f.tx_session(Writer::Doctor);
        let addons = f.client.join("Interface/AddOns");
        type IsExpected = fn(&FsxError) -> bool;
        let cases: Vec<(PathBuf, IsExpected)> = vec![
            (addons.join("Blizzard_AuctionUI/x.lua"), |e| {
                matches!(e, FsxError::Protected(_))
            }),
            (addons.join("BLIZZARD_AUCTIONUI"), |e| {
                matches!(e, FsxError::Protected(_))
            }),
            (addons.join("blizzard_new"), |e| {
                matches!(e, FsxError::Protected(_))
            }),
            (f.paths.lock_file(), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
            (f.client.join("Wow.exe"), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
            (f.client.join("Data/x"), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
            (f.paths.home().join("elsewhere"), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
            (addons.join("WeakAuras/../../Wow.exe"), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
            (PathBuf::from("relative/x"), |e| {
                matches!(e, FsxError::ScopeDenied(_))
            }),
        ];
        for (path, expected) in cases {
            let err = mutate(&mut s, &path, write(b"x")).unwrap_err();
            assert!(expected(&err), "{}: {err:?}", path.display());
        }
        std::os::unix::fs::symlink(f.client.join("Wow.exe"), addons.join("WeakAuras/dev")).unwrap();
        let err = mutate(&mut s, &addons.join("WeakAuras/dev"), write(b"x")).unwrap_err();
        assert!(matches!(err, FsxError::ScopeDenied(_)), "{err}");
        fs::remove_file(addons.join("WeakAuras/dev")).unwrap();
        assert_eq!(TreeDigest::compute(&root).unwrap(), before);
    }

    #[test]
    fn deferred_durability_is_only_for_untracked_staging() {
        let f = Fixture::new();
        let deferred = || MutateOpts {
            durability: Durability::Deferred,
            ..MutateOpts::default()
        };
        let mut untracked = WriteSession::untracked(Writer::Mutating, &f.scopes, "stage");
        ensure_dir_all(&mut untracked, &f.stage()).unwrap();
        mutate_with(
            &mut untracked,
            &f.stage().join("Foo"),
            Op::CreateDir,
            deferred(),
        )
        .unwrap();
        mutate_with(
            &mut untracked,
            &f.stage().join("Foo/Foo.toc"),
            write(b"x"),
            deferred(),
        )
        .unwrap();
        sync_filesystem(&f.stage()).unwrap();

        let state = f.paths.state_dir().join("x");
        let err = mutate_with(&mut untracked, &state, write(b"x"), deferred()).unwrap_err();
        assert!(
            matches!(err, FsxError::Precondition(_) | FsxError::ScopeDenied(_)),
            "{err}"
        );
        let mut tx = f.tx_session(Writer::Mutating);
        let err =
            mutate_with(&mut tx, &f.stage().join("Bar"), Op::CreateDir, deferred()).unwrap_err();
        assert!(matches!(err, FsxError::Precondition(_)), "{err}");
    }

    #[test]
    fn errors_map_to_the_registry() {
        let scope: Error = FsxError::ScopeDenied("x".into()).into();
        assert_eq!(
            (scope.code, scope.exit().code()),
            (ErrorCode::ScopeDenied, 4)
        );
        let protected: Error = FsxError::Protected("x".into()).into();
        assert_eq!(
            (protected.code, protected.exit().code()),
            (ErrorCode::ProtectedFolder, 4)
        );
        assert_eq!(
            protected.fix.argv().unwrap(),
            ["omaddons", "list", "--protected"]
        );
        let io: Error = FsxError::Io {
            path: "/x".into(),
            source: io::Error::other("boom"),
        }
        .into();
        assert_eq!(io.code, ErrorCode::Io);
        assert!(!ErrorCode::ScopeDenied.default_retryable());
    }
}
