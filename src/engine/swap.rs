//! The swap (vision §5.5 steps 7–8): the instant an addon changes on disk, inside the
//! commit window (the caller holds the global lock and has journaled `prepared`).
//!
//! Invariant (D25): at every instant each AddOns folder is the complete old version or
//! the complete new version. A replacement is one `RENAME_EXCHANGE` of the staged
//! folder with the live one; a new folder is one `RENAME_NOREPLACE`, so a folder that
//! appeared after planning is never overwritten (`plan_stale`). Old and removed
//! versions move to `Interface/.omaddons/snapshots/<tx>/AddOns/`. Every rename is
//! journaled first. Phases: everything new goes live, then old versions move to the
//! snapshot, then removals.
//!
//! Where the filesystem lacks the `renameat2` flags (NTFS via ntfs-3g, some FUSE), the
//! rest of the transaction uses two plain renames per folder under the journal and the
//! target reports `atomic_swap: false` with an `atomic_swap_unavailable` warning.
//!
//! The touched folders' SavedVariables are cloned (reflinks where the filesystem can)
//! before the global lock by [`prestage_saved_variables`]; under it,
//! [`finish_saved_variables`] keeps each copy whose file has the same stat stamp, redoes
//! the rest and writes the manifest, so the lock costs one `lstat` per unchanged file
//! (D24). [`snapshot_state`] clones the target's state files under the lock. The
//! SavedVariables manifest holds account, realm and character names; it stays local
//! and only counts are reported.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::journal::{Journal, Step, StepOp, StepPurpose};
use super::stage::Staged;
use crate::discover::find_children;
use crate::error::{Error, PlanStaleSource, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    CloneKind, ClonePolicy, FileStamp, FsxError, MutateOpts, Op, RenameMode, TreeDigest, TreeStamp,
    WriteSession, ensure_dir_all, failpoint, mutate, mutate_with,
};
use crate::limits::RACY_SLACK_NS;
use crate::model::{FolderName, MutableFolderName, TargetKey, TxId};
use crate::state::paths::{ClientPaths, Paths};

/// What happens to one AddOns folder. Every name is a [`MutableFolderName`]: a
/// `blizzard_*` folder cannot reach the swap (D14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOp {
    /// A staged folder that is not live yet.
    Add { name: MutableFolderName },
    /// A staged folder replacing the live one, whose on-disk spelling is kept.
    Replace {
        name: MutableFolderName,
        live: MutableFolderName,
        /// The live folder's digest from the files index; computed when `None`.
        old_tree: Option<TreeDigest>,
    },
    Remove {
        live: MutableFolderName,
        old_tree: Option<TreeDigest>,
    },
}

impl SwapOp {
    fn key(&self) -> &str {
        match self {
            Self::Add { name } | Self::Replace { name, .. } => name.key(),
            Self::Remove { live, .. } => live.key(),
        }
    }
}

/// Folders by what happened to them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SwapCounts {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
}

/// How the swap went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SwapReport {
    /// `false` when the filesystem forced two renames per folder.
    pub atomic_swap: bool,
    pub folders: SwapCounts,
    #[serde(skip)]
    pub warnings: Vec<Warning>,
}

/// SavedVariables snapshotting, by count only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SvReport {
    pub sv_files_snapshotted: usize,
    /// Every clone was a reflink (`true` when nothing needed copying).
    pub reflink: bool,
    /// Files left out, by why.
    pub sv_skipped: SvSkipped,
}

/// SavedVariables files that are never snapshotted or written, by why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SvSkipped {
    /// In a `SavedVariables` directory outside the client directory (a `WTF/` or account
    /// folder symlinked to a sync folder).
    pub symlinked: usize,
    /// Under a folder spelled two ways that differ only in case (`WTF` beside `wtf`): the
    /// game reads only one of them, and which is Wine's to say.
    pub two_spellings: usize,
}

impl SvSkipped {
    pub(super) fn count(&mut self, skip: SvSkip, files: usize) {
        match skip {
            SvSkip::Symlinked => self.symlinked += files,
            SvSkip::TwoSpellings => self.two_spellings += files,
        }
    }

    /// Each reason's larger count.
    pub(super) fn max(self, other: Self) -> Self {
        Self {
            symlinked: self.symlinked.max(other.symlinked),
            two_spellings: self.two_spellings.max(other.two_spellings),
        }
    }
}

/// Why a `SavedVariables` directory is left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SvSkip {
    Symlinked,
    TwoSpellings,
}

/// `snapshots/<tx>/WTF.manifest.json`: every candidate SavedVariables path, relative to
/// `WTF/`, with its SHA-256 before the transaction or `null` when absent. Local only:
/// the paths contain account, realm and character names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SvManifest {
    pub tx: TxId,
    pub entries: Vec<SvEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SvEntry {
    pub path: String,
    pub sha256: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Snapshots.

/// SavedVariables cloned before the global lock (D24): each present candidate's clone in
/// `snapshots/<tx>/WTF/`, by its path relative to `WTF/`, with its SHA-256 and the stamp
/// its file had. [`finish_saved_variables`] keeps each one whose file still has that
/// stamp and redoes the others under the lock.
#[derive(Debug, Default)]
pub struct SvPrestage {
    entries: BTreeMap<String, Prestaged>,
}

impl SvPrestage {
    /// How many files were cloned.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Each copied file, with the stamp it had and its SHA-256.
    pub(super) fn hashes(&self) -> impl Iterator<Item = (&Path, FileStamp, &str)> {
        self.entries
            .values()
            .map(|p| (p.source.as_path(), p.stamp, p.clone.sha256.as_str()))
    }
}

#[derive(Debug)]
struct Prestaged {
    source: PathBuf,
    stamp: FileStamp,
    clone: SvClone,
}

/// A SavedVariables file's copy in a snapshot.
#[derive(Debug)]
struct SvClone {
    sha256: String,
    reflink: bool,
}

/// Clones the SavedVariables of `folders` (account and per-character `<Folder>.lua` and
/// `.lua.bak`, any case; never Blizzard's own `SavedVariables.lua`) into
/// `snapshots/<tx>/WTF/` before the global lock, reading each file once: its hash is
/// the copy's. A file written within the racy slack is waited out, as staging does. One
/// that is still unsettled, vanishes or changes during its copy is left for
/// [`finish_saved_variables`].
pub fn prestage_saved_variables(
    s: &mut WriteSession<'_>,
    tx: &TxId,
    client: &ClientPaths,
    folders: &[FolderName],
) -> Result<SvPrestage> {
    let snapshot = client.snapshots(tx);
    let mut prestage = SvPrestage::default();
    for (rel, present) in sv_candidates(client, folders).files {
        let Some(source) = present else {
            continue;
        };
        let Some(stamp) = settled_stamp(&source) else {
            continue;
        };
        let clone = clone_sv(s, &snapshot, &rel, &source)?;
        if FileStamp::read(&source).ok().flatten() == Some(stamp) {
            let entry = Prestaged {
                source,
                stamp,
                clone,
            };
            prestage.entries.insert(rel, entry);
        } else {
            set_aside(s, &snapshot, &rel)?;
        }
    }
    Ok(prestage)
}

/// Snapshots the SavedVariables of `folders` under the global lock, and writes the
/// manifest. A file prestaged at the stamp it still has keeps its copy and hash, which
/// costs one `lstat`. Any other (new since, changed, unsettled, or not prestaged) is
/// cloned and hashed now, and a copy that no longer matches a candidate is set aside.
/// Files outside the client directory are only counted.
pub fn finish_saved_variables(
    s: &mut WriteSession<'_>,
    tx: &TxId,
    client: &ClientPaths,
    folders: &[FolderName],
    mut prestage: SvPrestage,
) -> Result<SvReport> {
    failpoint::hit("engine.snapshot.sv").map_err(|e| fail(&client.wtf(), &e))?;
    let snapshot = client.snapshots(tx);
    let candidates = sv_candidates(client, folders);
    let mut entries = Vec::new();
    let mut report = SvReport {
        sv_files_snapshotted: 0,
        reflink: true,
        sv_skipped: candidates.skipped,
    };
    for (rel, present) in candidates.files {
        let sha256 = match present {
            Some(path) => {
                let clone = match prestage.entries.remove(&rel) {
                    Some(kept)
                        if kept.source == path
                            && FileStamp::read(&path).ok().flatten() == Some(kept.stamp) =>
                    {
                        kept.clone
                    }
                    stale => {
                        if stale.is_some() {
                            set_aside(s, &snapshot, &rel)?;
                        }
                        clone_sv(s, &snapshot, &rel, &path)?
                    }
                };
                report.sv_files_snapshotted += 1;
                report.reflink &= clone.reflink;
                Some(clone.sha256)
            }
            None => None,
        };
        entries.push(SvEntry { path: rel, sha256 });
    }
    // Copies of files that are gone, or no longer candidates.
    for rel in prestage.entries.into_keys() {
        set_aside(s, &snapshot, &rel)?;
    }
    let manifest = SvManifest { tx: *tx, entries };
    let json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| Error::internal(format!("serializing the SavedVariables manifest: {e}")))?;
    ensure_dir_all(s, &snapshot)?;
    mutate(
        s,
        &snapshot.join("WTF.manifest.json"),
        Op::WriteFile {
            content: crate::fsx::Content::Bytes(&json),
            mode: Some(0o600),
        },
    )?;
    Ok(report)
}

/// The stamp of the regular file `source` once it is settled: a file written within the
/// racy slack is waited out once (at most `RACY_SLACK_NS`, before the lock). `None` when
/// it is gone, not a regular file, or still unsettled (whole-second timestamps, or
/// written again meanwhile).
fn settled_stamp(source: &Path) -> Option<FileStamp> {
    let mut waited = false;
    loop {
        let read_at = Timestamp::now().as_nanosecond();
        let stamp = FileStamp::read(source)
            .ok()
            .flatten()
            .filter(FileStamp::is_file)?;
        let wait = stamp.unsettled_ns(read_at);
        if wait < 0 {
            return Some(stamp);
        }
        if waited || wait > RACY_SLACK_NS {
            return None;
        }
        let nanos = u64::try_from(wait + 1).unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_nanos(nanos));
        waited = true;
    }
}

/// Clones `source` to `<snapshot>/WTF/<rel>`. Its SHA-256 is the copy's after-digest, so
/// the file is read once.
fn clone_sv(
    s: &mut WriteSession<'_>,
    snapshot: &Path,
    rel: &str,
    source: &Path,
) -> Result<SvClone> {
    let dest = snapshot.join("WTF").join(rel);
    if let Some(parent) = dest.parent() {
        ensure_dir_all(s, parent).map_err(|e| private(e.into()))?;
    }
    let op = Op::CloneFile {
        from: source,
        policy: ClonePolicy::Preferred,
    };
    let record = mutate(s, &dest, op).map_err(|e| private(e.into()))?;
    let sha256 = match record.after_sha256() {
        Some(sha256) => sha256.to_owned(),
        None => sha256_file(&dest).map_err(private)?,
    };
    Ok(SvClone {
        sha256,
        reflink: matches!(record.clone, Some(CloneKind::Reflink)),
    })
}

/// Moves a stale copy from `<snapshot>/WTF/<rel>` to `<snapshot>/sv-stale/<attempt>/`,
/// so `WTF/` holds exactly the manifest's files and the path is free for a fresh copy.
/// A plain rename: the attempt dir is new, and it works where renameat2 flags do not.
fn set_aside(s: &mut WriteSession<'_>, snapshot: &Path, rel: &str) -> Result<()> {
    let from = snapshot.join("WTF").join(rel);
    let attempt = ulid::Ulid::generate().to_string();
    let to = snapshot.join("sv-stale").join(attempt).join(rel);
    if let Some(parent) = to.parent() {
        ensure_dir_all(s, parent).map_err(|e| private(e.into()))?;
    }
    let aside = Op::Rename {
        to: &to,
        mode: RenameMode::Replace,
    };
    mutate(s, &from, aside).map_err(|e| private(e.into()))?;
    Ok(())
}

/// An error that never names the file: SavedVariables paths hold account, realm and
/// character names.
fn private(e: Error) -> Error {
    Error::new(
        e.code,
        "a SavedVariables file could not be snapshotted",
        Suggestion::command(["omaddons", "status"]),
    )
}

/// The SavedVariables candidates of some folders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SvCandidates {
    /// (path relative to `WTF/` with `/`, the existing file if any). Every folder keeps
    /// its spelling on disk, as do existing files; absent ones are spelled
    /// `<Folder>.lua`, in `SavedVariables` when that folder is absent too.
    pub files: Vec<(String, Option<PathBuf>)>,
    /// Existing candidate files that are never snapshotted or written, by why.
    pub skipped: SvSkipped,
}

/// Every SavedVariables candidate of `folders` under `<client>/WTF/Account/`, account-wide
/// and per character. `WTF`, `Account` and `SavedVariables` match ignoring case, as Wine
/// does, but a folder spelled two ways is left alone: the game reads only one of them.
/// Account, realm and character directories are followed through symlinks, but a
/// `SavedVariables` directory counts only where it resolves inside the client directory:
/// SavedVariables are protected there and nowhere else, as the write scope admits
/// nothing else. A directory reached under two names is listed once.
pub fn sv_candidates(client: &ClientPaths, folders: &[FolderName]) -> SvCandidates {
    let mut out = SvCandidates::default();
    let mut seen = BTreeSet::new();
    let wanted: Vec<String> = folders
        .iter()
        .flat_map(|folder| {
            [".lua", ".lua.bak"].map(|suffix| format!("{}{suffix}", folder.as_str()))
        })
        .filter(|want| !want.to_ascii_lowercase().starts_with("savedvariables.lua"))
        .collect();
    for (rel, sv) in sv_dirs(client) {
        let present: BTreeMap<String, String> = files(&sv)
            .into_iter()
            .map(|name| (name.to_ascii_lowercase(), name))
            .collect();
        if let Some(skip) = sv_skip(client, &sv) {
            let existing = wanted
                .iter()
                .filter(|want| present.contains_key(&want.to_ascii_lowercase()))
                .count();
            out.skipped.count(skip, existing);
            continue;
        }
        // The same directory under another name is listed once.
        if !seen.insert(resolved(&sv)) {
            continue;
        }
        for want in &wanted {
            match present.get(&want.to_ascii_lowercase()) {
                Some(actual) => out
                    .files
                    .push((format!("{rel}/{actual}"), Some(sv.join(actual)))),
                None => out.files.push((format!("{rel}/{want}"), None)),
            }
        }
    }
    out
}

/// Every `SavedVariables` directory under the client's WTF, account-wide and per
/// character, with its path relative to the WTF it is under, each folder spelled as on
/// disk (an absent `SavedVariables` as `SavedVariables`). Every spelling of `WTF`,
/// `Account` and `SavedVariables` is walked, so what a folder spelled two ways holds is
/// counted.
fn sv_dirs(client: &ClientPaths) -> Vec<(String, PathBuf)> {
    let name = |path: &Path| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    let mut add = |rel: String, dir: PathBuf| {
        let spellings = find_children(&dir, "SavedVariables");
        if spellings.is_empty() {
            out.push((format!("{rel}/SavedVariables"), dir.join("SavedVariables")));
        }
        for sv in spellings {
            out.push((format!("{rel}/{}", name(&sv)), sv));
        }
    };
    for wtf in find_children(client.client_dir(), "WTF") {
        for accounts in find_children(&wtf, "Account") {
            let a = name(&accounts);
            for account in dirs(&accounts) {
                let account_dir = accounts.join(&account);
                add(format!("{a}/{account}"), account_dir.clone());
                for realm in dirs(&account_dir) {
                    if realm.eq_ignore_ascii_case("SavedVariables") {
                        continue;
                    }
                    for character in dirs(&account_dir.join(&realm)) {
                        add(
                            format!("{a}/{account}/{realm}/{character}"),
                            account_dir.join(&realm).join(&character),
                        );
                    }
                }
            }
        }
    }
    out
}

/// Why the `SavedVariables` directory `sv` is left alone, if it is: a folder on its way
/// from the client directory is spelled two ways, or it resolves outside the client
/// directory, the only place SavedVariables are snapshotted and written. A directory
/// that does not exist yet resolves through its nearest existing parent.
pub(super) fn sv_skip(client: &ClientPaths, sv: &Path) -> Option<SvSkip> {
    if spelled_twice(client.client_dir(), sv) {
        return Some(SvSkip::TwoSpellings);
    }
    let inside = std::fs::canonicalize(client.client_dir());
    match (resolved(sv), inside) {
        (Some(real), Ok(inside)) if real.starts_with(&inside) => None,
        _ => Some(SvSkip::Symlinked),
    }
}

/// Whether a folder of `path` below `root` has a sibling whose name differs from it only
/// in case.
fn spelled_twice(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    for component in rel.components() {
        let Some(name) = component.as_os_str().to_str() else {
            return false;
        };
        if find_children(&at, name).len() > 1 {
            return true;
        }
        at.push(component);
    }
    false
}

/// Where `path` really is: the canonical path of its nearest existing ancestor, joined
/// with the rest of it. `None` when nothing resolves.
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        match std::fs::canonicalize(at) {
            Ok(real) => return Some(rest.iter().rev().fold(real, |p, c| p.join(c))),
            Err(_) => {
                rest.push(at.file_name()?);
                at = at.parent()?;
            }
        }
    }
}

/// Digests of live folders without a files-index entry (an unmanaged folder that
/// `install --force` replaces, say), hashed before the global lock, each with the stamp
/// of the tree it read (D24). By folder key.
#[derive(Debug, Default)]
pub struct PreDigests(BTreeMap<String, (TreeStamp, TreeDigest)>);

/// Hashes, before the lock, each live folder a replace or remove will move whose digest
/// the files index does not give. One written within the racy slack, or changed while
/// it was hashed, is left for the swap to hash.
pub fn predigest(addons: &Path, ops: &[SwapOp]) -> PreDigests {
    let mut out = PreDigests::default();
    for op in ops {
        let (SwapOp::Replace {
            live,
            old_tree: None,
            ..
        }
        | SwapOp::Remove {
            live,
            old_tree: None,
        }) = op
        else {
            continue;
        };
        let dir = addons.join(live.as_str());
        let read_at = Timestamp::now().as_nanosecond();
        let Ok(stamp) = TreeStamp::read(&dir) else {
            continue;
        };
        if !stamp.settled(read_at) {
            continue;
        }
        let Ok(digest) = TreeDigest::compute(&dir) else {
            continue;
        };
        if TreeStamp::read(&dir).is_ok_and(|now| now == stamp) {
            out.0.insert(live.key().to_owned(), (stamp, digest));
        }
    }
    out
}

/// Under the lock: fills in the missing digest of each op whose live folder still has
/// the stamp it was hashed at before the lock. The others stay `None`, for the swap to
/// hash.
pub fn with_predigests(ops: &mut [SwapOp], addons: &Path, pre: &PreDigests) {
    for op in ops {
        let (SwapOp::Replace { live, old_tree, .. } | SwapOp::Remove { live, old_tree }) = op
        else {
            continue;
        };
        if old_tree.is_some() {
            continue;
        }
        if let Some((stamp, digest)) = pre.0.get(live.key())
            && TreeStamp::read(&addons.join(live.as_str())).is_ok_and(|now| now == *stamp)
        {
            *old_tree = Some(*digest);
        }
    }
}

/// Clones the target's record, files index and the registry into
/// `<state>/omaddons/snapshots/<tx>/`.
pub fn snapshot_state(
    s: &mut WriteSession<'_>,
    paths: &Paths,
    tx: &TxId,
    key: &TargetKey,
) -> Result<()> {
    let dir = paths.state_snapshots_dir().join(tx.to_string());
    failpoint::hit("engine.snapshot.state").map_err(|e| fail(&dir, &e))?;
    ensure_dir_all(s, &dir)?;
    for source in [
        paths.target_record(key),
        paths.files_index(key),
        paths.registry_file(),
    ] {
        let Some(name) = source.file_name() else {
            continue;
        };
        if std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()) {
            let op = Op::CloneFile {
                from: &source,
                policy: ClonePolicy::Preferred,
            };
            mutate(s, &dir.join(name), op)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The swap.

/// Swaps the staged folders in, old versions and removals out, in folder-key order,
/// journaling each rename first; ends with `swapped`.
pub fn swap(
    s: &mut WriteSession<'_>,
    journal: &mut Journal,
    ops: &[SwapOp],
    staged: &Staged,
    client: &ClientPaths,
) -> Result<SwapReport> {
    let addons = client.addons();
    let snapshot = client.snapshots(&staged.tx).join("AddOns");
    let mut ops: Vec<&SwapOp> = ops.iter().collect();
    ops.sort_by(|a, b| a.key().cmp(b.key()));
    if ops.iter().any(|op| !matches!(op, SwapOp::Add { .. })) {
        ensure_dir_all(s, &snapshot)?;
    }
    let mut run = Run {
        journal,
        atomic: true,
        warnings: Vec::new(),
    };
    let mut counts = SwapCounts::default();

    // 1. Everything new goes live.
    let mut exchanged: Vec<(&MutableFolderName, &MutableFolderName, Option<TreeDigest>)> =
        Vec::new();
    for op in &ops {
        match op {
            SwapOp::Add { name } => {
                let (from, new_tree) = staged_folder(staged, name)?;
                let to = addons.join(name.as_str());
                absent(&addons, name)?;
                let m = Move::new(&from, &to, StepPurpose::SwapIn, Some(new_tree), None);
                run.flagged(s, StepOp::Noreplace, &m, || absent(&addons, name))?;
                counts.added += 1;
            }
            SwapOp::Replace {
                name,
                live,
                old_tree,
            } => {
                let (from, new_tree) = staged_folder(staged, name)?;
                let to = addons.join(live.as_str());
                if std::fs::symlink_metadata(&to).is_err() {
                    return Err(stale(format!(
                        "{} disappeared after planning",
                        live.as_str()
                    )));
                }
                if run.atomic {
                    let m = Move::new(&from, &to, StepPurpose::SwapIn, Some(new_tree), *old_tree);
                    if run.try_flagged(s, StepOp::Exchange, &m)? {
                        exchanged.push((name, live, *old_tree));
                        counts.replaced += 1;
                        continue;
                    }
                }
                let old = snapshot.join(live.as_str());
                let out = StepPurpose::FallbackLiveToSnapshot;
                run.plain(s, &Move::new(&to, &old, out, *old_tree, None))?;
                let back = StepPurpose::FallbackStageToLive;
                run.plain(s, &Move::new(&from, &to, back, Some(new_tree), None))?;
                counts.replaced += 1;
            }
            SwapOp::Remove { .. } => {}
        }
    }
    failpoint::hit("engine.swap.after_exchange").map_err(|e| fail(&addons, &e))?;

    // 2. Old versions, now at the stage path, move to the snapshot.
    for (name, live, old_tree) in exchanged {
        let from = staged.stage_dir.join(name.as_str());
        let to = snapshot.join(live.as_str());
        let m = Move::new(&from, &to, StepPurpose::OldToSnapshot, old_tree, None);
        run.flagged(s, StepOp::Noreplace, &m, || Ok(()))?;
    }

    // 3. Removals.
    for op in &ops {
        if let SwapOp::Remove { live, old_tree } = op {
            let from = addons.join(live.as_str());
            if std::fs::symlink_metadata(&from).is_err() {
                return Err(stale(format!(
                    "{} disappeared after planning",
                    live.as_str()
                )));
            }
            let to = snapshot.join(live.as_str());
            let m = Move::new(&from, &to, StepPurpose::RemoveToSnapshot, *old_tree, None);
            run.flagged(s, StepOp::Noreplace, &m, || Ok(()))?;
            counts.removed += 1;
        }
    }

    run.journal.mark_swapped(s)?;
    failpoint::hit("engine.swapped").map_err(|e| fail(&addons, &e))?;
    Ok(SwapReport {
        atomic_swap: run.atomic,
        folders: counts,
        warnings: run.warnings,
    })
}

/// The rename loop's state: the journal, and whether flagged renames still work.
struct Run<'j> {
    journal: &'j mut Journal,
    atomic: bool,
    warnings: Vec<Warning>,
}

/// One rename: its paths, why, and the moved folder's digests for the actions log.
struct Move<'a> {
    from: &'a Path,
    to: &'a Path,
    purpose: StepPurpose,
    /// The folder at `from` before the rename.
    tree_before: Option<TreeDigest>,
    /// What is at `from` afterwards (the other side of an exchange).
    tree_after: Option<TreeDigest>,
}

impl<'a> Move<'a> {
    fn new(
        from: &'a Path,
        to: &'a Path,
        purpose: StepPurpose,
        tree_before: Option<TreeDigest>,
        tree_after: Option<TreeDigest>,
    ) -> Self {
        Self {
            from,
            to,
            purpose,
            tree_before,
            tree_after,
        }
    }

    fn opts(&self) -> MutateOpts {
        MutateOpts {
            tree_before: self.tree_before,
            tree_after: self.tree_after,
            ..MutateOpts::default()
        }
    }
}

impl Run<'_> {
    /// A flagged rename; when the filesystem lacks the flag, switches to plain renames
    /// for the rest of the transaction, re-checks with `recheck`, and renames plainly.
    fn flagged(
        &mut self,
        s: &mut WriteSession<'_>,
        op: StepOp,
        m: &Move<'_>,
        recheck: impl Fn() -> Result<()>,
    ) -> Result<()> {
        if self.atomic && self.try_flagged(s, op, m)? {
            return Ok(());
        }
        recheck()?;
        self.plain(s, m)
    }

    /// Journals and runs a flagged rename; `Ok(false)` when the filesystem lacks the
    /// flag (the journaled step never ran, which recovery can tell).
    fn try_flagged(&mut self, s: &mut WriteSession<'_>, op: StepOp, m: &Move<'_>) -> Result<bool> {
        let mode = match op {
            StepOp::Exchange => RenameMode::Exchange,
            StepOp::Noreplace | StepOp::Rename => RenameMode::NoReplace,
        };
        let (to, purpose) = (m.to, m.purpose);
        self.step(s, op, m)?;
        match mutate_with(s, m.from, Op::Rename { to, mode }, m.opts()) {
            Ok(_) => Ok(true),
            Err(FsxError::Unsupported) => {
                self.atomic = false;
                self.warnings.push(atomic_swap_unavailable());
                Ok(false)
            }
            Err(FsxError::Precondition(why)) if purpose == StepPurpose::SwapIn => Err(stale(
                format!("{} changed after planning: {why}", to.display()),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Journals and runs a plain rename onto an absent path.
    fn plain(&mut self, s: &mut WriteSession<'_>, m: &Move<'_>) -> Result<()> {
        if std::fs::symlink_metadata(m.to).is_ok() {
            return Err(stale(format!("{} exists", m.to.display())));
        }
        self.step(s, StepOp::Rename, m)?;
        let op = Op::Rename {
            to: m.to,
            mode: RenameMode::Replace,
        };
        mutate_with(s, m.from, op, m.opts())?;
        Ok(())
    }

    fn step(&mut self, s: &mut WriteSession<'_>, op: StepOp, m: &Move<'_>) -> Result<()> {
        let step = Step {
            op,
            from: m.from.to_path_buf(),
            to: m.to.to_path_buf(),
            purpose: m.purpose,
        };
        self.journal.step(s, step)?;
        failpoint::hit("engine.swap.before_rename").map_err(|e| fail(m.from, &e))
    }
}

/// The staged path and digest of `name`.
fn staged_folder(staged: &Staged, name: &MutableFolderName) -> Result<(PathBuf, TreeDigest)> {
    staged
        .folders
        .iter()
        .find(|f| f.name == *name.as_folder())
        .map(|f| (f.path.clone(), f.index.digest()))
        .ok_or_else(|| Error::internal(format!("{} was not staged", name.as_str())))
}

/// `plan_stale` unless AddOns has no entry named `name` in any case.
fn absent(addons: &Path, name: &MutableFolderName) -> Result<()> {
    let taken = std::fs::read_dir(addons)
        .map(|listing| {
            listing.filter_map(std::result::Result::ok).any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name.as_str()))
            })
        })
        .unwrap_or(false);
    if taken {
        return Err(stale(format!(
            "a folder named {} appeared in AddOns after planning",
            name.as_str()
        )));
    }
    Ok(())
}

fn stale(message: String) -> Error {
    Error::plan_stale(
        PlanStaleSource::Replan,
        message,
        Suggestion::text("run the command again; it re-plans against the current folders"),
    )
}

fn fail(path: &Path, e: &std::io::Error) -> Error {
    Error::io(path, e, Suggestion::command(["omaddons", "status"]))
}

/// `saved_variables_out_of_scope`, one warning per reason: SavedVariables of `target`'s
/// changed addons are outside its client directory, or under a folder spelled two ways,
/// so `outcome` says what that cost (never which files: their paths hold account and
/// character names).
pub(super) fn out_of_scope_warnings(
    target: &str,
    skipped: SvSkipped,
    outcome: &str,
) -> Vec<Warning> {
    let mut warnings = Vec::new();
    if skipped.symlinked > 0 {
        warnings.push(
            Warning::new(
                WarningCode::SavedVariablesOutOfScope,
                format!(
                    "{} SavedVariables files of {target}'s changed addons are in a WTF folder reached through a symlink outside the client directory, so {outcome}",
                    skipped.symlinked
                ),
            )
            .with_fix(Suggestion::text(
                "keep WTF/ inside the client directory, not a symlink to a sync folder, for undo to restore SavedVariables",
            )),
        );
    }
    if skipped.two_spellings > 0 {
        warnings.push(
            Warning::new(
                WarningCode::SavedVariablesOutOfScope,
                format!(
                    "{} SavedVariables files of {target}'s changed addons are in a WTF, Account or SavedVariables folder spelled two ways, which the game reads only one of, so {outcome}",
                    skipped.two_spellings
                ),
            )
            .with_fix(Suggestion::text(
                "keep one spelling of each folder under WTF/ (merge wtf/ into WTF/, say) for undo to restore SavedVariables",
            )),
        );
    }
    warnings
}

/// `atomic_swap_unavailable`: the swap fell back to two journaled renames per folder,
/// said by the swap and by an in-process roll-forward of a swap that did.
pub(super) fn atomic_swap_unavailable() -> Warning {
    Warning::new(
        WarningCode::AtomicSwapUnavailable,
        "this filesystem cannot swap folders atomically, so omaddons used two renames per folder under its journal",
    )
    .with_fix(Suggestion::text(
        "the AddOns filesystem does not support atomic renames; omaddons stays crash-safe through its journal",
    ))
}

/// The swap report of a transaction an in-process roll-forward completed (the swap's
/// own report is lost with the error): `atomic_swap` from its journal, and
/// `atomic_swap_unavailable` when it was not, before what the roll-forward `left`.
pub(super) fn rolled_forward(atomic_swap: bool, left: Vec<Warning>) -> (bool, Vec<Warning>) {
    let mut warnings = Vec::with_capacity(left.len() + 1);
    if !atomic_swap {
        warnings.push(atomic_swap_unavailable());
    }
    warnings.extend(left);
    (atomic_swap, warnings)
}

/// The lowercase hex SHA-256 of the file at `path`, streamed (SavedVariables reach
/// 64 MiB, and undo staging hashes every restored file).
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    crate::fsx::count_hashed();
    crate::state::files_index::hash_file_streaming(path)
        .map(|(_, sha256)| sha256)
        .map_err(|e| fail(path, &e))
}

/// Directory names in `dir`, symlinks to directories included, sorted; nothing when it
/// cannot be read.
fn dirs(dir: &Path) -> Vec<String> {
    entries(dir, true)
}

/// Regular file names in `dir` (never a symlink), sorted.
fn files(dir: &Path) -> Vec<String> {
    entries(dir, false)
}

fn entries(dir: &Path, want_dirs: bool) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = listing
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            if want_dirs {
                std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir())
            } else {
                e.file_type().is_ok_and(|t| t.is_file())
            }
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::journal::{Phase, RecoveryEnv, StepRecord, parse, recover_all};
    use crate::engine::testing::{Scene, folder, snapshot_saved_variables};
    use crate::error::ErrorCode;
    use crate::state::files_index::FolderIndex;

    fn name(n: &str) -> MutableFolderName {
        FolderName::new(n).unwrap().into_mutable().unwrap()
    }

    /// Replace Foo, add Bar, remove Gone.
    fn ops() -> Vec<SwapOp> {
        vec![
            SwapOp::Replace {
                name: name("Foo"),
                live: name("Foo"),
                old_tree: None,
            },
            SwapOp::Add { name: name("Bar") },
            SwapOp::Remove {
                live: name("Gone"),
                old_tree: None,
            },
        ]
    }

    fn steps(scene: &Scene) -> (Vec<StepRecord>, Phase) {
        let text = std::fs::read_to_string(scene.paths.journal(&scene.tx)).unwrap();
        let parsed = parse(&text).unwrap();
        (parsed.steps, parsed.phase)
    }

    fn purposes(steps: &[StepRecord]) -> Vec<(StepOp, StepPurpose, String)> {
        steps
            .iter()
            .map(|s| {
                (
                    s.op,
                    s.purpose,
                    s.to.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn run(scene: &Scene, ops: &[SwapOp]) -> Result<SwapReport> {
        let lock = scene.lock();
        let mut s = scene.session();
        let mut journal = scene.journal(&mut s, &lock);
        swap(&mut s, &mut journal, ops, &scene.staged, &scene.client)
    }

    fn tree(path: &Path) -> TreeDigest {
        TreeDigest::compute(path).unwrap()
    }

    #[test]
    fn replace_add_and_remove() {
        let scene = Scene::new();
        let old_foo = tree(&scene.client.addons().join("Foo"));
        let old_gone = tree(&scene.client.addons().join("Gone"));
        let new_foo: &FolderIndex = &scene
            .staged
            .folders
            .iter()
            .find(|f| f.name.as_str() == "Foo")
            .unwrap()
            .index;
        let report = run(&scene, &ops()).unwrap();

        assert_eq!(
            (report.atomic_swap, report.folders),
            (
                true,
                SwapCounts {
                    added: 1,
                    replaced: 1,
                    removed: 1
                }
            )
        );
        assert!(report.warnings.is_empty());
        let addons = scene.client.addons();
        assert_eq!(
            tree(&addons.join("Foo")),
            new_foo.digest(),
            "the live tree is the staged tree"
        );
        assert_eq!(
            (
                scene.notes("Foo").as_deref(),
                scene.notes("Bar").as_deref(),
                scene.notes("Gone")
            ),
            (Some("new foo"), Some("new bar"), None)
        );
        let snapshot = scene.client.snapshots(&scene.tx).join("AddOns");
        assert_eq!(
            tree(&snapshot.join("Foo")),
            old_foo,
            "the old version is in the snapshot"
        );
        assert_eq!(tree(&snapshot.join("Gone")), old_gone);
        assert!(
            !scene.staged.stage_dir.join("Foo").exists()
                && !scene.staged.stage_dir.join("Bar").exists()
        );

        let (steps, phase) = steps(&scene);
        assert_eq!(phase, Phase::Swapped);
        assert_eq!(
            purposes(&steps),
            [
                (StepOp::Noreplace, StepPurpose::SwapIn, "Bar".into()),
                (StepOp::Exchange, StepPurpose::SwapIn, "Foo".into()),
                (StepOp::Noreplace, StepPurpose::OldToSnapshot, "Foo".into()),
                (
                    StepOp::Noreplace,
                    StepPurpose::RemoveToSnapshot,
                    "Gone".into()
                ),
            ],
            "new folders live first, in key order; then the old version; then removals"
        );
        let actions = std::fs::read_to_string(scene.actions_log()).unwrap();
        let exchange: serde_json::Value = actions
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|a| a["op"] == "rename_exchange")
            .unwrap();
        assert_eq!(
            exchange["before_hash"],
            new_foo.tree.as_str(),
            "the staged digest, not a re-hash"
        );
        assert_eq!(exchange["after_hash"], old_foo.to_string());
    }

    #[test]
    fn a_folder_that_appeared_after_planning_is_never_overwritten() {
        let scene = Scene::new();
        folder(&scene.client.addons().join("bar"), "someone else's");
        let err = run(&scene, &ops()).unwrap_err();
        assert_eq!(
            (err.code, err.retryable),
            (ErrorCode::PlanStale, true),
            "{}",
            err.message
        );
        assert_eq!(scene.notes("bar").as_deref(), Some("someone else's"));
        assert_eq!(
            scene.notes("Foo").as_deref(),
            Some("old foo"),
            "nothing swapped before the check"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn without_renameat2_flags_two_renames_under_the_journal() {
        let scene = Scene::new();
        let report = failpoint::with_spec("fsx.renameat2.einval", || run(&scene, &ops())).unwrap();
        assert!(!report.atomic_swap);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].code, WarningCode::AtomicSwapUnavailable);
        assert_eq!(report.warnings[0].fix.as_ref().and_then(|f| f.argv()), None);
        assert_eq!(
            (
                scene.notes("Foo").as_deref(),
                scene.notes("Bar").as_deref(),
                scene.notes("Gone")
            ),
            (Some("new foo"), Some("new bar"), None)
        );
        let (steps, _) = steps(&scene);
        let ran: Vec<_> = purposes(&steps)
            .into_iter()
            .filter(|(op, _, _)| *op == StepOp::Rename)
            .collect();
        assert_eq!(
            ran,
            [
                (StepOp::Rename, StepPurpose::SwapIn, "Bar".into()),
                (
                    StepOp::Rename,
                    StepPurpose::FallbackLiveToSnapshot,
                    "Foo".into()
                ),
                (
                    StepOp::Rename,
                    StepPurpose::FallbackStageToLive,
                    "Foo".into()
                ),
                (StepOp::Rename, StepPurpose::RemoveToSnapshot, "Gone".into()),
            ]
        );
        assert_eq!(
            steps[0].op,
            StepOp::Noreplace,
            "the flagged attempt is journaled; recovery sees it never ran"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_crash_between_exchange_and_snapshot_move_rolls_back() {
        let mut scene = Scene::new();
        let before = tree(&scene.client.addons());
        let err = failpoint::with_spec("engine.swap.after_exchange=error", || run(&scene, &ops()))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(
            scene.notes("Foo").as_deref(),
            Some("new foo"),
            "crashed mid-swap"
        );
        let lock = scene.lock();
        let recover = |scene: &Scene| {
            recover_all(&RecoveryEnv {
                lock: &lock,
                paths: &scene.paths,
                scopes: &scene.scopes,
                own: std::slice::from_ref(&scene.key),
                sv_writable: &|_| false,
            })
            .unwrap()
        };
        let warnings = recover(&scene);
        assert_eq!(
            warnings.len(),
            0,
            "the in-flight lock is still held by the scene"
        );
        scene.release_inflight();
        let warnings = recover(&scene);
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            tree(&scene.client.addons()),
            before,
            "the old trees are back"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn saved_variables_snapshot_counts_only() {
        let scene = Scene::new();
        let mut s = scene.session();
        let folders = [name("Foo"), name("Gone"), name("Bar")].map(MutableFolderName::into_folder);
        let report = failpoint::with_spec("fsx.reflink.unsupported", || {
            snapshot_saved_variables(&mut s, &scene.tx, &scene.client, &folders)
        })
        .unwrap();
        assert_eq!(
            report,
            SvReport {
                sv_files_snapshotted: 4,
                reflink: false,
                sv_skipped: SvSkipped::default(),
            }
        );
        let snap = scene
            .client
            .snapshots(&scene.tx)
            .join("WTF/Account/123456#1");
        for file in [
            "SavedVariables/Foo.lua",
            "SavedVariables/foo.lua.bak",
            "RealmName/CharName/SavedVariables/FOO.lua",
            "RealmName/CharName/SavedVariables/Gone.lua",
        ] {
            assert!(snap.join(file).is_file(), "{file}");
        }
        assert!(
            !snap.join("SavedVariables/SavedVariables.lua").exists()
                && !snap.join("SavedVariables/Other.lua").exists()
        );

        let manifest: SvManifest = serde_json::from_slice(
            &std::fs::read(scene.client.snapshots(&scene.tx).join("WTF.manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            manifest.entries.len(),
            12,
            "2 directories x 3 folders x .lua/.lua.bak"
        );
        let absent: Vec<&str> = manifest
            .entries
            .iter()
            .filter(|e| e.sha256.is_none())
            .map(|e| e.path.as_str())
            .collect();
        assert!(
            absent.contains(&"Account/123456#1/SavedVariables/Bar.lua"),
            "{absent:?}"
        );
        assert_eq!(absent.len(), 8);

        let wire = serde_json::to_string(&report).unwrap();
        for private in ["123456", "RealmName", "CharName"] {
            assert!(!wire.contains(private), "{wire}");
        }
    }

    fn manifest(scene: &Scene, tx: &TxId) -> SvManifest {
        let path = scene.client.snapshots(tx).join("WTF.manifest.json");
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// Every file under `dir`, relative to it, sorted.
    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            for entry in std::fs::read_dir(&at).into_iter().flatten() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let rel = path.strip_prefix(dir).unwrap();
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn prestaged_saved_variables_are_not_read_again_under_the_lock() {
        let scene = Scene::new();
        let folders = [name("Foo"), name("Gone"), name("Bar")].map(MutableFolderName::into_folder);
        let mut s = scene.session();
        let hashed = crate::fsx::files_hashed();
        let prestage =
            prestage_saved_variables(&mut s, &scene.tx, &scene.client, &folders).unwrap();
        assert_eq!(prestage.len(), 4);
        assert_eq!(
            crate::fsx::files_hashed() - hashed,
            4,
            "each file hashed once, as its copy"
        );
        let hashed = crate::fsx::files_hashed();
        let report =
            finish_saved_variables(&mut s, &scene.tx, &scene.client, &folders, prestage).unwrap();
        assert_eq!(
            crate::fsx::files_hashed(),
            hashed,
            "nothing hashed under the lock"
        );
        assert_eq!(
            (report.sv_files_snapshotted, report.sv_skipped),
            (4, SvSkipped::default())
        );

        // The same manifest as a snapshot taken under the lock alone.
        let other = TxId::new();
        let mut alone = WriteSession::tx(
            other,
            crate::fsx::Writer::Mutating,
            &scene.scopes,
            crate::fsx::SnapshotRoots::for_tx(&scene.paths, &other),
            scene.paths.state_snapshot(&other).join("actions.jsonl"),
        );
        snapshot_saved_variables(&mut alone, &other, &scene.client, &folders).unwrap();
        assert_eq!(
            manifest(&scene, &scene.tx).entries,
            manifest(&scene, &other).entries
        );
        let wtf = |tx: &TxId| scene.client.snapshots(tx).join("WTF");
        assert_eq!(files_under(&wtf(&scene.tx)), files_under(&wtf(&other)));
        assert!(!scene.client.snapshots(&scene.tx).join("sv-stale").exists());
    }

    #[test]
    fn what_changed_after_the_prestage_is_snapshotted_under_the_lock() {
        let scene = Scene::new();
        let folders = [name("Foo"), name("Gone"), name("Bar")].map(MutableFolderName::into_folder);
        let account = scene.client.wtf().join("Account/123456#1/SavedVariables");
        let character = scene
            .client
            .wtf()
            .join("Account/123456#1/RealmName/CharName/SavedVariables");
        let mut s = scene.session();
        let prestage =
            prestage_saved_variables(&mut s, &scene.tx, &scene.client, &folders).unwrap();
        // A client logged out in between: one file rewritten, one deleted, one new.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(account.join("Foo.lua"), "-- played a session\n").unwrap();
        std::fs::remove_file(character.join("Gone.lua")).unwrap();
        std::fs::write(account.join("Bar.lua"), "-- new\n").unwrap();
        let report =
            finish_saved_variables(&mut s, &scene.tx, &scene.client, &folders, prestage).unwrap();
        assert_eq!(report.sv_files_snapshotted, 4, "Foo x3 and Bar");

        let snapshot = scene.client.snapshots(&scene.tx);
        let entries: BTreeMap<String, Option<String>> = manifest(&scene, &scene.tx)
            .entries
            .into_iter()
            .map(|e| (e.path, e.sha256))
            .collect();
        let entry = |rel: &str| entries[&format!("Account/123456#1/{rel}")].clone();
        for (rel, text) in [
            ("SavedVariables/Foo.lua", "-- played a session\n"),
            ("SavedVariables/Bar.lua", "-- new\n"),
        ] {
            let copy = snapshot.join("WTF/Account/123456#1").join(rel);
            assert_eq!(std::fs::read_to_string(&copy).unwrap(), text, "{rel}");
            assert_eq!(entry(rel), Some(sha256_file(&copy).unwrap()), "{rel}");
        }
        assert_eq!(entry("RealmName/CharName/SavedVariables/Gone.lua"), None);

        // WTF/ holds exactly the manifest's files; the stale copies are set aside.
        let listed: Vec<String> = entries
            .iter()
            .filter(|(_, sha)| sha.is_some())
            .map(|(rel, _)| rel.clone())
            .collect();
        assert_eq!(files_under(&snapshot.join("WTF")), listed);
        let stale = files_under(&snapshot.join("sv-stale"));
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(
            stale.iter().any(|f| f.ends_with("SavedVariables/Foo.lua"))
                && stale.iter().any(|f| f.ends_with("SavedVariables/Gone.lua")),
            "{stale:?}"
        );
    }

    #[test]
    fn copies_of_folders_no_longer_touched_are_set_aside() {
        let scene = Scene::new();
        let planned = [name("Foo"), name("Gone")].map(MutableFolderName::into_folder);
        let mut s = scene.session();
        let prestage =
            prestage_saved_variables(&mut s, &scene.tx, &scene.client, &planned).unwrap();
        let now = [name("Foo")].map(MutableFolderName::into_folder);
        let report =
            finish_saved_variables(&mut s, &scene.tx, &scene.client, &now, prestage).unwrap();
        assert_eq!(report.sv_files_snapshotted, 3);
        let snapshot = scene.client.snapshots(&scene.tx);
        assert!(
            manifest(&scene, &scene.tx)
                .entries
                .iter()
                .all(|e| !e.path.contains("Gone")),
        );
        assert!(
            files_under(&snapshot.join("WTF"))
                .iter()
                .all(|f| !f.contains("Gone"))
        );
        assert_eq!(files_under(&snapshot.join("sv-stale")).len(), 1);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn stale_copies_are_set_aside_without_renameat2_flags() {
        let scene = Scene::new();
        let planned = [name("Foo"), name("Gone")].map(MutableFolderName::into_folder);
        let now = [name("Foo")].map(MutableFolderName::into_folder);
        let mut s = scene.session();
        failpoint::with_spec("fsx.renameat2.einval", || {
            let prestage =
                prestage_saved_variables(&mut s, &scene.tx, &scene.client, &planned).unwrap();
            finish_saved_variables(&mut s, &scene.tx, &scene.client, &now, prestage).unwrap();
        });
        let snapshot = scene.client.snapshots(&scene.tx);
        assert_eq!(files_under(&snapshot.join("sv-stale")).len(), 1);
    }

    /// Each op's old digest, `None` for an add.
    fn old_trees(ops: &[SwapOp]) -> Vec<Option<TreeDigest>> {
        ops.iter()
            .map(|op| match op {
                SwapOp::Replace { old_tree, .. } | SwapOp::Remove { old_tree, .. } => *old_tree,
                SwapOp::Add { .. } => None,
            })
            .collect()
    }

    #[test]
    fn unindexed_folders_are_hashed_before_the_lock_and_trusted_while_unchanged() {
        let scene = Scene::new();
        let addons = scene.client.addons();
        // Older than the racy slack, as a folder installed earlier is.
        std::thread::sleep(std::time::Duration::from_millis(30));
        let pre = predigest(&addons, &ops());
        let mut planned = ops();
        let hashed = crate::fsx::files_hashed();
        with_predigests(&mut planned, &addons, &pre);
        assert_eq!(
            crate::fsx::files_hashed(),
            hashed,
            "only stats under the lock"
        );
        let (foo, gone) = (tree(&addons.join("Foo")), tree(&addons.join("Gone")));
        assert_eq!(old_trees(&planned), [Some(foo), None, Some(gone)]);

        // A folder changed since it was hashed gets no digest: the swap hashes it.
        std::fs::write(addons.join("Gone/new.lua"), "--").unwrap();
        let mut planned = ops();
        with_predigests(&mut planned, &addons, &pre);
        assert_eq!(old_trees(&planned), [Some(foo), None, None]);

        // A digest from the files index is kept as it is.
        let indexed = TreeDigest::from_entries([("x.lua", 1, [7u8; 32])]);
        let mut planned = vec![SwapOp::Remove {
            live: name("Foo"),
            old_tree: Some(indexed),
        }];
        with_predigests(&mut planned, &addons, &pre);
        assert_eq!(old_trees(&planned), [Some(indexed)]);
    }

    /// Moves `from` to `to` and leaves a symlink to it in its place, as a player keeping
    /// `WTF/` (or part of it) in a sync folder does.
    fn link_away(from: &Path, to: &Path) {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(from, to).unwrap();
        std::os::unix::fs::symlink(to, from).unwrap();
    }

    /// The scene's candidates for Foo and Gone: (files, existing files, symlinked out).
    fn candidates(scene: &Scene) -> (usize, usize, usize) {
        let (files, existing, skipped) = candidates_skipped(scene);
        assert_eq!(skipped.two_spellings, 0);
        (files, existing, skipped.symlinked)
    }

    /// [`candidates`] with every reason a file was skipped.
    fn candidates_skipped(scene: &Scene) -> (usize, usize, SvSkipped) {
        let folders = [name("Foo"), name("Gone")].map(MutableFolderName::into_folder);
        let found = sv_candidates(&scene.client, &folders);
        let existing = found.files.iter().filter(|(_, f)| f.is_some()).count();
        (found.files.len(), existing, found.skipped)
    }

    #[test]
    fn saved_variables_count_only_inside_the_client() {
        let sync = tempfile::tempdir().unwrap();
        let account = |scene: &Scene| scene.client.wtf().join("Account/123456#1");

        // In place: 2 directories x 2 folders x .lua/.lua.bak; 4 of them exist.
        let scene = Scene::new();
        assert_eq!(candidates(&scene), (8, 4, 0));

        // WTF/ in a sync folder: nothing is a candidate, the existing files are counted.
        let scene = Scene::new();
        link_away(&scene.client.wtf(), &sync.path().join("a/WTF"));
        assert_eq!(candidates(&scene), (0, 0, 4));

        // One account symlinked out: counted, never skipped silently.
        let scene = Scene::new();
        link_away(&account(&scene), &sync.path().join("b/123456#1"));
        assert_eq!(candidates(&scene), (0, 0, 4));

        // Only a character's SavedVariables symlinked out: the account's still count.
        let scene = Scene::new();
        let character = account(&scene).join("RealmName/CharName/SavedVariables");
        link_away(&character, &sync.path().join("c/SavedVariables"));
        assert_eq!(candidates(&scene), (4, 2, 2));

        // A character reached under a second name inside the client: listed once.
        let scene = Scene::new();
        let realm = account(&scene).join("RealmName");
        std::os::unix::fs::symlink(realm.join("CharName"), realm.join("Alt")).unwrap();
        assert_eq!(candidates(&scene), (8, 4, 0));
    }

    #[test]
    fn saved_variables_folders_match_ignoring_case_unless_spelled_twice() {
        // Lowercase, as packs copied from Windows have them: found, spelled as on disk.
        let scene = Scene::new();
        let dir = scene.client.client_dir().to_path_buf();
        std::fs::rename(dir.join("WTF"), dir.join("wtf")).unwrap();
        std::fs::rename(dir.join("wtf/Account"), dir.join("wtf/account")).unwrap();
        let character = dir.join("wtf/account/123456#1/RealmName/CharName");
        std::fs::rename(
            character.join("SavedVariables"),
            character.join("savedvariables"),
        )
        .unwrap();
        assert_eq!(candidates(&scene), (8, 4, 0));
        let mut s = scene.session();
        let folders = [name("Foo"), name("Gone")].map(MutableFolderName::into_folder);
        snapshot_saved_variables(&mut s, &scene.tx, &scene.client, &folders).unwrap();
        let manifest: SvManifest = serde_json::from_slice(
            &std::fs::read(scene.client.snapshots(&scene.tx).join("WTF.manifest.json")).unwrap(),
        )
        .unwrap();
        let listed: Vec<&str> = manifest.entries.iter().map(|e| e.path.as_str()).collect();
        for path in [
            "account/123456#1/SavedVariables/Foo.lua",
            "account/123456#1/RealmName/CharName/savedvariables/FOO.lua",
        ] {
            assert!(listed.contains(&path), "{listed:?}");
            let copy = scene.client.snapshots(&scene.tx).join("WTF").join(path);
            assert!(copy.is_file(), "{path}");
        }

        // A folder spelled two ways at any level: what is under it is counted, never
        // listed, and other directories still are.
        let skipped = |two_spellings| SvSkipped {
            symlinked: 0,
            two_spellings,
        };
        let scene = Scene::new();
        std::fs::create_dir_all(scene.client.client_dir().join("wtf")).unwrap();
        assert_eq!(candidates_skipped(&scene), (0, 0, skipped(4)));
        let scene = Scene::new();
        std::fs::create_dir_all(scene.client.wtf().join("account")).unwrap();
        assert_eq!(candidates_skipped(&scene), (0, 0, skipped(4)));
        let scene = Scene::new();
        let character = scene
            .client
            .wtf()
            .join("Account/123456#1/RealmName/CharName");
        std::fs::create_dir_all(character.join("savedvariables")).unwrap();
        assert_eq!(
            candidates_skipped(&scene),
            (4, 2, skipped(2)),
            "the character's directory alone"
        );
        let warnings = out_of_scope_warnings("retail", skipped(2), "they were not snapshotted");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        assert!(
            warning.message.contains("spelled two ways"),
            "{}",
            warning.message
        );
        for private in ["123456", "RealmName", "CharName"] {
            assert!(!warning.message.contains(private), "{}", warning.message);
        }
    }

    #[test]
    fn saved_variables_outside_the_client_are_not_snapshotted() {
        let sync = tempfile::tempdir().unwrap();
        let scene = Scene::new();
        link_away(&scene.client.wtf(), &sync.path().join("WTF"));
        let before = tree(sync.path());
        let mut s = scene.session();
        let folders = [name("Foo"), name("Gone")].map(MutableFolderName::into_folder);
        let report = snapshot_saved_variables(&mut s, &scene.tx, &scene.client, &folders).unwrap();
        assert_eq!(
            (report.sv_files_snapshotted, report.sv_skipped.symlinked),
            (0, 4)
        );
        assert!(!scene.client.snapshots(&scene.tx).join("WTF").exists());
        assert_eq!(tree(sync.path()), before);
        let warnings =
            out_of_scope_warnings("retail", report.sv_skipped, "they were not snapshotted");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let warning = &warnings[0];
        assert_eq!(warning.code, WarningCode::SavedVariablesOutOfScope);
        let wire = format!(
            "{}{}",
            serde_json::to_string(&report).unwrap(),
            warning.message
        );
        for private in ["123456", "RealmName", "CharName", "WTF/Account"] {
            assert!(!wire.contains(private), "{wire}");
        }
    }

    #[test]
    fn state_files_are_cloned() {
        let scene = Scene::new();
        let mut s = scene.session();
        std::fs::create_dir_all(scene.paths.targets_dir()).unwrap();
        std::fs::write(scene.paths.target_record(&scene.key), "{}").unwrap();
        std::fs::write(scene.paths.registry_file(), "{}").unwrap();
        snapshot_state(&mut s, &scene.paths, &scene.tx, &scene.key).unwrap();
        let dir = scene.paths.state_snapshots_dir().join(scene.tx.to_string());
        assert!(dir.join(format!("{}.json", scene.key)).is_file());
        assert!(dir.join("registry.json").is_file());
        assert!(
            !dir.join(format!("{}.files.json", scene.key)).exists(),
            "absent files are skipped"
        );
    }
}
