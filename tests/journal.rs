//! The journal end to end with real renames: a six-step transaction (exchange, old to
//! snapshot, a new folder, a removal, and the two-rename fallback) stopped at every
//! point, then recovered under the lock; plus read-side reporting.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::env::TestEnv;
use common::http::paths;
use common::targets::battlenet;
use common::trees::tree_hash;
use jiff::Timestamp;
use omaddons::engine::journal::{
    CURRENT, CommitPayload, IndexChanges, Journal, Prepared, RecoveryEnv, Step, StepOp,
    StepPurpose, pending_warnings, recover_all,
};
use omaddons::error::{Warning, WarningCode};
use omaddons::fsx::{
    GlobalLock, InflightLock, Op, RenameMode, ScopeTable, WriteSession, Writer, mutate,
};
use omaddons::model::{Actor, FolderName, Target, TargetKey, TxId, TxKind};
use omaddons::state::files_index::{FilesIndex, FolderIndex};
use omaddons::state::history::{self, FolderOp, HistoryEntry, HistoryFilter, HistoryOp, TxResult};
use omaddons::state::paths::{ClientPaths, Paths};
use omaddons::state::records::{TargetRecord, load_record};
use omaddons::state::store;
use support::Scene;

#[cfg(test)]
mod support {
    use super::*;

    /// A retail client with AddOns `Foo`, `Gone` and `Baz`, and a stage holding new
    /// `Foo`, `Bar` and `Baz`.
    pub struct Scene {
        pub env: TestEnv,
        pub target: Target,
        pub paths: Paths,
        pub scopes: ScopeTable,
        pub key: TargetKey,
        pub tx: TxId,
        pub addons: PathBuf,
        pub stage: PathBuf,
        pub snaps: PathBuf,
        pub old: String,
    }

    fn folder(dir: &Path, text: &str) {
        fs::create_dir_all(dir).unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        fs::write(
            dir.join(format!("{name}.toc")),
            format!("## Interface: 120105\n## Notes: {text}\n"),
        )
        .unwrap();
    }

    impl Scene {
        pub fn new() -> Self {
            let env = TestEnv::new();
            let target = battlenet(&env, "wow");
            let paths = paths(&env);
            let scopes =
                ScopeTable::new(&paths, std::slice::from_ref(&target.client_dir), None).unwrap();
            let addons = target.addons_dir();
            for (name, text) in [("Foo", "old foo"), ("Gone", "gone"), ("Baz", "old baz")] {
                folder(&addons.join(name), text);
            }
            let tx = TxId::new();
            let client = ClientPaths::new(&target.client_dir);
            let (stage, snaps) = (client.stage(&tx), client.snapshots(&tx));
            for (name, text) in [("Foo", "new foo"), ("Bar", "new bar"), ("Baz", "new baz")] {
                folder(&stage.join(name), text);
            }
            fs::create_dir_all(&snaps).unwrap();
            let old = tree_hash(&addons);
            // Registration marks the client before any of its journals exists.
            let key = TargetKey::new();
            let marker = omaddons::discover::registry::marker_path(&target.client_dir);
            fs::create_dir_all(marker.parent().unwrap()).unwrap();
            fs::write(&marker, format!("{{\"key\":\"{key}\"}}")).unwrap();
            Self {
                env,
                target,
                paths,
                scopes,
                key,
                tx,
                addons,
                stage,
                snaps,
                old,
            }
        }

        pub fn steps(&self) -> Vec<Step> {
            let step = |op, from: PathBuf, to: PathBuf, purpose| Step {
                op,
                from,
                to,
                purpose,
            };
            vec![
                step(
                    StepOp::Exchange,
                    self.stage.join("Foo"),
                    self.addons.join("Foo"),
                    StepPurpose::SwapIn,
                ),
                step(
                    StepOp::Noreplace,
                    self.stage.join("Foo"),
                    self.snaps.join("Foo"),
                    StepPurpose::OldToSnapshot,
                ),
                step(
                    StepOp::Noreplace,
                    self.stage.join("Bar"),
                    self.addons.join("Bar"),
                    StepPurpose::SwapIn,
                ),
                step(
                    StepOp::Noreplace,
                    self.addons.join("Gone"),
                    self.snaps.join("Gone"),
                    StepPurpose::RemoveToSnapshot,
                ),
                step(
                    StepOp::Rename,
                    self.addons.join("Baz"),
                    self.snaps.join("Baz"),
                    StepPurpose::FallbackLiveToSnapshot,
                ),
                step(
                    StepOp::Rename,
                    self.stage.join("Baz"),
                    self.addons.join("Baz"),
                    StepPurpose::FallbackStageToLive,
                ),
            ]
        }

        pub fn prepared(&self) -> Prepared {
            let folder = |name: &str| FolderName::new(name).unwrap();
            Prepared {
                schema_version: CURRENT,
                tx: self.tx,
                target_key: self.key,
                client_dir: self.target.client_dir.clone(),
                kind: TxKind::Install,
                started_at: Timestamp::now(),
                creates_dirs: Vec::new(),
                commit: CommitPayload {
                    record: TargetRecord {
                        key: self.key,
                        generation: 1,
                        addons: Default::default(),
                    },
                    index: IndexChanges {
                        upsert: ["Foo", "Bar", "Baz"]
                            .iter()
                            .map(|n| FolderIndex::new(*n, Vec::new()))
                            .collect(),
                        remove: vec!["gone".into()],
                    },
                    history: HistoryEntry {
                        schema_version: history::CURRENT,
                        tx: self.tx,
                        kind: TxKind::Install,
                        at: Timestamp::now(),
                        target: self.key,
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into(), "install".into()],
                        ops: vec![
                            HistoryOp {
                                op: FolderOp::Replace,
                                folder: folder("Foo"),
                                addon: "Foo".into(),
                                edit: None,
                            },
                            HistoryOp {
                                op: FolderOp::Add,
                                folder: folder("Bar"),
                                addon: "Bar".into(),
                                edit: None,
                            },
                            HistoryOp {
                                op: FolderOp::Remove,
                                folder: folder("Gone"),
                                addon: "Gone".into(),
                                edit: None,
                            },
                        ],
                        result: TxResult::Committed,
                        reverts: None,
                        sv_deferred: false,
                        created_addons_dir: false,
                        run: None,
                        backup: None,
                    },
                    stage_dir: self.stage.clone(),
                    sv_restore: None,
                },
            }
        }

        /// What a reboot does on btrfs, FUSE or device-mapper: every device number the
        /// journal's steps recorded changes; inode numbers stay.
        pub fn renumber_devices(&self) {
            common::engine::renumber_journal_devices(&self.paths.journal_dir());
        }

        /// Runs `executed` steps, optionally records the next without running it, then
        /// the markers, and "crashes" (drops everything).
        pub fn run(&self, executed: usize, record_next: bool, swapped: bool, committed: bool) {
            let lock = GlobalLock::acquire(&self.paths, "test").unwrap();
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            let mut journal = Journal::create(&mut s, &self.paths, &lock, self.prepared()).unwrap();
            let steps = self.steps();
            for step in steps.iter().take(executed) {
                journal.step(&mut s, step.clone()).unwrap();
                let mode = match step.op {
                    StepOp::Exchange => RenameMode::Exchange,
                    StepOp::Noreplace => RenameMode::NoReplace,
                    StepOp::Rename => RenameMode::Replace,
                };
                mutate(&mut s, &step.from, Op::Rename { to: &step.to, mode }).unwrap();
            }
            if record_next {
                journal.step(&mut s, steps[executed].clone()).unwrap();
            }
            if swapped {
                journal.mark_swapped(&mut s).unwrap();
            }
            if committed {
                journal.mark_committed(&mut s).unwrap();
            }
        }

        pub fn recover(&self) -> Vec<Warning> {
            let lock = GlobalLock::acquire(&self.paths, "test").unwrap();
            let env = RecoveryEnv {
                lock: &lock,
                paths: &self.paths,
                scopes: &self.scopes,
                own: std::slice::from_ref(&self.key),
                sv_writable: &|_| false,
            };
            recover_all(&env).unwrap()
        }

        pub fn text(&self, folder: &str) -> Option<String> {
            fs::read_to_string(self.addons.join(folder).join(format!("{folder}.toc"))).ok()
        }

        pub fn history(&self) -> Vec<HistoryEntry> {
            history::read(&self.paths, &HistoryFilter::default())
                .unwrap()
                .entries
        }

        /// The journal and the stage dir are gone.
        pub fn assert_cleaned(&self) {
            assert!(!self.paths.journal(&self.tx).exists(), "journal left");
            assert!(!self.stage.exists(), "stage dir left");
        }
    }
}

#[test]
fn every_interrupted_prefix_rolls_back_to_the_old_trees() {
    // Also after a reboot that renumbered the devices the steps recorded.
    for (executed, record_next, reboot) in (0..=6)
        .flat_map(|e| [(e, false), (e, true)])
        .flat_map(|(e, r)| [(e, r, false), (e, r, true)])
    {
        if record_next && executed == 6 {
            continue;
        }
        let scene = Scene::new();
        scene.run(executed, record_next, false, false);
        if reboot {
            scene.renumber_devices();
        }
        let warnings = scene.recover();
        let label = format!("{executed} steps run, next recorded: {record_next}, reboot: {reboot}");
        assert_eq!(
            tree_hash(&scene.addons),
            scene.old,
            "{label}\n{}",
            common::trees::listing(&scene.addons)
        );
        scene.assert_cleaned();
        assert_eq!(warnings.len(), 1, "{label}");
        assert_eq!(warnings[0].code, WarningCode::JournalRecovered);
        assert!(
            warnings[0].message.contains("rolled back"),
            "{label}: {}",
            warnings[0].message
        );
        assert_eq!(
            warnings[0].fix.as_ref().and_then(|f| f.argv()),
            Some(
                &[
                    "omaddons".to_owned(),
                    "history".into(),
                    "--limit".into(),
                    "5".into()
                ][..]
            )
        );
        assert!(scene.history().is_empty(), "{label}: nothing committed");
        assert!(
            scene.recover().is_empty(),
            "{label}: a second recovery finds nothing"
        );
    }
}

#[test]
fn swapped_rolls_forward_once() {
    let scene = Scene::new();
    scene.run(6, false, true, false);
    let warnings = scene.recover();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].message.contains("rolled forward"),
        "{}",
        warnings[0].message
    );
    assert_eq!(
        (
            scene.text("Foo"),
            scene.text("Bar"),
            scene.text("Baz"),
            scene.text("Gone")
        ),
        (
            Some("## Interface: 120105\n## Notes: new foo\n".into()),
            Some("## Interface: 120105\n## Notes: new bar\n".into()),
            Some("## Interface: 120105\n## Notes: new baz\n".into()),
            None
        )
    );
    scene.assert_cleaned();
    assert_eq!(
        load_record(&scene.paths, &scene.key)
            .unwrap()
            .value
            .generation,
        1
    );
    let index = store::load::<FilesIndex>(&scene.paths.files_index(&scene.key))
        .unwrap()
        .unwrap()
        .value;
    assert_eq!(
        index.folders.keys().collect::<Vec<_>>(),
        ["bar", "baz", "foo"]
    );
    let history = scene.history();
    assert_eq!((history.len(), history[0].tx), (1, scene.tx));
    assert!(scene.recover().is_empty());
    assert_eq!(scene.history().len(), 1, "one history entry");
}

#[test]
fn a_roll_forward_interrupted_after_writing_state_does_not_repeat_it() {
    let scene = Scene::new();
    scene.run(6, false, true, false);
    // A first recovery got as far as the record and the history line, then died.
    let prepared = scene.prepared();
    let scopes = &scene.scopes;
    let mut s = WriteSession::untracked(Writer::Mutating, scopes, "test");
    fs::create_dir_all(scene.paths.targets_dir()).unwrap();
    store::save(
        &mut s,
        &scene.paths.target_record(&scene.key),
        &prepared.commit.record,
        None,
    )
    .unwrap();
    history::append(&mut s, &scene.paths, &prepared.commit.history).unwrap();

    assert_eq!(scene.recover().len(), 1);
    assert_eq!(
        scene.history().len(),
        1,
        "the history line is not appended twice"
    );
    assert_eq!(
        load_record(&scene.paths, &scene.key)
            .unwrap()
            .value
            .generation,
        1
    );
    scene.assert_cleaned();
}

#[test]
fn committed_journals_are_removed_silently() {
    let scene = Scene::new();
    scene.run(6, false, true, true);
    assert!(scene.recover().is_empty());
    scene.assert_cleaned();
    assert!(
        scene.history().is_empty(),
        "the writer wrote state; recovery does not"
    );
}

#[test]
fn torn_and_unprepared_journals() {
    let scene = Scene::new();
    scene.run(2, false, false, false);
    let journal = scene.paths.journal(&scene.tx);
    let mut text = fs::read_to_string(&journal).unwrap();
    text.push_str("{\"type\":\"step\",\"n\":3,\"op\":\"nore");
    fs::write(&journal, text).unwrap();
    assert_eq!(scene.recover().len(), 1);
    assert_eq!(tree_hash(&scene.addons), scene.old);

    let other = TxId::new();
    fs::write(scene.paths.journal(&other), "{\"type\":\"prep").unwrap();
    assert!(scene.recover().is_empty(), "nothing ran, nothing to report");
    assert!(!scene.paths.journal(&other).exists());
}

#[test]
fn a_live_transaction_is_never_recovered() {
    let scene = Scene::new();
    let inflight = InflightLock::create(&scene.paths, &scene.tx).unwrap();
    scene.run(1, false, false, false);
    assert!(scene.recover().is_empty());
    assert!(
        scene.paths.journal(&scene.tx).exists(),
        "its writer is alive"
    );
    drop(inflight);
    assert_eq!(scene.recover().len(), 1);
    assert_eq!(tree_hash(&scene.addons), scene.old);
}

#[test]
fn readers_report_without_writing() {
    let scene = Scene::new();
    assert!(pending_warnings(&scene.paths).unwrap().is_empty());
    scene.run(1, false, false, false);
    let state = scene.env.state();
    let before = tree_hash(&state);

    let free = pending_warnings(&scene.paths).unwrap();
    assert_eq!(tree_hash(&state), before, "reading wrote nothing");
    assert_eq!(free.len(), 1);
    assert_eq!(free[0].code, WarningCode::JournalPending);
    assert!(
        free[0].message.contains(&scene.tx.to_string())
            && free[0].message.contains(&scene.key.to_string()),
        "{}",
        free[0].message
    );
    assert_eq!(
        free[0].fix.as_ref().and_then(|f| f.argv()),
        Some(&["omaddons".to_owned(), "status".into()][..])
    );

    let held = {
        // Another command holds the lock (taking it records the holder).
        let _lock = GlobalLock::acquire(&scene.paths, "another command").unwrap();
        let before = tree_hash(&state);
        let held = pending_warnings(&scene.paths).unwrap();
        assert_eq!(tree_hash(&state), before, "reading wrote nothing");
        held
    };
    assert_eq!(held[0].code, WarningCode::TxInProgress);
    assert_eq!(held[0].fix.as_ref().and_then(|f| f.argv()), None);
}

#[test]
fn committed_journals_are_never_reported() {
    let scene = Scene::new();
    scene.run(6, false, true, true);
    assert!(scene.paths.journal(&scene.tx).exists());
    assert!(pending_warnings(&scene.paths).unwrap().is_empty());
}
