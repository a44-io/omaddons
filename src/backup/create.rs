//! One capture of a client's `WTF/` (M3-D23), published as `<meta>/backups/<id>/`.
//!
//! [`create`] copies without the global lock into `<meta>/stage/<tx>/backup/`, guarded
//! by the capture's in-flight lock, so the orphan sweep knows it for live work and
//! sweeps a crashed one's stage. A capture that changed while it was copied is tried
//! again once ([`BACKUP_CAPTURE_ATTEMPTS`]); a second one that changed too is kept and
//! marked `best_effort`. The stage is then described (`manifest.json`, `addons.json`,
//! `backup.json`) and renamed into place.
//!
//! It has no side effect beyond the backup dir and the inventory projection: no lock, no
//! history, no registration, no retention. Its callers add those: the `backup create`
//! command, and later automatic backups (history in a transaction's commit window) and a
//! whole restore's `before_restore` backup (no retention while the restore runs).

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::inventory::{self, Change, Ring};
use super::tree::{self, CloneError, CloneOpts, Step};
use super::{
    AddonProvenance, AddonsFile, BackupKind, BackupMeta, Baseline, Consistency, Integrity,
    Manifest, Reflinked, SourceNote, Trigger,
};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    self, ClonePolicy, Content, FsxError, InflightLock, LockUse, Op, PruneReason, RenameMode,
    ScopeTable, WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::BACKUP_CAPTURE_ATTEMPTS;
use crate::model::{Actor, BackupId, Target, TxId};
use crate::output::ProgressSink;
use crate::privacy::WtfPathRedactor;
use crate::sources::SourceId;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::load_record;
use crate::state::store::{self, Versioned};

/// Owner-only: a backup holds the player's settings, and its manifest names the account,
/// realm and character.
pub(crate) const DIR_MODE: u32 = 0o700;
pub(crate) const FILE_MODE: u32 = 0o600;

/// What to capture.
#[derive(Debug, Clone)]
pub struct CreateRequest<'a> {
    /// A registered target: its key goes in `backup.json`.
    pub target: &'a Target,
    pub kind: BackupKind,
    pub trigger: Option<Trigger>,
    pub actor: Actor,
    pub reason: Option<String>,
    pub policy: ClonePolicy,
    /// Hash every staged file before publication (`integrity: verified`).
    pub verify: bool,
    /// Whether the client ran when the capture began: recorded, never refused.
    pub client_running: bool,
    /// The command the inventory projection's write rings the doorbell for.
    pub command: &'a str,
}

/// A published backup.
#[derive(Debug, Clone)]
pub struct Created {
    pub meta: BackupMeta,
    /// `<meta>/backups/<id>`.
    pub dir: PathBuf,
    /// The capture's transaction id (its stage and in-flight lock), which the caller's
    /// history line for it takes.
    pub tx: TxId,
    /// `backup_copied`, `backup_unsettled`.
    pub warnings: Vec<Warning>,
}

/// Why [`create`] published nothing.
#[derive(Debug)]
pub enum CreateError {
    /// The policy required clones and the filesystem has none: no byte was copied.
    /// Automatic backups skip on it; a manual one never sees it (it byte-copies).
    NotReflinkable,
    Failed(Error),
}

impl From<Error> for CreateError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

impl CreateError {
    /// The command error.
    pub fn into_error(self) -> Error {
        match self {
            Self::NotReflinkable => CloneError::NotReflinkable
                .error(&WtfPathRedactor::for_paths(std::iter::empty::<&str>())),
            Self::Failed(error) => error,
        }
    }
}

/// Captures `req.target`'s `WTF/` and publishes it. `scopes` must contain the target's
/// client dir. Copy progress goes to `progress` (fixed labels, never a path).
pub fn create(
    paths: &Paths,
    scopes: &ScopeTable,
    req: &CreateRequest<'_>,
    progress: &ProgressSink,
) -> std::result::Result<Created, CreateError> {
    create_with(paths, scopes, req, progress, &mut |attempt, step| {
        // Debug-build seams where tests change the tree mid-capture (DEC-7): after a
        // file's first copy, and once every entry is copied, per attempt.
        let _ = match step {
            Step::Copied(_, 1) => fsx::failpoint::hit(&format!("backup.capture.copied.{attempt}")),
            Step::Copied(..) => Ok(()),
            Step::Done => fsx::failpoint::hit(&format!("backup.capture.done.{attempt}")),
        };
    })
}

/// [`create`], calling `hook` with the attempt (from 1) at each step of each copy.
pub(crate) fn create_with(
    paths: &Paths,
    scopes: &ScopeTable,
    req: &CreateRequest<'_>,
    progress: &ProgressSink,
    hook: &mut dyn FnMut(u32, Step<'_>),
) -> std::result::Result<Created, CreateError> {
    let key = req.target.key.ok_or_else(|| {
        Error::internal(format!(
            "backup of {} before it was registered",
            req.target.id
        ))
    })?;
    let client = ClientPaths::new(&req.target.client_dir);
    let source = locate(&client, &req.target.id)?;
    let tx = TxId::new();
    let inflight = InflightLock::create(paths, &tx)?;
    let result = (|| {
        let staged = capture(
            scopes, req, &client, &source, &tx, &inflight, progress, hook,
        )?;
        Ok(publish(
            paths, scopes, req, &client, key, &source, staged, tx,
        )?)
    })();
    cleanup(paths, scopes, &client, &tx, inflight);
    result
}

/// Where a capture reads `WTF/` from, and whether that is outside the client dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The directory itself, never a symlink to it.
    pub root: PathBuf,
    /// `WTF` is a symlink out of the client dir (a sync folder): recorded, and such a
    /// backup is never restored over it.
    pub outside_client: bool,
}

/// The `WTF/` of `client` (the target `target_id`), through a symlink to where it leads
/// (the copy never follows one); `not_found` when the client has none.
pub fn locate(client: &ClientPaths, target_id: &str) -> Result<Source> {
    let wtf = client.wtf();
    let none = || {
        Error::new(
            ErrorCode::NotFound,
            format!("{target_id} has no WTF/ yet: it has not been launched"),
            Suggestion::text("launch this client once; it creates WTF/"),
        )
        .details(serde_json::json!({ "target": target_id }))
    };
    let meta = std::fs::symlink_metadata(&wtf).map_err(|_| none())?;
    if !meta.file_type().is_symlink() {
        return if meta.is_dir() {
            Ok(Source {
                root: wtf,
                outside_client: false,
            })
        } else {
            Err(none())
        };
    }
    let root = std::fs::canonicalize(&wtf).map_err(|_| none())?;
    if !root.is_dir() {
        return Err(none());
    }
    let client_dir = std::fs::canonicalize(client.client_dir())
        .unwrap_or_else(|_| client.client_dir().to_path_buf());
    Ok(Source {
        outside_client: !root.starts_with(&client_dir),
        root,
    })
}

/// A staged capture: `<stage>/backup/` holding `WTF/`.
struct Staged {
    dir: PathBuf,
    copy: tree::TreeCopy,
    captured_at: Timestamp,
    hashes: Option<Vec<Option<String>>>,
}

/// Copies `source` into `<meta>/stage/<tx>/backup/WTF`, trying again once when the tree
/// changed while it was copied, then hashes it with `req.verify`.
#[allow(clippy::too_many_arguments)]
fn capture(
    scopes: &ScopeTable,
    req: &CreateRequest<'_>,
    client: &ClientPaths,
    source: &Source,
    tx: &TxId,
    inflight: &InflightLock,
    progress: &ProgressSink,
    hook: &mut dyn FnMut(u32, Step<'_>),
) -> std::result::Result<Staged, CreateError> {
    let redactor = WtfPathRedactor::for_client(client);
    let stage = client.stage(tx);
    let dir = stage.join("backup");
    let wtf = dir.join("WTF");
    let opts = CloneOpts::capture(req.policy);
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "backup");
    let none = ProgressSink::none();
    let mut attempt = 0;
    let (copy, captured_at) = loop {
        attempt += 1;
        let last = attempt >= BACKUP_CAPTURE_ATTEMPTS;
        let captured_at = crate::clock::now();
        // A retry's counts would step back: only the first attempt reports progress.
        let shown = if attempt == 1 { progress } else { &none };
        let copied = tree::clone_tree_with(
            &mut s,
            &source.root,
            &wtf,
            &opts,
            None,
            shown,
            &mut |step| {
                hook(attempt, step);
            },
        );
        match copied {
            Ok(copy) if copy.consistency == Consistency::Stable || last => {
                break (copy, captured_at);
            }
            Err(CloneError::NotReflinkable) => return Err(CreateError::NotReflinkable),
            Err(e) if last || !matches!(e, CloneError::Changed { .. }) => {
                return Err(e.error(&redactor).into());
            }
            // Best effort, or changed under the copy, with an attempt left: start over.
            Ok(_) | Err(_) => {
                fsx::prune(
                    scopes,
                    PruneReason::PostCommitCleanup,
                    std::slice::from_ref(&stage),
                    LockUse::Own(inflight),
                )?;
            }
        }
    };
    let hashes = if req.verify {
        let hashed =
            tree::hash_files(&wtf, &copy.manifest, progress).map_err(|e| e.error(&redactor))?;
        Some(hashed)
    } else {
        None
    };
    Ok(Staged {
        dir,
        copy,
        captured_at,
        hashes,
    })
}

/// Describes the staged capture and renames it to `<meta>/backups/<id>` (`0700`), then
/// records it in the inventory projection.
#[allow(clippy::too_many_arguments)]
fn publish(
    paths: &Paths,
    scopes: &ScopeTable,
    req: &CreateRequest<'_>,
    client: &ClientPaths,
    key: crate::model::TargetKey,
    source: &Source,
    staged: Staged,
    tx: TxId,
) -> Result<Created> {
    let Staged {
        dir: staged_dir,
        copy,
        captured_at,
        hashes,
    } = staged;
    let mut manifest = Manifest {
        entries: copy.manifest,
    };
    let verified_at = hashes.map(|hashes| {
        for (entry, sha256) in manifest.entries.iter_mut().zip(hashes) {
            entry.sha256 = sha256;
        }
        crate::clock::now()
    });
    let addons = addons(paths, &key)?;
    let id = BackupId::new();
    let meta = BackupMeta {
        id,
        target: key,
        target_id: req.target.id.clone(),
        family: req.target.family,
        created_at: crate::clock::now(),
        captured_at,
        kind: req.kind,
        trigger: req.trigger,
        actor: req.actor,
        reason: req.reason.clone(),
        client_version: req.target.version.to_string(),
        client_running: req.client_running,
        files: copy.files,
        dirs: copy.dirs,
        symlinks: copy.symlinks,
        skipped: copy.skipped,
        bytes: copy.bytes,
        reflinked: copy.reflinked,
        unsettled: copy.unsettled,
        consistency: copy.consistency,
        integrity: if verified_at.is_some() {
            Integrity::Verified
        } else {
            Integrity::MetadataOnly
        },
        verified_at,
        baseline: verified_at.map(|_| Baseline::Capture),
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
        source: None,
        partial: false,
        wtf_outside_client: source.outside_client,
    };

    let mut s = WriteSession::untracked(Writer::Backup, scopes, "backup");
    write(&mut s, &staged_dir.join("manifest.json"), &manifest)?;
    write(&mut s, &staged_dir.join("addons.json"), &addons)?;
    write(&mut s, &super::meta_file(&staged_dir), &meta)?;
    let backups = client.backups();
    ensure_dir_all(&mut s, &backups)?;
    mutate(&mut s, &backups, Op::Chmod { mode: DIR_MODE })?;
    mutate(&mut s, &staged_dir, Op::Chmod { mode: DIR_MODE })?;
    let dir = super::backup_dir(client, &id);
    rename_new(&mut s, &staged_dir, &dir)?;

    let ring = Ring {
        command: req.command,
        actor: req.actor,
    };
    inventory::record(
        paths,
        client,
        &key,
        Change::Added(Box::new(meta.clone())),
        ring,
    );
    let warnings = warnings(&req.target.id, &copy.changes, &meta, copy.copied_bytes);
    Ok(Created {
        meta,
        dir,
        tx,
        warnings,
    })
}

/// `addons.json`: each managed addon's name, folders, version and source, from the
/// target's record (no SavedVariables is read).
pub(crate) fn addons(paths: &Paths, key: &crate::model::TargetKey) -> Result<AddonsFile> {
    let record = load_record(paths, key)?.value;
    let addons = record
        .addons
        .values()
        .map(|a| AddonProvenance {
            name: a.name.clone(),
            folders: a.folders.clone(),
            version: Some(a.version.clone()),
            source: SourceNote {
                kind: SourceId::of(&a.r#ref).wire().to_owned(),
                r#ref: a.r#ref.to_string(),
                release_id: a.release_id.clone(),
            },
        })
        .collect();
    Ok(AddonsFile { addons })
}

/// Writes one of a backup's metadata files, owner-only.
pub(crate) fn write<T: Versioned>(s: &mut WriteSession<'_>, path: &Path, value: &T) -> Result<()> {
    let json = store::encode(value)?;
    let op = Op::WriteFile {
        content: Content::Bytes(&json),
        mode: Some(FILE_MODE),
    };
    mutate(s, path, op)?;
    Ok(())
}

/// Renames `from` to `to`, which must not exist. Without `renameat2` flags (NTFS through
/// ntfs-3g, some FUSE mounts), a fresh id's absence is checked and a plain rename used.
pub(crate) fn rename_new(s: &mut WriteSession<'_>, from: &Path, to: &Path) -> Result<()> {
    let op = |mode| Op::Rename { to, mode };
    match mutate(s, from, op(RenameMode::NoReplace)) {
        Err(FsxError::Unsupported) if std::fs::symlink_metadata(to).is_err() => {
            mutate(s, from, op(RenameMode::Replace))?;
        }
        other => {
            other?;
        }
    }
    Ok(())
}

/// What a published capture says about itself: `backup_copied` when some of its files
/// were byte-copied, `backup_unsettled` when it is best effort. Counts only: never a path.
fn warnings(
    target_id: &str,
    changes: &tree::InventoryDelta,
    meta: &BackupMeta,
    copied_bytes: u64,
) -> Vec<Warning> {
    let mut warnings = Vec::new();
    if meta.reflinked != Reflinked::All {
        let how = match meta.reflinked {
            Reflinked::None => "every file",
            _ => "some files",
        };
        warnings.push(Warning::new(
            WarningCode::BackupCopied,
            format!(
                "{target_id}: the filesystem could not clone {how} of WTF/, so {copied_bytes} bytes were copied and take their full size on disk"
            ),
        ));
    }
    if meta.consistency == Consistency::BestEffort {
        warnings.push(
            Warning::new(
                WarningCode::BackupUnsettled,
                format!(
                    "{target_id}: WTF/ changed while it was copied, and again on a second try ({} still changing, {} changed, {} added and {} removed after their copy): the backup is best effort",
                    crate::output::count(
                        usize::try_from(meta.unsettled).unwrap_or(usize::MAX),
                        "file"
                    ),
                    changes.changed,
                    changes.added,
                    changes.removed
                ),
            )
            .with_fix(Suggestion::text(
                "make another backup after the game exits",
            )),
        );
    }
    warnings
}

/// Removes the capture's stage and in-flight lock under its own lock (best effort: the
/// orphan sweep takes what is left), then lets the lock go. (`backup import`'s too.)
pub(crate) fn cleanup(
    paths: &Paths,
    scopes: &ScopeTable,
    client: &ClientPaths,
    tx: &TxId,
    inflight: InflightLock,
) {
    let mut victims = vec![client.stage(tx), paths.inflight_lock(tx)];
    victims.retain(|v| v.exists());
    if !victims.is_empty() {
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::error::ErrorBody;
    use crate::model::TargetKey;
    use crate::rules::{ClientRules, ClientVersion, Family, InstallKind, RulesTable, TargetKind};

    const PRIVATE: &[&str] = &["FAKEACCT", "FakeRealm", "FakeChar"];
    const SV: &str = "Account/FAKEACCT/SavedVariables";

    /// A registered retail client with a `WTF/`, its state dirs and scopes.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        paths: Paths,
        scopes: ScopeTable,
        target: Target,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(dir.path()).unwrap();
            let client = root.join("_retail_");
            fs::create_dir_all(client.join("Interface/AddOns")).unwrap();
            let wtf = client.join("WTF");
            fs::create_dir_all(wtf.join(SV)).unwrap();
            fs::create_dir_all(wtf.join("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables"))
                .unwrap();
            fs::write(wtf.join("Config.wtf"), "SET locale enUS\n").unwrap();
            for name in ["A", "B", "C"] {
                fs::write(
                    wtf.join(format!("{SV}/{name}.lua")),
                    format!("{name}DB = {{}}\n"),
                )
                .unwrap();
            }
            fs::write(
                wtf.join("Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/D.lua"),
                "DDB = {}\n",
            )
            .unwrap();
            let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
            let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
            let version: ClientVersion = "12.1.5.70391".parse().unwrap();
            let mut target = Target {
                key: Some(TargetKey::new()),
                id: "retail".into(),
                install_root: root.clone(),
                client_dir: client,
                flavor_dir: Some("_retail_".into()),
                product: Some("wow".into()),
                install_kind: InstallKind::Battlenet,
                family: Family::Mainline,
                kind: TargetKind::Live,
                version,
                interface: 120_105,
                exe_name: Some("WoW.exe".into()),
                label: None,
                rules: ClientRules::default(),
            };
            target.refresh_rules(&RulesTable::builtin());
            Self {
                _dir: dir,
                root,
                paths,
                scopes,
                target,
            }
        }

        fn client(&self) -> ClientPaths {
            ClientPaths::new(&self.target.client_dir)
        }

        fn wtf(&self) -> PathBuf {
            self.target.client_dir.join("WTF")
        }

        fn request(&self) -> CreateRequest<'_> {
            CreateRequest {
                target: &self.target,
                kind: BackupKind::Manual,
                trigger: None,
                actor: Actor::Agent,
                reason: Some("before the raid".into()),
                policy: ClonePolicy::Preferred,
                verify: false,
                client_running: false,
                command: "backup create",
            }
        }

        fn create(
            &self,
            req: &CreateRequest<'_>,
            hook: &mut dyn FnMut(u32, Step<'_>),
        ) -> std::result::Result<Created, CreateError> {
            create_with(&self.paths, &self.scopes, req, &ProgressSink::none(), hook)
        }

        /// The backup dirs published so far.
        fn published(&self) -> Vec<PathBuf> {
            fs::read_dir(self.client().backups())
                .map(|l| l.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default()
        }

        /// Nothing of a capture is left: no stage, no in-flight lock.
        fn assert_no_leftovers(&self) {
            let stage = self.client().meta_dir().join("stage");
            let stages: Vec<_> = fs::read_dir(&stage)
                .map(|l| l.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            assert!(stages.is_empty(), "{stages:?}");
            let locks: Vec<_> = fs::read_dir(self.paths.inflight_dir())
                .map(|l| l.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            assert!(locks.is_empty(), "{locks:?}");
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn assert_private(text: &str) {
        for name in PRIVATE {
            assert!(!text.contains(name), "{name} in {text}");
        }
    }

    #[test]
    fn a_capture_is_described_published_owner_only_and_leaves_nothing_behind() {
        let f = Fixture::new();
        let created = f.create(&f.request(), &mut |_, _| {}).unwrap();
        let meta = &created.meta;
        assert_eq!(f.published(), std::slice::from_ref(&created.dir));
        assert_eq!(created.dir, super::super::backup_dir(&f.client(), &meta.id));
        assert_eq!(
            (meta.files, meta.dirs, meta.symlinks, meta.skipped),
            (5, 6, 0, 0)
        );
        assert_eq!(meta.consistency, Consistency::Stable);
        assert_eq!(meta.integrity, Integrity::MetadataOnly);
        assert_eq!((meta.verified_at, meta.baseline), (None, None));
        assert_eq!(
            (meta.kind, meta.actor, meta.reason.as_deref()),
            (BackupKind::Manual, Actor::Agent, Some("before the raid"))
        );
        assert_eq!(meta.client_version, "12.1.5.70391");
        assert!(meta.captured_at <= meta.created_at);
        assert!(!meta.wtf_outside_client);
        for dir in [f.client().backups(), created.dir.clone()] {
            assert_eq!(mode(&dir), 0o700, "{}", dir.display());
        }
        for file in ["backup.json", "manifest.json", "addons.json"] {
            assert_eq!(mode(&created.dir.join(file)), 0o600, "{file}");
        }
        let manifest: Manifest = store::load(&created.dir.join("manifest.json"))
            .unwrap()
            .unwrap()
            .value;
        let fresh = tree::walk(&f.wtf(), usize::MAX, usize::MAX)
            .unwrap()
            .entries;
        assert_eq!(manifest.entries, fresh, "a stat walk of the source");
        let addons: AddonsFile = store::load(&created.dir.join("addons.json"))
            .unwrap()
            .unwrap()
            .value;
        assert!(addons.addons.is_empty(), "the record manages nothing");
        assert_eq!(
            fs::read(created.dir.join(format!("WTF/{SV}/A.lua"))).unwrap(),
            b"ADB = {}\n"
        );
        f.assert_no_leftovers();

        // The projection counts it, and its write rang the doorbell.
        let key = f.target.key.unwrap();
        assert!(matches!(
            inventory::load_validated(&f.paths, &f.client(), &key),
            inventory::Inventory::Fresh(i) if i.count() == 1
        ));
        let bell: serde_json::Value =
            serde_json::from_slice(&fs::read(f.paths.doorbell_file()).unwrap()).unwrap();
        assert_eq!(bell["command"], "backup create");
        assert_eq!(bell["actor"], "agent");
        // Another: two backups, never the same id.
        let again = f.create(&f.request(), &mut |_, _| {}).unwrap();
        assert_ne!(again.meta.id, created.meta.id);
        assert_eq!(f.published().len(), 2);
    }

    #[test]
    fn a_change_on_the_first_attempt_only_gives_a_stable_capture_from_the_second() {
        let f = Fixture::new();
        let a = f.wtf().join(format!("{SV}/A.lua"));
        let mut attempts = Vec::new();
        let created = f
            .create(&f.request(), &mut |attempt, step| {
                if step == Step::Done {
                    attempts.push(attempt);
                    if attempt == 1 {
                        fs::write(&a, "ADB = { changed = true }\n").unwrap();
                    }
                }
            })
            .unwrap();
        assert_eq!(attempts, [1, 2]);
        assert_eq!(created.meta.consistency, Consistency::Stable);
        assert!(
            created
                .warnings
                .iter()
                .all(|w| w.code != WarningCode::BackupUnsettled)
        );
        assert_eq!(
            fs::read_to_string(created.dir.join(format!("WTF/{SV}/A.lua"))).unwrap(),
            "ADB = { changed = true }\n"
        );
        assert_eq!(f.published().len(), 1);
        f.assert_no_leftovers();
    }

    #[test]
    fn a_change_on_both_attempts_is_kept_as_best_effort_and_says_so() {
        let f = Fixture::new();
        let a = f.wtf().join(format!("{SV}/A.lua"));
        let created = f
            .create(&f.request(), &mut |attempt, step| {
                if step == Step::Done {
                    fs::write(&a, "x".repeat(10 + attempt as usize)).unwrap();
                }
            })
            .unwrap();
        assert_eq!(created.meta.consistency, Consistency::BestEffort);
        assert!(!super::super::qualifying(&created.meta));
        let unsettled: Vec<&Warning> = created
            .warnings
            .iter()
            .filter(|w| w.code == WarningCode::BackupUnsettled)
            .collect();
        assert_eq!(unsettled.len(), 1, "{:?}", created.warnings);
        assert!(
            unsettled[0].message.contains("1 changed"),
            "{}",
            unsettled[0].message
        );
        assert_private(&unsettled[0].message);
        assert_eq!(f.published().len(), 1);
        f.assert_no_leftovers();
    }

    #[test]
    fn a_tree_that_loses_files_on_both_attempts_fails_redacted_and_publishes_nothing() {
        let f = Fixture::new();
        let wtf = f.wtf();
        let created = f.create(&f.request(), &mut |attempt, step| {
            // The first file copied is Account/FAKEACCT/FakeRealm/…/D.lua; B.lua comes
            // later, and goes missing under each attempt.
            if let Step::Copied(rel, 1) = step
                && rel.ends_with("D.lua")
            {
                match attempt {
                    1 => {
                        fs::remove_file(wtf.join(format!("{SV}/B.lua"))).unwrap();
                        fs::rename(
                            wtf.join(format!("{SV}/C.lua")),
                            wtf.join(format!("{SV}/E.lua")),
                        )
                        .unwrap();
                    }
                    _ => fs::remove_file(wtf.join(format!("{SV}/E.lua"))).unwrap(),
                }
            }
        });
        assert!(
            matches!(created, Err(CreateError::Failed(_))),
            "{created:?}"
        );
        let Err(CreateError::Failed(error)) = created else {
            return;
        };
        let body = ErrorBody::from(error);
        assert_eq!(body.code, ErrorCode::Io);
        assert!(body.retryable);
        let shown = format!("{} {}", body.message, body.details);
        assert!(shown.contains("E.lua"), "{shown}");
        assert_private(&shown);
        assert!(f.published().is_empty());
        f.assert_no_leftovers();
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_clone_only_capture_where_nothing_clones_publishes_nothing() {
        let f = Fixture::new();
        let req = CreateRequest {
            policy: ClonePolicy::Required,
            ..f.request()
        };
        let created = fsx::with_spec("fsx.reflink.unsupported", || f.create(&req, &mut |_, _| {}));
        assert!(
            matches!(created, Err(CreateError::NotReflinkable)),
            "{created:?}"
        );
        assert!(f.published().is_empty());
        f.assert_no_leftovers();

        // A manual backup byte-copies instead, and says so.
        let created = fsx::with_spec("fsx.reflink.unsupported", || {
            f.create(&f.request(), &mut |_, _| {})
        })
        .unwrap();
        assert_eq!(created.meta.reflinked, Reflinked::None);
        let copied: Vec<&Warning> = created
            .warnings
            .iter()
            .filter(|w| w.code == WarningCode::BackupCopied)
            .collect();
        assert_eq!(copied.len(), 1);
        let bytes = created.meta.bytes.to_string();
        assert!(copied[0].message.contains(&bytes), "{}", copied[0].message);
    }

    #[test]
    fn verify_records_each_files_hash_from_the_capture() {
        use sha2::{Digest, Sha256};
        let f = Fixture::new();
        let req = CreateRequest {
            verify: true,
            ..f.request()
        };
        let created = f.create(&req, &mut |_, _| {}).unwrap();
        let meta = &created.meta;
        assert_eq!(meta.integrity, Integrity::Verified);
        assert_eq!(meta.baseline, Some(Baseline::Capture));
        assert!(meta.verified_at.is_some());
        let manifest: Manifest = store::load(&created.dir.join("manifest.json"))
            .unwrap()
            .unwrap()
            .value;
        for entry in &manifest.entries {
            match entry.kind {
                super::super::EntryKind::File => {
                    let bytes = fs::read(f.wtf().join(&entry.path)).unwrap();
                    let hex: String = Sha256::digest(&bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    assert_eq!(
                        entry.sha256.as_deref(),
                        Some(hex.as_str()),
                        "{}",
                        entry.path
                    );
                }
                _ => assert_eq!(entry.sha256, None, "{}", entry.path),
            }
        }
    }

    #[test]
    fn a_wtf_symlinked_out_of_the_client_is_copied_from_where_it_leads() {
        let f = Fixture::new();
        let elsewhere = f.root.join("sync/WTF");
        fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        fs::rename(f.wtf(), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, f.wtf()).unwrap();
        let created = f.create(&f.request(), &mut |_, _| {}).unwrap();
        assert!(created.meta.wtf_outside_client);
        assert_eq!(created.meta.files, 5);
        assert!(created.dir.join(format!("WTF/{SV}/A.lua")).is_file());
        let (listed, _) = super::super::list(&f.client());
        assert!(!listed[0].restorable, "never restored over a link");

        // A link inside the client dir is no sync folder.
        fs::remove_file(f.wtf()).unwrap();
        let inside = f.target.client_dir.join("WTF.real");
        fs::rename(&elsewhere, &inside).unwrap();
        std::os::unix::fs::symlink("WTF.real", f.wtf()).unwrap();
        let created = f.create(&f.request(), &mut |_, _| {}).unwrap();
        assert!(!created.meta.wtf_outside_client);
    }

    #[test]
    fn a_client_without_wtf_is_not_found_and_nothing_is_staged() {
        let f = Fixture::new();
        fs::remove_dir_all(f.wtf()).unwrap();
        let created = f.create(&f.request(), &mut |_, _| {});
        assert!(
            matches!(created, Err(CreateError::Failed(_))),
            "{created:?}"
        );
        let Err(CreateError::Failed(error)) = created else {
            return;
        };
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.fix.argv(), None, "a prose fix");
        f.assert_no_leftovers();
        assert!(f.published().is_empty());
    }
}
