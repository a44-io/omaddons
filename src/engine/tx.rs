//! The transaction driver (vision §5.5 steps 5–9): the only code that holds the global
//! lock for a mutation, and only for revalidate → journal → swap → commit (D24).
//!
//! Plan (no lock, `plan_tx`'s in-flight lock protects its downloads), then per target,
//! in key order, one transaction with its own id and in-flight lock: stage (no lock);
//! lock; recover leftovers; register the target if new; re-check every planned addon
//! offline against the current record, inventory and running state (a changed
//! generation that now conflicts is `plan_stale`); journal; snapshot SavedVariables
//! and state; swap; write the record (`generation + 1`), the files index and history;
//! mark committed; unlock. Then `status.json`, and cleanup of the journal, stage dir,
//! temp downloads and in-flight lock. An error after `prepared` is recovered in
//! process by the journal: before `swapped` it rolls back, after it rolls forward.
//! One target's failure is a per-item failure; committed targets stay committed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::blobs::BlobCache;
use super::inspect::ArchiveLimits;
use super::journal::{
    self, CommitPayload, IndexChanges, Journal, Prepared, RecoveryAction, recover_all, recover_tx,
};
use super::plan::{
    self, AddonWork, Change, Plan, PlanCtx, PlanFailure, PlanRequest, RequestedChange, TargetPlan,
    TargetRequest, one_addons_dir, rejudge, target_kind,
};
use super::stage::{Staged, stage};
use super::swap::{SwapOp, snapshot_saved_variables, snapshot_state, swap};
use crate::discover::registry::LockedRegistry;
use crate::discover::running::detect_running;
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorBody, PlanStaleSource, Result, Suggestion, Warning};
use crate::fsx::{
    self, GlobalLock, GlobalLockGuard, InflightLock, LockUse, PruneReason, ScopeTable,
    SnapshotRoots, WriteSession, Writer, ensure_dir_all, failpoint,
};
use crate::model::{
    Actor, AddonState, FolderName, ReloadRequired, Target, TargetKey, TxId, TxKind,
};
use crate::net::Http;
use crate::rules::RulesTable;
use crate::sources::SourceRegistry;
use crate::state::config::RunningPolicy;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryOp, TxResult};
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
}

/// One target and its changes; unregistered targets are registered at commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxTarget {
    pub discovered: DiscoveredTarget,
    pub changes: Vec<RequestedChange>,
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

/// Runs a mutating request to the end.
pub fn run(req: &TxRequest, ctx: &EngineCtx<'_>) -> Result<TxOutcomes> {
    let client_dirs: Vec<PathBuf> = req
        .targets
        .iter()
        .map(|t| t.discovered.target.client_dir.clone())
        .collect();
    let scopes = ScopeTable::new(ctx.paths, &client_dirs, None)?;
    let blobs = BlobCache::new(ctx.paths);
    let plan_tx = TxId::new();
    let plan_inflight = InflightLock::create(ctx.paths, &plan_tx)?;
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
            })
            .collect(),
        force: req.force,
        while_running: req.while_running,
        dry_run: req.dry_run,
    };
    let plan_ctx = PlanCtx {
        paths: ctx.paths,
        scopes: &scopes,
        http: ctx.http,
        registry: ctx.registry,
        blobs: &blobs,
        table: ctx.table,
        tx: plan_tx,
        running: &running,
        policy: ctx.policy,
        limits: ctx.limits,
        actor: ctx.actor,
        argv: ctx.argv,
    };
    let planned = plan::plan(&plan_req, &plan_ctx);
    let plan = match planned {
        Ok(plan) => plan,
        Err(error) => {
            cleanup(ctx.paths, &scopes, plan_inflight, &plan_tx, None);
            return Err(error);
        }
    };

    let mut outcomes = Vec::new();
    let mut failures = plan.failures.clone();
    let mut warnings = Vec::new();
    if req.dry_run || !plan.has_ops() {
        outcomes = plan
            .targets
            .iter()
            .map(|tp| unchanged(tp, req.dry_run))
            .collect();
    } else {
        let mut order: Vec<usize> = (0..plan.targets.len()).collect();
        order.sort_by_key(|i| {
            (
                plan.targets[*i].key.is_none(),
                plan.targets[*i].key,
                plan.targets[*i].target.clone(),
            )
        });
        for i in order {
            let tp = &plan.targets[i];
            let Some(tt) = req.targets.get(i) else {
                continue;
            };
            if tp.work.is_empty() {
                outcomes.push(unchanged(tp, false));
                continue;
            }
            match commit_target(req, ctx, &scopes, &plan_req, &blobs, &tt.discovered, tp) {
                Ok((outcome, recovered)) => {
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
    cleanup(ctx.paths, &scopes, plan_inflight, &plan_tx, None);
    Ok(TxOutcomes {
        plan,
        outcomes,
        failures,
        warnings,
    })
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
    }
}

/// The client dirs whose game runs now.
pub(super) fn running_now(ctx: &EngineCtx<'_>, targets: &[Target]) -> BTreeSet<PathBuf> {
    match ctx.running {
        Some(running) => running.clone(),
        None => detect_running(targets).into_keys().collect(),
    }
}

/// One target's transaction, from staging to cleanup.
fn commit_target(
    req: &TxRequest,
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    plan_req: &PlanRequest,
    blobs: &BlobCache,
    discovered: &DiscoveredTarget,
    tp: &TargetPlan,
) -> Result<(TxOutcome, Vec<Warning>)> {
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let target = &discovered.target;
    let staged = match stage_all(&inflight, scopes, target, &tp.work) {
        Ok(staged) => staged,
        Err(error) => {
            cleanup(
                ctx.paths,
                scopes,
                inflight,
                &tx,
                Some(&ClientPaths::new(&target.client_dir).stage(&tx)),
            );
            return Err(error);
        }
    };
    let result = locked(
        req, ctx, scopes, plan_req, blobs, discovered, tp, &tx, &staged,
    );
    cleanup(ctx.paths, scopes, inflight, &tx, Some(&staged.stage_dir));
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
        if let Some(inspected) = &w.inspected {
            let staged = stage(inflight, scopes, target, w.blob.as_deref(), inspected)?;
            merged.folders.extend(staged.folders);
        }
    }
    Ok(merged)
}

/// The commit window, under the global lock.
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
    let guard = GlobalLock::acquire(paths, ctx.command)?;
    failpoint::hit("engine.lock.acquired").map_err(fail)?;
    let recovered = recover_all(&guard, paths, scopes)?;
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
    let plan_ctx = PlanCtx {
        paths,
        scopes,
        http: ctx.http,
        registry: ctx.registry,
        blobs,
        table: ctx.table,
        tx: *tx,
        running: &running,
        policy: ctx.policy,
        limits: ctx.limits,
        actor: ctx.actor,
        argv: ctx.argv,
    };
    let mut works = Vec::new();
    let mut changes = Vec::new();
    let mut reload_required = ReloadRequired::None;
    let mut warnings = Vec::new();
    for planned in &tp.work {
        match rejudge(plan_req, &plan_ctx, &target, planned) {
            Ok(Some(p)) => {
                reload_required = reload_required.max(p.reload_required);
                for w in p.warnings {
                    if !warnings.contains(&w) {
                        warnings.push(w);
                    }
                }
                changes.push(p.change);
                works.push(p.work);
            }
            Ok(None) => {}
            Err(error) if changed => return Err(stale(&error)),
            Err(error) => return Err(error),
        }
    }
    failpoint::hit("engine.revalidated").map_err(fail)?;
    let kind = target_kind(&changes);
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
            },
            recovered,
        ));
    };

    let now = Timestamp::now();
    let record = next_record(&current.value, &works, tx, now);
    let swap_ops: Vec<SwapOp> = works.iter().flat_map(|w| w.swap.clone()).collect();
    let index_changes = index_changes(&swap_ops, staged);
    // A client that has never been launched has no AddOns folder yet. The transaction
    // creates it once `prepared` records that it will; a case variant already there
    // (`interface/addons`) is used instead.
    let client = ClientPaths::new(&target.client_dir);
    one_addons_dir(&client)?;
    let addons = client.addons();
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
    let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;

    let committed = (|| -> Result<(bool, Vec<Warning>)> {
        failpoint::hit("engine.journal.prepared").map_err(fail)?;
        if creates_addons_dir {
            ensure_dir_all(&mut s, &addons)?;
            failpoint::hit("engine.addons_dir.created").map_err(fail)?;
        }
        let touched: Vec<FolderName> = swap_ops.iter().map(touched_folder).collect();
        snapshot_saved_variables(&mut s, tx, &client, &touched)?;
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
        Err(error) => match recover_tx(&guard, paths, scopes, tx)? {
            // Swapped already: the payload completed it, so the transaction stands.
            Some(RecoveryAction::RolledForward) => (true, Vec::new()),
            _ => return Err(error),
        },
    };
    warnings.extend(swap_warnings);
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

/// The record after `works`: changed addons in, removed ones out, transferred folders
/// taken from and degraded on their previous owners, generation + 1.
fn next_record(
    current: &TargetRecord,
    works: &[AddonWork],
    tx: &TxId,
    now: Timestamp,
) -> TargetRecord {
    let mut record = current.clone();
    for w in works {
        match &w.record {
            Some(new) => {
                let mut new = new.clone();
                new.tx_id = *tx;
                new.installed_at = now;
                record
                    .addons
                    .insert(TargetRecord::addon_key(&new.name), new);
            }
            None => {
                record.addons.remove(&TargetRecord::addon_key(&w.addon));
            }
        }
        for t in &w.transfers {
            if let Some(previous) = record.addons.get_mut(&TargetRecord::addon_key(&t.from)) {
                previous.folders.retain(|f| f != &t.folder);
            }
        }
        for key in &w.degraded {
            if let Some(previous) = record.addons.get_mut(key) {
                previous.state = AddonState::Degraded;
            }
        }
    }
    record.bump();
    record
}

/// Files index changes: staged indexes under their on-disk spelling in, removed out.
fn index_changes(ops: &[SwapOp], staged: &Staged) -> IndexChanges {
    let mut changes = IndexChanges::default();
    let staged_index = |name: &FolderName| {
        staged
            .folders
            .iter()
            .find(|f| f.name == *name)
            .map(|f| f.index.clone())
    };
    for op in ops {
        match op {
            SwapOp::Add { name } => changes.upsert.extend(staged_index(name)),
            SwapOp::Replace { name, live, .. } => {
                if let Some(index) = staged_index(name) {
                    changes
                        .upsert
                        .push(FolderIndex::new(live.as_str(), index.files));
                }
            }
            SwapOp::Remove { live, .. } => changes.remove.push(live.key().to_owned()),
        }
    }
    changes
}

fn history_ops(works: &[AddonWork]) -> Vec<HistoryOp> {
    works
        .iter()
        .flat_map(|w| {
            w.swap.iter().map(move |op| {
                let (kind, folder) = match op {
                    SwapOp::Add { name } => (FolderOp::Add, name.clone()),
                    SwapOp::Replace { live, .. } => (FolderOp::Replace, live.clone()),
                    SwapOp::Remove { live, .. } => (FolderOp::Remove, live.clone()),
                };
                HistoryOp {
                    op: kind,
                    folder,
                    addon: w.addon.clone(),
                }
            })
        })
        .collect()
}

fn touched_folder(op: &SwapOp) -> FolderName {
    match op {
        SwapOp::Add { name } => name.clone(),
        SwapOp::Replace { live, .. } | SwapOp::Remove { live, .. } => live.clone(),
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

/// Prunes what the transaction left (its journal, stage dir, temp downloads and lock
/// file) under its own in-flight lock, then drops the lock. Best effort: recovery and
/// retention sweep anything left.
pub(super) fn cleanup(
    paths: &Paths,
    scopes: &ScopeTable,
    inflight: InflightLock,
    tx: &TxId,
    stage_dir: Option<&PathBuf>,
) {
    let mut victims = vec![
        paths.journal(tx),
        paths.inflight_lock(tx),
        paths.cache_tmp(tx),
    ];
    victims.extend(stage_dir.cloned());
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

    fn leftovers(scene: &TxScene) -> Vec<String> {
        let mut found = Vec::new();
        for dir in [
            scene.paths.journal_dir(),
            scene.paths.inflight_dir(),
            scene.client.meta_dir().join("stage"),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(listing.map(|e| e.unwrap().path().display().to_string()));
            }
        }
        found
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
    fn updated_tree() -> TreeDigest {
        let scene = with_v1();
        let v2 = scene.zip("v2.zip", V2);
        scene.run(vec![scene.install("gh:o/foo", &v2)]).unwrap();
        scene.addons()
    }

    use crate::fsx::TreeDigest;

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
                "engine.plan.downloaded" | "engine.recover.step" | "engine.addons_dir.created"
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
        }
    }

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
}
