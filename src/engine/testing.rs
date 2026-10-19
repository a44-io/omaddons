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
    finish_saved_variables(s, tx, client, folders, SvPrestage::default())
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
                    target: self.key,
                    actor: Actor::Agent,
                    reason: None,
                    argv: vec!["omaddons".into(), "install".into()],
                    ops: Vec::new(),
                    result: TxResult::Committed,
                    reverts: None,
                    sv_deferred: false,
                    created_addons_dir: false,
                    run: None,
                },
                stage_dir: self.staged.stage_dir.clone(),
                sv_restore: None,
            },
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
        let req = super::tx::TxRequest {
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
        };
        self.with_ctx("install", |ctx| super::tx::run(&req, ctx))
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
