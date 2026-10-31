//! `doctor fix`'s planner (E6, §6.12): which findings a run repairs, their fixers' fixes
//! lowered to primitives through E5, every refusal decided before anything moves, the
//! journals the plan observes and the recoveries it delegates, and the remediation of a
//! fixer-backed finding. It reads (the view, lowering's stats, `journal::diagnose`) and
//! writes nothing, takes no lock and sends no request, so a preview and the run it
//! confirms plan one unchanged world to one token (`precondition::of_doctor_fix`).
//!
//! - **Groups** (M4-D38, M4-D39): each finding is planned or listed. A forced fixer runs
//!   only when `--only` names its id with `--force`; a disabled one only when `--only`
//!   names its id, never through a subsystem name.
//! - **Conflicts** (M4-D62) are overlaps of resource sets, never equal path strings: two
//!   fixes, or a fix and an unfinished journal the plan does not settle, that touch one
//!   resource (or one inside the other) are refused together.
//! - **One world, one refusal**, in this order: the force gate, a fixer's own refusal, a
//!   precondition, lowering (a mount, or a step a repair may never make), the size bound
//!   ([`DOCTOR_FIX_MAX_ACTIONS`]), a journal directory that does not list, a conflict.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::finding::{Finding, Remediation};
use super::registry::{
    FAILURE_MODES, FailureMode, FixerPrecondition, FixerSpec, Refusal, ScopeName, ids,
};
use super::report::{ActionPlanned, RecoveryPlanned};
use super::selection::Pick;
use super::view::DoctorView;
use crate::discover::running::RunningScan;
use crate::engine::journal::{
    self, Diagnoses, JournalDiagnosis, JournalFamily, JournalVerdict, ProtectedPaths,
    UnattributedCause,
};
use crate::engine::repair::{self, LowerCtx, LowerError, Lowered, RepairPhase};
use crate::engine::repair_action::{Fix, FixKind, Guard, Primitive, Step};
use crate::engine::resource::{PathKind, Resource, ResourceSet};
use crate::error::{Error, ErrorCode, Suggestion};
use crate::fsx::{InflightLock, canonical_lenient};
use crate::limits::{DOCTOR_FIX_MAX_ACTIONS, DOCTOR_SCAN_MAX_ENTRIES};
use crate::model::{TargetKey, TxId};
use crate::state::paths::{ClientPaths, FLATTEN_PREFIX, Paths};

// ---------------------------------------------------------------------------------------
// The plan.

/// Where a repair writes and what lowering reads besides the fixes (E5's context): a real
/// run passes its run directory and the repair transaction it allocated, a preview its run
/// directory and a provisional transaction id that is never journaled.
pub type RepairRoots<'a> = LowerCtx<'a>;

/// What a `doctor fix` asks for besides its findings.
pub struct FixOptions<'a> {
    /// `--only`, parsed; empty without it.
    pub only: &'a [Pick],
    /// `--force` (the parser requires `--yes` with it).
    pub force: bool,
    pub roots: &'a RepairRoots<'a>,
    /// The process table, read once by the caller.
    pub running: &'a RunningScan,
}

/// Why a finding of the run is not repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Its fixer runs only with `--only <fm> --force --yes` (M4-D38).
    NeedsForce,
    /// Its fixer runs only when `--only` names its id (M4-D39).
    Disabled,
    /// No fixer repairs it, or its fixer does not run on it outside an explicit
    /// `--only <fm>`.
    Manual,
    /// `--only` names neither its failure mode nor its subsystem (a finding every run
    /// reports, such as a check that could not finish).
    NotSelected,
    /// Its fixer needs a transaction whose writer is alive: nothing is stuck to repair.
    InFlight,
}

/// A finding a run lists without repairing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unplanned {
    pub fm: &'static str,
    pub instance: String,
    pub group: Group,
}

/// One fix of the plan, lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFix {
    /// 1-based in the plan, as the repair journal's `fixes[]` numbers it.
    pub n: u32,
    pub fm: &'static str,
    pub instance: String,
    /// The finding's client, by id, when it has one.
    pub target: Option<String>,
    pub kind: FixKind,
    pub steps: Vec<Step>,
    /// The union of its steps' resources.
    pub resources: ResourceSet,
    /// Its fixer's write scopes.
    pub writes_to: &'static [ScopeName],
    pub needs_force: bool,
}

/// A recovery the plan has the engine run (M4-D37), as `journal::diagnose` predicts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRecovery {
    pub fm: &'static str,
    pub tx: TxId,
    pub family: JournalFamily,
    pub phase: journal::Phase,
    /// A repair journal's own phase.
    pub repair_phase: Option<RepairPhase>,
    pub direction: journal::Direction,
    /// What that recovery may touch.
    pub protected: ResourceSet,
}

/// An interrupted journal whose writer is gone, as the plan saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedJournal {
    pub tx: TxId,
    pub family: JournalFamily,
    pub verdict: JournalVerdict,
    /// What its recovery may touch.
    pub protected: ResourceSet,
}

/// What a `doctor fix` run repairs, how, and what it leaves.
#[derive(Debug, Clone)]
pub struct FixPlan {
    pub fixes: Vec<PlannedFix>,
    /// By transaction.
    pub recoveries: Vec<PlannedRecovery>,
    /// Every journal whose writer is gone, by transaction.
    pub journals: Vec<ObservedJournal>,
    /// The clients a step touches or a recovery settles, by key.
    pub targets: Vec<TargetKey>,
    /// In the findings' order.
    pub unplanned: Vec<Unplanned>,
    /// The size of the files the repair overwrites, which it keeps as backups.
    pub bytes_to_back_up: u64,
    /// The whole repair, lowered: what E5 commits.
    pub lowered: Lowered,
    names: Names,
}

/// Why a run plans nothing: decided before anything moves.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanRefusal {
    /// `--only` names a forced fixer's failure mode, a finding of it exists, and there is
    /// no `--force`.
    ForceRequired {
        fm: &'static str,
        target: Option<String>,
    },
    /// A fixer refused to plan a selected finding: never skipped in silence, since a
    /// preview that dropped a selected fix would not say what the run does.
    Fixer {
        fm: &'static str,
        target: Option<String>,
        refusal: Refusal,
    },
    /// A fix would touch a client whose game runs, or may run.
    ClientRunning { fm: &'static str, target: String },
    /// A rename would cross a mount (M4-D35).
    FilesystemMismatch {
        fm: &'static str,
        target: Option<String>,
        resources: Vec<String>,
        message: String,
    },
    /// Lowering refused a step a repair may never make, with the code that says why
    /// (`scope_denied`, `protected_folder`, `file_exists`, `io`, `internal`), for the
    /// failure mode whose fix it was.
    Lowering {
        fm: Option<&'static str>,
        error: Error,
    },
    /// More primitives than [`DOCTOR_FIX_MAX_ACTIONS`].
    TooManyActions {
        actions: usize,
        /// The failure mode with the most of them.
        fm: Option<&'static str>,
        /// That mode's one client, when all its fixes are on one.
        target: Option<String>,
    },
    /// The journal directory exists and does not list: recovery would fail the same way.
    JournalsUnlisted { dir: PathBuf, error: String },
    /// Two fixes, or a fix and a journal the plan does not settle, touch one resource.
    FixerConflict {
        /// `(fm, instance)`: the first fix, then the other fix or the journal (its
        /// instance `tx=<tx>`). Boxed, so a refusal stays small.
        fixes: Box<[(String, String); 2]>,
        /// The first fix's client.
        target: Option<String>,
        /// The second is a journal.
        journal: bool,
        /// The overlapping resources of both sides, sorted.
        resources: Vec<String>,
    },
}

/// Plans the repairs of `findings`, in their report order (severity, failure mode,
/// instance), against the registry.
pub fn plan_fix(
    view: &DoctorView,
    findings: &[Finding],
    opts: &FixOptions<'_>,
) -> Result<FixPlan, PlanRefusal> {
    plan_fix_in(FAILURE_MODES, view, findings, opts)
}

/// [`plan_fix`] against `rows`, which hold each failure mode's fixer.
pub fn plan_fix_in(
    rows: &[FailureMode],
    view: &DoctorView,
    findings: &[Finding],
    opts: &FixOptions<'_>,
) -> Result<FixPlan, PlanRefusal> {
    let names = Names::of(opts.roots);
    let (selected, mut unplanned) = select(rows, findings, opts.only, opts.force)?;

    // Each selected finding's fixes; a fixer's refusal refuses the run.
    let mut proposed = Vec::with_capacity(selected.len());
    for (at, finding, spec) in selected {
        let fixes = (spec.plan)(view, finding).map_err(|refusal| PlanRefusal::Fixer {
            fm: finding.id,
            target: target_id(finding),
            refusal,
        })?;
        proposed.push(Proposed {
            at,
            finding,
            spec,
            fixes,
        });
    }

    // The journals, read once (a directory that does not list refuses in its place,
    // below), and the fixers' preconditions.
    let clients = Clients::of(view, opts, &names);
    let diagnoses = journal::diagnose(view.paths(), &clients.known, &|dir| {
        clients.sv_writable(dir)
    });
    let mut kept = Vec::with_capacity(proposed.len());
    for p in proposed {
        let needs = |pre| p.spec.preconditions.contains(&pre);
        if needs(FixerPrecondition::InflightFree)
            && p.fixes
                .iter()
                .filter_map(named_tx)
                .any(|tx| writer_alive(view.paths(), &diagnoses, tx))
        {
            unplanned.push((p.at, unplanned_of(p.finding, Group::InFlight)));
            continue;
        }
        if needs(FixerPrecondition::ClientNotRunning)
            && let Some(target) = clients.running_among(&touched(&p.fixes, &diagnoses))
        {
            return Err(PlanRefusal::ClientRunning {
                fm: p.finding.id,
                target,
            });
        }
        kept.push(p);
    }
    unplanned.sort_by_key(|(at, _)| *at);

    // Lowering, then the size bound.
    let inputs: Vec<repair::PlannedFix> = kept
        .iter()
        .flat_map(|p| {
            p.fixes.iter().map(|fix| repair::PlannedFix {
                fm: Cow::Borrowed(p.finding.id),
                fix: fix.clone(),
            })
        })
        .collect();
    let owners: Vec<&Proposed<'_>> = kept
        .iter()
        .flat_map(|p| std::iter::repeat_n(p, p.fixes.len()))
        .collect();
    let combined = repair::lower(opts.roots, &inputs);
    let per_fix = match &combined {
        Ok(lowered) => by_fix(lowered, inputs.len()),
        // Each fix lowered alone says what is wrong with it, and shows conflicts that
        // made the whole fail.
        Err(_) => {
            let mut alone = Vec::with_capacity(inputs.len());
            for (input, owner) in inputs.iter().zip(&owners) {
                let lowered = repair::lower(opts.roots, std::slice::from_ref(input))
                    .map_err(|err| lowering_refusal(owner.finding, err))?;
                alone.push((lowered.resources, lowered.steps.len()));
            }
            alone
        }
    };
    let actions: usize = per_fix.iter().map(|(_, steps)| steps).sum();
    if actions > DOCTOR_FIX_MAX_ACTIONS {
        return Err(too_many(actions, &owners, &per_fix));
    }
    if let Some(failure) = &diagnoses.unlisted {
        return Err(PlanRefusal::JournalsUnlisted {
            dir: failure.dir.clone(),
            error: failure.error.clone(),
        });
    }

    // The journals the plan observes, and the recoveries it delegates.
    let mut journals: Vec<ObservedJournal> = diagnoses
        .journals
        .iter()
        .filter(|d| !d.live)
        .map(|d| ObservedJournal {
            tx: d.tx,
            family: d.family,
            verdict: d.verdict,
            protected: protected_set(&d.protected),
        })
        .collect();
    journals.sort_by_key(|j| j.tx);
    let recoveries = recoveries(&inputs, &diagnoses);
    // A journal a fix suspends or delegates is that fix's to settle.
    let settled: BTreeSet<TxId> = inputs.iter().filter_map(|i| named_tx(&i.fix)).collect();

    // Conflicts: every pair of fixes, then every fix against every journal it leaves.
    let foreign: Vec<ResourceSet> = per_fix.iter().map(|(set, _)| names.foreign(set)).collect();
    for (i, ours) in foreign.iter().enumerate() {
        for (j, theirs) in foreign.iter().enumerate().skip(i + 1) {
            if ours.conflicts_with(theirs) {
                return Err(conflict(
                    owners[i].finding,
                    (owners[j].finding.id, instance_of(owners[j].finding)),
                    false,
                    ours,
                    theirs,
                ));
            }
        }
    }
    for (i, ours) in foreign.iter().enumerate() {
        for journal in journals.iter().filter(|j| !settled.contains(&j.tx)) {
            if ours.conflicts_with(&journal.protected) {
                return Err(conflict(
                    owners[i].finding,
                    (ids::JOURNAL_PENDING, format!("tx={}", journal.tx)),
                    true,
                    ours,
                    &journal.protected,
                ));
            }
        }
    }

    // Nothing explains a failed whole: its own error.
    let lowered = combined.map_err(|err| unexplained(err, &owners, &diagnoses))?;
    let fixes = planned_fixes(&lowered, &owners, &inputs);
    let mut targets = lowered.targets.clone();
    targets.extend(
        recoveries
            .iter()
            .filter_map(|r| diagnosis_of(&diagnoses, r.tx).and_then(|d| d.target)),
    );
    targets.sort();
    targets.dedup();
    Ok(FixPlan {
        fixes,
        recoveries,
        journals,
        targets,
        unplanned: unplanned.into_iter().map(|(_, u)| u).collect(),
        bytes_to_back_up: lowered.steps.iter().map(|s| s.backup_bytes).sum(),
        lowered,
        names,
    })
}

/// A selected finding, its fixer and the fixes it returned.
struct Proposed<'f> {
    /// Its place among the findings.
    at: usize,
    finding: &'f Finding,
    spec: &'f FixerSpec,
    fixes: Vec<Fix>,
}

/// The findings a run repairs, with their place and fixer, and the others with theirs and
/// their group; the force gate's refusal.
type Selected<'f> = (
    Vec<(usize, &'f Finding, &'f FixerSpec)>,
    Vec<(usize, Unplanned)>,
);

fn select<'f>(
    rows: &'f [FailureMode],
    findings: &'f [Finding],
    only: &[Pick],
    force: bool,
) -> Result<Selected<'f>, PlanRefusal> {
    let mut planned = Vec::new();
    let mut listed = Vec::new();
    for (at, finding) in findings.iter().enumerate() {
        let row = rows.iter().find(|row| row.id == finding.id);
        let by_id = only.contains(&Pick::Mode(finding.id));
        let reached = only.is_empty()
            || by_id
            || row.is_some_and(|row| only.contains(&Pick::Subsystem(row.subsystem)));
        let spec = match row.and_then(|row| row.fix.as_ref()) {
            None => Err(Group::Manual),
            Some(_) if !reached => Err(Group::NotSelected),
            Some(spec) if spec.needs_force => match (by_id, force) {
                (false, _) => Err(Group::NeedsForce),
                (true, false) => {
                    return Err(PlanRefusal::ForceRequired {
                        fm: finding.id,
                        target: target_id(finding),
                    });
                }
                (true, true) => Ok(spec),
            },
            Some(spec) if !spec.enabled => {
                if by_id {
                    Ok(spec)
                } else {
                    Err(Group::Disabled)
                }
            }
            // An explicit `--only <id>` plans every instance, so its fixer refuses the
            // ones it cannot repair by name.
            Some(spec) if by_id || finding.remediation.auto_fixable => Ok(spec),
            Some(_) => Err(Group::Manual),
        };
        match spec {
            Ok(spec) => planned.push((at, finding, spec)),
            Err(group) => listed.push((at, unplanned_of(finding, group))),
        }
    }
    Ok((planned, listed))
}

fn unplanned_of(finding: &Finding, group: Group) -> Unplanned {
    Unplanned {
        fm: finding.id,
        instance: instance_of(finding),
        group,
    }
}

fn instance_of(finding: &Finding) -> String {
    finding.instance.as_str().to_owned()
}

fn target_id(finding: &Finding) -> Option<String> {
    finding.target.as_ref().map(|target| target.id.clone())
}

/// The transaction a suspension sets aside or a delegation recovers.
fn named_tx(fix: &Fix) -> Option<TxId> {
    match fix {
        Fix::Suspend { journal } => Some(journal.tx),
        Fix::Delegate { what } => Some(what.tx),
        _ => None,
    }
}

fn diagnosis_of(diagnoses: &Diagnoses, tx: TxId) -> Option<&JournalDiagnosis> {
    diagnoses.journals.iter().find(|d| d.tx == tx)
}

/// Whether `tx`'s writer holds its in-flight lock, as the diagnosis saw it; a lock that
/// cannot be probed counts as held, so the fix waits rather than touch a live writer's
/// files.
fn writer_alive(paths: &Paths, diagnoses: &Diagnoses, tx: TxId) -> bool {
    match diagnosis_of(diagnoses, tx) {
        Some(d) => {
            d.live
                || matches!(
                    d.verdict,
                    JournalVerdict::Unattributed {
                        cause: UnattributedCause::LivenessUnknown
                    }
                )
        }
        None => InflightLock::is_live(paths, &tx).unwrap_or(true),
    }
}

/// The paths `fixes` act on: each fix's own, a suspension's artifacts, and the client a
/// delegated journal belongs to.
fn touched(fixes: &[Fix], diagnoses: &Diagnoses) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for fix in fixes {
        match fix {
            Fix::Write { path, .. } | Fix::Chmod { path, .. } | Fix::Quarantine { path, .. } => {
                out.push(path.clone());
            }
            Fix::Flatten { inner, outer } => out.extend([inner.clone(), outer.clone()]),
            Fix::Suspend { journal } => {
                out.extend(journal.artifacts.iter().map(|a| a.path.clone()));
            }
            Fix::Delegate { what } => {
                out.extend(diagnosis_of(diagnoses, what.tx).and_then(|d| d.client_dir.clone()));
            }
        }
    }
    out
}

/// Each lowered fix's resources and step count, by fix.
fn by_fix(lowered: &Lowered, fixes: usize) -> Vec<(ResourceSet, usize)> {
    let mut out = vec![(ResourceSet::new(), 0); fixes];
    for step in &lowered.steps {
        let at = (step.step.fix as usize).checked_sub(1);
        if let Some((set, steps)) = at.and_then(|at| out.get_mut(at)) {
            set.extend(&step.resources);
            *steps += 1;
        }
    }
    out
}

fn planned_fixes(
    lowered: &Lowered,
    owners: &[&Proposed<'_>],
    inputs: &[repair::PlannedFix],
) -> Vec<PlannedFix> {
    let mut fixes = Vec::with_capacity(inputs.len());
    for ((input, owner), n) in inputs.iter().zip(owners).zip(1u32..) {
        let mut resources = ResourceSet::new();
        let mut steps = Vec::new();
        for lowered in lowered.steps.iter().filter(|s| s.step.fix == n) {
            resources.extend(&lowered.resources);
            steps.push(lowered.step.clone());
        }
        fixes.push(PlannedFix {
            n,
            fm: owner.finding.id,
            instance: instance_of(owner.finding),
            target: target_id(owner.finding),
            kind: input.fix.kind(),
            steps,
            resources,
            writes_to: owner.spec.writes_to,
            needs_force: owner.spec.needs_force,
        });
    }
    fixes
}

/// One recovery per delegated transaction that recovery would still settle, by
/// transaction: a journal whose writer came back, or that recovery would no longer roll,
/// is no recovery of this plan.
fn recoveries(inputs: &[repair::PlannedFix], diagnoses: &Diagnoses) -> Vec<PlannedRecovery> {
    let mut out: Vec<PlannedRecovery> = Vec::new();
    for input in inputs {
        let Fix::Delegate { what } = &input.fix else {
            continue;
        };
        let Some(d) = diagnosis_of(diagnoses, what.tx).filter(|d| !d.live) else {
            continue;
        };
        let Some(direction) = d.direction() else {
            continue;
        };
        if out.iter().any(|r| r.tx == d.tx) {
            continue;
        }
        let fm = match &input.fm {
            Cow::Borrowed(fm) => *fm,
            Cow::Owned(_) => ids::JOURNAL_PENDING,
        };
        out.push(PlannedRecovery {
            fm,
            tx: d.tx,
            family: d.family,
            phase: d.phase,
            repair_phase: d.repair_phase,
            direction,
            protected: protected_set(&d.protected),
        });
    }
    out.sort_by_key(|r| r.tx);
    out
}

/// What a journal's recovery may touch, as resources resolved as a step's are, so the two
/// compare. A path that cannot be resolved is taken as written. The kind decides nothing:
/// paths overlap whatever their kinds.
fn protected_set(paths: &ProtectedPaths) -> ResourceSet {
    let resource = |path: &PathBuf| {
        Resource::canonical(path, PathKind::File).unwrap_or_else(|_| Resource::Path {
            path: path.clone(),
            kind: PathKind::File,
        })
    };
    let mut set = ResourceSet::new();
    for path in &paths.reads {
        set.read(resource(path));
    }
    for path in &paths.writes {
        set.write(resource(path));
    }
    set
}

fn lowering_refusal(finding: &Finding, err: LowerError) -> PlanRefusal {
    match err {
        LowerError::Refused {
            path, to, message, ..
        } => PlanRefusal::FilesystemMismatch {
            fm: finding.id,
            target: target_id(finding),
            resources: [Some(path), to]
                .into_iter()
                .flatten()
                .map(|path| path.display().to_string())
                .collect(),
            message,
        },
        LowerError::Error(error) => PlanRefusal::Lowering {
            fm: Some(finding.id),
            error,
        },
    }
}

/// A whole lowering that failed while every fix lowers alone and none conflict (two
/// folders lifted to names that differ only in case, say, or a root that cannot be
/// resolved): its own refusal, of the fix that names its path, else of the first fix.
fn unexplained(err: LowerError, owners: &[&Proposed<'_>], diagnoses: &Diagnoses) -> PlanRefusal {
    let named = |path: &Path| {
        owners.iter().find(|owner| {
            touched(&owner.fixes, diagnoses)
                .iter()
                .any(|p| path.starts_with(p) || p.starts_with(path))
        })
    };
    let owner = match &err {
        LowerError::Refused { path, .. } => named(path),
        LowerError::Error(_) => None,
    }
    .or_else(|| owners.first());
    match (owner, err) {
        (Some(owner), err) => lowering_refusal(owner.finding, err),
        (None, LowerError::Error(error)) => PlanRefusal::Lowering { fm: None, error },
        // A mount refusal names a step, so some fix was lowered.
        (None, LowerError::Refused { message, .. }) => PlanRefusal::Lowering {
            fm: None,
            error: Error::new(
                ErrorCode::Internal,
                message,
                Suggestion::text("nothing was written; report this as a bug"),
            ),
        },
    }
}

fn too_many(
    actions: usize,
    owners: &[&Proposed<'_>],
    per_fix: &[(ResourceSet, usize)],
) -> PlanRefusal {
    // Steps per failure mode, in plan order; the first of the most wins a tie.
    let mut by_mode: Vec<(&'static str, usize)> = Vec::new();
    for (owner, (_, steps)) in owners.iter().zip(per_fix) {
        match by_mode.iter_mut().find(|(fm, _)| *fm == owner.finding.id) {
            Some((_, n)) => *n += steps,
            None => by_mode.push((owner.finding.id, *steps)),
        }
    }
    let fm = by_mode
        .iter()
        .fold(None::<(&'static str, usize)>, |best, &(fm, n)| match best {
            Some((_, most)) if most >= n => best,
            _ => Some((fm, n)),
        })
        .map(|(fm, _)| fm);
    let mut clients: Vec<Option<String>> = owners
        .iter()
        .filter(|owner| Some(owner.finding.id) == fm)
        .map(|owner| target_id(owner.finding))
        .collect();
    clients.dedup();
    let target = match clients.as_slice() {
        [one] => one.clone(),
        _ => None,
    };
    PlanRefusal::TooManyActions {
        actions,
        fm,
        target,
    }
}

fn conflict(
    first: &Finding,
    second: (&str, String),
    journal: bool,
    ours: &ResourceSet,
    theirs: &ResourceSet,
) -> PlanRefusal {
    let mut resources: BTreeSet<String> = BTreeSet::new();
    for found in ours.conflicts(theirs) {
        for resource in [found.ours, found.theirs] {
            resources.insert(resource.text().unwrap_or_else(|_| resource.to_string()));
        }
    }
    PlanRefusal::FixerConflict {
        fixes: Box::new([
            (first.id.to_owned(), instance_of(first)),
            (second.0.to_owned(), second.1),
        ]),
        target: target_id(first),
        journal,
        resources: resources.into_iter().collect(),
    }
}

// ---------------------------------------------------------------------------------------
// The clients.

/// The clients a plan knows.
struct Clients {
    /// Where diagnosis looks for a transaction's artifacts: the clients the view found
    /// and those the repair may touch.
    known: Vec<PathBuf>,
    /// The clients the repair may touch, canonical, with their ids, by id.
    planned: Vec<(PathBuf, String)>,
    /// The clients whose game runs, canonical; `None` when the process table could not
    /// be read, and every client counts as running.
    running: Option<Vec<PathBuf>>,
}

impl Clients {
    fn of(view: &DoctorView, opts: &FixOptions<'_>, names: &Names) -> Self {
        let mut known: Vec<PathBuf> = view
            .world()
            .map(|world| {
                world
                    .resolved
                    .targets
                    .iter()
                    .map(|d| d.target.client_dir.clone())
                    .collect()
            })
            .unwrap_or_default();
        known.extend(opts.roots.clients.iter().map(|(dir, _)| dir.clone()));
        known.sort();
        known.dedup();
        let mut planned: Vec<(PathBuf, String)> = names
            .clients
            .iter()
            .map(|(dir, key)| (dir.clone(), view.target_ref(*key).id))
            .collect();
        planned.sort_by(|a, b| a.1.cmp(&b.1));
        let running = opts
            .running
            .unavailable
            .is_none()
            .then(|| opts.running.clients.keys().map(|dir| whole(dir)).collect());
        Self {
            known,
            planned,
            running,
        }
    }

    /// Whether recovery may write the SavedVariables of the client at `dir` now: one this
    /// plan knows, whose game is seen not to run.
    fn sv_writable(&self, dir: &Path) -> bool {
        let dir = whole(dir);
        self.running
            .as_ref()
            .is_some_and(|running| !running.contains(&dir))
            && self.known.iter().any(|known| whole(known) == dir)
    }

    /// The first client (by id) one of `paths` is in whose game runs, or may run.
    fn running_among(&self, paths: &[PathBuf]) -> Option<String> {
        let paths: Vec<PathBuf> = paths.iter().map(|path| whole(path)).collect();
        self.planned
            .iter()
            .find(|(dir, _)| {
                paths.iter().any(|path| path.starts_with(dir))
                    && self
                        .running
                        .as_ref()
                        .is_none_or(|running| running.contains(dir))
            })
            .map(|(_, id)| id.clone())
    }
}

/// The size of what is at `path` without the entries at `gone`, never following a link:
/// a file's length, a folder's regular files summed over at most
/// [`DOCTOR_SCAN_MAX_ENTRIES`] entries, a symlink or nothing 0. An estimate for a
/// preview, read only.
fn size_of(path: &Path, gone: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    let mut seen = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(at) = pending.pop() {
        if gone.contains(&at) {
            continue;
        }
        seen += 1;
        if seen > DOCTOR_SCAN_MAX_ENTRIES {
            break;
        }
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            continue;
        };
        if meta.is_file() {
            total = total.saturating_add(meta.len());
        } else if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&at)
        {
            pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
        }
    }
    total
}

/// `path` resolved as lowering resolves a root, or as given when it cannot be.
fn whole(path: &Path) -> PathBuf {
    canonical_lenient(path).unwrap_or_else(|| path.to_path_buf())
}

// ---------------------------------------------------------------------------------------
// This run's names.

/// What makes paths this run's own: its transaction, its run, and the quarantine roots a
/// rename's destination is named below; canonical, as lowering resolves them.
#[derive(Debug, Clone)]
struct Names {
    tx: TxId,
    run_id: String,
    run_dir: PathBuf,
    cache_quarantine: PathBuf,
    /// Each quarantine root and its side.
    sides: Vec<(PathBuf, &'static str)>,
    paths: Paths,
    skill_dirs: Vec<PathBuf>,
    /// The clients the repair may touch, canonical, with their keys.
    clients: Vec<(PathBuf, TargetKey)>,
}

impl Names {
    fn of(roots: &RepairRoots<'_>) -> Self {
        let run_dir = whole(&roots.run.dir);
        let cache_quarantine = whole(roots.paths.cache_dir())
            .join("quarantine")
            .join(&roots.run.id);
        let clients: Vec<(PathBuf, TargetKey)> = roots
            .clients
            .iter()
            .map(|(dir, key)| (whole(dir), *key))
            .collect();
        let quarantine = run_dir.join("quarantine");
        let mut sides = vec![
            (quarantine.join("state"), "state"),
            (quarantine.join("config"), "config"),
            (quarantine.join("skill"), "skill"),
            (cache_quarantine.clone(), "cache"),
        ];
        sides.extend(clients.iter().map(|(dir, _)| {
            (
                ClientPaths::new(dir)
                    .snapshots_repair(&roots.tx)
                    .join("quarantine"),
                "client",
            )
        }));
        Self {
            tx: roots.tx,
            run_id: roots.run.id.clone(),
            run_dir,
            cache_quarantine,
            sides,
            paths: roots.paths.clone(),
            skill_dirs: roots.skill_dirs.to_vec(),
            clients,
        }
    }

    /// `path` as a token names it: this repair's transaction written `<tx>` and its run
    /// `<run>`.
    fn neutral(&self, path: &Path) -> String {
        path.to_string_lossy()
            .replace(&self.tx.to_string(), "<tx>")
            .replace(&self.run_id, "<run>")
    }

    /// A rename's destination as a token names it: below its quarantine root, the side
    /// and the path under the root (a client's is the path below the client); elsewhere
    /// the whole path, neutral.
    fn to_rel(&self, to: &Path, quarantine: Option<&Path>) -> String {
        if let Some(base) = quarantine
            && let Some((_, side)) = self.sides.iter().find(|(root, _)| root == base)
            && let Ok(rel) = to.strip_prefix(base)
        {
            return format!("{side}/{}", self.neutral(rel));
        }
        self.neutral(to)
    }

    /// Whether `resource` is the repair's own, never a fix's (M4-D62): a backup, or a
    /// path below its run directory, its cache quarantine or a client's `<tx>.repair`
    /// half, or a flatten's provisional folder.
    fn owns(&self, resource: &Resource) -> bool {
        match resource {
            Resource::Backup { .. } => true,
            Resource::Path { path, .. } => {
                let half = format!("{}.repair", self.tx);
                let provisional = format!("{FLATTEN_PREFIX}{}", self.tx);
                path.starts_with(&self.run_dir)
                    || path.starts_with(&self.cache_quarantine)
                    || path
                        .components()
                        .any(|c| c.as_os_str() == OsStr::new(&half))
                    || path.file_name() == Some(OsStr::new(&provisional))
            }
            Resource::Journal(_) | Resource::Record(_) => false,
        }
    }

    /// `step` with every path it names [`Names::neutral`].
    fn neutral_step(&self, step: &Step) -> Step {
        let neutral = |path: &Path| PathBuf::from(self.neutral(path));
        let mut step = step.clone();
        step.op = match step.op {
            Primitive::WriteFile { path, mode, backup } => Primitive::WriteFile {
                path: neutral(&path),
                mode,
                backup: backup.as_deref().map(neutral),
            },
            Primitive::Chmod { path, mode } => Primitive::Chmod {
                path: neutral(&path),
                mode,
            },
            Primitive::RenameNoReplace {
                from,
                to,
                activation,
            } => Primitive::RenameNoReplace {
                from: neutral(&from),
                to: neutral(&to),
                activation,
            },
        };
        for guard in &mut step.guards {
            if let Guard::Epoch { path, .. } = guard {
                *path = neutral(path);
            }
        }
        step
    }

    /// `set` without the repair's own resources.
    fn foreign(&self, set: &ResourceSet) -> ResourceSet {
        let mut out = ResourceSet::new();
        for resource in set.reads().iter().filter(|r| !self.owns(r)) {
            out.read(resource.clone());
        }
        for resource in set.writes().iter().filter(|r| !self.owns(r)) {
            out.write(resource.clone());
        }
        out
    }

    /// The directories `scope` resolves to for steps that touch `touched`.
    fn scope_dirs(&self, scope: ScopeName, touched: &[&Path]) -> Vec<PathBuf> {
        let clients = || {
            self.clients
                .iter()
                .filter(|(dir, _)| touched.iter().any(|path| path.starts_with(dir)))
                .map(|(dir, _)| ClientPaths::new(dir))
        };
        match scope {
            ScopeName::State => vec![self.paths.state_dir().to_path_buf()],
            ScopeName::Config => vec![self.paths.config_dir().to_path_buf()],
            ScopeName::Cache => vec![self.paths.cache_dir().to_path_buf()],
            ScopeName::Skill => self.skill_dirs.clone(),
            ScopeName::ClientMeta => clients().map(|client| client.meta_dir()).collect(),
            ScopeName::AddOns => clients().map(|client| client.addons()).collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// What the token binds.

/// What `doctor fix`'s token binds (M4-D44), nothing of this run's in it:
/// [`FixPlan::bound`] builds it, `precondition::of_doctor_fix` hashes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRepair {
    pub targets: Vec<TargetKey>,
    /// In `seq` order.
    pub steps: Vec<BoundStep>,
    /// By transaction.
    pub recoveries: Vec<BoundRecovery>,
    /// By transaction.
    pub journals: Vec<BoundJournal>,
}

/// One primitive as the token binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundStep {
    pub seq: u32,
    pub fm: String,
    pub fix: u32,
    pub op: &'static str,
    /// Absolute and canonical, this repair's transaction written `<tx>` and its run
    /// `<run>`.
    pub path: String,
    /// A rename's destination: `<side>/<path below its quarantine root>`, else the
    /// neutral absolute path.
    pub to_rel: Option<String>,
    /// A write's or a chmod's mode.
    pub mode: Option<u32>,
    /// The rename that reactivates a set-aside journal.
    pub activation: bool,
    /// The step's `pre` predicate, in the journal's wire form.
    pub pre: Value,
    /// A set-aside's guards, in the journal's wire form.
    pub guards: Vec<Value>,
}

/// A delegated recovery as the token binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRecovery {
    pub tx: TxId,
    pub family: Value,
    pub phase: Value,
    /// A repair journal's own phase.
    pub repair_phase: Option<Value>,
    pub direction: Value,
}

/// An observed journal as the token binds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundJournal {
    pub tx: TxId,
    pub family: Value,
    pub verdict: Value,
    /// Its protected resources' text forms, sorted.
    pub reads: Vec<String>,
    pub writes: Vec<String>,
}

/// One serialized value: a wire form, never a failure (these types serialize as plain
/// data).
fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn texts(resources: &BTreeSet<Resource>) -> Vec<String> {
    let mut out: Vec<String> = resources
        .iter()
        .map(|r| r.text().unwrap_or_else(|_| r.to_string()))
        .collect();
    out.sort();
    out
}

impl FixPlan {
    /// What the token binds: no run id, run directory, transaction, backup path, size,
    /// label, message, age, pid or time.
    pub fn bound(&self) -> BoundRepair {
        let steps = self
            .lowered
            .steps
            .iter()
            .map(|lowered| {
                let step = &lowered.step;
                let (to_rel, mode, activation) = match &step.op {
                    Primitive::WriteFile { mode, .. } | Primitive::Chmod { mode, .. } => {
                        (None, Some(mode.bits()), false)
                    }
                    Primitive::RenameNoReplace { to, activation, .. } => (
                        Some(self.names.to_rel(to, lowered.quarantine.as_deref())),
                        None,
                        *activation,
                    ),
                };
                BoundStep {
                    seq: step.seq,
                    fm: step.fm.to_string(),
                    fix: step.fix,
                    op: step.op.name(),
                    path: self.names.neutral(step.op.path()),
                    to_rel,
                    mode,
                    activation,
                    pre: wire(&step.pre),
                    guards: step.guards.iter().map(wire).collect(),
                }
            })
            .collect();
        let recoveries = self
            .recoveries
            .iter()
            .map(|r| BoundRecovery {
                tx: r.tx,
                family: wire(&r.family),
                phase: wire(&r.phase),
                repair_phase: r.repair_phase.as_ref().map(wire),
                direction: wire(&r.direction),
            })
            .collect();
        let journals = self
            .journals
            .iter()
            .map(|j| BoundJournal {
                tx: j.tx,
                family: wire(&j.family),
                verdict: wire(&j.verdict),
                reads: texts(j.protected.reads()),
                writes: texts(j.protected.writes()),
            })
            .collect();
        BoundRepair {
            targets: self.targets.clone(),
            steps,
            recoveries,
            journals,
        }
    }

    /// Whether the plan changes nothing: no primitive and no recovery (a chmod to the
    /// mode a file has, or a delegation recovery no longer needs, lowers to none).
    pub fn is_empty(&self) -> bool {
        !self.has_steps() && self.recoveries.is_empty()
    }

    /// Whether the plan runs a primitive.
    pub fn has_steps(&self) -> bool {
        !self.lowered.steps.is_empty()
    }

    /// Each primitive as a preview shows it (A.5): its journal form with this run's own
    /// names written `<run>` and `<tx>`, and the bytes it writes or moves.
    pub fn actions_planned(&self) -> Vec<ActionPlanned> {
        // Where each path a step names is on disk now: below a destination an earlier
        // rename filled, it is what that rename moved. A subject counts without what
        // earlier renames took out of it.
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut taken: Vec<PathBuf> = Vec::new();
        let on_disk = |moved: &[(PathBuf, PathBuf)], path: &Path| -> PathBuf {
            moved
                .iter()
                .rev()
                .find_map(|(to, from)| path.strip_prefix(to).ok().map(|rest| from.join(rest)))
                .unwrap_or_else(|| path.to_path_buf())
        };
        let mut out = Vec::with_capacity(self.lowered.steps.len());
        for lowered in &self.lowered.steps {
            let bytes = match &lowered.step.op {
                Primitive::WriteFile { .. } => lowered
                    .content
                    .as_ref()
                    .map_or(0, |content| content.len() as u64),
                Primitive::Chmod { .. } => 0,
                Primitive::RenameNoReplace { from, to, .. } => {
                    let source = on_disk(&moved, from);
                    let gone: Vec<PathBuf> = taken
                        .iter()
                        .filter(|path| *path != &source && path.starts_with(&source))
                        .cloned()
                        .collect();
                    let bytes = size_of(&source, &gone);
                    moved.push((to.clone(), source.clone()));
                    taken.push(source);
                    bytes
                }
            };
            out.push(ActionPlanned {
                step: self.names.neutral_step(&lowered.step),
                bytes,
            });
        }
        out
    }

    /// Each recovery as a preview shows it.
    pub fn recoveries_planned(&self) -> Vec<RecoveryPlanned> {
        self.recoveries
            .iter()
            .map(|r| RecoveryPlanned {
                tx: r.tx,
                family: r.family,
                phase: r.phase,
                direction: r.direction,
            })
            .collect()
    }

    /// The failure modes the plan repairs or settles.
    pub fn modes(&self) -> BTreeSet<&'static str> {
        self.fixes
            .iter()
            .map(|fix| fix.fm)
            .chain(self.recoveries.iter().map(|r| r.fm))
            .collect()
    }

    /// For `--robot-triage` (E4): one action per failure mode with planned fixes, in plan
    /// order.
    pub fn triage(&self) -> Vec<TriageAction> {
        let mut out: Vec<TriageAction> = Vec::new();
        for fix in &self.fixes {
            let steps: Vec<&repair::LoweredStep> = self
                .lowered
                .steps
                .iter()
                .filter(|s| s.step.fix == fix.n)
                .collect();
            let bytes: u64 = steps
                .iter()
                .map(|s| s.content.as_ref().map_or(0, |c| c.len() as u64) + s.backup_bytes)
                .sum();
            let touched: Vec<&Path> = steps.iter().flat_map(|s| s.step.touched()).collect();
            let dirs = fix
                .writes_to
                .iter()
                .flat_map(|scope| self.names.scope_dirs(*scope, &touched))
                .map(|dir| dir.display().to_string());
            match out.iter_mut().find(|action| action.fixer_id == fix.fm) {
                Some(action) => {
                    action.writes_to.extend(dirs);
                    action.estimated_bytes += bytes;
                }
                None => out.push(TriageAction {
                    fixer_id: fix.fm,
                    writes_to: dirs.collect(),
                    estimated_bytes: bytes,
                }),
            }
        }
        for action in &mut out {
            action.writes_to.sort();
            action.writes_to.dedup();
        }
        out
    }

    /// The run that repairs what this plan repairs without `--force`: `omaddons doctor
    /// fix --only <its failure modes> [-t <target>]`; `None` when it repairs nothing a
    /// default run would. A forced fixer is never in it (M4-D38).
    pub fn recommended_argv(&self, target: Option<&str>) -> Option<Vec<String>> {
        let mut fms: Vec<&str> = Vec::new();
        for fix in self.fixes.iter().filter(|fix| !fix.needs_force) {
            if !fms.contains(&fix.fm) {
                fms.push(fix.fm);
            }
        }
        for recovery in &self.recoveries {
            if !fms.contains(&recovery.fm) {
                fms.push(recovery.fm);
            }
        }
        if fms.is_empty() {
            return None;
        }
        let mut argv = argv_of(&["omaddons", "doctor", "fix", "--only", &fms.join(",")]);
        with_target(&mut argv, target);
        Some(argv)
    }
}

/// What `--robot-triage` shows of one failure mode the plan repairs (E4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageAction {
    pub fixer_id: &'static str,
    /// Its fixer's write scopes, resolved to the directories this plan writes in, sorted.
    pub writes_to: Vec<String>,
    /// The bytes its writes write, and those of the files they replace, which the repair
    /// keeps. A rename moves no data: quarantine never crosses a filesystem (M4-D35).
    pub estimated_bytes: u64,
}

// ---------------------------------------------------------------------------------------
// Remediations and refusals.

/// A fixer-backed finding's remediation (M4-D53): `omaddons doctor fix --only <fm>
/// [-t <target_id>]`, with `--force --yes` for a forced fixer (shown to a person, who
/// reads `explain` first; never a refusal's fix), and the `doctor explain` beside it. It
/// is auto-fixable when the fixer is enabled, needs no force and the finding says so; its
/// estimate is the primitives the fixer's fixes lower to (none when the fixer refuses).
/// Detectors never spell a `doctor fix` argv: the runtime applies this.
pub fn remediation_for(
    view: &DoctorView,
    finding: &Finding,
    spec: &FixerSpec,
    target_id: Option<&str>,
) -> Remediation {
    let mut argv = argv_of(&["omaddons", "doctor", "fix", "--only", finding.id]);
    with_target(&mut argv, target_id);
    if spec.needs_force {
        argv.extend(argv_of(&["--force", "--yes"]));
    }
    let explain_argv = explain_argv(finding.id, target_id);
    let estimated_actions = (spec.plan)(view, finding).map_or(0, |fixes| {
        fixes
            .iter()
            .map(Fix::primitive_count)
            .fold(0u32, u32::saturating_add)
    });
    Remediation {
        command: shell_words::join(&argv),
        argv: Some(argv),
        explain_command: shell_words::join(&explain_argv),
        explain_argv,
        auto_fixable: spec.enabled && !spec.needs_force && finding.remediation.auto_fixable,
        needs_force: spec.needs_force,
        estimated_actions,
        destructive: false,
    }
}

fn argv_of(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn with_target(argv: &mut Vec<String>, target: Option<&str>) {
    if let Some(target) = target {
        argv.extend(["-t".to_owned(), target.to_owned()]);
    }
}

fn explain_argv(fm: &str, target: Option<&str>) -> Vec<String> {
    let mut argv = argv_of(&["omaddons", "doctor", "explain", fm]);
    with_target(&mut argv, target);
    argv
}

/// `omaddons doctor fix --only <fm> [-t <target>] --dry-run`: the preview of one mode's
/// repairs.
fn preview_argv(fm: &str, target: Option<&str>) -> Vec<String> {
    let mut argv = argv_of(&["omaddons", "doctor", "fix", "--only", fm]);
    with_target(&mut argv, target);
    argv.push("--dry-run".to_owned());
    argv
}

fn refused(message: String, fix: Suggestion, details: Value) -> Error {
    Error::new(ErrorCode::DoctorRefused, message, fix).details(details)
}

impl PlanRefusal {
    /// The code of the error it becomes: `doctor_refused`, or lowering's or the journal
    /// listing's own.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Lowering { error, .. } => error.code,
            Self::JournalsUnlisted { .. } => ErrorCode::Io,
            Self::ForceRequired { .. }
            | Self::Fixer { .. }
            | Self::ClientRunning { .. }
            | Self::FilesystemMismatch { .. }
            | Self::TooManyActions { .. }
            | Self::FixerConflict { .. } => ErrorCode::DoctorRefused,
        }
    }

    /// The error the run reports (M4-D40): `doctor_refused` with its `details.reason`,
    /// or the error lowering or listing the journals gave. Every fix keeps `target`, the
    /// user's `-t`, else names the finding's client, and never adds `--force` or
    /// `--yes`.
    pub fn into_error(self, target: Option<&str>) -> Error {
        let client = |own: Option<&str>| target.or(own).map(str::to_owned);
        match self {
            Self::ForceRequired { fm, target: own } => refused(
                format!(
                    "{fm} is repaired only with --force --yes, which this run was not given; nothing was changed: read what its repair does first"
                ),
                Suggestion::command(explain_argv(fm, client(own.as_deref()).as_deref())),
                json!({ "reason": "force_required", "fm": fm }),
            ),
            Self::Fixer {
                fm,
                target: own,
                refusal,
            } => refused(
                format!("{fm}: {}; nothing was changed", refusal.message),
                Suggestion::command(explain_argv(fm, client(own.as_deref()).as_deref())),
                json!({ "reason": refusal.reason.as_str(), "fm": fm }),
            ),
            Self::ClientRunning { fm, target: id } => refused(
                format!(
                    "{fm} would change {id} while its game runs, or while omaddons cannot tell whether it does; nothing was changed"
                ),
                Suggestion::text("quit the game client, then rerun"),
                json!({ "reason": "client_running", "fm": fm }),
            ),
            Self::FilesystemMismatch {
                fm,
                resources,
                message,
                ..
            } => {
                let victim = resources.first().cloned().unwrap_or_default();
                refused(
                    message,
                    Suggestion::text(format!(
                        "the repair would move {victim} across filesystems, which omaddons never does; keep <state>, <config> and <cache> on one filesystem and each client's .omaddons beside its client, or repair it by hand"
                    )),
                    json!({ "reason": "filesystem_mismatch", "fm": fm, "resources": resources }),
                )
            }
            Self::Lowering { error, .. } => error,
            Self::TooManyActions {
                actions,
                fm,
                target: own,
            } => {
                let message = format!(
                    "the repair would take {actions} filesystem operations, more than the {DOCTOR_FIX_MAX_ACTIONS} one run may make; nothing was changed"
                );
                let fix = match fm {
                    Some(fm) => {
                        Suggestion::command(preview_argv(fm, client(own.as_deref()).as_deref()))
                    }
                    None => Suggestion::text("repair fewer failure modes at a time with --only"),
                };
                let mut details = json!({
                    "reason": "too_many_actions",
                    "actions": actions,
                    "max_actions": DOCTOR_FIX_MAX_ACTIONS,
                });
                if let (Some(fm), Some(map)) = (fm, details.as_object_mut()) {
                    map.insert("fm".to_owned(), json!(fm));
                }
                refused(message, fix, details)
            }
            Self::JournalsUnlisted { dir, error } => journal::unattributed(
                Error::new(
                    ErrorCode::Io,
                    error,
                    Suggestion::command(["omaddons", "status"]),
                )
                .details(json!({ "path": dir.display().to_string() })),
                None,
            ),
            Self::FixerConflict {
                fixes,
                target: own,
                journal,
                resources,
            } => {
                let [(fm, instance), (other_fm, other_instance)] = *fixes;
                let client = client(own.as_deref());
                let at = match resources.as_slice() {
                    [] => "one resource".to_owned(),
                    [one] => one.clone(),
                    [first, rest @ ..] => format!("{first} and {} more", rest.len()),
                };
                let (what, fix) = if journal {
                    (
                        format!(
                            "{fm} ({instance}) would touch {at}, which an interrupted change ({other_instance}) still holds for its recovery"
                        ),
                        Suggestion::command(explain_argv(&fm, client.as_deref())),
                    )
                } else if fm == other_fm {
                    (
                        format!(
                            "two repairs of {fm} ({instance}, {other_instance}) would both touch {at}"
                        ),
                        Suggestion::command(explain_argv(&fm, client.as_deref())),
                    )
                } else {
                    (
                        format!(
                            "{fm} ({instance}) and {other_fm} ({other_instance}) would both touch {at}"
                        ),
                        Suggestion::command(preview_argv(&fm, client.as_deref())),
                    )
                };
                refused(
                    format!("{what}; they are never repaired in one run, and nothing was changed"),
                    fix,
                    json!({
                        "reason": "fixer_conflict",
                        "fm": fm,
                        "resources": resources,
                        "fixes": [
                            { "fm": fm, "instance": instance },
                            { "fm": other_fm, "instance": other_instance },
                        ],
                    }),
                )
            }
        }
    }
}

#[cfg(test)]
impl FixPlan {
    /// A plan of `steps` chmods of one fixture file and `recoveries`, nothing read: what
    /// the runtime's order tests script.
    pub(crate) fn scripted(steps: u32, recoveries: Vec<PlannedRecovery>) -> Self {
        use crate::engine::repair::LoweredStep;
        use crate::engine::repair_action::{Endpoint, Endpoints, Mode};
        let file = || Endpoint::File {
            identity: None,
            sha256: None,
            mode: Mode::new(0o600),
        };
        let lowered = Lowered {
            fixes: Vec::new(),
            steps: (1..=steps)
                .map(|seq| LoweredStep {
                    step: Step {
                        seq,
                        fix: 1,
                        fm: Cow::Borrowed("fm-secrets-test-chmod"),
                        op: Primitive::Chmod {
                            path: PathBuf::from("/fixture/config/omaddons/credentials/github"),
                            mode: Mode::new(0o600).unwrap_or_else(|| unreachable!()),
                        },
                        pre: Endpoints::One(file()),
                        post: Endpoints::One(file()),
                        guards: Vec::new(),
                    },
                    resources: ResourceSet::new(),
                    backup_bytes: 0,
                    quarantine: None,
                    rollback_to: None,
                    content: None,
                })
                .collect(),
            resources: ResourceSet::new(),
            targets: Vec::new(),
            client_dirs: Vec::new(),
            roots: Vec::new(),
        };
        let paths = Paths::with_roots(
            "/fixture/home",
            "/fixture/config",
            "/fixture/state",
            "/fixture/cache",
        );
        Self {
            fixes: Vec::new(),
            recoveries,
            journals: Vec::new(),
            targets: Vec::new(),
            unplanned: Vec::new(),
            bytes_to_back_up: 0,
            lowered,
            names: Names {
                tx: TxId::new(),
                run_id: "R".to_owned(),
                run_dir: PathBuf::from("/fixture/state/omaddons/doctor/runs/R"),
                cache_quarantine: PathBuf::from("/fixture/cache/omaddons/quarantine/R"),
                sides: Vec::new(),
                paths,
                skill_dirs: Vec::new(),
                clients: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::discover::running::{RunningEvidence, RunningInfo};
    use crate::doctor::finding::{Confidence, Evidence, Observation, Severity, Subject, Subsystem};
    use crate::doctor::registry::{Cardinality, RefusalReason, Tier};
    use crate::doctor::view::ViewOptions;
    use crate::engine::repair::RepairRun;
    use crate::engine::repair_action::{
        Attribution, Delegation, Direction as Delegated, JournalPhase, JournalSet, Mode,
        QuarantineRoot, SuspendedArtifact,
    };
    use crate::engine::testing::{REPAIR_RUN_ID, RepairHome, repair_home};

    // One failure mode per fixer shape; the subsystem matters where `--only` names one.
    const WRITE: &str = "fm-state_files-test-write";
    const OTHER_WRITE: &str = "fm-caches-test-write";
    const CLOSED_WRITE: &str = "fm-plugins-test-write-closed";
    const CHMOD: &str = "fm-secrets-test-chmod";
    const QUARANTINE: &str = "fm-schemas-test-quarantine";
    const FLATTEN: &str = "fm-external_artifacts-test-flatten";
    const FORCED: &str = "fm-concurrency_primitives-test-set-aside";
    const DELEGATE: &str = "fm-concurrency_primitives-test-delegate";
    const REFUSING: &str = "fm-configs-test-refusing";
    const MANUAL: &str = "fm-userland_state-test-manual";
    const MANY: &str = "fm-permissions-test-many";

    const CONTENT: &[u8] = b"repaired\n";
    const OTHER_RUN: &str = "2026-10-31T01-02-03Z__01M6A0000000000000000000ZZ";

    fn tx() -> TxId {
        "01JB8Q2VA00000000000000003".parse().unwrap()
    }

    fn mode(bits: u32) -> Mode {
        Mode::new(bits).unwrap()
    }

    // The fixers. Each reads what to repair from its finding's evidence.

    fn write(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        Ok(f.evidence
            .path
            .iter()
            .map(|path| Fix::Write {
                path: path.clone(),
                content: CONTENT.to_vec(),
                mode: mode(0o644),
            })
            .collect())
    }

    fn chmod(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        Ok(f.evidence
            .path
            .iter()
            .map(|path| Fix::Chmod {
                path: path.clone(),
                mode: mode(0o600),
            })
            .collect())
    }

    /// Into the quarantine of the side `evidence.reason` names; a client's is the dir in
    /// `evidence.paths`.
    fn quarantine(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        let root = match f.evidence.reason {
            Some("client") => QuarantineRoot::Client {
                client_dir: f
                    .evidence
                    .paths
                    .as_ref()
                    .and_then(|paths| paths.first())
                    .cloned()
                    .unwrap(),
            },
            Some("cache") => QuarantineRoot::Cache,
            Some("config") => QuarantineRoot::Config,
            Some("skill") => QuarantineRoot::Skill,
            _ => QuarantineRoot::State,
        };
        Ok(f.evidence
            .path
            .iter()
            .map(|path| Fix::Quarantine {
                path: path.clone(),
                root: root.clone(),
            })
            .collect())
    }

    /// `evidence.paths`: the inner folder, then its wrapper.
    fn flatten(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        Ok(match f.evidence.paths.as_deref() {
            Some([inner, outer]) => vec![Fix::Flatten {
                inner: inner.clone(),
                outer: outer.clone(),
            }],
            _ => Vec::new(),
        })
    }

    fn delegate(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        Ok(f.evidence
            .tx
            .iter()
            .map(|tx| Fix::Delegate {
                what: Delegation {
                    tx: *tx,
                    family: JournalFamily::Folder,
                    phase: JournalPhase::Prepared,
                    direction: Delegated::RollBack,
                },
            })
            .collect())
    }

    fn refusing(_: &DoctorView, _: &Finding) -> Result<Vec<Fix>, Refusal> {
        Err(Refusal {
            reason: RefusalReason::EditedLocally,
            message: "the file was edited by hand".to_owned(),
        })
    }

    /// `evidence.count` files created in the folder `evidence.path`.
    fn many(_: &DoctorView, f: &Finding) -> Result<Vec<Fix>, Refusal> {
        let (Some(dir), Some(count)) = (&f.evidence.path, f.evidence.count) else {
            return Ok(Vec::new());
        };
        Ok((0..count)
            .map(|i| Fix::Write {
                path: dir.join(i.to_string()),
                content: CONTENT.to_vec(),
                mode: mode(0o644),
            })
            .collect())
    }

    type PlanFn = fn(&DoctorView, &Finding) -> Result<Vec<Fix>, Refusal>;

    fn spec(
        plan: PlanFn,
        enabled: bool,
        needs_force: bool,
        preconditions: &'static [FixerPrecondition],
    ) -> FixerSpec {
        FixerSpec {
            plan,
            writes_to: &[ScopeName::State, ScopeName::Skill, ScopeName::AddOns],
            ops: &[FixKind::Write],
            enabled,
            needs_force,
            cardinality: Cardinality::Low,
            preconditions,
        }
    }

    fn row(id: &'static str, subsystem: Subsystem, fix: Option<FixerSpec>) -> FailureMode {
        FailureMode {
            id,
            subsystem,
            severity: Severity::P2,
            title: "a test state",
            description: "A state only these tests make.",
            why: "It exercises the planner.",
            tier: Tier::Default,
            online: false,
            cost_ms: 1,
            requires: &[],
            detect: None,
            fix,
            manual: None,
        }
    }

    fn rows() -> Vec<FailureMode> {
        use FixerPrecondition::{ClientNotRunning, InflightFree};
        vec![
            row(
                WRITE,
                Subsystem::StateFiles,
                Some(spec(write, true, false, &[])),
            ),
            row(
                OTHER_WRITE,
                Subsystem::Caches,
                Some(spec(write, true, false, &[])),
            ),
            row(
                CLOSED_WRITE,
                Subsystem::Plugins,
                Some(spec(write, true, false, &[ClientNotRunning])),
            ),
            row(
                CHMOD,
                Subsystem::Secrets,
                Some(spec(chmod, false, false, &[])),
            ),
            row(
                QUARANTINE,
                Subsystem::Schemas,
                Some(spec(quarantine, true, false, &[])),
            ),
            row(
                FLATTEN,
                Subsystem::ExternalArtifacts,
                Some(spec(flatten, true, false, &[ClientNotRunning])),
            ),
            row(
                FORCED,
                Subsystem::ConcurrencyPrimitives,
                Some(spec(quarantine, false, true, &[])),
            ),
            row(
                DELEGATE,
                Subsystem::ConcurrencyPrimitives,
                Some(spec(delegate, true, false, &[InflightFree])),
            ),
            row(
                REFUSING,
                Subsystem::Configs,
                Some(spec(refusing, true, false, &[])),
            ),
            row(MANUAL, Subsystem::UserlandState, None),
            row(
                MANY,
                Subsystem::Permissions,
                Some(spec(many, true, false, &[])),
            ),
        ]
    }

    /// `id`'s finding about `subject`.
    fn finding_as(id: &str, subject: Subject, evidence: Evidence) -> Finding {
        let rows = rows();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        row.finding(Observation {
            subject,
            title: "a test state".to_owned(),
            confidence: Confidence::High,
            addon: None,
            folder: None,
            evidence,
        })
    }

    /// `id`'s finding on retail, named by its evidence's path (the last of its paths),
    /// else its transaction.
    fn finding(id: &str, home: &RepairHome, evidence: Evidence) -> Finding {
        let subject = Subject::target("retail", home.key);
        let path = evidence
            .path
            .clone()
            .or_else(|| evidence.paths.as_ref().and_then(|p| p.last().cloned()));
        let subject = match (path, evidence.tx) {
            (Some(path), _) => subject.path(&path),
            (None, Some(tx)) => subject.tx(tx),
            (None, None) => subject,
        };
        finding_as(id, subject, evidence)
    }

    fn at(path: PathBuf) -> Evidence {
        Evidence {
            path: Some(path),
            ..Evidence::default()
        }
    }

    /// A quarantine of `path` into `side`'s root.
    fn into(home: &RepairHome, path: PathBuf, side: &'static str) -> Evidence {
        Evidence {
            path: Some(path),
            reason: Some(side),
            paths: (side == "client").then(|| vec![home.client.client_dir().to_path_buf()]),
            ..Evidence::default()
        }
    }

    fn lift(inner: PathBuf, outer: PathBuf) -> Evidence {
        Evidence {
            paths: Some(vec![inner, outer]),
            ..Evidence::default()
        }
    }

    fn of_tx(tx: TxId) -> Evidence {
        Evidence {
            tx: Some(tx),
            ..Evidence::default()
        }
    }

    type Mounts<'m> = &'m dyn Fn(&Path) -> std::io::Result<u64>;

    fn roots<'a>(
        home: &'a RepairHome,
        run: &'a RepairRun,
        tx: TxId,
        mount_id: Mounts<'a>,
    ) -> RepairRoots<'a> {
        LowerCtx {
            paths: &home.paths,
            tx,
            run,
            clients: &home.planned,
            skill_dirs: &home.skill_dirs,
            scopes: &home.scopes,
            mount_id,
        }
    }

    fn plan_with(
        home: &RepairHome,
        findings: &[Finding],
        only: &[Pick],
        force: bool,
        running: &RunningScan,
        mount_id: Mounts<'_>,
    ) -> Result<FixPlan, PlanRefusal> {
        let view = DoctorView::load_at(home.paths.clone(), ViewOptions::default());
        let roots = roots(home, &home.run, tx(), mount_id);
        let opts = FixOptions {
            only,
            force,
            roots: &roots,
            running,
        };
        plan_fix_in(&rows(), &view, findings, &opts)
    }

    fn plan(
        home: &RepairHome,
        findings: &[Finding],
        only: &[Pick],
    ) -> Result<FixPlan, PlanRefusal> {
        plan_with(
            home,
            findings,
            only,
            false,
            &RunningScan::default(),
            &crate::fsx::mount_id,
        )
    }

    fn planned(case: &str, result: Result<FixPlan, PlanRefusal>) -> FixPlan {
        match result {
            Ok(plan) => plan,
            Err(refusal) => panic!("{case}: refused: {refusal:#?}"), // ubs:ignore — fails the test
        }
    }

    fn refusal(case: &str, result: Result<FixPlan, PlanRefusal>) -> PlanRefusal {
        match result {
            Ok(plan) => panic!("{case}: planned {:#?}", plan.fixes), // ubs:ignore — fails the test
            Err(refusal) => refusal,
        }
    }

    fn fms(plan: &FixPlan) -> Vec<&'static str> {
        plan.fixes.iter().map(|fix| fix.fm).collect()
    }

    /// Creates the folder `path` with a TOC in it, as an addon folder.
    fn folder(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        std::fs::write(path.join(format!("{name}.toc")), "## Interface: 120105\n").unwrap();
    }

    fn text(path: &Path) -> String {
        path.display().to_string()
    }

    fn subtree(path: &Path) -> String {
        format!("{}/", path.display())
    }

    // -----------------------------------------------------------------------------------

    #[test]
    fn groups_findings_by_selection() {
        use Group::{Disabled, Manual, NeedsForce, NotSelected};
        let home = repair_home();
        let mut unfixable = finding(OTHER_WRITE, &home, at(home.skill_file()));
        unfixable.remediation.auto_fixable = false;
        let findings = [
            finding(WRITE, &home, at(home.skill_file())),
            finding(CHMOD, &home, at(home.credential())),
            finding(FORCED, &home, into(&home, home.record(), "state")),
            finding(MANUAL, &home, at(home.blob())),
            unfixable,
        ];
        let id = Pick::Mode;
        let sub = Pick::Subsystem;
        type Case = (
            &'static str,
            Vec<Pick>,
            bool,
            Vec<&'static str>,
            Vec<(&'static str, Group)>,
        );
        let cases: Vec<Case> = vec![
            (
                "default_plans_only_auto_fixable",
                vec![],
                false,
                vec![WRITE],
                vec![
                    (CHMOD, Disabled),
                    (FORCED, NeedsForce),
                    (MANUAL, Manual),
                    (OTHER_WRITE, Manual),
                ],
            ),
            (
                "only_id_plans_disabled_fixer",
                vec![id(CHMOD)],
                false,
                vec![CHMOD],
                vec![
                    (WRITE, NotSelected),
                    (FORCED, NotSelected),
                    (MANUAL, Manual),
                    (OTHER_WRITE, NotSelected),
                ],
            ),
            (
                "only_subsystem_never_enables_disabled",
                vec![sub(Subsystem::Secrets)],
                false,
                vec![],
                vec![
                    (WRITE, NotSelected),
                    (CHMOD, Disabled),
                    (FORCED, NotSelected),
                    (MANUAL, Manual),
                    (OTHER_WRITE, NotSelected),
                ],
            ),
            (
                "only_subsystem_plans_what_is_auto_fixable",
                vec![sub(Subsystem::StateFiles), sub(Subsystem::Caches)],
                false,
                vec![WRITE],
                vec![
                    (CHMOD, NotSelected),
                    (FORCED, NotSelected),
                    (MANUAL, Manual),
                    (OTHER_WRITE, Manual),
                ],
            ),
            (
                "only_id_needs_force_with_force_plans",
                vec![id(FORCED)],
                true,
                vec![FORCED],
                vec![
                    (WRITE, NotSelected),
                    (CHMOD, NotSelected),
                    (MANUAL, Manual),
                    (OTHER_WRITE, NotSelected),
                ],
            ),
            (
                "needs_force_without_only_is_listed",
                vec![],
                true,
                vec![WRITE],
                vec![
                    (CHMOD, Disabled),
                    (FORCED, NeedsForce),
                    (MANUAL, Manual),
                    (OTHER_WRITE, Manual),
                ],
            ),
            (
                "needs_force_through_a_subsystem_is_listed",
                vec![sub(Subsystem::ConcurrencyPrimitives)],
                true,
                vec![],
                vec![
                    (WRITE, NotSelected),
                    (CHMOD, NotSelected),
                    (FORCED, NeedsForce),
                    (MANUAL, Manual),
                    (OTHER_WRITE, NotSelected),
                ],
            ),
            (
                "no_fixer_is_manual",
                vec![id(MANUAL)],
                false,
                vec![],
                vec![
                    (WRITE, NotSelected),
                    (CHMOD, NotSelected),
                    (FORCED, NotSelected),
                    (MANUAL, Manual),
                    (OTHER_WRITE, NotSelected),
                ],
            ),
            (
                "only_id_plans_unfixable_instance_for_its_refusal",
                vec![id(OTHER_WRITE)],
                false,
                vec![OTHER_WRITE],
                vec![
                    (WRITE, NotSelected),
                    (CHMOD, NotSelected),
                    (FORCED, NotSelected),
                    (MANUAL, Manual),
                ],
            ),
        ];
        let rows = rows();
        for (case, only, force, want_planned, want_listed) in cases {
            let (chosen, listed) = match select(&rows, &findings, &only, force) {
                Ok(selected) => selected,
                Err(refusal) => panic!("{case}: refused: {refusal:?}"), // ubs:ignore — fails the test
            };
            let got: Vec<&str> = chosen.iter().map(|(_, f, _)| f.id).collect();
            assert_eq!(got, want_planned, "{case}: planned");
            let got: Vec<(&str, Group)> = listed.iter().map(|(_, u)| (u.fm, u.group)).collect();
            assert_eq!(got, want_listed, "{case}: listed");
            for (at, unplanned) in &listed {
                assert_eq!(
                    unplanned.instance,
                    findings[*at].instance.as_str(),
                    "{case}: listed by instance key, in the findings' order"
                );
            }
        }

        let refused = select(&rows, &findings, &[id(FORCED)], false);
        assert_eq!(
            refused.err(),
            Some(PlanRefusal::ForceRequired {
                fm: FORCED,
                target: Some("retail".to_owned()),
            }),
            "needs_force_only_without_force_refuses"
        );
    }

    #[test]
    fn force_gate() {
        let home = repair_home();
        let write = finding(WRITE, &home, at(home.skill_file()));

        let plan_of = |only: &[Pick], force| {
            plan_with(
                &home,
                std::slice::from_ref(&write),
                only,
                force,
                &RunningScan::default(),
                &crate::fsx::mount_id,
            )
        };
        let nothing = planned(
            "only_forced_without_finding_is_nothing_to_do",
            plan_of(&[Pick::Mode(FORCED)], false),
        );
        assert!(nothing.is_empty(), "{:?}", nothing.fixes);
        assert_eq!(
            nothing.unplanned,
            [unplanned_of(&write, Group::NotSelected)],
            "only_forced_without_finding_is_nothing_to_do"
        );
        assert_eq!(nothing.recommended_argv(None), None);

        let harmless = planned(
            "force_with_unforced_only_is_harmless",
            plan_of(&[Pick::Mode(WRITE)], true),
        );
        assert_eq!(fms(&harmless), [WRITE]);
    }

    #[test]
    fn refusals_in_order() {
        let home = repair_home();
        let many = home.paths.state_dir().join("many");
        std::fs::create_dir_all(&many).unwrap();
        let too_many = finding(
            MANY,
            &home,
            Evidence {
                path: Some(many.clone()),
                count: Some(1_001),
                ..Evidence::default()
            },
        );
        // Two modes that write one file.
        let both = [
            finding(WRITE, &home, at(home.skill_file())),
            finding(OTHER_WRITE, &home, at(home.skill_file())),
        ];
        let running = RunningScan {
            clients: BTreeMap::from([(
                home.client.client_dir().to_path_buf(),
                vec![RunningInfo {
                    pid: 4242,
                    evidence: RunningEvidence::ExePath,
                }],
            )]),
            unavailable: None,
        };
        let lift = finding(
            FLATTEN,
            &home,
            lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
        );
        let run_dir = home.run.dir.clone();
        let split = move |path: &Path| Ok(if path.starts_with(&run_dir) { 2 } else { 1 });

        let fixer_first = refusal(
            "fixer_before_conflict",
            plan(
                &home,
                &[
                    both[0].clone(),
                    both[1].clone(),
                    finding(REFUSING, &home, at(home.credential())),
                ],
                &[],
            ),
        );
        assert!(
            matches!(&fixer_first, PlanRefusal::Fixer { fm, .. } if *fm == REFUSING),
            "fixer_before_conflict: {fixer_first:?}"
        );

        let running_first = refusal(
            "precondition_before_count",
            plan_with(
                &home,
                &[too_many.clone(), lift.clone()],
                &[],
                false,
                &running,
                &crate::fsx::mount_id,
            ),
        );
        assert_eq!(
            running_first,
            PlanRefusal::ClientRunning {
                fm: FLATTEN,
                target: home.key.to_string(),
            },
            "precondition_before_count"
        );

        let mount_first = refusal(
            "lowering_before_count",
            plan_with(
                &home,
                &[
                    finding(QUARANTINE, &home, into(&home, home.record(), "state")),
                    too_many.clone(),
                ],
                &[],
                false,
                &RunningScan::default(),
                &split,
            ),
        );
        assert!(
            matches!(&mount_first, PlanRefusal::FilesystemMismatch { fm, .. } if *fm == QUARANTINE),
            "lowering_before_count: {mount_first:?}"
        );

        let count_first = refusal(
            "count_before_conflict",
            plan(&home, &[too_many, both[0].clone(), both[1].clone()], &[]),
        );
        assert!(
            matches!(
                &count_first,
                PlanRefusal::TooManyActions {
                    actions: 1_003,
                    fm: Some(MANY),
                    ..
                }
            ),
            "count_before_conflict: {count_first:?}"
        );

        if rustix::process::geteuid().is_root() {
            return; // root reads a folder of mode 000
        }
        use std::os::unix::fs::PermissionsExt;
        let journals = home.paths.journal_dir();
        let mode_of = std::fs::Permissions::from_mode;
        std::fs::set_permissions(&journals, mode_of(0o000)).unwrap();
        let unlisted = plan(&home, &both, &[]);
        std::fs::set_permissions(&journals, mode_of(0o755)).unwrap();
        assert!(
            matches!(&refusal("unlisted_before_conflict", unlisted), PlanRefusal::JournalsUnlisted { dir, .. } if *dir == journals),
            "unlisted_before_conflict"
        );
    }

    /// `details.fixes` of a conflict between `a` and `b`.
    fn pair(a: &Finding, b: &Finding) -> Vec<(String, String)> {
        [a, b]
            .iter()
            .map(|f| (f.id.to_owned(), instance_of(f)))
            .collect()
    }

    #[test]
    fn conflicts_by_overlap() {
        // Each case's findings on a home of its own, and the conflict it is refused with:
        // `(details.fixes, details.resources)`, or `None` when it plans.
        type Want = Option<(Vec<(String, String)>, Vec<String>)>;
        type Setup = fn(&RepairHome) -> (Vec<Finding>, Want);
        let cases: [(&str, Setup); 12] = [
            ("directory_and_its_child", |home| {
                let wrapper = finding(QUARANTINE, home, into(home, home.addons("Wrap"), "client"));
                let child = finding(WRITE, home, at(home.addons("Wrap/README.txt")));
                let resources = vec![
                    subtree(&home.addons("Wrap")),
                    text(&home.addons("Wrap/README.txt")),
                ];
                let want = Some((pair(&wrapper, &child), resources));
                (vec![wrapper, child], want)
            }),
            ("child_and_its_directory", |home| {
                let child = finding(WRITE, home, at(home.addons("Wrap/README.txt")));
                let wrapper = finding(QUARANTINE, home, into(home, home.addons("Wrap"), "client"));
                let resources = vec![
                    subtree(&home.addons("Wrap")),
                    text(&home.addons("Wrap/README.txt")),
                ];
                let want = Some((pair(&child, &wrapper), resources));
                (vec![child, wrapper], want)
            }),
            ("rename_destination_onto_another_fix", |home| {
                let lift = finding(
                    FLATTEN,
                    home,
                    lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
                );
                let file = finding(WRITE, home, at(home.addons("Inner")));
                let resources = vec![text(&home.addons("Inner")), subtree(&home.addons("Inner"))];
                let want = Some((pair(&lift, &file), resources));
                (vec![lift, file], want)
            }),
            ("same_path_two_fixes", |home| {
                let a = finding(WRITE, home, at(home.skill_file()));
                let b = finding(OTHER_WRITE, home, at(home.skill_file()));
                let want = Some((pair(&a, &b), vec![text(&home.skill_file())]));
                (vec![a, b], want)
            }),
            ("two_instances_of_one_mode", |home| {
                let retail = || Subject::target("retail", home.key);
                let a = finding_as(WRITE, retail().addon("A"), at(home.skill_file()));
                let b = finding_as(WRITE, retail().addon("B"), at(home.skill_file()));
                let want = Some((pair(&a, &b), vec![text(&home.skill_file())]));
                (vec![a, b], want)
            }),
            ("two_lifts_onto_one_name", |home| {
                folder(&home.addons("Other/Inner"));
                let a = finding(
                    FLATTEN,
                    home,
                    lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
                );
                let b = finding(
                    FLATTEN,
                    home,
                    lift(home.addons("Other/Inner"), home.addons("Other")),
                );
                let want = Some((pair(&a, &b), vec![subtree(&home.addons("Inner"))]));
                (vec![a, b], want)
            }),
            ("siblings_do_not_conflict", |home| {
                let notes = home.paths.skill_dir().join("NOTES.md");
                let findings = vec![
                    finding(WRITE, home, at(home.skill_file())),
                    finding(OTHER_WRITE, home, at(notes)),
                ];
                (findings, None)
            }),
            ("a_fixs_own_steps_never_conflict", |home| {
                let lift = finding(
                    FLATTEN,
                    home,
                    lift(home.addons("Same/Same"), home.addons("Same")),
                );
                (vec![lift], None)
            }),
            ("two_own_name_lifts_share_the_provisional_name", |home| {
                folder(&home.addons("Twin/Twin"));
                let findings = vec![
                    finding(
                        FLATTEN,
                        home,
                        lift(home.addons("Same/Same"), home.addons("Same")),
                    ),
                    finding(
                        FLATTEN,
                        home,
                        lift(home.addons("Twin/Twin"), home.addons("Twin")),
                    ),
                ];
                (findings, None)
            }),
            ("fix_against_undelegated_journal_refuses", |home| {
                let lock = home.paths.inflight_lock(&home.stuck());
                let fix = finding(QUARANTINE, home, into(home, lock.clone(), "state"));
                let journal = (
                    ids::JOURNAL_PENDING.to_owned(),
                    format!("tx={}", home.stuck()),
                );
                let fixes = vec![(fix.id.to_owned(), instance_of(&fix)), journal];
                (vec![fix], Some((fixes, vec![text(&lock)])))
            }),
            ("fix_against_unrelated_journal_plans", |home| {
                (vec![finding(WRITE, home, at(home.skill_file()))], None)
            }),
            ("fix_against_delegated_journal_is_left_to_f2", |home| {
                home.readable_stuck_journal();
                let lock = home.paths.inflight_lock(&home.stuck());
                let findings = vec![
                    finding(DELEGATE, home, of_tx(home.stuck())),
                    finding(QUARANTINE, home, into(home, lock, "state")),
                ];
                (findings, None)
            }),
        ];
        for (case, setup) in cases {
            let home = repair_home();
            let (findings, want) = setup(&home);
            let result = plan(&home, &findings, &[]);
            let Some((fixes, resources)) = want else {
                let plan = planned(case, result);
                assert_eq!(plan.fixes.len(), findings.len(), "{case}: one fix each");
                continue;
            };
            let refused = refusal(case, result);
            let err = refused.clone().into_error(None);
            assert_eq!(
                (err.code, err.details["reason"].as_str()),
                (ErrorCode::DoctorRefused, Some("fixer_conflict")),
                "{case}: {refused:?}"
            );
            let got: Vec<(String, String)> = err.details["fixes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    let field = |key: &str| f[key].as_str().unwrap().to_owned();
                    (field("fm"), field("instance"))
                })
                .collect();
            assert_eq!(got, fixes, "{case}: details.fixes");
            assert_eq!(
                err.details["resources"],
                json!(resources),
                "{case}: details.resources of {refused:?}"
            );
            assert_eq!(err.details["fm"], json!(fixes[0].0), "{case}: details.fm");
        }
    }

    #[test]
    fn max_actions() {
        for (case, count) in [("at_the_bound", 1_000), ("past_it", 1_001)] {
            let home = repair_home();
            let dir = home.paths.state_dir().join("many");
            std::fs::create_dir_all(&dir).unwrap();
            let evidence = Evidence {
                path: Some(dir),
                count: Some(count),
                ..Evidence::default()
            };
            let result = plan(&home, &[finding(MANY, &home, evidence)], &[]);
            if count <= DOCTOR_FIX_MAX_ACTIONS as u64 {
                let plan = planned(case, result);
                assert_eq!(plan.lowered.steps.len(), 1_000, "{case}");
                continue;
            }
            let refused = refusal(case, result);
            assert_eq!(
                refused,
                PlanRefusal::TooManyActions {
                    actions: 1_001,
                    fm: Some(MANY),
                    target: Some("retail".to_owned()),
                },
                "{case}"
            );
            let err = refused.into_error(None);
            assert_eq!(
                err.details,
                json!({
                    "reason": "too_many_actions",
                    "actions": 1_001,
                    "max_actions": 1_000,
                    "fm": MANY,
                }),
                "{case}"
            );
            assert_eq!(
                err.fix.argv(),
                Some(
                    argv_of(&[
                        "omaddons",
                        "doctor",
                        "fix",
                        "--only",
                        MANY,
                        "-t",
                        "retail",
                        "--dry-run"
                    ])
                    .as_slice()
                ),
                "{case}"
            );
        }
    }

    #[test]
    fn journals_and_delegations() {
        let home = repair_home();
        let stuck = home.stuck();
        let write = finding(WRITE, &home, at(home.skill_file()));
        let txs = |plan: &FixPlan| plan.journals.iter().map(|j| j.tx).collect::<Vec<_>>();

        // A journal whose writer is gone is observed, whatever its verdict.
        let unreadable = planned("unreadable", plan(&home, std::slice::from_ref(&write), &[]));
        assert_eq!(txs(&unreadable), [stuck], "unreadable");
        home.readable_stuck_journal();
        let readable = planned("readable", plan(&home, std::slice::from_ref(&write), &[]));
        assert_eq!(txs(&readable), [stuck], "readable");
        assert!(
            matches!(
                readable.journals[0].verdict,
                JournalVerdict::Recoverable { .. }
            ),
            "readable: {:?}",
            readable.journals[0].verdict
        );
        assert_ne!(
            unreadable.journals[0].verdict, readable.journals[0].verdict,
            "each as its verdict"
        );

        // A delegation of a journal recovery settles: one recovery, as diagnosed.
        let delegate = finding(DELEGATE, &home, of_tx(stuck));
        let delegated = planned(
            "delegated",
            plan(&home, std::slice::from_ref(&delegate), &[]),
        );
        let client = home.client.client_dir().to_path_buf();
        let diagnosis = journal::diagnose(&home.paths, &[client], &|_| true)
            .journals
            .into_iter()
            .find(|d| d.tx == stuck)
            .unwrap();
        let recoveries: Vec<_> = delegated
            .recoveries
            .iter()
            .map(|r| (r.fm, r.tx, r.family, r.phase, Some(r.direction)))
            .collect();
        assert_eq!(
            recoveries,
            [(
                DELEGATE,
                stuck,
                diagnosis.family,
                diagnosis.phase,
                diagnosis.direction()
            )],
            "delegated"
        );
        assert_eq!(
            delegated.targets,
            [home.key],
            "a recovery's client is a target"
        );
        assert!(
            delegated.lowered.steps.is_empty(),
            "a delegation is no primitive"
        );
        assert_eq!(
            delegated.recommended_argv(None),
            Some(argv_of(&["omaddons", "doctor", "fix", "--only", DELEGATE]))
        );

        // A transaction that left no journal is no recovery.
        let gone = finding(DELEGATE, &home, of_tx(TxId::new()));
        let gone = planned("gone", plan(&home, &[gone], &[]));
        assert!(gone.recoveries.is_empty(), "gone: {:?}", gone.recoveries);

        // A journal whose writer is alive is not observed: recovery skips it too.
        std::fs::remove_file(home.paths.inflight_lock(&stuck)).unwrap();
        let _writer = InflightLock::create(&home.paths, &stuck).unwrap();
        let live = planned("live", plan(&home, std::slice::from_ref(&write), &[]));
        assert!(live.journals.is_empty(), "live: {:?}", live.journals);
    }

    #[test]
    fn preconditions() {
        let home = repair_home();
        let lift = finding(
            FLATTEN,
            &home,
            lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
        );
        let running = RunningScan {
            clients: BTreeMap::from([(
                home.client.client_dir().to_path_buf(),
                vec![RunningInfo {
                    pid: 4242,
                    evidence: RunningEvidence::PrefixOnly,
                }],
            )]),
            unavailable: None,
        };
        let unknown = RunningScan {
            clients: BTreeMap::new(),
            unavailable: Some("/proc does not list this process".to_owned()),
        };
        let mounts = &crate::fsx::mount_id;
        let with = |findings: &[Finding], scan: &RunningScan| {
            plan_with(&home, findings, &[], false, scan, mounts)
        };
        let client_running = PlanRefusal::ClientRunning {
            fm: FLATTEN,
            target: home.key.to_string(),
        };
        for (case, scan) in [
            ("client_running_refuses", &running),
            ("unknown_running_state_refuses", &unknown),
        ] {
            assert_eq!(
                refusal(case, with(std::slice::from_ref(&lift), scan)),
                client_running,
                "{case}"
            );
        }
        let at_rest = planned(
            "client_at_rest_plans",
            with(std::slice::from_ref(&lift), &RunningScan::default()),
        );
        assert_eq!(fms(&at_rest), [FLATTEN]);
        let off_clients = finding(CLOSED_WRITE, &home, at(home.skill_file()));
        let off = planned(
            "a_fix_off_every_client_plans",
            with(&[off_clients], &unknown),
        );
        assert_eq!(fms(&off), [CLOSED_WRITE]);

        let busy = TxId::new();
        let _writer = InflightLock::create(&home.paths, &busy).unwrap();
        let held = finding(DELEGATE, &home, of_tx(busy));
        let dropped = planned(
            "inflight_held_drops_the_fix",
            plan(&home, std::slice::from_ref(&held), &[]),
        );
        assert!(dropped.is_empty(), "{:?}", dropped.fixes);
        assert_eq!(dropped.unplanned, [unplanned_of(&held, Group::InFlight)]);
    }

    #[test]
    fn bytes_to_back_up_sums_overwrites_only() {
        let home = repair_home();
        let seven = home.paths.skill_dir().join("SEVEN.md");
        std::fs::write(&seven, "1234567").unwrap();
        let notes = home.paths.skill_dir().join("NOTES.md");
        let findings = [
            finding(WRITE, &home, at(seven)),
            finding(OTHER_WRITE, &home, at(notes)),
        ];
        let plan = planned("bytes", plan(&home, &findings, &[]));
        assert_eq!(plan.bytes_to_back_up, 7, "{:#?}", plan.fixes);
    }

    #[test]
    fn refusal_errors() {
        let first = (FLATTEN.to_owned(), format!("{FLATTEN}@k:path=/a"));
        let conflict = |second: (&str, &str), journal| PlanRefusal::FixerConflict {
            fixes: Box::new([first.clone(), (second.0.to_owned(), second.1.to_owned())]),
            target: Some("retail".to_owned()),
            journal,
            resources: vec!["/a/".to_owned()],
        };
        let explain = |fm: &str, t: &str| argv_of(&["omaddons", "doctor", "explain", fm, "-t", t]);
        let preview = |fm: &str, t: &str| {
            argv_of(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                fm,
                "-t",
                t,
                "--dry-run",
            ])
        };
        let scope = Error::new(
            ErrorCode::ScopeDenied,
            "a path outside the doctor's scopes",
            Suggestion::text("nothing was written"),
        );
        // (case, refusal, the user's `-t`, code, reason, details keys, the fix's argv).
        type Case = (
            &'static str,
            PlanRefusal,
            Option<&'static str>,
            ErrorCode,
            Option<&'static str>,
            &'static [&'static str],
            Option<Vec<String>>,
        );
        let cases: Vec<Case> = vec![
            (
                "force_required",
                PlanRefusal::ForceRequired {
                    fm: FORCED,
                    target: Some("retail".to_owned()),
                },
                None,
                ErrorCode::DoctorRefused,
                Some("force_required"),
                &["fm", "reason"],
                Some(explain(FORCED, "retail")),
            ),
            (
                "force_required_keeps_the_users_target",
                PlanRefusal::ForceRequired {
                    fm: FORCED,
                    target: Some("retail".to_owned()),
                },
                Some("era"),
                ErrorCode::DoctorRefused,
                Some("force_required"),
                &["fm", "reason"],
                Some(explain(FORCED, "era")),
            ),
            (
                "fixer",
                PlanRefusal::Fixer {
                    fm: REFUSING,
                    target: Some("retail".to_owned()),
                    refusal: Refusal {
                        reason: RefusalReason::EditedLocally,
                        message: "the file was edited by hand".to_owned(),
                    },
                },
                None,
                ErrorCode::DoctorRefused,
                Some("edited_locally"),
                &["fm", "reason"],
                Some(explain(REFUSING, "retail")),
            ),
            (
                "client_running",
                PlanRefusal::ClientRunning {
                    fm: FLATTEN,
                    target: "retail".to_owned(),
                },
                None,
                ErrorCode::DoctorRefused,
                Some("client_running"),
                &["fm", "reason"],
                None,
            ),
            (
                "filesystem_mismatch",
                PlanRefusal::FilesystemMismatch {
                    fm: QUARANTINE,
                    target: None,
                    resources: vec!["/a".to_owned(), "/b".to_owned()],
                    message: "/a and /b: they are on different mounts".to_owned(),
                },
                None,
                ErrorCode::DoctorRefused,
                Some("filesystem_mismatch"),
                &["fm", "reason", "resources"],
                None,
            ),
            (
                "lowering",
                PlanRefusal::Lowering {
                    fm: Some(WRITE),
                    error: scope.clone(),
                },
                None,
                ErrorCode::ScopeDenied,
                None,
                &[],
                None,
            ),
            (
                "too_many_actions",
                PlanRefusal::TooManyActions {
                    actions: 1_001,
                    fm: Some(MANY),
                    target: None,
                },
                Some("era"),
                ErrorCode::DoctorRefused,
                Some("too_many_actions"),
                &["actions", "fm", "max_actions", "reason"],
                Some(preview(MANY, "era")),
            ),
            (
                "too_many_actions_of_no_mode",
                PlanRefusal::TooManyActions {
                    actions: 1_001,
                    fm: None,
                    target: None,
                },
                None,
                ErrorCode::DoctorRefused,
                Some("too_many_actions"),
                &["actions", "max_actions", "reason"],
                None,
            ),
            (
                "journals_unlisted",
                PlanRefusal::JournalsUnlisted {
                    dir: "/fixture/state/omaddons/journal".into(),
                    error: "/fixture/state/omaddons/journal: Permission denied".to_owned(),
                },
                None,
                ErrorCode::Io,
                Some("unattributed"),
                &["path", "reason"],
                Some(argv_of(&["omaddons", "status"])),
            ),
            (
                "fixer_conflict_of_two_modes",
                conflict((WRITE, "w"), false),
                None,
                ErrorCode::DoctorRefused,
                Some("fixer_conflict"),
                &["fixes", "fm", "reason", "resources"],
                Some(preview(FLATTEN, "retail")),
            ),
            (
                "fixer_conflict_of_one_mode",
                conflict((FLATTEN, "f"), false),
                None,
                ErrorCode::DoctorRefused,
                Some("fixer_conflict"),
                &["fixes", "fm", "reason", "resources"],
                Some(explain(FLATTEN, "retail")),
            ),
            (
                "fixer_conflict_with_a_journal",
                conflict(
                    (ids::JOURNAL_PENDING, "tx=01JB8Q2VA00000000000000009"),
                    true,
                ),
                None,
                ErrorCode::DoctorRefused,
                Some("fixer_conflict"),
                &["fixes", "fm", "reason", "resources"],
                Some(explain(FLATTEN, "retail")),
            ),
        ];
        // `doctor fix` arrives with its command leaf; until then its argvs cannot parse,
        // and from then on they are parsed like every other fix.
        let fix_exists = crate::app::command_entry("doctor fix").is_some();
        let mut unparsed = Vec::new();
        for (case, refusal, target, code, reason, keys, fix) in cases {
            let err = refusal.into_error(target);
            assert_eq!(err.code, code, "{case}: {}", err.message);
            assert_eq!(err.details["reason"].as_str(), reason, "{case}");
            let got: Vec<&str> = err
                .details
                .as_object()
                .map(|map| map.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(got, keys, "{case}: {}", err.details);
            assert_eq!(err.fix.argv(), fix.as_deref(), "{case}: {}", err.fix);
            let argv = err.fix.argv().unwrap_or_default();
            assert!(
                !argv.iter().any(|a| a == "--force" || a == "--yes"),
                "{case}: {argv:?}"
            );
            let names_fix = argv.get(1..3).is_some_and(|path| path == ["doctor", "fix"]);
            if names_fix && !fix_exists {
                unparsed.push(case);
            } else {
                assert!(crate::cli::parses(&err.fix), "{case}: {argv:?} parses");
            }
        }
        assert_eq!(
            unparsed.len(),
            if fix_exists { 0 } else { 2 },
            "only previews of doctor fix wait for their command: {unparsed:?}"
        );
    }

    #[test]
    fn a_whole_that_fails_while_each_fix_lowers_says_why() {
        // Two folders lifted to names that differ only in case: each lifts alone, the
        // resources differ, and the second finds the first's in the whole.
        let home = repair_home();
        folder(&home.addons("Other/INNER"));
        let findings = [
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
            ),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Other/INNER"), home.addons("Other")),
            ),
        ];
        let refused = refusal("case", plan(&home, &findings, &[]));
        let PlanRefusal::Lowering { fm, error } = &refused else {
            panic!("{refused:?}"); // ubs:ignore — fails the test
        };
        assert_eq!(*fm, Some(FLATTEN));
        assert_eq!(error.code, ErrorCode::FileExists, "{}", error.message);
    }

    #[test]
    fn remediation_for_fixer_rows() {
        let home = repair_home();
        let view = DoctorView::load_at(home.paths.clone(), ViewOptions::default());
        let rows = rows();
        let spec_of = |id: &str| rows.iter().find(|r| r.id == id).unwrap().fix.unwrap();
        let mut unfixable = finding(WRITE, &home, at(home.skill_file()));
        unfixable.remediation.auto_fixable = false;
        // (case, finding, target, argv after `omaddons doctor fix --only <fm>`,
        // auto_fixable, needs_force, estimated_actions).
        type Case = (
            &'static str,
            Finding,
            Option<&'static str>,
            &'static [&'static str],
            bool,
            bool,
            u32,
        );
        let cases: Vec<Case> = vec![
            (
                "enabled",
                finding(WRITE, &home, at(home.skill_file())),
                None,
                &[],
                true,
                false,
                1,
            ),
            (
                "with_target",
                finding(WRITE, &home, at(home.skill_file())),
                Some("retail"),
                &["-t", "retail"],
                true,
                false,
                1,
            ),
            (
                "forced",
                finding(FORCED, &home, into(&home, home.record(), "state")),
                Some("retail"),
                &["-t", "retail", "--force", "--yes"],
                false,
                true,
                1,
            ),
            (
                "disabled",
                finding(CHMOD, &home, at(home.credential())),
                None,
                &[],
                false,
                false,
                1,
            ),
            ("unfixable_instance", unfixable, None, &[], false, false, 1),
            (
                "refusing",
                finding(REFUSING, &home, at(home.credential())),
                None,
                &[],
                true,
                false,
                0,
            ),
            (
                "own_name_lift",
                finding(
                    FLATTEN,
                    &home,
                    lift(home.addons("Same/Same"), home.addons("Same")),
                ),
                Some("retail"),
                &["-t", "retail"],
                true,
                false,
                3,
            ),
        ];
        for (case, f, target, tail, auto, force, actions) in cases {
            let got = remediation_for(&view, &f, &spec_of(f.id), target);
            let mut argv = argv_of(&["omaddons", "doctor", "fix", "--only", f.id]);
            argv.extend(argv_of(tail));
            assert_eq!(got.argv.as_ref(), Some(&argv), "{case}");
            assert_eq!(got.command, shell_words::join(&argv), "{case}");
            assert_eq!(
                got.explain_argv,
                explain_argv(f.id, target),
                "{case}: explain"
            );
            assert_eq!(
                (
                    got.auto_fixable,
                    got.needs_force,
                    got.estimated_actions,
                    got.destructive
                ),
                (auto, force, actions, false),
                "{case}"
            );
        }

        // The estimate is what lowering makes of the fixer's fixes.
        let lift = finding(
            FLATTEN,
            &home,
            lift(home.addons("Same/Same"), home.addons("Same")),
        );
        let estimate = remediation_for(&view, &lift, &spec_of(FLATTEN), None).estimated_actions;
        let lowered = planned("lift", plan(&home, &[lift], &[]));
        assert_eq!(estimate as usize, lowered.lowered.steps.len());
    }

    #[test]
    fn primitive_count_matches_lowering() {
        let home = repair_home();
        folder(&home.addons("Mixed/MIXED"));
        let stuck = home.stuck();
        let client = QuarantineRoot::Client {
            client_dir: home.client.client_dir().to_path_buf(),
        };
        let suspend = Fix::Suspend {
            journal: JournalSet {
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
                        root: client,
                    },
                ],
                guards: Vec::new(),
                attribution: Attribution::Complete,
                unattributed: Vec::new(),
            },
        };
        let flatten = |inner: &str, outer: &str| Fix::Flatten {
            inner: home.addons(inner),
            outer: home.addons(outer),
        };
        let cases = [
            (
                "write_over",
                Fix::Write {
                    path: home.skill_file(),
                    content: CONTENT.to_vec(),
                    mode: mode(0o644),
                },
            ),
            (
                "write_create",
                Fix::Write {
                    path: home.paths.skill_dir().join("NOTES.md"),
                    content: CONTENT.to_vec(),
                    mode: mode(0o644),
                },
            ),
            (
                "chmod",
                Fix::Chmod {
                    path: home.credential(),
                    mode: mode(0o600),
                },
            ),
            (
                "quarantine",
                Fix::Quarantine {
                    path: home.record(),
                    root: QuarantineRoot::State,
                },
            ),
            ("flatten", flatten("Wrap/Inner", "Wrap")),
            ("flatten_own_name", flatten("Same/Same", "Same")),
            (
                "flatten_own_name_in_another_case",
                flatten("Mixed/MIXED", "Mixed"),
            ),
            ("suspend", suspend),
            (
                "delegate",
                Fix::Delegate {
                    what: Delegation {
                        tx: stuck,
                        family: JournalFamily::Folder,
                        phase: JournalPhase::Prepared,
                        direction: Delegated::RollBack,
                    },
                },
            ),
        ];
        let roots = roots(&home, &home.run, tx(), &crate::fsx::mount_id);
        for (case, fix) in cases {
            let input = repair::PlannedFix {
                fm: Cow::Borrowed(WRITE),
                fix: fix.clone(),
            };
            let lowered = match repair::lower(&roots, &[input]) {
                Ok(lowered) => lowered,
                Err(err) => panic!("{case}: {err}"), // ubs:ignore — fails the test
            };
            assert_eq!(
                fix.primitive_count() as usize,
                lowered.steps.len(),
                "{case}: {fix:?}"
            );
        }
    }

    #[test]
    fn triage_and_recommended_argv() {
        let home = repair_home();
        let skill_size = std::fs::metadata(home.skill_file()).unwrap().len();
        let findings = [
            finding(WRITE, &home, at(home.skill_file())),
            finding(
                OTHER_WRITE,
                &home,
                at(home.paths.skill_dir().join("NOTES.md")),
            ),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
            ),
        ];
        let plan = planned("triage", plan(&home, &findings, &[]));
        let dirs = |paths: &[&Path]| {
            let mut dirs: Vec<String> = paths.iter().map(|path| text(path)).collect();
            dirs.sort();
            dirs
        };
        let (state, skill) = (home.paths.state_dir(), home.paths.skill_dir());
        let content = CONTENT.len() as u64;
        assert_eq!(
            plan.triage(),
            [
                TriageAction {
                    fixer_id: WRITE,
                    writes_to: dirs(&[state, &skill]),
                    estimated_bytes: content + skill_size,
                },
                TriageAction {
                    fixer_id: OTHER_WRITE,
                    writes_to: dirs(&[state, &skill]),
                    estimated_bytes: content,
                },
                TriageAction {
                    fixer_id: FLATTEN,
                    writes_to: dirs(&[state, &skill, &home.client.addons()]),
                    estimated_bytes: 0,
                },
            ]
        );
        let only = [WRITE, OTHER_WRITE, FLATTEN].join(",");
        assert_eq!(
            plan.recommended_argv(None),
            Some(argv_of(&["omaddons", "doctor", "fix", "--only", &only]))
        );
        assert_eq!(
            plan.recommended_argv(Some("retail")),
            Some(argv_of(&[
                "omaddons", "doctor", "fix", "--only", &only, "-t", "retail"
            ]))
        );

        // A forced fixer is never in a recommendation.
        let forced = finding(FORCED, &home, into(&home, home.record(), "state"));
        let forced = planned(
            "forced",
            plan_with(
                &home,
                &[forced],
                &[Pick::Mode(FORCED)],
                true,
                &RunningScan::default(),
                &crate::fsx::mount_id,
            ),
        );
        assert_eq!(fms(&forced), [FORCED]);
        assert_eq!(forced.recommended_argv(None), None);
    }

    #[test]
    fn doctor_fix_token_has_nothing_run_specific() {
        use crate::app::precondition::of_doctor_fix;
        let home = repair_home();
        let other = RepairRun {
            dir: home.paths.doctor_runs_dir().join(OTHER_RUN),
            id: OTHER_RUN.to_owned(),
        };
        std::fs::create_dir_all(&other.dir).unwrap();
        let findings = [
            finding(QUARANTINE, &home, into(&home, home.record(), "state")),
            finding(QUARANTINE, &home, into(&home, home.blob(), "cache")),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Same/Same"), home.addons("Same")),
            ),
            finding(WRITE, &home, at(home.skill_file())),
        ];
        let view = DoctorView::load_at(home.paths.clone(), ViewOptions::default());
        let running = RunningScan::default();
        let rows = rows();
        let plan_in = |run: &RepairRun, tx: TxId| {
            let roots = roots(&home, run, tx, &crate::fsx::mount_id);
            let opts = FixOptions {
                only: &[],
                force: false,
                roots: &roots,
                running: &running,
            };
            planned("run", plan_fix_in(&rows, &view, &findings, &opts))
        };
        let first = plan_in(&home.run, tx());
        let second = plan_in(&other, TxId::new());
        assert_ne!(
            first.lowered, second.lowered,
            "each repair names its own run and transaction"
        );
        assert_eq!(first.bound(), second.bound());
        let [a, b] = [&first, &second].map(of_doctor_fix);
        assert_eq!(a.parts(), b.parts());
        assert_eq!(a.token(), b.token());
    }

    #[test]
    fn dry_run_paths_use_placeholders() {
        let home = repair_home();
        let findings = [
            finding(WRITE, &home, at(home.skill_file())),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Same/Same"), home.addons("Same")),
            ),
        ];
        let plan = planned("placeholders", plan(&home, &findings, &[]));
        let planned = plan.actions_planned();
        let ops: Vec<serde_json::Value> = planned
            .iter()
            .map(|a| serde_json::to_value(&a.step).unwrap())
            .collect();
        let field = |at: usize, key: &str| ops[at][key].as_str().unwrap().to_owned();
        let provisional = text(&home.addons(".omaddons-flatten-<tx>"));
        let parked = home
            .client
            .client_dir()
            .join("Interface/.omaddons/snapshots/<tx>.repair/quarantine/Interface/AddOns/Same");
        let backup = home.paths.doctor_runs_dir().join("<run>/backups/1");
        let want = [
            (field(0, "path"), text(&home.skill_file())),
            (field(0, "backup"), text(&backup)),
            (field(1, "to"), provisional.clone()),
            (field(2, "path"), format!("{provisional}/Same")),
            (field(3, "path"), provisional),
            (field(3, "to"), text(&parked)),
        ];
        for (got, want) in want {
            assert_eq!(got, want);
        }
        let all = serde_json::to_string(&ops).unwrap();
        let (tx, run) = (tx().to_string(), REPAIR_RUN_ID);
        assert!(!all.contains(&tx), "no transaction id: {all}");
        assert!(!all.contains(run), "no run id: {all}");
        // The wrapper moves whole, the lifted folder alone, then the wrapper without it.
        let bytes: Vec<u64> = planned.iter().map(|a| a.bytes).collect();
        let whole = size_of(&home.addons("Same"), &[]);
        let inner = size_of(&home.addons("Same/Same"), &[]);
        assert!(inner > 0 && whole > inner, "{whole} {inner}");
        assert_eq!(
            bytes,
            [CONTENT.len() as u64, whole, inner, whole - inner],
            "a write's content, then each rename's subject"
        );
    }

    #[test]
    fn to_rel_per_root() {
        let home = repair_home();
        let below_home = home.skill_file();
        let below_home = below_home.strip_prefix(home.paths.home()).unwrap();
        let findings = [
            finding(QUARANTINE, &home, into(&home, home.record(), "state")),
            finding(QUARANTINE, &home, into(&home, home.credential(), "config")),
            finding(QUARANTINE, &home, into(&home, home.blob(), "cache")),
            finding(
                QUARANTINE,
                &home,
                into(&home, home.addons("Stray"), "client"),
            ),
            finding(QUARANTINE, &home, into(&home, home.skill_file(), "skill")),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Wrap/Inner"), home.addons("Wrap")),
            ),
            finding(
                FLATTEN,
                &home,
                lift(home.addons("Same/Same"), home.addons("Same")),
            ),
        ];
        let plan = planned("to_rel", plan(&home, &findings, &[]));
        let got: Vec<(String, Option<String>)> = plan
            .bound()
            .steps
            .into_iter()
            .map(|step| (step.path, step.to_rel))
            .collect();
        let provisional = text(&home.addons(".omaddons-flatten-<tx>"));
        let some = |s: &str| Some(s.to_owned());
        let want = vec![
            (
                text(&home.record()),
                Some(format!("state/targets/{}.json", home.key)),
            ),
            (text(&home.credential()), some("config/credentials/github")),
            (text(&home.blob()), some("cache/blobs/ab")),
            (
                text(&home.addons("Stray")),
                some("client/Interface/AddOns/Stray"),
            ),
            (
                text(&home.skill_file()),
                Some(format!("skill/{}", below_home.display())),
            ),
            (
                text(&home.addons("Wrap/Inner")),
                Some(text(&home.addons("Inner"))),
            ),
            (
                text(&home.addons("Wrap")),
                some("client/Interface/AddOns/Wrap"),
            ),
            (text(&home.addons("Same")), Some(provisional.clone())),
            (
                format!("{provisional}/Same"),
                Some(text(&home.addons("Same"))),
            ),
            (provisional, some("client/Interface/AddOns/Same")),
        ];
        assert_eq!(got, want);
    }
}
