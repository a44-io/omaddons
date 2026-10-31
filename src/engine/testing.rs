//! Shared builders for the engine's unit tests (failpoint tests must be unit tests:
//! `failpoint::with_spec` is per-thread and test-only).

// The single-writer scan skips files marked test-only.
#![cfg(test)]

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::inspect::{ArchiveLimits, inspect_dir};
use super::journal::{self, CommitPayload, IndexChanges, Journal, Prepared};
use super::stage::{Staged, stage};
use super::swap::{SvPrestage, SvReport, finish_saved_variables};
use crate::fsx::{
    GlobalLock, GlobalLockGuard, InflightLock, ScopeTable, SnapshotRoots, WriteSession, Writer,
};
use crate::model::{Actor, FolderName, Target, TargetKey, TxId, TxKind};
use crate::rules::{ClientRules, ClientVersion, Family, InstallKind, RulesTable, TargetKind};
use crate::state::history::{self, HistoryEntry, TxResult};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::TargetRecord;

/// The SavedVariables snapshot as a commit window takes it with nothing copied before
/// the lock: every file cloned and hashed there.
pub fn snapshot_saved_variables(
    s: &mut WriteSession<'_>,
    tx: &TxId,
    client: &ClientPaths,
    folders: &[FolderName],
) -> crate::error::Result<SvReport> {
    let snapshot = client.snapshots(tx);
    finish_saved_variables(s, tx, &snapshot, client, folders, SvPrestage::default())
}

/// A retail target whose client dir is `client_dir`.
pub fn target(client_dir: &Path) -> Target {
    let mut target = Target {
        key: None,
        id: "retail".into(),
        install_root: client_dir.parent().unwrap_or(client_dir).to_path_buf(),
        client_dir: client_dir.to_path_buf(),
        flavor_dir: Some("_retail_".into()),
        product: Some("wow".into()),
        install_kind: InstallKind::Battlenet,
        family: Family::Mainline,
        kind: TargetKind::Live,
        version: "12.1.5.70391".parse::<ClientVersion>().unwrap(),
        interface: 120_105,
        exe_name: None,
        label: None,
        rules: ClientRules::default(),
        judge: String::new(),
    };
    target.refresh_rules(&RulesTable::builtin());
    target
}

/// Marks `client_dir` as `key`'s, as registration does: recovery writes a journal's
/// client only when its marker names the journal's target.
pub fn mark(client_dir: &Path, key: TargetKey) {
    let marker = crate::discover::registry::marker_path(client_dir);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(marker, format!("{{\"key\":\"{key}\"}}")).unwrap();
}

/// Writes `<dir>/<name>.toc` (and makes `dir`) with `text` in it.
pub fn folder(dir: &Path, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let name = dir.file_name().unwrap().to_str().unwrap();
    std::fs::write(
        dir.join(format!("{name}.toc")),
        format!("## Interface: 120105\n## Notes: {text}\n"),
    )
    .unwrap();
}

/// A client with AddOns `Foo` (old) and `Gone`, a WTF tree with SavedVariables, and
/// a transaction that staged new `Foo` and `Bar`.
pub struct Scene {
    /// Keeps the tempdir alive for the scene's lifetime.
    _root: tempfile::TempDir,
    pub paths: Paths,
    pub scopes: ScopeTable,
    pub client: ClientPaths,
    pub key: TargetKey,
    pub tx: TxId,
    /// Held until [`Scene::release_inflight`].
    pub inflight: Option<InflightLock>,
    pub staged: Staged,
}

impl Scene {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let client_dir = base.join("_retail_");
        let client = ClientPaths::new(&client_dir);
        folder(&client.addons().join("Foo"), "old foo");
        folder(&client.addons().join("Gone"), "gone");
        let account = client.wtf().join("Account/123456#1");
        let character = account.join("RealmName/CharName");
        for (dir, files) in [
            (
                account.join("SavedVariables"),
                &["Foo.lua", "foo.lua.bak", "SavedVariables.lua", "Other.lua"][..],
            ),
            (
                character.join("SavedVariables"),
                &["FOO.lua", "Gone.lua"][..],
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            for file in files {
                // ubs:ignore — a fixture file's content, not a secret
                std::fs::write(dir.join(file), format!("-- {file}\n")).unwrap();
            }
        }
        std::fs::write(account.join("bindings-cache.wtf"), "x").unwrap();
        let source = base.join("src/Repo-main");
        folder(&source.join("Foo"), "new foo");
        folder(&source.join("Bar"), "new bar");

        let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client_dir), None).unwrap();
        let tx = TxId::new();
        let inflight = InflightLock::create(&paths, &tx).unwrap();
        let inspected =
            inspect_dir(&source, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap();
        let staged = stage(&inflight, &scopes, &target(&client_dir), None, &inspected).unwrap();
        Self {
            _root: root,
            paths,
            scopes,
            client,
            key: TargetKey::new(),
            tx,
            inflight: Some(inflight),
            staged,
        }
    }

    /// The transaction's fsx session, logging to its actions log.
    pub fn session(&self) -> WriteSession<'_> {
        WriteSession::tx(
            self.tx,
            Writer::Mutating,
            &self.scopes,
            SnapshotRoots::for_tx(&self.paths, &self.tx),
            self.actions_log(),
        )
    }

    /// The transaction's client snapshot (folder ops: the bare name).
    pub fn snapshot(&self) -> PathBuf {
        self.client.snapshots(&self.tx)
    }

    pub fn actions_log(&self) -> PathBuf {
        self.paths
            .state_snapshots_dir()
            .join(self.tx.to_string())
            .join("actions.jsonl")
    }

    pub fn lock(&self) -> GlobalLockGuard {
        GlobalLock::acquire(&self.paths, "test").unwrap()
    }

    /// Drops the in-flight lock (the writer "dies") and waits until no process holds
    /// it: a child that another test forks shares the lock's open file description
    /// until it execs.
    pub fn release_inflight(&mut self) {
        drop(self.inflight.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while InflightLock::is_live(&self.paths, &self.tx).unwrap_or(false)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Creates the journal with a minimal payload.
    pub fn journal(&self, s: &mut WriteSession<'_>, lock: &GlobalLockGuard) -> Journal {
        mark(self.client.client_dir(), self.key);
        let prepared = Prepared {
            schema_version: journal::CURRENT,
            tx: self.tx,
            target_key: self.key,
            client_dir: self.client.client_dir().to_path_buf(),
            kind: TxKind::Install,
            started_at: Timestamp::now(),
            creates_dirs: Vec::new(),
            commit: CommitPayload {
                record: TargetRecord {
                    key: self.key,
                    generation: 1,
                    addons: Default::default(),
                },
                index: IndexChanges::default(),
                history: HistoryEntry {
                    schema_version: history::CURRENT,
                    tx: self.tx,
                    kind: TxKind::Install,
                    at: Timestamp::now(),
                    target: Some(self.key),
                    targets: Vec::new(),
                    actor: Actor::Agent,
                    reason: None,
                    argv: vec!["omaddons".into(), "install".into()],
                    ops: Vec::new(),
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
                },
                stage_dir: self.staged.stage_dir.clone(),
                sv_restore: None,
            },
            writes_wtf: false,
            staged_stamp: None,
            staged_files: None,
            source_backup: None,
            client: None,
        };
        Journal::create(s, &self.paths, lock, prepared).unwrap()
    }

    /// `<Folder>.toc`'s notes in AddOns, if the folder is there.
    pub fn notes(&self, folder: &str) -> Option<String> {
        let toc = std::fs::read_to_string(
            self.client
                .addons()
                .join(folder)
                .join(format!("{folder}.toc")),
        )
        .ok()?;
        toc.lines()
            .find_map(|l| l.strip_prefix("## Notes: "))
            .map(str::to_owned)
    }
}

/// A user's install of `r` (from `file`, if given); a `version` pins it, as
/// `install --version` does.
pub fn install_change(
    r: &str,
    file: Option<&Path>,
    version: Option<&str>,
) -> super::plan::RequestedChange {
    super::plan::RequestedChange::Install {
        r#ref: r.parse().unwrap(),
        channel: crate::model::Channel::Stable,
        channel_explicit: false,
        version: version.map(str::to_owned),
        file: file.map(Path::to_path_buf),
        name: None,
        pin: if version.is_some() {
            super::plan::PinIntent::Pin
        } else {
            super::plan::PinIntent::Keep
        },
        reason: crate::model::AddonReason::Explicit,
        reinstall: false,
        blob_sha256: None,
        asset: None,
        revision: None,
        any_flavor: super::plan::AnyFlavorIntent::Keep,
    }
}

/// The files index entry of the live folder `dir`: every regular file's path, size,
/// mtime and SHA-256.
pub fn live_index(dir: &Path) -> crate::state::files_index::FolderIndex {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<crate::state::files_index::FileEntry>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                walk(&entry.path(), &path, out);
            } else {
                let bytes = std::fs::read(entry.path()).unwrap();
                out.push(crate::state::files_index::FileEntry {
                    path,
                    size: meta.size(),
                    mtime_ns: i128::from(meta.mtime()) * 1_000_000_000
                        + i128::from(meta.mtime_nsec()),
                    sha256: Sha256::digest(&bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                });
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    crate::state::files_index::FolderIndex::new(name, files)
}

/// Fails every request: `--file` installs never touch the network.
pub struct NoNetwork;

impl crate::net::Http for NoNetwork {
    fn send(&self, req: crate::net::Request) -> crate::error::Result<crate::net::Response> {
        Err(crate::error::Error::internal(format!(
            "unexpected request to {}",
            req.url
        )))
    }
}

/// A retail client for whole transactions: zips on disk installed with `--file`.
pub struct TxScene {
    _root: tempfile::TempDir,
    pub base: PathBuf,
    pub paths: Paths,
    pub client: ClientPaths,
    pub discovered: crate::discover::target::DiscoveredTarget,
    pub running: std::collections::BTreeSet<PathBuf>,
    /// Who runs the next transaction.
    pub actor: Actor,
    /// `--force` on installs.
    pub force: bool,
    /// `--while-running`.
    pub while_running: bool,
    /// Gives the registry a git runner that may read `file://` repositories.
    pub with_git: bool,
    /// The effective `[limits]` retention.
    pub retention: super::retention::RetentionLimits,
    /// What a git checkout may hold (`limits.download_max_bytes`).
    pub git_download_max_bytes: u64,
    /// The stored plan the next transaction is applied from (`apply`).
    pub applying: Option<crate::model::PlanId>,
    /// `auto_backup` for the next transaction; `None` makes no automatic backup.
    pub auto_backup: Option<crate::state::config::AutoBackup>,
    /// The limits automatic backups keep: the defaults, with no free-space floor.
    pub backup_limits: crate::limits::EffectiveLimits,
}

impl TxScene {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(root.path()).unwrap();
        let client_dir = base.join("_retail_");
        std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
        let mut found = target(&client_dir);
        found.id = "retail".into();
        let discovered = crate::discover::target::DiscoveredTarget {
            target: found,
            default_id: "retail".into(),
            qualifier: "battlenet".into(),
            source: crate::discover::DiscoverySource::Scan,
        };
        Self {
            paths: Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k")),
            client: ClientPaths::new(&client_dir),
            base,
            _root: root,
            discovered,
            running: std::collections::BTreeSet::new(),
            actor: Actor::Agent,
            force: false,
            while_running: false,
            with_git: false,
            retention: super::retention::RetentionLimits::default(),
            git_download_max_bytes: crate::limits::DOWNLOAD_MAX_BYTES,
            applying: None,
            auto_backup: None,
            backup_limits: crate::limits::EffectiveLimits {
                backup_min_free: 0,
                ..crate::limits::EffectiveLimits::default()
            },
        }
    }

    /// The target as currently registered (its marker names the key).
    pub fn current_target(&self) -> crate::discover::target::DiscoveredTarget {
        let mut discovered = self.discovered.clone();
        discovered.target.key = crate::discover::registry::read_marker(self.client.client_dir());
        discovered
    }

    /// Calls `f` with an engine context for `command` (no network, the scene's running
    /// set and actor).
    pub fn with_ctx<R>(&self, command: &str, f: impl FnOnce(&super::tx::EngineCtx<'_>) -> R) -> R {
        self.with_bound_ctx(command, None, f)
    }

    /// Runs `changes` as `run_as` does, as a dry run or not, collecting witnesses under
    /// `witness`, with `binding` asked by the engine.
    pub fn run_bound(
        &self,
        changes: Vec<super::plan::RequestedChange>,
        kind: Option<TxKind>,
        default_kind: TxKind,
        dry_run: bool,
        witness: Option<super::witness::WitnessScope>,
        binding: Option<&dyn super::tx::Binding>,
    ) -> crate::error::Result<super::tx::TxOutcomes> {
        let mut req = self.request(changes, kind, default_kind);
        req.dry_run = dry_run;
        req.witness = witness;
        self.with_bound_ctx("install", binding, |ctx| super::tx::run(&req, ctx))
    }

    /// [`TxScene::with_ctx`] with `binding` in the context.
    pub fn with_bound_ctx<R>(
        &self,
        command: &str,
        binding: Option<&dyn super::tx::Binding>,
        f: impl FnOnce(&super::tx::EngineCtx<'_>) -> R,
    ) -> R {
        let mut registry = crate::sources::SourceRegistry::new();
        if self.with_git {
            registry = registry.with_git(
                crate::sources::git_hosts::GitRunner::for_tests()
                    .with_download_max_bytes(self.git_download_max_bytes)
                    .with_home(&self.paths),
            );
        }
        let table = RulesTable::builtin();
        let argv: Vec<String> = ["omaddons", command, "test"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let ctx = super::tx::EngineCtx {
            paths: &self.paths,
            http: &NoNetwork,
            registry: &registry,
            table: &table,
            policy: crate::state::config::RunningPolicy::Safe,
            limits: ArchiveLimits::default(),
            retention: self.retention,
            actor: self.actor,
            argv: &argv,
            command,
            running: Some(&self.running),
            known_targets: std::slice::from_ref(&self.discovered.target),
            cache: None,
            progress: &crate::output::ProgressSink::none(),
            catalog: None,
            default_channel: crate::model::Channel::Stable,
            downloads: None,
            indexes: None,
            notes: None,
            binding,
            auto_backup: self
                .auto_backup
                .map(|mode| crate::backup::auto::AutoPolicy {
                    mode,
                    limits: &self.backup_limits,
                }),
            applying: self.applying,
        };
        f(&ctx)
    }

    /// Removes the installed addon `addon`.
    pub fn remove(&self, addon: &str) -> super::plan::RequestedChange {
        super::plan::RequestedChange::Remove {
            addon: addon.to_owned(),
        }
    }

    /// Undoes the selected transaction.
    pub fn undo(
        &self,
        selector: super::undo::UndoSelector,
        sv_policy: super::undo::SvPolicy,
    ) -> crate::error::Result<super::undo::UndoOutcome> {
        self.undo_with(selector, sv_policy, false)
    }

    /// [`TxScene::undo`], optionally as a dry run.
    pub fn undo_with(
        &self,
        selector: super::undo::UndoSelector,
        sv_policy: super::undo::SvPolicy,
        dry_run: bool,
    ) -> crate::error::Result<super::undo::UndoOutcome> {
        let req = super::undo::UndoRequest {
            selector,
            sv_policy,
            dry_run,
            while_running: self.while_running,
            reason: None,
            targets: vec![self.current_target()],
            run: crate::model::RunId::new(),
        };
        self.with_ctx("undo", |ctx| super::undo::undo(&req, ctx))
    }

    /// Writes a zip of `(path, text)` entries.
    pub fn zip(&self, name: &str, entries: &[(&str, &str)]) -> PathBuf {
        use std::io::Write;
        let path = self.base.join("zips").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (entry, text) in entries {
            zip.start_file(*entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    /// An `--file` install of `zip` as `ref`.
    pub fn install(&self, r: &str, zip: &Path) -> super::plan::RequestedChange {
        install_change(r, Some(zip), None)
    }

    /// An `--file` install of `zip` as `ref` at `version`, which pins it.
    pub fn install_pinned(
        &self,
        r: &str,
        zip: &Path,
        version: &str,
    ) -> super::plan::RequestedChange {
        install_change(r, Some(zip), Some(version))
    }

    /// Runs `changes` on the target as currently registered, as an install.
    pub fn run(
        &self,
        changes: Vec<super::plan::RequestedChange>,
    ) -> crate::error::Result<super::tx::TxOutcomes> {
        self.run_as(changes, None, TxKind::Install)
    }

    /// Runs `changes` with the kind override `kind` and the command's `default_kind`.
    pub fn run_as(
        &self,
        changes: Vec<super::plan::RequestedChange>,
        kind: Option<TxKind>,
        default_kind: TxKind,
    ) -> crate::error::Result<super::tx::TxOutcomes> {
        let req = self.request(changes, kind, default_kind);
        self.with_ctx("install", |ctx| super::tx::run(&req, ctx))
    }

    /// A dry run of `changes`, as an install, that collects an approval witness per
    /// target under `scope` (`plan.witnesses`).
    pub fn run_witnessed(
        &self,
        changes: Vec<super::plan::RequestedChange>,
        scope: super::witness::WitnessScope,
    ) -> crate::error::Result<super::tx::TxOutcomes> {
        let mut req = self.request(changes, None, TxKind::Install);
        req.dry_run = true;
        req.witness = Some(scope);
        self.with_ctx("install", |ctx| super::tx::run(&req, ctx))
    }

    /// The request [`TxScene::run_as`] runs, for a test to adjust (a dry run, a second
    /// target, a witness scope).
    pub fn request(
        &self,
        changes: Vec<super::plan::RequestedChange>,
        kind: Option<TxKind>,
        default_kind: TxKind,
    ) -> super::tx::TxRequest {
        super::tx::TxRequest {
            targets: vec![super::tx::TxTarget {
                discovered: self.current_target(),
                changes,
                default_kind,
                dependency_locks: std::collections::BTreeMap::new(),
            }],
            force: self.force,
            while_running: self.while_running,
            dry_run: false,
            reason: Some("testing".into()),
            kind,
            run: crate::model::RunId::new(),
            no_deps: false,
            witness: None,
        }
    }

    /// A records-only edit of the installed addon `addon`.
    pub fn edit(&self, addon: &str, edit: super::edit::RecordEdit) -> super::plan::RequestedChange {
        super::plan::RequestedChange::Edit {
            addon: addon.to_owned(),
            edit,
        }
    }

    /// The target's record now.
    pub fn record(&self) -> TargetRecord {
        let key = crate::discover::registry::read_marker(self.client.client_dir()).unwrap();
        crate::state::records::load_record(&self.paths, &key)
            .unwrap()
            .value
    }

    /// An adoption of the unmanaged folder `folder` as the addon of the same name, its
    /// files indexed as they are now.
    pub fn adopt(&self, folder: &str) -> super::plan::RequestedChange {
        let name = crate::model::FolderName::new(folder).unwrap();
        let index = live_index(&self.client.addons().join(folder));
        let record = crate::model::AddonRecord {
            r#ref: format!("gh:o/{}", folder.to_ascii_lowercase())
                .parse()
                .unwrap(),
            name: folder.to_owned(),
            version: "unknown".into(),
            version_confidence: crate::model::VersionConfidence::Toc,
            release_id: None,
            channel: crate::model::Channel::Stable,
            pinned: false,
            reason: crate::model::AddonReason::Explicit,
            required_by: Vec::new(),
            folders: vec![name.clone()],
            state: crate::model::AddonState::Ok,
            installed_at: "2000-01-01T00:00:00Z".parse().unwrap(),
            tx_id: TxId::new(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        };
        let spec = super::edit::AdoptSpec {
            record,
            folders: vec![(name, index)],
        };
        self.edit(folder, super::edit::RecordEdit::Adopt(Box::new(spec)))
    }

    /// The target's files index now.
    pub fn index(&self) -> crate::state::files_index::FilesIndex {
        let key = crate::discover::registry::read_marker(self.client.client_dir()).unwrap();
        crate::state::store::load::<crate::state::files_index::FilesIndex>(
            &self.paths.files_index(&key),
        )
        .unwrap()
        .unwrap()
        .value
    }

    /// The AddOns tree, as [`tree_snapshot`] sees it.
    pub fn addons(&self) -> Vec<TreeEntry> {
        tree_snapshot(&self.client.addons())
    }
}

/// A hand-made addon folder at `dir`, shaped as no archive install is: its TOC, an empty
/// directory, `libs/` at 0775, a symlink `link.lua -> libs/lib.lua`, and `private/` at
/// 0700 holding a 0600 file.
pub fn hand_made(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    for sub in ["empty", "libs", "private"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(dir.join(format!("{name}.toc")), "## Interface: 120105\n").unwrap();
    std::fs::write(dir.join("libs/lib.lua"), "-- lib").unwrap();
    std::os::unix::fs::symlink("libs/lib.lua", dir.join("link.lua")).unwrap();
    std::fs::write(dir.join("private/p.lua"), "-- private").unwrap();
    mode(&dir.join("libs"), 0o775);
    mode(&dir.join("private/p.lua"), 0o600);
    mode(&dir.join("private"), 0o700);
}

/// One entry under a [`tree_snapshot`] root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: PathBuf,
    /// `d`, `f`, `l` or `?`.
    pub kind: char,
    /// Permission bits, setuid, setgid and sticky included.
    pub mode: u32,
    pub link: Option<PathBuf>,
    pub sha256: Option<String>,
}

/// Everything a restore must bring back under `dir`, sorted by path: every entry's kind
/// and mode, symlink targets (never followed), empty directories and file contents.
/// Timestamps are left out (a restore writes new ones); a missing `dir` is empty.
pub fn tree_snapshot(dir: &Path) -> Vec<TreeEntry> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&at) else {
            continue;
        };
        for child in children {
            let path = child.unwrap().path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let kind = meta.file_type();
            let (kind, link, sha256) = if kind.is_symlink() {
                ('l', Some(std::fs::read_link(&path).unwrap()), None)
            } else if kind.is_dir() {
                pending.push(path.clone());
                ('d', None, None)
            } else if kind.is_file() {
                let (_, sha256) = crate::state::files_index::hash_file_streaming(&path).unwrap();
                ('f', None, Some(sha256))
            } else {
                ('?', None, None)
            };
            out.push(TreeEntry {
                path: path.strip_prefix(dir).unwrap().to_path_buf(),
                kind,
                mode: meta.permissions().mode() & 0o7777,
                link,
                sha256,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// One thread's `pause` failpoint markers (`failpoint::with_spec_in`): the test waits
/// until the thread [`Pause::reached`] a step, acts, then [`Pause::resume`]s it, so the
/// interleaving a test names is the one that runs.
pub struct Pause {
    dir: tempfile::TempDir,
}

impl Pause {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// Where the thread's markers go: pass it to `failpoint::with_spec_in`.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Waits (30 s at most) until the thread stops at `name`.
    pub fn reached(&self, name: &str) {
        let marker = self.dir().join(format!("{name}.reached"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the thread never reached {name}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Lets the thread go on from `name`; later hits of `name` pass straight through.
    pub fn resume(&self, name: &str) {
        std::fs::write(self.dir().join(format!("{name}.resume")), "").unwrap();
    }
}

/// What recovery did to one journal, as the in-crate differential reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Forward,
    Back,
    Pruned,
    Untouched,
    /// Recovery failed the caller here.
    Failed {
        code: crate::error::ErrorCode,
        reason: Option<String>,
        step: Option<u32>,
    },
    /// Recovery only warned of it.
    Warned {
        reason: Option<String>,
    },
}

/// E11's differential in process (`tests/common/differential.rs` holds the binary's):
/// `journal::diagnose` on `paths`, with the clients and the `sv_writable` recovery will
/// have, predicts (twice, equally, writing nothing); then `recover` runs, and each
/// diagnosis's `expected()` must agree with what it did for a caller committing `own`,
/// unless the diagnosis flags itself as approximate. The first journal that fails the
/// caller ends the run. Returns recovery's result and the diagnoses.
pub fn predict_then_compare(
    test: &str,
    paths: &Paths,
    clients: &[PathBuf],
    sv_writable: &dyn Fn(&Path) -> bool,
    own: &[TargetKey],
    recover: impl FnOnce() -> crate::error::Result<Vec<crate::error::Warning>>,
) -> (
    crate::error::Result<Vec<crate::error::Warning>>,
    journal::Diagnoses,
) {
    use crate::error::WarningCode;
    use journal::{Expected, StopReason};

    let diagnoses = journal::diagnose(paths, clients, sv_writable);
    assert_eq!(
        journal::diagnose(paths, clients, sv_writable),
        diagnoses,
        "{test}: two diagnoses differ"
    );
    let result = recover();
    let step_in = |message: &str| -> Option<u32> {
        let (_, rest) = message.split_once("journal step ")?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let reason_in = |message: &str| -> Option<String> {
        ["could not be recovered (", "cannot recover it ("]
            .iter()
            .find_map(|lead| message.split_once(lead))
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(reason, _)| reason.to_owned())
    };
    let mut ended = false;
    for diagnosis in &diagnoses.journals {
        let tx = diagnosis.tx.to_string();
        let warned = |code: WarningCode| {
            result.as_ref().ok().and_then(|warnings| {
                warnings
                    .iter()
                    .find(|w| w.code == code && w.message.contains(&tx))
            })
        };
        let seen = match &result {
            Err(e) if e.details["journal_pending"] == tx.as_str() => Seen::Failed {
                code: e.code,
                reason: e.details["reason"].as_str().map(str::to_owned),
                step: step_in(&e.message),
            },
            _ => match (
                warned(WarningCode::JournalRecovered),
                warned(WarningCode::JournalPending),
            ) {
                (Some(w), _) if w.message.contains("rolled forward") => Seen::Forward,
                (Some(_), _) => Seen::Back,
                (None, Some(w)) => Seen::Warned {
                    reason: reason_in(&w.message),
                },
                (None, None) if journal::existing(paths, &diagnosis.tx).is_some() => {
                    Seen::Untouched
                }
                (None, None) => Seen::Pruned,
            },
        };
        let want = if ended {
            Seen::Untouched
        } else {
            match diagnosis.expected() {
                Expected::RolledForward => Seen::Forward,
                Expected::RolledBack => Seen::Back,
                Expected::Pruned => Seen::Pruned,
                Expected::Skipped => Seen::Untouched,
                Expected::Stops { code, reason, step } => {
                    let fails = diagnosis.target.is_none_or(|t| own.contains(&t))
                        || reason == StopReason::Unattributed;
                    ended |= fails;
                    let reason = Some(reason.as_str().to_owned());
                    if fails {
                        Seen::Failed { code, reason, step }
                    } else {
                        Seen::Warned { reason }
                    }
                }
            }
        };
        let agrees = match (&want, &seen) {
            (
                Seen::Failed { code, reason, step },
                Seen::Failed {
                    code: seen_code,
                    reason: seen_reason,
                    step: seen_step,
                },
            ) => {
                code == seen_code && reason == seen_reason && (step.is_none() || step == seen_step)
            }
            _ => want == seen,
        };
        assert!(
            agrees || !diagnosis.exact(),
            "{test}: diagnose predicted {want:?}, recovery did {seen:?}\n{diagnosis:#?}\n{result:?}"
        );
    }
    (result, diagnoses)
}

/// A doctor run's directory name, as E3's runtime names one.
pub const REPAIR_RUN_ID: &str = "2026-10-30T20-45-20Z__01M678YN3A22K2ASPW2VZH88A8";

/// The stuck journal [`repair_home`] leaves for a suspension to set aside.
pub const REPAIR_STUCK_TX: &str = "01JB8Q2VA00000000000000009";

/// The account folder of [`repair_home`]'s `WTF/`: a name no message may carry.
pub const REPAIR_ACCOUNT: &str = "WOWACCOUNTFIXTURE";

/// A home to lower repairs in (E5): its state (a target record and a stuck journal with
/// its in-flight lock), its config (a 0644 GitHub credential), its cache (a blob and a
/// temporary link a crash left), an agent skill (with a temporary link beside it), the
/// bar widget's folder, one client the repair is planned for and one it is not, and a
/// published doctor run. The planned client's AddOns hold
/// `Stray/`, `Wrap/Inner/` (a wrapped addon), `Same/Same/` (one wrapped in its own name),
/// `Dup/Stray/` and `Case/STRAY/` (which would land on `Stray`), `Blizzard_Foo/` and
/// `Linked`, a developer's symlinked checkout; its `WTF/` holds settings and an
/// account's SavedVariables, and its stage what the stuck journal staged.
pub struct RepairHome {
    /// Keeps the tempdir alive for the home's lifetime.
    _root: tempfile::TempDir,
    /// Everything the home holds is below it.
    pub base: PathBuf,
    pub paths: Paths,
    pub client: ClientPaths,
    pub key: TargetKey,
    /// Registered in the scope table, but not one of `planned`.
    pub unplanned: ClientPaths,
    /// The client a repair here may touch.
    pub planned: Vec<(PathBuf, TargetKey)>,
    pub skill_dirs: Vec<PathBuf>,
    pub scopes: ScopeTable,
    pub run: crate::engine::repair::RepairRun,
}

impl RepairHome {
    /// `<client>/Interface/AddOns/<rel>`.
    pub fn addons(&self, rel: &str) -> PathBuf {
        self.client.addons().join(rel)
    }

    pub fn skill_file(&self) -> PathBuf {
        self.paths.skill_dir().join("SKILL.md")
    }

    pub fn credential(&self) -> PathBuf {
        self.paths.credential_file("github")
    }

    pub fn record(&self) -> PathBuf {
        self.paths.target_record(&self.key)
    }

    pub fn blob(&self) -> PathBuf {
        self.paths.blobs_dir().join("ab")
    }

    /// The temporary link a crash inside `symlink_atomic` left in the cache.
    pub fn temp_link(&self) -> PathBuf {
        self.paths.cache_dir().join("tmp/.omaddons-link")
    }

    pub fn stuck(&self) -> TxId {
        REPAIR_STUCK_TX.parse().unwrap()
    }
}

pub fn repair_home() -> RepairHome {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let home = base.join("home");
    let paths = Paths::with_roots(
        &home,
        home.join(".config"),
        home.join(".local/state"),
        home.join(".cache"),
    );
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let key = TargetKey::new();
    write(&paths.target_record(&key), "{}\n");
    let stuck: TxId = REPAIR_STUCK_TX.parse().unwrap();
    write(&paths.journal(&stuck), "{\"line\":\"prepared\"}\n");
    write(&paths.inflight_lock(&stuck), "");
    let credential = paths.credential_file("github");
    write(&credential, "ghp_fixture\n");
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o644)).unwrap();
    write(&paths.blobs_dir().join("ab"), "blob\n");
    std::fs::create_dir_all(paths.cache_dir().join("tmp")).unwrap();
    symlink("../blobs/ab", paths.cache_dir().join("tmp/.omaddons-link")).unwrap();
    let skill = paths.skill_dir().join("SKILL.md");
    write(&skill, "---\nname: omaddons\n---\nold guide\n");
    std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o644)).unwrap();
    // A temporary link outside omaddons' own folders, which the doctor never moves.
    symlink("SKILL.md", paths.skill_dir().join(".omaddons-guide")).unwrap();
    // The widget's folder: a write scope of the doctor's with no quarantine of its own.
    std::fs::create_dir_all(paths.omarchy_plugin_dir()).unwrap();

    let client_dir = base.join("games/wow/_retail_");
    let client = ClientPaths::new(&client_dir);
    let addons = client.addons();
    folder(&addons.join("Stray"), "stray");
    write(&addons.join("Stray/stray.lua"), "-- stray\n");
    folder(&addons.join("Wrap/Inner"), "inner");
    write(&addons.join("Wrap/README.txt"), "wrapped by a zip\n");
    folder(&addons.join("Same/Same"), "same");
    write(&addons.join("Same/notes.txt"), "wrapped in its own name\n");
    folder(&addons.join("Dup/Stray"), "a second stray");
    folder(&addons.join("Case/STRAY"), "a stray in capitals");
    folder(&addons.join("Blizzard_Foo"), "the client's");
    folder(&base.join("dev/Linked"), "a checkout");
    symlink(base.join("dev/Linked"), addons.join("Linked")).unwrap();
    write(&client.wtf().join("Config.wtf"), "SET realmList \"x\"\n");
    write(
        &client
            .wtf()
            .join(format!("Account/{REPAIR_ACCOUNT}/SavedVariables/Stray.lua")),
        "StrayDB = {}\n",
    );
    write(
        &client.stage(&stuck).join("Foo/Foo.toc"),
        "## Interface: 120105\n",
    );
    mark(&client_dir, key);

    let unplanned_dir = base.join("games/wow/_classic_");
    let unplanned = ClientPaths::new(&unplanned_dir);
    folder(&unplanned.addons().join("Other"), "another client's");
    mark(&unplanned_dir, TargetKey::new());

    let run_dir = paths.doctor_runs_dir().join(REPAIR_RUN_ID);
    std::fs::create_dir_all(&run_dir).unwrap();
    let scopes = ScopeTable::new(&paths, &[client_dir.clone(), unplanned_dir], None).unwrap();
    RepairHome {
        _root: root,
        base,
        skill_dirs: vec![paths.skill_dir()],
        paths,
        client,
        key,
        unplanned,
        planned: vec![(client_dir, key)],
        scopes,
        run: crate::engine::repair::RepairRun {
            dir: run_dir,
            id: REPAIR_RUN_ID.to_owned(),
        },
    }
}

/// Every entry below `dir`: its path, mode, mtime, inode and content (a symlink's
/// target). Equal hashes mean nothing below `dir` was written, created, removed, moved
/// or even touched.
pub fn tree_hash(dir: &Path) -> String {
    tree_hash_except(dir, &[])
}

/// [`tree_hash`] without the entries at `except` (the global lock file, which taking the
/// lock writes).
pub fn tree_hash_except(dir: &Path, except: &[PathBuf]) -> String {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let mut entries = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for child in std::fs::read_dir(&at).unwrap() {
            let path = child.unwrap().path();
            if except.contains(&path) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let content = if meta.is_dir() {
                pending.push(path.clone());
                Vec::new()
            } else if meta.file_type().is_symlink() {
                std::fs::read_link(&path)
                    .unwrap()
                    .as_os_str()
                    .as_bytes()
                    .to_vec()
            } else {
                std::fs::read(&path).unwrap()
            };
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            let stat = (meta.mode(), meta.mtime(), meta.mtime_nsec(), meta.ino());
            entries.push((rel, stat, content));
        }
    }
    entries.sort();
    let mut hash = Sha256::new();
    for (rel, (mode, mtime, nsec, ino), content) in entries {
        hash.update(rel.as_os_str().as_bytes());
        hash.update(format!("\0{mode:o}\0{mtime}\0{nsec}\0{ino}\0").as_bytes());
        hash.update(&content);
        hash.update(b"\n");
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
