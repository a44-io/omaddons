//! `journal::diagnose` (E11): what recovery will do with each interrupted journal, read
//! only. It runs recovery's own decision procedure ([`journal::route`],
//! [`journal::walk_back`], [`journal::walk_back_wtf`], [`journal::candidates`] and
//! preservation's checks) against a simulated tree that applies each inverse rename in
//! memory, so the doctor and `status` never classify a journal with logic of their own
//! that could drift from recovery.
//!
//! The tree reads each path of the disk (a [`Base`]) at most once per call and answers
//! every later query through the renames simulated so far. Journals are simulated in
//! recovery's order on one shared tree, as recovery leaves the disk for the next journal;
//! a journal whose writer is alive leaves the tree as it was, as recovery skips it.
//!
//! It never writes, takes no lock and never fails: what it cannot read is a verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::journal::{
    self, Disk, FileId, LiveEntry, LiveKind, LiveView, Parsed, Phase, Prepared, Replay, Route,
    StepOp, StepRecord, StopReason, Unrecoverable, Walked, WtfWalked,
};
use super::preserve::{self, Placed};
use crate::backup::BackupMeta;
use crate::error::ErrorCode;
use crate::fsx::{InflightLock, ScopeTable, StampDigest};
use crate::model::{BackupId, TargetKey, TxId, TxKind};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store::{self, SchemaVersion};

// ---------------------------------------------------------------------------------------
// The API.

/// Every journal recovery would meet, diagnosed, in recovery's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnoses {
    pub journals: Vec<JournalDiagnosis>,
    /// The journal directory exists and cannot be listed: recovery then fails every
    /// committing command (`unattributed`), so it is never read as "no journals".
    pub unlisted: Option<ListingFailure>,
}

/// Why the journal directory could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListingFailure {
    pub dir: PathBuf,
    pub error: String,
}

/// One listed journal and its text, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listed {
    pub tx: TxId,
    pub path: PathBuf,
    pub text: Result<String, String>,
}

/// Which kind of journal it is. E5 adds repair journals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalFamily {
    /// A folder or settings transaction (`journal/<tx>.jsonl`, `<tx>.wtf.jsonl`).
    Folder,
}

/// What recovery will do with one journal, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalDiagnosis {
    pub tx: TxId,
    pub family: JournalFamily,
    pub path: PathBuf,
    /// The `prepared` line's target, read even when that line does not decode.
    pub target: Option<TargetKey>,
    /// The `prepared` line's client dir, when that line decodes.
    pub client_dir: Option<PathBuf>,
    pub phase: Phase,
    /// Its writer holds its in-flight lock: recovery skips it.
    pub live: bool,
    pub verdict: JournalVerdict,
    pub evidence: JournalEvidence,
    pub protected: ProtectedPaths,
}

/// What recovery's decision procedure concludes for a journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalVerdict {
    /// Recovery settles it: rolls it forward or back, or only deletes it.
    Recoverable { phase: Phase, direction: Direction },
    /// A settings restore that waits for its game to exit.
    WaitsForGame,
    /// Started by a newer major version; its target is known.
    TooNew,
    /// A complete line this build cannot read; its target is known.
    Unreadable,
    /// A settings restore's step recovery cannot place: it stops there.
    UnexpectedState { step: u32 },
    /// A rollback's step in neither its before nor its after state: it stops there.
    StepUnrecognized { step: u32 },
    /// Its client no longer carries its target's marker.
    MarkerMismatch,
    /// What a settings restore displaced has no durable home yet: recovery finishes the
    /// preservation (`stops: false`) or stops (`stops: true`).
    PreservationIncomplete { stops: bool },
    /// A failure no single target owns: it fails every committing command.
    Unattributed { cause: UnattributedCause },
}

/// Why a journal's failure cannot be attributed to one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnattributedCause {
    /// Its in-flight lock cannot be probed.
    LivenessUnknown,
    /// Its file cannot be read.
    ReadFailed,
    /// A newer major version, with no readable target.
    TooNew,
    /// A line this build cannot read, with no readable target.
    Unreadable,
}

/// Which way recovery settles a journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Forward,
    Back,
    /// Nothing to finish or undo: it only deletes what the transaction left.
    Prune,
}

/// What recovery is expected to do with a journal: the prediction E11's differential
/// test holds recovery to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Expected {
    RolledForward,
    RolledBack,
    Pruned,
    /// Its writer is alive.
    Skipped,
    /// It stops with this error code and `details.reason`, at this step when it names one.
    Stops {
        code: ErrorCode,
        reason: StopReason,
        step: Option<u32>,
    },
}

impl JournalDiagnosis {
    /// What recovery is expected to do with the journal.
    pub fn expected(&self) -> Expected {
        if self.live {
            return Expected::Skipped;
        }
        let stops = |code, reason, step| Expected::Stops { code, reason, step };
        match self.verdict {
            JournalVerdict::Unattributed {
                cause: UnattributedCause::LivenessUnknown | UnattributedCause::ReadFailed,
            } => stops(ErrorCode::Io, StopReason::Unattributed, None),
            JournalVerdict::Unattributed {
                cause: UnattributedCause::TooNew,
            } => stops(ErrorCode::StateTooNew, StopReason::Unattributed, None),
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            } => stops(ErrorCode::StateCorrupt, StopReason::Unattributed, None),
            JournalVerdict::TooNew => {
                stops(ErrorCode::StateTooNew, StopReason::JournalTooNew, None)
            }
            JournalVerdict::Unreadable => {
                stops(ErrorCode::StateCorrupt, StopReason::JournalUnreadable, None)
            }
            JournalVerdict::MarkerMismatch => {
                stops(ErrorCode::ScopeDenied, StopReason::MarkerMismatch, None)
            }
            JournalVerdict::WaitsForGame => {
                stops(ErrorCode::GameRunning, StopReason::SavedVariables, None)
            }
            JournalVerdict::Recoverable { direction, .. } => match direction {
                Direction::Prune => Expected::Pruned,
                Direction::Forward => Expected::RolledForward,
                Direction::Back => Expected::RolledBack,
            },
            JournalVerdict::StepUnrecognized { step } => stops(
                ErrorCode::Internal,
                StopReason::StepUnrecognized,
                Some(step),
            ),
            JournalVerdict::UnexpectedState { step } => {
                stops(ErrorCode::Internal, StopReason::UnexpectedState, Some(step))
            }
            JournalVerdict::PreservationIncomplete { stops: false } => Expected::RolledBack,
            JournalVerdict::PreservationIncomplete { stops: true } => stops(
                ErrorCode::StateCorrupt,
                StopReason::PreservationIncomplete,
                None,
            ),
        }
    }

    /// The way recovery settles it, when it does.
    pub fn direction(&self) -> Option<Direction> {
        match self.verdict {
            JournalVerdict::Recoverable { direction, .. } => Some(direction),
            JournalVerdict::PreservationIncomplete { stops: false } => Some(Direction::Back),
            _ => None,
        }
    }

    /// Whether the prediction holds without an approximation.
    pub fn exact(&self) -> bool {
        self.evidence.approximations.is_empty()
    }
}

/// What the diagnosis rests on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct JournalEvidence {
    /// The raw `prepared` line's version.
    pub version: Option<SchemaVersion>,
    pub kind: Option<TxKind>,
    pub writes_wtf: bool,
    /// Steps recorded.
    pub steps: usize,
    /// The steps the simulated walk reversed, ascending.
    pub ran: Vec<u32>,
    pub torn_tail: bool,
    pub unknown_lines: usize,
    /// What the client dir's marker names now.
    pub marker: Option<TargetKey>,
    /// Settings-restore steps left as found whose displaced side is kept.
    pub displaced: usize,
    pub preservation: Option<Preservation>,
    pub artifacts: Vec<Artifact>,
    pub approximations: Vec<Approximation>,
}

/// A settings restore's preservation, as recovery would carry it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Preservation {
    /// The journaled intent's backup id; `None` for an intent recovery would write.
    pub intent: Option<BackupId>,
    pub items: usize,
    /// An intent is journaled and not yet `preserved`.
    pub pending: bool,
}

/// Something the transaction left, found by its id where omaddons puts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: ArtifactKind,
    /// The client dir it is in; `None` on the state side.
    pub client: Option<PathBuf>,
    /// Whether that client's marker names the journal's target; `None` when the target is
    /// unknown or the artifact is state-side.
    pub marker_matches: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Journal,
    Inflight,
    StateSnapshot,
    ClientStage,
    ClientSnapshot,
}

/// Where the prediction may differ from recovery, by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Approximation {
    /// Preservation would run; its describe walk's bounds are not simulated.
    PreservationWalk,
    /// A tree's stamp was read on its origin while a simulated move lay inside it.
    CompositeTreeStamp,
    /// A journal without `writes_wtf` names a path below its client's `WTF/`, which
    /// recovery's writer may not rename; that refusal is not simulated.
    WtfPathWithoutFlag,
}

/// What recovery may touch for a journal: what a doctor must keep and fence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProtectedPaths {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
}

/// Diagnoses every journal of `paths`, reading the disk. `client_dirs` are the clients the
/// caller knows, where a transaction's artifacts are looked for; `sv_writable` is the
/// predicate the caller's recovery would pass. Never fails, writes nothing, takes no lock.
pub fn diagnose(
    paths: &Paths,
    client_dirs: &[PathBuf],
    sv_writable: &dyn Fn(&Path) -> bool,
) -> Diagnoses {
    let (found, unlisted) = match journal::list(paths) {
        Ok(found) => (found, None),
        Err(error) => (
            Vec::new(),
            Some(ListingFailure {
                dir: paths.journal_dir(),
                error: error.message.clone(),
            }),
        ),
    };
    let listing = found
        .into_iter()
        .map(|(tx, path)| Listed {
            text: std::fs::read(&path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| format!("{}: {e}", path.display())),
            tx,
            path,
        })
        .collect();
    let base = DiskBase::new(paths);
    Diagnoses {
        journals: diagnose_journals(&base, paths, listing, client_dirs, sv_writable),
        unlisted,
    }
}

/// The pure core of [`diagnose`]: the same procedure over any base, journal by journal on
/// one simulated tree. Unit tests and the fuzz target call it with a [`MapBase`].
pub fn diagnose_journals<B: Base>(
    base: &B,
    paths: &Paths,
    listing: Vec<Listed>,
    client_dirs: &[PathBuf],
    sv_writable: &dyn Fn(&Path) -> bool,
) -> Vec<JournalDiagnosis> {
    let mut sim = Simulated::new(base);
    listing
        .into_iter()
        .map(|listed| diagnose_one(&mut sim, paths, listed, client_dirs, sv_writable))
        .collect()
}

// ---------------------------------------------------------------------------------------
// The base: what the simulation reads.

/// What the simulation reads besides recovery's step reads.
pub trait Base {
    /// What is at `path`, never following a final symlink.
    fn entry(&self, path: &Path) -> Option<LiveEntry>;

    /// Whether a directory is at `path`, as recovery reads it (`Path::is_dir`).
    fn is_dir(&self, path: &Path) -> bool {
        matches!(
            self.entry(path),
            Some(LiveEntry {
                kind: LiveKind::Dir,
                ..
            })
        )
    }

    /// `journal::file_stamp` of the entry at `path`.
    fn file_stamp(&self, path: &Path) -> Option<String>;

    /// `StampDigest::compute(path)` as text.
    fn tree_stamp(&self, path: &Path) -> std::io::Result<String>;

    /// Whether recovery may write `prepared`'s client dir: its marker names the target,
    /// and the dir can be scoped.
    fn owns_client_dir(&self, prepared: &Prepared) -> bool;

    /// Whether `tx`'s writer holds its in-flight lock; why that cannot be told.
    fn writer_alive(&self, tx: &TxId) -> Result<bool, String>;

    /// Whether a backup of what `prepared` displaced can be described.
    fn describable(&self, prepared: &Prepared) -> bool;

    /// Whether the backup at `dir` has metadata that loads.
    fn backup_meta_loads(&self, dir: &Path) -> bool;

    /// The target the client dir's marker names.
    fn marker(&self, client_dir: &Path) -> Option<TargetKey>;
}

/// The disk, read only, as recovery reads it.
pub struct DiskBase {
    paths: Paths,
    /// A scratch scope table, so the ownership test is recovery's own (`journal_scopes`).
    scopes: Option<ScopeTable>,
}

impl DiskBase {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            scopes: ScopeTable::new(paths, &[], None).ok(),
        }
    }
}

impl Base for DiskBase {
    fn entry(&self, path: &Path) -> Option<LiveEntry> {
        Disk.entry(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        Disk.is_dir(path)
    }

    fn file_stamp(&self, path: &Path) -> Option<String> {
        journal::file_stamp(path)
    }

    fn tree_stamp(&self, path: &Path) -> std::io::Result<String> {
        StampDigest::compute(path).map(|digest| digest.to_string())
    }

    fn owns_client_dir(&self, prepared: &Prepared) -> bool {
        // ubs:ignore — comparing target keys (public ULIDs), not secrets
        self.marker(&prepared.client_dir) == Some(prepared.target_key)
            && match &self.scopes {
                Some(scopes) => scopes.clone().add_client_dir(&prepared.client_dir).is_ok(),
                None => prepared.client_dir.is_absolute(),
            }
    }

    fn writer_alive(&self, tx: &TxId) -> Result<bool, String> {
        InflightLock::is_live(&self.paths, tx).map_err(|error| error.message.clone())
    }

    fn describable(&self, prepared: &Prepared) -> bool {
        preserve::described_client(prepared).is_ok()
    }

    fn backup_meta_loads(&self, dir: &Path) -> bool {
        matches!(
            store::load::<BackupMeta>(&crate::backup::meta_file(dir)),
            Ok(Some(_))
        )
    }

    fn marker(&self, client_dir: &Path) -> Option<TargetKey> {
        crate::discover::registry::read_marker(client_dir)
    }
}

/// A base held in maps, for unit tests and the fuzz target: what is not in a map is
/// absent (an entry, a stamp, a marker), dead (a writer), or fails (a tree stamp, a
/// description, a backup's metadata).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MapBase {
    pub entries: BTreeMap<PathBuf, LiveEntry>,
    pub file_stamps: BTreeMap<PathBuf, String>,
    /// A tree's stamp, or the error its walk fails with.
    pub tree_stamps: BTreeMap<PathBuf, Result<String, String>>,
    /// Client dir → the target its marker names.
    pub markers: BTreeMap<PathBuf, TargetKey>,
    /// A writer that holds its lock (`Ok(true)`), or one that cannot be probed.
    pub alive: BTreeMap<TxId, Result<bool, String>>,
    /// Transactions whose displaced versions can be described.
    pub describable: BTreeSet<TxId>,
    /// Backups whose metadata loads.
    pub backup_metas: BTreeSet<PathBuf>,
}

impl Base for MapBase {
    fn entry(&self, path: &Path) -> Option<LiveEntry> {
        self.entries.get(path).copied()
    }

    fn file_stamp(&self, path: &Path) -> Option<String> {
        self.file_stamps.get(path).cloned()
    }

    fn tree_stamp(&self, path: &Path) -> std::io::Result<String> {
        match self.tree_stamps.get(path) {
            Some(Ok(stamp)) => Ok(stamp.clone()),
            Some(Err(why)) => Err(std::io::Error::other(why.clone())),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no stamp for {}", path.display()),
            )),
        }
    }

    fn owns_client_dir(&self, prepared: &Prepared) -> bool {
        prepared.client_dir.is_absolute()
            // ubs:ignore — comparing target keys (public ULIDs), not secrets
            && self.marker(&prepared.client_dir) == Some(prepared.target_key)
    }

    fn writer_alive(&self, tx: &TxId) -> Result<bool, String> {
        self.alive.get(tx).cloned().unwrap_or(Ok(false))
    }

    fn describable(&self, prepared: &Prepared) -> bool {
        self.describable.contains(&prepared.tx)
    }

    fn backup_meta_loads(&self, dir: &Path) -> bool {
        self.backup_metas.contains(dir)
    }

    fn marker(&self, client_dir: &Path) -> Option<TargetKey> {
        self.markers.get(client_dir).copied()
    }
}

/// A base read at most once per question and path in one call, and counted.
pub(crate) struct Cached<'b, B: Base> {
    base: &'b B,
    entries: BTreeMap<PathBuf, Option<LiveEntry>>,
    dirs: BTreeMap<PathBuf, bool>,
    file_stamps: BTreeMap<PathBuf, Option<String>>,
    tree_stamps: BTreeMap<PathBuf, Result<String, (std::io::ErrorKind, String)>>,
    owns: BTreeMap<(PathBuf, TargetKey), bool>,
    alive: BTreeMap<TxId, Result<bool, String>>,
    describable: BTreeMap<TxId, bool>,
    metas: BTreeMap<PathBuf, bool>,
    markers: BTreeMap<PathBuf, Option<TargetKey>>,
    reads: usize,
}

impl<'b, B: Base> Cached<'b, B> {
    fn new(base: &'b B) -> Self {
        Self {
            base,
            entries: BTreeMap::new(),
            dirs: BTreeMap::new(),
            file_stamps: BTreeMap::new(),
            tree_stamps: BTreeMap::new(),
            owns: BTreeMap::new(),
            alive: BTreeMap::new(),
            describable: BTreeMap::new(),
            metas: BTreeMap::new(),
            markers: BTreeMap::new(),
            reads: 0,
        }
    }

    /// How many questions reached the base.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.reads
    }

    fn entry(&mut self, path: &Path) -> Option<LiveEntry> {
        let (base, reads) = (self.base, &mut self.reads);
        *self.entries.entry(path.to_path_buf()).or_insert_with(|| {
            *reads += 1;
            base.entry(path)
        })
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        let (base, reads) = (self.base, &mut self.reads);
        *self.dirs.entry(path.to_path_buf()).or_insert_with(|| {
            *reads += 1;
            base.is_dir(path)
        })
    }

    fn file_stamp(&mut self, path: &Path) -> Option<String> {
        let (base, reads) = (self.base, &mut self.reads);
        self.file_stamps
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                *reads += 1;
                base.file_stamp(path)
            })
            .clone()
    }

    fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String> {
        let (base, reads) = (self.base, &mut self.reads);
        self.tree_stamps
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                *reads += 1;
                base.tree_stamp(path).map_err(|e| (e.kind(), e.to_string()))
            })
            .clone()
            .map_err(|(kind, why)| std::io::Error::new(kind, why))
    }

    fn owns_client_dir(&mut self, prepared: &Prepared) -> bool {
        let key = (prepared.client_dir.clone(), prepared.target_key);
        let (base, reads) = (self.base, &mut self.reads);
        *self.owns.entry(key).or_insert_with(|| {
            *reads += 1;
            base.owns_client_dir(prepared)
        })
    }

    fn writer_alive(&mut self, tx: &TxId) -> Result<bool, String> {
        let (base, reads) = (self.base, &mut self.reads);
        self.alive
            .entry(*tx)
            .or_insert_with(|| {
                *reads += 1;
                base.writer_alive(tx)
            })
            .clone()
    }

    fn describable(&mut self, prepared: &Prepared) -> bool {
        let (base, reads) = (self.base, &mut self.reads);
        *self.describable.entry(prepared.tx).or_insert_with(|| {
            *reads += 1;
            base.describable(prepared)
        })
    }

    fn backup_meta_loads(&mut self, dir: &Path) -> bool {
        let (base, reads) = (self.base, &mut self.reads);
        *self.metas.entry(dir.to_path_buf()).or_insert_with(|| {
            *reads += 1;
            base.backup_meta_loads(dir)
        })
    }

    fn marker(&mut self, client_dir: &Path) -> Option<TargetKey> {
        let (base, reads) = (self.base, &mut self.reads);
        *self
            .markers
            .entry(client_dir.to_path_buf())
            .or_insert_with(|| {
                *reads += 1;
                base.marker(client_dir)
            })
    }
}

// ---------------------------------------------------------------------------------------
// The simulated tree.

/// One simulated rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Move {
    /// What was at `from` is now at `to`, and nothing is at `from`.
    Rename { from: PathBuf, to: PathBuf },
    /// `a` and `b` swapped.
    Exchange { a: PathBuf, b: PathBuf },
}

/// The disk as recovery would leave it: a cached base, and the renames simulated on it.
pub(crate) struct Simulated<'b, B: Base> {
    cache: Cached<'b, B>,
    moves: Vec<Move>,
    /// The steps reversed since the driver last cleared it.
    reversed: Vec<u32>,
    /// A tree stamp was read over a moved subtree since the driver last cleared it.
    composite: bool,
}

impl<'b, B: Base> Simulated<'b, B> {
    pub(crate) fn new(base: &'b B) -> Self {
        Self {
            cache: Cached::new(base),
            moves: Vec::new(),
            reversed: Vec::new(),
            composite: false,
        }
    }

    pub(crate) fn checkpoint(&self) -> usize {
        self.moves.len()
    }

    /// Forgets the moves made since `checkpoint`.
    pub(crate) fn restore(&mut self, checkpoint: usize) {
        self.moves.truncate(checkpoint);
    }

    pub(crate) fn apply(&mut self, applied: Move) {
        self.moves.push(applied);
    }

    /// Where what is at `path` now was in the base, or `None` when the moves left
    /// nothing there. Latest move first: below a rename's `to` is what was below its
    /// `from`; below its `from` is nothing; below one side of an exchange is what was
    /// below the other. Anything else, an ancestor of a moved path included, is
    /// unchanged.
    fn origin(&self, path: &Path) -> Option<PathBuf> {
        let mut at = path.to_path_buf();
        for done in self.moves.iter().rev() {
            match done {
                Move::Rename { from, to } => {
                    if let Ok(rest) = at.strip_prefix(to) {
                        at = joined(from, rest);
                    } else if at.starts_with(from) {
                        return None;
                    }
                }
                Move::Exchange { a, b } => {
                    if let Ok(rest) = at.strip_prefix(a) {
                        at = joined(b, rest);
                    } else if let Ok(rest) = at.strip_prefix(b) {
                        at = joined(a, rest);
                    }
                }
            }
        }
        Some(at)
    }

    /// Whether a simulated move lies strictly inside `path`'s subtree.
    fn moved_inside(&self, path: &Path) -> bool {
        let inside = |p: &Path| p != path && p.starts_with(path);
        self.moves.iter().any(|done| match done {
            Move::Rename { from, to } => inside(from) || inside(to),
            Move::Exchange { a, b } => inside(a) || inside(b),
        })
    }
}

/// `base` with `rest` below it, never with a trailing separator.
fn joined(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

impl<B: Base> LiveView for Simulated<'_, B> {
    fn entry(&mut self, path: &Path) -> Option<LiveEntry> {
        let origin = self.origin(path)?;
        self.cache.entry(&origin)
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        match self.origin(path) {
            Some(origin) => self.cache.is_dir(&origin),
            None => false,
        }
    }

    fn file_stamp(&mut self, path: &Path) -> Option<String> {
        let origin = self.origin(path)?;
        self.cache.file_stamp(&origin)
    }

    /// Exact when no move lies strictly inside `path`'s subtree, as for every journal
    /// shape the engine writes; otherwise the origin's stamp, flagged.
    fn tree_stamp(&mut self, path: &Path) -> std::io::Result<String> {
        if self.moved_inside(path) {
            self.composite = true;
        }
        match self.origin(path) {
            Some(origin) => self.cache.tree_stamp(&origin),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{}: nothing there", path.display()),
            )),
        }
    }
}

impl<B: Base> Replay for Simulated<'_, B> {
    fn reverse(&mut self, step: &StepRecord) -> crate::error::Result<()> {
        self.moves.push(match step.op {
            StepOp::Exchange => Move::Exchange {
                a: step.from.clone(),
                b: step.to.clone(),
            },
            StepOp::Noreplace | StepOp::Rename => Move::Rename {
                from: step.to.clone(),
                to: step.from.clone(),
            },
        });
        self.reversed.push(step.n);
        Ok(())
    }

    fn stepped(&mut self, _: &StepRecord) -> crate::error::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// The driver.

/// One journal, on the shared tree.
fn diagnose_one<B: Base>(
    sim: &mut Simulated<'_, B>,
    paths: &Paths,
    listed: Listed,
    client_dirs: &[PathBuf],
    sv_writable: &dyn Fn(&Path) -> bool,
) -> JournalDiagnosis {
    let Listed { tx, path, text } = listed;
    let alive = sim.cache.writer_alive(&tx);
    let parsed = text.as_ref().ok().map(|text| journal::parse(text));
    let checkpoint = sim.checkpoint();
    sim.reversed.clear();
    sim.composite = false;
    let mut evidence = JournalEvidence::default();
    // Whether recovery may write the client dir: asked only of a journal this build can
    // read, as recovery asks it.
    let owns = match &parsed {
        Some(parsed) => match &parsed.prepared {
            Some(prepared) if parsed.unrecoverable().is_none() => {
                sim.cache.owns_client_dir(prepared)
            }
            _ => false,
        },
        None => false,
    };
    let verdict = match (&alive, &parsed) {
        (Err(_), _) => JournalVerdict::Unattributed {
            cause: UnattributedCause::LivenessUnknown,
        },
        (Ok(_), None) => JournalVerdict::Unattributed {
            cause: UnattributedCause::ReadFailed,
        },
        (Ok(_), Some(parsed)) => verdict(sim, parsed, owns, sv_writable, &mut evidence),
    };
    let live = matches!(alive, Ok(true));
    if live {
        sim.restore(checkpoint);
    }
    let mut ran = std::mem::take(&mut sim.reversed);
    ran.sort_unstable();
    evidence.ran = ran;
    if std::mem::take(&mut sim.composite) {
        evidence
            .approximations
            .push(Approximation::CompositeTreeStamp);
    }
    let target = parsed.as_ref().and_then(|p| p.target);
    if let Some(parsed) = &parsed {
        evidence.version = parsed.version;
        evidence.steps = parsed.steps.len();
        evidence.torn_tail = parsed.torn_tail;
        evidence.unknown_lines = parsed.unknown_lines;
        if let Some(prepared) = &parsed.prepared {
            evidence.kind = Some(prepared.kind);
            evidence.writes_wtf = prepared.writes_wtf;
            evidence.marker = sim.cache.marker(&prepared.client_dir);
            if !prepared.writes_wtf && names_wtf_paths(prepared, &parsed.steps) {
                evidence
                    .approximations
                    .push(Approximation::WtfPathWithoutFlag);
            }
        }
    }
    evidence.approximations.sort_unstable();
    evidence.approximations.dedup();
    evidence.artifacts = artifacts(sim, paths, &tx, client_dirs, target);
    let trusted = match parsed.as_ref().and_then(|p| p.prepared.as_ref()) {
        Some(prepared) if owns && trusts(verdict) => Some(prepared),
        _ => None,
    };
    let protected = match (trusted, &parsed) {
        (Some(prepared), Some(parsed)) => {
            trusted_paths(paths, &path, parsed, prepared, &evidence.artifacts)
        }
        _ => untrusted_paths(&evidence.artifacts),
    };
    JournalDiagnosis {
        tx,
        family: JournalFamily::Folder,
        target,
        client_dir: parsed
            .as_ref()
            .and_then(|p| p.prepared.as_ref())
            .map(|p| p.client_dir.clone()),
        phase: parsed.as_ref().map_or(Phase::Unprepared, |p| p.phase),
        path: cut_below_wtf(&path),
        live,
        verdict,
        evidence,
        protected,
    }
}

/// The verdict for a journal that reads, by recovery's own route and walks.
fn verdict<B: Base>(
    sim: &mut Simulated<'_, B>,
    parsed: &Parsed,
    owns: bool,
    sv_writable: &dyn Fn(&Path) -> bool,
    evidence: &mut JournalEvidence,
) -> JournalVerdict {
    match journal::route(parsed, owns, sv_writable) {
        Route::Unrecoverable(why) => match (parsed.target, why) {
            // No target to attribute it to: recovery fails every committing command.
            (None, Unrecoverable::TooNew) => JournalVerdict::Unattributed {
                cause: UnattributedCause::TooNew,
            },
            (None, Unrecoverable::Unreadable) => JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
            (Some(_), Unrecoverable::TooNew) => JournalVerdict::TooNew,
            (Some(_), Unrecoverable::Unreadable) => JournalVerdict::Unreadable,
        },
        Route::MarkerMismatch(_) => JournalVerdict::MarkerMismatch,
        Route::WaitsForGame(_) => JournalVerdict::WaitsForGame,
        Route::Prune => JournalVerdict::Recoverable {
            phase: parsed.phase,
            direction: Direction::Prune,
        },
        Route::Forward(_) => JournalVerdict::Recoverable {
            phase: Phase::Swapped,
            direction: Direction::Forward,
        },
        Route::Back(_) => {
            let walked = journal::walk_back(sim, &parsed.steps);
            debug_assert!(walked.is_ok(), "a simulated walk never fails");
            match walked {
                Ok(Walked::StoppedAt(step)) => JournalVerdict::StepUnrecognized { step: step.n },
                Ok(Walked::Done) | Err(_) => JournalVerdict::Recoverable {
                    phase: Phase::Prepared,
                    direction: Direction::Back,
                },
            }
        }
        Route::BackPreserving(prepared) => preserving(sim, parsed, prepared, evidence),
    }
}

/// A settings restore's rollback, as `roll_back_preserving` runs it: a journaled intent
/// is resumed without a walk; otherwise the walk, then rule 3's candidates.
fn preserving<B: Base>(
    sim: &mut Simulated<'_, B>,
    parsed: &Parsed,
    prepared: &Prepared,
    evidence: &mut JournalEvidence,
) -> JournalVerdict {
    let back = JournalVerdict::Recoverable {
        phase: Phase::Prepared,
        direction: Direction::Back,
    };
    let pending = parsed
        .preserve
        .iter()
        .find(|intent| !parsed.preserved.contains(&intent.id));
    if let Some(intent) = pending {
        evidence.preservation = Some(Preservation {
            intent: Some(intent.id),
            items: intent.items.len(),
            pending: true,
        });
        evidence
            .approximations
            .push(Approximation::PreservationWalk);
        return JournalVerdict::PreservationIncomplete {
            stops: !resume(sim, parsed, prepared),
        };
    }
    if let Some(intent) = parsed.preserve.last() {
        evidence.preservation = Some(Preservation {
            intent: Some(intent.id),
            items: intent.items.len(),
            pending: false,
        });
        return back;
    }
    let live = ClientPaths::new(&prepared.client_dir).wtf();
    let walked = journal::walk_back_wtf(sim, &live, &parsed.steps);
    debug_assert!(walked.is_ok(), "a simulated walk never fails");
    let displaced = match walked {
        Ok(WtfWalked::Unplaced(step)) => {
            return JournalVerdict::UnexpectedState { step: step.n };
        }
        Ok(WtfWalked::Done(displaced)) => displaced,
        Err(_) => Vec::new(),
    };
    evidence.displaced = displaced.len();
    match journal::candidates(sim, prepared, displaced) {
        Err(_) => JournalVerdict::PreservationIncomplete { stops: true },
        Ok(items) if items.is_empty() => back,
        Ok(items) => {
            evidence.preservation = Some(Preservation {
                intent: None,
                items: items.len(),
                pending: false,
            });
            evidence
                .approximations
                .push(Approximation::PreservationWalk);
            if sim.cache.describable(prepared) {
                back
            } else {
                JournalVerdict::PreservationIncomplete { stops: true }
            }
        }
    }
}

/// `preserve::run` for every intent not yet `preserved`, on the simulated tree: whether
/// each would finish.
fn resume<B: Base>(sim: &mut Simulated<'_, B>, parsed: &Parsed, prepared: &Prepared) -> bool {
    let client = ClientPaths::new(&prepared.client_dir);
    for intent in parsed
        .preserve
        .iter()
        .filter(|intent| !parsed.preserved.contains(&intent.id))
    {
        let published = crate::backup::backup_dir(&client, &intent.id);
        match preserve::published_matches(sim, &published, intent) {
            Some(true) => {
                if !sim.cache.backup_meta_loads(&published) {
                    return false;
                }
            }
            Some(false) => return false,
            None => {
                for item in &intent.items {
                    match preserve::placed(sim, item, &intent.assembly) {
                        Placed::InAssembly => {}
                        Placed::AtFound => sim.apply(Move::Rename {
                            from: item.path.clone(),
                            to: preserve::place(&intent.assembly, &item.rel),
                        }),
                        Placed::Neither => return false,
                    }
                }
                if !sim.cache.describable(prepared) {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether a journal names a path below its client's `WTF/` (in any spelling).
fn names_wtf_paths(prepared: &Prepared, steps: &[StepRecord]) -> bool {
    steps.iter().any(|step| {
        [&step.from, &step.to].iter().any(|path| {
            path.strip_prefix(&prepared.client_dir)
                .ok()
                .and_then(|rest| rest.components().next())
                .is_some_and(|first| first.as_os_str().eq_ignore_ascii_case("WTF"))
        })
    })
}

/// The verdicts whose journal is trusted to name what recovery touches, once its
/// `prepared` line decodes and its client dir is its target's.
fn trusts(verdict: JournalVerdict) -> bool {
    matches!(
        verdict,
        JournalVerdict::Recoverable { .. }
            | JournalVerdict::WaitsForGame
            | JournalVerdict::StepUnrecognized { .. }
            | JournalVerdict::UnexpectedState { .. }
            | JournalVerdict::PreservationIncomplete { .. }
    )
}

/// What `tx` left where omaddons puts a transaction's files, found by its id alone.
fn artifacts<B: Base>(
    sim: &mut Simulated<'_, B>,
    paths: &Paths,
    tx: &TxId,
    client_dirs: &[PathBuf],
    target: Option<TargetKey>,
) -> Vec<Artifact> {
    let mut found = Vec::new();
    let state = [
        (paths.journal(tx), ArtifactKind::Journal),
        (paths.journal_wtf(tx), ArtifactKind::Journal),
        (paths.inflight_lock(tx), ArtifactKind::Inflight),
        (paths.state_snapshot(tx), ArtifactKind::StateSnapshot),
        (paths.state_snapshot_wtf(tx), ArtifactKind::StateSnapshot),
    ];
    for (path, kind) in state {
        if sim.cache.entry(&path).is_some() {
            found.push(Artifact {
                path: cut_below_wtf(&path),
                kind,
                client: None,
                marker_matches: None,
            });
        }
    }
    for dir in client_dirs {
        let client = ClientPaths::new(dir);
        let kinds = [
            (client.stage(tx), ArtifactKind::ClientStage),
            (client.stage_wtf(tx), ArtifactKind::ClientStage),
            (client.snapshots(tx), ArtifactKind::ClientSnapshot),
            (client.snapshots_wtf(tx), ArtifactKind::ClientSnapshot),
        ];
        for (path, kind) in kinds {
            if sim.cache.entry(&path).is_some() {
                let marker_matches = match target {
                    Some(target) => Some(sim.cache.marker(dir) == Some(target)),
                    None => None,
                };
                found.push(Artifact {
                    path: cut_below_wtf(&path),
                    kind,
                    client: Some(dir.clone()),
                    marker_matches,
                });
            }
        }
    }
    found
}

/// What recovery may touch for a journal it trusts: everything the journal names.
fn trusted_paths(
    paths: &Paths,
    journal_path: &Path,
    parsed: &Parsed,
    prepared: &Prepared,
    artifacts: &[Artifact],
) -> ProtectedPaths {
    let tx = &prepared.tx;
    let client = ClientPaths::new(&prepared.client_dir);
    let mut writes = vec![
        journal_path.to_path_buf(),
        paths.inflight_lock(tx),
        prepared.commit.stage_dir.clone(),
    ];
    for step in &parsed.steps {
        writes.push(step.from.clone());
        writes.push(step.to.clone());
    }
    if prepared.puts_settings_back() {
        writes.push(client.snapshots_wtf(tx));
        writes.push(paths.state_snapshot_wtf(tx));
    } else {
        writes.push(client.snapshots(tx));
        writes.push(paths.state_snapshot(tx));
    }
    if matches!(parsed.phase, Phase::Swapped) {
        writes.push(paths.target_record(&prepared.target_key));
        writes.push(paths.files_index(&prepared.target_key));
        writes.push(paths.history_file());
    }
    if prepared.writes_wtf {
        for intent in &parsed.preserve {
            writes.push(intent.assembly.clone());
            writes.push(crate::backup::backup_dir(&client, &intent.id));
        }
    }
    writes.extend(artifacts.iter().map(|artifact| artifact.path.clone()));
    let mut reads = vec![client.marker()];
    if prepared.writes_wtf {
        if let Some(source) = prepared.source_backup {
            reads.push(crate::backup::backup_dir(&client, &source.id));
        }
        if let Some(reverted) = prepared.commit.history.reverts {
            reads.push(
                super::retention::snapshot_of(&client, &reverted)
                    .join(super::retention::WTF_PREVIOUS),
            );
        }
    }
    ProtectedPaths {
        reads: settled(reads),
        writes: settled(writes),
    }
}

/// What recovery may touch for a journal it does not trust: only what is named by its
/// transaction id, never a path its payload names (M4-D38).
fn untrusted_paths(artifacts: &[Artifact]) -> ProtectedPaths {
    ProtectedPaths {
        reads: Vec::new(),
        writes: settled(
            artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
                .collect(),
        ),
    }
}

/// `paths` cut below `WTF/`, sorted and deduplicated.
fn settled(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = paths.iter().map(|path| cut_below_wtf(path)).collect();
    paths.sort();
    paths.dedup();
    paths
}

/// `path` up to its first `WTF` or `WTF.previous` component (common rule 9): below it live
/// account, realm and character names, which a diagnosis never carries.
pub(crate) fn cut_below_wtf(path: &Path) -> PathBuf {
    let mut cut = PathBuf::new();
    for component in path.components() {
        cut.push(component);
        let name = component.as_os_str();
        if name.eq_ignore_ascii_case("WTF")
            || name.eq_ignore_ascii_case(super::retention::WTF_PREVIOUS)
        {
            break;
        }
    }
    cut
}

// ---------------------------------------------------------------------------------------
// The fuzz property (E11): the `journal` target and the seeds' gate test run it.

/// Any bytes as a journal: they parse and diagnose without a panic, the same bytes always
/// diagnose the same, and every diagnosis agrees with the parse and names nothing below
/// `WTF/`. Panics with the violated property; never writes.
#[doc(hidden)]
pub fn fuzz_property(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let parsed = journal::parse(&text);
    let len = data.len();
    assert!(
        parsed.unknown_lines <= parsed.lines,
        "{len} bytes: more unknown lines than lines"
    );
    if parsed.torn_tail {
        let last = text.lines().last().unwrap_or_default();
        assert!(
            !text.ends_with('\n') || serde_json::from_str::<serde_json::Value>(last).is_err(),
            "{len} bytes: a torn tail that is a complete JSON line"
        );
    }
    if parsed.prepared.is_none() {
        assert!(
            matches!(parsed.phase, Phase::Unprepared),
            "{len} bytes: a phase without a prepared line"
        );
    }
    assert_eq!(
        journal::parse(&text),
        parsed,
        "{len} bytes: two parses differ"
    );
    let found = fuzz_diagnosis(data);
    assert_eq!(
        fuzz_diagnosis(data),
        found,
        "{len} bytes: two diagnoses differ"
    );
    let alive = FuzzCase::new(data, &parsed).alive;
    for diagnosis in &found {
        let _ = (
            diagnosis.expected(),
            diagnosis.direction(),
            diagnosis.exact(),
        );
        let agrees = match (diagnosis.verdict, parsed.unrecoverable()) {
            _ if alive.is_err() => matches!(
                diagnosis.verdict,
                JournalVerdict::Unattributed {
                    cause: UnattributedCause::LivenessUnknown
                }
            ),
            (
                JournalVerdict::TooNew
                | JournalVerdict::Unreadable
                | JournalVerdict::Unattributed { .. },
                Some(_),
            ) => true,
            (_, Some(_)) => false,
            (verdict, None) if matches!(parsed.phase, Phase::Committed | Phase::Unprepared) => {
                matches!(
                    verdict,
                    JournalVerdict::Recoverable {
                        direction: Direction::Prune,
                        ..
                    }
                )
            }
            (
                JournalVerdict::StepUnrecognized { step }
                | JournalVerdict::UnexpectedState { step },
                None,
            ) => parsed
                .steps
                .iter()
                // ubs:ignore — comparing step numbers, not secrets
                .any(|recorded| recorded.n == step),
            _ => true,
        };
        assert!(
            agrees,
            "{len} bytes: the verdict {:?} disagrees with the parse",
            diagnosis.verdict
        );
        let named = std::iter::once(&diagnosis.path)
            .chain(&diagnosis.protected.reads)
            .chain(&diagnosis.protected.writes)
            .chain(diagnosis.evidence.artifacts.iter().map(|a| &a.path));
        for path in named {
            assert!(
                cut_below_wtf(path) == *path,
                "{len} bytes: a diagnosis names a path below WTF/"
            );
        }
    }
}

/// The diagnoses [`fuzz_property`] checks: the journal `data` on a base whose every path
/// the journal names is absent, as recorded before or after its step, or something else,
/// each chosen by a hash of `data` and the path.
#[doc(hidden)]
pub fn fuzz_diagnosis(data: &[u8]) -> Vec<JournalDiagnosis> {
    let text = String::from_utf8_lossy(data).into_owned();
    let parsed = journal::parse(&text);
    let case = FuzzCase::new(data, &parsed);
    let paths = Paths::with_roots("/fuzz", "/fuzz/c", "/fuzz/s", "/fuzz/k");
    let tx = parsed
        .prepared
        .as_ref()
        .map_or(TxId::from_ulid(ulid::Ulid::nil()), |p| p.tx);
    let listing = vec![Listed {
        tx,
        path: paths.journal(&tx),
        text: Ok(text),
    }];
    let writable = case.pick("sv_writable", 2) == 0;
    let client_dirs: Vec<PathBuf> = parsed
        .prepared
        .iter()
        .map(|p| p.client_dir.clone())
        .collect();
    diagnose_journals(&case.base, &paths, listing, &client_dirs, &|_| writable)
}

/// `paths`'s journals diagnosed on disk, with a listing of the home taken before and
/// after: `diagnose` must write nothing. The caller has written the journal.
#[doc(hidden)]
pub fn fuzz_disk_property(paths: &Paths) {
    let before = listing(paths.home());
    let _ = diagnose(paths, &[], &|_| false);
    assert_eq!(listing(paths.home()), before, "diagnose changed the disk");
}

/// Every path under `root` with its kind, size, mode and modification time.
fn listing(root: &Path) -> Vec<(PathBuf, String)> {
    use std::os::unix::fs::MetadataExt;
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let path = child.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(path.clone());
            }
            let kind = meta.file_type();
            let shape = format!(
                "{}:{}:{:o}:{}.{}",
                if kind.is_dir() {
                    "d"
                } else if kind.is_symlink() {
                    "l"
                } else {
                    "f"
                },
                meta.size(),
                meta.mode(),
                meta.mtime(),
                meta.mtime_nsec()
            );
            found.push((path, shape));
        }
    }
    found.sort();
    found
}

/// The base and answers one fuzz input diagnoses with.
struct FuzzCase<'d> {
    data: &'d [u8],
    base: MapBase,
    alive: Result<bool, String>,
}

impl<'d> FuzzCase<'d> {
    fn new(data: &'d [u8], parsed: &Parsed) -> Self {
        let mut case = Self {
            data,
            base: MapBase::default(),
            alive: Ok(false),
        };
        let mut other = 0u64;
        let mut place =
            |case: &mut Self, path: &Path, before: Option<FileId>, after: Option<FileId>| {
                if case.base.entries.contains_key(path) {
                    return;
                }
                other += 1;
                let someone = FileId {
                    dev: 0,
                    ino: u64::MAX - other,
                    born_ns: None,
                };
                let id = match case.pick(&path.to_string_lossy(), 4) {
                    0 => None,
                    1 => before,
                    2 => after,
                    _ => Some(someone),
                };
                if let Some(id) = id {
                    case.base.entries.insert(
                        path.to_path_buf(),
                        LiveEntry {
                            kind: LiveKind::Dir,
                            id,
                        },
                    );
                }
            };
        for step in &parsed.steps {
            let after_from = match step.op {
                StepOp::Exchange => step.to_id,
                StepOp::Noreplace | StepOp::Rename => None,
            };
            place(&mut case, &step.from, step.from_id, after_from);
            place(&mut case, &step.to, step.to_id, step.from_id);
        }
        if let Some(prepared) = &parsed.prepared {
            let staged = prepared.commit.stage_dir.join("WTF");
            place(&mut case, &prepared.commit.stage_dir, None, None);
            place(&mut case, &staged, None, None);
            let stamp = match (case.pick("staged_stamp", 3), &prepared.staged_stamp) {
                (0, Some(recorded)) => Ok(recorded.clone()),
                (1, _) => Err("cannot be listed".to_owned()),
                _ => Ok("stamp:other".to_owned()),
            };
            case.base.tree_stamps.insert(staged.clone(), stamp);
            for file in prepared.staged_files.iter().flatten() {
                let path = staged.join(&file.rel);
                place(&mut case, &path, None, None);
                if case.pick(&format!("stamp {}", file.rel), 2) == 0 {
                    case.base.file_stamps.insert(path, file.stamp.clone());
                }
            }
            let client = ClientPaths::new(&prepared.client_dir);
            if let Some(source) = prepared.source_backup {
                let dir = crate::backup::backup_dir(&client, &source.id);
                place(&mut case, &dir, Some(source.dir), None);
            }
            for intent in &parsed.preserve {
                for item in &intent.items {
                    let assembled = preserve::place(&intent.assembly, &item.rel);
                    place(&mut case, &item.path, Some(item.identity), None);
                    place(&mut case, &assembled, Some(item.identity), None);
                }
                let published = crate::backup::backup_dir(&client, &intent.id);
                place(&mut case, &published, None, None);
                if case.pick("backup meta", 2) == 0 {
                    case.base.backup_metas.insert(published);
                }
            }
            match case.pick("marker", 3) {
                0 => {
                    case.base
                        .markers
                        .insert(prepared.client_dir.clone(), prepared.target_key);
                }
                1 => {
                    case.base.markers.insert(
                        prepared.client_dir.clone(),
                        TargetKey::from_ulid(ulid::Ulid(1)),
                    );
                }
                _ => {}
            }
            if case.pick("describable", 2) == 0 {
                case.base.describable.insert(prepared.tx);
            }
            case.alive = match case.pick("alive", 4) {
                0 => Ok(true),
                1 => Err("cannot be probed".to_owned()),
                _ => Ok(false),
            };
            case.base.alive.insert(prepared.tx, case.alive.clone());
        }
        case
    }

    /// One of `choices` values, by an FNV-1a hash of the input and `what`.
    fn pick(&self, what: &str, choices: u64) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.data.iter().chain(what.as_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash % choices
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use jiff::Timestamp;

    use super::*;
    use crate::engine::journal::{
        CommitPayload, FileId, IndexChanges, JournalLine, PreserveIntent, PreserveItem, Preserved,
        SourceBackup, StagedFile, StepPurpose,
    };
    use crate::model::Actor;
    use crate::state::history::{self, HistoryEntry, TxResult};
    use crate::state::records::TargetRecord;

    fn paths() -> Paths {
        Paths::with_roots("/sim", "/sim/c", "/sim/s", "/sim/k")
    }

    fn client() -> PathBuf {
        PathBuf::from("/sim/_retail_")
    }

    /// A disk held in a [`MapBase`] that renames as the engine does, for building the
    /// state a journal left: each new entry gets a fresh identity, and a rename moves
    /// the subtree with its stamps.
    struct World {
        base: MapBase,
        next: u64,
    }

    impl World {
        fn new() -> Self {
            Self {
                base: MapBase::default(),
                next: 100,
            }
        }

        fn put(&mut self, path: &Path, kind: LiveKind) -> FileId {
            self.next += 1;
            let id = FileId {
                dev: 7,
                ino: self.next,
                born_ns: Some(i64::try_from(self.next).unwrap() * 1_000),
            };
            self.base
                .entries
                .insert(path.to_path_buf(), LiveEntry { kind, id });
            id
        }

        fn dir(&mut self, path: &Path) -> FileId {
            self.put(path, LiveKind::Dir)
        }

        fn file(&mut self, path: &Path, stamp: &str) -> FileId {
            self.base
                .file_stamps
                .insert(path.to_path_buf(), stamp.to_owned());
            self.put(path, LiveKind::File)
        }

        fn id(&self, path: &Path) -> Option<FileId> {
            self.base.entries.get(path).map(|entry| entry.id)
        }

        fn remove(&mut self, path: &Path) {
            self.base.entries.retain(|p, _| !p.starts_with(path));
            self.base.file_stamps.retain(|p, _| !p.starts_with(path));
            self.base.tree_stamps.retain(|p, _| !p.starts_with(path));
        }

        /// Moves what is at `from`, and everything below it, to `to`.
        fn rename(&mut self, from: &Path, to: &Path) {
            fn moved<V>(map: &mut BTreeMap<PathBuf, V>, from: &Path, to: &Path) {
                let below: Vec<PathBuf> = map
                    .keys()
                    .filter(|p| p.starts_with(from))
                    .cloned()
                    .collect();
                for path in below {
                    let value = map.remove(&path).unwrap();
                    map.insert(joined(to, path.strip_prefix(from).unwrap()), value);
                }
            }
            moved(&mut self.base.entries, from, to);
            moved(&mut self.base.file_stamps, from, to);
            moved(&mut self.base.tree_stamps, from, to);
        }

        fn exchange(&mut self, a: &Path, b: &Path) {
            let aside = Path::new("/sim/.exchanging");
            self.rename(a, aside);
            self.rename(b, a);
            self.rename(aside, b);
        }

        /// Records a step as `Journal::step` does, with the identities at its paths now.
        fn record(
            &self,
            n: u32,
            op: StepOp,
            from: &Path,
            to: &Path,
            purpose: StepPurpose,
        ) -> StepRecord {
            StepRecord {
                n,
                op,
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                purpose,
                from_id: self.id(from),
                to_id: self.id(to),
            }
        }

        /// Runs a recorded step's rename.
        fn run(&mut self, step: &StepRecord) {
            match step.op {
                StepOp::Exchange => self.exchange(&step.from, &step.to),
                StepOp::Noreplace | StepOp::Rename => self.rename(&step.from, &step.to),
            }
        }

        /// Undoes a step that ran, as recovery would.
        fn undo(&mut self, step: &StepRecord) {
            match step.op {
                StepOp::Exchange => self.exchange(&step.from, &step.to),
                StepOp::Noreplace | StepOp::Rename => self.rename(&step.to, &step.from),
            }
        }

        /// Marks `client_dir` as `key`'s.
        fn mark(&mut self, client_dir: &Path, key: TargetKey) {
            self.base.markers.insert(client_dir.to_path_buf(), key);
        }
    }

    /// A `prepared` payload for `tx` on [`client`], an install unless changed.
    fn prepared(tx: TxId, key: TargetKey) -> Prepared {
        Prepared {
            schema_version: journal::CURRENT,
            tx,
            target_key: key,
            client_dir: client(),
            kind: TxKind::Install,
            started_at: Timestamp::UNIX_EPOCH,
            creates_dirs: Vec::new(),
            commit: CommitPayload {
                record: TargetRecord::empty(key),
                index: IndexChanges::default(),
                history: HistoryEntry {
                    schema_version: history::CURRENT,
                    tx,
                    kind: TxKind::Install,
                    at: Timestamp::UNIX_EPOCH,
                    target: Some(key),
                    targets: Vec::new(),
                    actor: Actor::Agent,
                    reason: None,
                    argv: vec!["omaddons".into()],
                    ops: Vec::new(),
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    sv_restored: None,
                    created_addons_dir: false,
                    run: None,
                    backup: None,
                    plan: None,
                    doctor_run_id: None,
                    repairs: Vec::new(),
                },
                stage_dir: ClientPaths::new(client()).stage(&tx),
                sv_restore: None,
            },
            writes_wtf: false,
            staged_stamp: None,
            staged_files: None,
            source_backup: None,
            client: None,
        }
    }

    /// A journal's text: `prepared`, its steps, then `tail`.
    fn text(prepared: &Prepared, steps: &[StepRecord], tail: &[JournalLine]) -> String {
        std::iter::once(JournalLine::Prepared(Box::new(prepared.clone())))
            .chain(steps.iter().cloned().map(JournalLine::Step))
            .chain(tail.iter().cloned())
            .map(|line| format!("{}\n", serde_json::to_string(&line).unwrap()))
            .collect()
    }

    /// One journal's diagnosis on `world`.
    fn diagnose_text(world: &World, tx: TxId, text: &str, sv_writable: bool) -> JournalDiagnosis {
        let listing = vec![Listed {
            tx,
            path: paths().journal(&tx),
            text: Ok(text.to_owned()),
        }];
        diagnose_journals(&world.base, &paths(), listing, &[client()], &|_| {
            sv_writable
        })
        .remove(0)
    }

    const BACK: JournalVerdict = JournalVerdict::Recoverable {
        phase: Phase::Prepared,
        direction: Direction::Back,
    };

    /// `tests/journal.rs`'s six-step folder transaction (exchange Foo, old Foo to the
    /// snapshot, Bar in, Gone out, Baz by the two-rename fallback): `executed` steps ran,
    /// and the next one is recorded too when `record_next`.
    struct Folder {
        world: World,
        tx: TxId,
        prepared: Prepared,
        steps: Vec<StepRecord>,
    }

    impl Folder {
        fn new(executed: usize, record_next: bool) -> Self {
            let mut world = World::new();
            let (tx, key) = (TxId::new(), TargetKey::new());
            let at = ClientPaths::new(client());
            let (addons, stage, snaps) = (at.addons(), at.stage(&tx), at.snapshots(&tx));
            for name in ["Foo", "Gone", "Baz"] {
                world.dir(&addons.join(name));
            }
            world.dir(&stage);
            for name in ["Foo", "Bar", "Baz"] {
                world.dir(&stage.join(name));
            }
            world.dir(&snaps);
            world.mark(&client(), key);
            let plan = [
                (
                    StepOp::Exchange,
                    stage.join("Foo"),
                    addons.join("Foo"),
                    StepPurpose::SwapIn,
                ),
                (
                    StepOp::Noreplace,
                    stage.join("Foo"),
                    snaps.join("Foo"),
                    StepPurpose::OldToSnapshot,
                ),
                (
                    StepOp::Noreplace,
                    stage.join("Bar"),
                    addons.join("Bar"),
                    StepPurpose::SwapIn,
                ),
                (
                    StepOp::Noreplace,
                    addons.join("Gone"),
                    snaps.join("Gone"),
                    StepPurpose::RemoveToSnapshot,
                ),
                (
                    StepOp::Rename,
                    addons.join("Baz"),
                    snaps.join("Baz"),
                    StepPurpose::FallbackLiveToSnapshot,
                ),
                (
                    StepOp::Rename,
                    stage.join("Baz"),
                    addons.join("Baz"),
                    StepPurpose::FallbackStageToLive,
                ),
            ];
            let recorded = (executed + usize::from(record_next)).min(plan.len());
            let mut steps = Vec::new();
            for (i, (op, from, to, purpose)) in plan.into_iter().enumerate().take(recorded) {
                let step = world.record(u32::try_from(i + 1).unwrap(), op, &from, &to, purpose);
                if i < executed {
                    world.run(&step);
                }
                steps.push(step);
            }
            Self {
                world,
                tx,
                prepared: prepared(tx, key),
                steps,
            }
        }

        fn diagnose(&self, tail: &[JournalLine]) -> JournalDiagnosis {
            let text = text(&self.prepared, &self.steps, tail);
            diagnose_text(&self.world, self.tx, &text, true)
        }
    }

    /// A whole `WTF/` restore on [`client`]: the live tree, the staged copy (stamped
    /// `stamp:staged`, and recorded so), its source backup, and the exchange of the two,
    /// journaled and, when `exchanged`, done. Describable.
    struct Restore {
        world: World,
        tx: TxId,
        prepared: Prepared,
        steps: Vec<StepRecord>,
        live: PathBuf,
        staged: PathBuf,
        backup: PathBuf,
    }

    impl Restore {
        fn new(exchanged: bool) -> Self {
            let mut world = World::new();
            let (tx, key) = (TxId::new(), TargetKey::new());
            let at = ClientPaths::new(client());
            let live = at.wtf();
            world.dir(&live);
            world.dir(&live.join("Account"));
            let stage_dir = at.stage_wtf(&tx);
            let staged = stage_dir.join("WTF");
            world.dir(&staged);
            world
                .base
                .tree_stamps
                .insert(staged.clone(), Ok("stamp:staged".into()));
            let id = BackupId::new();
            let backup = crate::backup::backup_dir(&at, &id);
            let backup_id = world.dir(&backup);
            world.mark(&client(), key);
            world.base.describable.insert(tx);
            let mut prepared = prepared(tx, key);
            prepared.kind = TxKind::Restore;
            prepared.writes_wtf = true;
            prepared.commit.stage_dir = stage_dir;
            prepared.staged_stamp = Some("stamp:staged".into());
            prepared.source_backup = Some(SourceBackup { id, dir: backup_id });
            let step = world.record(1, StepOp::Exchange, &staged, &live, StepPurpose::SwapIn);
            if exchanged {
                world.run(&step);
            }
            Self {
                world,
                tx,
                prepared,
                steps: vec![step],
                live,
                staged,
                backup,
            }
        }

        fn diagnose_with(&self, tail: &[JournalLine], sv_writable: bool) -> JournalDiagnosis {
            let text = text(&self.prepared, &self.steps, tail);
            diagnose_text(&self.world, self.tx, &text, sv_writable)
        }

        fn diagnose(&self) -> JournalDiagnosis {
            self.diagnose_with(&[], true)
        }

        /// Someone moved the live tree away and put another `WTF/` in its place.
        fn replace_live(&mut self) {
            let aside = client().join("WTF.aside");
            self.world.rename(&self.live, &aside);
            let live = self.live.clone();
            self.world.dir(&live);
        }
    }

    // -----------------------------------------------------------------------------------
    // The tree.

    /// A tree over `world` with `moves` applied, and what it says is at each path.
    fn seen(world: &World, moves: &[Move], at: &[&str]) -> Vec<Option<u64>> {
        let mut sim = Simulated::new(&world.base);
        for applied in moves {
            sim.apply(applied.clone());
        }
        at.iter()
            .map(|path| sim.entry(Path::new(path)).map(|entry| entry.id.ino))
            .collect()
    }

    fn rename(from: &str, to: &str) -> Move {
        Move::Rename {
            from: from.into(),
            to: to.into(),
        }
    }

    fn tree() -> (World, [u64; 4]) {
        let mut world = World::new();
        let ids = ["/t/a", "/t/a/x", "/t/b", "/t/b/y"].map(|p| world.dir(Path::new(p)).ino);
        (world, ids)
    }

    #[test]
    fn a_rename_moves_what_was_at_its_source() {
        let (world, [a, ..]) = tree();
        let seen = seen(&world, &[rename("/t/a", "/t/c")], &["/t/c", "/t/a"]);
        assert_eq!(seen, [Some(a), None]);
    }

    #[test]
    fn an_exchange_swaps_both_sides() {
        let (world, [a, x, b, y]) = tree();
        let exchange = Move::Exchange {
            a: "/t/a".into(),
            b: "/t/b".into(),
        };
        let seen = seen(&world, &[exchange], &["/t/a", "/t/b", "/t/a/y", "/t/b/x"]);
        assert_eq!(seen, [Some(b), Some(a), Some(y), Some(x)]);
    }

    #[test]
    fn a_subtree_moves_with_its_directory() {
        let (world, [_, x, ..]) = tree();
        let seen = seen(&world, &[rename("/t/a", "/t/c")], &["/t/c/x", "/t/a/x"]);
        assert_eq!(seen, [Some(x), None]);
    }

    #[test]
    fn an_ancestor_keeps_its_identity() {
        let mut world = World::new();
        let t = world.dir(Path::new("/t")).ino;
        world.dir(Path::new("/t/a"));
        let seen = seen(&world, &[rename("/t/a", "/t/c")], &["/t"]);
        assert_eq!(seen, [Some(t)]);
    }

    #[test]
    fn two_moves_compose_latest_first() {
        let (world, [a, x, b, _]) = tree();
        // `a` goes to `c`, then `b` goes to where `a` was, then `c` goes under `b`'s place.
        let moves = [
            rename("/t/a", "/t/c"),
            rename("/t/b", "/t/a"),
            rename("/t/c", "/t/b"),
        ];
        let seen = seen(&world, &moves, &["/t/a", "/t/b", "/t/b/x", "/t/c"]);
        assert_eq!(seen, [Some(b), Some(a), Some(x), None]);
    }

    #[test]
    fn restore_truncates_the_moves() {
        let (world, [a, ..]) = tree();
        let mut sim = Simulated::new(&world.base);
        let checkpoint = sim.checkpoint();
        sim.apply(rename("/t/a", "/t/c"));
        assert_eq!(sim.entry(Path::new("/t/a")), None);
        sim.restore(checkpoint);
        assert_eq!(sim.entry(Path::new("/t/a")).map(|e| e.id.ino), Some(a));
    }

    // -----------------------------------------------------------------------------------
    // Recoverable.

    #[test]
    fn every_prefix_of_the_six_step_journal_rolls_back() {
        for executed in 0..=6 {
            for record_next in [false, true] {
                let folder = Folder::new(executed, record_next);
                let found = folder.diagnose(&[]);
                let ran: Vec<u32> = (1..=u32::try_from(executed).unwrap()).collect();
                assert_eq!(
                    (found.verdict, &found.evidence.ran, found.expected()),
                    (BACK, &ran, Expected::RolledBack),
                    "{executed} executed, next recorded: {record_next}\n{:#?}\n{:#?}",
                    folder.steps,
                    found
                );
                assert!(found.exact(), "{found:#?}");
            }
        }
    }

    #[test]
    fn a_half_done_rollback_is_still_recoverable() {
        // The exchange and the old tree's move ran; a recovery undid both and died. The
        // second step now sees the staged tree at its source: neither state, after a step
        // in its before state, so not run.
        let mut folder = Folder::new(2, false);
        let (one, two) = (folder.steps[0].clone(), folder.steps[1].clone());
        folder.world.undo(&two);
        folder.world.undo(&one);
        let found = folder.diagnose(&[]);
        assert_eq!(
            (found.verdict, found.evidence.ran.len()),
            (BACK, 0),
            "{found:#?}"
        );
    }

    #[test]
    fn a_renumbered_device_still_decides() {
        let mut folder = Folder::new(3, false);
        for entry in folder.world.base.entries.values_mut() {
            entry.id.dev ^= 0x5a5a;
        }
        let found = folder.diagnose(&[]);
        assert_eq!(
            (found.verdict, found.evidence.ran.as_slice()),
            (BACK, &[1, 2, 3][..])
        );
    }

    #[test]
    fn swapped_rolls_forward() {
        let found = Folder::new(6, false).diagnose(&[JournalLine::Swapped]);
        let forward = JournalVerdict::Recoverable {
            phase: Phase::Swapped,
            direction: Direction::Forward,
        };
        assert_eq!(
            (found.verdict, found.expected(), found.evidence.ran.len()),
            (forward, Expected::RolledForward, 0),
            "{found:#?}"
        );
    }

    #[test]
    fn committed_and_unprepared_are_pruned() {
        let folder = Folder::new(6, false);
        let committed = text(
            &folder.prepared,
            &folder.steps,
            &[JournalLine::Swapped, JournalLine::Committed],
        );
        let torn = committed.lines().next().unwrap().to_owned();
        for (name, text, phase) in [
            ("empty", String::new(), Phase::Unprepared),
            ("torn_prepared_only", torn, Phase::Unprepared),
            ("committed", committed, Phase::Committed),
        ] {
            let found = diagnose_text(&folder.world, folder.tx, &text, true);
            let pruned = JournalVerdict::Recoverable {
                phase,
                direction: Direction::Prune,
            };
            assert_eq!(
                (found.verdict, found.expected()),
                (pruned, Expected::Pruned),
                "{name}\n{text}\n{found:#?}"
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // Stops.

    #[test]
    fn step_unrecognized_names_the_first_step_in_neither_state() {
        let mut folder = Folder::new(6, false);
        // Gone's snapshot copy replaced: step 4 is in neither state.
        let gone = folder.steps[3].to.clone();
        folder.world.remove(&gone);
        folder.world.dir(&gone);
        let found = folder.diagnose(&[]);
        assert_eq!(
            (found.verdict, found.evidence.ran.as_slice()),
            (JournalVerdict::StepUnrecognized { step: 4 }, &[5, 6][..]),
            "{found:#?}"
        );
        assert_eq!(
            found.expected(),
            Expected::Stops {
                code: ErrorCode::Internal,
                reason: StopReason::StepUnrecognized,
                step: Some(4)
            }
        );
    }

    #[test]
    fn a_reused_inode_with_another_birth_time_is_neither() {
        let mut folder = Folder::new(1, false);
        let live = folder.steps[0].to.clone();
        let entry = folder.world.base.entries.get_mut(&live).unwrap();
        entry.id.born_ns = entry.id.born_ns.map(|ns| ns - 1_000_000_000);
        let found = folder.diagnose(&[]);
        assert_eq!(
            found.verdict,
            JournalVerdict::StepUnrecognized { step: 1 },
            "{found:#?}"
        );
    }

    #[test]
    fn waits_for_game_cases() {
        let waits = JournalVerdict::WaitsForGame;
        let swapped = [JournalLine::Swapped];
        let mut cases: Vec<(&str, Restore, &[JournalLine], bool, JournalVerdict)> = vec![
            (
                "a_step_and_not_writable",
                Restore::new(true),
                &[],
                false,
                waits,
            ),
            ("writable_walks", Restore::new(true), &[], true, BACK),
            (
                "swapped_and_not_writable",
                Restore::new(true),
                &swapped,
                false,
                waits,
            ),
        ];
        let mut no_step = Restore::new(false);
        no_step.steps.clear();
        cases.push(("no_step_walks", no_step, &[], false, BACK));
        let mut plain = Restore::new(true);
        plain.prepared.writes_wtf = false;
        cases.push(("not_writes_wtf_walks", plain, &[], false, BACK));
        for (name, restore, tail, writable, verdict) in cases {
            let found = restore.diagnose_with(tail, writable);
            assert_eq!(found.verdict, verdict, "{name}: {found:#?}");
        }
        let found = Restore::new(true).diagnose_with(&[], false);
        assert_eq!(
            found.expected(),
            Expected::Stops {
                code: ErrorCode::GameRunning,
                reason: StopReason::SavedVariables,
                step: None
            }
        );
    }

    #[test]
    fn unrecoverable_precedes_everything() {
        // A newer major, its client unmarked and its game running.
        let mut restore = Restore::new(true);
        restore.world.base.markers.clear();
        let newer = text(&restore.prepared, &restore.steps, &[]).replacen(
            "\"schema_version\":\"1.1\"",
            "\"schema_version\":\"9.0\"",
            1,
        );
        let found = diagnose_text(&restore.world, restore.tx, &newer, false);
        assert_eq!(found.verdict, JournalVerdict::TooNew, "{found:#?}");
        // An undecodable line in the middle.
        let folder = Folder::new(2, false);
        let (first, rest) = text(&folder.prepared, &folder.steps, &[])
            .split_once('\n')
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .unwrap();
        let middle = format!("{first}\n{{\"type\":\"teleport\"}}\n{rest}");
        let found = diagnose_text(&folder.world, folder.tx, &middle, true);
        assert_eq!(found.verdict, JournalVerdict::Unreadable, "{found:#?}");
        assert_eq!(
            found.expected(),
            Expected::Stops {
                code: ErrorCode::StateCorrupt,
                reason: StopReason::JournalUnreadable,
                step: None
            }
        );
    }

    /// `text` with its `prepared` line's target key garbled.
    fn without_target(text: &str) -> String {
        let key = "\"target_key\":\"";
        let at = text.find(key).unwrap() + key.len();
        let end = at + text[at..].find('"').unwrap();
        format!("{}garbage{}", &text[..at], &text[end..])
    }

    #[test]
    fn unattributed_causes() {
        let folder = Folder::new(2, false);
        let plain = text(&folder.prepared, &folder.steps, &[]);
        let newer = without_target(&plain.replacen(
            "\"schema_version\":\"1.1\"",
            "\"schema_version\":\"9.0\"",
            1,
        ));
        let undecodable = without_target(&plain);
        let unattributed = |cause| JournalVerdict::Unattributed { cause };
        for (name, text, cause, code) in [
            (
                "newer_major_without_target",
                Ok(newer),
                UnattributedCause::TooNew,
                ErrorCode::StateTooNew,
            ),
            (
                "undecodable_prepared_without_target",
                Ok(undecodable),
                UnattributedCause::Unreadable,
                ErrorCode::StateCorrupt,
            ),
            (
                "unreadable_file",
                Err("permission denied".to_owned()),
                UnattributedCause::ReadFailed,
                ErrorCode::Io,
            ),
        ] {
            let listing = vec![Listed {
                tx: folder.tx,
                path: paths().journal(&folder.tx),
                text,
            }];
            let found =
                diagnose_journals(&folder.world.base, &paths(), listing, &[client()], &|_| {
                    true
                })
                .remove(0);
            assert_eq!(found.verdict, unattributed(cause), "{name}: {found:#?}");
            assert_eq!(
                found.expected(),
                Expected::Stops {
                    code,
                    reason: StopReason::Unattributed,
                    step: None
                },
                "{name}"
            );
        }
        let mut probed = Folder::new(2, false);
        probed
            .world
            .base
            .alive
            .insert(probed.tx, Err("permission denied".into()));
        let found = probed.diagnose(&[]);
        assert_eq!(
            (found.verdict, found.live, found.expected()),
            (
                unattributed(UnattributedCause::LivenessUnknown),
                false,
                Expected::Stops {
                    code: ErrorCode::Io,
                    reason: StopReason::Unattributed,
                    step: None
                }
            ),
            "liveness_unknown: {found:#?}"
        );
    }

    #[test]
    fn marker_mismatch_only_while_unfinished() {
        for (name, tail, verdict) in [
            ("prepared", vec![], JournalVerdict::MarkerMismatch),
            (
                "swapped",
                vec![JournalLine::Swapped],
                JournalVerdict::MarkerMismatch,
            ),
            (
                "committed",
                vec![JournalLine::Swapped, JournalLine::Committed],
                JournalVerdict::Recoverable {
                    phase: Phase::Committed,
                    direction: Direction::Prune,
                },
            ),
        ] {
            let mut folder = Folder::new(6, false);
            folder.world.mark(&client(), TargetKey::new());
            let found = folder.diagnose(&tail);
            assert_eq!(found.verdict, verdict, "{name}: {found:#?}");
            assert!(found.evidence.ran.is_empty(), "{name}: nothing walked");
        }
    }

    // -----------------------------------------------------------------------------------
    // Settings restores.

    #[test]
    fn a_replaced_restored_copy_is_left_and_the_walk_goes_on() {
        // A restore into an empty place, then our copy replaced by another tree.
        let mut restore = Restore::new(false);
        restore.world.remove(&restore.live.clone());
        let step = restore.world.record(
            1,
            StepOp::Noreplace,
            &restore.staged,
            &restore.live,
            StepPurpose::SwapIn,
        );
        restore.world.run(&step);
        restore.steps = vec![step];
        restore.world.remove(&restore.live.clone());
        let live = restore.live.clone();
        restore.world.dir(&live);
        let found = restore.diagnose();
        assert_eq!(
            (
                found.verdict,
                found.evidence.displaced,
                found.evidence.preservation
            ),
            (BACK, 0, None),
            "{found:#?}"
        );
    }

    #[test]
    fn a_displaced_side_becomes_a_preservation_item() {
        let mut restore = Restore::new(true);
        restore.replace_live();
        let found = restore.diagnose();
        assert_eq!(found.verdict, BACK, "{found:#?}");
        assert_eq!(found.evidence.displaced, 1);
        assert_eq!(
            found.evidence.preservation,
            Some(Preservation {
                intent: None,
                items: 1,
                pending: false
            })
        );
        assert_eq!(
            found.evidence.approximations,
            [Approximation::PreservationWalk]
        );
        assert!(!found.exact());
    }

    #[test]
    fn classify_cannot_place_is_unexpected_state() {
        let mut restore = Restore::new(true);
        restore.replace_live();
        let staged = restore.staged.clone();
        restore.world.rename(&staged, &client().join("moved"));
        let found = restore.diagnose();
        assert_eq!(
            (found.verdict, found.evidence.ran.len()),
            (JournalVerdict::UnexpectedState { step: 1 }, 0),
            "{found:#?}"
        );
        assert_eq!(
            found.expected(),
            Expected::Stops {
                code: ErrorCode::Internal,
                reason: StopReason::UnexpectedState,
                step: Some(1)
            }
        );
    }

    #[test]
    fn a_played_on_stage_is_preserved() {
        let mut restore = Restore::new(true);
        // The restored copy, live now, was played on.
        let live = restore.live.clone();
        restore
            .world
            .base
            .tree_stamps
            .insert(live, Ok("stamp:played".into()));
        let found = restore.diagnose();
        assert_eq!(
            (
                found.verdict,
                found.evidence.ran.as_slice(),
                found.evidence.preservation.map(|p| p.items)
            ),
            (BACK, &[1][..], Some(1)),
            "{found:#?}"
        );
        // Unplayed and its source there: nothing to keep.
        let found = Restore::new(true).diagnose();
        assert_eq!((found.verdict, found.evidence.preservation), (BACK, None));
        assert!(found.exact(), "{found:#?}");
    }

    #[test]
    fn a_lost_source_is_preserved() {
        for (name, replaced) in [("source_replaced", true), ("source_gone", false)] {
            let mut restore = Restore::new(true);
            let backup = restore.backup.clone();
            restore.world.remove(&backup);
            if replaced {
                restore.world.dir(&backup);
            }
            let found = restore.diagnose();
            assert_eq!(
                (found.verdict, found.evidence.preservation.map(|p| p.items)),
                (BACK, Some(1)),
                "{name}: {found:#?}"
            );
        }
    }

    #[test]
    fn an_undescribable_client_stops() {
        let mut restore = Restore::new(true);
        restore.replace_live();
        restore.world.base.describable.clear();
        let found = restore.diagnose();
        assert_eq!(
            found.verdict,
            JournalVerdict::PreservationIncomplete { stops: true },
            "{found:#?}"
        );
        assert_eq!(
            found.expected(),
            Expected::Stops {
                code: ErrorCode::StateCorrupt,
                reason: StopReason::PreservationIncomplete,
                step: None
            }
        );
    }

    #[test]
    fn a_stamp_failure_stops() {
        let mut restore = Restore::new(true);
        let live = restore.live.clone();
        restore
            .world
            .base
            .tree_stamps
            .insert(live, Err("a folder that cannot be listed".into()));
        let found = restore.diagnose();
        assert_eq!(
            found.verdict,
            JournalVerdict::PreservationIncomplete { stops: true },
            "{found:#?}"
        );
    }

    /// A partial restore (M3-D27): each file exchanged with its live copy, then the
    /// staged `WTF/` moved to the snapshot, all of it run. `played` names files the game
    /// wrote since.
    fn partial_restore(played: &[&str]) -> (World, TxId, Prepared, Vec<StepRecord>) {
        let mut world = World::new();
        let (tx, key) = (TxId::new(), TargetKey::new());
        let at = ClientPaths::new(client());
        let (live, stage_dir) = (at.wtf(), at.stage_wtf(&tx));
        let staged = stage_dir.join("WTF");
        let rels = [
            "Account/FAKEACCT/SavedVariables/A.lua",
            "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/B.lua",
        ];
        world.dir(&staged);
        let mut files = Vec::new();
        for rel in rels {
            world.file(&staged.join(rel), &format!("stamp:{rel}"));
            world.file(&live.join(rel), &format!("stamp:live {rel}"));
            files.push(StagedFile {
                rel: rel.into(),
                stamp: format!("stamp:{rel}"),
            });
        }
        let id = BackupId::new();
        let backup = world.dir(&crate::backup::backup_dir(&at, &id));
        world.mark(&client(), key);
        world.base.describable.insert(tx);
        let mut prepared = prepared(tx, key);
        prepared.kind = TxKind::Restore;
        prepared.writes_wtf = true;
        prepared.commit.stage_dir = stage_dir;
        prepared.staged_files = Some(files);
        prepared.source_backup = Some(SourceBackup { id, dir: backup });
        let mut steps = Vec::new();
        for rel in rels {
            let n = u32::try_from(steps.len() + 1).unwrap();
            let step = world.record(
                n,
                StepOp::Exchange,
                &staged.join(rel),
                &live.join(rel),
                StepPurpose::SwapIn,
            );
            world.run(&step);
            steps.push(step);
        }
        let into = at.snapshots_wtf(&tx).join("WTF");
        let step = world.record(
            3,
            StepOp::Noreplace,
            &staged,
            &into,
            StepPurpose::OldToSnapshot,
        );
        world.run(&step);
        steps.push(step);
        for rel in played {
            world
                .base
                .file_stamps
                .insert(live.join(rel), format!("stamp:played {rel}"));
        }
        (world, tx, prepared, steps)
    }

    #[test]
    fn a_partial_restore_simulates_its_files() {
        let (world, tx, prepared, steps) =
            partial_restore(&["Account/FAKEACCT/SavedVariables/A.lua"]);
        let found = diagnose_text(&world, tx, &text(&prepared, &steps, &[]), true);
        assert_eq!(
            (
                found.verdict,
                found.evidence.ran.as_slice(),
                found.evidence.preservation.map(|p| p.items)
            ),
            (BACK, &[1, 2, 3][..], Some(1)),
            "the played-on file is kept, the other is not: {found:#?}"
        );
        let (world, tx, prepared, steps) = partial_restore(&[]);
        let found = diagnose_text(&world, tx, &text(&prepared, &steps, &[]), true);
        assert_eq!(
            (found.verdict, found.evidence.preservation),
            (BACK, None),
            "{found:#?}"
        );
    }

    // -----------------------------------------------------------------------------------
    // Resumption.

    /// A restore whose walk is done and whose preservation intent for the displaced
    /// pre-restore tree is journaled: the item is where recovery found it.
    fn intent_journaled() -> (Restore, PreserveIntent) {
        let mut restore = Restore::new(true);
        restore.replace_live();
        let id = BackupId::new();
        let intent = PreserveIntent {
            id,
            items: vec![PreserveItem {
                path: restore.staged.clone(),
                identity: restore.world.id(&restore.staged).unwrap(),
                rel: String::new(),
            }],
            assembly: restore
                .prepared
                .commit
                .stage_dir
                .join("recovered")
                .join(id.to_string()),
        };
        (restore, intent)
    }

    #[test]
    fn resume_cases() {
        type Setup = fn(&mut Restore, &PreserveIntent);
        let cases: [(&str, Setup, bool); 5] = [
            ("resume_moves_found_items", |_, _| {}, false),
            (
                "resume_skips_assembled_items",
                |r, i| {
                    r.world
                        .rename(&i.items[0].path, &preserve::place(&i.assembly, ""))
                },
                false,
            ),
            (
                "resume_stops_on_an_item_in_neither_place",
                |r, i| {
                    r.world
                        .rename(&i.items[0].path, &client().join("elsewhere"))
                },
                true,
            ),
            (
                "resume_accepts_a_published_backup_with_its_items",
                |r, i| {
                    let published = crate::backup::backup_dir(&ClientPaths::new(client()), &i.id);
                    r.world.dir(&published);
                    r.world
                        .rename(&i.items[0].path, &preserve::place(&published, ""));
                    r.world.base.backup_metas.insert(published);
                },
                false,
            ),
            (
                "resume_stops_on_a_published_backup_without_its_items",
                |r, i| {
                    let published = crate::backup::backup_dir(&ClientPaths::new(client()), &i.id);
                    r.world.dir(&published);
                    r.world.base.backup_metas.insert(published);
                },
                true,
            ),
        ];
        for (name, setup, stops) in cases {
            let (mut restore, intent) = intent_journaled();
            setup(&mut restore, &intent);
            let found = restore.diagnose_with(&[JournalLine::Preserve(intent.clone())], true);
            assert_eq!(
                found.verdict,
                JournalVerdict::PreservationIncomplete { stops },
                "{name}: {found:#?}"
            );
            assert_eq!(
                found.evidence.preservation,
                Some(Preservation {
                    intent: Some(intent.id),
                    items: 1,
                    pending: true
                }),
                "{name}"
            );
            assert!(
                found.evidence.ran.is_empty(),
                "{name}: no walk once an intent is journaled"
            );
            let back = if stops { None } else { Some(Direction::Back) };
            assert_eq!(found.direction(), back, "{name}");
        }
    }

    #[test]
    fn all_intents_preserved_is_recoverable() {
        let (restore, intent) = intent_journaled();
        let tail = [
            JournalLine::Preserve(intent.clone()),
            JournalLine::Preserved(Preserved { id: intent.id }),
        ];
        let found = restore.diagnose_with(&tail, true);
        assert_eq!(found.verdict, BACK, "{found:#?}");
        assert_eq!(
            found.evidence.preservation,
            Some(Preservation {
                intent: Some(intent.id),
                items: 1,
                pending: false
            })
        );
    }

    // -----------------------------------------------------------------------------------
    // The driver.

    /// Two journals on one client: the first exchanged Foo; the second, listed after it,
    /// recorded a move of the live Foo it found (the new one) and did not run it.
    fn two_journals(first_alive: bool) -> Vec<JournalDiagnosis> {
        let mut first = Folder::new(1, false);
        let tx = TxId::new();
        let live = first.steps[0].to.clone();
        let second = first.world.record(
            1,
            StepOp::Noreplace,
            &live,
            &ClientPaths::new(client()).snapshots(&tx).join("Foo"),
            StepPurpose::RemoveToSnapshot,
        );
        if first_alive {
            first.world.base.alive.insert(first.tx, Ok(true));
        }
        let listing = vec![
            Listed {
                tx: first.tx,
                path: paths().journal(&first.tx),
                text: Ok(text(&first.prepared, &first.steps, &[])),
            },
            Listed {
                tx,
                path: paths().journal(&tx),
                text: Ok(text(
                    &prepared(tx, first.prepared.target_key),
                    &[second],
                    &[],
                )),
            },
        ];
        diagnose_journals(&first.world.base, &paths(), listing, &[client()], &|_| true)
    }

    #[test]
    fn a_live_journal_is_skipped_and_leaves_the_tree_as_it_was() {
        let found = two_journals(true);
        assert_eq!(
            (found[0].live, found[0].expected(), found[0].verdict),
            (true, Expected::Skipped, BACK),
            "{found:#?}"
        );
        assert_eq!(
            (found[1].verdict, found[1].expected()),
            (BACK, Expected::RolledBack),
            "the second sees the new Foo still live: {found:#?}"
        );
    }

    #[test]
    fn journals_share_one_simulated_tree_in_recovery_order() {
        let found = two_journals(false);
        assert_eq!(found[0].verdict, BACK, "{found:#?}");
        assert_eq!(
            found[1].verdict,
            JournalVerdict::StepUnrecognized { step: 1 },
            "the first's rollback put the old Foo back, which the second never saw: {found:#?}"
        );
    }

    /// Every verdict, once.
    fn every_verdict() -> Vec<JournalVerdict> {
        let mut all = vec![
            JournalVerdict::WaitsForGame,
            JournalVerdict::TooNew,
            JournalVerdict::Unreadable,
            JournalVerdict::UnexpectedState { step: 2 },
            JournalVerdict::StepUnrecognized { step: 3 },
            JournalVerdict::MarkerMismatch,
            JournalVerdict::PreservationIncomplete { stops: false },
            JournalVerdict::PreservationIncomplete { stops: true },
        ];
        for (phase, direction) in [
            (Phase::Prepared, Direction::Back),
            (Phase::Swapped, Direction::Forward),
            (Phase::Committed, Direction::Prune),
            (Phase::Unprepared, Direction::Prune),
        ] {
            all.push(JournalVerdict::Recoverable { phase, direction });
        }
        for cause in [
            UnattributedCause::LivenessUnknown,
            UnattributedCause::ReadFailed,
            UnattributedCause::TooNew,
            UnattributedCause::Unreadable,
        ] {
            all.push(JournalVerdict::Unattributed { cause });
        }
        all
    }

    fn with_verdict(verdict: JournalVerdict, live: bool) -> JournalDiagnosis {
        JournalDiagnosis {
            tx: TxId::new(),
            family: JournalFamily::Folder,
            path: PathBuf::from("/sim/s/omaddons/journal/x.jsonl"),
            target: None,
            client_dir: None,
            phase: Phase::Prepared,
            live,
            verdict,
            evidence: JournalEvidence::default(),
            protected: ProtectedPaths::default(),
        }
    }

    #[test]
    fn expected_is_total() {
        let stops = |code, reason, step| Expected::Stops { code, reason, step };
        for verdict in every_verdict() {
            let expected = match verdict {
                JournalVerdict::Recoverable {
                    direction: Direction::Back,
                    ..
                }
                | JournalVerdict::PreservationIncomplete { stops: false } => Expected::RolledBack,
                JournalVerdict::Recoverable {
                    direction: Direction::Forward,
                    ..
                } => Expected::RolledForward,
                JournalVerdict::Recoverable {
                    direction: Direction::Prune,
                    ..
                } => Expected::Pruned,
                JournalVerdict::WaitsForGame => {
                    stops(ErrorCode::GameRunning, StopReason::SavedVariables, None)
                }
                JournalVerdict::TooNew => {
                    stops(ErrorCode::StateTooNew, StopReason::JournalTooNew, None)
                }
                JournalVerdict::Unreadable => {
                    stops(ErrorCode::StateCorrupt, StopReason::JournalUnreadable, None)
                }
                JournalVerdict::UnexpectedState { step } => {
                    stops(ErrorCode::Internal, StopReason::UnexpectedState, Some(step))
                }
                JournalVerdict::StepUnrecognized { step } => stops(
                    ErrorCode::Internal,
                    StopReason::StepUnrecognized,
                    Some(step),
                ),
                JournalVerdict::MarkerMismatch => {
                    stops(ErrorCode::ScopeDenied, StopReason::MarkerMismatch, None)
                }
                JournalVerdict::PreservationIncomplete { stops: true } => stops(
                    ErrorCode::StateCorrupt,
                    StopReason::PreservationIncomplete,
                    None,
                ),
                JournalVerdict::Unattributed { cause } => {
                    let code = match cause {
                        UnattributedCause::LivenessUnknown | UnattributedCause::ReadFailed => {
                            ErrorCode::Io
                        }
                        UnattributedCause::TooNew => ErrorCode::StateTooNew,
                        UnattributedCause::Unreadable => ErrorCode::StateCorrupt,
                    };
                    stops(code, StopReason::Unattributed, None)
                }
            };
            assert_eq!(
                with_verdict(verdict, false).expected(),
                expected,
                "{verdict:?}"
            );
            assert_eq!(
                with_verdict(verdict, true).expected(),
                Expected::Skipped,
                "{verdict:?} live"
            );
        }
    }

    #[test]
    fn direction_matches_expected() {
        for verdict in every_verdict() {
            let found = with_verdict(verdict, false);
            let from_expected = match found.expected() {
                Expected::RolledForward => Some(Direction::Forward),
                Expected::RolledBack => Some(Direction::Back),
                Expected::Pruned => Some(Direction::Prune),
                Expected::Skipped | Expected::Stops { .. } => None,
            };
            assert_eq!(found.direction(), from_expected, "{verdict:?}");
        }
    }

    // -----------------------------------------------------------------------------------
    // Protected paths and artifacts.

    /// Whether `path` has a component after a `WTF` or `WTF.previous` one.
    fn below_wtf(path: &Path) -> bool {
        let mut components = path.components();
        components.any(|c| {
            c.as_os_str().eq_ignore_ascii_case("WTF")
                || c.as_os_str()
                    .eq_ignore_ascii_case(super::super::retention::WTF_PREVIOUS)
        }) && components.next().is_some()
    }

    fn named_paths(found: &JournalDiagnosis) -> Vec<PathBuf> {
        let mut all = vec![found.path.clone()];
        all.extend(found.protected.reads.iter().cloned());
        all.extend(found.protected.writes.iter().cloned());
        all.extend(found.evidence.artifacts.iter().map(|a| a.path.clone()));
        all
    }

    #[test]
    fn protected_paths_never_name_a_path_below_wtf() {
        let whole = Restore::new(true).diagnose();
        let (world, tx, prepared, steps) =
            partial_restore(&["Account/FAKEACCT/SavedVariables/A.lua"]);
        let partial = diagnose_text(&world, tx, &text(&prepared, &steps, &[]), true);
        let mut undo = Restore::new(true);
        undo.prepared.kind = TxKind::Undo;
        undo.prepared.source_backup = None;
        undo.prepared.commit.history.reverts = Some(TxId::new());
        let undo = undo.diagnose();
        for (name, found) in [("whole", &whole), ("partial", &partial), ("undo", &undo)] {
            assert!(
                !found.protected.writes.is_empty(),
                "{name}: trusted, so it protects"
            );
            for path in named_paths(found) {
                assert!(
                    !below_wtf(&path),
                    "{name}: {} is below WTF/",
                    path.display()
                );
            }
            let json = serde_json::to_string(found).unwrap();
            for planted in ["FAKEACCT", "FakeRealm", "FakeChar"] {
                assert!(!json.contains(planted), "{name}: {planted} in {json}");
            }
        }
        assert!(
            partial.protected.writes.iter().any(|p| p.ends_with("WTF")),
            "the live WTF/ is protected, cut: {:#?}",
            partial.protected
        );
        assert!(
            undo.protected
                .reads
                .iter()
                .any(|p| p.ends_with(super::super::retention::WTF_PREVIOUS)),
            "an undo of a restore reads the reverted WTF.previous: {:#?}",
            undo.protected
        );
    }

    #[test]
    fn untrusted_journals_protect_only_artifacts() {
        let mut folder = Folder::new(2, false);
        let classic = PathBuf::from("/sim/_classic_");
        let elsewhere = folder.world.record(
            3,
            StepOp::Noreplace,
            &classic.join("Interface/AddOns/Foo"),
            &classic.join("Interface/AddOns/Bar"),
            StepPurpose::SwapIn,
        );
        folder.steps.push(elsewhere);
        let journal = paths().journal(&folder.tx);
        folder.world.file(&journal, "a journal");
        let (first, rest) = text(&folder.prepared, &folder.steps, &[])
            .split_once('\n')
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .unwrap();
        let unreadable = format!("{first}\n{{\"type\":\"teleport\"}}\n{rest}");
        let found = diagnose_text(&folder.world, folder.tx, &unreadable, true);
        assert_eq!(found.verdict, JournalVerdict::Unreadable);
        assert_eq!(found.protected.reads, Vec::<PathBuf>::new());
        let artifacts: Vec<PathBuf> = found
            .evidence
            .artifacts
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect();
        assert!(artifacts.contains(&journal), "{found:#?}");
        assert_eq!(found.protected.writes, settled(artifacts), "{found:#?}");
        assert!(
            !serde_json::to_string(&found.protected)
                .unwrap()
                .contains("_classic_"),
            "never a path the payload names"
        );
        // A marker that is not the target's makes even a readable journal untrusted.
        let mut folder = Folder::new(2, false);
        folder.world.mark(&client(), TargetKey::new());
        let found = folder.diagnose(&[]);
        assert_eq!(found.verdict, JournalVerdict::MarkerMismatch);
        let artifacts = found
            .evidence
            .artifacts
            .iter()
            .map(|a| a.path.clone())
            .collect();
        assert_eq!(found.protected.writes, settled(artifacts), "{found:#?}");
        assert!(
            found
                .protected
                .writes
                .iter()
                .all(|p| !p.to_string_lossy().contains("AddOns")),
            "no step path: {found:#?}"
        );
    }

    #[test]
    fn artifacts_are_found_by_tx_id_in_every_known_location() {
        let folder = Folder::new(2, false);
        let mut world = folder.world;
        let tx = folder.tx;
        let p = paths();
        for path in [p.journal(&tx), p.inflight_lock(&tx), p.state_snapshot(&tx)] {
            world.file(&path, "x");
        }
        let classic = PathBuf::from("/sim/_classic_");
        world.mark(&classic, TargetKey::new());
        let at = ClientPaths::new(&classic);
        world.dir(&at.stage(&tx));
        world.dir(&at.snapshots_wtf(&tx));
        let listing = |text: Result<String, String>| {
            vec![Listed {
                tx,
                path: p.journal(&tx),
                text,
            }]
        };
        let dirs = [client(), classic.clone()];
        let found = diagnose_journals(
            &world.base,
            &p,
            listing(Ok(text(&folder.prepared, &folder.steps, &[]))),
            &dirs,
            &|_| true,
        )
        .remove(0);
        let kinds: Vec<(ArtifactKind, Option<PathBuf>, Option<bool>)> = found
            .evidence
            .artifacts
            .iter()
            .map(|a| (a.kind, a.client.clone(), a.marker_matches))
            .collect();
        assert_eq!(
            kinds,
            [
                (ArtifactKind::Journal, None, None),
                (ArtifactKind::Inflight, None, None),
                (ArtifactKind::StateSnapshot, None, None),
                (ArtifactKind::ClientStage, Some(client()), Some(true)),
                (ArtifactKind::ClientSnapshot, Some(client()), Some(true)),
                (
                    ArtifactKind::ClientStage,
                    Some(classic.clone()),
                    Some(false)
                ),
                (
                    ArtifactKind::ClientSnapshot,
                    Some(classic.clone()),
                    Some(false)
                ),
            ],
            "{found:#?}"
        );
        let unknown = diagnose_journals(
            &world.base,
            &p,
            listing(Err("unreadable".into())),
            &dirs,
            &|_| true,
        )
        .remove(0);
        assert!(
            unknown
                .evidence
                .artifacts
                .iter()
                .all(|a| a.marker_matches.is_none()),
            "no target, no marker test: {unknown:#?}"
        );
        assert_eq!(
            unknown.protected.writes.len(),
            unknown.evidence.artifacts.len()
        );
    }

    // -----------------------------------------------------------------------------------
    // Approximations.

    #[test]
    fn preservation_records_its_walk_approximation() {
        let mut restore = Restore::new(true);
        restore.replace_live();
        let found = restore.diagnose();
        assert!(
            found
                .evidence
                .approximations
                .contains(&Approximation::PreservationWalk)
        );
        assert!(!found.exact());
    }

    #[test]
    fn a_composite_tree_stamp_is_flagged() {
        // A second step moved a folder out of the old tree once it sat at the stage path:
        // undoing it moves a folder inside the tree whose stamp rule 3 then reads.
        let mut restore = Restore::new(true);
        let sub = restore.staged.join("Account");
        let out = client().join("out");
        let step =
            restore
                .world
                .record(2, StepOp::Noreplace, &sub, &out, StepPurpose::OldToSnapshot);
        restore.world.run(&step);
        restore.steps.push(step);
        let found = restore.diagnose();
        assert!(
            found
                .evidence
                .approximations
                .contains(&Approximation::CompositeTreeStamp),
            "{found:#?}"
        );
        assert!(!found.exact());
        assert!(
            Restore::new(true).diagnose().exact(),
            "a whole tree moved whole is exact"
        );
    }

    #[test]
    fn a_wtf_path_without_the_flag_is_flagged() {
        let mut restore = Restore::new(true);
        restore.prepared.writes_wtf = false;
        let found = restore.diagnose();
        assert_eq!(found.verdict, BACK, "{found:#?}");
        assert_eq!(
            found.evidence.approximations,
            [Approximation::WtfPathWithoutFlag]
        );
        assert!(
            Folder::new(6, false).diagnose(&[]).exact(),
            "folder steps never name WTF/"
        );
    }

    // -----------------------------------------------------------------------------------
    // Reads and determinism.

    /// A base that counts every question it is asked, by question and path.
    struct Counting<'b> {
        base: &'b MapBase,
        asked: RefCell<BTreeMap<String, usize>>,
    }

    impl Counting<'_> {
        fn ask(&self, question: &str, key: impl std::fmt::Debug) {
            *self
                .asked
                .borrow_mut()
                .entry(format!("{question} {key:?}"))
                .or_default() += 1;
        }
    }

    impl Base for Counting<'_> {
        fn entry(&self, path: &Path) -> Option<LiveEntry> {
            self.ask("entry", path);
            self.base.entry(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.ask("is_dir", path);
            self.base.is_dir(path)
        }

        fn file_stamp(&self, path: &Path) -> Option<String> {
            self.ask("file_stamp", path);
            self.base.file_stamp(path)
        }

        fn tree_stamp(&self, path: &Path) -> std::io::Result<String> {
            self.ask("tree_stamp", path);
            self.base.tree_stamp(path)
        }

        fn owns_client_dir(&self, prepared: &Prepared) -> bool {
            self.ask("owns", (&prepared.client_dir, prepared.target_key));
            self.base.owns_client_dir(prepared)
        }

        fn writer_alive(&self, tx: &TxId) -> Result<bool, String> {
            self.ask("alive", tx);
            self.base.writer_alive(tx)
        }

        fn describable(&self, prepared: &Prepared) -> bool {
            self.ask("describable", prepared.tx);
            self.base.describable(prepared)
        }

        fn backup_meta_loads(&self, dir: &Path) -> bool {
            self.ask("meta", dir);
            self.base.backup_meta_loads(dir)
        }

        fn marker(&self, client_dir: &Path) -> Option<TargetKey> {
            self.ask("marker", client_dir);
            self.base.marker(client_dir)
        }
    }

    #[test]
    fn each_path_is_read_once() {
        let folder = Folder::new(6, false);
        let counting = Counting {
            base: &folder.world.base,
            asked: RefCell::new(BTreeMap::new()),
        };
        let listing = vec![Listed {
            tx: folder.tx,
            path: paths().journal(&folder.tx),
            text: Ok(text(&folder.prepared, &folder.steps, &[])),
        }];
        let found = diagnose_journals(&counting, &paths(), listing, &[client()], &|_| true);
        assert_eq!(found[0].verdict, BACK);
        let asked = counting.asked.into_inner();
        let twice: Vec<_> = asked.iter().filter(|(_, n)| **n > 1).collect();
        assert!(twice.is_empty(), "asked more than once: {twice:#?}");
        // The cache counts what reached the base.
        let mut sim = Simulated::new(&folder.world.base);
        for _ in 0..3 {
            sim.entry(&folder.steps[0].to);
        }
        assert_eq!(sim.cache.reads(), 1);
    }

    #[test]
    fn two_diagnoses_are_equal() {
        let mut restore = Restore::new(true);
        restore.replace_live();
        assert_eq!(restore.diagnose(), restore.diagnose());
        let folder = Folder::new(4, true);
        assert_eq!(folder.diagnose(&[]), folder.diagnose(&[]));
    }

    // -----------------------------------------------------------------------------------
    // On disk.

    #[test]
    fn diagnose_on_disk_writes_nothing() {
        use crate::engine::testing::{Scene, tree_snapshot};
        use crate::fsx::{Op, RenameMode, mutate};
        let mut scene = Scene::new();
        {
            let lock = scene.lock();
            let mut s = scene.session();
            let mut journal = scene.journal(&mut s, &lock);
            let step = journal::Step {
                op: StepOp::Noreplace,
                from: scene.staged.stage_dir.join("Bar"),
                to: scene.client.addons().join("Bar"),
                purpose: StepPurpose::SwapIn,
            };
            journal.step(&mut s, step.clone()).unwrap();
            let mode = RenameMode::NoReplace;
            mutate(&mut s, &step.from, Op::Rename { to: &step.to, mode }).unwrap();
        }
        scene.release_inflight();
        let roots = [
            scene.paths.state_dir().to_path_buf(),
            scene.client.client_dir().to_path_buf(),
        ];
        let before = roots.each_ref().map(|root| tree_snapshot(root));
        let clients = [scene.client.client_dir().to_path_buf()];
        let found = diagnose(&scene.paths, &clients, &|_| true);
        assert_eq!(
            roots.each_ref().map(|root| tree_snapshot(root)),
            before,
            "nothing written"
        );
        assert_eq!(found.unlisted, None);
        assert_eq!(found.journals.len(), 1, "{found:#?}");
        let one = &found.journals[0];
        assert_eq!(
            (one.verdict, one.evidence.ran.as_slice(), one.expected()),
            (BACK, &[1][..], Expected::RolledBack),
            "{one:#?}"
        );
        assert_eq!(
            diagnose(&scene.paths, &clients, &|_| true),
            found,
            "deterministic"
        );
    }

    #[test]
    fn diagnose_on_disk_writes_nothing_for_a_settings_restore() {
        use crate::engine::journal::{ClientFacts, Journal, Step};
        use crate::engine::testing::{Scene, tree_snapshot};
        use crate::fsx::{Op, RenameMode, WriteSession, Writer, mutate};
        let mut scene = Scene::new();
        let staged = scene.staged.stage_dir.join("WTF");
        std::fs::create_dir_all(staged.join("Account")).unwrap();
        std::fs::write(staged.join("Account/restored.txt"), "from the backup").unwrap();
        let stamp = StampDigest::compute(&staged).unwrap().to_string();
        {
            let lock = scene.lock();
            let mut prepared = {
                let mut probe = scene.session();
                let journal = scene.journal(&mut probe, &lock);
                let text = std::fs::read_to_string(journal.path()).unwrap();
                std::fs::remove_file(journal.path()).unwrap();
                journal::parse(&text).prepared.unwrap()
            };
            prepared.kind = TxKind::Restore;
            prepared.writes_wtf = true;
            prepared.staged_stamp = Some(stamp);
            prepared.client = Some(ClientFacts {
                target_id: "retail".into(),
                family: crate::rules::Family::Mainline,
                version: "12.1.5.70391".into(),
            });
            let mut s = WriteSession::untracked(Writer::BackupRestore, &scene.scopes, "test");
            let mut journal = Journal::create(&mut s, &scene.paths, &lock, prepared).unwrap();
            let live = scene.client.wtf();
            let step = Step {
                op: StepOp::Exchange,
                from: staged.clone(),
                to: live.clone(),
                purpose: StepPurpose::SwapIn,
            };
            journal.step(&mut s, step).unwrap();
            let op = Op::Rename {
                to: &live,
                mode: RenameMode::Exchange,
            };
            mutate(&mut s, &staged, op).unwrap();
        }
        scene.release_inflight();
        let roots = [
            scene.paths.state_dir().to_path_buf(),
            scene.client.client_dir().to_path_buf(),
        ];
        let before = roots.each_ref().map(|root| tree_snapshot(root));
        let clients = [scene.client.client_dir().to_path_buf()];
        let found = diagnose(&scene.paths, &clients, &|_| true);
        assert_eq!(
            roots.each_ref().map(|root| tree_snapshot(root)),
            before,
            "nothing written"
        );
        let one = &found.journals[0];
        // Unplayed, but with no source backup the stage holds that content's last copy.
        assert_eq!(
            (
                one.verdict,
                one.evidence.ran.as_slice(),
                one.evidence.preservation.map(|p| p.items)
            ),
            (BACK, &[1][..], Some(1)),
            "{one:#?}"
        );
        for path in named_paths(one) {
            assert!(!below_wtf(&path), "{} is below WTF/", path.display());
        }
    }

    #[test]
    fn an_unlistable_journal_directory_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let scene = crate::engine::testing::Scene::new();
        let dir = scene.paths.journal_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let found = diagnose(&scene.paths, &[], &|_| true);
        std::fs::set_permissions(&dir, mode).unwrap();
        assert!(found.journals.is_empty(), "{found:#?}");
        assert_eq!(found.unlisted.map(|failure| failure.dir), Some(dir));
    }

    // -----------------------------------------------------------------------------------
    // The fuzz property.

    #[test]
    fn fuzz_property_accepts_the_shapes_it_is_built_for() {
        let folder = Folder::new(3, true);
        let prefix = text(&folder.prepared, &folder.steps, &[]);
        let torn = &prefix[..prefix.len() - 7];
        let mut restore = Restore::new(true);
        restore.replace_live();
        let wtf = text(&restore.prepared, &restore.steps, &[]);
        let cases: [(&str, Vec<u8>); 6] = [
            ("six_step_prefix", prefix.clone().into_bytes()),
            ("torn", torn.as_bytes().to_vec()),
            ("whole_restore", wtf.into_bytes()),
            ("empty", Vec::new()),
            (
                "not_utf8",
                b"\xff\xfe{\"type\":\"prepared\"\x80}\n\xc3".to_vec(),
            ),
            ("braces", vec![b'{'; 64 * 1024]),
        ];
        for (name, bytes) in cases {
            let found = fuzz_diagnosis(&bytes);
            assert_eq!(found.len(), 1, "{name}");
            fuzz_property(&bytes);
        }
    }

    #[test]
    fn the_disk_arm_sees_nothing_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        let folder = Folder::new(3, false);
        let journal = paths.journal(&folder.tx);
        std::fs::write(&journal, text(&folder.prepared, &folder.steps, &[])).unwrap();
        fuzz_disk_property(&paths);
        assert_eq!(diagnose(&paths, &[], &|_| false).journals.len(), 1);
    }

    // -----------------------------------------------------------------------------------
    // Purity.

    /// The doctor's detectors' rule: read, never write.
    #[test]
    fn the_simulation_never_writes() {
        let source = include_str!("simulate.rs");
        let production = source.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        let writes = [
            "WriteSession",
            "mutate(",
            "mutate_with(",
            "prune(",
            "fs::write",
            "fs::rename",
            "fs::remove",
            "fs::create",
            "OpenOptions",
        ];
        let found: Vec<String> = production
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim_start().starts_with("//"))
            .flat_map(|(at, line)| {
                writes
                    .iter()
                    .filter(move |write| line.contains(*write))
                    .map(move |write| {
                        format!(
                            "src/engine/simulate.rs:{} writes ({write}): {}",
                            at + 1,
                            line.trim()
                        )
                    })
            })
            .collect();
        assert!(found.is_empty(), "{}", found.join("\n"));
        assert!(
            production.len() > 10_000,
            "the scan sees the production code"
        );
    }
}
