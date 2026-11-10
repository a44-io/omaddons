//! Lowering (E5, M4-D31): the fixes a doctor run returns, turned into one immutable,
//! ordered list of primitive steps before anything is written. It reads the filesystem
//! and writes nothing, so `doctor fix`'s preview is exactly what the commit will run, and
//! the same fixes over an unchanged home lower to equal values (M4-D44).
//!
//! - **Each step's predicates are decided here.** Recovery classifies a step only by the
//!   `pre` and `post` recorded before it ran, so each is captured from the live paths
//!   through a view of the steps lowered before it: a flatten's second rename is
//!   predicted against the state its first leaves, never against the disk as it is now.
//!   A crash between two renames then matches one of them.
//! - **Paths are canonical**, resolved as `mutate()` resolves its targets (the parent
//!   through the filesystem, the final component not followed), so a step compares with
//!   the scope table and with the actions log.
//! - **A quarantine stays on its filesystem** (M4-D35): a rename whose two ends are on
//!   different mounts, of a mount point, or for which the kernel gives no mount id, is
//!   refused, as is a created file whose rollback would be such a rename.
//! - **What a repair may never do** is refused before anything is written: a path below
//!   a client's `WTF/`, in a client the repair was not planned for, or outside the
//!   doctor's write scopes; a chmod of anything but a file; any symlink but a temporary
//!   one omaddons left; a flatten onto an existing name.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde_json::json;
use sha2::Digest as _;

use super::RepairRun;
use super::undo::{
    BackupRead, Dest, InverseKind, InverseStep, QuarantineSide, RepairUndoPlan, VerifiedPlan,
};
use crate::engine::repair_action::{
    Endpoint, Endpoints, Fix, FixEntry, Guard, Identity, JournalSet, Mode, Primitive,
    QuarantineRoot, Sha256, Stamp, Step, check_path, validate_steps,
};
use crate::engine::resource::{PathKind, Resource, ResourceSet};
use crate::error::{Error, ErrorCode, Suggestion};
use crate::fsx::sys::TEMP_PREFIX;
use crate::fsx::{
    Access, Expect, FileId, FileKind, OwnedFolders, Parts, Refusal, ScopeTable, Side, StampDigest,
    Writer, canonical_lenient,
};
use crate::limits::SKILL_DIRS_MAX;
use crate::model::{TargetKey, TxId};
use crate::state::paths::{ClientPaths, FLATTEN_PREFIX, Paths};

/// What lowering reads besides the fixes.
pub struct LowerCtx<'a> {
    pub paths: &'a Paths,
    pub tx: TxId,
    pub run: &'a RepairRun,
    /// The registered clients a step may touch, each with its key (markers checked by
    /// the caller).
    pub clients: &'a [(PathBuf, TargetKey)],
    /// The skill directories a write may touch: `Paths::skill_dir()` and
    /// `history_tail.skill_dirs[]`.
    pub skill_dirs: &'a [PathBuf],
    /// The scope table the commit will use (`with_skill_dirs` applied).
    pub scopes: &'a ScopeTable,
    /// `fsx::mount_id` in production; a seam in unit tests.
    pub mount_id: &'a dyn Fn(&Path) -> io::Result<u64>,
}

/// One fix a fixer returned, with the failure mode it repairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFix {
    pub fm: Cow<'static, str>,
    pub fix: Fix,
}

/// One primitive and what the commit and the preview need beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredStep {
    pub step: Step,
    /// What it writes: a rename's two ends, a write's path and its backup, a chmod's path.
    pub resources: ResourceSet,
    /// The size of the file an overwriting write replaces, which its backup keeps; 0
    /// otherwise.
    pub backup_bytes: u64,
    /// The quarantine root a rename moves into (the base its destination is relative
    /// to), so a preview can show the destination below it; `None` for a write, a chmod
    /// and a flatten's renames within AddOns.
    pub quarantine: Option<PathBuf>,
    /// Where an in-process rollback moves a file this write creates (it is quarantined,
    /// never deleted): `<root>/rolled_back/<seq>`, the root of the file's own side.
    pub rollback_to: Option<PathBuf>,
    /// What a write writes, which the commit needs and the journal never records (its
    /// digest is the step's `post`); `None` for every other primitive.
    pub content: Option<Vec<u8>>,
}

/// A whole repair, lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lowered {
    /// One entry per planned fix, in order, a delegation and a chmod to the mode a file
    /// already has included.
    pub fixes: Vec<FixEntry>,
    pub steps: Vec<LoweredStep>,
    /// The union of the steps' resources.
    pub resources: ResourceSet,
    /// The clients whose directories hold a step's path or destination, by key.
    pub targets: Vec<TargetKey>,
    /// Those clients' directories, canonical, by key.
    pub client_dirs: Vec<(PathBuf, TargetKey)>,
    /// Every directory the commit must create before `prepared` (sorted, deduplicated):
    /// the missing parents of each destination, doctor-owned, never primitives.
    pub roots: Vec<PathBuf>,
}

/// Why lowering refused a repair the doctor reports as `doctor_refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedReason {
    /// A rename would cross a mount (M4-D40's `filesystem_mismatch`).
    FilesystemMismatch,
}

impl RefusedReason {
    /// Its `details.reason`.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::FilesystemMismatch => "filesystem_mismatch",
        }
    }
}

/// Why a repair was not lowered. Nothing was written either way.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LowerError {
    /// A refusal `doctor fix` reports as `doctor_refused`.
    #[error("{message}")]
    Refused {
        reason: RefusedReason,
        path: PathBuf,
        to: Option<PathBuf>,
        message: String,
    },
    #[error(transparent)]
    Error(#[from] Error),
}

/// Lowers `fixes`, in order, into the primitives the commit will run.
pub fn lower(ctx: &LowerCtx<'_>, fixes: &[PlannedFix]) -> Result<Lowered, LowerError> {
    let mut lowering = Lowering {
        ctx,
        bases: Bases::new(ctx)?,
        view: View::default(),
        steps: Vec::new(),
        roots: BTreeSet::new(),
        touched: BTreeSet::new(),
    };
    let mut entries = Vec::with_capacity(fixes.len());
    for (planned, fix) in fixes.iter().zip(1u32..) {
        entries.push(FixEntry {
            fix,
            fm: planned.fm.clone(),
            kind: planned.fix.kind(),
        });
        lowering.fix(fix, planned)?;
    }
    lowering.finish(entries)
}

// ---------------------------------------------------------------------------------------
// Refusals. Each says what is wrong and that nothing was written, and names no command.

fn scope_denied(message: String) -> LowerError {
    Error::new(
        ErrorCode::ScopeDenied,
        message,
        Suggestion::text("nothing was written; move the path aside yourself if it must change"),
    )
    .into()
}

fn protected(message: String) -> LowerError {
    Error::new(
        ErrorCode::ProtectedFolder,
        message,
        Suggestion::text(
            "Blizzard_* folders belong to the client and are never changed; nothing was written",
        ),
    )
    .into()
}

fn exists(path: &Path) -> LowerError {
    Error::new(
        ErrorCode::FileExists,
        format!("{} already exists; it is never replaced", path.display()),
        Suggestion::text("move or delete that entry yourself; nothing was written"),
    )
    .details(json!({ "file": path.display().to_string() }))
    .into()
}

fn internal(message: String) -> LowerError {
    Error::new(
        ErrorCode::Internal,
        message,
        Suggestion::text(
            "nothing was written; report this as a bug: the doctor planned a repair it cannot make",
        ),
    )
    .into()
}

fn io_error(path: &Path, err: &io::Error) -> LowerError {
    Error::io(
        path,
        err,
        Suggestion::text(format!(
            "nothing was written; check {} and its permissions",
            path.display()
        )),
    )
    .into()
}

fn missing(path: &Path) -> LowerError {
    io_error(
        path,
        &io::Error::new(io::ErrorKind::NotFound, "nothing is there"),
    )
}

fn refusal(refusal: Refusal) -> LowerError {
    match refusal {
        Refusal::ScopeDenied(message) => scope_denied(message),
        Refusal::Protected(message) => protected(message),
    }
}

fn mount_refusal(from: &Path, to: &Path, message: String) -> LowerError {
    LowerError::Refused {
        reason: RefusedReason::FilesystemMismatch,
        path: from.to_path_buf(),
        to: Some(to.to_path_buf()),
        message,
    }
}

// ---------------------------------------------------------------------------------------
// Where paths live.

/// The canonical directories lowering places paths by, each resolved whole as the scope
/// table resolves its roots.
struct Bases {
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
    cache: PathBuf,
    journal: PathBuf,
    run: PathBuf,
    run_id: String,
    clients: Vec<(PathBuf, TargetKey)>,
    /// The scope table's other clients: registered, but not this repair's.
    unplanned: Vec<PathBuf>,
    skills: Vec<PathBuf>,
}

/// Which side of the machine a path is on, for its quarantine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Below `clients[i]`.
    Client(usize),
    Skill,
    State,
    Config,
    Cache,
    Other,
}

impl Bases {
    fn new(ctx: &LowerCtx<'_>) -> Result<Self, LowerError> {
        let whole = |dir: &Path| {
            canonical_lenient(dir)
                .ok_or_else(|| internal(format!("{} cannot be resolved", dir.display())))
        };
        let clients = ctx
            .clients
            .iter()
            .map(|(dir, key)| Ok((whole(dir)?, *key)))
            .collect::<Result<Vec<_>, LowerError>>()?;
        let unplanned = ctx
            .scopes
            .client_dirs()
            .iter()
            .filter(|dir| !clients.iter().any(|(planned, _)| planned == *dir))
            .cloned()
            .collect();
        let mut skills: Vec<PathBuf> = Vec::new();
        for dir in ctx.skill_dirs {
            let dir = whole(dir)?;
            if !skills.contains(&dir) {
                skills.push(dir);
            }
        }
        if skills.len() > SKILL_DIRS_MAX {
            return Err(scope_denied(format!(
                "a repair names {} skill directories; at most {SKILL_DIRS_MAX} are admitted",
                skills.len()
            )));
        }
        Ok(Self {
            home: whole(ctx.paths.home())?,
            state: whole(ctx.paths.state_dir())?,
            config: whole(ctx.paths.config_dir())?,
            cache: whole(ctx.paths.cache_dir())?,
            journal: whole(&ctx.paths.journal_dir())?,
            run: whole(&ctx.run.dir)?,
            run_id: ctx.run.id.clone(),
            clients,
            unplanned,
            skills,
        })
    }

    fn place(&self, path: &Path) -> Place {
        let below = |root: &Path| path != root && path.starts_with(root);
        if let Some(i) = self.clients.iter().position(|(dir, _)| below(dir)) {
            Place::Client(i)
        } else if self.skills.iter().any(|dir| below(dir)) {
            Place::Skill
        } else if below(&self.state) {
            Place::State
        } else if below(&self.config) {
            Place::Config
        } else if below(&self.cache) {
            Place::Cache
        } else {
            Place::Other
        }
    }

    /// A side's quarantine root and the directory its victims' paths are relative to
    /// (M4-D35); `None` for a path on no side that has one.
    fn quarantine(&self, place: Place, tx: &TxId) -> Option<(PathBuf, &Path)> {
        let run = self.run.join("quarantine");
        Some(match place {
            Place::Client(i) => {
                let dir = &self.clients[i].0;
                (
                    ClientPaths::new(dir)
                        .snapshots_repair(tx)
                        .join("quarantine"),
                    dir.as_path(),
                )
            }
            Place::Skill => (run.join("skill"), self.home.as_path()),
            Place::State => (run.join("state"), self.state.as_path()),
            Place::Config => (run.join("config"), self.config.as_path()),
            Place::Cache => (
                self.cache.join("quarantine").join(&self.run_id),
                self.cache.as_path(),
            ),
            Place::Other => return None,
        })
    }

    /// Where `root` receives `victim`, and that root: refused unless the root is the
    /// victim's own side.
    fn destination(
        &self,
        victim: &Path,
        root: &QuarantineRoot,
        tx: &TxId,
    ) -> Result<(PathBuf, PathBuf), LowerError> {
        let place = self.place(victim);
        let own = match (root, place) {
            (QuarantineRoot::State, Place::State)
            | (QuarantineRoot::Config, Place::Config)
            | (QuarantineRoot::Cache, Place::Cache)
            | (QuarantineRoot::Skill, Place::Skill) => true,
            (QuarantineRoot::Client { client_dir }, Place::Client(i)) => {
                canonical_lenient(client_dir).as_ref() == Some(&self.clients[i].0)
            }
            _ => false,
        };
        let Some((base, relative_to)) = self.quarantine(place, tx).filter(|_| own) else {
            return Err(scope_denied(format!(
                "the quarantine root of {} must be on its own side",
                victim.display()
            )));
        };
        let rel = victim.strip_prefix(relative_to).map_err(|_| {
            scope_denied(format!(
                "{} is outside the home directory, so it has no quarantine",
                victim.display()
            ))
        })?;
        Ok((base.join(rel), base))
    }

    /// The client dir whose `WTF/` holds `path`, planned or not.
    fn wtf_of(&self, path: &Path) -> Option<&Path> {
        let planned = self.clients.iter().map(|(dir, _)| dir);
        planned.chain(&self.unplanned).find_map(|dir| {
            let first = path.strip_prefix(dir).ok()?.components().next()?;
            matches!(first, Component::Normal(name) if name.eq_ignore_ascii_case("WTF"))
                .then_some(dir.as_path())
        })
    }
}

/// `path` resolved as `mutate()` resolves a target: its parent through the filesystem, a
/// missing tail appended, its final component not followed.
fn resolve(path: &Path) -> Option<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_lenient(parent).map(|p| p.join(name)),
        _ => canonical_lenient(path),
    }
}

/// `path` with every `..` taken back lexically, for the `WTF/` check of a path that
/// [`check_path`] would refuse naming it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn parent_of(path: &Path) -> Result<&Path, LowerError> {
    path.parent()
        .ok_or_else(|| internal(format!("{} has no parent", path.display())))
}

fn resource(path: &Path, kind: PathKind) -> Result<Resource, LowerError> {
    Resource::canonical(path, kind).map_err(|err| internal(err.to_string()))
}

fn kind_of(meta: &fs::Metadata) -> &'static str {
    let kind = meta.file_type();
    if kind.is_dir() {
        "folder"
    } else if kind.is_symlink() {
        "symlink"
    } else if kind.is_file() {
        "regular file"
    } else {
        "special file"
    }
}

fn temporary(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with(TEMP_PREFIX))
}

// ---------------------------------------------------------------------------------------
// The view: what the steps lowered so far leave.

/// The steps lowered so far, as events, so each step's `pre` is the state the steps
/// before it leave.
#[derive(Debug, Default)]
struct View {
    events: Vec<Event>,
}

#[derive(Debug)]
enum Event {
    /// A rename: what was at or below `from` is at or below `to` from then on.
    Move { from: PathBuf, to: PathBuf },
    /// A write or a chmod, after which `path` holds `post`, `size` bytes long.
    Set {
        path: PathBuf,
        post: Endpoint,
        size: u64,
    },
}

/// Where what the view has at a path comes from.
#[derive(Debug)]
enum Origin {
    /// What is at this path on disk now, if anything.
    Disk(PathBuf),
    /// Nothing: an earlier step moved it away.
    Gone,
    /// What an earlier write or chmod left.
    Set { post: Endpoint, size: u64 },
}

impl View {
    fn origin(&self, path: &Path) -> Origin {
        self.origin_at(path, self.events.len())
    }

    /// The origin of `path` as the first `n` events leave it.
    fn origin_at(&self, path: &Path, n: usize) -> Origin {
        let mut path = path.to_path_buf();
        for event in self.events[..n].iter().rev() {
            match event {
                Event::Move { from, to } => {
                    if let Ok(rest) = path.strip_prefix(to) {
                        path = joined(from, rest);
                    } else if path.starts_with(from) {
                        return Origin::Gone;
                    }
                }
                Event::Set {
                    path: set,
                    post,
                    size,
                } => {
                    if path == *set {
                        return Origin::Set {
                            post: post.clone(),
                            size: *size,
                        };
                    }
                    if path.starts_with(set) {
                        return Origin::Gone;
                    }
                }
            }
        }
        Origin::Disk(path)
    }

    /// Where an entry at `path` after the first `start` events is once the moves after
    /// them ran.
    fn forward(&self, path: &Path, start: usize) -> PathBuf {
        let mut path = path.to_path_buf();
        for event in &self.events[start..] {
            if let Event::Move { from, to } = event
                && let Ok(rest) = path.strip_prefix(from)
            {
                path = joined(to, rest);
            }
        }
        path
    }
}

/// `base` joined with `rest`, without the trailing separator an empty `rest` adds.
fn joined(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

/// What the view has at a path.
enum Seen {
    Nothing,
    /// On disk at `real`.
    Disk {
        real: PathBuf,
        meta: fs::Metadata,
    },
    Set {
        post: Endpoint,
        size: u64,
    },
}

/// One entry of a stat walk, as [`StampDigest`] digests it.
struct Stat {
    kind: &'static str,
    size: u64,
    mode: u32,
    mtime_ns: i64,
}

impl Stat {
    fn of(meta: &fs::Metadata) -> Self {
        let kind = meta.file_type();
        let kind = if kind.is_dir() {
            "dir"
        } else if kind.is_file() {
            "file"
        } else if kind.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        Self {
            kind,
            size: meta.len(),
            mode: meta.mode() & 0o7777,
            // Saturating, as `StampDigest::compute`: an mtime past 2262 does not fit.
            mtime_ns: meta
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.mtime_nsec()),
        }
    }
}

/// Every entry below `root` by its path (`root` too when `with_root`), read with `lstat`
/// and never through a symlink: what [`StampDigest::compute`] reads.
fn stat_walk(root: &Path, with_root: bool) -> io::Result<Vec<(PathBuf, Stat)>> {
    let meta = fs::symlink_metadata(root)?;
    let mut out = Vec::new();
    if with_root {
        out.push((root.to_path_buf(), Stat::of(&meta)));
    }
    let mut pending = if meta.is_dir() {
        vec![root.to_path_buf()]
    } else {
        Vec::new()
    };
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                pending.push(path.clone());
            }
            out.push((path, Stat::of(&meta)));
        }
    }
    Ok(out)
}

/// What fsx captured, as the repair journal records it.
fn endpoint(expect: Expect) -> Result<Endpoint, LowerError> {
    let Expect::Present(found) = expect else {
        return Ok(Endpoint::Absent);
    };
    let identity = found.identity.map(identity);
    Ok(match found.file_type {
        Some(FileKind::Directory) => Endpoint::Dir {
            identity,
            stamp: found.stamp.map(stamp).transpose()?,
        },
        Some(FileKind::Symlink) => Endpoint::Symlink {
            identity,
            link_target: found.link_target,
        },
        Some(FileKind::Regular) | None => Endpoint::File {
            identity,
            sha256: found.sha256.map(Sha256),
            mode: found.mode.and_then(Mode::new),
        },
    })
}

fn identity(id: FileId) -> Identity {
    Identity {
        ino: id.ino,
        born_ns: id.born_ns,
    }
}

fn stamp(digest: StampDigest) -> Result<Stamp, LowerError> {
    Stamp::from_str(&digest.to_string()).map_err(|err| internal(err.to_string()))
}

// ---------------------------------------------------------------------------------------
// Lowering.

struct Lowering<'c, 'a> {
    ctx: &'c LowerCtx<'a>,
    bases: Bases,
    view: View,
    steps: Vec<LoweredStep>,
    roots: BTreeSet<PathBuf>,
    /// Indexes into `bases.clients` of the clients a step touches.
    touched: BTreeSet<usize>,
}

/// The fix a step lowers: its 1-based place in the run, and its failure mode.
struct FixOf {
    fix: u32,
    fm: Cow<'static, str>,
}

impl Lowering<'_, '_> {
    fn fix(&mut self, fix: u32, planned: &PlannedFix) -> Result<(), LowerError> {
        let of = &FixOf {
            fix,
            fm: planned.fm.clone(),
        };
        match &planned.fix {
            Fix::Write {
                path,
                content,
                mode,
            } => self.write(of, path, content, *mode),
            Fix::Chmod { path, mode } => self.chmod(of, path, *mode),
            Fix::Quarantine { path, root } => self.quarantine(of, path, root),
            Fix::Flatten { inner, outer } => self.flatten(of, inner, outer),
            Fix::Suspend { journal } => self.suspend(of, journal),
            // The engine's own recovery settles it before the primitives run (M4-D37).
            // So does its snapshot retention (`doctor fix`'s prunes), after them.
            Fix::Delegate { .. } | Fix::Retain { .. } => Ok(()),
        }
    }

    fn finish(self, fixes: Vec<FixEntry>) -> Result<Lowered, LowerError> {
        let steps: Vec<Step> = self
            .steps
            .iter()
            .map(|lowered| lowered.step.clone())
            .collect();
        validate_steps(&fixes, &steps).map_err(|err| internal(err.to_string()))?;
        let mut resources = ResourceSet::new();
        for lowered in &self.steps {
            resources.extend(&lowered.resources);
        }
        let mut client_dirs: Vec<(PathBuf, TargetKey)> = self
            .touched
            .iter()
            .map(|&i| self.bases.clients[i].clone())
            .collect();
        client_dirs.sort_by_key(|(_, key)| *key);
        client_dirs.dedup_by_key(|(_, key)| *key);
        Ok(Lowered {
            fixes,
            targets: client_dirs.iter().map(|(_, key)| *key).collect(),
            client_dirs,
            steps: self.steps,
            resources,
            roots: self.roots.into_iter().collect(),
        })
    }

    fn next_seq(&self) -> Result<u32, LowerError> {
        u32::try_from(self.steps.len() + 1)
            .map_err(|_| internal("a repair has more steps than a journal numbers".to_owned()))
    }

    fn push(&mut self, lowered: LoweredStep) {
        for path in lowered.step.touched() {
            if let Place::Client(i) = self.bases.place(path) {
                self.touched.insert(i);
            }
        }
        self.steps.push(lowered);
    }

    /// A path a step names, refused unless a repair may touch it with `access`, and
    /// resolved as `mutate()` will resolve it. Returns the side fsx will match it on.
    fn admit(&self, seq: u32, path: &Path, access: Access) -> Result<(PathBuf, Side), LowerError> {
        let resolved = path.is_absolute().then(|| resolve(path)).flatten();
        // Before anything that would print the path: one below `WTF/` names an account.
        let wtf = [
            Some(path.to_path_buf()),
            Some(lexical(path)),
            resolved.clone(),
        ]
        .into_iter()
        .flatten()
        .find_map(|p| self.bases.wtf_of(&p).map(Path::to_path_buf));
        if let Some(client) = wtf {
            return Err(scope_denied(format!(
                "a repair would write below the WTF/ of {}, and the doctor never writes WTF/",
                client.display()
            )));
        }
        check_path(seq, path).map_err(|err| internal(err.to_string()))?;
        let resolved =
            resolved.ok_or_else(|| internal(format!("{} cannot be resolved", path.display())))?;
        if let Some(client) = self
            .bases
            .unplanned
            .iter()
            .find(|dir| resolved.starts_with(dir))
        {
            return Err(scope_denied(format!(
                "{} is in a client this repair was not planned for ({})",
                resolved.display(),
                client.display()
            )));
        }
        let matched = self
            .ctx
            .scopes
            .check(Writer::Doctor, &OwnedFolders::default(), &resolved, access)
            .map_err(refusal)?;
        Ok((resolved, matched.side))
    }

    fn seen(&self, path: &Path) -> Result<Seen, LowerError> {
        match self.view.origin(path) {
            Origin::Gone => Ok(Seen::Nothing),
            Origin::Set { post, size } => Ok(Seen::Set { post, size }),
            Origin::Disk(real) => match fs::symlink_metadata(&real) {
                Ok(meta) => Ok(Seen::Disk { real, meta }),
                // ubs:ignore — an io::ErrorKind, not a secret
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Seen::Nothing),
                Err(err) => Err(io_error(&real, &err)),
            },
        }
    }

    fn capture(&self, real: &Path, parts: Parts) -> Result<Endpoint, LowerError> {
        endpoint(Expect::capture(real, parts).map_err(|err| io_error(real, &err))?)
    }

    /// Lists `dir` and each of its missing ancestors as roots the commit creates, up to
    /// the first folder the view has.
    fn create(&mut self, dir: &Path) -> Result<(), LowerError> {
        for ancestor in dir.ancestors() {
            match self.seen(ancestor)? {
                Seen::Nothing => {
                    self.ctx
                        .scopes
                        .check(
                            Writer::Doctor,
                            &OwnedFolders::default(),
                            ancestor,
                            Access::CreateDir,
                        )
                        .map_err(refusal)?;
                    self.roots.insert(ancestor.to_path_buf());
                }
                Seen::Disk { meta, .. } if meta.is_dir() => return Ok(()),
                _ => {
                    return Err(scope_denied(format!(
                        "{} is not a folder, so nothing can be moved below it",
                        ancestor.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// The mount `path` (on disk) is on. A kernel that gives no mount id fails closed.
    fn mount(&self, path: &Path, from: &Path, to: &Path) -> Result<u64, LowerError> {
        (self.ctx.mount_id)(path).map_err(|err| {
            // ubs:ignore — an io::ErrorKind, not a secret
            if err.kind() == io::ErrorKind::Unsupported {
                mount_refusal(
                    from,
                    to,
                    format!(
                        "{}: the kernel gives no mount id, so the doctor cannot tell whether {} is on its mount; nothing was moved",
                        from.display(),
                        to.display()
                    ),
                )
            } else {
                io_error(path, &err)
            }
        })
    }

    /// The mount of the deepest ancestor of `dir` (itself included) the view has on disk.
    fn mount_of(&self, dir: &Path, from: &Path, to: &Path) -> Result<u64, LowerError> {
        for ancestor in dir.ancestors() {
            if let Origin::Disk(real) = self.view.origin(ancestor)
                && fs::symlink_metadata(&real).is_ok()
            {
                return self.mount(&real, from, to);
            }
        }
        Err(internal(format!(
            "no folder above {} exists",
            dir.display()
        )))
    }

    /// Refused unless a rename from `from` to `to` stays on one mount and moves no mount
    /// point (`real_from`, where the source is on disk now).
    fn same_mount(
        &self,
        from: &Path,
        to: &Path,
        real_from: Option<&Path>,
    ) -> Result<(), LowerError> {
        let mount = self.mount_of(parent_of(from)?, from, to)?;
        if let Some(real) = real_from
            && self.mount(real, from, to)? != mount
        {
            return Err(mount_refusal(
                from,
                to,
                format!(
                    "{} is a mount point, and a quarantine never crosses one; nothing was moved",
                    from.display()
                ),
            ));
        }
        if self.mount_of(parent_of(to)?, from, to)? != mount {
            return Err(mount_refusal(
                from,
                to,
                format!(
                    "{} and {}: they are on different mounts, and a quarantine never crosses one; nothing was moved",
                    from.display(),
                    to.display()
                ),
            ));
        }
        Ok(())
    }

    /// The stamp the directory at `dir` (on disk at `origin`) will have once the steps
    /// before this one ran: the entries below it and below anything moved into it, each
    /// where the moves leave it. Exact for a flatten, whose renames move entries straight
    /// out of or into a stamped folder, whose own mtime a stamp leaves out. A move deeper
    /// inside would change a folder's mtime the view cannot know; only two fixes over one
    /// path do that, and `doctor fix` refuses those (M4-D62).
    fn stamp_of(&self, dir: &Path, origin: &Path) -> Result<Stamp, LowerError> {
        let mut roots = vec![(origin.to_path_buf(), false)];
        for (at, event) in self.view.events.iter().enumerate() {
            if let Event::Move { from, to } = event
                && self
                    .view
                    .forward(to, at + 1)
                    .strip_prefix(dir)
                    .is_ok_and(|rest| !rest.as_os_str().is_empty())
                && let Origin::Disk(real) = self.view.origin_at(from, at)
            {
                roots.push((real, true));
            }
        }
        let mut read = BTreeSet::new();
        let mut entries = Vec::new();
        for (root, with_root) in roots {
            for (real, stat) in stat_walk(&root, with_root).map_err(|err| io_error(&root, &err))? {
                let now = self.view.forward(&real, 0);
                if !read.insert(real) {
                    continue;
                }
                if let Ok(rel) = now.strip_prefix(dir)
                    && !rel.as_os_str().is_empty()
                {
                    entries.push((
                        rel.to_string_lossy().into_owned(),
                        stat.kind,
                        stat.size,
                        stat.mode,
                        stat.mtime_ns,
                    ));
                }
            }
        }
        stamp(StampDigest::from_entries(entries))
    }

    /// A rename's source as the view has it, and where it is on disk now: its kind with
    /// its identity, and its stamp (a folder) or link target (a symlink).
    fn source(&self, path: &Path) -> Result<(Endpoint, Option<PathBuf>), LowerError> {
        match self.seen(path)? {
            Seen::Nothing => Err(missing(path)),
            Seen::Set { post, .. } => Ok((post, None)),
            Seen::Disk { real, meta } => {
                let kind = meta.file_type();
                let parts = Parts {
                    identity: true,
                    link_target: kind.is_symlink(),
                    ..Parts::default()
                };
                if !(kind.is_dir() || kind.is_file() || kind.is_symlink()) {
                    return Err(scope_denied(format!(
                        "{} is a special file; the doctor moves files, folders and its own temporary links only",
                        path.display()
                    )));
                }
                let mut source = self.capture(&real, parts)?;
                if let Endpoint::Dir { stamp, .. } = &mut source {
                    *stamp = Some(self.stamp_of(path, &real)?);
                }
                Ok((source, Some(real)))
            }
        }
    }

    /// Only a temporary link a crash left in omaddons' own folders moves, and only into a
    /// quarantine, as fsx's one symlink exception admits.
    fn temp_link(&self, from: &Path, to: &Path) -> Result<(), LowerError> {
        if !temporary(from) {
            return Err(scope_denied(format!(
                "{} is a symlink; the doctor moves only the temporary links omaddons leaves ({TEMP_PREFIX}…)",
                from.display()
            )));
        }
        let scopes = self.ctx.scopes;
        if !(scopes.is_app_owned(from) && scopes.is_quarantine(to) && temporary(to)) {
            return Err(scope_denied(format!(
                "{} is a temporary link, which the doctor moves only from omaddons' own folders into a quarantine",
                from.display()
            )));
        }
        Ok(())
    }

    /// One `rename_noreplace` from `from` to `to`, both admitted: the source as the view
    /// has it, a destination nothing holds, one mount, and the folders it needs listed.
    fn rename(
        &mut self,
        of: &FixOf,
        (from, to): (&Path, &Path),
        quarantine: Option<PathBuf>,
        guards: Vec<Guard>,
    ) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        let (source, real) = self.source(from)?;
        if matches!(source, Endpoint::Symlink { .. }) {
            self.temp_link(from, to)?;
        }
        if !matches!(self.seen(to)?, Seen::Nothing) {
            return Err(exists(to));
        }
        self.same_mount(from, to, real.as_deref())?;
        self.create(parent_of(to)?)?;
        let (kind, after) = match &source {
            Endpoint::Dir { identity, .. } => (
                PathKind::Subtree,
                Endpoint::Dir {
                    identity: *identity,
                    stamp: None,
                },
            ),
            Endpoint::File { identity, .. } => (
                PathKind::File,
                Endpoint::File {
                    identity: *identity,
                    sha256: None,
                    mode: None,
                },
            ),
            Endpoint::Symlink { identity, .. } => (
                PathKind::File,
                Endpoint::Symlink {
                    identity: *identity,
                    link_target: None,
                },
            ),
            Endpoint::Absent => return Err(missing(from)),
        };
        let mut resources = ResourceSet::new();
        resources.rename(resource(from, kind)?, resource(to, kind)?);
        self.view.events.push(Event::Move {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
        self.push(LoweredStep {
            step: Step {
                seq,
                fix: of.fix,
                fm: of.fm.clone(),
                op: Primitive::RenameNoReplace {
                    from: from.to_path_buf(),
                    to: to.to_path_buf(),
                    activation: false,
                },
                pre: Endpoints::Pair {
                    path: source,
                    to: Endpoint::Absent,
                },
                post: Endpoints::Pair {
                    path: Endpoint::Absent,
                    to: after,
                },
                guards,
            },
            resources,
            backup_bytes: 0,
            quarantine,
            rollback_to: None,
            content: None,
        });
        Ok(())
    }

    fn write(
        &mut self,
        of: &FixOf,
        path: &Path,
        content: &[u8],
        mode: Mode,
    ) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        let (path, side) = self.admit(seq, path, Access::Write)?;
        let over = match self.seen(&path)? {
            Seen::Nothing => None,
            Seen::Set { post, size } => Some((post, size)),
            Seen::Disk { real, meta } if meta.is_file() => {
                let parts = Parts {
                    sha256: true,
                    mode: true,
                    ..Parts::default()
                };
                Some((self.capture(&real, parts)?, meta.len()))
            }
            Seen::Disk { meta, .. } => {
                return Err(scope_denied(format!(
                    "{} is a {}, not a regular file; the doctor writes whole files only",
                    path.display(),
                    kind_of(&meta)
                )));
            }
        };
        let mut resources = ResourceSet::new();
        resources.write(resource(&path, PathKind::File)?);
        let (pre, backup, backup_bytes, rollback_to) = match over {
            Some((pre, size)) => {
                let (backup, kept) = self.backup(seq, &side)?;
                resources.write(kept);
                (pre, Some(backup), size, None)
            }
            None => {
                let folder = parent_of(&path)?;
                if !matches!(self.seen(folder)?, Seen::Disk { meta, .. } if meta.is_dir()) {
                    return Err(io_error(
                        folder,
                        &io::Error::new(
                            io::ErrorKind::NotFound,
                            "this folder does not exist, so the doctor cannot create a file in it",
                        ),
                    ));
                }
                (Endpoint::Absent, None, 0, Some(self.rollback(seq, &path)?))
            }
        };
        let post = Endpoint::File {
            identity: None,
            sha256: Some(Sha256(sha2::Sha256::digest(content).into())),
            mode: Some(mode),
        };
        self.view.events.push(Event::Set {
            path: path.clone(),
            post: post.clone(),
            size: content.len() as u64,
        });
        self.push(LoweredStep {
            step: Step {
                seq,
                fix: of.fix,
                fm: of.fm.clone(),
                op: Primitive::WriteFile { path, mode, backup },
                pre: Endpoints::One(pre),
                post: Endpoints::One(post),
                guards: Vec::new(),
            },
            resources,
            backup_bytes,
            quarantine: None,
            rollback_to,
            content: Some(content.to_vec()),
        });
        Ok(())
    }

    /// Where fsx keeps the file the write at `seq` overwrites, and the resource it is. The
    /// commit's tracked session numbers its actions 1, 2, … in primitive order, so its
    /// backup of action `seq` lands exactly here.
    fn backup(&self, seq: u32, side: &Side) -> Result<(PathBuf, Resource), LowerError> {
        match side {
            Side::Home => {
                let path = self.bases.run.join("backups").join(seq.to_string());
                let kept = Resource::backup(&self.bases.run_id, seq)
                    .map_err(|err| internal(err.to_string()))?;
                Ok((path, kept))
            }
            Side::Client(dir) => {
                let path = ClientPaths::new(dir)
                    .snapshots_repair(&self.ctx.tx)
                    .join("files")
                    .join(seq.to_string());
                let kept = resource(&path, PathKind::File)?;
                Ok((path, kept))
            }
        }
    }

    /// Where a rollback quarantines the file the write at `seq` creates, checked now as
    /// any rename is, with its folder listed as a root.
    fn rollback(&mut self, seq: u32, path: &Path) -> Result<PathBuf, LowerError> {
        let place = self.bases.place(path);
        let Some((base, _)) = self.bases.quarantine(place, &self.ctx.tx) else {
            return Err(scope_denied(format!(
                "{} has no quarantine on its side, so a file the doctor created there could not be rolled back",
                path.display()
            )));
        };
        let folder = base.join("rolled_back");
        let (to, _) = self.admit(seq, &folder.join(seq.to_string()), Access::Rename)?;
        self.same_mount(path, &to, None)?;
        self.create(&folder)?;
        Ok(to)
    }

    fn chmod(&mut self, of: &FixOf, path: &Path, mode: Mode) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        let (path, _) = self.admit(seq, path, Access::Write)?;
        let (found, size) = match self.seen(&path)? {
            Seen::Nothing => return Err(missing(&path)),
            Seen::Set { post, size } => (post, size),
            Seen::Disk { real, meta } if meta.is_file() => {
                let parts = Parts {
                    identity: true,
                    mode: true,
                    ..Parts::default()
                };
                (self.capture(&real, parts)?, meta.len())
            }
            Seen::Disk { meta, .. } => {
                return Err(scope_denied(format!(
                    "{} is a {}, not a regular file; the doctor changes modes of files only",
                    path.display(),
                    kind_of(&meta)
                )));
            }
        };
        let Endpoint::File {
            identity,
            mode: old,
            ..
        } = found
        else {
            return Err(internal(format!(
                "{} was planned as a file and is not one",
                path.display()
            )));
        };
        // Nothing to do; the fix stays in `fixes`.
        if old == Some(mode) {
            return Ok(());
        }
        let post = Endpoint::File {
            identity,
            sha256: None,
            mode: Some(mode),
        };
        let mut resources = ResourceSet::new();
        resources.write(resource(&path, PathKind::File)?);
        self.view.events.push(Event::Set {
            path: path.clone(),
            post: post.clone(),
            size,
        });
        self.push(LoweredStep {
            step: Step {
                seq,
                fix: of.fix,
                fm: of.fm.clone(),
                op: Primitive::Chmod { path, mode },
                pre: Endpoints::One(Endpoint::File {
                    identity,
                    sha256: None,
                    mode: old,
                }),
                post: Endpoints::One(post),
                guards: Vec::new(),
            },
            resources,
            backup_bytes: 0,
            quarantine: None,
            rollback_to: None,
            content: None,
        });
        Ok(())
    }

    fn quarantine(
        &mut self,
        of: &FixOf,
        path: &Path,
        root: &QuarantineRoot,
    ) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        let (from, _) = self.admit(seq, path, Access::Rename)?;
        let (to, base) = self.bases.destination(&from, root, &self.ctx.tx)?;
        let (to, _) = self.admit(seq, &to, Access::Rename)?;
        self.rename(of, (&from, &to), Some(base), Vec::new())
    }

    /// One rename per artifact, in order, each into its root; the suspension's guards go
    /// on the rename that moves the journal file out of `<state>/journal/` (M4-D38).
    fn suspend(&mut self, of: &FixOf, journal: &JournalSet) -> Result<(), LowerError> {
        let mut guarded = false;
        for artifact in &journal.artifacts {
            let seq = self.next_seq()?;
            let (from, _) = self.admit(seq, &artifact.path, Access::Rename)?;
            let (to, base) = self
                .bases
                .destination(&from, &artifact.root, &self.ctx.tx)?;
            let (to, _) = self.admit(seq, &to, Access::Rename)?;
            let guards = if from.parent() == Some(self.bases.journal.as_path()) {
                guarded = true;
                journal.guards.clone()
            } else {
                Vec::new()
            };
            self.rename(of, (&from, &to), Some(base), guards)?;
        }
        if !guarded && !journal.guards.is_empty() {
            return Err(internal(format!(
                "the suspension of {} moves no journal file out of {}, so its guards would be lost",
                journal.tx,
                self.bases.journal.display()
            )));
        }
        Ok(())
    }

    /// `inner` (`<A>/X/Y`) lifted to `<A>/Y` and its emptied wrapper `outer` (`<A>/X`)
    /// quarantined under its own name. When `Y` is `X`'s own name (in any case), the
    /// wrapper first steps aside to `<A>/.omaddons-flatten-<tx>`, its child then moves to
    /// `<A>/Y`, and the wrapper goes last.
    fn flatten(&mut self, of: &FixOf, inner: &Path, outer: &Path) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        let (outer, _) = self.admit(seq, outer, Access::Rename)?;
        let (inner, _) = self.admit(seq, inner, Access::Rename)?;
        if inner.parent() != Some(outer.as_path()) {
            return Err(internal(format!(
                "{} is not a direct child of {}; a flatten lifts a folder out of the folder that wraps it",
                inner.display(),
                outer.display()
            )));
        }
        let addons = parent_of(&outer)?.to_path_buf();
        let client = self
            .bases
            .clients
            .iter()
            .position(|(dir, _)| ClientPaths::new(dir).addons() == addons)
            .ok_or_else(|| {
                scope_denied(format!(
                    "{} is not a folder of a client's Interface/AddOns; a flatten lifts addon folders only",
                    outer.display()
                ))
            })?;
        for folder in [&outer, &inner] {
            match self.seen(folder)? {
                Seen::Disk { meta, .. } if meta.is_dir() => {}
                Seen::Nothing => return Err(missing(folder)),
                _ => {
                    return Err(scope_denied(format!(
                        "{} is not a folder; a flatten lifts folders only",
                        folder.display()
                    )));
                }
            }
        }
        let (Some(wrapper), Some(name)) = (outer.file_name(), inner.file_name()) else {
            return Err(internal(format!("{} names no folder", inner.display())));
        };
        let own_name = name.eq_ignore_ascii_case(wrapper);
        if let Some(taken) = self.case_variant(&addons, name, own_name.then_some(wrapper))? {
            return Err(exists(&taken));
        }
        let (client_dir, _) = &self.bases.clients[client];
        let base = ClientPaths::new(client_dir)
            .snapshots_repair(&self.ctx.tx)
            .join("quarantine");
        let parked = base.join(
            outer
                .strip_prefix(client_dir)
                .map_err(|_| internal(format!("{} left its client", outer.display())))?,
        );
        let (parked, _) = self.admit(seq, &parked, Access::Rename)?;
        let (lifted, _) = self.admit(seq, &addons.join(name), Access::Rename)?;
        if own_name {
            let provisional = addons.join(format!("{FLATTEN_PREFIX}{}", self.ctx.tx));
            let (provisional, _) = self.admit(seq, &provisional, Access::Rename)?;
            let (moved, _) = self.admit(seq, &provisional.join(name), Access::Rename)?;
            self.rename(of, (&outer, &provisional), None, Vec::new())?;
            self.rename(of, (&moved, &lifted), None, Vec::new())?;
            self.rename(of, (&provisional, &parked), Some(base), Vec::new())
        } else {
            self.rename(of, (&inner, &lifted), None, Vec::new())?;
            self.rename(of, (&outer, &parked), Some(base), Vec::new())
        }
    }

    /// An entry of `dir`, as the view has it, whose name is `name` in any case (but
    /// `except`): the game folds case, so a lifted folder never lands beside one.
    fn case_variant(
        &self,
        dir: &Path,
        name: &OsStr,
        except: Option<&OsStr>,
    ) -> Result<Option<PathBuf>, LowerError> {
        let mut entries = Vec::new();
        if let Origin::Disk(real) = self.view.origin(dir) {
            for entry in fs::read_dir(&real).map_err(|err| io_error(&real, &err))? {
                let entry = entry.map_err(|err| io_error(&real, &err))?;
                entries.push(self.view.forward(&entry.path(), 0));
            }
        }
        for (at, event) in self.view.events.iter().enumerate() {
            if let Event::Move { to, .. } = event {
                entries.push(self.view.forward(to, at + 1));
            }
        }
        Ok(entries.into_iter().find(|entry| {
            entry.parent() == Some(dir)
                && entry
                    .file_name()
                    .is_some_and(|found| found.eq_ignore_ascii_case(name) && Some(found) != except)
        }))
    }
}

// ---------------------------------------------------------------------------------------
// The undo of a repair (E7).

/// The undo of a repair-family transaction, lowered from its verified plan: one step per
/// inverse, in the plan's order, its predicates the plan's (verification proved them
/// live on what the steps before it leave; nothing is captured again). Each quarantine
/// destination is resolved below this undo's own run (or its client's `<tx>.repair`
/// half), each write reserves its backup there and takes its content from the reverted
/// repair's backup, read again and checked against the digest verification proved, and
/// every path is admitted and every rename's mounts checked as a repair's are. It writes
/// nothing: the commit creates the roots, and only the folders no earlier inverse puts in
/// place are roots.
pub fn lowered_from_inverses(
    ctx: &LowerCtx<'_>,
    verified: &VerifiedPlan,
) -> Result<Lowered, LowerError> {
    let plan = &verified.plan;
    let mut lowering = Lowering {
        ctx,
        bases: Bases::new(ctx)?,
        view: View::default(),
        steps: Vec::new(),
        roots: BTreeSet::new(),
        touched: BTreeSet::new(),
    };
    for inverse in &plan.inverses {
        lowering.inverse(plan, inverse)?;
    }
    let mut lowered = lowering.finish(plan.fixes.clone())?;
    for read in &plan.reads {
        let resource =
            Resource::backup(&read.run, read.seq).map_err(|err| internal(err.to_string()))?;
        lowered.resources.read(resource);
    }
    Ok(lowered)
}

impl Lowering<'_, '_> {
    fn inverse(&mut self, plan: &RepairUndoPlan, inverse: &InverseStep) -> Result<(), LowerError> {
        let seq = self.next_seq()?;
        // ubs:ignore — comparing step numbers, not secrets
        if seq != inverse.seq {
            return Err(internal(format!(
                "the undo's inverse {} would be its step {seq}",
                inverse.seq
            )));
        }
        let of = FixOf {
            fix: inverse.fix,
            fm: Cow::Owned(inverse.fm.clone()),
        };
        match (inverse.kind, &inverse.to, inverse.mode) {
            (InverseKind::RestoreWrite, Dest::None, Some(mode)) => {
                self.restore_write(plan, &of, inverse, mode)
            }
            (InverseKind::ChmodBack, Dest::None, Some(mode)) => {
                let (path, _) = self.admit(seq, &inverse.path, Access::Write)?;
                let size = fs::symlink_metadata(&path).map_or(0, |meta| meta.len());
                let Endpoints::One(post) = &inverse.post else {
                    return Err(internal(format!("inverse {seq} is a chmod with two ends")));
                };
                self.view.events.push(Event::Set {
                    path: path.clone(),
                    post: post.clone(),
                    size,
                });
                let mut resources = ResourceSet::new();
                resources.write(resource(&path, PathKind::File)?);
                self.push(LoweredStep {
                    step: Step {
                        seq,
                        fix: of.fix,
                        fm: of.fm,
                        op: Primitive::Chmod { path, mode },
                        pre: inverse.pre.clone(),
                        post: inverse.post.clone(),
                        guards: Vec::new(),
                    },
                    resources,
                    backup_bytes: 0,
                    quarantine: None,
                    rollback_to: None,
                    content: None,
                });
                Ok(())
            }
            (InverseKind::QuarantineCreated, Dest::Quarantine { side, rel }, None) => {
                let place = match side {
                    QuarantineSide::State => Place::State,
                    QuarantineSide::Config => Place::Config,
                    QuarantineSide::Cache => Place::Cache,
                    QuarantineSide::Skill => Place::Skill,
                    QuarantineSide::Client(key) => self
                        .bases
                        .clients
                        .iter()
                        .position(|(_, client)| client == key)
                        .map(Place::Client)
                        .ok_or_else(|| {
                            internal(format!(
                                "inverse {seq} quarantines a file of target {key}, which this undo was not planned for"
                            ))
                        })?,
                };
                let Some((base, _)) = self.bases.quarantine(place, &self.ctx.tx) else {
                    return Err(internal(format!(
                        "inverse {seq} names a side with no quarantine"
                    )));
                };
                let to = base.join(rel);
                self.rename_back(&of, inverse, &to, Some(base))
            }
            (
                InverseKind::RenameBack
                | InverseKind::ToProvisional
                | InverseKind::Activate
                | InverseKind::Deactivate,
                Dest::Path(to),
                None,
            ) => self.rename_back(&of, inverse, to, None),
            _ => Err(internal(format!(
                "inverse {seq} ({:?}) does not fit its destination or mode",
                inverse.kind
            ))),
        }
    }

    /// A write of the reverted repair's backup back over its file, with the mode it
    /// replaced, keeping what it overwrites in this undo's own backup.
    fn restore_write(
        &mut self,
        plan: &RepairUndoPlan,
        of: &FixOf,
        inverse: &InverseStep,
        mode: Mode,
    ) -> Result<(), LowerError> {
        let seq = inverse.seq;
        let (path, side) = self.admit(seq, &inverse.path, Access::Write)?;
        let read = plan
            .reads
            .iter()
            // ubs:ignore — comparing step numbers, not secrets
            .find(|read| read.seq == inverse.reverts)
            .ok_or_else(|| {
                internal(format!(
                    "inverse {seq} restores a backup the plan lists not"
                ))
            })?;
        let content = backup_content(plan, read)?;
        let (backup, kept) = self.backup(seq, &side)?;
        let backup_bytes = fs::symlink_metadata(&path).map_or(0, |meta| meta.len());
        let Endpoints::One(post) = &inverse.post else {
            return Err(internal(format!("inverse {seq} is a write with two ends")));
        };
        self.view.events.push(Event::Set {
            path: path.clone(),
            post: post.clone(),
            size: content.len() as u64,
        });
        let mut resources = ResourceSet::new();
        resources
            .write(resource(&path, PathKind::File)?)
            .write(kept);
        self.push(LoweredStep {
            step: Step {
                seq,
                fix: of.fix,
                fm: of.fm.clone(),
                op: Primitive::WriteFile {
                    path,
                    mode,
                    backup: Some(backup),
                },
                pre: inverse.pre.clone(),
                post: inverse.post.clone(),
                guards: Vec::new(),
            },
            resources,
            backup_bytes,
            quarantine: None,
            rollback_to: None,
            content: Some(content),
        });
        Ok(())
    }

    /// One rename of the undo, from the inverse's path to `to`: both admitted, the
    /// destination free on what the steps before it leave, one mount, and the folders it
    /// needs that nothing puts in place listed as roots.
    fn rename_back(
        &mut self,
        of: &FixOf,
        inverse: &InverseStep,
        to: &Path,
        quarantine: Option<PathBuf>,
    ) -> Result<(), LowerError> {
        let seq = inverse.seq;
        let (from, _) = self.admit(seq, &inverse.path, Access::Rename)?;
        let (to, _) = self.admit(seq, to, Access::Rename)?;
        if !matches!(self.seen(&to)?, Seen::Nothing) {
            return Err(exists(&to));
        }
        let real = match self.view.origin(&from) {
            Origin::Disk(real) if fs::symlink_metadata(&real).is_ok() => Some(real),
            _ => None,
        };
        // A folder's stamp as the steps before it leave it: a flatten's wrapper back at
        // its provisional name holds the inner folder again by the time it moves on.
        // Verification read the stamp from the disk as it is now, before any of them.
        let mut pre = inverse.pre.clone();
        if let (
            Endpoints::Pair {
                path: Endpoint::Dir { stamp, .. },
                ..
            },
            Some(real),
        ) = (&mut pre, real.as_deref())
        {
            *stamp = Some(self.stamp_of(&from, real)?);
        }
        self.same_mount(&from, &to, real.as_deref())?;
        self.create(parent_of(&to)?)?;
        let kind = match &inverse.pre {
            Endpoints::Pair {
                path: Endpoint::Dir { .. },
                ..
            } => PathKind::Subtree,
            _ => PathKind::File,
        };
        let mut resources = ResourceSet::new();
        resources.rename(resource(&from, kind)?, resource(&to, kind)?);
        self.view.events.push(Event::Move {
            from: from.clone(),
            to: to.clone(),
        });
        self.push(LoweredStep {
            step: Step {
                seq,
                fix: of.fix,
                fm: of.fm.clone(),
                op: Primitive::RenameNoReplace {
                    from,
                    to,
                    activation: inverse.activation,
                },
                pre,
                post: inverse.post.clone(),
                guards: Vec::new(),
            },
            resources,
            backup_bytes: 0,
            quarantine,
            rollback_to: None,
            content: None,
        });
        Ok(())
    }
}

/// The bytes of a reverted write's backup, read again (bounded by the size it has) and
/// checked against the digest verification proved: anything else is the backup changed
/// since, refused as verification refuses it.
fn backup_content(plan: &RepairUndoPlan, read: &BackupRead) -> Result<Vec<u8>, LowerError> {
    use std::io::Read as _;
    let changed = || -> LowerError {
        Error::new(
            ErrorCode::UndoConflict,
            format!(
                "1 path(s) changed since the repair {}; undoing it now could overwrite that",
                plan.reverted
            ),
            Suggestion::command(["omaddons", "history"]),
        )
        .details(json!({
            "tx": plan.reverted,
            "reason": "changed_since_repair",
            "paths": [read.path.display().to_string()],
        }))
        .into()
    };
    let meta = fs::symlink_metadata(&read.path).map_err(|_| changed())?;
    if !meta.is_file() {
        return Err(changed());
    }
    let file = fs::File::open(&read.path).map_err(|err| io_error(&read.path, &err))?;
    let mut content = Vec::new();
    file.take(meta.len().saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|err| io_error(&read.path, &err))?;
    let digest: [u8; 32] = sha2::Sha256::digest(&content).into();
    // ubs:ignore — a content digest compared for integrity, not a secret
    if content.len() as u64 != meta.len() || digest != read.sha256 {
        return Err(changed());
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use sha2::Digest as _;

    use super::*;
    use crate::engine::repair_action::{
        Attribution, Delegation, Direction, JournalFamily, JournalPhase, SuspendedArtifact,
    };
    use crate::engine::testing::{
        REPAIR_ACCOUNT, REPAIR_RUN_ID, RepairHome, repair_home, tree_hash,
    };

    const SKILL: &str = "fm-plugins-skill-version-drift";
    const FILE_MODE: &str = "fm-secrets-credential-file-mode";
    const NESTED: &str = "fm-external_artifacts-nested-addon-folder";
    const CORRUPT: &str = "fm-schemas-state-corrupt";
    const TEMP: &str = "fm-state_files-orphan-temp-file";
    const TOO_NEW: &str = "fm-concurrency_primitives-journal-too-new";
    const PENDING: &str = "fm-concurrency_primitives-journal-pending";

    const NEW_GUIDE: &[u8] = b"---\nname: omaddons\n---\nnew guide\n";

    fn tx() -> TxId {
        "01JB8Q2VA00000000000000003".parse().unwrap()
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    fn planned(fm: &'static str, fix: Fix) -> PlannedFix {
        PlannedFix {
            fm: Cow::Borrowed(fm),
            fix,
        }
    }

    type Mounts<'m> = &'m dyn Fn(&Path) -> io::Result<u64>;

    fn lower_in(
        home: &RepairHome,
        skill_dirs: &[PathBuf],
        fixes: &[PlannedFix],
        mount_id: Mounts<'_>,
    ) -> Result<Lowered, LowerError> {
        let ctx = LowerCtx {
            paths: &home.paths,
            tx: tx(),
            run: &home.run,
            clients: &home.planned,
            skill_dirs,
            scopes: &home.scopes,
            mount_id,
        };
        lower(&ctx, fixes)
    }

    fn try_lower(home: &RepairHome, fixes: &[PlannedFix]) -> Result<Lowered, LowerError> {
        lower_in(home, &home.skill_dirs, fixes, &crate::fsx::mount_id)
    }

    fn lowered(home: &RepairHome, case: &str, fixes: &[PlannedFix]) -> Lowered {
        match try_lower(home, fixes) {
            Ok(lowered) => lowered,
            Err(err) => panic!("{case}: {err}\nfixes: {fixes:#?}"), // ubs:ignore — fails the test
        }
    }

    /// A lowered repair's steps as JSON, for messages.
    fn json(lowered: &Lowered) -> String {
        let steps: Vec<&Step> = lowered.steps.iter().map(|lowered| &lowered.step).collect();
        serde_json::to_string_pretty(&steps).unwrap()
    }

    fn client_root(home: &RepairHome) -> QuarantineRoot {
        QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        }
    }

    /// The client's quarantine root for this repair.
    fn client_quarantine(home: &RepairHome) -> PathBuf {
        home.client.snapshots_repair(&tx()).join("quarantine")
    }

    fn run_quarantine(home: &RepairHome, side: &str) -> PathBuf {
        home.run.dir.join("quarantine").join(side)
    }

    fn cache_quarantine(home: &RepairHome) -> PathBuf {
        home.paths.cache_quarantine(REPAIR_RUN_ID)
    }

    fn provisional(home: &RepairHome) -> PathBuf {
        home.addons(&format!(".omaddons-flatten-{}", tx()))
    }

    fn stuck_journal(home: &RepairHome) -> JournalSet {
        let stuck = home.stuck();
        JournalSet {
            tx: stuck,
            family: JournalFamily::Folder,
            target: Some(home.key),
            artifacts: vec![
                SuspendedArtifact {
                    path: home.paths.journal(&stuck),
                    root: QuarantineRoot::State,
                },
                SuspendedArtifact {
                    path: home.paths.inflight_lock(&stuck),
                    root: QuarantineRoot::State,
                },
                SuspendedArtifact {
                    path: home.client.stage(&stuck),
                    root: client_root(home),
                },
            ],
            guards: vec![Guard::Generation {
                target: home.key,
                generation: 7,
            }],
            attribution: Attribution::Complete,
            unattributed: Vec::new(),
        }
    }

    /// `(op, path, to)` of one primitive.
    type Op = (&'static str, PathBuf, Option<PathBuf>);

    fn op_of(step: &Step) -> Op {
        match &step.op {
            Primitive::WriteFile { path, .. } => ("write_file", path.clone(), None),
            Primitive::Chmod { path, .. } => ("chmod", path.clone(), None),
            Primitive::RenameNoReplace { from, to, .. } => {
                ("rename_noreplace", from.clone(), Some(to.clone()))
            }
        }
    }

    fn rename(from: PathBuf, to: PathBuf) -> Op {
        ("rename_noreplace", from, Some(to))
    }

    /// One case per fix kind, named, with its fix and the primitives it lowers to.
    fn kinds(home: &RepairHome) -> Vec<(&'static str, PlannedFix, Vec<Op>)> {
        let a = |rel: &str| home.addons(rel);
        let parked = |name: &str| client_quarantine(home).join("Interface/AddOns").join(name);
        let stuck = home.stuck();
        let notes = home.paths.skill_dir().join("NOTES.md");
        vec![
            (
                "write_over",
                planned(
                    SKILL,
                    Fix::Write {
                        path: home.skill_file(),
                        content: NEW_GUIDE.to_vec(),
                        mode: mode(0o644),
                    },
                ),
                vec![("write_file", home.skill_file(), None)],
            ),
            (
                "write_create",
                planned(
                    SKILL,
                    Fix::Write {
                        path: notes.clone(),
                        content: b"notes\n".to_vec(),
                        mode: mode(0o644),
                    },
                ),
                vec![("write_file", notes, None)],
            ),
            (
                "chmod",
                planned(
                    FILE_MODE,
                    Fix::Chmod {
                        path: home.credential(),
                        mode: mode(0o600),
                    },
                ),
                vec![("chmod", home.credential(), None)],
            ),
            (
                "quarantine_state_file",
                planned(
                    CORRUPT,
                    Fix::Quarantine {
                        path: home.record(),
                        root: QuarantineRoot::State,
                    },
                ),
                vec![rename(
                    home.record(),
                    run_quarantine(home, "state").join(format!("targets/{}.json", home.key)),
                )],
            ),
            (
                "quarantine_config_file",
                planned(
                    CORRUPT,
                    Fix::Quarantine {
                        path: home.credential(),
                        root: QuarantineRoot::Config,
                    },
                ),
                vec![rename(
                    home.credential(),
                    run_quarantine(home, "config").join("credentials/github"),
                )],
            ),
            (
                "quarantine_cache_file",
                planned(
                    CORRUPT,
                    Fix::Quarantine {
                        path: home.blob(),
                        root: QuarantineRoot::Cache,
                    },
                ),
                vec![rename(home.blob(), cache_quarantine(home).join("blobs/ab"))],
            ),
            (
                "quarantine_client_dir",
                planned(
                    NESTED,
                    Fix::Quarantine {
                        path: a("Stray"),
                        root: client_root(home),
                    },
                ),
                vec![rename(a("Stray"), parked("Stray"))],
            ),
            (
                "quarantine_temp_symlink",
                planned(
                    TEMP,
                    Fix::Quarantine {
                        path: home.temp_link(),
                        root: QuarantineRoot::Cache,
                    },
                ),
                vec![rename(
                    home.temp_link(),
                    cache_quarantine(home).join("tmp/.omaddons-link"),
                )],
            ),
            (
                "flatten_two",
                planned(
                    NESTED,
                    Fix::Flatten {
                        inner: a("Wrap/Inner"),
                        outer: a("Wrap"),
                    },
                ),
                vec![
                    rename(a("Wrap/Inner"), a("Inner")),
                    rename(a("Wrap"), parked("Wrap")),
                ],
            ),
            (
                "flatten_three",
                planned(
                    NESTED,
                    Fix::Flatten {
                        inner: a("Same/Same"),
                        outer: a("Same"),
                    },
                ),
                vec![
                    rename(a("Same"), provisional(home)),
                    rename(provisional(home).join("Same"), a("Same")),
                    rename(provisional(home), parked("Same")),
                ],
            ),
            (
                "suspend_three_artifacts",
                planned(
                    TOO_NEW,
                    Fix::Suspend {
                        journal: stuck_journal(home),
                    },
                ),
                vec![
                    rename(
                        home.paths.journal(&stuck),
                        run_quarantine(home, "state").join(format!("journal/{stuck}.jsonl")),
                    ),
                    rename(
                        home.paths.inflight_lock(&stuck),
                        run_quarantine(home, "state").join(format!("inflight/{stuck}.lock")),
                    ),
                    rename(
                        home.client.stage(&stuck),
                        client_quarantine(home).join(format!("Interface/.omaddons/stage/{stuck}")),
                    ),
                ],
            ),
            (
                "delegate",
                planned(
                    PENDING,
                    Fix::Delegate {
                        what: Delegation {
                            tx: stuck,
                            family: JournalFamily::Folder,
                            phase: JournalPhase::Prepared,
                            direction: Direction::RollBack,
                        },
                    },
                ),
                vec![],
            ),
        ]
    }

    fn kind(home: &RepairHome, name: &str) -> PlannedFix {
        kinds(home)
            .into_iter()
            .find(|(case, ..)| *case == name)
            .map(|(_, fix, _)| fix)
            .unwrap()
    }

    #[test]
    fn each_fix_kind_lowers_to_its_primitives() {
        // Lowering writes nothing, so every case lowers on one home.
        let home = repair_home();
        for (case, fix, want) in kinds(&home) {
            let lowered = lowered(&home, case, std::slice::from_ref(&fix));
            let shown = json(&lowered);
            let got: Vec<Op> = lowered.steps.iter().map(|s| op_of(&s.step)).collect();
            assert_eq!(got, want, "{case}: the primitives\n{shown}");
            assert_eq!(
                lowered.fixes,
                vec![FixEntry {
                    fix: 1,
                    fm: fix.fm.clone(),
                    kind: fix.fix.kind()
                }],
                "{case}: one entry for its fix\n{shown}"
            );
            for (lowered, seq) in lowered.steps.iter().zip(1u32..) {
                let step = &lowered.step;
                assert_eq!(
                    (step.seq, step.fix),
                    (seq, 1),
                    "{case}: seq and fix\n{shown}"
                );
                assert_eq!(step.fm, fix.fm, "{case}: step {seq}'s failure mode");
                let into_quarantine = matches!(&step.op,
                    Primitive::RenameNoReplace { to, .. } if home.scopes.is_quarantine(to));
                assert_eq!(
                    lowered.quarantine.is_some(),
                    into_quarantine,
                    "{case}: step {seq} names its quarantine root exactly when it moves into one\n{shown}"
                );
                if let Some(root) = &lowered.quarantine {
                    assert!(
                        step.touched()[1].starts_with(root),
                        "{case}: step {seq} moves below {}\n{shown}",
                        root.display()
                    );
                }
                let journal_file = matches!(&step.op,
                    // ubs:ignore — comparing paths, not secrets
                    Primitive::RenameNoReplace { from, .. } if *from == home.paths.journal(&home.stuck()));
                assert_eq!(
                    !step.guards.is_empty(),
                    journal_file,
                    "{case}: guards go on the journal file's rename only\n{shown}"
                );
            }
            let steps: Vec<Step> = lowered.steps.iter().map(|s| s.step.clone()).collect();
            assert_eq!(
                validate_steps(&lowered.fixes, &steps),
                Ok(()),
                "{case}\n{shown}"
            );
            let backup = match &lowered.steps.first().map(|s| &s.step.op) {
                Some(Primitive::WriteFile { backup, .. }) => backup.clone(),
                _ => None,
            };
            let want_backup = (case == "write_over").then(|| home.run.dir.join("backups/1"));
            assert_eq!(backup, want_backup, "{case}: the reserved backup\n{shown}");
            let rollback = lowered.steps.first().and_then(|s| s.rollback_to.clone());
            let want_rollback = (case == "write_create")
                .then(|| run_quarantine(&home, "skill").join("rolled_back/1"));
            assert_eq!(
                rollback, want_rollback,
                "{case}: a created file's rollback\n{shown}"
            );
        }
    }

    fn id(path: &Path) -> Option<Identity> {
        FileId::of(path).map(identity)
    }

    fn sha(bytes: &[u8]) -> Sha256 {
        Sha256(sha2::Sha256::digest(bytes).into())
    }

    fn stamp_now(dir: &Path) -> Stamp {
        stamp(StampDigest::compute(dir).unwrap()).unwrap()
    }

    fn file(identity: Option<Identity>, sha256: Option<Sha256>, mode: Option<Mode>) -> Endpoint {
        Endpoint::File {
            identity,
            sha256,
            mode,
        }
    }

    fn moved(source: Endpoint, after: Endpoint) -> (Endpoints, Endpoints) {
        (
            Endpoints::Pair {
                path: source,
                to: Endpoint::Absent,
            },
            Endpoints::Pair {
                path: Endpoint::Absent,
                to: after,
            },
        )
    }

    #[test]
    fn predicates_are_captured_from_the_live_paths() {
        let home = repair_home();
        let old_guide = fs::read(home.skill_file()).unwrap();
        let (stray, link) = (home.addons("Stray"), home.temp_link());
        let dir = |path: &Path, stamp: Option<Stamp>| Endpoint::Dir {
            identity: id(path),
            stamp,
        };
        let symlink = |path: &Path, link_target: Option<PathBuf>| Endpoint::Symlink {
            identity: id(path),
            link_target,
        };
        let cases: Vec<(&str, (Endpoints, Endpoints))> = vec![
            (
                "write_over",
                (
                    Endpoints::One(file(None, Some(sha(&old_guide)), Some(mode(0o644)))),
                    Endpoints::One(file(None, Some(sha(NEW_GUIDE)), Some(mode(0o644)))),
                ),
            ),
            (
                "write_create",
                (
                    Endpoints::One(Endpoint::Absent),
                    Endpoints::One(file(None, Some(sha(b"notes\n")), Some(mode(0o644)))),
                ),
            ),
            (
                "chmod",
                (
                    Endpoints::One(file(id(&home.credential()), None, Some(mode(0o644)))),
                    Endpoints::One(file(id(&home.credential()), None, Some(mode(0o600)))),
                ),
            ),
            (
                "quarantine_state_file",
                moved(
                    file(id(&home.record()), None, None),
                    file(id(&home.record()), None, None),
                ),
            ),
            (
                "quarantine_client_dir",
                moved(dir(&stray, Some(stamp_now(&stray))), dir(&stray, None)),
            ),
            (
                "quarantine_temp_symlink",
                moved(
                    symlink(&link, Some(PathBuf::from("../blobs/ab"))),
                    symlink(&link, None),
                ),
            ),
        ];
        assert!(id(&home.credential()).is_some(), "identities are read");
        for (case, (pre, post)) in cases {
            let lowered = lowered(&home, case, &[kind(&home, case)]);
            let step = &lowered.steps[0].step;
            assert_eq!(step.pre, pre, "{case}: pre\n{}", json(&lowered));
            assert_eq!(step.post, post, "{case}: post\n{}", json(&lowered));
        }
    }

    /// What is at `path` now, with exactly the parts `like` records.
    fn live(path: &Path, like: &Endpoint) -> Endpoint {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return Endpoint::Absent;
        };
        let identity = like.identity().and(id(path));
        if meta.is_dir() {
            let stamp = matches!(like, Endpoint::Dir { stamp: Some(_), .. });
            Endpoint::Dir {
                identity,
                stamp: stamp.then(|| stamp_now(path)),
            }
        } else if meta.file_type().is_symlink() {
            let target = matches!(
                like,
                Endpoint::Symlink {
                    link_target: Some(_),
                    ..
                }
            );
            Endpoint::Symlink {
                identity,
                link_target: target.then(|| fs::read_link(path).unwrap()),
            }
        } else {
            let (sha256, mode_part) = match like {
                Endpoint::File { sha256, mode, .. } => (sha256.is_some(), mode.is_some()),
                _ => (true, true),
            };
            Endpoint::File {
                identity,
                sha256: sha256.then(|| sha(&fs::read(path).unwrap())),
                mode: mode_part.then(|| mode(meta.mode() & 0o7777)),
            }
        }
    }

    /// Runs a lowered repair of renames as the commit would (its roots first), and checks
    /// every step's `pre` against what the steps before it left, and its `post` after it.
    fn replay(case: &str, lowered: &Lowered) {
        let shown = json(lowered);
        for root in &lowered.roots {
            fs::create_dir_all(root).unwrap();
        }
        for lowered in &lowered.steps {
            let step = &lowered.step;
            let seq = step.seq;
            let Primitive::RenameNoReplace { from, to, .. } = &step.op else {
                panic!("{case}: step {seq} is not a rename\n{shown}"); // ubs:ignore — fails the test
            };
            let (
                Endpoints::Pair { path, to: dest },
                Endpoints::Pair {
                    path: gone,
                    to: arrived,
                },
            ) = (&step.pre, &step.post)
            else {
                panic!("{case}: step {seq} records one endpoint\n{shown}"); // ubs:ignore — fails the test
            };
            assert_eq!(
                &live(from, path),
                path,
                "{case}: step {seq}'s source before it\n{shown}"
            );
            assert_eq!(
                &live(to, dest),
                dest,
                "{case}: step {seq}'s destination before it\n{shown}"
            );
            fs::rename(from, to).unwrap();
            assert_eq!(
                &live(from, gone),
                gone,
                "{case}: step {seq}'s source after it\n{shown}"
            );
            assert_eq!(
                &live(to, arrived),
                arrived,
                "{case}: step {seq}'s destination after it\n{shown}"
            );
        }
    }

    #[test]
    fn a_three_rename_flatten_predicts_each_steps_endpoints() {
        let home = repair_home();
        let same = home.addons("Same");
        // The wrapper's stamp without its child, from one stat walk before anything moves.
        let without_child: Vec<_> = stat_walk(&same, false)
            .unwrap()
            .into_iter()
            .filter(|(path, _)| !path.starts_with(same.join("Same")))
            .map(|(path, stat)| {
                let rel = path
                    .strip_prefix(&same)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                (rel, stat.kind, stat.size, stat.mode, stat.mtime_ns)
            })
            .collect();
        assert!(
            !without_child.is_empty(),
            "the wrapper holds more than its child"
        );
        let wrapper_left = stamp(StampDigest::from_entries(without_child)).unwrap();
        let lowered = lowered(&home, "flatten_three", &[kind(&home, "flatten_three")]);
        let shown = json(&lowered);
        let pre_source = |seq: usize| match &lowered.steps[seq].step.pre {
            Endpoints::Pair { path, .. } => path.clone(),
            Endpoints::One(_) => panic!("step {seq} records one endpoint\n{shown}"), // ubs:ignore — fails the test
        };
        assert_eq!(
            pre_source(0),
            Endpoint::Dir {
                identity: id(&same),
                stamp: Some(stamp_now(&same))
            },
            "step 1: the wrapper as it is\n{shown}"
        );
        assert_eq!(
            pre_source(1),
            Endpoint::Dir {
                identity: id(&same.join("Same")),
                stamp: Some(stamp_now(&same.join("Same")))
            },
            "step 2: the inner folder, at a name that does not exist yet\n{shown}"
        );
        assert_eq!(
            pre_source(2),
            Endpoint::Dir {
                identity: id(&same),
                stamp: Some(wrapper_left)
            },
            "step 3: the wrapper without its child\n{shown}"
        );
        assert!(!provisional(&home).exists(), "lowering moved nothing");
        replay("flatten_three", &lowered);
        assert!(
            home.addons("Same/Same.toc").is_file(),
            "the addon is lifted"
        );
        assert!(
            !provisional(&home).exists(),
            "no provisional folder is left"
        );
    }

    #[test]
    fn every_rename_is_predicted_against_what_the_steps_before_it_leave() {
        for case in [
            "quarantine_state_file",
            "quarantine_config_file",
            "quarantine_cache_file",
            "quarantine_client_dir",
            "quarantine_temp_symlink",
            "flatten_two",
            "flatten_three",
            "suspend_three_artifacts",
        ] {
            let home = repair_home();
            let lowered = lowered(&home, case, &[kind(&home, case)]);
            replay(case, &lowered);
        }
        // Two fixes over one path (which `doctor fix` refuses, M4-D62): the quarantine of
        // the lifted folder sees it where the flatten leaves it, not where it is now.
        let home = repair_home();
        let fixes = [
            kind(&home, "flatten_two"),
            planned(
                NESTED,
                Fix::Quarantine {
                    path: home.addons("Inner"),
                    root: client_root(&home),
                },
            ),
        ];
        let lowered = lowered(&home, "flatten_then_quarantine", &fixes);
        assert_eq!(lowered.steps.len(), 3, "{}", json(&lowered));
        replay("flatten_then_quarantine", &lowered);
    }

    #[test]
    fn the_reserved_backup_is_the_primitives_seq() {
        let home = repair_home();
        let stray_lua = home.addons("Stray/stray.lua");
        for (case, last, want) in [
            (
                "home_side",
                kind(&home, "write_over"),
                Some(home.run.dir.join("backups/3")),
            ),
            (
                "client_side",
                planned(
                    NESTED,
                    Fix::Write {
                        path: stray_lua.clone(),
                        content: b"-- fixed\n".to_vec(),
                        mode: mode(0o644),
                    },
                ),
                Some(home.client.snapshots_repair(&tx()).join("files/3")),
            ),
            ("creating", kind(&home, "write_create"), None),
        ] {
            let fixes = [
                kind(&home, "chmod"),
                kind(&home, "quarantine_state_file"),
                last,
            ];
            let lowered = lowered(&home, case, &fixes);
            let third = &lowered.steps[2];
            let Primitive::WriteFile { backup, .. } = &third.step.op else {
                let shown = json(&lowered);
                panic!("{case}: the third primitive is the write\n{shown}"); // ubs:ignore — fails the test
            };
            assert_eq!(backup, &want, "{case}\n{}", json(&lowered));
            let size = match case {
                "home_side" => fs::metadata(home.skill_file()).unwrap().len(),
                "client_side" => fs::metadata(&stray_lua).unwrap().len(),
                _ => 0,
            };
            assert_eq!(
                third.backup_bytes, size,
                "{case}: the overwritten file's size"
            );
        }
    }

    #[test]
    fn quarantine_roots_follow_the_victims_side() {
        let home = repair_home();
        let skill = run_quarantine(&home, "skill");
        for (case, victim, root, base, to) in [
            (
                "state",
                home.record(),
                QuarantineRoot::State,
                run_quarantine(&home, "state"),
                format!("targets/{}.json", home.key),
            ),
            (
                "config",
                home.credential(),
                QuarantineRoot::Config,
                run_quarantine(&home, "config"),
                "credentials/github".to_owned(),
            ),
            (
                "cache",
                home.blob(),
                QuarantineRoot::Cache,
                cache_quarantine(&home),
                "blobs/ab".to_owned(),
            ),
            (
                "client",
                home.addons("Stray"),
                client_root(&home),
                client_quarantine(&home),
                "Interface/AddOns/Stray".to_owned(),
            ),
            (
                "skill_dir",
                home.skill_file(),
                QuarantineRoot::Skill,
                skill,
                ".claude/skills/omaddons/SKILL.md".to_owned(),
            ),
        ] {
            let fix = planned(CORRUPT, Fix::Quarantine { path: victim, root });
            let lowered = lowered(&home, case, &[fix]);
            let step = &lowered.steps[0];
            assert_eq!(
                step.step.touched()[1],
                base.join(&to),
                "{case}\n{}",
                json(&lowered)
            );
            assert_eq!(step.quarantine.as_ref(), Some(&base), "{case}: its root");
            assert!(
                home.scopes.is_quarantine(&base.join(&to)),
                "{case}: fsx agrees"
            );
        }
    }

    /// The error an `Err` holds, with its code and a phrase of its message checked.
    fn refused(
        case: &str,
        result: Result<Lowered, LowerError>,
        code: ErrorCode,
        phrase: &str,
    ) -> Error {
        match result {
            Err(LowerError::Error(err)) => {
                assert_eq!(err.code, code, "{case}: {}", err.message);
                assert!(
                    err.message.contains(phrase),
                    "{case}: {:?} does not say {phrase:?}",
                    err.message
                );
                assert!(
                    !err.fix.display().contains("omaddons "),
                    "{case}: a prose fix, naming no command: {}",
                    err.fix.display()
                );
                err
            }
            other => panic!("{case}: expected {code} saying {phrase:?}, got {other:?}"), // ubs:ignore — fails the test
        }
    }

    #[test]
    fn a_root_on_the_wrong_side_is_refused() {
        let home = repair_home();
        let before = tree_hash(&home.base);
        let unplanned = QuarantineRoot::Client {
            client_dir: home.unplanned.client_dir().to_path_buf(),
        };
        for (case, victim, root) in [
            (
                "client_path_state_root",
                home.addons("Stray"),
                QuarantineRoot::State,
            ),
            (
                "state_path_cache_root",
                home.record(),
                QuarantineRoot::Cache,
            ),
            (
                "cache_path_config_root",
                home.blob(),
                QuarantineRoot::Config,
            ),
            (
                "config_path_state_root",
                home.credential(),
                QuarantineRoot::State,
            ),
            (
                "skill_path_state_root",
                home.skill_file(),
                QuarantineRoot::State,
            ),
            (
                "state_path_skill_root",
                home.record(),
                QuarantineRoot::Skill,
            ),
            ("state_path_client_root", home.record(), client_root(&home)),
            (
                "client_path_other_client_root",
                home.addons("Stray"),
                unplanned,
            ),
        ] {
            let fix = planned(CORRUPT, Fix::Quarantine { path: victim, root });
            refused(
                case,
                try_lower(&home, &[fix]),
                ErrorCode::ScopeDenied,
                "must be on its own side",
            );
        }
        assert_eq!(tree_hash(&home.base), before, "nothing was written");
    }

    #[test]
    fn resources_are_the_union_of_the_steps() {
        let home = repair_home();
        let texts = |lowered: &Lowered| -> BTreeSet<String> {
            assert!(lowered.resources.reads().is_empty(), "a repair only writes");
            lowered
                .resources
                .writes()
                .iter()
                .map(|r| r.text().unwrap())
                .collect()
        };
        let a = |rel: &str| home.addons(rel).display().to_string();
        let parked = client_quarantine(&home).join("Interface/AddOns/Wrap");
        let flatten = lowered(&home, "flatten_two", &[kind(&home, "flatten_two")]);
        let want: BTreeSet<String> = [
            format!("{}/", a("Wrap/Inner")),
            format!("{}/", a("Inner")),
            format!("{}/", a("Wrap")),
            format!("{}/", parked.display()),
        ]
        .into();
        assert_eq!(texts(&flatten), want, "flatten_two\n{}", json(&flatten));
        let write = lowered(&home, "write_over", &[kind(&home, "write_over")]);
        let want: BTreeSet<String> = [
            home.skill_file().display().to_string(),
            format!("backup:{REPAIR_RUN_ID}/1"),
        ]
        .into();
        assert_eq!(texts(&write), want, "write_over");
        let both = lowered(
            &home,
            "both",
            &[kind(&home, "write_over"), kind(&home, "flatten_two")],
        );
        let mut union = ResourceSet::new();
        for step in &both.steps {
            union.extend(&step.resources);
        }
        assert_eq!(both.resources, union, "the run's set is its steps' union");
        assert_eq!(
            both.targets,
            vec![home.key],
            "the flatten touches the client"
        );
        assert_eq!(
            both.client_dirs,
            vec![(home.client.client_dir().to_path_buf(), home.key)]
        );
        let home_only = lowered(&home, "write_over", &[kind(&home, "write_over")]);
        assert!(
            home_only.targets.is_empty(),
            "a home-side write touches no client"
        );
    }

    #[test]
    fn a_victim_on_another_mount_is_refused_before_anything() {
        let home = repair_home();
        let meta = home.client.meta_dir();
        let config = home.paths.config_dir().to_path_buf();
        let run = home.run.dir.clone();
        let cache_quarantined = home.paths.cache_dir().join("quarantine");
        fs::create_dir_all(&cache_quarantined).unwrap();
        let before = tree_hash(&home.base);
        let stray = home.addons("Stray");
        // Every path on mount 1 but those below `root`, on `mount`.
        let on = |root: PathBuf, mount: u64| {
            move |p: &Path| Ok::<u64, io::Error>(if p.starts_with(&root) { mount } else { 1 })
        };
        type Seam = Box<dyn Fn(&Path) -> io::Result<u64>>;
        let cases: Vec<(&str, &str, Seam, &str)> = vec![
            (
                "client_dir_vs_meta",
                "quarantine_client_dir",
                Box::new(on(meta, 2)),
                "different mounts",
            ),
            (
                "config_vs_state",
                "quarantine_config_file",
                Box::new(on(config, 3)),
                "different mounts",
            ),
            (
                "state_vs_run",
                "quarantine_state_file",
                Box::new(on(run, 4)),
                "different mounts",
            ),
            (
                "cache_vs_cache_quarantine",
                "quarantine_cache_file",
                Box::new(on(cache_quarantined, 5)),
                "different mounts",
            ),
            (
                "mount_point",
                "quarantine_client_dir",
                Box::new(move |p: &Path| Ok(if p == stray.as_path() { 7 } else { 1 })),
                "is a mount point",
            ),
            (
                "no_mount_id",
                "quarantine_client_dir",
                Box::new(|_: &Path| Err(io::Error::new(io::ErrorKind::Unsupported, "old kernel"))),
                "no mount id",
            ),
            // The rollback root does not exist yet: its deepest folder that does is the run.
            (
                "created_file_rollback",
                "write_create",
                Box::new(on(home.run.dir.clone(), 6)),
                "different mounts",
            ),
        ];
        for (case, fix_case, mount_id, phrase) in cases {
            let fix = kind(&home, fix_case);
            let (_, _, ops) = kinds(&home)
                .into_iter()
                .find(|(name, ..)| *name == fix_case)
                .unwrap();
            let (victim, to) = (ops[0].1.clone(), ops[0].2.clone());
            match lower_in(&home, &home.skill_dirs, &[fix], &*mount_id) {
                Err(LowerError::Refused {
                    reason: RefusedReason::FilesystemMismatch,
                    path,
                    to: refused_to,
                    message,
                }) => {
                    assert_eq!(path, victim, "{case}: the victim");
                    let to =
                        to.or_else(|| Some(run_quarantine(&home, "skill").join("rolled_back/1")));
                    assert_eq!(refused_to, to, "{case}: its destination");
                    assert!(
                        message.contains(phrase),
                        "{case}: {message:?} lacks {phrase:?}"
                    );
                    assert!(message.contains("nothing was moved"), "{case}: {message}");
                }
                other => panic!("{case}: expected filesystem_mismatch, got {other:?}"), // ubs:ignore — fails the test
            }
        }
        assert_eq!(tree_hash(&home.base), before, "nothing was written");
    }

    #[test]
    fn lowering_refuses_what_a_repair_may_never_do() {
        let home = repair_home();
        let wtf = home.client.wtf();
        let skill = home.paths.skill_dir();
        let write = |path: PathBuf| {
            planned(
                SKILL,
                Fix::Write {
                    path,
                    content: b"x".to_vec(),
                    mode: mode(0o644),
                },
            )
        };
        let quarantine =
            |path: PathBuf, root: QuarantineRoot| planned(CORRUPT, Fix::Quarantine { path, root });
        let flatten = |inner: &str, outer: &str| {
            planned(
                NESTED,
                Fix::Flatten {
                    inner: home.addons(inner),
                    outer: home.addons(outer),
                },
            )
        };
        let before = tree_hash(&home.base);
        let cases: Vec<(&str, PlannedFix, ErrorCode, String)> = vec![
            (
                "wtf_path",
                write(wtf.join("Config.wtf")),
                ErrorCode::ScopeDenied,
                "never writes WTF/".into(),
            ),
            (
                "saved_variables_path",
                write(wtf.join(format!("Account/{REPAIR_ACCOUNT}/SavedVariables/Stray.lua"))),
                ErrorCode::ScopeDenied,
                "never writes WTF/".into(),
            ),
            (
                "blizzard_folder",
                quarantine(home.addons("Blizzard_Foo"), client_root(&home)),
                ErrorCode::ProtectedFolder,
                "Blizzard_Foo".into(),
            ),
            (
                "chmod_directory",
                planned(
                    FILE_MODE,
                    Fix::Chmod {
                        path: home.addons("Stray"),
                        mode: mode(0o755),
                    },
                ),
                ErrorCode::ScopeDenied,
                "is a folder, not a regular file".into(),
            ),
            (
                "write_over_a_folder",
                write(home.addons("Stray")),
                ErrorCode::ScopeDenied,
                "is a folder".into(),
            ),
            (
                "unknown_client",
                quarantine(
                    home.unplanned.addons().join("Other"),
                    QuarantineRoot::Client {
                        client_dir: home.unplanned.client_dir().to_path_buf(),
                    },
                ),
                ErrorCode::ScopeDenied,
                "not planned for".into(),
            ),
            (
                "flatten_destination_exists",
                flatten("Dup/Stray", "Dup"),
                ErrorCode::FileExists,
                format!("{} already exists", home.addons("Stray").display()),
            ),
            (
                "flatten_case_variant_exists",
                flatten("Case/STRAY", "Case"),
                ErrorCode::FileExists,
                format!("{} already exists", home.addons("Stray").display()),
            ),
            (
                "flatten_inner_not_a_child",
                flatten("Wrap/Inner", "Same"),
                ErrorCode::Internal,
                "not a direct child".into(),
            ),
            (
                "flatten_outside_addons",
                planned(
                    NESTED,
                    Fix::Flatten {
                        inner: home.blob(),
                        outer: home.paths.blobs_dir(),
                    },
                ),
                ErrorCode::ScopeDenied,
                "lifts addon folders only".into(),
            ),
            (
                "plain_symlink_source",
                quarantine(home.addons("Linked"), client_root(&home)),
                ErrorCode::ScopeDenied,
                "is a symlink".into(),
            ),
            (
                "temp_symlink_outside_own_folders",
                quarantine(skill.join(".omaddons-guide"), QuarantineRoot::Skill),
                ErrorCode::ScopeDenied,
                "only from omaddons' own folders into a quarantine".into(),
            ),
            (
                "relative_path",
                write("relative/SKILL.md".into()),
                ErrorCode::Internal,
                "is not absolute".into(),
            ),
            (
                "dotdot_path",
                write(skill.join("../omaddons/SKILL.md")),
                ErrorCode::Internal,
                "`..`".into(),
            ),
            (
                "create_without_folder",
                write(skill.join("missing/NOTES.md")),
                ErrorCode::Io,
                "does not exist".into(),
            ),
            (
                "create_without_quarantine",
                write(home.paths.omarchy_plugin_dir().join("new.txt")),
                ErrorCode::ScopeDenied,
                "has no quarantine on its side".into(),
            ),
            (
                "outside_every_scope",
                write(home.paths.home().join("elsewhere.txt")),
                ErrorCode::ScopeDenied,
                "outside every write scope".into(),
            ),
            (
                "missing_victim",
                quarantine(
                    home.paths.targets_dir().join("none.json"),
                    QuarantineRoot::State,
                ),
                ErrorCode::Io,
                "nothing is there".into(),
            ),
        ];
        for (case, fix, code, phrase) in cases {
            let err = refused(case, try_lower(&home, &[fix]), code, &phrase);
            assert!(
                !err.message.contains(REPAIR_ACCOUNT),
                "{case}: no account name: {}",
                err.message
            );
        }
        let nine: Vec<PathBuf> = (0..=SKILL_DIRS_MAX)
            .map(|n| home.paths.home().join(format!("agent-{n}/skills/omaddons")))
            .collect();
        let result = lower_in(
            &home,
            &nine,
            &[kind(&home, "write_over")],
            &crate::fsx::mount_id,
        );
        refused(
            "too_many_skill_dirs",
            result,
            ErrorCode::ScopeDenied,
            "at most 8",
        );
        assert_eq!(tree_hash(&home.base), before, "nothing was written");
    }

    #[test]
    fn a_repair_never_writes_wtf() {
        let home = repair_home();
        let wtf = home.client.wtf();
        let config = wtf.join("Config.wtf");
        let through_a_link = home.paths.state_dir().join("to-wtf");
        symlink(&wtf, &through_a_link).unwrap();
        let before = tree_hash(&home.base);
        let mut journal = stuck_journal(&home);
        journal.artifacts = vec![SuspendedArtifact {
            path: wtf.join(format!("Account/{REPAIR_ACCOUNT}/SavedVariables/Stray.lua")),
            root: client_root(&home),
        }];
        journal.guards.clear();
        let cases = [
            (
                "write",
                Fix::Write {
                    path: config.clone(),
                    content: b"x".to_vec(),
                    mode: mode(0o644),
                },
            ),
            (
                "write_lowercase",
                Fix::Write {
                    path: home.client.client_dir().join("wtf/Config.wtf"),
                    content: b"x".to_vec(),
                    mode: mode(0o644),
                },
            ),
            (
                "write_through_a_symlink",
                Fix::Write {
                    path: through_a_link.join("Config.wtf"),
                    content: b"x".to_vec(),
                    mode: mode(0o644),
                },
            ),
            (
                "chmod",
                Fix::Chmod {
                    path: config,
                    mode: mode(0o600),
                },
            ),
            (
                "quarantine",
                Fix::Quarantine {
                    path: wtf.join(format!("Account/{REPAIR_ACCOUNT}")),
                    root: client_root(&home),
                },
            ),
            (
                "flatten",
                Fix::Flatten {
                    inner: wtf.join(format!("Account/{REPAIR_ACCOUNT}")),
                    outer: wtf.join("Account"),
                },
            ),
            ("suspend", Fix::Suspend { journal }),
        ];
        for (case, fix) in cases {
            let err = refused(
                case,
                try_lower(&home, &[planned(CORRUPT, fix)]),
                ErrorCode::ScopeDenied,
                "never writes WTF/",
            );
            assert!(
                !err.message.contains(REPAIR_ACCOUNT),
                "{case}: no account name: {}",
                err.message
            );
        }
        assert_eq!(
            tree_hash(&home.base),
            before,
            "no root, journal or backup was made"
        );
    }

    #[test]
    fn a_chmod_to_the_mode_a_file_has_is_dropped() {
        let home = repair_home();
        let fix = planned(
            FILE_MODE,
            Fix::Chmod {
                path: home.credential(),
                mode: mode(0o644),
            },
        );
        let lowered = lowered(
            &home,
            "same_mode",
            &[fix.clone(), kind(&home, "write_over")],
        );
        assert_eq!(lowered.fixes.len(), 2, "the chmod's fix stays");
        let ops: Vec<Op> = lowered.steps.iter().map(|s| op_of(&s.step)).collect();
        assert_eq!(
            ops,
            vec![("write_file", home.skill_file(), None)],
            "{}",
            json(&lowered)
        );
        assert_eq!(
            lowered.steps[0].step.fix, 2,
            "the write serves the second fix"
        );
    }

    #[test]
    fn lowering_is_deterministic() {
        let home = repair_home();
        for (case, fix, _) in kinds(&home) {
            let fixes = [fix];
            assert_eq!(
                lowered(&home, case, &fixes),
                lowered(&home, case, &fixes),
                "{case}"
            );
        }
        let all: Vec<PlannedFix> = [
            "write_over",
            "chmod",
            "flatten_two",
            "suspend_three_artifacts",
        ]
        .into_iter()
        .map(|case| kind(&home, case))
        .collect();
        assert_eq!(
            lowered(&home, "all", &all),
            lowered(&home, "all", &all),
            "a whole repair"
        );
    }

    #[test]
    fn lowering_writes_nothing() {
        let home = repair_home();
        let before = tree_hash(&home.base);
        for (case, fix, _) in kinds(&home) {
            lowered(&home, case, &[fix]);
            assert_eq!(tree_hash(&home.base), before, "{case} wrote");
        }
    }

    /// Lowering plans; the commit writes. Nothing here may reach fsx's writer, or write
    /// through `std::fs` around it.
    #[test]
    fn lower_rs_never_writes() {
        use crate::source_scan::{Literals, production_code};
        const WRITES: [&str; 16] = [
            "WriteSession",
            "mutate(",
            "mutate_with(",
            "ensure_dir_all(",
            "prune(",
            "Op::",
            "OpenOptions",
            "InflightLock",
            "fs::write",
            "fs::create_dir",
            "fs::remove",
            "fs::rename",
            "fs::copy",
            "fs::hard_link",
            "set_permissions",
            "File::create",
        ];
        let writes =
            |code: &str| -> Vec<&str> { WRITES.into_iter().filter(|w| code.contains(w)).collect() };
        let code = production_code(include_str!("lower.rs"), Literals::Blank).unwrap();
        assert_eq!(writes(&code), Vec::<&str>::new(), "lower.rs writes");
        let planted =
            production_code("fn f() { std::fs::rename(a, b); }", Literals::Blank).unwrap();
        assert_eq!(writes(&planted), ["fs::rename"], "the scan sees one");
    }
}
