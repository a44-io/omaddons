//! The journal end to end with real renames: a six-step transaction (exchange, old to
//! snapshot, a new folder, a removal, and the two-rename fallback) stopped at every
//! point, then recovered under the lock; plus read-side reporting.
//!
//! e2e suite: E11's differential in process: `journal::diagnose` predicts every state of
//! the six-step transaction, each stuck state and each unreadable input, and recovery
//! then does what it predicted.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::differential::{
    Caller, Games, Prediction, assert_agrees, assert_settled, observe_result, predict,
};
use common::env::TestEnv;
use common::http::paths;
use common::targets::battlenet;
use common::tlog::{TestLog, tlog};
use common::trees::tree_hash;
use jiff::Timestamp;
use omaddons::engine::journal::{
    CURRENT, CommitPayload, Expected, IndexChanges, Journal, JournalVerdict, Prepared, RecoveryEnv,
    Step, StepOp, StepPurpose, UnattributedCause, pending_warnings, recover_all,
};
use omaddons::error::{Result, Warning, WarningCode};
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
                writes_wtf: false,
                staged_stamp: None,
                staged_files: None,
                source_backup: None,
                client: None,
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
                        target: Some(self.key),
                        targets: Vec::new(),
                        actor: Actor::Agent,
                        reason: None,
                        argv: vec!["omaddons".into(), "install".into()],
                        ops: vec![
                            HistoryOp {
                                op: FolderOp::Replace,
                                folder: folder("Foo"),
                                addon: "Foo".into(),
                                edit: None,
                                backup: None,
                                from: None,
                                files: None,
                            },
                            HistoryOp {
                                op: FolderOp::Add,
                                folder: folder("Bar"),
                                addon: "Bar".into(),
                                edit: None,
                                backup: None,
                                from: None,
                                files: None,
                            },
                            HistoryOp {
                                op: FolderOp::Remove,
                                folder: folder("Gone"),
                                addon: "Gone".into(),
                                edit: None,
                                backup: None,
                                from: None,
                                files: None,
                            },
                        ],
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

        /// What `journal::diagnose` predicts of the scene now, with recovery's own
        /// `sv_writable` (nothing writable, as [`Self::recover`] has it).
        pub fn predict(&self, log: &TestLog, label: &str) -> Prediction {
            log.env(&self.env);
            let clients = [self.target.client_dir.clone()];
            predict(log, label, &self.paths, &clients, &Games::Unknown)
        }

        /// Recovery in process, held to `prediction` (E11): what it did to each journal,
        /// and, once none is left, nothing diagnosed after. Its result.
        pub fn recover_predicted(
            &self,
            log: &TestLog,
            prediction: &Prediction,
        ) -> Result<Vec<Warning>> {
            let result = {
                let lock = GlobalLock::acquire(&self.paths, "test").unwrap();
                let env = RecoveryEnv {
                    lock: &lock,
                    paths: &self.paths,
                    scopes: &self.scopes,
                    own: std::slice::from_ref(&self.key),
                    sv_writable: &|_| false,
                };
                recover_all(&env)
            };
            let after: Vec<PathBuf> = fs::read_dir(self.paths.journal_dir())
                .map(|l| l.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            let observation = observe_result(&result, prediction, &after);
            let caller = Caller::Commit {
                own: vec![self.key],
            };
            assert_agrees(log, prediction, &observation, &caller);
            if after.is_empty() {
                let clients = [self.target.client_dir.clone()];
                assert_settled(log, prediction, &self.paths, &clients);
            }
            result
        }

        /// Predicts, then recovers in process and compares.
        pub fn predicted(&self, log: &TestLog, label: &str) -> (Prediction, Result<Vec<Warning>>) {
            log.step(label);
            let prediction = self.predict(log, label);
            let result = self.recover_predicted(log, &prediction);
            (prediction, result)
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

// -----------------------------------------------------------------------------------------
// E11: `journal::diagnose` predicts, recovery does.

/// The only diagnosis of `prediction`.
fn only(prediction: &Prediction) -> (JournalVerdict, Expected) {
    assert_eq!(
        prediction.diagnoses.journals.len(),
        1,
        "{:#?}",
        prediction.diagnoses
    );
    let diagnosis = &prediction.diagnoses.journals[0];
    (diagnosis.verdict, diagnosis.expected())
}

#[test]
fn diagnose_predicts_every_interrupted_prefix() {
    let log = tlog!();
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
        let label = format!("{executed} steps run, next recorded: {record_next}, reboot: {reboot}");
        let (prediction, result) = scene.predicted(&log, &label);
        assert_eq!(only(&prediction).1, Expected::RolledBack, "{label}");
        let ran: Vec<u32> = (1..=u32::try_from(executed).unwrap()).collect();
        assert_eq!(
            prediction.diagnoses.journals[0].evidence.ran, ran,
            "{label}: the steps the walk reverses are those that ran"
        );
        assert_eq!(result.unwrap().len(), 1, "{label}");
        assert_eq!(tree_hash(&scene.addons), scene.old, "{label}");
    }
}

#[test]
fn diagnose_predicts_forward_commit_torn_and_unprepared() {
    let log = tlog!();
    let scene = Scene::new();
    scene.run(6, false, true, false);
    let (prediction, _) = scene.predicted(&log, "swapped");
    assert_eq!(only(&prediction).1, Expected::RolledForward);

    let scene = Scene::new();
    scene.run(6, false, true, true);
    let (prediction, result) = scene.predicted(&log, "committed");
    assert_eq!(only(&prediction).1, Expected::Pruned);
    assert!(result.unwrap().is_empty());

    let scene = Scene::new();
    scene.run(2, false, false, false);
    let journal = scene.paths.journal(&scene.tx);
    let mut text = fs::read_to_string(&journal).unwrap();
    text.push_str("{\"type\":\"step\",\"n\":3,\"op\":\"nore");
    fs::write(&journal, text).unwrap();
    let (prediction, _) = scene.predicted(&log, "a torn step");
    assert_eq!(only(&prediction).1, Expected::RolledBack);
    assert!(prediction.diagnoses.journals[0].evidence.torn_tail);

    let other = TxId::new();
    fs::write(scene.paths.journal(&other), "{\"type\":\"prep").unwrap();
    let (prediction, _) = scene.predicted(&log, "a torn prepared line");
    assert_eq!(only(&prediction).1, Expected::Pruned);
}

#[test]
fn diagnose_reports_a_live_writer_as_skipped() {
    let log = tlog!();
    let scene = Scene::new();
    let inflight = InflightLock::create(&scene.paths, &scene.tx).unwrap();
    scene.run(1, false, false, false);
    let (prediction, result) = scene.predicted(&log, "its writer is alive");
    assert!(prediction.diagnoses.journals[0].live);
    assert_eq!(only(&prediction).1, Expected::Skipped);
    assert!(result.unwrap().is_empty());
    assert!(scene.paths.journal(&scene.tx).exists(), "untouched");
    drop(inflight);
    let (prediction, _) = scene.predicted(&log, "its writer is gone");
    assert_eq!(only(&prediction).1, Expected::RolledBack);
    assert_eq!(tree_hash(&scene.addons), scene.old);
}

#[test]
fn diagnose_predicts_each_stuck_state() {
    /// Rewrites each JSON line of `journal` with `edit`.
    fn rewrite(journal: &Path, edit: &dyn Fn(&mut serde_json::Value)) {
        let mut text = String::new();
        for line in fs::read_to_string(journal).unwrap().lines() {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            edit(&mut value);
            text.push_str(&value.to_string());
            text.push('\n');
        }
        fs::write(journal, text).unwrap();
    }

    type Plant = fn(&Scene);
    let cases: [(&str, Plant, JournalVerdict); 6] = [
        (
            "marker_mismatch",
            |scene| {
                let marker = omaddons::discover::registry::marker_path(&scene.target.client_dir);
                fs::write(marker, format!("{{\"key\":\"{}\"}}", TargetKey::new())).unwrap();
            },
            JournalVerdict::MarkerMismatch,
        ),
        (
            "step_unrecognized",
            |scene| {
                rewrite(&scene.paths.journal(&scene.tx), &|v| {
                    if v["type"] == "step" && v["from_id"].is_object() {
                        v["from_id"]["ino"] = 1u64.into();
                    }
                });
            },
            JournalVerdict::StepUnrecognized { step: 3 },
        ),
        (
            "too_new",
            |scene| {
                rewrite(&scene.paths.journal(&scene.tx), &|v| {
                    if v["type"] == "prepared" {
                        v["schema_version"] = "9.0".into();
                    }
                });
            },
            JournalVerdict::TooNew,
        ),
        (
            "unreadable",
            |scene| {
                let journal = scene.paths.journal(&scene.tx);
                let text = fs::read_to_string(&journal).unwrap();
                let first = text.find('\n').unwrap() + 1;
                fs::write(
                    &journal,
                    format!("{}not json\n{}", &text[..first], &text[first..]),
                )
                .unwrap();
            },
            JournalVerdict::Unreadable,
        ),
        (
            "unattributed_too_new",
            |scene| {
                rewrite(&scene.paths.journal(&scene.tx), &|v| {
                    if v["type"] == "prepared" {
                        v["schema_version"] = "9.0".into();
                        v["target_key"] = "garbage".into();
                    }
                });
            },
            JournalVerdict::Unattributed {
                cause: UnattributedCause::TooNew,
            },
        ),
        (
            "unattributed_unreadable",
            |scene| {
                rewrite(&scene.paths.journal(&scene.tx), &|v| {
                    if v["type"] == "prepared" {
                        v["kind"] = "teleport".into();
                        v["target_key"] = "garbage".into();
                    }
                });
            },
            JournalVerdict::Unattributed {
                cause: UnattributedCause::Unreadable,
            },
        ),
    ];
    let log = tlog!();
    for (name, plant, verdict) in cases {
        let scene = Scene::new();
        scene.run(3, false, false, false);
        plant(&scene);
        let (prediction, result) = scene.predicted(&log, name);
        assert_eq!(only(&prediction).0, verdict, "{name}");
        assert!(result.is_err(), "{name}: the caller's own target stops");
        assert!(scene.paths.journal(&scene.tx).exists(), "{name}: kept");
    }
}

#[test]
fn diagnose_and_recovery_agree_on_unreadable_inputs() {
    use std::os::unix::fs::PermissionsExt;

    let log = tlog!();
    for name in ["journal_file", "inflight_lock", "journal_dir"] {
        let scene = Scene::new();
        scene.run(2, false, false, false);
        let sealed = match name {
            "journal_file" => scene.paths.journal(&scene.tx),
            "inflight_lock" => {
                let lock = scene.paths.inflight_lock(&scene.tx);
                fs::create_dir_all(lock.parent().unwrap()).unwrap();
                fs::write(&lock, "").unwrap();
                lock
            }
            _ => scene.paths.journal_dir(),
        };
        let mode = fs::metadata(&sealed).unwrap().permissions().mode();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        log.step(name);
        let prediction = scene.predict(&log, name);
        let result = scene.recover_predicted(&log, &prediction);
        fs::set_permissions(&sealed, fs::Permissions::from_mode(mode)).unwrap();
        let error = result.unwrap_err();
        assert_eq!(
            (error.code.wire(), error.details["reason"].as_str()),
            ("io", Some("unattributed")),
            "{name}: {error:?}"
        );
        let want = match name {
            "journal_file" => Some(UnattributedCause::ReadFailed),
            "inflight_lock" => Some(UnattributedCause::LivenessUnknown),
            _ => None,
        };
        match want {
            Some(cause) => assert_eq!(
                only(&prediction).0,
                JournalVerdict::Unattributed { cause },
                "{name}"
            ),
            None => assert!(prediction.diagnoses.unlisted.is_some(), "{name}"),
        }
        // Once readable again, it rolls back as predicted.
        let (prediction, _) = scene.predicted(&log, &format!("{name}, readable again"));
        assert_eq!(only(&prediction).1, Expected::RolledBack, "{name}");
    }
}

#[test]
fn diagnose_after_recovery_finds_nothing() {
    let log = tlog!();
    for (executed, swapped) in [(3, false), (6, true)] {
        let scene = Scene::new();
        scene.run(executed, false, swapped, false);
        let (prediction, _) =
            scene.predicted(&log, &format!("{executed} steps, swapped: {swapped}"));
        let clients = [scene.target.client_dir.clone()];
        let after = omaddons::engine::journal::diagnose(&scene.paths, &clients, &|_| false);
        assert!(after.journals.is_empty(), "{after:#?}");
        assert!(!prediction.txs().is_empty());
    }
}

// ---------------------------------------------------------------------------------------
// The doctor's repairs (E5): `diagnose` on a home a crashed repair left, held to what the
// binary's recovery then does, for every repair crash state of every fix kind, every
// planted stop and a crashed recovery. Each repair runs in a child process of this test
// binary (`child_repair`), crashed by a real `abort`.

/// A child process of the repair scenes (`common::repair::child_main`). Run on its own
/// it does nothing.
#[cfg(debug_assertions)]
#[test]
#[ignore = "a child process of the repair scenes"]
fn child_repair() {
    common::repair::child_main();
}

#[cfg(test)]
#[cfg(debug_assertions)]
mod repair_support {
    use std::fs;
    use std::path::Path;

    use super::common::differential::{Caller, Differential};
    use super::common::repair::RepairScene;
    use super::common::tlog::TestLog;

    /// Rewrites the scene's repair journal through `edit`.
    pub fn edit_repair_journal(r: &RepairScene, edit: impl FnOnce(String) -> String) {
        let dir = r.s.home.state().join("journal");
        let journal = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with(".repair.jsonl"))
            .unwrap();
        let text = fs::read_to_string(&journal).unwrap();
        fs::write(&journal, edit(text)).unwrap();
    }

    pub fn set_mode(path: &Path, bits: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(bits)).unwrap();
    }

    /// `omaddons <args> --json --progress json`.
    pub fn json_args(args: &[&str]) -> Vec<String> {
        args.iter()
            .copied()
            .chain(["--json", "--progress", "json"])
            .map(str::to_owned)
            .collect()
    }

    /// `config set check_interval_hours 6`: recovery under the lock, committing nothing.
    pub fn config_set() -> Vec<String> {
        json_args(&["config", "set", "check_interval_hours", "6"])
    }

    /// The install of Foo v2 from its archive: a command that commits retail.
    pub fn install_v2(r: &RepairScene) -> Vec<String> {
        let mut argv = r.s.install_args("gh:o/foo", "v2");
        argv.extend(["--progress".to_owned(), "json".to_owned()]);
        argv
    }

    /// Who recovers when `argv` runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum By {
        /// A command that commits retail (`install`).
        Commit,
        /// A command that recovers under the lock alone (`config set`).
        Under,
    }

    /// Predicts `r` as it is now (`label`), runs `argv`, and holds the prediction to
    /// what its recovery did. Returns how many journals it compared.
    #[track_caller]
    pub fn predicted(
        log: &TestLog,
        r: &RepairScene,
        label: &str,
        argv: &[String],
        by: By,
    ) -> usize {
        let mut differential = Differential::new(log, &r.s.home, &["_retail_"], &[]);
        let caller = match by {
            By::Commit => {
                differential.predict(log, label);
                differential.commit(&["retail"])
            }
            By::Under => {
                differential.predict_unknown(log, label);
                Caller::Under
            }
        };
        let ran = log.run(&r.s, argv);
        ran.assert_private();
        let envelope = ran.envelope().clone();
        differential.compare(log, &envelope, &r.s.journals(), &caller)
    }
}

#[cfg(debug_assertions)]
#[test]
fn diagnose_predicts_recovery_of_every_repair_crash_state() {
    use std::collections::BTreeMap;

    use common::repair::{Kind, RepairScene, checked_sweep, crash_specs};
    use repair_support::{By, config_set, predicted};

    let log = tlog!();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for kind in Kind::ALL {
        let mut compared = 0;
        let mut crashed: Vec<(String, RepairScene)> = Vec::new();
        for spec in crash_specs(kind) {
            let label = format!("{} at {spec}", kind.label());
            log.step(&label);
            let r = RepairScene::new(kind);
            r.child(&log, &spec);
            crashed.push((label, r));
        }
        crashed.extend(
            checked_sweep(|| RepairScene::new(kind), &log)
                .into_iter()
                .map(|(n, r)| {
                    (
                        format!("{} at fsx.mutate.checked=abort@{n}", kind.label()),
                        r,
                    )
                }),
        );
        for (label, r) in crashed {
            log.step(&format!("{label}: recover"));
            log.json("journal at crash", &r.journal_lines());
            log.json("actions at crash", &r.actions_lines());
            compared += predicted(&log, &r, &label, &config_set(), By::Under);
        }
        counts.insert(kind.label(), compared);
        let least = 5 + 4 * kind.primitives() as usize;
        assert!(
            compared >= least,
            "{}: {compared} states compared, at least {least} expected",
            kind.label()
        );
    }
    log.json("states compared", &counts);
}

#[cfg(debug_assertions)]
#[test]
fn diagnose_predicts_every_planted_repair_stop() {
    use common::repair::{Child, Kind, RepairScene};
    use repair_support::{By, config_set, edit_repair_journal, install_v2, predicted, set_mode};

    /// A crashed repair, then the plant that makes it stop (or makes its writer live).
    type Plant = fn(&RepairScene) -> Option<fs::File>;
    let neither_mode: Plant = |r| {
        let credential = r.s.home.env.config().join("omaddons/credentials/github");
        set_mode(&credential, 0o640);
        None
    };
    let neither_moved: Plant = |r| {
        fs::rename(
            r.s.addons().join("Inner"),
            r.s.home.env.root().join("elsewhere"),
        )
        .unwrap();
        None
    };
    let marker_changed: Plant = |r| {
        let marker =
            r.s.home
                .client("_retail_")
                .join("Interface/.omaddons/target.json");
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&marker).unwrap()).unwrap();
        value["key"] = TargetKey::new().to_string().into();
        fs::write(&marker, value.to_string()).unwrap();
        None
    };
    let unknown_skill_dir: Plant = |r| {
        let credential = r.s.home.env.config().join("omaddons/credentials/github");
        let elsewhere = r.s.home.env.home().join("elsewhere/SKILL.md");
        edit_repair_journal(r, |text| {
            text.replace(
                &credential.display().to_string(),
                &elsewhere.display().to_string(),
            )
        });
        None
    };
    let unreadable: Plant = |r| {
        edit_repair_journal(r, |text| {
            let (first, rest) = text.split_once('\n').unwrap();
            format!("{first}\nnot json\n{rest}")
        });
        None
    };
    let too_new: Plant = |r| {
        edit_repair_journal(r, |text| {
            text.replacen("\"format\":\"1.0\"", "\"format\":\"9.0\"", 1)
        });
        None
    };
    let live: Plant = |r| {
        let tx = r.tx().unwrap();
        let lock = r.s.home.state().join(format!("inflight/{tx}.lock"));
        let holder = fs::File::open(lock).unwrap();
        holder.lock().unwrap();
        Some(holder)
    };
    let chmod_ran = (Kind::Chmod, "fsx.mutate.executed=abort@1");
    let flatten_ran = (Kind::FlattenTwo, "fsx.mutate.executed=abort@1");
    let chmod_stepped = (Kind::Chmod, "doctor.repair.step=abort@1");
    let cases: Vec<(&str, (Kind, &str), Plant)> = vec![
        ("neither_state_chmod", chmod_ran, neither_mode),
        ("neither_state_flatten", flatten_ran, neither_moved),
        ("marker_changed", flatten_ran, marker_changed),
        ("unknown_skill_dir", chmod_stepped, unknown_skill_dir),
        ("unreadable", chmod_stepped, unreadable),
        ("too_new", chmod_stepped, too_new),
        ("live", flatten_ran, live),
    ];
    let log = tlog!();
    for (case, (kind, spec), plant) in cases {
        for by in [By::Commit, By::Under] {
            let label = format!("{case} ({by:?})");
            log.step(&label);
            let r = RepairScene::new(kind);
            assert_eq!(r.child(&log, spec), Child::Crashed, "{label}");
            log.note(format!("plant: {case}"));
            let _held = plant(&r);
            log.json("journal planted", &r.journal_lines());
            let argv = match by {
                By::Commit => install_v2(&r),
                By::Under => config_set(),
            };
            let compared = predicted(&log, &r, &label, &argv, by);
            assert_eq!(compared, 1, "{label}: the repair was compared");
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_crashed_repair_recovery_is_predicted_again() {
    use common::differential::{Differential, assert_settled};
    use common::repair::{Child, End, Kind, RepairScene};
    use repair_support::{By, config_set, predicted};

    let log = tlog!();
    for kind in Kind::ALL {
        let label = kind.label();
        log.step(&format!("{label}: crash with every step run"));
        let r = RepairScene::new(kind);
        let spec = format!("fsx.mutate.executed=abort@{}", kind.primitives());
        assert_eq!(r.child(&log, &spec), Child::Crashed, "{label}");
        let tx: TxId = r.tx().unwrap().parse().unwrap();
        let mut differential = Differential::new(&log, &r.s.home, &["_retail_"], &[]);
        differential.predict_unknown(&log, &format!("{label}: before recovery"));
        log.step(&format!("{label}: recovery crashed at its first inverse"));
        let crashed = log
            .cmd(&r.s, &config_set())
            .failpoints("doctor.recover.step=abort@1")
            .run();
        crashed.aborted();
        log.step(&format!("{label}: predicted again, then finished"));
        let compared = predicted(
            &log,
            &r,
            &format!("{label}: again"),
            &config_set(),
            By::Under,
        );
        assert_eq!(compared, 1, "{label}");
        assert_eq!(r.accounted(End::Before), Vec::<String>::new(), "{label}");
        let prediction = differential.prediction.as_ref().unwrap();
        assert!(prediction.txs().contains(&tx), "{label}");
        assert_settled(&log, prediction, &differential.paths, &differential.clients);
    }
}
