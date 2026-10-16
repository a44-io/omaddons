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

use common::engine::{Scene, warning_codes};
use omaddons::engine::FAILPOINTS;
use support::{Op, RecordsOp, expected_new, matrix, record_matrix};

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
