//! The transaction driver (vision §5.5 steps 5–9): the only code that holds the global
//! lock for a mutation, and only for revalidate → journal → swap → commit (D24).
//!
//! Plan (no lock, `plan_tx`'s in-flight lock protects its downloads), then per target,
//! in key order, one transaction with its own id and in-flight lock: stage and clone
//! the planned folders' SavedVariables (no lock); lock; recover leftovers; register the
//! target if new; re-check every planned addon offline against the current record,
//! inventory and running state (a changed generation that now conflicts is
//! `plan_stale`); journal; finish the SavedVariables snapshot (a stat per unchanged
//! file) and snapshot state; swap; write the record (`generation + 1`), the files index
//! and history; mark committed; unlock. Then `status.json`, and cleanup of the journal,
//! stage dir, temp downloads and in-flight lock. An error after `prepared` is recovered
//! in process by the journal: before `swapped` it rolls back, after it rolls forward.
//! One target's failure is a per-item failure; committed targets stay committed.

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
    SvSkipped, SwapOp, finish_saved_variables, out_of_scope_warnings, predigest,
    prestage_saved_variables, snapshot_state, swap, with_predigests,
};
use crate::discover::registry::{LockedRegistry, same_dir};
use crate::discover::running::{RunningScan, detect_running, running_unknown};
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorBody, ErrorCode, PlanStaleSource, Result, Suggestion, Warning};
use crate::fsx::{
    self, GlobalLock, GlobalLockGuard, InflightLock, LockUse, PruneReason, ScopeTable,
    SnapshotRoots, WriteSession, Writer, ensure_dir_all, failpoint,
};
use crate::model::{
    Actor, AddonState, Channel, FolderName, MutableFolderName, Ref, ReloadRequired, RunId, Target,
    TargetKey, TxId, TxKind,
};
use crate::net::Http;
use crate::output::{Phase, ProgressSink};
use crate::resolve::cache::ReleaseCache;
use crate::resolve::catalog::Catalog;
use crate::rules::RulesTable;
use crate::sources::SourceRegistry;
use crate::state::config::RunningPolicy;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::history::{self, FolderOp, HistoryEdit, HistoryEntry, HistoryOp, TxResult};
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
    "engine.recover.step",
    "engine.recover.sv",
    "engine.recover.forward",
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
    let scopes = ScopeTable::new(ctx.paths, &client_dirs, None)?;
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
    let planned = plan::plan(&plan_req, &plan_ctx);
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
            match commit_target(
                req, ctx, &scopes, &plan_req, &blobs, discovered, tp, with_work,
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
    journal::recover_waiting(ctx.paths, ctx.command, &sv_writable)
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

/// The client dirs whose game runs now, or every target's when that cannot be told.
pub(super) fn running_now(ctx: &EngineCtx<'_>, targets: &[Target]) -> RunningNow {
    match ctx.running {
        Some(running) => RunningNow {
            dirs: running.clone(),
            unknown: None,
        },
        None => RunningNow::from_scan(detect_running(targets), targets),
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
    error.fix = Suggestion::command(["omaddons", "status"]);
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
    with_work: u64,
) -> Result<(TxOutcome, Vec<Warning>)> {
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let target = &discovered.target;
    let client = ClientPaths::new(&target.client_dir);
    let staged = match stage_all(&inflight, scopes, target, &tp.work) {
        Ok(staged) => {
            ctx.progress.event(Phase::Stage, with_work, &target.id);
            staged
        }
        Err(error) => {
            cleanup(ctx.paths, scopes, inflight, &tx, Some(&client), false);
            return Err(error);
        }
    };
    // The index stamps taken under the lock must postdate every staged mtime by the
    // racy slack.
    staged.wait_out_racy_slack();
    let result = locked(
        req, ctx, scopes, plan_req, blobs, discovered, tp, &tx, &staged,
    );
    let committed = result
        .as_ref()
        .is_ok_and(|(outcome, _)| outcome.tx.is_some());
    cleanup(ctx.paths, scopes, inflight, &tx, Some(&client), committed);
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

/// Retention after a commit (vision §5.5), best effort: its warnings join the outcome
/// and a failure becomes `retention_failed`, never a failed command.
pub(super) fn retain(
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
        let pins = super::retention::pinned_blobs(ctx.paths)?;
        let report = super::retention::run(
            ctx.paths,
            scopes,
            key,
            &ClientPaths::new(client_dir),
            &ctx.retention,
            Timestamp::now(),
            &pins,
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
    tx: &TxId,
    staged: &Staged,
) -> Result<(TxOutcome, Vec<Warning>)> {
    let paths = ctx.paths;
    let fail = |e: std::io::Error| {
        Error::io(
            paths.state_dir(),
            &e,
            Suggestion::command(["omaddons", "status"]),
        )
    };
    let actions = paths
        .state_snapshots_dir()
        .join(tx.to_string())
        .join("actions.jsonl");
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
    let planned_ops: Vec<SwapOp> = tp.work.iter().flat_map(|w| w.swap().to_vec()).collect();
    let planned_folders: Vec<FolderName> = planned_ops.iter().map(touched_folder).collect();
    let predigests = predigest(&client.addons(), &planned_ops);
    let prestage = prestage_saved_variables(&mut s, tx, &client, &planned_folders)?;
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

    // Re-check every addon offline, with running state as it is now.
    let current = load_record(paths, &key)?;
    // ubs:ignore — comparing record generations, not secrets
    let changed = current.value.generation != tp.generation;
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
    let mut works = Vec::new();
    let mut changes = Vec::new();
    let mut reload_required = ReloadRequired::None;
    // The addons planning left unchanged keep their warnings and notes; the work is
    // judged again.
    let mut warnings = tp.unchanged_warnings.clone();
    let mut notes = tp.notes.clone();
    // One fresh look at the target, and the planned works replayed over it in order:
    // releases first, then each claim (M2-D6).
    let state = TargetState::now(&plan_ctx, &target)?;
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
            &plan_ctx,
            &target,
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
        target: key,
        actor: ctx.actor,
        reason: req.reason.clone(),
        argv: ctx.argv.to_vec(),
        ops: history_ops(&works),
        result: TxResult::Committed,
        reverts: None,
        sv_deferred: false,
        created_addons_dir: creates_addons_dir,
        run: Some(req.run),
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
        sv_skipped = finish_saved_variables(&mut s, tx, &client, &touched, prestage)?.sv_skipped;
        snapshot_state(&mut s, paths, tx, &key)?;
        let report = swap(&mut s, &mut journal, &swap_ops, staged, &client)?;
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
        failpoint::hit("engine.history.appended").map_err(fail)?;
        journal.mark_committed(&mut s)?;
        failpoint::hit("engine.committed").map_err(fail)?;
        Ok((report.atomic_swap, report.warnings))
    })();
    let (atomic_swap, swap_warnings) = match committed {
        Ok(done) => done,
        Err(error) => match recover_tx(&recovery, tx) {
            // Swapped already: the payload completed it, so the transaction stands.
            Ok(Some(Recovered {
                action: RecoveryAction::RolledForward,
                atomic_swap,
                warnings,
                ..
            })) => super::swap::rolled_forward(atomic_swap, warnings),
            Ok(_) => return Err(error),
            Err(failed) => return Err(recovery_failed(error, tx, &failed)),
        },
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
        format!(
            "the target changed while this command planned: {}",
            cause.message
        ),
        Suggestion::text("run the command again; it re-plans against the current state"),
    )
    .details(serde_json::json!({ "cause": cause.code.wire(), "cause_message": cause.message }))
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
/// race only delays the widget's doorbell.
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
        victims.extend([paths.journal(tx), paths.inflight_lock(tx)]);
        victims.extend(client.map(|c| c.stage(tx)));
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
    if !committed && !paths.journal(tx).exists() {
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
    use crate::engine::testing::TxScene;
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
        let stage_root = scene.client.meta_dir().join("stage");
        std::thread::scope(|s| {
            let worker = s.spawn(|| {
                failpoint::with_spec("engine.stage.synced=sleep:600", || {
                    scene.run(vec![scene.install("gh:o/foo", &v2)])
                })
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::fs::read_dir(&stage_root).map_or(true, |mut d| d.next().is_none()) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "staging never started"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let lock =
                GlobalLock::acquire_with_timeout(&scene.paths, "other", Duration::from_millis(200));
            assert!(
                lock.is_ok(),
                "another process can take the lock during staging"
            );
            drop(lock);
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
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.stage.synced=sleep:400", || {
                    scene.run(vec![scene.install("gh:o/foo", &v1)])
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            let fast = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
            assert_eq!(fast.outcomes[0].kind, Some(TxKind::Install));
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
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.stage.synced=sleep:400", || {
                    scene.run(vec![scene.install("gh:o/bar", &mine)])
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            scene
                .run(vec![plan::RequestedChange::Remove {
                    addon: "Foo".into(),
                }])
                .unwrap();
            // An unmanaged folder now takes Bar's name.
            std::fs::create_dir_all(scene.client.addons().join("Bar")).unwrap();
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

    #[test]
    fn failpoints_list_every_engine_failpoint() {
        let mut found = BTreeSet::new();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            for (at, _) in code.match_indices("failpoint::hit(\"engine.") {
                let rest = &code[at + "failpoint::hit(\"".len()..];
                found.insert(rest.split('"').next().unwrap().to_owned());
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
        std::thread::scope(|s| {
            let slow =
                s.spawn(|| failpoint::with_spec("engine.plan.done=sleep:400", || pin(&scene)));
            std::thread::sleep(Duration::from_millis(150));
            scene.run(vec![scene.install("gh:o/bar", &bar)]).unwrap();
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
        let updated = std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.plan.done=sleep:400", || {
                    let drop = scene.edit("Foo", RecordEdit::RemoveRequiredBy("Bar".into()));
                    scene.run(vec![drop])
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
            let updated = scene.record().addons["foo"].release_id.clone();
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
        std::thread::scope(|s| {
            let slow =
                s.spawn(|| failpoint::with_spec("engine.plan.done=sleep:400", || pin(&scene)));
            std::thread::sleep(Duration::from_millis(150));
            scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
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
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.plan.done=sleep:400", || {
                    scene.run_as(vec![adopt.clone()], Some(TxKind::Adopt), TxKind::Install)
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            std::fs::write(baz.join("extra.lua"), "-- added after indexing").unwrap();
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
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.plan.done=sleep:400", || {
                    scene.run_as(vec![scene.remove("Foo")], None, TxKind::Remove)
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            crate::engine::testing::folder(&scene.client.addons().join("FooOld"), "back");
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
        std::thread::scope(|s| {
            let slow = s.spawn(|| {
                failpoint::with_spec("engine.stage.synced=sleep:400", || {
                    scene.run(vec![scene.install_pinned("gh:o/foo", &v1, "1")])
                })
            });
            std::thread::sleep(Duration::from_millis(150));
            let fast = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
            assert_eq!(fast.outcomes[0].kind, Some(TxKind::Install));
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
        ] {
            assert!(
                rolled_forward.contains(&name),
                "{name} did not roll forward: {rolled_forward:?}"
            );
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
}
