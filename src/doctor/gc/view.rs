//! The retention plan's input (M4-D36): everything the doctor keeps (its run
//! directories, the `.repair` client halves of repair transactions, the cache
//! quarantines), and everything that may still need it (unfinished journals, live
//! in-flight locks, history's repairs and set-asides, history lines that do not read),
//! read once by [`GcView::load`].
//!
//! Reads only. Nothing here writes, takes a lock or makes a request, and every listing is
//! bounded by entries alone, never by time, so a preview and its bound run read the same
//! home the same way. What cannot be read is never taken for "nothing to protect": a
//! repair journal that does not read pins everything (`unknowable_journal`), a lock that
//! cannot be probed counts as held, and a run whose report does not read has no terminal
//! state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::plan::DeletionKind;
use crate::doctor::budget::{self, Budget, EntryKind, WalkStep};
use crate::doctor::finding::FindingTarget;
use crate::doctor::runs::{self, RunSummary};
use crate::doctor::runtime::DoctorRunId;
use crate::engine::journal::{
    self as journals, JournalFamily, JournalVerdict, UnattributedCause, no_saved_variables,
};
use crate::engine::repair::journal::{self as repair_journal, half_tx};
use crate::engine::repair::{JournalName, PreparedRead, guard, journal_file};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{FileId, InflightLock, canonical_lenient};
use crate::limits::API_RESPONSE_MAX_BYTES;
use crate::model::{TargetKey, TxId};
use crate::output::RunKind;
use crate::state::history::{self, HistoryEntry, HistoryFilter, RepairOp};
use crate::state::paths::{ClientPaths, Paths};

/// A registered client: its directory and its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub dir: PathBuf,
    pub target: FindingTarget,
}

/// What [`GcView::load`] reads with.
#[derive(Debug, Clone, Copy)]
pub struct GcInputs<'a> {
    pub paths: &'a Paths,
    /// The registered clients: only their halves are seen (the scope table admits no
    /// other client's).
    pub clients: &'a [Client],
    pub now: Timestamp,
    /// What started before it is eligible.
    pub before: Timestamp,
    /// Entries each listing, and each candidate's size walk, may read.
    pub bound: u64,
}

/// Everything the retention plan decides from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcView {
    pub now: Timestamp,
    pub before: Timestamp,
    /// Every run, and every repair half no run claims, with what it holds.
    pub groups: Vec<Group>,
    /// Every unfinished journal of either family.
    pub journals: Vec<JournalView>,
    /// Every history line with repair operations: repairs, and undos of repairs.
    pub repairs: Vec<RepairLineView>,
    /// The raw text of every history line that does not read.
    pub unreadable_history: Vec<String>,
    pub latest: LatestState,
    /// A repair journal this build cannot read (a newer format, or a line it cannot
    /// decode): what it protects is unknown, so it pins everything.
    pub unknowable_journal: Option<TxId>,
    /// False when a listing stopped at its bound or could not be read: more may exist.
    pub listings_complete: bool,
}

/// What a group is named for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Anchor {
    /// A doctor run, by its id: its directory (or its stage), its cache quarantine and
    /// the halves of its transactions, whichever exist.
    Run(DoctorRunId),
    /// A repair's halves that no run, history line or journal claims.
    Half(TxId),
}

impl Anchor {
    /// The name groups of one age are ordered by.
    pub fn name(&self) -> String {
        match self {
            Self::Run(id) => id.as_str().to_owned(),
            Self::Half(tx) => tx.to_string(),
        }
    }

    pub fn run(&self) -> Option<&DoctorRunId> {
        match self {
            Self::Run(id) => Some(id),
            Self::Half(_) => None,
        }
    }
}

/// One run, or one orphaned repair, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub anchor: Anchor,
    /// The run id's time, or an orphan's transaction's. `None` when the id names no
    /// instant: never eligible.
    pub started_at: Option<Timestamp>,
    /// Its transactions, sorted: the report's, and those its history lines and repair
    /// journals name.
    pub txs: Vec<TxId>,
    /// The run's own transaction: a fix run's repair or an undo run's undo, else the
    /// first of `txs`.
    pub tx: Option<TxId>,
    /// One of its transactions' in-flight locks is held, or cannot be probed.
    pub live: bool,
    /// Its `report.json`, when it reads.
    pub report: Option<RunSummary>,
    pub candidates: Vec<Candidate>,
}

/// Something gc may delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: DeletionKind,
    /// As the doctor names it: what the report prints and what deletion takes.
    pub path: PathBuf,
    /// Resolved through the filesystem: what every overlap compares.
    pub canonical: PathBuf,
    pub identity: FileId,
    /// The apparent size of its regular files; `None` past the bound, or when a
    /// directory in it could not be read.
    pub bytes: Option<u64>,
    /// It holds what something kept: a run's non-empty `backups/` or `quarantine/`, a
    /// half's non-empty `files/` or `quarantine/`.
    pub kept_bytes: bool,
    /// A half's own transaction.
    pub tx: Option<TxId>,
    /// A half's client.
    pub target: Option<FindingTarget>,
    /// A run under its stage name, `.<id>.tmp`.
    pub staged: bool,
}

/// One unfinished journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalView {
    pub tx: TxId,
    pub family: JournalFamily,
    /// A repair journal of the undo of a repair (its `prepared` line names `reverts`).
    pub undo_of_repair: bool,
    /// What recovery may read or write for it, resolved.
    pub protected: Vec<PathBuf>,
}

/// One history line with repair operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairLineView {
    pub tx: TxId,
    /// A later line reverts it: an undo committed.
    pub reverted: bool,
    pub ops: Vec<RepairOpView>,
    /// What its set-aside guards read now; `Hold` for a line with none.
    pub guards: GuardState,
    /// The repair journals it set aside, as read where it put them.
    pub suspended: Vec<SuspendedRepair>,
}

/// One operation of a repair line, as gc needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairOpView {
    pub op: RepairOp,
    /// A rename's destination, resolved.
    pub to: Option<PathBuf>,
    /// The destination exists now.
    pub to_exists: bool,
    /// The backup an overwriting write reserved, resolved.
    pub backup: Option<PathBuf>,
    /// It carries a set-aside's suspension manifest.
    pub guarded: bool,
}

/// A set-aside's guards, read now as strict undo reads them before it moves anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardState {
    /// Every guard reads what it recorded: the set-aside can be reactivated.
    Hold,
    /// A guard reads something else: a later change made reactivation impossible.
    Changed,
    /// A guard could not be read.
    Unknown,
}

/// A repair journal a set-aside suspended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendedRepair {
    pub tx: TxId,
    /// The doctor run its `prepared` line names, whose backups its rollback reads; `None`
    /// when that line cannot be read.
    pub run: Option<DoctorRunId>,
}

/// `<state>/doctor/latest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestState {
    Missing,
    /// Something that is not a symlink: never touched.
    NotSymlink,
    Link {
        /// The run it names, when its target is `runs/<id>`.
        names: Option<DoctorRunId>,
        /// It leads to a directory.
        resolves: bool,
    },
}

impl GcView {
    /// Reads everything the plan needs, and writes nothing. Fails only when a store gc
    /// must see whole cannot be read: the runs directory, the cache quarantine, the
    /// journal directory or the history.
    pub fn load(inputs: &GcInputs<'_>) -> Result<Self> {
        let paths = inputs.paths;
        let bound = inputs.bound;
        let mut complete = true;

        let listing = runs::list_runs(paths, &mut Budget::entries_only(bound))
            .map_err(|e| io_error(&paths.doctor_runs_dir(), &e))?;
        complete &= listing.complete;

        let mut halves: Vec<(TxId, PathBuf, FindingTarget)> = Vec::new();
        for client in inputs.clients {
            let dir = ClientPaths::new(&client.dir).snapshots_dir();
            match budget::list_dir(&dir, &mut Budget::entries_only(bound)) {
                Ok(found) => {
                    complete &= found.complete;
                    halves.extend(
                        found
                            .entries
                            .into_iter()
                            .filter(|(_, kind)| *kind == EntryKind::Dir)
                            .filter_map(|(name, _)| {
                                let tx = half_tx(name.to_str()?)?;
                                Some((tx, dir.join(&name), client.target.clone()))
                            }),
                    );
                }
                // ubs:ignore — an io::ErrorKind, not a secret
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // A client whose snapshots cannot be listed: its halves are not seen, so
                // nothing of them is deleted.
                Err(_) => complete = false,
            }
        }

        let quarantine_dir = paths.cache_quarantine_dir();
        let mut quarantines: Vec<(DoctorRunId, PathBuf)> = Vec::new();
        match budget::list_dir(&quarantine_dir, &mut Budget::entries_only(bound)) {
            Ok(found) => {
                complete &= found.complete;
                quarantines.extend(
                    found
                        .entries
                        .into_iter()
                        .filter(|(_, kind)| *kind == EntryKind::Dir)
                        .filter_map(|(name, _)| {
                            let id = DoctorRunId::parse(name.to_str()?)?;
                            Some((id, quarantine_dir.join(&name)))
                        }),
                );
            }
            // ubs:ignore — an io::ErrorKind, not a secret
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(&quarantine_dir, &e)),
        }

        let repairs_found = read_repair_journals(paths)?;
        let mut unknowable = repairs_found.unknowable;

        let client_dirs: Vec<PathBuf> = inputs.clients.iter().map(|c| c.dir.clone()).collect();
        let diagnoses = journals::diagnose(paths, &client_dirs, &no_saved_variables);
        if let Some(unlisted) = &diagnoses.unlisted {
            return Err(Error::new(
                ErrorCode::Io,
                format!(
                    "{} cannot be listed: {}",
                    unlisted.dir.display(),
                    unlisted.error
                ),
                Suggestion::command(["omaddons", "status"]),
            )
            .details(serde_json::json!({ "path": unlisted.dir })));
        }
        let mut journal_views = Vec::new();
        for diagnosis in &diagnoses.journals {
            if diagnosis.family == JournalFamily::Repair && unknowable_verdict(diagnosis.verdict) {
                unknowable.insert(diagnosis.tx);
            }
            let protected: BTreeSet<PathBuf> = diagnosis
                .protected
                .reads
                .iter()
                .chain(&diagnosis.protected.writes)
                .map(|path| resolved(path))
                .collect();
            journal_views.push(JournalView {
                tx: diagnosis.tx,
                family: diagnosis.family,
                undo_of_repair: diagnosis.family == JournalFamily::Repair
                    && repairs_found.undos.contains(&diagnosis.tx),
                protected: protected.into_iter().collect(),
            });
        }
        journal_views.sort_by(|a, b| (a.tx, &a.protected).cmp(&(b.tx, &b.protected)));

        let read = history::read(paths, &HistoryFilter::default())?;
        let reverted: BTreeSet<TxId> = read.entries.iter().filter_map(|e| e.reverts).collect();
        let guard_clients: Vec<(TargetKey, PathBuf)> = inputs
            .clients
            .iter()
            .map(|c| (c.target.key, c.dir.clone()))
            .collect();
        let mut line_runs: Vec<(TxId, DoctorRunId)> = Vec::new();
        let mut repair_lines = Vec::new();
        for entry in &read.entries {
            if let Some(run) = entry.doctor_run_id.as_deref().and_then(DoctorRunId::parse) {
                line_runs.push((entry.tx, run));
            }
            if entry.repairs.is_empty() {
                continue;
            }
            repair_lines.push(repair_line(
                paths,
                entry,
                reverted.contains(&entry.tx),
                &guard_clients,
            ));
        }
        repair_lines.sort_by_key(|line| line.tx);
        let unreadable_history: Vec<String> = read
            .unreadable
            .iter()
            .map(|line| line.raw.clone())
            .collect();

        // Which run each transaction belongs to: its report, its history line, its journal.
        let summaries: BTreeMap<&DoctorRunId, RunSummary> = listing
            .runs
            .iter()
            .filter_map(|run| Some((&run.id, runs::read_summary(&run.dir)?)))
            .fold(BTreeMap::new(), |mut found, (id, summary)| {
                found.entry(id).or_insert(summary);
                found
            });
        let mut owner: BTreeMap<TxId, DoctorRunId> = BTreeMap::new();
        for (id, summary) in &summaries {
            if let Some(tx) = own_tx(summary) {
                owner.entry(tx).or_insert_with(|| (*id).clone());
            }
        }
        for (tx, run) in line_runs.iter().chain(&repairs_found.runs) {
            owner.entry(*tx).or_insert_with(|| run.clone());
        }

        let mut builders: BTreeMap<Anchor, Builder> = BTreeMap::new();
        for run in &listing.runs {
            run_group(&mut builders, &run.id)
                .runs
                .push((run.dir.clone(), run.staged));
        }
        for (id, dir) in &quarantines {
            run_group(&mut builders, id).quarantine = Some(dir.clone());
        }
        for (tx, id) in &owner {
            run_group(&mut builders, id).txs.insert(*tx);
        }
        for (id, summary) in &summaries {
            run_group(&mut builders, id).report = Some(summary.clone());
        }
        for (tx, dir, target) in halves {
            let anchor = owner
                .get(&tx)
                .map_or(Anchor::Half(tx), |id| Anchor::Run(id.clone()));
            let builder = builders.entry(anchor.clone()).or_insert_with(|| Builder {
                started_at: tx_time(tx),
                ..Builder::default()
            });
            builder.txs.insert(tx);
            builder.halves.push((tx, dir, target));
        }

        let mut groups = Vec::new();
        for (anchor, builder) in builders {
            let group = builder.build(paths, anchor, bound);
            if !group.candidates.is_empty() {
                groups.push(group);
            }
        }

        Ok(Self {
            now: inputs.now,
            before: inputs.before,
            groups,
            journals: journal_views,
            repairs: repair_lines,
            unreadable_history,
            latest: latest_state(paths),
            unknowable_journal: unknowable.first().copied(),
            listings_complete: complete,
        })
    }
}

/// What the repair journals say of themselves.
#[derive(Default)]
struct RepairJournals {
    /// Each journal whose `prepared` line names a doctor run.
    runs: Vec<(TxId, DoctorRunId)>,
    /// The journals of undos of repairs.
    undos: BTreeSet<TxId>,
    /// The journals this build cannot read.
    unknowable: BTreeSet<TxId>,
}

/// Every repair journal, read for its run and whether it is an undo. A journal that
/// cannot be read, or whose first line is too new or does not decode, is unknowable.
fn read_repair_journals(paths: &Paths) -> Result<RepairJournals> {
    let mut found = RepairJournals::default();
    for (tx, path) in repair_journal::list(paths)? {
        let Ok(parsed) = repair_journal::read(paths, &path) else {
            found.unknowable.insert(tx);
            continue;
        };
        match parsed.prepared_read() {
            PreparedRead::Decoded(prepared) => {
                if prepared.reverts.is_some() {
                    found.undos.insert(tx);
                }
                if let Some(run) = DoctorRunId::parse(&prepared.doctor_run_id) {
                    found.runs.push((tx, run));
                }
            }
            PreparedRead::TooNew | PreparedRead::Undecodable => {
                found.unknowable.insert(tx);
            }
            PreparedRead::Absent => {}
        }
    }
    Ok(found)
}

/// A repair journal's verdict that says this build cannot read it.
const fn unknowable_verdict(verdict: JournalVerdict) -> bool {
    matches!(
        verdict,
        JournalVerdict::TooNew
            | JournalVerdict::Unreadable
            | JournalVerdict::Unattributed {
                cause: UnattributedCause::TooNew
                    | UnattributedCause::Unreadable
                    | UnattributedCause::ReadFailed
            }
    )
}

/// The transaction a run's report names as the run's own: a fix's repair, an undo's undo.
fn own_tx(summary: &RunSummary) -> Option<TxId> {
    match summary.kind? {
        RunKind::Fix => summary.tx,
        RunKind::Undo => summary.undo_tx,
        RunKind::Diagnose => None,
    }
}

/// When a transaction started: its id's time.
fn tx_time(tx: TxId) -> Option<Timestamp> {
    let ms = i64::try_from(tx.ulid().timestamp_ms()).ok()?;
    Timestamp::from_millisecond(ms).ok()
}

/// `path` resolved through the filesystem as far as it exists, for comparing paths
/// recorded in different forms; as given when nothing of it resolves.
fn resolved(path: &Path) -> PathBuf {
    canonical_lenient(path).unwrap_or_else(|| path.to_path_buf())
}

fn io_error(path: &Path, e: &io::Error) -> Error {
    Error::io(path, e, Suggestion::command(["omaddons", "status"]))
}

/// A history line with repair operations, as gc needs it: what its inverse reads, and,
/// for a set-aside not undone, its guards read now and the repair journals it suspended.
fn repair_line(
    paths: &Paths,
    entry: &HistoryEntry,
    reverted: bool,
    clients: &[(TargetKey, PathBuf)],
) -> RepairLineView {
    let ops: Vec<RepairOpView> = entry
        .repairs
        .iter()
        .map(|op| {
            let to = op.to.as_deref().map(resolved);
            RepairOpView {
                op: op.op,
                to_exists: op
                    .to
                    .as_deref()
                    .is_some_and(|to| fs::symlink_metadata(to).is_ok()),
                to,
                backup: op.backup.as_deref().map(resolved),
                guarded: !op.guards.is_empty(),
            }
        })
        .collect();
    let set_aside = !reverted && entry.repairs.iter().any(|op| !op.guards.is_empty());
    let guards = if set_aside {
        guard_state(entry, clients, paths)
    } else {
        GuardState::Hold
    };
    let suspended = if set_aside {
        entry
            .repairs
            .iter()
            .filter(|op| op.op == RepairOp::RenameNoReplace && !op.guards.is_empty())
            .filter_map(|op| {
                let (tx, name) = journal_file(paths, &op.path)?;
                let to = op.to.as_deref()?;
                (name == JournalName::Repair && fs::symlink_metadata(to).is_ok()).then(|| {
                    SuspendedRepair {
                        tx,
                        run: suspended_run(paths, to),
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    RepairLineView {
        tx: entry.tx,
        reverted,
        ops,
        guards,
        suspended,
    }
}

/// Every guard of a set-aside's line, read now: `Unknown` when one cannot be read (fail
/// closed: such a set-aside is kept), else `Changed` when one differs, else `Hold`.
fn guard_state(
    entry: &HistoryEntry,
    clients: &[(TargetKey, PathBuf)],
    paths: &Paths,
) -> GuardState {
    let mut changed = false;
    for recorded in entry.repairs.iter().flat_map(|op| &op.guards) {
        match guard::current(recorded, clients, paths) {
            Err(_) => return GuardState::Unknown,
            Ok(value) => changed |= value != guard::recorded(recorded),
        }
    }
    if changed {
        GuardState::Changed
    } else {
        GuardState::Hold
    }
}

/// The doctor run a suspended repair journal's `prepared` line names, read where the
/// set-aside put it; `None` when it cannot be read whole.
fn suspended_run(paths: &Paths, journal: &Path) -> Option<DoctorRunId> {
    let mut bytes = Vec::new();
    fs::File::open(journal)
        .and_then(|file| {
            file.take(API_RESPONSE_MAX_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .ok()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > API_RESPONSE_MAX_BYTES {
        return None;
    }
    let parsed = repair_journal::parse(&String::from_utf8_lossy(&bytes)).checked(paths);
    match parsed.prepared_read() {
        PreparedRead::Decoded(prepared) => DoctorRunId::parse(&prepared.doctor_run_id),
        _ => None,
    }
}

/// What `<state>/doctor/latest` is.
fn latest_state(paths: &Paths) -> LatestState {
    let latest = paths.doctor_latest();
    let Ok(meta) = fs::symlink_metadata(&latest) else {
        return LatestState::Missing;
    };
    if !meta.file_type().is_symlink() {
        return LatestState::NotSymlink;
    }
    let names = fs::read_link(&latest).ok().and_then(|target| {
        let id = target.strip_prefix("runs").ok()?.to_str()?;
        DoctorRunId::parse(id)
    });
    LatestState::Link {
        names,
        resolves: fs::metadata(&latest).is_ok_and(|meta| meta.is_dir()),
    }
}

/// The group of run `id`, made when it is first named.
fn run_group<'a>(builders: &'a mut BTreeMap<Anchor, Builder>, id: &DoctorRunId) -> &'a mut Builder {
    builders
        .entry(Anchor::Run(id.clone()))
        .or_insert_with(|| Builder {
            started_at: runs::started_at(id),
            ..Builder::default()
        })
}

/// A group as it is collected.
#[derive(Default)]
struct Builder {
    started_at: Option<Timestamp>,
    txs: BTreeSet<TxId>,
    report: Option<RunSummary>,
    /// Its directory and its stage, either of which a crash may leave.
    runs: Vec<(PathBuf, bool)>,
    quarantine: Option<PathBuf>,
    halves: Vec<(TxId, PathBuf, FindingTarget)>,
}

impl Builder {
    fn build(self, paths: &Paths, anchor: Anchor, bound: u64) -> Group {
        let txs: Vec<TxId> = self.txs.into_iter().collect();
        let live = txs
            .iter()
            .any(|tx| InflightLock::is_live(paths, tx).unwrap_or(true));
        let mut candidates = Vec::new();
        if let Some(dir) = &self.quarantine {
            candidates.extend(candidate(DeletionKind::CacheQuarantine, dir, bound));
        }
        let mut halves = self.halves;
        halves.sort_by(|a, b| a.1.cmp(&b.1));
        for (tx, dir, target) in halves {
            candidates.extend(candidate(DeletionKind::ClientRepair, &dir, bound).map(|c| {
                Candidate {
                    tx: Some(tx),
                    target: Some(target),
                    ..c
                }
            }));
        }
        let mut dirs = self.runs;
        dirs.sort();
        for (dir, staged) in dirs {
            candidates.extend(
                candidate(DeletionKind::Run, &dir, bound).map(|c| Candidate { staged, ..c }),
            );
        }
        let tx = self
            .report
            .as_ref()
            .and_then(own_tx)
            .or(txs.first().copied());
        Group {
            anchor,
            started_at: self.started_at,
            txs,
            tx,
            live,
            report: self.report,
            candidates,
        }
    }
}

/// The candidate at `path`, when something is there.
fn candidate(kind: DeletionKind, path: &Path, bound: u64) -> Option<Candidate> {
    let identity = FileId::of(path)?;
    let kept_bytes = match kind {
        DeletionKind::Run => {
            non_empty(&path.join("backups")) || non_empty(&path.join("quarantine"))
        }
        DeletionKind::ClientRepair => {
            non_empty(&path.join("files")) || non_empty(&path.join("quarantine"))
        }
        DeletionKind::CacheQuarantine => false,
    };
    Some(Candidate {
        kind,
        path: path.to_path_buf(),
        canonical: resolved(path),
        identity,
        bytes: apparent_bytes(path, bound),
        kept_bytes,
        tx: None,
        target: None,
        staged: false,
    })
}

/// Whether the directory holds anything; one that exists but cannot be read counts as
/// holding something.
fn non_empty(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

/// The apparent size of the regular files below `root`, a stat walk of at most `bound`
/// entries that reads no content; `None` past it, or when a directory could not be read.
/// Its depth needs no bound of its own: every level costs an entry.
fn apparent_bytes(root: &Path, bound: u64) -> Option<u64> {
    let mut total: u64 = 0;
    let walked = budget::walk(
        root,
        usize::MAX,
        &mut Budget::entries_only(bound),
        |entry| {
            if entry.kind == EntryKind::File {
                total = total.saturating_add(entry.meta.len());
            }
            WalkStep::Continue
        },
    );
    (walked.complete && walked.unreadable.is_empty()).then_some(total)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use ulid::Ulid;

    use super::*;
    use crate::doctor::gc::{Pin, plan};
    use crate::engine::repair::{FORMAT, RepairKind, RepairLine, RepairPrepared, ResourcesWire};
    use crate::engine::repair_action::Guard;
    use crate::engine::testing::{TreeEntry, tree_snapshot};
    use crate::limits::{DOCTOR_GC_MAX_DELETIONS, DOCTOR_SCAN_MAX_ENTRIES};
    use crate::model::{Actor, RunId, TxKind};
    use crate::state::history::{
        PredicateKind, RepairEndpoints, RepairEntry, RepairPredicate, TxResult,
    };

    const NOW: &str = "2026-11-30T00:00:00Z";

    /// A home with one registered client and one that is not.
    struct Home {
        _root: tempfile::TempDir,
        base: PathBuf,
        paths: Paths,
        client: ClientPaths,
        unregistered: ClientPaths,
        clients: Vec<Client>,
    }

    fn home() -> Home {
        let root = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let paths = Paths::with_roots(
            base.join("home"),
            base.join("config"),
            base.join("state"),
            base.join("cache"),
        );
        let client = ClientPaths::new(base.join("games/retail"));
        let unregistered = ClientPaths::new(base.join("games/classic"));
        for c in [&client, &unregistered] {
            fs::create_dir_all(c.snapshots_dir()).unwrap();
        }
        let clients = vec![Client {
            dir: client.client_dir().to_path_buf(),
            target: FindingTarget {
                id: "retail".to_owned(),
                key: TargetKey::from_ulid(Ulid::from_parts(1, 1)),
            },
        }];
        Home {
            _root: root,
            base,
            paths,
            client,
            unregistered,
            clients,
        }
    }

    impl Home {
        fn inputs(&self, bound: u64) -> GcInputs<'_> {
            let now: Timestamp = NOW.parse().unwrap();
            GcInputs {
                paths: &self.paths,
                clients: &self.clients,
                now,
                before: now
                    .checked_sub(jiff::SignedDuration::from_hours(24 * 30))
                    .unwrap(),
                bound,
            }
        }

        fn load(&self) -> GcView {
            GcView::load(&self.inputs(DOCTOR_SCAN_MAX_ENTRIES)).unwrap()
        }

        /// A published run with `report` as its `report.json` (none when `None`).
        fn run(&self, id: &DoctorRunId, report: Option<String>) -> PathBuf {
            let dir = self.paths.doctor_run(id);
            fs::create_dir_all(&dir).unwrap();
            if let Some(report) = report {
                fs::write(dir.join("report.json"), report).unwrap();
            }
            dir
        }

        /// `tx`'s half on `client`, holding one backup.
        fn half(&self, client: &ClientPaths, tx: TxId) -> PathBuf {
            let dir = client.snapshots_repair(&tx);
            fs::create_dir_all(dir.join("files")).unwrap();
            fs::write(dir.join("files/1"), "kept").unwrap();
            dir
        }

        fn cache_quarantine(&self, id: &DoctorRunId) -> PathBuf {
            let dir = self.paths.cache_quarantine(id.as_str());
            fs::create_dir_all(dir.join("blobs")).unwrap();
            fs::write(dir.join("blobs/ab"), "blob").unwrap();
            dir
        }

        fn repair_journal(
            &self,
            tx: TxId,
            run: &DoctorRunId,
            reverts: Option<TxId>,
            reads: &[String],
        ) {
            let path = self.paths.journal_repair(&tx);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, prepared_line(tx, run, reverts, reads)).unwrap();
        }

        fn history(&self, entries: &[HistoryEntry], unreadable: &[&str]) {
            let mut text = String::new();
            for entry in entries {
                text.push_str(&serde_json::to_string(entry).unwrap());
                text.push('\n');
            }
            for line in unreadable {
                text.push_str(line);
                text.push('\n');
            }
            fs::create_dir_all(self.paths.history_file().parent().unwrap()).unwrap();
            fs::write(self.paths.history_file(), text).unwrap();
        }
    }

    fn run_id(time: &str, n: u128) -> DoctorRunId {
        let at: Timestamp = time.parse().unwrap();
        DoctorRunId::new(
            at,
            Ulid::from_parts(u64::try_from(at.as_millisecond()).unwrap(), n),
        )
    }

    fn tx_at(time: &str, n: u128) -> TxId {
        let at: Timestamp = time.parse().unwrap();
        TxId::from_ulid(Ulid::from_parts(
            u64::try_from(at.as_millisecond()).unwrap(),
            n,
        ))
    }

    fn report(kind: &str, state: &str, tx: Option<TxId>) -> Option<String> {
        let tx = tx.map_or_else(|| "null".to_owned(), |tx| format!("\"{tx}\""));
        Some(format!(
            r#"{{"schema_version":"1.0","kind":"{kind}","state":"{state}","tx":{tx}}}"#
        ))
    }

    /// A repair journal's `prepared` line with no primitive: a repair of run `run`, or
    /// the undo of `reverts`, declaring `reads`.
    fn prepared_line(
        tx: TxId,
        run: &DoctorRunId,
        reverts: Option<TxId>,
        reads: &[String],
    ) -> String {
        let prepared = RepairPrepared {
            format: FORMAT,
            tx,
            run: RunId::new(),
            doctor_run_id: run.as_str().to_owned(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".to_owned(), "doctor".to_owned(), "fix".to_owned()],
            at: NOW.parse().unwrap(),
            targets: Vec::new(),
            client_dirs: Vec::new(),
            resources: ResourcesWire {
                reads: reads.to_vec(),
                writes: Vec::new(),
            },
            fixes: Vec::new(),
            primitives: Vec::new(),
            rollbacks: Vec::new(),
            kind: if reverts.is_some() {
                RepairKind::Undo
            } else {
                RepairKind::Repair
            },
            reverts,
            plan: None,
        };
        format!(
            "{}\n",
            serde_json::to_string(&RepairLine::Prepared(Box::new(prepared))).unwrap()
        )
    }

    fn predicate() -> RepairPredicate {
        RepairPredicate {
            kind: PredicateKind::Absent,
            identity: None,
            sha256: None,
            mode: None,
            stamp: None,
            link_target: None,
        }
    }

    fn rename(path: PathBuf, to: PathBuf, guards: Vec<Guard>) -> RepairEntry {
        RepairEntry {
            seq: 1,
            fix: 1,
            fm: "fm-test-suspend".to_owned(),
            op: RepairOp::RenameNoReplace,
            path,
            to: Some(to),
            mode: None,
            pre: RepairEndpoints::Path(predicate()),
            post: RepairEndpoints::Path(predicate()),
            backup: None,
            activation: false,
            guards,
            target: None,
            folder: None,
        }
    }

    fn entry(
        tx: TxId,
        kind: TxKind,
        run: Option<&DoctorRunId>,
        repairs: Vec<RepairEntry>,
        reverts: Option<TxId>,
    ) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx,
            kind,
            at: NOW.parse().unwrap(),
            target: None,
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".to_owned()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: run.map(|id| id.as_str().to_owned()),
            repairs,
            transitions: Vec::new(),
        }
    }

    /// Each group as (anchor, its candidates' kinds and paths).
    fn shape(view: &GcView) -> Vec<(Anchor, Vec<(DeletionKind, PathBuf)>)> {
        view.groups
            .iter()
            .map(|g| {
                (
                    g.anchor.clone(),
                    g.candidates
                        .iter()
                        .map(|c| (c.kind, c.path.clone()))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn groups_halves_and_cache_quarantine_by_run() {
        let h = home();
        // A: the report names its repair.
        let a = run_id("2026-09-01T00:00:00Z", 1);
        let ta = tx_at("2026-09-01T00:00:01Z", 1);
        let a_dir = h.run(&a, report("fix", "DONE_OK", Some(ta)));
        let a_half = h.half(&h.client, ta);
        let a_cache = h.cache_quarantine(&a);
        // B: a history line names it.
        let b = run_id("2026-09-02T00:00:00Z", 2);
        let tb = tx_at("2026-09-02T00:00:01Z", 2);
        let b_dir = h.run(&b, report("diagnose", "DONE_FINDINGS", None));
        let b_half = h.half(&h.client, tb);
        h.history(
            &[entry(tb, TxKind::Repair, Some(&b), Vec::new(), None)],
            &[],
        );
        // C: only its journal names it.
        let c = run_id("2026-09-03T00:00:00Z", 3);
        let tc = tx_at("2026-09-03T00:00:01Z", 3);
        let c_dir = h.run(&c, None);
        let c_half = h.half(&h.client, tc);
        h.repair_journal(tc, &c, None, &[]);
        let view = h.load();
        assert_eq!(
            shape(&view),
            vec![
                (
                    Anchor::Run(a.clone()),
                    vec![
                        (DeletionKind::CacheQuarantine, a_cache),
                        (DeletionKind::ClientRepair, a_half),
                        (DeletionKind::Run, a_dir),
                    ]
                ),
                (
                    Anchor::Run(b.clone()),
                    vec![
                        (DeletionKind::ClientRepair, b_half),
                        (DeletionKind::Run, b_dir)
                    ]
                ),
                (
                    Anchor::Run(c.clone()),
                    vec![
                        (DeletionKind::ClientRepair, c_half),
                        (DeletionKind::Run, c_dir)
                    ]
                ),
            ],
            "{view:#?}"
        );
        let txs: Vec<(Vec<TxId>, Option<TxId>)> =
            view.groups.iter().map(|g| (g.txs.clone(), g.tx)).collect();
        assert_eq!(
            txs,
            vec![
                (vec![ta], Some(ta)),
                (vec![tb], Some(tb)),
                (vec![tc], Some(tc))
            ]
        );
        assert_eq!(
            view.groups[0].started_at,
            Some("2026-09-01T00:00:00Z".parse().unwrap())
        );
        let half = &view.groups[0].candidates[1];
        assert_eq!(half.tx, Some(ta));
        assert_eq!(half.target, Some(h.clients[0].target.clone()));
        assert!(half.kept_bytes, "its files/ holds a backup");
        assert!(
            !view.groups[1].candidates[1].kept_bytes,
            "an empty run keeps nothing"
        );
        assert!(
            view.journals
                .iter()
                .any(|j| j.tx == tc && j.family == JournalFamily::Repair)
        );
        assert_eq!(view.unknowable_journal, None);
        assert!(view.listings_complete);
    }

    #[test]
    fn orphans_take_their_own_timestamp() {
        let h = home();
        let lone_tx = tx_at("2026-08-01T12:00:00Z", 4);
        let lone = h.half(&h.client, lone_tx);
        let d = run_id("2026-08-02T00:00:00Z", 5);
        let d_cache = h.cache_quarantine(&d);
        let view = h.load();
        assert_eq!(
            shape(&view),
            vec![
                (
                    Anchor::Run(d.clone()),
                    vec![(DeletionKind::CacheQuarantine, d_cache)]
                ),
                (
                    Anchor::Half(lone_tx),
                    vec![(DeletionKind::ClientRepair, lone)]
                ),
            ]
        );
        assert_eq!(
            view.groups[0].started_at,
            Some("2026-08-02T00:00:00Z".parse().unwrap())
        );
        assert_eq!(
            view.groups[1].started_at,
            Some("2026-08-01T12:00:00Z".parse().unwrap())
        );
        let deleted = plan(&view, DOCTOR_GC_MAX_DELETIONS);
        let runs: Vec<(DeletionKind, Option<&str>)> = deleted
            .report
            .deletions
            .iter()
            .map(|d| (d.kind, d.run_id.as_deref()))
            .collect();
        assert_eq!(
            runs,
            vec![
                (DeletionKind::ClientRepair, None),
                (DeletionKind::CacheQuarantine, Some(d.as_str())),
            ],
            "the older orphan half first, naming no run: {:#?}",
            deleted.report
        );
    }

    #[test]
    fn halves_of_unregistered_clients_are_not_seen() {
        let h = home();
        h.half(&h.unregistered, tx_at("2026-08-01T00:00:00Z", 6));
        assert_eq!(shape(&h.load()), Vec::new());
    }

    #[test]
    fn bytes_walk_stops_at_the_bound() {
        let h = home();
        let id = run_id("2026-08-01T00:00:00Z", 7);
        let dir = h.run(&id, None);
        for n in 0..5 {
            fs::write(dir.join(format!("f{n}")), "12345").unwrap();
        }
        let bytes = |bound| GcView::load(&h.inputs(bound)).unwrap().groups[0].candidates[0].bytes;
        assert_eq!(bytes(3), None, "five entries past a bound of three");
        assert_eq!(bytes(DOCTOR_SCAN_MAX_ENTRIES), Some(25));
    }

    #[test]
    fn a_listing_at_the_bound_is_incomplete() {
        let h = home();
        for n in 0..3 {
            h.run(&run_id("2026-08-01T00:00:00Z", 10 + n), None);
        }
        let bounded = GcView::load(&h.inputs(2)).unwrap();
        assert!(!bounded.listings_complete);
        assert_eq!(bounded.groups.len(), 2, "{:#?}", shape(&bounded));
        assert!(h.load().listings_complete);
    }

    #[test]
    fn latest_states() {
        let present = run_id("2026-08-01T00:00:00Z", 20);
        let gone = run_id("2026-08-02T00:00:00Z", 21);
        /// Plants `latest` in a home holding the run `present`, not `gone`.
        type Plant = fn(&Home, &DoctorRunId, &DoctorRunId);
        let cases: [(&str, Plant, LatestState); 5] = [
            ("missing", |_, _, _| {}, LatestState::Missing),
            (
                "a_regular_file",
                |h, _, _| fs::write(h.paths.doctor_latest(), "not a link").unwrap(),
                LatestState::NotSymlink,
            ),
            (
                "a_live_link",
                |h, present, _| {
                    symlink(
                        format!("runs/{}", present.as_str()),
                        h.paths.doctor_latest(),
                    )
                    .unwrap();
                },
                LatestState::Link {
                    names: Some(run_id("2026-08-01T00:00:00Z", 20)),
                    resolves: true,
                },
            ),
            (
                "a_dangling_link",
                |h, _, gone| {
                    symlink(format!("runs/{}", gone.as_str()), h.paths.doctor_latest()).unwrap();
                },
                LatestState::Link {
                    names: Some(run_id("2026-08-02T00:00:00Z", 21)),
                    resolves: false,
                },
            ),
            (
                "a_link_outside_runs",
                |h, _, _| {
                    fs::create_dir_all(h.base.join("elsewhere")).unwrap();
                    symlink(h.base.join("elsewhere"), h.paths.doctor_latest()).unwrap();
                },
                LatestState::Link {
                    names: None,
                    resolves: true,
                },
            ),
        ];
        for (case, plant, want) in cases {
            let h = home();
            h.run(&present, None);
            plant(&h, &present, &gone);
            assert_eq!(h.load().latest, want, "{case}");
        }
    }

    #[test]
    fn named_backup_resource_resolves() {
        let h = home();
        let r = run_id("2026-09-01T00:00:00Z", 30);
        let tr = tx_at("2026-09-01T00:00:01Z", 30);
        let r_dir = h.run(&r, report("fix", "DONE_OK", Some(tr)));
        fs::create_dir_all(r_dir.join("backups")).unwrap();
        fs::write(r_dir.join("backups/3"), "old content").unwrap();
        // A young undo of that repair, crashed: it reads the backup by its named form.
        let u = run_id("2026-11-29T00:00:00Z", 31);
        let tu = tx_at("2026-11-29T00:00:01Z", 31);
        h.run(&u, None);
        h.repair_journal(tu, &u, Some(tr), &[format!("backup:{}/3", r.as_str())]);
        let view = h.load();
        let undo = view.journals.iter().find(|j| j.tx == tu).unwrap();
        assert!(undo.undo_of_repair);
        assert!(
            undo.protected.contains(&r_dir.join("backups/3")),
            "{:?}",
            undo.protected
        );
        let planned = plan(&view, DOCTOR_GC_MAX_DELETIONS);
        let pins: Vec<(String, Pin)> = planned
            .report
            .exclusions
            .iter()
            .map(|e| (e.path.clone(), e.pin))
            .collect();
        assert_eq!(
            pins,
            vec![(
                r_dir.join("backups/3").display().to_string(),
                Pin::ActiveUndo
            )],
            "{:#?}",
            planned.report
        );
        assert!(planned.report.deletions.is_empty(), "{:#?}", planned.report);
    }

    #[test]
    fn an_unreadable_repair_journal_is_unknowable() {
        for (case, text) in [
            ("too_new", "{\"type\":\"prepared\",\"format\":\"2.0\"}\n"),
            (
                "undecodable",
                "{\"type\":\"prepared\",\"format\":\"1.0\"}\n",
            ),
        ] {
            let h = home();
            let tx = tx_at("2026-09-01T00:00:00Z", 40);
            let path = h.paths.journal_repair(&tx);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            assert_eq!(h.load().unknowable_journal, Some(tx), "{case}");
        }
    }

    #[test]
    fn a_held_inflight_lock_makes_its_group_live() {
        let h = home();
        let id = run_id("2026-09-01T00:00:00Z", 41);
        let tx = tx_at("2026-09-01T00:00:01Z", 41);
        h.run(&id, report("fix", "DONE_FAILED", Some(tx)));
        assert!(!h.load().groups[0].live);
        let held = InflightLock::create(&h.paths, &tx).unwrap();
        assert!(h.load().groups[0].live);
        drop(held);
    }

    /// A set-aside `s` of run `s_run` that suspended the repair journal of `stuck` (whose
    /// `prepared` line names `stuck_run`), guarded by the epoch of `watched`.
    fn set_aside(h: &Home, watched: &Path) -> (TxId, DoctorRunId) {
        let s_run = run_id("2026-09-10T00:00:00Z", 50);
        let s = tx_at("2026-09-10T00:00:01Z", 50);
        let stuck = tx_at("2026-09-05T00:00:01Z", 51);
        let stuck_run = run_id("2026-09-05T00:00:00Z", 51);
        let s_dir = h.run(&s_run, report("fix", "DONE_OK", Some(s)));
        let to = s_dir.join(format!("quarantine/state/journal/{stuck}.repair.jsonl"));
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, prepared_line(stuck, &stuck_run, None, &[])).unwrap();
        let guards = vec![Guard::Epoch {
            path: watched.to_path_buf(),
            stamp: None,
        }];
        h.history(
            &[entry(
                s,
                TxKind::Repair,
                Some(&s_run),
                vec![rename(h.paths.journal_repair(&stuck), to, guards)],
                None,
            )],
            &[],
        );
        (s, stuck_run)
    }

    #[test]
    fn a_set_asides_guards_and_suspended_repairs_are_read() {
        let h = home();
        let watched = h.base.join("watched");
        let (s, stuck_run) = set_aside(&h, &watched);
        let stuck = tx_at("2026-09-05T00:00:01Z", 51);
        let line = |view: &GcView| view.repairs.iter().find(|l| l.tx == s).unwrap().clone();

        let held = line(&h.load());
        assert_eq!(held.guards, GuardState::Hold, "the folder is still absent");
        assert_eq!(
            held.suspended,
            vec![SuspendedRepair {
                tx: stuck,
                run: Some(stuck_run.clone()),
            }]
        );
        assert!(held.ops[0].guarded && held.ops[0].to_exists);

        fs::create_dir_all(&watched).unwrap();
        assert_eq!(
            line(&h.load()).guards,
            GuardState::Changed,
            "the folder appeared"
        );

        // A file where the guarded folder's parent should be: the guard cannot be read.
        let h2 = home();
        let blocked = h2.base.join("file");
        fs::write(&blocked, "x").unwrap();
        set_aside(&h2, &blocked.join("below"));
        assert_eq!(
            line(&h2.load()).guards,
            GuardState::Unknown,
            "a guard that cannot be read"
        );
    }

    #[test]
    fn an_unreadable_suspended_journal_names_no_run() {
        let h = home();
        let (s, _) = set_aside(&h, &h.base.join("watched"));
        let stuck = tx_at("2026-09-05T00:00:01Z", 51);
        let to = h
            .paths
            .doctor_run(&run_id("2026-09-10T00:00:00Z", 50))
            .join(format!("quarantine/state/journal/{stuck}.repair.jsonl"));
        fs::write(&to, "{\"type\":\"prepared\",\"format\":\"9.0\"}\n").unwrap();
        let view = h.load();
        let line = view.repairs.iter().find(|l| l.tx == s).unwrap();
        assert_eq!(
            line.suspended,
            vec![SuspendedRepair {
                tx: stuck,
                run: None
            }]
        );
    }

    #[test]
    fn a_reverted_set_aside_reads_no_guard() {
        let h = home();
        let watched = h.base.join("file");
        fs::write(&watched, "x").unwrap();
        let (s, s_run) = {
            let (s, _) = set_aside(&h, &watched.join("below"));
            (s, run_id("2026-09-10T00:00:00Z", 50))
        };
        let mut lines: Vec<HistoryEntry> = history::read(&h.paths, &HistoryFilter::default())
            .unwrap()
            .entries;
        // The undo moved the journal back: its own line is the inverse rename.
        let back = lines[0].repairs[0].clone();
        let inverse = rename(back.to.clone().unwrap(), back.path, Vec::new());
        let undo = tx_at("2026-09-11T00:00:00Z", 52);
        lines.push(entry(
            undo,
            TxKind::Undo,
            Some(&s_run),
            vec![inverse],
            Some(s),
        ));
        h.history(&lines, &[]);
        let view = h.load();
        let line = view.repairs.iter().find(|l| l.tx == s).unwrap();
        assert!(line.reverted);
        assert_eq!(line.guards, GuardState::Hold, "never read once undone");
        assert!(line.suspended.is_empty());
    }

    #[test]
    fn unreadable_history_lines_are_kept_raw() {
        let h = home();
        // JSON a newer omaddons could have written; a line cut short is a torn write,
        // which recovery appends again, and is not one of these.
        let newer = "{\"kind\":\"repair\",\"doctor_run_id\":\"x\",\"schema_version\":\"9.0\"}";
        h.history(&[], &[newer, "{\"kind\":\"repair\""]);
        assert_eq!(h.load().unreadable_history, vec![newer.to_owned()]);
    }

    #[test]
    fn loading_writes_nothing() {
        let h = home();
        let a = run_id("2026-09-01T00:00:00Z", 60);
        let ta = tx_at("2026-09-01T00:00:01Z", 60);
        let a_dir = h.run(&a, report("fix", "DONE_OK", Some(ta)));
        fs::create_dir_all(a_dir.join("backups")).unwrap();
        fs::write(a_dir.join("backups/1"), "kept").unwrap();
        h.half(&h.client, ta);
        h.cache_quarantine(&a);
        h.repair_journal(tx_at("2026-11-29T00:00:00Z", 61), &a, Some(ta), &[]);
        set_aside(&h, &h.base.join("watched"));
        symlink(format!("runs/{}", a.as_str()), h.paths.doctor_latest()).unwrap();
        fs::create_dir_all(
            h.paths
                .doctor_runs_dir()
                .join(format!(".{}.tmp", a.as_str())),
        )
        .unwrap();
        let before: Vec<TreeEntry> = tree_snapshot(&h.base);
        let view = h.load();
        assert!(!view.groups.is_empty());
        let _ = plan(&view, DOCTOR_GC_MAX_DELETIONS);
        assert_eq!(tree_snapshot(&h.base), before, "loading the view wrote");
    }

    /// `runs.rs` and `gc/` hold no write: the deletion is `gc.rs`'s alone (M4-D36).
    #[test]
    fn the_plan_and_its_view_never_write() {
        use crate::source_scan::{Literals, production_code, sources};
        const WRITES: [&str; 8] = [
            "WriteSession",
            "mutate(",
            "ensure_dir_all(",
            "prune(",
            "Op::",
            "OpenOptions",
            "fs::write",
            "remove_",
        ];
        // A needle that starts a name: `Op::` but not `RepairOp::`.
        let writes = |code: &str| -> Vec<&str> {
            WRITES
                .into_iter()
                .filter(|w| {
                    code.match_indices(w).any(|(at, _)| {
                        !code[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    })
                })
                .collect()
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let runs = root.join("src/doctor/runs.rs");
        let scanned = sources(&root.join("src/doctor/gc"))
            .into_iter()
            .chain([(runs.clone(), fs::read_to_string(&runs).unwrap())]);
        let mut found = Vec::new();
        for (path, text) in scanned {
            let code = production_code(&text, Literals::Blank).unwrap_or_default();
            found.extend(
                writes(&code)
                    .into_iter()
                    .map(|w| format!("{}: {w}", path.display())),
            );
        }
        assert!(
            found.is_empty(),
            "the retention plan writes:\n{}",
            found.join("\n")
        );
        let planted = production_code(
            "fn f() { let o = RepairOp::Chmod; crate::fsx::mutate(s, p, Op::Remove); }",
            Literals::Blank,
        );
        assert_eq!(
            writes(&planted.unwrap_or_default()),
            ["mutate(", "Op::"],
            "the scan sees a write, and not RepairOp"
        );
    }
}
