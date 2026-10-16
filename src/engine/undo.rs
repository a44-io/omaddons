//! Undo (vision §5.5 "Undo"): reverts a committed transaction from its snapshots, as a
//! transaction of its own (kind `undo`, `reverts: <tx>`), which can be undone in turn.
//!
//! Selection reads `history.jsonl`: the newest undoable transaction, the newest by one
//! actor, or one by id. A later transaction of any kind that touched one of its folders
//! (by key, on the same target) is an `undo_conflict`, so undoing the newest never
//! conflicts. Folders come back from `Interface/.omaddons/snapshots/<tx>/AddOns/`,
//! cloned into the undo's own stage dir first so that snapshot stays intact: an add is
//! removed, a replace swaps the old version back, a remove adds it back. The record and
//! the files index are restored per addon and per folder from
//! `<state>/omaddons/snapshots/<tx>/`, so later changes to other addons stand; the
//! generation still goes up.
//!
//! SavedVariables follow `--saved-variables`, per file of the transaction's manifest: an
//! unchanged file is left alone (the new code never ran); a changed one is restored
//! after the swap, the current one having been snapshotted first, and one the new code
//! created moves into the undo's snapshot. They are never written while the client runs:
//! `auto` defers them (`saved_variables_deferred`, finished later by
//! `undo <tx> --saved-variables restore`, SavedVariables only), `restore` refuses with
//! `game_running`, `keep` never touches them. The restore list rides in the journal
//! payload, so roll-forward completes it after a crash. Account, realm and character
//! names never reach the output: only counts do.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::edit::{EditField, changed_fields};
use super::journal::{
    self, CommitPayload, IndexChanges, Journal, Prepared, RecoveryAction, recover_all, recover_tx,
};
use super::plan::{Change, ChangeKind, FolderSummary};
use super::running::{self, PolicyContext, PolicyOp};
use super::stage::{Staged, stage_trees};
use super::swap::{
    SvEntry, SvManifest, SwapOp, sha256_file, snapshot_saved_variables, snapshot_state, swap,
};
use super::tx::{EngineCtx, TxOutcome, cleanup, millis, running_now, write_status};
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    GlobalLock, InflightLock, Op, OwnedFolders, RenameMode, ScopeTable, SnapshotRoots,
    WriteSession, Writer, ensure_dir_all, failpoint, mutate,
};
use crate::model::{Actor, FolderName, ReloadRequired, RunId, Target, TxId, TxKind};
use crate::state::files_index::FilesIndex;
use crate::state::history::{
    self, FolderOp, HistoryEdit, HistoryEntry, HistoryFilter, HistoryOp, TxResult,
};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::{TargetRecord, load_record};
use crate::state::store;
use crate::toc::{DiskFolder, loaded_toc};

/// Which transaction to revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoSelector {
    /// The newest undoable transaction.
    Latest,
    Tx(TxId),
    /// That actor's newest undoable transaction.
    LastBy(Actor),
}

/// `--saved-variables`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SvPolicy {
    /// Restore what changed; defer while the client runs.
    #[default]
    Auto,
    /// Restore what changed; refuse while the client runs.
    Restore,
    /// Never touch SavedVariables.
    Keep,
}

/// What `undo` is asked for.
#[derive(Debug, Clone)]
pub struct UndoRequest {
    pub selector: UndoSelector,
    pub sv_policy: SvPolicy,
    pub dry_run: bool,
    pub while_running: bool,
    /// `--reason`, recorded verbatim in history.
    pub reason: Option<String>,
    /// Every resolved target; the reverted transaction's own is acted on.
    pub targets: Vec<DiscoveredTarget>,
    /// The undo command's invocation, shared by the undo transactions of one run.
    pub run: RunId,
}

/// SavedVariables files of the reverted transaction, by what the undo does with them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SvCounts {
    pub restored: usize,
    /// Changed, but the client runs (`auto`).
    pub deferred: usize,
    pub unchanged: usize,
    /// Changed, and `--saved-variables keep`.
    pub kept: usize,
}

/// What an undo did (or, under `--dry-run`, would do).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UndoOutcome {
    /// The transaction reverted.
    pub undoes: TxId,
    #[serde(flatten)]
    pub outcome: TxOutcome,
    pub sv: SvCounts,
}

/// The SavedVariables an undo writes back after its swap (journal payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SvRestore {
    /// The reverted transaction, whose snapshot holds the files as they were.
    pub from: TxId,
    /// Each file to bring back to its state before `from` (`sha256: null`: absent).
    pub files: Vec<SvEntry>,
}

impl SvRestore {
    /// The folders whose SavedVariables these are, for the `Undo` writer's scope.
    pub fn owned_folders(&self) -> OwnedFolders {
        let folders: Vec<FolderName> = self
            .files
            .iter()
            .filter_map(|f| sv_folder(&f.path))
            .collect();
        OwnedFolders::new(&folders)
    }
}

/// The folder a SavedVariables path (`…/SavedVariables/<Folder>.lua[.bak]`) belongs to.
fn sv_folder(path: &str) -> Option<FolderName> {
    let name = path.rsplit('/').next()?;
    let lower = name.to_ascii_lowercase();
    let stem = [".lua.bak", ".lua"]
        .into_iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(str::len))?;
    FolderName::new(name.get(..stem)?).ok()
}

// ---------------------------------------------------------------------------------------
// The undo.

/// Reverts the selected transaction (vision §5.5): plan without the lock, stage the
/// snapshot's folders, then the commit window under the global lock, planned again.
pub fn undo(req: &UndoRequest, ctx: &EngineCtx<'_>) -> Result<UndoOutcome> {
    let entries = history::read(ctx.paths, &HistoryFilter::default())?.entries;
    let at = select(&entries, req.selector)?;
    let reverted = entries[at].clone();
    let discovered = req
        .targets
        .iter()
        .find(|d| d.target.key == Some(reverted.target))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::TargetNotFound,
                format!(
                    "transaction {} changed target {}, which is not on this machine now",
                    reverted.tx, reverted.target
                ),
                Suggestion::command(["omaddons", "targets"]),
            )
        })?;
    let target = &discovered.target;
    let running = running_now(ctx, std::slice::from_ref(target)).contains(&target.client_dir);
    let planned = plan(ctx, req, &entries, at, target, running)?;
    if req.dry_run {
        return Ok(planned.outcome(target, None, true, true, Vec::new(), None));
    }

    let scopes = ScopeTable::new(ctx.paths, std::slice::from_ref(&target.client_dir), None)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let staged = match stage_trees(&inflight, &scopes, target, &planned.folders.sources) {
        Ok(staged) => staged,
        Err(error) => {
            let stage_dir = ClientPaths::new(&target.client_dir).stage(&tx);
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&stage_dir));
            return Err(error);
        }
    };
    // The index stamps taken under the lock must postdate every staged mtime by the
    // racy slack.
    staged.wait_out_racy_slack();
    let result = commit(ctx, req, &scopes, discovered, reverted.tx, &tx, &staged);
    cleanup(ctx.paths, &scopes, inflight, &tx, Some(&staged.stage_dir));
    let mut done = result?;
    if let (Some(key), Some(_)) = (done.outcome.key, done.outcome.tx) {
        let retained = super::tx::retain(ctx, &scopes, key, &target.client_dir);
        done.outcome.warnings.extend(retained);
    }
    Ok(done)
}

/// The commit window, under the global lock: recover, plan again against the state
/// now, journal, snapshot, swap, restore SavedVariables, write state, commit.
fn commit(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    scopes: &ScopeTable,
    discovered: &DiscoveredTarget,
    reverts: TxId,
    tx: &TxId,
    staged: &Staged,
) -> Result<UndoOutcome> {
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

    let target = &discovered.target;
    let entries = history::read(paths, &HistoryFilter::default())?.entries;
    let at = select(&entries, UndoSelector::Tx(reverts))?;
    let running = running_now(ctx, std::slice::from_ref(target)).contains(&target.client_dir);
    let planned = plan(ctx, req, &entries, at, target, running)?;
    failpoint::hit("engine.revalidated").map_err(fail)?;

    let key = planned.key;
    let client = ClientPaths::new(&target.client_dir);
    let now = Timestamp::now();
    // The folders swapped back in are the staged copies (new mtimes): record those
    // entries, stamped now, rather than the reverted transaction's before entries.
    let index_changes = undo_index(
        &planned.index,
        &planned.folders.swap,
        staged,
        now.as_nanosecond(),
    );
    let history = HistoryEntry {
        schema_version: history::CURRENT,
        tx: *tx,
        kind: TxKind::Undo,
        at: now,
        target: key,
        actor: ctx.actor,
        reason: req.reason.clone(),
        argv: ctx.argv.to_vec(),
        ops: planned.folders.history.clone(),
        result: TxResult::Committed,
        reverts: Some(reverts),
        sv_deferred: planned.sv.counts.deferred > 0,
        created_addons_dir: false,
        run: Some(req.run),
    };
    let sv_restore = (!planned.sv.restore.is_empty()).then(|| SvRestore {
        from: reverts,
        files: planned.sv.restore.clone(),
    });
    let prepared = Prepared {
        schema_version: journal::CURRENT,
        tx: *tx,
        target_key: key,
        client_dir: target.client_dir.clone(),
        kind: TxKind::Undo,
        started_at: now,
        creates_dirs: Vec::new(),
        commit: CommitPayload {
            record: planned.record.clone(),
            index: index_changes.clone(),
            history: history.clone(),
            stage_dir: staged.stage_dir.clone(),
            sv_restore: sv_restore.clone(),
        },
    };
    let touched = planned.folders.touched.clone();
    let mut owned: Vec<FolderName> = touched.clone();
    if let Some(sv) = &sv_restore {
        owned.extend(sv.files.iter().filter_map(|f| sv_folder(&f.path)));
    }
    let actions = paths.state_snapshot(tx).join("actions.jsonl");
    let mut s = WriteSession::tx(
        *tx,
        Writer::Undo,
        scopes,
        SnapshotRoots::for_tx(paths, tx),
        actions,
    )
    .with_owned_folders(OwnedFolders::new(&owned));
    let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;

    let committed = (|| -> Result<(bool, Vec<Warning>)> {
        failpoint::hit("engine.journal.prepared").map_err(fail)?;
        snapshot_saved_variables(&mut s, tx, &client, &touched)?;
        snapshot_state(&mut s, paths, tx, &key)?;
        let report = swap(&mut s, &mut journal, &planned.folders.swap, staged, &client)?;
        if let Some(sv) = &sv_restore {
            restore_saved_variables(&mut s, &client, sv, tx)?;
        }
        ensure_dir_all(&mut s, &paths.targets_dir())?;
        let current = load_record(paths, &key)?;
        store::save(
            &mut s,
            &paths.target_record(&key),
            &planned.record,
            Some(&current.status),
        )?;
        failpoint::hit("engine.record.written").map_err(fail)?;
        let loaded = store::load::<FilesIndex>(&paths.files_index(&key))?;
        let mut index = loaded.as_ref().map_or_else(
            || FilesIndex {
                key,
                written_at_ns: 0,
                folders: Default::default(),
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
            Some(RecoveryAction::RolledForward) => (true, Vec::new()),
            _ => return Err(error),
        },
    };
    let held = millis(&guard);
    drop(guard);
    failpoint::hit("engine.unlocked").map_err(fail)?;
    write_status(paths, scopes, target, &planned.record);
    let mut warnings = recovered;
    warnings.extend(swap_warnings);
    Ok(planned.outcome(target, Some(*tx), false, atomic_swap, warnings, Some(held)))
}

// ---------------------------------------------------------------------------------------
// Planning.

/// Everything one undo does, computed against the state now.
#[derive(Debug, Clone)]
struct Planned {
    reverts: TxId,
    key: crate::model::TargetKey,
    folders: FolderPlan,
    record: TargetRecord,
    index: IndexChanges,
    changes: Vec<Change>,
    sv: SvPlan,
    reload_required: ReloadRequired,
    warnings: Vec<Warning>,
}

impl Planned {
    fn outcome(
        &self,
        target: &Target,
        tx: Option<TxId>,
        dry_run: bool,
        atomic_swap: bool,
        extra: Vec<Warning>,
        lock_held_ms: Option<u64>,
    ) -> UndoOutcome {
        let mut warnings = self.warnings.clone();
        for w in extra {
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
        UndoOutcome {
            undoes: self.reverts,
            outcome: TxOutcome {
                target: target.id.clone(),
                key: Some(self.key),
                tx,
                kind: Some(TxKind::Undo),
                dry_run,
                changes: self.changes.clone(),
                atomic_swap,
                reload_required: self.reload_required,
                warnings,
                creates_addons_dir: false,
                lock_held_ms,
                notes: Vec::new(),
            },
            sv: self.sv.counts,
        }
    }
}

/// Plans the undo of `entries[at]`: its conflicts, folders, record, index and
/// SavedVariables, judged by the running policy.
fn plan(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    entries: &[HistoryEntry],
    at: usize,
    target: &Target,
    running: bool,
) -> Result<Planned> {
    let reverted = &entries[at];
    let deferred_by = entries[at + 1..]
        .iter()
        .rposition(|e| e.kind == TxKind::Undo && e.reverts == Some(reverted.tx) && e.sv_deferred);
    // A deferred undo already reverted the folders: only its SavedVariables are left.
    let sv_only = match deferred_by {
        Some(i) if req.sv_policy == SvPolicy::Restore => Some(at + 1 + i),
        Some(i) => return Err(deferred(reverted, &entries[at + 1 + i])),
        None => None,
    };
    check_conflicts(entries, at, sv_only)?;

    let paths = ctx.paths;
    let client = ClientPaths::new(&target.client_dir);
    if !client.snapshots(&reverted.tx).is_dir() || !paths.state_snapshot(&reverted.tx).is_dir() {
        return Err(pruned(&reverted.tx));
    }
    let key = reverted.target;
    let current = load_record(paths, &key)?.value;
    let mut folders = match sv_only {
        Some(_) => FolderPlan::record_only(reverted),
        None => FolderPlan::inverse(reverted, &client, target)?,
    };
    let (record, index, changes) = match sv_only {
        Some(_) => (current.clone(), IndexChanges::default(), Vec::new()),
        None => {
            let (record, index, changes, edit_ops) =
                restored_state(paths, &current, reverted, &folders)?;
            folders.history.extend(edit_ops);
            (record, index, changes)
        }
    };

    let policy_ctx = PolicyContext {
        target_id: &target.id,
        argv: ctx.argv,
    };
    let judged = running::evaluate(
        &policy_ctx,
        ctx.policy,
        running,
        &target.rules,
        &std::mem::take(&mut folders.policy),
        req.while_running,
    )?;
    let mut warnings = judged.warnings;
    let sv = plan_saved_variables(&client, reverted, req.sv_policy, running, &policy_ctx, ctx)?;
    if sv.counts.deferred > 0 {
        warnings.push(
            Warning::new(
                WarningCode::SavedVariablesDeferred,
                format!(
                    "{}'s game client is running, so {} SavedVariables files were left as they are",
                    target.id, sv.counts.deferred
                ),
            )
            .with_fix(Suggestion::command([
                "omaddons".to_owned(),
                "undo".to_owned(),
                reverted.tx.to_string(),
                "--saved-variables".to_owned(),
                "restore".to_owned(),
            ])),
        );
    }
    Ok(Planned {
        reverts: reverted.tx,
        key,
        folders,
        record,
        index,
        changes,
        sv,
        reload_required: judged.reload_required,
        warnings,
    })
}

/// The index of the transaction `selector` names.
fn select(entries: &[HistoryEntry], selector: UndoSelector) -> Result<usize> {
    let none = |message: String| {
        Error::new(
            ErrorCode::NotFound,
            message,
            Suggestion::command(["omaddons", "history"]),
        )
    };
    match selector {
        UndoSelector::Latest => entries
            .iter()
            .rposition(|e| e.kind.is_undoable())
            .ok_or_else(|| none("there is no transaction to undo".to_owned())),
        UndoSelector::LastBy(actor) => entries
            .iter()
            .rposition(|e| e.kind.is_undoable() && e.actor == actor)
            .ok_or_else(|| none(format!("there is no transaction by {actor} to undo"))),
        UndoSelector::Tx(tx) => {
            let at = entries
                .iter()
                .position(|e| e.tx == tx)
                .ok_or_else(|| none(format!("there is no transaction {tx} in the history")))?;
            if !entries[at].kind.is_undoable() {
                return Err(none(format!(
                    "transaction {tx} is a {:?} transaction, which undo does not revert",
                    entries[at].kind
                )));
            }
            Ok(at)
        }
    }
}

/// `undo_conflict` when a transaction after `entries[at]` (or after the deferred undo
/// `sv_only`, for a SavedVariables follow-up) touched one of its folders on its target,
/// or already undid it. Ops carrying an in-place `required_by` edit count on neither
/// side: they are undone in place (M2-D35).
fn check_conflicts(entries: &[HistoryEntry], at: usize, sv_only: Option<usize>) -> Result<()> {
    let reverted = &entries[at];
    let keys: BTreeSet<&str> = reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none())
        .map(|o| o.folder.key())
        .collect();
    let after = sv_only.unwrap_or(at) + 1;
    let conflicting: Vec<&HistoryEntry> = entries[after..]
        .iter()
        .filter(|e| {
            let touches = e
                .ops
                .iter()
                .any(|o| o.edit.is_none() && keys.contains(o.folder.key()));
            let undid = e.kind == TxKind::Undo && e.reverts == Some(reverted.tx);
            e.target == reverted.target && (touches || undid)
        })
        .collect();
    let Some(newest) = conflicting.last() else {
        return Ok(());
    };
    let message = if newest.kind == TxKind::Undo && newest.reverts == Some(reverted.tx) {
        format!(
            "transaction {} was already undone by {}",
            reverted.tx, newest.tx
        )
    } else {
        format!(
            "{} later transaction(s) changed the folders of {}; the newest is {}",
            conflicting.len(),
            reverted.tx,
            newest.tx
        )
    };
    let listed: Vec<serde_json::Value> = conflicting
        .iter()
        .map(|e| {
            let folders: Vec<&str> = e
                .ops
                .iter()
                .filter(|o| keys.contains(o.folder.key()))
                .map(|o| o.folder.as_str())
                .collect();
            serde_json::json!({ "tx": e.tx, "kind": e.kind, "folders": folders })
        })
        .collect();
    Err(Error::new(
        ErrorCode::UndoConflict,
        message,
        Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            newest.tx.to_string(),
        ]),
    )
    .details(serde_json::json!({ "tx": reverted.tx, "conflicting": listed })))
}

/// The transaction was undone with its SavedVariables deferred; only `restore` finishes.
fn deferred(reverted: &HistoryEntry, by: &HistoryEntry) -> Error {
    Error::new(
        ErrorCode::UndoConflict,
        format!(
            "transaction {} was already undone by {}, which left its SavedVariables for later",
            reverted.tx, by.tx
        ),
        Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            reverted.tx.to_string(),
            "--saved-variables".to_owned(),
            "restore".to_owned(),
        ]),
    )
    .alternative(Suggestion::command([
        "omaddons".to_owned(),
        "undo".to_owned(),
        by.tx.to_string(),
    ]))
    .details(serde_json::json!({ "tx": reverted.tx, "undone_by": by.tx }))
}

/// The snapshot a transaction needs is gone.
fn pruned(tx: &TxId) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!("snapshot for {tx} was pruned by retention"),
        Suggestion::command(["omaddons", "history"]),
    )
    .details(serde_json::json!({ "tx": tx, "reason": "snapshot_pruned" }))
}

/// The folder side of an undo.
#[derive(Debug, Clone, Default)]
struct FolderPlan {
    swap: Vec<SwapOp>,
    /// Snapshot folders to stage, by the name the swap uses.
    sources: Vec<(FolderName, PathBuf)>,
    /// The undo's own history ops.
    history: Vec<HistoryOp>,
    policy: Vec<PolicyOp>,
    /// Every folder the reverted transaction touched.
    touched: Vec<FolderName>,
    /// Folder lists per addon, for the outcome's changes.
    summaries: Vec<(String, FolderSummary)>,
}

impl FolderPlan {
    /// No folder changes: the SavedVariables follow-up of a deferred undo. Its history
    /// still names the folders, so later undos see it touched them.
    fn record_only(reverted: &HistoryEntry) -> Self {
        // In-place edits were undone with the folders; they claim no folder now.
        let ops = || reverted.ops.iter().filter(|o| o.edit.is_none());
        Self {
            history: ops()
                .map(|o| HistoryOp {
                    op: FolderOp::Record,
                    folder: o.folder.clone(),
                    addon: o.addon.clone(),
                    edit: None,
                })
                .collect(),
            touched: ops().map(|o| o.folder.clone()).collect(),
            ..Self::default()
        }
    }

    /// The inverse of every folder op of `reverted`, against AddOns now.
    fn inverse(reverted: &HistoryEntry, client: &ClientPaths, target: &Target) -> Result<Self> {
        let addons = client.addons();
        let snapshots = client.snapshots(&reverted.tx).join("AddOns");
        let mut plan = Self::default();
        let loaded = |live: &FolderName| {
            DiskFolder::new(addons.join(live.as_str()))
                .ok()
                .and_then(|view| loaded_toc(&view, &target.rules).ok().flatten())
                .map(|l| l.file)
        };
        // In-place `required_by` edits move no folder and touch nothing: `restored_state`
        // reverses them on the record.
        for op in reverted.ops.iter().filter(|o| o.edit.is_none()) {
            let folder = &op.folder;
            plan.touched.push(folder.clone());
            let live = live_name(&addons, folder);
            let snapshot = snapshots.join(folder.as_str());
            let needs_snapshot = matches!(op.op, FolderOp::Replace | FolderOp::Remove);
            if needs_snapshot && !snapshot.is_dir() {
                return Err(pruned(&reverted.tx));
            }
            let (swap, policy, history) = match (op.op, live) {
                // Already gone: only the record changes.
                (FolderOp::Add, None) | (FolderOp::Record, _) => (None, None, FolderOp::Record),
                (FolderOp::Add, Some(live)) => (
                    Some(SwapOp::Remove {
                        live: live.clone(),
                        old_tree: None,
                    }),
                    Some(PolicyOp::RemoveFolder {
                        loaded_toc_before: loaded(&live),
                        folder: live,
                    }),
                    FolderOp::Remove,
                ),
                (FolderOp::Replace, Some(live)) => (
                    Some(SwapOp::Replace {
                        name: folder.clone(),
                        live: live.clone(),
                        old_tree: None,
                    }),
                    Some(PolicyOp::ReplaceFolder {
                        loaded_toc_before: loaded(&live),
                        toc_names_after: toc_names(&snapshot),
                        folder: live,
                    }),
                    FolderOp::Replace,
                ),
                (FolderOp::Replace | FolderOp::Remove, None) => (
                    Some(SwapOp::Add {
                        name: folder.clone(),
                    }),
                    Some(PolicyOp::AddFolder {
                        folder: folder.clone(),
                    }),
                    FolderOp::Add,
                ),
                (FolderOp::Remove, Some(live)) => {
                    return Err(Error::new(
                        ErrorCode::UndoConflict,
                        format!(
                            "{} is in AddOns again, put there outside omaddons after {} removed it",
                            live.as_str(),
                            reverted.tx
                        ),
                        Suggestion::text(format!(
                            "move {} out of AddOns, then retry",
                            live.as_str()
                        )),
                    )
                    .details(serde_json::json!({ "tx": reverted.tx, "folder": live.as_str() })));
                }
            };
            if matches!(swap, Some(SwapOp::Add { .. } | SwapOp::Replace { .. })) {
                plan.sources.push((folder.clone(), snapshot));
            }
            let summary = match plan.summaries.iter_mut().find(|(a, _)| *a == op.addon) {
                Some((_, summary)) => summary,
                None => {
                    plan.summaries
                        .push((op.addon.clone(), FolderSummary::default()));
                    match plan.summaries.last_mut() {
                        Some((_, summary)) => summary,
                        None => return Err(Error::internal("an empty folder summary list")),
                    }
                }
            };
            match &swap {
                Some(SwapOp::Add { name }) => summary.added.push(name.clone()),
                Some(SwapOp::Replace { live, .. }) => summary.replaced.push(live.clone()),
                Some(SwapOp::Remove { live, .. }) => summary.removed.push(live.clone()),
                None => summary.recorded.push(folder.clone()),
            }
            plan.swap.extend(swap);
            plan.policy.extend(policy);
            plan.history.push(HistoryOp {
                op: history,
                folder: folder.clone(),
                addon: op.addon.clone(),
                edit: None,
            });
        }
        Ok(plan)
    }
}

/// The on-disk spelling of `folder` in AddOns, matched case-insensitively.
fn live_name(addons: &Path, folder: &FolderName) -> Option<FolderName> {
    std::fs::read_dir(addons)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .find(|n| n.eq_ignore_ascii_case(folder.as_str()))
        .and_then(|n| FolderName::new(&n).ok())
}

/// The TOC files directly inside `dir`.
fn toc_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .map(|listing| {
            listing
                .filter_map(std::result::Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.to_ascii_lowercase().ends_with(".toc"))
                .collect()
        })
        .unwrap_or_default()
}

/// The record after the undo, its index changes, the per-addon changes and the history
/// ops of in-place edits.
type Restored = (TargetRecord, IndexChanges, Vec<Change>, Vec<HistoryOp>);

/// The undo commit's index changes: every folder it swaps in gets its staged entries
/// (the restored copies' own mtimes), recorded at `stamp_ns`, under the spelling the
/// swap leaves; folders it removes lose theirs. `restored` (the reverted transaction's
/// before entries) still covers touched folders the undo does not swap: record-only
/// ops, folders already gone.
fn undo_index(
    restored: &IndexChanges,
    swap: &[SwapOp],
    staged: &Staged,
    stamp_ns: i128,
) -> IndexChanges {
    let mut changes = super::tx::index_changes(swap, staged, stamp_ns);
    let swapped: BTreeSet<String> = changes
        .upsert
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .chain(changes.remove.iter().cloned())
        .collect();
    changes.upsert.extend(
        restored
            .upsert
            .iter()
            .filter(|f| !swapped.contains(&f.name.to_ascii_lowercase()))
            .cloned(),
    );
    changes.remove.extend(
        restored
            .remove
            .iter()
            .filter(|key| !swapped.contains(*key))
            .cloned(),
    );
    changes
}

/// The record and index after the undo, and the per-addon changes: every addon the
/// reverted transaction changed comes back as it was before it (or goes, if it was not
/// there); an owner it took folders from (`--force`) gets them back with its old state;
/// an in-place `required_by` edit is reversed on the addon as it is now, its index left
/// alone (M2-D35); everything else stays as it is now. A record restored from the
/// snapshot keeps only `required_by` names still recorded. The generation goes up.
fn restored_state(
    paths: &Paths,
    current: &TargetRecord,
    reverted: &HistoryEntry,
    folders: &FolderPlan,
) -> Result<Restored> {
    let key = reverted.target;
    let snapshot = paths.state_snapshot(&reverted.tx);
    let named = |path: PathBuf| path.file_name().map(|n| snapshot.join(n));
    let before = match named(paths.target_record(&key)) {
        Some(path) => store::load::<TargetRecord>(&path)?.map(|l| l.value),
        None => None,
    }
    .unwrap_or_else(|| TargetRecord::empty(key));
    let before_index = match named(paths.files_index(&key)) {
        Some(path) => store::load::<FilesIndex>(&path)?.map(|l| l.value),
        None => None,
    };

    let touched: BTreeSet<&str> = folders.touched.iter().map(FolderName::key).collect();
    let mut direct: Vec<String> = Vec::new();
    for op in reverted.ops.iter().filter(|o| o.edit.is_none()) {
        let addon_key = TargetRecord::addon_key(&op.addon);
        if !direct.contains(&addon_key) {
            direct.push(addon_key);
        }
    }
    // Records that come back from the snapshot, whose `required_by` may name addons
    // gone since.
    let mut restored: Vec<String> = Vec::new();
    let mut record = current.clone();
    let mut changes = Vec::new();
    for addon_key in &direct {
        let (now, then) = (current.addons.get(addon_key), before.addons.get(addon_key));
        let summary = folders
            .summaries
            .iter()
            .find(|(a, _)| TargetRecord::addon_key(a) == *addon_key)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let swapped = !(summary.added.is_empty()
            && summary.replaced.is_empty()
            && summary.removed.is_empty());
        let kind = match (now, then) {
            (Some(_), None) => ChangeKind::Remove,
            (None, Some(_)) => ChangeKind::Install,
            // Reverting a records-only change (a pin) moves no folder.
            (Some(_), Some(_)) if !swapped => ChangeKind::Edit,
            (Some(_), Some(_)) => ChangeKind::Update,
            (None, None) => continue,
        };
        let Some(addon) = then.or(now) else { continue };
        let fields = match kind {
            ChangeKind::Edit => changed_fields(now, then),
            _ => Vec::new(),
        };
        changes.push(Change {
            addon: addon.name.clone(),
            r#ref: addon.r#ref.clone(),
            kind,
            fields,
            from_version: now.map(|a| a.version.clone()),
            to_version: then.map(|a| a.version.clone()),
            folders: summary,
        });
        match then {
            Some(then) => {
                record.addons.insert(addon_key.clone(), then.clone());
                restored.push(addon_key.clone());
            }
            None => {
                record.addons.remove(addon_key);
            }
        }
    }
    for (addon_key, old) in &before.addons {
        if direct.contains(addon_key) {
            continue;
        }
        let lost: Vec<&FolderName> = old
            .folders
            .iter()
            .filter(|f| touched.contains(f.key()))
            .collect();
        if lost.is_empty() {
            continue;
        }
        if let Some(now) = record.addons.get_mut(addon_key) {
            for folder in lost {
                if !now.folders.iter().any(|f| f.key() == folder.key()) {
                    now.folders.push(folder.clone());
                }
            }
            now.state = old.state;
            restored.push(addon_key.clone());
        }
    }
    // A restored record forgets requesters that are no longer recorded.
    let recorded: BTreeSet<String> = record.addons.keys().cloned().collect();
    for addon_key in &restored {
        if let Some(addon) = record.addons.get_mut(addon_key) {
            addon
                .required_by
                .retain(|by| recorded.contains(&TargetRecord::addon_key(by)));
        }
    }
    let edit_ops = reverse_edits(reverted, &mut record, &mut changes);
    record.bump();

    let mut index = IndexChanges::default();
    for key in &touched {
        match before_index.as_ref().and_then(|i| i.folders.get(*key)) {
            Some(folder) => index.upsert.push(folder.clone()),
            None => index.remove.push((*key).to_owned()),
        }
    }
    Ok((record, index, changes, edit_ops))
}

/// Reverses the in-place `required_by` edits of `reverted`, newest first, on each addon
/// still recorded, and returns the undo's ops for them (the inverse edit on each folder
/// the reverted ops named). One `edit` change per addon edited.
fn reverse_edits(
    reverted: &HistoryEntry,
    record: &mut TargetRecord,
    changes: &mut Vec<Change>,
) -> Vec<HistoryOp> {
    // Each distinct (addon, edit), newest first, with the folders its ops named.
    let mut edits: Vec<(String, HistoryEdit, Vec<FolderName>)> = Vec::new();
    for op in reverted.ops.iter().rev() {
        let Some(edit) = &op.edit else { continue };
        let key = TargetRecord::addon_key(&op.addon);
        match edits.iter_mut().find(|(k, e, _)| *k == key && e == edit) {
            Some((_, _, folders)) => folders.push(op.folder.clone()),
            None => edits.push((key, edit.clone(), vec![op.folder.clone()])),
        }
    }
    let mut ops = Vec::new();
    for (key, edit, mut folders) in edits {
        let Some(addon) = record.addons.get_mut(&key) else {
            continue;
        };
        let inverse = edit.inverse();
        match &inverse {
            HistoryEdit::RequiredByRemove(by) => {
                let by = TargetRecord::addon_key(by);
                addon
                    .required_by
                    .retain(|n| TargetRecord::addon_key(n) != by);
            }
            HistoryEdit::RequiredByAdd(by) => {
                if !addon
                    .required_by
                    .iter()
                    .any(|n| TargetRecord::addon_key(n) == TargetRecord::addon_key(by))
                {
                    addon.required_by.push(by.clone());
                }
                addon
                    .required_by
                    .sort_by_key(|n| TargetRecord::addon_key(n));
            }
        }
        folders.sort_by(|a, b| a.key().cmp(b.key()));
        folders.dedup();
        for folder in &folders {
            ops.push(HistoryOp {
                op: FolderOp::Record,
                folder: folder.clone(),
                addon: addon.name.clone(),
                edit: Some(inverse.clone()),
            });
        }
        if !changes
            .iter()
            .any(|c| TargetRecord::addon_key(&c.addon) == key && c.kind == ChangeKind::Edit)
        {
            changes.push(Change {
                addon: addon.name.clone(),
                r#ref: addon.r#ref.clone(),
                kind: ChangeKind::Edit,
                fields: vec![EditField::RequiredBy],
                from_version: Some(addon.version.clone()),
                to_version: Some(addon.version.clone()),
                folders: FolderSummary {
                    recorded: folders,
                    ..FolderSummary::default()
                },
            });
        }
    }
    ops
}

// ---------------------------------------------------------------------------------------
// SavedVariables.

/// What happens to the reverted transaction's SavedVariables.
#[derive(Debug, Clone, Default)]
struct SvPlan {
    counts: SvCounts,
    /// The changed files to restore (empty when deferred, kept or refused).
    restore: Vec<SvEntry>,
}

/// Compares each file of the transaction's manifest with the file now and applies the
/// policy. While the client runs: `auto` defers, `restore` refuses with `game_running`.
fn plan_saved_variables(
    client: &ClientPaths,
    reverted: &HistoryEntry,
    policy: SvPolicy,
    running: bool,
    policy_ctx: &PolicyContext<'_>,
    ctx: &EngineCtx<'_>,
) -> Result<SvPlan> {
    let manifest_path = client.snapshots(&reverted.tx).join("WTF.manifest.json");
    let bytes = std::fs::read(&manifest_path).map_err(|_| pruned(&reverted.tx))?;
    let manifest: SvManifest = serde_json::from_slice(&bytes).map_err(|_| {
        Error::internal(format!(
            "the SavedVariables manifest of {} does not parse",
            reverted.tx
        ))
    })?;
    let wtf = client.wtf();
    let mut plan = SvPlan::default();
    let mut changed = Vec::new();
    for entry in manifest.entries {
        let now = present_hash(&wtf.join(relative(&entry.path)?))?;
        if now == entry.sha256 {
            plan.counts.unchanged += 1;
        } else {
            changed.push(entry);
        }
    }
    if changed.is_empty() {
        return Ok(plan);
    }
    match policy {
        SvPolicy::Keep => plan.counts.kept = changed.len(),
        SvPolicy::Auto if running => plan.counts.deferred = changed.len(),
        SvPolicy::Restore if running => {
            running::evaluate(
                policy_ctx,
                ctx.policy,
                true,
                &crate::rules::ClientRules::default(),
                &[PolicyOp::RestoreSavedVariables {
                    files: changed.len(),
                }],
                true,
            )?;
            return Err(Error::internal(
                "restoring SavedVariables while the client runs",
            ));
        }
        SvPolicy::Auto | SvPolicy::Restore => {
            plan.counts.restored = changed.len();
            plan.restore = changed;
        }
    }
    Ok(plan)
}

/// Brings each file of `sv` back to its state before `sv.from`; a file already there
/// is skipped, so roll-forward can run it again. A file that existed is cloned from the
/// reverted transaction's snapshot into the undo's own and renamed over the live one;
/// a file that did not exist moves into the undo's snapshot. Errors never name the
/// file: its path holds account and character names.
pub fn restore_saved_variables(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    sv: &SvRestore,
    undo_tx: &TxId,
) -> Result<usize> {
    let wtf = client.wtf();
    let attempt = ulid::Ulid::generate().to_string();
    let mut restored = 0;
    for entry in &sv.files {
        let rel = relative(&entry.path)?;
        let live = wtf.join(&rel);
        if present_hash(&live)? == entry.sha256 {
            continue;
        }
        let existed = entry.sha256.is_some();
        restore_one(s, client, sv, undo_tx, &attempt, &rel, existed).map_err(|e| {
            Error::new(
                e.code,
                "a SavedVariables file could not be restored; the next omaddons command that changes addons finishes the undo",
                Suggestion::command(["omaddons", "status"]),
            )
        })?;
        restored += 1;
    }
    Ok(restored)
}

/// One file of [`restore_saved_variables`]: `existed` before `sv.from`, or not.
fn restore_one(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    sv: &SvRestore,
    undo_tx: &TxId,
    attempt: &str,
    rel: &Path,
    existed: bool,
) -> Result<()> {
    let live = client.wtf().join(rel);
    let undo_snapshot = client.snapshots(undo_tx);
    if existed {
        let source = client.snapshots(&sv.from).join("WTF").join(rel);
        let staged = undo_snapshot.join("sv-restore").join(attempt).join(rel);
        if let Some(parent) = staged.parent() {
            ensure_dir_all(s, parent)?;
        }
        mutate(s, &staged, Op::CloneFile { from: &source })?;
        let replace = Op::Rename {
            to: &live,
            mode: RenameMode::Replace,
        };
        mutate(s, &staged, replace)?;
    } else {
        let away = undo_snapshot.join("sv-created").join(rel);
        if let Some(parent) = away.parent() {
            ensure_dir_all(s, parent)?;
        }
        let aside = Op::Rename {
            to: &away,
            mode: RenameMode::Replace,
        };
        mutate(s, &live, aside)?;
    }
    Ok(())
}

/// A manifest path as a relative path of plain components.
fn relative(path: &str) -> Result<PathBuf> {
    let rel = PathBuf::from(path);
    let plain = !path.is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)));
    if plain {
        Ok(rel)
    } else {
        Err(Error::internal(
            "a SavedVariables manifest entry is not a relative path",
        ))
    }
}

/// The SHA-256 of the regular file at `path`, `None` when absent. Errors never name it.
fn present_hash(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => sha256_file(path).map(Some).map_err(|e| {
            Error::new(
                e.code,
                "a SavedVariables file could not be read",
                Suggestion::command(["omaddons", "status"]),
            )
        }),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testing::TxScene;
    use crate::fsx::{TreeDigest, failpoint};

    const V1: &[(&str, &str)] = &[
        ("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n"),
        ("Foo/core.lua", "v1"),
        ("FooOld/FooOld.toc", "## Interface: 120105\n"),
    ];
    const V2: &[(&str, &str)] = &[
        ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
        ("Foo/core.lua", "v2"),
        ("Foo/lib/new.lua", "--"),
    ];
    const BAR: &[(&str, &str)] = &[("Bar/Bar.toc", "## Interface: 120105\n## Version: 7\n")];
    const ACCOUNT: &str = "Account/123456#1/SavedVariables";
    const CHARACTER: &str = "Account/123456#1/RealmName/CharName/SavedVariables";

    fn install(scene: &TxScene, name: &str, entries: &[(&str, &str)]) -> TxId {
        let zip = scene.zip(name, entries);
        let done = scene.run(vec![scene.install("gh:o/foo", &zip)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        done.outcomes[0].tx.unwrap()
    }

    fn install_as(scene: &TxScene, r: &str, name: &str, entries: &[(&str, &str)]) -> TxId {
        let zip = scene.zip(name, entries);
        let done = scene.run(vec![scene.install(r, &zip)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        done.outcomes[0].tx.unwrap()
    }

    fn record(scene: &TxScene) -> TargetRecord {
        let key = crate::discover::registry::read_marker(scene.client.client_dir()).unwrap();
        load_record(&scene.paths, &key).unwrap().value
    }

    fn history(scene: &TxScene) -> Vec<HistoryEntry> {
        history::read(&scene.paths, &HistoryFilter::default())
            .unwrap()
            .entries
    }

    fn tree(scene: &TxScene) -> TreeDigest {
        scene.addons()
    }

    fn wtf(scene: &TxScene, rel: &str) -> PathBuf {
        scene.client.wtf().join(rel)
    }

    fn read(scene: &TxScene, rel: &str) -> Option<String> {
        std::fs::read_to_string(wtf(scene, rel)).ok()
    }

    /// Account and character SavedVariables of Foo, as before the first install.
    fn saved_variables(scene: &TxScene) {
        for (dir, text) in [(ACCOUNT, "-- before"), (CHARACTER, "-- char before")] {
            std::fs::create_dir_all(wtf(scene, dir)).unwrap();
            std::fs::write(wtf(scene, &format!("{dir}/Foo.lua")), text).unwrap();
        }
    }

    /// What the new code does in game: changes one file, creates another.
    fn play(scene: &TxScene) {
        std::fs::write(wtf(scene, &format!("{ACCOUNT}/Foo.lua")), "-- after").unwrap();
        std::fs::write(wtf(scene, &format!("{ACCOUNT}/Foo.lua.bak")), "-- created").unwrap();
    }

    fn mtime(scene: &TxScene, rel: &str) -> std::time::SystemTime {
        std::fs::metadata(wtf(scene, rel))
            .unwrap()
            .modified()
            .unwrap()
    }

    fn leftovers(scene: &TxScene) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for dir in [
            scene.paths.journal_dir(),
            scene.paths.inflight_dir(),
            scene.client.meta_dir().join("stage"),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(listing.map(|e| e.unwrap().path()));
            }
        }
        found
    }

    #[test]
    fn undoing_an_install_an_update_and_a_remove_restores_folders_and_records() {
        // Install.
        let scene = TxScene::new();
        let empty = tree(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        let generation = record(&scene).generation;
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(done.undoes, t1);
        assert_eq!(tree(&scene), empty, "byte for byte");
        let after = record(&scene);
        assert!(
            after.addons.is_empty() && after.generation > generation,
            "{after:?}"
        );
        assert_eq!(done.outcome.changes[0].kind, ChangeKind::Remove);
        assert_eq!(done.outcome.changes[0].folders.removed.len(), 2);
        let last = history(&scene).pop().unwrap();
        assert_eq!((last.kind, last.reverts), (TxKind::Undo, Some(t1)));
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));

        // Update.
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let v1 = tree(&scene);
        install(&scene, "v2.zip", V2);
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(tree(&scene), v1);
        let foo = &record(&scene).addons["foo"];
        assert_eq!((foo.version.as_str(), foo.folders.len()), ("1", 2));
        assert_eq!(done.outcome.changes[0].kind, ChangeKind::Update);
        assert_eq!(
            (
                done.outcome.changes[0].from_version.as_deref(),
                done.outcome.changes[0].to_version.as_deref()
            ),
            (Some("2"), Some("1"))
        );

        // Remove.
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let installed = (tree(&scene), record(&scene).addons);
        scene.run(vec![scene.remove("Foo")]).unwrap();
        assert!(!scene.client.addons().join("Foo").exists());
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!((tree(&scene), record(&scene).addons), installed);
        assert_eq!(done.outcome.changes[0].kind, ChangeKind::Install);
    }

    #[test]
    fn undoing_an_undo_reapplies_the_change() {
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let installed = (tree(&scene), record(&scene).addons);
        let first = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        let second = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(second.undoes, first.outcome.tx.unwrap());
        assert_eq!((tree(&scene), record(&scene).addons), installed);
        // And back again.
        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert!(record(&scene).addons.is_empty());
    }

    #[test]
    fn a_later_transaction_on_the_same_folders_is_a_conflict() {
        let scene = TxScene::new();
        let t1 = install(&scene, "v1.zip", V1);
        let t2 = install(&scene, "v2.zip", V2);
        let before = tree(&scene);
        let err = scene
            .undo(UndoSelector::Tx(t1), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::UndoConflict, 4));
        assert_eq!(err.details["conflicting"][0]["tx"], t2.to_string());
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons".to_owned(), "undo".to_owned(), t2.to_string()]
        );
        assert_eq!(tree(&scene), before, "nothing changed");

        // Another addon's later install does not conflict, and stands.
        let scene = TxScene::new();
        let t1 = install(&scene, "v1.zip", V1);
        install_as(&scene, "gh:o/bar", "bar.zip", BAR);
        scene.undo(UndoSelector::Tx(t1), SvPolicy::Auto).unwrap();
        let addons = record(&scene).addons;
        assert_eq!(addons.keys().collect::<Vec<_>>(), ["bar"]);
        assert!(
            scene.client.addons().join("Bar").is_dir()
                && !scene.client.addons().join("Foo").exists()
        );

        // An undone transaction is not undone twice.
        let again = scene
            .undo(UndoSelector::Tx(t1), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(again.code, ErrorCode::UndoConflict);
        assert!(
            again.message.contains("already undone"),
            "{}",
            again.message
        );
    }

    #[test]
    fn last_by_reverts_only_that_actors_newest_transaction() {
        let mut scene = TxScene::new();
        scene.actor = Actor::Agent;
        let t1 = install(&scene, "v1.zip", V1);
        scene.actor = Actor::Human;
        install_as(&scene, "gh:o/bar", "bar.zip", BAR);
        let done = scene
            .undo(UndoSelector::LastBy(Actor::Agent), SvPolicy::Auto)
            .unwrap();
        assert_eq!(done.undoes, t1);
        assert_eq!(record(&scene).addons.keys().collect::<Vec<_>>(), ["bar"]);
        let err = scene
            .undo(UndoSelector::LastBy(Actor::Widget), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn saved_variables_unchanged_changed_created_and_kept() {
        let scene = TxScene::new();
        saved_variables(&scene);
        install(&scene, "v1.zip", V1);
        play(&scene);
        let character = format!("{CHARACTER}/Foo.lua");
        let untouched = mtime(&scene, &character);
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(
            done.sv,
            SvCounts {
                restored: 2,
                deferred: 0,
                unchanged: 6,
                kept: 0
            }
        );
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- before")
        );
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")),
            None,
            "created: moved away"
        );
        assert_eq!(read(&scene, &character).as_deref(), Some("-- char before"));
        assert_eq!(mtime(&scene, &character), untouched, "unchanged: untouched");
        // Nothing is lost: the undo snapshotted what it replaced.
        let undo_tx = done.outcome.tx.unwrap();
        let kept = scene
            .client
            .snapshots(&undo_tx)
            .join("WTF")
            .join(ACCOUNT)
            .join("Foo.lua");
        assert_eq!(std::fs::read_to_string(kept).unwrap(), "-- after");
        // Undoing the undo brings the played state back.
        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")).as_deref(),
            Some("-- created")
        );

        // keep touches nothing.
        let scene = TxScene::new();
        saved_variables(&scene);
        install(&scene, "v1.zip", V1);
        play(&scene);
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Keep).unwrap();
        assert_eq!((done.sv.kept, done.sv.restored), (2, 0));
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")).as_deref(),
            Some("-- created")
        );
    }

    #[test]
    fn a_running_client_defers_or_refuses_saved_variables() {
        // auto: folders revert (with --while-running), SavedVariables wait.
        let mut scene = TxScene::new();
        saved_variables(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        play(&scene);
        scene
            .running
            .insert(scene.client.client_dir().to_path_buf());
        scene.while_running = true;
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!((done.sv.deferred, done.sv.restored), (2, 0));
        assert!(
            !scene.client.addons().join("Foo").exists(),
            "folders reverted"
        );
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );
        let warning = done
            .outcome
            .warnings
            .iter()
            .find(|w| w.code == WarningCode::SavedVariablesDeferred)
            .unwrap();
        let follow_up = [
            "omaddons".to_owned(),
            "undo".to_owned(),
            t1.to_string(),
            "--saved-variables".to_owned(),
            "restore".to_owned(),
        ];
        assert_eq!(warning.fix.as_ref().unwrap().argv().unwrap(), follow_up);
        assert!(history(&scene).pop().unwrap().sv_deferred);

        // Only restore finishes it, and never while the client runs.
        let err = scene
            .undo(UndoSelector::Tx(t1), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict);
        assert_eq!(err.fix.argv().unwrap(), follow_up);
        let err = scene
            .undo(UndoSelector::Tx(t1), SvPolicy::Restore)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GameRunning);
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );

        // Once it exits: SavedVariables only.
        scene.running.clear();
        let folders = tree(&scene);
        let done = scene.undo(UndoSelector::Tx(t1), SvPolicy::Restore).unwrap();
        assert_eq!((done.undoes, done.sv.restored), (t1, 2));
        assert!(done.outcome.changes.is_empty());
        assert_eq!(tree(&scene), folders, "no folder changes");
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- before")
        );
        assert_eq!(read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")), None);

        // restore: refused while running, before any change.
        let mut scene = TxScene::new();
        saved_variables(&scene);
        install(&scene, "v1.zip", V1);
        play(&scene);
        let before = tree(&scene);
        scene
            .running
            .insert(scene.client.client_dir().to_path_buf());
        scene.while_running = true;
        let err = scene
            .undo(UndoSelector::Latest, SvPolicy::Restore)
            .unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::GameRunning, 4));
        assert_eq!(tree(&scene), before);
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );

        // Without --while-running the folders follow the safe policy.
        scene.while_running = false;
        let err = scene
            .undo(UndoSelector::Latest, SvPolicy::Keep)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::GameRunning);
        assert_eq!(tree(&scene), before);
    }

    #[test]
    fn a_pruned_snapshot_is_not_found() {
        let scene = TxScene::new();
        let t1 = install(&scene, "v1.zip", V1);
        std::fs::remove_dir_all(scene.client.snapshots(&t1)).unwrap();
        let err = scene
            .undo(UndoSelector::Latest, SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(
            err.message,
            format!("snapshot for {t1} was pruned by retention")
        );

        let err = scene
            .undo(UndoSelector::Tx(TxId::new()), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let fresh = TxScene::new();
        let err = fresh
            .undo(UndoSelector::Latest, SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "history"]);
    }

    #[test]
    fn a_dry_run_changes_nothing_and_output_names_no_account() {
        let scene = TxScene::new();
        saved_variables(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        play(&scene);
        let (before, entries) = (tree(&scene), history(&scene).len());
        let planned = scene
            .undo_with(UndoSelector::Latest, SvPolicy::Auto, true)
            .unwrap();
        assert_eq!(
            (planned.undoes, planned.outcome.tx, planned.outcome.dry_run),
            (t1, None, true)
        );
        assert_eq!(planned.sv.restored, 2);
        assert_eq!((tree(&scene), history(&scene).len()), (before, entries));
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );
        assert!(leftovers(&scene).is_empty());

        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        let json = serde_json::to_string(&done).unwrap();
        for private in ["123456", "RealmName", "CharName"] {
            assert!(!json.contains(private), "{private} in {json}");
        }
        assert_eq!(
            serde_json::to_value(&done).unwrap()["undoes"],
            t1.to_string()
        );
    }

    #[test]
    fn a_forced_transfer_is_undone_for_both_owners() {
        let mut scene = TxScene::new();
        install_as(
            &scene,
            "gh:o/a",
            "a.zip",
            &[
                ("A/A.toc", "## Interface: 120105\n"),
                ("Shared/Shared.toc", "## Interface: 120105\n## Notes: a\n"),
            ],
        );
        let installed = (tree(&scene), record(&scene).addons);
        scene.force = true;
        install_as(
            &scene,
            "gh:o/b",
            "b.zip",
            &[
                ("B/B.toc", "## Interface: 120105\n"),
                ("Shared/Shared.toc", "## Interface: 120105\n## Notes: b\n"),
            ],
        );
        assert_eq!(
            record(&scene).addons["a"].state,
            crate::model::AddonState::Degraded
        );
        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        let addons = record(&scene).addons;
        assert_eq!((tree(&scene), &addons), (installed.0, &installed.1));
    }

    #[test]
    fn an_error_at_any_failpoint_leaves_the_undo_wholly_done_or_not_done() {
        let after_swap = [
            "engine.swapped",
            "engine.record.written",
            "engine.history.appended",
            "engine.committed",
            "engine.unlocked",
        ];
        let names = [
            "engine.stage.begin",
            "engine.stage.synced",
            "engine.lock.acquired",
            "engine.recovered",
            "engine.revalidated",
            "engine.journal.prepared",
            "engine.snapshot.sv",
            "engine.snapshot.state",
            "engine.swap.after_exchange",
        ]
        .iter()
        .chain(after_swap.iter());
        for name in names {
            let scene = TxScene::new();
            saved_variables(&scene);
            install(&scene, "v1.zip", V1);
            let v1 = tree(&scene);
            let t2 = install(&scene, "v2.zip", V2);
            let v2 = tree(&scene);
            play(&scene);
            let result = failpoint::with_spec(&format!("{name}=error"), || {
                scene.undo(UndoSelector::Latest, SvPolicy::Auto)
            });
            let undos = || {
                history(&scene)
                    .iter()
                    .filter(|e| e.kind == TxKind::Undo)
                    .count()
            };
            if after_swap.contains(name) {
                assert_eq!(tree(&scene), v1, "{name}: wholly undone after {result:?}");
                assert_eq!(
                    read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
                    Some("-- before"),
                    "{name}"
                );
                assert_eq!(undos(), 1, "{name}: one history line");
            } else {
                assert!(result.is_err(), "{name}: the error surfaces");
                assert_eq!(tree(&scene), v2, "{name}: wholly not done");
                assert_eq!(
                    read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
                    Some("-- after"),
                    "{name}"
                );
                assert_eq!(undos(), 0, "{name}");
                scene.undo(UndoSelector::Tx(t2), SvPolicy::Auto).unwrap();
                assert_eq!(tree(&scene), v1, "{name}: a rerun converges");
            }
            assert!(
                leftovers(&scene).is_empty(),
                "{name}: {:?}",
                leftovers(&scene)
            );
        }
    }

    #[test]
    fn roll_forward_finishes_an_interrupted_saved_variables_restore() {
        let scene = TxScene::new();
        saved_variables(&scene);
        install(&scene, "v1.zip", V1);
        play(&scene);
        // The error hits right after the swap, before SavedVariables are written; the
        // in-process recovery rolls forward, payload and all.
        let done = failpoint::with_spec("engine.swapped=error", || {
            scene.undo(UndoSelector::Latest, SvPolicy::Auto)
        });
        assert!(done.is_ok(), "{done:?}");
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- before")
        );
        assert_eq!(read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")), None);
    }

    #[test]
    fn a_failed_saved_variables_snapshot_names_no_account() {
        let scene = TxScene::new();
        saved_variables(&scene);
        let zip = scene.zip("v1.zip", V1);
        let done = failpoint::with_spec("fsx.clone_file.before=error", || {
            scene.run(vec![scene.install("gh:o/foo", &zip)])
        })
        .unwrap();
        let error = serde_json::to_string(&done.failures).unwrap();
        assert!(error.contains("could not be snapshotted"), "{error}");
        for private in ["123456", "RealmName", "CharName"] {
            assert!(!error.contains(private), "{private} in {error}");
        }
    }

    #[test]
    fn sv_folders_come_from_file_names() {
        let f = |p: &str| sv_folder(p).map(|n| n.as_str().to_owned());
        assert_eq!(
            f("Account/A/SavedVariables/Foo.lua").as_deref(),
            Some("Foo")
        );
        assert_eq!(
            f("Account/A/R/C/SavedVariables/foo.LUA.bak").as_deref(),
            Some("foo")
        );
        assert_eq!(f("Account/A/SavedVariables/.lua"), None);
        assert!(relative("../x").is_err() && relative("/x").is_err() && relative("").is_err());
        assert!(relative("Account/A/SavedVariables/Foo.lua").is_ok());
    }

    /// Removing an addon whose only folder another work of the same transaction takes
    /// over (a handover): its record still reaches history, so undo restores it.
    #[test]
    fn a_remove_whose_only_folder_is_handed_over_undoes() {
        let scene = TxScene::new();
        let toc = "## Interface: 120105\n";
        install_as(&scene, "gh:o/old", "old.zip", &[("Old/Old.toc", toc)]);
        let before = tree(&scene);
        let zip = scene.zip(
            "b.zip",
            &[
                ("B/B.toc", toc),
                ("Old/Old.toc", "## Interface: 120105\n## Version: 2\n"),
            ],
        );
        let done = scene
            .run(vec![scene.remove("Old"), scene.install("gh:o/b", &zip)])
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let tx = done.outcomes[0].tx.unwrap();
        let line = history(&scene).into_iter().find(|e| e.tx == tx).unwrap();
        let ops: Vec<(FolderOp, &str, &str)> = line
            .ops
            .iter()
            .map(|o| (o.op, o.folder.as_str(), o.addon.as_str()))
            .collect();
        assert!(ops.contains(&(FolderOp::Replace, "Old", "B")), "{ops:?}");
        assert!(ops.contains(&(FolderOp::Record, "Old", "Old")), "{ops:?}");
        assert!(
            !ops.iter().any(|(op, ..)| *op == FolderOp::Remove),
            "{ops:?}"
        );
        let after = record(&scene);
        assert!(!after.addons.contains_key("old"));
        assert_eq!(after.addons["b"].state, crate::model::AddonState::Ok);

        scene.undo(UndoSelector::Tx(tx), SvPolicy::Auto).unwrap();
        assert_eq!(tree(&scene), before);
        let restored = record(&scene);
        assert!(restored.addons.contains_key("old") && !restored.addons.contains_key("b"));
    }

    /// `required_by` edits, undone in place (M2-D35): D is a dependency, R requires it.
    mod required_by {
        use super::*;
        use crate::engine::edit::RecordEdit;

        const D1: &[(&str, &str)] = &[("Dep/Dep.toc", "## Interface: 120105\n## Version: 1\n")];
        const D2: &[(&str, &str)] = &[
            ("Dep/Dep.toc", "## Interface: 120105\n## Version: 2\n"),
            ("Dep/new.lua", "--"),
        ];
        const R: &[(&str, &str)] = &[("Req/Req.toc", "## Interface: 120105\n")];

        fn ok(scene: &TxScene, changes: Vec<crate::engine::plan::RequestedChange>) -> TxId {
            let done = scene.run(changes).unwrap();
            assert!(done.failures.is_empty(), "{:?}", done.failures);
            done.outcomes[0].tx.unwrap()
        }

        fn undo(scene: &TxScene, tx: TxId) -> TxId {
            scene
                .undo(UndoSelector::Tx(tx), SvPolicy::Auto)
                .unwrap()
                .outcome
                .tx
                .unwrap()
        }

        fn required_by(scene: &TxScene) -> Vec<String> {
            record(scene).addons["dep"].required_by.clone()
        }

        fn dep_index(scene: &TxScene) -> String {
            serde_json::to_string(&scene.index().folders["dep"]).unwrap()
        }

        /// The ops of `tx` as (op, folder, edit).
        fn ops(scene: &TxScene, tx: TxId) -> Vec<(FolderOp, String, Option<HistoryEdit>)> {
            history(scene)
                .into_iter()
                .find(|e| e.tx == tx)
                .unwrap()
                .ops
                .into_iter()
                .map(|o| (o.op, o.folder.as_str().to_owned(), o.edit))
                .collect()
        }

        /// D installed; then tx1 installs R and adds R to D's `required_by`.
        fn scene_with_requirement() -> (TxScene, TxId) {
            let scene = TxScene::new();
            install_as(&scene, "gh:o/dep", "d1.zip", D1);
            let zip = scene.zip("r.zip", R);
            let tx1 = ok(
                &scene,
                vec![
                    scene.install("gh:o/req", &zip),
                    scene.edit("Dep", RecordEdit::AddRequiredBy("Req".into())),
                ],
            );
            (scene, tx1)
        }

        #[test]
        fn an_install_is_undone_after_its_dependency_updates() {
            let (scene, tx1) = scene_with_requirement();
            let tx1_ops = ops(&scene, tx1);
            assert_eq!(tx1_ops.len(), 2, "{tx1_ops:?}");
            assert!(tx1_ops.contains(&(FolderOp::Add, "Req".into(), None)));
            assert!(tx1_ops.contains(&(
                FolderOp::Record,
                "Dep".into(),
                Some(HistoryEdit::RequiredByAdd("Req".into()))
            )));
            assert_eq!(required_by(&scene), ["Req"]);
            let tx2 = install_as(&scene, "gh:o/dep", "d2.zip", D2);
            let index = dep_index(&scene);

            let undo1 = undo(&scene, tx1);
            let after = record(&scene);
            assert!(!after.addons.contains_key("req"), "R is gone");
            assert_eq!(after.addons["dep"].version, "2", "D keeps its update");
            assert!(after.addons["dep"].required_by.is_empty());
            assert!(!scene.client.addons().join("Req").exists());
            assert!(scene.client.addons().join("Dep/new.lua").exists());
            assert_eq!(dep_index(&scene), index, "D's index entry is untouched");
            let undo_ops = ops(&scene, undo1);
            assert!(undo_ops.contains(&(FolderOp::Remove, "Req".into(), None)));
            assert!(undo_ops.contains(&(
                FolderOp::Record,
                "Dep".into(),
                Some(HistoryEdit::RequiredByRemove("Req".into()))
            )));

            // Undoing D's update brings v1 back without the stale requester.
            undo(&scene, tx2);
            let dep = &record(&scene).addons["dep"];
            assert_eq!(dep.version, "1");
            assert!(dep.required_by.is_empty(), "{:?}", dep.required_by);

            // Undoing the undo of tx1 re-installs R and re-adds it.
            undo(&scene, undo1);
            assert!(record(&scene).addons.contains_key("req"));
            assert_eq!(required_by(&scene), ["Req"]);
        }

        #[test]
        fn a_remove_that_dropped_a_requirement_is_undone_exactly() {
            let (scene, _) = scene_with_requirement();
            let before = required_by(&scene);
            let tx = ok(
                &scene,
                vec![
                    scene.remove("Req"),
                    scene.edit("Dep", RecordEdit::RemoveRequiredBy("Req".into())),
                ],
            );
            assert!(required_by(&scene).is_empty());
            undo(&scene, tx);
            assert!(record(&scene).addons.contains_key("req"));
            assert_eq!(required_by(&scene), before);
        }

        #[test]
        fn a_requirement_beside_another_edit_undoes_by_snapshot() {
            let (scene, _) = scene_with_requirement();
            let before = record(&scene).addons["dep"].clone();
            let tx = ok(
                &scene,
                vec![
                    scene.edit("Dep", RecordEdit::SetPinned(true)),
                    scene.edit("Dep", RecordEdit::RemoveRequiredBy("Req".into())),
                ],
            );
            assert!(ops(&scene, tx).iter().all(|(_, _, edit)| edit.is_none()));
            undo(&scene, tx);
            let dep = &record(&scene).addons["dep"];
            assert_eq!(
                (dep.pinned, &dep.required_by),
                (before.pinned, &before.required_by)
            );
        }

        #[test]
        fn conflicts_ignore_in_place_edits_on_both_sides() {
            let (scene, tx1) = scene_with_requirement();
            // A later in-place edit on D does not conflict with undoing tx1.
            let later = ok(
                &scene,
                vec![scene.edit("Dep", RecordEdit::AddRequiredBy("Other".into()))],
            );
            assert!(ops(&scene, later).iter().all(|(_, _, e)| e.is_some()));
            let entries = history(&scene);
            let at = entries.iter().position(|e| e.tx == tx1).unwrap();
            assert!(check_conflicts(&entries, at, None).is_ok());
            // A later change to R's folder does.
            install_as(
                &scene,
                "gh:o/req",
                "r2.zip",
                &[("Req/Req.toc", "## Interface: 120105\n## Version: 2\n")],
            );
            let entries = history(&scene);
            assert_eq!(
                check_conflicts(&entries, at, None).unwrap_err().code,
                ErrorCode::UndoConflict
            );
        }
    }
}
