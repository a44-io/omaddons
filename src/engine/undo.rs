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
//! payload, so roll-forward completes it after a crash, or defers it the same way when
//! the client runs by then or the restore fails. Account, realm and character names
//! never reach the output: only counts do.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::edit::{EditField, changed_fields};
use super::journal::{
    self, CommitPayload, IndexChanges, Journal, Prepared, Recovered, RecoveryAction, RecoveryEnv,
    recover_tx,
};
use super::plan::{Change, ChangeKind, FolderSummary};
use super::retention::{snapshot_of, state_snapshot_of};
use super::running::{self, PolicyContext, PolicyOp};
use super::stage::{Staged, stage_trees};
use super::swap::{
    SvEntry, SvManifest, SvPrestage, SvSkip, SvSkipped, SwapOp, finish_saved_variables,
    out_of_scope_warnings, predigest, prestage_saved_variables, sha256_file, snapshot_state,
    sv_candidates, sv_skip, swap, with_predigests,
};
use super::tx::{
    EngineCtx, RunningNow, TxOutcome, cleanup, millis, recovery_failed, running_now, sv_writable,
    write_status,
};
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    ClonePolicy, FileStamp, FsxError, GlobalLock, InflightLock, MutateOpts, Op, OwnedFolders,
    RenameMode, ScopeTable, SnapshotRoots, WriteSession, Writer, ensure_dir_all, failpoint, mutate,
    mutate_with, sha256_from_hex,
};
use crate::model::{
    Actor, AddonRecord, FolderName, ReloadRequired, RunId, Target, TargetKey, TxId, TxKind,
};
use crate::state::files_index::FilesIndex;
use crate::state::history::{
    self, FolderOp, HistoryEdit, HistoryEntry, HistoryFilter, HistoryOp, HistoryRead, RepairEntry,
    TxResult, UnreadableLine,
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
    /// Changed, and left as it is: `--saved-variables keep`, or its `SavedVariables`
    /// directory is gone (a deleted character), which undo never creates.
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
    /// What its plan covers, for a preview's token (M3-D46).
    #[serde(skip)]
    pub plan: UndoFacts,
    /// The undo of a whole restore: what it reverts besides (M3-D28 rule 4).
    #[serde(skip)]
    pub wtf: Option<WtfUndo>,
    /// The undo of a repair-family line (E7): its plan as verified, its run and the
    /// inverses it committed; `None` on every other undo.
    #[serde(skip)]
    pub repair: Option<super::repair::undo::RepairUndone>,
}

/// What undoing a whole restore (or the undo of one) reverts with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WtfUndo {
    /// The files the play since it changed in the live `WTF/`, which the undo's own
    /// snapshot keeps; `None` when that cannot be told (the backup it restored is gone).
    pub changed_since: Option<u64>,
}

/// What an undo's plan covers, so that a run can be bound to its preview (M3-D46).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoFacts {
    /// The target's record generation it was planned against.
    pub generation: u64,
    /// Its folder operations, as its history line records them.
    pub ops: Vec<HistoryOp>,
    /// Each SavedVariables file it restores (relative to `WTF/`) with the stamp that file
    /// has now ([`FileStamp::fingerprint`]; `None` when absent).
    pub saved_variables: Vec<(String, Option<String>)>,
    /// The undo of a whole restore: the live `WTF/`'s stamp digest when planned (`absent`
    /// without one), so a play session after the preview refuses the bound run.
    pub wtf: Option<String>,
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
pub(super) fn sv_folder(path: &str) -> Option<FolderName> {
    let name = path.rsplit('/').next()?;
    let lower = name.to_ascii_lowercase();
    let stem = [".lua.bak", ".lua"]
        .into_iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(str::len))?;
    FolderName::new(name.get(..stem)?).ok()
}

// ---------------------------------------------------------------------------------------
// The undo.

/// The client the transaction `entry` changed. A target-less line (a repair, or the undo
/// of one) is refused with `not_found` here, before anything is planned, locked or
/// written: it is the newest change, never stepped over (M4-D46), and this build cannot
/// undo it yet.
pub(crate) fn target_of(entry: &HistoryEntry) -> Result<TargetKey> {
    entry.target.ok_or_else(|| {
        let what = if entry.kind == TxKind::Undo {
            "undid a repair"
        } else {
            "is a repair"
        };
        Error::new(
            ErrorCode::NotFound,
            format!(
                "transaction {} {what}, which this omaddons cannot undo yet",
                entry.tx
            ),
            Suggestion::command(["omaddons", "history"]),
        )
    })
}

/// Reverts the selected transaction (vision §5.5): plan without the lock, stage the
/// snapshot's folders, then the commit window under the global lock, planned again.
pub fn undo(req: &UndoRequest, ctx: &EngineCtx<'_>) -> Result<UndoOutcome> {
    let read = history::read(ctx.paths, &HistoryFilter::default())?;
    let entries = &read.entries;
    let at = select(entries, req.selector)?;
    // A repair, or the undo of one, is undone by inverting its primitives (E7).
    if super::repair::undo::is_repair_family(&entries[at]) {
        return super::repair::undo::undo_repair(req, ctx, &read, at);
    }
    let reverted = entries[at].clone();
    let key = target_of(&reverted)?;
    let discovered = req
        .targets
        .iter()
        .find(|d| d.target.key == Some(key))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::TargetNotFound,
                format!(
                    "transaction {} changed target {key}, which is not on this machine now",
                    reverted.tx
                ),
                Suggestion::command(["omaddons", "targets"]),
            )
        })?;
    if restores_wtf(&reverted) {
        return undo_whole(req, ctx, &read, at, discovered);
    }
    if req.sv_policy == SvPolicy::Keep && settings_only(&reverted) {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "transaction {} put addons' settings back; undoing it puts them back as they were, so --saved-variables keep would leave nothing to undo",
                reverted.tx
            ),
            Suggestion::command([
                "omaddons".to_owned(),
                "undo".to_owned(),
                reverted.tx.to_string(),
            ]),
        ));
    }
    let target = &discovered.target;
    let running = running_now(ctx, std::slice::from_ref(target));
    let mut hashes = SvHashes::default();
    let planned = plan(ctx, req, &read, at, target, &running, &mut hashes, None)?;
    if req.dry_run {
        return Ok(planned.outcome(target, None, true, true, Vec::new(), None));
    }
    // A bound run stages nothing unless it planned what the preview showed.
    if let Some(binding) = ctx.binding {
        binding.undo(&planned.outcome(target, None, true, true, Vec::new(), None))?;
    }
    // Before anything is staged: never refused only at the save, after the swap.
    super::tx::refuse_newer_state(ctx.paths, target)?;

    let scopes = super::fence::table(ctx.paths, std::slice::from_ref(&target.client_dir), None)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let client = ClientPaths::new(&target.client_dir);
    let moves = !planned.folders.swap.is_empty() || !planned.sv.restore.is_empty();
    let (auto_line, auto_warnings) = super::tx::pre_change(
        ctx,
        &scopes,
        discovered,
        moves,
        req.reason.as_ref(),
        req.run,
    );
    let staged = match stage_trees(&inflight, &scopes, target, &planned.folders.sources) {
        Ok(staged) => staged,
        Err(error) => {
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), false);
            super::tx::append_alone(ctx, &scopes, auto_line.as_ref(), Writer::Mutating);
            return Err(error);
        }
    };
    // The index stamps taken under the lock must postdate every staged mtime by the
    // racy slack.
    staged.wait_out_racy_slack();
    let result = commit(
        ctx,
        req,
        &scopes,
        discovered,
        &planned,
        &tx,
        &staged,
        hashes,
        auto_line.as_ref(),
    );
    let committed = result.as_ref().is_ok_and(|done| done.outcome.tx.is_some());
    cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), committed);
    // A lock that timed out once is not waited on twice.
    // ubs:ignore — comparing error codes, not secrets
    if !committed && !matches!(&result, Err(e) if e.code == ErrorCode::Locked) {
        super::tx::append_alone(ctx, &scopes, auto_line.as_ref(), Writer::Mutating);
    }
    super::tx::ring_after_window(ctx, &result);
    let mut done = result?;
    done.outcome.warnings.extend(auto_warnings);
    if let (Some(key), Some(_)) = (done.outcome.key, done.outcome.tx) {
        let retained = super::tx::retain(ctx, &scopes, key, &target.client_dir);
        done.outcome.warnings.extend(retained);
    }
    Ok(done)
}

/// Undoes `read.entries[at]`, a whole restore or the undo of one (M3-D28 rule 4): its
/// snapshot's `WTF.previous` goes back in place by one journaled exchange, and the tree it
/// replaces is kept in this undo's own snapshot, so the undo is undoable in turn. It puts
/// every setting back, so `--saved-variables keep` has nothing to keep, and it never runs
/// while the client runs, whatever the policy or flag.
fn undo_whole(
    req: &UndoRequest,
    ctx: &EngineCtx<'_>,
    read: &HistoryRead,
    at: usize,
    discovered: &DiscoveredTarget,
) -> Result<UndoOutcome> {
    let reverted = &read.entries[at];
    if req.sv_policy == SvPolicy::Keep {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "transaction {} put a whole WTF/ in place; undoing it puts every setting back, so --saved-variables keep would leave nothing to undo",
                reverted.tx
            ),
            Suggestion::command([
                "omaddons".to_owned(),
                "undo".to_owned(),
                reverted.tx.to_string(),
            ]),
        ));
    }
    let conflict_ctx = ConflictCtx {
        paths: ctx.paths,
        client_dir: &discovered.target.client_dir,
    };
    whole_conflicts(read, at, &conflict_ctx)?;
    // Under the lock, the history is read again: a write of settings since the plan
    // conflicts as it would have then.
    let recheck = || -> Result<()> {
        let read = history::read(ctx.paths, &HistoryFilter::default())?;
        let at = select(&read.entries, UndoSelector::Tx(reverted.tx))?;
        whole_conflicts(&read, at, &conflict_ctx)
    };
    super::restore::undo_whole(ctx, req, reverted, discovered, &recheck)
}

/// M3-D28 rules 1 and 5 for undoing the whole restore `read.entries[at]`: a later write of
/// settings, or a newer line this build cannot read, refuses it. Keeping SavedVariables
/// is no way out here, so rule 5 offers no `keep`. A later repair that touched its
/// client's record or its snapshot halves refuses it too (M4-D62).
fn whole_conflicts(read: &HistoryRead, at: usize, ctx: &ConflictCtx<'_>) -> Result<()> {
    check_conflicts_in(&read.entries, at, None, false, Some(ctx))?;
    check_unreadable(&read.unreadable, at, &read.entries[at], true).map_err(|mut error| {
        error.alternatives.clear();
        error
    })
}

/// What `before` (the plan made without the lock) reads and copies before the lock,
/// then the commit window under it: recover, plan again against the state now,
/// journal, snapshot, swap, restore SavedVariables, write state, commit.
#[expect(
    clippy::too_many_arguments,
    reason = "the commit window threads the request, its plan and the staged state; a struct would only rename them"
)]
fn commit(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    scopes: &ScopeTable,
    discovered: &DiscoveredTarget,
    before: &Planned,
    tx: &TxId,
    staged: &Staged,
    mut hashes: SvHashes,
    auto_line: Option<&HistoryEntry>,
) -> Result<UndoOutcome> {
    let paths = ctx.paths;
    let fail = |e: std::io::Error| {
        Error::io(
            paths.state_dir(),
            &e,
            Suggestion::command(["omaddons", "status"]),
        )
    };
    let target = &discovered.target;
    let client = ClientPaths::new(&target.client_dir);
    // The undo of a partial restore puts settings back: its snapshots are named
    // `<tx>.wtf`, as its journal will be.
    let (snapshot, state_snapshot, roots) = if history::puts_settings_back(&before.folders.history)
    {
        (
            client.snapshots_wtf(tx),
            paths.state_snapshot_wtf(tx),
            SnapshotRoots::for_wtf_tx(paths, tx),
        )
    } else {
        (
            client.snapshots(tx),
            paths.state_snapshot(tx),
            SnapshotRoots::for_tx(paths, tx),
        )
    };
    let actions = state_snapshot.join("actions.jsonl");
    let mut s = WriteSession::tx(*tx, Writer::Undo, scopes, roots, actions);
    // Before the lock (D24): the undo's own SavedVariables snapshot, the copies its
    // restore brings back (checked against the manifest), and the digests of folders the
    // files index does not give. Under the lock, what is unchanged since costs a stat.
    let prestage = prestage_saved_variables(&mut s, &snapshot, &client, &before.sv_folders)?;
    hashes.absorb(&prestage);
    let copies = prestage_restores(&mut s, &client, before, &snapshot)?;
    let predigests = predigest(&client.addons(), &before.folders.swap);
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
    // Taken with the outcome; any other way out of the window notes it (omad-h590).
    let recovered = journal::recover_kept(&recovery, ctx.notes)?;
    failpoint::hit("engine.recovered").map_err(fail)?;
    // The automatic backup's line first, before the journal (M3-D33).
    if let Some(line) = auto_line {
        super::tx::append_once(paths, scopes, &guard, line, Writer::Mutating)?;
    }
    // Again under the lock: a newer omaddons may have written since the first look.
    super::tx::refuse_newer_state(paths, target)?;
    // A repair that became stuck since the table was built, or one fencing this target,
    // a folder the undo puts back or the `WTF/` it restores settings in (E14).
    let addons = client.addons();
    let mut first_hand: Vec<PathBuf> = before
        .folders
        .touched
        .iter()
        .map(|folder| addons.join(folder.as_str()))
        .collect();
    if !before.sv.restore.is_empty() {
        first_hand.push(client.wtf());
    }
    super::fence::under_lock(scopes, paths, Some(target), &first_hand)?;

    let reverts = before.reverts;
    let read = history::read(paths, &HistoryFilter::default())?;
    let at = select(&read.entries, UndoSelector::Tx(reverts))?;
    let running = running_now(ctx, std::slice::from_ref(target));
    let mut planned = plan(
        ctx,
        req,
        &read,
        at,
        target,
        &running,
        &mut hashes,
        Some(*tx),
    )?;
    with_predigests(&mut planned.folders.swap, &client.addons(), &predigests);
    // Planned anew under the lock: still the previewed undo, or nothing (M3-D46).
    if let Some(binding) = ctx.binding {
        binding.undo(&planned.outcome(target, None, true, true, Vec::new(), None))?;
    }
    failpoint::hit("engine.revalidated").map_err(fail)?;

    let key = planned.key;
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
        target: Some(key),
        targets: Vec::new(),
        actor: ctx.actor,
        reason: req.reason.clone(),
        argv: ctx.argv.to_vec(),
        ops: planned.folders.history.clone(),
        result: TxResult::Committed,
        reverts: Some(reverts),
        sv_deferred: planned.sv.counts.deferred > 0,
        // Always set, 0 included: only lines from before M3 lack it (M3-D28).
        sv_restored: Some(u64::try_from(planned.sv.counts.restored).unwrap_or(u64::MAX)),
        created_addons_dir: false,
        run: Some(req.run),
        backup: None,
        plan: ctx.applying,
        doctor_run_id: None,
        repairs: Vec::new(),
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
        writes_wtf: false,
        staged_stamp: None,
        staged_files: None,
        source_backup: None,
        client: None,
    };
    let touched = planned.folders.touched.clone();
    let mut owned: Vec<FolderName> = touched.clone();
    if let Some(sv) = &sv_restore {
        owned.extend(sv.files.iter().filter_map(|f| sv_folder(&f.path)));
    }
    let mut s = s.with_owned_folders(OwnedFolders::new(&owned));
    let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;

    let committed = (|| -> Result<(bool, Vec<Warning>)> {
        failpoint::hit("engine.journal.prepared").map_err(fail)?;
        finish_saved_variables(
            &mut s,
            tx,
            &snapshot,
            &client,
            &planned.sv_folders,
            prestage,
        )?;
        snapshot_state(&mut s, paths, &state_snapshot, &key)?;
        let swap_ops = &planned.folders.swap;
        let report = swap(&mut s, &mut journal, swap_ops, staged, &client, &snapshot)?;
        if let Some(sv) = &sv_restore {
            restore_saved_variables(&mut s, &client, sv, &snapshot, &copies, &mut hashes)?;
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
        super::tx::note_undo(ctx, tx);
        failpoint::hit("engine.history.appended").map_err(fail)?;
        journal.mark_committed(&mut s)?;
        failpoint::hit("engine.committed").map_err(fail)?;
        Ok((report.atomic_swap, report.warnings))
    })();
    let (atomic_swap, swap_warnings) = match committed {
        Ok(done) => done,
        Err(error) => {
            // Read before recovery prunes it: committed already, the undo stands.
            let committed_swap = super::journal::committed_swap(paths, tx);
            match (recover_tx(&recovery, tx), committed_swap) {
                (Err(failed), _) => return Err(recovery_failed(error, tx, &failed)),
                (
                    Ok(Some(Recovered {
                        action: RecoveryAction::RolledForward,
                        atomic_swap,
                        warnings,
                        sv_deferred,
                        ..
                    })),
                    _,
                ) => {
                    super::tx::note_undo(ctx, tx);
                    // What roll-forward could not restore waits, as under a running client.
                    let counts = &mut planned.sv.counts;
                    counts.deferred += sv_deferred;
                    counts.restored = counts.restored.saturating_sub(sv_deferred);
                    super::swap::rolled_forward(atomic_swap, warnings)
                }
                (Ok(None), Some(atomic_swap)) => {
                    super::tx::note_undo(ctx, tx);
                    super::swap::rolled_forward(atomic_swap, Vec::new())
                }
                (Ok(_), _) => return Err(error),
            }
        }
    };
    let held = millis(&guard);
    drop(guard);
    failpoint::hit("engine.unlocked").map_err(fail)?;
    write_status(paths, scopes, target, &planned.record);
    let mut warnings = recovered.take();
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
    /// The record generation it was planned against.
    generation: u64,
    folders: FolderPlan,
    record: TargetRecord,
    index: IndexChanges,
    changes: Vec<Change>,
    sv: SvPlan,
    /// The SavedVariables follow-up of a deferred undo: no folder changes.
    sv_only: bool,
    /// The folders whose SavedVariables the undo snapshots: those of the files the
    /// reverted transaction's manifest lists, the only ones it can write. A records-only
    /// change (a pin) lists none, so undoing its undo rewinds no settings.
    sv_folders: Vec<FolderName>,
    reload_required: ReloadRequired,
    warnings: Vec<Warning>,
    /// `-v` lines: where the undo's snapshot keeps a folder's only copy.
    notes: Vec<String>,
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
        let wtf = ClientPaths::new(&target.client_dir).wtf();
        let saved_variables = self
            .sv
            .restore
            .iter()
            .map(|entry| {
                let now = FileStamp::read(&wtf.join(&entry.path))
                    .ok()
                    .flatten()
                    .map(|stamp| stamp.fingerprint());
                (entry.path.clone(), now)
            })
            .collect();
        UndoOutcome {
            plan: UndoFacts {
                generation: self.generation,
                ops: self.folders.history.clone(),
                saved_variables,
                wtf: None,
            },
            wtf: None,
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
                notes: self.notes.clone(),
            },
            sv: self.sv.counts,
            repair: None,
        }
    }
}

/// Plans the undo of `entries[at]`: its conflicts, folders, record, index and
/// SavedVariables, judged by the running policy, and the dependency warnings `remove`
/// would raise for the same records (committed as `tx`, their fix undoes this undo).
/// SavedVariables are hashed through `hashes`, so a second plan reads only what changed
/// since the first.
#[expect(
    clippy::too_many_arguments,
    reason = "the plan reads the request, the history, the target and the run's state; a struct would only rename them"
)]
fn plan(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    history: &HistoryRead,
    at: usize,
    target: &Target,
    running_now: &RunningNow,
    hashes: &mut SvHashes,
    tx: Option<TxId>,
) -> Result<Planned> {
    let entries = &history.entries[..];
    let running = running_now.contains(&target.client_dir);
    let reverted = &entries[at];
    // The newest undo of it, when that one deferred: a follow-up that finished since is
    // newer, and the undo is then simply done.
    let deferred_by = entries[at + 1..]
        .iter()
        .rposition(|e| e.kind == TxKind::Undo && e.reverts == Some(reverted.tx))
        .filter(|i| entries[at + 1 + i].sv_deferred);
    // A deferred undo already reverted the folders: only its SavedVariables are left.
    let sv_only = match deferred_by {
        Some(i) if req.sv_policy == SvPolicy::Restore => Some(at + 1 + i),
        Some(i) => return Err(deferred(reverted, &entries[at + 1 + i])),
        None => None,
    };
    let paths = ctx.paths;
    let client = ClientPaths::new(&target.client_dir);
    // A follow-up restores SavedVariables only; an undo, when its snapshot has them.
    let snapshot = snapshot_of(&client, &reverted.tx);
    let manifest = snapshot.join("WTF.manifest.json");
    // A manifest with files in it: a records-only change (a pin) writes an empty one, and
    // its undo restores no settings, so it conflicts with no later restore over them.
    // One that does not parse may have files: it counts (and fails its plan later).
    let listed = std::fs::read(&manifest)
        .ok()
        .map(|bytes| serde_json::from_slice::<SvManifest>(&bytes).ok());
    let has_sv = listed
        .as_ref()
        .is_some_and(|m| m.as_ref().is_none_or(|m| !m.entries.is_empty()));
    let restores_sv = sv_only.is_some() || (req.sv_policy != SvPolicy::Keep && has_sv);
    let mut sv_folders: Vec<FolderName> = Vec::new();
    for folder in listed
        .flatten()
        .iter()
        .flat_map(|m| &m.entries)
        .filter_map(|e| sv_folder(&e.path))
    {
        if !sv_folders.iter().any(|f| f.key() == folder.key()) {
            sv_folders.push(folder);
        }
    }
    let conflict_ctx = ConflictCtx {
        paths,
        client_dir: &target.client_dir,
    };
    check_conflicts_in(entries, at, sv_only, restores_sv, Some(&conflict_ctx))?;
    if !snapshot.is_dir() || !state_snapshot_of(paths, &reverted.tx).is_dir() {
        return Err(pruned(&reverted.tx));
    }
    let key = target_of(reverted)?;
    let current = load_record(paths, &key)?.value;
    let mut folders = match sv_only {
        Some(_) => FolderPlan::record_only(reverted),
        None => {
            let index = store::load::<FilesIndex>(&paths.files_index(&key))?.map(|l| l.value);
            FolderPlan::inverse(reverted, &client, target, index.as_ref())?
        }
    };
    let (record, index, changes) = match sv_only {
        Some(_) => (current.clone(), IndexChanges::default(), Vec::new()),
        None => {
            let (record, index, changes, edit_ops) =
                restored_state(paths, &current, entries, at, &folders)?;
            folders.history.extend(edit_ops);
            (record, index, changes)
        }
    };

    let policy_ctx = PolicyContext {
        target_id: &target.id,
        argv: ctx.argv,
        running_unknown: running_now.unknown.as_deref(),
    };
    let judged = running::evaluate(
        &policy_ctx,
        ctx.policy,
        running,
        &target.rules,
        &std::mem::take(&mut folders.policy),
        req.while_running,
    )?;
    let mut warnings: Vec<Warning> = running_now.warning().into_iter().collect();
    warnings.extend(judged.warnings);
    let sv = plan_saved_variables(
        &client,
        reverted,
        req.sv_policy,
        running,
        &policy_ctx,
        ctx,
        hashes,
    )?;
    // Every SavedVariables file of the transaction's snapshot, unless `keep`: a newer line
    // this build cannot read may have written any of them.
    let restores = req.sv_policy != SvPolicy::Keep
        && sv.counts.restored + sv.counts.deferred + sv.counts.unchanged > 0;
    check_unreadable(&history.unreadable, at, reverted, restores)?;
    if sv.counts.deferred > 0 {
        let is = if running_now.unknown.is_some() {
            "may be running"
        } else {
            "is running"
        };
        warnings.push(
            Warning::new(
                WarningCode::SavedVariablesDeferred,
                format!(
                    "{}'s game client {is}, so {} SavedVariables files were left as they are",
                    target.id, sv.counts.deferred
                ),
            )
            .with_fix(sv_follow_up(&reverted.tx)),
        );
    }
    warnings.extend(out_of_scope_warnings(
        &target.id,
        sv.out_of_scope,
        "undo left them as they are",
    ));
    // An undo goes ahead like `remove`: a dependency it takes away from an addon that
    // stays is `still_required`, one it leaves to nothing `orphaned_dependency`. What it
    // takes away the reverted transaction installed, so an addon that transaction also
    // changed comes back as it was before, when it did not require it: only later
    // requesters count.
    let direct: BTreeSet<String> = reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none() && o.op.is_folder())
        .map(|o| TargetRecord::addon_key(&o.addon))
        .collect();
    let mut was = current.clone();
    for (key, addon) in &mut was.addons {
        if !record.addons.contains_key(key) {
            addon
                .required_by
                .retain(|by| !direct.contains(&TargetRecord::addon_key(by)));
        }
    }
    warnings.extend(super::plan::record_change_warnings(
        &was, &record, &target.id, tx,
    ));
    // A folder the undo moves out that exists nowhere else lives on only in the undo's
    // snapshot, until retention prunes it.
    let mut only: Vec<FolderName> = Vec::new();
    for op in &folders.swap {
        if let Some(live) = super::plan::moves_only_copy(&current, op)
            && !only.iter().any(|f| f.key() == live.key())
        {
            only.push(live.as_folder().clone());
        }
    }
    only.sort_by(|a, b| a.key().cmp(b.key()));
    warnings.extend(super::plan::only_copy_warning(
        &target.id,
        &only,
        tx,
        &ctx.retention,
    ));
    let notes = match tx {
        Some(tx) => {
            // Named as `commit` names it.
            let snapshot = if history::puts_settings_back(&folders.history) {
                client.snapshots_wtf(&tx)
            } else {
                client.snapshots(&tx)
            };
            only.iter()
                .map(|folder| {
                    format!(
                        "the only copy of {} is kept in {}",
                        folder.as_str(),
                        snapshot.join("AddOns").join(folder.as_str()).display()
                    )
                })
                .collect()
        }
        None => Vec::new(),
    };
    Ok(Planned {
        reverts: reverted.tx,
        key,
        generation: current.generation,
        folders,
        record,
        index,
        changes,
        sv,
        sv_only: sv_only.is_some(),
        sv_folders,
        reload_required: judged.reload_required,
        warnings,
        notes,
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

/// The transactions whose undo deferred its SavedVariables restore and whose follow-up
/// (`undo <tx> --saved-variables restore`) would still pass its conflict check: it has
/// not run, the deferred undo was not undone, and nothing later changed those folders.
/// Retention keeps their snapshots, which hold the SavedVariables copies. `entries` is
/// the whole history, oldest first.
pub(super) fn pending_sv_restores(entries: &[HistoryEntry]) -> BTreeSet<TxId> {
    // Each reverted transaction's latest deferred undo, as the follow-up finds it.
    let mut deferred: BTreeMap<TxId, usize> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let (TxKind::Undo, Some(of), true) = (e.kind, e.reverts, e.sv_deferred) {
            deferred.insert(of, i);
        }
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(at, e)| {
            // A target-less line (a repair) never defers a SavedVariables restore.
            e.target?;
            let by = *deferred.get(&e.tx)?;
            (by > at && check_conflicts(entries, at, Some(by), true).is_ok()).then_some(e.tx)
        })
        .collect()
}

/// `undo_conflict` when a transaction after `entries[at]` (or after the deferred undo
/// `sv_only`, for a SavedVariables follow-up), on its target, already undid it or would
/// be overwritten by undoing it (M3-D28):
/// - a later transaction touched one of its folders;
/// - rule 1: it is a whole restore (a `wtf` op), and a later one wrote settings: a `wtf`
///   or `saved_variables` op, or an undo that restored SavedVariables (one from before M3,
///   without `sv_restored`, may have);
/// - rule 2: the undo `restores_sv` (its snapshot holds a SavedVariables manifest and
///   `--saved-variables keep` was not asked; always, for a follow-up), and a whole
///   restore came later: that restore's undo is the fix, `keep` the alternative;
/// - rule 3: a partial restore of folder F's SavedVariables (a `saved_variables` op)
///   counts as a write of F's SavedVariables, both ways: a later one conflicts with
///   undoing an earlier transaction whose undo would restore F's SavedVariables (it
///   touched F and `restores_sv`), and undoing a partial restore of F conflicts with a
///   later transaction that wrote F's SavedVariables (a `saved_variables` op on F, or an
///   undo that may have restored SavedVariables and touched F).
///
/// Ops carrying an in-place `required_by` edit count on neither side: they are undone in
/// place (M2-D35). A later transaction that a later undo reverted cancels out with that
/// undo, and neither counts (see [`cancelled_after`]): a restore followed by its own
/// undo no longer counts as a later restore.
///
/// A later repair, or the undo of one, counts when one of its operations touched an
/// AddOns folder the undo writes back (E12's folder rule). This form applies that rule
/// alone, which is retention's: fewer conflicts keep more snapshots.
fn check_conflicts(
    entries: &[HistoryEntry],
    at: usize,
    sv_only: Option<usize>,
    restores_sv: bool,
) -> Result<()> {
    check_conflicts_in(entries, at, sv_only, restores_sv, None)
}

/// Where the transaction being undone lives, for [`check_conflicts_in`]'s resource rule.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConflictCtx<'a> {
    pub paths: &'a Paths,
    /// The discovered directory of its client.
    pub client_dir: &'a Path,
}

/// The paths a later repair must not have touched for an undo to write them back
/// (M4-D62), canonical and each standing for its subtree: the undo's AddOns folders (by
/// key, below the client's canonical `Interface/AddOns`), its client's record and files
/// index, and both halves of the reverted transaction's snapshot.
struct UndoResources {
    addons: PathBuf,
    keys: BTreeSet<String>,
    paths: Vec<PathBuf>,
}

impl UndoResources {
    fn of(
        ctx: &ConflictCtx<'_>,
        reverted: &HistoryEntry,
        key: TargetKey,
        keys: &BTreeSet<&str>,
    ) -> Self {
        let whole = |path: PathBuf| crate::fsx::canonical_lenient(&path).unwrap_or(path);
        let client = ClientPaths::new(ctx.client_dir);
        let tx = &reverted.tx;
        Self {
            addons: whole(client.addons()),
            keys: keys.iter().map(|key| (*key).to_owned()).collect(),
            paths: vec![
                whole(ctx.paths.target_record(&key)),
                whole(ctx.paths.files_index(&key)),
                whole(client.snapshots(tx)),
                whole(client.snapshots_wtf(tx)),
                whole(ctx.paths.state_snapshot(tx)),
                whole(ctx.paths.state_snapshot_wtf(tx)),
            ],
        }
    }

    /// The AddOns folders of the undo that `repair` touched by path, and whether it
    /// touched one of the other paths.
    fn touched(&self, repair: &RepairEntry) -> (BTreeSet<FolderName>, bool) {
        let mut folders = BTreeSet::new();
        let mut other = false;
        for path in [
            Some(&repair.path),
            repair.to.as_ref(),
            repair.backup.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(name) = path
                .strip_prefix(&self.addons)
                .ok()
                .and_then(|rel| rel.components().next())
                .and_then(|first| first.as_os_str().to_str())
                .and_then(|name| FolderName::new(name).ok())
                .filter(|folder| self.keys.contains(folder.key()))
            {
                folders.insert(name);
            }
            other |= self
                .paths
                .iter()
                .any(|held| path.starts_with(held) || held.starts_with(path));
        }
        (folders, other)
    }
}

/// [`check_conflicts`], and with `ctx` the resource rule (M4-D62) too: a later repair
/// conflicts when one of its operations' paths overlaps a folder the undo writes back,
/// its client's record or files index, or the reverted transaction's snapshot halves.
/// Such an overlap that names no folder lists `folders: []` in `details.conflicting`.
pub(crate) fn check_conflicts_in(
    entries: &[HistoryEntry],
    at: usize,
    sv_only: Option<usize>,
    restores_sv: bool,
    ctx: Option<&ConflictCtx<'_>>,
) -> Result<()> {
    let reverted = &entries[at];
    let keys: BTreeSet<&str> = reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none() && o.op.is_folder())
        .map(|o| o.folder.key())
        .collect();
    let whole = restores_wtf(reverted);
    // Rule 3: the folders whose SavedVariables undoing it writes.
    let sv_keys: BTreeSet<&str> = reverted
        .ops
        .iter()
        .filter(|o| {
            o.edit.is_none()
                && (matches!(o.op, FolderOp::SavedVariables) || (restores_sv && o.op.is_folder()))
        })
        .map(|o| o.folder.key())
        .collect();
    let key = target_of(reverted)?;
    let after = sv_only.unwrap_or(at) + 1;
    // With a context, the resources a later repair must not have touched (M4-D62).
    let resources = ctx.map(|ctx| UndoResources::of(ctx, reverted, key, &keys));
    let by_resource = |e: &HistoryEntry| {
        resources.as_ref().is_some_and(|resources| {
            e.repairs.iter().any(|r| {
                let (folders, other) = resources.touched(r);
                other || !folders.is_empty()
            })
        })
    };
    // Every later line on its target, a repair that touched it included, and with a
    // context a repair whose paths touched what the undo writes back from the home side
    // (a record it quarantined names no client).
    let later: Vec<&HistoryEntry> = entries[after..]
        .iter()
        .filter(|e| e.touches_target(key) || by_resource(e))
        .collect();
    let cancelled = cancelled_after(&entries[after..], reverted.tx);
    let conflicts = |o: &HistoryOp| {
        o.edit.is_none()
            && match o.op {
                FolderOp::Wtf => whole,
                FolderOp::SavedVariables => whole || sv_keys.contains(o.folder.key()),
                FolderOp::Add | FolderOp::Replace | FolderOp::Remove | FolderOp::Record => {
                    keys.contains(o.folder.key())
                }
            }
    };
    let restored_sv_of = |e: &HistoryEntry| {
        matches!(e.kind, TxKind::Undo)
            && may_have_restored(e)
            && e.ops
                .iter()
                .any(|o| o.edit.is_none() && sv_keys.contains(o.folder.key()))
    };
    // A later repair, or the undo of one, that touched one of the folders this undo
    // writes back: on this target, or on a client its primitive does not name (M4-D31
    // fails closed); with a context, also by the resources of its paths (M4-D62).
    let repaired = |e: &HistoryEntry| -> BTreeSet<FolderName> {
        let mut folders: BTreeSet<FolderName> = e
            .repairs
            .iter()
            .filter(|r| r.target.is_none_or(|t| t == key))
            .flat_map(RepairEntry::addon_folders)
            .filter(|f| keys.contains(f.key()))
            .collect();
        if let Some(resources) = &resources {
            for repair in &e.repairs {
                folders.extend(resources.touched(repair).0);
            }
        }
        folders
    };
    let repaired_elsewhere = |e: &HistoryEntry| {
        resources
            .as_ref()
            .is_some_and(|resources| e.repairs.iter().any(|r| resources.touched(r).1))
    };
    let touches = |e: &HistoryEntry| {
        e.ops.iter().any(conflicts)
            || (whole && matches!(e.kind, TxKind::Undo) && may_have_restored(e))
            || restored_sv_of(e)
            || !repaired(e).is_empty()
            || repaired_elsewhere(e)
    };
    let undid = |e: &HistoryEntry| matches!(e.kind, TxKind::Undo) && e.reverts == Some(reverted.tx);
    let conflicting: Vec<&HistoryEntry> = later
        .iter()
        .copied()
        .filter(|e| undid(e) || (touches(e) && !cancelled.contains(&e.tx)))
        .collect();
    if let Some(newest) = conflicting.last() {
        let message = if undid(newest) {
            format!(
                "transaction {} was already undone by {}",
                reverted.tx, newest.tx
            )
        } else if whole {
            format!(
                "{} later transaction(s) wrote settings after the restore {}; the newest is {}",
                conflicting.len(),
                reverted.tx,
                newest.tx
            )
        } else {
            format!(
                "{} later transaction(s) changed the folders of {}; the newest is {}",
                conflicting.len(),
                reverted.tx,
                newest.tx
            )
        };
        return Err(undo_first(
            reverted,
            newest,
            message,
            &conflicting,
            &conflicts,
            &repaired,
        ));
    }
    // Rule 2: a later whole restore put settings in place that this undo would overwrite.
    if restores_sv && !whole {
        let restored: Vec<&HistoryEntry> = later
            .iter()
            .copied()
            .filter(|e| restores_wtf(e) && !cancelled.contains(&e.tx))
            .collect();
        if let Some(newest) = restored.last() {
            let message = format!(
                "{} restored WTF/ after {}; undoing {} would put its older SavedVariables back over the restored ones",
                newest.tx, reverted.tx, reverted.tx
            );
            let wtf = |o: &HistoryOp| matches!(o.op, FolderOp::Wtf);
            let refusal = undo_first(reverted, newest, message, &restored, &wtf, &|_| {
                BTreeSet::new()
            });
            // A follow-up restores SavedVariables only: keeping them leaves nothing.
            return Err(match sv_only {
                Some(_) => refusal,
                None => refusal.alternative(Suggestion::command([
                    "omaddons".to_owned(),
                    "undo".to_owned(),
                    reverted.tx.to_string(),
                    "--saved-variables".to_owned(),
                    "keep".to_owned(),
                ])),
            });
        }
    }
    Ok(())
}

/// Whether `entry` put a whole `WTF/` in place: a restore, or the undo of one.
fn restores_wtf(entry: &HistoryEntry) -> bool {
    entry.ops.iter().any(|o| matches!(o.op, FolderOp::Wtf))
}

/// Whether `entry` put only addons' SavedVariables back: a partial restore, or the undo
/// of one (M3-D27).
fn settings_only(entry: &HistoryEntry) -> bool {
    !entry.ops.is_empty()
        && entry
            .ops
            .iter()
            .all(|o| matches!(o.op, FolderOp::SavedVariables))
}

/// Whether the undo `entry` may have written SavedVariables: it says it restored some, or
/// it is from before M3 and does not say (M3-D28: unknown counts as "may have").
fn may_have_restored(entry: &HistoryEntry) -> bool {
    entry.sv_restored != Some(0)
}

/// The `undo_conflict` refusal of undoing `reverted` while `conflicting` stand: undo
/// `newest` first. `details` lists each one's folders: those of its ops that `counts`,
/// then those its repair operations touched (`repaired`, in folder-key order).
fn undo_first(
    reverted: &HistoryEntry,
    newest: &HistoryEntry,
    message: String,
    conflicting: &[&HistoryEntry],
    counts: &dyn Fn(&HistoryOp) -> bool,
    repaired: &dyn Fn(&HistoryEntry) -> BTreeSet<FolderName>,
) -> Error {
    let listed: Vec<serde_json::Value> = conflicting
        .iter()
        .map(|e| {
            let mut folders: Vec<String> = Vec::new();
            let ops = e.ops.iter().filter(|o| counts(o)).map(|o| o.folder.clone());
            for folder in ops.chain(repaired(e)) {
                if !folders
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(folder.as_str()))
                {
                    folders.push(folder.as_str().to_owned());
                }
            }
            serde_json::json!({ "tx": e.tx, "kind": e.kind, "folders": folders })
        })
        .collect();
    Error::new(
        ErrorCode::UndoConflict,
        message,
        Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            newest.tx.to_string(),
        ]),
    )
    .details(serde_json::json!({ "tx": reverted.tx, "conflicting": listed }))
}

/// What a history line this build cannot read may have touched on a target (M3-D28 rule
/// 5): every SavedVariables file of it, and these folders.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Touched {
    /// Another target's line.
    Nothing,
    /// Folder keys (lowercase).
    Folders(BTreeSet<String>),
    /// Every folder: it is not JSON, names no folder, or names `WTF`.
    Everything,
}

/// What `line` may have touched on `target`, read from its raw JSON: only a `target`
/// that names another key rules it out, and only `ops[].folder` strings limit its
/// folders. Whatever cannot be read is assumed touched.
fn touched_by(line: &UnreadableLine, target: TargetKey) -> Touched {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&line.raw) else {
        return Touched::Everything;
    };
    let on = value
        .get("target")
        .and_then(serde_json::Value::as_str)
        .and_then(|t| t.parse::<TargetKey>().ok());
    // ubs:ignore — comparing target keys (public ULIDs), not secrets
    if on.is_some_and(|t| t != target) {
        return Touched::Nothing;
    }
    let Some(ops) = value.get("ops").and_then(serde_json::Value::as_array) else {
        return Touched::Everything;
    };
    let mut folders = BTreeSet::new();
    for op in ops {
        match op.get("folder").and_then(serde_json::Value::as_str) {
            Some(folder) if !folder.eq_ignore_ascii_case("WTF") => {
                folders.insert(folder.to_ascii_lowercase());
            }
            _ => return Touched::Everything,
        }
    }
    if folders.is_empty() {
        Touched::Everything
    } else {
        Touched::Folders(folders)
    }
}

/// M3-D28 rule 5: `undo_conflict` (`details.reason: "history_unreadable"`) when a
/// history line this build cannot read, newer than `entries[at]` (`reverted`), may have
/// touched what the undo writes back: one of its folders (refused outright), or, when
/// the undo `restores` SavedVariables, any of them (refused, with `--saved-variables
/// keep` as the alternative). A conflict the history hides is never assumed away.
fn check_unreadable(
    unreadable: &[UnreadableLine],
    at: usize,
    reverted: &HistoryEntry,
    restores: bool,
) -> Result<()> {
    let key = target_of(reverted)?;
    let newer: Vec<Touched> = unreadable
        .iter()
        .filter(|line| line.after > at)
        .map(|line| touched_by(line, key))
        .filter(|touched| *touched != Touched::Nothing)
        .collect();
    if newer.is_empty() {
        return Ok(());
    }
    let keys: BTreeSet<&str> = reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none())
        .map(|o| o.folder.key())
        .collect();
    let overlap = newer.iter().any(|touched| match touched {
        Touched::Everything => !keys.is_empty(),
        Touched::Folders(folders) => folders.iter().any(|f| keys.contains(f.as_str())),
        Touched::Nothing => false,
    });
    if !overlap && !restores {
        return Ok(());
    }
    Err(unreadable_conflict(reverted, newer.len(), overlap))
}

/// [`check_unreadable`]'s refusal of undoing `reverted`, for `count` lines that may have
/// touched its folders (`overlap`) or only its SavedVariables.
fn unreadable_conflict(reverted: &HistoryEntry, count: usize, overlap: bool) -> Error {
    let what = if overlap {
        "its folders"
    } else {
        "its SavedVariables"
    };
    let plural = if count == 1 { "" } else { "s" };
    let conflict = Error::new(
        ErrorCode::UndoConflict,
        format!(
            "history.jsonl has {count} newer line{plural} this omaddons cannot read; they may have changed {what} since {}, so undoing it could overwrite that",
            reverted.tx
        ),
        Suggestion::text("a newer omaddons wrote that history; undo it with that version"),
    )
    .details(serde_json::json!({
        "tx": reverted.tx,
        "reason": "history_unreadable",
        "lines": count,
    }));
    // Keeping the SavedVariables avoids the conflict only when no folder is in question.
    if overlap {
        conflict
    } else {
        conflict.alternative(Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            reverted.tx.to_string(),
            "--saved-variables".to_owned(),
            "keep".to_owned(),
        ]))
    }
}

/// The transactions of `later` that cancel out: each one reverted by a later undo that is
/// still in effect, and that undo. Walked newest first, so an undo undone in turn
/// cancels nothing (the transaction it reverted counts again). An undo of `reverted`
/// itself is never cancelled here: it means `reverted` was already undone. The repair
/// family's in-effect rule (`repair::undo::undo_state`) reads it too.
pub(crate) fn cancelled_after(later: &[HistoryEntry], reverted: TxId) -> BTreeSet<TxId> {
    let present: BTreeSet<TxId> = later.iter().map(|e| e.tx).collect();
    let mut cancelled: BTreeSet<TxId> = BTreeSet::new();
    for e in later.iter().rev() {
        if cancelled.contains(&e.tx) {
            continue;
        }
        if let (TxKind::Undo, Some(of)) = (e.kind, e.reverts)
            && of != reverted
            && present.contains(&of)
        {
            cancelled.insert(of);
            cancelled.insert(e.tx);
        }
    }
    cancelled
}

/// A whole `WTF/` restore reached the folder undo, which it never should: `undo` sends it
/// to the restore's own undo first.
fn restore_undo_unsupported(reverted: &HistoryEntry) -> Error {
    Error::internal(format!(
        "transaction {} put settings back, which this omaddons cannot undo yet",
        reverted.tx
    ))
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
pub(super) fn pruned(tx: &TxId) -> Error {
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
        // In-place edits were undone with the folders; they claim no folder now. A
        // partial restore's folders stay `saved_variables` ops.
        let ops = || {
            reverted.ops.iter().filter(|o| {
                o.edit.is_none() && (o.op.is_folder() || matches!(o.op, FolderOp::SavedVariables))
            })
        };
        Self {
            history: ops()
                .map(|o| {
                    let settings = matches!(o.op, FolderOp::SavedVariables);
                    HistoryOp {
                        op: if settings {
                            FolderOp::SavedVariables
                        } else {
                            FolderOp::Record
                        },
                        folder: o.folder.clone(),
                        addon: o.addon.clone(),
                        edit: None,
                        backup: None,
                        from: settings.then_some(crate::state::history::WtfSource::Snapshot),
                        files: None,
                    }
                })
                .collect(),
            touched: ops().fold(Vec::new(), |mut touched: Vec<FolderName>, o| {
                if !touched.iter().any(|f| f.key() == o.folder.key()) {
                    touched.push(o.folder.clone());
                }
                touched
            }),
            ..Self::default()
        }
    }

    /// The inverse of every folder op of `reverted`, against AddOns now. A live folder's
    /// digest comes from the files `index`, as the install planner takes it, so the
    /// swap need not hash it under the lock.
    fn inverse(
        reverted: &HistoryEntry,
        client: &ClientPaths,
        target: &Target,
        index: Option<&FilesIndex>,
    ) -> Result<Self> {
        let addons = client.addons();
        let snapshots = snapshot_of(client, &reverted.tx).join("AddOns");
        let mut plan = Self::default();
        let old_tree = |live: &FolderName| {
            index?
                .folders
                .get(live.key())
                .map(crate::state::files_index::FolderIndex::digest)
        };
        let loaded = |live: &FolderName| {
            DiskFolder::new(addons.join(live.as_str()))
                .ok()
                .and_then(|view| loaded_toc(&view, &target.rules).ok().flatten())
                .map(|l| l.file)
        };
        // History never records a client folder (the ownership check refused it); an
        // entry naming one is refused before anything moves (D14).
        let mutable = |f: &FolderName| {
            f.clone().into_mutable().map_err(|f| {
                Error::new(
                    ErrorCode::ProtectedFolder,
                    format!(
                        "{} names the client's own folder {}; undo never changes it",
                        reverted.tx,
                        f.as_str()
                    ),
                    Suggestion::command(["omaddons", "history"]),
                )
            })
        };
        // In-place `required_by` edits move no folder and touch nothing: `restored_state`
        // reverses them on the record.
        for op in reverted.ops.iter().filter(|o| o.edit.is_none()) {
            let folder = &op.folder;
            // Once each: a handover names its folder in two ops.
            if !plan.touched.iter().any(|f| f.key() == folder.key()) {
                plan.touched.push(folder.clone());
            }
            // A partial restore's folder (or its undo's): no folder moves, its
            // SavedVariables come back as any undo's do, and the undo's own snapshot of
            // them makes it undoable in turn (M3-D27).
            if matches!(op.op, FolderOp::SavedVariables) {
                plan.history.push(HistoryOp {
                    op: FolderOp::SavedVariables,
                    folder: folder.clone(),
                    addon: op.addon.clone(),
                    edit: None,
                    backup: None,
                    from: Some(crate::state::history::WtfSource::Snapshot),
                    files: None,
                });
                continue;
            }
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
                        live: mutable(&live)?,
                        old_tree: old_tree(&live),
                    }),
                    Some(PolicyOp::RemoveFolder {
                        loaded_toc_before: loaded(&live),
                        folder: live,
                    }),
                    FolderOp::Remove,
                ),
                (FolderOp::Replace, Some(live)) => (
                    Some(SwapOp::Replace {
                        name: mutable(folder)?,
                        live: mutable(&live)?,
                        old_tree: old_tree(&live),
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
                        name: mutable(folder)?,
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
                // A whole `WTF/` put back: `undo` sent it to the restore's own undo.
                (FolderOp::Wtf | FolderOp::SavedVariables, _) => {
                    return Err(restore_undo_unsupported(reverted));
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
                Some(SwapOp::Add { name }) => summary.added.push(name.as_folder().clone()),
                Some(SwapOp::Replace { live, .. }) => {
                    summary.replaced.push(live.as_folder().clone());
                }
                Some(SwapOp::Remove { live, .. }) => {
                    summary.removed.push(live.as_folder().clone());
                }
                None => summary.recorded.push(folder.clone()),
            }
            plan.swap.extend(swap);
            plan.policy.extend(policy);
            plan.history.push(HistoryOp {
                op: history,
                folder: folder.clone(),
                addon: op.addon.clone(),
                edit: None,
                backup: None,
                from: None,
                files: None,
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
/// snapshot gets the later transactions' in-place edits replayed onto it, then keeps
/// only `required_by` names still recorded. The generation goes up. `entries[at]` is
/// the reverted transaction.
fn restored_state(
    paths: &Paths,
    current: &TargetRecord,
    entries: &[HistoryEntry],
    at: usize,
    folders: &FolderPlan,
) -> Result<Restored> {
    let reverted = &entries[at];
    let key = target_of(reverted)?;
    let snapshot = state_snapshot_of(paths, &reverted.tx);
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

    // The AddOns folders it touched: a partial restore's folders had only their
    // SavedVariables put back, so their files index and owners stay as they are now.
    let touched: BTreeSet<&str> = reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none() && o.op.is_folder())
        .map(|o| o.folder.key())
        .collect();
    let mut direct: Vec<String> = Vec::new();
    for op in reverted
        .ops
        .iter()
        .filter(|o| o.edit.is_none() && o.op.is_folder())
    {
        let addon_key = TargetRecord::addon_key(&op.addon);
        if !direct.contains(&addon_key) {
            direct.push(addon_key);
        }
    }
    // Records that come back from the snapshot, whose `required_by` may name addons
    // gone since (and, for `from_snapshot`, lacks what later transactions edited).
    let mut restored: Vec<String> = Vec::new();
    let mut from_snapshot: Vec<String> = Vec::new();
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
                from_snapshot.push(addon_key.clone());
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
    replay_later_edits(&mut record, &from_snapshot, entries, at);
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
        apply_history_edit(addon, &inverse);
        folders.sort_by(|a, b| a.key().cmp(b.key()));
        folders.dedup();
        for folder in &folders {
            ops.push(HistoryOp {
                op: FolderOp::Record,
                folder: folder.clone(),
                addon: addon.name.clone(),
                edit: Some(inverse.clone()),
                backup: None,
                from: None,
                files: None,
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

/// Applies an in-place `required_by` edit to `addon`.
fn apply_history_edit(addon: &mut AddonRecord, edit: &HistoryEdit) {
    match edit {
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
}

/// Applies the in-place `required_by` edits that transactions after `entries[at]` made
/// on its target, in history order, to each addon of `from_snapshot` (records the undo
/// brings back from before `entries[at]`): such a record keeps what later transactions
/// did to it (M2-D35). A later transaction that a later undo reverted cancels out with
/// that undo, as in conflict detection ([`cancelled_after`]).
fn replay_later_edits(
    record: &mut TargetRecord,
    from_snapshot: &[String],
    entries: &[HistoryEntry],
    at: usize,
) {
    let reverted = &entries[at];
    // Undo selects no target-less line (`target_of`), so this never returns early.
    let Some(key) = reverted.target else {
        return;
    };
    let later = &entries[at + 1..];
    let cancelled = cancelled_after(later, reverted.tx);
    // Lines on the same target only: a target-less line carries no record edits.
    let edits = later
        .iter()
        .filter(|e| e.target == Some(key) && !cancelled.contains(&e.tx))
        .flat_map(|e| &e.ops)
        .filter_map(|op| Some((TargetRecord::addon_key(&op.addon), op.edit.as_ref()?)));
    for (key, edit) in edits {
        if !from_snapshot.contains(&key) {
            continue;
        }
        if let Some(addon) = record.addons.get_mut(&key) {
            apply_history_edit(addon, edit);
        }
    }
}

// ---------------------------------------------------------------------------------------
// SavedVariables.

/// What happens to the reverted transaction's SavedVariables.
#[derive(Debug, Clone, Default)]
struct SvPlan {
    counts: SvCounts,
    /// The changed files to restore (empty when deferred, kept or refused).
    restore: Vec<SvEntry>,
    /// The transaction's SavedVariables files outside the client directory or under a
    /// folder spelled two ways, which the undo leaves as they are.
    out_of_scope: SvSkipped,
}

/// Compares each file of the transaction's manifest with the file now and applies the
/// policy. While the client runs: `auto` defers, `restore` refuses with `game_running`.
/// A file whose `SavedVariables` directory now resolves outside the client, or sits
/// under a folder now spelled two ways, is neither compared nor restored (a snapshot
/// from before such files were left out can hold one), so nothing after `swapped` can be
/// refused for its path.
fn plan_saved_variables(
    client: &ClientPaths,
    reverted: &HistoryEntry,
    policy: SvPolicy,
    running: bool,
    policy_ctx: &PolicyContext<'_>,
    ctx: &EngineCtx<'_>,
    hashes: &mut SvHashes,
) -> Result<SvPlan> {
    let manifest_path = snapshot_of(client, &reverted.tx).join("WTF.manifest.json");
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
    let mut skipped = SvSkipped::default();
    let mut gone = 0;
    for entry in manifest.entries {
        let live = wtf.join(relative(&entry.path)?);
        let skip = live
            .parent()
            .map_or(Some(SvSkip::Symlinked), |sv| sv_skip(client, sv));
        if let Some(skip) = skip {
            let exists = std::fs::symlink_metadata(&live).is_ok_and(|m| m.is_file());
            skipped.count(skip, usize::from(entry.sha256.is_some() || exists));
            continue;
        }
        // Its `SavedVariables` directory is gone (a deleted character, a reset `WTF/`):
        // undo never creates one, so the file is left as it is, never a restore that
        // fails after `swapped` and waits forever.
        if entry.sha256.is_some() && !live.parent().is_some_and(Path::is_dir) {
            gone += 1;
            continue;
        }
        let now = hashes.present_hash(&live)?;
        if now == entry.sha256 {
            plan.counts.unchanged += 1;
        } else {
            changed.push(entry);
        }
    }
    // And the files a newer snapshot left out in the first place.
    let folders: Vec<FolderName> = reverted
        .ops
        .iter()
        .filter(|op| {
            op.edit.is_none() && (op.op.is_folder() || matches!(op.op, FolderOp::SavedVariables))
        })
        .map(|op| op.folder.clone())
        .collect();
    plan.out_of_scope = skipped.max(sv_candidates(client, &folders).skipped);
    plan.counts.kept = gone;
    if changed.is_empty() {
        return Ok(plan);
    }
    match policy {
        SvPolicy::Keep => plan.counts.kept += changed.len(),
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

/// The command that finishes a deferred SavedVariables restore of `reverted`'s undo.
pub(super) fn sv_follow_up(reverted: &TxId) -> Suggestion {
    Suggestion::command([
        "omaddons".to_owned(),
        "undo".to_owned(),
        reverted.to_string(),
        "--saved-variables".to_owned(),
        "restore".to_owned(),
    ])
}

/// How many files of `sv` are not back to their state before `sv.from`; one that
/// cannot be read counts as not back.
pub(super) fn unrestored(client: &ClientPaths, sv: &SvRestore) -> usize {
    let wtf = client.wtf();
    sv.files
        .iter()
        .filter(|entry| {
            relative(&entry.path)
                .and_then(|rel| present_hash(&wtf.join(rel)))
                .map_or(true, |now| now != entry.sha256)
        })
        .count()
}

/// SHA-256s of SavedVariables files, each with the stamp its file had when hashed, so
/// a file whose stamp has not changed is not read again (D24). Only settled stamps that
/// held while their file was hashed are kept.
#[derive(Debug, Default)]
pub(super) struct SvHashes(BTreeMap<PathBuf, (FileStamp, String)>);

impl SvHashes {
    /// The hashes a SavedVariables prestage took of the files it copied.
    fn absorb(&mut self, prestage: &SvPrestage) {
        for (path, stamp, sha256) in prestage.hashes() {
            self.0
                .insert(path.to_path_buf(), (stamp, sha256.to_owned()));
        }
    }

    /// [`present_hash`], reading the file only when its stamp is not one it was hashed
    /// at here.
    fn present_hash(&mut self, path: &Path) -> Result<Option<String>> {
        let read_at = Timestamp::now().as_nanosecond();
        let stamp = match FileStamp::read(path) {
            Ok(Some(stamp)) if stamp.is_file() => stamp,
            _ => return Ok(None),
        };
        if let Some((known, sha256)) = self.0.get(path)
            && *known == stamp
        {
            return Ok(Some(sha256.clone()));
        }
        let sha256 = present_hash(path)?;
        if let Some(sha256) = &sha256
            && stamp.settled(read_at)
            && FileStamp::read(path).ok().flatten() == Some(stamp)
        {
            self.0.insert(path.to_path_buf(), (stamp, sha256.clone()));
        }
        Ok(sha256)
    }
}

/// Copies of the files a restore brings back, cloned from the reverted snapshot before
/// the global lock and checked against its manifest: by manifest path, with the SHA-256
/// each was checked for.
#[derive(Debug, Default)]
pub(super) struct SvCopies(BTreeMap<String, (String, PathBuf)>);

impl SvCopies {
    /// The checked copy of `entry`'s file, while it is still there.
    fn get(&self, entry: &SvEntry) -> Option<&Path> {
        let (checked, path) = self.0.get(&entry.path)?;
        let current = entry
            .sha256
            .as_deref()
            .is_some_and(|want| want.eq_ignore_ascii_case(checked));
        let there = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
        (current && there).then_some(path.as_path())
    }
}

/// Before the lock: clones each file `before` restores that existed before the reverted
/// transaction out of its snapshot, into `<undo_snapshot>/sv-restore/<attempt>/`, and
/// checks the copy's hash against the manifest. A copy that does not match means the
/// snapshot was changed or damaged: the undo fails `state_corrupt` before anything
/// moves.
fn prestage_restores(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    before: &Planned,
    undo_snapshot: &Path,
) -> Result<SvCopies> {
    let attempt = ulid::Ulid::generate().to_string();
    let mut copies = SvCopies::default();
    for entry in &before.sv.restore {
        let Some(expected) = &entry.sha256 else {
            continue;
        };
        let rel = relative(&entry.path)?;
        let (staged, copy_hash) =
            clone_restore(s, client, &before.reverts, undo_snapshot, &attempt, &rel)
                .map_err(not_restored)?;
        let copy_hash = match copy_hash {
            Some(copy_hash) => copy_hash,
            None => sha256_file(&staged).map_err(not_restored)?,
        };
        if !copy_hash.eq_ignore_ascii_case(expected) {
            return Err(snapshot_mismatch(&before.reverts, before.sv_only));
        }
        copies
            .0
            .insert(entry.path.clone(), (expected.clone(), staged));
    }
    Ok(copies)
}

/// Clones `rel` out of `from`'s SavedVariables snapshot into
/// `<undo_snapshot>/sv-restore/<attempt>/`: the copy, and its SHA-256 when the session
/// hashed it.
fn clone_restore(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    from: &TxId,
    undo_snapshot: &Path,
    attempt: &str,
    rel: &Path,
) -> Result<(PathBuf, Option<String>)> {
    let source = snapshot_of(client, from).join("WTF").join(rel);
    let staged = undo_snapshot.join("sv-restore").join(attempt).join(rel);
    if let Some(parent) = staged.parent() {
        ensure_dir_all(s, parent)?;
    }
    let op = Op::CloneFile {
        from: &source,
        policy: ClonePolicy::Preferred,
    };
    let cloned = mutate(s, &staged, op)?;
    let copy_hash = cloned.after_sha256().map(str::to_owned);
    Ok((staged, copy_hash))
}

/// A SavedVariables copy in `from`'s snapshot that its manifest does not describe. The
/// fix reverts the folders without it, unless only SavedVariables were left to restore.
fn snapshot_mismatch(from: &TxId, sv_only: bool) -> Error {
    let fix = if sv_only {
        Suggestion::text(
            "the SavedVariables cannot be restored from this snapshot; they stay as they are",
        )
    } else {
        Suggestion::command([
            "omaddons".to_owned(),
            "undo".to_owned(),
            from.to_string(),
            "--saved-variables".to_owned(),
            "keep".to_owned(),
        ])
    };
    Error::new(
        ErrorCode::StateCorrupt,
        format!(
            "a SavedVariables file in the snapshot of {from} no longer matches its manifest, so undo cannot restore it"
        ),
        fix,
    )
    .details(serde_json::json!({ "tx": from, "reason": "snapshot_mismatch" }))
}

/// An error that never names the file: its path holds account and character names.
fn not_restored(e: Error) -> Error {
    Error::new(
        e.code,
        "a SavedVariables file could not be restored",
        Suggestion::command(["omaddons", "status"]),
    )
}

/// Brings each file of `sv` back to its state before `sv.from`; a file already there
/// is skipped, so roll-forward can run it again. A file that existed comes back from its
/// checked copy in `copies` (else a copy cloned now) by an exchange with the live one:
/// no backup copy is needed, and the live file stays at the copy's path, inside the
/// undo's snapshot. A rename replaces it instead where the filesystem cannot exchange
/// or nothing is there. A file that did not exist moves into the undo's snapshot
/// (`undo_snapshot`). Files are hashed through `hashes`, so one unchanged since its last
/// hash is not read again.
pub(super) fn restore_saved_variables(
    s: &mut WriteSession<'_>,
    client: &ClientPaths,
    sv: &SvRestore,
    undo_snapshot: &Path,
    copies: &SvCopies,
    hashes: &mut SvHashes,
) -> Result<usize> {
    let wtf = client.wtf();
    let attempt = ulid::Ulid::generate().to_string();
    let restore = Restore {
        client,
        from: &sv.from,
        undo_snapshot,
        attempt: &attempt,
        copies,
    };
    let mut restored = 0;
    for entry in &sv.files {
        let live = wtf.join(relative(&entry.path)?);
        let now = hashes.present_hash(&live)?;
        if now == entry.sha256 {
            continue;
        }
        restore_one(s, &restore, entry, now.as_deref()).map_err(not_restored)?;
        restored += 1;
    }
    Ok(restored)
}

/// Where [`restore_one`] takes files from and puts them.
struct Restore<'a> {
    client: &'a ClientPaths,
    /// The reverted transaction.
    from: &'a TxId,
    /// The undo's client snapshot.
    undo_snapshot: &'a Path,
    attempt: &'a str,
    copies: &'a SvCopies,
}

/// One file of [`restore_saved_variables`], whose live file's SHA-256 is now
/// `live_sha256` (`None`: absent).
fn restore_one(
    s: &mut WriteSession<'_>,
    r: &Restore<'_>,
    entry: &SvEntry,
    live_sha256: Option<&str>,
) -> Result<()> {
    let rel = relative(&entry.path)?;
    let live = r.client.wtf().join(&rel);
    let known = |sha256: Option<&str>| sha256.and_then(sha256_from_hex);
    let Some(expected) = entry.sha256.as_deref() else {
        // The reverted transaction's code created it: it moves into the undo's snapshot.
        let away = r.undo_snapshot.join("sv-created").join(&rel);
        if let Some(parent) = away.parent() {
            ensure_dir_all(s, parent)?;
        }
        let aside = Op::Rename {
            to: &away,
            mode: RenameMode::Replace,
        };
        let opts = MutateOpts {
            sha256_before: known(live_sha256),
            ..MutateOpts::default()
        };
        mutate_with(s, &live, aside, opts)?;
        return Ok(());
    };
    let (staged, staged_sha256) = match r.copies.get(entry) {
        Some(copy) => (copy.to_path_buf(), Some(expected.to_owned())),
        None => clone_restore(s, r.client, r.from, r.undo_snapshot, r.attempt, &rel)?,
    };
    let staged_known = known(staged_sha256.as_deref());
    if live_sha256.is_some() {
        let exchange = Op::Rename {
            to: &live,
            mode: RenameMode::Exchange,
        };
        let opts = MutateOpts {
            sha256_before: staged_known,
            sha256_after: known(live_sha256),
            ..MutateOpts::default()
        };
        match mutate_with(s, &staged, exchange, opts) {
            Ok(_) => return Ok(()),
            // No exchange on this filesystem: a rename replaces it, after a backup copy.
            Err(FsxError::Unsupported) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let replace = Op::Rename {
        to: &live,
        mode: RenameMode::Replace,
    };
    let opts = MutateOpts {
        sha256_before: staged_known,
        ..MutateOpts::default()
    };
    mutate_with(s, &staged, replace, opts)?;
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
    use crate::engine::testing::{TreeEntry, TxScene};
    use crate::fsx::failpoint;

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

    /// A whole restore's history line on `target`.
    fn restore_line(target: TargetKey) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind: TxKind::Restore,
            at: Timestamp::now(),
            target: Some(target),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into(), "backup".into(), "restore".into()],
            ops: vec![HistoryOp {
                op: FolderOp::Wtf,
                folder: FolderName::new("WTF").unwrap(),
                addon: String::new(),
                edit: None,
                backup: Some(crate::model::BackupId::new()),
                from: Some(history::WtfSource::Backup),
                files: Some(3),
            }],
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
        }
    }

    /// A line on `target` of `kind` with one op per `(op, folder)`.
    fn line(target: TargetKey, kind: TxKind, ops: &[(FolderOp, &str)]) -> HistoryEntry {
        HistoryEntry {
            kind,
            ops: ops
                .iter()
                .map(|(op, folder)| HistoryOp {
                    op: *op,
                    folder: FolderName::new(folder).unwrap(),
                    addon: (*folder).to_owned(),
                    edit: None,
                    backup: None,
                    from: None,
                    files: None,
                })
                .collect(),
            ..restore_line(target)
        }
    }

    /// An undo on `target` of `of` that restored `sv_restored` SavedVariables files.
    fn undo_line(target: TargetKey, of: TxId, sv_restored: Option<u64>) -> HistoryEntry {
        HistoryEntry {
            reverts: Some(of),
            sv_restored,
            ..line(target, TxKind::Undo, &[(FolderOp::Replace, "Other")])
        }
    }

    /// The tx `check_conflicts` names to undo first, or `None` when there is no conflict.
    fn first(
        entries: &[HistoryEntry],
        at: usize,
        sv_only: Option<usize>,
        sv: bool,
    ) -> Option<TxId> {
        check_conflicts(entries, at, sv_only, sv).err().map(|err| {
            assert_eq!(err.code, ErrorCode::UndoConflict);
            err.fix.argv().unwrap()[2].parse().unwrap()
        })
    }

    /// M3-D28 rule 1: undoing a whole restore conflicts with every later write of
    /// settings on its target, an undo from before M3 counting as one; folders alone, an
    /// undo that restored nothing, or another target's restore do not.
    #[test]
    fn undoing_a_restore_conflicts_with_every_later_write_of_settings() {
        let key = TargetKey::new();
        let restore = restore_line(key);
        let with = |later: HistoryEntry| vec![restore.clone(), later];
        for quiet in [
            line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]),
            undo_line(key, TxId::new(), Some(0)),
            restore_line(TargetKey::new()),
        ] {
            assert_eq!(
                first(&with(quiet.clone()), 0, None, true),
                None,
                "{quiet:?}"
            );
        }
        for writes in [
            restore_line(key),
            line(key, TxKind::Restore, &[(FolderOp::SavedVariables, "Foo")]),
            undo_line(key, TxId::new(), Some(2)),
            undo_line(key, TxId::new(), None),
        ] {
            let tx = writes.tx;
            let entries = with(writes);
            assert_eq!(first(&entries, 0, None, true), Some(tx), "{:?}", entries[1]);
            let err = check_conflicts(&entries, 0, None, true).unwrap_err();
            assert!(
                err.message.contains("wrote settings after the restore"),
                "{}",
                err.message
            );
        }
        // Undone by its own undo, the later restore counts no more.
        let later = restore_line(key);
        let undone = undo_line(key, later.tx, None);
        assert_eq!(
            first(&[restore.clone(), later, undone], 0, None, true),
            None
        );
    }

    /// M3-D28 rule 2: undoing a transaction whose SavedVariables it would restore, after a
    /// later whole restore, names that restore to undo first, keeping them the
    /// alternative; a follow-up gets no `keep`; a restore undone since does not count.
    #[test]
    fn undoing_settings_from_before_a_restore_names_the_restore_first() {
        let key = TargetKey::new();
        let install = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let restore = restore_line(key);
        let entries = [install.clone(), restore.clone()];
        assert_eq!(
            first(&entries, 0, None, false),
            None,
            "keep, or no manifest"
        );
        let err = check_conflicts(&entries, 0, None, true).unwrap_err();
        assert_eq!(err.fix.argv().unwrap()[2], restore.tx.to_string());
        let keep = err.alternatives[0].argv().unwrap();
        assert_eq!(
            keep[2..],
            [
                install.tx.to_string(),
                "--saved-variables".into(),
                "keep".into()
            ]
        );
        assert_eq!(
            err.details["conflicting"][0]["folders"],
            serde_json::json!(["WTF"])
        );

        // The deferred follow-up: refused the same way, with nothing to keep.
        let deferred = HistoryEntry {
            sv_deferred: true,
            ..undo_line(key, install.tx, Some(0))
        };
        let entries = [install.clone(), deferred, restore.clone()];
        let err = check_conflicts(&entries, 0, Some(1), true).unwrap_err();
        assert_eq!(err.fix.argv().unwrap()[2], restore.tx.to_string());
        assert!(err.alternatives.is_empty());

        // The restore was undone: nothing stands in the way.
        let undone = undo_line(key, restore.tx, None);
        assert_eq!(first(&[install, restore, undone], 0, None, true), None);
    }

    /// M3-D28 rule 3: a partial restore of a folder's SavedVariables conflicts as a write
    /// of them, both ways, and with nothing else: a later one with undoing an earlier
    /// transaction of that folder whose undo restores SavedVariables, and undoing one
    /// with a later write of that folder's SavedVariables.
    #[test]
    fn a_partial_restore_conflicts_where_saved_variables_are_written() {
        let key = TargetKey::new();
        let install = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let partial =
            |folder: &str| line(key, TxKind::Restore, &[(FolderOp::SavedVariables, folder)]);
        let foo = partial("Foo");
        // A later partial restore of Foo, over an install of Foo whose undo would restore
        // Foo's SavedVariables: undo the restore first. Keeping them, nothing conflicts;
        // and Bar's settings are another folder's.
        assert_eq!(
            first(&[install.clone(), foo.clone()], 0, None, true),
            Some(foo.tx)
        );
        assert_eq!(first(&[install.clone(), foo.clone()], 0, None, false), None);
        assert_eq!(
            first(&[install.clone(), partial("Bar")], 0, None, true),
            None
        );
        // Undoing the partial restore after a later write of Foo's SavedVariables: another
        // partial restore of it, or an undo that restored some and touched Foo.
        let again = partial("Foo");
        assert_eq!(
            first(&[foo.clone(), again.clone()], 0, None, true),
            Some(again.tx)
        );
        let restoring = HistoryEntry {
            reverts: Some(TxId::new()),
            sv_restored: Some(2),
            ..line(key, TxKind::Undo, &[(FolderOp::Remove, "Foo")])
        };
        assert_eq!(
            first(&[foo.clone(), restoring.clone()], 0, None, true),
            Some(restoring.tx)
        );
        // Not after an undo that restored none, an update that wrote no SavedVariables,
        // or a partial restore of another folder.
        let quiet = HistoryEntry {
            sv_restored: Some(0),
            ..restoring
        };
        let update = line(key, TxKind::Update, &[(FolderOp::Replace, "Foo")]);
        for later in [quiet, update, partial("Bar")] {
            assert_eq!(
                first(&[foo.clone(), later.clone()], 0, None, true),
                None,
                "{later:?}"
            );
        }
        // A whole restore before it: rule 1, whatever its folders.
        let whole = restore_line(key);
        assert_eq!(first(&[whole, foo.clone()], 0, None, false), Some(foo.tx));
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

    /// The AddOns tree: modes, symlinks and empty directories as well as file bytes.
    fn tree(scene: &TxScene) -> Vec<TreeEntry> {
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

    /// Foo v1 installed over SavedVariables the game then changed, then undone while the
    /// client ran: its folders reverted, its SavedVariables restore deferred. Returns v1's
    /// transaction.
    fn deferred_undo(scene: &mut TxScene) -> TxId {
        saved_variables(scene);
        let t1 = install(scene, "v1.zip", V1);
        play(scene);
        let dir = scene.client.client_dir().to_path_buf();
        scene.running.insert(dir.clone());
        scene.while_running = true;
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(done.sv.deferred, 2);
        scene.running.remove(&dir);
        scene.while_running = false;
        t1
    }

    /// Installs a one-folder addon `name` that shares nothing with Foo.
    fn unrelated(scene: &TxScene, name: &str) {
        let toc = format!("{name}/{name}.toc");
        let zip = scene.zip(
            &format!("{name}.zip"),
            &[(toc.as_str(), "## Interface: 120105\n")],
        );
        let r = format!("gh:o/{}", name.to_ascii_lowercase());
        let done = scene.run(vec![scene.install(&r, &zip)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
    }

    /// Whether both halves of `tx`'s snapshot are still there after retention runs
    /// eight days from now: past the 24-hour floor and the 7-day window.
    fn kept_eight_days_on(scene: &TxScene, tx: &TxId) -> bool {
        let scopes = ScopeTable::new(
            &scene.paths,
            &[scene.client.client_dir().to_path_buf()],
            None,
        )
        .unwrap();
        let later = Timestamp::from_second(Timestamp::now().as_second() + 8 * 86_400).unwrap();
        crate::engine::retention::run(
            &scene.paths,
            &scopes,
            record(scene).key,
            &scene.client,
            &crate::engine::retention::RetentionLimits::default(),
            later,
            &|| Ok(BTreeSet::new()),
        )
        .unwrap();
        scene.client.snapshots(tx).is_dir() && scene.paths.state_snapshot(tx).is_dir()
    }

    /// The same, by the pass over every client (bmcx.3), which reaches the scene's client
    /// as one a commit on another client found.
    fn swept_eight_days_on(scene: &TxScene, tx: &TxId) -> bool {
        let mut target = scene.current_target().target;
        target.key = Some(record(scene).key);
        let later = Timestamp::from_second(Timestamp::now().as_second() + 8 * 86_400).unwrap();
        let swept = crate::engine::retention::sweep(
            &scene.paths,
            crate::fsx::Writer::Mutating,
            &[target],
            None,
            &crate::engine::retention::RetentionLimits::default(),
            later,
        );
        assert!(swept.ran && swept.warnings.is_empty(), "{swept:?}");
        scene.client.snapshots(tx).is_dir() && scene.paths.state_snapshot(tx).is_dir()
    }

    #[test]
    fn retention_keeps_what_a_deferred_restore_needs_until_it_can_no_longer_run() {
        // Pending: kept past every age rule, and the follow-up then restores the files.
        let mut scene = TxScene::new();
        let t1 = deferred_undo(&mut scene);
        unrelated(&scene, "Bar");
        unrelated(&scene, "Baz");
        assert_eq!(pending_sv_restores(&history(&scene)), BTreeSet::from([t1]));
        assert!(swept_eight_days_on(&scene, &t1), "pinned, for the pass too");
        assert!(kept_eight_days_on(&scene, &t1), "pinned");
        scene.undo(UndoSelector::Tx(t1), SvPolicy::Restore).unwrap();
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- before")
        );
        unrelated(&scene, "Qux");
        assert!(pending_sv_restores(&history(&scene)).is_empty());
        assert!(!kept_eight_days_on(&scene, &t1), "the follow-up ran");

        // A later transaction changed those folders: the follow-up cannot run any more.
        let mut scene = TxScene::new();
        let t1 = deferred_undo(&mut scene);
        install(&scene, "v2.zip", V2);
        unrelated(&scene, "Bar");
        assert!(pending_sv_restores(&history(&scene)).is_empty());
        assert!(!kept_eight_days_on(&scene, &t1), "Foo changed since");

        // The deferred undo was undone.
        let mut scene = TxScene::new();
        let t1 = deferred_undo(&mut scene);
        scene.undo(UndoSelector::Latest, SvPolicy::Keep).unwrap();
        unrelated(&scene, "Bar");
        assert!(pending_sv_restores(&history(&scene)).is_empty());
        assert!(!kept_eight_days_on(&scene, &t1), "its undo was undone");
    }

    fn mtime(scene: &TxScene, rel: &str) -> std::time::SystemTime {
        std::fs::metadata(wtf(scene, rel))
            .unwrap()
            .modified()
            .unwrap()
    }

    /// Journals, locks and stage dirs, and the snapshot dirs of transactions that did not
    /// commit (no history line).
    fn leftovers(scene: &TxScene) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for dir in [
            scene.paths.journal_dir(),
            scene.paths.inflight_dir(),
            scene.paths.cache_inflight_dir(),
            scene.client.meta_dir().join("stage"),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(listing.map(|e| e.unwrap().path()));
            }
        }
        let committed: std::collections::BTreeSet<String> =
            history(scene).iter().map(|e| e.tx.to_string()).collect();
        for dir in [
            scene.client.meta_dir().join("snapshots"),
            scene.paths.state_snapshots_dir(),
        ] {
            if let Ok(listing) = std::fs::read_dir(&dir) {
                found.extend(
                    listing
                        .map(|e| e.unwrap())
                        .filter(|e| !committed.contains(e.file_name().to_str().unwrap()))
                        .map(|e| e.path()),
                );
            }
        }
        found
    }

    /// An undo `apply` runs (M4-D25) records the plan; the install it reverts does not.
    #[test]
    fn an_undo_while_applying_records_the_plan() {
        let mut scene = TxScene::new();
        let t1 = install(&scene, "v1.zip", V1);
        let plan_id: crate::model::PlanId = "01M9P0000000000000000000PA".parse().unwrap();
        scene.applying = Some(plan_id);
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(done.undoes, t1);
        let lines = history(&scene);
        let found: Vec<(TxKind, Option<crate::model::PlanId>)> =
            lines.iter().map(|l| (l.kind, l.plan)).collect();
        assert_eq!(
            found,
            [(TxKind::Install, None), (TxKind::Undo, Some(plan_id))],
            "{lines:#?}"
        );
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

    /// The undo of a records-only change writes no settings, so it snapshots none:
    /// undoing it in turn (the pin again) rewinds no play.
    #[test]
    fn undoing_the_undo_of_a_pin_rewinds_no_settings() {
        use crate::engine::edit::RecordEdit;
        let scene = TxScene::new();
        saved_variables(&scene);
        install(&scene, "v1.zip", V1);
        scene
            .run(vec![scene.edit("Foo", RecordEdit::SetPinned(true))])
            .unwrap();
        let unpinned = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert!(!record(&scene).addons["foo"].pinned);
        play(&scene);
        let again = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(again.undoes, unpinned.outcome.tx.unwrap());
        assert!(record(&scene).addons["foo"].pinned, "pinned again");
        assert_eq!(again.sv.restored, 0);
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after"),
            "the play since stays"
        );
    }

    #[test]
    fn a_later_transaction_undone_in_turn_is_no_conflict() {
        use crate::engine::edit::RecordEdit;
        let tx_of = |done: crate::engine::tx::TxOutcomes| done.outcomes[0].tx.unwrap();
        // Install, update, undo the update: the install undoes as if the update never was.
        let scene = TxScene::new();
        let empty = tree(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        let t2 = install(&scene, "v2.zip", V2);
        scene.undo(UndoSelector::Tx(t2), SvPolicy::Auto).unwrap();
        scene.undo(UndoSelector::Tx(t1), SvPolicy::Auto).unwrap();
        assert_eq!(tree(&scene), empty);
        assert!(record(&scene).addons.is_empty());

        // Unpin (records only), update, undo the update: the unpin undoes (the pin is
        // back), and the folders stay as the update's undo left them.
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        scene
            .run(vec![scene.edit("Foo", RecordEdit::SetPinned(true))])
            .unwrap();
        let unpin = tx_of(
            scene
                .run(vec![scene.edit("Foo", RecordEdit::SetPinned(false))])
                .unwrap(),
        );
        let v1 = tree(&scene);
        let update = install(&scene, "v2.zip", V2);
        scene
            .undo(UndoSelector::Tx(update), SvPolicy::Auto)
            .unwrap();
        scene.undo(UndoSelector::Tx(unpin), SvPolicy::Auto).unwrap();
        assert!(record(&scene).addons["foo"].pinned);
        assert_eq!(tree(&scene), v1);

        // The update's undo undone in turn: the update stands again, a conflict.
        let scene = TxScene::new();
        let t1 = install(&scene, "v1.zip", V1);
        let t2 = install(&scene, "v2.zip", V2);
        let u2 = scene.undo(UndoSelector::Tx(t2), SvPolicy::Auto).unwrap();
        let u2 = u2.outcome.tx.unwrap();
        scene.undo(UndoSelector::Tx(u2), SvPolicy::Auto).unwrap();
        let err = scene
            .undo(UndoSelector::Tx(t1), SvPolicy::Auto)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict, "{err:?}");
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

    /// The `actions.jsonl` lines of `tx` that renamed onto a path ending in `to` (an
    /// attempt the filesystem refused is logged too, with `ok: false`).
    fn renames_onto(scene: &TxScene, tx: &TxId, to: &str) -> Vec<serde_json::Value> {
        let log = scene.paths.state_snapshot(tx).join("actions.jsonl");
        std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|a| {
                a["ok"] == true
                    && a["op"].as_str().unwrap().starts_with("rename")
                    && a["to"].as_str().is_some_and(|t| t.ends_with(to))
            })
            .collect()
    }

    #[cfg(debug_assertions)]
    #[test]
    fn saved_variables_come_back_by_an_exchange_or_a_rename_without_one() {
        for spec in ["", "fsx.renameat2.einval"] {
            let scene = TxScene::new();
            saved_variables(&scene);
            let t1 = install(&scene, "v1.zip", V1);
            play(&scene);
            let done =
                failpoint::with_spec(spec, || scene.undo(UndoSelector::Latest, SvPolicy::Auto))
                    .unwrap();
            assert_eq!(done.sv.restored, 2, "{spec}");
            assert_eq!(
                read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
                Some("-- before"),
                "{spec}"
            );
            assert_eq!(read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")), None);
            let undo_tx = done.outcome.tx.unwrap();
            let renames = renames_onto(&scene, &undo_tx, &format!("{ACCOUNT}/Foo.lua"));
            assert_eq!(renames.len(), 1, "{spec}: {renames:?}");
            let rename = &renames[0];
            if spec.is_empty() {
                // Swapped with the live file: no backup copy, and the played file stays
                // in the undo's snapshot where the copy was.
                assert_eq!(rename["op"], "rename_exchange");
                assert_eq!(rename["backup"], serde_json::Value::Null);
                let displaced = std::path::Path::new(rename["path"].as_str().unwrap());
                assert!(displaced.starts_with(scene.client.snapshots(&undo_tx)));
                assert_eq!(std::fs::read_to_string(displaced).unwrap(), "-- after");
            } else {
                assert_eq!(rename["op"], "rename_replace", "{spec}");
                assert!(rename["backup"].is_string(), "{spec}");
            }
            // The restored copy's digest is the manifest's, checked before the lock.
            let manifest: SvManifest = serde_json::from_slice(
                &std::fs::read(scene.client.snapshots(&t1).join("WTF.manifest.json")).unwrap(),
            )
            .unwrap();
            let entry = manifest
                .entries
                .iter()
                .find(|e| e.path == format!("{ACCOUNT}/Foo.lua"))
                .unwrap();
            let sha256 = entry.sha256.as_deref().unwrap();
            assert_eq!(rename["before_hash"], format!("sha256:{sha256}"), "{spec}");
            assert!(
                leftovers(&scene).is_empty(),
                "{spec}: {:?}",
                leftovers(&scene)
            );
        }
    }

    #[test]
    fn a_saved_variables_snapshot_that_no_longer_matches_fails_before_anything_moves() {
        let scene = TxScene::new();
        saved_variables(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        play(&scene);
        let copy = scene
            .client
            .snapshots(&t1)
            .join("WTF")
            .join(ACCOUNT)
            .join("Foo.lua");
        std::fs::write(&copy, "-- damaged").unwrap();
        let before = tree(&scene);
        let err = scene
            .undo(UndoSelector::Latest, SvPolicy::Auto)
            .unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::StateCorrupt, 1));
        let keep = [
            "omaddons".to_owned(),
            "undo".to_owned(),
            t1.to_string(),
            "--saved-variables".to_owned(),
            "keep".to_owned(),
        ];
        assert_eq!(err.fix.argv().unwrap(), keep);
        assert_eq!(err.details["reason"], "snapshot_mismatch");
        for private in ["123456", "RealmName", "CharName"] {
            assert!(!err.message.contains(private), "{}", err.message);
        }
        assert_eq!(tree(&scene), before, "no folder moved");
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after")
        );
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));

        // The fix reverts the folders and leaves the SavedVariables as they are.
        let done = scene.undo(UndoSelector::Tx(t1), SvPolicy::Keep).unwrap();
        assert_eq!((done.sv.kept, done.sv.restored), (2, 0));
        assert!(!scene.client.addons().join("Foo").exists());
    }

    #[test]
    fn saved_variables_are_hashed_again_only_when_they_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Foo.lua");
        std::fs::write(&path, "-- one").unwrap();
        // Older than the racy slack.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut hashes = SvHashes::default();
        let hashed = crate::fsx::files_hashed();
        let first = hashes.present_hash(&path).unwrap();
        assert!(first.is_some());
        assert_eq!(hashes.present_hash(&path).unwrap(), first);
        assert_eq!(
            crate::fsx::files_hashed() - hashed,
            1,
            "unchanged: read once"
        );
        std::fs::write(&path, "-- two, longer").unwrap();
        let second = hashes.present_hash(&path).unwrap();
        assert!(second.is_some() && second != first);
        assert_eq!(crate::fsx::files_hashed() - hashed, 2);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(hashes.present_hash(&path).unwrap(), None);
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

    /// A folder only the snapshot holds comes back as it was: directory modes, the 0600
    /// file, the symlink and the empty directory.
    #[test]
    fn undoing_a_forced_replace_restores_a_hand_made_folder_as_it_was() {
        let mut scene = TxScene::new();
        crate::engine::testing::hand_made(&scene.client.addons().join("Hand"));
        let before = tree(&scene);
        scene.force = true;
        install_as(
            &scene,
            "gh:o/hand",
            "hand.zip",
            &[(
                "Hand/Hand.toc",
                "## Interface: 120105\n## Notes: packaged\n",
            )],
        );
        assert_ne!(tree(&scene), before);
        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(tree(&scene), before);
    }

    #[test]
    fn undoing_a_remove_restores_an_adopted_hand_made_folder_as_it_was() {
        let scene = TxScene::new();
        let hand = scene.client.addons().join("Hand");
        crate::engine::testing::hand_made(&hand);
        // A folder with a symlink inside cannot be indexed, so it is never adopted.
        std::fs::remove_file(hand.join("link.lua")).unwrap();
        let adopt = scene.adopt("Hand");
        let done = scene
            .run_as(vec![adopt], Some(TxKind::Adopt), TxKind::Install)
            .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let before = tree(&scene);
        let done = scene.run(vec![scene.remove("hand")]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        assert!(!hand.exists());
        scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(tree(&scene), before);
    }

    #[cfg(debug_assertions)]
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
            "engine.snapshot.prestaged",
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
            // An error after the commit point keeps the committed undo's snapshot.
            let lost: Vec<TxId> = history(&scene)
                .into_iter()
                .map(|e| e.tx)
                .filter(|tx| !scene.paths.state_snapshot(tx).is_dir())
                .collect();
            assert!(lost.is_empty(), "{name}: {lost:?}");
        }
    }

    /// An undo completed by the in-process roll-forward reports the swap its journal
    /// recorded, as an install does.
    #[cfg(debug_assertions)]
    #[test]
    fn a_rolled_forward_undo_reports_the_swap_it_journaled() {
        for (spec, atomic) in [
            ("engine.record.written=error", true),
            ("fsx.renameat2.einval,engine.record.written=error", false),
            ("fsx.renameat2.einval,engine.swapped=error", false),
        ] {
            let scene = TxScene::new();
            install(&scene, "v1.zip", V1);
            let installed = tree(&scene);
            install(&scene, "v2.zip", V2);
            let done =
                failpoint::with_spec(spec, || scene.undo(UndoSelector::Latest, SvPolicy::Auto))
                    .unwrap();
            let unavailable = done
                .outcome
                .warnings
                .iter()
                .filter(|w| matches!(w.code, WarningCode::AtomicSwapUnavailable))
                .count();
            assert_eq!(
                (done.outcome.atomic_swap, unavailable),
                (atomic, usize::from(!atomic)),
                "{spec}: {:?}",
                done.outcome.warnings
            );
            assert_eq!(tree(&scene), installed, "{spec}: rolled forward");
        }
    }

    #[cfg(debug_assertions)]
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
    fn saved_variables_moved_out_of_the_client_are_left_and_counted() {
        // Snapshotted in place, then WTF/ moved to a sync folder and symlinked back: the
        // manifest names files that now resolve outside the client.
        let scene = TxScene::new();
        saved_variables(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        play(&scene);
        let sync = tempfile::tempdir().unwrap();
        let wtf = scene.client.wtf();
        let away = sync.path().join("WTF");
        std::fs::rename(&wtf, &away).unwrap();
        std::os::unix::fs::symlink(&away, &wtf).unwrap();

        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(done.undoes, t1);
        assert!(
            !scene.client.addons().join("Foo").exists(),
            "folders reverted"
        );
        assert_eq!((done.sv.restored, done.sv.deferred), (0, 0));
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- after"),
            "left as it is"
        );
        let warning = done
            .outcome
            .warnings
            .iter()
            .find(|w| w.code == WarningCode::SavedVariablesOutOfScope)
            .unwrap();
        // Account Foo.lua (changed), its .lua.bak (created) and the character's Foo.lua.
        assert!(
            warning.message.starts_with("3 SavedVariables files"),
            "{}",
            warning.message
        );
        assert!(warning.message.ends_with("undo left them as they are"));
        assert!(record(&scene).addons.is_empty());
        assert_eq!(history(&scene).pop().unwrap().reverts, Some(t1));
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_undo_whose_recovery_fails_keeps_its_journal_for_the_next_command() {
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let v1 = tree(&scene);
        let t2 = install(&scene, "v2.zip", V2);
        let v2 = tree(&scene);
        let spec = "engine.swap.after_exchange=error,engine.recover.step=error";
        let err = failpoint::with_spec(spec, || scene.undo(UndoSelector::Latest, SvPolicy::Auto))
            .unwrap_err();
        assert!(
            err.message.contains("recovering it failed too"),
            "{}",
            err.message
        );
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "status"]);
        let tx: TxId = err.details["journal_pending"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(scene.paths.journal(&tx).is_file() && scene.client.stage(&tx).is_dir());
        assert_ne!(tree(&scene), v2, "the undo is half done");

        // The next undo recovers it (back to v2), then undoes t2 itself.
        let done = scene.undo(UndoSelector::Latest, SvPolicy::Auto).unwrap();
        assert_eq!(done.undoes, t2);
        assert!(
            done.outcome
                .warnings
                .iter()
                .any(|w| w.code == WarningCode::JournalRecovered)
        );
        assert_eq!(tree(&scene), v1);
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_in_process_roll_forward_that_cannot_restore_saved_variables_defers_them() {
        let scene = TxScene::new();
        saved_variables(&scene);
        let t1 = install(&scene, "v1.zip", V1);
        play(&scene);
        let spec = "engine.swapped=error,engine.recover.sv=error";
        let done = failpoint::with_spec(spec, || scene.undo(UndoSelector::Latest, SvPolicy::Auto))
            .unwrap();
        assert!(
            !scene.client.addons().join("Foo").exists(),
            "folders reverted"
        );
        assert_eq!((done.sv.restored, done.sv.deferred), (0, 2));
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
        assert_eq!(
            warning.fix.as_ref().unwrap().argv().unwrap(),
            sv_follow_up(&t1).argv().unwrap()
        );
        assert!(history(&scene).pop().unwrap().sv_deferred);

        // The follow-up restores them.
        let done = scene.undo(UndoSelector::Tx(t1), SvPolicy::Restore).unwrap();
        assert_eq!(done.sv.restored, 2);
        assert_eq!(
            read(&scene, &format!("{ACCOUNT}/Foo.lua")).as_deref(),
            Some("-- before")
        );
        assert_eq!(read(&scene, &format!("{ACCOUNT}/Foo.lua.bak")), None);
        assert!(leftovers(&scene).is_empty(), "{:?}", leftovers(&scene));
    }

    #[cfg(debug_assertions)]
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
            assert!(check_conflicts(&entries, at, None, false).is_ok());
            // A later change to R's folder does.
            install_as(
                &scene,
                "gh:o/req",
                "r2.zip",
                &[("Req/Req.toc", "## Interface: 120105\n## Version: 2\n")],
            );
            let entries = history(&scene);
            assert_eq!(
                check_conflicts(&entries, at, None, false).unwrap_err().code,
                ErrorCode::UndoConflict
            );
        }

        /// D (required by Req) updated to v2 as tx2; then S installed as tx3.
        fn scene_with_update() -> (TxScene, TxId) {
            let (scene, _) = scene_with_requirement();
            let tx2 = install_as(&scene, "gh:o/dep", "d2.zip", D2);
            install_as(
                &scene,
                "gh:o/s",
                "s.zip",
                &[("S/S.toc", "## Interface: 120105\n")],
            );
            (scene, tx2)
        }

        #[test]
        fn a_restored_record_keeps_the_requirements_added_after_it() {
            let (scene, tx2) = scene_with_update();
            ok(
                &scene,
                vec![scene.edit("Dep", RecordEdit::AddRequiredBy("S".into()))],
            );
            assert_eq!(required_by(&scene), ["Req", "S"]);
            let undo2 = undo(&scene, tx2);
            let dep = &record(&scene).addons["dep"];
            assert_eq!(dep.version, "1");
            assert_eq!(
                dep.required_by,
                ["Req", "S"],
                "S's later requirement replayed"
            );
            // Undoing the undo restores the state before it exactly.
            undo(&scene, undo2);
            let dep = &record(&scene).addons["dep"];
            assert_eq!(
                (dep.version.as_str(), &dep.required_by[..]),
                ("2", &["Req".to_owned(), "S".to_owned()][..])
            );
        }

        #[test]
        fn later_edits_replay_in_order_and_undone_ones_cancel() {
            // Added, then dropped again: not there.
            let (scene, tx2) = scene_with_update();
            let add = |s: &TxScene| {
                ok(
                    s,
                    vec![s.edit("Dep", RecordEdit::AddRequiredBy("S".into()))],
                )
            };
            add(&scene);
            ok(
                &scene,
                vec![scene.edit("Dep", RecordEdit::RemoveRequiredBy("S".into()))],
            );
            undo(&scene, tx2);
            assert_eq!(required_by(&scene), ["Req"]);

            // Added by a transaction since undone: not there either.
            let (scene, tx2) = scene_with_update();
            let tx3 = add(&scene);
            undo(&scene, tx3);
            undo(&scene, tx2);
            assert_eq!(required_by(&scene), ["Req"]);

            // That undo undone in turn: the requirement holds again.
            let (scene, tx2) = scene_with_update();
            let tx3 = add(&scene);
            let undo3 = undo(&scene, tx3);
            undo(&scene, undo3);
            undo(&scene, tx2);
            assert_eq!(required_by(&scene), ["Req", "S"]);
        }

        #[test]
        fn undoing_a_dependency_install_that_a_later_addon_requires_warns_still_required() {
            // D installed as tx0; Req, installed after it, requires it.
            let scene = TxScene::new();
            let tx0 = install_as(&scene, "gh:o/dep", "d1.zip", D1);
            let zip = scene.zip("r.zip", R);
            ok(
                &scene,
                vec![
                    scene.install("gh:o/req", &zip),
                    scene.edit("Dep", RecordEdit::AddRequiredBy("Req".into())),
                ],
            );
            let still = |warnings: &[Warning]| -> Vec<Warning> {
                warnings
                    .iter()
                    .filter(|w| w.code == WarningCode::StillRequired)
                    .cloned()
                    .collect()
            };
            let planned = scene
                .undo_with(UndoSelector::Tx(tx0), SvPolicy::Auto, true)
                .unwrap();
            let warned = still(&planned.outcome.warnings);
            assert_eq!(warned.len(), 1, "{:?}", planned.outcome.warnings);
            assert!(
                warned[0].fix.as_ref().unwrap().argv().is_none(),
                "dry run: text"
            );

            let done = scene.undo(UndoSelector::Tx(tx0), SvPolicy::Auto).unwrap();
            let warned = still(&done.outcome.warnings);
            assert_eq!(warned.len(), 1, "{:?}", done.outcome.warnings);
            assert_eq!(
                warned[0].message,
                "On retail, Dep is required by Req, which will not load without it"
            );
            let undo_tx = done.outcome.tx.unwrap().to_string();
            assert_eq!(
                warned[0].fix.as_ref().unwrap().argv().unwrap(),
                ["omaddons", "undo", undo_tx.as_str()]
            );
            let after = record(&scene);
            assert!(!after.addons.contains_key("dep") && after.addons.contains_key("req"));
        }
    }

    /// A repair line (M4-D31) on `key`: target-less, its client in `targets`.
    fn repair_line(key: TargetKey) -> HistoryEntry {
        HistoryEntry {
            kind: TxKind::Repair,
            target: None,
            targets: vec![key],
            ops: Vec::new(),
            argv: vec!["omaddons".into(), "doctor".into(), "fix".into()],
            run: Some(RunId::new()),
            doctor_run_id: Some("2026-10-25T12:00:00Z__01M9Z00000000000000000000".into()),
            ..restore_line(key)
        }
    }

    fn plant(scene: &TxScene, line: &HistoryEntry) {
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        crate::fsx::sys::append_durable(&scene.paths.history_file(), &bytes).unwrap();
    }

    #[test]
    fn undo_refuses_a_target_less_line_before_anything_moves() {
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let repair = repair_line(scene.record().key);
        plant(&scene, &repair);
        let before = crate::engine::testing::tree_snapshot(scene.client.client_dir());
        for (name, selector) in [
            ("latest", UndoSelector::Latest),
            ("tx", UndoSelector::Tx(repair.tx)),
            ("last_by", UndoSelector::LastBy(Actor::Human)),
        ] {
            // A repair line reaches the repair undo (E7), which finds nothing to invert
            // in one that names no operation.
            let err = scene.undo(selector, SvPolicy::Auto).unwrap_err();
            assert_eq!(err.code, ErrorCode::NotFound, "{name}: {err:?}");
            assert!(
                err.message.contains(&repair.tx.to_string())
                    && err.message.contains("changed no file"),
                "{name}: {}",
                err.message
            );
            assert_eq!(
                err.fix.argv(),
                Some(&["omaddons".to_owned(), "history".to_owned()][..])
            );
            let journals = std::fs::read_dir(scene.paths.journal_dir())
                .map(|dir| dir.count())
                .unwrap_or(0);
            assert_eq!(journals, 0, "{name}: no journal was started");
            assert_eq!(
                crate::engine::testing::tree_snapshot(scene.client.client_dir()),
                before,
                "{name}: nothing moved"
            );
        }
    }

    #[test]
    fn a_target_less_line_is_never_a_pending_saved_variables_restore() {
        let key = TargetKey::new();
        let repair = repair_line(key);
        // A synthetic undo naming the repair, as if it had deferred a restore.
        let undo = HistoryEntry {
            sv_deferred: true,
            ..undo_line(key, repair.tx, Some(0))
        };
        let pending = pending_sv_restores(&[repair.clone(), undo]);
        assert!(!pending.contains(&repair.tx), "{pending:?}");
    }

    #[test]
    fn later_edits_replay_only_from_lines_on_the_same_target() {
        let scene = TxScene::new();
        install(&scene, "v1.zip", V1);
        let mut record = scene.record();
        let key = record.key;
        let (addon_key, name) = record
            .addons
            .iter()
            .next()
            .map(|(k, a)| (k.clone(), a.name.clone()))
            .unwrap();
        let edit = |requester: &str| HistoryOp {
            op: FolderOp::Record,
            folder: FolderName::new("Foo").unwrap(),
            addon: name.clone(),
            edit: Some(HistoryEdit::RequiredByAdd(requester.into())),
            backup: None,
            from: None,
            files: None,
        };
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        // A target-less line carrying a record edit anyway (built in memory only).
        let target_less = HistoryEntry {
            ops: vec![edit("Req")],
            ..repair_line(key)
        };
        let on_target = HistoryEntry {
            ops: vec![edit("Other")],
            ..line(key, TxKind::Install, &[])
        };
        replay_later_edits(
            &mut record,
            std::slice::from_ref(&addon_key),
            &[reverted, target_less, on_target],
            0,
        );
        let required_by = &record.addons[&addon_key].required_by;
        assert!(
            required_by.iter().any(|r| r == "Other") && !required_by.iter().any(|r| r == "Req"),
            "only the line on the target replays: {required_by:?}"
        );
    }

    /// One repair primitive: `(target, folder, path, to)`.
    type Primitive<'a> = (Option<TargetKey>, Option<&'a str>, &'a str, Option<&'a str>);

    /// A repair line on `targets` with one primitive per `(target, folder, path, to)`.
    fn repair_with(targets: &[TargetKey], primitives: &[Primitive<'_>]) -> HistoryEntry {
        use crate::state::history::{PredicateKind, RepairEndpoints, RepairOp, RepairPredicate};
        let absent = || RepairPredicate {
            kind: PredicateKind::Absent,
            identity: None,
            sha256: None,
            mode: None,
            stamp: None,
            link_target: None,
        };
        let repairs = primitives
            .iter()
            .zip(1..)
            .map(|(&(target, folder, path, to), seq)| RepairEntry {
                seq,
                fix: 1,
                fm: "fm-external_artifacts-nested-addon-folder".into(),
                op: RepairOp::RenameNoReplace,
                path: path.into(),
                to: to.map(Into::into),
                mode: None,
                pre: RepairEndpoints::Path(absent()),
                post: RepairEndpoints::Path(absent()),
                backup: None,
                activation: false,
                guards: Vec::new(),
                target,
                folder: folder.map(|f| FolderName::new(f).unwrap()),
            })
            .collect();
        HistoryEntry {
            targets: targets.to_vec(),
            repairs,
            ..repair_line(TargetKey::new())
        }
    }

    #[test]
    fn a_later_repair_of_one_of_its_folders_conflicts() {
        let key = TargetKey::new();
        let other = TargetKey::new();
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let in_foo = repair_with(
            &[key],
            &[(
                Some(key),
                Some("Foo"),
                "/c/Interface/AddOns/Foo/x.lua",
                None,
            )],
        );
        let undo_of_it = HistoryEntry {
            kind: TxKind::Undo,
            reverts: Some(in_foo.tx),
            ..repair_with(
                &[key],
                &[(
                    Some(key),
                    Some("Foo"),
                    "/c/Interface/AddOns/Foo/x.lua",
                    None,
                )],
            )
        };
        let with = |later: &[HistoryEntry]| {
            let mut entries = vec![reverted.clone()];
            entries.extend_from_slice(later);
            entries
        };
        let conflicts = [
            ("folder_field_on_target", in_foo.clone()),
            (
                "rename_path_out_of_folder",
                repair_with(
                    &[key],
                    &[(
                        Some(key),
                        None,
                        "/c/Interface/AddOns/Foo/Inner",
                        Some("/c/Interface/AddOns/Inner"),
                    )],
                ),
            ),
            (
                "rename_to_into_folder",
                repair_with(
                    &[key],
                    &[(
                        Some(key),
                        None,
                        "/c/Interface/AddOns/Wrap/Foo",
                        Some("/c/Interface/AddOns/Foo"),
                    )],
                ),
            ),
            (
                "primitive_names_no_client",
                repair_with(
                    &[key],
                    &[(None, Some("Foo"), "/c/Interface/AddOns/Foo", None)],
                ),
            ),
            (
                "every_line_with_folder",
                repair_with(&[], &[(None, Some("Foo"), "/c/Interface/AddOns/Foo", None)]),
            ),
        ];
        for (name, repair) in conflicts {
            let entries = with(std::slice::from_ref(&repair));
            let err = check_conflicts(&entries, 0, None, false).unwrap_err();
            assert_eq!(err.code, ErrorCode::UndoConflict, "{name}: {repair:?}");
            assert_eq!(
                err.details["conflicting"][0],
                serde_json::json!({"tx": repair.tx, "kind": "repair", "folders": ["Foo"]}),
                "{name}: {repair:?} gave {}",
                err.details
            );
            assert_eq!(
                first(&entries, 0, None, false),
                Some(repair.tx),
                "{name}: the fix"
            );
        }

        let quiet: [(&str, Vec<HistoryEntry>); 5] = [
            (
                "other_folder",
                with(&[repair_with(
                    &[key],
                    &[(Some(key), Some("Bar"), "/c/Interface/AddOns/Bar", None)],
                )]),
            ),
            (
                "other_target",
                with(&[repair_with(
                    &[other],
                    &[(Some(other), Some("Foo"), "/o/Interface/AddOns/Foo", None)],
                )]),
            ),
            (
                "home_side_only",
                with(&[repair_with(
                    &[key],
                    &[(None, None, "/cfg/credentials/github", None)],
                )]),
            ),
            ("repair_then_its_undo", with(&[in_foo.clone(), undo_of_it])),
            ("older_repair", vec![in_foo.clone(), reverted.clone()]),
        ];
        for (name, entries) in quiet {
            let at = entries.iter().position(|e| e.tx == reverted.tx).unwrap();
            let output = check_conflicts(&entries, at, None, false)
                .err()
                .map(|e| e.details.clone());
            assert_eq!(output, None, "{name}: {:?}", &entries[at + 1..]);
        }

        // A whole restore's undo writes back no AddOns folder: a repair never conflicts.
        let restore = restore_line(key);
        let entries = vec![restore, in_foo];
        let output = check_conflicts(&entries, 0, None, true)
            .err()
            .map(|e| e.details.clone());
        assert_eq!(output, None, "whole_restore_undo");
    }

    /// A home's paths, for the resource rule's cases: its client and another's.
    struct ConflictHome {
        _dir: tempfile::TempDir,
        paths: Paths,
        client_dir: PathBuf,
        other_dir: PathBuf,
    }

    fn conflict_home() -> ConflictHome {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &base,
            base.join("config"),
            base.join("state"),
            base.join("cache"),
        );
        ConflictHome {
            client_dir: base.join("games/wow/_retail_"),
            other_dir: base.join("games/wow/_classic_"),
            _dir: dir,
            paths,
        }
    }

    fn text(path: PathBuf) -> String {
        path.display().to_string()
    }

    /// With its context, a later repair conflicts by the resources of its paths (M4-D62):
    /// an AddOns folder the undo writes back, its client's record or files index, or
    /// either half of the reverted transaction's snapshot, an ancestor of one included;
    /// without it (retention), only by the folders E12's rule names.
    #[test]
    fn with_its_context_a_later_repair_conflicts_by_its_resources() {
        let home = conflict_home();
        let key = TargetKey::new();
        let ctx = ConflictCtx {
            paths: &home.paths,
            client_dir: &home.client_dir,
        };
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let client = ClientPaths::new(&home.client_dir);
        let away = text(home.paths.doctor_runs_dir().join("R/quarantine/state/x"));
        let paths = &home.paths;
        let cases: Vec<(&str, String, &[&str], bool)> = vec![
            (
                "its_folder",
                text(client.addons().join("Foo/x.lua")),
                &["Foo"],
                true,
            ),
            ("its_record", text(paths.target_record(&key)), &[], false),
            ("its_files_index", text(paths.files_index(&key)), &[], false),
            (
                "an_ancestor_of_its_record",
                text(paths.target_record(&key).parent().unwrap().to_path_buf()),
                &[],
                false,
            ),
            (
                "its_client_snapshot_half",
                text(client.snapshots(&reverted.tx).join("AddOns/Foo")),
                &[],
                false,
            ),
            (
                "its_state_snapshot_half",
                text(paths.state_snapshot(&reverted.tx)),
                &[],
                false,
            ),
        ];
        for (case, path, folders, without) in cases {
            // As lowering records them: a client's folder names its client; a home-side
            // path (a record, the state half of a snapshot) names none, not even in
            // `targets`, and `touches_target` alone would never see it.
            let client_side = path.starts_with(&text(home.client_dir.clone()));
            let target = client_side.then_some(key);
            let targets: &[TargetKey] = if client_side { &[key] } else { &[] };
            let repair = repair_with(targets, &[(target, None, &path, Some(&away))]);
            let entries = vec![reverted.clone(), repair.clone()];
            let err = check_conflicts_in(&entries, 0, None, false, Some(&ctx)).unwrap_err();
            assert_eq!(err.code, ErrorCode::UndoConflict, "case {case}: {err:?}");
            assert_eq!(
                err.details["conflicting"],
                serde_json::json!([{"tx": repair.tx, "kind": "repair", "folders": folders}]),
                "case {case}: {}",
                err.details
            );
            assert_eq!(
                err.fix.argv().unwrap()[2],
                repair.tx.to_string(),
                "case {case}"
            );
            assert_eq!(
                check_conflicts(&entries, 0, None, false).is_err(),
                without,
                "case {case}: without a context, only E12's folder rule"
            );
        }
    }

    #[test]
    fn an_unrelated_later_repair_or_one_on_another_client_does_not_conflict() {
        let home = conflict_home();
        let key = TargetKey::new();
        let other = TargetKey::new();
        let ctx = ConflictCtx {
            paths: &home.paths,
            client_dir: &home.client_dir,
        };
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let others = ClientPaths::new(&home.other_dir);
        let away = text(home.paths.doctor_runs_dir().join("R/quarantine/state/x"));
        let credential = text(home.paths.credential_file("github"));
        let in_foo = repair_with(
            &[key],
            &[(
                Some(key),
                Some("Foo"),
                &text(ClientPaths::new(&home.client_dir).addons().join("Foo")),
                Some(&away),
            )],
        );
        let undo_of_it = HistoryEntry {
            kind: TxKind::Undo,
            reverts: Some(in_foo.tx),
            ..in_foo.clone()
        };
        let cases: Vec<(&str, Vec<HistoryEntry>)> = vec![
            (
                "a_credential",
                vec![repair_with(&[], &[(None, None, &credential, None)])],
            ),
            (
                "another_clients_record",
                vec![repair_with(
                    &[],
                    &[(None, None, &text(home.paths.target_record(&other)), None)],
                )],
            ),
            (
                "another_clients_folder",
                vec![repair_with(
                    &[other],
                    &[(
                        Some(other),
                        Some("Foo"),
                        &text(others.addons().join("Foo")),
                        None,
                    )],
                )],
            ),
            (
                "another_folder_of_its_client",
                vec![repair_with(
                    &[key],
                    &[(
                        Some(key),
                        Some("Bar"),
                        &text(ClientPaths::new(&home.client_dir).addons().join("Bar")),
                        None,
                    )],
                )],
            ),
            ("a_repair_its_undo_cancelled", vec![in_foo, undo_of_it]),
        ];
        for (case, later) in cases {
            let mut entries = vec![reverted.clone()];
            entries.extend(later);
            let output = check_conflicts_in(&entries, 0, None, false, Some(&ctx))
                .err()
                .map(|e| e.details.clone());
            assert_eq!(output, None, "case {case}: {:?}", &entries[1..]);
        }
    }

    /// Retention keeps a deferred restore's snapshot by E12's folder rule alone: a later
    /// repair of only the record leaves it pending there, while the follow-up itself, with
    /// its context, refuses.
    #[test]
    fn retention_keeps_the_conservative_folder_rule() {
        let home = conflict_home();
        let key = TargetKey::new();
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let deferred = HistoryEntry {
            sv_deferred: true,
            ..undo_line(key, reverted.tx, Some(0))
        };
        let away = text(home.paths.doctor_runs_dir().join("R/quarantine/state/x"));
        let repair = repair_with(
            &[],
            &[(
                None,
                None,
                &text(home.paths.target_record(&key)),
                Some(&away),
            )],
        );
        let entries = vec![reverted.clone(), deferred, repair];
        assert!(
            pending_sv_restores(&entries).contains(&reverted.tx),
            "retention keeps it"
        );
        let ctx = ConflictCtx {
            paths: &home.paths,
            client_dir: &home.client_dir,
        };
        assert_eq!(
            check_conflicts_in(&entries, 0, Some(1), true, Some(&ctx))
                .unwrap_err()
                .code,
            ErrorCode::UndoConflict,
            "the follow-up refuses"
        );
    }

    #[test]
    fn a_repair_counts_like_a_folder_change_for_a_saved_variables_follow_up() {
        let key = TargetKey::new();
        let reverted = line(key, TxKind::Install, &[(FolderOp::Add, "Foo")]);
        let deferred = HistoryEntry {
            sv_deferred: true,
            ..undo_line(key, reverted.tx, Some(0))
        };
        let repair = repair_with(
            &[key],
            &[(Some(key), Some("Foo"), "/c/Interface/AddOns/Foo", None)],
        );
        let after = vec![reverted.clone(), deferred.clone(), repair.clone()];
        assert_eq!(
            first(&after, 0, Some(1), true),
            Some(repair.tx),
            "a repair after the deferred undo conflicts with its follow-up"
        );
        let before = vec![reverted, repair, deferred];
        assert_eq!(
            first(&before, 0, Some(2), true),
            None,
            "a repair before the deferred undo does not"
        );
    }
}
