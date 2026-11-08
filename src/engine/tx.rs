//! The transaction driver (vision §5.5 steps 5–9): it holds the global lock for a
//! transaction only for revalidate → journal → swap → commit (D24).
//!
//! Plan (no lock, `plan_tx`'s in-flight lock protects its downloads), then per target,
//! in key order, one transaction with its own id and in-flight lock: stage and clone
//! the planned folders' SavedVariables (no lock); lock; recover leftovers; register the
//! target if new; re-check every planned addon offline against the current record,
//! inventory and running state (a changed generation that now conflicts is
//! `plan_stale`); journal; finish the SavedVariables snapshot (a stat per unchanged
//! file) and snapshot state; swap; write the record (`generation + 1`), the files index
//! and history; mark committed; unlock. Then `status.json`, cleanup of the journal,
//! stage dir, temp downloads and in-flight lock, and the doorbell. An error after
//! `prepared` is recovered in process by the journal: before `swapped` it rolls back,
//! after it rolls forward. One target's failure is a per-item failure; committed targets
//! stay committed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::blobs::BlobCache;
use super::edit::RecordEdit;
use super::inspect::ArchiveLimits;
use super::journal::{
    self, CommitPayload, IndexChanges, Journal, Prepared, Recovered, RecoveryAction, RecoveryEnv,
    recover_all, recover_tx,
};
use super::plan::{
    self, AddonWork, Change, Plan, PlanCtx, PlanFailure, PlanRequest, Planned, RequestedChange,
    TargetPlan, TargetRequest, TargetState, WorkMode, claim, one_addons_dir, push_new, rejudge,
    released_view, target_kind,
};
use super::stage::{Staged, stage};
use super::swap::{
    Seen, SvSkipped, SwapOp, finish_saved_variables, out_of_scope_warnings, predigest,
    prestage_saved_variables, snapshot_state, swap_checked, with_predigests,
};
use super::witness::{ApprovalWitness, Operations, RecheckCost, WitnessScope};
use crate::backup::Trigger;
use crate::backup::auto::{self, AutoResult, AutoRun};
use crate::backup::create::Created;
use crate::backup::inventory::Ring;
use crate::backup::prune::{self, PruneOpts};
use crate::contract::actions::Action;
use crate::discover::registry::{LockedRegistry, same_dir};
use crate::discover::running::{RunningScan, detect_running, running_unknown};
use crate::discover::target::DiscoveredTarget;
use crate::error::{
    Error, ErrorBody, ErrorCode, PlanStaleSource, Result, StalePart, Suggestion, Warning,
    WarningCode,
};
use crate::fsx::{
    self, GlobalLock, GlobalLockGuard, InflightLock, LockUse, PruneReason, ScopeTable,
    SnapshotRoots, WriteSession, Writer, ensure_dir_all, failpoint,
};
use crate::model::{
    Actor, AddonState, Channel, FolderName, MutableFolderName, PlanId, Ref, ReloadRequired, RunId,
    Target, TargetKey, TargetRef, TxId, TxKind,
};
use crate::net::Http;
use crate::output::{Phase, ProgressSink};
use crate::resolve::cache::ReleaseCache;
use crate::resolve::catalog::Catalog;
use crate::rules::RulesTable;
use crate::sources::SourceRegistry;
use crate::state::config::RunningPolicy;
use crate::state::doorbell;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::history::{
    self, BackupNote, FolderOp, HistoryEdit, HistoryEntry, HistoryOp, TxResult,
};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::{TargetRecord, load_record};
use crate::state::status::{StatusFile, TargetMeta, write_monotonic};
use crate::state::store;

/// Every engine failpoint (DEC-7); a test checks it against the source.
pub const FAILPOINTS: &[&str] = &[
    "engine.plan.fetched",
    "engine.plan.downloaded",
    "engine.plan.done",
    "engine.stage.begin",
    "engine.stage.file",
    "engine.stage.synced",
    "engine.snapshot.prestaged",
    "engine.lock.acquired",
    "engine.recovered",
    "engine.registered",
    "engine.revalidated",
    "engine.journal.prepared",
    "engine.addons_dir.created",
    "engine.snapshot.sv",
    "engine.snapshot.state",
    "engine.swap.before_rename",
    "engine.swap.after_exchange",
    "engine.swapped",
    "engine.record.written",
    "engine.history.appended",
    "engine.committed",
    "engine.unlocked",
    "engine.cleanup.done",
    "engine.retention",
    "engine.retention.sweep",
    "engine.recover.step",
    "engine.recover.sv",
    "engine.recover.forward",
    "engine.restore.staged",
    "engine.restore.exchanged",
    "engine.restore.snapshotted",
    "engine.preserve.intent",
    "engine.preserve.moved",
    "engine.preserve.published",
    "engine.autobackup",
];

/// What a mutating command asks the engine for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRequest {
    pub targets: Vec<TxTarget>,
    pub force: bool,
    pub while_running: bool,
    pub dry_run: bool,
    /// `--reason`, recorded verbatim in history.
    pub reason: Option<String>,
    /// The transaction kind, whatever the changes are (`adopt`, `pin`, `sync`).
    pub kind: Option<TxKind>,
    /// The command invocation, recorded on every transaction this request commits.
    pub run: RunId,
    /// `--no-deps`: what the TOCs require is checked and warned about, not installed.
    pub no_deps: bool,
    /// Collect an approval witness per target while planning: a dry run that returns a
    /// token, or a run bound to one. `None` collects none.
    pub witness: Option<WitnessScope>,
}

/// One target and its changes; unregistered targets are registered at commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxTarget {
    pub discovered: DiscoveredTarget,
    pub changes: Vec<RequestedChange>,
    /// What the command means when only records change: install for install, remove
    /// for remove, update for update.
    pub default_kind: TxKind,
    /// Versions to fetch for dependencies resolving to these refs (`sync`'s locks).
    pub dependency_locks: BTreeMap<Ref, String>,
}

/// What the engine reads.
pub struct EngineCtx<'a> {
    pub paths: &'a Paths,
    pub http: &'a dyn Http,
    pub registry: &'a SourceRegistry,
    pub table: &'a RulesTable,
    pub policy: RunningPolicy,
    pub limits: ArchiveLimits,
    /// What retention keeps after each commit.
    pub retention: super::retention::RetentionLimits,
    pub actor: Actor,
    pub argv: &'a [String],
    /// The command, recorded as the lock holder.
    pub command: &'a str,
    /// Running client dirs; `None` detects them live (production).
    pub running: Option<&'a BTreeSet<PathBuf>>,
    /// Every target the command discovered: recovery writes SavedVariables only in a
    /// client among them whose game is not running.
    pub known_targets: &'a [Target],
    /// The command's release metadata, shared by its engine runs; `None` fetches
    /// without a memo.
    pub cache: Option<&'a ReleaseCache<'a>>,
    /// Where the run's progress goes.
    pub progress: &'a ProgressSink,
    /// Where dependencies resolve; `None` resolves none from the catalog.
    pub catalog: Option<&'a Catalog>,
    /// The channel dependencies install from.
    pub default_channel: Channel,
    /// What the command downloaded, shared by its engine runs; `None` shares downloads
    /// within this run only.
    pub downloads: Option<&'a super::plan::DownloadMemo>,
    /// The Tukui list and WoWInterface index dependencies resolve from after the catalog,
    /// loaded once per command; `None`: the catalog only.
    pub indexes: Option<&'a super::indexes::DepIndexes>,
    /// The run's notes, which keep each committed transaction's undo should the run then
    /// panic; `None` keeps nothing.
    pub notes: Option<&'a crate::output::RunNotes>,
    /// `--if-match`: the preview this run is bound to (M3-D46); `None` binds nothing.
    pub binding: Option<&'a dyn Binding>,
    /// Automatic backups before the day's first change (M3-D31); `None` makes none.
    pub auto_backup: Option<crate::backup::auto::AutoPolicy<'a>>,
    /// `apply`'s plan (M4-D25): written into each history line this run commits; `None`
    /// on every other run.
    pub applying: Option<PlanId>,
}

/// A run bound to the preview it was confirmed from (`--if-match`, M3-D46): what it plans
/// must be what the preview showed, or it changes nothing. The command implements it; the
/// engine asks it before staging and again under the lock.
pub trait Binding {
    /// Refuses (`plan_stale`) a transaction run's `plan` that is not the previewed one;
    /// asked after planning, before anything is staged.
    fn plan(&self, plan: &Plan) -> Result<()>;
    /// Refuses an undo whose plan (`planned`, as its dry run reports it) is not the
    /// previewed one; asked before staging and again, planned anew, under the lock.
    fn undo(&self, planned: &super::undo::UndoOutcome) -> Result<()>;
    /// The refusal for the target `target`, whose records a commit changed between
    /// planning and the lock.
    fn moved(&self, target: &str) -> Error;
    /// The refusal for the target `target`, whose files a partial restore puts back
    /// changed between planning and the lock (the game wrote them): by default what
    /// [`Self::moved`] says.
    fn observed(&self, target: &str) -> Error {
        self.moved(target)
    }
    /// Checks the approval witness again under the commit lock, before the re-judge
    /// (M4-D60): `Ok(Some(cost))` when it checked and it holds, `Ok(None)` when this
    /// binding checks no witness. By default nothing is checked.
    fn witness(&self, _at: &UnderLock<'_>) -> Result<Option<RecheckCost>> {
        Ok(None)
    }
    /// Refuses a target whose operations, re-judged under the lock, are not the ones its
    /// run planned (and its first check proved the preview's). By default they agree.
    fn operations(
        &self,
        _at: &UnderLock<'_>,
        _approved: &Operations,
        _rejudged: &Operations,
    ) -> Result<()> {
        Ok(())
    }
    /// The refusal for the target `target`, one of whose renames found its folder changed
    /// after the checks under the lock (`why`), once the transaction rolled back: by
    /// default what [`Self::observed`] says.
    fn operation_refused(&self, target: &str, _why: &str) -> Error {
        self.observed(target)
    }
}

/// One target's commit window as a [`Binding`] sees it under the lock: the engine owns
/// these mechanics, the binding the policy and the refusal.
pub struct UnderLock<'a> {
    /// The target's id, for messages.
    pub id: &'a str,
    /// The target as the plan named it.
    pub planned: &'a TargetRef,
    /// Its key now.
    pub key: TargetKey,
    /// This window registered it, so its marker is new.
    pub registered_here: bool,
    pub client: &'a ClientPaths,
    /// The run's own pre-lock witness of this target, when it collected one.
    pub witness: Option<&'a ApprovalWitness>,
    pub limits: &'a ArchiveLimits,
}

/// Notes `omaddons undo <tx>` for a transaction whose history line is written: it
/// stands from here, whatever happens to the run.
pub(super) fn note_undo(ctx: &EngineCtx<'_>, tx: &TxId) {
    if let Some(notes) = ctx.notes {
        let argv = ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()];
        notes.hint(crate::error::CommandHint::new(Action::Undo, argv));
    }
}

/// One target's result: the `data` core of install, remove and undo.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TxOutcome {
    pub target: String,
    pub key: Option<TargetKey>,
    pub tx: Option<TxId>,
    pub kind: Option<TxKind>,
    pub dry_run: bool,
    pub changes: Vec<Change>,
    pub atomic_swap: bool,
    pub reload_required: ReloadRequired,
    pub warnings: Vec<Warning>,
    /// The transaction creates (under `--dry-run`: would create) the client's
    /// `Interface/AddOns`, which a client makes on its first launch.
    pub creates_addons_dir: bool,
    /// Global-lock hold time, for the `-v` log event.
    #[serde(skip)]
    pub lock_held_ms: Option<u64>,
    /// `-v` notes on addons left unchanged (see [`TargetPlan::notes`]).
    #[serde(skip)]
    pub notes: Vec<String>,
}

/// Everything a run produced.
#[derive(Debug, Clone)]
pub struct TxOutcomes {
    pub plan: Plan,
    pub outcomes: Vec<TxOutcome>,
    /// Plan failures and per-target commit failures.
    pub failures: Vec<PlanFailure>,
    /// `journal_recovered` from recovering earlier transactions.
    pub warnings: Vec<Warning>,
}

/// One release to check on one target ([`check_releases`]).
#[derive(Debug, Clone)]
pub struct CheckJob {
    pub target: Target,
    pub wanted: crate::sources::Wanted,
    pub set: crate::model::ReleaseSet,
    /// Names the primary folder (a catalog name, a source's name).
    pub name_hint: Option<String>,
}

/// What [`check_releases`] found: one answer per job, in order, and what checking
/// learned.
#[derive(Debug, Default)]
pub struct CheckedReleases {
    pub results: Vec<Result<plan::Checked>>,
    pub notes: plan::CheckNotes,
}

/// Checks each job's release on its target as installing it would, without ownership,
/// the running policy, staging or the global lock ([`plan::check_release`]): downloads
/// go through the blob cache under an in-flight lock removed afterwards. Writes nothing
/// but the cache.
pub fn check_releases(jobs: &[CheckJob], ctx: &EngineCtx<'_>) -> Result<CheckedReleases> {
    let mut checked = CheckedReleases::default();
    if jobs.is_empty() {
        return Ok(checked);
    }
    let scopes = ScopeTable::new(ctx.paths, &[], None)?;
    let blobs = BlobCache::new(ctx.paths).with_max_download(ctx.limits.max_download);
    let tx = TxId::new();
    // Guards only the cache scratch of the checks: kept in the cache.
    let inflight = InflightLock::create_scratch(ctx.paths, &tx)?;
    let transient;
    let cache = match ctx.cache {
        Some(cache) => cache,
        None => {
            transient = ReleaseCache::transient(ctx.http, ctx.registry);
            &transient
        }
    };
    let no_catalog = Catalog::default();
    let nothing_runs = BTreeSet::new();
    let run_downloads = super::plan::DownloadMemo::default();
    let plan_ctx = PlanCtx {
        paths: ctx.paths,
        scopes: &scopes,
        http: ctx.http,
        registry: ctx.registry,
        cache,
        progress: ctx.progress,
        blobs: &blobs,
        table: ctx.table,
        tx,
        running: &nothing_runs,
        running_unknown: None,
        known_targets: ctx.known_targets,
        policy: ctx.policy,
        limits: ctx.limits,
        retention: ctx.retention,
        actor: ctx.actor,
        argv: ctx.argv,
        catalog: ctx.catalog.unwrap_or(&no_catalog),
        default_channel: ctx.default_channel,
        downloads: ctx.downloads.unwrap_or(&run_downloads),
        indexes: ctx.indexes,
    };
    for job in jobs {
        checked.results.push(plan::check_release(
            &plan_ctx,
            &job.target,
            &job.wanted,
            &job.set,
            job.name_hint.as_deref(),
            &mut checked.notes,
        ));
    }
    cleanup(ctx.paths, &scopes, inflight, &tx, None, false);
    Ok(checked)
}

/// Runs a mutating request to the end.
pub fn run(req: &TxRequest, ctx: &EngineCtx<'_>) -> Result<TxOutcomes> {
    let client_dirs: Vec<PathBuf> = req
        .targets
        .iter()
        .map(|t| t.discovered.target.client_dir.clone())
        .collect();
    // Fenced (E14): a stuck repair refuses this run's writes that overlap what it
    // protects, its dry run's included.
    let scopes = super::fence::table(ctx.paths, &client_dirs, None)?;
    let blobs = BlobCache::new(ctx.paths).with_max_download(ctx.limits.max_download);
    let plan_tx = TxId::new();
    // Planning writes only cache scratch (downloads, inspections), so its lock lives in
    // the cache and a dry run leaves the state directory alone; each target's
    // transaction takes its own lock in the state directory.
    let plan_inflight = InflightLock::create_scratch(ctx.paths, &plan_tx)?;
    let targets: Vec<Target> = req
        .targets
        .iter()
        .map(|t| t.discovered.target.clone())
        .collect();
    let running = running_now(ctx, &targets);
    let plan_req = PlanRequest {
        targets: req
            .targets
            .iter()
            .map(|t| TargetRequest {
                target: t.discovered.target.clone(),
                changes: t.changes.clone(),
                default_kind: t.default_kind,
                dependency_locks: t.dependency_locks.clone(),
            })
            .collect(),
        force: req.force,
        while_running: req.while_running,
        dry_run: req.dry_run,
        kind: req.kind,
        no_deps: req.no_deps,
        witness: req.witness.clone(),
    };
    let no_catalog = Catalog::default();
    let transient;
    let cache = match ctx.cache {
        Some(cache) => cache,
        None => {
            transient = ReleaseCache::transient(ctx.http, ctx.registry);
            &transient
        }
    };
    let run_downloads = super::plan::DownloadMemo::default();
    let plan_ctx = PlanCtx {
        paths: ctx.paths,
        scopes: &scopes,
        http: ctx.http,
        registry: ctx.registry,
        cache,
        progress: ctx.progress,
        blobs: &blobs,
        table: ctx.table,
        tx: plan_tx,
        running: &running.dirs,
        running_unknown: running.unknown.as_deref(),
        known_targets: ctx.known_targets,
        policy: ctx.policy,
        limits: ctx.limits,
        retention: ctx.retention,
        actor: ctx.actor,
        argv: ctx.argv,
        catalog: ctx.catalog.unwrap_or(&no_catalog),
        default_channel: ctx.default_channel,
        downloads: ctx.downloads.unwrap_or(&run_downloads),
        indexes: ctx.indexes,
    };
    let planned = plan::plan(&plan_req, &plan_ctx).and_then(|plan| match ctx.binding {
        // A bound run, dry or not, stages and reports nothing unless it planned what the
        // preview showed (M4-D19). What its planning downloaded is still true: the
        // command remembers it, so the re-preview finds those archives cached (M4-D20).
        Some(binding) => match binding.plan(&plan) {
            Ok(()) => Ok(plan),
            Err(error) => {
                cache.learn(&plan.facts);
                Err(error)
            }
        },
        None => Ok(plan),
    });
    let plan = match planned {
        Ok(plan) => plan,
        Err(error) => {
            cleanup(ctx.paths, &scopes, plan_inflight, &plan_tx, None, false);
            return Err(error);
        }
    };

    let mut outcomes = Vec::new();
    let mut failures = plan.failures.clone();
    // Said once for the run; every later look under the lock fails closed the same way.
    let mut warnings: Vec<Warning> = running.warning().into_iter().collect();
    if req.dry_run || !plan.has_ops() {
        outcomes = plan
            .targets
            .iter()
            .map(|tp| unchanged(tp, req.dry_run))
            .collect();
        if !req.dry_run {
            warnings.extend(recover_waiting(ctx));
        }
    } else {
        let mut order: Vec<usize> = (0..plan.targets.len()).collect();
        order.sort_by_key(|i| {
            (
                plan.targets[*i].key.is_none(),
                plan.targets[*i].key,
                plan.targets[*i].target.clone(),
            )
        });
        let with_work = plan.targets.iter().filter(|t| !t.work.is_empty()).count() as u64;
        for i in order {
            let tp = &plan.targets[i];
            let Some(tt) = req.targets.get(i) else {
                continue;
            };
            if tp.work.is_empty() {
                outcomes.push(unchanged(tp, false));
                continue;
            }
            let discovered = &tt.discovered;
            // Found by the target it names, never by position.
            let planned = TargetRef::of(tp.key, &tp.target);
            // ubs:ignore — comparing target refs, not secrets
            let witness = plan.witnesses.iter().find(|w| w.target == planned);
            match commit_target(
                req, ctx, &scopes, &plan_req, &blobs, discovered, tp, witness, with_work,
            ) {
                Ok((outcome, recovered)) => {
                    if outcome.tx.is_some() {
                        ctx.progress
                            .event(Phase::Commit, with_work, &outcome.target);
                    }
                    outcomes.push(outcome);
                    warnings.extend(recovered);
                }
                Err(error) => failures.push(PlanFailure {
                    r#ref: None,
                    addon: None,
                    target: tp.target.clone(),
                    error: ErrorBody::from(error.clone()),
                    cause: Some(error),
                }),
            }
        }
    }
    cleanup(ctx.paths, &scopes, plan_inflight, &plan_tx, None, false);
    Ok(TxOutcomes {
        plan,
        outcomes,
        failures,
        warnings,
    })
}

/// [`journal::recover_waiting`] for a run that commits nothing, writing SavedVariables
/// as a commit's recovery would: only in a discovered client that is not running.
pub fn recover_waiting(ctx: &EngineCtx<'_>) -> Vec<Warning> {
    let sv_writable = |dir: &Path| sv_writable(ctx, dir);
    journal::recover_waiting(ctx.paths, ctx.command, ctx.actor, &sv_writable)
}

/// A target's outcome when nothing is committed.
fn unchanged(tp: &TargetPlan, dry_run: bool) -> TxOutcome {
    TxOutcome {
        target: tp.target.clone(),
        key: tp.key,
        tx: None,
        kind: if dry_run { tp.kind } else { None },
        dry_run,
        changes: if dry_run {
            tp.changes.clone()
        } else {
            Vec::new()
        },
        atomic_swap: true,
        reload_required: if dry_run {
            tp.reload_required
        } else {
            ReloadRequired::None
        },
        warnings: tp.warnings.clone(),
        creates_addons_dir: dry_run && tp.creates_addons_dir,
        lock_held_ms: None,
        notes: tp.notes.clone(),
    }
}

/// The client dirs to treat as running now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RunningNow {
    /// Every given target's client dir when the process table could not be read: decisions
    /// fail closed.
    pub dirs: BTreeSet<PathBuf>,
    /// Why the process table could not be read, when it could not.
    pub unknown: Option<String>,
}

impl RunningNow {
    fn from_scan(scan: RunningScan, targets: &[Target]) -> Self {
        match scan.unavailable {
            Some(cause) => Self {
                dirs: targets.iter().map(|t| t.client_dir.clone()).collect(),
                unknown: Some(cause),
            },
            None => Self {
                dirs: scan.clients.into_keys().collect(),
                unknown: None,
            },
        }
    }

    pub fn contains(&self, client_dir: &Path) -> bool {
        self.dirs.contains(client_dir)
    }

    /// `running_unknown`, once per command: the caller that plans adds it.
    pub fn warning(&self) -> Option<Warning> {
        self.unknown.as_deref().map(running_unknown)
    }
}

/// The client dirs whose game runs now, or every target's when that cannot be told. A
/// client sharing a target's AddOns directory sees its folders change too, so it is
/// looked for as well.
pub(super) fn running_now(ctx: &EngineCtx<'_>, targets: &[Target]) -> RunningNow {
    match ctx.running {
        Some(running) => RunningNow {
            dirs: running.clone(),
            unknown: None,
        },
        None => {
            let mut scanned: Vec<Target> = targets.to_vec();
            for target in targets {
                for sibling in crate::discover::shared::siblings(target, ctx.known_targets) {
                    if !scanned.iter().any(|t| t.client_dir == sibling.client_dir) {
                        scanned.push(sibling.clone());
                    }
                }
            }
            RunningNow::from_scan(detect_running(&scanned), &scanned)
        }
    }
}

/// The commit's own `error` when recovering its transaction in process failed too: the
/// journal and the staged folders stay (see [`cleanup`]) for the next command that
/// commits a change, which finishes it. `details` gain `journal_pending` (the
/// transaction) and `recovery_error` (`{code, message}`).
pub(super) fn recovery_failed(mut error: Error, tx: &TxId, recovery: &Error) -> Error {
    error.message = format!(
        "{}; recovering it failed too ({}), so its journal is kept and the next omaddons command that commits a change finishes it",
        error.message, recovery.message
    );
    error.retryable = false;
    // A recovery that stopped leaves the journal in exactly that state: the doctor's
    // explanation of it (M4-D53). One that failed on I/O is retried by the next commit.
    error.fix = if super::journal::is_stop(recovery) {
        recovery.fix.clone()
    } else {
        Suggestion::command(["omaddons", "status"])
    };
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    if let Some(details) = error.details.as_object_mut() {
        details.insert("journal_pending".into(), tx.to_string().into());
        details.insert(
            "recovery_error".into(),
            serde_json::json!({"code": recovery.code, "message": recovery.message}),
        );
    }
    error
}

/// Whether recovery may write the SavedVariables of the client at `client_dir` now
/// (vision §5.5: never while its game runs): only in a client the command discovered,
/// checked live. A client it did not discover might be running, so its SavedVariables
/// wait: a false "running" only defers them, a false "not running" would write them
/// under the game.
pub(super) fn sv_writable(ctx: &EngineCtx<'_>, client_dir: &Path) -> bool {
    ctx.known_targets
        .iter()
        .find(|t| same_dir(&t.client_dir, client_dir))
        .is_some_and(|t| !running_now(ctx, std::slice::from_ref(t)).contains(&t.client_dir))
}

/// `state_too_new` when `target`'s record or files index, or, to register it, the
/// registry, was written by a newer omaddons (M3-D45). Every change calls it before it
/// stages anything, so a newer state is refused before any file of the client changes,
/// not only at the save after the swap (which still refuses, as a backstop).
pub(super) fn refuse_newer_state(paths: &Paths, target: &Target) -> Result<()> {
    match target.key {
        Some(key) => {
            store::refuse_newer::<TargetRecord>(&paths.target_record(&key))?;
            store::refuse_newer::<FilesIndex>(&paths.files_index(&key))
        }
        None => store::refuse_newer::<crate::state::registry::Registry>(&paths.registry_file()),
    }
}

/// Appends a backup's history `line` under `_lock`, unless the history has it already:
/// the backup stands whatever the change it came with does. A history that cannot be
/// read might hold it, so nothing is appended then.
pub(super) fn append_once(
    paths: &Paths,
    scopes: &ScopeTable,
    _lock: &GlobalLockGuard,
    line: &HistoryEntry,
    writer: Writer,
) -> Result<()> {
    let text = history::text(paths)?;
    if super::retention::in_history(&text, &line.tx) {
        return Ok(());
    }
    let mut s = WriteSession::untracked(writer, scopes, "history");
    ensure_dir_all(&mut s, paths.state_dir())?;
    history::append(&mut s, paths, line)
}

/// Appends a backup's `line` under a short lock of its own when the change failed before
/// its window appended it. Best effort: the backup is then listed without a line.
pub(super) fn append_alone(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    line: Option<&HistoryEntry>,
    writer: Writer,
) {
    let Some(line) = line else { return };
    let Ok(guard) = GlobalLock::acquire(ctx.paths, ctx.command) else {
        return;
    };
    let _ = append_once(ctx.paths, scopes, &guard, line, writer);
    drop(guard);
}

/// The pre-change automatic backup of `discovered` (M3-D31), before a change that moves
/// folders or writes SavedVariables (`moves`) stages anything: at most one a local day,
/// and never failing the change. Its history line (of `run`, with `reason`), when one was
/// made, for the commit window, and what it warned.
pub(super) fn pre_change(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    discovered: &DiscoveredTarget,
    moves: bool,
    reason: Option<&String>,
    run: RunId,
) -> (Option<HistoryEntry>, Vec<Warning>) {
    let Some(policy) = ctx.auto_backup.filter(|_| moves) else {
        return (None, Vec::new());
    };
    let target = &discovered.target;
    // Recorded, never refused: SavedVariables change on disk only at logout.
    let running = running_now(ctx, std::slice::from_ref(target)).contains(&target.client_dir);
    let auto = AutoRun {
        paths: ctx.paths,
        scopes,
        target: discovered,
        trigger: Trigger::PreChange,
        mode: policy.mode,
        limits: policy.limits,
        running,
        actor: ctx.actor,
        reason: reason.cloned(),
        command: ctx.command,
        progress: ctx.progress,
        record_skips: false,
    };
    let mut outcome = auto::run(&auto, &|| register_early(ctx, scopes, discovered));
    let (line, warning) = match &outcome.result {
        AutoResult::Created(created) => (
            Some(backup_line(ctx, created, reason, run)),
            retain_backups(
                ctx,
                scopes,
                discovered,
                created,
                policy.limits.backup_auto_keep_days,
            ),
        ),
        AutoResult::Skipped(_) => (None, None),
        AutoResult::Failed(error) => (None, Some(auto::failed(&target.id, error))),
    };
    outcome.warnings.extend(warning);
    (line, outcome.warnings)
}

/// Backup retention after the pre-change backup (M3-D32), as after every backup: outside
/// the lock, the new backup pinned. A failure is only `retention_failed`: the backup and
/// the change stand.
fn retain_backups(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    discovered: &DiscoveredTarget,
    created: &Created,
    keep_days: usize,
) -> Option<Warning> {
    let target = &discovered.target;
    let opts = PruneOpts {
        pins: BTreeSet::from([created.meta.id]),
        ..PruneOpts::default()
    };
    let ring = Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    let client = ClientPaths::new(&target.client_dir);
    let pass = prune::retain(
        ctx.paths,
        scopes,
        &client,
        Some(&created.meta.target),
        keep_days,
        &opts,
        ring,
    );
    let error = pass.err()?;
    Some(
        Warning::new(
            WarningCode::RetentionFailed,
            format!(
                "{}: pruning old backups after the automatic backup failed: {}",
                target.id, error.message
            ),
        )
        .with_fix(Suggestion::command([
            "omaddons",
            "backup",
            "prune",
            "-t",
            target.id.as_str(),
        ])),
    )
}

/// `discovered` registered before its change's commit window, for an automatic backup
/// that must name its key: under a short global lock, recovering first as every change
/// does.
fn register_early(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    discovered: &DiscoveredTarget,
) -> Result<Target> {
    let guard = GlobalLock::acquire(ctx.paths, ctx.command)?;
    let sv_writable = |dir: &Path| sv_writable(ctx, dir);
    let recovery = RecoveryEnv {
        lock: &guard,
        paths: ctx.paths,
        scopes,
        own: discovered.target.key.as_slice(),
        sv_writable: &sv_writable,
    };
    let recovered = recover_all(&recovery)?;
    if let Some(notes) = ctx.notes {
        notes.warn_all(&recovered);
    }
    // Registration writes the registry and the client's marker (E14's gate).
    let marker = ClientPaths::new(&discovered.target.client_dir).marker();
    super::fence::under_lock(
        scopes,
        ctx.paths,
        None,
        &[ctx.paths.registry_file(), marker],
    )?;
    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "registry");
    let mut registry = LockedRegistry::load(&guard, ctx.paths)?;
    let key = registry.ensure_registered(&mut s, discovered)?;
    let mut target = discovered.target.clone();
    if let Some(entry) = registry.registry().entry(&key) {
        target.id.clone_from(&entry.id);
    }
    target.key = Some(key);
    Ok(target)
}

/// An automatic backup's history line (M3-D33), of the change's `run`.
fn backup_line(
    ctx: &EngineCtx<'_>,
    created: &Created,
    reason: Option<&String>,
    run: RunId,
) -> HistoryEntry {
    let meta = &created.meta;
    HistoryEntry {
        schema_version: history::CURRENT,
        tx: created.tx,
        kind: TxKind::Backup,
        at: crate::clock::now(),
        target: Some(meta.target),
        targets: Vec::new(),
        actor: ctx.actor,
        reason: reason.cloned(),
        argv: ctx.argv.to_vec(),
        ops: Vec::new(),
        result: TxResult::Committed,
        reverts: None,
        sv_deferred: false,
        sv_restored: None,
        created_addons_dir: false,
        run: Some(run),
        backup: Some(BackupNote {
            id: meta.id,
            kind: meta.kind,
            files: meta.files,
            bytes: meta.bytes,
        }),
        // Never the plan being applied: a backup is no part of what a plan previews, and
        // `plan_evidence` never counts a backup line.
        plan: None,
        doctor_run_id: None,
        repairs: Vec::new(),
    }
}

/// One target's transaction, from staging to cleanup.
#[allow(clippy::too_many_arguments)]
fn commit_target(
    req: &TxRequest,
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    plan_req: &PlanRequest,
    blobs: &BlobCache,
    discovered: &DiscoveredTarget,
    tp: &TargetPlan,
    witness: Option<&ApprovalWitness>,
    with_work: u64,
) -> Result<(TxOutcome, Vec<Warning>)> {
    let target = &discovered.target;
    refuse_newer_state(ctx.paths, target)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let client = ClientPaths::new(&target.client_dir);
    let moves = tp.work.iter().any(|w| !w.swap().is_empty());
    let (auto_line, auto_warnings) =
        pre_change(ctx, scopes, discovered, moves, req.reason.as_ref(), req.run);
    let staged = match stage_all(&inflight, scopes, target, &tp.work) {
        Ok(staged) => {
            ctx.progress.event(Phase::Stage, with_work, &target.id);
            staged
        }
        Err(error) => {
            cleanup(ctx.paths, scopes, inflight, &tx, Some(&client), false);
            append_alone(ctx, scopes, auto_line.as_ref(), Writer::Mutating);
            return Err(error);
        }
    };
    // The index stamps taken under the lock must postdate every staged mtime by the
    // racy slack.
    staged.wait_out_racy_slack();
    let result = locked(
        req,
        ctx,
        scopes,
        plan_req,
        blobs,
        discovered,
        tp,
        witness,
        &tx,
        &staged,
        auto_line.as_ref(),
    );
    let committed = result
        .as_ref()
        .is_ok_and(|(outcome, _)| outcome.tx.is_some());
    cleanup(ctx.paths, scopes, inflight, &tx, Some(&client), committed);
    // A lock that timed out once is not waited on twice.
    // ubs:ignore — comparing error codes, not secrets
    if !committed && !matches!(&result, Err(e) if e.code == ErrorCode::Locked) {
        append_alone(ctx, scopes, auto_line.as_ref(), Writer::Mutating);
    }
    ring_after_window(ctx, &result);
    let result = result.map(|(mut outcome, recovered)| {
        outcome.warnings.extend(auto_warnings);
        (outcome, recovered)
    });
    let fail = |e: std::io::Error| {
        Error::io(
            ctx.paths.state_dir(),
            &e,
            Suggestion::command(["omaddons", "status"]),
        )
    };
    failpoint::hit("engine.cleanup.done").map_err(fail)?;
    let (mut outcome, recovered) = result?;
    if let (Some(key), Some(_)) = (outcome.key, outcome.tx) {
        outcome
            .warnings
            .extend(retain(ctx, scopes, key, &target.client_dir));
    }
    Ok((outcome, recovered))
}

/// Rings the doorbell once a commit window has run and its lock is released (M3-D4).
/// Every way out of the window may have changed what `status` reports: the target
/// registered, other journals recovered, the commit itself, or its own journal left
/// pending when recovering it failed. Only a lock that timed out (`locked`) leaves all as
/// it was. An error before the lock rings too: a false ring costs the widget one
/// `status --cached`.
pub(super) fn ring_after_window<T>(ctx: &EngineCtx<'_>, result: &Result<T>) {
    // ubs:ignore — comparing error codes, not secrets
    if !matches!(result, Err(error) if error.code == ErrorCode::Locked) {
        doorbell::ring(ctx.paths, ctx.command, ctx.actor);
    }
}

/// Retention after a commit (vision §5.5), best effort: its warnings join the outcome
/// and a failure becomes `retention_failed`, never a failed command. Then, at most once
/// a day, the same for every other client the command discovered
/// ([`super::retention::sweep`]).
pub(super) fn retain(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    key: TargetKey,
    client_dir: &std::path::Path,
) -> Vec<Warning> {
    let mut warnings = retain_own(ctx, scopes, key, client_dir);
    let swept = super::retention::sweep(
        ctx.paths,
        Writer::Mutating,
        ctx.known_targets,
        Some(key),
        &ctx.retention,
        super::retention::RetentionClock::current().system,
    );
    warnings.extend(swept.warnings);
    warnings
}

/// The committed client's own retention.
fn retain_own(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    key: TargetKey,
    client_dir: &std::path::Path,
) -> Vec<Warning> {
    let run = || -> Result<Vec<Warning>> {
        failpoint::hit("engine.retention").map_err(|e| {
            Error::io(
                ctx.paths.state_dir(),
                &e,
                Suggestion::command(["omaddons", "status"]),
            )
        })?;
        let report = super::retention::run(
            ctx.paths,
            scopes,
            key,
            &ClientPaths::new(client_dir),
            &ctx.retention,
            super::retention::RetentionClock::current(),
            &|| super::retention::pinned_blobs(ctx.paths),
        )?;
        Ok(report.warnings)
    };
    run().unwrap_or_else(|error| {
        vec![
            Warning::new(
                crate::error::WarningCode::RetentionFailed,
                format!(
                    "pruning old snapshots after the commit failed: {}",
                    error.message
                ),
            )
            .with_fix(Suggestion::command(["omaddons", "status"])),
        ]
    })
}

/// Stages every addon with an archive into the transaction's stage dir.
fn stage_all(
    inflight: &InflightLock,
    scopes: &ScopeTable,
    target: &Target,
    work: &[AddonWork],
) -> Result<Staged> {
    let mut merged = Staged {
        tx: inflight.tx(),
        stage_dir: ClientPaths::new(&target.client_dir).stage(&inflight.tx()),
        folders: Vec::new(),
    };
    for w in work {
        if let Some(inspected) = w.inspected() {
            let staged = stage(inflight, scopes, target, w.blob(), inspected)?;
            merged.folders.extend(staged.folders);
        }
    }
    Ok(merged)
}

/// What re-judging one target's planned work under the lock leaves to commit.
struct Rejudged {
    works: Vec<AddonWork>,
    changes: Vec<Change>,
    warnings: Vec<Warning>,
    notes: Vec<String>,
    reload_required: ReloadRequired,
}

/// The planned work of `tp` judged again offline, over one fresh look at the target and
/// with running state as it is now: what still commits, handovers settled. `changed`: a
/// commit moved the target's records since planning, so a failure is `plan_stale`.
fn rejudge_target(
    plan_req: &PlanRequest,
    plan_ctx: &PlanCtx<'_>,
    target: &Target,
    tp: &TargetPlan,
    changed: bool,
) -> Result<Rejudged> {
    let mut works = Vec::new();
    let mut changes = Vec::new();
    let mut reload_required = ReloadRequired::None;
    // The addons planning left unchanged keep their warnings and notes; the work is
    // judged again.
    let mut warnings = tp.unchanged_warnings.clone();
    let mut notes = tp.notes.clone();
    // One fresh look at the target, and the planned works replayed over it in order:
    // releases first, then each claim (M2-D6).
    let state = TargetState::now(plan_ctx, target)?;
    let mut view = released_view(&tp.work);
    // The addons whose works still commit: a dependency (or a `required_by` edit) whose
    // every requester was planned and dropped out goes too.
    let planned_addons: BTreeSet<String> = tp
        .work
        .iter()
        .map(|w| TargetRecord::addon_key(&w.addon))
        .collect();
    let mut kept: BTreeSet<String> = BTreeSet::new();
    for planned in &tp.work {
        if requesters_dropped(&requesters_of(planned), &planned_addons, &kept) {
            continue;
        }
        match rejudge(
            plan_req,
            plan_ctx,
            target,
            &state,
            planned,
            tp.generation,
            &view,
        ) {
            Ok(Planned::Change(p)) => {
                let p = *p;
                claim(&mut view, &p.work);
                kept.insert(TargetRecord::addon_key(&p.work.addon));
                reload_required = reload_required.max(p.reload_required);
                push_new(&mut warnings, p.warnings);
                changes.push(p.change);
                works.push(p.work);
            }
            // A pinned no-op is not a refusal: never plan_stale.
            Ok(Planned::Unchanged {
                warnings: dropped,
                notes: noted,
            }) => {
                push_new(&mut warnings, dropped);
                notes.extend(noted);
            }
            // An adoption whose folders changed is stale whatever the generation.
            // ubs:ignore — comparing error codes, not secrets
            Err(error) if error.code == ErrorCode::PlanStale => return Err(error),
            Err(error) if changed => return Err(stale(&error)),
            Err(error) => return Err(error),
        }
    }
    plan::settle_handovers(&mut works, &mut changes, None);
    Ok(Rejudged {
        works,
        changes,
        warnings,
        notes,
        reload_required,
    })
}

/// The `-v` note on what checking a target's approval under the lock cost.
fn recheck_note(target: &str, cost: &RecheckCost) -> String {
    format!(
        "{target}: approval checked under the lock in {} us: folders {}, entries {}, rehashed {}, files {}, bytes {}",
        cost.elapsed_us,
        cost.folders,
        cost.entries_stamped,
        cost.folders_rehashed,
        cost.files_hashed,
        cost.bytes_hashed
    )
}

/// The planned folders' SavedVariables cloned, then the commit window under the global
/// lock.
#[expect(
    clippy::too_many_arguments,
    reason = "the commit window threads the request, its plan and the staged state; a struct would only rename them"
)]
fn locked(
    req: &TxRequest,
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    plan_req: &PlanRequest,
    blobs: &BlobCache,
    discovered: &DiscoveredTarget,
    tp: &TargetPlan,
    witness: Option<&ApprovalWitness>,
    tx: &TxId,
    staged: &Staged,
    auto_line: Option<&HistoryEntry>,
) -> Result<(TxOutcome, Vec<Warning>)> {
    let paths = ctx.paths;
    let fail = |e: std::io::Error| {
        Error::io(
            paths.state_dir(),
            &e,
            Suggestion::command(["omaddons", "status"]),
        )
    };
    // Folder ops only: the bare names, which a pre-M3 build recovers as its own.
    let actions = paths.state_snapshot(tx).join("actions.jsonl");
    let mut s = WriteSession::tx(
        *tx,
        Writer::Mutating,
        scopes,
        SnapshotRoots::for_tx(paths, tx),
        actions,
    );
    // Before the lock (D24): under it, an unchanged SavedVariables file or unindexed
    // folder costs a stat. A copy left by a failure here goes with the transaction's
    // cleanup.
    let client = ClientPaths::new(&discovered.target.client_dir);
    let snapshot = client.snapshots(tx);
    let planned_ops: Vec<SwapOp> = tp.work.iter().flat_map(|w| w.swap().to_vec()).collect();
    let planned_folders: Vec<FolderName> = planned_ops.iter().map(touched_folder).collect();
    let predigests = predigest(&client.addons(), &planned_ops);
    let prestage = prestage_saved_variables(&mut s, &snapshot, &client, &planned_folders)?;
    failpoint::hit("engine.snapshot.prestaged").map_err(fail)?;
    let guard = GlobalLock::acquire(paths, ctx.command)?;
    failpoint::hit("engine.lock.acquired").map_err(fail)?;
    let sv_writable = |dir: &Path| sv_writable(ctx, dir);
    let recovery = RecoveryEnv {
        lock: &guard,
        paths,
        scopes,
        own: discovered.target.key.as_slice(),
        sv_writable: &sv_writable,
    };
    let recovered = recover_all(&recovery)?;
    failpoint::hit("engine.recovered").map_err(fail)?;
    // The automatic backup stands whatever the change does: its line goes first, before
    // the journal, so a roll-forward (which replays only the change's own line) never
    // leaves it out (M3-D33).
    if let Some(line) = auto_line {
        append_once(paths, scopes, &guard, line, Writer::Mutating)?;
    }

    let mut target = discovered.target.clone();
    let key = match target.key {
        Some(key) => key,
        None => {
            let mut s = WriteSession::untracked(Writer::Mutating, scopes, "registry");
            let mut registry = LockedRegistry::load(&guard, paths)?;
            let key = registry.ensure_registered(&mut s, discovered)?;
            target.key = Some(key);
            key
        }
    };
    failpoint::hit("engine.registered").map_err(fail)?;
    // Again under the lock: a newer omaddons may have written since the first look.
    refuse_newer_state(paths, &target)?;
    // A repair that became stuck since the table was built, or one fencing this target
    // or a folder the swap writes, refuses before the journal (E14).
    let addons = client.addons();
    let first_hand: Vec<PathBuf> = planned_folders
        .iter()
        .map(|folder| addons.join(folder.as_str()))
        .collect();
    super::fence::under_lock(scopes, paths, Some(&target), &first_hand)?;

    // Re-check every addon offline, with running state as it is now.
    let current = load_record(paths, &key)?;
    // ubs:ignore — comparing record generations, not secrets
    let changed = current.value.generation != tp.generation;
    // A run bound to a preview commits only onto the records the preview saw.
    if changed && let Some(binding) = ctx.binding {
        return Err(binding.moved(&target.id));
    }
    // The identities each rename will require, taken before the content checks, so those
    // checks validate the very folders the renames move (M4-D60 check 3): a folder
    // replaced after this fails at its rename, bound run or not.
    let seen = Seen::capture(&client.addons(), &planned_ops, staged);
    let planned = TargetRef::of(tp.key, &tp.target);
    let at = UnderLock {
        id: &target.id,
        planned: &planned,
        key,
        registered_here: discovered.target.key.is_none(),
        client: &client,
        witness,
        limits: &ctx.limits,
    };
    // A bound run commits only over what its preview approved (M4-D60): a refusal here
    // has moved nothing.
    let checked = ctx.binding.map(|b| b.witness(&at)).transpose()?.flatten();
    let running = running_now(ctx, std::slice::from_ref(&target));
    // Re-judging is offline: it never fetches, resolves or downloads, so it needs no
    // memo, no catalog and no downloads.
    let offline = ReleaseCache::transient(ctx.http, ctx.registry);
    let no_catalog = Catalog::default();
    let no_downloads = super::plan::DownloadMemo::default();
    let plan_ctx = PlanCtx {
        paths,
        scopes,
        http: ctx.http,
        registry: ctx.registry,
        cache: &offline,
        progress: ctx.progress,
        blobs,
        table: ctx.table,
        tx: *tx,
        running: &running.dirs,
        running_unknown: running.unknown.as_deref(),
        known_targets: ctx.known_targets,
        policy: ctx.policy,
        limits: ctx.limits,
        retention: ctx.retention,
        actor: ctx.actor,
        argv: ctx.argv,
        catalog: &no_catalog,
        default_channel: ctx.default_channel,
        downloads: &no_downloads,
        indexes: None,
    };
    let Rejudged {
        works,
        changes,
        mut warnings,
        mut notes,
        reload_required,
    } = rejudge_target(plan_req, &plan_ctx, &target, tp, changed)?;
    if let Some(binding) = ctx.binding {
        // Exactly the approved operations, or none: a run left with nothing to do while
        // its preview approved something is refused here, not reported unchanged.
        let approved = Operations::of(&tp.work, &tp.changes);
        binding.operations(&at, &approved, &Operations::of(&works, &changes))?;
    }
    if let Some(cost) = checked {
        notes.push(recheck_note(&target.id, &cost));
    }
    failpoint::hit("engine.revalidated").map_err(fail)?;
    let kind = target_kind(&changes, plan_req.kind, tp.default_kind);
    let Some(kind) = kind else {
        // Everything became unnecessary (a concurrent transaction did it).
        return Ok((
            TxOutcome {
                target: target.id.clone(),
                key: Some(key),
                tx: None,
                kind: None,
                dry_run: false,
                changes: Vec::new(),
                atomic_swap: true,
                reload_required: ReloadRequired::None,
                warnings,
                creates_addons_dir: false,
                lock_held_ms: Some(millis(&guard)),
                notes,
            },
            recovered,
        ));
    };

    let now = Timestamp::now();
    let record = next_record(&current.value, &works, tx, now);
    let mut swap_ops: Vec<SwapOp> = works.iter().flat_map(|w| w.swap().to_vec()).collect();
    // Stamped here, under the lock and after the racy wait, so the journal carries the
    // stamps and roll-forward writes exactly these.
    let mut index_changes = index_changes(&swap_ops, staged, now.as_nanosecond());
    records_index_changes(&works, &mut index_changes);
    // A client that has never been launched has no AddOns folder yet. The transaction
    // creates it once `prepared` records that it will; a case variant already there
    // (`interface/addons`) is used instead.
    one_addons_dir(&client)?;
    let addons = client.addons();
    with_predigests(&mut swap_ops, &addons, &predigests);
    let creates_addons_dir = std::fs::symlink_metadata(&addons).is_err()
        && swap_ops
            .iter()
            .any(|op| !matches!(op, SwapOp::Remove { .. }));
    let history = HistoryEntry {
        schema_version: history::CURRENT,
        tx: *tx,
        kind,
        at: now,
        target: Some(key),
        targets: Vec::new(),
        actor: ctx.actor,
        reason: req.reason.clone(),
        argv: ctx.argv.to_vec(),
        ops: history_ops(&works),
        result: TxResult::Committed,
        reverts: None,
        sv_deferred: false,
        sv_restored: None,
        created_addons_dir: creates_addons_dir,
        run: Some(req.run),
        backup: None,
        plan: ctx.applying,
        doctor_run_id: None,
        repairs: Vec::new(),
    };
    let prepared = Prepared {
        schema_version: journal::CURRENT,
        tx: *tx,
        target_key: key,
        client_dir: target.client_dir.clone(),
        kind,
        started_at: now,
        creates_dirs: if creates_addons_dir {
            vec![addons.clone()]
        } else {
            Vec::new()
        },
        commit: CommitPayload {
            record: record.clone(),
            index: index_changes.clone(),
            history: history.clone(),
            stage_dir: staged.stage_dir.clone(),
            sv_restore: None,
        },
        writes_wtf: false,
        staged_stamp: None,
        staged_files: None,
        source_backup: None,
        client: None,
    };
    let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;

    // Kept outside the commit so an in-process roll-forward still reports it.
    let mut sv_skipped = SvSkipped::default();
    let committed = (|| -> Result<(bool, Vec<Warning>)> {
        failpoint::hit("engine.journal.prepared").map_err(fail)?;
        if creates_addons_dir {
            ensure_dir_all(&mut s, &addons)?;
            failpoint::hit("engine.addons_dir.created").map_err(fail)?;
        }
        let touched: Vec<FolderName> = swap_ops.iter().map(touched_folder).collect();
        sv_skipped =
            finish_saved_variables(&mut s, tx, &snapshot, &client, &touched, prestage)?.sv_skipped;
        snapshot_state(&mut s, paths, &paths.state_snapshot(tx), &key)?;
        let report = swap_checked(
            &mut s,
            &mut journal,
            &swap_ops,
            staged,
            &client,
            &snapshot,
            &seen,
        )?;
        ensure_dir_all(&mut s, &paths.targets_dir())?;
        store::save(
            &mut s,
            &paths.target_record(&key),
            &record,
            Some(&current.status),
        )?;
        failpoint::hit("engine.record.written").map_err(fail)?;
        let loaded = store::load::<FilesIndex>(&paths.files_index(&key))?;
        let mut index = loaded.as_ref().map_or_else(
            || FilesIndex {
                key,
                written_at_ns: 0,
                folders: BTreeMap::new(),
            },
            |l| l.value.clone(),
        );
        index_changes.apply(&mut index);
        index.written_at_ns = Timestamp::now().as_nanosecond();
        store::save(
            &mut s,
            &paths.files_index(&key),
            &index,
            loaded.as_ref().map(|l| &l.status),
        )?;
        history::append(&mut s, paths, &history)?;
        note_undo(ctx, tx);
        failpoint::hit("engine.history.appended").map_err(fail)?;
        journal.mark_committed(&mut s)?;
        failpoint::hit("engine.committed").map_err(fail)?;
        Ok((report.atomic_swap, report.warnings))
    })();
    let (atomic_swap, swap_warnings) = match committed {
        Ok(done) => done,
        Err(error) => {
            // Read before recovery prunes it: committed already, the transaction stands.
            let committed_swap = journal::committed_swap(paths, tx);
            match (recover_tx(&recovery, tx), committed_swap) {
                // Swapped already: the payload completed it, so the transaction stands.
                (
                    Ok(Some(Recovered {
                        action: RecoveryAction::RolledForward,
                        atomic_swap,
                        warnings,
                        ..
                    })),
                    _,
                ) => {
                    note_undo(ctx, tx);
                    super::swap::rolled_forward(atomic_swap, warnings)
                }
                (Ok(None), Some(atomic_swap)) => {
                    note_undo(ctx, tx);
                    super::swap::rolled_forward(atomic_swap, Vec::new())
                }
                // Rolled back cleanly: a bound run's rename that found its folder changed
                // is its binding's refusal (M4-D60).
                // ubs:ignore — comparing error codes, not secrets
                (Ok(_), _) if error.code == ErrorCode::PlanStale => {
                    return Err(match ctx.binding {
                        Some(binding) => binding.operation_refused(&target.id, &error.message),
                        None => error,
                    });
                }
                (Ok(_), _) => return Err(error),
                (Err(failed), _) => return Err(recovery_failed(error, tx, &failed)),
            }
        }
    };
    warnings.extend(swap_warnings);
    warnings.extend(out_of_scope_warnings(
        &target.id,
        sv_skipped,
        "they were not snapshotted and undo will not restore them",
    ));
    // What the committed works did to `required_by`, with this transaction's undo.
    push_new(
        &mut warnings,
        plan::bookkeeping_warnings(&current.value, &works, &target.id, Some(*tx)),
    );
    // Files changed by hand were replaced: undoing this transaction brings them back
    // (M2-D17); `-v` names where its snapshot keeps them.
    let snapshots = ClientPaths::new(&target.client_dir).snapshots(tx);
    for work in &works {
        if let Some(changes) = work.local_changes() {
            warnings.push(plan::local_changes_warning(
                &work.addon,
                changes,
                Some(*tx),
                &ctx.retention,
            ));
            for folder in &changes.folders {
                notes.push(format!(
                    "{}'s replaced files are kept in {}",
                    work.addon,
                    snapshots.join("AddOns").join(folder.as_str()).display()
                ));
            }
        }
    }
    // Folders that exist nowhere else now live only in the snapshot, which retention
    // prunes in time; `-v` names where, for a copy to keep.
    let only = plan::only_copies(&current.value, &works);
    warnings.extend(plan::only_copy_warning(
        &target.id,
        &only,
        Some(*tx),
        &ctx.retention,
    ));
    for folder in &only {
        notes.push(format!(
            "the only copy of {} is kept in {}",
            folder.as_str(),
            snapshots.join("AddOns").join(folder.as_str()).display()
        ));
    }
    let held = millis(&guard);
    drop(guard);
    failpoint::hit("engine.unlocked").map_err(fail)?;
    write_status(paths, scopes, &target, &record);
    Ok((
        TxOutcome {
            target: target.id.clone(),
            key: Some(key),
            tx: Some(*tx),
            kind: Some(kind),
            dry_run: false,
            changes,
            atomic_swap,
            reload_required,
            warnings,
            creates_addons_dir,
            lock_held_ms: Some(held),
            notes,
        },
        recovered,
    ))
}

pub(super) fn millis(guard: &GlobalLockGuard) -> u64 {
    u64::try_from(guard.held_for().as_millis()).unwrap_or(u64::MAX)
}

/// `plan_stale` for a re-plan that conflicts, with the underlying code as the cause.
pub(super) fn stale(cause: &Error) -> Error {
    Error::plan_stale(
        PlanStaleSource::Replan,
        &[StalePart::Targets],
        format!(
            "the target changed while this command planned: {}",
            cause.message
        ),
        Suggestion::text("run the command again; it re-plans against the current state"),
    )
    .extend_details(
        serde_json::json!({ "cause": cause.code.wire(), "cause_message": cause.message }),
    )
}

/// Who a work is for, when another work asked for it: a dependency install's
/// requesters, or the requesters a `required_by`-only edit adds. Empty otherwise.
fn requesters_of(work: &AddonWork) -> Vec<String> {
    match &work.mode {
        WorkMode::Folders(f) => f
            .intent
            .as_ref()
            .filter(|i| i.from_resolver())
            .map(|i| i.required_by_add.iter().cloned().collect())
            .unwrap_or_default(),
        WorkMode::Records(r) => {
            let names: Option<Vec<String>> = r
                .edits
                .iter()
                .map(|e| match e {
                    RecordEdit::AddRequiredBy(by) => Some(by.clone()),
                    _ => None,
                })
                .collect();
            names.unwrap_or_default()
        }
    }
}

/// Whether a work asked for by `requesters` goes under the lock: every one of them was
/// planned in this transaction (`planned`, by addon key) and none still commits
/// (`kept`). A work no one asked for, or asked for by an addon outside the transaction,
/// stays.
fn requesters_dropped(
    requesters: &[String],
    planned: &BTreeSet<String>,
    kept: &BTreeSet<String>,
) -> bool {
    !requesters.is_empty()
        && requesters.iter().all(|r| {
            let key = TargetRecord::addon_key(r);
            planned.contains(&key) && !kept.contains(&key)
        })
}

/// The record after `works`: changed addons in, removed ones out, transferred folders
/// taken from and degraded on their previous owners, generation + 1. A folder change
/// stamps `installed_at`; a records-only change keeps it, except an adoption, which
/// takes the commit time.
fn next_record(
    current: &TargetRecord,
    works: &[AddonWork],
    tx: &TxId,
    now: Timestamp,
) -> TargetRecord {
    let mut record = current.clone();
    for w in works {
        match w.record() {
            Some(new) => {
                let mut new = new.clone();
                new.tx_id = *tx;
                match &w.mode {
                    WorkMode::Records(r) if r.before.is_some() => {}
                    _ => new.installed_at = now,
                }
                record
                    .addons
                    .insert(TargetRecord::addon_key(&new.name), new);
            }
            None => {
                record.addons.remove(&TargetRecord::addon_key(&w.addon));
            }
        }
        for t in w.transfers() {
            if let Some(previous) = record.addons.get_mut(&TargetRecord::addon_key(&t.from)) {
                previous.folders.retain(|f| f != &t.folder);
            }
        }
        for key in w.degraded() {
            if let Some(previous) = record.addons.get_mut(key) {
                previous.state = AddonState::Degraded;
            }
        }
    }
    record.bump();
    record
}

/// Records-only works' index changes: an adoption's folders in, a forgotten addon's
/// out. They ride in the journal payload with the swaps'.
fn records_index_changes(works: &[AddonWork], changes: &mut IndexChanges) {
    for w in works {
        if let WorkMode::Records(r) = &w.mode {
            changes
                .upsert
                .extend(r.index_upserts.iter().map(|(_, index)| index.clone()));
            changes
                .remove
                .extend(r.index_removals.iter().map(|f| f.key().to_owned()));
        }
    }
}

/// Files index changes: staged indexes under their on-disk spelling in, each recorded at
/// `stamp_ns`; removed folders out.
pub(super) fn index_changes(ops: &[SwapOp], staged: &Staged, stamp_ns: i128) -> IndexChanges {
    let mut changes = IndexChanges::default();
    let staged_index = |name: &MutableFolderName| {
        staged
            .folders
            .iter()
            .find(|f| f.name == *name.as_folder())
            .map(|f| f.index.clone())
    };
    for op in ops {
        match op {
            SwapOp::Add { name } => changes
                .upsert
                .extend(staged_index(name).map(|i| i.stamped(stamp_ns))),
            SwapOp::Replace { name, live, .. } => {
                if let Some(index) = staged_index(name) {
                    changes
                        .upsert
                        .push(FolderIndex::new(live.as_str(), index.files).stamped(stamp_ns));
                }
            }
            SwapOp::Remove { live, .. } => changes.remove.push(live.key().to_owned()),
        }
    }
    changes
}

/// One op per folder swapped, and one `record` op per folder of a records-only work's
/// record before or after: what undo and its conflict check read.
fn history_ops(works: &[AddonWork]) -> Vec<HistoryOp> {
    let mut ops = Vec::new();
    for w in works {
        let op = |op, folder: &FolderName| HistoryOp {
            op,
            folder: folder.clone(),
            addon: w.addon.clone(),
            edit: None,
            backup: None,
            from: None,
            files: None,
        };
        match &w.mode {
            // A work whose every folder was handed over still changed its record (a
            // removed addon): its folders as record ops, so undo restores it.
            WorkMode::Folders(f) if f.swap.is_empty() && !f.handed_over.is_empty() => ops.extend(
                f.handed_over
                    .iter()
                    .map(|folder| op(FolderOp::Record, folder)),
            ),
            WorkMode::Folders(f) => ops.extend(f.swap.iter().map(|swap| match swap {
                SwapOp::Add { name } => op(FolderOp::Add, name.as_folder()),
                SwapOp::Replace { live, .. } => op(FolderOp::Replace, live.as_folder()),
                SwapOp::Remove { live, .. } => op(FolderOp::Remove, live.as_folder()),
            })),
            WorkMode::Records(r) => {
                let mut folders: Vec<&FolderName> = Vec::new();
                for folder in r
                    .before
                    .iter()
                    .chain(r.after.iter())
                    .flat_map(|a| &a.folders)
                {
                    if !folders.contains(&folder) {
                        folders.push(folder);
                    }
                }
                folders.sort_by(|a, b| a.key().cmp(b.key()));
                // Only `required_by` changed: one op per folder and edit, undone in place
                // (M2-D35). Any other edit keeps plain ops and snapshot-restore undo.
                let in_place: Option<Vec<HistoryEdit>> = r
                    .edits
                    .iter()
                    .map(|e| match e {
                        RecordEdit::AddRequiredBy(by) => {
                            Some(HistoryEdit::RequiredByAdd(by.clone()))
                        }
                        RecordEdit::RemoveRequiredBy(by) => {
                            Some(HistoryEdit::RequiredByRemove(by.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                match in_place.filter(|edits| !edits.is_empty()) {
                    Some(edits) => {
                        for f in folders {
                            for edit in &edits {
                                ops.push(HistoryOp {
                                    edit: Some(edit.clone()),
                                    ..op(FolderOp::Record, f)
                                });
                            }
                        }
                    }
                    None => ops.extend(folders.into_iter().map(|f| op(FolderOp::Record, f))),
                }
            }
        }
    }
    ops
}

fn touched_folder(op: &SwapOp) -> FolderName {
    match op {
        SwapOp::Add { name } => name.as_folder().clone(),
        SwapOp::Replace { live, .. } | SwapOp::Remove { live, .. } => live.as_folder().clone(),
    }
}

/// Rewrites `status.json` (monotonic). Best effort: it is derived data, and a lost
/// race only leaves the summary behind.
pub(super) fn write_status(
    paths: &Paths,
    scopes: &ScopeTable,
    target: &Target,
    record: &TargetRecord,
) {
    let mut records = vec![record.clone()];
    let mut meta: BTreeMap<TargetKey, TargetMeta> = BTreeMap::new();
    if let Ok(Some(existing)) = store::load::<StatusFile>(&paths.status_file()) {
        for (key, summary) in existing.value.targets {
            meta.insert(
                key,
                TargetMeta {
                    id: summary.id,
                    family: summary.family,
                    version: summary.version,
                    interface: summary.interface,
                },
            );
            // ubs:ignore — comparing target keys, not secrets
            if key != record.key
                && let Ok(loaded) = load_record(paths, &key)
            {
                records.push(loaded.value);
            }
        }
    }
    meta.insert(
        record.key,
        TargetMeta {
            id: target.id.clone(),
            family: serde_json::to_value(target.family)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            version: target.version.to_string(),
            interface: target.interface,
        },
    );
    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "status.json");
    let _ = write_monotonic(&mut s, paths, &StatusFile::build(&records, &meta));
}

/// Prunes what the transaction left (its journal, stage dir in `client`, temp downloads
/// and lock file) under its own in-flight lock, then drops the lock. Unless the caller
/// saw it commit, its snapshots go too once no journal names it and no history line
/// does (a failure after the commit point still committed). An unfinished journal (the
/// commit failed and so did recovering it) stays, with its stage dir, which can hold the
/// old folders, its snapshots and its lock file: dropping the lock frees them for the
/// next global lock holder, which recovers them. Best effort otherwise: recovery and
/// retention sweep anything left.
pub(super) fn cleanup(
    paths: &Paths,
    scopes: &ScopeTable,
    inflight: InflightLock,
    tx: &TxId,
    client: Option<&ClientPaths>,
    committed: bool,
) {
    let mut victims = vec![paths.cache_inflight_lock(tx), paths.cache_tmp(tx)];
    if !journal::unfinished(paths, tx) {
        victims.extend([
            paths.journal(tx),
            paths.journal_wtf(tx),
            paths.inflight_lock(tx),
        ]);
        victims.extend(
            client
                .into_iter()
                .flat_map(|c| [c.stage(tx), c.stage_wtf(tx)]),
        );
    }
    victims.retain(|v| v.exists());
    if !victims.is_empty() {
        // Under the transaction's own in-flight lock: cleanup never waits for the
        // global lock, so planning and --dry-run never take it.
        let _ = fsx::prune(
            scopes,
            PruneReason::PostCommitCleanup,
            &victims,
            LockUse::Own(&inflight),
        );
    }
    if !committed && journal::existing(paths, tx).is_none() {
        let dead = super::retention::dead_snapshots(paths, client, tx);
        // An unreadable history might name it: kept.
        let in_history =
            || history::text(paths).map_or(true, |text| super::retention::in_history(&text, tx));
        if !dead.victims.is_empty() && !in_history() {
            let _ = fsx::prune(
                scopes,
                PruneReason::Retention,
                &dead.victims,
                LockUse::Own(&inflight),
            );
        }
    }
    drop(inflight);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::engine::testing::{Pause, TxScene};
    use crate::error::ErrorCode;
    use crate::state::history::HistoryFilter;

    const V1: &[(&str, &str)] = &[
        ("Foo/Foo.toc", "## Interface: 120105\n## Notes: v1\n"),
        ("FooOld/FooOld.toc", "## Interface: 120105\n"),
    ];
    const V2: &[(&str, &str)] = &[
        ("Foo/Foo.toc", "## Interface: 120105\n## Notes: v2\n"),
        ("Foo/new.lua", "--"),
    ];

    /// A commit whose in-process recovery stopped carries the stop's fix (the doctor's
    /// explanation); one whose recovery failed on I/O, or waits for the game, keeps
    /// `omaddons status`. Both keep the journal and the recovery's error in `details`.
    #[test]
    fn recovery_failed_keeps_a_stops_fix() {
        let tx: TxId = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let status = Suggestion::command(["omaddons", "status"]);
        let explained = crate::doctor::registry::explain_fix(
            crate::doctor::registry::ids::JOURNAL_UNEXPECTED_STATE,
            Some("retail"),
        );
        for (case, recovery, fix) in [
            (
                "recovery_is_io",
                Error::new(ErrorCode::Io, "disk", status.clone()),
                status.clone(),
            ),
            (
                "recovery_is_a_stop",
                Error::new(ErrorCode::Internal, "stopped", explained.clone())
                    .details(serde_json::json!({"reason": "unexpected_state"})),
                explained.clone(),
            ),
            (
                "recovery_is_the_game_wait",
                Error::new(
                    ErrorCode::GameRunning,
                    "waits",
                    Suggestion::text("quit the game; the next change finishes or undoes it"),
                )
                .details(serde_json::json!({"reason": "saved_variables"})),
                status.clone(),
            ),
        ] {
            let cause = Error::new(ErrorCode::Io, "commit", status.clone());
            let failed = recovery_failed(cause, &tx, &recovery);
            assert_eq!(failed.fix, fix, "{case}");
            assert_eq!(failed.details["journal_pending"], tx.to_string(), "{case}");
            assert_eq!(
                failed.details["recovery_error"]["code"],
                serde_json::to_value(recovery.code).unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_dependency_goes_only_when_every_requester_in_the_transaction_dropped_out() {
        let keys = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|n| TargetRecord::addon_key(n)).collect()
        };
        let by =
            |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() };
        let planned = keys(&["LittleWigs", "Other", "BigWigs"]);
        // Asked for by no one (a user's request): stays.
        assert!(!requesters_dropped(&[], &planned, &BTreeSet::new()));
        // Its requester still commits (keys fold case): stays.
        assert!(!requesters_dropped(
            &by(&["littlewigs"]),
            &planned,
            &keys(&["LittleWigs"])
        ));
        // Its only requester dropped out: goes.
        assert!(requesters_dropped(
            &by(&["LittleWigs"]),
            &planned,
            &BTreeSet::new()
        ));
        // One of two still commits: stays; both gone: goes.
        let two = by(&["LittleWigs", "Other"]);
        assert!(!requesters_dropped(&two, &planned, &keys(&["Other"])));
        assert!(requesters_dropped(&two, &planned, &BTreeSet::new()));
        // A requester outside the transaction (an installed addon): stays.
        assert!(!requesters_dropped(
            &by(&["Installed"]),
            &planned,
            &BTreeSet::new()
        ));
    }

    /// A scene with v1 of gh:o/foo committed.
    fn with_v1() -> TxScene {
        let scene = TxScene::new();
        let v1 = scene.zip("v1.zip", V1);
        let done = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        scene
    }

    /// A binding that counts its `plan` calls and refuses or accepts.
    struct Counting {
        refuse: bool,
        asked: std::cell::Cell<u32>,
    }

    impl Binding for Counting {
        fn plan(&self, _plan: &Plan) -> Result<()> {
            self.asked.set(self.asked.get() + 1);
            if self.refuse {
                Err(Error::plan_stale(
                    crate::error::PlanStaleSource::Apply,
                    &[crate::error::StalePart::Selection],
                    "refused by the test binding",
                    Suggestion::text("preview again"),
                ))
            } else {
                Ok(())
            }
        }

        fn undo(&self, _planned: &super::super::undo::UndoOutcome) -> Result<()> {
            Ok(())
        }

        fn moved(&self, _target: &str) -> Error {
            Error::internal("not asked in a dry run")
        }
    }

    #[test]
    fn a_bound_dry_run_asks_its_binding() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let state = crate::engine::testing::tree_snapshot(scene.paths.state_dir());
        let refusing = Counting {
            refuse: true,
            asked: std::cell::Cell::new(0),
        };
        let refused = scene.run_bound(
            vec![scene.install("gh:o/foo", &v2)],
            None,
            TxKind::Install,
            true,
            None,
            Some(&refusing),
        );
        assert!(
            // ubs:ignore — comparing error codes, not secrets
            matches!(&refused, Err(e) if e.code == ErrorCode::PlanStale),
            "{refused:?}"
        );
        assert_eq!(refusing.asked.get(), 1);
        assert_eq!(
            crate::engine::testing::tree_snapshot(scene.paths.state_dir()),
            state,
            "a refused bound dry run writes nothing under <state>"
        );
        let accepting = Counting {
            refuse: false,
            asked: std::cell::Cell::new(0),
        };
        let done = scene
            .run_bound(
                vec![scene.install("gh:o/foo", &v2)],
                None,
                TxKind::Install,
                true,
                None,
                Some(&accepting),
            )
            .unwrap();
        assert_eq!(accepting.asked.get(), 1, "asked exactly once");
        assert!(done.plan.has_ops(), "the dry run returns its plan");
    }

    #[test]
    fn an_unbound_dry_run_asks_nothing() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let before = generation(&scene);
        let done = scene
            .run_bound(
                vec![scene.install("gh:o/foo", &v2)],
                None,
                TxKind::Install,
                true,
                None,
                None,
            )
            .unwrap();
        assert!(done.plan.has_ops() && done.outcomes.iter().all(|o| o.tx.is_none()));
        assert_eq!(generation(&scene), before, "a dry run commits nothing");
    }

    fn generation(scene: &TxScene) -> u64 {
        let key = crate::discover::registry::read_marker(scene.client.client_dir()).unwrap();
        load_record(&scene.paths, &key).unwrap().value.generation
    }

    fn history(scene: &TxScene) -> Vec<HistoryEntry> {
        history::read(&scene.paths, &HistoryFilter::default())
            .unwrap()
            .entries
    }

    /// Journals, locks and stage dirs, and the snapshot dirs of transactions that did not
    /// commit (no history line).
    fn leftovers(scene: &TxScene) -> Vec<String> {
        let mut found = Vec::new();
        for dir in [
            scene.paths.journal_dir(),
            scene.paths.inflight_dir(),
            scene.paths.cache_inflight_dir(),
            scene.client.meta_dir().join("stage"),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(listing.map(|e| e.unwrap().path().display().to_string()));
            }
        }
        let committed: BTreeSet<String> = history(scene).iter().map(|e| e.tx.to_string()).collect();
        for dir in [
            scene.client.meta_dir().join("snapshots"),
            scene.paths.state_snapshots_dir(),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(
                    listing
                        .map(|e| e.unwrap())
                        .filter(|e| !committed.contains(e.file_name().to_str().unwrap()))
                        .map(|e| e.path().display().to_string()),
                );
            }
        }
        found
    }

    /// Committed transactions whose snapshot is gone. Retention keeps a day of them, so
    /// a test sees none: an error after the commit point must not take a snapshot.
    fn lost_snapshots(scene: &TxScene) -> Vec<TxId> {
        history(scene)
            .into_iter()
            .map(|e| e.tx)
            .filter(|tx| !scene.paths.state_snapshot(tx).is_dir())
            .collect()
    }

    #[test]
    fn a_first_install_registers_commits_and_cleans_up() {
        let scene = TxScene::new();
        let v1 = scene.zip("v1.zip", V1);
        let done = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        assert_eq!(
            (outcome.kind, outcome.atomic_swap, outcome.dry_run),
            (Some(TxKind::Install), true, false)
        );
        assert!(outcome.tx.is_some() && outcome.key.is_some());
        assert!(
            outcome.lock_held_ms.is_some_and(|ms| ms < 2_000),
            "{:?}",
            outcome.lock_held_ms
        );
        let key = outcome.key.unwrap();
        assert_eq!(
            crate::discover::registry::read_marker(scene.client.client_dir()),
            Some(key),
            "registered under the lock"
        );
        assert!(
            scene.client.addons().join("Foo/Foo.toc").is_file()
                && scene.client.addons().join("FooOld").is_dir()
        );
        let record = load_record(&scene.paths, &key).unwrap().value;
        assert_eq!(record.generation, 1);
        let foo = &record.addons["foo"];
        assert_eq!((foo.tx_id, foo.folders.len()), (outcome.tx.unwrap(), 2));
        let index = store::load::<FilesIndex>(&scene.paths.files_index(&key))
            .unwrap()
            .unwrap()
            .value;
        assert_eq!(index.folders.keys().collect::<Vec<_>>(), ["foo", "fooold"]);
        assert_eq!(index.folders["foo"].files.len(), 1);
        let entries = history(&scene);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (
                entries[0].reason.as_deref(),
                entries[0].argv[0].as_str(),
                entries[0].actor
            ),
            (Some("testing"), "omaddons", Actor::Agent)
        );
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
        let status = store::load::<StatusFile>(&scene.paths.status_file())
            .unwrap()
            .unwrap()
            .value;
        assert_eq!(status.generations[&key], 1);
    }

    #[test]
    fn updates_replace_and_drop_folders_and_removes_take_them_out() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let done = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        assert_eq!(done.outcomes[0].kind, Some(TxKind::Update));
        assert!(!scene.client.addons().join("FooOld").exists());
        assert!(scene.client.addons().join("Foo/new.lua").is_file());
        assert_eq!(generation(&scene), 2);

        let done = scene
            .run(vec![plan::RequestedChange::Remove {
                addon: "Foo".into(),
            }])
            .unwrap();
        assert_eq!(done.outcomes[0].kind, Some(TxKind::Remove));
        assert!(!scene.client.addons().join("Foo").exists());
        assert_eq!(generation(&scene), 3);
        assert_eq!(history(&scene).len(), 3);
        assert!(leftovers(&scene).is_empty());

        let again = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        assert_eq!(again.outcomes[0].kind, Some(TxKind::Install));
        let same = scene.run(vec![scene.install("gh:o/foo", &v2)]);
        assert!(same.is_ok());
    }

    /// The tree after a clean v1 → v2 update.
    fn updated_tree() -> Vec<TreeEntry> {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        scene.addons()
    }

    use crate::engine::testing::TreeEntry;

    /// An update completed by the in-process roll-forward reports the swap its journal
    /// recorded: two plain renames per folder are not atomic, also when the error hits
    /// inside the swap, whose own report is then lost.
    #[cfg(debug_assertions)]
    #[test]
    fn a_rolled_forward_update_reports_the_swap_it_journaled() {
        let new_tree = updated_tree();
        for (spec, atomic) in [
            ("fsx.renameat2.einval", false),
            ("engine.record.written=error", true),
            ("fsx.renameat2.einval,engine.record.written=error", false),
            ("fsx.renameat2.einval,engine.swapped=error", false),
        ] {
            let scene = with_v1();
            let v2 = scene.zip("v2.zip", V2);
            let done = crate::fsx::failpoint::with_spec(spec, || {
                scene.run(vec![scene.install("gh:o/foo", &v2)])
            })
            .unwrap();
            assert!(done.failures.is_empty(), "{spec}: {:?}", done.failures);
            let outcome = &done.outcomes[0];
            let unavailable = outcome
                .warnings
                .iter()
                .filter(|w| matches!(w.code, crate::error::WarningCode::AtomicSwapUnavailable))
                .count();
            assert_eq!(
                (outcome.atomic_swap, unavailable),
                (atomic, usize::from(!atomic)),
                "{spec}: {:?}",
                outcome.warnings
            );
            assert_eq!(scene.addons(), new_tree, "{spec}");
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_error_at_any_failpoint_leaves_folders_wholly_old_or_wholly_new() {
        let new_tree = updated_tree();
        let after_swap = [
            "engine.swapped",
            "engine.record.written",
            "engine.history.appended",
            "engine.committed",
            "engine.unlocked",
            "engine.cleanup.done",
            // Retention runs after the commit; its failure is a warning, never an error.
            "engine.retention",
            // The pass over the other clients, the same; this scene has none.
            "engine.retention.sweep",
        ];
        for name in FAILPOINTS {
            // Not on an update's path; the first-install crash matrix covers the
            // AddOns folder's creation.
            if matches!(
                *name,
                "engine.plan.downloaded"
                    | "engine.recover.step"
                    | "engine.recover.sv"
                    | "engine.recover.forward"
                    | "engine.addons_dir.created"
                    // A restore's own, and its recovery's preservation: their matrices
                    // are engine_crash's restore scenes.
                    | "engine.restore.staged"
                    | "engine.restore.exchanged"
                    | "engine.restore.snapshotted"
                    | "engine.preserve.intent"
                    | "engine.preserve.moved"
                    | "engine.preserve.published"
                    // An automatic backup's: its failure never fails the change, and its
                    // scenes are engine_crash's and backup_auto's.
                    | "engine.autobackup"
            ) {
                continue;
            }
            let scene = with_v1();
            let old_tree = scene.addons();
            let v2 = scene.zip("v2.zip", V2);
            let result = failpoint::with_spec(&format!("{name}=error"), || {
                scene.run(vec![scene.install("gh:o/foo", &v2)])
            });
            let failed = match &result {
                Err(_) => true,
                Ok(done) => !done.failures.is_empty(),
            };
            if after_swap.contains(name) {
                assert_eq!(scene.addons(), new_tree, "{name}: wholly new");
                assert_eq!(generation(&scene), 2, "{name}: record written");
                assert_eq!(history(&scene).len(), 2, "{name}: history written once");
            } else {
                assert!(failed, "{name}: the error surfaces");
                assert_eq!(
                    scene.addons(),
                    old_tree,
                    "{name}: wholly old after {result:?}"
                );
                assert_eq!(generation(&scene), 1, "{name}: no record change");
            }
            // A later run finishes or redoes cleanly.
            let rerun = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
            assert!(rerun.failures.is_empty(), "{name}: {:?}", rerun.failures);
            assert_eq!(scene.addons(), new_tree, "{name}: converges");
            assert!(
                leftovers(&scene).is_empty(),
                "{name}: {:?}",
                leftovers(&scene)
            );
            assert!(
                lost_snapshots(&scene).is_empty(),
                "{name}: {:?}",
                lost_snapshots(&scene)
            );
        }
    }

    #[test]
    fn an_unreadable_process_table_fails_closed() {
        let targets: Vec<Target> = ["/c/_retail_", "/c/_classic_"]
            .iter()
            .map(|d| crate::engine::testing::target(Path::new(d)))
            .collect();
        let unknown = RunningNow::from_scan(
            RunningScan {
                clients: BTreeMap::new(),
                unavailable: Some("/proc could not be listed: permission denied".into()),
            },
            &targets,
        );
        assert!(targets.iter().all(|t| unknown.contains(&t.client_dir)));
        let warning = unknown.warning().unwrap();
        assert_eq!(warning.code, crate::error::WarningCode::RunningUnknown);
        assert!(
            warning.message.contains("permission denied"),
            "{}",
            warning.message
        );

        let known = RunningNow::from_scan(RunningScan::default(), &targets);
        assert!(!targets.iter().any(|t| known.contains(&t.client_dir)));
        assert_eq!(known.warning(), None);
    }

    /// A commit that fails, and whose in-process recovery fails too, keeps the journal,
    /// the stage dir (holding the old Foo after the exchange) and its snapshot; the next
    /// lock holder then rolls it back, or forward with one history line.
    #[cfg(debug_assertions)]
    #[test]
    fn a_failed_recovery_keeps_the_journal_for_the_next_lock_holder() {
        let new_tree = updated_tree();
        for (spec, forward) in [
            (
                "engine.swap.after_exchange=error,engine.recover.step=error",
                false,
            ),
            (
                "engine.record.written=error,engine.recover.forward=error",
                true,
            ),
        ] {
            let scene = with_v1();
            let old_tree = scene.addons();
            let v2 = scene.zip("v2.zip", V2);
            let done =
                failpoint::with_spec(spec, || scene.run(vec![scene.install("gh:o/foo", &v2)]))
                    .unwrap();
            assert_eq!(done.failures.len(), 1, "{spec}");
            let error = &done.failures[0].error;
            assert!(
                error.message.contains("recovering it failed too"),
                "{spec}: {}",
                error.message
            );
            assert_eq!(
                error.fix_argv.as_deref(),
                Some(&["omaddons".to_owned(), "status".to_owned()][..])
            );
            assert!(
                error.details["recovery_error"]["code"].is_string(),
                "{spec}"
            );
            let tx: TxId = error.details["journal_pending"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!(scene.paths.journal(&tx).is_file(), "{spec}: journal kept");
            assert!(scene.client.stage(&tx).is_dir(), "{spec}: stage dir kept");
            assert!(
                scene.client.snapshots(&tx).is_dir(),
                "{spec}: snapshot kept"
            );

            let lock = GlobalLock::acquire(&scene.paths, "test").unwrap();
            let scopes = ScopeTable::new(
                &scene.paths,
                &[scene.client.client_dir().to_path_buf()],
                None,
            )
            .unwrap();
            let warnings = recover_all(&RecoveryEnv {
                lock: &lock,
                paths: &scene.paths,
                scopes: &scopes,
                own: &[],
                sv_writable: &|_| false,
            })
            .unwrap();
            drop(lock);
            assert_eq!(warnings.len(), 1, "{spec}: {warnings:?}");
            assert_eq!(
                warnings[0].code,
                crate::error::WarningCode::JournalRecovered
            );
            if forward {
                assert_eq!(scene.addons(), new_tree, "{spec}: rolled forward");
                assert_eq!(
                    (generation(&scene), history(&scene).len()),
                    (2, 2),
                    "{spec}"
                );
            } else {
                assert_eq!(scene.addons(), old_tree, "{spec}: rolled back");
                assert_eq!(
                    (generation(&scene), history(&scene).len()),
                    (1, 1),
                    "{spec}"
                );
            }
            assert!(
                leftovers(&scene).is_empty(),
                "{spec}: {:?}",
                leftovers(&scene)
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_lock_is_free_while_staging() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let worker = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                    scene.run(vec![scene.install("gh:o/foo", &v2)])
                })
            });
            pause.reached("engine.stage.synced");
            let lock =
                GlobalLock::acquire_with_timeout(&scene.paths, "other", Duration::from_millis(200));
            assert!(
                lock.is_ok(),
                "another process can take the lock during staging"
            );
            drop(lock);
            pause.resume("engine.stage.synced");
            let done = worker.join().unwrap().unwrap();
            assert!(done.failures.is_empty(), "{:?}", done.failures);
        });
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_concurrent_change_is_replanned_once() {
        // Same release committed meanwhile: a no-op.
        let scene = TxScene::new();
        let v1 = scene.zip("v1.zip", V1);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                    scene.run(vec![scene.install("gh:o/foo", &v1)])
                })
            });
            pause.reached("engine.stage.synced");
            let fast = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
            assert_eq!(fast.outcomes[0].kind, Some(TxKind::Install));
            pause.resume("engine.stage.synced");
            let slow = slow.join().unwrap().unwrap();
            assert!(slow.failures.is_empty(), "{:?}", slow.failures);
            assert_eq!(
                (slow.outcomes[0].tx, slow.outcomes[0].kind),
                (None, None),
                "became a no-op"
            );
        });
        assert_eq!(generation(&scene), 1);

        // A conflicting change: plan_stale naming the cause.
        let scene = with_v1();
        let other = scene.zip(
            "other.zip",
            &[("FooOld/FooOld.toc", "## Interface: 120105\n")],
        );
        let mine = scene.zip("mine.zip", &[("Bar/Bar.toc", "## Interface: 120105\n")]);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                    scene.run(vec![scene.install("gh:o/bar", &mine)])
                })
            });
            pause.reached("engine.stage.synced");
            scene
                .run(vec![plan::RequestedChange::Remove {
                    addon: "Foo".into(),
                }])
                .unwrap();
            // An unmanaged folder now takes Bar's name.
            std::fs::create_dir_all(scene.client.addons().join("Bar")).unwrap();
            pause.resume("engine.stage.synced");
            let slow = slow.join().unwrap().unwrap();
            let error = slow.failures[0].cause.as_ref().unwrap();
            assert_eq!((error.code, error.retryable), (ErrorCode::PlanStale, true));
            assert_eq!(error.details["cause"], "unmanaged_folder");
            let _ = other;
        });
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[test]
    fn a_held_lock_times_out_as_locked() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let _held = GlobalLock::acquire(&scene.paths, "someone else").unwrap();
        let done = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        let error = done.failures[0].cause.as_ref().unwrap();
        assert_eq!(
            (error.code, error.exit().code(), error.retryable),
            (ErrorCode::Locked, 5, true)
        );
        assert_eq!(error.details["holder"]["command"], "someone else");
    }

    // -----------------------------------------------------------------------------------
    // A run applied from a stored plan (`apply`, M4-D25).

    const PLAN: &str = "01M9P0000000000000000000PA";
    const BAR: &[(&str, &str)] = &[("Bar/Bar.toc", "## Interface: 120105\n")];

    fn plan_id() -> crate::model::PlanId {
        PLAN.parse().unwrap()
    }

    /// The history file's lines as written.
    fn raw_history(scene: &TxScene) -> Vec<String> {
        std::fs::read_to_string(scene.paths.history_file())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_commit_while_applying_records_the_plan() {
        let mut scene = with_v1();
        scene.applying = Some(plan_id());
        let v2 = scene.zip("v2.zip", V2);
        let done = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let lines = history(&scene);
        let raw = raw_history(&scene);
        assert_eq!(
            lines.iter().map(|l| l.plan).collect::<Vec<_>>(),
            [None, Some(plan_id())],
            "the install made directly, then the one applied: {raw:#?}"
        );
        assert!(!raw[0].contains("\"plan\""), "no key at all: {}", raw[0]);
        assert!(
            raw[1].contains(&format!("\"plan\":\"{PLAN}\"")),
            "{}",
            raw[1]
        );
    }

    #[test]
    fn an_automatic_backup_line_never_carries_the_plan() {
        let mut scene = with_v1();
        std::fs::create_dir_all(scene.client.wtf()).unwrap();
        std::fs::write(scene.client.wtf().join("Config.wtf"), "SET a \"1\"\n").unwrap();
        // Byte copies, so the backup does not depend on the scene's filesystem cloning;
        // the first change since automatic backups were on is the day's first.
        scene.auto_backup = Some(crate::state::config::AutoBackup::DailyCopy);
        scene.applying = Some(plan_id());
        let v2 = scene.zip("v2.zip", V2);
        let done = scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let lines = history(&scene);
        let kinds: Vec<(TxKind, Option<crate::model::PlanId>)> =
            lines.iter().map(|l| (l.kind, l.plan)).collect();
        // ubs:ignore — comparing transaction kinds, not secrets
        let backup = lines.iter().position(|l| l.kind == TxKind::Backup);
        assert!(
            backup.is_some(),
            "an automatic backup line: {kinds:?}, warnings {:?}",
            done.warnings
        );
        assert_eq!(
            kinds[1..],
            [(TxKind::Backup, None), (lines[2].kind, Some(plan_id()))],
            "the backup before the change carries no plan; the change does"
        );
        assert!(
            !raw_history(&scene)[1].contains("\"plan\""),
            "{:?}",
            raw_history(&scene)
        );
    }

    #[test]
    fn the_doorbell_and_the_holder_name_apply() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let req = scene.request(vec![scene.install("gh:o/foo", &v2)], None, TxKind::Install);
        let done = scene
            .with_ctx("apply", |ctx| super::run(&req, ctx))
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let read = |path: PathBuf| -> serde_json::Value {
            let text = std::fs::read_to_string(&path).unwrap();
            let value = serde_json::from_str(text.trim());
            assert!(value.is_ok(), "{path:?}: {value:?}: {text}");
            value.unwrap()
        };
        let bell = read(scene.paths.doorbell_file());
        assert_eq!(bell["command"], "apply", "{bell}");
        // The lock file keeps its last holder's line after the lock is released.
        let holder = read(scene.paths.lock_file());
        assert_eq!(holder["command"], "apply", "{holder}");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_rolled_forward_commit_keeps_the_plan() {
        let mut scene = with_v1();
        scene.applying = Some(plan_id());
        let v2 = scene.zip("v2.zip", V2);
        // Swapped, then the in-process roll-forward fails too: the journal waits.
        let done =
            failpoint::with_spec("engine.swapped=error,engine.recover.forward=error", || {
                scene.run(vec![scene.install("gh:o/foo", &v2)])
            })
            .unwrap();
        assert_eq!(done.failures.len(), 1, "{:?}", done.failures);
        assert_eq!(history(&scene).len(), 1, "nothing appended yet");
        // The next commit, made directly, recovers it first.
        scene.applying = None;
        let bar = scene.zip("bar.zip", BAR);
        let done = scene.run(vec![scene.install("gh:o/bar", &bar)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let addons = scene.client.addons();
        assert!(
            addons.join("Foo/new.lua").is_file() && addons.join("Bar/Bar.toc").is_file(),
            "Foo rolled forward to v2, then Bar installed: {:?}",
            scene.addons()
        );
        let lines = history(&scene);
        assert_eq!(
            lines.iter().map(|l| l.plan).collect::<Vec<_>>(),
            [None, Some(plan_id()), None],
            "the recovered line is the one prepared while applying: {:#?}",
            raw_history(&scene)
        );
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[test]
    fn failpoints_list_every_engine_failpoint() {
        use crate::source_scan::{Literals, production_code, sources};

        let mut found = BTreeSet::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // The engine's, and the automatic backup its changes start with.
        for dir in [root.join("src/engine"), root.join("src/backup")] {
            for (_, text) in sources(&dir) {
                let code = production_code(&text, Literals::Keep).unwrap_or_default();
                for (at, _) in code.match_indices("failpoint::hit(\"engine.") {
                    let rest = &code[at + "failpoint::hit(\"".len()..];
                    found.insert(rest.split('"').next().unwrap().to_owned());
                }
            }
        }
        let listed: BTreeSet<String> = FAILPOINTS.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(found, listed);
    }

    // ---- Records-only work -----------------------------------------------------------

    use crate::engine::edit::{EditField, RecordEdit};
    use crate::engine::plan::ChangeKind;
    use crate::engine::undo::{SvPolicy, UndoSelector};

    fn names(list: &[FolderName]) -> Vec<&str> {
        list.iter().map(FolderName::as_str).collect()
    }

    fn pin(scene: &TxScene) -> Result<TxOutcomes> {
        scene.run(vec![scene.edit("Foo", RecordEdit::SetPinned(true))])
    }

    /// The last history line's ops as `(op, folder)`.
    fn last_ops(scene: &TxScene) -> Vec<(FolderOp, String)> {
        let entries = history(scene);
        entries
            .last()
            .unwrap()
            .ops
            .iter()
            .map(|o| (o.op, o.folder.as_str().to_owned()))
            .collect()
    }

    fn records(folders: &[&str]) -> Vec<(FolderOp, String)> {
        folders
            .iter()
            .map(|f| (FolderOp::Record, (*f).to_owned()))
            .collect()
    }

    #[test]
    fn a_pin_is_a_records_only_transaction_that_undo_reverts() {
        let scene = with_v1();
        let tree = scene.addons();
        let before = scene.record().addons["foo"].clone();
        let done = pin(&scene).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        assert_eq!(
            (outcome.kind, outcome.reload_required, outcome.tx.is_some()),
            (Some(TxKind::Pin), ReloadRequired::None, true)
        );
        let change = &outcome.changes[0];
        assert_eq!(
            (change.kind, change.fields.as_slice()),
            (ChangeKind::Edit, &[EditField::Pinned][..])
        );
        assert_eq!(names(&change.folders.recorded), ["Foo", "FooOld"]);
        assert_eq!(
            (change.from_version.as_deref(), change.to_version.as_deref()),
            (Some(before.version.as_str()), Some(before.version.as_str()))
        );
        let after = scene.record().addons["foo"].clone();
        assert!(after.pinned);
        assert_eq!(
            (after.installed_at, after.release_id.as_ref()),
            (before.installed_at, before.release_id.as_ref()),
            "a records-only change keeps installed_at and the release"
        );
        assert_eq!(after.tx_id, outcome.tx.unwrap());
        assert_eq!(scene.addons(), tree, "AddOns untouched");
        assert_eq!(last_ops(&scene), records(&["Foo", "FooOld"]));
        assert_eq!(history(&scene).last().unwrap().kind, TxKind::Pin);
        assert_eq!(generation(&scene), 2);
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));

        let undone = scene
            .undo(UndoSelector::Latest, SvPolicy::Auto)
            .unwrap()
            .outcome;
        assert!(!scene.record().addons["foo"].pinned, "unpinned");
        assert_eq!(generation(&scene), 3);
        assert_eq!(scene.addons(), tree, "AddOns untouched by the undo too");
        assert_eq!(
            (undone.changes[0].kind, undone.changes[0].fields.as_slice()),
            (ChangeKind::Edit, &[EditField::Pinned][..])
        );
        assert_eq!(
            names(&undone.changes[0].folders.recorded),
            ["Foo", "FooOld"]
        );
        assert_eq!(last_ops(&scene), records(&["Foo", "FooOld"]));
    }

    #[test]
    fn an_edit_that_changes_nothing_commits_nothing() {
        let scene = with_v1();
        let unpin = || scene.edit("Foo", RecordEdit::SetPinned(false));
        let done = scene.run(vec![unpin()]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        assert_eq!((done.outcomes[0].tx, done.outcomes[0].kind), (None, None));
        let pin_then_unpin = vec![scene.edit("Foo", RecordEdit::SetPinned(true)), unpin()];
        let done = scene.run(pin_then_unpin).unwrap();
        assert_eq!(
            (done.outcomes[0].tx, done.outcomes[0].kind),
            (None, None),
            "folded edits that cancel out"
        );
        assert_eq!(generation(&scene), 1);
        let done = scene
            .run(vec![scene.edit("Nope", RecordEdit::SetPinned(true))])
            .unwrap();
        let error = done.failures[0].cause.as_ref().unwrap();
        assert_eq!(error.code, ErrorCode::NotManaged);
    }

    #[test]
    fn an_update_and_an_edit_of_one_addon_fold_into_one_work_in_either_order() {
        for edit_first in [false, true] {
            let scene = with_v1();
            let old_release = scene.record().addons["foo"].release_id.clone();
            let v2 = scene.zip("v2.zip", V2);
            let update = scene.install("gh:o/foo", &v2);
            let edit = scene.edit("Foo", RecordEdit::AddRequiredBy("Bar".into()));
            let changes = if edit_first {
                vec![edit, update]
            } else {
                vec![update, edit]
            };
            let done = scene.run(changes).unwrap();
            assert!(
                done.failures.is_empty(),
                "{edit_first}: {:?}",
                done.failures
            );
            let outcome = &done.outcomes[0];
            assert_eq!(outcome.changes.len(), 1, "{edit_first}: one change");
            assert_eq!(
                (outcome.kind, outcome.changes[0].kind),
                (Some(TxKind::Update), ChangeKind::Update)
            );
            let foo = scene.record().addons["foo"].clone();
            assert_eq!(foo.required_by, ["Bar"], "{edit_first}");
            assert_ne!(foo.release_id, old_release, "{edit_first}: the new release");
            assert!(scene.client.addons().join("Foo/new.lua").is_file());
            assert_eq!(
                last_ops(&scene),
                [
                    (FolderOp::Replace, "Foo".to_owned()),
                    (FolderOp::Remove, "FooOld".to_owned())
                ],
                "{edit_first}: the folder work's ops only"
            );
        }
    }

    #[test]
    fn removing_an_addon_whose_folders_are_gone_forgets_it_and_undo_restores_it() {
        let scene = with_v1();
        let index = scene.index();
        for folder in ["Foo", "FooOld"] {
            std::fs::remove_dir_all(scene.client.addons().join(folder)).unwrap();
        }
        let tree = scene.addons();
        let done = scene
            .run_as(vec![scene.remove("Foo")], None, TxKind::Remove)
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        let change = &outcome.changes[0];
        assert_eq!(
            (outcome.kind, change.kind),
            (Some(TxKind::Remove), ChangeKind::Remove)
        );
        assert!(change.folders.removed.is_empty() && change.fields.is_empty());
        assert_eq!(names(&change.folders.recorded), ["Foo", "FooOld"]);
        assert!(!scene.record().addons.contains_key("foo"));
        assert!(scene.index().folders.is_empty(), "its index entries go");
        assert_eq!(last_ops(&scene), records(&["Foo", "FooOld"]));

        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        let foo = &scene.record().addons["foo"];
        assert_eq!(names(&foo.folders), ["Foo", "FooOld"], "the record is back");
        assert_eq!(
            scene.index().folders,
            index.folders,
            "and its index entries"
        );
        assert_eq!(scene.addons(), tree, "no folder comes back: none was there");
    }

    #[test]
    fn records_only_work_commits_while_the_client_runs() {
        let mut scene = with_v1();
        scene
            .running
            .insert(scene.client.client_dir().to_path_buf());
        let done = pin(&scene).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        assert_eq!(
            (done.outcomes[0].kind, done.outcomes[0].reload_required),
            (Some(TxKind::Pin), ReloadRequired::None)
        );
        let removed = scene.run(vec![scene.remove("Foo")]).unwrap();
        let error = removed.failures[0].cause.as_ref().unwrap();
        assert_eq!(error.code, ErrorCode::GameRunning, "folders still wait");
    }

    #[test]
    fn an_adoption_records_folders_as_they_are_and_undo_forgets_them() {
        let scene = with_v1();
        crate::engine::testing::folder(&scene.client.addons().join("Baz"), "mine");
        let tree = scene.addons();
        let adopt = scene.adopt("Baz");
        let done = scene
            .run_as(vec![adopt], Some(TxKind::Adopt), TxKind::Install)
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        assert_eq!(
            (outcome.kind, outcome.changes[0].kind),
            (Some(TxKind::Adopt), ChangeKind::Adopt)
        );
        assert_eq!(names(&outcome.changes[0].folders.recorded), ["Baz"]);
        let baz = scene.record().addons["baz"].clone();
        assert_eq!(names(&baz.folders), ["Baz"]);
        assert_ne!(
            baz.installed_at,
            "2000-01-01T00:00:00Z".parse::<Timestamp>().unwrap(),
            "an adoption takes the commit time"
        );
        assert_eq!(scene.index().folders["baz"].files.len(), 1);
        assert_eq!(scene.addons(), tree, "adopted as they are");
        assert_eq!(last_ops(&scene), records(&["Baz"]));

        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert!(!scene.record().addons.contains_key("baz"));
        assert!(!scene.index().folders.contains_key("baz"));
        assert_eq!(scene.addons(), tree, "the folder stays, unmanaged again");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn edits_survive_concurrent_changes_their_guards_allow() {
        // A pin, while another addon is installed.
        let scene = with_v1();
        let bar = scene.zip("bar.zip", &[("Bar/Bar.toc", "## Interface: 120105\n")]);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || pin(&scene))
            });
            pause.reached("engine.plan.done");
            scene.run(vec![scene.install("gh:o/bar", &bar)]).unwrap();
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap().unwrap();
            assert!(slow.failures.is_empty(), "{:?}", slow.failures);
            assert_eq!(slow.outcomes[0].kind, Some(TxKind::Pin));
        });
        let record = scene.record();
        assert!(record.addons["foo"].pinned && record.addons.contains_key("bar"));

        // A RemoveRequiredBy, while its addon is updated: it keeps the new release.
        let scene = with_v1();
        let add = scene.edit("Foo", RecordEdit::AddRequiredBy("Bar".into()));
        scene.run(vec![add]).unwrap();
        let v2 = scene.zip("v2.zip", V2);
        let pause = Pause::new();
        let updated = std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || {
                    let drop = scene.edit("Foo", RecordEdit::RemoveRequiredBy("Bar".into()));
                    scene.run(vec![drop])
                })
            });
            pause.reached("engine.plan.done");
            scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
            let updated = scene.record().addons["foo"].release_id.clone();
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap().unwrap();
            assert!(slow.failures.is_empty(), "{:?}", slow.failures);
            assert!(slow.outcomes[0].tx.is_some());
            updated
        });
        let foo = &scene.record().addons["foo"];
        assert!(foo.required_by.is_empty());
        assert_eq!(foo.release_id, updated, "the update stands");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_concurrent_update_breaking_a_pin_guard_is_plan_stale() {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || pin(&scene))
            });
            pause.reached("engine.plan.done");
            scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap().unwrap();
            let error = slow.failures[0].cause.as_ref().unwrap();
            assert_eq!((error.code, error.retryable), (ErrorCode::PlanStale, true));
            assert_eq!(error.details["cause"], "not_found");
        });
        assert!(!scene.record().addons["foo"].pinned);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_changed_adopted_file_is_plan_stale_even_at_an_unchanged_generation() {
        let scene = with_v1();
        let baz = scene.client.addons().join("Baz");
        crate::engine::testing::folder(&baz, "mine");
        let adopt = scene.adopt("Baz");
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || {
                    scene.run_as(vec![adopt.clone()], Some(TxKind::Adopt), TxKind::Install)
                })
            });
            pause.reached("engine.plan.done");
            std::fs::write(baz.join("extra.lua"), "-- added after indexing").unwrap();
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap().unwrap();
            let error = slow.failures[0].cause.as_ref().unwrap();
            assert_eq!((error.code, error.retryable), (ErrorCode::PlanStale, true));
            assert_eq!(error.details["cause"], "hash_mismatch");
        });
        assert_eq!(generation(&scene), 1, "nothing else committed");
        assert!(!scene.record().addons.contains_key("baz"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_forget_whose_folder_came_back_by_hand_is_dropped() {
        let scene = with_v1();
        for folder in ["Foo", "FooOld"] {
            std::fs::remove_dir_all(scene.client.addons().join(folder)).unwrap();
        }
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || {
                    scene.run_as(vec![scene.remove("Foo")], None, TxKind::Remove)
                })
            });
            pause.reached("engine.plan.done");
            crate::engine::testing::folder(&scene.client.addons().join("FooOld"), "back");
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap().unwrap();
            assert!(slow.failures.is_empty(), "{:?}", slow.failures);
            assert_eq!(
                (slow.outcomes[0].tx, slow.outcomes[0].kind),
                (None, None),
                "no longer needed at an unchanged generation"
            );
        });
        assert!(scene.record().addons.contains_key("foo"));
    }

    /// An install of `zip` as gh:o/foo with this pin intent and no version.
    fn install_with(
        scene: &TxScene,
        zip: &std::path::Path,
        pin: plan::PinIntent,
    ) -> RequestedChange {
        let mut change = scene.install("gh:o/foo", zip);
        if let RequestedChange::Install { pin: intent, .. } = &mut change {
            *intent = pin;
        }
        change
    }

    #[test]
    fn a_record_only_difference_is_a_records_only_transaction_of_the_requests_kind() {
        let scene = with_v1();
        let tree = scene.addons();
        let v1 = scene.zip("v1.zip", V1);
        // The same release with --version: pinned-only, so a pin.
        let done = scene
            .run(vec![scene.install_pinned("gh:o/foo", &v1, "1")])
            .unwrap();
        let outcome = &done.outcomes[0];
        assert_eq!(
            (outcome.kind, outcome.changes[0].kind),
            (Some(TxKind::Pin), ChangeKind::Edit)
        );
        assert_eq!(last_ops(&scene), records(&["Foo", "FooOld"]));
        assert_eq!(scene.addons(), tree);

        // Pinned and promoted at once: mixed fields keep the request's kind.
        let scene = with_v1();
        scene
            .run(vec![scene.edit(
                "Foo",
                RecordEdit::SetReason(crate::model::AddonReason::Dependency),
            )])
            .unwrap();
        let done = scene
            .run(vec![scene.install_pinned("gh:o/foo", &v1, "1")])
            .unwrap();
        let outcome = &done.outcomes[0];
        assert_eq!(outcome.kind, Some(TxKind::Install));
        assert_eq!(
            outcome.changes[0].fields,
            [EditField::Pinned, EditField::Reason]
        );
    }

    #[test]
    fn a_pin_intent_alone_never_moves_the_release() {
        let scene = with_v1();
        let release = scene.record().addons["foo"].release_id.clone();
        let tree = scene.addons();
        let v2 = scene.zip("v2.zip", V2);
        let done = scene
            .run(vec![install_with(&scene, &v2, plan::PinIntent::Pin)])
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        assert_eq!(done.outcomes[0].kind, Some(TxKind::Pin));
        let foo = &scene.record().addons["foo"];
        assert!(foo.pinned);
        assert_eq!(foo.release_id, release, "v2 was not installed");
        assert_eq!(scene.addons(), tree);
        let done = scene
            .run(vec![install_with(&scene, &v2, plan::PinIntent::Unpin)])
            .unwrap();
        assert_eq!(done.outcomes[0].kind, Some(TxKind::Pin), "unpin too");
        assert!(!scene.record().addons["foo"].pinned);
        assert_eq!(scene.addons(), tree);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_pin_racing_an_install_of_the_same_release_still_records_itself() {
        let scene = TxScene::new();
        let v1 = scene.zip("v1.zip", V1);
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.stage.synced=pause", || {
                    scene.run(vec![scene.install_pinned("gh:o/foo", &v1, "1")])
                })
            });
            pause.reached("engine.stage.synced");
            let fast = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
            assert_eq!(fast.outcomes[0].kind, Some(TxKind::Install));
            pause.resume("engine.stage.synced");
            let slow = slow.join().unwrap().unwrap();
            assert!(slow.failures.is_empty(), "{:?}", slow.failures);
            assert_eq!(
                slow.plan.targets[0].changes[0].kind,
                ChangeKind::Install,
                "planned before the other install"
            );
            let outcome = &slow.outcomes[0];
            assert_eq!(
                (outcome.kind, outcome.changes[0].kind),
                (Some(TxKind::Pin), ChangeKind::Edit),
                "planned as an install, committed as a pin"
            );
        });
        assert!(scene.record().addons["foo"].pinned);
        assert_eq!(generation(&scene), 2);
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_error_at_any_failpoint_leaves_the_pin_iff_its_history_line() {
        let (mut failed, mut rolled_forward) = (Vec::new(), Vec::new());
        for name in FAILPOINTS {
            let scene = with_v1();
            let tree = scene.addons();
            let result = failpoint::with_spec(&format!("{name}=error"), || pin(&scene));
            if result
                .as_ref()
                .map_or(true, |done| !done.failures.is_empty())
            {
                failed.push(*name);
            }
            let pinned = scene.record().addons["foo"].pinned;
            if pinned {
                rolled_forward.push(*name);
            }
            let entries = history(&scene);
            // ubs:ignore — comparing transaction kinds, not secrets
            let logged = entries.iter().any(|e| e.kind == TxKind::Pin);
            assert_eq!(
                pinned, logged,
                "{name}: pinned iff logged, after {result:?}"
            );
            assert_eq!(
                generation(&scene),
                entries.len() as u64,
                "{name}: one generation per commit"
            );
            assert_eq!(scene.addons(), tree, "{name}: folders untouched");
            let rerun = pin(&scene).unwrap();
            assert!(rerun.failures.is_empty(), "{name}: {:?}", rerun.failures);
            assert!(scene.record().addons["foo"].pinned, "{name}: converges");
            assert!(
                leftovers(&scene).is_empty(),
                "{name}: {:?}",
                leftovers(&scene)
            );
        }
        // The commit window's failpoints are on a records-only path too: before
        // `swapped` the error surfaces and nothing changes, after it the journal rolls
        // the edit forward in process.
        for name in [
            "engine.lock.acquired",
            "engine.revalidated",
            "engine.journal.prepared",
            "engine.snapshot.sv",
            "engine.snapshot.state",
            "engine.swap.after_exchange",
        ] {
            assert!(failed.contains(&name), "{name} never failed: {failed:?}");
            assert!(!rolled_forward.contains(&name), "{name}");
        }
        for name in [
            "engine.swapped",
            "engine.record.written",
            "engine.history.appended",
            "engine.committed",
        ] {
            assert!(
                rolled_forward.contains(&name),
                "{name} did not roll forward: {rolled_forward:?}"
            );
            // The transaction stands, so the command reports it, not a failure.
            assert!(!failed.contains(&name), "{name} failed: {failed:?}");
        }
    }

    /// `--force` over an unmanaged folder: the snapshot holds its only copy, which the
    /// commit says once, with the configured retention, and `-v` says where.
    #[test]
    fn a_forced_replace_of_an_unmanaged_folder_says_its_snapshot_holds_the_only_copy() {
        let mut scene = TxScene::new();
        scene.force = true;
        scene.retention.tx.keep_last = 4;
        let hand = scene.client.addons().join("Hand");
        std::fs::create_dir_all(&hand).unwrap();
        std::fs::write(hand.join("Hand.toc"), "## Interface: 120105\n").unwrap();
        std::fs::write(hand.join("Hand.lua"), "-- mine\n").unwrap();
        let zip = scene.zip(
            "hand.zip",
            &[("Hand/Hand.toc", "## Interface: 120105\n## Version: 2\n")],
        );
        let done = scene.run(vec![scene.install("gh:o/hand", &zip)]).unwrap();
        let outcome = &done.outcomes[0];
        let tx = outcome.tx.unwrap();
        let only: Vec<&Warning> = outcome
            .warnings
            .iter()
            .filter(|w| matches!(w.code, crate::error::WarningCode::SnapshotOnlyCopy))
            .collect();
        assert_eq!(only.len(), 1, "{:?}", outcome.warnings);
        assert!(
            only[0]
                .message
                .contains(&format!("{tx}'s snapshot holds the only copy of Hand:")),
            "{}",
            only[0].message
        );
        assert!(
            only[0].message.contains("pruned once 4 newer transactions"),
            "{}",
            only[0].message
        );
        let kept = scene.client.snapshots(&tx).join("AddOns").join("Hand");
        assert_eq!(
            std::fs::read_to_string(kept.join("Hand.lua")).unwrap(),
            "-- mine\n"
        );
        let note = format!("the only copy of Hand is kept in {}", kept.display());
        assert!(outcome.notes.contains(&note), "{:?}", outcome.notes);
    }

    // ---- The approval checked again under the lock (M4-D60) --------------------------

    /// A binding that accepts its preview's plan and records each call: `witness`
    /// answers `witness_cost` (or refuses), `operations` keeps both sides and refuses
    /// when told to, or when `strict` and they differ.
    #[derive(Default)]
    struct Recording {
        calls: std::sync::Mutex<Vec<&'static str>>,
        witness_cost: Option<RecheckCost>,
        refuse_witness: bool,
        refuse_operations: bool,
        strict: bool,
        seen: std::sync::Mutex<Vec<(Operations, Operations)>>,
        /// A folder `witness` replaces with an identical copy, after the identities the
        /// renames require were taken.
        replace_in_witness: Option<PathBuf>,
    }

    impl Recording {
        fn call(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn seen(&self) -> Vec<(Operations, Operations)> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn refused_by(check: &str) -> Error {
        Error::plan_stale(
            crate::error::PlanStaleSource::Apply,
            &[crate::error::StalePart::Observed],
            format!("refused by the test binding's {check}"),
            Suggestion::text("preview again"),
        )
    }

    impl Binding for Recording {
        fn plan(&self, _plan: &Plan) -> Result<()> {
            self.call("plan");
            Ok(())
        }

        fn undo(&self, _planned: &super::super::undo::UndoOutcome) -> Result<()> {
            Ok(())
        }

        fn moved(&self, _target: &str) -> Error {
            self.call("moved");
            refused_by("moved")
        }

        fn witness(&self, _at: &UnderLock<'_>) -> Result<Option<RecheckCost>> {
            self.call("witness");
            if let Some(dir) = &self.replace_in_witness {
                replace_with_copy(dir);
            }
            if self.refuse_witness {
                return Err(refused_by("witness"));
            }
            Ok(self.witness_cost)
        }

        fn operations(
            &self,
            _at: &UnderLock<'_>,
            approved: &Operations,
            rejudged: &Operations,
        ) -> Result<()> {
            self.call("operations");
            self.seen
                .lock()
                .unwrap()
                .push((approved.clone(), rejudged.clone()));
            if self.refuse_operations || (self.strict && approved != rejudged) {
                return Err(refused_by("operations"));
            }
            Ok(())
        }

        fn operation_refused(&self, _target: &str, _why: &str) -> Error {
            self.call("operation_refused");
            refused_by("operation_refused")
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// Replaces the folder at `dir` with an identical copy: only its identity changes.
    fn replace_with_copy(dir: &Path) {
        let copy = dir.with_extension("copy");
        copy_tree(dir, &copy);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::rename(&copy, dir).unwrap();
    }

    /// `changes` run for real, bound to `binding`.
    fn bound(
        scene: &TxScene,
        changes: Vec<RequestedChange>,
        kind: Option<TxKind>,
        default_kind: TxKind,
        binding: &dyn Binding,
    ) -> TxOutcomes {
        scene
            .run_bound(changes, kind, default_kind, false, None, Some(binding))
            .unwrap()
    }

    /// The scene, its changes, the kind override and the command's default kind.
    type Shape = (TxScene, Vec<RequestedChange>, Option<TxKind>, TxKind);

    fn install_v1() -> Shape {
        let scene = TxScene::new();
        let v1 = scene.zip("v1.zip", V1);
        let changes = vec![scene.install("gh:o/foo", &v1)];
        (scene, changes, None, TxKind::Install)
    }

    fn update_to_v2() -> Shape {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        let changes = vec![scene.install("gh:o/foo", &v2)];
        (scene, changes, None, TxKind::Install)
    }

    /// A at v1 (A and Old), then A at v2 (A alone) with B (B and Old): Old is handed
    /// from A to B in the one transaction.
    fn update_with_a_handover() -> Shape {
        let scene = TxScene::new();
        let toc = |name: &str, v: &str| {
            (
                name.to_owned(),
                format!("## Interface: 120105\n## Version: {v}\n"),
            )
        };
        let zip = |scene: &TxScene, file: &str, entries: &[(String, String)]| {
            let entries: Vec<(&str, &str)> = entries
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str()))
                .collect();
            scene.zip(file, &entries)
        };
        let a1 = zip(
            &scene,
            "a1.zip",
            &[toc("A/A.toc", "1"), toc("Old/Old.toc", "1")],
        );
        let done = scene.run(vec![scene.install("gh:o/a", &a1)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let a2 = zip(&scene, "a2.zip", &[toc("A/A.toc", "2")]);
        let b = zip(
            &scene,
            "b.zip",
            &[toc("B/B.toc", "1"), toc("Old/Old.toc", "2")],
        );
        let changes = vec![scene.install("gh:o/a", &a2), scene.install("gh:o/b", &b)];
        (scene, changes, None, TxKind::Install)
    }

    fn install_with_an_unmanaged_dependency() -> Shape {
        let scene = TxScene::new();
        crate::engine::testing::folder(&scene.client.addons().join("LibFoo"), "lib");
        let zip = scene.zip(
            "foo.zip",
            &[(
                "Foo/Foo.toc",
                "## Interface: 120105\n## Dependencies: LibFoo\n",
            )],
        );
        let changes = vec![scene.install("gh:o/foo", &zip)];
        (scene, changes, None, TxKind::Install)
    }

    fn remove_foo() -> Shape {
        let scene = with_v1();
        let changes = vec![scene.remove("Foo")];
        (scene, changes, None, TxKind::Remove)
    }

    fn forget_foo() -> Shape {
        let scene = with_v1();
        for folder in ["Foo", "FooOld"] {
            std::fs::remove_dir_all(scene.client.addons().join(folder)).unwrap();
        }
        let changes = vec![scene.remove("Foo")];
        (scene, changes, None, TxKind::Remove)
    }

    fn adopt_baz() -> Shape {
        let scene = with_v1();
        crate::engine::testing::folder(&scene.client.addons().join("Baz"), "mine");
        let changes = vec![scene.adopt("Baz")];
        (scene, changes, Some(TxKind::Adopt), TxKind::Install)
    }

    fn pin_foo() -> Shape {
        let scene = with_v1();
        let changes = vec![scene.edit("Foo", RecordEdit::SetPinned(true))];
        (scene, changes, None, TxKind::Install)
    }

    fn unpin_foo() -> Shape {
        let scene = with_v1();
        let done = pin(&scene).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let changes = vec![scene.edit("Foo", RecordEdit::SetPinned(false))];
        (scene, changes, None, TxKind::Install)
    }

    fn first_install_creating_addons() -> Shape {
        let (scene, changes, kind, default_kind) = install_v1();
        std::fs::remove_dir(scene.client.addons()).unwrap();
        (scene, changes, kind, default_kind)
    }

    fn force_over_an_unmanaged_folder() -> Shape {
        let (mut scene, changes, kind, default_kind) = install_v1();
        scene.force = true;
        crate::engine::testing::folder(&scene.client.addons().join("Foo"), "by hand");
        (scene, changes, kind, default_kind)
    }

    fn sync_prune() -> Shape {
        let scene = with_v1();
        let changes = vec![scene.remove("Foo")];
        (scene, changes, Some(TxKind::Sync), TxKind::Remove)
    }

    #[test]
    fn an_unchanged_world_rejudges_to_the_approved_operations() {
        type Build = fn() -> Shape;
        let shapes: [(&str, Build); 12] = [
            ("install_new", install_v1),
            ("update", update_to_v2),
            ("update_with_a_handover", update_with_a_handover),
            (
                "install_with_an_unmanaged_dependency",
                install_with_an_unmanaged_dependency,
            ),
            ("remove", remove_foo),
            ("forget", forget_foo),
            ("adopt", adopt_baz),
            ("pin", pin_foo),
            ("unpin", unpin_foo),
            (
                "first_install_creating_addons",
                first_install_creating_addons,
            ),
            (
                "force_over_an_unmanaged_folder",
                force_over_an_unmanaged_folder,
            ),
            ("sync_prune", sync_prune),
        ];
        for (case, shape) in shapes {
            let (scene, changes, kind, default_kind) = shape();
            let binding = Recording::default();
            let done = bound(&scene, changes, kind, default_kind, &binding);
            assert!(done.failures.is_empty(), "{case}: {:?}", done.failures);
            assert!(done.outcomes[0].tx.is_some(), "{case}: committed");
            let seen = binding.seen();
            assert_eq!(seen.len(), 1, "{case}: one target, one comparison");
            let (approved, rejudged) = &seen[0];
            assert!(!approved.changes.is_empty(), "{case}: {approved:#?}");
            assert_eq!(
                approved, rejudged,
                "{case}: approved {approved:#?}\nre-judged {rejudged:#?}"
            );
        }
    }

    #[test]
    fn the_bound_checks_run_after_the_generation_and_around_the_rejudge() {
        let (scene, changes, kind, default_kind) = update_to_v2();
        let binding = Recording::default();
        bound(&scene, changes, kind, default_kind, &binding);
        assert_eq!(binding.calls(), ["plan", "witness", "operations"]);

        // The record's generation moved between planning and the lock: `moved`, and
        // neither check.
        let (scene, changes, kind, default_kind) = update_to_v2();
        let binding = Recording::default();
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || {
                    bound(&scene, changes, kind, default_kind, &binding)
                })
            });
            pause.reached("engine.plan.done");
            let pinned = pin(&scene).unwrap();
            assert!(pinned.failures.is_empty(), "{:?}", pinned.failures);
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap();
            let refused = slow.failures[0].cause.as_ref().unwrap();
            assert!(refused.message.contains("moved"), "{refused:?}");
        });
        assert_eq!(binding.calls(), ["plan", "moved"]);
    }

    /// What a refused bound run must leave as it found.
    fn untouched(scene: &TxScene) -> (Vec<TreeEntry>, serde_json::Value, serde_json::Value, usize) {
        (
            scene.addons(),
            serde_json::to_value(scene.record()).unwrap(),
            serde_json::to_value(scene.index()).unwrap(),
            history(scene).len(),
        )
    }

    #[test]
    fn a_refusal_under_the_lock_moves_nothing() {
        let witness = Recording {
            refuse_witness: true,
            ..Recording::default()
        };
        let operations = Recording {
            refuse_operations: true,
            ..Recording::default()
        };
        for (case, binding, calls) in [
            ("witness", &witness, &["plan", "witness"][..]),
            (
                "operations",
                &operations,
                &["plan", "witness", "operations"][..],
            ),
        ] {
            let (scene, changes, kind, default_kind) = update_to_v2();
            let before = untouched(&scene);
            let done = bound(&scene, changes, kind, default_kind, binding);
            let refused = done.failures[0].cause.as_ref().unwrap();
            assert_eq!(
                refused.message,
                refused_by(case).message,
                "{case}: the binding's own refusal"
            );
            assert_eq!(done.outcomes.len(), 0, "{case}: {:?}", done.outcomes);
            // ubs:ignore — comparing trees and records, not secrets
            assert!(untouched(&scene) == before, "{case}: something moved");
            assert_eq!(leftovers(&scene), Vec::<String>::new(), "{case}");
            assert_eq!(binding.calls(), calls, "{case}");
        }
    }

    #[test]
    fn a_bound_run_left_with_nothing_to_do_is_refused_not_unchanged() {
        let (scene, changes, kind, default_kind) = forget_foo();
        let binding = Recording {
            strict: true,
            ..Recording::default()
        };
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), "engine.plan.done=pause", || {
                    bound(&scene, changes, kind, default_kind, &binding)
                })
            });
            pause.reached("engine.plan.done");
            crate::engine::testing::folder(&scene.client.addons().join("FooOld"), "back");
            pause.resume("engine.plan.done");
            let slow = slow.join().unwrap();
            let refused = slow.failures[0].cause.as_ref().unwrap();
            assert_eq!(refused.message, refused_by("operations").message);
        });
        let seen = binding.seen();
        let (approved, rejudged) = &seen[0];
        assert_eq!(approved.changes.len(), 1, "{approved:#?}");
        assert!(
            rejudged.ops.is_empty() && rejudged.changes.is_empty(),
            "{rejudged:#?}"
        );
        assert!(
            scene.record().addons.contains_key("foo"),
            "nothing forgotten"
        );
    }

    fn checked_notes(outcome: &TxOutcome) -> Vec<&String> {
        outcome
            .notes
            .iter()
            .filter(|n| n.contains("approval checked under the lock"))
            .collect()
    }

    #[test]
    fn an_unbound_commit_calls_no_check() {
        let (scene, changes, kind, default_kind) = update_to_v2();
        let done = scene.run_as(changes, kind, default_kind).unwrap();
        assert!(done.outcomes[0].tx.is_some());
        assert!(checked_notes(&done.outcomes[0]).is_empty());
    }

    #[test]
    fn m3_bindings_accept_by_default() {
        let (scene, changes, kind, default_kind) = update_to_v2();
        let binding = Counting {
            refuse: false,
            asked: std::cell::Cell::new(0),
        };
        let done = bound(&scene, changes, kind, default_kind, &binding);
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        assert!(done.outcomes[0].tx.is_some(), "committed");
        assert!(
            checked_notes(&done.outcomes[0]).is_empty(),
            "no check, no note"
        );
    }

    #[test]
    fn a_checked_window_notes_its_cost() {
        let (scene, changes, kind, default_kind) = update_to_v2();
        let cost = RecheckCost {
            folders: 2,
            entries_stamped: 13,
            folders_rehashed: 1,
            files_hashed: 4,
            bytes_hashed: 512,
            elapsed_us: 77,
        };
        let binding = Recording {
            witness_cost: Some(cost),
            ..Recording::default()
        };
        let done = bound(&scene, changes, kind, default_kind, &binding);
        let notes = checked_notes(&done.outcomes[0]);
        assert_eq!(notes.len(), 1, "{:?}", done.outcomes[0].notes);
        let note = notes[0];
        assert!(
            note.starts_with("retail: approval checked under the lock in "),
            "{note}"
        );
        let numbers: Vec<u64> = note
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(numbers, [77, 2, 13, 1, 4, 512], "{note}");
    }

    // ---- Each rename requires what the checks saw (M4-D60 check 3) -------------------

    #[test]
    fn identities_are_captured_before_the_witness_check() {
        // The witness check passes (the binding's replaces Foo with identical bytes and
        // accepts), and Foo's exchange refuses: the identities were taken before it.
        let (scene, changes, kind, default_kind) = update_to_v2();
        let foo = scene.client.addons().join("Foo");
        let tree = scene.addons();
        let record = serde_json::to_value(scene.record()).unwrap();
        let binding = Recording {
            replace_in_witness: Some(foo),
            ..Recording::default()
        };
        let done = bound(&scene, changes, kind, default_kind, &binding);
        let refused = done.failures[0].cause.as_ref().unwrap();
        assert_eq!(
            refused.message,
            refused_by("operation_refused").message,
            "a bound run's refused rename is its binding's refusal"
        );
        assert_eq!(
            binding.calls(),
            ["plan", "witness", "operations", "operation_refused"]
        );
        assert!(
            scene.addons() == tree,
            "rolled back: every folder as it was"
        );
        assert_eq!(serde_json::to_value(scene.record()).unwrap(), record);
        assert_eq!(leftovers(&scene), Vec::<String>::new());
    }

    /// `changes` run unbound, paused once everything was re-judged (after the identities
    /// were taken), while `meanwhile` runs.
    fn paused_after_revalidation(
        scene: &TxScene,
        changes: Vec<RequestedChange>,
        spec: &str,
        binding: Option<&Recording>,
        meanwhile: impl FnOnce(),
    ) -> TxOutcomes {
        let pause = Pause::new();
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec_in(pause.dir(), spec, || {
                    let binding = binding.map(|b| b as &dyn Binding);
                    scene
                        .run_bound(changes, None, TxKind::Install, false, None, binding)
                        .unwrap()
                })
            });
            pause.reached("engine.revalidated");
            meanwhile();
            pause.resume("engine.revalidated");
            slow.join().unwrap()
        })
    }

    #[test]
    fn an_unbound_runs_refused_rename_stays_retryable() {
        let (scene, changes, _, _) = update_to_v2();
        let foo = scene.client.addons().join("Foo");
        let tree = scene.addons();
        let done =
            paused_after_revalidation(&scene, changes, "engine.revalidated=pause", None, || {
                replace_with_copy(&foo)
            });
        let refused = done.failures[0].cause.as_ref().unwrap();
        assert_eq!(
            (refused.code, refused.retryable),
            (ErrorCode::PlanStale, true),
            "{refused:?}"
        );
        assert!(
            refused
                .message
                .contains("changed after the approval was checked"),
            "{refused:?}"
        );
        assert!(scene.addons() == tree, "rolled back");
    }

    #[test]
    fn a_refused_rename_whose_rollback_failed_stays_a_recovery_failure() {
        let (scene, changes, _, _) = update_to_v2();
        let foo = scene.client.addons().join("Foo");
        let binding = Recording::default();
        let done = paused_after_revalidation(
            &scene,
            changes,
            "engine.revalidated=pause,engine.recover.step=error",
            Some(&binding),
            || replace_with_copy(&foo),
        );
        let failed = ErrorBody::from(done.failures[0].cause.clone().unwrap());
        assert!(
            failed.details.get("journal_pending").is_some(),
            "the journal is kept for the next change: {failed:?}"
        );
        assert!(
            !binding.calls().contains(&"operation_refused"),
            "not the binding's refusal: {:?}",
            binding.calls()
        );
    }
}
