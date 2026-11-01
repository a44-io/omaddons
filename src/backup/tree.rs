//! The one `WTF/` tree copy (M3-D23): `backup create` copies a live tree into a stage,
//! and a restore stages a stored copy (or a snapshot) the same way. It never hashes file
//! content: the manifest records each source entry's stats, and the destination's
//! [`StampDigest`] is taken by a stat walk afterwards.
//!
//! A tree is not captured atomically, so the copy measures what happened (capture
//! consistency): each file is stamped before and after its copy (with `settle`, a file
//! that changed is copied once more, and a second change counts as `unsettled`); the
//! root's identity is compared before and after; and a second stat walk of the source
//! is compared with the manifest, which catches a file changed, added or removed after
//! its own copy. `Stable` means observed quiescent, not a filesystem snapshot.
//!
//! The caller passes an untracked session whose destination lies under a client's
//! `<meta>/stage/<tx>/`, the only place deferred durability is allowed; one `syncfs`
//! ends the copy. Paths in errors stay relative to `WTF/`, and [`CloneError::error`]
//! redacts them: every path below `WTF/Account/` names the player.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::{Consistency, EntryKind, ManifestEntry, Reflinked};
use crate::error::{Error, ErrorCode, Suggestion};
use crate::fsx::{
    CloneKind, ClonePolicy, Durability, FileStamp, FsxError, MutateOpts, Op, RenameMode,
    StampDigest, WriteSession, ensure_dir_all, mutate, mutate_with, sync_filesystem,
};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::output::{Phase, ProgressSink};
use crate::privacy::WtfPathRedactor;

/// The fixed label of `copy` progress (M3-D21: never a path).
pub const COPY_LABEL: &str = "WTF/ files";

/// The fixed label of hashing progress (`backup create --verify`).
pub const VERIFY_LABEL: &str = "verifying";

/// Files between two `copy` progress events.
const PROGRESS_EVERY: u64 = 256;

/// How to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneOpts {
    /// Copy a file that changed during its copy once more.
    pub settle: bool,
    pub policy: ClonePolicy,
    pub max_entries: usize,
    pub max_depth: usize,
    /// Give directories exactly their recorded modes (a restore's staging, from the
    /// backup's manifest); otherwise each stays owner-writable, so a stored copy can
    /// always be pruned.
    pub exact_modes: bool,
}

impl CloneOpts {
    /// A backup's capture of a live `WTF/`.
    pub fn capture(policy: ClonePolicy) -> Self {
        Self {
            settle: true,
            policy,
            max_entries: BACKUP_TREE_MAX_ENTRIES,
            max_depth: BACKUP_TREE_MAX_DEPTH,
            exact_modes: false,
        }
    }

    /// A restore's staging of a stored copy, which nothing writes to.
    pub fn restore(policy: ClonePolicy) -> Self {
        Self {
            settle: false,
            exact_modes: true,
            ..Self::capture(policy)
        }
    }
}

/// What the second walk of the source found different from the manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InventoryDelta {
    pub changed: u64,
    pub added: u64,
    pub removed: u64,
}

impl InventoryDelta {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What a copy did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeCopy {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    /// Devices, FIFOs, sockets and names that are not UTF-8: never copied.
    pub skipped: u64,
    /// The apparent size of the files copied.
    pub bytes: u64,
    /// Payload bytes written by byte copies rather than cloned.
    pub copied_bytes: u64,
    pub reflinked: Reflinked,
    pub unsettled: u64,
    pub consistency: Consistency,
    pub changes: InventoryDelta,
    /// The destination's stat digest after the copy.
    pub stamp_digest: StampDigest,
    /// The source's entries as copied, in path order.
    pub manifest: Vec<ManifestEntry>,
}

/// Why a copy stopped. Its `Display` never names a path.
#[derive(Debug, thiserror::Error)]
pub enum CloneError {
    /// Past a bound (`limit` names the constant).
    #[error("the tree is larger than {limit} allows ({max})")]
    TooLarge { limit: &'static str, max: usize },
    /// The policy required a reflink and the filesystem has none; no payload byte was
    /// copied.
    #[error("the filesystem cannot clone files")]
    NotReflinkable,
    /// An entry vanished or changed type mid-copy, or the root was replaced (`WTF`).
    #[error("the tree changed while it was copied")]
    Changed { path: String },
    /// A read or write failed at `path` (relative; `""` for the root).
    #[error("copying the tree failed: {cause}")]
    Io { path: String, cause: String },
    /// A stuck journal's fence refused a write (E14): reported as that journal's stop.
    #[error("{}", .0.message())]
    Fenced(Box<crate::fsx::Fenced>),
}

impl CloneError {
    /// The command error, with any path passed through `redactor`: `TooLarge` is
    /// `archive_too_large` (`details.limit` the bound, `details.constant` its name), a
    /// change is a retryable `io`, and so is a failed read or write.
    pub fn error(&self, redactor: &WtfPathRedactor) -> Error {
        let retry = || Suggestion::text("run it again once the game and any sync tool are idle");
        match self {
            Self::TooLarge { limit, max } => Error::new(
                ErrorCode::ArchiveTooLarge,
                format!("WTF/ is larger than a backup copies ({limit}: {max})"),
                Suggestion::text("move files that do not belong in WTF/ out of it"),
            )
            .details(serde_json::json!({ "limit": max, "constant": limit })),
            Self::NotReflinkable => Error::new(
                ErrorCode::Io,
                "the filesystem under this client cannot clone files, and this copy may not byte-copy",
                Suggestion::text(
                    "a copy-on-write filesystem (btrfs, xfs) clones; on others a byte copy is needed",
                ),
            ),
            Self::Changed { path } => Error::new(
                ErrorCode::Io,
                format!("WTF/ changed while it was copied ({})", redactor.rel(path)),
                retry(),
            )
            .retryable(true)
            .details(serde_json::json!({ "path": redactor.rel(path) })),
            Self::Io { path, cause } => Error::new(
                ErrorCode::Io,
                format!("copying WTF/ failed at {:?}: {cause}", redactor.rel(path)),
                retry(),
            )
            .retryable(true)
            .details(serde_json::json!({ "path": redactor.rel(path) })),
            // It names the fenced resource, never a path below `WTF/`.
            Self::Fenced(fenced) => FsxError::Fenced(fenced.clone()).into(),
        }
    }
}

/// A walk of a tree: its entries in path order (a directory before its contents), and
/// what it did not list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walk {
    pub entries: Vec<ManifestEntry>,
    pub skipped: u64,
}

/// Every entry below `root`, stat'd without following a symlink, in path order. An
/// entry that vanishes mid-walk is [`CloneError::Changed`]; past `max_entries` entries
/// or `max_depth` levels it is [`CloneError::TooLarge`].
pub fn walk(root: &Path, max_entries: usize, max_depth: usize) -> Result<Walk, CloneError> {
    let mut entries = Vec::new();
    let mut skipped = 0;
    let mut seen = 0usize;
    let mut pending: Vec<(String, usize)> = vec![(String::new(), 0)];
    while let Some((rel, depth)) = pending.pop() {
        let dir = join(root, &rel);
        let listing = std::fs::read_dir(&dir).map_err(|e| vanished_or_io(&rel, &e))?;
        let mut names: Vec<OsString> = Vec::new();
        for item in listing {
            names.push(item.map_err(|e| vanished_or_io(&rel, &e))?.file_name());
        }
        names.sort();
        for name in names {
            seen += 1;
            if seen > max_entries {
                return Err(CloneError::TooLarge {
                    limit: "BACKUP_TREE_MAX_ENTRIES",
                    max: max_entries,
                });
            }
            if depth + 1 > max_depth {
                return Err(CloneError::TooLarge {
                    limit: "BACKUP_TREE_MAX_DEPTH",
                    max: max_depth,
                });
            }
            let Some(name) = name.to_str() else {
                skipped += 1;
                continue;
            };
            let child = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            let meta = std::fs::symlink_metadata(join(root, &child))
                .map_err(|e| vanished_or_io(&child, &e))?;
            let kind = meta.file_type();
            let (kind, link) = if kind.is_dir() {
                pending.push((child.clone(), depth + 1));
                (EntryKind::Dir, None)
            } else if kind.is_file() {
                (EntryKind::File, None)
            } else if kind.is_symlink() {
                let target = std::fs::read_link(join(root, &child))
                    .map_err(|e| vanished_or_io(&child, &e))?;
                (
                    EntryKind::Symlink,
                    Some(target.to_string_lossy().into_owned()),
                )
            } else {
                skipped += 1;
                continue;
            };
            entries.push(entry(child, kind, &meta, link));
        }
    }
    entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(Walk { entries, skipped })
}

/// Copies the tree at `from` to `to` (which must not exist; its parent is created) under
/// `opts`, reporting `copy` progress under [`COPY_LABEL`]. `recorded` is the manifest a
/// restore stages from: with `exact_modes`, each directory gets the mode it records
/// (the stored copy's own were kept owner-writable); `None` takes the source's modes.
pub fn clone_tree(
    s: &mut WriteSession<'_>,
    from: &Path,
    to: &Path,
    opts: &CloneOpts,
    recorded: Option<&[ManifestEntry]>,
    progress: &ProgressSink,
) -> Result<TreeCopy, CloneError> {
    clone_tree_with(s, from, to, opts, recorded, progress, &mut |_| {})
}

/// A point in a copy, for tests that change the source there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step<'a> {
    /// `rel` was copied (attempt 1 or 2), before its second stamp.
    Copied(&'a str, u8),
    /// Every entry is copied; the root's identity and the second walk come next.
    Done,
}

pub(crate) fn clone_tree_with(
    s: &mut WriteSession<'_>,
    from: &Path,
    to: &Path,
    opts: &CloneOpts,
    recorded: Option<&[ManifestEntry]>,
    progress: &ProgressSink,
    hook: &mut dyn FnMut(Step<'_>),
) -> Result<TreeCopy, CloneError> {
    let recorded: BTreeMap<&str, u32> = recorded
        .unwrap_or_default()
        .iter()
        .filter(|e| e.kind == EntryKind::Dir)
        .map(|e| (e.path.as_str(), e.mode))
        .collect();
    let root_before = identity(from)?;
    let walked = walk(from, opts.max_entries, opts.max_depth)?;
    let total = walked
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .count() as u64;
    if let Some(parent) = to.parent() {
        ensure_dir_all(s, parent).map_err(|e| fsx_error("", e))?;
    }
    mutate_with(s, to, Op::CreateDir, deferred()).map_err(|e| fsx_error("", e))?;

    let mut copy = Counts::default();
    let mut manifest = Vec::with_capacity(walked.entries.len());
    for entry in walked.entries {
        let (src, dst) = (join(from, &entry.path), join(to, &entry.path));
        match entry.kind {
            EntryKind::Dir => {
                mutate_with(s, &dst, Op::CreateDir, deferred())
                    .map_err(|e| fsx_error(&entry.path, e))?;
                copy.dirs += 1;
                manifest.push(entry);
            }
            EntryKind::Symlink => {
                let target = PathBuf::from(entry.link.clone().unwrap_or_default());
                mutate(s, &dst, Op::SymlinkAtomic { target: &target })
                    .map_err(|e| fsx_error(&entry.path, e))?;
                copy.symlinks += 1;
                manifest.push(entry);
            }
            EntryKind::File => {
                let copied = copy_file(s, &src, &dst, &entry.path, opts, hook)?;
                copy.files += 1;
                copy.bytes += copied.entry.size;
                match copied.kind {
                    CloneKind::Reflink => copy.reflinked += 1,
                    CloneKind::ByteCopy => copy.copied_bytes += copied.entry.size,
                }
                copy.unsettled += u64::from(!copied.settled);
                if copy.files % PROGRESS_EVERY == 0 {
                    progress.count(Phase::Copy, copy.files, total, COPY_LABEL);
                }
                manifest.push(copied.entry);
            }
        }
    }
    progress.count(Phase::Copy, copy.files, total, COPY_LABEL);

    // Directory modes last, deepest first, so a read-only directory gets its children.
    for entry in manifest.iter().rev().filter(|e| e.kind == EntryKind::Dir) {
        let mode = if opts.exact_modes {
            recorded
                .get(entry.path.as_str())
                .copied()
                .unwrap_or(entry.mode)
                & 0o7777
        } else {
            (entry.mode & 0o777) | 0o700
        };
        mutate(s, &join(to, &entry.path), Op::Chmod { mode })
            .map_err(|e| fsx_error(&entry.path, e))?;
    }
    hook(Step::Done);
    if identity(from)? != root_before {
        return Err(CloneError::Changed {
            path: "WTF".to_owned(),
        });
    }
    let changes = recheck(from, &manifest, opts);
    let stamp_digest = StampDigest::compute(to).map_err(|e| io_error("", &e))?;
    sync_filesystem(to).map_err(|e| fsx_error("", e))?;
    let consistency = if copy.unsettled == 0 && changes.is_empty() {
        Consistency::Stable
    } else {
        Consistency::BestEffort
    };
    let reflinked = match copy.reflinked {
        // ubs:ignore — comparing file counts, not secrets
        n if n == copy.files => Reflinked::All,
        0 => Reflinked::None,
        _ => Reflinked::Some,
    };
    Ok(TreeCopy {
        files: copy.files,
        dirs: copy.dirs,
        symlinks: copy.symlinks,
        skipped: walked.skipped,
        bytes: copy.bytes,
        copied_bytes: copy.copied_bytes,
        reflinked,
        unsettled: copy.unsettled,
        consistency,
        changes,
        stamp_digest,
        manifest,
    })
}

/// The lowercase hex SHA-256 of each regular file `entries` lists below `root`, in their
/// order; `None` for a directory or a symlink. Reports `verify` progress under
/// [`VERIFY_LABEL`] (a phase of its own: counts never step back within one, and a copy's
/// have already reached their total). `backup create --verify` records them in its
/// manifest, and `backup verify` compares them with one.
pub fn hash_files(
    root: &Path,
    entries: &[ManifestEntry],
    progress: &ProgressSink,
) -> Result<Vec<Option<String>>, CloneError> {
    let total = entries.iter().filter(|e| e.kind == EntryKind::File).count() as u64;
    let mut hashed = 0;
    let mut hashes = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.kind != EntryKind::File {
            hashes.push(None);
            continue;
        }
        crate::fsx::count_hashed();
        let (_, sha256) = crate::state::files_index::hash_file_streaming(&join(root, &entry.path))
            .map_err(|e| io_error(&entry.path, &e))?;
        hashes.push(Some(sha256));
        hashed += 1;
        if hashed % PROGRESS_EVERY == 0 {
            progress.count(Phase::Verify, hashed, total, VERIFY_LABEL);
        }
    }
    progress.count(Phase::Verify, hashed, total, VERIFY_LABEL);
    Ok(hashes)
}

#[derive(Debug, Default)]
struct Counts {
    files: u64,
    dirs: u64,
    symlinks: u64,
    bytes: u64,
    copied_bytes: u64,
    reflinked: u64,
    unsettled: u64,
}

/// One file's copy.
struct Copied {
    /// The source's stats as copied.
    entry: ManifestEntry,
    kind: CloneKind,
    settled: bool,
}

/// Clones `src` to `dst`, stamping `src` before and after; with `settle`, a change in
/// between copies it once more, through a temp name renamed over the first copy.
fn copy_file(
    s: &mut WriteSession<'_>,
    src: &Path,
    dst: &Path,
    rel: &str,
    opts: &CloneOpts,
    hook: &mut dyn FnMut(Step<'_>),
) -> Result<Copied, CloneError> {
    let stamp = || match FileStamp::read(src) {
        Ok(Some(stamp)) if stamp.is_file() => Ok(stamp),
        Ok(_) => Err(CloneError::Changed {
            path: rel.to_owned(),
        }),
        Err(e) => Err(vanished_or_io(rel, &e)),
    };
    let before = stamp()?;
    let mut kind = clone(s, src, dst, rel, opts.policy)?;
    hook(Step::Copied(rel, 1));
    let mut settled = stamp()? == before;
    // The stamp the copy is judged against: the manifest's stats must be of it too.
    let mut compared = before;
    if !settled && opts.settle {
        let again_before = stamp()?;
        compared = again_before;
        let temp = dst.with_file_name(format!(
            ".omaddons-settle-{}",
            dst.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
        kind = clone(s, src, &temp, rel, opts.policy)?;
        let op = Op::Rename {
            to: dst,
            mode: RenameMode::Replace,
        };
        mutate(s, &temp, op).map_err(|e| fsx_error(rel, e))?;
        hook(Step::Copied(rel, 2));
        settled = stamp()? == again_before;
    }
    let meta = std::fs::symlink_metadata(src).map_err(|e| vanished_or_io(rel, &e))?;
    if !meta.is_file() {
        return Err(CloneError::Changed {
            path: rel.to_owned(),
        });
    }
    // A write between the last stamp and these stats would put its size and mtime in the
    // manifest beside the older bytes copied: then the copy is not settled.
    settled &= stamp()? == compared;
    let mut recorded = entry(rel.to_owned(), EntryKind::File, &meta, None);
    if !settled {
        // The bytes kept are the copy's, written before the source changed: their size is
        // what a structure check compares, or the backup fails its own check.
        recorded.size = std::fs::symlink_metadata(dst)
            .map_err(|e| io_error(rel, &e))?
            .len();
    }
    Ok(Copied {
        entry: recorded,
        kind,
        settled,
    })
}

fn clone(
    s: &mut WriteSession<'_>,
    src: &Path,
    dst: &Path,
    rel: &str,
    policy: ClonePolicy,
) -> Result<CloneKind, CloneError> {
    let op = Op::CloneFile { from: src, policy };
    match mutate_with(s, dst, op, deferred()) {
        Ok(record) => Ok(record.clone.unwrap_or(CloneKind::ByteCopy)),
        Err(FsxError::Io { source, .. })
            if policy == ClonePolicy::Required && source.kind() == io::ErrorKind::Unsupported =>
        {
            Err(CloneError::NotReflinkable)
        }
        Err(FsxError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Err(CloneError::Changed {
                path: rel.to_owned(),
            })
        }
        Err(e) => Err(fsx_error(rel, e)),
    }
}

/// What a second walk of `from` finds different from `manifest`: entries added or
/// removed, of another kind, or files and symlinks of another size or mtime.
fn recheck(from: &Path, manifest: &[ManifestEntry], opts: &CloneOpts) -> InventoryDelta {
    let mut delta = InventoryDelta::default();
    let Ok(again) = walk(from, opts.max_entries, opts.max_depth) else {
        // It changed (or grew past a bound) while it was walked again.
        delta.changed += 1;
        return delta;
    };
    let copied: BTreeMap<&str, &ManifestEntry> =
        manifest.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut found = 0;
    for now in &again.entries {
        match copied.get(now.path.as_str()) {
            None => delta.added += 1,
            Some(was) => {
                found += 1;
                let stats = now.kind != EntryKind::Dir
                    && (now.size != was.size || now.mtime_ns != was.mtime_ns);
                if now.kind != was.kind || stats {
                    delta.changed += 1;
                }
            }
        }
    }
    delta.removed = (manifest.len() - found) as u64;
    delta
}

/// A root's identity: device, inode and, where the filesystem keeps one, birth time.
fn identity(root: &Path) -> Result<(u64, u64, Option<std::time::SystemTime>), CloneError> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| io_error("", &e))?;
    if !meta.is_dir() {
        return Err(CloneError::Changed {
            path: "WTF".to_owned(),
        });
    }
    Ok((meta.dev(), meta.ino(), meta.created().ok()))
}

fn entry(
    path: String,
    kind: EntryKind,
    meta: &std::fs::Metadata,
    link: Option<String>,
) -> ManifestEntry {
    ManifestEntry {
        path,
        kind,
        size: meta.len(),
        mode: meta.mode() & 0o7777,
        // Saturating: an mtime past 2262 (or before 1677) does not fit in i64 nanoseconds.
        mtime_ns: meta
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.mtime_nsec()),
        link,
        sha256: None,
    }
}

fn join(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

fn deferred() -> MutateOpts {
    MutateOpts {
        durability: Durability::Deferred,
        ..MutateOpts::default()
    }
}

fn vanished_or_io(rel: &str, e: &io::Error) -> CloneError {
    // ubs:ignore — an io::ErrorKind, not a secret
    if e.kind() == io::ErrorKind::NotFound {
        CloneError::Changed {
            path: rel.to_owned(),
        }
    } else {
        io_error(rel, e)
    }
}

/// An I/O failure; `std::io::Error` names no path.
fn io_error(rel: &str, e: &io::Error) -> CloneError {
    CloneError::Io {
        path: rel.to_owned(),
        cause: e.to_string(),
    }
}

/// What an I/O failure was, without any path: some of fsx's own errors name the
/// absolute path in their message (`… is not a regular file`), so only the OS error, or
/// else the kind, is said.
pub(crate) fn io_cause(e: &io::Error) -> String {
    e.raw_os_error().map_or_else(
        || e.kind().to_string(),
        |code| io::Error::from_raw_os_error(code).to_string(),
    )
}

/// An fsx failure, without the absolute path its message carries.
fn fsx_error(rel: &str, e: FsxError) -> CloneError {
    let cause = match e {
        FsxError::Io { source, .. } => io_cause(&source),
        FsxError::ScopeDenied(_) | FsxError::Protected(_) => {
            "the filesystem writer refused the path".to_owned()
        }
        FsxError::Precondition(_) => "the path was not as expected".to_owned(),
        FsxError::Unsupported => "the filesystem cannot rename atomically".to_owned(),
        // The fencing journal's own stop, whole (E14).
        FsxError::Fenced(fenced) => return CloneError::Fenced(fenced),
    };
    CloneError::Io {
        path: rel.to_owned(),
        cause,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{ScopeTable, Writer};
    use crate::state::paths::{ClientPaths, Paths};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const PRIVATE: &[&str] = &["FAKEACCT", "FakeRealm", "FakeChar", "OTHERACCT"];

    /// A client dir with a fixture `WTF/` and the scopes to stage under it.
    struct Fixture {
        _dir: tempfile::TempDir,
        client: PathBuf,
        scopes: ScopeTable,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(dir.path()).unwrap();
            let client = root.join("_retail_");
            fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
            let wtf = client.join("WTF");
            fs::create_dir_all(&wtf).unwrap();
            fs::write(wtf.join("Config.wtf"), "SET locale enUS\n").unwrap();
            let mut n = 0;
            for (account, chars) in [
                ("FAKEACCT", &["FakeRealm/FakeChar", "FakeRealm/Second"][..]),
                ("OTHERACCT", &["Other/Third"][..]),
            ] {
                let sv = wtf.join("Account").join(account).join("SavedVariables");
                fs::create_dir_all(&sv).unwrap();
                for char_dir in chars {
                    let dir = wtf.join("Account").join(account).join(char_dir);
                    fs::create_dir_all(dir.join("SavedVariables")).unwrap();
                    fs::write(dir.join("AddOns.txt"), "Foo: enabled\n").unwrap();
                    n += 1;
                }
                for i in 0..23 {
                    fs::write(sv.join(format!("Addon{i}.lua")), format!("DB{i} = {{}}\n")).unwrap();
                }
            }
            assert_eq!(n, 3);
            fs::create_dir(wtf.join("empty")).unwrap();
            std::os::unix::fs::symlink("Config.wtf", wtf.join("Config.link")).unwrap();
            let ro = wtf.join("readonly");
            fs::create_dir(&ro).unwrap();
            fs::write(ro.join("kept.txt"), "x").unwrap();
            fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
            let paths = Paths::with_roots(
                &root,
                root.join(".config"),
                root.join(".local/state"),
                root.join(".cache"),
            );
            let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
            Self {
                _dir: dir,
                client,
                scopes,
            }
        }

        fn wtf(&self) -> PathBuf {
            self.client.join("WTF")
        }

        /// `<meta>/stage/<name>/WTF`.
        fn stage(&self, name: &str) -> PathBuf {
            ClientPaths::new(&self.client)
                .meta_dir()
                .join("stage")
                .join(name)
                .join("WTF")
        }

        fn copy(&self, to: &Path, opts: &CloneOpts) -> Result<TreeCopy, CloneError> {
            self.copy_with(to, opts, &mut |_| {})
        }

        fn copy_with(
            &self,
            to: &Path,
            opts: &CloneOpts,
            hook: &mut dyn FnMut(Step<'_>),
        ) -> Result<TreeCopy, CloneError> {
            let mut s = WriteSession::untracked(Writer::Backup, &self.scopes, "backup");
            let none = ProgressSink::none();
            clone_tree_with(&mut s, &self.wtf(), to, opts, None, &none, hook)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Let the tempdir go: read-only directories are made writable again.
            for root in [self.wtf(), ClientPaths::new(&self.client).meta_dir()] {
                for entry in walk(&root, usize::MAX, usize::MAX)
                    .map(|w| w.entries)
                    .unwrap_or_default()
                {
                    if entry.kind == EntryKind::Dir {
                        let _ = fs::set_permissions(
                            join(&root, &entry.path),
                            fs::Permissions::from_mode(0o755),
                        );
                    }
                }
            }
        }
    }

    /// Paths, kinds, exact modes, link targets and contents below `dir`.
    fn tree_hash(dir: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        for e in walk(dir, usize::MAX, usize::MAX).unwrap().entries {
            hash.update(format!("{}\0{}\0{:o}\0", e.path, e.kind.wire(), e.mode));
            match e.kind {
                EntryKind::File => hash.update(fs::read(join(dir, &e.path)).unwrap()),
                EntryKind::Symlink => hash.update(e.link.unwrap_or_default()),
                EntryKind::Dir => {}
            }
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn stats(entries: &[ManifestEntry]) -> Vec<(String, EntryKind, u64, u32, Option<String>)> {
        entries
            .iter()
            .map(|e| (e.path.clone(), e.kind, e.size, e.mode, e.link.clone()))
            .collect()
    }

    #[test]
    fn a_copy_matches_its_source_and_an_exact_restore_reproduces_it() {
        let f = Fixture::new();
        let stored = f.stage("one");
        let copy = f
            .copy(&stored, &CloneOpts::capture(ClonePolicy::Preferred))
            .unwrap();
        assert_eq!((copy.files, copy.dirs, copy.symlinks), (51, 15, 1));
        assert_eq!(copy.consistency, Consistency::Stable);
        assert_eq!(
            (copy.unsettled, copy.changes),
            (0, InventoryDelta::default())
        );
        let fresh = walk(&f.wtf(), usize::MAX, usize::MAX).unwrap().entries;
        assert_eq!(
            copy.manifest, fresh,
            "the manifest is a stat walk of the source"
        );
        assert_eq!(
            fs::read_link(stored.join("Config.link")).unwrap(),
            Path::new("Config.wtf")
        );
        let mode = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode(&stored.join("readonly")),
            0o700,
            "stored owner-writable"
        );
        assert_eq!(
            copy.stamp_digest,
            StampDigest::compute(&stored).unwrap(),
            "the destination's stamp"
        );
        assert_ne!(tree_hash(&stored), tree_hash(&f.wtf()), "one mode differs");

        // A restore's staging of the stored copy, with the modes its manifest records.
        let mut s = WriteSession::untracked(Writer::Backup, &f.scopes, "restore");
        let restored = f.stage("two");
        let opts = CloneOpts::restore(ClonePolicy::Preferred);
        let none = ProgressSink::none();
        let staged = clone_tree(
            &mut s,
            &stored,
            &restored,
            &opts,
            Some(&copy.manifest),
            &none,
        )
        .unwrap();
        assert_eq!(
            tree_hash(&restored),
            tree_hash(&f.wtf()),
            "exactly the source"
        );
        assert_eq!(mode(&restored.join("readonly")), 0o500);
        assert_eq!(stats(&staged.manifest).len(), stats(&copy.manifest).len());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn clone_only_copies_no_byte_where_it_cannot_clone() {
        use crate::fsx::failpoint;
        let f = Fixture::new();
        let to = f.stage("required");
        let err = failpoint::with_spec("fsx.reflink.unsupported", || {
            f.copy(&to, &CloneOpts::capture(ClonePolicy::Required))
        })
        .unwrap_err();
        assert!(matches!(err, CloneError::NotReflinkable), "{err:?}");
        let files: Vec<ManifestEntry> = walk(&to, usize::MAX, usize::MAX)
            .unwrap()
            .entries
            .into_iter()
            .filter(|e| e.kind != EntryKind::Dir)
            .collect();
        assert!(files.is_empty(), "no partial file: {files:?}");

        let to = f.stage("preferred");
        let copy = failpoint::with_spec("fsx.reflink.unsupported", || {
            f.copy(&to, &CloneOpts::capture(ClonePolicy::Preferred))
        })
        .unwrap();
        assert_eq!(copy.reflinked, Reflinked::None);
        assert_eq!(copy.copied_bytes, copy.bytes);
        assert!(copy.bytes > 0);
    }

    #[test]
    fn a_file_rewritten_during_its_copy_is_copied_again() {
        let f = Fixture::new();
        let config = f.wtf().join("Config.wtf");
        let to = f.stage("settle");
        let copy = f
            .copy_with(
                &to,
                &CloneOpts::capture(ClonePolicy::Preferred),
                &mut |step| {
                    if step == Step::Copied("Config.wtf", 1) {
                        fs::write(&config, "SET locale deDE\nSET more 1\n").unwrap();
                    }
                },
            )
            .unwrap();
        assert_eq!(copy.unsettled, 0);
        assert_eq!(copy.consistency, Consistency::Stable);
        assert_eq!(
            fs::read_to_string(to.join("Config.wtf")).unwrap(),
            "SET locale deDE\nSET more 1\n"
        );
        assert!(!to.join(".omaddons-settle-Config.wtf").exists());

        // Changing on both attempts: unsettled.
        let to = f.stage("restless");
        let mut n = 0;
        let copy = f
            .copy_with(
                &to,
                &CloneOpts::capture(ClonePolicy::Preferred),
                &mut |step| {
                    if let Step::Copied("Config.wtf", _) = step {
                        n += 1;
                        fs::write(&config, "x".repeat(n)).unwrap();
                    }
                },
            )
            .unwrap();
        assert_eq!(
            (copy.unsettled, copy.consistency),
            (1, Consistency::BestEffort)
        );
        // Its manifest records the bytes kept, so the backup passes its own structure check.
        let recorded = copy
            .manifest
            .iter()
            .find(|e| e.path == "Config.wtf")
            .unwrap()
            .size;
        assert_eq!(recorded, fs::metadata(to.join("Config.wtf")).unwrap().len());
    }

    /// A named change to a source tree, and what the second walk must find.
    type Case<'a> = (&'a str, &'a dyn Fn(&Path), InventoryDelta);

    #[test]
    fn changes_after_a_files_own_copy_make_a_best_effort_copy() {
        let sv = "Account/FAKEACCT/SavedVariables";
        let cases: [Case<'_>; 3] = [
            (
                "changed",
                &|wtf: &Path| {
                    fs::write(wtf.join(format!("{sv}/Addon0.lua")), "DB0 = {1}\n").unwrap()
                },
                InventoryDelta {
                    changed: 1,
                    ..InventoryDelta::default()
                },
            ),
            (
                "added",
                &|wtf: &Path| fs::write(wtf.join(format!("{sv}/New.lua")), "").unwrap(),
                InventoryDelta {
                    added: 1,
                    ..InventoryDelta::default()
                },
            ),
            (
                "removed",
                &|wtf: &Path| fs::remove_file(wtf.join(format!("{sv}/Addon1.lua"))).unwrap(),
                InventoryDelta {
                    removed: 1,
                    ..InventoryDelta::default()
                },
            ),
        ];
        for (name, change, expected) in cases {
            let f = Fixture::new();
            let wtf = f.wtf();
            let to = f.stage(name);
            let copy = f
                .copy_with(
                    &to,
                    &CloneOpts::capture(ClonePolicy::Preferred),
                    &mut |step| {
                        // While a later file copies, one copied earlier changes.
                        if step == Step::Copied("Config.wtf", 1) {
                            change(&wtf);
                        }
                    },
                )
                .unwrap();
            assert_eq!(copy.changes, expected, "{name}");
            assert_eq!(copy.consistency, Consistency::BestEffort, "{name}");
        }
    }

    #[test]
    fn a_replaced_root_or_a_vanished_entry_is_a_change() {
        let f = Fixture::new();
        let wtf = f.wtf();
        let elsewhere = f.client.join("WTF.other");
        fs::create_dir(&elsewhere).unwrap();
        let err = f
            .copy_with(
                &f.stage("root"),
                &CloneOpts::capture(ClonePolicy::Preferred),
                &mut |step| {
                    if step == Step::Done {
                        fs::rename(&wtf, f.client.join("WTF.old")).unwrap();
                        fs::rename(&elsewhere, &wtf).unwrap();
                    }
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, CloneError::Changed { path } if path == "WTF"),
            "{err:?}"
        );

        let f = Fixture::new();
        let wtf = f.wtf();
        let err = f
            .copy_with(
                &f.stage("gone"),
                &CloneOpts::capture(ClonePolicy::Preferred),
                &mut |step| {
                    if let Step::Copied(rel, 1) = step
                        && rel.ends_with("Addon0.lua")
                    {
                        fs::remove_file(wtf.join("Account/FAKEACCT/SavedVariables/Addon1.lua"))
                            .unwrap();
                    }
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, CloneError::Changed { path } if path == "Account/FAKEACCT/SavedVariables/Addon1.lua"),
            "{err:?}"
        );
        let redactor = WtfPathRedactor::for_client(&ClientPaths::new(&f.client));
        let body = crate::error::ErrorBody::from(err.error(&redactor));
        let shown = format!("{} {} {err}", body.message, body.details);
        for name in PRIVATE {
            assert!(!shown.contains(name), "{shown}");
        }
        assert!(body.retryable);
    }

    #[test]
    fn fifos_are_skipped_and_the_bounds_trip() {
        let f = Fixture::new();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            f.wtf().join("pipe"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let copy = f
            .copy(
                &f.stage("fifo"),
                &CloneOpts::capture(ClonePolicy::Preferred),
            )
            .unwrap();
        assert_eq!(copy.skipped, 1);
        assert!(!f.stage("fifo").join("pipe").exists());

        let mut few = CloneOpts::capture(ClonePolicy::Preferred);
        few.max_entries = 10;
        let err = f.copy(&f.stage("many"), &few).unwrap_err();
        assert!(matches!(
            err,
            CloneError::TooLarge {
                limit: "BACKUP_TREE_MAX_ENTRIES",
                max: 10
            }
        ));
        let mut shallow = CloneOpts::capture(ClonePolicy::Preferred);
        shallow.max_depth = 3;
        let err = f.copy(&f.stage("deep"), &shallow).unwrap_err();
        assert!(matches!(
            err,
            CloneError::TooLarge {
                limit: "BACKUP_TREE_MAX_DEPTH",
                max: 3
            }
        ));
        let body = crate::error::ErrorBody::from(
            err.error(&WtfPathRedactor::for_paths(std::iter::empty())),
        );
        assert_eq!(body.code, ErrorCode::ArchiveTooLarge);
        assert_eq!(
            body.details,
            serde_json::json!({ "limit": 3, "constant": "BACKUP_TREE_MAX_DEPTH" })
        );
    }

    #[test]
    fn progress_carries_counts_under_a_fixed_label() {
        use crate::output::{OutputConfig, ProgressMode, Reporter};
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let f = Fixture::new();
        let cfg = OutputConfig {
            progress: ProgressMode::Json,
            ..OutputConfig::machine()
        };
        let captured = Captured::default();
        let sink: Box<dyn std::io::Write + Send> = Box::new(captured.clone());
        let reporter = Arc::new(Mutex::new(Reporter::new(sink, &cfg)));
        let progress = ProgressSink::new(&reporter, &cfg);
        let mut s = WriteSession::untracked(Writer::Backup, &f.scopes, "backup");
        let copy = clone_tree(
            &mut s,
            &f.wtf(),
            &f.stage("progress"),
            &CloneOpts::capture(ClonePolicy::Preferred),
            None,
            &progress,
        )
        .unwrap();
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events.len(), 1, "{text}");
        assert_eq!(events[0]["phase"], "copy");
        assert_eq!(events[0]["message"], COPY_LABEL);
        assert_eq!(events[0]["current"], copy.files);
        for name in PRIVATE {
            assert!(!text.contains(name), "{text}");
        }
    }
}
