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
    CommitPayload, FileId, IndexChanges, Journal, Prepared, Recovered, RecoveryAction, RecoveryEnv,
    SourceBackup, Step, StepOp, StepPurpose, recover_all, recover_tx,
};
use super::retention::WTF_PREVIOUS;
use super::running::{PolicyContext, PolicyOp};
use super::swap::snapshot_state;
use super::tx::{
    EngineCtx, cleanup, millis, recovery_failed, refuse_newer_state, retain, ring_after_window,
    running_now, sv_writable, write_status,
};
use crate::backup::tree::{CloneOpts, TreeCopy, clone_tree, hash_files, walk};
use crate::backup::{BackupRef, EntryKind, Integrity, Manifest};
use crate::discover::registry::LockedRegistry;
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorCode, PlanStaleSource, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    ClonePolicy, DirLock, FsxError, GlobalLock, GlobalLockGuard, InflightLock, MutateOpts, Op,
    RenameMode, ScopeTable, SnapshotRoots, StampDigest, WriteSession, Writer, ensure_dir_all,
    failpoint, mount_id, mutate_with, probe_exchange,
};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::model::{FolderName, RunId, Target, TargetKey, TxId, TxKind};
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
fn topology(
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
fn incomplete(backup: &BackupRef, first: Option<&str>, names: &WtfPathRedactor) -> Error {
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

/// The staged copy, before the lock.
struct Staged {
    copy: TreeCopy,
    /// The live `WTF/`'s stamp digest; `None` when there is none.
    live_stamp: Option<StampDigest>,
    integrity: Integrity,
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
    let staged_dir = client.stage(&tx).join("WTF");
    let staged = match stage(ctx, &scopes, backup, plan, &staged_dir, &topology, &names) {
        Ok(staged) => staged,
        Err(error) => {
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), false);
            append_alone(ctx, &scopes, before_restore.as_ref());
            return Err(error);
        }
    };
    let window = Window {
        ctx,
        scopes: &scopes,
        discovered,
        backup,
        plan,
        topology: &topology,
        tx,
        staged: &staged,
        before_restore: before_restore.as_ref(),
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
        append_alone(ctx, &scopes, before_restore.as_ref());
    }
    let (key, held, mut warnings) = result?;
    warnings.extend(retain(ctx, &scopes, key, &target.client_dir));
    let ring = crate::backup::inventory::Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    crate::backup::inventory::rebuild(ctx.paths, &client, &key, ring);
    if staged.copy.copied_bytes > 0 {
        warnings.push(Warning::new(
            WarningCode::BackupCopied,
            format!(
                "{}: the filesystem could not clone the backup's files, so {} bytes were copied to restore them",
                target.id, staged.copy.copied_bytes
            ),
        ));
    }
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
            return Err(Error::hash_mismatch(
                2,
                format!(
                    "backup {} is damaged: {} does not match the hash it was recorded with",
                    backup.id,
                    names.rel(&entry.path)
                ),
                Suggestion::command(["omaddons", "backup", "list"]),
            )
            .details(serde_json::json!({ "backup": backup.id })));
        }
        Integrity::Verified
    } else {
        Integrity::MetadataOnly
    };
    let live_stamp = match &topology.wtf {
        Some(live) => Some(StampDigest::compute(live).map_err(fail)?),
        None => None,
    };
    Ok(Staged {
        copy,
        live_stamp,
        integrity,
    })
}

/// Appends the `before_restore` backup's line under `lock`, unless the history has it
/// already: the backup stands whatever the restore does. A history that cannot be read
/// might hold it, so nothing is appended then.
fn append_before_restore(
    paths: &Paths,
    scopes: &ScopeTable,
    _lock: &GlobalLockGuard,
    line: &HistoryEntry,
) -> Result<()> {
    let text = history::text(paths)?;
    if super::retention::in_history(&text, &line.tx) {
        return Ok(());
    }
    let mut s = WriteSession::untracked(Writer::BackupRestore, scopes, "history");
    ensure_dir_all(&mut s, paths.state_dir())?;
    history::append(&mut s, paths, line)
}

/// Appends the `before_restore` backup's line under a short lock of its own when the
/// restore failed before its window appended it (M3-D26). Best effort: the backup is
/// then listed without a line.
fn append_alone(ctx: &EngineCtx<'_>, scopes: &ScopeTable, line: Option<&HistoryEntry>) {
    let Some(line) = line else { return };
    let Ok(guard) = GlobalLock::acquire(ctx.paths, ctx.command) else {
        return;
    };
    let _ = append_before_restore(ctx.paths, scopes, &guard, line);
    drop(guard);
}

/// Everything the commit window reads.
struct Window<'a> {
    ctx: &'a EngineCtx<'a>,
    scopes: &'a ScopeTable,
    discovered: &'a DiscoveredTarget,
    backup: &'a BackupRef,
    plan: &'a RestorePlan,
    topology: &'a Topology,
    tx: TxId,
    staged: &'a Staged,
    before_restore: Option<&'a HistoryEntry>,
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
        if let Some(line) = self.before_restore {
            append_before_restore(paths, self.scopes, &guard, line)?;
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
        // ubs:ignore — comparing record generations, not secrets
        if current.value.generation != self.plan.generation {
            return Err(moved("records"));
        }
        // The mounts again, and the client's WTF/ as the stage expects it.
        let seen = topology(&client, &self.backup.wtf(), &mount_id)?;
        // ubs:ignore — comparing mount ids, not secrets
        if seen.mounts() != self.topology.mounts() {
            return Err(unsupported("stage_cross_mount", &self.backup.wtf()));
        }
        // ubs:ignore — comparing paths, not secrets
        if seen.wtf != self.topology.wtf {
            return Err(moved("WTF/"));
        }
        // The last running check before the live transition (M3-D29): a refusal check,
        // not exclusion; `--while-running` never lifts it.
        let running = running_now(ctx, std::slice::from_ref(&target));
        if running.dirs.contains(&target.client_dir) {
            let policy_ctx = PolicyContext {
                target_id: &target.id,
                argv: ctx.argv,
                running_unknown: running.unknown.as_deref(),
            };
            let files = usize::try_from(self.plan.files.restored).unwrap_or(usize::MAX);
            super::running::evaluate(
                &policy_ctx,
                ctx.policy,
                true,
                &target.rules,
                &[PolicyOp::RestoreSavedVariables { files }],
                true,
            )?;
        }
        failpoint::hit("engine.revalidated").map_err(fail)?;

        let mut record = current.value.clone();
        record.generation += 1;
        let now = Timestamp::now();
        let history = self.history_line(key, now)?;
        let stage_dir = client.stage(&tx);
        let staged = stage_dir.join("WTF");
        let prepared = Prepared {
            schema_version: super::journal::CURRENT,
            tx,
            target_key: key,
            client_dir: target.client_dir.clone(),
            kind: TxKind::Restore,
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
            staged_stamp: Some(self.staged.copy.stamp_digest.to_string()),
            source_backup: Some(SourceBackup {
                id: self.backup.id,
                dir: FileId::of(&self.backup.dir).ok_or_else(|| {
                    Error::new(
                        ErrorCode::NotFound,
                        format!("backup {} is gone", self.backup.id),
                        Suggestion::command(["omaddons", "backup", "list"]),
                    )
                })?,
            }),
        };
        let actions = paths.state_snapshot(&tx).join("actions.jsonl");
        let mut s = WriteSession::tx(
            tx,
            Writer::BackupRestore,
            self.scopes,
            SnapshotRoots::for_tx(paths, &tx),
            actions,
        );
        let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;
        let live = self.topology.wtf.clone();
        let committed = (|| -> Result<()> {
            failpoint::hit("engine.journal.prepared").map_err(fail)?;
            // Undo data before the first rename, a client snapshot dir included even
            // when there is no live WTF/ (an undo relies on it).
            snapshot_state(&mut s, paths, &tx, &key)?;
            ensure_dir_all(&mut s, &client.snapshots(&tx))?;
            let into_place = match &live {
                Some(live) => (live.clone(), StepOp::Exchange, RenameMode::Exchange),
                None => (client.wtf(), StepOp::Noreplace, RenameMode::NoReplace),
            };
            let backup_wtf = self.backup.wtf();
            rename_tree(
                &mut s,
                &mut journal,
                &Move {
                    from: &staged,
                    to: &into_place.0,
                    op: into_place.1,
                    mode: into_place.2,
                    purpose: StepPurpose::SwapIn,
                    stamp_before: Some(self.staged.copy.stamp_digest),
                    stamp_after: self.staged.live_stamp,
                },
                &backup_wtf,
            )?;
            failpoint::hit("engine.restore.exchanged").map_err(fail)?;
            if live.is_some() {
                let previous = client.snapshots(&tx).join(WTF_PREVIOUS);
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
                    &backup_wtf,
                )?;
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
            failpoint::hit("engine.history.appended").map_err(fail)?;
            journal.mark_committed(&mut s)?;
            failpoint::hit("engine.committed").map_err(fail)?;
            Ok(())
        })();
        if let Err(error) = committed {
            // In-process recovery checks running afresh (a writes_wtf journal waits).
            match recover_tx(&recovery, &tx) {
                // Swapped already: the payload completed it, so the restore stands.
                Ok(Some(Recovered {
                    action: RecoveryAction::RolledForward,
                    warnings: rolled,
                    ..
                })) => warnings.extend(rolled),
                Ok(_) => return Err(error),
                Err(failed) => return Err(recovery_failed(error, &tx, &failed)),
            }
        }
        let held = millis(&guard);
        drop(guard);
        failpoint::hit("engine.unlocked").map_err(fail)?;
        write_status(paths, self.scopes, &target, &record);
        Ok((key, held, warnings))
    }

    /// The restore's history line: one `wtf` op naming the backup.
    fn history_line(&self, key: TargetKey, at: Timestamp) -> Result<HistoryEntry> {
        let wtf = FolderName::new("WTF").map_err(|_| Error::internal("WTF is a folder name"))?;
        Ok(HistoryEntry {
            schema_version: history::CURRENT,
            tx: self.tx,
            kind: TxKind::Restore,
            at,
            target: key,
            actor: self.ctx.actor,
            reason: self.reason.clone(),
            argv: self.ctx.argv.to_vec(),
            ops: vec![HistoryOp {
                op: FolderOp::Wtf,
                folder: wtf,
                addon: String::new(),
                edit: None,
                backup: Some(self.backup.id),
                from: Some(WtfSource::Backup),
                files: Some(self.plan.files.restored),
            }],
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            created_addons_dir: false,
            run: Some(self.run),
            backup: None,
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
            let probe = self.scene.client.stage(&tx).join("probe");
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
                target: self.key(),
                actor: Actor::Agent,
                reason: None,
                argv: Vec::new(),
                ops: Vec::new(),
                result: TxResult::Committed,
                reverts: None,
                sv_deferred: false,
                created_addons_dir: false,
                run: None,
                backup: Some(BackupNote {
                    id: self.backup.id,
                    kind: BackupKind::BeforeRestore,
                    files: 4,
                    bytes: 24,
                }),
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
        let previous = r.scene.client.snapshots(&done.tx).join(WTF_PREVIOUS);
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
        let actions =
            fs::read_to_string(r.scene.paths.state_snapshot(&done.tx).join("actions.jsonl"))
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
        let snapshot = r.scene.client.snapshots(&done.tx);
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
                let previous = r.scene.client.snapshots(&done.tx).join(WTF_PREVIOUS);
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
