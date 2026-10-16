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
//! Before the swap, [`snapshot_saved_variables`] clones the touched folders'
//! SavedVariables (reflinks where the filesystem can) and [`snapshot_state`] clones the
//! target's state files. The SavedVariables manifest holds account, realm and
//! character names; it stays local and only counts are reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::journal::{Journal, Step, StepOp, StepPurpose};
use super::stage::Staged;
use crate::error::{Error, PlanStaleSource, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    CloneKind, FsxError, MutateOpts, Op, RenameMode, TreeDigest, WriteSession, ensure_dir_all,
    failpoint, mutate, mutate_with,
};
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

/// Clones the SavedVariables of `folders` (account and per-character `<Folder>.lua` and
/// `.lua.bak`, any case; never Blizzard's own `SavedVariables.lua`) into
/// `snapshots/<tx>/WTF/`, and writes the manifest.
pub fn snapshot_saved_variables(
    s: &mut WriteSession<'_>,
    tx: &TxId,
    client: &ClientPaths,
    folders: &[FolderName],
) -> Result<SvReport> {
    failpoint::hit("engine.snapshot.sv").map_err(|e| fail(&client.wtf(), &e))?;
    let wtf = client.wtf();
    let snapshot = client.snapshots(tx);
    let mut entries = Vec::new();
    let mut report = SvReport {
        sv_files_snapshotted: 0,
        reflink: true,
    };
    // Errors never name a file: the paths hold account, realm and character names.
    let private = |e: Error| {
        Error::new(
            e.code,
            "a SavedVariables file could not be snapshotted",
            Suggestion::command(["omaddons", "status"]),
        )
    };
    for (rel, present) in sv_candidates(&wtf, folders) {
        let sha256 = match &present {
            Some(path) => {
                let dest = snapshot.join("WTF").join(&rel);
                if let Some(parent) = dest.parent() {
                    ensure_dir_all(s, parent).map_err(|e| private(e.into()))?;
                }
                let record = mutate(s, &dest, Op::CloneFile { from: path })
                    .map_err(|e| private(e.into()))?;
                report.sv_files_snapshotted += 1;
                report.reflink &= matches!(record.clone, Some(CloneKind::Reflink));
                Some(sha256_file(path).map_err(private)?)
            }
            None => None,
        };
        entries.push(SvEntry { path: rel, sha256 });
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

/// Every SavedVariables candidate of `folders` under `<wtf>/Account/`: (path relative
/// to `wtf` with `/`, the existing file if any). Existing files keep their spelling;
/// absent ones are spelled `<Folder>.lua`.
pub fn sv_candidates(wtf: &Path, folders: &[FolderName]) -> Vec<(String, Option<PathBuf>)> {
    let mut out = Vec::new();
    for account in dirs(&wtf.join("Account")) {
        let mut sv_dirs = vec![(
            format!("Account/{account}"),
            wtf.join("Account").join(&account),
        )];
        for realm in dirs(&wtf.join("Account").join(&account)) {
            if realm.eq_ignore_ascii_case("SavedVariables") {
                continue;
            }
            for character in dirs(&wtf.join("Account").join(&account).join(&realm)) {
                sv_dirs.push((
                    format!("Account/{account}/{realm}/{character}"),
                    wtf.join("Account")
                        .join(&account)
                        .join(&realm)
                        .join(&character),
                ));
            }
        }
        for (rel, dir) in sv_dirs {
            let sv = dir.join("SavedVariables");
            let present: BTreeMap<String, String> = files(&sv)
                .into_iter()
                .map(|name| (name.to_ascii_lowercase(), name))
                .collect();
            for folder in folders {
                for suffix in [".lua", ".lua.bak"] {
                    let want = format!("{}{suffix}", folder.as_str());
                    if want.to_ascii_lowercase().starts_with("savedvariables.lua") {
                        continue;
                    }
                    match present.get(&want.to_ascii_lowercase()) {
                        Some(actual) => out.push((
                            format!("{rel}/SavedVariables/{actual}"),
                            Some(sv.join(actual)),
                        )),
                        None => out.push((format!("{rel}/SavedVariables/{want}"), None)),
                    }
                }
            }
        }
    }
    out
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
            mutate(s, &dir.join(name), Op::CloneFile { from: &source })?;
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
                self.warnings.push(
                    Warning::new(
                        WarningCode::AtomicSwapUnavailable,
                        "this filesystem cannot swap folders atomically, so omaddons used two renames per folder under its journal",
                    )
                    .with_fix(Suggestion::text(
                        "the AddOns filesystem does not support atomic renames; omaddons stays crash-safe through its journal",
                    )),
                );
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

/// The lowercase hex SHA-256 of the file at `path`, streamed (SavedVariables reach
/// 64 MiB, and undo staging hashes every restored file).
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    crate::state::files_index::hash_file_streaming(path)
        .map(|(_, sha256)| sha256)
        .map_err(|e| fail(path, &e))
}

/// Directory names in `dir`, sorted; nothing when it cannot be read.
fn dirs(dir: &Path) -> Vec<String> {
    entries(dir, true)
}

/// Regular file names in `dir`, sorted.
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
            e.file_type()
                .is_ok_and(|t| if want_dirs { t.is_dir() } else { t.is_file() })
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::journal::{Phase, StepRecord, parse, recover_all};
    use crate::engine::testing::{Scene, folder};
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
        let warnings = recover_all(&lock, &scene.paths, &scene.scopes).unwrap();
        assert_eq!(
            warnings.len(),
            0,
            "the in-flight lock is still held by the scene"
        );
        scene.release_inflight();
        let warnings = recover_all(&lock, &scene.paths, &scene.scopes).unwrap();
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
                reflink: false
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
