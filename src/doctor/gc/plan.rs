//! The retention plan (M4-D36): what `doctor gc` deletes, what it keeps and why, which
//! undos the deletions take away, and where `latest` goes. Pure and deterministic: the
//! same [`GcView`], its lists in any order, gives the same plan.
//!
//! The rules, in the order they apply to each eligible candidate (one that started
//! strictly before the cutoff):
//!
//! 1. **Active** (`active_run`, the whole group): one of its transactions' in-flight
//!    locks is held, or it has no report that reads and started less than
//!    `ORPHAN_MIN_AGE` ago.
//! 2. **Its own journal** (`unfinished_journal`, the whole group): a repair journal of one
//!    of its transactions is still there, in any phase. Recovery removes a journal it
//!    settled, so one still there means the run is not settled.
//! 3. **Reachability**, by overlap (one path the other or an ancestor of it): what an
//!    unfinished journal may read or write (`active_undo` for the undo of a repair,
//!    `unfinished_journal` otherwise); what a set-aside that can still be reactivated put
//!    in quarantine, and the run whose backups a repair it suspended reads
//!    (`recovery_dependency`); anything a history line that does not read mentions
//!    (`recovery_dependency`).
//! 4. **An unknowable journal** pins every eligible candidate (`unfinished_journal`).
//!
//! Every other eligible candidate is deleted, whole groups oldest first, at most `cap`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::Serialize;

use super::view::{Anchor, Candidate, GcView, Group, GuardState, LatestState};
use crate::doctor::finding::FindingTarget;
use crate::doctor::runtime::DoctorRunId;
use crate::engine::journal::JournalFamily;
use crate::fsx::FileId;
use crate::limits::ORPHAN_MIN_AGE;
use crate::model::{TargetKey, TxId};
use crate::state::history::RepairOp;

/// The states of a run `latest` may name (M4-D32): a run that finished its work.
const LATEST_STATES: [&str; 3] = ["DONE_OK", "DONE_FINDINGS", "DONE_PARTIAL"];

/// What `doctor gc` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GcReport {
    /// A preview: nothing was deleted.
    pub dry_run: bool,
    /// What started before this is eligible.
    #[schemars(with = "String")]
    pub before: Timestamp,
    /// What is deleted (or, in a preview, would be), in deletion order.
    pub deletions: Vec<GcDeletion>,
    /// What is kept although it is old enough, and why, sorted by path.
    pub exclusions: Vec<GcExclusion>,
    /// The repairs and undos of repairs whose own undo reads what is deleted: they can no
    /// longer be undone.
    pub lost_undos: Vec<TxId>,
    /// Where `latest` goes; `null` when it stays as it is.
    pub latest: Option<LatestChange>,
    /// Victims the deletion cap left for the next run; `null` when a listing stopped at
    /// its bound, so more may exist.
    pub remaining: Option<u64>,
    /// A preview's token, which `--if-match` takes; `null` on a real run.
    pub precondition: Option<String>,
}

/// One deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GcDeletion {
    pub kind: DeletionKind,
    /// The run it belongs to; `null` for a repair half no run claims.
    pub run_id: Option<String>,
    /// A repair half's client; `null` for the others.
    pub target: Option<FindingTarget>,
    /// Its transaction: a half's own, a run's repair or undo; `null` for a diagnose run.
    pub tx: Option<TxId>,
    /// Absolute.
    pub path: String,
    /// The apparent size of its files; `null` when the walk stopped at its bound.
    pub bytes: Option<u64>,
    /// It holds what a repair kept (a backup, or something set aside), which may exist
    /// nowhere else.
    pub may_hold_only_copy: bool,
}

/// One kept candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GcExclusion {
    pub kind: ExcludedKind,
    /// The run it belongs to; `null` for a repair half no run claims.
    pub run_id: Option<String>,
    /// What is protected, absolute: a backup or quarantine inside the candidate, or the
    /// candidate itself.
    pub path: String,
    pub pin: Pin,
}

/// Where `latest` goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LatestChange {
    /// The run it names now; `null` when it names none.
    pub from: Option<String>,
    /// The run it will name; `null` when it is removed.
    pub to: Option<String>,
}

/// What gc deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionKind {
    /// A doctor run directory (or its stage).
    Run,
    /// A repair's client-side half, `<client>/Interface/.omaddons/snapshots/<tx>.repair`.
    ClientRepair,
    /// What a run set aside from the cache, `<cache>/quarantine/<run id>`.
    CacheQuarantine,
}

/// What an exclusion keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExcludedKind {
    Run,
    ClientRepair,
    CacheQuarantine,
    /// A backup a repair kept, inside a run or a half.
    Backup,
    /// Something set aside, inside a quarantine.
    Quarantine,
}

/// Why something old enough is kept, strongest first: when several apply, the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Pin {
    /// An unfinished journal may still read or write it.
    UnfinishedJournal,
    /// An unfinished undo of a repair reads it.
    ActiveUndo,
    /// A set-aside that can still be reactivated needs it, or a history line that does
    /// not read may.
    RecoveryDependency,
    /// Its run is still going, or may be.
    ActiveRun,
}

/// The plan: the report, and what deletion acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    /// `dry_run` and `precondition` are the command's to fill.
    pub report: GcReport,
    /// The deletions, in order, with the identity each was planned against.
    pub victims: Vec<Victim>,
    /// A repair journal this build cannot read: a real run refuses with its stop error.
    pub unknowable_journal: Option<TxId>,
}

/// One deletion, as deletion acts on it and the token binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Victim {
    pub kind: DeletionKind,
    pub path: PathBuf,
    pub identity: FileId,
    /// A half's client.
    pub client: Option<TargetKey>,
}

/// One eligible candidate and why it is kept, if it is.
struct Decided<'a> {
    group: &'a Group,
    candidate: &'a Candidate,
    /// Each pin that applies, with the protected path it applies through (`None`: the
    /// candidate as a whole).
    pins: Vec<(Pin, Option<&'a Path>)>,
}

/// The retention plan of `view`, at most `cap` deletions.
pub fn plan(view: &GcView, cap: usize) -> GcPlan {
    let mut order: Vec<&Group> = view.groups.iter().filter(|g| eligible(g, view)).collect();
    order.sort_by_key(|g| (g.started_at, g.anchor.name()));

    // Each eligible group's candidates in deletion order, with their pins.
    let decided: Vec<Vec<Decided<'_>>> = order
        .iter()
        .map(|group| {
            let whole = group_pins(view, group);
            let mut candidates: Vec<&Candidate> = group.candidates.iter().collect();
            candidates.sort_by(|a, b| {
                (deletion_rank(a.kind), &a.path).cmp(&(deletion_rank(b.kind), &b.path))
            });
            candidates
                .into_iter()
                .map(|candidate| {
                    let mut pins: Vec<(Pin, Option<&Path>)> =
                        whole.iter().map(|p| (*p, None)).collect();
                    pins.extend(reach_pins(view, candidate));
                    Decided {
                        group,
                        candidate,
                        pins,
                    }
                })
                .collect()
        })
        .collect();

    // Whole groups, oldest first, while they fit under the cap.
    let mut deleted: Vec<&Decided<'_>> = Vec::new();
    let mut remaining: u64 = 0;
    let mut stopped = false;
    for group in &decided {
        let victims: Vec<&Decided<'_>> = group.iter().filter(|d| d.pins.is_empty()).collect();
        if victims.is_empty() {
            continue;
        }
        if !stopped && deleted.len() + victims.len() <= cap {
            deleted.extend(victims);
        } else {
            stopped = true;
            remaining += victims.len() as u64;
        }
    }

    let gone: Vec<&Path> = deleted
        .iter()
        .map(|d| d.candidate.canonical.as_path())
        .collect();
    let inside_a_deletion = |path: &Path| gone.iter().any(|g| path.starts_with(g));

    let mut exclusions: Vec<GcExclusion> = decided
        .iter()
        .flatten()
        .filter(|d| !d.pins.is_empty())
        .map(exclusion)
        .collect();
    exclusions.sort_by(|a, b| (&a.path, a.kind).cmp(&(&b.path, b.kind)));

    let lost_undos: Vec<TxId> = view
        .repairs
        .iter()
        .filter(|line| !line.reverted)
        .filter(|line| {
            line.ops.iter().any(|op| {
                let read = match op.op {
                    RepairOp::WriteFile => op.backup.as_deref(),
                    RepairOp::RenameNoReplace => op.to.as_deref(),
                    RepairOp::Chmod => None,
                };
                read.is_some_and(inside_a_deletion)
            })
        })
        .map(|line| line.tx)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let latest = latest_change(view, &deleted);
    let report = GcReport {
        dry_run: false,
        before: view.before,
        deletions: deleted.iter().map(|d| deletion(d)).collect(),
        exclusions,
        lost_undos,
        latest,
        remaining: view.listings_complete.then_some(remaining),
        precondition: None,
    };
    let plan = GcPlan {
        victims: deleted
            .iter()
            .map(|d| Victim {
                kind: d.candidate.kind,
                path: d.candidate.path.clone(),
                identity: d.candidate.identity,
                client: d.candidate.target.as_ref().map(|t| t.key),
            })
            .collect(),
        report,
        unknowable_journal: view.unknowable_journal,
    };
    debug_assert!(
        invariant_violations(view, &plan).is_empty(),
        "the retention plan breaks its invariant: {:?}",
        invariant_violations(view, &plan)
    );
    plan
}

/// Started strictly before the cutoff.
fn eligible(group: &Group, view: &GcView) -> bool {
    group.started_at.is_some_and(|at| at < view.before)
}

/// Its run may still be going: a held (or unprobeable) lock, or no report and young.
fn active(group: &Group, view: &GcView) -> bool {
    let young = group.started_at.is_none_or(|at| {
        let min = SignedDuration::try_from(ORPHAN_MIN_AGE).unwrap_or(SignedDuration::MAX);
        view.now.duration_since(at) < min
    });
    group.live || (group.report.is_none() && young)
}

/// The pins that keep the whole group.
fn group_pins(view: &GcView, group: &Group) -> Vec<Pin> {
    let mut pins = Vec::new();
    if active(group, view) {
        pins.push(Pin::ActiveRun);
    }
    let own_journal = view
        .journals
        .iter()
        .any(|j| j.family == JournalFamily::Repair && group.txs.contains(&j.tx));
    if own_journal || view.unknowable_journal.is_some() {
        pins.push(Pin::UnfinishedJournal);
    }
    if suspended_run_of(view, group) || mentioned_unreadably(view, group) {
        pins.push(Pin::RecoveryDependency);
    }
    pins
}

/// A set-aside that can still be reactivated suspended a repair whose rollback reads this
/// run's backups and quarantines: its `prepared` line names the run, or, unreadable, it
/// may name any run that started no later than it (a repair's run starts before it).
fn suspended_run_of(view: &GcView, group: &Group) -> bool {
    let Anchor::Run(id) = &group.anchor else {
        return false;
    };
    view.repairs
        .iter()
        .filter(|line| reactivatable(line))
        .flat_map(|line| &line.suspended)
        .any(|suspended| match &suspended.run {
            Some(run) => run == id,
            None => match (group.started_at, tx_time(suspended.tx)) {
                (Some(started), Some(at)) => started <= at,
                _ => true,
            },
        })
}

/// A history line that does not read names the run or one of its transactions: it may
/// be a set-aside's, whose guards gc cannot read (fail closed).
fn mentioned_unreadably(view: &GcView, group: &Group) -> bool {
    let mut needles: Vec<String> = group.txs.iter().map(ToString::to_string).collect();
    needles.extend(group.anchor.run().map(|id| id.as_str().to_owned()));
    needles.extend(
        group
            .candidates
            .iter()
            .filter_map(|c| c.tx)
            .map(|tx| tx.to_string()),
    );
    view.unreadable_history
        .iter()
        .any(|raw| needles.iter().any(|needle| raw.contains(needle.as_str())))
}

/// A set-aside not undone whose guards may still hold, with something still where it put
/// it.
fn reactivatable(line: &super::view::RepairLineView) -> bool {
    !line.reverted
        && line.guards != GuardState::Changed
        && line.ops.iter().any(|op| op.guarded && op.to_exists)
}

/// The pins that reach this candidate through what something else may read or write.
fn reach_pins<'a>(view: &'a GcView, candidate: &Candidate) -> Vec<(Pin, Option<&'a Path>)> {
    let mut pins = Vec::new();
    let at = candidate.canonical.as_path();
    for journal in &view.journals {
        let pin = if journal.undo_of_repair {
            Pin::ActiveUndo
        } else {
            Pin::UnfinishedJournal
        };
        pins.extend(
            journal
                .protected
                .iter()
                .filter(|p| overlaps(at, p))
                .map(|p| (pin, Some(p.as_path()))),
        );
    }
    for line in view.repairs.iter().filter(|line| reactivatable(line)) {
        pins.extend(
            line.ops
                .iter()
                .filter(|op| op.op == RepairOp::RenameNoReplace && op.to_exists)
                .filter_map(|op| op.to.as_deref())
                .filter(|to| overlaps(at, to))
                .map(|to| (Pin::RecoveryDependency, Some(to))),
        );
    }
    pins
}

/// One path is the other or an ancestor of it, by whole components.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The order of a group's deletions: its cache quarantine, its halves, its run directory
/// last, since the run directory names the rest.
const fn deletion_rank(kind: DeletionKind) -> u8 {
    match kind {
        DeletionKind::CacheQuarantine => 0,
        DeletionKind::ClientRepair => 1,
        DeletionKind::Run => 2,
    }
}

fn run_id(group: &Group) -> Option<String> {
    group.anchor.run().map(|id| id.as_str().to_owned())
}

fn deletion(d: &Decided<'_>) -> GcDeletion {
    let c = d.candidate;
    GcDeletion {
        kind: c.kind,
        run_id: run_id(d.group),
        target: c.target.clone(),
        tx: match c.kind {
            DeletionKind::ClientRepair => c.tx,
            DeletionKind::Run | DeletionKind::CacheQuarantine => d.group.tx,
        },
        path: c.path.display().to_string(),
        bytes: c.bytes,
        may_hold_only_copy: c.kind != DeletionKind::CacheQuarantine && c.kept_bytes,
    }
}

/// The strongest pin, and what it protects: a backup or a quarantine strictly inside the
/// candidate, the smallest path first, else the candidate itself.
fn exclusion(d: &Decided<'_>) -> GcExclusion {
    let c = d.candidate;
    let pin = d
        .pins
        .iter()
        .map(|(pin, _)| *pin)
        .min()
        .unwrap_or(Pin::ActiveRun);
    let inside = d
        .pins
        .iter()
        .filter(|(p, _)| *p == pin)
        .filter_map(|(_, path)| *path)
        .filter_map(|path| {
            let rel = path.strip_prefix(&c.canonical).ok()?;
            let first = rel.components().next()?.as_os_str().to_str()?;
            let kind = match (c.kind, first) {
                (DeletionKind::Run, "backups") | (DeletionKind::ClientRepair, "files") => {
                    ExcludedKind::Backup
                }
                (DeletionKind::Run | DeletionKind::ClientRepair, "quarantine")
                | (DeletionKind::CacheQuarantine, _) => ExcludedKind::Quarantine,
                _ => return None,
            };
            Some((c.path.join(rel), kind))
        })
        .min();
    let (path, kind) = inside.unwrap_or_else(|| {
        (
            c.path.clone(),
            match c.kind {
                DeletionKind::Run => ExcludedKind::Run,
                DeletionKind::ClientRepair => ExcludedKind::ClientRepair,
                DeletionKind::CacheQuarantine => ExcludedKind::CacheQuarantine,
            },
        )
    });
    GcExclusion {
        kind,
        run_id: run_id(d.group),
        path: path.display().to_string(),
        pin,
    }
}

/// `latest` moves only when it names a run being deleted or leads nowhere: to the newest
/// run kept whose report says it finished its work, or away when there is none.
fn latest_change(view: &GcView, deleted: &[&Decided<'_>]) -> Option<LatestChange> {
    let LatestState::Link { names, resolves } = &view.latest else {
        return None;
    };
    let deleted_runs: BTreeSet<&DoctorRunId> = deleted
        .iter()
        .filter(|d| d.candidate.kind == DeletionKind::Run && !d.candidate.staged)
        .filter_map(|d| d.group.anchor.run())
        .collect();
    let run_deleted = |id: &DoctorRunId| deleted_runs.contains(id);
    if *resolves && !names.as_ref().is_some_and(run_deleted) {
        return None;
    }
    let to = view
        .groups
        .iter()
        .filter_map(|g| Some((g.anchor.run()?, g)))
        .filter(|(id, g)| {
            g.candidates
                .iter()
                .any(|c| c.kind == DeletionKind::Run && !c.staged)
                && !run_deleted(id)
                && g.report
                    .as_ref()
                    .is_some_and(|r| LATEST_STATES.contains(&r.state.as_str()))
        })
        .map(|(id, _)| id)
        .max();
    Some(LatestChange {
        from: names.as_ref().map(|id| id.as_str().to_owned()),
        to: to.map(|id| id.as_str().to_owned()),
    })
}

fn tx_time(tx: TxId) -> Option<Timestamp> {
    let ms = i64::try_from(tx.ulid().timestamp_ms()).ok()?;
    Timestamp::from_millisecond(ms).ok()
}

/// What breaks the plan's invariant: a deletion that overlaps what an unfinished journal
/// protects, or that belongs to an active group. Empty for every plan [`plan`] makes.
pub fn invariant_violations(view: &GcView, plan: &GcPlan) -> Vec<String> {
    let mut found = Vec::new();
    for victim in &plan.victims {
        let Some((group, candidate)) = view.groups.iter().find_map(|g| {
            g.candidates
                .iter()
                .find(|c| c.path == victim.path)
                .map(|c| (g, c))
        }) else {
            found.push(format!("{} is no candidate", victim.path.display()));
            continue;
        };
        if active(group, view) {
            found.push(format!(
                "{} belongs to an active group",
                victim.path.display()
            ));
        }
        for journal in &view.journals {
            if let Some(p) = journal
                .protected
                .iter()
                .find(|p| overlaps(&candidate.canonical, p))
            {
                found.push(format!(
                    "{} overlaps {}, which journal {} protects",
                    victim.path.display(),
                    p.display(),
                    journal.tx
                ));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;
    use ulid::Ulid;

    use super::super::view::{JournalView, RepairLineView, RepairOpView, SuspendedRepair};
    use super::*;
    use crate::doctor::runs::RunSummary;
    use crate::output::RunKind;

    const NOW: &str = "2026-11-30T00:00:00Z";
    const RUNS: &str = "/s/omaddons/doctor/runs";
    const QUARANTINE: &str = "/k/omaddons/quarantine";
    const CLIENT: &str = "/games/retail";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    fn days(n: i64) -> SignedDuration {
        SignedDuration::from_hours(24 * n)
    }

    fn mins(n: i64) -> SignedDuration {
        SignedDuration::from_mins(n)
    }

    fn at(ago: SignedDuration) -> Timestamp {
        now().checked_sub(ago).unwrap()
    }

    fn ms(t: Timestamp) -> u64 {
        u64::try_from(t.as_millisecond()).unwrap()
    }

    fn run_id(ago: SignedDuration, n: u128) -> DoctorRunId {
        DoctorRunId::new(at(ago), Ulid::from_parts(ms(at(ago)), n))
    }

    fn tx(ago: SignedDuration, n: u128) -> TxId {
        TxId::from_ulid(Ulid::from_parts(ms(at(ago)), n))
    }

    fn run_path(id: &DoctorRunId) -> String {
        format!("{RUNS}/{}", id.as_str())
    }

    fn quarantine_path(id: &DoctorRunId) -> String {
        format!("{QUARANTINE}/{}", id.as_str())
    }

    fn half_path(t: TxId) -> String {
        format!("{CLIENT}/Interface/.omaddons/snapshots/{t}.repair")
    }

    fn client() -> FindingTarget {
        FindingTarget {
            id: "retail".to_owned(),
            key: TargetKey::from_ulid(Ulid::from_parts(1, 1)),
        }
    }

    fn cand(kind: DeletionKind, path: &str) -> Candidate {
        Candidate {
            kind,
            path: PathBuf::from(path),
            canonical: PathBuf::from(path),
            identity: FileId {
                dev: 1,
                ino: path.bytes().map(u64::from).sum(),
                born_ns: None,
            },
            bytes: Some(10),
            kept_bytes: false,
            tx: None,
            target: None,
            staged: false,
        }
    }

    fn summary(kind: RunKind, state: &str) -> RunSummary {
        RunSummary {
            kind: Some(kind),
            state: state.to_owned(),
            tx: None,
            undo_tx: None,
        }
    }

    /// A finished diagnose run `ago` old, `n` its ULID's random part.
    fn run(ago: SignedDuration, n: u128) -> Group {
        terminal(ago, n, "DONE_OK")
    }

    fn terminal(ago: SignedDuration, n: u128, state: &str) -> Group {
        let id = run_id(ago, n);
        Group {
            started_at: crate::doctor::runs::started_at(&id),
            candidates: vec![cand(DeletionKind::Run, &run_path(&id))],
            anchor: Anchor::Run(id),
            txs: Vec::new(),
            tx: None,
            live: false,
            report: Some(summary(RunKind::Diagnose, state)),
        }
    }

    fn no_report(mut g: Group) -> Group {
        g.report = None;
        g
    }

    fn id_of(g: &Group) -> DoctorRunId {
        g.anchor.run().unwrap().clone()
    }

    /// The run of a repair `t`.
    fn fix(mut g: Group, t: TxId) -> Group {
        g.txs.push(t);
        g.txs.sort();
        g.tx = Some(t);
        g.report = Some(RunSummary {
            tx: Some(t),
            ..summary(RunKind::Fix, "DONE_OK")
        });
        g
    }

    fn with_half(mut g: Group, t: TxId) -> Group {
        g.candidates.push(Candidate {
            tx: Some(t),
            target: Some(client()),
            ..cand(DeletionKind::ClientRepair, &half_path(t))
        });
        if !g.txs.contains(&t) {
            g.txs.push(t);
            g.txs.sort();
        }
        g
    }

    fn with_quarantine(mut g: Group) -> Group {
        let path = quarantine_path(&id_of(&g));
        g.candidates
            .push(cand(DeletionKind::CacheQuarantine, &path));
        g
    }

    fn kept(mut g: Group) -> Group {
        for c in &mut g.candidates {
            c.kept_bytes = true;
        }
        g
    }

    fn orphan(t: TxId) -> Group {
        Group {
            anchor: Anchor::Half(t),
            started_at: tx_time(t),
            txs: vec![t],
            tx: Some(t),
            live: false,
            report: None,
            candidates: vec![Candidate {
                tx: Some(t),
                target: Some(client()),
                ..cand(DeletionKind::ClientRepair, &half_path(t))
            }],
        }
    }

    fn base(groups: Vec<Group>) -> GcView {
        GcView {
            now: now(),
            before: at(days(30)),
            groups,
            journals: Vec::new(),
            repairs: Vec::new(),
            unreadable_history: Vec::new(),
            latest: LatestState::Missing,
            unknowable_journal: None,
            listings_complete: true,
        }
    }

    fn journal(t: TxId, family: JournalFamily, undo: bool, protected: &[&str]) -> JournalView {
        JournalView {
            tx: t,
            family,
            undo_of_repair: undo,
            protected: protected.iter().map(PathBuf::from).collect(),
        }
    }

    fn op(op: RepairOp, to: Option<&str>, backup: Option<&str>, guarded: bool) -> RepairOpView {
        RepairOpView {
            op,
            to: to.map(PathBuf::from),
            to_exists: to.is_some(),
            backup: backup.map(PathBuf::from),
            guarded,
        }
    }

    fn line(t: TxId, ops: Vec<RepairOpView>) -> RepairLineView {
        RepairLineView {
            tx: t,
            reverted: false,
            ops,
            guards: GuardState::Hold,
            suspended: Vec::new(),
        }
    }

    /// A set-aside `t` whose guarded rename put a journal into `<run>/quarantine/state/`,
    /// and whose other rename put a stage into the half's quarantine.
    fn set_aside(t: TxId, run: &DoctorRunId, guards: GuardState) -> RepairLineView {
        RepairLineView {
            guards,
            ..line(
                t,
                vec![
                    op(
                        RepairOp::RenameNoReplace,
                        Some(&format!(
                            "{}/quarantine/state/journal/x.jsonl",
                            run_path(run)
                        )),
                        None,
                        true,
                    ),
                    op(
                        RepairOp::RenameNoReplace,
                        Some(&format!("{}/quarantine/stage/x", half_path(t))),
                        None,
                        false,
                    ),
                ],
            )
        }
    }

    fn latest_names(g: &Group, resolves: bool) -> LatestState {
        LatestState::Link {
            names: Some(id_of(g)),
            resolves,
        }
    }

    fn change(from: Option<&Group>, to: Option<&Group>) -> Option<LatestChange> {
        Some(LatestChange {
            from: from.map(|g| id_of(g).as_str().to_owned()),
            to: to.map(|g| id_of(g).as_str().to_owned()),
        })
    }

    struct Case {
        name: &'static str,
        view: GcView,
        cap: usize,
        deletions: Vec<(DeletionKind, String)>,
        exclusions: Vec<(ExcludedKind, String, Pin)>,
        lost_undos: Vec<TxId>,
        latest: Option<LatestChange>,
        remaining: Option<u64>,
        /// Each deletion's `may_hold_only_copy`, when the case checks it.
        only_copy: Option<Vec<bool>>,
    }

    impl Case {
        fn new(name: &'static str, view: GcView) -> Self {
            Self {
                name,
                view,
                cap: crate::limits::DOCTOR_GC_MAX_DELETIONS,
                deletions: Vec::new(),
                exclusions: Vec::new(),
                lost_undos: Vec::new(),
                latest: None,
                remaining: Some(0),
                only_copy: None,
            }
        }

        fn deletes(mut self, kind: DeletionKind, path: String) -> Self {
            self.deletions.push((kind, path));
            self
        }

        fn excludes(mut self, kind: ExcludedKind, path: String, pin: Pin) -> Self {
            self.exclusions.push((kind, path, pin));
            self
        }
    }

    use DeletionKind::{CacheQuarantine, ClientRepair, Run};

    #[expect(clippy::too_many_lines, reason = "the table is the specification")]
    fn cases() -> Vec<Case> {
        let mut cases = Vec::new();

        let old = run(days(40), 1);
        cases.push(
            Case::new("old_diagnose_run_is_deleted", base(vec![old.clone()]))
                .deletes(Run, run_path(&id_of(&old))),
        );
        cases.last_mut().unwrap().only_copy = Some(vec![false]);

        cases.push(Case::new(
            "run_at_the_cutoff_is_not_eligible",
            base(vec![run(days(30), 2)]),
        ));
        cases.push(Case::new(
            "young_run_is_ignored",
            base(vec![run(days(10), 3)]),
        ));

        let bare = no_report(run(days(40), 4));
        cases.push(
            Case::new(
                "old_run_without_report_is_deleted",
                base(vec![bare.clone()]),
            )
            .deletes(Run, run_path(&id_of(&bare))),
        );

        let mut stage = no_report(run(mins(30), 5));
        stage.candidates[0].staged = true;
        stage.candidates[0].path = PathBuf::from(format!("{RUNS}/.{}.tmp", id_of(&stage).as_str()));
        stage.candidates[0].canonical = stage.candidates[0].path.clone();
        let staged_path = stage.candidates[0].path.display().to_string();
        let mut view = base(vec![stage]);
        view.before = now();
        cases.push(Case::new("young_orphaned_stage_is_active", view).excludes(
            ExcludedKind::Run,
            staged_path,
            Pin::ActiveRun,
        ));

        let t = tx(days(40), 6);
        let mut live = with_quarantine(with_half(fix(run(days(40), 6), t), t));
        live.live = true;
        cases.push(
            Case::new(
                "live_inflight_lock_keeps_the_whole_group",
                base(vec![live.clone()]),
            )
            .excludes(ExcludedKind::ClientRepair, half_path(t), Pin::ActiveRun)
            .excludes(
                ExcludedKind::CacheQuarantine,
                quarantine_path(&id_of(&live)),
                Pin::ActiveRun,
            )
            .excludes(ExcludedKind::Run, run_path(&id_of(&live)), Pin::ActiveRun),
        );

        let t = tx(days(40), 7);
        let crashed = with_half(fix(run(days(40), 7), t), t);
        let mut view = base(vec![crashed.clone()]);
        view.journals = vec![journal(
            t,
            JournalFamily::Repair,
            false,
            &[&format!("/s/omaddons/journal/{t}.repair.jsonl")],
        )];
        cases.push(
            Case::new("own_journal_keeps_the_whole_group", view)
                .excludes(
                    ExcludedKind::ClientRepair,
                    half_path(t),
                    Pin::UnfinishedJournal,
                )
                .excludes(
                    ExcludedKind::Run,
                    run_path(&id_of(&crashed)),
                    Pin::UnfinishedJournal,
                ),
        );

        let r = fix(run(days(40), 8), tx(days(40), 8));
        let backup = format!("{}/backups/3", run_path(&id_of(&r)));
        let mut view = base(vec![r.clone()]);
        view.journals = vec![journal(
            tx(mins(5), 8),
            JournalFamily::Repair,
            true,
            &[&backup],
        )];
        cases.push(
            Case::new("backup_read_by_an_unfinished_undo_is_active_undo", view).excludes(
                ExcludedKind::Backup,
                backup,
                Pin::ActiveUndo,
            ),
        );

        let r = fix(run(days(40), 9), tx(days(40), 9));
        let into = format!("{}/quarantine/state/blobs/ab", run_path(&id_of(&r)));
        let mut view = base(vec![r]);
        view.journals = vec![journal(
            tx(mins(5), 9),
            JournalFamily::Repair,
            false,
            &[&into],
        )];
        cases.push(
            Case::new(
                "overlap_with_an_unfinished_repair_is_unfinished_journal",
                view,
            )
            .excludes(ExcludedKind::Quarantine, into, Pin::UnfinishedJournal),
        );

        let t = tx(days(40), 10);
        let r = with_half(fix(run(days(40), 10), t), t);
        let f = tx(mins(5), 10);
        let mut view = base(vec![r.clone()]);
        view.journals = vec![journal(
            f,
            JournalFamily::Folder,
            false,
            &[
                &format!("{CLIENT}/Interface/AddOns/Foo"),
                &format!("{CLIENT}/Interface/.omaddons/snapshots/{f}"),
                &format!("{CLIENT}/Interface/.omaddons/stage/{f}"),
            ],
        )];
        cases.push(
            Case::new("folder_journal_pins_nothing_here", view)
                .deletes(ClientRepair, half_path(t))
                .deletes(Run, run_path(&id_of(&r))),
        );

        let s_tx = tx(days(40), 11);
        let s = kept(with_half(fix(run(days(40), 11), s_tx), s_tx));
        let s_id = id_of(&s);
        let journal_in = format!("{}/quarantine/state/journal/x.jsonl", run_path(&s_id));
        let stage_in = format!("{}/quarantine/stage/x", half_path(s_tx));
        for (name, guards) in [
            (
                "reactivatable_set_aside_is_recovery_dependency",
                GuardState::Hold,
            ),
            ("set_aside_with_unknown_guards_is_kept", GuardState::Unknown),
        ] {
            let mut view = base(vec![s.clone()]);
            view.repairs = vec![set_aside(s_tx, &s_id, guards)];
            cases.push(
                Case::new(name, view)
                    .excludes(
                        ExcludedKind::Quarantine,
                        stage_in.clone(),
                        Pin::RecoveryDependency,
                    )
                    .excludes(
                        ExcludedKind::Quarantine,
                        journal_in.clone(),
                        Pin::RecoveryDependency,
                    ),
            );
        }
        let mut view = base(vec![s.clone()]);
        view.repairs = vec![set_aside(s_tx, &s_id, GuardState::Changed)];
        let mut case = Case::new("set_aside_whose_guards_changed_is_deleted", view)
            .deletes(ClientRepair, half_path(s_tx))
            .deletes(Run, run_path(&s_id));
        case.lost_undos = vec![s_tx];
        case.only_copy = Some(vec![true, true]);
        cases.push(case);
        let mut view = base(vec![s.clone()]);
        view.repairs = vec![RepairLineView {
            reverted: true,
            ..set_aside(s_tx, &s_id, GuardState::Hold)
        }];
        cases.push(
            Case::new("reactivated_set_aside_pins_nothing", view)
                .deletes(ClientRepair, half_path(s_tx))
                .deletes(Run, run_path(&s_id)),
        );

        let named = run(days(41), 12);
        let other = run(days(40), 13);
        let mut view = base(vec![named.clone(), other.clone()]);
        view.unreadable_history = vec![format!(
            r#"{{"kind":"repair","doctor_run_id":"{}","garbage"#,
            id_of(&named).as_str()
        )];
        cases.push(
            Case::new("unreadable_history_line_naming_a_run_keeps_it", view)
                .excludes(
                    ExcludedKind::Run,
                    run_path(&id_of(&named)),
                    Pin::RecoveryDependency,
                )
                .deletes(Run, run_path(&id_of(&other))),
        );

        let t = tx(days(50), 14);
        let mut view = base(vec![
            with_quarantine(with_half(fix(run(days(50), 14), t), t)),
            run(days(40), 15),
            run(days(5), 16),
        ]);
        view.unknowable_journal = Some(tx(mins(1), 14));
        let a = &view.groups[0];
        let mut case = Case::new("unknowable_journal_pins_everything", view.clone())
            .excludes(
                ExcludedKind::ClientRepair,
                half_path(t),
                Pin::UnfinishedJournal,
            )
            .excludes(
                ExcludedKind::CacheQuarantine,
                quarantine_path(&id_of(a)),
                Pin::UnfinishedJournal,
            );
        let mut runs: Vec<String> = view.groups[..2]
            .iter()
            .map(|g| run_path(&id_of(g)))
            .collect();
        runs.sort();
        for path in runs {
            case = case.excludes(ExcludedKind::Run, path, Pin::UnfinishedJournal);
        }
        cases.push(case);

        // One candidate, every reason at once; each step drops the strongest.
        let p = run(days(40), 17);
        let p_path = run_path(&id_of(&p));
        let mut p_live = p.clone();
        p_live.live = true;
        let reasons = |undo: bool, unfinished: bool, unreadable: bool| {
            let mut view = base(vec![p_live.clone()]);
            if unfinished {
                view.journals.push(journal(
                    tx(mins(1), 1),
                    JournalFamily::Repair,
                    false,
                    &[&p_path],
                ));
            }
            if undo {
                view.journals.push(journal(
                    tx(mins(1), 2),
                    JournalFamily::Repair,
                    true,
                    &[&p_path],
                ));
            }
            if unreadable {
                view.unreadable_history = vec![format!("{} …", id_of(&p).as_str())];
            }
            view
        };
        for (name, view, pin) in [
            (
                "pin_precedence_unfinished_journal",
                reasons(true, true, true),
                Pin::UnfinishedJournal,
            ),
            (
                "pin_precedence_active_undo",
                reasons(true, false, true),
                Pin::ActiveUndo,
            ),
            (
                "pin_precedence_recovery_dependency",
                reasons(false, false, true),
                Pin::RecoveryDependency,
            ),
            (
                "pin_precedence_active_run",
                reasons(false, false, false),
                Pin::ActiveRun,
            ),
        ] {
            cases.push(Case::new(name, view).excludes(ExcludedKind::Run, p_path.clone(), pin));
        }

        let r = run(days(45), 18);
        let u = run(days(44), 19);
        let (r_path, u_path) = (run_path(&id_of(&r)), run_path(&id_of(&u)));
        let lost = |n: u128| tx(days(45), 100 + n);
        let mut view = base(vec![r.clone(), u.clone()]);
        view.repairs = vec![
            line(
                lost(1),
                vec![op(
                    RepairOp::WriteFile,
                    None,
                    Some(&format!("{r_path}/backups/1")),
                    false,
                )],
            ),
            line(lost(2), vec![op(RepairOp::Chmod, None, None, false)]),
            line(lost(3), vec![op(RepairOp::WriteFile, None, None, false)]),
            line(
                lost(4),
                vec![op(
                    RepairOp::RenameNoReplace,
                    Some(&format!("{r_path}/quarantine/client/x")),
                    None,
                    false,
                )],
            ),
            RepairLineView {
                reverted: true,
                ..line(
                    lost(5),
                    vec![op(
                        RepairOp::RenameNoReplace,
                        Some(&format!("{r_path}/quarantine/state/y")),
                        None,
                        false,
                    )],
                )
            },
            line(
                lost(6),
                vec![op(
                    RepairOp::WriteFile,
                    None,
                    Some(&format!("{u_path}/backups/1")),
                    false,
                )],
            ),
        ];
        let mut case = Case::new("lost_undos_from_inverse_reads", view)
            .deletes(Run, r_path)
            .deletes(Run, u_path);
        case.lost_undos = vec![lost(1), lost(4), lost(6)];
        cases.push(case);

        let backups = kept(run(days(50), 20));
        let quarantined = kept(run(days(49), 21));
        let empty = run(days(48), 22);
        let t = tx(days(47), 23);
        let mut half = orphan(t);
        half.candidates[0].kept_bytes = true;
        let mut cache = no_report(run(days(46), 24));
        cache.candidates = vec![cand(CacheQuarantine, &quarantine_path(&id_of(&cache)))];
        cache.candidates[0].kept_bytes = true;
        let mut case = Case::new(
            "only_copy_rule",
            base(vec![
                backups.clone(),
                quarantined.clone(),
                empty.clone(),
                half,
                cache.clone(),
            ]),
        )
        .deletes(Run, run_path(&id_of(&backups)))
        .deletes(Run, run_path(&id_of(&quarantined)))
        .deletes(Run, run_path(&id_of(&empty)))
        .deletes(ClientRepair, half_path(t))
        .deletes(CacheQuarantine, quarantine_path(&id_of(&cache)));
        case.only_copy = Some(vec![true, true, false, true, false]);
        cases.push(case);

        let victim = run(days(40), 25);
        let failed_newest = terminal(days(1), 26, "DONE_FAILED");
        let findings = terminal(days(2), 27, "DONE_FINDINGS");
        let ok_older = terminal(days(10), 28, "DONE_OK");
        let mut view = base(vec![
            victim.clone(),
            failed_newest,
            findings.clone(),
            ok_older,
        ]);
        view.latest = latest_names(&victim, true);
        let mut case = Case::new("latest_retargets_to_newest_retained_terminal", view)
            .deletes(Run, run_path(&id_of(&victim)));
        case.latest = change(Some(&victim), Some(&findings));
        cases.push(case);

        let mut view = base(vec![
            victim.clone(),
            terminal(days(1), 29, "DONE_FAILED"),
            terminal(days(2), 30, "REFUSING"),
        ]);
        view.latest = latest_names(&victim, true);
        let mut case = Case::new("latest_skips_non_latest_states", view)
            .deletes(Run, run_path(&id_of(&victim)));
        case.latest = change(Some(&victim), None);
        cases.push(case);

        let older = run(days(41), 31);
        let mut view = base(vec![victim.clone(), older.clone()]);
        view.latest = latest_names(&victim, true);
        let mut case = Case::new("latest_removed_when_nothing_remains", view)
            .deletes(Run, run_path(&id_of(&older)))
            .deletes(Run, run_path(&id_of(&victim)));
        case.latest = change(Some(&victim), None);
        cases.push(case);

        let young = run(days(1), 32);
        let mut view = base(vec![victim.clone(), young.clone()]);
        view.latest = latest_names(&young, true);
        cases.push(
            Case::new("latest_naming_a_retained_run_is_left", view)
                .deletes(Run, run_path(&id_of(&victim))),
        );

        let gone = run(days(3), 33);
        let mut view = base(vec![young.clone(), findings.clone()]);
        view.latest = latest_names(&gone, false);
        let mut case = Case::new("dangling_latest_is_retargeted", view);
        case.latest = change(Some(&gone), Some(&young));
        cases.push(case);

        for (name, latest) in [
            ("latest_not_a_symlink_is_left", LatestState::NotSymlink),
            ("latest_missing_is_left", LatestState::Missing),
        ] {
            let mut view = base(vec![victim.clone()]);
            view.latest = latest;
            cases.push(Case::new(name, view).deletes(Run, run_path(&id_of(&victim))));
        }

        let g1 = run(days(50), 34);
        let t2 = tx(days(40), 35);
        let g2 = with_quarantine(with_half(fix(run(days(40), 35), t2), t2));
        let g3 = run(days(35), 36);
        cases.push(
            Case::new(
                "deletion_order",
                base(vec![g3.clone(), g1.clone(), g2.clone()]),
            )
            .deletes(Run, run_path(&id_of(&g1)))
            .deletes(CacheQuarantine, quarantine_path(&id_of(&g2)))
            .deletes(ClientRepair, half_path(t2))
            .deletes(Run, run_path(&id_of(&g2)))
            .deletes(Run, run_path(&id_of(&g3))),
        );

        let ta = tx(days(50), 37);
        let tb = tx(days(45), 38);
        let a = with_quarantine(with_half(fix(run(days(50), 37), ta), ta));
        let b = with_quarantine(with_half(fix(run(days(45), 38), tb), tb));
        let c = run(days(40), 39);
        let mut case = Case::new("cap_keeps_whole_groups", base(vec![c, b, a.clone()]))
            .deletes(CacheQuarantine, quarantine_path(&id_of(&a)))
            .deletes(ClientRepair, half_path(ta))
            .deletes(Run, run_path(&id_of(&a)));
        case.cap = 4;
        case.remaining = Some(4);
        cases.push(case);

        let mut view = base(vec![run(days(40), 40)]);
        view.listings_complete = false;
        let mut case = Case::new("remaining_null_when_a_listing_stopped", view.clone())
            .deletes(Run, run_path(&id_of(&view.groups[0])));
        case.remaining = None;
        cases.push(case);

        let old_half = tx(days(40), 41);
        let new_half = tx(mins(10), 42);
        cases.push(
            Case::new(
                "orphan_half_takes_its_tx_time",
                base(vec![orphan(new_half), orphan(old_half)]),
            )
            .deletes(ClientRepair, half_path(old_half)),
        );

        let mut lone = no_report(run(days(40), 43));
        lone.candidates = vec![cand(CacheQuarantine, &quarantine_path(&id_of(&lone)))];
        cases.push(
            Case::new("orphan_cache_quarantine", base(vec![lone.clone()]))
                .deletes(CacheQuarantine, quarantine_path(&id_of(&lone))),
        );

        let mut staged = no_report(run(days(2), 44));
        staged.candidates[0].staged = true;
        staged.candidates[0].path =
            PathBuf::from(format!("{RUNS}/.{}.tmp", id_of(&staged).as_str()));
        staged.candidates[0].canonical = staged.candidates[0].path.clone();
        let staged_path = staged.candidates[0].path.display().to_string();
        let mut view = base(vec![staged]);
        view.before = now();
        cases.push(Case::new("staged_run", view).deletes(Run, staged_path));

        // A set-aside of a stuck repair keeps the repair's own run, whose backups its
        // rollback reads, while it can be reactivated.
        let stuck = tx(days(60), 45);
        let r = with_quarantine(kept(fix(run(days(60), 45), stuck)));
        let s_tx = tx(days(40), 46);
        let s = kept(fix(run(days(40), 46), s_tx));
        let suspended = |guards, run: Option<DoctorRunId>| RepairLineView {
            suspended: vec![SuspendedRepair { tx: stuck, run }],
            ..set_aside(s_tx, &id_of(&s), guards)
        };
        let mut view = base(vec![r.clone(), s.clone()]);
        view.repairs = vec![suspended(GuardState::Hold, Some(id_of(&r)))];
        cases.push(
            Case::new("a_suspended_repairs_run_is_kept_while_reactivatable", view)
                .excludes(
                    ExcludedKind::CacheQuarantine,
                    quarantine_path(&id_of(&r)),
                    Pin::RecoveryDependency,
                )
                .excludes(
                    ExcludedKind::Run,
                    run_path(&id_of(&r)),
                    Pin::RecoveryDependency,
                )
                .excludes(
                    ExcludedKind::Quarantine,
                    format!("{}/quarantine/state/journal/x.jsonl", run_path(&id_of(&s))),
                    Pin::RecoveryDependency,
                ),
        );
        let mut view = base(vec![r.clone(), s.clone()]);
        view.repairs = vec![suspended(GuardState::Changed, Some(id_of(&r)))];
        let mut case = Case::new("a_suspended_repairs_run_goes_once_it_cannot_return", view)
            .deletes(CacheQuarantine, quarantine_path(&id_of(&r)))
            .deletes(Run, run_path(&id_of(&r)))
            .deletes(Run, run_path(&id_of(&s)));
        case.lost_undos = vec![s_tx];
        cases.push(case);
        // Unreadable, it may belong to any run that started no later than it.
        let later = run(days(50), 47);
        let mut view = base(vec![r.clone(), s.clone(), later.clone()]);
        view.repairs = vec![suspended(GuardState::Hold, None)];
        cases.push(
            Case::new(
                "an_unreadable_suspended_repair_keeps_every_run_before_it",
                view,
            )
            .deletes(Run, run_path(&id_of(&later)))
            .excludes(
                ExcludedKind::CacheQuarantine,
                quarantine_path(&id_of(&r)),
                Pin::RecoveryDependency,
            )
            .excludes(
                ExcludedKind::Run,
                run_path(&id_of(&r)),
                Pin::RecoveryDependency,
            )
            .excludes(
                ExcludedKind::Quarantine,
                format!("{}/quarantine/state/journal/x.jsonl", run_path(&id_of(&s))),
                Pin::RecoveryDependency,
            ),
        );

        cases
    }

    /// What a case checks: deletions, exclusions, `lost_undos`, `latest` and `remaining`.
    type Shown = (
        Vec<(DeletionKind, String)>,
        Vec<(ExcludedKind, String, Pin)>,
        Vec<TxId>,
        Option<LatestChange>,
        Option<u64>,
    );

    /// What a plan shows of a case, for comparing and for messages.
    fn shown(plan: &GcPlan) -> Shown {
        let r = &plan.report;
        (
            r.deletions
                .iter()
                .map(|d| (d.kind, d.path.clone()))
                .collect(),
            r.exclusions
                .iter()
                .map(|e| (e.kind, e.path.clone(), e.pin))
                .collect(),
            r.lost_undos.clone(),
            r.latest.clone(),
            r.remaining,
        )
    }

    #[test]
    fn plan_cases() {
        for case in cases() {
            let got = plan(&case.view, case.cap);
            let want = (
                case.deletions.clone(),
                case.exclusions.clone(),
                case.lost_undos.clone(),
                case.latest.clone(),
                case.remaining,
            );
            assert_eq!(
                shown(&got),
                want,
                "{}: (deletions, exclusions, lost_undos, latest, remaining)",
                case.name
            );
            if let Some(only) = &case.only_copy {
                let copies: Vec<bool> = got
                    .report
                    .deletions
                    .iter()
                    .map(|d| d.may_hold_only_copy)
                    .collect();
                assert_eq!(&copies, only, "{}: may_hold_only_copy", case.name);
            }
            let victims: Vec<(DeletionKind, String)> = got
                .victims
                .iter()
                .map(|v| (v.kind, v.path.display().to_string()))
                .collect();
            assert_eq!(
                victims, case.deletions,
                "{}: victims follow the deletions",
                case.name
            );
        }
    }

    #[test]
    fn the_invariant_holds_on_every_case() {
        for case in cases() {
            let got = plan(&case.view, case.cap);
            let broken = invariant_violations(&case.view, &got);
            assert!(broken.is_empty(), "{}: {broken:?}", case.name);
        }
    }

    #[test]
    fn the_invariant_check_sees_a_violation() {
        let t = tx(days(40), 1);
        let mut view = base(vec![fix(run(days(40), 1), t)]);
        let victim_path = view.groups[0].candidates[0].path.clone();
        view.journals = vec![journal(
            t,
            JournalFamily::Repair,
            false,
            &[&format!("{}/backups/1", victim_path.display())],
        )];
        view.groups[0].live = true;
        let forged = GcPlan {
            report: plan(&base(Vec::new()), 1).report,
            victims: vec![Victim {
                kind: Run,
                path: victim_path,
                identity: view.groups[0].candidates[0].identity,
                client: None,
            }],
            unknowable_journal: None,
        };
        let broken = invariant_violations(&view, &forged);
        assert_eq!(broken.len(), 2, "{broken:?}");
    }

    /// Every list of a view reversed, or rotated by one.
    fn shuffled(view: &GcView, rotate: bool) -> GcView {
        fn mix<T: Clone>(items: &[T], rotate: bool) -> Vec<T> {
            let mut items = items.to_vec();
            if rotate {
                if !items.is_empty() {
                    items.rotate_left(1);
                }
            } else {
                items.reverse();
            }
            items
        }
        let mut view = view.clone();
        view.groups = mix(&view.groups, rotate);
        for group in &mut view.groups {
            group.candidates = mix(&group.candidates, rotate);
        }
        view.journals = mix(&view.journals, rotate);
        for journal in &mut view.journals {
            journal.protected = mix(&journal.protected, rotate);
        }
        view.repairs = mix(&view.repairs, rotate);
        for line in &mut view.repairs {
            line.ops = mix(&line.ops, rotate);
            line.suspended = mix(&line.suspended, rotate);
        }
        view.unreadable_history = mix(&view.unreadable_history, rotate);
        view
    }

    #[test]
    fn plan_is_deterministic() {
        for case in cases() {
            let want = plan(&case.view, case.cap);
            let text = serde_json::to_string(&want.report).unwrap();
            for rotate in [false, true] {
                let got = plan(&shuffled(&case.view, rotate), case.cap);
                assert_eq!(
                    serde_json::to_string(&got.report).unwrap(),
                    text,
                    "{} (rotate: {rotate})",
                    case.name
                );
                assert_eq!(got, want, "{} (rotate: {rotate})", case.name);
            }
        }
    }

    #[test]
    fn the_report_serializes_in_its_wire_names() {
        let t = tx(days(40), 1);
        let g = with_quarantine(with_half(fix(kept(run(days(40), 1)), t), t));
        let mut view = base(vec![g, no_report(run(days(41), 2))]);
        view.journals = vec![journal(
            tx(mins(1), 3),
            JournalFamily::Repair,
            true,
            &[&format!("{}/backups/2", run_path(&run_id(days(41), 2)))],
        )];
        view.latest = latest_names(&view.groups[0], false);
        let value = serde_json::to_value(plan(&view, 10).report).unwrap();
        insta::assert_json_snapshot!(value);
    }
}
