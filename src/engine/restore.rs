//! A whole-`WTF/` restore (M3-D24): one journaled `RENAME_EXCHANGE` of the client's
//! `WTF` directory with a staged copy of a backup, so `WTF/` is wholly old or wholly new
//! at every instant, the old tree kept as the transaction's `WTF.previous` for undo.
//!
//! The topology preflight runs before anything is copied: a restore needs `WTF` to be a
//! plain directory on the client's own mount, the stage on that mount too (a rename
//! across mounts fails, whatever their devices say), and a filesystem that can exchange.
//! There is no fallback to two renames: it would leave a moment with no `WTF/`, in which
//! a game launched writes a fresh default one.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::journal::{
    ClientFacts, CommitPayload, FileId, IndexChanges, Journal, Prepared, Recovered, RecoveryAction,
    RecoveryEnv, SourceBackup, Step, StepOp, StepPurpose, recover_all, recover_tx,
};
use super::retention::WTF_PREVIOUS;
use super::running::{PolicyContext, PolicyOp};
use super::swap::snapshot_state;
use super::tx::{
    EngineCtx, TxOutcome, append_alone, append_once, cleanup, millis, note_undo, recovery_failed,
    refuse_newer_state, retain, ring_after_window, running_now, sv_writable, write_status,
};
use super::undo::{SvCounts, UndoFacts, UndoOutcome, UndoRequest, WtfUndo};
use crate::backup::tree::{CloneOpts, TreeCopy, clone_tree, hash_files, walk};
use crate::backup::{BackupRef, EntryKind, Integrity, Manifest, ManifestEntry};
use crate::discover::registry::LockedRegistry;
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorCode, PlanStaleSource, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    ClonePolicy, DirLock, FsxError, GlobalLock, InflightLock, MutateOpts, Op, RenameMode,
    ScopeTable, SnapshotRoots, StampDigest, WriteSession, Writer, ensure_dir_all, failpoint,
    mount_id, mutate_with, probe_exchange,
};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::model::{FolderName, ReloadRequired, RunId, Target, TargetKey, TxId, TxKind};
use crate::privacy::WtfPathRedactor;
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryOp, TxResult, WtfSource};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::load_record;
use crate::state::store;

/// Whether the exchange a restore needs was proved where it will run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    /// Probed with two files in the transaction's stage.
    Verified,
    /// Not probed: a dry run writes nothing.
    Unverified,
}

/// What the preflight established; the commit window compares the mounts again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// The mount of the client dir.
    pub client_mount: u64,
    /// The live `WTF/`, `None` when there is none yet (the exchange is then a plain
    /// `Noreplace` into place).
    pub wtf: Option<PathBuf>,
    /// The mount the stage is on, which is the client's.
    pub stage_mount: u64,
    pub exchange: Exchange,
}

impl Topology {
    /// The client dir's mount and the stage's.
    fn mounts(&self) -> (u64, u64) {
        (self.client_mount, self.stage_mount)
    }
}

/// Where a real run probes the exchange: the transaction's own stage, which goes with
/// it (the orphan sweep reclaims it after a crash), with the run's scopes.
pub struct Probe<'a> {
    pub scopes: &'a ScopeTable,
    pub dir: &'a Path,
}

/// The topology preflight (M3-D24 step 2), before any copying: `WTF` a plain directory
/// on the client dir's mount (`wtf_is_mount_point`), the stage on that mount too
/// (`stage_cross_mount`), and, on a real run, an exchange the filesystem supports
/// (`no_rename_exchange`). A dry run passes no `probe`: it writes nothing and reports
/// the exchange `unverified`. `backup_dir` is the backup's `WTF/`, named by the refusal
/// as what a user can copy by hand.
pub fn preflight(
    client: &ClientPaths,
    backup_dir: &Path,
    probe: Option<Probe<'_>>,
) -> Result<Topology> {
    let topology = topology(client, backup_dir, &mount_id)?;
    let exchange = match probe {
        None => Exchange::Unverified,
        Some(probe) => {
            let mut s = WriteSession::untracked(Writer::BackupRestore, probe.scopes, "probe");
            match probe_exchange(&mut s, probe.dir) {
                Ok(true) => Exchange::Verified,
                Ok(false) => return Err(unsupported("no_rename_exchange", backup_dir)),
                Err(err) => return Err(err.into()),
            }
        }
    };
    Ok(Topology {
        exchange,
        ..topology
    })
}

/// [`preflight`]'s mount checks, with the mount ids `mount` gives.
pub(super) fn topology(
    client: &ClientPaths,
    backup_dir: &Path,
    mount: &dyn Fn(&Path) -> io::Result<u64>,
) -> Result<Topology> {
    let read = |path: &Path| {
        mount(path)
            .map_err(|err| Error::io(path, &err, Suggestion::command(["omaddons", "status"])))
    };
    let client_mount = read(client.client_dir())?;
    let wtf = client.wtf();
    let present = match std::fs::symlink_metadata(&wtf) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                "the client's WTF/ is a symlink, which a restore would replace by a directory",
                Suggestion::text(
                    "copy what you need from the backup folder into the folder the link points to, with the game closed",
                ),
            )
            .details(serde_json::json!({
                "reason": "wtf_symlinked",
                "backup_dir": backup_dir.display().to_string(),
            })));
        }
        Ok(meta) if meta.is_dir() => true,
        Ok(_) => {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                "the client's WTF is not a directory",
                Suggestion::text("move it aside, then restore"),
            ));
        }
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(Error::io(
                &wtf,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    // ubs:ignore — comparing mount ids, not secrets
    if present && read(&wtf)? != client_mount {
        return Err(unsupported("wtf_is_mount_point", backup_dir));
    }
    // The stage lives under `Interface/.omaddons/stage`, created on first use: what
    // decides its mount is its nearest existing ancestor.
    let stage = client.meta_dir().join("stage");
    let existing = stage
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(client.client_dir());
    let stage_mount = read(existing)?;
    if stage_mount != client_mount {
        return Err(unsupported("stage_cross_mount", backup_dir));
    }
    Ok(Topology {
        client_mount,
        wtf: present.then_some(wtf),
        stage_mount,
        exchange: Exchange::Unverified,
    })
}

/// `filesystem_unsupported` for `reason`, naming the backup folder a user can copy by
/// hand. Not retryable, and no command helps: a partial restore needs the same.
pub fn unsupported(reason: &str, backup_dir: &Path) -> Error {
    let why = match reason {
        "wtf_is_mount_point" => "the client's WTF/ is a mount point",
        "stage_cross_mount" => "the client's Interface/.omaddons/ is on another mount than WTF/",
        _ => "the client's filesystem cannot exchange two directories atomically",
    };
    Error::new(
        ErrorCode::FilesystemUnsupported,
        format!("a restore cannot run here: {why}"),
        Suggestion::text(
            "a restore needs WTF/ to be a plain directory on the same mount as Interface/, on a filesystem with atomic exchange (btrfs, ext4, xfs, tmpfs); otherwise copy what you need from the backup folder by hand with the game closed",
        ),
    )
    .details(serde_json::json!({
        "reason": reason,
        "backup_dir": backup_dir.display().to_string(),
    }))
}

// ---------------------------------------------------------------------------------------
// The transaction (M3-D24): reserve, plan and preflight (the command's), then stage
// without the lock, revalidate under it, journal, undo data first, two steps, commit.

/// The reader reservation on a backup (M3-D48): a shared `flock` on its own directory,
/// held from plan to commit, so a prune skips it as busy. No lock file is created, and
/// the kernel releases it when the process dies, so a crashed reader never pins a backup.
#[derive(Debug)]
pub struct Reservation {
    lock: DirLock,
    dir: PathBuf,
}

impl Reservation {
    /// Whether the reserved directory is still the backup's (not moved or deleted).
    pub fn holds(&self) -> bool {
        self.lock.same_dir(&self.dir)
    }
}

/// Reserves `backup`, waiting while a prune holds it; one moved or deleted is
/// `not_found`. Taken before the global lock, never waited on while holding it.
pub fn reserve(backup: &BackupRef) -> Result<Reservation> {
    let gone = || {
        Error::new(
            ErrorCode::NotFound,
            format!("backup {} is gone", backup.id),
            Suggestion::command(["omaddons", "backup", "list"]),
        )
        .details(serde_json::json!({ "backup": backup.id }))
    };
    let lock = DirLock::shared(&backup.dir).map_err(|_| gone())?;
    let reservation = Reservation {
        lock,
        dir: backup.dir.clone(),
    };
    if !reservation.holds() {
        return Err(gone());
    }
    Ok(reservation)
}

/// What a whole restore puts back, against the live `WTF/` when planned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RestoreFiles {
    /// The backup's files: what the live `WTF/` holds afterwards.
    pub restored: u64,
    /// Of those, the ones the live tree has with another size or time, or lacks.
    pub differed: u64,
    /// Live files the backup does not have: kept in `WTF.previous`.
    pub created_since: u64,
    /// The backup's files the live tree has as recorded.
    pub unchanged: u64,
}

/// A restore's plan: read only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    /// The target's record generation it was planned against (0 while unregistered).
    pub generation: u64,
    pub files: RestoreFiles,
    pub manifest: Manifest,
    /// The live `WTF/`'s stamp digest as planned (`None` without one): what a
    /// precondition token observes.
    pub observed: Option<StampDigest>,
}

/// Plans restoring `backup` onto `target`, reading only: the backup restorable and
/// structurally whole, and how the live `WTF/` compares with its manifest (recorded
/// sizes and times, never content).
pub fn plan(paths: &Paths, target: &Target, backup: &BackupRef) -> Result<RestorePlan> {
    let client = ClientPaths::new(&target.client_dir);
    let names = WtfPathRedactor::for_client(&client);
    if !backup.restorable {
        return Err(Error::new(
            ErrorCode::StateCorrupt,
            format!(
                "backup {} cannot be restored: its description or manifest does not read, or it is damaged",
                backup.id
            ),
            Suggestion::command(["omaddons", "backup", "list"]),
        ));
    }
    let check = crate::backup::check_structure(backup)?;
    if !check.complete() {
        return Err(incomplete(backup, check.first.as_deref(), &names));
    }
    let manifest = store::load::<Manifest>(&backup.dir.join("manifest.json"))?
        .map(|loaded| loaded.value)
        .unwrap_or_default();
    let live = client.wtf();
    let (live_entries, observed) = if live.is_dir() {
        let walked = walk(&live, BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH)
            .map_err(|e| e.error(&names))?;
        let stamp = StampDigest::compute(&live)
            .map_err(|e| Error::io(&live, &e, Suggestion::command(["omaddons", "status"])))?;
        (walked.entries, Some(stamp))
    } else {
        (Vec::new(), None)
    };
    let live_files: BTreeMap<&str, (u64, i64)> = live_entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| (e.path.as_str(), (e.size, e.mtime_ns)))
        .collect();
    let mut files = RestoreFiles::default();
    let mut in_backup = std::collections::BTreeSet::new();
    for entry in manifest
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
    {
        in_backup.insert(entry.path.as_str());
        files.restored += 1;
        match live_files.get(entry.path.as_str()) {
            // ubs:ignore — comparing file sizes and times, not secrets
            Some(&(size, mtime)) if size == entry.size && mtime == entry.mtime_ns => {
                files.unchanged += 1;
            }
            _ => files.differed += 1,
        }
    }
    files.created_since = live_files
        .keys()
        .filter(|p| !in_backup.contains(*p))
        .count() as u64;
    let generation = match target.key {
        Some(key) => load_record(paths, &key)?.value.generation,
        None => 0,
    };
    Ok(RestorePlan {
        generation,
        files,
        manifest,
        observed,
    })
}

/// A backup whose tree is not what its manifest says, or changed while it was staged.
pub(super) fn incomplete(
    backup: &BackupRef,
    first: Option<&str>,
    names: &WtfPathRedactor,
) -> Error {
    Error::new(
        ErrorCode::StateCorrupt,
        format!(
            "backup {} is not whole: its WTF/ differs from its manifest{}",
            backup.id,
            first
                .map(|p| format!(" (first at {})", names.rel(p)))
                .unwrap_or_default()
        ),
        Suggestion::command(["omaddons", "backup", "list"]),
    )
    .details(serde_json::json!({ "backup": backup.id }))
}

/// What the command established before the engine restores (M3-D24's plan order).
pub struct RestoreRequest<'a> {
    pub target: &'a DiscoveredTarget,
    pub backup: &'a BackupRef,
    /// Held to the commit, then released (dropped) right after the doorbell.
    pub reservation: Reservation,
    pub topology: Topology,
    pub tx: TxId,
    pub inflight: InflightLock,
    pub plan: &'a RestorePlan,
    /// The `before_restore` backup's history line: appended in the commit window before
    /// the restore's, or under a short separate lock when the restore fails first.
    pub before_restore: Option<HistoryEntry>,
    pub reason: Option<String>,
    pub run: RunId,
}

/// What a whole restore did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub tx: TxId,
    pub key: TargetKey,
    pub files: RestoreFiles,
    /// `verified` when the manifest held hashes and the staged copy matched them.
    pub integrity: Integrity,
    pub exchange: Exchange,
    pub lock_held_ms: u64,
    pub warnings: Vec<Warning>,
}

/// What goes in place and what is there now, read before the lock.
struct Staged {
    /// The staged copy; `None` when the live tree only moves out (the undo of a restore
    /// onto a client that had no `WTF/`).
    copy: Option<TreeCopy>,
    /// The live `WTF/`'s stamp digest; `None` when there is none.
    live_stamp: Option<StampDigest>,
    integrity: Integrity,
}

impl Staged {
    /// The bytes the copy wrote, where the filesystem could not clone.
    fn copied_bytes(&self) -> u64 {
        self.copy.as_ref().map_or(0, |copy| copy.copied_bytes)
    }
}

/// A staging failure of a restore of `backup` onto `target_id`, with a damaged backup
/// recorded as such ([`crate::backup::verify::damage_found`]); the reservation is still
/// held.
pub(super) fn damage_found(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    backup: &BackupRef,
    target_id: &str,
    error: Error,
) -> Error {
    let ring = crate::backup::inventory::Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    crate::backup::verify::damage_found(ctx.paths, scopes, backup, target_id, ring, error)
}

/// `backup_copied`, when `staged` wrote `bytes` to put a tree back on `target_id`.
pub(super) fn copied(target_id: &str, bytes: u64) -> Option<Warning> {
    (bytes > 0).then(|| {
        Warning::new(
            WarningCode::BackupCopied,
            format!(
                "{target_id}: the filesystem could not clone the files put back, so {bytes} bytes were copied"
            ),
        )
    })
}

/// The live `WTF/`'s stamp digest (a stat walk, before the lock); `None` without one.
fn live_stamp(topology: &Topology) -> Result<Option<StampDigest>> {
    topology
        .wtf
        .as_deref()
        .map(|live| {
            StampDigest::compute(live)
                .map_err(|e| Error::io(live, &e, Suggestion::command(["omaddons", "status"])))
        })
        .transpose()
}

/// The refusals a whole restore makes before anything under the client dir is written
/// (M3-D24), the `before_restore` backup included: a record, files index or registry a
/// newer omaddons wrote (`state_too_new`, M3-D45), then a client that runs or may
/// (`game_running`, M3-D29). The commit window checks both again.
pub fn refuse_early(ctx: &EngineCtx<'_>, target: &Target, files: u64) -> Result<()> {
    refuse_newer_state(ctx.paths, target)?;
    refuse_running(ctx, target, files)
}

/// A real restore begun (M3-D24): its transaction, in-flight lock, and the topology the
/// exchange probe proved.
pub struct Begun {
    pub tx: TxId,
    pub inflight: InflightLock,
    pub topology: Topology,
}

/// Begins a real restore of `backup` onto `client`: a transaction id, its in-flight lock,
/// and the preflight with the exchange probed in the transaction's own stage, before
/// anything is copied. A refusal takes the stage and the lock away again.
pub fn begin(paths: &Paths, client: &ClientPaths, backup: &BackupRef) -> Result<Begun> {
    let scopes = ScopeTable::new(paths, &[client.client_dir().to_path_buf()], None)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(paths, &tx)?;
    let probe = client.stage_wtf(&tx).join("probe");
    let probe = Probe {
        scopes: &scopes,
        dir: &probe,
    };
    match preflight(client, &backup.wtf(), Some(probe)) {
        Ok(topology) => Ok(Begun {
            tx,
            inflight,
            topology,
        }),
        Err(error) => {
            cleanup(paths, &scopes, inflight, &tx, Some(client), false);
            Err(error)
        }
    }
}

impl Begun {
    /// Gives a begun restore up before it stages (its `before_restore` backup failed):
    /// the stage and the in-flight lock go.
    pub fn abandon(self, paths: &Paths, client: &ClientPaths) {
        if let Ok(scopes) = ScopeTable::new(paths, &[client.client_dir().to_path_buf()], None) {
            cleanup(paths, &scopes, self.inflight, &self.tx, Some(client), false);
        }
    }
}

/// `game_running` while `target`'s client runs or may (M3-D29): a whole `WTF/` is never
/// written then, whatever the running policy or `--while-running`.
pub(super) fn refuse_running(ctx: &EngineCtx<'_>, target: &Target, files: u64) -> Result<()> {
    let running = running_now(ctx, std::slice::from_ref(target));
    if !running.dirs.contains(&target.client_dir) {
        return Ok(());
    }
    let policy_ctx = PolicyContext {
        target_id: &target.id,
        argv: ctx.argv,
        running_unknown: running.unknown.as_deref(),
    };
    let files = usize::try_from(files).unwrap_or(usize::MAX);
    super::running::evaluate(
        &policy_ctx,
        ctx.policy,
        true,
        &target.rules,
        &[PolicyOp::RestoreSavedVariables { files }],
        true,
    )
    .map(|_| ())
}

/// `WTF`, as a history op's folder.
fn wtf_folder() -> Result<FolderName> {
    FolderName::new("WTF").map_err(|_| Error::internal("WTF is a folder name"))
}

/// Restores `req.backup` over the client's whole `WTF/` (M3-D24).
pub fn restore(ctx: &EngineCtx<'_>, req: RestoreRequest<'_>) -> Result<RestoreOutcome> {
    let RestoreRequest {
        target: discovered,
        backup,
        reservation,
        topology,
        tx,
        inflight,
        plan,
        before_restore,
        reason,
        run,
    } = req;
    let target = &discovered.target;
    let client = ClientPaths::new(&target.client_dir);
    let scopes = ScopeTable::new(ctx.paths, std::slice::from_ref(&target.client_dir), None)?;
    let names = WtfPathRedactor::for_client(&client);
    let staged_dir = client.stage_wtf(&tx).join("WTF");
    let staged = match stage(ctx, &scopes, backup, plan, &staged_dir, &topology, &names) {
        Ok(staged) => staged,
        Err(error) => {
            let error = damage_found(ctx, &scopes, backup, &target.id, error);
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), false);
            append_alone(ctx, &scopes, before_restore.as_ref(), Writer::BackupRestore);
            return Err(error);
        }
    };
    let window = Window {
        ctx,
        scopes: &scopes,
        discovered,
        topology: &topology,
        tx,
        staged: &staged,
        change: WtfChange::Restore {
            backup,
            plan,
            before_restore: before_restore.as_ref(),
        },
        reason,
        run,
    };
    let result = window.commit();
    let committed = result.is_ok();
    ring_after_window(ctx, &result);
    drop(reservation);
    cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), committed);
    // A lock that timed out once is not waited on twice.
    // ubs:ignore — comparing error codes, not secrets
    if !committed && !matches!(&result, Err(e) if e.code == ErrorCode::Locked) {
        append_alone(ctx, &scopes, before_restore.as_ref(), Writer::BackupRestore);
    }
    let (key, held, mut warnings) = result?;
    warnings.extend(retain(ctx, &scopes, key, &target.client_dir));
    let ring = crate::backup::inventory::Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    crate::backup::inventory::rebuild(ctx.paths, &client, &key, ring);
    warnings.extend(copied(&target.id, staged.copied_bytes()));
    Ok(RestoreOutcome {
        tx,
        key,
        files: plan.files,
        integrity: staged.integrity,
        exchange: Exchange::Verified,
        lock_held_ms: held,
        warnings,
    })
}

/// Stages the backup's `WTF/` (no lock): an untracked copy with the manifest's exact
/// modes, deferred durability and one `syncfs`, checked against the manifest (and
/// against its hashes when it has them), then the live tree's stamp digest. Fails before
/// anything changed when the copy is not the backup.
fn stage(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    backup: &BackupRef,
    plan: &RestorePlan,
    staged: &Path,
    topology: &Topology,
    names: &WtfPathRedactor,
) -> Result<Staged> {
    let fail =
        |e: std::io::Error| Error::io(staged, &e, Suggestion::command(["omaddons", "status"]));
    let mut s = WriteSession::untracked(Writer::BackupRestore, scopes, "restore-stage");
    let copy = clone_tree(
        &mut s,
        &backup.wtf(),
        staged,
        &CloneOpts::restore(ClonePolicy::Preferred),
        Some(&plan.manifest.entries),
        ctx.progress,
    )
    .map_err(|e| e.error(names))?;
    failpoint::hit("engine.restore.staged").map_err(fail)?;
    let shape = |entries: &[crate::backup::ManifestEntry]| -> BTreeMap<String, (EntryKind, u64)> {
        entries
            .iter()
            .map(|e| {
                let size = if e.kind == EntryKind::File { e.size } else { 0 };
                (e.path.clone(), (e.kind, size))
            })
            .collect()
    };
    let (copied, recorded) = (shape(&copy.manifest), shape(&plan.manifest.entries));
    if copied != recorded {
        let first = copied
            .keys()
            .chain(recorded.keys())
            .find(|p| copied.get(*p) != recorded.get(*p))
            .cloned();
        return Err(incomplete(backup, first.as_deref(), names));
    }
    let integrity = if plan.manifest.entries.iter().any(|e| e.sha256.is_some()) {
        let hashes =
            hash_files(staged, &plan.manifest.entries, ctx.progress).map_err(|e| e.error(names))?;
        let damaged = plan
            .manifest
            .entries
            .iter()
            .zip(&hashes)
            .find(|(entry, now)| entry.sha256.is_some() && entry.sha256 != **now);
        if let Some((entry, _)) = damaged {
            return Err(crate::backup::verify::mismatch(
                &backup.id,
                &names.rel(&entry.path),
            ));
        }
        Integrity::Verified
    } else {
        Integrity::MetadataOnly
    };
    Ok(Staged {
        copy: Some(copy),
        live_stamp: live_stamp(topology)?,
        integrity,
    })
}

/// The `WTF/` change a commit window makes.
enum WtfChange<'a> {
    /// A restore of `backup` as `plan` planned it, the `before_restore` backup's line
    /// appended first.
    Restore {
        backup: &'a BackupRef,
        plan: &'a RestorePlan,
        before_restore: Option<&'a HistoryEntry>,
    },
    /// The undo of `reverted` (a restore, or the undo of one), recorded as `op`. `by_hand`
    /// is what a user copies from when the filesystem cannot exchange; `recheck` runs
    /// under the lock with the record generation now (the history's conflicts, a bound
    /// preview).
    Undo {
        reverted: &'a HistoryEntry,
        op: &'a HistoryOp,
        by_hand: &'a Path,
        recheck: &'a dyn Fn(u64) -> Result<()>,
    },
}

impl WtfChange<'_> {
    /// The tree a user copies from by hand when the filesystem cannot exchange.
    fn by_hand(&self) -> PathBuf {
        match self {
            Self::Restore { backup, .. } => backup.wtf(),
            Self::Undo { by_hand, .. } => by_hand.to_path_buf(),
        }
    }

    /// The files it puts in place.
    fn files(&self) -> u64 {
        match self {
            Self::Restore { plan, .. } => plan.files.restored,
            Self::Undo { op, .. } => op.files.unwrap_or(0),
        }
    }
}

/// Everything the commit window reads.
struct Window<'a> {
    ctx: &'a EngineCtx<'a>,
    scopes: &'a ScopeTable,
    discovered: &'a DiscoveredTarget,
    topology: &'a Topology,
    tx: TxId,
    staged: &'a Staged,
    change: WtfChange<'a>,
    reason: Option<String>,
    run: RunId,
}

impl Window<'_> {
    /// Under the global lock: recover, register, revalidate (records, mounts, and the
    /// last running check), journal, undo data, the two steps, commit. Returns the key,
    /// how long the lock was held, and the recovery's warnings.
    fn commit(&self) -> Result<(TargetKey, u64, Vec<Warning>)> {
        let (ctx, paths, tx) = (self.ctx, self.ctx.paths, self.tx);
        let fail = |e: std::io::Error| {
            Error::io(
                paths.state_dir(),
                &e,
                Suggestion::command(["omaddons", "status"]),
            )
        };
        let mut target = self.discovered.target.clone();
        let client = ClientPaths::new(&target.client_dir);
        let guard = GlobalLock::acquire(paths, ctx.command)?;
        failpoint::hit("engine.lock.acquired").map_err(fail)?;
        let writable = |dir: &Path| sv_writable(ctx, dir);
        let own: Vec<TargetKey> = target.key.into_iter().collect();
        let recovery = RecoveryEnv {
            lock: &guard,
            paths,
            scopes: self.scopes,
            own: &own,
            sv_writable: &writable,
        };
        let mut warnings = recover_all(&recovery)?;
        failpoint::hit("engine.recovered").map_err(fail)?;
        // The `before_restore` backup stands whatever the restore does: its line goes
        // first, before the journal, so a roll-forward (which replays only the restore's
        // own line) never leaves it out.
        if let WtfChange::Restore {
            before_restore: Some(line),
            ..
        } = &self.change
        {
            append_once(paths, self.scopes, &guard, line, Writer::BackupRestore)?;
        }
        let key = match target.key {
            Some(key) => key,
            None => {
                let mut s = WriteSession::untracked(Writer::BackupRestore, self.scopes, "registry");
                let mut registry = LockedRegistry::load(&guard, paths)?;
                let key = registry.ensure_registered(&mut s, self.discovered)?;
                target.key = Some(key);
                key
            }
        };
        failpoint::hit("engine.registered").map_err(fail)?;
        refuse_newer_state(paths, &target)?;
        // Retryable, unless the run is bound to a preview: it then commits only onto what
        // the preview saw (M3-D46).
        let moved = |what: &str| match ctx.binding {
            Some(binding) => binding.moved(&target.id),
            None => Error::plan_stale(
                PlanStaleSource::Replan,
                format!(
                    "{}'s {what} changed since the restore was planned",
                    target.id
                ),
                Suggestion::text("run the command again; it plans against the current state"),
            ),
        };
        let current = load_record(paths, &key)?;
        match &self.change {
            WtfChange::Restore { plan, .. } => {
                // ubs:ignore — comparing record generations, not secrets
                if current.value.generation != plan.generation {
                    return Err(moved("records"));
                }
            }
            WtfChange::Undo { recheck, .. } => recheck(current.value.generation)?,
        }
        // The mounts again, and the client's WTF/ as the stage expects it.
        let by_hand = self.change.by_hand();
        let seen = topology(&client, &by_hand, &mount_id)?;
        // ubs:ignore — comparing mount ids, not secrets
        if seen.mounts() != self.topology.mounts() {
            return Err(unsupported("stage_cross_mount", &by_hand));
        }
        // ubs:ignore — comparing paths, not secrets
        if seen.wtf != self.topology.wtf {
            return Err(moved("WTF/"));
        }
        // The last running check before the live transition (M3-D29): a refusal check,
        // not exclusion; `--while-running` never lifts it.
        refuse_running(ctx, &target, self.change.files())?;
        failpoint::hit("engine.revalidated").map_err(fail)?;

        let mut record = current.value.clone();
        record.generation += 1;
        let now = Timestamp::now();
        let history = self.history_line(key, now)?;
        let stage_dir = client.stage_wtf(&tx);
        let staged = stage_dir.join("WTF");
        // A restore's source is checked by identity on recovery; an undo's is the
        // reverted transaction's snapshot, which its history line names.
        let source_backup = match &self.change {
            WtfChange::Restore { backup, .. } => Some(SourceBackup {
                id: backup.id,
                dir: FileId::of(&backup.dir).ok_or_else(|| {
                    Error::new(
                        ErrorCode::NotFound,
                        format!("backup {} is gone", backup.id),
                        Suggestion::command(["omaddons", "backup", "list"]),
                    )
                })?,
            }),
            WtfChange::Undo { .. } => None,
        };
        let prepared = Prepared {
            schema_version: super::journal::CURRENT,
            tx,
            target_key: key,
            client_dir: target.client_dir.clone(),
            kind: history.kind,
            started_at: now,
            creates_dirs: Vec::new(),
            commit: CommitPayload {
                record: record.clone(),
                index: IndexChanges::default(),
                history: history.clone(),
                stage_dir: stage_dir.clone(),
                sv_restore: None,
            },
            writes_wtf: true,
            staged_stamp: self
                .staged
                .copy
                .as_ref()
                .map(|copy| copy.stamp_digest.to_string()),
            staged_files: None,
            source_backup,
            client: Some(ClientFacts {
                target_id: target.id.clone(),
                family: target.family,
                version: target.version.to_string(),
            }),
        };
        // Named `<tx>.wtf` like the journal and the stage, so a pre-M3 build's orphan
        // sweep never takes the replaced tree.
        let actions = paths.state_snapshot_wtf(&tx).join("actions.jsonl");
        let mut s = WriteSession::tx(
            tx,
            Writer::BackupRestore,
            self.scopes,
            SnapshotRoots::for_wtf_tx(paths, &tx),
            actions,
        );
        let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;
        let live = self.topology.wtf.clone();
        let committed = (|| -> Result<()> {
            failpoint::hit("engine.journal.prepared").map_err(fail)?;
            // Undo data before the first rename, a client snapshot dir included even
            // when there is no live WTF/ (an undo relies on it).
            snapshot_state(&mut s, paths, &paths.state_snapshot_wtf(&tx), &key)?;
            ensure_dir_all(&mut s, &client.snapshots_wtf(&tx))?;
            let previous = client.snapshots_wtf(&tx).join(WTF_PREVIOUS);
            match (&self.staged.copy, &live) {
                // The copy goes in (exchanged with the live tree, or into its empty
                // place), and the old tree on from the stage to this snapshot.
                (Some(copy), _) => {
                    let into_place = match &live {
                        Some(live) => (live.clone(), StepOp::Exchange, RenameMode::Exchange),
                        None => (client.wtf(), StepOp::Noreplace, RenameMode::NoReplace),
                    };
                    rename_tree(
                        &mut s,
                        &mut journal,
                        &Move {
                            from: &staged,
                            to: &into_place.0,
                            op: into_place.1,
                            mode: into_place.2,
                            purpose: StepPurpose::SwapIn,
                            stamp_before: Some(copy.stamp_digest),
                            stamp_after: self.staged.live_stamp,
                        },
                        &by_hand,
                    )?;
                    failpoint::hit("engine.restore.exchanged").map_err(fail)?;
                    if live.is_some() {
                        rename_tree(
                            &mut s,
                            &mut journal,
                            &Move {
                                from: &staged,
                                to: &previous,
                                op: StepOp::Noreplace,
                                mode: RenameMode::NoReplace,
                                purpose: StepPurpose::OldToSnapshot,
                                stamp_before: self.staged.live_stamp,
                                stamp_after: None,
                            },
                            &by_hand,
                        )?;
                    }
                }
                // Nothing to put back (the undo of a restore onto a client that had no
                // `WTF/`): the live tree only moves out, to this snapshot.
                (None, Some(live)) => {
                    rename_tree(
                        &mut s,
                        &mut journal,
                        &Move {
                            from: live,
                            to: &previous,
                            op: StepOp::Noreplace,
                            mode: RenameMode::NoReplace,
                            purpose: StepPurpose::RemoveToSnapshot,
                            stamp_before: self.staged.live_stamp,
                            stamp_after: None,
                        },
                        &by_hand,
                    )?;
                    failpoint::hit("engine.restore.exchanged").map_err(fail)?;
                }
                (None, None) => {}
            }
            failpoint::hit("engine.restore.snapshotted").map_err(fail)?;
            journal.mark_swapped(&mut s)?;
            failpoint::hit("engine.swapped").map_err(fail)?;
            ensure_dir_all(&mut s, &paths.targets_dir())?;
            store::save(
                &mut s,
                &paths.target_record(&key),
                &record,
                Some(&current.status),
            )?;
            failpoint::hit("engine.record.written").map_err(fail)?;
            history::append(&mut s, paths, &history)?;
            note_undo(ctx, &tx);
            failpoint::hit("engine.history.appended").map_err(fail)?;
            journal.mark_committed(&mut s)?;
            failpoint::hit("engine.committed").map_err(fail)?;
            Ok(())
        })();
        if let Err(error) = committed {
            // Read before recovery prunes it: committed already, the restore stands.
            let committed_swap = super::journal::committed_swap(paths, &tx);
            // In-process recovery checks running afresh (a writes_wtf journal waits).
            match (recover_tx(&recovery, &tx), committed_swap) {
                // Swapped already: the payload completed it, so the restore stands.
                (
                    Ok(Some(Recovered {
                        action: RecoveryAction::RolledForward,
                        warnings: rolled,
                        ..
                    })),
                    _,
                ) => {
                    note_undo(ctx, &tx);
                    warnings.extend(rolled);
                }
                (Ok(None), Some(_)) => note_undo(ctx, &tx),
                (Ok(_), _) => return Err(error),
                (Err(failed), _) => return Err(recovery_failed(error, &tx, &failed)),
            }
        }
        let held = millis(&guard);
        drop(guard);
        failpoint::hit("engine.unlocked").map_err(fail)?;
        write_status(paths, self.scopes, &target, &record);
        Ok((key, held, warnings))
    }

    /// The history line: one `wtf` op, naming the backup a restore put in place, or, for
    /// an undo, the snapshot it put back.
    fn history_line(&self, key: TargetKey, at: Timestamp) -> Result<HistoryEntry> {
        let (kind, reverts, op) = match &self.change {
            WtfChange::Restore { backup, plan, .. } => (
                TxKind::Restore,
                None,
                HistoryOp {
                    op: FolderOp::Wtf,
                    folder: wtf_folder()?,
                    addon: String::new(),
                    edit: None,
                    backup: Some(backup.id),
                    from: Some(WtfSource::Backup),
                    files: Some(plan.files.restored),
                },
            ),
            WtfChange::Undo { reverted, op, .. } => {
                (TxKind::Undo, Some(reverted.tx), (*op).clone())
            }
        };
        Ok(HistoryEntry {
            schema_version: history::CURRENT,
            tx: self.tx,
            kind,
            at,
            target: Some(key),
            targets: Vec::new(),
            actor: self.ctx.actor,
            reason: self.reason.clone(),
            argv: self.ctx.argv.to_vec(),
            ops: vec![op],
            result: TxResult::Committed,
            reverts,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(self.run),
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        })
    }
}

/// One journaled rename of a whole `WTF` tree.
struct Move<'a> {
    from: &'a Path,
    to: &'a Path,
    op: StepOp,
    mode: RenameMode,
    purpose: StepPurpose,
    /// What is at `from` before and after, as stamp digests (M3-D25).
    stamp_before: Option<StampDigest>,
    stamp_after: Option<StampDigest>,
}

/// Journals `m`, then renames, with the stamp digests given, so a session never
/// content-hashes a `WTF/` under the lock. A filesystem that cannot do the flagged rename
/// is `filesystem_unsupported` (`backup_wtf` named): the step did not run, so rolling
/// back is a no-op, and there is no fallback (M3-D24).
fn rename_tree(
    s: &mut WriteSession<'_>,
    journal: &mut Journal,
    m: &Move<'_>,
    backup_wtf: &Path,
) -> Result<()> {
    debug_assert!(
        m.stamp_before.is_some(),
        "a WTF/ rename carries its stamp digests (M3-D25)"
    );
    let step = Step {
        op: m.op,
        from: m.from.to_path_buf(),
        to: m.to.to_path_buf(),
        purpose: m.purpose,
    };
    journal.step(s, step)?;
    failpoint::hit("engine.swap.before_rename")
        .map_err(|e| Error::io(m.from, &e, Suggestion::command(["omaddons", "status"])))?;
    let opts = MutateOpts {
        stamp_before: m.stamp_before,
        stamp_after: m.stamp_after,
        ..MutateOpts::default()
    };
    let op = Op::Rename {
        to: m.to,
        mode: m.mode,
    };
    match mutate_with(s, m.from, op, opts) {
        Ok(_) => Ok(()),
        Err(FsxError::Unsupported) => Err(unsupported("no_rename_exchange", backup_wtf)),
        Err(err) => Err(err.into()),
    }
}

// ---------------------------------------------------------------------------------------
// Undoing a whole restore (M3-D28 rule 4): the same transaction, put back from the
// reverted transaction's `WTF.previous` instead of a backup, with the same preflight, no
// fallback and the same running checks; the tree it replaces goes to the undo's own
// `WTF.previous`, so the undo is undoable in turn.

/// Undoes `reverted` (a whole restore, or the undo of one) on `discovered`. A dry run
/// stops after the plan and the preflight's mount checks. `recheck` runs under the lock:
/// the history's conflicts again.
pub(super) fn undo_whole(
    ctx: &EngineCtx<'_>,
    req: &UndoRequest,
    reverted: &HistoryEntry,
    discovered: &DiscoveredTarget,
    recheck: &dyn Fn() -> Result<()>,
) -> Result<UndoOutcome> {
    let target = &discovered.target;
    let client = ClientPaths::new(&target.client_dir);
    let names = WtfPathRedactor::for_client(&client);
    let snapshot = super::retention::snapshot_of(&client, &reverted.tx);
    if !snapshot.is_dir() {
        return Err(super::undo::pruned(&reverted.tx));
    }
    // `None`: there was no `WTF/` before it, so the undo only moves the live one out.
    let source = Some(snapshot.join(WTF_PREVIOUS)).filter(|previous| previous.is_dir());
    let by_hand = source.clone().unwrap_or_else(|| snapshot.clone());
    let files = match &source {
        Some(source) => walk(source, BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH)
            .map_err(|e| e.error(&names))?
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .count() as u64,
        None => 0,
    };
    refuse_running(ctx, target, files)?;
    let live = client.wtf();
    let observed = if live.is_dir() {
        StampDigest::compute(&live)
            .map_err(|e| Error::io(&live, &e, Suggestion::command(["omaddons", "status"])))?
            .to_string()
    } else {
        "absent".to_owned()
    };
    let op = HistoryOp {
        op: FolderOp::Wtf,
        folder: wtf_folder()?,
        addon: String::new(),
        edit: None,
        backup: None,
        from: Some(WtfSource::Snapshot),
        files: Some(files),
    };
    let wtf = WtfUndo {
        changed_since: changed_since(&client, reverted),
    };
    let key = super::undo::target_of(reverted)?;
    let generation = load_record(ctx.paths, &key)?.value.generation;
    let outcome = |generation: u64, tx: Option<TxId>, warnings: Vec<Warning>, held: Option<u64>| {
        UndoOutcome {
            undoes: reverted.tx,
            outcome: TxOutcome {
                target: target.id.clone(),
                key: Some(key),
                tx,
                kind: Some(TxKind::Undo),
                dry_run: tx.is_none(),
                changes: Vec::new(),
                atomic_swap: true,
                // Informational: the client reads `WTF/` at startup, and this never runs
                // while it does.
                reload_required: ReloadRequired::Restart,
                warnings,
                creates_addons_dir: false,
                lock_held_ms: held,
                notes: Vec::new(),
            },
            sv: SvCounts::default(),
            plan: UndoFacts {
                generation,
                ops: vec![op.clone()],
                saved_variables: Vec::new(),
                wtf: Some(observed.clone()),
            },
            wtf: Some(wtf),
        }
    };
    if req.dry_run {
        preflight(&client, &by_hand, None)?;
        return Ok(outcome(generation, None, Vec::new(), None));
    }
    // A bound run stages nothing unless it planned what the preview showed (M3-D46).
    if let Some(binding) = ctx.binding {
        binding.undo(&outcome(generation, None, Vec::new(), None))?;
    }
    refuse_newer_state(ctx.paths, target)?;

    let scopes = ScopeTable::new(ctx.paths, std::slice::from_ref(&target.client_dir), None)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(ctx.paths, &tx)?;
    let probe = client.stage_wtf(&tx).join("probe");
    let probe = Probe {
        scopes: &scopes,
        dir: &probe,
    };
    let prepared = preflight(&client, &by_hand, Some(probe)).and_then(|topology| {
        let into = client.stage_wtf(&tx).join("WTF");
        let staged = stage_snapshot(ctx, &scopes, source.as_deref(), &into, &topology, &names)?;
        Ok((topology, staged))
    });
    let (topology, staged) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), false);
            return Err(error);
        }
    };
    // Under the lock: the history's conflicts, and a bound preview against the records
    // as they are now (the live tree's digest was taken before the lock, as D5's are).
    let recheck = |generation: u64| -> Result<()> {
        recheck()?;
        if let Some(binding) = ctx.binding {
            binding.undo(&outcome(generation, None, Vec::new(), None))?;
        }
        Ok(())
    };
    let window = Window {
        ctx,
        scopes: &scopes,
        discovered,
        topology: &topology,
        tx,
        staged: &staged,
        change: WtfChange::Undo {
            reverted,
            op: &op,
            by_hand: &by_hand,
            recheck: &recheck,
        },
        reason: req.reason.clone(),
        run: req.run,
    };
    let result = window.commit();
    let committed = result.is_ok();
    ring_after_window(ctx, &result);
    cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), committed);
    let (key, held, mut warnings) = result?;
    warnings.extend(retain(ctx, &scopes, key, &target.client_dir));
    let ring = crate::backup::inventory::Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    crate::backup::inventory::rebuild(ctx.paths, &client, &key, ring);
    warnings.extend(copied(&target.id, staged.copied_bytes()));
    Ok(outcome(generation, Some(tx), warnings, Some(held)))
}

/// Stages `source` (the reverted transaction's `WTF.previous`) at `into` without the lock:
/// an untracked copy keeping the source's own modes, then the live tree's stamp digest.
/// `None` stages nothing.
fn stage_snapshot(
    ctx: &EngineCtx<'_>,
    scopes: &ScopeTable,
    source: Option<&Path>,
    into: &Path,
    topology: &Topology,
    names: &WtfPathRedactor,
) -> Result<Staged> {
    let copy = match source {
        Some(source) => {
            let mut s = WriteSession::untracked(Writer::BackupRestore, scopes, "restore-stage");
            let opts = CloneOpts::restore(ClonePolicy::Preferred);
            let copy = clone_tree(&mut s, source, into, &opts, None, ctx.progress)
                .map_err(|e| e.error(names))?;
            failpoint::hit("engine.restore.staged")
                .map_err(|e| Error::io(into, &e, Suggestion::command(["omaddons", "status"])))?;
            Some(copy)
        }
        None => None,
    };
    Ok(Staged {
        copy,
        live_stamp: live_stamp(topology)?,
        integrity: Integrity::MetadataOnly,
    })
}

/// How many files the play since `reverted` changed in the live `WTF/`, which undoing it
/// reverts too (they stay in the undo's own snapshot). Against the tree `reverted` put in
/// place (the restored backup's manifest, or the snapshot an undo copied from): a live
/// file it did not hold, holds at another size, or written after `reverted` committed
/// (the copies it put in place all predate that), and a file it held that is gone. The
/// copies get new times, so a time alone is never compared with the manifest's. `None`
/// when that tree's record is gone, or a tree is too large to walk.
fn changed_since(client: &ClientPaths, reverted: &HistoryEntry) -> Option<u64> {
    let op = reverted
        .ops
        .iter()
        .find(|o| matches!(o.op, FolderOp::Wtf))?;
    let walked = |root: &Path| {
        walk(root, BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH)
            .ok()
            .map(|walk| walk.entries)
    };
    let put = match op.from? {
        WtfSource::Backup => {
            let dir = crate::backup::backup_dir(client, &op.backup?);
            store::load::<Manifest>(&dir.join("manifest.json"))
                .ok()??
                .value
                .entries
        }
        WtfSource::Snapshot => {
            let snapshot = super::retention::snapshot_of(client, &reverted.reverts?);
            let previous = snapshot.join(WTF_PREVIOUS);
            if previous.is_dir() {
                walked(&previous)?
            } else if snapshot.is_dir() {
                // The undo of a restore onto a client that had no `WTF/` put none back.
                Vec::new()
            } else {
                return None;
            }
        }
    };
    let live = client.wtf();
    let live = if live.is_dir() {
        walked(&live)?
    } else {
        Vec::new()
    };
    let files = |entries: &[ManifestEntry]| -> BTreeMap<String, (u64, i64)> {
        entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| (e.path.clone(), (e.size, e.mtime_ns)))
            .collect()
    };
    let (put, live) = (files(&put), files(&live));
    let since = reverted.at.as_nanosecond();
    let changed = live
        .iter()
        .filter(|(path, (size, mtime))| match put.get(*path) {
            // ubs:ignore — comparing file sizes, not secrets
            Some((was, _)) => was != size || i128::from(*mtime) > since,
            None => true,
        })
        .count();
    let gone = put.keys().filter(|path| !live.contains_key(*path)).count();
    u64::try_from(changed + gone).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::*;

    /// A client with `WTF/` (unless `wtf` is false) and `Interface/`.
    fn client(wtf: bool) -> (tempfile::TempDir, ClientPaths) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("Interface/AddOns")).unwrap();
        if wtf {
            fs::create_dir_all(root.join("WTF/Account")).unwrap();
        }
        (dir, ClientPaths::new(root))
    }

    /// Mount ids by path; any other path is on mount 1.
    fn mounts(planted: &[(&Path, u64)]) -> impl Fn(&Path) -> io::Result<u64> {
        let planted: BTreeMap<PathBuf, u64> =
            planted.iter().map(|(p, m)| (p.to_path_buf(), *m)).collect();
        move |path: &Path| Ok(planted.get(path).copied().unwrap_or(1))
    }

    fn reason(err: &Error) -> (ErrorCode, String) {
        (
            err.code,
            err.details["reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    #[test]
    fn a_plain_wtf_on_the_clients_mount_passes() {
        let (_dir, c) = client(true);
        let backup = Path::new("/backups/01M6K2VA/WTF");
        let t = topology(&c, backup, &mounts(&[])).unwrap();
        assert_eq!(
            (t.client_mount, t.wtf, t.stage_mount, t.exchange),
            (1, Some(c.wtf()), 1, Exchange::Unverified)
        );
        // No WTF yet: allowed, the exchange is then a Noreplace.
        let (_dir, bare) = client(false);
        assert_eq!(topology(&bare, backup, &mounts(&[])).unwrap().wtf, None);
    }

    #[test]
    fn a_mount_point_or_a_cross_mount_stage_is_refused() {
        let (_dir, c) = client(true);
        let backup = Path::new("/backups/01M6K2VA/WTF");
        let wtf = c.wtf();
        let err = topology(&c, backup, &mounts(&[(&wtf, 2)])).unwrap_err();
        assert_eq!(
            reason(&err),
            (
                ErrorCode::FilesystemUnsupported,
                "wtf_is_mount_point".into()
            )
        );
        assert_eq!(err.details["backup_dir"], "/backups/01M6K2VA/WTF");
        assert_eq!(err.fix.argv(), None, "no command helps");
        // The stage's nearest existing ancestor is Interface/ here.
        let interface = c.interface();
        let err = topology(&c, backup, &mounts(&[(&interface, 3)])).unwrap_err();
        assert_eq!(
            reason(&err),
            (ErrorCode::FilesystemUnsupported, "stage_cross_mount".into())
        );
    }

    #[test]
    fn a_symlinked_wtf_is_refused() {
        let (dir, c) = client(false);
        let elsewhere = dir.path().join("sync");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.client_dir().join("WTF")).unwrap();
        let err = topology(&c, Path::new("/b"), &mounts(&[])).unwrap_err();
        assert_eq!(
            reason(&err),
            (ErrorCode::ScopeDenied, "wtf_symlinked".into())
        );
    }

    #[test]
    fn a_real_run_probes_the_exchange_and_a_dry_run_writes_nothing() {
        let (_dir, c) = client(true);
        let paths = crate::state::paths::Paths::with_roots(
            c.client_dir(),
            c.client_dir().join("c"),
            c.client_dir().join("s"),
            c.client_dir().join("k"),
        );
        let scopes = ScopeTable::new(&paths, &[c.client_dir().to_path_buf()], None).unwrap();
        let backup = Path::new("/backups/x/WTF");
        let dry = preflight(&c, backup, None).unwrap();
        assert_eq!(dry.exchange, Exchange::Unverified);
        assert!(!c.meta_dir().exists(), "a dry run writes nothing");
        let probe_dir = c.meta_dir().join("stage/T/probe");
        let probe = Probe {
            scopes: &scopes,
            dir: &probe_dir,
        };
        assert_eq!(
            preflight(&c, backup, Some(probe)).unwrap().exchange,
            Exchange::Verified
        );
        #[cfg(debug_assertions)]
        {
            let again = c.meta_dir().join("stage/U/probe");
            let probe = Probe {
                scopes: &scopes,
                dir: &again,
            };
            let err = crate::fsx::failpoint::with_spec("fsx.renameat2.einval", || {
                preflight(&c, backup, Some(probe))
            })
            .unwrap_err();
            assert_eq!(
                reason(&err),
                (
                    ErrorCode::FilesystemUnsupported,
                    "no_rename_exchange".into()
                )
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // The transaction, in the command's plan order (M3-D24).

    use std::os::unix::fs::PermissionsExt;

    use crate::backup::BackupKind;
    use crate::backup::create::{CreateRequest, create};
    use crate::engine::testing::{TreeEntry, TxScene, tree_snapshot};
    use crate::model::Actor;
    use crate::output::ProgressSink;
    use crate::state::history::{BackupNote, HistoryFilter};

    const SV: &str = "Account/FAKEACCT/SavedVariables";

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A registered retail client whose `WTF/` was backed up (`before`), then played on
    /// (`after`, the live tree): a file changed, one gone, one new. The backup holds an
    /// empty directory, a symlink and modes of its own.
    struct Restorable {
        scene: TxScene,
        backup: BackupRef,
        before: Vec<TreeEntry>,
        after: Vec<TreeEntry>,
    }

    impl Restorable {
        fn new(verify: bool) -> Self {
            Self::with(verify, |_| {})
        }

        /// [`Self::new`], with `more` adding to the tree before it is backed up.
        fn with(verify: bool, more: impl FnOnce(&Path)) -> Self {
            let mut scene = TxScene::new();
            let wtf = scene.client.wtf();
            fs::create_dir_all(wtf.join(SV)).unwrap();
            fs::create_dir_all(wtf.join("Account/FAKEACCT/Empty")).unwrap();
            fs::create_dir_all(wtf.join("Account/FAKEACCT/Modes")).unwrap();
            fs::write(wtf.join("Config.wtf"), "SET locale enUS\n").unwrap();
            fs::write(wtf.join(SV).join("A.lua"), "A = 1\n").unwrap();
            fs::write(wtf.join(SV).join("B.lua"), "B = 1\n").unwrap();
            fs::write(wtf.join("Account/FAKEACCT/Modes/C.lua"), "C = 1\n").unwrap();
            chmod(&wtf.join("Account/FAKEACCT/Modes/C.lua"), 0o640);
            chmod(&wtf.join("Account/FAKEACCT/Modes"), 0o750);
            std::os::unix::fs::symlink("SavedVariables", wtf.join("Account/FAKEACCT/link"))
                .unwrap();
            more(&wtf);
            let before = tree_snapshot(&wtf);
            let scopes = scopes(&scene);
            let guard = GlobalLock::acquire(&scene.paths, "test").unwrap();
            let mut s = WriteSession::untracked(Writer::Backup, &scopes, "registry");
            let mut registry = LockedRegistry::load(&guard, &scene.paths).unwrap();
            let key = registry
                .ensure_registered(&mut s, &scene.discovered)
                .unwrap();
            drop(guard);
            scene.discovered.target.key = Some(key);
            let req = CreateRequest {
                target: &scene.discovered.target,
                kind: BackupKind::Manual,
                trigger: None,
                actor: Actor::Agent,
                reason: None,
                policy: ClonePolicy::Preferred,
                verify,
                client_running: false,
                command: "backup create",
            };
            create(&scene.paths, &scopes, &req, &ProgressSink::none()).unwrap();
            let backup = crate::backup::list(&scene.client).0.remove(0);
            fs::write(wtf.join(SV).join("A.lua"), "A = 22\n").unwrap();
            fs::remove_file(wtf.join("Config.wtf")).unwrap();
            fs::write(wtf.join(SV).join("New.lua"), "N = 1\n").unwrap();
            let after = tree_snapshot(&wtf);
            Self {
                scene,
                backup,
                before,
                after,
            }
        }

        fn restore(&self) -> Result<RestoreOutcome> {
            self.run(Twist::default())
        }

        /// Reserve, plan, `between` (after planning), a transaction id with its
        /// in-flight lock, the probing preflight (its result through `topology`), then
        /// the engine's restore, with `failpoints` armed for it alone.
        fn run(&self, twist: Twist<'_>) -> Result<RestoreOutcome> {
            let discovered = self.scene.current_target();
            let reservation = reserve(&self.backup)?;
            let plan = plan(&self.scene.paths, &discovered.target, &self.backup)?;
            if let Some(between) = twist.between {
                between(self);
            }
            let tx = TxId::new();
            let inflight = InflightLock::create(&self.scene.paths, &tx)?;
            let scopes = scopes(&self.scene);
            let probe = self.scene.client.stage_wtf(&tx).join("probe");
            let probe = Probe {
                scopes: &scopes,
                dir: &probe,
            };
            let mut seen = preflight(&self.scene.client, &self.backup.wtf(), Some(probe))?;
            if let Some(topology) = twist.topology {
                topology(&mut seen);
            }
            let engine = || {
                self.scene.with_ctx("backup restore", |ctx| {
                    restore(
                        ctx,
                        RestoreRequest {
                            target: &discovered,
                            backup: &self.backup,
                            reservation,
                            topology: seen,
                            tx,
                            inflight,
                            plan: &plan,
                            before_restore: twist.before_restore,
                            reason: Some("the raid went badly".into()),
                            run: RunId::new(),
                        },
                    )
                })
            };
            #[cfg(debug_assertions)]
            if let Some(spec) = twist.failpoints {
                return failpoint::with_spec(spec, engine);
            }
            engine()
        }

        fn key(&self) -> TargetKey {
            self.scene.discovered.target.key.unwrap()
        }

        fn live(&self) -> Vec<TreeEntry> {
            tree_snapshot(&self.scene.client.wtf())
        }

        fn history(&self) -> Vec<HistoryEntry> {
            let text = history::text(&self.scene.paths).unwrap();
            history::parse(&text, &HistoryFilter::default()).entries
        }

        fn generation(&self) -> u64 {
            load_record(&self.scene.paths, &self.key())
                .unwrap()
                .value
                .generation
        }

        /// A `before_restore` backup's history line.
        fn backup_line(&self) -> HistoryEntry {
            HistoryEntry {
                schema_version: history::CURRENT,
                tx: TxId::new(),
                kind: TxKind::Backup,
                at: Timestamp::now(),
                target: Some(self.key()),
                targets: Vec::new(),
                actor: Actor::Agent,
                reason: None,
                argv: Vec::new(),
                ops: Vec::new(),
                result: TxResult::Committed,
                reverts: None,
                sv_deferred: false,
                sv_restored: None,
                created_addons_dir: false,
                run: None,
                backup: Some(BackupNote {
                    id: self.backup.id,
                    kind: BackupKind::BeforeRestore,
                    files: 4,
                    bytes: 24,
                }),
                plan: None,
                doctor_run_id: None,
                repairs: Vec::new(),
            }
        }

        /// Nothing of a transaction is left: no journal, stage or in-flight lock.
        fn assert_no_leftovers(&self) {
            let paths = &self.scene.paths;
            let listed = |dir: PathBuf| -> Vec<PathBuf> {
                fs::read_dir(dir)
                    .map(|l| l.map(|e| e.unwrap().path()).collect())
                    .unwrap_or_default()
            };
            let journals = paths.journal(&TxId::new()).parent().unwrap().to_path_buf();
            assert_eq!(listed(journals), Vec::<PathBuf>::new(), "journals");
            let stage = self.scene.client.meta_dir().join("stage");
            assert_eq!(listed(stage), Vec::<PathBuf>::new(), "stage");
            assert_eq!(
                listed(paths.inflight_dir()),
                Vec::<PathBuf>::new(),
                "inflight"
            );
        }

        /// The live tree is still the played-on one and the restore left no trace.
        fn assert_unchanged(&self, generation: u64) {
            assert_eq!(self.live(), self.after);
            assert_eq!(self.generation(), generation);
            assert!(
                !self
                    .history()
                    .iter()
                    .any(|e| matches!(e.kind, TxKind::Restore)),
                "no restore line"
            );
            self.assert_no_leftovers();
        }
    }

    /// What a scene changes about a plain restore.
    #[derive(Default)]
    struct Twist<'a> {
        before_restore: Option<HistoryEntry>,
        between: Option<&'a dyn Fn(&Restorable)>,
        topology: Option<&'a dyn Fn(&mut Topology)>,
        #[cfg_attr(not(debug_assertions), allow(dead_code))]
        failpoints: Option<&'a str>,
    }

    fn scopes(scene: &TxScene) -> ScopeTable {
        let client = scene.discovered.target.client_dir.clone();
        ScopeTable::new(&scene.paths, std::slice::from_ref(&client), None).unwrap()
    }

    #[test]
    fn a_restore_puts_the_backup_back_and_keeps_the_old_tree() {
        let r = Restorable::new(false);
        let generation = r.generation();
        let line = r.backup_line();
        let done = r
            .run(Twist {
                before_restore: Some(line.clone()),
                ..Twist::default()
            })
            .unwrap();
        assert_eq!(
            r.live(),
            r.before,
            "kinds, modes, links, empty dirs and contents"
        );
        let previous = r.scene.client.snapshots_wtf(&done.tx).join(WTF_PREVIOUS);
        assert_eq!(tree_snapshot(&previous), r.after, "the old tree, for undo");
        assert_eq!(
            done.files,
            RestoreFiles {
                restored: 4,
                differed: 2,
                created_since: 1,
                unchanged: 2,
            }
        );
        assert_eq!(
            (done.key, done.integrity, done.exchange),
            (r.key(), Integrity::MetadataOnly, Exchange::Verified)
        );
        assert_eq!(r.generation(), generation + 1);
        // The before_restore line first, then the restore's own.
        let history = r.history();
        let lines: Vec<(TxId, TxKind)> = history.iter().map(|e| (e.tx, e.kind)).collect();
        assert_eq!(
            lines,
            vec![(line.tx, TxKind::Backup), (done.tx, TxKind::Restore)]
        );
        let restore_line = &history[1];
        assert_eq!(restore_line.reason.as_deref(), Some("the raid went badly"));
        assert_eq!(
            restore_line.ops,
            vec![HistoryOp {
                op: FolderOp::Wtf,
                folder: FolderName::new("WTF").unwrap(),
                addon: String::new(),
                edit: None,
                backup: Some(r.backup.id),
                from: Some(WtfSource::Backup),
                files: Some(4),
            }]
        );
        r.assert_no_leftovers();
        // Every whole-tree rename was logged with stamp digests, never content hashes.
        let actions = fs::read_to_string(
            r.scene
                .paths
                .state_snapshot_wtf(&done.tx)
                .join("actions.jsonl"),
        )
        .unwrap();
        let renames: Vec<&str> = actions.lines().filter(|l| l.contains("rename")).collect();
        assert_eq!(renames.len(), 2, "{actions}");
        assert!(
            renames
                .iter()
                .all(|l| l.contains("stamp:sha256:") && !l.contains("tree:sha256:")),
            "{actions}"
        );
    }

    /// Staging and every stat walk run before the lock; under it, a restore of a
    /// 5,000-file tree journals, takes undo data and renames two directories. The
    /// release budget (50 ms) is `tests/perf.rs`'s; this is the debug sanity ceiling.
    #[test]
    fn a_large_tree_holds_the_lock_briefly() {
        let r = Restorable::with(false, |wtf| {
            for character in 0..10 {
                let dir = wtf.join(format!(
                    "Account/FAKEACCT/FakeRealm/Char{character}/SavedVariables"
                ));
                fs::create_dir_all(&dir).unwrap();
                for n in 0..500 {
                    fs::write(dir.join(format!("Addon{n}.lua")), [b'x'; 256]).unwrap();
                }
            }
        });
        let done = r.restore().unwrap();
        assert_eq!(done.files.restored, 5_004);
        assert!(done.lock_held_ms < 2_000, "{} ms", done.lock_held_ms);
        assert_eq!(r.live(), r.before);
    }

    #[test]
    fn a_plan_observes_the_live_tree_and_writes_nothing() {
        let r = Restorable::new(false);
        let target = r.scene.current_target().target;
        let planned = plan(&r.scene.paths, &target, &r.backup).unwrap();
        let live = StampDigest::compute(&r.scene.client.wtf()).unwrap();
        assert_eq!(
            (planned.observed, planned.generation),
            (Some(live), r.generation())
        );
        assert_eq!(planned.files.restored, 4);
        r.assert_unchanged(r.generation());
        fs::remove_dir_all(r.scene.client.wtf()).unwrap();
        let bare = plan(&r.scene.paths, &target, &r.backup).unwrap();
        assert_eq!((bare.observed, bare.files.differed), (None, 4));
    }

    #[test]
    fn a_restore_onto_no_wtf_moves_the_copy_into_place() {
        let r = Restorable::new(false);
        fs::remove_dir_all(r.scene.client.wtf()).unwrap();
        let done = r.restore().unwrap();
        assert_eq!(r.live(), r.before);
        let snapshot = r.scene.client.snapshots_wtf(&done.tx);
        assert!(
            snapshot.is_dir(),
            "the client snapshot dir exists all the same"
        );
        assert!(!snapshot.join(WTF_PREVIOUS).exists());
        assert_eq!(done.files.created_since, 0);
        assert_eq!(done.files.differed, 4);
        r.assert_no_leftovers();
    }

    #[test]
    fn a_verified_backup_is_checked_and_a_damaged_one_changes_nothing() {
        let r = Restorable::new(true);
        let generation = r.generation();
        // Same size, same time: only the recorded hash can tell.
        let a = r.backup.wtf().join(SV).join("A.lua");
        let mtime = fs::metadata(&a).unwrap().modified().unwrap();
        chmod(&a, 0o644);
        fs::write(&a, "A = 9\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let err = r.restore().unwrap_err();
        assert_eq!(err.code, ErrorCode::HashMismatch, "{err:?}");
        assert!(!err.retryable);
        assert!(err.message.contains("is damaged"), "{}", err.message);
        assert!(
            err.message.contains("SavedVariables/A.lua") && !err.message.contains("FAKEACCT"),
            "redacted: {}",
            err.message
        );
        r.assert_unchanged(generation);
        // Put back, the same backup restores, verified.
        fs::write(&a, "A = 1\n").unwrap();
        assert_eq!(r.restore().unwrap().integrity, Integrity::Verified);
        assert_eq!(r.live(), r.before);
    }

    #[test]
    fn a_backup_missing_a_file_is_refused_before_staging() {
        let r = Restorable::new(false);
        let generation = r.generation();
        fs::remove_file(r.backup.wtf().join(SV).join("B.lua")).unwrap();
        let err = r.restore().unwrap_err();
        assert_eq!(err.code, ErrorCode::StateCorrupt, "{err:?}");
        assert!(err.message.contains("is not whole"), "{}", err.message);
        r.assert_unchanged(generation);
    }

    #[test]
    fn records_that_moved_since_the_plan_are_plan_stale_and_keep_the_backup_line() {
        let r = Restorable::new(false);
        let line = r.backup_line();
        let bump = |r: &Restorable| {
            let scopes = scopes(&r.scene);
            let mut s = WriteSession::untracked(Writer::Mutating, &scopes, "test");
            let mut record = load_record(&r.scene.paths, &r.key()).unwrap();
            record.value.generation += 1;
            ensure_dir_all(&mut s, &r.scene.paths.targets_dir()).unwrap();
            store::save(
                &mut s,
                &r.scene.paths.target_record(&r.key()),
                &record.value,
                None,
            )
            .unwrap();
        };
        let generation = r.generation() + 1;
        let err = r
            .run(Twist {
                before_restore: Some(line.clone()),
                between: Some(&bump),
                ..Twist::default()
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PlanStale, "{err:?}");
        assert!(err.retryable, "no --if-match: retrying re-plans");
        r.assert_unchanged(generation);
        // The before_restore backup stands: its line, once.
        let backups: Vec<TxId> = r.history().iter().map(|e| e.tx).collect();
        assert_eq!(backups, vec![line.tx]);
    }

    #[test]
    fn a_client_that_started_is_refused_at_the_last_check() {
        let mut r = Restorable::new(false);
        let generation = r.generation();
        r.scene
            .running
            .insert(r.scene.discovered.target.client_dir.clone());
        r.scene.while_running = true;
        let err = r.restore().unwrap_err();
        assert_eq!(err.code, ErrorCode::GameRunning, "{err:?}");
        r.assert_unchanged(generation);
    }

    #[test]
    fn a_mount_that_changed_under_the_lock_is_refused() {
        let r = Restorable::new(false);
        let generation = r.generation();
        let err = r
            .run(Twist {
                topology: Some(&|t| t.client_mount = t.client_mount.wrapping_add(1)),
                ..Twist::default()
            })
            .unwrap_err();
        assert_eq!(
            reason(&err),
            (ErrorCode::FilesystemUnsupported, "stage_cross_mount".into())
        );
        r.assert_unchanged(generation);
    }

    #[test]
    fn a_reservation_holds_its_backup_and_a_moved_one_is_gone() {
        let r = Restorable::new(false);
        let held = reserve(&r.backup).unwrap();
        assert!(held.holds());
        drop(held);
        let aside = r.backup.dir.with_extension("aside");
        fs::rename(&r.backup.dir, &aside).unwrap();
        let err = reserve(&r.backup).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    // -----------------------------------------------------------------------------------
    // Undoing a whole restore (M3-D28 rule 4).

    use crate::engine::undo::{SvPolicy, UndoSelector};

    /// Foo's account-wide SavedVariables, which installing Foo snapshots.
    const FOO_SV: &str = "Account/FAKEACCT/SavedVariables/Foo.lua";

    impl Restorable {
        /// A scene whose `WTF/` also holds Foo's SavedVariables.
        fn with_foo() -> Self {
            Self::with(false, |wtf| {
                fs::write(wtf.join(FOO_SV), "FooDB = {}\n").unwrap()
            })
        }

        fn undo(&self, tx: TxId, policy: SvPolicy) -> Result<UndoOutcome> {
            self.scene.undo(UndoSelector::Tx(tx), policy)
        }

        /// Installs Foo; returns its transaction.
        fn install_foo(&self) -> TxId {
            let zip = self
                .scene
                .zip("foo.zip", &[("Foo/Foo.toc", "## Interface: 120105\n")]);
            let done = self
                .scene
                .run(vec![self.scene.install("gh:o/foo", &zip)])
                .unwrap();
            assert!(done.failures.is_empty(), "{:?}", done.failures);
            done.outcomes[0].tx.unwrap()
        }

        fn previous(&self, tx: &TxId) -> Vec<TreeEntry> {
            tree_snapshot(&self.scene.client.snapshots_wtf(tx).join(WTF_PREVIOUS))
        }

        /// The history line of `tx`.
        fn line_of(&self, tx: Option<TxId>) -> HistoryEntry {
            self.history()
                .into_iter()
                // ubs:ignore — comparing transaction ids (public ULIDs), not secrets
                .find(|e| Some(e.tx) == tx)
                .unwrap()
        }
    }

    #[test]
    fn a_restore_is_undone_and_its_undo_undone_again() {
        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let undone = r.undo(restored, SvPolicy::Auto).unwrap();
        let u = undone.outcome.tx.unwrap();
        assert_eq!(r.live(), r.after, "the tree from before the restore");
        assert_eq!(
            r.previous(&u),
            r.before,
            "the restored tree, kept for this undo's undo"
        );
        assert_eq!(
            r.previous(&restored),
            r.after,
            "the reverted snapshot stays whole"
        );
        assert_eq!(
            (undone.wtf, undone.outcome.reload_required),
            (
                Some(WtfUndo {
                    changed_since: Some(0)
                }),
                ReloadRequired::Restart
            ),
            "no play since the restore"
        );
        let line = r.line_of(Some(u));
        assert_eq!((line.kind, line.reverts), (TxKind::Undo, Some(restored)));
        assert_eq!(
            (line.ops[0].op, line.ops[0].from, line.ops[0].files),
            (FolderOp::Wtf, Some(WtfSource::Snapshot), Some(4)),
            "B.lua and C.lua, and the played A.lua and New.lua"
        );
        let actions =
            fs::read_to_string(r.scene.paths.state_snapshot_wtf(&u).join("actions.jsonl")).unwrap();
        let renames: Vec<&str> = actions.lines().filter(|l| l.contains("rename")).collect();
        assert_eq!(renames.len(), 2, "{actions}");
        assert!(
            renames.iter().all(|l| l.contains("stamp:sha256:")),
            "{actions}"
        );

        // The undo, undone in turn: the restored tree again.
        r.undo(u, SvPolicy::Auto).unwrap();
        assert_eq!(r.live(), r.before);
        r.assert_no_leftovers();
    }

    #[test]
    fn a_restore_onto_no_wtf_is_undone_by_moving_the_tree_out() {
        let r = Restorable::new(false);
        fs::remove_dir_all(r.scene.client.wtf()).unwrap();
        let restored = r.restore().unwrap().tx;
        let u = r
            .undo(restored, SvPolicy::Auto)
            .unwrap()
            .outcome
            .tx
            .unwrap();
        assert!(
            !r.scene.client.wtf().exists(),
            "no WTF/, as before the restore"
        );
        assert_eq!(r.previous(&u), r.before);
        r.undo(u, SvPolicy::Auto).unwrap();
        assert_eq!(r.live(), r.before);
        r.assert_no_leftovers();

        // Restored onto nothing, then deleted by hand: the undo has nothing to move, and
        // commits all the same, so history says the restore was undone.
        let r = Restorable::new(false);
        fs::remove_dir_all(r.scene.client.wtf()).unwrap();
        let restored = r.restore().unwrap().tx;
        fs::remove_dir_all(r.scene.client.wtf()).unwrap();
        let u = r
            .undo(restored, SvPolicy::Auto)
            .unwrap()
            .outcome
            .tx
            .unwrap();
        assert_eq!(r.line_of(Some(u)).reverts, Some(restored));
        assert!(!r.scene.client.wtf().exists());
        r.assert_no_leftovers();
    }

    /// A restore and its undo keep both snapshot halves as `<tx>.wtf`, which a pre-M3
    /// build's orphan sweep never takes (omad-aepa). A restore committed before that, its
    /// halves under the bare id, is undone all the same.
    #[test]
    fn restore_snapshots_are_named_wtf_and_found_under_either_name() {
        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let (client, paths) = (&r.scene.client, &r.scene.paths);
        let named = |tx: &TxId| {
            (
                client.snapshots_wtf(tx).join(WTF_PREVIOUS).is_dir(),
                paths.state_snapshot_wtf(tx).is_dir(),
                client.snapshots(tx).exists() || paths.state_snapshot(tx).exists(),
            )
        };
        assert_eq!(named(&restored), (true, true, false), "the restore");
        let undone = r
            .undo(restored, SvPolicy::Auto)
            .unwrap()
            .outcome
            .tx
            .unwrap();
        assert_eq!(named(&undone), (true, true, false), "its undo");
        assert_eq!(r.live(), r.after);

        // A restore committed under the bare names, then undone.
        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let (client, paths) = (&r.scene.client, &r.scene.paths);
        fs::rename(client.snapshots_wtf(&restored), client.snapshots(&restored)).unwrap();
        fs::rename(
            paths.state_snapshot_wtf(&restored),
            paths.state_snapshot(&restored),
        )
        .unwrap();
        r.undo(restored, SvPolicy::Auto).unwrap();
        assert_eq!(r.live(), r.after, "the tree from before the restore");
        r.assert_no_leftovers();
    }

    #[test]
    fn the_undo_of_a_restore_needs_its_snapshot_and_all_its_settings() {
        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let keep = r.undo(restored, SvPolicy::Keep).unwrap_err();
        assert_eq!(keep.code, ErrorCode::Usage, "nothing would be left to undo");
        let snapshot = r.scene.client.snapshots_wtf(&restored);
        fs::rename(&snapshot, snapshot.with_extension("aside")).unwrap();
        let err = r.undo(restored, SvPolicy::Auto).unwrap_err();
        assert_eq!(
            (err.code, err.details["reason"].as_str()),
            (ErrorCode::NotFound, Some("snapshot_pruned"))
        );
        assert_eq!(r.live(), r.before);
    }

    /// M3-D28 rule 2, end to end: an install before a restore, undone after it, would
    /// put its SavedVariables back over the restored ones: refused, naming the restore,
    /// unless they are kept; once the restore is undone, it goes through.
    #[test]
    fn an_install_before_a_restore_is_undone_only_keeping_settings() {
        let r = Restorable::with_foo();
        let install = r.install_foo();
        let restored = r.restore().unwrap().tx;
        let err = r.undo(install, SvPolicy::Auto).unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict, "{err:?}");
        assert_eq!(err.fix.argv().unwrap()[2], restored.to_string());
        assert!(
            err.alternatives[0]
                .argv()
                .unwrap()
                .ends_with(&["--saved-variables".into(), "keep".into()])
        );
        r.undo(install, SvPolicy::Keep).unwrap();
        assert!(!r.scene.client.addons().join("Foo").exists());
        assert_eq!(r.live(), r.before, "the restored settings stand");

        // A restore undone since no longer stands in the way.
        let r = Restorable::with_foo();
        let install = r.install_foo();
        let restored = r.restore().unwrap().tx;
        r.undo(restored, SvPolicy::Auto).unwrap();
        r.undo(install, SvPolicy::Auto).unwrap();
        assert!(!r.scene.client.addons().join("Foo").exists());
    }

    /// Rule 2 also guards the follow-up of an undo that deferred SavedVariables while the
    /// game ran: after a restore, `--saved-variables restore` is refused, naming the
    /// restore, with nothing to keep instead.
    #[test]
    fn a_deferred_follow_up_after_a_restore_names_the_restore() {
        let mut r = Restorable::with_foo();
        let install = r.install_foo();
        let sv = r.scene.client.wtf().join(FOO_SV);
        fs::write(&sv, "FooDB = { played = 1 }\n").unwrap();
        let client = r.scene.discovered.target.client_dir.clone();
        r.scene.running.insert(client.clone());
        r.scene.while_running = true;
        let deferred = r.undo(install, SvPolicy::Auto).unwrap();
        assert_eq!(deferred.sv.deferred, 1);
        r.scene.running.remove(&client);
        let restored = r.restore().unwrap().tx;
        let err = r.undo(install, SvPolicy::Restore).unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict, "{err:?}");
        assert_eq!(err.fix.argv().unwrap()[2], restored.to_string());
        assert!(err.alternatives.is_empty(), "{:?}", err.alternatives);
        assert_eq!(r.live(), r.before, "the restored settings stand");
    }

    /// M3-D28 rule 1, end to end: a restore undone, then an older install undone with its
    /// SavedVariables restored. Redoing the restore (undoing its undo) would put a whole
    /// tree over what that undo wrote: refused, naming that undo. An install and its own
    /// undo after a restore cancel out, so they never block its undo.
    #[test]
    fn a_restore_is_not_put_back_over_settings_an_undo_restored_since() {
        let played = "FooDB = { played = 1 }\n";
        let r = Restorable::with_foo();
        let install = r.install_foo();
        let restored = r.restore().unwrap().tx;
        let undone = r
            .undo(restored, SvPolicy::Auto)
            .unwrap()
            .outcome
            .tx
            .unwrap();
        fs::write(r.scene.client.wtf().join(FOO_SV), played).unwrap();
        let undid = r.undo(install, SvPolicy::Auto).unwrap();
        assert_eq!(undid.sv.restored, 1);
        let line = r.line_of(undid.outcome.tx);
        assert_eq!(line.sv_restored, Some(1));
        let err = r.undo(undone, SvPolicy::Auto).unwrap_err();
        assert_eq!(err.code, ErrorCode::UndoConflict, "{err:?}");
        assert_eq!(err.fix.argv().unwrap()[2], line.tx.to_string());

        let r = Restorable::with_foo();
        let restored = r.restore().unwrap().tx;
        let install = r.install_foo();
        fs::write(r.scene.client.wtf().join(FOO_SV), played).unwrap();
        assert_eq!(r.undo(install, SvPolicy::Auto).unwrap().sv.restored, 1);
        r.undo(restored, SvPolicy::Auto).unwrap();
        assert!(
            !r.scene.client.wtf().join("Config.wtf").exists(),
            "the pre-restore tree, played without Config.wtf, is back"
        );
    }

    // Failpoints exist only in debug builds (DEC-7).
    #[cfg(debug_assertions)]
    #[test]
    fn an_undo_of_a_restore_without_an_exchange_or_while_the_game_runs_moves_nothing() {
        let mut r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let err = failpoint::with_spec("fsx.renameat2.einval", || r.undo(restored, SvPolicy::Auto))
            .unwrap_err();
        assert_eq!(
            reason(&err),
            (
                ErrorCode::FilesystemUnsupported,
                "no_rename_exchange".into()
            )
        );
        assert_eq!(
            (r.live(), r.previous(&restored)),
            (r.before.clone(), r.after.clone())
        );
        let said = format!("{} {}", err.message, err.details);
        assert!(
            ["FAKEACCT", "FakeRealm", "FakeChar"]
                .iter()
                .all(|name| !said.contains(name)),
            "{said}"
        );
        r.scene
            .running
            .insert(r.scene.discovered.target.client_dir.clone());
        r.scene.while_running = true;
        let err = r.undo(restored, SvPolicy::Auto).unwrap_err();
        assert_eq!(
            (err.code, err.details["reason"].as_str()),
            (ErrorCode::GameRunning, Some("saved_variables"))
        );
        assert_eq!(r.live(), r.before);
        r.assert_no_leftovers();
    }

    /// The preview's token covers the live `WTF/`: a play session between the dry run and
    /// the bound run refuses it (`plan_stale`, `observed`), and nothing changes.
    #[test]
    fn a_play_session_after_the_preview_refuses_the_bound_undo() {
        use crate::app::precondition::{UndoBinding, of_undo};

        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let preview = r
            .scene
            .undo_with(UndoSelector::Tx(restored), SvPolicy::Auto, true)
            .unwrap();
        assert!(preview.outcome.dry_run && preview.outcome.tx.is_none());
        let token = of_undo(&[(restored, r.key(), Ok(&preview))]).token();
        let binding = UndoBinding {
            tokens: BTreeMap::from([(restored, token)]),
            preview: vec!["omaddons".into(), "undo".into(), "--dry-run".into()],
        };
        fs::write(r.scene.client.wtf().join(SV).join("A.lua"), "A = 333\n").unwrap();
        let played = r.live();
        let req = UndoRequest {
            selector: UndoSelector::Tx(restored),
            sv_policy: SvPolicy::Auto,
            dry_run: false,
            while_running: false,
            reason: None,
            targets: vec![r.scene.current_target()],
            run: RunId::new(),
        };
        let err = r
            .scene
            .with_ctx("undo", |ctx| {
                let bound = EngineCtx {
                    binding: Some(&binding),
                    ..*ctx
                };
                crate::engine::undo::undo(&req, &bound)
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::PlanStale, "{err:?}");
        assert!(
            err.details["changed"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("observed")),
            "{err:?}"
        );
        assert_eq!(r.live(), played, "nothing changed");
        r.assert_no_leftovers();
    }

    /// `changed_since` counts the play since the restore against the backup's manifest,
    /// and is unknown once that backup is gone.
    #[test]
    fn changed_since_counts_the_play_since_the_restore() {
        let r = Restorable::new(false);
        let restored = r.restore().unwrap().tx;
        let wtf = r.scene.client.wtf();
        fs::write(wtf.join(SV).join("A.lua"), "A = 4444\n").unwrap();
        fs::write(wtf.join(SV).join("Played.lua"), "P = 1\n").unwrap();
        fs::remove_file(wtf.join("Config.wtf")).unwrap();
        let dry = |r: &Restorable| {
            r.scene
                .undo_with(UndoSelector::Tx(restored), SvPolicy::Auto, true)
                .unwrap()
                .wtf
                .unwrap()
                .changed_since
        };
        assert_eq!(dry(&r), Some(3), "one rewritten, one created, one gone");
        fs::remove_dir_all(&r.backup.dir).unwrap();
        assert_eq!(dry(&r), None);
    }

    #[cfg(debug_assertions)]
    mod failing {
        use super::*;

        /// A restore that fails at `spec`: its outcome and the generation before it.
        fn fail_at(spec: &str) -> (Restorable, u64, Result<RestoreOutcome>) {
            let r = Restorable::new(false);
            let generation = r.generation();
            let result = r.run(Twist {
                failpoints: Some(spec),
                ..Twist::default()
            });
            (r, generation, result)
        }

        #[test]
        fn a_failure_before_swapped_rolls_the_tree_back() {
            for at in [
                "engine.restore.staged",
                "engine.journal.prepared",
                "engine.restore.exchanged",
                "engine.restore.snapshotted",
            ] {
                let (r, generation, result) = fail_at(&format!("{at}=error"));
                assert!(result.is_err(), "{at}");
                r.assert_unchanged(generation);
                // The client snapshot dir went with the rolled-back transaction.
                let snapshots = r.scene.client.meta_dir().join("snapshots");
                let left: Vec<_> = fs::read_dir(&snapshots)
                    .map(|l| l.map(|e| e.unwrap().path()).collect())
                    .unwrap_or_default();
                assert_eq!(left, Vec::<PathBuf>::new(), "{at}");
            }
        }

        #[test]
        fn a_failure_from_swapped_rolls_the_restore_forward() {
            for at in [
                "engine.swapped",
                "engine.record.written",
                "engine.history.appended",
            ] {
                let (r, generation, result) = fail_at(&format!("{at}=error"));
                assert!(result.is_ok(), "{at}: {result:?}");
                let done = result.unwrap();
                assert_eq!(r.live(), r.before, "{at}");
                let previous = r.scene.client.snapshots_wtf(&done.tx).join(WTF_PREVIOUS);
                assert_eq!(tree_snapshot(&previous), r.after, "{at}");
                assert_eq!(r.generation(), generation + 1, "{at}");
                let restores = r
                    .history()
                    .iter()
                    .filter(|e| matches!(e.kind, TxKind::Restore))
                    .count();
                assert_eq!(restores, 1, "{at}");
                r.assert_no_leftovers();
            }
        }

        #[test]
        fn an_exchange_the_filesystem_refuses_changes_nothing() {
            let (r, generation, result) = fail_at("fsx.renameat2.einval");
            let err = result.unwrap_err();
            assert_eq!(
                reason(&err),
                (
                    ErrorCode::FilesystemUnsupported,
                    "no_rename_exchange".into()
                )
            );
            r.assert_unchanged(generation);
        }
    }
}
