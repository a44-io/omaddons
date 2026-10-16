//! Shared builders for the engine's unit tests (failpoint tests must be unit tests:
//! `failpoint::with_spec` is per-thread and test-only).

// The single-writer scan skips files marked test-only.
#![cfg(test)]

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::inspect::{ArchiveLimits, inspect_dir};
use super::journal::{self, CommitPayload, IndexChanges, Journal, Prepared};
use super::stage::{Staged, stage};
use crate::fsx::{
    GlobalLock, GlobalLockGuard, InflightLock, ScopeTable, SnapshotRoots, WriteSession, Writer,
};
use crate::model::{Actor, Target, TargetKey, TxId, TxKind};
use crate::rules::{ClientRules, ClientVersion, Family, InstallKind, RulesTable, TargetKind};
use crate::state::history::{self, HistoryEntry, TxResult};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::TargetRecord;

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
    }
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
            registry = registry.with_git(crate::sources::git_hosts::GitRunner::for_tests());
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
            retention: super::retention::RetentionLimits::default(),
            actor: self.actor,
            argv: &argv,
            command,
            running: Some(&self.running),
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

    /// Runs `changes` on the target as currently registered.
    pub fn run(
        &self,
        changes: Vec<super::plan::RequestedChange>,
    ) -> crate::error::Result<super::tx::TxOutcomes> {
        let req = super::tx::TxRequest {
            targets: vec![super::tx::TxTarget {
                discovered: self.current_target(),
                changes,
            }],
            force: self.force,
            while_running: self.while_running,
            dry_run: false,
            reason: Some("testing".into()),
        };
        self.with_ctx("install", |ctx| super::tx::run(&req, ctx))
    }

    /// The AddOns tree's digest.
    pub fn addons(&self) -> crate::fsx::TreeDigest {
        crate::fsx::TreeDigest::compute(&self.client.addons()).unwrap()
    }
}
