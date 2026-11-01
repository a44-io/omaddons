//! `mutate()`: the one function that changes the filesystem (vision §5.5, D26).
//!
//! Steps, in the doctor contract's order: (1) normalize the path, run the deny-list,
//! the scope table, the operation's preconditions and the caller's endpoint predicates
//! (`fsx::expect`); (2) before-digest; (3) verbatim
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

use super::expect::Expect;
use super::failpoint;
use super::fence::{Fenced, FencedOp};
use super::op::Op;
use super::scope::{Access, Refusal, Writer};
use super::session::{SessionKind, TxState, WriteSession};
use super::sys::{self, CloneKind, ClonePolicy, Content, Durability, RenameError, RenameMode};
use crate::error::{Error, ErrorCode, Fencing, Suggestion};
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
    /// A stuck journal's fence protects what the operation would write (M4-D62).
    #[error("{}", .0.message())]
    Fenced(Box<Fenced>),
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
            // The fencing journal's own stop, as recovery reports it (`journal_pending`,
            // `reason`, `target`), plus the journal and the resource it protects. A stop
            // with no journal (a journal directory that cannot be listed) names none,
            // rather than a `null` one.
            FsxError::Fenced(fenced) => {
                let stop = &fenced.stop;
                let mut details = serde_json::Map::new();
                if let Some(tx) = stop.tx {
                    details.insert("journal_pending".into(), serde_json::json!(tx));
                }
                if let Some(reason) = stop.reason {
                    details.insert("reason".into(), reason.into());
                }
                details.insert("target".into(), serde_json::json!(stop.target));
                if let Some(tx) = stop.tx {
                    details.insert("tx".into(), serde_json::json!(tx));
                }
                details.insert("resource".into(), fenced.resource.clone().into());
                let how = if fenced.closed {
                    Fencing::Closed
                } else {
                    Fencing::Path
                };
                Error::new(stop.code, fenced.message(), stop.fix.clone())
                    .details(serde_json::Value::Object(details))
                    .fenced(how)
            }
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

/// Options for [`mutate_with`].
#[derive(Debug, Clone, Default)]
pub struct MutateOpts {
    /// The predicate on the operation's own path, checked at step 1.
    pub expect: Option<Expect>,
    /// The predicate on a rename's destination, checked with `expect`; refused on any
    /// other operation.
    pub expect_to: Option<Expect>,
    /// Asks for the one exception to the symlink refusal: a doctor's no-replace rename of
    /// a temporary link a crash inside `symlink_atomic` left (`.omaddons-…`), between an
    /// omaddons-owned directory and a quarantine root, either way. fsx checks every
    /// condition ([`temp_symlink_rename`]), so a caller cannot widen it.
    pub temp_symlink: bool,
    /// The tree digest of a directory at the path before the operation, from the
    /// caller's per-file index, so large folders need not be re-hashed.
    pub tree_before: Option<TreeDigest>,
    /// The same, for what is at the path afterwards (an exchange).
    pub tree_after: Option<TreeDigest>,
    /// The SHA-256 of a regular file at the path before the operation, when the caller
    /// knows it (a copy it verified, or a file whose stat stamp has not changed since it
    /// hashed it), so large files need not be re-hashed.
    pub sha256_before: Option<[u8; 32]>,
    /// The same, for what is at the path afterwards (an exchange).
    pub sha256_after: Option<[u8; 32]>,
    /// A whole tree's stat digest before the operation (M3-D25), recorded instead of a
    /// content digest: a renamed `WTF/` is never hashed in a transaction session. It
    /// takes precedence over `tree_before` for a directory.
    pub stamp_before: Option<StampDigest>,
    /// The same, for what is at the path afterwards (an exchange).
    pub stamp_after: Option<StampDigest>,
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

/// A whole tree's stat digest (M3-D25): SHA-256 over the sorted lines
/// `<relative path>\0<kind>\0<size>\0<mode, octal>\0<mtime ns>\n` of every entry below
/// the root, empty directories and symlinks included. No content is read, so a large
/// `WTF/` is digested by a stat walk, never hashed under the lock; a file rewritten in
/// place with the same size and mtime is not noticed, which is the stamp rule everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StampDigest([u8; 32]);

impl StampDigest {
    /// The digest a record holds (a repair journal's `stamp:sha256:…`), taken back as is.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// From entries `(relative path with '/', kind, size, mode, mtime ns)`, in any order.
    /// `kind` is `file`, `dir`, `symlink` or `other`.
    pub fn from_entries<P: Into<String>>(
        entries: impl IntoIterator<Item = (P, &'static str, u64, u32, i64)>,
    ) -> Self {
        let mut lines: Vec<(String, &str, u64, u32, i64)> = entries
            .into_iter()
            .map(|(path, kind, size, mode, mtime)| (path.into(), kind, size, mode, mtime))
            .collect();
        lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut hasher = Sha256::new();
        for (path, kind, size, mode, mtime) in lines {
            hasher.update(path.as_bytes());
            hasher.update(format!("\0{kind}\0{size}\0{mode:o}\0{mtime}\n").as_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// Walks `dir` with `lstat`, never following a symlink, and digests every entry.
    pub fn compute(dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let mut entries = Vec::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            for entry in fs::read_dir(dir.join(&rel))? {
                let entry = entry?;
                let rel_child = rel.join(entry.file_name());
                let meta = fs::symlink_metadata(dir.join(&rel_child))?;
                let kind = meta.file_type();
                let kind = if kind.is_dir() {
                    pending.push(rel_child.clone());
                    "dir"
                } else if kind.is_file() {
                    "file"
                } else if kind.is_symlink() {
                    "symlink"
                } else {
                    "other"
                };
                // Saturating: an mtime past 2262 does not fit in i64 nanoseconds.
                let mtime = meta
                    .mtime()
                    .saturating_mul(1_000_000_000)
                    .saturating_add(meta.mtime_nsec());
                entries.push((
                    rel_child.to_string_lossy().into_owned(),
                    kind,
                    meta.len(),
                    meta.mode() & 0o7777,
                    mtime,
                ));
            }
        }
        Ok(Self::from_entries(entries))
    }
}

/// `stamp:sha256:<hex>`, never mistaken for a content digest.
impl fmt::Display for StampDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stamp:sha256:{}", hex(&self.0))
    }
}

/// The state of a path that an operation changes, recorded before and after it:
/// `sha256:<hex>` (file content), `tree:sha256:<hex>` (directory), `len:<n>` (an
/// append), `mode:<octal>` (chmod) or `link:<target>` (symlink).
#[derive(Debug, Clone, PartialEq, Eq)]
enum StateDigest {
    File([u8; 32]),
    Tree(TreeDigest),
    Stamp(StampDigest),
    Len(u64),
    Mode(u32),
    Link(PathBuf),
}

impl fmt::Display for StateDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(sha) => write!(f, "sha256:{}", hex(sha)),
            Self::Tree(tree) => tree.fmt(f),
            Self::Stamp(stamp) => stamp.fmt(f),
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
    /// 1-based within a transaction or a logged untracked session; 0 in an untracked one.
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

impl ActionRecord {
    /// The lowercase hex SHA-256 of the regular file at `path` after the operation,
    /// when its after-digest is one (a `CloneFile`'s copy, say).
    pub fn after_sha256(&self) -> Option<&str> {
        self.after_hash.as_deref()?.strip_prefix("sha256:")
    }
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
    let link_op = matches!(op, Op::SymlinkAtomic { .. })
        || (opts.temp_symlink && temp_symlink_rename(s, path, &op));
    let target = normalize(path, link_op)?;
    let access = match op {
        Op::CreateDir => Access::CreateDir,
        Op::Rename { .. } => Access::Rename,
        _ => Access::Write,
    };
    let matched = s.scopes.check(s.writer, &s.owned, &target, access)?;
    let to = match &op {
        Op::Rename { to, .. } => {
            let to = normalize(to, false)?;
            s.scopes.check(s.writer, &s.owned, &to, Access::Rename)?;
            Some(to)
        }
        _ => None,
    };
    // A stuck journal's fences (M4-D62): after the scope checks, so an out-of-scope path
    // stays `scope_denied`, and before anything is read or written.
    let fenced = match (&op, to.as_deref()) {
        (Op::CreateDir, _) => FencedOp::CreateDir(&target),
        (Op::Rename { .. }, Some(to)) => FencedOp::Rename { from: &target, to },
        _ => FencedOp::Write(&target),
    };
    s.scopes.fences().check(fenced).map_err(FsxError::Fenced)?;
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
    preconditions(
        &target,
        to.as_deref(),
        &op,
        opts.expect.as_ref(),
        opts.expect_to.as_ref(),
    )?;
    failpoint::hit("fsx.mutate.checked").map_err(io_err(&target))?;

    let state = match &mut s.kind {
        SessionKind::Tx(state) => state,
        SessionKind::Untracked { .. } => {
            let executed = execute(
                &target,
                to.as_deref(),
                op,
                (opts.durability, opts.expect.as_ref(), false),
            )?;
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
        }
        // As untracked, then the line, written for a failed execution too.
        SessionKind::UntrackedLogged { log, .. } => {
            let exchange = matches!(
                op,
                Op::Rename {
                    mode: RenameMode::Exchange,
                    ..
                }
            );
            let executed = execute(
                &target,
                to.as_deref(),
                op,
                (opts.durability, opts.expect.as_ref(), false),
            );
            let record = ActionRecord {
                seq: log.next_seq(),
                path: target.to_string_lossy().into_owned(),
                to: to.as_ref().map(|to| to.to_string_lossy().into_owned()),
                op: wire.to_owned(),
                before_hash: None,
                after_hash: None,
                backup: None,
                started_at_ns,
                finished_at_ns: now_ns(),
                tx: None,
                ok: executed.is_ok(),
                clone: executed.as_ref().ok().and_then(|e| e.clone),
            };
            let moved = to
                .as_deref()
                .filter(|_| executed.is_ok())
                .map(|to| (target.as_path(), to, exchange));
            log.record(record.clone(), moved)?;
            executed?;
            return Ok(record);
        }
    };
    // The table's roots are canonical, while these two are built from `Paths` as given:
    // compared resolved, so a symlinked `~/.local` (or state home) is still inside.
    let internal = |path: &Path| {
        s.scopes.is_internal(
            &super::prune::canonical_lenient(path).unwrap_or_else(|| path.to_path_buf()),
        )
    };
    if !internal(&state.actions_log) {
        return Err(FsxError::ScopeDenied(format!(
            "the actions log {} is outside the state and client meta dirs",
            state.actions_log.display()
        )));
    }
    let seq = state.next_seq();

    // 2. Before-digest.
    let known_before = Known {
        tree: opts.tree_before,
        stamp: opts.stamp_before,
        sha256: opts.sha256_before,
    };
    let before = digest(&target, &op, known_before).map_err(io_err(&target))?;

    // 3. Verbatim backup of content the operation overwrites.
    let backup_dir = state.snapshot_roots.dir_for(&matched.side, &state.tx);
    if !internal(&backup_dir) {
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
    let known_after = Known {
        tree: opts.tree_after,
        stamp: opts.stamp_after,
        sha256: opts.sha256_after,
    };
    let digest_op = DigestOp::of(&op);
    let executed = execute(
        &target,
        to.as_deref(),
        op,
        (opts.durability, opts.expect.as_ref(), true),
    )
    .and_then(|written| {
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
        Ok(_) => digest_after(&target, digest_op, known_after).ok().flatten(),
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
    // An existing ancestor may be a symlink to a directory (a linked `~/.config`): it is
    // followed, as every `CreateDir` below resolves its parent anyway, and a dangling
    // link reads as missing, whose `CreateDir` fsx refuses.
    loop {
        match fs::metadata(current) {
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
    let is_dir = |dir: &Path| fs::metadata(dir).is_ok_and(|m| m.is_dir());
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

/// Whether the filesystem holding `dir` can `RENAME_EXCHANGE` (M3-D24): two empty files
/// created in `dir` are exchanged. A restore probes before it copies anything, in its own
/// stage, which goes with the transaction; `false` only when the kernel says the flag is
/// unsupported there, any other failure an error.
pub fn probe_exchange(s: &mut WriteSession<'_>, dir: &Path) -> Result<bool, FsxError> {
    ensure_dir_all(s, dir)?;
    let (a, b) = (dir.join("a"), dir.join("b"));
    for file in [&a, &b] {
        let op = Op::WriteFile {
            content: Content::Bytes(b""),
            mode: Some(0o600),
        };
        mutate(s, file, op)?;
    }
    let exchange = Op::Rename {
        to: &b,
        mode: RenameMode::Exchange,
    };
    match mutate(s, &a, exchange) {
        Ok(_) => Ok(true),
        Err(FsxError::Unsupported) => Ok(false),
        Err(err) => Err(err),
    }
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

/// Whether `path` → `op` is the rename [`MutateOpts::temp_symlink`] admits: a no-replace
/// rename by the doctor, of a symlink, both names temporary (`.omaddons-…`), with one end
/// in an omaddons-owned directory and the other below a quarantine root. Only a crash
/// inside `symlink_atomic` leaves such a link where omaddons keeps its own files; a
/// developer's symlink in AddOns stays immutable.
fn temp_symlink_rename(s: &WriteSession<'_>, path: &Path, op: &Op<'_>) -> bool {
    let Op::Rename {
        to,
        mode: RenameMode::NoReplace,
    } = op
    else {
        return false;
    };
    if !matches!(s.writer, Writer::Doctor) {
        return false;
    }
    let (Ok(from), Ok(to)) = (normalize(path, true), normalize(to, true)) else {
        return false;
    };
    let temporary = |p: &Path| {
        p.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(sys::TEMP_PREFIX))
    };
    let link = fs::symlink_metadata(&from).is_ok_and(|m| m.file_type().is_symlink());
    let (owned, quarantine) = (
        |p: &Path| s.scopes.is_app_owned(p),
        |p: &Path| s.scopes.is_quarantine(p),
    );
    link && temporary(&from)
        && temporary(&to)
        && ((owned(&from) && quarantine(&to)) || (quarantine(&from) && owned(&to)))
}

/// The canonical parent joined with the final component. The final component must not
/// be an existing symlink unless `allow_symlink` (a `SymlinkAtomic` link path, or the
/// rename [`temp_symlink_rename`] admits), which keeps developer symlinks in AddOns
/// immutable.
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
    expect_to: Option<&Expect>,
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
        Op::CloneFile { from, .. } => {
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
    if expect_to.is_some() && !matches!(op, Op::Rename { .. }) {
        return fail("a destination predicate needs a rename");
    }
    endpoint(target, expect)?;
    match to {
        Some(to) => endpoint(to, expect_to),
        None => Ok(()),
    }
}

/// Checks the caller's predicate on one endpoint, naming that endpoint when it fails.
fn endpoint(path: &Path, expect: Option<&Expect>) -> Result<(), FsxError> {
    let Some(expect) = expect else {
        return Ok(());
    };
    match expect.check(path).map_err(io_err(path))? {
        Some(mismatch) => Err(FsxError::Precondition(format!(
            "{}: {mismatch}",
            path.display()
        ))),
        None => Ok(()),
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

/// What the caller already knows of a path's content: a directory's tree digest or a
/// regular file's SHA-256, used instead of reading it.
#[derive(Debug, Clone, Copy, Default)]
struct Known {
    tree: Option<TreeDigest>,
    stamp: Option<StampDigest>,
    sha256: Option<[u8; 32]>,
}

fn digest(path: &Path, op: &Op<'_>, known: Known) -> io::Result<Option<StateDigest>> {
    digest_after(path, DigestOp::of(op), known)
}

/// The state of `path` as the operation sees it; `None` when nothing is there.
fn digest_after(path: &Path, op: DigestOp, known: Known) -> io::Result<Option<StateDigest>> {
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
        DigestOp::Content if meta.is_dir() => match (known.stamp, known.tree) {
            (Some(stamp), _) => StateDigest::Stamp(stamp),
            (None, Some(tree)) => StateDigest::Tree(tree),
            (None, None) => StateDigest::Tree(TreeDigest::compute(path)?),
        },
        DigestOp::Content => StateDigest::File(match known.sha256 {
            Some(sha256) => sha256,
            None => hash_file(path)?,
        }),
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
    sys::clone_file(source, &copy, Durability::Immediate, ClonePolicy::Preferred)
        .map_err(io_err(&copy))?;
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
/// Runs `op`. `tracked` (a transaction session) makes a chmod durable before its
/// actions line can be: a power loss must never keep a repair's `done` line and lose the
/// mode change it records.
fn execute(
    target: &Path,
    to: Option<&Path>,
    op: Op<'_>,
    (durability, expect, tracked): (Durability, Option<&Expect>, bool),
) -> Result<Executed, FsxError> {
    let io = io_err(target);
    let none = |()| Executed::default();
    match op {
        // Expected absent: published without ever replacing what appeared meanwhile.
        Op::WriteFile { content, mode } => {
            let publish = match expect {
                Some(Expect::Absent) => sys::Publish::NoClobber,
                _ => sys::Publish::Replace,
            };
            match sys::write_atomic_to(target, content, mode, durability, publish) {
                Ok(written) => Ok(Executed {
                    sha256: Some(written.sha256),
                    clone: None,
                }),
                Err(err)
                    if matches!(publish, sys::Publish::NoClobber)
                        && matches!(err.kind(), io::ErrorKind::AlreadyExists) =>
                {
                    Err(FsxError::Precondition(format!(
                        "{} appeared while it was written",
                        target.display()
                    )))
                }
                Err(err) => Err(io(err)),
            }
        }
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
        Op::CloneFile { from, policy } => sys::clone_file(from, target, durability, policy)
            .map(|kind| Executed {
                sha256: None,
                clone: Some(kind),
            })
            .map_err(io),
        Op::Chmod { mode } if tracked => sys::chmod_durable(target, mode).map(none).map_err(io),
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

#[cfg(test)]
thread_local! {
    /// Files hashed on this thread, so a test can tell what a commit window reads.
    static FILES_HASHED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many files this thread has hashed.
#[cfg(test)]
pub(crate) fn files_hashed() -> u64 {
    FILES_HASHED.with(std::cell::Cell::get)
}

/// Counts one file hashed on this thread, for [`files_hashed`]; hashing helpers outside
/// fsx call it too. A no-op outside tests, so callers need no `cfg`.
pub(crate) fn count_hashed() {
    #[cfg(test)]
    FILES_HASHED.with(|n| n.set(n.get() + 1));
}

pub(super) fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    count_hashed();
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

/// A SHA-256 from its hex form (either case); `None` unless it is exactly 64 hex digits.
pub fn sha256_from_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let digit = |b: &u8| char::from(*b).to_digit(16);
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let [high, low] = pair else {
            return None;
        };
        *byte = u8::try_from(digit(high)? * 16 + digit(low)?).ok()?;
    }
    Some(out)
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
    use crate::fsx::{FileKind, Parts, Predicate};
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

        /// The same layout, reached through `<root>/home`, a symlink to the real home.
        fn through_a_linked_home() -> Self {
            let mut f = Self::new();
            let root = f.client.ancestors().nth(3).unwrap().to_path_buf();
            fs::rename(root.join("home"), root.join("real-home")).unwrap();
            std::os::unix::fs::symlink(root.join("real-home"), root.join("home")).unwrap();
            f.scopes = ScopeTable::new(&f.paths, std::slice::from_ref(&f.client), None).unwrap();
            f
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

    /// A home reached through a symlink (a linked `~/.local`, `~/.config` or home): the
    /// directories are created through the link, and a transaction's actions log and
    /// backups, built from `Paths` as given, are still inside the canonical state dir.
    #[test]
    fn a_tx_session_works_through_a_linked_home() {
        let f = Fixture::through_a_linked_home();
        let mut s = f.tx_session(Writer::Mutating);
        let record = f.paths.state_dir().join("targets/key.json");
        ensure_dir_all(&mut s, &f.paths.state_dir().join("targets")).unwrap();
        mutate(&mut s, &record, write(b"v1")).unwrap();
        let second = mutate(&mut s, &record, write(b"v2")).unwrap();
        assert!(second.backup.is_some(), "{second:?}");
        assert_eq!(fs::read(&record).unwrap(), b"v2");
        let ops: Vec<String> = f.actions().into_iter().map(|a| a.op).collect();
        assert!(
            ops.ends_with(&["write_file".to_owned(), "write_file".to_owned()]),
            "{ops:?}"
        );
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
        let cloned = mutate(
            &mut s,
            &copy,
            Op::CloneFile {
                from: &sv,
                policy: ClonePolicy::Preferred,
            },
        )
        .unwrap();
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
            expect: Some(Expect::hash(sha(b"v0"))),
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
            expect: Some(Expect::hash(sha(b"v1"))),
            ..MutateOpts::default()
        };
        mutate_with(&mut s, &file, write(b"v2"), fresh).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"v2");
        assert!(matches!(
            mutate(&mut s, &state, Op::CreateDir),
            Err(FsxError::Precondition(_))
        ));
    }

    /// A stop for the fence tests: `tx` and retail's key, `internal`, `step_unrecognized`.
    fn fence_stop(tx: &str) -> crate::fsx::FenceStop {
        crate::fsx::FenceStop {
            tx: Some(tx.parse().unwrap()),
            target: Some("01M5ZQ7Y3KJ8V9W2X4C6B0N1RT".parse().unwrap()),
            code: ErrorCode::Internal,
            reason: Some("step_unrecognized"),
            summary: format!(
                "repair transaction {tx} did not finish, and recovery could not settle it (step_unrecognized)"
            ),
            fix: Suggestion::command([
                "omaddons",
                "doctor",
                "explain",
                "fm-concurrency_primitives-journal-step-unrecognized",
            ]),
        }
    }

    /// One fence, on `path`, its resource text the path with a trailing `/`.
    fn fence_on(path: &Path) -> crate::fsx::FenceSet {
        let mut fences = crate::fsx::FenceSet::new();
        fences.fence(
            path.to_path_buf(),
            format!("{}/", path.display()),
            fence_stop("01JB8Q2VA00000000000000009"),
        );
        fences
    }

    /// One operation of each `Op` kind, each on a path the `Mutating` writer may write.
    #[derive(Debug, Clone, Copy)]
    enum FencedCase {
        WriteFile,
        AppendFile,
        CreateDir,
        RenameFencedSource,
        RenameFencedDestination,
        CloneFile,
        Chmod,
        SymlinkAtomic,
    }

    impl FencedCase {
        const ALL: [Self; 8] = [
            Self::WriteFile,
            Self::AppendFile,
            Self::CreateDir,
            Self::RenameFencedSource,
            Self::RenameFencedDestination,
            Self::CloneFile,
            Self::Chmod,
            Self::SymlinkAtomic,
        ];

        fn record(f: &Fixture) -> PathBuf {
            f.paths.state_dir().join("targets/key.json")
        }

        fn addon(f: &Fixture) -> PathBuf {
            f.client.join("Interface/AddOns/Plater")
        }

        fn copy(f: &Fixture) -> PathBuf {
            f.client
                .join("Interface/.omaddons/snapshots")
                .join(TX)
                .join("WeakAuras.lua")
        }

        /// Makes what the operation needs, unfenced.
        fn prepare(self, f: &Fixture) {
            let mut s = f.tx_session(Writer::Mutating);
            ensure_dir_all(&mut s, &f.paths.state_dir().join("targets")).unwrap();
            match self {
                Self::Chmod => {
                    mutate(&mut s, &Self::record(f), write(b"v1")).unwrap();
                }
                Self::RenameFencedSource | Self::RenameFencedDestination => {
                    ensure_dir_all(&mut s, &f.stage().join("Plater")).unwrap();
                    fs::write(f.stage().join("Plater/Plater.toc"), b"## Interface: 120105")
                        .unwrap();
                }
                Self::CloneFile => {
                    ensure_dir_all(&mut s, Self::copy(f).parent().unwrap()).unwrap();
                }
                _ => {}
            }
        }

        /// The path a fence on which refuses the operation.
        fn fenced(self, f: &Fixture) -> PathBuf {
            let state = f.paths.state_dir();
            match self {
                Self::WriteFile | Self::Chmod => Self::record(f),
                Self::AppendFile => state.join("history.jsonl"),
                Self::CreateDir => state.join("targets/sub"),
                Self::RenameFencedSource => f.stage().join("Plater"),
                Self::RenameFencedDestination => Self::addon(f),
                Self::CloneFile => Self::copy(f),
                Self::SymlinkAtomic => state.join("latest"),
            }
        }

        fn apply(self, f: &Fixture, s: &mut WriteSession<'_>) -> Result<ActionRecord, FsxError> {
            let state = f.paths.state_dir();
            match self {
                Self::WriteFile => mutate(s, &Self::record(f), write(b"v1")),
                Self::AppendFile => mutate(
                    s,
                    &state.join("history.jsonl"),
                    Op::AppendFile { content: b"{}\n" },
                ),
                Self::CreateDir => mutate(s, &state.join("targets/sub"), Op::CreateDir),
                Self::RenameFencedSource | Self::RenameFencedDestination => mutate(
                    s,
                    &f.stage().join("Plater"),
                    Op::Rename {
                        to: &Self::addon(f),
                        mode: RenameMode::NoReplace,
                    },
                ),
                Self::CloneFile => mutate(
                    s,
                    &Self::copy(f),
                    Op::CloneFile {
                        from: &f.client.join("WTF/Account/A/SavedVariables/WeakAuras.lua"),
                        policy: ClonePolicy::Preferred,
                    },
                ),
                Self::Chmod => mutate(s, &Self::record(f), Op::Chmod { mode: 0o600 }),
                Self::SymlinkAtomic => mutate(
                    s,
                    &state.join("latest"),
                    Op::SymlinkAtomic {
                        target: Path::new("run-1"),
                    },
                ),
            }
        }
    }

    /// Every entry under `root`: its kind, mode, and content (a file's bytes, a link's
    /// target).
    fn tree_state(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        use std::os::unix::fs::PermissionsExt;
        listing(root)
            .into_iter()
            .map(|path| {
                let meta = fs::symlink_metadata(&path).unwrap();
                let mode = meta.permissions().mode();
                let what = if meta.is_symlink() {
                    format!("link {}", fs::read_link(&path).unwrap().display())
                } else if meta.is_dir() {
                    format!("dir {mode:o}")
                } else {
                    format!("file {mode:o} {}", hex(&sha(&fs::read(&path).unwrap())))
                };
                (path, what)
            })
            .collect()
    }

    /// The lines of the fixture's actions log, none when it is absent.
    fn action_lines(f: &Fixture) -> usize {
        fs::read_to_string(f.actions_log()).map_or(0, |log| log.lines().count())
    }

    /// Runs `op` with `fsx.mutate.checked` armed to fail, so reaching it would show.
    #[cfg(debug_assertions)]
    fn checked_fails<T>(op: impl FnOnce() -> T) -> T {
        failpoint::with_spec("fsx.mutate.checked=error", op)
    }

    #[cfg(not(debug_assertions))]
    fn checked_fails<T>(op: impl FnOnce() -> T) -> T {
        op()
    }

    #[test]
    fn a_fenced_write_touches_nothing() {
        for case in FencedCase::ALL {
            let f = Fixture::new();
            case.prepare(&f);
            let fenced = f.scopes.clone().with_fences(fence_on(&case.fenced(&f)));
            let root = f.paths.home().parent().unwrap().to_path_buf();
            let (before, lines) = (tree_state(&root), action_lines(&f));
            let mut s = WriteSession::tx(
                f.tx,
                Writer::Mutating,
                &fenced,
                SnapshotRoots::for_tx(&f.paths, &f.tx),
                f.actions_log(),
            );
            let result = checked_fails(|| case.apply(&f, &mut s));
            assert!(
                matches!(result, Err(FsxError::Fenced(_))),
                "{case:?}: a fence on {}: {result:?}",
                case.fenced(&f).display()
            );
            assert_eq!(tree_state(&root), before, "{case:?}: nothing changed");
            assert_eq!(action_lines(&f), lines, "{case:?}: no actions line");
        }
    }

    #[test]
    fn without_fences_writes_as_before() {
        for case in FencedCase::ALL {
            let f = Fixture::new();
            case.prepare(&f);
            let fenced = f.scopes.clone().with_fences(fence_on(&case.fenced(&f)));
            let unfenced = fenced.without_fences();
            assert!(unfenced.fences().is_empty() && !fenced.fences().is_empty());
            let mut s = WriteSession::tx(
                f.tx,
                Writer::Mutating,
                &unfenced,
                SnapshotRoots::for_tx(&f.paths, &f.tx),
                f.actions_log(),
            );
            let result = case.apply(&f, &mut s);
            assert!(result.is_ok(), "{case:?}: {result:?}");
        }
    }

    #[test]
    fn ensure_dir_all_below_a_missing_parent_of_a_fence() {
        let f = Fixture::new();
        let state = f.paths.state_dir().to_path_buf();
        let mut s = f.tx_session(Writer::Mutating);
        ensure_dir_all(&mut s, &state).unwrap();
        let fenced = f.scopes.clone().with_fences(fence_on(&state.join("a/b/c")));
        let mut s = WriteSession::tx(
            f.tx,
            Writer::Mutating,
            &fenced,
            SnapshotRoots::for_tx(&f.paths, &f.tx),
            f.actions_log(),
        );
        ensure_dir_all(&mut s, &state.join("a/b")).unwrap();
        assert!(state.join("a/b").is_dir());
        let refused = ensure_dir_all(&mut s, &state.join("a/b/c"));
        assert!(matches!(refused, Err(FsxError::Fenced(_))), "{refused:?}");
        assert!(!state.join("a/b/c").exists());
    }

    #[test]
    fn fenced_error_has_the_stops_code_details_and_fix() {
        let f = Fixture::new();
        let state = f.paths.state_dir().to_path_buf();
        let mut s = f.tx_session(Writer::Mutating);
        let protected = state.join("fenced");
        ensure_dir_all(&mut s, &protected).unwrap();
        let tx = "01JB8Q2VA00000000000000009";
        for reason in [Some("step_unrecognized"), None] {
            let mut stop = fence_stop(tx);
            stop.reason = reason;
            let mut fences = crate::fsx::FenceSet::new();
            fences.fence(
                protected.clone(),
                "<state>/fenced/".to_owned(),
                stop.clone(),
            );
            let fenced = f.scopes.clone().with_fences(fences);
            let mut s = WriteSession::tx(
                f.tx,
                Writer::Mutating,
                &fenced,
                SnapshotRoots::for_tx(&f.paths, &f.tx),
                f.actions_log(),
            );
            let attempted = protected.join("FAKEACCT.json");
            let err = Error::from(mutate(&mut s, &attempted, write(b"x")).unwrap_err());
            let mut details = serde_json::json!({
                "journal_pending": tx,
                "target": "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT",
                "tx": tx,
                "resource": "<state>/fenced/",
            });
            if let Some(reason) = reason {
                details["reason"] = reason.into();
            }
            assert_eq!(
                (err.code, &err.details, &err.fix),
                (ErrorCode::Internal, &details, &stop.fix),
                "{reason:?}"
            );
            assert!(
                err.message.contains("it protects <state>/fenced/")
                    && !err.message.contains("FAKEACCT"),
                "{reason:?}: {}",
                err.message
            );
            // Marked, so no caller records it as a verdict on what it wrote (E14).
            assert_eq!(err.fenced, Some(Fencing::Path), "{reason:?}");
        }
    }

    /// Every path under `dir`, to compare what a refused operation left.
    fn listing(dir: &Path) -> BTreeSet<PathBuf> {
        let mut out = BTreeSet::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path.clone());
                }
                out.insert(path);
            }
        }
        out
    }

    fn identity_of(path: &Path) -> Expect {
        let parts = Parts {
            identity: true,
            ..Parts::default()
        };
        Expect::capture(path, parts).unwrap()
    }

    /// Puts another file with the same bytes and mode at `path`: only its identity
    /// changes.
    fn replace_with_a_copy(path: &Path) {
        let copy = path.with_extension("copy");
        fs::copy(path, &copy).unwrap();
        fs::rename(&copy, path).unwrap();
    }

    fn expecting(expect: Expect) -> MutateOpts {
        MutateOpts {
            expect: Some(expect),
            ..MutateOpts::default()
        }
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn refused_with(result: Result<ActionRecord, FsxError>, path: &Path, text: &str) -> bool {
        matches!(&result, Err(FsxError::Precondition(message))
            if message.starts_with(&*path.to_string_lossy()) && message.ends_with(text))
    }

    /// The link a crash inside `symlink_atomic` left in `<state>`, and a quarantine
    /// directory of a doctor run to set it aside in.
    fn crashed_link(f: &Fixture) -> (PathBuf, PathBuf) {
        let state = f.paths.state_dir().to_path_buf();
        fs::create_dir_all(state.join("doctor/runs/old")).unwrap();
        let quarantine = state.join("doctor/runs/R/quarantine/state");
        fs::create_dir_all(&quarantine).unwrap();
        let link = state.join(".omaddons-abc");
        std::os::unix::fs::symlink("doctor/runs/old", &link).unwrap();
        (link, quarantine)
    }

    fn temp_symlink() -> MutateOpts {
        MutateOpts {
            temp_symlink: true,
            ..MutateOpts::default()
        }
    }

    fn no_replace(to: &Path) -> Op<'_> {
        Op::Rename {
            to,
            mode: RenameMode::NoReplace,
        }
    }

    #[test]
    fn a_temp_symlink_renames_into_a_quarantine_root() {
        let f = Fixture::new();
        let (link, quarantine) = crashed_link(&f);
        let dest = quarantine.join(".omaddons-abc");
        let mut s = f.tx_session(Writer::Doctor);
        let record = mutate_with(&mut s, &link, no_replace(&dest), temp_symlink()).unwrap();
        assert!(
            fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&dest).unwrap(), Path::new("doctor/runs/old"));
        assert!(fs::symlink_metadata(&link).is_err(), "the link moved");
        assert!(
            f.paths.state_dir().join("doctor/runs/old").is_dir(),
            "its target untouched"
        );
        assert_eq!(
            (record.op.as_str(), record.before_hash.as_deref()),
            ("rename_noreplace", Some("link:doctor/runs/old"))
        );
        assert_eq!(
            f.actions().last().map(|a| a.op.clone()).as_deref(),
            Some("rename_noreplace")
        );
    }

    #[test]
    fn a_temp_symlink_renames_back_out_of_quarantine() {
        let f = Fixture::new();
        let (link, quarantine) = crashed_link(&f);
        let dest = quarantine.join(".omaddons-abc");
        let mut s = f.tx_session(Writer::Doctor);
        mutate_with(&mut s, &link, no_replace(&dest), temp_symlink()).unwrap();
        mutate_with(&mut s, &dest, no_replace(&link), temp_symlink()).unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("doctor/runs/old"));
        assert!(fs::symlink_metadata(&dest).is_err());
    }

    #[test]
    fn the_symlink_exception_refuses_everything_else() {
        // Each case: its name, the session's writer, whether it asks for the exception,
        // and the shape it changes from a good rename into quarantine.
        let cases: [(&str, Writer, bool, &str); 11] = [
            ("flag_not_set", Writer::Doctor, false, "quarantine"),
            (
                "no_temp_prefix_on_source",
                Writer::Doctor,
                true,
                "plain_source",
            ),
            (
                "no_temp_prefix_on_destination",
                Writer::Doctor,
                true,
                "plain_destination",
            ),
            ("writer_mutating", Writer::Mutating, true, "quarantine"),
            ("rename_replace", Writer::Doctor, true, "replace"),
            ("rename_exchange", Writer::Doctor, true, "exchange"),
            (
                "destination_not_quarantine",
                Writer::Doctor,
                true,
                "elsewhere",
            ),
            ("both_ends_owned", Writer::Doctor, true, "cache"),
            ("source_in_addons", Writer::Doctor, true, "addons"),
            ("write_through_link", Writer::Doctor, true, "write"),
            ("chmod_link", Writer::Doctor, true, "chmod"),
        ];
        for (case, writer, flag, shape) in cases {
            let f = Fixture::new();
            let (mut link, quarantine) = crashed_link(&f);
            let state = f.paths.state_dir().to_path_buf();
            match shape {
                "plain_source" => {
                    link = state.join(".cache-link");
                    std::os::unix::fs::symlink("doctor/runs/old", &link).unwrap();
                }
                "addons" => {
                    link = f.client.join("Interface/AddOns/.omaddons-abc");
                    std::os::unix::fs::symlink("WeakAuras", &link).unwrap();
                }
                _ => {}
            }
            let elsewhere = state.join("elsewhere");
            let cache = f.paths.cache_dir().join("x");
            for dir in [&elsewhere, &cache] {
                fs::create_dir_all(dir).unwrap();
            }
            let other = quarantine.join("other");
            fs::write(&other, b"x").unwrap();
            let dest = match shape {
                "plain_destination" => quarantine.join("abc"),
                "elsewhere" => elsewhere.join(".omaddons-abc"),
                "cache" => cache.join(".omaddons-abc"),
                _ => quarantine.join(link.file_name().unwrap()),
            };
            let op = match shape {
                "replace" => Op::Rename {
                    to: &dest,
                    mode: RenameMode::Replace,
                },
                "exchange" => Op::Rename {
                    to: &other,
                    mode: RenameMode::Exchange,
                },
                "write" => write(b"through"),
                "chmod" => Op::Chmod { mode: 0o600 },
                _ => no_replace(&dest),
            };
            let opts = if flag {
                temp_symlink()
            } else {
                MutateOpts::default()
            };
            let before = fs::read_link(&link).unwrap();
            let mut s = f.tx_session(writer);
            let result = mutate_with(&mut s, &link, op, opts);
            assert!(
                matches!(&result, Err(FsxError::ScopeDenied(m)) if m.contains("is a symlink")),
                "{case}: {} → {result:?}",
                link.display()
            );
            assert_eq!(
                fs::read_link(&link).unwrap(),
                before,
                "{case}: the link is unchanged"
            );
            // An exchange names an existing file; every other case a free destination.
            assert!(
                fs::symlink_metadata(&dest).is_err() || matches!(shape, "exchange"),
                "{case}: nothing at the destination"
            );
            assert!(!f.actions_log().exists(), "{case}: no actions line");
        }
    }

    #[test]
    fn a_tracked_chmod_is_durable() {
        let f = Fixture::new();
        let file = f.paths.state_dir().join("x");
        let mut s = f.tx_session(Writer::Mutating);
        ensure_dir_all(&mut s, f.paths.state_dir()).unwrap();
        mutate(&mut s, &file, write(b"x")).unwrap();
        let before = sys::sync_counts();
        mutate(&mut s, &file, Op::Chmod { mode: 0o200 }).unwrap();
        // The actions line's own fsync, and now the mode's.
        assert_eq!(sys::sync_counts().fsync - before.fsync, 2);
        assert_eq!(
            mode_of(&file),
            0o200,
            "set through the descriptor opened before"
        );
    }

    #[test]
    fn an_untracked_chmod_adds_no_fsync() {
        let f = Fixture::new();
        let file = f.paths.cache_dir().join("x");
        fs::create_dir_all(f.paths.cache_dir()).unwrap();
        fs::write(&file, b"x").unwrap();
        let mut s = WriteSession::untracked(Writer::Derived, &f.scopes, "a test");
        let before = sys::sync_counts();
        mutate(&mut s, &file, Op::Chmod { mode: 0o600 }).unwrap();
        assert_eq!(sys::sync_counts().fsync - before.fsync, 0);
    }

    #[test]
    fn a_predicate_failure_is_a_precondition_and_changes_nothing() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let file = state.join("registry.json");
        mutate(&mut s, &file, write(b"v1")).unwrap();
        let stale = identity_of(&file);
        replace_with_a_copy(&file);
        let snapshot = f.paths.state_snapshot(&f.tx);
        let (lines, backups, mode) = (f.actions().len(), listing(&snapshot), mode_of(&file));
        let got = mutate_with(&mut s, &file, write(b"v2"), expecting(stale));
        assert!(
            refused_with(
                got,
                &file,
                "another file is at this path (its identity changed)"
            ),
            "a stale identity is refused"
        );
        assert_eq!(f.actions().len(), lines, "no actions line");
        assert_eq!(listing(&snapshot), backups, "no backup");
        assert_eq!(fs::read(&file).unwrap(), b"v1");
        assert_eq!(mode_of(&file), mode);
    }

    /// Each part refuses its own kind of change through `mutate_with`, naming the path and
    /// the part, and the refused operation logs nothing. Each case's operation passes its
    /// own rules, so the predicate is what refuses.
    #[test]
    fn every_part_refuses_through_mutate_with() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let moved = state.join("moved");
        let rename = Op::Rename {
            to: &moved,
            mode: RenameMode::NoReplace,
        };
        let link_target = state.join("elsewhere");
        let symlink = Op::SymlinkAtomic {
            target: &link_target,
        };
        // (case, subject, operation, expectation, change, refusal)
        type Setup = fn(&Path);
        type Capture = fn(&Path) -> Expect;
        let cases: [(&str, Setup, Op<'_>, Capture, Setup, &str); 8] = [
            (
                "identity",
                |p| fs::write(p, b"v1").unwrap(),
                write(b"v2"),
                |p| identity_of(p),
                replace_with_a_copy,
                "another file is at this path (its identity changed)",
            ),
            (
                "sha256",
                |p| fs::write(p, b"v1").unwrap(),
                write(b"v2"),
                |_| Expect::hash(sha(b"v1")),
                |p| fs::write(p, b"v0").unwrap(),
                "its content changed (hash mismatch)",
            ),
            (
                "mode",
                |p| {
                    fs::write(p, b"v1").unwrap();
                    fs::set_permissions(p, fs::Permissions::from_mode(0o644)).unwrap();
                },
                write(b"v2"),
                |p| {
                    let parts = Parts {
                        mode: true,
                        ..Parts::default()
                    };
                    Expect::capture(p, parts).unwrap()
                },
                |p| fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap(),
                "its mode is 0600, expected 0644",
            ),
            (
                "stamp",
                |p| {
                    fs::create_dir(p).unwrap();
                    fs::write(p.join("a"), b"a").unwrap();
                },
                rename,
                |p| {
                    let parts = Parts {
                        stamp: true,
                        ..Parts::default()
                    };
                    Expect::capture(p, parts).unwrap()
                },
                |p| fs::write(p.join("b"), b"b").unwrap(),
                "the directory's contents changed (stamp mismatch)",
            ),
            (
                "link_target",
                |p| std::os::unix::fs::symlink("first", p).unwrap(),
                symlink,
                |p| {
                    let parts = Parts {
                        link_target: true,
                        ..Parts::default()
                    };
                    Expect::capture(p, parts).unwrap()
                },
                |p| {
                    fs::remove_file(p).unwrap();
                    std::os::unix::fs::symlink("second", p).unwrap();
                },
                "the symlink points elsewhere",
            ),
            (
                "file_type",
                |p| fs::create_dir(p).unwrap(),
                Op::Rename {
                    to: &moved,
                    mode: RenameMode::NoReplace,
                },
                |_| {
                    Expect::Present(Predicate {
                        file_type: Some(FileKind::Directory),
                        ..Predicate::default()
                    })
                },
                |p| {
                    fs::remove_dir(p).unwrap();
                    fs::write(p, b"a file now").unwrap();
                },
                "is a regular file, expected a directory",
            ),
            (
                "absent",
                |_| {},
                write(b"v1"),
                |_| Expect::Absent,
                |p| fs::write(p, b"another writer").unwrap(),
                "expected nothing at this path",
            ),
            (
                "present",
                |p| fs::write(p, b"v1").unwrap(),
                write(b"v2"),
                |_| Expect::Present(Predicate::default()),
                |p| fs::remove_file(p).unwrap(),
                "expected something at this path",
            ),
        ];
        for (case, setup, op, capture, change, refusal) in cases {
            let subject = state.join(case);
            setup(&subject);
            let expect = capture(&subject);
            change(&subject);
            let lines = f.actions().len();
            let before = listing(&state);
            let got = mutate_with(&mut s, &subject, op, expecting(expect));
            assert!(
                refused_with(got, &subject, refusal),
                "{case}: refused with {refusal:?}"
            );
            assert_eq!(f.actions().len(), lines, "{case}: no actions line");
            assert_eq!(listing(&state), before, "{case}: nothing moved");
        }
    }

    #[test]
    fn predicates_are_checked_in_untracked_sessions_too() {
        let f = Fixture::new();
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "status.json");
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let status = state.join("status.json");
        mutate(&mut s, &status, write(b"{}")).unwrap();
        let stale = identity_of(&status);
        replace_with_a_copy(&status);
        for (case, expect) in [("absent", Expect::Absent), ("stale_identity", stale)] {
            let got = mutate_with(&mut s, &status, write(b"{\"v\":2}"), expecting(expect));
            assert!(
                matches!(got, Err(FsxError::Precondition(_))),
                "{case}: {got:?}"
            );
        }
        assert_eq!(fs::read(&status).unwrap(), b"{}");
    }

    #[test]
    fn a_rename_checks_its_destination() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let (a, b) = (state.join("a"), state.join("b"));
        mutate(&mut s, &a, write(b"A")).unwrap();
        mutate(&mut s, &b, write(b"B")).unwrap();
        let rename = |s: &mut WriteSession<'_>, mode, expect_to| {
            let opts = MutateOpts {
                expect_to: Some(expect_to),
                ..MutateOpts::default()
            };
            mutate_with(s, &a, Op::Rename { to: &b, mode }, opts)
        };

        let recorded = identity_of(&b);
        replace_with_a_copy(&b);
        let got = rename(&mut s, RenameMode::Exchange, recorded);
        assert!(
            refused_with(got, &b, "(its identity changed)"),
            "exchange_with_a_replaced_destination: refused, naming the destination"
        );
        assert_eq!(
            (fs::read(&a).unwrap(), fs::read(&b).unwrap()),
            (b"A".to_vec(), b"B".to_vec())
        );

        let recorded = identity_of(&b);
        let got = rename(&mut s, RenameMode::Exchange, recorded);
        assert!(
            got.is_ok(),
            "exchange_with_the_recorded_destination: {got:?}"
        );
        assert_eq!(fs::read(&b).unwrap(), b"A");

        fs::remove_file(&b).unwrap();
        let got = rename(&mut s, RenameMode::NoReplace, Expect::Absent);
        assert!(got.is_ok(), "noreplace_with_an_absent_destination: {got:?}");
        assert_eq!(fs::read(&b).unwrap(), b"B");

        mutate(&mut s, &a, write(b"new")).unwrap();
        let got = rename(&mut s, RenameMode::Replace, Expect::hash(sha(b"edited")));
        assert!(
            refused_with(got, &b, "its content changed (hash mismatch)"),
            "replace_with_the_destinations_sha256: refused once the destination changed"
        );
        let got = rename(&mut s, RenameMode::Replace, Expect::hash(sha(b"B")));
        assert!(got.is_ok(), "replace_with_the_destinations_sha256: {got:?}");
        assert_eq!(fs::read(&b).unwrap(), b"new");
    }

    #[test]
    fn a_destination_predicate_needs_a_rename() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let file = state.join("registry.json");
        let opts = MutateOpts {
            expect_to: Some(Expect::Absent),
            ..MutateOpts::default()
        };
        let got = mutate_with(&mut s, &file, write(b"v1"), opts);
        assert!(
            refused_with(got, &file, "a destination predicate needs a rename"),
            "a WriteFile with expect_to"
        );
        assert!(!file.exists(), "nothing was written");
    }

    #[test]
    fn the_existing_messages_are_kept() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let file = state.join("registry.json");
        mutate(&mut s, &file, write(b"v1")).unwrap();
        for (case, expect, text) in [
            ("absent", Expect::Absent, "expected nothing at this path"),
            (
                "sha256",
                Expect::hash(sha(b"v0")),
                "its content changed (hash mismatch)",
            ),
        ] {
            let got = mutate_with(&mut s, &file, write(b"v2"), expecting(expect));
            assert!(refused_with(got, &file, text), "{case}");
        }
    }

    #[test]
    fn a_present_predicate_replaces_as_hash_did() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let file = state.join("registry.json");
        mutate(&mut s, &file, write(b"v1")).unwrap();
        let lines = f.actions().len();
        let record = mutate_with(
            &mut s,
            &file,
            write(b"v2"),
            expecting(Expect::hash(sha(b"v1"))),
        )
        .unwrap();
        assert_eq!(
            fs::read(record.backup.unwrap()).unwrap(),
            b"v1",
            "verified backup"
        );
        assert_eq!(fs::read(&file).unwrap(), b"v2");
        assert_eq!(f.actions().len(), lines + 1, "one action logged");
    }

    /// M4-D31's compound chmod predicate: a regular file of the recorded identity with the
    /// old mode, so a same-mode replacement is never taken for the file inspected.
    #[test]
    fn a_chmod_checks_identity_and_old_mode_together() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let parts = Parts {
            identity: true,
            mode: true,
            ..Parts::default()
        };
        type Change = fn(&Path);
        let cases: [(&str, Change, Option<&str>); 3] = [
            ("unchanged", |_| {}, None),
            (
                "replaced_by_a_same_mode_copy",
                replace_with_a_copy,
                Some("(its identity changed)"),
            ),
            (
                "mode_already_changed",
                |p| fs::set_permissions(p, fs::Permissions::from_mode(0o640)).unwrap(),
                Some("its mode is 0640, expected 0644"),
            ),
        ];
        for (case, change, refusal) in cases {
            let file = state.join(case);
            mutate(&mut s, &file, write(b"token")).unwrap();
            let expect = Expect::capture(&file, parts).unwrap();
            assert!(
                matches!(
                    &expect,
                    Expect::Present(Predicate {
                        file_type: Some(FileKind::Regular),
                        identity: Some(_),
                        mode: Some(0o644),
                        ..
                    })
                ),
                "{case}: {expect:?}"
            );
            change(&file);
            let before = mode_of(&file);
            let got = mutate_with(&mut s, &file, Op::Chmod { mode: 0o600 }, expecting(expect));
            match refusal {
                None => {
                    assert!(got.is_ok(), "{case}: {got:?}");
                    assert_eq!(mode_of(&file), 0o600, "{case}");
                }
                Some(text) => {
                    assert!(refused_with(got, &file, text), "{case}");
                    assert_eq!(mode_of(&file), before, "{case}: the mode is untouched");
                }
            }
        }
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

    #[test]
    fn stamp_digests_cover_stats_not_content() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Account/A/SavedVariables")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        let sv = root.join("Account/A/SavedVariables/Foo.lua");
        fs::write(&sv, b"Foo = 1").unwrap();
        std::os::unix::fs::symlink("Account", root.join("link")).unwrap();
        let first = StampDigest::compute(root).unwrap();
        assert!(first.to_string().starts_with("stamp:sha256:"), "{first}");
        assert_eq!(first.to_string().len(), "stamp:sha256:".len() + 64);
        assert_eq!(StampDigest::compute(root).unwrap(), first, "stable");

        // From entries, in any order: the same digest.
        let entry = |rel: &str, kind| {
            let m = fs::symlink_metadata(root.join(rel)).unwrap();
            let mtime = m.mtime() * 1_000_000_000 + m.mtime_nsec();
            (rel.to_owned(), kind, m.len(), m.mode() & 0o7777, mtime)
        };
        let mut entries = vec![
            entry("Account", "dir"),
            entry("Account/A", "dir"),
            entry("Account/A/SavedVariables", "dir"),
            entry("Account/A/SavedVariables/Foo.lua", "file"),
            entry("empty", "dir"),
            entry("link", "symlink"),
        ];
        assert_eq!(StampDigest::from_entries(entries.clone()), first);
        entries.reverse();
        assert_eq!(StampDigest::from_entries(entries), first, "sorted first");

        // Content rewritten with the same size and mtime: no change (a stamp).
        let mtime = fs::metadata(&sv).unwrap().modified().unwrap();
        fs::write(&sv, b"Foo = 2").unwrap();
        fs::File::options()
            .write(true)
            .open(&sv)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(StampDigest::compute(root).unwrap(), first);
        // A mode, an empty directory or a new mtime: a change.
        fs::set_permissions(&sv, fs::Permissions::from_mode(0o600)).unwrap();
        let moded = StampDigest::compute(root).unwrap();
        assert_ne!(moded, first);
        fs::create_dir(root.join("empty/more")).unwrap();
        assert_ne!(StampDigest::compute(root).unwrap(), moded);
        assert_ne!(
            TreeDigest::compute(root).unwrap().to_string(),
            first.to_string()
        );
    }

    #[test]
    fn provided_file_digests_are_logged_without_reading_the_files() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::Mutating);
        let state = f.paths.state_dir().to_path_buf();
        ensure_dir_all(&mut s, &state).unwrap();
        let (a, b) = (state.join("a"), state.join("b"));
        fs::write(&a, b"aaa").unwrap();
        fs::write(&b, b"bbb").unwrap();
        let hashed = files_hashed();
        let known = MutateOpts {
            sha256_before: Some(sha(b"aaa")),
            sha256_after: Some(sha(b"bbb")),
            ..MutateOpts::default()
        };
        let exchange = Op::Rename {
            to: &b,
            mode: RenameMode::Exchange,
        };
        let record = mutate_with(&mut s, &a, exchange, known).unwrap();
        assert_eq!(files_hashed(), hashed, "neither file read");
        assert_eq!(
            (record.before_hash, record.after_hash, record.backup),
            (
                Some(format!("sha256:{}", hex(&sha(b"aaa")))),
                Some(format!("sha256:{}", hex(&sha(b"bbb")))),
                None
            ),
            "an exchange takes no backup"
        );
        assert_eq!(fs::read(&a).unwrap(), b"bbb");
    }

    #[test]
    fn sha256_hex_round_trips() {
        let digest = sha(b"saved variables");
        assert_eq!(sha256_from_hex(&hex(&digest)), Some(digest));
        assert_eq!(
            sha256_from_hex(&hex(&digest).to_ascii_uppercase()),
            Some(digest)
        );
        for bad in ["", "abc", &"g".repeat(64), &"ab".repeat(33)] {
            assert_eq!(sha256_from_hex(bad), None, "{bad}");
        }
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

    /// A restore's renames of a whole `WTF/` record the stat digests it passes
    /// (`stamp:sha256:`), never a content digest computed in the session (M3-D25); a
    /// rename without them keeps the content digest.
    #[test]
    fn a_whole_tree_rename_records_its_stamps() {
        let f = Fixture::new();
        let mut s = f.tx_session(Writer::BackupRestore);
        let staged = f.stage().join("WTF");
        ensure_dir_all(&mut s, &staged.join("Account/A")).unwrap();
        fs::write(staged.join("Account/A/bindings-cache.wtf"), b"x").unwrap();
        let live = f.client.join("WTF");
        let (staged_stamp, live_stamp) = (
            StampDigest::compute(&staged).unwrap(),
            StampDigest::compute(&live).unwrap(),
        );
        let opts = MutateOpts {
            stamp_before: Some(staged_stamp),
            stamp_after: Some(live_stamp),
            ..MutateOpts::default()
        };
        let op = Op::Rename {
            to: &live,
            mode: RenameMode::Exchange,
        };
        let swapped = mutate_with(&mut s, &staged, op, opts).unwrap();
        assert_eq!(
            (swapped.before_hash, swapped.after_hash),
            (Some(staged_stamp.to_string()), Some(live_stamp.to_string()))
        );
        assert!(staged_stamp.to_string().starts_with("stamp:sha256:"));
        assert!(
            live.join("Account/A/bindings-cache.wtf").is_file(),
            "exchanged"
        );
        // The same rename back, without stamps: the content digest, as before.
        let back = mutate(
            &mut s,
            &staged,
            Op::Rename {
                to: &live,
                mode: RenameMode::Exchange,
            },
        )
        .unwrap();
        assert!(
            back.before_hash.unwrap().starts_with("tree:sha256:"),
            "no stamp given"
        );
    }

    #[test]
    fn the_exchange_probe_says_what_the_filesystem_can_do() {
        let f = Fixture::new();
        let mut s = WriteSession::untracked(Writer::BackupRestore, &f.scopes, "probe");
        let probe = f.stage().join("probe");
        assert!(probe_exchange(&mut s, &probe).unwrap(), "tempdirs exchange");
        #[cfg(debug_assertions)]
        {
            let again = f.stage().join("probe-again");
            let unsupported =
                failpoint::with_spec("fsx.renameat2.einval", || probe_exchange(&mut s, &again));
            assert!(!unsupported.unwrap(), "EINVAL is unsupported, not an error");
        }
    }

    // ---- logged untracked sessions: the doctor runtime's `runtime.jsonl` ----

    /// `<state>/doctor/runs/`, the state dir made as the runtime finds it.
    fn runs_of(f: &Fixture) -> PathBuf {
        fs::create_dir_all(f.paths.state_dir()).unwrap();
        f.paths.doctor_runs_dir()
    }

    fn logged<'a>(f: &'a Fixture, log: &Path) -> WriteSession<'a> {
        WriteSession::untracked_logged(Writer::Doctor, &f.scopes, "doctor runtime", log.into())
    }

    /// The log's lines, and its text for failure messages.
    fn lines_of(log: &Path) -> (Vec<ActionRecord>, String) {
        let text = fs::read_to_string(log).unwrap_or_default();
        let lines = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (lines, text)
    }

    fn create_dir(s: &mut WriteSession<'_>, path: &Path) {
        mutate(s, path, Op::CreateDir).unwrap();
    }

    #[test]
    fn logged_session_writes_one_line_per_mutate() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        let log = run.join("runtime.jsonl");
        let mut s = logged(&f, &log);
        assert_eq!(
            (s.untracked_reason(), s.tx_id(), s.actions_log()),
            (Some("doctor runtime"), None, None)
        );
        for dir in [runs.parent().unwrap(), &runs, &run] {
            create_dir(&mut s, dir);
        }
        let report = mutate(&mut s, &run.join("report.json"), write(b"{}")).unwrap();
        assert_eq!((report.seq, report.tx), (4, None), "{report:?}");

        let (lines, text) = lines_of(&log);
        let ops: Vec<(u64, &str)> = lines.iter().map(|l| (l.seq, l.op.as_str())).collect();
        assert_eq!(
            ops,
            [
                (1, "create_dir"),
                (2, "create_dir"),
                (3, "create_dir"),
                (4, "write_file")
            ],
            "{text}"
        );
        for line in &lines {
            assert_eq!(
                (
                    &line.tx,
                    &line.before_hash,
                    &line.after_hash,
                    &line.backup,
                    line.ok
                ),
                (&None, &None, &None, &None, true),
                "no transaction, digests or backups: {text}"
            );
        }
        assert_eq!(lines[3].path, run.join("report.json").to_string_lossy());
        assert_eq!(s.logged_seq(), 4);
    }

    #[test]
    fn lines_wait_until_the_log_directory_exists() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        let log = run.join("runtime.jsonl");
        let mut s = logged(&f, &log);
        create_dir(&mut s, runs.parent().unwrap());
        create_dir(&mut s, &runs);
        assert!(!log.exists(), "no directory, no log yet");
        assert_eq!(s.logged_seq(), 2, "both lines wait");
        create_dir(&mut s, &run);
        let (lines, text) = lines_of(&log);
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        assert_eq!(seqs, [1, 2, 3], "written in order once it exists: {text}");
    }

    #[test]
    fn the_log_follows_a_rename_of_its_parent() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        fs::create_dir_all(&runs).unwrap();
        let staged = runs.join(".x.tmp");
        let run = runs.join("x");
        let mut s = logged(&f, &staged.join("runtime.jsonl"));
        create_dir(&mut s, &staged);
        mutate(
            &mut s,
            &staged,
            Op::Rename {
                to: &run,
                mode: RenameMode::NoReplace,
            },
        )
        .unwrap();
        assert_eq!(s.log_path(), Some(run.join("runtime.jsonl").as_path()));
        mutate(&mut s, &run.join("report.json"), write(b"{}")).unwrap();
        assert!(!staged.exists(), "the staged name is gone");
        let (lines, text) = lines_of(&run.join("runtime.jsonl"));
        let ops: Vec<&str> = lines.iter().map(|l| l.op.as_str()).collect();
        assert_eq!(
            ops,
            ["create_dir", "rename_noreplace", "write_file"],
            "{text}"
        );
        assert_eq!(
            lines[1].to.as_deref(),
            Some(run.to_string_lossy().as_ref()),
            "{text}"
        );
    }

    #[test]
    fn appends_survive_chmod_0400() {
        if rustix::process::geteuid().is_root() {
            return; // root writes a 0400 file anyway, so there is nothing to show
        }
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        fs::create_dir_all(&run).unwrap();
        let log = run.join("runtime.jsonl");
        let report = run.join("report.json");
        let mut s = logged(&f, &log);
        mutate(&mut s, &report, write(b"{}")).unwrap();
        mutate(&mut s, &log, Op::Chmod { mode: 0o400 }).unwrap();
        mutate(&mut s, &report, Op::Chmod { mode: 0o400 }).unwrap();
        let (lines, text) = lines_of(&log);
        let ops: Vec<(&str, &str)> = lines
            .iter()
            .map(|l| (l.op.as_str(), l.path.rsplit('/').next().unwrap_or_default()))
            .collect();
        assert_eq!(
            ops,
            [
                ("write_file", "report.json"),
                ("chmod", "runtime.jsonl"),
                ("chmod", "report.json")
            ],
            "its own chmod and the next line, through the open descriptor: {text}"
        );
        let mode = fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400);
    }

    #[test]
    fn a_refused_op_is_not_logged() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(runs.join("y")).unwrap();
        let log = run.join("runtime.jsonl");
        let mut s = logged(&f, &log);
        mutate(&mut s, &run.join("report.json"), write(b"{}")).unwrap();
        let outside = f.client.parent().unwrap().join("elsewhere.txt");
        let cases: [(&str, &Path, Op<'_>); 2] = [
            ("outside every scope", &outside, write(b"x")),
            (
                "onto an existing destination",
                &run,
                Op::Rename {
                    to: &runs.join("y"),
                    mode: RenameMode::NoReplace,
                },
            ),
        ];
        for (case, path, op) in cases {
            let refused = mutate(&mut s, path, op);
            assert!(
                matches!(
                    refused,
                    Err(FsxError::ScopeDenied(_) | FsxError::Precondition(_))
                ),
                "{case}: {refused:?}"
            );
            let (lines, text) = lines_of(&log);
            assert_eq!(
                (s.logged_seq(), lines.len()),
                (1, 1),
                "{case}: no line: {text}"
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_logged_session_logs_a_failed_execution_with_ok_false() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        fs::create_dir_all(&run).unwrap();
        let log = run.join("runtime.jsonl");
        let mut s = logged(&f, &log);
        let failed = failpoint::with_spec("fsx.write_atomic.before=error", || {
            mutate(&mut s, &run.join("report.json"), write(b"{}"))
        });
        assert!(matches!(failed, Err(FsxError::Io { .. })), "{failed:?}");
        let (lines, text) = lines_of(&log);
        let logged: Vec<(u64, &str, bool)> =
            lines.iter().map(|l| (l.seq, l.op.as_str(), l.ok)).collect();
        assert_eq!(logged, [(1, "write_file", false)], "{text}");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_line_that_cannot_be_written_fails_its_mutate() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        fs::create_dir_all(&run).unwrap();
        let log = run.join("runtime.jsonl");
        let report = run.join("report.json");
        let mut s = logged(&f, &log);
        let failed = failpoint::with_spec("fsx.log_line.before=error", || {
            mutate(&mut s, &report, write(b"{}"))
        });
        let named = match &failed {
            Err(FsxError::Io { path, .. }) => Some(path.as_path()),
            _ => None,
        };
        assert_eq!(
            named,
            Some(log.as_path()),
            "an I/O error naming the log: {failed:?}"
        );
        assert_eq!(
            fs::read(&report).unwrap(),
            b"{}",
            "the operation itself ran"
        );
        // The line waits, and goes out with the next one.
        mutate(&mut s, &report, Op::Chmod { mode: 0o600 }).unwrap();
        let (lines, text) = lines_of(&log);
        let seqs: Vec<u64> = lines.iter().map(|l| l.seq).collect();
        assert_eq!(seqs, [1, 2], "{text}");
    }

    #[test]
    fn the_log_is_created_0600() {
        let f = Fixture::new();
        let runs = runs_of(&f);
        let run = runs.join("x");
        fs::create_dir_all(&run).unwrap();
        let log = run.join("runtime.jsonl");
        let mut s = logged(&f, &log);
        mutate(&mut s, &run.join("report.json"), write(b"{}")).unwrap();
        let mode = fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}
