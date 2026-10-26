//! The approval witness's foundation (M4-D60): the content of one folder as an
//! [`EntryDigest`], in which every entry kind counts (a symlink, an empty directory and
//! an empty file are content too), and the folder's stat stamp as private proof of
//! "unchanged since" once the folder has settled. A file whose stamp proves it
//! racy-clean against the files index takes its digest from the index, as drift's stat
//! mode trusts it (plus a ctime test, which an mtime put back cannot fool); every other
//! file is hashed.
//!
//! Read-only, and careful about what it opens: only entries the stamp calls regular
//! files, never following a link and never blocking, each checked by inode once open. A
//! folder that cannot be read completely fails closed: a token must never approve bytes
//! nobody observed.
//!
//! On that foundation, a target's [`ApprovalWitness`]: what one target's plan was
//! approved against, [`collect`]ed at the end of its planning when the command asks for
//! one (`TxRequest.witness`).

use std::collections::BTreeMap;
use std::fs::FileType;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use super::inspect::ArchiveLimits;
use super::plan::{ChangeKind, DepReport, TargetPlan, UnresolvedReason};
use super::swap::SwapOp;
use crate::error::{Error, Result, Suggestion};
use crate::fsx::{FileStamp, TreeStamp, WalkError};
use crate::model::{AssetLocation, FolderName, Target, TargetRef};
use crate::state::files_index::{
    FilesIndex, FolderIndex, hash_reader, limit_exceeded, unindexable,
};
use crate::state::paths::Paths;
use crate::state::store;

/// The content of one folder, every entry kind counted: a SHA-256 over length-prefixed
/// fields, each entry by its raw relative path in byte order. A `touch` or a `chmod`
/// does not move it; an added empty directory, empty file or symlink does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryDigest([u8; 32]);

impl std::fmt::Display for EntryDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("entry:sha256:")?;
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl EntryDigest {
    /// The digest of the folder at `dir` with every file hashed (no index): the re-hash
    /// under the commit lock.
    pub fn of_dir(dir: &Path, limits: &ArchiveLimits) -> Result<Self> {
        folder(dir, None, limits, i128::MAX, &mut || {}).map(|f| f.digest)
    }
}

/// What a witnessed folder is to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WitnessRole {
    Replaced,
    Removed,
    CopySource,
    Adopted,
}

impl WitnessRole {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Removed => "removed",
            Self::CopySource => "copy_source",
            Self::Adopted => "adopted",
        }
    }
}

/// One folder of a target's witness: its role, its key and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderWitness {
    pub role: WitnessRole,
    pub key: String,
    pub digest: EntryDigest,
}

/// What witnessing cost: files hashed, files whose digest the index gave, bytes hashed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WitnessCost {
    pub files_hashed: u64,
    pub files_reused: u64,
    pub bytes_hashed: u64,
}

/// One folder's evidence: its digest, its stamp when it was settled and did not move
/// while read (the under-lock check compares it; without one, the folder is hashed
/// again), and the cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub digest: EntryDigest,
    pub stamp: Option<TreeStamp>,
    pub cost: WitnessCost,
}

/// One entry as the digest encodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir,
    File {
        size: u64,
        sha256: String,
    },
    Link(Vec<u8>),
    /// A FIFO, socket, device or unknown kind, never opened.
    Other(&'static str),
    /// It changed while it was read: matches no steady-state digest.
    Changed,
    /// The folder itself is absent.
    Absent,
}

impl Entry {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Dir => "d",
            Self::File { .. } => "f",
            Self::Link(_) => "l",
            Self::Other(kind) => kind,
            Self::Changed => "x",
            Self::Absent => "a",
        }
    }
}

/// A big-endian `u64` length, then the bytes: no name can forge a boundary.
fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn digest_of(entries: &[(Vec<u8>, Entry)]) -> EntryDigest {
    let mut hasher = Sha256::new();
    field(&mut hasher, b"omaddons entry digest 1");
    for (path, entry) in entries {
        field(&mut hasher, entry.kind().as_bytes());
        field(&mut hasher, path);
        match entry {
            Entry::File { size, sha256 } => {
                field(&mut hasher, size.to_string().as_bytes());
                field(&mut hasher, sha256.as_bytes());
            }
            Entry::Link(target) => field(&mut hasher, target),
            Entry::Dir | Entry::Other(_) | Entry::Changed | Entry::Absent => {}
        }
    }
    EntryDigest(hasher.finalize().into())
}

/// The evidence of the folder at `dir` (see the module doc). `index` is the folder's
/// files index entry with its index, when the folder is managed; `now_ns` is the wall
/// clock taken before the first read (settledness is judged against it); `between` runs
/// after the entries are digested and before the stamp is read again (a no-op outside
/// tests).
pub fn folder(
    dir: &Path,
    index: Option<(&FilesIndex, &FolderIndex)>,
    limits: &ArchiveLimits,
    now_ns: i128,
    between: &mut dyn FnMut(),
) -> Result<Folder> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cost = WitnessCost::default();
    let mut retried = false;
    loop {
        let root = FileStamp::read(dir).map_err(|e| unreadable(&name, Path::new(""), &e))?;
        let Some(root) = root else {
            let digest = digest_of(&[(Vec::new(), Entry::Absent)]);
            return Ok(Folder {
                digest,
                stamp: None,
                cost,
            });
        };
        if !root.file_type().is_dir() {
            // A symlink, file or other kind at the folder's place: one entry, never walked.
            let entry = encode(dir, Path::new(""), &root, None, &name, limits, &mut cost)?;
            return Ok(Folder {
                digest: digest_of(&[(Vec::new(), entry)]),
                stamp: None,
                cost,
            });
        }
        let first = match TreeStamp::read_at_most(dir, limits.max_entries) {
            Ok(Some(stamp)) => stamp,
            Ok(None) => {
                return Err(limit_exceeded(
                    &name,
                    "entries",
                    limits.max_entries.saturating_add(1),
                    limits.max_entries,
                    "limits.archive_max_entries",
                ));
            }
            // Something moved while it was listed: read it once more.
            Err(e) if e.kind() == io::ErrorKind::NotFound && !retried => {
                retried = true;
                continue;
            }
            Err(e) => {
                let rel = WalkError::rel_of(&e).unwrap_or_else(|| Path::new(""));
                return Err(unreadable(&name, rel, &e));
            }
        };
        // Replaced between the two reads: start again once, then take it as it is.
        let root_is_dir = first
            .entries()
            .first()
            .is_some_and(|(_, stamp)| stamp.file_type().is_dir());
        if !root_is_dir && !retried {
            retried = true;
            continue;
        }
        let mut entries = Vec::with_capacity(first.entries().len());
        for (rel, stamp) in first.entries() {
            let entry = encode(dir, rel, stamp, index, &name, limits, &mut cost)?;
            entries.push((rel.as_os_str().as_bytes().to_vec(), entry));
        }
        between();
        let again = TreeStamp::read_at_most(dir, limits.max_entries)
            .ok()
            .flatten();
        let steady = again.as_ref() == Some(&first) && first.settled(now_ns);
        return Ok(Folder {
            digest: digest_of(&entries),
            stamp: steady.then_some(first),
            cost,
        });
    }
}

/// `dir/rel`, the folder itself for the empty path (no trailing slash).
fn full(dir: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    }
}

/// One entry, encoded from its stamp; a regular file's digest from the index when it is
/// racy-clean there, else hashed.
fn encode(
    dir: &Path,
    rel: &Path,
    stamp: &FileStamp,
    index: Option<(&FilesIndex, &FolderIndex)>,
    name: &str,
    limits: &ArchiveLimits,
    cost: &mut WitnessCost,
) -> Result<Entry> {
    let path = full(dir, rel);
    let kind = stamp.file_type();
    if kind.is_dir() {
        return Ok(Entry::Dir);
    }
    if kind.is_symlink() {
        return match std::fs::read_link(&path) {
            Ok(target) => Ok(Entry::Link(target.into_os_string().into_vec())),
            // Gone, or no longer a symlink.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(Entry::Changed)
            }
            Err(e) => Err(unreadable(name, rel, &e)),
        };
    }
    if !kind.is_file() {
        return Ok(Entry::Other(other_kind(kind)));
    }
    if let Some(reused) = reuse(rel, stamp, index) {
        cost.files_reused += 1;
        return Ok(reused);
    }
    let over = |measured: u64| {
        limit_exceeded(
            name,
            "bytes",
            measured,
            limits.max_uncompressed,
            "limits.archive_max_uncompressed",
        )
    };
    let remaining = limits.max_uncompressed.saturating_sub(cost.bytes_hashed);
    if stamp.size() > remaining {
        return Err(over(cost.bytes_hashed.saturating_add(stamp.size())));
    }
    let Some(file) = open_regular(&path, stamp).map_err(|e| unreadable(name, rel, &e))? else {
        return Ok(Entry::Changed);
    };
    // Bounded even for a file that grows while it is read.
    let (size, sha256) = hash_reader(file.take(remaining.saturating_add(1)))
        .map_err(|e| unreadable(name, rel, &e))?;
    cost.files_hashed += 1;
    cost.bytes_hashed = cost.bytes_hashed.saturating_add(size);
    if size > remaining {
        return Err(over(cost.bytes_hashed));
    }
    Ok(Entry::File { size, sha256 })
}

/// The index's digest of the file at `rel`, when its stamp proves it racy-clean: the
/// size and mtime the index recorded, and an mtime and a ctime both strictly older than
/// the moment the entries were recorded less drift's slack. User space can put an mtime
/// back, never a ctime.
fn reuse(
    rel: &Path,
    stamp: &FileStamp,
    index: Option<(&FilesIndex, &FolderIndex)>,
) -> Option<Entry> {
    let (files, folder) = index?;
    let rel = rel.to_str()?;
    let at = folder
        .files
        .binary_search_by(|f| f.path.as_bytes().cmp(rel.as_bytes()))
        .ok()?;
    let entry = folder.files.get(at)?;
    let racy_from = files.recorded_at_ns(folder) - crate::drift::slack(folder);
    let clean = stamp.size() == entry.size
        && stamp.mtime_ns() == entry.mtime_ns
        && stamp.mtime_ns() < racy_from
        && stamp.ctime_ns() < racy_from;
    clean.then(|| Entry::File {
        size: entry.size,
        sha256: entry.sha256.clone(),
    })
}

/// The regular file the stamp describes, opened read-only without following a link or
/// blocking, and checked to be that inode; `None` when it is gone, a symlink, not a
/// regular file or another inode (it changed).
fn open_regular(path: &Path, stamp: &FileStamp) -> io::Result<Option<std::fs::File>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) if [Errno::NOENT, Errno::LOOP, Errno::NXIO, Errno::NOTDIR].contains(&e) => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let stat = rustix::fs::fstat(&fd)?;
    let regular = rustix::fs::FileType::from_raw_mode(stat.st_mode).is_file();
    let same = stat.st_dev == stamp.dev() && stat.st_ino == stamp.ino();
    Ok((regular && same).then(|| std::fs::File::from(fd)))
}

fn other_kind(kind: FileType) -> &'static str {
    if kind.is_fifo() {
        "o:fifo"
    } else if kind.is_socket() {
        "o:socket"
    } else if kind.is_char_device() {
        "o:char"
    } else if kind.is_block_device() {
        "o:block"
    } else {
        "o:unknown"
    }
}

/// `unindexable` for an entry the witness cannot read (the folder itself as `.`).
fn unreadable(name: &str, rel: &Path, e: &io::Error) -> Error {
    let rel = if rel.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        rel.to_string_lossy().into_owned()
    };
    unindexable(
        name,
        &rel,
        format!(
            "{name}/{rel} cannot be read ({e}), so the folder cannot be witnessed for this preview"
        ),
        Suggestion::text("make it readable, or move the folder out of AddOns, then preview again"),
    )
}

// ---------------------------------------------------------------------------------------
// A target's approval witness.

/// What one target's plan was approved against: the folders it moves, the names it
/// creates, the target's identity, its local inputs and its dependency choices. Not a
/// wire shape: only its digest leaves the engine (`precondition::of_plan`).
#[derive(Debug, Clone)]
pub struct ApprovalWitness {
    pub target: TargetRef,
    /// The client marker: `sha256:<hex>` of its bytes, `link:<target>` or `kind:<type>`;
    /// `None` when absent (an unregistered target).
    pub marker: Option<String>,
    /// Sorted by (key, role).
    pub folders: Vec<FolderWitness>,
    /// The folder keys the plan creates, and `Interface/AddOns` when it creates that
    /// (a folder key never holds a `/`); sorted bytewise.
    pub absent: Vec<String>,
    /// Sorted by (kind, source).
    pub inputs: Vec<InputWitness>,
    /// Sorted by (requester ASCII-lowercase, requester, folder, by).
    pub dependencies: Vec<DependencyChoice>,
    /// `Target.judge`.
    pub judge: String,
    /// Each witnessed folder's settled stamp, by its directory: the under-lock check's
    /// shortcut, never hashed.
    stamps: Vec<(PathBuf, TreeStamp)>,
    cost: WitnessCost,
}

impl ApprovalWitness {
    /// A witness of these parts, its lists sorted, with no stamps.
    pub fn new(
        target: TargetRef,
        marker: Option<String>,
        mut folders: Vec<FolderWitness>,
        mut absent: Vec<String>,
        mut inputs: Vec<InputWitness>,
        mut dependencies: Vec<DependencyChoice>,
        judge: String,
    ) -> Self {
        folders.sort_by(|a, b| (a.key.as_str(), a.role).cmp(&(b.key.as_str(), b.role)));
        absent.sort();
        absent.dedup();
        inputs.sort_by(|a, b| (a.kind, &a.source).cmp(&(b.kind, &b.source)));
        inputs.dedup();
        dependencies.sort_by_cached_key(DependencyChoice::order);
        Self {
            target,
            marker,
            folders,
            absent,
            inputs,
            dependencies,
            judge,
            stamps: Vec::new(),
            cost: WitnessCost::default(),
        }
    }

    /// The settled stamp of the witnessed folder at `dir`, when it had one.
    #[expect(
        dead_code,
        reason = "D8 (omad-m4-agent-surface-doctor-c6cm.4.8) reads it under the commit lock"
    )]
    pub(crate) fn stamp(&self, dir: &Path) -> Option<&TreeStamp> {
        self.stamps
            .iter()
            .find(|(at, _)| at == dir)
            .map(|(_, stamp)| stamp)
    }

    /// What collecting it cost, summed over its folders.
    pub fn cost(&self) -> WitnessCost {
        self.cost
    }
}

/// A local input of an install: a `--file` zip, a `--file` directory or a git checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputWitness {
    pub kind: InputKind,
    /// The path as given, or the repository URL.
    pub source: String,
    /// The zip's SHA-256, the directory's listing hash, or the commit.
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputKind {
    Zip,
    Dir,
    Git,
}

impl InputKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Dir => "dir",
            Self::Git => "git",
        }
    }
}

/// Which addon satisfies which requirement of a requester.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyChoice {
    pub requester: String,
    /// The required folder's key.
    pub folder: String,
    pub by: DependencyBy,
}

impl DependencyChoice {
    fn order(&self) -> (String, String, String, &'static str, Option<String>) {
        (
            self.requester.to_ascii_lowercase(),
            self.requester.clone(),
            self.folder.clone(),
            self.by.wire(),
            self.by.detail(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyBy {
    /// Installed with its requester: the dependency's addon.
    Installs(String),
    /// Present but unmanaged.
    Unmanaged,
    Unresolved(UnresolvedReason),
    /// Resolved, but it failed to plan: its ref.
    Failed(String),
}

impl DependencyBy {
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::Installs(_) => "install",
            Self::Unmanaged => "unmanaged",
            Self::Unresolved(_) => "unresolved",
            Self::Failed(_) => "failed",
        }
    }

    /// The addon, the reason's wire name or the ref; `None` for an unmanaged folder.
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Installs(addon) => Some(addon.clone()),
            Self::Unmanaged => None,
            Self::Unresolved(reason) => serde_json::to_value(reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned)),
            Self::Failed(r) => Some(r.clone()),
        }
    }
}

/// What a command asks the witness to cover beyond the plan itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WitnessScope {
    /// `copy`: the source client and the folders it reads there.
    pub copy_from: Option<CopyFrom>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFrom {
    pub target: Target,
    pub folders: Vec<FolderName>,
}

/// What [`collect`] reads with.
pub struct WitnessCtx<'a> {
    pub paths: &'a Paths,
    pub target: &'a Target,
    /// The target's files index, for managed folders' clean files.
    pub index: Option<&'a FilesIndex>,
    pub scope: &'a WitnessScope,
    pub limits: ArchiveLimits,
}

/// The witness of one target's plan, collected at the end of its planning: every folder
/// a replace or remove moves (managed ones with the index's clean files reused), every
/// adopted folder, `copy`'s source folders, the names it creates, the marker, the local
/// inputs, the dependency choices and the judge. Read-only.
pub fn collect(tp: &TargetPlan, ctx: &WitnessCtx<'_>) -> Result<ApprovalWitness> {
    let now_ns = Timestamp::now().as_nanosecond();
    let target = ctx.target;
    let addons = target.addons_dir();
    let mut gathered = Gathered::default();
    let mut absent = Vec::new();
    let mut inputs = Vec::new();
    for work in &tp.work {
        for op in work.swap() {
            let (live, role) = match op {
                SwapOp::Add { name } => {
                    absent.push(name.as_folder().key().to_owned());
                    continue;
                }
                SwapOp::Replace { live, .. } => (live, WitnessRole::Replaced),
                SwapOp::Remove { live, .. } => (live, WitnessRole::Removed),
            };
            let seen = Seen {
                dir: &addons,
                index: ctx.index,
                limits: &ctx.limits,
                now_ns,
            };
            gathered.witness(&seen, live.as_folder(), role)?;
        }
        inputs.extend(work.input().cloned());
    }
    // Adopted folders are hashed: the index entries an adoption records are its own.
    let unindexed = Seen {
        dir: &addons,
        index: None,
        limits: &ctx.limits,
        now_ns,
    };
    for change in tp.changes.iter().filter(|c| c.kind == ChangeKind::Adopt) {
        for folder in &change.folders.recorded {
            gathered.witness(&unindexed, folder, WitnessRole::Adopted)?;
        }
    }
    if let Some(copy) = &ctx.scope.copy_from {
        // The index only spares hashing clean files: without one, every file is hashed.
        let index = copy.target.key.and_then(|key| {
            store::load::<FilesIndex>(&ctx.paths.files_index(&key))
                .ok()
                .flatten()
                .map(|loaded| loaded.value)
        });
        let from = copy.target.addons_dir();
        let seen = Seen {
            dir: &from,
            index: index.as_ref(),
            limits: &ctx.limits,
            now_ns,
        };
        for folder in &copy.folders {
            gathered.witness(&seen, folder, WitnessRole::CopySource)?;
        }
    }
    if tp.creates_addons_dir {
        absent.push("Interface/AddOns".to_owned());
    }
    let mut witness = ApprovalWitness::new(
        TargetRef::of(target.key, &target.id),
        marker(&target.client_dir, &ctx.limits)?,
        gathered.folders,
        absent,
        inputs,
        dependency_choices(&tp.dependencies),
        target.judge.clone(),
    );
    witness.stamps = gathered.stamps;
    witness.cost = gathered.cost;
    Ok(witness)
}

/// The witness of a target with no folder plan: its marker and judge.
pub fn bare(target: &Target) -> Result<ApprovalWitness> {
    Ok(ApprovalWitness::new(
        TargetRef::of(target.key, &target.id),
        marker(&target.client_dir, &ArchiveLimits::default())?,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        target.judge.clone(),
    ))
}

/// The input an install reads from `location`: a zip outside the blob cache (its SHA-256
/// is `blob_sha256`), a directory (its listing hash) or a git checkout (its commit). A
/// download, a cached blob or a WowAce tree is no input: `releases[]` names it.
pub(crate) fn input_of(
    location: &AssetLocation,
    blob_sha256: Option<&str>,
    blobs_dir: &Path,
) -> Option<InputWitness> {
    match location {
        AssetLocation::LocalZip(path) if !path.starts_with(blobs_dir) => Some(InputWitness {
            kind: InputKind::Zip,
            source: path.display().to_string(),
            digest: blob_sha256?.to_owned(),
        }),
        AssetLocation::LocalDir(path) => Some(InputWitness {
            kind: InputKind::Dir,
            source: path.display().to_string(),
            digest: crate::sources::local::listing_hash(path),
        }),
        AssetLocation::GitCheckout { url, sha, .. } => Some(InputWitness {
            kind: InputKind::Git,
            source: url.clone(),
            digest: sha.clone(),
        }),
        AssetLocation::LocalZip(_)
        | AssetLocation::Url(_)
        | AssetLocation::Deferred { .. }
        | AssetLocation::WowAceTree { .. } => None,
    }
}

/// Every requester's dependency choices: installed with it, present unmanaged,
/// unresolved (the reason; candidates are hints for people) or failed (the ref). Bridge
/// dependencies and `first_install` are reports, not choices.
fn dependency_choices(dependencies: &BTreeMap<String, DepReport>) -> Vec<DependencyChoice> {
    let mut choices = Vec::new();
    for (requester, report) in dependencies {
        let mut choose = |folder: &FolderName, by: DependencyBy| {
            choices.push(DependencyChoice {
                requester: requester.clone(),
                folder: folder.key().to_owned(),
                by,
            });
        };
        for (folder, addon) in &report.installs {
            choose(folder, DependencyBy::Installs(addon.clone()));
        }
        for folder in &report.unmanaged {
            choose(folder, DependencyBy::Unmanaged);
        }
        for unresolved in &report.unresolved {
            choose(
                &unresolved.folder,
                DependencyBy::Unresolved(unresolved.reason),
            );
        }
        for (folder, r) in &report.failed {
            choose(folder, DependencyBy::Failed(r.to_string()));
        }
    }
    choices
}

/// Where and how [`Gathered::witness`] reads a folder.
struct Seen<'a> {
    /// The AddOns directory the folder is in.
    dir: &'a Path,
    index: Option<&'a FilesIndex>,
    limits: &'a ArchiveLimits,
    now_ns: i128,
}

/// The folders witnessed so far, their stamps and their cost.
#[derive(Default)]
struct Gathered {
    folders: Vec<FolderWitness>,
    stamps: Vec<(PathBuf, TreeStamp)>,
    cost: WitnessCost,
}

impl Gathered {
    /// Witnesses `name` under `seen.dir` as `role`, once per (key, role).
    fn witness(&mut self, seen: &Seen<'_>, name: &FolderName, role: WitnessRole) -> Result<()> {
        let key = name.key();
        if self.folders.iter().any(|f| f.role == role && f.key == key) {
            return Ok(());
        }
        let dir = seen.dir.join(name.as_str());
        let entry = seen
            .index
            .and_then(|index| index.folders.get(key).map(|f| (index, f)));
        let found = folder(&dir, entry, seen.limits, seen.now_ns, &mut || {})?;
        self.cost.files_hashed += found.cost.files_hashed;
        self.cost.files_reused += found.cost.files_reused;
        self.cost.bytes_hashed = self
            .cost
            .bytes_hashed
            .saturating_add(found.cost.bytes_hashed);
        if let Some(stamp) = found.stamp
            && !self.stamps.iter().any(|(at, _)| *at == dir)
        {
            self.stamps.push((dir, stamp));
        }
        self.folders.push(FolderWitness {
            role,
            key: key.to_owned(),
            digest: found.digest,
        });
        Ok(())
    }
}

/// The client marker as the witness records it (see [`ApprovalWitness::marker`]). An
/// unreadable marker fails the target.
fn marker(client_dir: &Path, limits: &ArchiveLimits) -> Result<Option<String>> {
    let path = crate::discover::registry::marker_path(client_dir);
    let fail = |e: &io::Error| {
        unindexable(
            "Interface/.omaddons",
            "target.json",
            format!(
                "Interface/.omaddons/target.json cannot be read ({e}), so the target cannot be witnessed for this preview"
            ),
            Suggestion::text("make it readable, then preview again"),
        )
    };
    let Some(stamp) = FileStamp::read(&path).map_err(|e| fail(&e))? else {
        return Ok(None);
    };
    let kind = stamp.file_type();
    if kind.is_symlink() {
        let to = std::fs::read_link(&path).map_err(|e| fail(&e))?;
        return Ok(Some(format!("link:{}", to.to_string_lossy())));
    }
    if kind.is_dir() {
        return Ok(Some("kind:dir".to_owned()));
    }
    if !kind.is_file() {
        let other = other_kind(kind);
        return Ok(Some(format!(
            "kind:{}",
            other.strip_prefix("o:").unwrap_or(other)
        )));
    }
    let Some(file) = open_regular(&path, &stamp).map_err(|e| fail(&e))? else {
        // Replaced while it was read: matches no steady marker.
        return Ok(Some("kind:changed".to_owned()));
    };
    let (_, sha256) = hash_reader(file.take(limits.max_uncompressed)).map_err(|e| fail(&e))?;
    Ok(Some(format!("sha256:{sha256}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;
    use crate::drift::{DriftMode, check_folder};
    use crate::engine::edit::RecordEdit;
    use crate::engine::plan::{Plan, RequestedChange, Unresolved};
    use crate::engine::testing::{self, TxScene};
    use crate::engine::tx::TxTarget;
    use crate::error::ErrorCode;
    use crate::model::{FolderName, TargetKey, TxKind};
    use crate::state::files_index::index_live_folder;

    /// Longer than the kernel's file-timestamp tick, so the next change gets a new ctime.
    fn tick() {
        std::thread::sleep(Duration::from_millis(30));
    }

    /// A folder `Foo` holding `files` (`/`-separated paths), in a fresh tempdir.
    fn folder_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Foo");
        fs::create_dir(&dir).unwrap();
        for (path, bytes) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        (tmp, dir)
    }

    /// The index of `dir` as a commit would record it, recorded at `at_ns`.
    fn indexed(dir: &Path, at_ns: i128) -> (FilesIndex, FolderIndex) {
        let folder = index_live_folder(dir, &ArchiveLimits::default())
            .unwrap()
            .stamped(at_ns);
        let files = FilesIndex {
            key: TargetKey::new(),
            written_at_ns: 0,
            folders: BTreeMap::from([("foo".to_owned(), folder.clone())]),
        };
        (files, folder)
    }

    /// The newest of every entry's mtime and ctime.
    fn newest(dir: &Path) -> i128 {
        TreeStamp::read(dir)
            .unwrap()
            .entries()
            .iter()
            .map(|(_, s)| s.mtime_ns().max(s.ctime_ns()))
            .max()
            .unwrap()
    }

    fn of_dir(dir: &Path) -> EntryDigest {
        EntryDigest::of_dir(dir, &ArchiveLimits::default()).unwrap()
    }

    fn run(dir: &Path, index: Option<(&FilesIndex, &FolderIndex)>) -> Folder {
        folder(dir, index, &ArchiveLimits::default(), i128::MAX, &mut || {}).unwrap()
    }

    /// Every entry's kind and path, as the digest encodes them.
    fn kinds(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let stamp = TreeStamp::read_at_most(dir, u64::MAX).unwrap().unwrap();
        let mut cost = WitnessCost::default();
        stamp
            .entries()
            .iter()
            .map(|(rel, s)| {
                let limits = ArchiveLimits::default();
                let entry = encode(dir, rel, s, None, "Foo", &limits, &mut cost).unwrap();
                (entry.kind().to_owned(), rel.as_os_str().as_bytes().to_vec())
            })
            .collect()
    }

    #[test]
    fn the_entry_digest_counts_every_kind() {
        let base: &[(&str, &[u8])] = &[
            ("Foo.toc", b"## Title: Foo\n"),
            ("libs/lib.lua", b"-- lib\n"),
        ];
        type Change = fn(&Path);
        let rows: &[(&str, Change, bool)] = &[
            (
                "added empty dir",
                |d| fs::create_dir(d.join("empty")).unwrap(),
                true,
            ),
            (
                "added empty file",
                |d| fs::write(d.join("empty.lua"), "").unwrap(),
                true,
            ),
            (
                "added symlink",
                |d| std::os::unix::fs::symlink("Foo.toc", d.join("link")).unwrap(),
                true,
            ),
            (
                "symlink retargeted",
                |d| {
                    fs::remove_file(d.join("link")).unwrap();
                    std::os::unix::fs::symlink("libs", d.join("link")).unwrap();
                },
                true,
            ),
            (
                "file renamed",
                |d| fs::rename(d.join("libs/lib.lua"), d.join("libs/Lib.lua")).unwrap(),
                true,
            ),
            (
                "file deleted",
                |d| fs::remove_file(d.join("Foo.toc")).unwrap(),
                true,
            ),
            (
                "same size, one byte changed",
                |d| fs::write(d.join("libs/lib.lua"), "-- LIB\n").unwrap(),
                true,
            ),
            (
                "file grew",
                |d| fs::write(d.join("libs/lib.lua"), "-- lib\n-- more\n").unwrap(),
                true,
            ),
            (
                "touch",
                |d| {
                    let file = fs::File::options()
                        .write(true)
                        .open(d.join("Foo.toc"))
                        .unwrap();
                    file.set_modified(std::time::SystemTime::now()).unwrap();
                },
                false,
            ),
            (
                "chmod 600",
                |d| {
                    fs::set_permissions(d.join("Foo.toc"), fs::Permissions::from_mode(0o600))
                        .unwrap()
                },
                false,
            ),
            (
                "recreated identical (new inode)",
                |d| {
                    let bytes = fs::read(d.join("Foo.toc")).unwrap();
                    fs::remove_file(d.join("Foo.toc")).unwrap();
                    fs::write(d.join("Foo.toc"), bytes).unwrap();
                },
                false,
            ),
        ];
        for (row, change, moves) in rows {
            // Each row on a fresh base, plus the symlink the retarget row needs.
            let (_tmp, dir) = folder_with(base);
            if *row == "symlink retargeted" {
                std::os::unix::fs::symlink("Foo.toc", dir.join("link")).unwrap();
            }
            let before = run(&dir, None);
            tick();
            change(&dir);
            let after = run(&dir, None);
            // ubs:ignore — comparing content digests, not secrets
            let moved = before.digest != after.digest;
            assert_eq!(
                moved, *moves,
                "{row}: digest {} vs {}, cost {:?}",
                before.digest, after.digest, after.cost
            );
        }
    }

    #[test]
    fn the_eleventh_changed_file_moves_the_digest() {
        let names: Vec<String> = (0..11).map(|i| format!("f{i:02}.lua")).collect();
        let files: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), &b"-- a\n"[..])).collect();
        let (_tmp, dir) = folder_with(&files);
        for name in &names[..10] {
            fs::write(dir.join(name), "-- b\n").unwrap();
        }
        let ten = of_dir(&dir);
        fs::write(dir.join(&names[10]), "-- b\n").unwrap();
        let eleven = of_dir(&dir);
        assert_ne!(ten, eleven, "digest {ten} vs {eleven}");
    }

    #[test]
    fn a_root_that_is_not_a_directory_is_one_entry() {
        let (tmp, dir) = folder_with(&[("Foo.toc", b"x")]);
        let real = of_dir(&dir);
        // A copy elsewhere, reached through a symlink at the folder's place.
        let copy = tmp.path().join("Copy");
        fs::create_dir(&copy).unwrap();
        fs::write(copy.join("Foo.toc"), "x").unwrap();
        let link = tmp.path().join("Linked");
        std::os::unix::fs::symlink(&copy, &link).unwrap();
        let linked = run(&link, None);
        assert_ne!(
            linked.digest, real,
            "a symlinked root is not its target's content"
        );
        assert_eq!(linked.cost.files_hashed, 0, "never walked");
        assert_eq!(kinds(&link), [("l".to_owned(), Vec::new())]);
        let file = tmp.path().join("File");
        fs::write(&file, "x").unwrap();
        assert_eq!(kinds(&file), [("f".to_owned(), Vec::new())]);
        let absent = run(&tmp.path().join("Absent"), None);
        assert_eq!(absent.digest, digest_of(&[(Vec::new(), Entry::Absent)]));
        assert_eq!(absent.stamp, None);
    }

    #[test]
    fn clean_indexed_files_are_reused_and_others_hashed() {
        let (_tmp, dir) = folder_with(&[
            ("Foo.toc", b"## Title: Foo\n"),
            ("a.lua", b"a"),
            ("b/c.lua", b"c"),
        ]);
        let (files, folder) = indexed(&dir, newest(&dir) + 1_000_000_000);
        let clean = run(&dir, Some((&files, &folder)));
        assert_eq!(
            (clean.cost.files_reused, clean.cost.files_hashed),
            (3, 0),
            "cost {:?}",
            clean.cost
        );
        assert_eq!(
            clean.digest,
            of_dir(&dir),
            "the reused digest is the hashed one"
        );
        tick();
        fs::write(dir.join("a.lua"), "A").unwrap();
        let edited = run(&dir, Some((&files, &folder)));
        assert_eq!(edited.cost.files_hashed, 1, "cost {:?}", edited.cost);
        assert_eq!(edited.digest, of_dir(&dir));
    }

    #[test]
    fn the_racy_clean_edge() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let at = newest(&dir);
        let slack = crate::limits::RACY_SLACK_NS;
        let hashed = |files: &FilesIndex, folder: &FolderIndex| {
            run(&dir, Some((files, folder))).cost.files_hashed
        };
        let (files, folder) = indexed(&dir, at + slack + 1);
        assert_eq!(
            hashed(&files, &folder),
            0,
            "recorded at ctime + slack + 1: reused"
        );
        let (files, folder) = indexed(&dir, at + slack);
        assert_eq!(
            hashed(&files, &folder),
            1,
            "recorded at ctime + slack: hashed"
        );
        let (files, mut folder) = indexed(&dir, at + slack + 1);
        folder.files[0].mtime_ns += 1;
        assert_eq!(hashed(&files, &folder), 1, "index mtime differs: hashed");
        let (files, mut folder) = indexed(&dir, at + slack + 1);
        folder.files[0].size += 1;
        assert_eq!(hashed(&files, &folder), 1, "index size differs: hashed");
        // Whole-second index mtimes take the coarse slack.
        let whole = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let file = fs::File::options()
            .write(true)
            .open(dir.join("Foo.lua"))
            .unwrap();
        file.set_modified(whole).unwrap();
        let at = newest(&dir);
        let (files, folder) = indexed(&dir, at + slack + 1);
        assert_eq!(folder.files[0].mtime_ns.rem_euclid(1_000_000_000), 0);
        assert_eq!(
            hashed(&files, &folder),
            1,
            "whole-second mtimes: the coarse slack applies"
        );
        let coarse = crate::limits::RACY_SLACK_COARSE_NS;
        let (files, folder) = indexed(&dir, at + coarse + 1);
        assert_eq!(hashed(&files, &folder), 0, "past the coarse slack: reused");
    }

    #[test]
    fn an_mtime_preserving_same_size_edit_is_hashed() {
        let (tmp, dir) = folder_with(&[("Foo.lua", b"-- one\n")]);
        let (files, folder) = indexed(&dir, newest(&dir) + crate::limits::RACY_SLACK_NS + 1);
        let before = run(&dir, Some((&files, &folder)));
        assert_eq!(before.cost.files_reused, 1);
        tick();
        let mtime = fs::metadata(dir.join("Foo.lua"))
            .unwrap()
            .modified()
            .unwrap();
        fs::write(dir.join("Foo.lua"), "-- two\n").unwrap();
        let file = fs::File::options()
            .write(true)
            .open(dir.join("Foo.lua"))
            .unwrap();
        file.set_modified(mtime).unwrap();
        let after = run(&dir, Some((&files, &folder)));
        assert_eq!(after.cost.files_hashed, 1, "cost {:?}", after.cost);
        assert_ne!(
            after.digest, before.digest,
            "digest {} vs {}",
            after.digest, before.digest
        );
        // The control: drift's stat mode, which reads no ctime, sees nothing.
        let name = FolderName::new("Foo").unwrap();
        let drift = check_folder(
            tmp.path(),
            &name,
            Some(&folder),
            files.recorded_at_ns(&folder),
            DriftMode::Stat,
        );
        assert_eq!(drift.drift.changed, 0, "{:?}", drift.drift);
    }

    #[test]
    fn a_trusted_index_digest_is_used_as_recorded() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let (files, mut folder) = indexed(&dir, newest(&dir) + 1_000_000_000);
        folder.files[0].sha256 = "0".repeat(64);
        let trusted = run(&dir, Some((&files, &folder)));
        assert_eq!(trusted.cost.files_reused, 1);
        assert_ne!(
            trusted.digest,
            of_dir(&dir),
            "the index is believed, as drift believes it"
        );
    }

    #[test]
    fn a_folder_that_moves_while_hashed_keeps_no_stamp() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        let before = of_dir(&dir);
        let mut edit = || fs::write(dir.join("new.lua"), "-- new\n").unwrap();
        let moved = folder(&dir, None, &ArchiveLimits::default(), i128::MAX, &mut edit).unwrap();
        assert_eq!(moved.stamp, None);
        assert_eq!(moved.digest, before, "the digest is the content read");
    }

    #[test]
    fn an_unsettled_folder_keeps_no_stamp() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n"), ("libs/lib.lua", b"-- lib\n")]);
        let ctime = TreeStamp::read(&dir)
            .unwrap()
            .entries()
            .iter()
            .map(|(_, s)| s.ctime_ns())
            .max()
            .unwrap();
        let at = |now| folder(&dir, None, &ArchiveLimits::default(), now, &mut || {}).unwrap();
        assert_eq!(
            at(ctime + crate::limits::RACY_SLACK_NS).stamp,
            None,
            "unsettled"
        );
        assert_eq!(
            at(ctime + crate::limits::RACY_SLACK_NS + 1).stamp,
            Some(TreeStamp::read(&dir).unwrap()),
            "settled"
        );
    }

    #[test]
    fn a_fifo_is_recorded_and_never_opened() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.join("pipe"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let walked = dir.clone();
        std::thread::spawn(move || {
            let _ = send.send(of_dir(&walked));
        });
        assert!(
            receive.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the FIFO was opened"
        );
        assert!(
            kinds(&dir).contains(&("o:fifo".to_owned(), b"pipe".to_vec())),
            "{:?}",
            kinds(&dir)
        );
    }

    #[test]
    fn a_symlink_is_never_followed() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        std::os::unix::fs::symlink("/", dir.join("escape")).unwrap();
        std::os::unix::fs::symlink(".", dir.join("loop")).unwrap();
        let walked = run(&dir, None);
        assert_eq!(
            walked.cost.files_hashed, 1,
            "only Foo.lua: cost {:?}",
            walked.cost
        );
        let found = kinds(&dir);
        assert!(
            found.contains(&("l".to_owned(), b"escape".to_vec())),
            "{found:?}"
        );
        assert!(
            found.contains(&("l".to_owned(), b"loop".to_vec())),
            "{found:?}"
        );
    }

    #[test]
    fn open_regular_refuses_another_inode_and_never_blocks() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n"), ("Bar.lua", b"-- bar\n")]);
        let stamp_of = |name: &str| FileStamp::read(&dir.join(name)).unwrap().unwrap();
        let foo = stamp_of("Foo.lua");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.join("pipe"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        std::os::unix::fs::symlink("Foo.lua", dir.join("link")).unwrap();
        for (case, path) in [
            ("a FIFO", dir.join("pipe")),
            ("a symlink", dir.join("link")),
            ("a gone path", dir.join("gone.lua")),
            ("another inode", dir.join("Bar.lua")),
        ] {
            let opened = open_regular(&path, &foo).unwrap();
            assert!(opened.is_none(), "{case}");
        }
        assert!(
            open_regular(&dir.join("Foo.lua"), &foo).unwrap().is_some(),
            "the file itself"
        );
    }

    #[test]
    fn a_non_utf8_name_is_hashed_by_its_bytes() {
        let digest = |name: &[u8]| {
            let (_tmp, dir) = folder_with(&[]);
            fs::write(dir.join(OsStr::from_bytes(name)), "x").unwrap();
            of_dir(&dir)
        };
        assert_ne!(digest(b"\xff"), digest(b"\xfe"));
    }

    fn assert_unindexable(error: &Error, path: &str) {
        assert_eq!(error.code, ErrorCode::Io, "{}", error.message);
        assert_eq!(
            error.details,
            serde_json::json!({ "reason": "unindexable", "folder": "Foo", "path": path }),
            "{}",
            error.message
        );
    }

    #[test]
    fn an_unreadable_file_fails_with_unindexable() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 file
        }
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"-- foo\n")]);
        fs::set_permissions(dir.join("Foo.lua"), fs::Permissions::from_mode(0o000)).unwrap();
        let error = EntryDigest::of_dir(&dir, &ArchiveLimits::default()).unwrap_err();
        fs::set_permissions(dir.join("Foo.lua"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_unindexable(&error, "Foo.lua");
    }

    #[test]
    fn an_unreadable_directory_fails_with_unindexable() {
        if rustix::process::geteuid().is_root() {
            return; // root lists a mode-000 directory
        }
        let (_tmp, dir) = folder_with(&[("libs/lib.lua", b"-- lib\n")]);
        fs::set_permissions(dir.join("libs"), fs::Permissions::from_mode(0o000)).unwrap();
        let error = EntryDigest::of_dir(&dir, &ArchiveLimits::default()).unwrap_err();
        fs::set_permissions(dir.join("libs"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_unindexable(&error, "libs");
    }

    #[test]
    fn past_the_entry_limit_fails_with_limit_exceeded() {
        let (_tmp, dir) = folder_with(&[("a", b"a"), ("b", b"b"), ("c/d", b"d")]);
        let limits = ArchiveLimits {
            max_entries: 3,
            ..ArchiveLimits::default()
        };
        let error = EntryDigest::of_dir(&dir, &limits).unwrap_err();
        assert_eq!(error.code, ErrorCode::ArchiveTooLarge, "{}", error.message);
        assert_eq!(
            error.details,
            serde_json::json!({
                "reason": "limit_exceeded",
                "folder": "Foo",
                "measured": 4,
                "limit": 3,
                "config_key": "limits.archive_max_entries",
            })
        );
    }

    #[test]
    fn past_the_byte_limit_fails_but_reused_bytes_do_not_count() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", &[b'x'; 20])]);
        let limits = ArchiveLimits {
            max_uncompressed: 10,
            ..ArchiveLimits::default()
        };
        let error = folder(&dir, None, &limits, i128::MAX, &mut || {}).unwrap_err();
        assert_eq!(error.code, ErrorCode::ArchiveTooLarge, "{}", error.message);
        assert_eq!(error.details["measured"], 20);
        assert_eq!(
            error.details["config_key"],
            "limits.archive_max_uncompressed"
        );
        let (files, folder_index) = indexed(&dir, newest(&dir) + 1_000_000_000);
        let reused = folder(
            &dir,
            Some((&files, &folder_index)),
            &limits,
            i128::MAX,
            &mut || {},
        );
        assert!(
            reused.is_ok(),
            "reused bytes count against no bound: {reused:?}"
        );
    }

    #[test]
    fn entry_digest_displays_as_entry_sha256() {
        let (_tmp, dir) = folder_with(&[("Foo.lua", b"x")]);
        let shown = of_dir(&dir).to_string();
        let hex = shown.strip_prefix("entry:sha256:").unwrap_or_default();
        assert_eq!(hex.len(), 64, "{shown}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{shown}"
        );
        assert_eq!(WitnessRole::CopySource.wire(), "copy_source");
    }

    // ---- A target's witness ------------------------------------------------------

    const TOC: &str = "## Interface: 120105\n## Version: 1\n";

    /// A witnessed dry run's plan.
    fn witnessed(scene: &TxScene, changes: Vec<RequestedChange>, scope: WitnessScope) -> Plan {
        scene.run_witnessed(changes, scope).unwrap().plan
    }

    /// The witness's public parts and cost, for assertion messages.
    fn shown(w: &ApprovalWitness) -> String {
        format!(
            "target {}\nmarker {:?}\nfolders {:?}\nabsent {:?}\ninputs {:?}\ndependencies {:?}\ncost {:?}",
            w.target,
            w.marker,
            w.folders,
            w.absent,
            w.inputs,
            w.dependencies,
            w.cost()
        )
    }

    fn roles(w: &ApprovalWitness) -> Vec<(String, WitnessRole)> {
        w.folders.iter().map(|f| (f.key.clone(), f.role)).collect()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `scene` with Foo installed from a zip (the target registered by the commit).
    fn with_foo() -> TxScene {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let done = scene.run(vec![scene.install("gh:o/foo", &zip)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        scene
    }

    #[test]
    fn an_update_witnesses_its_replaced_folder_and_reuses_the_index() {
        let scene = TxScene::new();
        testing::folder(&scene.client.addons().join("Foo"), TOC);
        // Older than the index by more than the racy slack, so its files are clean.
        std::thread::sleep(Duration::from_millis(40));
        let adopted = scene
            .run_as(
                vec![scene.adopt("Foo")],
                Some(TxKind::Adopt),
                TxKind::Install,
            )
            .unwrap();
        assert!(adopted.failures.is_empty(), "{:?}", adopted.failures);
        let v2 = scene.zip(
            "v2.zip",
            &[("Foo/Foo.toc", TOC), ("FooExtra/FooExtra.toc", TOC)],
        );
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &v2)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            roles(w),
            [("foo".to_owned(), WitnessRole::Replaced)],
            "{}",
            shown(w)
        );
        let cost = w.cost();
        assert!(
            cost.files_reused > 0 && cost.files_hashed == 0,
            "clean files come from the index\n{}",
            shown(w)
        );
        assert_eq!(
            w.absent,
            ["fooextra"],
            "an update lists only the folders it adds\n{}",
            shown(w)
        );
    }

    #[test]
    fn a_forced_replace_of_an_unmanaged_folder_is_hashed() {
        let mut scene = TxScene::new();
        testing::hand_made(&scene.client.addons().join("Foo"));
        scene.force = true;
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &zip)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            roles(w),
            [("foo".to_owned(), WitnessRole::Replaced)],
            "{}",
            shown(w)
        );
        let cost = w.cost();
        assert!(
            cost.files_hashed > 0 && cost.files_reused == 0,
            "{}",
            shown(w)
        );
        assert_eq!(
            w.folders[0].digest,
            EntryDigest::of_dir(
                &scene.client.addons().join("Foo"),
                &ArchiveLimits::default()
            )
            .unwrap(),
            "{}",
            shown(w)
        );
    }

    #[test]
    fn a_remove_witnesses_the_removed_folder() {
        let scene = with_foo();
        let plan = witnessed(&scene, vec![scene.remove("Foo")], WitnessScope::default());
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            roles(w),
            [("foo".to_owned(), WitnessRole::Removed)],
            "{}",
            shown(w)
        );
        assert!(w.absent.is_empty(), "{}", shown(w));
    }

    #[test]
    fn an_adopt_witnesses_its_recorded_folders() {
        let scene = with_foo();
        testing::folder(&scene.client.addons().join("Bar"), TOC);
        let plan = witnessed(&scene, vec![scene.adopt("Bar")], WitnessScope::default());
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            roles(w),
            [("bar".to_owned(), WitnessRole::Adopted)],
            "{}",
            shown(w)
        );
        assert!(w.cost().files_hashed > 0, "{}", shown(w));
    }

    #[test]
    fn a_pin_witnesses_no_folder() {
        let scene = with_foo();
        let plan = witnessed(
            &scene,
            vec![scene.edit("Foo", RecordEdit::SetPinned(true))],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        assert_eq!(plan.targets[0].changes.len(), 1, "{:?}", plan.targets[0]);
        let w = &plan.witnesses[0];
        assert!(w.folders.is_empty() && w.absent.is_empty(), "{}", shown(w));
    }

    #[test]
    fn a_copy_witnesses_its_source_folders() {
        let scene = TxScene::new();
        let other = scene.base.join("_classic_");
        let from = other.join("Interface/AddOns");
        testing::folder(&from.join("Foo"), TOC);
        let mut source = testing::target(&other);
        source.id = "classic".into();
        let scope = WitnessScope {
            copy_from: Some(CopyFrom {
                target: source,
                folders: ["Foo", "Gone"]
                    .map(|n| FolderName::new(n).unwrap())
                    .to_vec(),
            }),
        };
        let plan = witnessed(&scene, Vec::new(), scope);
        let w = &plan.witnesses[0];
        assert_eq!(
            roles(w),
            [
                ("foo".to_owned(), WitnessRole::CopySource),
                ("gone".to_owned(), WitnessRole::CopySource),
            ],
            "{}",
            shown(w)
        );
        let limits = ArchiveLimits::default();
        assert_eq!(
            w.folders[0].digest,
            EntryDigest::of_dir(&from.join("Foo"), &limits).unwrap()
        );
        assert_eq!(
            w.folders[1].digest,
            EntryDigest::of_dir(&from.join("Gone"), &limits).unwrap(),
            "a missing source folder digests as absent"
        );
    }

    #[test]
    fn a_first_install_lists_its_absent_names() {
        let scene = TxScene::new();
        fs::remove_dir(scene.client.addons()).unwrap();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &zip)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(w.absent, ["Interface/AddOns", "foo"], "{}", shown(w));
        assert!(w.folders.is_empty(), "{}", shown(w));
    }

    #[test]
    fn the_marker_and_the_judge_identify_the_target() {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &zip)],
            WitnessScope::default(),
        );
        let w = &plan.witnesses[0];
        assert_eq!(w.marker, None, "unregistered\n{}", shown(w));
        assert_eq!(w.target, TargetRef::Provisional("retail".into()));

        scene.run(vec![scene.install("gh:o/foo", &zip)]).unwrap();
        let plan = witnessed(&scene, vec![scene.remove("Foo")], WitnessScope::default());
        let w = &plan.witnesses[0];
        let client_dir = scene.client.client_dir();
        let marker = fs::read(crate::discover::registry::marker_path(client_dir)).unwrap();
        assert_eq!(
            w.marker,
            Some(format!("sha256:{}", sha256_hex(&marker))),
            "registered\n{}",
            shown(w)
        );
        let target = scene.current_target().target;
        assert_eq!(w.target, TargetRef::of(target.key, "retail"));
        assert!(matches!(w.target, TargetRef::Key(_)), "{}", shown(w));
        assert!(!w.judge.is_empty());
        assert_eq!(w.judge, target.judge);
    }

    #[test]
    fn local_inputs_are_witnessed() {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &zip)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            w.inputs,
            [InputWitness {
                kind: InputKind::Zip,
                source: zip.display().to_string(),
                digest: sha256_hex(&fs::read(&zip).unwrap()),
            }],
            "{}",
            shown(w)
        );

        let dir = scene.base.join("src/Bar");
        testing::folder(&dir, TOC);
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/bar", &dir)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        assert_eq!(
            w.inputs,
            [InputWitness {
                kind: InputKind::Dir,
                source: dir.display().to_string(),
                digest: crate::sources::local::listing_hash(&dir),
            }],
            "{}",
            shown(w)
        );
    }

    #[test]
    fn locations_map_to_their_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs = tmp.path().join("blobs");
        let dir = tmp.path().join("Foo");
        testing::folder(&dir, TOC);
        let zip = |kind, source: &str, digest: &str| {
            Some(InputWitness {
                kind,
                source: source.to_owned(),
                digest: digest.to_owned(),
            })
        };
        let cases: [(&str, AssetLocation, Option<&str>, Option<InputWitness>); 8] = [
            (
                "a user's zip",
                AssetLocation::LocalZip("/x/Foo.zip".into()),
                Some("ab12"),
                zip(InputKind::Zip, "/x/Foo.zip", "ab12"),
            ),
            (
                "a cached blob",
                AssetLocation::LocalZip(blobs.join("ab12")),
                Some("ab12"),
                None,
            ),
            (
                "a directory",
                AssetLocation::LocalDir(dir.clone()),
                None,
                zip(
                    InputKind::Dir,
                    &dir.display().to_string(),
                    &crate::sources::local::listing_hash(&dir),
                ),
            ),
            (
                "a git checkout",
                AssetLocation::GitCheckout {
                    url: "https://git.example/o/foo.git".into(),
                    rev: "main".into(),
                    sha: "0123abcd".into(),
                },
                None,
                zip(InputKind::Git, "https://git.example/o/foo.git", "0123abcd"),
            ),
            (
                "a download",
                AssetLocation::Url("https://example.com/Foo.zip".into()),
                Some("ab12"),
                None,
            ),
            (
                "a deferred download",
                AssetLocation::Deferred {
                    source: crate::sources::SourceId::Tukui,
                    key: "elvui".into(),
                },
                Some("ab12"),
                None,
            ),
            (
                "a WowAce tree",
                AssetLocation::WowAceTree {
                    base: "https://repos.example/!svn/bc/7/foo".into(),
                    revision: 7,
                },
                None,
                None,
            ),
            (
                "a zip whose hash is unknown",
                AssetLocation::LocalZip("/x/Foo.zip".into()),
                None,
                None,
            ),
        ];
        for (case, location, sha, expected) in cases {
            assert_eq!(input_of(&location, sha, &blobs), expected, "{case}");
        }
        assert_eq!(
            [InputKind::Zip, InputKind::Dir, InputKind::Git].map(InputKind::wire),
            ["zip", "dir", "git"]
        );
    }

    #[test]
    fn dependency_choices_are_witnessed() {
        let scene = TxScene::new();
        testing::folder(&scene.client.addons().join("LibFoo"), TOC);
        let zip = scene.zip(
            "foo.zip",
            &[(
                "Foo/Foo.toc",
                "## Interface: 120105\n## Dependencies: LibFoo, NoSuchLib\n",
            )],
        );
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &zip)],
            WitnessScope::default(),
        );
        assert!(plan.failures.is_empty(), "{:?}", plan.failures);
        let w = &plan.witnesses[0];
        let by: Vec<(&str, &str, &str)> = w
            .dependencies
            .iter()
            .map(|d| (d.requester.as_str(), d.folder.as_str(), d.by.wire()))
            .collect();
        assert_eq!(
            by,
            [
                ("Foo", "libfoo", "unmanaged"),
                ("Foo", "nosuchlib", "unresolved")
            ],
            "{}",
            shown(w)
        );
    }

    #[test]
    fn dependency_choices_cover_every_kind() {
        let folder = |n: &str| FolderName::new(n).unwrap();
        let report = DepReport {
            installs: vec![(folder("LibA"), "LibA".into())],
            unmanaged: vec![folder("LibB")],
            unresolved: vec![Unresolved {
                folder: folder("LibC"),
                reason: UnresolvedReason::NoSource,
                candidates: Vec::new(),
            }],
            failed: vec![(folder("LibD"), "gh:o/libd".parse().unwrap())],
            ..DepReport::default()
        };
        let deps = BTreeMap::from([
            ("zed".to_owned(), report.clone()),
            ("Alpha".to_owned(), report),
        ]);
        let w = ApprovalWitness::new(
            TargetRef::Provisional("retail".into()),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            dependency_choices(&deps),
            String::new(),
        );
        let shown: Vec<(String, String, &str, Option<String>)> = w
            .dependencies
            .iter()
            .map(|d| {
                (
                    d.requester.clone(),
                    d.folder.clone(),
                    d.by.wire(),
                    d.by.detail(),
                )
            })
            .collect();
        let expected = |requester: &str| {
            [
                ("liba", "install", Some("LibA")),
                ("libb", "unmanaged", None),
                ("libc", "unresolved", Some("no_source")),
                ("libd", "failed", Some("gh:o/libd")),
            ]
            .map(|(f, by, detail)| {
                (
                    requester.to_owned(),
                    f.to_owned(),
                    by,
                    detail.map(str::to_owned),
                )
            })
        };
        let all: Vec<_> = expected("Alpha")
            .into_iter()
            .chain(expected("zed"))
            .collect();
        assert_eq!(shown, all, "sorted by requester (case-folded), then folder");
    }

    #[test]
    fn a_target_that_cannot_be_witnessed_fails_alone() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 file
        }
        let scene = with_foo();
        // A file nobody can read, unknown to the index, in the folder the update replaces.
        let secret = scene.client.addons().join("Foo/secret.lua");
        fs::write(&secret, "x").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        let other = scene.base.join("_other_");
        fs::create_dir_all(other.join("Interface/AddOns")).unwrap();
        let mut second = scene.discovered.clone();
        second.target = testing::target(&other);
        second.target.id = "other".into();
        second.default_id = "other".into();
        let v2 = scene.zip(
            "v2.zip",
            &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n")],
        );
        let bar = scene.zip("bar.zip", &[("Bar/Bar.toc", TOC)]);
        let mut req = scene.request(vec![scene.install("gh:o/foo", &v2)], None, TxKind::Install);
        req.dry_run = true;
        req.witness = Some(WitnessScope::default());
        req.targets.push(TxTarget {
            discovered: second,
            changes: vec![scene.install("gh:o/bar", &bar)],
            default_kind: TxKind::Install,
            dependency_locks: BTreeMap::new(),
        });
        let done = scene.with_ctx("install", |ctx| crate::engine::tx::run(&req, ctx));
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        let plan = done.unwrap().plan;

        let retail = &plan.targets[0];
        assert!(
            retail.work.is_empty() && retail.changes.is_empty() && retail.kind.is_none(),
            "{retail:?}"
        );
        let failed: Vec<_> = plan
            .failures
            .iter()
            .filter(|f| matches!(f.target.as_str(), "retail"))
            .collect();
        assert_eq!(failed.len(), 1, "{:?}", plan.failures);
        assert_eq!(failed[0].addon.as_deref(), Some("Foo"));
        let cause = failed[0].cause.as_ref().unwrap();
        assert_eq!(cause.code, ErrorCode::Io, "{cause:?}");
        assert_eq!(cause.details["reason"], "unindexable", "{cause:?}");
        assert_eq!(plan.witnesses.len(), 2);
        let bare = &plan.witnesses[0];
        assert!(
            bare.folders.is_empty() && bare.absent.is_empty() && bare.marker.is_some(),
            "{}",
            shown(bare)
        );
        let other = &plan.witnesses[1];
        assert_eq!(other.target, TargetRef::Provisional("other".into()));
        assert_eq!(other.absent, ["bar"], "{}", shown(other));
        assert!(
            plan.failures
                .iter()
                .all(|f| !matches!(f.target.as_str(), "other")),
            "{:?}",
            plan.failures
        );
    }

    #[test]
    fn a_dry_run_without_a_scope_collects_nothing() {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", TOC)]);
        let mut req = scene.request(vec![scene.install("gh:o/foo", &zip)], None, TxKind::Install);
        req.dry_run = true;
        let plan = scene
            .with_ctx("install", |ctx| crate::engine::tx::run(&req, ctx))
            .unwrap()
            .plan;
        assert_eq!(plan.targets.len(), 1);
        assert!(plan.witnesses.is_empty());
    }

    #[test]
    fn collection_writes_nothing() {
        let scene = with_foo();
        let state = scene.paths.state_dir().to_path_buf();
        let client = scene.client.client_dir().to_path_buf();
        let before = (
            testing::tree_snapshot(&state),
            testing::tree_snapshot(&client),
        );
        let v2 = scene.zip(
            "v2.zip",
            &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n")],
        );
        let plan = witnessed(
            &scene,
            vec![scene.install("gh:o/foo", &v2)],
            WitnessScope::default(),
        );
        assert_eq!(plan.witnesses.len(), 1);
        let after = (
            testing::tree_snapshot(&state),
            testing::tree_snapshot(&client),
        );
        assert!(
            before == after,
            "a witnessed dry run changed <state> or the client dir"
        );
    }
}
