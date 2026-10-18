//! The cross-process crash matrix (vision §7, M1 checkpoints): every engine failpoint
//! as a real crash (`abort`: no unwinding, the kernel releases the flocks), for a new
//! install, a first install into a client never launched (no `Interface/`), an update
//! that drops a folder, and a removal. After each crash every folder
//! is wholly old or wholly new; a reader reports the unfinished transaction and changes
//! nothing; the next command that takes the lock recovers it (`journal_recovered`),
//! after which all folders sit on one side, consistent with the record generation and
//! history, and the lock file is the same inode.
//!
//! Records-only transactions (a pin, a forget) change no folder, so their oracle is the
//! record: the edit is there exactly when history holds the transaction, the generation
//! agrees, and every folder is byte-identical to before.

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::engine::{Scene, shapes, warning_codes};
use omaddons::engine::FAILPOINTS;
use support::{Op, RecordsOp, Shape, expected_new, matrix, record_matrix, shape_matrix};

#[cfg(test)]
mod support {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    pub enum Op {
        Install,
        /// An install into a client never launched: no `Interface/` at all.
        FirstInstall,
        Update,
        Remove,
    }

    impl Op {
        pub fn kind(self) -> &'static str {
            match self {
                Self::Install | Self::FirstInstall => "install",
                Self::Update => "update",
                Self::Remove => "remove",
            }
        }

        /// Sets up the baseline and returns the command.
        pub fn prepare(self, s: &Scene) -> Vec<String> {
            match self {
                Self::Install => s.install_args("gh:o/foo", "v1"),
                Self::FirstInstall => {
                    fs::remove_dir_all(s.home.client("_retail_").join("Interface")).unwrap();
                    s.install_args("gh:o/foo", "v1")
                }
                Self::Update => {
                    s.install("gh:o/foo", "v1");
                    s.install_args("gh:o/foo", "v2")
                }
                Self::Remove => {
                    s.install("gh:o/foo", "v1");
                    ["remove", "Foo", "-t", "retail", "--offline", "--json"]
                        .iter()
                        .map(|a| (*a).to_owned())
                        .collect()
                }
            }
        }
    }

    /// The folders after `op` runs without a failpoint.
    pub fn expected_new(op: Op) -> BTreeMap<String, Option<String>> {
        let s = Scene::new();
        let args = op.prepare(&s);
        s.run(&args, "").expect(0);
        s.folders()
    }

    /// Journals that never reached `committed`.
    pub fn pending(s: &Scene) -> usize {
        s.journals()
            .iter()
            .filter(|p| {
                !fs::read_to_string(p)
                    .unwrap_or_default()
                    .contains("\"type\":\"committed\"")
            })
            .count()
    }

    /// Every failpoint as a crash of `op`.
    pub fn matrix(op: Op) {
        let new = expected_new(op);
        let mut crashed_at = Vec::new();
        for failpoint in FAILPOINTS {
            let s = Scene::new();
            let args = op.prepare(&s);
            let old = s.folders();
            let (old_generation, old_history, old_inode) =
                (s.generation(), s.history(), s.lock_inode());
            let r = s.run(&args, &format!("{failpoint}=abort"));
            let context = |what: &str| {
                format!(
                    "{op:?} at {failpoint}: {what}\nold {old:?}\nnew {new:?}\nnow {:?}\njournals {:?}\n{}",
                    s.folders(),
                    s.journals()
                        .iter()
                        .map(|p| fs::read_to_string(p).unwrap_or_default())
                        .collect::<Vec<_>>(),
                    r.context()
                )
            };
            if r.code == -1 {
                crashed_at.push(*failpoint);
            } else {
                assert_eq!(
                    r.code,
                    0,
                    "{}",
                    context("not on this path, so it must succeed")
                );
            }

            // Each folder, on its own, is wholly old or wholly new at every instant.
            for (folder, now) in s.folders() {
                assert!(
                    now == old[&folder] || now == new[&folder],
                    "{}",
                    context(&format!("{folder} is neither old nor new"))
                );
            }

            // A reader reports the unfinished transaction and changes nothing.
            let pending_before = pending(&s);
            let journals = s.journals();
            let before = s.snapshot();
            let status = s
                .run(&["status".to_owned(), "--json".to_owned()], "")
                .expect(0);
            let codes = warning_codes(&status);
            assert_eq!(
                codes.iter().any(|c| c == "journal_pending"),
                pending_before > 0,
                "{}",
                context(&format!("status warnings {codes:?}"))
            );
            assert_eq!(s.snapshot(), before, "{}", context("status wrote"));
            assert_eq!(s.journals(), journals, "{}", context("status recovered"));

            // The next command that takes the lock recovers.
            let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
            assert_eq!(
                warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
                pending_before > 0,
                "{}",
                context("recovery warning")
            );
            assert!(
                s.journals().is_empty(),
                "{}",
                context("journals left after recovery")
            );
            // AddOns holds whole addon folders only: nothing staged or half-moved.
            let mut stray: Vec<String> = fs::read_dir(s.addons())
                .map(|l| {
                    l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            stray.retain(|n| !["Foo", "FooOld", "Bar"].contains(&n.as_str()));
            assert!(
                stray.is_empty(),
                "{}",
                context(&format!("stray AddOns entries {stray:?}"))
            );

            // All folders on one side, consistent with state.
            let now = s.folders();
            let went_new = now == new;
            assert!(
                went_new || now == old,
                "{}",
                context("folders split across sides")
            );
            let commits = u64::from(went_new) + 1;
            assert_eq!(
                s.generation(),
                old_generation + commits,
                "{}",
                context("generation")
            );
            let mut history = old_history.clone();
            if went_new {
                history.push(op.kind().to_owned());
            }
            history.push("install".to_owned());
            assert_eq!(s.history(), history, "{}", context("history"));
            if let Some(inode) = old_inode {
                assert_eq!(
                    s.lock_inode(),
                    Some(inode),
                    "{}",
                    context("lock file replaced")
                );
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "{op:?}: crashed at {} of {} failpoints: {crashed_at:?}\n",
                crashed_at.len(),
                FAILPOINTS.len()
            ),
        );
        assert!(
            crashed_at.len() >= 10,
            "{op:?}: most failpoints are on the path: {crashed_at:?}"
        );
    }

    /// An M2 transaction shape (H2), from release fixtures through the real CLI.
    #[derive(Debug, Clone, Copy)]
    pub enum Shape {
        /// `update` of three addons.
        UpdateMany,
        /// `install` with a dependency round and a `required_by` edit.
        InstallWithDep,
        /// `sync --prune`: an install, a remove and an unpin.
        SyncMixed,
        /// `update` in which one addon's folder passes to another.
        Handover,
        /// `adopt --apply`: records only.
        Adopt,
    }

    impl Shape {
        pub fn kind(self) -> &'static str {
            match self {
                Self::UpdateMany | Self::Handover => "update",
                Self::InstallWithDep => "install",
                Self::SyncMixed => "sync",
                Self::Adopt => "adopt",
            }
        }

        pub fn folders(self) -> &'static [&'static str] {
            match self {
                Self::UpdateMany => shapes::UPDATE_MANY,
                Self::InstallWithDep => shapes::INSTALL_WITH_DEP,
                Self::SyncMixed => shapes::SYNC_MIXED,
                Self::Handover => shapes::HANDOVER,
                Self::Adopt => shapes::ADOPT,
            }
        }

        pub fn prepare(self, s: &Scene) -> Vec<String> {
            match self {
                Self::UpdateMany => shapes::update_many(s),
                Self::InstallWithDep => shapes::install_with_dep(s),
                Self::SyncMixed => shapes::sync_mixed(s),
                Self::Handover => shapes::handover(s),
                Self::Adopt => shapes::adopt(s),
            }
        }
    }

    /// What a shape is judged by: its folders, every record (volatile fields aside) and
    /// the files index, Bar's (the recovery's) left out.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Side {
        folders: BTreeMap<String, Option<String>>,
        records: BTreeMap<String, serde_json::Value>,
        indexed: Vec<String>,
    }

    impl Side {
        pub fn of(s: &Scene, shape: Shape) -> Self {
            let mut indexed = s.indexed();
            indexed.retain(|k| k != "bar");
            Self {
                folders: s.folders_of(shape.folders()),
                records: s.view(),
                indexed,
            }
        }
    }

    /// The shape after it ran without a failpoint.
    pub fn reference(shape: Shape) -> Side {
        let s = Scene::new();
        let args = shape.prepare(&s);
        s.run(&args, "").expect(0);
        Side::of(&s, shape)
    }

    /// Every failpoint as a crash of `shape`. Each folder is wholly old or wholly new at
    /// every instant; a reader reports and writes nothing; the next command recovers;
    /// then folders, records and the files index sit together on one side, the side
    /// history says (a mix of new folders and old records fails), every record's folders
    /// exist, and the generation and lock inode agree. Records-only shapes never move a
    /// folder.
    pub fn shape_matrix(shape: Shape) {
        let new = reference(shape);
        let mut crashed_at = Vec::new();
        for failpoint in FAILPOINTS {
            let s = Scene::new();
            let args = shape.prepare(&s);
            let old = Side::of(&s, shape);
            assert_ne!(old, new, "{shape:?}: the reference run changed nothing");
            let (old_generation, old_history, old_inode) =
                (s.generation(), s.history(), s.lock_inode());
            let r = s.run(&args, &format!("{failpoint}=abort"));
            let context = |what: &str| {
                format!(
                    "{shape:?} at {failpoint}: {what}\nold {old:?}\nnew {new:?}\nnow {:?}\nhistory {:?}\njournals {:?}\n{}",
                    Side::of(&s, shape),
                    s.history_ops(),
                    s.journals()
                        .iter()
                        .map(|p| fs::read_to_string(p).unwrap_or_default())
                        .collect::<Vec<_>>(),
                    r.context()
                )
            };
            if r.code == -1 {
                crashed_at.push(*failpoint);
            } else {
                assert_eq!(r.code, 0, "{}", context("off the path, so it must succeed"));
            }
            for (folder, now) in s.folders_of(shape.folders()) {
                assert!(
                    now == old.folders[&folder] || now == new.folders[&folder],
                    "{}",
                    context(&format!("{folder} is neither old nor new"))
                );
            }

            // A reader reports the unfinished transaction and changes nothing.
            let pending_before = pending(&s);
            let journals = s.journals();
            let before = s.snapshot();
            let status = s
                .run(&["status".to_owned(), "--json".to_owned()], "")
                .expect(0);
            assert_eq!(
                warning_codes(&status)
                    .iter()
                    .any(|c| c == "journal_pending"),
                pending_before > 0,
                "{}",
                context("status warnings")
            );
            assert_eq!(s.snapshot(), before, "{}", context("status wrote"));
            assert_eq!(s.journals(), journals, "{}", context("status recovered"));

            // The next command that takes the lock recovers.
            let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
            assert_eq!(
                warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
                pending_before > 0,
                "{}",
                context("recovery warning")
            );
            assert!(s.journals().is_empty(), "{}", context("journals left"));
            let allowed: Vec<&str> = shape.folders().iter().copied().chain(["Bar"]).collect();
            let stray = s.stray(&allowed);
            assert!(stray.is_empty(), "{}", context(&format!("stray {stray:?}")));

            // One side for folders, records and index together, the one history says.
            let history = s.history();
            let committed = history.len() == old_history.len() + 2
                && history.get(old_history.len()).map(String::as_str) == Some(shape.kind());
            let mut expected = old_history.clone();
            if committed {
                expected.push(shape.kind().to_owned());
            }
            expected.push("install".to_owned());
            assert_eq!(history, expected, "{}", context("history"));
            let now = Side::of(&s, shape);
            let side = if committed { &new } else { &old };
            assert_eq!(&now, side, "{}", context("not one consistent side"));
            assert_eq!(
                s.generation(),
                old_generation + u64::from(committed) + 1,
                "{}",
                context("generation")
            );
            for (name, record) in s.view() {
                for folder in record["folders"].as_array().into_iter().flatten() {
                    let folder = folder.as_str().unwrap_or_default();
                    assert!(
                        s.addons().join(folder).is_dir(),
                        "{}",
                        context(&format!("{name} records {folder}, which is gone"))
                    );
                }
            }
            if let Some(inode) = old_inode {
                assert_eq!(s.lock_inode(), Some(inode), "{}", context("lock replaced"));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "{shape:?}: crashed at {} of {} failpoints: {crashed_at:?}\n",
                crashed_at.len(),
                FAILPOINTS.len()
            ),
        );
        assert!(
            crashed_at.len() >= 10,
            "{shape:?}: most failpoints are on the path: {crashed_at:?}"
        );
    }

    /// A records-only transaction.
    #[derive(Debug, Clone, Copy)]
    pub enum RecordsOp {
        /// Foo v1 installed, then pinned.
        Pin,
        /// Foo v1 installed, its folders deleted by hand, then removed: forgotten.
        Forget,
    }

    impl RecordsOp {
        pub fn kind(self) -> &'static str {
            match self {
                Self::Pin => "pin",
                Self::Forget => "remove",
            }
        }

        /// Sets up the baseline and returns the command.
        pub fn prepare(self, s: &Scene) -> Vec<String> {
            s.install("gh:o/foo", "v1");
            match self {
                Self::Pin => s.args(&["pin", "Foo"]),
                Self::Forget => {
                    for folder in ["Foo", "FooOld"] {
                        fs::remove_dir_all(s.addons().join(folder)).unwrap();
                    }
                    s.args(&["remove", "Foo"])
                }
            }
        }

        /// Whether the transaction's edit is in the record, given Foo's record `before`.
        pub fn edited(self, s: &Scene, before: &serde_json::Value) -> bool {
            let foo = s.record("foo");
            match self {
                Self::Pin => foo["pinned"] == true && foo["release_id"] == before["release_id"],
                Self::Forget => {
                    let indexed = s.indexed();
                    foo.is_null() && !indexed.iter().any(|k| k == "foo" || k == "fooold")
                }
            }
        }
    }

    /// Every failpoint as a crash of the records-only `op`, judged by the record oracle.
    pub fn record_matrix(op: RecordsOp) {
        let mut crashed_at = Vec::new();
        for failpoint in FAILPOINTS {
            let s = Scene::new();
            let args = op.prepare(&s);
            let old = s.folders();
            let (old_generation, old_history, old_inode) =
                (s.generation(), s.history(), s.lock_inode());
            let old_record = s.record("foo");
            let r = s.run(&args, &format!("{failpoint}=abort"));
            let context = |what: &str| {
                format!(
                    "{op:?} at {failpoint}: {what}\nrecord {}\nhistory {:?}\njournals {:?}\n{}",
                    s.record("foo"),
                    s.history_ops(),
                    s.journals()
                        .iter()
                        .map(|p| fs::read_to_string(p).unwrap_or_default())
                        .collect::<Vec<_>>(),
                    r.context()
                )
            };
            if r.code == -1 {
                crashed_at.push(*failpoint);
            } else {
                assert_eq!(r.code, 0, "{}", context("off the path, so it must succeed"));
            }
            assert_eq!(
                s.folders(),
                old,
                "{}",
                context("a records-only crash moved a folder")
            );

            // A reader reports the unfinished transaction and changes nothing.
            let pending_before = pending(&s);
            let journals = s.journals();
            let before = s.snapshot();
            let status = s
                .run(&["status".to_owned(), "--json".to_owned()], "")
                .expect(0);
            assert_eq!(
                warning_codes(&status)
                    .iter()
                    .any(|c| c == "journal_pending"),
                pending_before > 0,
                "{}",
                context("status warnings")
            );
            assert_eq!(s.snapshot(), before, "{}", context("status wrote"));
            assert_eq!(s.journals(), journals, "{}", context("status recovered"));

            // The next command that takes the lock recovers.
            let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
            assert_eq!(
                warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
                pending_before > 0,
                "{}",
                context("recovery warning")
            );
            assert!(s.journals().is_empty(), "{}", context("journals left"));
            let stray = s.stray(&["Foo", "FooOld", "Bar"]);
            assert!(stray.is_empty(), "{}", context(&format!("stray {stray:?}")));

            // The record oracle: the edit is there exactly when history holds it.
            let history = s.history();
            let committed = history.len() == old_history.len() + 2
                && history.get(old_history.len()).map(String::as_str) == Some(op.kind());
            let mut expected = old_history.clone();
            if committed {
                expected.push(op.kind().to_owned());
            }
            expected.push("install".to_owned());
            assert_eq!(history, expected, "{}", context("history"));
            assert_eq!(
                op.edited(&s, &old_record),
                committed,
                "{}",
                context("edit present iff committed")
            );
            assert_eq!(
                s.generation(),
                old_generation + u64::from(committed) + 1,
                "{}",
                context("generation")
            );
            assert_eq!(s.folders(), old, "{}", context("folders after recovery"));
            if committed {
                let (_, ops) = &s.history_ops()[old_history.len()];
                assert!(
                    !ops.is_empty() && ops.iter().all(|(o, _)| o == "record"),
                    "{}",
                    context("a records-only line holds record ops only")
                );
            }
            if let Some(inode) = old_inode {
                assert_eq!(s.lock_inode(), Some(inode), "{}", context("lock replaced"));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "{op:?}: crashed at {} of {} failpoints: {crashed_at:?}\n",
                crashed_at.len(),
                FAILPOINTS.len()
            ),
        );
        assert!(
            crashed_at.len() >= 10,
            "{op:?}: most failpoints are on the path: {crashed_at:?}"
        );
    }
}

#[test]
fn crash_matrix_install() {
    matrix(Op::Install);
}

#[test]
fn crash_matrix_first_install() {
    matrix(Op::FirstInstall);
}

#[test]
fn crash_matrix_update() {
    matrix(Op::Update);
}

#[test]
fn crash_matrix_remove() {
    matrix(Op::Remove);
}

#[test]
fn crash_matrix_pin() {
    record_matrix(RecordsOp::Pin);
}

#[test]
fn crash_matrix_update_many() {
    shape_matrix(Shape::UpdateMany);
}

#[test]
fn crash_matrix_install_with_dependencies() {
    shape_matrix(Shape::InstallWithDep);
}

#[test]
fn crash_matrix_sync_mixed() {
    shape_matrix(Shape::SyncMixed);
}

#[test]
fn crash_matrix_handover() {
    shape_matrix(Shape::Handover);
}

#[test]
fn crash_matrix_adopt() {
    shape_matrix(Shape::Adopt);
}

#[test]
fn the_m2_reference_runs_make_the_change_each_shape_names() {
    // InstallWithDep: Big as a dependency of Little, and Lib gained Little.
    let s = Scene::new();
    let args = Shape::InstallWithDep.prepare(&s);
    s.run(&args, "").expect(0);
    let records = s.view();
    assert_eq!(records["big"]["reason"], "dependency");
    assert_eq!(records["big"]["required_by"], serde_json::json!(["Little"]));
    assert_eq!(records["lib"]["required_by"], serde_json::json!(["Little"]));
    // Handover: Alpha owns Shared, nothing is degraded.
    let s = Scene::new();
    let args = Shape::Handover.prepare(&s);
    s.run(&args, "").expect(0);
    let records = s.view();
    let owners: Vec<&String> = records
        .iter()
        .filter(|(_, r)| {
            r["folders"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x == "Shared"))
        })
        .map(|(k, _)| k)
        .collect();
    assert_eq!(owners, ["alpha"]);
    assert!(records.values().all(|r| r["state"] == "ok"), "{records:?}");
    // SyncMixed: Kilo in, Lima out, Mike unpinned.
    let s = Scene::new();
    let args = Shape::SyncMixed.prepare(&s);
    s.run(&args, "").expect(0);
    let records = s.view();
    assert!(records.contains_key("kilo") && !records.contains_key("lima"));
    assert_eq!(records["mike"]["pinned"], false);
    // UpdateMany: three at v2. Adopt: two records, no folder moved.
    let s = Scene::new();
    let args = Shape::UpdateMany.prepare(&s);
    s.run(&args, "").expect(0);
    assert!(s.view().values().all(|r| r["version"] == "v2"));
    let s = Scene::new();
    let args = Shape::Adopt.prepare(&s);
    let before = s.folders_of(shapes::ADOPT);
    s.run(&args, "").expect(0);
    assert_eq!(s.view().len(), 2);
    assert_eq!(s.folders_of(shapes::ADOPT), before);
}

#[test]
fn crash_matrix_forget() {
    record_matrix(RecordsOp::Forget);
}

#[test]
fn the_reference_runs_change_what_they_should() {
    // Records-only: the edit, and no folder moved.
    for op in [RecordsOp::Pin, RecordsOp::Forget] {
        let s = Scene::new();
        let args = op.prepare(&s);
        let (folders, before) = (s.folders(), s.record("foo"));
        s.run(&args, "").expect(0);
        assert!(op.edited(&s, &before), "{op:?}: {}", s.record("foo"));
        assert_eq!(s.folders(), folders, "{op:?}");
    }

    let install = expected_new(Op::Install);
    let first = expected_new(Op::FirstInstall);
    let update = expected_new(Op::Update);
    let remove = expected_new(Op::Remove);
    assert!(install["Foo"].is_some() && install["FooOld"].is_some());
    assert_eq!(
        first, install,
        "a first install creates AddOns and installs"
    );
    assert!(
        update["Foo"].is_some() && update["FooOld"].is_none(),
        "the update drops a folder"
    );
    assert_ne!(install["Foo"], update["Foo"]);
    assert!(remove.values().all(Option::is_none));
}

/// An undo crashed at each failpoint after its swap, then recovered by a change made
/// while the client runs. Recovery finishes folders, record and history, and never
/// writes SavedVariables under the running client: a restore the crash cut off waits
/// (`sv_deferred`, `saved_variables_deferred`); one done before the crash stays done.
#[test]
fn crash_matrix_undo_recovered_while_the_client_runs() {
    const SV: &str = "WTF/Account/ACCT/SavedVariables/Foo.lua";
    // Each failpoint after `swapped`, whether the restore had run before it, and whether
    // the journal was left unfinished (a committed one is pruned silently).
    let points = [
        ("engine.swapped", false, true),
        ("engine.record.written", true, true),
        ("engine.history.appended", true, true),
        ("engine.committed", true, false),
    ];
    for (failpoint, restored, unfinished) in points {
        let mut s = Scene::new();
        let sv = s.home.client("_retail_").join(SV);
        fs::create_dir_all(sv.parent().unwrap()).unwrap();
        fs::write(&sv, "-- before").unwrap();
        let empty = s.folders();
        s.install("gh:o/foo", "v1");
        fs::write(&sv, "-- after").unwrap();

        let undo: Vec<String> = ["undo", "--offline", "--json"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let crashed = s.run(&undo, &format!("{failpoint}=abort"));
        assert_eq!(crashed.code, -1, "{failpoint}: {}", crashed.context());

        s.home.retail_running();
        let r = s.run(&s.install_args("gh:o/bar", "bar"), "");
        let envelope = r.expect(0);
        let codes = warning_codes(&envelope);
        let context = format!("{failpoint}\n{}", r.context());
        assert_eq!(
            codes.iter().any(|c| c == "journal_recovered"),
            unfinished,
            "{context}"
        );
        assert_eq!(
            codes.iter().any(|c| c == "saved_variables_deferred"),
            !restored,
            "{context}"
        );
        let want = if restored { "-- before" } else { "-- after" };
        assert_eq!(fs::read_to_string(&sv).unwrap(), want, "{context}");
        assert_eq!(s.folders(), empty, "{context}: the undo's folders stand");
        assert!(s.journals().is_empty(), "{context}");
        let line = fs::read_to_string(s.home.state().join("history.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["kind"] == "undo")
            .unwrap();
        assert_eq!(
            line["sv_deferred"].as_bool().unwrap_or(false),
            !restored,
            "{context}: {line}"
        );
    }
}

/// A commit that fails right after its exchange, whose rollback fails too (the third
/// rename, the exchange back, is refused): the old folder exists only in the stage dir.
/// The command fails naming the kept journal, and the next change command rolls it back
/// byte for byte: a local edit, and an unmanaged folder replaced with `--force`, both
/// come back.
#[test]
fn a_failed_recovery_is_finished_by_the_next_command() {
    use common::archives::zip_text;

    const DOUBLE_FAULT: &str = "engine.swap.after_exchange=error,fsx.rename_with.before=error@3";
    for force in [false, true] {
        let s = Scene::new();
        let foo = s.addons().join("Foo");
        let zip = |name: &str, entries: &[(&str, &str)]| {
            fs::write(s.dl.join(format!("{name}.zip")), zip_text(entries)).unwrap();
        };
        // Foo 2 with a new folder Bar: Bar goes in (rename 1), Foo is exchanged (2).
        zip(
            "v2bar",
            &[
                ("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n"),
                ("Foo/core.lua", "-- v2\n"),
                ("Bar/Bar.toc", "## Interface: 120105\n"),
            ],
        );
        zip("baz", &[("Baz/Baz.toc", "## Interface: 120105\n")]);
        let mut args = if force {
            // Someone's own Foo, unmanaged, replaced with --force.
            fs::create_dir_all(&foo).unwrap();
            fs::write(foo.join("Foo.toc"), "## Interface: 120105\n").unwrap();
            fs::write(foo.join("mine.lua"), "-- MINE\n").unwrap();
            let mut args = s.install_args("gh:o/foo", "v2bar");
            args.push("--force".to_owned());
            args
        } else {
            s.install("gh:o/foo", "v1");
            let mut core = fs::read_to_string(foo.join("core.lua")).unwrap();
            core.push_str("-- MY LOCAL EDIT\n");
            fs::write(foo.join("core.lua"), core).unwrap();
            s.install_args("gh:o/foo", "v2bar")
        };
        let before = common::trees::tree_hash(&foo);
        let generation = s.generation();

        let r = s.run(&args, DOUBLE_FAULT);
        let context = format!("force: {force}\n{}", r.context());
        let envelope = r.expect(1);
        let error = &envelope["error"];
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("recovering it failed too"),
            "{context}"
        );
        assert_eq!(error["fix_argv"], serde_json::json!(["omaddons", "status"]));
        assert!(
            error["details"]["recovery_error"]["code"].is_string(),
            "{context}"
        );
        let tx = error["details"]["journal_pending"].as_str().unwrap();
        assert_eq!(s.journals().len(), 1, "{context}");
        let stage = s
            .home
            .client("_retail_")
            .join("Interface/.omaddons/stage")
            .join(tx);
        assert_eq!(
            common::trees::tree_hash(&stage.join("Foo")),
            before,
            "{context}: the old Foo is kept in the stage dir"
        );
        let status = s
            .run(&["status".to_owned(), "--json".to_owned()], "")
            .expect(0);
        assert!(
            warning_codes(&status)
                .iter()
                .any(|c| c == "journal_pending"),
            "{context}"
        );

        // The next change recovers first: everything is as before the failed install.
        args = s.install_args("gh:o/baz", "baz");
        args.push("--force".to_owned());
        let r = s.run(&args, "");
        let envelope = r.expect(0);
        assert!(
            warning_codes(&envelope)
                .iter()
                .any(|c| c == "journal_recovered"),
            "{context}\n{}",
            r.context()
        );
        assert_eq!(common::trees::tree_hash(&foo), before, "{context}");
        assert!(!s.addons().join("Bar").exists(), "{context}");
        assert!(s.addons().join("Baz").is_dir(), "{context}");
        assert!(s.journals().is_empty() && !stage.exists(), "{context}");
        assert_eq!(
            s.generation(),
            generation + 1,
            "{context}: only Baz committed"
        );
    }
}

/// One client's leftover journal never blocks another client: a change on classic
/// recovers retail's interrupted transaction (forward from `swapped`, back from before)
/// and goes ahead; a retail journal recovery cannot finish only warns there, while
/// retail's own commands keep failing until it is repaired.
#[test]
fn another_clients_journal_is_recovered_or_reported_never_blocking() {
    use common::archives::zip_text;
    use common::home::Home;
    use serde_json::Value;

    // (failpoint, Foo's version on retail after recovery, whether recovery can finish)
    for (failpoint, foo, recoverable) in [
        ("engine.swapped=abort", "2", true),
        ("engine.swap.after_exchange=abort", "1", true),
        ("engine.swap.after_exchange=abort", "", false),
    ] {
        let home = Home::new(&["wow", "wow_classic"]);
        let dl = home.env.root().join("dl");
        fs::create_dir_all(&dl).unwrap();
        let zip = |name: &str, entries: &[(&str, &str)]| {
            let path = dl.join(format!("{name}.zip"));
            fs::write(&path, zip_text(entries)).unwrap();
            path.display().to_string()
        };
        let foo1 = zip(
            "foo1",
            &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n")],
        );
        let foo2 = zip(
            "foo2",
            &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n")],
        );
        let baz = zip("baz", &[("Baz/Baz.toc", "## Interface: 50504\n")]);
        let install = |file: &str, r: &str, target: &str, extra: &[(&str, &str)]| {
            let argv: Vec<String> = ["install", r, "--file", file, "-t", target, "--json"]
                .iter()
                .map(|a| (*a).to_owned())
                .collect();
            home.exec_with(&argv, extra)
        };
        install(&foo1, "gh:o/foo", "retail", &[]).expect(0);
        let crashed = install(
            &foo2,
            "gh:o/foo",
            "retail",
            &[("OMADDONS_FAILPOINTS", failpoint)],
        );
        assert_eq!(crashed.code, -1, "{failpoint}: {}", crashed.context());
        let journal_dir = home.state().join("journal");
        let journal = fs::read_dir(&journal_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let tx = journal.file_stem().unwrap().to_string_lossy().into_owned();
        if !recoverable {
            // A step that matches neither its before nor its after state.
            let mut text = String::new();
            for line in fs::read_to_string(&journal).unwrap().lines() {
                let mut value: Value = serde_json::from_str(line).unwrap();
                if value["type"] == "step" && value["from_id"].is_object() {
                    value["from_id"]["ino"] = Value::from(1u64);
                }
                text.push_str(&value.to_string());
                text.push('\n');
            }
            fs::write(&journal, text).unwrap();
        }
        let context = format!("{failpoint}, recoverable: {recoverable}");

        let r = install(&baz, "gh:o/baz", "classic", &[]);
        let envelope = r.expect(0);
        assert!(home.addons("_classic_").join("Baz").is_dir(), "{context}");
        let warnings = envelope["warnings"].as_array().unwrap();
        let about = |code: &str| {
            warnings
                .iter()
                .any(|w| w["code"] == code && w["message"].as_str().unwrap().contains(&tx))
        };
        if recoverable {
            assert!(about("journal_recovered"), "{context}: {}", r.context());
            assert!(
                fs::read_dir(&journal_dir).unwrap().next().is_none(),
                "{context}"
            );
            let toc = fs::read_to_string(home.addons("_retail_").join("Foo/Foo.toc")).unwrap();
            assert!(toc.contains(&format!("Version: {foo}")), "{context}: {toc}");
            let stage = home
                .client("_retail_")
                .join("Interface/.omaddons/stage")
                .join(&tx);
            assert!(!stage.exists(), "{context}");
            let history = fs::read_to_string(home.state().join("history.jsonl")).unwrap();
            assert_eq!(
                history.matches(&tx).count(),
                usize::from(foo == "2"),
                "{context}: rolled forward once, or back"
            );
        } else {
            assert!(about("journal_pending"), "{context}: {}", r.context());
            assert!(journal.exists(), "{context}: kept for repair");
            // Retail's own changes still stop at it.
            let r = install(&foo2, "gh:o/foo", "retail", &[]);
            assert_ne!(r.code, 0, "{context}: {}", r.context());
            assert!(
                r.stdout.contains("manual repair"),
                "{context}: {}",
                r.context()
            );
        }
    }
}
