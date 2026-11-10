//! `doctor fix`'s and `doctor diff`'s runtime (E6 s3; M4-D32, M4-D37, M4-D44, M4-D45,
//! §6.12). A fix plans without the lock; a dry run ends there. A real run takes the
//! global lock and, under it, in this order and no other: a fresh view, the planned
//! failure modes detected again, the plan made again, its token compared with
//! `--if-match`, then the recoveries the plan disclosed, then the repair's primitives
//! (E5's commit). It verifies after releasing the lock and records the run with an
//! `undo.sh`. The order is written once, in [`fix_flow`], over the effects it calls, so a
//! recording fake proves it on every path: nothing is recovered or moved before the
//! comparison, and nothing at all after a mismatch.
//!
//! `doctor diff` plans as a default `doctor fix --dry-run` does, with no run directory,
//! no lock and no write.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use super::delegate::{DelegatedOutcome, RecoveryDelegate, RecoveryOutcome};
use super::exit::{Exit, exit_for};
use super::finding::Finding;
use super::fix::{
    FixOptions, FixPlan, PlanRefusal, PlannedPrune, PlannedRecovery, RepairRoots, plan_fix_in,
};
use super::plan::DiagnosePlan;
use super::registry::{FailureMode, ids};
use super::report::{
    self, ActionDone, DOCTOR_CONTRACT_VERSION, DiffReport, FixReport, FixSummary, PruneDone,
    PruneOutcome, RunState,
};
use super::runtime::{self, Diagnosis, DoctorRunId, Runtime};
use super::selection::Selection;
use super::undo_script::{UNDO_SCRIPT, undo_script};
use super::view::{DoctorView, ViewOptions};
use crate::app::precondition::{self, of_doctor_fix};
use crate::contract::actions::Action;
use crate::discover::running::{RunningScan, detect_running};
use crate::engine::fence;
use crate::engine::journal::JournalVerdict;
use crate::engine::repair::{
    self, CommitCtx, RepairCleanup, RepairMeta, RepairOutcome, RepairRun, RepairTx,
};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{self, GlobalLock, GlobalLockGuard, ScopeTable};
use crate::model::{Actor, RunId, Target, TargetKey, TxId};
use crate::output::{DoctorEnvelope, DoctorFailure, Meta, RunKind, RunNotes};
use crate::state::paths::Paths;

/// The repair journal's actions log in the run directory (E5's tracked session).
pub const ACTIONS_LOG: &str = "actions.jsonl";

// ---------------------------------------------------------------------------------------
// Requests and outcomes.

/// What a `doctor fix` run asks for.
pub struct FixRequest {
    /// `-t`, `--only` and `--skip`.
    pub selection: Selection,
    /// The checks it makes: E3's plan of the selection.
    pub checks: DiagnosePlan,
    /// `--force` (with `--yes`).
    pub force: bool,
    pub dry_run: bool,
    pub if_match: Option<String>,
    /// The command line, for history, the preview of a stale run and the hints.
    pub argv: Vec<String>,
    pub actor: Actor,
    pub reason: Option<String>,
    pub offline: bool,
    /// The run's notes: what it warned of before and while it ran, recorded with it.
    pub notes: RunNotes,
}

/// A fix run's result: its report, and the doctor exit it ends with.
#[derive(Debug)]
pub struct FixOutcome {
    pub report: FixReport,
    pub exit: Exit,
    /// A run that refused, lost the lock or failed (beside its report).
    pub error: Option<Error>,
    pub warnings: Vec<Warning>,
    pub commands: Vec<CommandHint>,
    /// `-v` debug notes for the dispatch arm to log.
    pub notes: Vec<String>,
    /// The set-asides the run planned (M4-D38), for the human output.
    pub suspended: Vec<crate::doctor::fix::Suspended>,
}

/// What a `doctor diff` asks for.
pub struct DiffRequest {
    pub selection: Selection,
    pub checks: DiagnosePlan,
    /// A run to compare with, and the instance keys it reported; `None` for what a fix
    /// would change.
    pub since: Option<(DoctorRunId, Vec<String>)>,
}

/// A diff's result.
#[derive(Debug)]
pub struct DiffOutcome {
    pub report: DiffReport,
    pub exit: Exit,
    pub warnings: Vec<Warning>,
    pub commands: Vec<CommandHint>,
}

// ---------------------------------------------------------------------------------------
// The order.

/// Why a run stopped before it changed anything (or a commit could not finish its
/// rollback).
#[derive(Debug, Clone)]
pub(crate) enum Stop {
    Plan(PlanRefusal),
    Error(Error),
}

impl Stop {
    fn code(&self) -> ErrorCode {
        match self {
            Self::Plan(refusal) => refusal.code(),
            Self::Error(error) => error.code,
        }
    }

    fn into_error(self, target: Option<&str>) -> Error {
        match self {
            Self::Plan(refusal) => refusal.into_error(target),
            Self::Error(error) => error,
        }
    }
}

/// What a run verified after releasing the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Verification {
    /// The planned instances still found.
    pub remaining: u32,
}

/// How a run ended, as [`fix_flow`] hands it to `finish`.
#[derive(Debug)]
pub(crate) enum Terminal {
    /// A dry run's plan.
    Planned(FixPlan),
    /// It refused, lost the lock or failed: nothing was repaired. `delegated` holds the
    /// recoveries that ran before.
    Stopped {
        stop: Stop,
        delegated: Vec<DelegatedOutcome>,
        /// The prunes that ran before.
        pruned: Vec<PruneDone>,
    },
    /// A real run that ended: with nothing to do, committed, or rolled back.
    Done {
        plan: FixPlan,
        delegated: Vec<DelegatedOutcome>,
        pruned: Vec<PruneDone>,
        commit: Option<RepairOutcome>,
        verification: Option<Verification>,
    },
}

/// The effects of a fix run, in the order [`fix_flow`] calls them.
pub(crate) trait FixEffects {
    type Lock;
    type Out;
    /// `PLANNING`, without the lock.
    fn plan(&mut self) -> std::result::Result<FixPlan, Stop>;
    /// `doctor.fix.planned`.
    fn planned_failpoint(&mut self) -> std::io::Result<()>;
    /// `ACQUIRING_LOCK`.
    fn acquire(&mut self) -> Result<Self::Lock>;
    /// `PLANNING` under the lock: a fresh view, the planned modes detected again.
    fn fresh_plan(&mut self, lock: &Self::Lock) -> std::result::Result<FixPlan, Stop>;
    /// `--if-match` against `plan`'s token.
    fn compare(&mut self, plan: &FixPlan) -> Result<()>;
    fn delegate(
        &mut self,
        lock: &Self::Lock,
        disclosed: &[PlannedRecovery],
    ) -> Result<Vec<DelegatedOutcome>>;
    /// Engine retention prunes exactly the plan's prunes still due, after the recoveries.
    fn retain(&mut self, lock: &Self::Lock, prunes: &[PlannedPrune]) -> Vec<PruneDone>;
    /// `MUTATING`: E5's commit, which rolls back a failed step itself.
    fn commit(&mut self, lock: &Self::Lock, plan: &FixPlan) -> Result<RepairOutcome>;
    fn release(&mut self, lock: Self::Lock);
    /// `VERIFYING`, outside the lock.
    fn verify(&mut self, plan: &FixPlan) -> Verification;
    fn finish(&mut self, terminal: Terminal) -> Self::Out;
}

/// A fix run's order (M4-D45). A dry run never takes the lock; a bound run compares its
/// token under the lock before the first recovery or primitive, and on a mismatch or a
/// refusal changes nothing; the delegate runs at most once, with the plan's recoveries
/// only, before the commit; verification runs after the lock is released, and only after
/// a commit or a delegation.
pub(crate) fn fix_flow<F: FixEffects + ?Sized>(fx: &mut F, dry_run: bool, bound: bool) -> F::Out {
    let stopped = |stop| Terminal::Stopped {
        stop,
        delegated: Vec::new(),
        pruned: Vec::new(),
    };
    let plan = match fx.plan() {
        Ok(plan) => plan,
        Err(stop) => return fx.finish(stopped(stop)),
    };
    if let Err(err) = fx.planned_failpoint() {
        return fx.finish(stopped(Stop::Error(failpoint_error(&err))));
    }
    if dry_run {
        if bound && let Err(error) = fx.compare(&plan) {
            return fx.finish(stopped(Stop::Error(error)));
        }
        return fx.finish(Terminal::Planned(plan));
    }
    let lock = match fx.acquire() {
        Ok(lock) => lock,
        Err(error) => return fx.finish(stopped(Stop::Error(error))),
    };
    let fresh = match fx.fresh_plan(&lock) {
        Ok(plan) => plan,
        Err(stop) => {
            fx.release(lock);
            return fx.finish(stopped(stop));
        }
    };
    if bound && let Err(error) = fx.compare(&fresh) {
        fx.release(lock);
        return fx.finish(stopped(Stop::Error(error)));
    }
    if fresh.is_empty() {
        fx.release(lock);
        return fx.finish(Terminal::Done {
            plan: fresh,
            delegated: Vec::new(),
            pruned: Vec::new(),
            commit: None,
            verification: None,
        });
    }
    let mut delegated = Vec::new();
    if !fresh.recoveries.is_empty() {
        match fx.delegate(&lock, &fresh.recoveries) {
            Ok(outcomes) => delegated = outcomes,
            Err(error) => {
                fx.release(lock);
                return fx.finish(stopped(Stop::Error(error)));
            }
        }
    }
    // After the recoveries, as a commit's retention runs after recovery.
    let pruned = if fresh.prunes.is_empty() {
        Vec::new()
    } else {
        fx.retain(&lock, &fresh.prunes)
    };
    let commit = if fresh.has_steps() {
        match fx.commit(&lock, &fresh) {
            Ok(outcome) => Some(outcome),
            Err(error) => {
                fx.release(lock);
                return fx.finish(Terminal::Stopped {
                    stop: Stop::Error(error),
                    delegated,
                    pruned,
                });
            }
        }
    } else {
        None
    };
    fx.release(lock);
    if matches!(commit, Some(RepairOutcome::RolledBack { .. })) {
        return fx.finish(Terminal::Done {
            plan: fresh,
            delegated,
            pruned,
            commit,
            verification: None,
        });
    }
    let verification = fx.verify(&fresh);
    fx.finish(Terminal::Done {
        plan: fresh,
        delegated,
        pruned,
        commit,
        verification: Some(verification),
    })
}

fn failpoint_error(err: &std::io::Error) -> Error {
    Error::new(
        ErrorCode::Io,
        format!("doctor fix stopped after planning: {err}"),
        Suggestion::text("nothing was changed; run doctor fix again"),
    )
}

/// The state and exit of a run that ended as `terminal`, `findings` being whether any
/// finding remains in its report. A stop is `REFUSING` when its error exits 4,
/// `LOCK_LOST` when it exits 5, else `DONE_FAILED` with its error's exit.
/// Whether `f` is the runtime's finding about the check of one of `modes` (its
/// `evidence.detector`: a check cut short or crashed).
fn about_check_of(f: &Finding, modes: &BTreeSet<&'static str>) -> bool {
    f.evidence.detector.is_some_and(|d| modes.contains(d))
}

pub(crate) fn state_of(terminal: &Terminal, findings: bool) -> (RunState, Exit) {
    let state = match terminal {
        Terminal::Stopped { stop, .. } => {
            let exit = exit_for(stop.code());
            let state = match exit {
                Exit::RefusedUnsafe => RunState::Refusing,
                Exit::ConcurrencyLost => RunState::LockLost,
                _ => RunState::DoneFailed,
            };
            return (state, exit);
        }
        Terminal::Done {
            commit: Some(RepairOutcome::RolledBack { .. }),
            ..
        } => RunState::DoneFailed,
        Terminal::Done {
            verification: Some(verification),
            delegated,
            pruned,
            ..
        } => {
            let settled = delegated.iter().all(|d| {
                matches!(
                    d.outcome,
                    RecoveryOutcome::RolledBack | RecoveryOutcome::RolledForward
                )
            }) && pruned.iter().all(|p| p.outcome == PruneOutcome::Pruned);
            // A repaired mode still found, or a recovery left unsettled, is partial; with
            // the repairs verified, anything else the report still holds (another mode, an
            // instance its fixer cannot repair, a check that could not finish) is left, so
            // the run is never called healthy (M4-D43).
            if verification.remaining > 0 || !settled {
                RunState::DonePartial
            } else if findings {
                RunState::DoneFindings
            } else {
                RunState::DoneOk
            }
        }
        Terminal::Planned(_) | Terminal::Done { .. } if findings => RunState::DoneFindings,
        Terminal::Planned(_) | Terminal::Done { .. } => RunState::DoneOk,
    };
    (state, state.exit())
}

// ---------------------------------------------------------------------------------------
// The real effects.

struct RealEffects<'a> {
    req: &'a FixRequest,
    paths: &'a Paths,
    scopes: &'a ScopeTable,
    delegate: &'a dyn RecoveryDelegate,
    /// Taken by `finish`.
    runtime: Option<Runtime<'a>>,
    run: RepairRun,
    diagnosis: Diagnosis,
    /// The registered clients a repair may touch, with their keys.
    clients: Vec<(PathBuf, TargetKey)>,
    skill_dirs: Vec<PathBuf>,
    running: RunningScan,
    /// The clients the view under the lock discovered, for the delegation.
    fresh_targets: Vec<Target>,
    /// A real run's transaction, held from planning to its commit.
    rtx: Option<RepairTx>,
    tx: TxId,
    /// The transaction a commit started, whether it committed or rolled back.
    started: Option<TxId>,
    /// The failure modes the plan before the lock repairs: what the run detects again.
    modes: BTreeSet<&'static str>,
    /// Those modes' findings as last detected, under the lock and then after it.
    fresh: Option<Vec<Finding>>,
    /// The failure modes verification checked again, once it did (`fresh` then covers
    /// them, the runtime's findings about their checks included); `None` while `fresh`
    /// covers [`Self::modes`] alone.
    rechecked: Option<BTreeSet<&'static str>>,
    cleanup: Option<RepairCleanup>,
    warnings: Vec<Warning>,
    notes: Vec<String>,
    clock: Instant,
}

impl RealEffects<'_> {
    /// A real run that stopped before its commit (a refusal, a stale token, nothing to
    /// repair) gives up its transaction (E5's `abandon`).
    fn abandon(&mut self) {
        if let Some(rtx) = self.rtx.take() {
            self.warnings.extend(repair::abandon(rtx, self.scopes));
        }
    }

    fn plan_on(
        &self,
        view: &DoctorView,
        findings: &[Finding],
    ) -> std::result::Result<FixPlan, PlanRefusal> {
        let roots = RepairRoots {
            paths: self.paths,
            tx: self.tx,
            run: &self.run,
            clients: &self.clients,
            skill_dirs: &self.skill_dirs,
            scopes: self.scopes,
            mount_id: &fsx::mount_id,
        };
        let opts = FixOptions {
            only: &self.req.selection.only,
            force: self.req.force,
            roots: &roots,
            running: &self.running,
        };
        plan_fix_in(&self.req.checks.rows, view, findings, &opts)
    }

    /// The planned modes' findings in a fresh view.
    fn detect_again(&self, view: &DoctorView) -> Result<Vec<Finding>> {
        self.detect_modes(view, &self.modes, false)
    }

    /// The findings of `modes` in a fresh view, and with `runtime` the runtime's findings
    /// about their checks (a check cut short or crashed).
    fn detect_modes(
        &self,
        view: &DoctorView,
        modes: &BTreeSet<&'static str>,
        runtime: bool,
    ) -> Result<Vec<Finding>> {
        let rows: Vec<FailureMode> = self
            .req
            .checks
            .rows
            .iter()
            .filter(|row| modes.contains(row.id))
            .copied()
            .collect();
        let checks = &self.req.checks;
        let found = runtime::diagnose(
            &rows,
            view,
            &self.req.selection,
            &checks.tiers,
            checks.budgets,
        )?;
        Ok(found
            .findings
            .into_iter()
            .filter(|f| modes.contains(f.id) || runtime && about_check_of(f, modes))
            .collect())
    }

    /// What verification checks again (M4-D43): the repaired modes, and every other
    /// offline mode the diagnosis found or could not finish checking, since a repair or a
    /// recovery may have settled it (a rolled-forward update leaves its files as its
    /// record says). An online finding is about a remote state no local repair changes,
    /// so it stands as found, and a runtime row has no check of its own.
    fn rechecked_modes(&self) -> BTreeSet<&'static str> {
        let rows = &self.req.checks.rows;
        let offline_check = |id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .filter(|row| row.detect.is_some() && !row.online)
                .map(|row| row.id)
        };
        let mut modes = self.modes.clone();
        for f in &self.diagnosis.findings {
            modes.extend(offline_check(f.id));
            modes.extend(f.evidence.detector.and_then(offline_check));
        }
        modes
    }

    /// A view read afresh; `under_lock` when this process holds the global lock, so its
    /// own hold hides no journal (M4-D45).
    fn fresh_view(&self, under_lock: bool) -> DoctorView {
        DoctorView::load_at(
            self.paths.clone(),
            ViewOptions {
                caller_holds_lock: under_lock,
                online: self.req.checks.online,
                ..ViewOptions::default()
            },
        )
    }

    /// The findings the report ends with: the planned modes' as last detected, and the
    /// others' as diagnosed, in report order.
    fn final_findings(&self) -> Vec<Finding> {
        let Some(fresh) = &self.fresh else {
            return self.diagnosis.findings.clone();
        };
        // What `fresh` covers replaces what the diagnosis found of it.
        let stale = |f: &&Finding| match &self.rechecked {
            Some(modes) => modes.contains(f.id) || about_check_of(f, modes),
            None => self.modes.contains(f.id),
        };
        let mut findings: Vec<Finding> = self
            .diagnosis
            .findings
            .iter()
            .filter(|f| !stale(f))
            .cloned()
            .chain(fresh.iter().cloned())
            .collect();
        findings
            .sort_by(|a, b| (a.severity, a.id, &a.instance).cmp(&(b.severity, b.id, &b.instance)));
        findings
    }

    /// One `journal_pending` warning per journal recovery would settle that the plan does
    /// not delegate.
    fn undelegated(&self, plan: &FixPlan) -> Vec<Warning> {
        let view = self.fresh_view(false);
        plan.journals
            .iter()
            .filter(|j| matches!(j.verdict, JournalVerdict::Recoverable { .. }))
            .filter(|j| !plan.recoveries.iter().any(|r| r.tx == j.tx))
            .map(|j| {
                let mut argv = vec![
                    "omaddons".to_owned(),
                    "doctor".to_owned(),
                    "explain".to_owned(),
                    ids::JOURNAL_PENDING.to_owned(),
                ];
                let target = self
                    .diagnosis
                    .findings
                    .iter()
                    .find(|f| f.id == ids::JOURNAL_PENDING && f.evidence.tx == Some(j.tx))
                    .and_then(|f| f.target.as_ref().map(|t| t.key))
                    .map(|key| view.target_ref(key).id);
                if let Some(target) = target {
                    argv.extend(["-t".to_owned(), target]);
                }
                Warning::new(
                    WarningCode::JournalPending,
                    format!(
                        "interrupted change {} waits to be finished; this repair does not touch what it holds",
                        j.tx
                    ),
                )
                .with_fix(Suggestion::command(argv))
            })
            .collect()
    }

    /// `undo.sh`, for a committed repair: `None` when it could not be written.
    fn write_undo_script(&mut self, tx: TxId) -> std::result::Result<(), Error> {
        let binary = crate::omarchy::install::current_binary()?;
        let ctx = crate::omarchy::render::Context::of(self.paths);
        let Some(runtime) = self.runtime.as_mut() else {
            return Err(Error::internal("the run's record was already written"));
        };
        let script = undo_script(&binary, &ctx, runtime.id(), tx);
        runtime
            .write_file(UNDO_SCRIPT, &script, 0o600)
            .map(|_| ())
            .map_err(|failure| failure.error)
    }

    fn outcome(&mut self, terminal: Terminal) -> FixOutcome {
        let dry_run = self.req.dry_run;
        let target = self.req.selection.target.as_deref();
        let findings = self.final_findings();
        let (state, mut exit) = state_of(&terminal, !findings.is_empty());
        let mut warnings = std::mem::take(&mut self.warnings);
        let (plan, delegated, pruned, commit, mut error) = match terminal {
            Terminal::Planned(plan) => (Some(plan), Vec::new(), Vec::new(), None, None),
            Terminal::Stopped {
                stop,
                delegated,
                pruned,
            } => (None, delegated, pruned, None, Some(stop.into_error(target))),
            Terminal::Done {
                plan,
                delegated,
                pruned,
                commit,
                ..
            } => (Some(plan), delegated, pruned, commit, None),
        };
        if let Some(pending) = error
            .as_ref()
            .and_then(|e| e.details.get("journal_pending"))
            .and_then(|tx| tx.as_str())
        {
            warnings.push(Warning::new(
                WarningCode::JournalPending,
                format!(
                    "repair transaction {pending} could not finish its rollback; the next change finishes it"
                ),
            ));
        }
        if let Some(plan) = &plan {
            warnings.extend(self.undelegated(plan));
        }
        let run_dir = self.run.dir.clone();
        let committed = match &commit {
            Some(RepairOutcome::Committed { tx, .. }) => Some(*tx),
            _ => None,
        };
        if let Some(tx) = committed
            && let Err(failed) = self.write_undo_script(tx)
        {
            exit = exit_for(failed.code);
            error = Some(failed);
        }
        let steps = plan
            .as_ref()
            .map(|plan| plan.lowered.steps.iter().map(|s| s.step.clone()).collect())
            .unwrap_or_else(Vec::new);
        let (actions, taken, failed, bytes_backed_up, restored) = match &commit {
            Some(RepairOutcome::Committed {
                primitives,
                bytes_backed_up,
                ..
            }) => (
                steps
                    .into_iter()
                    .map(|step| ActionDone { step, ok: true })
                    .collect(),
                *primitives,
                0,
                *bytes_backed_up,
                0,
            ),
            Some(RepairOutcome::RolledBack {
                failed_seq,
                backups_restored,
                ..
            }) => (
                steps
                    .into_iter()
                    .filter(|step| failed_seq.is_none_or(|seq| step.seq <= seq))
                    .map(|step| ActionDone { step, ok: false })
                    .collect(),
                0,
                1,
                0,
                *backups_restored,
            ),
            None => (Vec::new(), 0, 0, 0, 0),
        };
        let precondition = plan
            .as_ref()
            .filter(|_| dry_run)
            .map(|plan| of_doctor_fix(plan).token());
        let mut commands: Vec<CommandHint> = Vec::new();
        match (&plan, &precondition) {
            (Some(plan), Some(token)) if !plan.is_empty() => {
                commands.push(CommandHint::new(
                    Action::DoctorFix,
                    bound_argv(&self.req.argv, token),
                ));
            }
            _ if !dry_run => {
                commands.extend(
                    report::explain_argvs(&findings)
                        .into_iter()
                        .map(|argv| CommandHint::new(Action::DoctorExplain, argv)),
                );
            }
            _ => {}
        }
        let run_id = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.id().clone())
            .unwrap_or_else(|| DoctorRunId::new(crate::clock::now(), ulid::Ulid::nil()));
        let undo_argv = committed
            .filter(|_| crate::app::command_entry("doctor undo").is_some())
            .map(|_| {
                ["omaddons", "doctor", "undo", run_id.as_str()]
                    .map(str::to_owned)
                    .to_vec()
            });
        if let Some(argv) = &undo_argv {
            commands.push(CommandHint::new(Action::Undo, argv.clone()));
        }
        let duration_ms = u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let report = FixReport {
            doctor_contract_version: DOCTOR_CONTRACT_VERSION,
            run_id,
            run_dir: run_dir.display().to_string(),
            started_at: self.diagnosis.started_at,
            finished_at: crate::clock::now(),
            state,
            dry_run,
            tx: self.started,
            summary: FixSummary {
                findings: report::summary(&findings, &self.diagnosis.skipped, duration_ms),
                findings_before: count(self.diagnosis.findings.len()),
                findings_after: (!dry_run).then(|| count(findings.len())),
                actions_taken: taken,
                actions_failed: failed,
                bytes_to_back_up: plan.as_ref().map_or(0, |p| p.bytes_to_back_up),
                bytes_backed_up,
                backups_restored: restored,
            },
            findings,
            skipped: self.diagnosis.skipped.clone(),
            actions: (!dry_run).then_some(actions),
            delegated: (!dry_run).then_some(delegated),
            prunes: (!dry_run).then_some(pruned),
            actions_planned: plan
                .as_ref()
                .filter(|_| dry_run)
                .map(FixPlan::actions_planned),
            delegated_planned: plan
                .as_ref()
                .filter(|_| dry_run)
                .map(FixPlan::recoveries_planned),
            prunes_planned: plan
                .as_ref()
                .filter(|_| dry_run)
                .map(FixPlan::prunes_planned),
            precondition,
            actions_jsonl_path: self
                .started
                .map(|_| run_dir.join(ACTIONS_LOG).display().to_string()),
            backups_dir: committed.map(|_| run_dir.join("backups").display().to_string()),
            undo_command: undo_argv.as_ref().map(shell_words::join),
            undo_argv,
            next_steps: commands.iter().map(|c| c.command().to_owned()).collect(),
        };
        FixOutcome {
            report,
            exit,
            error,
            warnings,
            commands,
            notes: std::mem::take(&mut self.notes),
            suspended: plan.map(|plan| plan.suspended).unwrap_or_default(),
        }
    }
}

impl FixEffects for RealEffects<'_> {
    type Lock = GlobalLockGuard;
    type Out = std::result::Result<FixOutcome, DoctorFailure>;

    fn plan(&mut self) -> std::result::Result<FixPlan, Stop> {
        let view = self.fresh_view(false);
        let plan = self
            .plan_on(&view, &self.diagnosis.findings)
            .map_err(Stop::Plan)?;
        self.modes = plan.modes();
        Ok(plan)
    }

    fn planned_failpoint(&mut self) -> std::io::Result<()> {
        fsx::failpoint::hit("doctor.fix.planned")
    }

    fn acquire(&mut self) -> Result<GlobalLockGuard> {
        GlobalLock::acquire(self.paths, "doctor fix")
    }

    fn fresh_plan(&mut self, _: &GlobalLockGuard) -> std::result::Result<FixPlan, Stop> {
        let view = self.fresh_view(true);
        // The clients the delegated recoveries may write SavedVariables in: this view's,
        // never a second discovery.
        self.fresh_targets = clients_of(&view).1;
        let findings = if self.modes.is_empty() {
            Vec::new()
        } else {
            self.detect_again(&view).map_err(Stop::Error)?
        };
        let plan = self.plan_on(&view, &findings).map_err(Stop::Plan);
        self.fresh = Some(findings);
        plan
    }

    fn compare(&mut self, plan: &FixPlan) -> Result<()> {
        let preview = precondition::preview_argv(&self.req.argv);
        precondition::check(self.req.if_match.as_deref(), &of_doctor_fix(plan), &preview)
    }

    fn delegate(
        &mut self,
        lock: &GlobalLockGuard,
        disclosed: &[PlannedRecovery],
    ) -> Result<Vec<DelegatedOutcome>> {
        let report = self
            .delegate
            .recover(lock, disclosed, &self.fresh_targets)?;
        self.warnings.extend(report.warnings);
        Ok(report.delegated)
    }

    fn retain(&mut self, lock: &GlobalLockGuard, prunes: &[PlannedPrune]) -> Vec<PruneDone> {
        self.delegate.retain(lock, prunes)
    }

    fn commit(&mut self, lock: &GlobalLockGuard, plan: &FixPlan) -> Result<RepairOutcome> {
        let Some(rtx) = self.rtx.take() else {
            return Err(Error::internal(
                "a doctor fix committed without its repair transaction",
            ));
        };
        self.started = Some(rtx.tx());
        // The repair writes through a table fenced now, under the lock and after the
        // delegated recoveries (E14): a stuck repair refuses what overlaps what it
        // protects. The runtime's own table, which writes the run directory, is not.
        let fenced = self
            .scopes
            .clone()
            .with_fences(fence::load(self.paths, self.scopes.client_dirs()));
        let ctx = CommitCtx {
            lock,
            scopes: &fenced,
            run: &self.run,
            mount_id: &fsx::mount_id,
        };
        let meta = RepairMeta {
            run: RunId::new(),
            actor: self.req.actor,
            reason: self.req.reason.clone(),
            argv: self.req.argv.clone(),
            command: "doctor fix",
            undo: None,
        };
        let (outcome, cleanup) = repair::commit(&ctx, rtx, &plan.lowered, &meta)?;
        self.cleanup = Some(cleanup);
        Ok(outcome)
    }

    fn release(&mut self, lock: GlobalLockGuard) {
        let held = lock.held_for();
        drop(lock);
        self.notes.push(format!(
            "lock released: tx {}, lock_held_ms {}",
            self.tx,
            held.as_millis()
        ));
        if let Some(cleanup) = self.cleanup.take() {
            self.warnings.extend(cleanup.finish());
        }
    }

    fn verify(&mut self, plan: &FixPlan) -> Verification {
        let view = self.fresh_view(false);
        let rechecked = self.rechecked_modes();
        // A check that cannot run now verifies nothing: every planned instance counts as
        // remaining.
        let found = self.detect_modes(&view, &rechecked, true).ok();
        let planned: BTreeSet<&str> = plan.fixes.iter().map(|f| f.instance.as_str()).collect();
        let remaining = match &found {
            Some(findings) => findings
                .iter()
                .filter(|f| planned.contains(f.instance.as_str()))
                .count(),
            None => planned.len(),
        };
        if let Some(findings) = found {
            self.fresh = Some(findings);
            self.rechecked = Some(rechecked);
        }
        Verification {
            remaining: u32::try_from(remaining).unwrap_or(u32::MAX),
        }
    }

    fn finish(&mut self, terminal: Terminal) -> Self::Out {
        self.abandon();
        let mut outcome = self.outcome(terminal);
        let mut harden: Vec<(&str, u32)> = Vec::new();
        if self.run.dir.join(ACTIONS_LOG).is_file() {
            harden.push((ACTIONS_LOG, 0o400));
        }
        if self.run.dir.join(UNDO_SCRIPT).is_file() {
            harden.push((UNDO_SCRIPT, 0o500));
        }
        let record = record_of(self.req, &outcome)?;
        let Some(runtime) = self.runtime.take() else {
            return Ok(outcome);
        };
        if let Err(failure) =
            runtime.finish_with(&record, outcome.report.state.moves_latest(), &harden)
        {
            outcome.exit = failure.exit;
            outcome.error = Some(failure.error);
        }
        Ok(outcome)
    }
}

/// `argv` bound to `token`: the same command without `--dry-run` and any earlier
/// `--if-match`, then `--if-match <token>`, without presentation flags.
fn bound_argv(argv: &[String], token: &str) -> Vec<String> {
    let without = crate::cli::without_flag(argv, "--if-match");
    let mut bound: Vec<String> = without.into_iter().filter(|a| a != "--dry-run").collect();
    bound.extend(["--if-match".to_owned(), token.to_owned()]);
    crate::cli::fix_argv(&bound)
}

/// The run's `report.json`: the envelope's record form, the run's warnings first, then
/// the outcome's not among them.
fn record_of(req: &FixRequest, outcome: &FixOutcome) -> Result<Vec<u8>> {
    let (mut warnings, _) = req.notes.snapshot();
    let found = warnings.len();
    for warning in &outcome.warnings {
        if !warnings[..found].contains(warning) {
            warnings.push(warning.clone());
        }
    }
    let meta = Meta {
        elapsed_ms: 0,
        offline: req.offline,
    };
    let envelope = DoctorEnvelope::report(
        "doctor fix",
        &outcome.report,
        outcome.exit,
        outcome.error.as_ref(),
        &warnings,
        &outcome.commands,
        meta,
    )
    .record(RunKind::Fix);
    serde_json::to_vec(&envelope)
        .map_err(|e| Error::internal(format!("the doctor's record could not be serialized: {e}")))
}

/// The clients a repair may touch: the registered clients the view discovered whose
/// marker names their key; and every discovered client, for the process table.
fn clients_of(view: &DoctorView) -> (Vec<(PathBuf, TargetKey)>, Vec<Target>) {
    let Some(world) = view.world() else {
        return (Vec::new(), Vec::new());
    };
    let targets: Vec<Target> = world
        .resolved
        .targets
        .iter()
        .map(|d| d.target.clone())
        .collect();
    let clients = targets
        .iter()
        .filter_map(|t| {
            let key = t.key?;
            (crate::discover::registry::read_marker(&t.client_dir) == Some(key))
                .then(|| (t.client_dir.clone(), key))
        })
        .collect();
    (clients, targets)
}

// ---------------------------------------------------------------------------------------
// The commands' entry points.

/// `doctor fix`: diagnose the selection in a run of its own, then plan, preview or repair
/// as [`fix_flow`] orders it, and record the run.
pub fn run_fix(
    req: &FixRequest,
    delegate: &dyn RecoveryDelegate,
) -> std::result::Result<FixOutcome, DoctorFailure> {
    run_fix_at(&Paths::from_env()?, req, delegate)
}

/// [`run_fix`] on the machine under `paths`.
pub fn run_fix_at(
    paths: &Paths,
    req: &FixRequest,
    delegate: &dyn RecoveryDelegate,
) -> std::result::Result<FixOutcome, DoctorFailure> {
    let clock = Instant::now();
    let view = DoctorView::load_at(
        paths.clone(),
        ViewOptions {
            online: req.checks.online,
            ..ViewOptions::default()
        },
    );
    // -t resolves before the run starts: a client that is not there creates nothing.
    req.selection.scope(&view)?;
    let (clients, targets) = clients_of(&view);
    let dirs: Vec<PathBuf> = clients.iter().map(|(dir, _)| dir.clone()).collect();
    let scopes = ScopeTable::new(paths, &dirs, None)?;
    let runtime = Runtime::start(paths, &scopes, crate::clock::now())?;
    let mut diagnosis = runtime.diagnose(
        &req.checks.rows,
        &view,
        &req.selection,
        &req.checks.tiers,
        req.checks.budgets,
    )?;
    diagnosis
        .skipped
        .extend(req.checks.online_skipped.iter().cloned());
    diagnosis
        .skipped
        .sort_by(|a, b| (a.id, &a.target).cmp(&(b.id, &b.target)));
    let running = detect_running(&targets);
    let rtx = if req.dry_run {
        None
    } else {
        Some(RepairTx::begin(paths)?)
    };
    let tx = rtx.as_ref().map_or_else(TxId::new, RepairTx::tx);
    let run = RepairRun {
        dir: runtime.dir(),
        id: runtime.id().as_str().to_owned(),
    };
    let mut warnings = view.warnings();
    warnings.extend(running.warning());
    let mut fx = RealEffects {
        req,
        paths,
        scopes: &scopes,
        delegate,
        runtime: Some(runtime),
        run,
        diagnosis,
        clients,
        skill_dirs: vec![paths.skill_dir()],
        running,
        fresh_targets: Vec::new(),
        rtx,
        tx,
        started: None,
        modes: BTreeSet::new(),
        fresh: None,
        rechecked: None,
        cleanup: None,
        warnings,
        notes: Vec::new(),
        clock,
    };
    fix_flow(&mut fx, req.dry_run, req.if_match.is_some())
}

/// `doctor diff`: what a default `doctor fix --dry-run` would change, or what changed
/// since a run. No run directory, no lock, no request, no write. Its token is the dry
/// run's on an unchanged home.
pub fn run_diff(req: &DiffRequest) -> std::result::Result<DiffOutcome, DoctorFailure> {
    run_diff_at(&Paths::from_env()?, req)
}

/// [`run_diff`] on the machine under `paths`.
pub fn run_diff_at(
    paths: &Paths,
    req: &DiffRequest,
) -> std::result::Result<DiffOutcome, DoctorFailure> {
    let view = DoctorView::load_at(paths.clone(), ViewOptions::default());
    req.selection.scope(&view)?;
    let checks = &req.checks;
    let diagnosis = runtime::diagnose(
        &checks.rows,
        &view,
        &req.selection,
        &checks.tiers,
        checks.budgets,
    )?;
    let exit = RunState::of(&diagnosis.findings).exit();
    let mut warnings = view.warnings();
    if let Some((run, old)) = &req.since {
        return Ok(DiffOutcome {
            report: DiffReport {
                actions_planned: None,
                delegated_planned: None,
                prunes_planned: None,
                bytes_to_back_up: None,
                precondition: None,
                since: Some(report::since(run.clone(), old, &diagnosis.findings)),
            },
            exit,
            warnings,
            commands: Vec::new(),
        });
    }
    let (clients, targets) = clients_of(&view);
    let dirs: Vec<PathBuf> = clients.iter().map(|(dir, _)| dir.clone()).collect();
    let scopes = ScopeTable::new(paths, &dirs, None)?;
    let running = detect_running(&targets);
    warnings.extend(running.warning());
    // No run: its names are the placeholders a token never holds anyway.
    let run = RepairRun {
        dir: paths.doctor_runs_dir().join("<run>"),
        id: "<run>".to_owned(),
    };
    let skill_dirs = [paths.skill_dir()];
    let roots = RepairRoots {
        paths,
        tx: TxId::new(),
        run: &run,
        clients: &clients,
        skill_dirs: &skill_dirs,
        scopes: &scopes,
        mount_id: &fsx::mount_id,
    };
    let opts = FixOptions {
        only: &req.selection.only,
        force: false,
        roots: &roots,
        running: &running,
    };
    let plan = plan_fix_in(&checks.rows, &view, &diagnosis.findings, &opts).map_err(|refusal| {
        DoctorFailure::from(refusal.into_error(req.selection.target.as_deref()))
    })?;
    let token = of_doctor_fix(&plan).token();
    let mut commands = Vec::new();
    if !plan.is_empty() {
        let mut argv: Vec<String> = ["omaddons", "doctor", "fix", "--if-match", &token]
            .map(str::to_owned)
            .to_vec();
        if let Some(target) = &req.selection.target {
            argv.extend(["-t".to_owned(), target.clone()]);
        }
        commands.push(CommandHint::new(Action::DoctorFix, argv));
    }
    Ok(DiffOutcome {
        report: DiffReport {
            actions_planned: Some(plan.actions_planned()),
            delegated_planned: Some(plan.recoveries_planned()),
            prunes_planned: Some(plan.prunes_planned()),
            bytes_to_back_up: Some(plan.bytes_to_back_up),
            precondition: Some(token),
            since: None,
        },
        exit,
        warnings,
        commands,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;
    use crate::doctor::budget::Budget;
    use crate::doctor::delegate::NoDelegate;
    use crate::doctor::finding::{
        Confidence, Detected, Evidence, Observation, Severity, Subject, Subsystem,
    };
    use crate::doctor::fix::PlannedRecovery;
    use crate::doctor::registry::{Cardinality, FixerSpec, Refusal, ScopeName, Tier};
    use crate::doctor::runtime::{Budgets, Latest};
    use crate::engine::journal::{Direction, JournalFamily, Phase};
    use crate::engine::repair_action::{Fix, FixKind, Mode};
    use crate::engine::resource::ResourceSet;
    use crate::error::PlanStaleSource;

    // -----------------------------------------------------------------------------------
    // The order, on a recording fake.

    const ORDER: [&str; 10] = [
        "plan",
        "planned_failpoint",
        "acquire",
        "fresh_plan",
        "compare",
        "delegate",
        "commit",
        "release",
        "verify",
        "finish",
    ];

    fn recovery() -> PlannedRecovery {
        PlannedRecovery {
            fm: ids::JOURNAL_PENDING,
            tx: "01JB8Q2VA00000000000000009".parse().unwrap(),
            family: JournalFamily::Folder,
            phase: Phase::Prepared,
            repair_phase: None,
            direction: Direction::Back,
            protected: ResourceSet::new(),
            disclosed: crate::engine::repair_action::Delegation {
                tx: "01JB8Q2VA00000000000000009".parse().unwrap(),
                family: JournalFamily::Folder,
                phase: crate::engine::repair_action::JournalPhase::Prepared,
                direction: crate::engine::repair_action::Direction::RollBack,
            },
        }
    }

    fn plan(steps: u32, recoveries: bool) -> FixPlan {
        FixPlan::scripted(
            steps,
            if recoveries {
                vec![recovery()]
            } else {
                Vec::new()
            },
        )
    }

    fn committed() -> RepairOutcome {
        RepairOutcome::Committed {
            tx: TxId::new(),
            primitives: 1,
            bytes_backed_up: 0,
            backups: Vec::new(),
        }
    }

    fn delegated(outcome: RecoveryOutcome) -> Vec<DelegatedOutcome> {
        vec![DelegatedOutcome {
            fm: ids::JOURNAL_PENDING,
            tx: recovery().tx,
            family: recovery().family,
            outcome,
            reason: None,
        }]
    }

    fn stale() -> Error {
        Error::plan_stale(
            PlanStaleSource::Apply,
            &[crate::error::StalePart::Observed],
            "the world moved since the preview",
            Suggestion::text("preview it again"),
        )
    }

    /// What each effect answers; `None` takes its default (one step, no recovery, every
    /// call succeeding).
    #[derive(Default)]
    struct Script {
        plan: Option<std::result::Result<FixPlan, Stop>>,
        acquire: Option<Result<()>>,
        fresh: Option<std::result::Result<FixPlan, Stop>>,
        compare: Option<Result<()>>,
        delegate: Option<Result<Vec<DelegatedOutcome>>>,
        commit: Option<Result<RepairOutcome>>,
        remaining: u32,
    }

    struct Recording {
        script: Script,
        calls: Vec<&'static str>,
        /// How many recoveries each delegation was handed.
        handed: Vec<usize>,
    }

    impl FixEffects for Recording {
        type Lock = ();
        type Out = Terminal;

        fn plan(&mut self) -> std::result::Result<FixPlan, Stop> {
            self.calls.push("plan");
            self.script
                .plan
                .take()
                .unwrap_or_else(|| Ok(plan(1, false)))
        }

        fn planned_failpoint(&mut self) -> std::io::Result<()> {
            self.calls.push("planned_failpoint");
            Ok(())
        }

        fn acquire(&mut self) -> Result<()> {
            self.calls.push("acquire");
            self.script.acquire.take().unwrap_or(Ok(()))
        }

        fn fresh_plan(&mut self, (): &()) -> std::result::Result<FixPlan, Stop> {
            self.calls.push("fresh_plan");
            self.script
                .fresh
                .take()
                .unwrap_or_else(|| Ok(plan(1, false)))
        }

        fn compare(&mut self, _: &FixPlan) -> Result<()> {
            self.calls.push("compare");
            self.script.compare.take().unwrap_or(Ok(()))
        }

        fn delegate(
            &mut self,
            (): &(),
            disclosed: &[PlannedRecovery],
        ) -> Result<Vec<DelegatedOutcome>> {
            self.calls.push("delegate");
            self.handed.push(disclosed.len());
            self.script
                .delegate
                .take()
                .unwrap_or_else(|| Ok(delegated(RecoveryOutcome::RolledBack)))
        }

        fn retain(&mut self, (): &(), prunes: &[PlannedPrune]) -> Vec<PruneDone> {
            self.calls.push("retain");
            prunes
                .iter()
                .map(|p| PruneDone {
                    fm: p.fm,
                    target: p.target.clone(),
                    key: p.key,
                    outcome: PruneOutcome::Pruned,
                    pruned: p.txs.clone(),
                    reason: None,
                })
                .collect()
        }

        fn commit(&mut self, (): &(), _: &FixPlan) -> Result<RepairOutcome> {
            self.calls.push("commit");
            self.script.commit.take().unwrap_or_else(|| Ok(committed()))
        }

        fn release(&mut self, (): ()) {
            self.calls.push("release");
        }

        fn verify(&mut self, _: &FixPlan) -> Verification {
            self.calls.push("verify");
            Verification {
                remaining: self.script.remaining,
            }
        }

        fn finish(&mut self, terminal: Terminal) -> Terminal {
            self.calls.push("finish");
            terminal
        }
    }

    /// `ORDER` without the calls `skip` names, in order.
    fn order_without(skip: &[&str]) -> Vec<&'static str> {
        ORDER.into_iter().filter(|c| !skip.contains(c)).collect()
    }

    /// `plan(steps, recoveries)` that also prunes one client's two snapshots.
    fn pruning(steps: u32, recoveries: bool) -> FixPlan {
        let mut p = plan(steps, recoveries);
        p.prunes = vec![PlannedPrune {
            fm: ids::SNAPSHOTS_PAST_RETENTION,
            target: "era".to_owned(),
            key: "01JB8Q2VA00000000000000002".parse().unwrap(),
            client_dir: PathBuf::from("/fixture/_classic_era_"),
            txs: vec![TxId::new(), TxId::new()],
            limits: crate::engine::retention::RetentionLimits::default(),
        }];
        p
    }

    /// Retention prunes after the recoveries and before the commit, under the lock and
    /// only once the token matched; a plan that only prunes is no empty plan, and a
    /// mismatch or a dry run prunes nothing.
    #[test]
    fn a_plan_that_prunes_retains_after_recovery_and_before_the_commit() {
        let run = |script: Script, dry_run: bool, bound: bool| {
            let mut fake = Recording {
                script,
                calls: Vec::new(),
                handed: Vec::new(),
            };
            let terminal = fix_flow(&mut fake, dry_run, bound);
            (fake.calls, terminal)
        };
        let (calls, terminal) = run(
            Script {
                plan: Some(Ok(pruning(1, true))),
                fresh: Some(Ok(pruning(1, true))),
                ..Script::default()
            },
            false,
            true,
        );
        let at = |name: &str| calls.iter().position(|c| *c == name).unwrap();
        assert!(
            at("delegate") < at("retain") && at("retain") < at("commit"),
            "{calls:?}"
        );
        assert!(
            at("compare") < at("retain") && at("retain") < at("release"),
            "{calls:?}"
        );
        let Terminal::Done { pruned, .. } = &terminal else {
            panic!("{terminal:?}");
        };
        assert_eq!(pruned.len(), 1);
        assert_eq!(state_of(&terminal, false).0, RunState::DoneOk);

        let (calls, terminal) = run(
            Script {
                plan: Some(Ok(pruning(0, false))),
                fresh: Some(Ok(pruning(0, false))),
                ..Script::default()
            },
            false,
            false,
        );
        assert!(
            calls.contains(&"retain") && !calls.contains(&"commit"),
            "{calls:?}"
        );
        assert!(calls.contains(&"verify"), "a prune is verified: {calls:?}");
        assert!(matches!(
            terminal,
            Terminal::Done {
                verification: Some(_),
                ..
            }
        ));

        for (case, script, dry_run) in [
            (
                "mismatch",
                Script {
                    fresh: Some(Ok(pruning(1, false))),
                    compare: Some(Err(stale())),
                    ..Script::default()
                },
                false,
            ),
            (
                "dry run",
                Script {
                    plan: Some(Ok(pruning(1, false))),
                    ..Script::default()
                },
                true,
            ),
        ] {
            let (calls, _) = run(script, dry_run, true);
            assert!(!calls.contains(&"retain"), "{case}: {calls:?}");
        }
    }

    #[test]
    fn a_failed_prune_makes_the_run_partial() {
        let mut p = pruning(0, false);
        let done = PruneDone {
            fm: ids::SNAPSHOTS_PAST_RETENTION,
            target: "era".to_owned(),
            key: p.prunes[0].key,
            outcome: PruneOutcome::Failed,
            pruned: Vec::new(),
            reason: Some("io".to_owned()),
        };
        p.prunes.clear();
        let terminal = Terminal::Done {
            plan: p,
            delegated: Vec::new(),
            pruned: vec![done],
            commit: None,
            verification: Some(Verification { remaining: 0 }),
        };
        assert_eq!(state_of(&terminal, false).0, RunState::DonePartial);
    }

    #[test]
    fn fix_order_on_every_path() {
        use RunState::{DoneFailed, DoneFindings, DoneOk, LockLost, Refusing};
        // (case, script, dry run, bound, findings remain, the calls, state, exit code).
        type Case = (
            &'static str,
            Script,
            bool,
            bool,
            bool,
            Vec<&'static str>,
            RunState,
            u8,
        );
        let force = || PlanRefusal::ForceRequired {
            fm: "fm-concurrency_primitives-test-set-aside",
            target: None,
        };
        let cases: Vec<Case> = vec![
            (
                "fix_order_bound_match",
                Script {
                    plan: Some(Ok(plan(1, true))),
                    fresh: Some(Ok(plan(1, true))),
                    ..Script::default()
                },
                false,
                true,
                false,
                ORDER.to_vec(),
                DoneOk,
                0,
            ),
            (
                "fix_order_bound_mismatch_never_delegates_or_commits",
                Script {
                    fresh: Some(Ok(plan(1, true))),
                    compare: Some(Err(stale())),
                    ..Script::default()
                },
                false,
                true,
                false,
                order_without(&["delegate", "commit", "verify"]),
                Refusing,
                4,
            ),
            (
                "fix_order_unbound_with_recoveries",
                Script {
                    fresh: Some(Ok(plan(1, true))),
                    ..Script::default()
                },
                false,
                false,
                false,
                order_without(&["compare"]),
                DoneOk,
                0,
            ),
            (
                "fix_order_without_recoveries_skips_delegate",
                Script::default(),
                false,
                false,
                false,
                order_without(&["compare", "delegate"]),
                DoneOk,
                0,
            ),
            (
                "fix_order_recoveries_alone_skip_commit",
                Script {
                    fresh: Some(Ok(plan(0, true))),
                    ..Script::default()
                },
                false,
                false,
                false,
                order_without(&["compare", "commit"]),
                DoneOk,
                0,
            ),
            (
                "fix_order_dry_run_never_acquires",
                Script::default(),
                true,
                false,
                true,
                vec!["plan", "planned_failpoint", "finish"],
                DoneFindings,
                1,
            ),
            (
                "fix_order_bound_dry_run_compares_without_lock",
                Script::default(),
                true,
                true,
                false,
                vec!["plan", "planned_failpoint", "compare", "finish"],
                DoneOk,
                0,
            ),
            (
                "fix_order_stale_dry_run_refuses",
                Script {
                    compare: Some(Err(stale())),
                    ..Script::default()
                },
                true,
                true,
                false,
                vec!["plan", "planned_failpoint", "compare", "finish"],
                Refusing,
                4,
            ),
            (
                "fix_order_lock_timeout_is_lock_lost",
                Script {
                    acquire: Some(Err(Error::new(
                        ErrorCode::Locked,
                        "another omaddons process holds the lock",
                        Suggestion::text("run it again"),
                    ))),
                    ..Script::default()
                },
                false,
                true,
                false,
                vec!["plan", "planned_failpoint", "acquire", "finish"],
                LockLost,
                5,
            ),
            (
                "fix_order_refusal_before_lock",
                Script {
                    plan: Some(Err(Stop::Plan(force()))),
                    ..Script::default()
                },
                false,
                true,
                false,
                vec!["plan", "finish"],
                Refusing,
                4,
            ),
            (
                "fix_order_refusal_under_lock_releases_first",
                Script {
                    fresh: Some(Err(Stop::Plan(force()))),
                    ..Script::default()
                },
                false,
                true,
                false,
                vec![
                    "plan",
                    "planned_failpoint",
                    "acquire",
                    "fresh_plan",
                    "release",
                    "finish",
                ],
                Refusing,
                4,
            ),
            (
                "fix_order_commit_failure_rolls_back_and_skips_verify",
                Script {
                    commit: Some(Ok(RepairOutcome::RolledBack {
                        tx: TxId::new(),
                        failed_seq: Some(1),
                        cause: Error::internal("the step failed"),
                        backups_restored: 0,
                    })),
                    ..Script::default()
                },
                false,
                false,
                false,
                order_without(&["compare", "delegate", "verify"]),
                DoneFailed,
                3,
            ),
            (
                "fix_order_unfinished_rollback_is_its_error",
                Script {
                    commit: Some(Err(Error::internal(
                        "a rollback met a step it did not know",
                    ))),
                    ..Script::default()
                },
                false,
                false,
                false,
                order_without(&["compare", "delegate", "verify"]),
                DoneFailed,
                74,
            ),
            (
                "fix_order_delegate_error_skips_commit",
                Script {
                    fresh: Some(Ok(plan(1, true))),
                    delegate: Some(Err(Error::internal("no delegate"))),
                    ..Script::default()
                },
                false,
                false,
                false,
                order_without(&["compare", "commit", "verify"]),
                DoneFailed,
                74,
            ),
            (
                "fix_order_empty_fresh_plan_commits_nothing",
                Script {
                    fresh: Some(Ok(plan(0, false))),
                    ..Script::default()
                },
                false,
                true,
                false,
                vec![
                    "plan",
                    "planned_failpoint",
                    "acquire",
                    "fresh_plan",
                    "compare",
                    "release",
                    "finish",
                ],
                DoneOk,
                0,
            ),
            (
                "fix_order_empty_fresh_plan_with_unplanned_findings",
                Script {
                    fresh: Some(Ok(plan(0, false))),
                    ..Script::default()
                },
                false,
                false,
                true,
                vec![
                    "plan",
                    "planned_failpoint",
                    "acquire",
                    "fresh_plan",
                    "release",
                    "finish",
                ],
                DoneFindings,
                1,
            ),
        ];
        for (case, script, dry_run, bound, findings, want, state, exit) in cases {
            let mut fake = Recording {
                script,
                calls: Vec::new(),
                handed: Vec::new(),
            };
            let terminal = fix_flow(&mut fake, dry_run, bound);
            assert_eq!(
                fake.calls, want,
                "{case}\n  expected {want:?}\n  recorded {:?}",
                fake.calls
            );
            let (got_state, got_exit) = state_of(&terminal, findings);
            assert_eq!(
                (got_state, got_exit.code()),
                (state, exit),
                "{case}: {terminal:?}"
            );
            // The rules the order exists for, whatever the path.
            let at = |name| fake.calls.iter().position(|c| *c == name);
            if let Some(compare) = at("compare") {
                for after in ["delegate", "commit"] {
                    assert!(
                        at(after).is_none_or(|i| i > compare),
                        "{case}: {after} before compare"
                    );
                }
            }
            assert!(
                fake.handed.iter().all(|n| *n == 1),
                "{case}: the delegate is handed the plan's recoveries only: {:?}",
                fake.handed
            );
            assert!(fake.handed.len() <= 1, "{case}: delegated more than once");
            if let Some(verify) = at("verify") {
                assert!(at("release").is_some_and(|r| r < verify), "{case}");
            }
            if dry_run {
                assert_eq!(at("acquire"), None, "{case}: a dry run took the lock");
            }
        }
    }

    /// A committed run's terminal state: partial while a repaired mode is still found or a
    /// recovery is unsettled; otherwise findings left in the report (`left`) end it
    /// `DONE_FINDINGS`, exit 1, and only a report with nothing left is `DONE_OK`.
    #[test]
    fn verification_outcomes() {
        use RunState::{DoneFindings, DoneOk, DonePartial};
        // (case, planned instances still found, the recovery's outcome, findings left,
        // state, exit)
        let cases = [
            ("all_gone", 0, RecoveryOutcome::RolledBack, false, DoneOk, 0),
            (
                "rolled_forward",
                0,
                RecoveryOutcome::RolledForward,
                false,
                DoneOk,
                0,
            ),
            (
                "repaired_with_leftovers",
                0,
                RecoveryOutcome::RolledBack,
                true,
                DoneFindings,
                1,
            ),
            (
                "rolled_forward_with_leftovers",
                0,
                RecoveryOutcome::RolledForward,
                true,
                DoneFindings,
                1,
            ),
            (
                "one_remains",
                1,
                RecoveryOutcome::RolledBack,
                false,
                DonePartial,
                2,
            ),
            (
                "one_remains_with_leftovers",
                1,
                RecoveryOutcome::RolledBack,
                true,
                DonePartial,
                2,
            ),
            (
                "a_recovery_waits",
                0,
                RecoveryOutcome::Waiting,
                false,
                DonePartial,
                2,
            ),
            (
                "a_recovery_waits_with_leftovers",
                0,
                RecoveryOutcome::Waiting,
                true,
                DonePartial,
                2,
            ),
            (
                "a_recovery_failed",
                0,
                RecoveryOutcome::Failed,
                false,
                DonePartial,
                2,
            ),
        ];
        for (case, remaining, outcome, left, state, exit) in cases {
            let terminal = Terminal::Done {
                plan: plan(1, true),
                delegated: delegated(outcome),
                pruned: Vec::new(),
                commit: Some(committed()),
                verification: Some(Verification { remaining }),
            };
            let (got, got_exit) = state_of(&terminal, left);
            assert_eq!((got, got_exit.code()), (state, exit), "{case}");
        }
    }

    // -----------------------------------------------------------------------------------
    // The run directory.

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let home = base.join("home");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        for root in [paths.config_dir(), paths.state_dir(), paths.cache_dir()] {
            std::fs::create_dir_all(root).unwrap();
        }
        (dir, paths)
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn latest_and_hardening_per_terminal_state() {
        use RunState::*;
        for state in [
            DoneOk,
            DoneFindings,
            DonePartial,
            DoneFailed,
            Refusing,
            LockLost,
        ] {
            let (_dir, paths) = home();
            let scopes = ScopeTable::new(&paths, &[], None).unwrap();
            let runtime = Runtime::start(&paths, &scopes, crate::clock::now()).unwrap();
            let run = runtime.dir();
            // What a repair leaves in the run directory.
            std::fs::write(run.join(ACTIONS_LOG), "{}\n").unwrap();
            std::fs::write(run.join(UNDO_SCRIPT), "#!/bin/sh\n").unwrap();
            for folder in ["backups", "quarantine"] {
                std::fs::create_dir(run.join(folder)).unwrap();
            }
            let harden = [(ACTIONS_LOG, 0o400), (UNDO_SCRIPT, 0o500)];
            let finished = runtime
                .finish_with(b"{}", state.moves_latest(), &harden)
                .unwrap();
            let want_latest = if state.moves_latest() {
                Latest::Moved
            } else {
                Latest::Skipped
            };
            assert_eq!(finished.latest, want_latest, "{state:?}");
            assert_eq!(
                paths.doctor_latest().exists(),
                state.moves_latest(),
                "{state:?}: latest"
            );
            let modes: Vec<(&str, u32)> = [
                REPORT_NAME,
                RUNTIME_NAME,
                ACTIONS_LOG,
                UNDO_SCRIPT,
                "backups",
                "quarantine",
            ]
            .into_iter()
            .map(|name| (name, mode_of(&run.join(name))))
            .collect();
            let want = [
                (REPORT_NAME, 0o400),
                (RUNTIME_NAME, 0o400),
                (ACTIONS_LOG, 0o400),
                (UNDO_SCRIPT, 0o500),
                ("backups", 0o755),
                ("quarantine", 0o755),
            ];
            assert_eq!(modes, want, "{state:?}");
            assert_eq!(mode_of(&run), 0o700, "{state:?}: the run directory");
        }
    }

    const REPORT_NAME: &str = crate::doctor::runtime::REPORT_FILE;
    const RUNTIME_NAME: &str = crate::doctor::runtime::RUNTIME_LOG;

    // -----------------------------------------------------------------------------------
    // The real flow.

    const STALE: &str = "fm-state_files-test-stale-file";
    const FIXED: &[u8] = b"fixed\n";

    /// Every `fixme-*.txt` in the state directory not yet holding `FIXED`.
    fn stale_files(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
        let mut found = Vec::new();
        let mut names: Vec<PathBuf> = std::fs::read_dir(view.paths().state_dir())
            .map(|dir| dir.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        names.sort();
        for path in names {
            let named = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("fixme-"));
            if named && std::fs::read(&path).is_ok_and(|bytes| bytes != FIXED) {
                found.push(Observation {
                    subject: Subject::machine().path(&path),
                    title: "a stale file".to_owned(),
                    confidence: Confidence::High,
                    addon: None,
                    folder: None,
                    evidence: Evidence {
                        path: Some(path),
                        ..Evidence::default()
                    },
                });
            }
        }
        Detected::complete(found)
    }

    fn rewrite(_: &DoctorView, f: &Finding) -> std::result::Result<Vec<Fix>, Refusal> {
        Ok(f.evidence
            .path
            .iter()
            .map(|path| Fix::Write {
                path: path.clone(),
                content: FIXED.to_vec(),
                mode: Mode::new(0o644).unwrap(),
            })
            .collect())
    }

    fn stale_row() -> FailureMode {
        FailureMode {
            id: STALE,
            subsystem: Subsystem::StateFiles,
            severity: Severity::P3,
            title: "a stale file",
            description: "A file only these tests make.",
            why: "It exercises doctor fix.",
            tier: Tier::Default,
            online: false,
            cost_ms: 1,
            requires: &[],
            detect: Some(stale_files),
            fix: Some(FixerSpec {
                plan: rewrite,
                writes_to: &[ScopeName::State],
                ops: &[FixKind::Write],
                enabled: true,
                needs_force: false,
                cardinality: Cardinality::Low,
                preconditions: &[],
            }),
            manual: None,
        }
    }

    fn request(dry_run: bool) -> FixRequest {
        FixRequest {
            selection: Selection::all(),
            checks: DiagnosePlan {
                rows: vec![stale_row()],
                tiers: vec![Tier::Default],
                budgets: Budgets::TierDefaults,
                online_skipped: Vec::new(),
                max_severity: None,
                online: false,
            },
            force: false,
            dry_run,
            if_match: None,
            argv: ["omaddons", "doctor", "fix"].map(str::to_owned).to_vec(),
            actor: Actor::Human,
            reason: None,
            offline: true,
            notes: RunNotes::default(),
        }
    }

    fn stale_file(paths: &Paths, n: u32, text: &str) -> PathBuf {
        let path = paths.state_dir().join(format!("fixme-{n}.txt"));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn real_flow_commits_and_verifies() {
        let (_dir, paths) = home();
        let file = stale_file(&paths, 1, "stale\n");
        let outcome = run_fix_at(&paths, &request(false), &NoDelegate).unwrap();
        let report = &outcome.report;
        assert_eq!(
            (report.state, outcome.exit),
            (RunState::DoneOk, Exit::Healthy),
            "{:?}",
            outcome.error
        );
        assert_eq!(std::fs::read(&file).unwrap(), FIXED);
        assert_eq!(report.summary.actions_taken, 1);
        assert_eq!(report.summary.findings_before, 1);
        assert_eq!(report.summary.findings_after, Some(0));
        assert!(report.tx.is_some());
        let run = PathBuf::from(&report.run_dir);
        let backup = run.join("backups/1");
        assert_eq!(std::fs::read(&backup).unwrap(), b"stale\n", "its backup");
        assert_eq!(mode_of(&run.join(UNDO_SCRIPT)), 0o500);
        for name in [REPORT_NAME, RUNTIME_NAME, ACTIONS_LOG] {
            assert_eq!(mode_of(&run.join(name)), 0o400, "{name}");
        }
        assert_ne!(
            mode_of(&run.join("backups")) & 0o200,
            0,
            "backups/ stays writable"
        );
        assert!(paths.doctor_latest().exists(), "latest moved");
        let actions = report.actions.as_ref().unwrap();
        assert!(actions.iter().all(|a| a.ok), "{actions:?}");
        assert!(report.actions_planned.is_none() && report.precondition.is_none());
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run.join(REPORT_NAME)).unwrap()).unwrap();
        assert_eq!(recorded["kind"], "fix");
        assert_eq!(recorded["state"], "DONE_OK");
    }

    /// A run whose plan only delegates (M4-D37) runs the engine's recovery through the real
    /// delegate and commits no repair: `tx`, `undo_command` and `undo_argv` are `null`,
    /// and history holds no line, while `delegated[]` reports what recovery did.
    #[test]
    fn a_run_with_only_delegations_commits_no_repair() {
        use crate::doctor::delegate::{JournalDelegate, RecoveryOutcome};
        use crate::doctor::registry::{self, ids};
        let (_dir, paths) = home();
        std::fs::create_dir_all(paths.journal_dir()).unwrap();
        let tx = TxId::new();
        // No complete line: recovery only removes it, a rollback of nothing.
        std::fs::write(paths.journal(&tx), "").unwrap();
        let mut req = request(false);
        req.checks.rows = vec![*registry::row(ids::JOURNAL_PENDING).unwrap()];
        req.checks.tiers = vec![Tier::Cached];
        let delegate = JournalDelegate {
            paths: &paths,
            actor: Actor::Human,
        };
        let outcome = run_fix_at(&paths, &req, &delegate).unwrap();
        let report = &outcome.report;
        assert_eq!(
            (report.state, outcome.exit),
            (RunState::DoneOk, Exit::Healthy),
            "{:?}",
            outcome.error
        );
        let delegated = report.delegated.as_ref().unwrap();
        assert_eq!(
            delegated
                .iter()
                .map(|d| (d.fm, d.tx, d.outcome, d.reason.clone()))
                .collect::<Vec<_>>(),
            [(ids::JOURNAL_PENDING, tx, RecoveryOutcome::RolledBack, None)],
            "{delegated:?}"
        );
        assert!(!paths.journal(&tx).exists(), "recovered");
        assert!(
            report.tx.is_none(),
            "no repair transaction: {:?}",
            report.tx
        );
        assert!(report.undo_command.is_none() && report.undo_argv.is_none());
        assert_eq!(report.actions.as_deref(), Some(&[][..]));
        assert_eq!(report.summary.findings_after, Some(0));
        let history = std::fs::read_to_string(paths.history_file()).unwrap_or_default();
        assert!(history.is_empty(), "no history line: {history}");
        assert!(
            !PathBuf::from(&report.run_dir).join(UNDO_SCRIPT).exists(),
            "no undo.sh"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn real_flow_rolls_back_on_a_step_error() {
        let (_dir, paths) = home();
        let first = stale_file(&paths, 1, "one\n");
        let second = stale_file(&paths, 2, "two\n");
        let outcome = crate::fsx::with_spec("doctor.repair.step=error@2", || {
            run_fix_at(&paths, &request(false), &NoDelegate)
        })
        .unwrap();
        let report = &outcome.report;
        assert_eq!(
            (report.state, outcome.exit),
            (RunState::DoneFailed, Exit::FixFailedRolledBack)
        );
        assert_eq!(report.summary.backups_restored, 1);
        assert_eq!(std::fs::read(&first).unwrap(), b"one\n", "rolled back");
        assert_eq!(std::fs::read(&second).unwrap(), b"two\n", "never written");
        assert!(!PathBuf::from(&report.run_dir).join(UNDO_SCRIPT).exists());
        assert!(
            !paths.doctor_latest().exists(),
            "a failed fix never moves latest"
        );
    }

    #[test]
    fn a_dry_run_previews_with_placeholders_and_a_token() {
        let (_dir, paths) = home();
        let file = stale_file(&paths, 1, "stale\n");
        let outcome = run_fix_at(&paths, &request(true), &NoDelegate).unwrap();
        let report = &outcome.report;
        assert_eq!(
            (report.state, outcome.exit),
            (RunState::DoneFindings, Exit::FindingsPresent)
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"stale\n", "nothing written");
        assert!(report.tx.is_none() && report.actions.is_none() && report.delegated.is_none());
        assert_eq!(report.summary.findings_after, None);
        let planned = report.actions_planned.as_ref().unwrap();
        assert_eq!(planned.len(), 1);
        let step = serde_json::to_value(&planned[0]).unwrap();
        assert_eq!(step["path"], file.display().to_string());
        assert_eq!(
            step["backup"],
            paths
                .doctor_runs_dir()
                .join("<run>/backups/1")
                .display()
                .to_string(),
            "the run's own names are placeholders: {step}"
        );
        assert_eq!(step["bytes"], FIXED.len());
        let token = report.precondition.clone().unwrap();
        assert!(precondition::is_token(&token));
        let hint = outcome.commands.first().unwrap();
        assert_eq!(hint.action(), Action::DoctorFix);
        assert_eq!(
            hint.argv(),
            ["omaddons", "doctor", "fix", "--if-match", token.as_str()]
        );
        // `doctor diff` plans the same repair to the same token, writing no run.
        let runs = || std::fs::read_dir(paths.doctor_runs_dir()).unwrap().count();
        let before = runs();
        let diff = run_diff_at(
            &paths,
            &DiffRequest {
                selection: Selection::all(),
                checks: request(true).checks,
                since: None,
            },
        )
        .unwrap();
        assert_eq!(diff.report.precondition.as_deref(), Some(token.as_str()));
        assert_eq!(
            diff.report.actions_planned.as_deref(),
            Some(planned.as_slice())
        );
        assert_eq!(runs(), before, "a diff writes no run");
    }

    #[test]
    fn a_bound_run_with_a_stale_token_moves_nothing() {
        let (_dir, paths) = home();
        let file = stale_file(&paths, 1, "stale\n");
        let mut req = request(false);
        req.if_match = Some(format!("pc1:{}", "0".repeat(64)));
        let outcome = run_fix_at(&paths, &req, &NoDelegate).unwrap();
        assert_eq!(
            (outcome.report.state, outcome.exit),
            (RunState::Refusing, Exit::RefusedUnsafe)
        );
        let error = outcome.error.unwrap();
        assert_eq!(error.code, ErrorCode::PlanStale);
        assert_eq!(
            error.fix.argv().unwrap(),
            ["omaddons", "doctor", "fix", "--dry-run"]
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"stale\n", "nothing moved");
        assert!(outcome.report.tx.is_none());
        let locks: Vec<_> = std::fs::read_dir(paths.inflight_dir())
            .map(|entries| entries.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(locks.is_empty(), "no in-flight lock is left: {locks:?}");
    }

    /// A real run with nothing to repair leaves no in-flight lock either.
    #[test]
    fn a_real_run_with_nothing_to_repair_leaves_no_lock() {
        let (_dir, paths) = home();
        let outcome = run_fix_at(&paths, &request(false), &NoDelegate).unwrap();
        assert_eq!(outcome.report.state, RunState::DoneOk);
        let locks: Vec<_> = std::fs::read_dir(paths.inflight_dir())
            .map(|entries| entries.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(locks.is_empty(), "{locks:?}");
    }

    /// `value` with what changes from run to run replaced: the temporary home in every
    /// string, the run's volatile keys, and each file identity (an inode).
    fn scrubbed(value: serde_json::Value, base: &Path) -> serde_json::Value {
        fn walk(value: &mut serde_json::Value, base: &str) {
            match value {
                serde_json::Value::String(text) => *text = text.replace(base, "<base>"),
                serde_json::Value::Array(items) => items.iter_mut().for_each(|v| walk(v, base)),
                serde_json::Value::Object(map) => {
                    for (key, item) in map.iter_mut() {
                        if key == "identity" {
                            *item = serde_json::json!("<identity>");
                        } else {
                            walk(item, base);
                        }
                    }
                }
                _ => {}
            }
        }
        // The run's id and its transaction's are in paths too: replaced everywhere first.
        let mut text = serde_json::to_string(&value).unwrap();
        // A token hashes the temporary home's paths.
        for (key, placeholder) in [
            ("run_id", "<run_id>"),
            ("tx", "<tx>"),
            ("precondition", "<token>"),
        ] {
            if let Some(id) = text_of(&value, key) {
                text = text.replace(&id, placeholder);
            }
        }
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        for key in ["started_at", "finished_at"] {
            value[key] = serde_json::json!(format!("<{key}>"));
        }
        value["summary"]["duration_ms"] = serde_json::json!(0);
        walk(&mut value, &base.display().to_string());
        value
    }

    fn text_of(value: &serde_json::Value, key: &str) -> Option<String> {
        value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
    }

    fn base_of(paths: &Paths) -> PathBuf {
        paths.home().parent().unwrap().to_path_buf()
    }

    #[test]
    fn fix_report_dry_run_snapshot() {
        let (_dir, paths) = home();
        stale_file(&paths, 1, "stale\n");
        let outcome = run_fix_at(&paths, &request(true), &NoDelegate).unwrap();
        let value = serde_json::to_value(&outcome.report).unwrap();
        insta::assert_json_snapshot!("fix_report_dry_run", scrubbed(value, &base_of(&paths)));
    }

    #[test]
    fn fix_report_real_run_snapshot() {
        let (_dir, paths) = home();
        stale_file(&paths, 1, "stale\n");
        let outcome = run_fix_at(&paths, &request(false), &NoDelegate).unwrap();
        let value = serde_json::to_value(&outcome.report).unwrap();
        insta::assert_json_snapshot!("fix_report_real_run", scrubbed(value, &base_of(&paths)));
    }

    #[test]
    fn diff_report_both_modes() {
        let (_dir, paths) = home();
        stale_file(&paths, 1, "stale\n");
        let checks = request(true).checks;
        let plan = run_diff_at(
            &paths,
            &DiffRequest {
                selection: Selection::all(),
                checks: checks.clone(),
                since: None,
            },
        )
        .unwrap();
        let earlier = DoctorRunId::parse("2026-10-30T18-03-40Z__01M8S7XQKZ0000000000000ABC");
        let since = run_diff_at(
            &paths,
            &DiffRequest {
                selection: Selection::all(),
                checks,
                since: earlier.map(|run| (run, Vec::new())),
            },
        )
        .unwrap();
        let both = serde_json::json!({
            "plan": serde_json::to_value(&plan.report).unwrap(),
            "since": serde_json::to_value(&since.report).unwrap(),
        });
        let mut text = serde_json::to_string(&both).unwrap();
        text = text.replace(&base_of(&paths).display().to_string(), "<base>");
        if let Some(token) = &plan.report.precondition {
            text = text.replace(token, "<token>");
        }
        let both: serde_json::Value = serde_json::from_str(&text).unwrap();
        insta::assert_json_snapshot!("diff_report_both_modes", both);
    }

    #[test]
    fn fix_and_diff_reports_have_every_key_in_both_modes() {
        let (_dir, paths) = home();
        stale_file(&paths, 1, "stale\n");
        let keys = |value: serde_json::Value| -> Vec<String> {
            value.as_object().unwrap().keys().cloned().collect()
        };
        let dry = run_fix_at(&paths, &request(true), &NoDelegate).unwrap();
        let real = run_fix_at(&paths, &request(false), &NoDelegate).unwrap();
        let [dry, real] = [dry, real].map(|o| serde_json::to_value(&o.report).unwrap());
        assert_eq!(keys(dry.clone()), keys(real.clone()));
        for key in [
            "actions",
            "delegated",
            "prunes",
            "tx",
            "undo_command",
            "undo_argv",
        ] {
            assert!(dry[key].is_null(), "a dry run's {key} is null");
        }
        for key in [
            "actions_planned",
            "delegated_planned",
            "prunes_planned",
            "precondition",
        ] {
            assert!(real[key].is_null(), "a real run's {key} is null");
        }
        let diffs = [
            DiffReport {
                actions_planned: None,
                delegated_planned: None,
                prunes_planned: None,
                bytes_to_back_up: None,
                precondition: None,
                since: None,
            },
            DiffReport {
                actions_planned: Some(Vec::new()),
                delegated_planned: Some(Vec::new()),
                prunes_planned: Some(Vec::new()),
                bytes_to_back_up: Some(0),
                precondition: Some(format!("pc1:{}", "0".repeat(64))),
                since: None,
            },
        ];
        // `Value` sorts its keys: the sets compare, every key present in both modes.
        let [none, some] = diffs.map(|d| keys(serde_json::to_value(d).unwrap()));
        assert_eq!(none, some);
        assert_eq!(
            none,
            [
                "actions_planned",
                "bytes_to_back_up",
                "delegated_planned",
                "precondition",
                "prunes_planned",
                "since"
            ]
        );
    }
}
