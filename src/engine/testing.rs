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
    pub inflight: InflightLock,
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
            inflight,
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

    /// Creates the journal with a minimal payload.
    pub fn journal(&self, s: &mut WriteSession<'_>, lock: &GlobalLockGuard) -> Journal {
        let prepared = Prepared {
            schema_version: journal::CURRENT,
            tx: self.tx,
            target_key: self.key,
            client_dir: self.client.client_dir().to_path_buf(),
            kind: TxKind::Install,
            started_at: Timestamp::now(),
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
                },
                stage_dir: self.staged.stage_dir.clone(),
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
