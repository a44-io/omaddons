//! A restore of single addons' SavedVariables (M3-D27): `backup restore <id> --addon A`
//! puts back only the SavedVariables files of the folders an addon owned when the backup
//! was taken, under every account and character, as one journaled transaction that undo
//! already understands.
//!
//! Each file is staged from the backup before the lock (checked against its recorded
//! hash when the manifest has one), then, under the lock, exchanged with the live one
//! (`replace`), put where there is none now (`restore`), or, live but not in the backup,
//! moved into the stage (`move aside`). There is no fallback to two renames: a file that
//! is briefly absent reads as "reset this addon" to the game, so a filesystem that cannot
//! exchange is refused before anything is copied, as for a whole restore. The stage then
//! holds the files as they were, and becomes the transaction's `snapshots/<tx>/WTF/`,
//! beside a pre-restore `WTF.manifest.json`: exactly the layout undo's SavedVariables
//! restore reads, so undoing it needs no code of its own. It never runs while the client
//! runs, or may (M3-D29), and names no account, realm or character in any message.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::journal::{
    ClientFacts, CommitPayload, FileId, IndexChanges, Journal, Prepared, Recovered, RecoveryAction,
    RecoveryEnv, SourceBackup, StagedFile, Step, StepOp, StepPurpose, file_stamp, recover_all,
    recover_tx,
};
use super::restore::{
    Reservation, Topology, copied, incomplete, refuse_running, topology, unsupported,
};
use super::swap::{
    SvEntry, SvManifest, SvSkipped, sha256_file, snapshot_state, sv_candidates, sv_skip,
};
use super::tx::{
    EngineCtx, cleanup, millis, note_undo, recovery_failed, refuse_newer_state, retain,
    ring_after_window, sv_writable, write_status,
};
use crate::backup::{BackupRef, EntryKind, Integrity, Manifest, ManifestEntry};
use crate::discover::registry::LockedRegistry;
use crate::discover::target::DiscoveredTarget;
use crate::error::{Error, ErrorCode, PlanStaleSource, Result, Suggestion, Warning};
use crate::fsx::{
    CloneKind, ClonePolicy, FileStamp, FsxError, GlobalLock, InflightLock, MutateOpts, Op,
    RenameMode, ScopeTable, SnapshotRoots, StampDigest, WriteSession, Writer, ensure_dir_all,
    failpoint, mount_id, mutate, mutate_with, sha256_from_hex, sync_filesystem,
};
use crate::model::{FolderName, RunId, Target, TargetKey, TxId, TxKind};
use crate::privacy::WtfPathRedactor;
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryOp, TxResult, WtfSource};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::load_record;
use crate::state::store;

/// One addon whose settings a partial restore puts back: the name its history ops
/// record, and the folders whose SavedVariables it restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonSv {
    pub addon: String,
    pub folders: Vec<FolderName>,
}

/// What a partial restore does to one addon's files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AddonFiles {
    /// Live, and in the backup: exchanged with the backup's copy.
    pub replaced: u64,
    /// In the backup only: put back where there is none now.
    pub restored: u64,
    /// Live only, written since the backup: moved aside, into the undo snapshot.
    pub created_since: u64,
}

/// What a partial restore does with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvAction {
    Replace,
    Restore,
    MoveAside,
}

impl SvAction {
    /// The name a preview's selection records it by.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Restore => "restore",
            Self::MoveAside => "move_aside",
        }
    }
}

/// One file of a partial restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvFile {
    /// Its path under the live `WTF/`, with `/`: the live spelling where there is one,
    /// else the backup's.
    pub live: String,
    /// The backup's manifest entry, when the backup has it.
    pub backup: Option<ManifestEntry>,
    pub action: SvAction,
    /// The addon it belongs to (an index into the request's addons), and its folder.
    pub addon: usize,
    pub folder: FolderName,
}

/// A partial restore's plan: read only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonPlan {
    /// The target's record generation it was planned against (0 while unregistered).
    pub generation: u64,
    /// By live path.
    pub files: Vec<SvFile>,
    /// Per addon, in the request's order.
    pub counts: Vec<AddonFiles>,
    /// Files neither compared nor written: under a `SavedVariables` folder outside the
    /// client, or one spelled two ways (here or in the backup).
    pub out_of_scope: SvSkipped,
}

impl AddonPlan {
    /// The files the backup has a copy of: what is staged.
    fn staged(&self) -> impl Iterator<Item = (&SvFile, &ManifestEntry)> {
        self.files
            .iter()
            .filter_map(|file| file.backup.as_ref().map(|entry| (file, entry)))
    }
}

/// Plans putting `addons`' SavedVariables back from `backup` onto `target`, reading only:
/// the backup restorable and structurally whole, then each candidate file's action from
/// the union of the backup's files and the live ones (M3-D27). Paths match ignoring case,
/// as Wine does, and a file keeps its live spelling where it has one, else the backup's.
pub fn plan(
    paths: &Paths,
    target: &Target,
    backup: &BackupRef,
    addons: &[AddonSv],
) -> Result<AddonPlan> {
    let client = ClientPaths::new(&target.client_dir);
    let names = redactor(&client, backup);
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
    let owners = Owners::new(addons);
    let folders: Vec<FolderName> = addons.iter().flat_map(|a| a.folders.clone()).collect();
    let live = sv_candidates(&client, &folders);
    let mut out_of_scope = live.skipped;

    // The backup's candidates by lowercase path; two spellings of one are left alone.
    let mut backed: BTreeMap<String, Option<&ManifestEntry>> = BTreeMap::new();
    for entry in manifest
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
    {
        let Some(name) = sv_file_name(&entry.path) else {
            continue;
        };
        if owners.of(name).is_none() {
            continue;
        }
        match backed.entry(entry.path.to_ascii_lowercase()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Some(entry));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let first = usize::from(slot.get().is_some());
                out_of_scope.two_spellings += 1 + first;
                slot.insert(None);
            }
        }
    }

    let mut files = Vec::new();
    let mut matched = BTreeSet::new();
    for (rel, present) in &live.files {
        let key = rel.to_ascii_lowercase();
        let Some((addon, folder)) = rel.rsplit('/').next().and_then(|n| owners.of(n)) else {
            continue;
        };
        let entry = match backed.get(&key) {
            Some(None) => continue,
            Some(Some(entry)) => {
                matched.insert(key);
                Some(*entry)
            }
            None => None,
        };
        let (live_path, action) = match (present, entry) {
            (Some(_), Some(_)) => (rel.clone(), SvAction::Replace),
            (Some(_), None) => (rel.clone(), SvAction::MoveAside),
            // The live folder's spelling, the backup's file name.
            (None, Some(entry)) => (respelled(rel, &entry.path), SvAction::Restore),
            (None, None) => continue,
        };
        files.push(SvFile {
            live: live_path,
            backup: entry.cloned(),
            action,
            addon,
            folder: folder.clone(),
        });
    }
    // The backup's files under no live account or character: put back with the backup's
    // spelling, unless the folder they would go in leaves the client.
    let wtf = client.wtf();
    for (key, entry) in &backed {
        let Some(entry) = entry else { continue };
        if matched.contains(key) {
            continue;
        }
        let Some((addon, folder)) = sv_file_name(&entry.path).and_then(|n| owners.of(n)) else {
            continue;
        };
        let dir = parent(&entry.path).map_or_else(|| wtf.clone(), |p| wtf.join(p));
        if let Some(skip) = sv_skip(&client, &dir) {
            out_of_scope.count(skip, 1);
            continue;
        }
        files.push(SvFile {
            live: entry.path.clone(),
            backup: Some((*entry).clone()),
            action: SvAction::Restore,
            addon,
            folder: folder.clone(),
        });
    }
    files.sort_by(|a, b| a.live.cmp(&b.live));

    let mut counts = vec![AddonFiles::default(); addons.len()];
    for file in &files {
        let count = &mut counts[file.addon];
        match file.action {
            SvAction::Replace => count.replaced += 1,
            SvAction::Restore => count.restored += 1,
            SvAction::MoveAside => count.created_since += 1,
        }
    }
    let generation = match target.key {
        Some(key) => load_record(paths, &key)?.value.generation,
        None => 0,
    };
    Ok(AddonPlan {
        generation,
        files,
        counts,
        out_of_scope,
    })
}

/// Which addon and folder a SavedVariables file name (`<Folder>.lua[.bak]`, any case)
/// belongs to; the first addon that names the folder wins. Blizzard's own
/// `SavedVariables.lua` is never one.
struct Owners<'a>(BTreeMap<String, (usize, &'a FolderName)>);

impl<'a> Owners<'a> {
    fn new(addons: &'a [AddonSv]) -> Self {
        let mut owners = BTreeMap::new();
        for (at, addon) in addons.iter().enumerate() {
            for folder in &addon.folders {
                for suffix in [".lua", ".lua.bak"] {
                    let name = format!("{}{suffix}", folder.as_str()).to_ascii_lowercase();
                    owners.entry(name).or_insert((at, folder));
                }
            }
        }
        owners.remove("savedvariables.lua");
        owners.remove("savedvariables.lua.bak");
        Self(owners)
    }

    /// The addon (its index) and folder `name` belongs to.
    fn of(&self, name: &str) -> Option<(usize, FolderName)> {
        self.0
            .get(&name.to_ascii_lowercase())
            .map(|(at, folder)| (*at, (*folder).clone()))
    }
}

/// The file name of `path` (relative to `WTF/`) when it is a SavedVariables file where
/// the game keeps them: `Account/<account>/SavedVariables/<file>` or
/// `Account/<account>/<realm>/<character>/SavedVariables/<file>` (`Account` and
/// `SavedVariables` in any case).
fn sv_file_name(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    let sv = |name: &str| name.eq_ignore_ascii_case("SavedVariables");
    let shaped = match parts.as_slice() {
        [account, _, dir, _] => account.eq_ignore_ascii_case("Account") && sv(dir),
        [account, _, realm, _, dir, _] => {
            account.eq_ignore_ascii_case("Account") && !sv(realm) && sv(dir)
        }
        _ => false,
    };
    shaped
        .then(|| parts.last().copied())
        .flatten()
        .filter(|name| !name.is_empty())
}

/// `path`'s directory part, `None` for a bare name.
fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(dir, _)| dir)
}

/// The live candidate `rel` (its folders spelled as on disk) with the file name `backup`
/// (a backup path) ends in.
fn respelled(rel: &str, backup: &str) -> String {
    let name = backup.rsplit('/').next().unwrap_or(backup);
    match parent(rel) {
        Some(dir) => format!("{dir}/{name}"),
        None => name.to_owned(),
    }
}

/// A redactor that knows the names under the live `WTF/` and the backup's.
struct Names {
    live: WtfPathRedactor,
    backup: WtfPathRedactor,
}

impl Names {
    fn rel(&self, rel: &str) -> String {
        self.backup.text(&self.live.rel(rel))
    }

    /// `error` with every learned name replaced, in its message and its details.
    fn private(&self, mut error: Error) -> Error {
        error.message = self.backup.text(&self.live.text(&error.message));
        let details = self
            .backup
            .text(&self.live.text(&error.details.to_string()));
        error.details = serde_json::from_str(&details).unwrap_or_else(|_| serde_json::json!({}));
        error
    }
}

impl std::ops::Deref for Names {
    type Target = WtfPathRedactor;

    fn deref(&self) -> &WtfPathRedactor {
        &self.live
    }
}

fn redactor(client: &ClientPaths, backup: &BackupRef) -> Names {
    Names {
        live: WtfPathRedactor::for_client(client),
        backup: WtfPathRedactor::for_client(&ClientPaths::new(&backup.dir)),
    }
}

/// `saved_variables_out_of_scope` for the files a partial restore of `target` leaves as
/// they are: counts only, never a path.
pub fn out_of_scope_warnings(target: &str, skipped: SvSkipped) -> Vec<Warning> {
    let mut warnings = Vec::new();
    for (files, where_) in [
        (
            skipped.symlinked,
            "in a WTF folder reached through a symlink outside the client directory",
        ),
        (
            skipped.two_spellings,
            "in a WTF, Account or SavedVariables folder spelled two ways, which the game reads only one of",
        ),
    ] {
        if files > 0 {
            warnings.push(
                Warning::new(
                    crate::error::WarningCode::SavedVariablesOutOfScope,
                    format!(
                        "{files} SavedVariables files of {target}'s restored addons are {where_}, so the restore left them as they are"
                    ),
                )
                .with_fix(Suggestion::text(
                    "with the game closed, copy those files by hand from the backup folder",
                )),
            );
        }
    }
    warnings
}

/// The per-file mount check (M3-D27): each file moves between the stage and its live
/// folder, so that folder (or, not there yet, its nearest existing ancestor) must be on
/// the stage's mount, else `stage_cross_mount`. `backup_dir` is the backup's `WTF/`, which
/// the refusal names for a copy by hand.
pub fn check_mounts(
    client: &ClientPaths,
    plan: &AddonPlan,
    stage_mount: u64,
    backup_dir: &Path,
) -> Result<()> {
    mounts_with(client, plan, stage_mount, backup_dir, &mount_id)
}

/// [`check_mounts`], with the mount ids `mount` gives.
fn mounts_with(
    client: &ClientPaths,
    plan: &AddonPlan,
    stage_mount: u64,
    backup_dir: &Path,
    mount: &dyn Fn(&Path) -> std::io::Result<u64>,
) -> Result<()> {
    let wtf = client.wtf();
    let mut seen = BTreeSet::new();
    for file in &plan.files {
        let dir = parent(&file.live).map_or_else(|| wtf.clone(), |p| wtf.join(p));
        let existing = dir
            .ancestors()
            .find(|p| std::fs::symlink_metadata(p).is_ok())
            .unwrap_or(client.client_dir())
            .to_path_buf();
        if !seen.insert(existing.clone()) {
            continue;
        }
        let id = mount(&existing).map_err(|err| {
            Error::io(
                client.client_dir(),
                &err,
                Suggestion::command(["omaddons", "status"]),
            )
        })?;
        // ubs:ignore — comparing mount ids, not secrets
        if id != stage_mount {
            return Err(unsupported("stage_cross_mount", backup_dir));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The transaction: reserve, plan, preflight and begin (the command's), then stage without
// the lock, revalidate under it, journal, undo data first, a step per file, the stage into
// the snapshot, commit.

/// What the command established before the engine restores addons (M3-D27's plan
/// order, M3-D24's for the rest).
pub struct AddonsRequest<'a> {
    pub target: &'a DiscoveredTarget,
    pub backup: &'a BackupRef,
    /// Held to the commit, then released (dropped) right after the doorbell.
    pub reservation: Reservation,
    pub topology: Topology,
    pub tx: TxId,
    pub inflight: InflightLock,
    pub plan: &'a AddonPlan,
    pub addons: &'a [AddonSv],
    pub reason: Option<String>,
    pub run: RunId,
}

/// What a partial restore did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonsOutcome {
    pub tx: TxId,
    pub key: TargetKey,
    /// `verified` when every copy put back matched a recorded hash.
    pub integrity: Integrity,
    pub lock_held_ms: u64,
    pub warnings: Vec<Warning>,
}

/// One file as staged and found, before the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Prestaged {
    /// The staged copy's SHA-256 and stamp ([`file_stamp`]); `None` for a file the
    /// backup does not have.
    copy: Option<(String, String)>,
    /// The live file's SHA-256 and stamp ([`FileStamp::fingerprint`]); `None` when
    /// there is none.
    live: Option<(String, String)>,
}

/// Everything staged before the lock.
struct Staged {
    /// Per file of the plan, in its order.
    files: Vec<Prestaged>,
    /// Folders under `WTF/` a restored file goes in that are not there now, shallowest
    /// first: created before the first rename, and left by recovery.
    creates: Vec<PathBuf>,
    copied_bytes: u64,
    integrity: Integrity,
}

/// Restores `req.addons`' SavedVariables from `req.backup` (M3-D27).
pub fn restore(ctx: &EngineCtx<'_>, req: AddonsRequest<'_>) -> Result<AddonsOutcome> {
    let AddonsRequest {
        target: discovered,
        backup,
        reservation,
        topology,
        tx,
        inflight,
        plan,
        addons,
        reason,
        run,
    } = req;
    let target = &discovered.target;
    let client = ClientPaths::new(&target.client_dir);
    let scopes = ScopeTable::new(ctx.paths, std::slice::from_ref(&target.client_dir), None)?;
    let names = redactor(&client, backup);
    let staged_dir = client.stage_wtf(&tx).join("WTF");
    let prepared =
        check_mounts(&client, plan, topology.stage_mount, &backup.wtf()).and_then(|()| {
            let into = Into {
                client: &client,
                target_id: &target.id,
                staged: &staged_dir,
            };
            stage(&scopes, &into, backup, plan, &names)
        });
    let staged = match prepared {
        Ok(staged) => staged,
        Err(error) => {
            cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), false);
            return Err(error);
        }
    };
    let window = Window {
        ctx,
        scopes: &scopes,
        discovered,
        backup,
        topology: &topology,
        tx,
        plan,
        addons,
        staged: &staged,
        names: &names,
        reason,
        run,
    };
    let result = window.commit();
    let committed = result.is_ok();
    ring_after_window(ctx, &result);
    drop(reservation);
    cleanup(ctx.paths, &scopes, inflight, &tx, Some(&client), committed);
    let (key, held, mut warnings) = result?;
    warnings.extend(retain(ctx, &scopes, key, &target.client_dir));
    warnings.extend(copied(&target.id, staged.copied_bytes));
    Ok(AddonsOutcome {
        tx,
        key,
        integrity: staged.integrity,
        lock_held_ms: held,
        warnings,
    })
}

/// Where a partial restore stages, and for whom.
struct Into<'a> {
    client: &'a ClientPaths,
    target_id: &'a str,
    /// `<meta>/stage/<tx>/WTF`.
    staged: &'a Path,
}

/// Stages the backup's copy of each file at `staged/<live path>` (no lock): an untracked
/// clone with the manifest's mode, checked against the manifest's hash when it has one
/// (`hash_mismatch` before anything live moves), then one `syncfs`. Then each live file's
/// hash and stamp, which the commit window checks again; a file there that the plan saw
/// absent, or gone that it saw, is `plan_stale`.
fn stage(
    scopes: &ScopeTable,
    into: &Into<'_>,
    backup: &BackupRef,
    plan: &AddonPlan,
    names: &Names,
) -> Result<Staged> {
    let (client, staged) = (into.client, into.staged);
    let private = |e: Error| names.private(e);
    let fail = |path: &Path, e: &std::io::Error| {
        private(Error::io(
            path,
            e,
            Suggestion::command(["omaddons", "status"]),
        ))
    };
    let mut s = WriteSession::untracked(Writer::BackupRestore, scopes, "restore-stage");
    ensure_dir_all(&mut s, staged).map_err(|e| private(e.into()))?;
    let mut copies: BTreeMap<&str, (String, String)> = BTreeMap::new();
    let mut copied_bytes = 0;
    let mut checked = 0usize;
    for (file, entry) in plan.staged() {
        let dest = staged.join(&file.live);
        if let Some(dir) = dest.parent() {
            ensure_dir_all(&mut s, dir).map_err(|e| private(e.into()))?;
        }
        let source = backup.wtf().join(&entry.path);
        let op = Op::CloneFile {
            from: &source,
            policy: ClonePolicy::Preferred,
        };
        let record = mutate(&mut s, &dest, op).map_err(|e| private(e.into()))?;
        if matches!(record.clone, Some(CloneKind::ByteCopy)) {
            copied_bytes += entry.size;
        }
        mutate(
            &mut s,
            &dest,
            Op::Chmod {
                mode: entry.mode & 0o7777,
            },
        )
        .map_err(|e| private(e.into()))?;
        let sha256 = match record.after_sha256() {
            Some(sha256) => sha256.to_owned(),
            None => sha256_file(&dest).map_err(private)?,
        };
        if let Some(want) = &entry.sha256 {
            if !want.eq_ignore_ascii_case(&sha256) {
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
            checked += 1;
        }
        let stamp =
            file_stamp(&dest).ok_or_else(|| private(Error::internal("a staged file is gone")))?;
        copies.insert(file.live.as_str(), (sha256, stamp));
    }
    // Where a file moved aside goes: its folder in the stage, which holds no copy when
    // the backup has no file beside it.
    for file in plan.files.iter().filter(|f| f.backup.is_none()) {
        if let Some(dir) = staged.join(&file.live).parent() {
            ensure_dir_all(&mut s, dir).map_err(|e| private(e.into()))?;
        }
    }
    sync_filesystem(staged).map_err(|e| private(e.into()))?;

    let wtf = client.wtf();
    let mut files = Vec::new();
    for file in &plan.files {
        let path = wtf.join(&file.live);
        let stamp = FileStamp::read(&path).map_err(|e| fail(&path, &e))?;
        let live = match (file.action, stamp) {
            (SvAction::Replace | SvAction::MoveAside, Some(stamp)) if stamp.is_file() => {
                let sha256 = sha256_file(&path).map_err(private)?;
                // Read again: a file written while it was hashed is not this one.
                // ubs:ignore — comparing file stamps (evidence of change), not secrets
                if FileStamp::read(&path).ok().flatten() != Some(stamp) {
                    return Err(changed(into.target_id));
                }
                Some((sha256, stamp.fingerprint()))
            }
            (SvAction::Restore, None) => None,
            _ => return Err(changed(into.target_id)),
        };
        files.push(Prestaged {
            copy: copies.get(file.live.as_str()).cloned(),
            live,
        });
    }
    // The folders a restored file goes in that are not there now.
    let mut creates: BTreeSet<PathBuf> = BTreeSet::new();
    for file in plan
        .files
        .iter()
        .filter(|f| matches!(f.action, SvAction::Restore))
    {
        let Some(dir) = parent(&file.live) else {
            continue;
        };
        let mut at = wtf.join(dir);
        while at != wtf && std::fs::symlink_metadata(&at).is_err() {
            creates.insert(at.clone());
            match at.parent() {
                Some(up) => at = up.to_path_buf(),
                None => break,
            }
        }
    }
    let mut creates: Vec<PathBuf> = creates.into_iter().collect();
    creates.sort_by_key(|dir| dir.components().count());
    failpoint::hit("engine.restore.staged").map_err(|e| fail(staged, &e))?;
    let staged_count = copies.len();
    let integrity = if staged_count > 0 && checked == staged_count {
        Integrity::Verified
    } else {
        Integrity::MetadataOnly
    };
    Ok(Staged {
        files,
        creates,
        copied_bytes,
        integrity,
    })
}

/// `plan_stale`: `target`'s live SavedVariables changed since the restore was planned (the
/// game ran); retrying plans against them as they are now.
fn changed(target: &str) -> Error {
    Error::plan_stale(
        PlanStaleSource::Replan,
        format!("{target}'s SavedVariables changed since the restore was planned"),
        Suggestion::text("run the command again; it plans against the current state"),
    )
}

/// Everything the commit window reads.
struct Window<'a> {
    ctx: &'a EngineCtx<'a>,
    scopes: &'a ScopeTable,
    discovered: &'a DiscoveredTarget,
    backup: &'a BackupRef,
    topology: &'a Topology,
    tx: TxId,
    plan: &'a AddonPlan,
    addons: &'a [AddonSv],
    staged: &'a Staged,
    names: &'a Names,
    reason: Option<String>,
    run: RunId,
}

impl Window<'_> {
    /// Under the global lock: recover, register, revalidate (records, mounts, every live
    /// file as hashed, and the last running check), journal, undo data, a step per file,
    /// the stage into the snapshot, commit. Returns the key, how long the lock was held,
    /// and the recovery's warnings.
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
        let wtf = client.wtf();
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
        let current = load_record(paths, &key)?;
        // ubs:ignore — comparing record generations, not secrets
        if current.value.generation != self.plan.generation {
            return Err(match ctx.binding {
                Some(binding) => binding.moved(&target.id),
                None => Error::plan_stale(
                    PlanStaleSource::Replan,
                    format!(
                        "{}'s records changed since the restore was planned",
                        target.id
                    ),
                    Suggestion::text("run the command again; it plans against the current state"),
                ),
            });
        }
        // The mounts again, and every live file as it was hashed.
        let by_hand = self.backup.wtf();
        let seen = topology(&client, &by_hand, &mount_id)?;
        // ubs:ignore — comparing mount ids, not secrets
        if (seen.client_mount, seen.stage_mount)
            != (self.topology.client_mount, self.topology.stage_mount)
        {
            return Err(unsupported("stage_cross_mount", &by_hand));
        }
        check_mounts(&client, self.plan, self.topology.stage_mount, &by_hand)?;
        for (file, staged) in self.plan.files.iter().zip(&self.staged.files) {
            let now = FileStamp::read(&wtf.join(&file.live))
                .ok()
                .flatten()
                .map(|stamp| stamp.fingerprint());
            // ubs:ignore — comparing file stamps (evidence of change), not secrets
            if now.as_deref() != staged.live.as_ref().map(|(_, stamp)| stamp.as_str()) {
                return Err(match ctx.binding {
                    Some(binding) => binding.observed(&target.id),
                    None => changed(&target.id),
                });
            }
        }
        // The last running check before the live transition (M3-D29): a refusal check,
        // not exclusion; `--while-running` never lifts it.
        refuse_running(ctx, &target, self.plan.files.len() as u64)?;
        failpoint::hit("engine.revalidated").map_err(fail)?;

        let mut record = current.value.clone();
        record.generation += 1;
        let now = Timestamp::now();
        let history = self.history_line(key, now);
        let stage_dir = client.stage_wtf(&tx);
        let staged = stage_dir.join("WTF");
        let staged_files: Vec<StagedFile> = self
            .plan
            .files
            .iter()
            .zip(&self.staged.files)
            .filter_map(|(file, pre)| {
                pre.copy.as_ref().map(|(_, stamp)| StagedFile {
                    rel: file.live.clone(),
                    stamp: stamp.clone(),
                })
            })
            .collect();
        let prepared = Prepared {
            schema_version: super::journal::CURRENT,
            tx,
            target_key: key,
            client_dir: target.client_dir.clone(),
            kind: TxKind::Restore,
            started_at: now,
            creates_dirs: self.staged.creates.clone(),
            commit: CommitPayload {
                record: record.clone(),
                index: IndexChanges::default(),
                history: history.clone(),
                stage_dir: stage_dir.clone(),
                sv_restore: None,
            },
            writes_wtf: true,
            staged_stamp: None,
            staged_files: Some(staged_files),
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
            client: Some(ClientFacts {
                target_id: target.id.clone(),
                family: target.family,
                version: target.version.to_string(),
            }),
        };
        // The files as they are before: undo's SavedVariables manifest (M3-D27).
        let manifest = SvManifest {
            tx,
            entries: self
                .plan
                .files
                .iter()
                .zip(&self.staged.files)
                .map(|(file, pre)| SvEntry {
                    path: file.live.clone(),
                    sha256: pre.live.as_ref().map(|(sha256, _)| sha256.clone()),
                })
                .collect(),
        };
        let manifest = serde_json::to_vec_pretty(&manifest).map_err(|e| {
            Error::internal(format!("serializing the SavedVariables manifest: {e}"))
        })?;
        let actions = paths.state_snapshot(&tx).join("actions.jsonl");
        let mut s = WriteSession::tx(
            tx,
            Writer::BackupRestore,
            self.scopes,
            SnapshotRoots::for_tx(paths, &tx),
            actions,
        );
        let mut journal = Journal::create(&mut s, paths, &guard, prepared)?;
        let private = |e: Error| self.names.private(e);
        let committed = (|| -> Result<()> {
            failpoint::hit("engine.journal.prepared").map_err(fail)?;
            // Undo data before the first rename.
            snapshot_state(&mut s, paths, &tx, &key)?;
            let snapshot = client.snapshots(&tx);
            ensure_dir_all(&mut s, &snapshot)?;
            let op = Op::WriteFile {
                content: crate::fsx::Content::Bytes(&manifest),
                mode: Some(0o600),
            };
            mutate(&mut s, &snapshot.join("WTF.manifest.json"), op)?;
            failpoint::hit("engine.snapshot.sv").map_err(fail)?;
            for dir in &self.staged.creates {
                ensure_dir_all(&mut s, dir).map_err(|e| private(e.into()))?;
            }
            for (file, pre) in self.plan.files.iter().zip(&self.staged.files) {
                let (stage_side, live) = (staged.join(&file.live), wtf.join(&file.live));
                let copy = pre.copy.as_ref().map(|(sha256, _)| sha256.as_str());
                let was = pre.live.as_ref().map(|(sha256, _)| sha256.as_str());
                let rename = match file.action {
                    SvAction::Replace => Rename {
                        from: &stage_side,
                        to: &live,
                        op: StepOp::Exchange,
                        purpose: StepPurpose::SwapIn,
                        before: copy,
                        after: was,
                    },
                    SvAction::Restore => Rename {
                        from: &stage_side,
                        to: &live,
                        op: StepOp::Noreplace,
                        purpose: StepPurpose::SwapIn,
                        before: copy,
                        after: None,
                    },
                    SvAction::MoveAside => Rename {
                        from: &live,
                        to: &stage_side,
                        op: StepOp::Noreplace,
                        purpose: StepPurpose::RemoveToSnapshot,
                        before: was,
                        after: None,
                    },
                };
                rename_file(&mut s, &mut journal, &rename, &by_hand).map_err(private)?;
            }
            failpoint::hit("engine.restore.exchanged").map_err(fail)?;
            // The stage, now holding the files as they were, becomes the snapshot undo
            // restores from.
            let stamp = StampDigest::compute(&staged).map_err(|e| {
                private(Error::io(
                    &staged,
                    &e,
                    Suggestion::command(["omaddons", "status"]),
                ))
            })?;
            let into_snapshot = snapshot.join("WTF");
            let step = Step {
                op: StepOp::Noreplace,
                from: staged.clone(),
                to: into_snapshot.clone(),
                purpose: StepPurpose::OldToSnapshot,
            };
            journal.step(&mut s, step)?;
            failpoint::hit("engine.swap.before_rename").map_err(fail)?;
            let opts = MutateOpts {
                stamp_before: Some(stamp),
                ..MutateOpts::default()
            };
            let op = Op::Rename {
                to: &into_snapshot,
                mode: RenameMode::NoReplace,
            };
            mutate_with(&mut s, &staged, op, opts).map_err(|e| private(e.into()))?;
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

    /// The history line: one `saved_variables` op per folder that has files, naming the
    /// backup they came from and how many it put in place or moved aside.
    fn history_line(&self, key: TargetKey, at: Timestamp) -> HistoryEntry {
        let mut ops: Vec<HistoryOp> = Vec::new();
        for file in &self.plan.files {
            let addon = self.addons[file.addon].addon.clone();
            // ubs:ignore — comparing folder keys, not secrets
            let same = |op: &&mut HistoryOp| op.folder.key() == file.folder.key();
            match ops.iter_mut().find(same) {
                Some(op) => op.files = Some(op.files.unwrap_or(0) + 1),
                None => ops.push(HistoryOp {
                    op: FolderOp::SavedVariables,
                    folder: file.folder.clone(),
                    addon,
                    edit: None,
                    backup: Some(self.backup.id),
                    from: Some(WtfSource::Backup),
                    files: Some(1),
                }),
            }
        }
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: self.tx,
            kind: TxKind::Restore,
            at,
            target: key,
            actor: self.ctx.actor,
            reason: self.reason.clone(),
            argv: self.ctx.argv.to_vec(),
            ops,
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(self.run),
            backup: None,
        }
    }
}

/// One journaled rename of a single file, with the SHA-256s of what is at `from` before
/// and after (so the session reads neither).
struct Rename<'a> {
    from: &'a Path,
    to: &'a Path,
    op: StepOp,
    purpose: StepPurpose,
    before: Option<&'a str>,
    after: Option<&'a str>,
}

/// Journals `r`, then renames. A filesystem that cannot do the flagged rename is
/// `filesystem_unsupported` (`backup_wtf` named): the step did not run, so rolling back is
/// a no-op, and there is no fallback (M3-D27).
fn rename_file(
    s: &mut WriteSession<'_>,
    journal: &mut Journal,
    r: &Rename<'_>,
    backup_wtf: &Path,
) -> Result<()> {
    let step = Step {
        op: r.op,
        from: r.from.to_path_buf(),
        to: r.to.to_path_buf(),
        purpose: r.purpose,
    };
    journal.step(s, step)?;
    failpoint::hit("engine.swap.before_rename")
        .map_err(|e| Error::io(r.from, &e, Suggestion::command(["omaddons", "status"])))?;
    let mode = match r.op {
        StepOp::Exchange => RenameMode::Exchange,
        StepOp::Noreplace | StepOp::Rename => RenameMode::NoReplace,
    };
    let opts = MutateOpts {
        sha256_before: r.before.and_then(sha256_from_hex),
        sha256_after: r.after.and_then(sha256_from_hex),
        ..MutateOpts::default()
    };
    let op = Op::Rename { to: r.to, mode };
    match mutate_with(s, r.from, op, opts) {
        Ok(_) => Ok(()),
        Err(FsxError::Unsupported) => Err(unsupported("no_rename_exchange", backup_wtf)),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::backup::BackupKind;
    use crate::backup::create::{CreateRequest, create};
    use crate::engine::restore::{Probe, preflight, reserve};
    use crate::engine::testing::TxScene;
    use crate::model::Actor;
    use crate::output::ProgressSink;

    fn foo() -> FolderName {
        FolderName::new("Foo").unwrap()
    }

    #[test]
    fn saved_variables_files_are_where_the_game_keeps_them() {
        for (path, name) in [
            ("Account/A/SavedVariables/Foo.lua", Some("Foo.lua")),
            ("account/A/savedvariables/Foo.lua", Some("Foo.lua")),
            (
                "Account/A/R/C/SavedVariables/Foo.lua.bak",
                Some("Foo.lua.bak"),
            ),
            ("Account/A/SavedVariables/C/SavedVariables/Foo.lua", None),
            ("Account/A/Foo.lua", None),
            ("Account/A/R/C/X/SavedVariables/Foo.lua", None),
            ("Account/A/SavedVariables/", None),
            ("Config.wtf", None),
        ] {
            assert_eq!(sv_file_name(path), name, "{path}");
        }
        assert_eq!(
            respelled(
                "Account/A/SavedVariables/Foo.lua",
                "Account/a/savedvariables/FOO.lua"
            ),
            "Account/A/SavedVariables/FOO.lua",
            "the live folders, the backup's file name"
        );
        let addons = [AddonSv {
            addon: "Foo".into(),
            folders: vec![foo(), FolderName::new("SavedVariables").unwrap()],
        }];
        let owners = Owners::new(&addons);
        assert_eq!(owners.of("FOO.LUA.BAK"), Some((0, foo())));
        assert_eq!(owners.of("SavedVariables.lua"), None, "Blizzard's own file");
    }

    #[test]
    fn a_folder_on_another_mount_than_the_stage_is_refused() {
        let scene = TxScene::new();
        let wtf = scene.client.wtf();
        let account = wtf.join("Account/FAKEACCT");
        let sv = account.join("SavedVariables");
        fs::create_dir_all(&sv).unwrap();
        let file = |live: &str| SvFile {
            live: live.into(),
            backup: None,
            action: SvAction::MoveAside,
            addon: 0,
            folder: foo(),
        };
        let plan = AddonPlan {
            generation: 0,
            files: vec![
                file("Account/FAKEACCT/SavedVariables/Foo.lua"),
                // A character gone since: judged by its nearest existing folder.
                file("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua"),
            ],
            counts: Vec::new(),
            out_of_scope: SvSkipped::default(),
        };
        let backup = Path::new("/backups/x/WTF");
        mounts_with(&scene.client, &plan, 1, backup, &|_| Ok(1)).unwrap();
        for elsewhere in [&sv, &account] {
            let planted = |p: &Path| Ok(if p == elsewhere.as_path() { 2 } else { 1 });
            let err = mounts_with(&scene.client, &plan, 1, backup, &planted).unwrap_err();
            assert_eq!(
                (err.code, err.details["reason"].as_str()),
                (ErrorCode::FilesystemUnsupported, Some("stage_cross_mount")),
                "{}",
                elsewhere.display()
            );
            assert_eq!(err.details["backup_dir"], "/backups/x/WTF");
        }
    }

    /// Writes `text` at `rel` under `wtf`, its folders made.
    fn write(wtf: &Path, rel: &str, text: &str) {
        let path = wtf.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Registers the scene's client and backs its `WTF/` up.
    fn backed_up(scene: &mut TxScene) -> BackupRef {
        let scopes = ScopeTable::new(
            &scene.paths,
            std::slice::from_ref(&scene.discovered.target.client_dir),
            None,
        )
        .unwrap();
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
            verify: true,
            client_running: false,
            command: "backup create",
        };
        create(&scene.paths, &scopes, &req, &ProgressSink::none()).unwrap();
        crate::backup::list(&scene.client).0.remove(0)
    }

    /// The plan is the union of the backup's files and the live ones, matched ignoring
    /// case; the restore keeps the live spelling, makes the folders of a character gone
    /// since, leaves every other file alone, and writes undo's manifest of how they were.
    #[test]
    fn the_plan_is_the_union_of_both_sides_and_the_restore_follows_it() {
        let mut scene = TxScene::new();
        let wtf = scene.client.wtf();
        let (account, char_, gone, two) = (
            "Account/FAKEACCT/SavedVariables/Foo.lua",
            "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua",
            "Account/FAKEACCT/FakeRealm/FakeCharGone/SavedVariables/Foo.lua.bak",
            "Account/FAKEACCT/FakeRealm/FakeCharTwo/SavedVariables/Foo.lua",
        );
        write(&wtf, account, "Foo = 1\n");
        write(&wtf, char_, "Foo = 2\n");
        write(&wtf, gone, "Foo = 3\n");
        write(&wtf, "Account/FAKEACCT/SavedVariables/Bar.lua", "Bar = 1\n");
        let backup = backed_up(&mut scene);
        // Played: Foo's account file rewritten under another spelling, FakeChar's
        // deleted, FakeCharTwo's created, FakeCharGone deleted, Bar's changed.
        fs::remove_file(wtf.join(account)).unwrap();
        let respelled = "Account/FAKEACCT/SavedVariables/foo.lua";
        write(&wtf, respelled, "Foo = 10\n");
        fs::remove_file(wtf.join(char_)).unwrap();
        write(&wtf, two, "Foo = 20\n");
        fs::remove_dir_all(wtf.join("Account/FAKEACCT/FakeRealm/FakeCharGone")).unwrap();
        write(&wtf, "Account/FAKEACCT/SavedVariables/Bar.lua", "Bar = 2\n");

        let addons = [AddonSv {
            addon: "Foo".into(),
            folders: vec![foo()],
        }];
        let discovered = scene.current_target();
        let planned = plan(&scene.paths, &discovered.target, &backup, &addons).unwrap();
        let actions: Vec<(&str, SvAction)> = planned
            .files
            .iter()
            .map(|f| (f.live.as_str(), f.action))
            .collect();
        assert_eq!(
            actions,
            [
                (char_, SvAction::Restore),
                (gone, SvAction::Restore),
                (two, SvAction::MoveAside),
                (respelled, SvAction::Replace),
            ]
        );
        assert_eq!(
            planned.counts,
            [AddonFiles {
                replaced: 1,
                restored: 2,
                created_since: 1
            }]
        );

        let reservation = reserve(&backup).unwrap();
        let tx = TxId::new();
        let inflight = InflightLock::create(&scene.paths, &tx).unwrap();
        let scopes = ScopeTable::new(
            &scene.paths,
            std::slice::from_ref(&discovered.target.client_dir),
            None,
        )
        .unwrap();
        let probe_dir = scene.client.stage_wtf(&tx).join("probe");
        let probe = Probe {
            scopes: &scopes,
            dir: &probe_dir,
        };
        let topology = preflight(&scene.client, &backup.wtf(), Some(probe)).unwrap();
        let done = scene
            .with_ctx("backup restore", |ctx| {
                restore(
                    ctx,
                    AddonsRequest {
                        target: &discovered,
                        backup: &backup,
                        reservation,
                        topology,
                        tx,
                        inflight,
                        plan: &planned,
                        addons: &addons,
                        reason: None,
                        run: RunId::new(),
                    },
                )
            })
            .unwrap();
        assert_eq!(done.integrity, Integrity::Verified, "every copy checked");
        let read = |rel: &str| fs::read_to_string(wtf.join(rel)).ok();
        assert_eq!(
            read(respelled).as_deref(),
            Some("Foo = 1\n"),
            "the live spelling"
        );
        assert!(
            !fs::read_dir(wtf.join("Account/FAKEACCT/SavedVariables"))
                .unwrap()
                .any(|e| e.unwrap().file_name() == "Foo.lua"),
            "no second spelling"
        );
        assert_eq!(read(char_).as_deref(), Some("Foo = 2\n"));
        assert_eq!(read(gone).as_deref(), Some("Foo = 3\n"), "its folders made");
        assert_eq!(read(two), None, "moved aside");
        assert_eq!(
            read("Account/FAKEACCT/SavedVariables/Bar.lua").as_deref(),
            Some("Bar = 2\n")
        );
        let snapshot = scene.client.snapshots(&tx);
        let manifest: SvManifest =
            serde_json::from_slice(&fs::read(snapshot.join("WTF.manifest.json")).unwrap()).unwrap();
        let before: Vec<(&str, bool)> = manifest
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.sha256.is_some()))
            .collect();
        assert_eq!(
            before,
            [
                (char_, false),
                (gone, false),
                (two, true),
                (respelled, true)
            ]
        );
        assert_eq!(
            fs::read_to_string(snapshot.join("WTF").join(two)).unwrap(),
            "Foo = 20\n",
            "the moved-aside file, kept for undo"
        );
        assert_eq!(
            fs::read_to_string(snapshot.join("WTF").join(respelled)).unwrap(),
            "Foo = 10\n",
            "the replaced one"
        );
    }
}
