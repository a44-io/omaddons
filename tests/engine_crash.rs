//! The cross-process crash matrix (vision §7, M1 checkpoints): every engine failpoint
//! as a real crash (`abort`: no unwinding, the kernel releases the flocks), for a new
//! install, a first install into a client never launched (no `Interface/`), an update
//! that drops a folder, a removal, and the undo of that update after a play session
//! (restoring SavedVariables, keeping them, or deferring them while the client runs),
//! each over SavedVariables. A crash before each journaled rename
//! (`engine.swap.before_rename=abort@N`), with atomic renames and in the two-rename
//! fallback, reaches every step of a multi-folder swap. After each crash every folder
//! is wholly old or wholly new (or absent, between the fallback's two renames), and so
//! are the SavedVariables; a reader reports the unfinished transaction and changes
//! nothing; the next command that takes the lock recovers it (`journal_recovered`),
//! after which all folders sit on one side, consistent with the record generation and
//! history, and the lock file is the same inode. A crash before `prepared` may leave
//! the SavedVariables copies made before the lock, which the orphan sweep takes once
//! they are old. Recovery itself is crashed too (`engine.recover.*`), and the command
//! after it must finish; so is a journal, history or actions line cut in half.
//!
//! Records-only transactions (a pin, a forget) change no folder, so their oracle is the
//! record: the edit is there exactly when history holds the transaction, the generation
//! agrees, and every folder is byte-identical to before.
//!
//! Debug builds only: every scene arms failpoints, in process or in the binary it runs,
//! and they exist only there (DEC-7).
//!
//! e2e suite: every crash matrix, with E11's differential: before each recovering command,
//! `journal::diagnose` predicts what it will do to each journal (proved to write nothing),
//! and the prediction is held to what it did.
#![cfg(debug_assertions)]

use crate::common;

use std::collections::BTreeMap;
use std::fs;

use common::archives::zip_text;
use common::differential::{Caller, Differential, Tally};
use common::engine::{Scene, shapes, warning_codes};
use common::tlog::{TestLog, tlog};
use omaddons::engine::FAILPOINTS;
use serde_json::json;
use support::{
    Op, RecordsOp, Shape, expected_new, matrix, record_matrix, recovery_matrix, rename_matrix,
    shape_matrix, shape_rename_matrix,
};

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
        /// The undo of the update, after a play session: folders back to v1, and the
        /// SavedVariables restored (the changed ones back, the created one moved aside).
        Undo,
        /// The same with `--saved-variables keep`: the played SavedVariables stay.
        UndoKeep,
        /// The same while the client runs: folders revert, the restore is deferred.
        UndoRunning,
    }

    impl Op {
        pub fn kind(self) -> &'static str {
            match self {
                Self::Install | Self::FirstInstall => "install",
                Self::Update => "update",
                Self::Remove => "remove",
                Self::Undo | Self::UndoKeep | Self::UndoRunning => "undo",
            }
        }

        /// A fresh scene: a retail client with Foo's and FooOld's SavedVariables.
        pub fn scene(self) -> Scene {
            Scene::new().with_saved_variables(common::engine::FOLDERS)
        }

        /// Sets up the baseline and returns the command.
        pub fn prepare(self, s: &mut Scene) -> Vec<String> {
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
                    s.args(&["remove", "Foo"])
                }
                Self::Undo | Self::UndoKeep | Self::UndoRunning => {
                    s.install("gh:o/foo", "v1");
                    s.install("gh:o/foo", "v2");
                    s.play(common::engine::FOLDERS);
                    match self {
                        Self::UndoKeep => s.args(&["undo", "--saved-variables", "keep"]),
                        Self::UndoRunning => {
                            s.home.retail_running();
                            s.args(&["undo"])
                        }
                        _ => s.args(&["undo"]),
                    }
                }
            }
        }
    }

    /// Cuts the file at `path` in the middle of its last line, as a crash during that
    /// line's write leaves it.
    pub fn cut_last_line(path: &std::path::Path) {
        let text = fs::read(path).unwrap();
        let body = text.strip_suffix(b"\n").unwrap_or(&text);
        let start = body
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |at| at + 1);
        let cut = start + (body.len() - start) / 2;
        assert!(cut > start, "{} has an empty last line", path.display());
        fs::write(path, &text[..cut]).unwrap();
    }

    /// What the Foo oracle judges: each folder's tree hash, and the `WTF/` tree.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Foo {
        pub folders: BTreeMap<String, Option<String>>,
        pub wtf: Option<String>,
    }

    impl Foo {
        pub fn of(s: &Scene) -> Self {
            Self {
                folders: s.folders(),
                wtf: s.wtf(),
            }
        }
    }

    /// The state after `op` runs without a failpoint.
    pub fn expected_new(op: Op) -> Foo {
        let mut s = op.scene();
        let args = op.prepare(&mut s);
        s.run(&args, "").expect(0);
        Foo::of(&s)
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

    /// After the recovering command: every committed transaction keeps its snapshots,
    /// and no snapshot dir of one that never committed is left, except what a crash
    /// before `prepared` left (`crash_leftovers`): the SavedVariables copied before the
    /// lock, which no journal named and which hold no addon folder. [`sweep_aged`] then
    /// checks that the orphan sweep takes them once they are old.
    pub fn assert_snapshots(s: &Scene, context: &dyn Fn(&str) -> String, crash_leftovers: bool) {
        let lost = s.lost_snapshots();
        assert!(
            lost.is_empty(),
            "{}",
            context(&format!(
                "committed transactions lost their snapshots {lost:?}"
            ))
        );
        let orphans = s.orphan_snapshots();
        assert!(
            orphans.is_empty() || crash_leftovers,
            "{}",
            context(&format!(
                "snapshots of uncommitted transactions {orphans:?}"
            ))
        );
        for dir in orphans {
            let folders = fs::read_dir(dir.join("AddOns")).map_or(0, Iterator::count);
            assert_eq!(
                folders,
                0,
                "{}",
                context(&format!(
                    "a crash leftover {} holds a folder",
                    dir.display()
                ))
            );
        }
    }

    /// Crash leftovers aged past `ORPHAN_MIN_AGE` are gone after the next commit's
    /// retention (a pin of Bar, which the recovering command installed). Whether there
    /// were any.
    pub fn sweep_aged(s: &Scene, context: &dyn Fn(&str) -> String) -> bool {
        if s.orphan_snapshots().is_empty() {
            return false;
        }
        s.age_orphan_snapshots();
        s.run(&s.args(&["pin", "Bar"]), "").expect(0);
        let left = s.orphan_snapshots();
        assert!(
            left.is_empty(),
            "{}",
            context(&format!("aged crash leftovers survived the sweep {left:?}"))
        );
        true
    }

    /// What one crashed run did.
    #[derive(Debug, Clone, Copy)]
    pub struct Crash {
        /// The command died at a failpoint; otherwise it ran to the end.
        pub crashed: bool,
        /// Until recovery, some folders were new and others not.
        pub mixed: bool,
        /// It left SavedVariables copies the sweep took once they were old.
        pub swept: bool,
        /// It left a journal for the next command to recover.
        pub pending: bool,
        /// The first recovering command died mid-recovery (its `recover` failpoints).
        pub recovering_crashed: bool,
        /// E11: the crash left a journal, and its prediction was compared.
        pub with_journal: bool,
        pub compared: bool,
    }

    /// The tally of crashes `done`.
    pub fn tally(done: &[Crash]) -> Tally {
        let mut tally = Tally::default();
        for crash in done {
            tally.count(crash.with_journal, crash.compared);
        }
        tally
    }

    /// `op` run with the failpoints `spec` armed, then the oracle. Each folder, on its
    /// own, is wholly old or wholly new at every instant (or absent, between the two
    /// renames of the `fallback`), and SavedVariables are wholly old or wholly new; a
    /// reader reports the unfinished transaction and changes nothing; the next command
    /// that takes the lock recovers (with `recover` armed it may die mid-recovery,
    /// keeping the journal, and the command after it finishes); then folders and
    /// SavedVariables sit on one side, consistent with the generation and history, with
    /// nothing stray in AddOns and the same lock file. E11's differential: `diagnose`
    /// predicts the recovering command (again after a recovery that died), and the
    /// prediction is held to what it did.
    pub fn crash(
        log: &TestLog,
        op: Op,
        spec: &str,
        recover: &str,
        new: &Foo,
        fallback: bool,
    ) -> Crash {
        let label = format!("{op:?} at {spec}, then {recover:?}");
        log.step(&label);
        let mut s = op.scene();
        let args = op.prepare(&mut s);
        let s = s;
        let old = Foo::of(&s);
        let (old_generation, old_history, old_inode) =
            (s.generation(), s.history(), s.lock_inode());
        let r = s.run(&args, spec);
        let context = |what: &str| {
            format!(
                "{op:?} at {spec}, then {recover:?}: {what}\nold {old:?}\nnew {new:?}\nnow {:?}\njournals {:?}\n{}",
                Foo::of(&s),
                s.journals()
                    .iter()
                    .map(|p| fs::read_to_string(p).unwrap_or_default())
                    .collect::<Vec<_>>(),
                r.context()
            )
        };
        let crashed = r.code == -1;
        if !crashed {
            assert_eq!(
                r.code,
                0,
                "{}",
                context("not on this path, so it must succeed")
            );
        }

        let at_crash = s.folders();
        for (folder, now) in &at_crash {
            assert!(
                *now == old.folders[folder]
                    || *now == new.folders[folder]
                    || (fallback && now.is_none()),
                "{}",
                context(&format!("{folder} is neither old nor new"))
            );
        }
        let mixed = at_crash != old.folders && at_crash != new.folders;
        let wtf = s.wtf();
        assert!(
            wtf == old.wtf || wtf == new.wtf,
            "{}",
            context("SavedVariables neither old nor new")
        );

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

        // E11: what `diagnose` predicts of the recovering command.
        let running: &[&str] = if matches!(op, Op::UndoRunning) {
            &["_retail_"]
        } else {
            &[]
        };
        let mut differential = Differential::new(log, &s.home, &["_retail_"], running);
        differential.predict(log, &label);

        // The next command that takes the lock recovers. One armed with `recover` may die
        // mid-recovery: the journal stays for the command after it.
        let install_bar = s.install_args("gh:o/bar", "bar");
        let mut recovering_crashed = false;
        let mut recovered_by = None;
        if !recover.is_empty() {
            let first = s.run(&install_bar, recover);
            recovering_crashed = first.code == -1;
            if recovering_crashed {
                assert_eq!(
                    pending(&s),
                    pending_before,
                    "{}",
                    context(&format!(
                        "a recovery that died dropped the journal\n{}",
                        first.context()
                    ))
                );
                // A killed recovery is no prediction's to match: predict what it left.
                differential.predict(log, &format!("{label}, after the recovery that died"));
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &bar, &s.journals(), &caller) > 0;
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
        assert_snapshots(&s, &context, crashed && pending_before == 0);
        // AddOns holds whole addon folders only: nothing staged or half-moved.
        let stray = s.stray(&["Foo", "FooOld", "Bar"]);
        assert!(
            stray.is_empty(),
            "{}",
            context(&format!("stray AddOns entries {stray:?}"))
        );

        // Folders and SavedVariables on one side, consistent with state.
        let now = Foo::of(&s);
        let went_new = now == *new;
        assert!(went_new || now == old, "{}", context("split across sides"));
        // RG-32: these undos revert an install of a given archive (`--file`), which
        // `update` never moves, so no crash and no recovery ever leaves a skip mark
        // (condition 5); `Shape::UndoUpdate` crashes an undo that does mark.
        if op.kind() == "undo" {
            let foo = s.view().remove("foo").unwrap_or_default();
            assert_eq!(
                foo["skipped_releases"],
                serde_json::Value::Null,
                "{}",
                context("an undo of a given archive marks nothing")
            );
        }
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
        let swept = sweep_aged(&s, &context);
        Crash {
            crashed,
            mixed,
            swept,
            pending: pending_before > 0,
            recovering_crashed,
            with_journal: !journals.is_empty(),
            compared,
        }
    }

    /// Every failpoint as a crash of `op`. Some crash before `prepared` leaves
    /// SavedVariables copies, which the sweep takes.
    pub fn matrix(log: &TestLog, op: Op) {
        let new = expected_new(op);
        let mut swept = false;
        let mut done_all = Vec::new();
        let crashed_at: Vec<&str> = FAILPOINTS
            .iter()
            .copied()
            .filter(|failpoint| {
                let done = crash(log, op, &format!("{failpoint}=abort"), "", &new, false);
                swept |= done.swept;
                done_all.push(done);
                done.crashed
            })
            .collect();
        tally(&done_all).assert_complete(log, &format!("{op:?}"));
        assert!(
            swept,
            "{op:?}: no crash left SavedVariables copies to sweep"
        );
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

    /// The `engine.swap.before_rename` crashes of a run: before each journaled rename,
    /// with atomic renames and with the two-rename fallback (`fsx.renameat2.einval`).
    /// `crash` takes a spec and whether the fallback is armed, and runs its oracle; the
    /// first N that does not crash must run to the end. Some crash after the first
    /// rename must leave the folders mixed until recovery: the window is reached. Every
    /// state with a journal must have been compared (E11).
    pub fn renames(log: &TestLog, what: &str, crash: impl Fn(&str, bool) -> Crash) {
        let mut done_all = Vec::new();
        for fallback in [false, true] {
            let einval = if fallback {
                "fsx.renameat2.einval=error,"
            } else {
                ""
            };
            let mut crashes = Vec::new();
            let mut finished = false;
            for n in 1..=16 {
                let done = crash(
                    &format!("{einval}engine.swap.before_rename=abort@{n}"),
                    fallback,
                );
                done_all.push(done);
                if !done.crashed {
                    finished = true;
                    break;
                }
                crashes.push((n, done.mixed));
            }
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("{what} (fallback {fallback}): renames crashed {crashes:?}\n"),
            );
            assert!(finished, "{what} (fallback {fallback}): {crashes:?}");
            assert!(
                crashes.iter().any(|(n, mixed)| *n > 1 && *mixed),
                "{what} (fallback {fallback}): no crash after the first rename left the folders mixed: {crashes:?}"
            );
        }
        tally(&done_all).assert_complete(log, &format!("{what} renames"));
    }

    /// Every journaled rename of `op` as a crash.
    pub fn rename_matrix(log: &TestLog, op: Op) {
        let new = expected_new(op);
        renames(log, &format!("{op:?}"), |spec, fallback| {
            crash(log, op, spec, "", &new, fallback)
        });
    }

    /// Every crash of `op` that leaves a journal, recovered by a command that dies in
    /// that recovery too: after each step a rollback undoes (`engine.recover.step=abort@K`
    /// for every K it reaches), as a roll-forward starts (`engine.recover.forward`), and
    /// before an undo's SavedVariables restore (`engine.recover.sv`). The command after it
    /// finishes, and the oracle holds then. Each kind of recovery crash must happen.
    pub fn recovery_matrix(log: &TestLog, op: Op, kinds: &[&str]) {
        let new = expected_new(op);
        let mut crashed: Vec<(&str, String)> = Vec::new();
        let mut done_all = Vec::new();
        for failpoint in FAILPOINTS {
            let spec = format!("{failpoint}=abort");
            let mut recovers: Vec<String> = Vec::new();
            for k in 1..=8 {
                let recover = format!("engine.recover.step=abort@{k}");
                let done = crash(log, op, &spec, &recover, &new, false);
                done_all.push(done);
                if !done.recovering_crashed {
                    recovers.clear();
                    if done.pending {
                        recovers.extend(
                            ["engine.recover.forward", "engine.recover.sv"]
                                .map(|r| format!("{r}=abort")),
                        );
                    }
                    break;
                }
                crashed.push((failpoint, recover));
            }
            for recover in recovers {
                let done = crash(log, op, &spec, &recover, &new, false);
                done_all.push(done);
                if done.recovering_crashed {
                    crashed.push((failpoint, recover));
                }
            }
        }
        tally(&done_all).assert_complete(log, &format!("{op:?} recoveries"));
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("{op:?}: recoveries crashed {crashed:?}\n"),
        );
        for kind in kinds {
            assert!(
                crashed.iter().any(|(_, r)| r.starts_with(kind)),
                "{op:?}: no recovery crashed at {kind}: {crashed:?}"
            );
        }
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
        /// `undo` of an update from release fixtures: the folder back, the release it
        /// leaves marked skipped in the same record (RG-32).
        UndoUpdate,
    }

    impl Shape {
        pub fn kind(self) -> &'static str {
            match self {
                Self::UpdateMany | Self::Handover => "update",
                Self::InstallWithDep => "install",
                Self::SyncMixed => "sync",
                Self::Adopt => "adopt",
                Self::UndoUpdate => "undo",
            }
        }

        pub fn folders(self) -> &'static [&'static str] {
            match self {
                Self::UpdateMany => shapes::UPDATE_MANY,
                Self::InstallWithDep => shapes::INSTALL_WITH_DEP,
                Self::SyncMixed => shapes::SYNC_MIXED,
                Self::Handover => shapes::HANDOVER,
                Self::Adopt => shapes::ADOPT,
                Self::UndoUpdate => shapes::UNDO_UPDATE,
            }
        }

        pub fn prepare(self, s: &Scene) -> Vec<String> {
            match self {
                Self::UpdateMany => shapes::update_many(s),
                Self::InstallWithDep => shapes::install_with_dep(s),
                Self::SyncMixed => shapes::sync_mixed(s),
                Self::Handover => shapes::handover(s),
                Self::Adopt => shapes::adopt(s),
                Self::UndoUpdate => shapes::undo_update(s),
            }
        }

        /// A fresh scene, with SavedVariables of every folder the shape touches.
        pub fn scene(self) -> Scene {
            Scene::new().with_saved_variables(self.folders())
        }
    }

    /// What a shape is judged by: its folders, every record (volatile fields aside), the
    /// files index, Bar's (the recovery's) left out, and the `WTF/` tree.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Side {
        folders: BTreeMap<String, Option<String>>,
        records: BTreeMap<String, serde_json::Value>,
        indexed: Vec<String>,
        wtf: Option<String>,
    }

    impl Side {
        pub fn of(s: &Scene, shape: Shape) -> Self {
            let mut indexed = s.indexed();
            indexed.retain(|k| k != "bar");
            Self {
                folders: s.folders_of(shape.folders()),
                records: s.view(),
                indexed,
                wtf: s.wtf(),
            }
        }
    }

    /// The shape after it ran without a failpoint.
    pub fn reference(shape: Shape) -> Side {
        let s = shape.scene();
        let args = shape.prepare(&s);
        s.run(&args, "").expect(0);
        Side::of(&s, shape)
    }

    /// `shape` run with the failpoints `spec` armed, then the shape oracle. Each folder
    /// is wholly old or wholly new at every instant (or absent, between the two renames
    /// of the `fallback`); a reader reports and writes nothing; the next command
    /// recovers; then folders, records, the files index and SavedVariables sit together
    /// on one side, the side history says (a mix of new folders and old records fails),
    /// every record's folders exist, and the generation and lock inode agree.
    /// Records-only shapes never move a folder.
    pub fn crash_shape(
        log: &TestLog,
        shape: Shape,
        spec: &str,
        new: &Side,
        fallback: bool,
    ) -> Crash {
        let label = format!("{shape:?} at {spec}");
        log.step(&label);
        let s = shape.scene();
        let args = shape.prepare(&s);
        let old = Side::of(&s, shape);
        assert_ne!(&old, new, "{shape:?}: the reference run changed nothing");
        let (old_generation, old_history, old_inode) =
            (s.generation(), s.history(), s.lock_inode());
        let r = s.run(&args, spec);
        let context = |what: &str| {
            format!(
                "{shape:?} at {spec}: {what}\nold {old:?}\nnew {new:?}\nnow {:?}\nhistory {:?}\njournals {:?}\n{}",
                Side::of(&s, shape),
                s.history_ops(),
                s.journals()
                    .iter()
                    .map(|p| fs::read_to_string(p).unwrap_or_default())
                    .collect::<Vec<_>>(),
                r.context()
            )
        };
        let crashed = r.code == -1;
        if !crashed {
            assert_eq!(r.code, 0, "{}", context("off the path, so it must succeed"));
        }
        let at_crash = s.folders_of(shape.folders());
        for (folder, now) in &at_crash {
            assert!(
                *now == old.folders[folder]
                    || *now == new.folders[folder]
                    || (fallback && now.is_none()),
                "{}",
                context(&format!("{folder} is neither old nor new"))
            );
        }
        let mixed = at_crash != old.folders && at_crash != new.folders;
        assert_eq!(s.wtf(), old.wtf, "{}", context("SavedVariables changed"));

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

        // E11: what `diagnose` predicts of the recovering command.
        let mut differential = Differential::new(log, &s.home, &["_retail_"], &[]);
        differential.predict(log, &label);

        // The next command that takes the lock recovers.
        let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &bar, &s.journals(), &caller) > 0;
        assert_eq!(
            warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
            pending_before > 0,
            "{}",
            context("recovery warning")
        );
        assert!(s.journals().is_empty(), "{}", context("journals left"));
        assert_snapshots(&s, &context, crashed && pending_before == 0);
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
        let side = if committed { new } else { &old };
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
        let swept = sweep_aged(&s, &context);
        Crash {
            crashed,
            mixed,
            swept,
            pending: pending_before > 0,
            recovering_crashed: false,
            with_journal: !journals.is_empty(),
            compared,
        }
    }

    /// Every journaled rename of `shape` as a crash.
    pub fn shape_rename_matrix(log: &TestLog, shape: Shape) {
        let new = reference(shape);
        renames(log, &format!("{shape:?}"), |spec, fallback| {
            crash_shape(log, shape, spec, &new, fallback)
        });
    }

    /// Every failpoint as a crash of `shape`. Unless it is records-only, some crash
    /// before `prepared` leaves SavedVariables copies, which the sweep takes.
    pub fn shape_matrix(log: &TestLog, shape: Shape) {
        let new = reference(shape);
        let mut swept = false;
        let mut done_all = Vec::new();
        let crashed_at: Vec<&str> = FAILPOINTS
            .iter()
            .copied()
            .filter(|failpoint| {
                let done = crash_shape(log, shape, &format!("{failpoint}=abort"), &new, false);
                swept |= done.swept;
                done_all.push(done);
                done.crashed
            })
            .collect();
        tally(&done_all).assert_complete(log, &format!("{shape:?}"));
        let records_only = matches!(shape, Shape::Adopt);
        assert_eq!(
            swept, !records_only,
            "{shape:?}: SavedVariables copies left to sweep"
        );
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
    pub fn record_matrix(log: &TestLog, op: RecordsOp) {
        let mut crashed_at = Vec::new();
        let mut tally = Tally::default();
        for failpoint in FAILPOINTS {
            let label = format!("{op:?} at {failpoint}");
            log.step(&label);
            let s = Scene::new().with_saved_variables(common::engine::FOLDERS);
            let args = op.prepare(&s);
            let (old, old_wtf) = (s.folders(), s.wtf());
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
            let crashed = r.code == -1;
            if crashed {
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
            assert_eq!(s.wtf(), old_wtf, "{}", context("SavedVariables changed"));

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

            // E11: what `diagnose` predicts of the recovering command.
            let mut differential = Differential::new(log, &s.home, &["_retail_"], &[]);
            differential.predict(log, &label);

            // The next command that takes the lock recovers.
            let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
            let caller = differential.commit(&["retail"]);
            let compared = differential.compare(log, &bar, &s.journals(), &caller) > 0;
            tally.count(!journals.is_empty(), compared);
            assert_eq!(
                warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
                pending_before > 0,
                "{}",
                context("recovery warning")
            );
            assert!(s.journals().is_empty(), "{}", context("journals left"));
            assert_snapshots(&s, &context, crashed && pending_before == 0);
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
            sweep_aged(&s, &context);
        }
        tally.assert_complete(log, &format!("{op:?}"));
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
    matrix(&tlog!(), Op::Install);
}

#[test]
fn crash_matrix_first_install() {
    matrix(&tlog!(), Op::FirstInstall);
}

#[test]
fn crash_matrix_update() {
    matrix(&tlog!(), Op::Update);
}

#[test]
fn crash_matrix_remove() {
    matrix(&tlog!(), Op::Remove);
}

#[test]
fn crash_matrix_pin() {
    record_matrix(&tlog!(), RecordsOp::Pin);
}

#[test]
fn crash_matrix_undo() {
    matrix(&tlog!(), Op::Undo);
}

#[test]
fn crash_matrix_undo_keeping_saved_variables() {
    matrix(&tlog!(), Op::UndoKeep);
}

#[test]
fn crash_matrix_undo_while_the_client_runs() {
    matrix(&tlog!(), Op::UndoRunning);
}

#[test]
fn a_crash_at_every_rename_of_an_undo() {
    rename_matrix(&tlog!(), Op::Undo);
}

#[test]
fn a_crash_during_recovery_of_an_install() {
    recovery_matrix(
        &tlog!(),
        Op::Install,
        &["engine.recover.step", "engine.recover.forward"],
    );
}

#[test]
fn a_crash_during_recovery_of_an_update() {
    recovery_matrix(
        &tlog!(),
        Op::Update,
        &["engine.recover.step", "engine.recover.forward"],
    );
}

#[test]
fn a_crash_during_recovery_of_an_undo() {
    recovery_matrix(
        &tlog!(),
        Op::Undo,
        &[
            "engine.recover.step",
            "engine.recover.forward",
            "engine.recover.sv",
        ],
    );
}

/// An update crashed after its exchange and its move of the old folder to the snapshot,
/// whose recovery dies after each undo in turn. Once both renames are undone the staged
/// folder is back at the stage path the second step moved the old one from; the
/// command after it must still finish the rollback, not take that step for a stranger.
#[test]
fn a_rollback_that_died_after_undoing_an_exchange_is_finished() {
    let log = tlog!();
    let new = expected_new(Op::Update);
    let mut crashed = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = support::crash(
            &log,
            Op::Update,
            "engine.swap.before_rename=abort@3",
            &recover,
            &new,
            false,
        );
        if !done.recovering_crashed {
            break;
        }
        crashed.push(k);
    }
    assert_eq!(
        crashed,
        [1, 2, 3],
        "each undo of three steps, crashed after"
    );
}

#[test]
fn a_crash_at_every_rename_of_an_install() {
    rename_matrix(&tlog!(), Op::Install);
}

#[test]
fn a_crash_at_every_rename_of_an_update() {
    rename_matrix(&tlog!(), Op::Update);
}

#[test]
fn a_crash_at_every_rename_of_a_remove() {
    rename_matrix(&tlog!(), Op::Remove);
}

#[test]
fn a_crash_at_every_rename_of_an_update_of_three() {
    shape_rename_matrix(&tlog!(), Shape::UpdateMany);
}

#[test]
fn a_crash_at_every_rename_of_a_handover() {
    shape_rename_matrix(&tlog!(), Shape::Handover);
}

#[test]
fn crash_matrix_update_many() {
    shape_matrix(&tlog!(), Shape::UpdateMany);
}

#[test]
fn crash_matrix_install_with_dependencies() {
    shape_matrix(&tlog!(), Shape::InstallWithDep);
}

#[test]
fn crash_matrix_sync_mixed() {
    shape_matrix(&tlog!(), Shape::SyncMixed);
}

#[test]
fn crash_matrix_handover() {
    shape_matrix(&tlog!(), Shape::Handover);
}

#[test]
fn crash_matrix_adopt() {
    shape_matrix(&tlog!(), Shape::Adopt);
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
    // UndoUpdate: Alpha back at v1, with v2 marked skipped by the undo (RG-32). Its
    // matrix compares whole records, so every crash leaves Alpha at v2 with no mark, or
    // at v1 with this one.
    let s = Scene::new();
    let args = Shape::UndoUpdate.prepare(&s);
    assert_eq!(
        s.view()["alpha"]["skipped_releases"],
        serde_json::Value::Null
    );
    s.run(&args, "").expect(0);
    let alpha = &s.view()["alpha"];
    assert_eq!(alpha["version"], "v1");
    assert_eq!(
        alpha["skipped_releases"],
        json!([{"release": {"source": "gh:o/alpha", "tag": "v2"}, "version": "v2", "origin": "undo"}])
    );
}

#[test]
fn crash_matrix_undo_of_an_update() {
    shape_matrix(&tlog!(), Shape::UndoUpdate);
}

#[test]
fn crash_matrix_forget() {
    record_matrix(&tlog!(), RecordsOp::Forget);
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
    let sv = Op::Install.scene().wtf();
    assert!(sv.is_some());
    for (op, done) in [
        ("install", &install),
        ("update", &update),
        ("remove", &remove),
    ] {
        assert_eq!(done.wtf, sv, "{op} never changes SavedVariables");
    }
    let (install, first, update, remove) = (
        install.folders,
        first.folders,
        update.folders,
        remove.folders,
    );
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

    // Undo: v1's folders back, and the SavedVariables restored, kept as played, or
    // (the client running) left as played.
    let played = {
        let s = Op::Undo.scene();
        s.play(common::engine::FOLDERS);
        s.wtf()
    };
    assert_ne!(played, sv);
    for (op, wtf) in [
        (Op::Undo, &sv),
        (Op::UndoKeep, &played),
        (Op::UndoRunning, &played),
    ] {
        let done = expected_new(op);
        assert_eq!(done.folders, install, "{op:?}");
        assert_eq!(&done.wtf, wtf, "{op:?}");
    }
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
    let log = tlog!();
    let mut tally = Tally::default();
    for (failpoint, restored, unfinished) in points {
        log.step(&format!(
            "an undo at {failpoint}, recovered while the client runs"
        ));
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
        let mut differential = Differential::new(&log, &s.home, &["_retail_"], &["_retail_"]);
        differential.predict(&log, failpoint);
        let with_journal = !s.journals().is_empty();
        let r = s.run(&s.install_args("gh:o/bar", "bar"), "");
        let envelope = r.expect(0);
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(&log, &envelope, &s.journals(), &caller) > 0;
        tally.count(with_journal, compared);
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
    tally.assert_complete(&log, "an undo recovered while the client runs");
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
    let log = tlog!();
    for force in [false, true] {
        log.step(&format!(
            "a double fault (force: {force}), then the next change"
        ));
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
        let mut differential = Differential::new(&log, &s.home, &["_retail_"], &[]);
        differential.predict(&log, &format!("the kept journal (force: {force})"));
        args = s.install_args("gh:o/baz", "baz");
        args.push("--force".to_owned());
        let r = s.run(&args, "");
        let envelope = r.expect(0);
        let caller = differential.commit(&["retail"]);
        assert_eq!(
            differential.compare(&log, &envelope, &s.journals(), &caller),
            1
        );
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

    let log = tlog!();
    // (failpoint, Foo's version on retail after recovery, whether recovery can finish)
    for (failpoint, foo, recoverable) in [
        ("engine.swapped=abort", "2", true),
        ("engine.swap.after_exchange=abort", "1", true),
        ("engine.swap.after_exchange=abort", "", false),
    ] {
        log.step(&format!(
            "retail at {failpoint}, recoverable: {recoverable}"
        ));
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
        let clients = ["_retail_", "_classic_"];
        let mut differential = Differential::new(&log, &home, &clients, &[]);
        differential.predict(&log, &format!("{context}, by classic"));
        let journals = || restore_support::listed(&journal_dir);

        let r = install(&baz, "gh:o/baz", "classic", &[]);
        let envelope = r.expect(0);
        let classic = differential.commit(&["classic"]);
        assert_eq!(
            differential.compare(&log, &envelope, &journals(), &classic),
            1
        );
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
            differential.predict(&log, &format!("{context}, by retail"));
            let r = install(&foo2, "gh:o/foo", "retail", &[]);
            assert_ne!(r.code, 0, "{context}: {}", r.context());
            let retail = differential.commit(&["retail"]);
            differential.compare(&log, &r.envelope(), &journals(), &retail);
            assert!(
                r.stdout.contains("manual repair"),
                "{context}: {}",
                r.context()
            );
        }
    }
}

/// E11's stuck states, through the binary: a retail install crashed after its exchange,
/// then each plant, on a fresh two-client home per caller. `diagnose` names the state, and
/// what it predicts is what each recovering command then does: an install on retail (the
/// journal's own target), one on classic (another target), and `config set` (recovery
/// under the lock alone). Every mode a plant takes away is given back.
#[test]
fn every_stuck_state_is_predicted_through_the_binary() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use common::differential::Prediction;
    use common::home::Home;
    use omaddons::engine::journal::{JournalVerdict, UnattributedCause};
    use serde_json::Value;

    /// Rewrites each JSON line of `journal` with `edit`.
    fn rewrite(journal: &Path, edit: &dyn Fn(&mut Value)) {
        let mut text = String::new();
        for line in fs::read_to_string(journal).unwrap().lines() {
            let mut value: Value = serde_json::from_str(line).unwrap();
            edit(&mut value);
            text.push_str(&value.to_string());
            text.push('\n');
        }
        fs::write(journal, text).unwrap();
    }
    /// `path` at mode `0o000`, to be given back.
    fn seal(path: &Path) -> Vec<(PathBuf, u32)> {
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        vec![(path.to_path_buf(), mode)]
    }
    fn prepared(value: &mut Value, edit: &dyn Fn(&mut Value)) {
        if value["type"] == "prepared" {
            edit(value);
        }
    }
    fn verdict(p: &Prediction) -> Option<JournalVerdict> {
        p.diagnoses.journals.first().map(|d| d.verdict)
    }

    type Plant = Box<dyn Fn(&Home, &Path) -> Vec<(PathBuf, u32)>>;
    type Named = Box<dyn Fn(&Prediction) -> bool>;
    let unattributed = |cause| Some(JournalVerdict::Unattributed { cause });
    let cases: Vec<(&str, Plant, Named)> = vec![
        (
            "marker_mismatch",
            Box::new(|home, _| {
                let marker = omaddons::discover::registry::marker_path(&home.client("_retail_"));
                let other = omaddons::model::TargetKey::new();
                fs::write(marker, format!("{{\"key\":\"{other}\"}}")).unwrap();
                Vec::new()
            }),
            Box::new(|p| verdict(p) == Some(JournalVerdict::MarkerMismatch)),
        ),
        (
            "step_unrecognized",
            Box::new(|_, journal| {
                rewrite(journal, &|v| {
                    if v["type"] == "step" && v["from_id"].is_object() {
                        v["from_id"]["ino"] = Value::from(1u64);
                    }
                });
                Vec::new()
            }),
            Box::new(|p| {
                let steps = p.diagnoses.journals[0].evidence.steps;
                verdict(p)
                    == Some(JournalVerdict::StepUnrecognized {
                        step: u32::try_from(steps).unwrap(),
                    })
            }),
        ),
        (
            "too_new",
            Box::new(|_, journal| {
                rewrite(journal, &|v| {
                    prepared(v, &|v| v["schema_version"] = "9.0".into())
                });
                Vec::new()
            }),
            Box::new(|p| verdict(p) == Some(JournalVerdict::TooNew)),
        ),
        (
            "unreadable",
            Box::new(|_, journal| {
                let text = fs::read_to_string(journal).unwrap();
                let body = text.strip_suffix('\n').unwrap();
                let last = body.rfind('\n').unwrap() + 1;
                fs::write(
                    journal,
                    format!("{}not json\n{}", &text[..last], &text[last..]),
                )
                .unwrap();
                Vec::new()
            }),
            Box::new(|p| verdict(p) == Some(JournalVerdict::Unreadable)),
        ),
        (
            "unattributed_too_new",
            Box::new(|_, journal| {
                rewrite(journal, &|v| {
                    prepared(v, &|v| {
                        v["schema_version"] = "9.0".into();
                        v["target_key"] = "garbage".into();
                    });
                });
                Vec::new()
            }),
            Box::new(move |p| verdict(p) == unattributed(UnattributedCause::TooNew)),
        ),
        (
            "unattributed_unreadable",
            Box::new(|_, journal| {
                rewrite(journal, &|v| {
                    prepared(v, &|v| {
                        v["kind"] = "teleport".into();
                        v["target_key"] = "garbage".into();
                    });
                });
                Vec::new()
            }),
            Box::new(move |p| verdict(p) == unattributed(UnattributedCause::Unreadable)),
        ),
        (
            "unattributed_read_failed",
            Box::new(|_, journal| seal(journal)),
            Box::new(move |p| verdict(p) == unattributed(UnattributedCause::ReadFailed)),
        ),
        (
            "unattributed_liveness",
            Box::new(|home, journal| {
                let tx = journal.file_stem().unwrap().to_string_lossy().into_owned();
                let lock = home.state().join("inflight").join(format!("{tx}.lock"));
                if !lock.exists() {
                    fs::create_dir_all(lock.parent().unwrap()).unwrap();
                    fs::write(&lock, "").unwrap();
                }
                seal(&lock)
            }),
            Box::new(move |p| verdict(p) == unattributed(UnattributedCause::LivenessUnknown)),
        ),
        (
            "unlisted",
            Box::new(|_, journal| seal(journal.parent().unwrap())),
            Box::new(|p| p.diagnoses.unlisted.is_some() && p.diagnoses.journals.is_empty()),
        ),
    ];

    let log = tlog!();
    let callers: [(&str, &[&str]); 3] = [
        ("retail", &["install", "gh:o/qux", "-t", "retail"]),
        ("classic", &["install", "gh:o/baz", "-t", "classic"]),
        ("config set", &["config", "set", "running_policy", "refuse"]),
    ];
    for (case, plant, named) in &cases {
        for (caller_name, argv) in callers {
            log.step(&format!("{case}, recovered by {caller_name}"));
            let home = Home::new(&["wow", "wow_classic"]);
            let dl = home.env.root().join("dl");
            fs::create_dir_all(&dl).unwrap();
            let zip = |name: &str, entries: &[(&str, &str)]| {
                let path = dl.join(format!("{name}.zip"));
                fs::write(&path, zip_text(entries)).unwrap();
                path.display().to_string()
            };
            let install = |file: &str, r: &str, target: &str, extra: &[(&str, &str)]| {
                let argv =
                    ["install", r, "--file", file, "-t", target, "--json"].map(str::to_owned);
                home.exec_with(&argv, extra)
            };
            let foo1 = zip(
                "foo1",
                &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n")],
            );
            let foo2 = zip(
                "foo2",
                &[("Foo/Foo.toc", "## Interface: 120105\n## Version: 2\n")],
            );
            let files = [
                (
                    "gh:o/qux",
                    zip("qux", &[("Qux/Qux.toc", "## Interface: 120105\n")]),
                ),
                (
                    "gh:o/baz",
                    zip("baz", &[("Baz/Baz.toc", "## Interface: 50504\n")]),
                ),
            ];
            install(&foo1, "gh:o/foo", "retail", &[]).expect(0);
            let crashed = install(
                &foo2,
                "gh:o/foo",
                "retail",
                &[("OMADDONS_FAILPOINTS", "engine.swap.after_exchange=abort")],
            );
            assert_eq!(crashed.code, -1, "{case}: {}", crashed.context());
            let journal_dir = home.state().join("journal");
            let journal = restore_support::listed(&journal_dir).remove(0);
            let sealed = plant(&home, &journal);

            let clients = ["_retail_", "_classic_"];
            let mut differential = Differential::new(&log, &home, &clients, &[]);
            let under = argv[0] == "config";
            let prediction = if under {
                differential.predict_unknown(&log, case)
            } else {
                differential.predict(&log, case)
            };
            assert!(named(prediction), "{case}: {:#?}", prediction.diagnoses);
            let mut argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
            if let Some((_, file)) = files
                .iter()
                .find(|(r, _)| argv.get(1) == Some(&(*r).to_owned()))
            {
                argv.extend(["--file".to_owned(), file.clone()]);
            }
            argv.push("--json".to_owned());
            let run = home.exec(&argv);
            let journals_after = if sealed.iter().any(|(p, _)| *p == journal_dir) {
                vec![journal.clone()]
            } else {
                restore_support::listed(&journal_dir)
            };
            let caller = if under {
                Caller::Under
            } else {
                differential.commit(&[caller_name])
            };
            differential.compare(&log, &run.envelope(), &journals_after, &caller);
            for (path, mode) in sealed {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            }
        }
    }
}

/// A crash after Bar's history line was written, then a power loss that cut the line
/// short (by 40 bytes, or by its newline alone): the next command rolls Bar forward
/// with exactly one whole line of its own, and `history` and `undo` see Foo, Bar and
/// Baz, with nothing to warn about.
#[test]
fn a_history_line_cut_short_by_a_crash_hides_no_transaction() {
    for cut in [40, 1] {
        let s = Scene::new();
        let baz = zip_text(&[("Baz/Baz.toc", "## Interface: 120105\n")]);
        fs::write(s.dl.join("baz.zip"), baz).unwrap();
        s.install("gh:o/foo", "v1");
        let crashed = s.run(
            &s.install_args("gh:o/bar", "bar"),
            "engine.history.appended=abort",
        );
        assert_eq!(crashed.code, -1, "{}", crashed.context());
        let journals = s.journals();
        let bar = journals[0]
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let history = s.home.state().join("history.jsonl");
        let len = fs::metadata(&history).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&history)
            .unwrap()
            .set_len(len - cut)
            .unwrap();

        let recovering = s.install("gh:o/baz", "baz");
        assert!(
            warning_codes(&recovering).contains(&"journal_recovered".to_owned()),
            "cut {cut}: {recovering}"
        );
        let txs = s.history_txs();
        assert_eq!(txs.len(), 3, "cut {cut}: {txs:?}");
        assert_eq!(txs.iter().filter(|t| **t == bar).count(), 1, "cut {cut}");
        let listed = s.ok(&["history"]);
        assert_eq!(
            listed["data"]["entries"].as_array().unwrap().len(),
            3,
            "cut {cut}: {listed}"
        );
        assert!(warning_codes(&listed).is_empty(), "cut {cut}: {listed}");

        let first = s.ok(&["undo"]);
        assert!(!s.addons().join("Baz").exists(), "cut {cut}: {first}");
        let second = s.ok(&["undo", &bar]);
        assert_eq!(second["data"]["reverted"]["tx"], bar.as_str(), "cut {cut}");
        assert!(!s.addons().join("Bar").exists(), "cut {cut}: {second}");
    }
}

/// Half a history line after a clean commit (an append a power loss cut short): a read
/// lists the commit and writes nothing, and the next change closes the fragment off.
/// Both transactions are listed, nothing is warned about, and both undo.
#[test]
fn a_torn_history_tail_is_closed_off_by_the_next_change() {
    let s = Scene::new().with_saved_variables(common::engine::FOLDERS);
    s.install("gh:o/foo", "v1");
    let history = s.home.state().join("history.jsonl");
    let mut f = fs::OpenOptions::new().append(true).open(&history).unwrap();
    std::io::Write::write_all(&mut f, br#"{"schema_version":"1.0","tx":"01M5"#).unwrap();
    drop(f);

    let before = s.snapshot();
    let listed = s.ok(&["history"]);
    assert_eq!(s.snapshot(), before, "a read wrote");
    assert_eq!(listed["data"]["entries"].as_array().unwrap().len(), 1);
    assert!(warning_codes(&listed).is_empty(), "{listed}");

    s.install("gh:o/bar", "bar");
    let listed = s.ok(&["history"]);
    let entries = listed["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{listed}");
    assert!(warning_codes(&listed).is_empty(), "{listed}");
    assert!(fs::read(&history).unwrap().ends_with(b"\n"));
    let foo = s.history_txs()[0].clone();
    s.ok(&["undo"]);
    s.ok(&["undo", &foo]);
    assert_eq!(s.stray(&[]), Vec::<String>::new(), "both undone");
}

/// A crash while the journal's last line was written (a step record before its rename,
/// or the `swapped` mark after the last one), the line cut in half: the next command
/// reads the journal up to the cut and rolls back, so folders and SavedVariables are
/// all old, history holds no line of the crashed transaction, and nothing is left.
#[test]
fn a_journal_cut_short_by_a_crash_rolls_back() {
    let old = {
        let mut s = Op::Update.scene();
        Op::Update.prepare(&mut s);
        support::Foo::of(&s)
    };
    let log = tlog!();
    for failpoint in ["engine.swap.before_rename", "engine.swapped"] {
        log.step(&format!("an update at {failpoint}, its journal cut"));
        let mut s = Op::Update.scene();
        let args = Op::Update.prepare(&mut s);
        let history = s.history();
        let crashed = s.run(&args, &format!("{failpoint}=abort"));
        assert_eq!(crashed.code, -1, "{failpoint}: {}", crashed.context());
        let journals = s.journals();
        assert_eq!(journals.len(), 1, "{failpoint}");
        support::cut_last_line(&journals[0]);

        let before = s.snapshot();
        let status = s.ok(&["status"]);
        assert!(
            warning_codes(&status).contains(&"journal_pending".to_owned()),
            "{failpoint}: {status}"
        );
        assert_eq!(s.snapshot(), before, "{failpoint}: a read wrote");

        let mut differential = Differential::new(&log, &s.home, &["_retail_"], &[]);
        differential.predict(&log, failpoint);
        let bar = s.install("gh:o/bar", "bar");
        let caller = differential.commit(&["retail"]);
        assert_eq!(differential.compare(&log, &bar, &s.journals(), &caller), 1);
        assert!(
            warning_codes(&bar).contains(&"journal_recovered".to_owned()),
            "{failpoint}: {bar}"
        );
        assert_eq!(support::Foo::of(&s), old, "{failpoint}: rolled back");
        assert!(s.journals().is_empty(), "{failpoint}");
        let mut expected = history.clone();
        expected.push("install".to_owned());
        assert_eq!(s.history(), expected, "{failpoint}");
        assert_eq!(s.stray(&["Foo", "FooOld", "Bar"]), Vec::<String>::new());
    }
}

/// A crash after the swap with `actions.jsonl`, the audit log, cut mid-line: recovery
/// never reads it, so the transaction rolls forward, is listed, and undoes.
#[test]
fn a_torn_actions_log_never_stops_recovery() {
    let mut s = Op::Update.scene();
    let args = Op::Update.prepare(&mut s);
    let crashed = s.run(&args, "engine.swapped=abort");
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    let journal = s.journals().pop().unwrap();
    let tx = journal.file_stem().unwrap().to_str().unwrap().to_owned();
    let actions = s
        .home
        .state()
        .join("snapshots")
        .join(&tx)
        .join("actions.jsonl");
    support::cut_last_line(&actions);

    let bar = s.install("gh:o/bar", "bar");
    assert!(
        warning_codes(&bar).contains(&"journal_recovered".to_owned()),
        "{bar}"
    );
    assert_eq!(
        support::Foo::of(&s),
        expected_new(Op::Update),
        "rolled forward"
    );
    assert!(s.history_txs().contains(&tx));
    s.ok(&["undo"]);
    let undone = s.ok(&["undo", &tx]);
    assert_eq!(undone["data"]["reverted"]["tx"], tx.as_str(), "{undone}");
    let v1 = s.folders();
    assert!(v1["Foo"].is_some() && v1["FooOld"].is_some(), "{v1:?}");
}

/// Every command that takes the global lock to change something recovers a journal
/// left by a crash first: a setting, a target, the credentials, the export file, and a
/// change command that turns out to change nothing. Their dry runs never do, and take
/// no lock; neither does a no-op install with no journal waiting.
#[test]
fn every_command_that_changes_something_recovers_first() {
    use common::home::Run;
    use common::http::paths;
    use omaddons::fsx::GlobalLock;
    use serde_json::Value;
    use std::time::{Duration, Instant};

    let crashed = || {
        let s = Scene::new();
        s.install("gh:o/bar", "bar");
        let crashed = s.run(&s.install_args("gh:o/foo", "v1"), "engine.swapped=abort");
        assert_eq!(crashed.code, -1, "{}", crashed.context());
        assert_eq!(s.journals().len(), 1);
        s
    };
    let noop: &[&str] = &["install", "gh:o/bar", "--file", "<bar>", "-t", "retail"];
    let commands: [&[&str]; 10] = [
        &["config", "set", "running_policy", "refuse"],
        &["targets", "rename", "retail", "main"],
        &["targets", "default", "retail"],
        &["auth", "github", "clear"],
        &["export", "--write"],
        noop,
        &["update", "-t", "retail", "--offline"],
        &["unpin", "Bar", "-t", "retail", "--offline"],
        &["adopt", "--apply", "-t", "retail", "--offline"],
        &["sync", "-t", "retail", "--offline"],
    ];
    let argv = |s: &Scene, args: &[&str], extra: &[&str]| -> Vec<String> {
        let bar = s.zip("bar");
        args.iter()
            .map(|a| if *a == "<bar>" { bar.as_str() } else { a })
            .chain(extra.iter().copied())
            .chain(["--json"])
            .map(str::to_owned)
            .collect()
    };
    // What `sync` reads, written by a read-only export (which recovers nothing).
    let desired = |s: &Scene| {
        let export = s.home.exec(&["export".to_owned()]);
        assert_eq!(export.code, 0, "{}", export.context());
        let dir = s.home.env.config().join("omaddons");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("addons.toml"), export.stdout).unwrap();
    };
    // Each reports what it recovered, and no longer calls it pending: the journal
    // warnings describe the end of the run, on stdout and on stderr.
    let recovered_only = |r: &Run, envelope: &Value, args: &[&str]| {
        let codes = warning_codes(envelope);
        assert!(
            codes.contains(&"journal_recovered".to_owned()),
            "{args:?}: {}",
            r.context()
        );
        assert!(
            !codes.contains(&"journal_pending".to_owned())
                && !codes.contains(&"tx_in_progress".to_owned())
                && !r.stderr.contains("journal_pending"),
            "{args:?}: {}",
            r.context()
        );
    };
    for args in commands {
        let s = crashed();
        if args[0] == "sync" {
            desired(&s);
        }
        let r = s.run(&argv(&s, args, &[]), "");
        let envelope = r.expect(0);
        recovered_only(&r, &envelope, args);
        assert!(s.journals().is_empty(), "{args:?}");
        assert!(s.folders()["Foo"].is_some(), "{args:?}: rolled forward");
        assert!(s.addons().join("Bar").is_dir(), "{args:?}");
    }
    // A command that commits recovers in its commit window.
    let s = crashed();
    let args = ["remove", "Bar", "-t", "retail"];
    let r = s.run(&argv(&s, &args, &[]), "");
    let envelope = r.expect(0);
    recovered_only(&r, &envelope, &args);
    assert!(!s.addons().join("Bar").exists());

    // A dry run recovers nothing, so still reports the journal.
    let s = crashed();
    let r = s.run(&argv(&s, noop, &["--dry-run"]), "");
    let codes = warning_codes(&r.expect(0));
    assert!(
        codes.contains(&"journal_pending".to_owned())
            && !codes.contains(&"journal_recovered".to_owned()),
        "{}",
        r.context()
    );

    // Dry runs leave the journal and wait for no lock.
    let s = crashed();
    let lock = GlobalLock::acquire(&paths(&s.home.env), "test").unwrap();
    for args in commands {
        if args[0] == "sync" {
            desired(&s);
        }
        let started = Instant::now();
        let r = s.run(&argv(&s, args, &["--dry-run"]), "");
        let envelope = r.expect(0);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{args:?}: waited for the lock"
        );
        assert!(
            !warning_codes(&envelope).contains(&"journal_recovered".to_owned()),
            "{args:?}: {}",
            r.context()
        );
        assert_eq!(s.journals().len(), 1, "{args:?}");
    }
    drop(lock);

    // With no journal waiting, a no-op install never takes the lock.
    let s = Scene::new();
    s.install("gh:o/bar", "bar");
    let lock = GlobalLock::acquire(&paths(&s.home.env), "test").unwrap();
    let started = Instant::now();
    s.run(&argv(&s, noop, &[]), "").expect(0);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "waited for the lock"
    );
    drop(lock);
}

/// A journal that recovery cannot finish never fails a command outside the engine: it
/// goes ahead and names the reason in `journal_pending`, and the journal stays.
#[test]
fn a_journal_recovery_cannot_finish_only_warns_outside_the_engine() {
    use serde_json::Value;

    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let crashed = s.run(
        &s.install_args("gh:o/foo", "v2"),
        "engine.swap.after_exchange=abort",
    );
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    let journal = s.journals().pop().unwrap();
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

    // E11: `config set` runs no discovery, and recovers under the lock alone.
    let log = tlog!();
    let mut differential = Differential::new(&log, &s.home, &["_retail_"], &[]);
    differential.predict_unknown(&log, "a step in neither state, by config set");
    let r = s.home.run(&["config", "set", "running_policy", "refuse"]);
    let envelope = r.expect(0);
    assert_eq!(
        differential.compare(&log, &envelope, &s.journals(), &Caller::Under),
        1
    );
    let pending: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "journal_pending")
        .filter_map(|w| w["message"].as_str())
        .collect();
    assert!(
        pending.iter().any(|m| m.contains("could not be recovered")),
        "{}",
        r.context()
    );
    assert!(journal.exists(), "kept for repair");
    let config = fs::read_to_string(s.home.env.config().join("omaddons/config.toml")).unwrap();
    assert!(config.contains("refuse"), "{config}");
}

// ---------------------------------------------------------------------------------------
// Whole-`WTF/` restores (M3-D24). No command runs one before `backup restore` (D8), so the
// engine's restore runs in a child process of this test binary (`child_restore`), in the
// plan order D8 follows; every recovery is the binary's. The oracle: `WTF/` wholly old or
// wholly new at every crash, the direction its journal's durable phase says, history
// naming the restore exactly when it stands, the old tree kept as `WTF.previous`, nothing
// staged left once the sweep has run, and recovery that changes nothing when run again.

#[cfg(test)]
mod restore_support {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    use common::trees::tree_hash;
    use common::wtf::{Wtf, writable_again};
    use omaddons::backup::BackupKind;
    use omaddons::discover::DiscoverySource;
    use omaddons::discover::battlenet::{BattlenetHints, discover_root};
    use omaddons::discover::registry::reconcile;
    use omaddons::engine::inspect::ArchiveLimits;
    use omaddons::engine::journal::{RecoveryEnv, recover_all};
    use omaddons::engine::restore::{Probe, RestoreRequest, plan, preflight, reserve, restore};
    use omaddons::engine::restore_addons::{self, AddonSv, AddonsRequest};
    use omaddons::engine::retention::RetentionLimits;
    use omaddons::engine::{EngineCtx, SvPolicy, UndoRequest, UndoSelector};
    use omaddons::error::{Error, Result};
    use omaddons::fsx::{GlobalLock, InflightLock, LockUse, PruneReason, ScopeTable, prune};
    use omaddons::model::{Actor, Channel, FolderName, RunId, TargetKey, TxId};
    use omaddons::net::{Http, Request, Response};
    use omaddons::output::ProgressSink;
    use omaddons::rules::RulesTable;
    use omaddons::sources::{SourceConfigView, SourceRegistry};
    use omaddons::state::config::RunningPolicy;
    use omaddons::state::paths::{ClientPaths, Paths};
    use omaddons::state::registry::load_registry;
    use serde_json::{Value, json};

    use super::*;

    /// The account-wide SavedVariables a play session rewrites.
    const PLAYED: &str = "Account/FAKEACCT/SavedVariables/Foo.lua";

    /// A restore makes no request.
    struct NoNetwork;

    impl Http for NoNetwork {
        fn send(&self, req: Request) -> Result<Response> {
            Err(Error::internal(format!("a restore requested {}", req.url)))
        }
    }

    /// The engine's restore of the newest manual backup onto the retail client under the
    /// Battle.net root `root`, in D8's plan order: reserve, plan, a transaction id and its
    /// in-flight lock, the probing preflight (whose leftovers are pruned on a refusal, as
    /// the command's cleanup will), then the engine, with `OMADDONS_RESTORE_FAILPOINTS`
    /// armed around it alone. With `OMADDONS_RESTORE_MODE=undo`, the engine's undo of the
    /// newest transaction instead (the restore a scene committed); with `addon`, the
    /// partial restore of Foo's SavedVariables (D7). Its outcome as JSON.
    pub fn engine_restore(root: &Path) -> Value {
        let var = |key: &str| PathBuf::from(std::env::var_os(key).unwrap());
        let paths = Paths::with_roots(
            var("HOME"),
            var("XDG_CONFIG_HOME"),
            var("XDG_STATE_HOME"),
            var("XDG_CACHE_HOME"),
        );
        let table = RulesTable::builtin();
        let hints = BattlenetHints::default();
        let (found, _) = discover_root(root, &table, &hints, "battlenet", &DiscoverySource::Scan);
        let registry = load_registry(&paths).unwrap().value;
        let discovered = reconcile(&registry, &found)
            .targets
            .into_iter()
            .find(|d| d.target.flavor_dir.as_deref() == Some("_retail_"))
            .unwrap();
        let target = &discovered.target;
        let client = ClientPaths::new(&target.client_dir);
        let failed = |e: Error| {
            json!({
                "ok": false,
                "code": e.code.wire(),
                "retryable": e.retryable,
                "message": e.message,
                "details": e.details,
            })
        };
        let sources = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        let argv: Vec<String> = ["omaddons", "backup", "restore"]
            .map(str::to_owned)
            .to_vec();
        let progress = ProgressSink::none();
        let ctx = EngineCtx {
            paths: &paths,
            http: &NoNetwork,
            registry: &sources,
            table: &table,
            policy: RunningPolicy::default(),
            limits: ArchiveLimits::default(),
            retention: RetentionLimits::default(),
            actor: Actor::Agent,
            argv: &argv,
            command: "backup restore",
            // Read live from the fake /proc, so a scene can start the game mid-run.
            running: None,
            known_targets: std::slice::from_ref(target),
            cache: None,
            progress: &progress,
            catalog: None,
            default_channel: Channel::Stable,
            downloads: None,
            indexes: None,
            notes: None,
            binding: None,
            auto_backup: None,
            applying: None,
        };
        let spec = std::env::var("OMADDONS_RESTORE_FAILPOINTS").unwrap_or_default();
        let armed = |engine: &mut dyn FnMut() -> Result<Value>| -> Value {
            let result = match std::env::var_os("OMADDONS_FAILPOINT_DIR") {
                Some(dir) => omaddons::fsx::with_spec_in(Path::new(&dir), &spec, engine),
                None => omaddons::fsx::with_spec(&spec, engine),
            };
            result.unwrap_or_else(failed)
        };
        // Recovery alone, as a commit window starts it, with the failpoints armed around
        // it: every hit counted is the recovery's own.
        if std::env::var("OMADDONS_RESTORE_MODE").as_deref() == Ok("recover") {
            let lock = GlobalLock::acquire(&paths, "recovery").unwrap();
            let scopes =
                ScopeTable::new(&paths, std::slice::from_ref(&target.client_dir), None).unwrap();
            let own: Vec<TargetKey> = target.key.into_iter().collect();
            return armed(&mut || {
                let under_lock = RecoveryEnv {
                    lock: &lock,
                    paths: &paths,
                    scopes: &scopes,
                    own: &own,
                    sv_writable: &|_| true,
                };
                recover_all(&under_lock).map(|warnings| {
                    let codes: Vec<&str> = warnings.iter().map(|w| w.code.wire()).collect();
                    json!({"ok": true, "warnings": codes})
                })
            });
        }
        if std::env::var("OMADDONS_RESTORE_MODE").as_deref() == Ok("undo") {
            let req = UndoRequest {
                selector: UndoSelector::Latest,
                sv_policy: SvPolicy::Auto,
                dry_run: false,
                while_running: false,
                reason: None,
                targets: vec![discovered.clone()],
                run: RunId::new(),
            };
            return armed(&mut || {
                omaddons::engine::undo(&req, &ctx).map(|done| {
                    let (tx, held) = (done.outcome.tx, done.outcome.lock_held_ms);
                    json!({"ok": true, "tx": tx, "lock_held_ms": held})
                })
            });
        }
        let backup = omaddons::backup::list(&client)
            .0
            .into_iter()
            .find(|b| {
                b.meta
                    .as_ref()
                    .is_some_and(|m| m.kind == BackupKind::Manual)
            })
            .unwrap();
        let reservation = match reserve(&backup) {
            Ok(reservation) => reservation,
            Err(e) => return failed(e),
        };
        let planned = match plan(&paths, target, &backup) {
            Ok(planned) => planned,
            Err(e) => return failed(e),
        };
        let tx = TxId::new();
        let inflight = InflightLock::create(&paths, &tx).unwrap();
        let scopes =
            ScopeTable::new(&paths, std::slice::from_ref(&target.client_dir), None).unwrap();
        let probe = client.stage_wtf(&tx).join("probe");
        let probe = Probe {
            scopes: &scopes,
            dir: &probe,
        };
        let topology = match preflight(&client, &backup.wtf(), Some(probe)) {
            Ok(topology) => topology,
            Err(e) => {
                let left: Vec<PathBuf> = [client.stage_wtf(&tx), paths.inflight_lock(&tx)]
                    .into_iter()
                    .filter(|p| p.exists())
                    .collect();
                prune(
                    &scopes,
                    PruneReason::PostCommitCleanup,
                    &left,
                    LockUse::Own(&inflight),
                )
                .unwrap();
                return failed(e);
            }
        };
        if std::env::var("OMADDONS_RESTORE_MODE").as_deref() == Ok("addon") {
            let addons = [AddonSv {
                addon: "Foo".to_owned(),
                folders: vec![FolderName::new("Foo").unwrap()],
            }];
            let planned = match restore_addons::plan(&paths, target, &backup, &addons) {
                Ok(planned) => planned,
                Err(e) => return failed(e),
            };
            let mut req = Some(AddonsRequest {
                target: &discovered,
                backup: &backup,
                reservation,
                topology,
                tx,
                inflight,
                plan: &planned,
                addons: &addons,
                reason: None,
                run: RunId::new(),
            });
            return armed(&mut || {
                let req = req
                    .take()
                    .ok_or_else(|| Error::internal("restored twice"))?;
                restore_addons::restore(&ctx, req).map(
                    |done| json!({"ok": true, "tx": done.tx, "lock_held_ms": done.lock_held_ms}),
                )
            });
        }
        let mut req = Some(RestoreRequest {
            target: &discovered,
            backup: &backup,
            reservation,
            topology,
            tx,
            inflight,
            plan: &planned,
            before_restore: None,
            reason: None,
            run: RunId::new(),
        });
        armed(&mut || {
            let req = req
                .take()
                .ok_or_else(|| Error::internal("restored twice"))?;
            restore(&ctx, req)
                .map(|done| json!({"ok": true, "tx": done.tx, "lock_held_ms": done.lock_held_ms}))
        })
    }

    /// A retail client whose `WTF/` was backed up by `backup create`, then played on (a
    /// file changed, one created, `Config.wtf` gone) or, `live: false`, removed. The tree:
    /// two accounts, three characters, a symlink, an empty directory and a `0500` one,
    /// under fake names.
    pub struct RestoreScene {
        pub s: Scene,
        /// `WTF/` as backed up: what a restore puts back.
        pub backed_up: Option<String>,
        /// `WTF/` as it is now: what a restore replaces (`None`: there is none).
        pub played: Option<String>,
    }

    impl RestoreScene {
        pub fn new(live: bool) -> Self {
            let s = Scene::new();
            let wtf = Wtf::at(s.home.client("_retail_"))
                .config_wtf("SET locale \"enUS\"\n")
                .saved_variables("Foo", 300)
                .character("FakeCharTwo")
                .saved_variables("Baz", 200)
                .account("FAKEACCTTWO")
                .character("FakeCharThree")
                .saved_variables("Bar", 100)
                .symlink("Account/FAKEACCT/SavedVariables/Foo.link", "Foo.lua")
                .mode("Account/FAKEACCT/Empty", 0o755)
                .file("Account/FAKEACCT/Sealed/Sealed.lua", b"Sealed = true\n")
                .mode("Account/FAKEACCT/Sealed", 0o500)
                .build();
            s.run(&s.args(&["backup", "create"]), "").expect(0);
            let backed_up = s.wtf();
            if live {
                fs::write(wtf.join(PLAYED), "FooDB = { played = true }\n").unwrap();
                let created = "Account/FAKEACCT/SavedVariables/New.lua";
                fs::write(wtf.join(created), "NewDB = {}\n").unwrap();
                fs::remove_file(wtf.join("Config.wtf")).unwrap();
            } else {
                writable_again(&wtf);
                fs::remove_dir_all(&wtf).unwrap();
            }
            let played = s.wtf();
            Self {
                s,
                backed_up,
                played,
            }
        }

        pub fn client(&self) -> PathBuf {
            self.s.home.client("_retail_")
        }

        /// `<meta>/stage`, where a restore stages its copy.
        pub fn stage_root(&self) -> PathBuf {
            self.client().join("Interface/.omaddons/stage")
        }

        /// The `recovered` backups, each as its id and its `WTF/` tree hash.
        pub fn recovered(&self) -> Vec<(String, String)> {
            listed(&self.client().join("Interface/.omaddons/backups"))
                .into_iter()
                .filter(|dir| {
                    fs::read(dir.join("backup.json"))
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                        .is_some_and(|meta| meta["kind"] == "recovered")
                })
                .map(|dir| {
                    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
                    (id, tree_hash(&dir.join("WTF")))
                })
                .collect()
        }

        /// The backup the scene restores.
        pub fn backup_dir(&self) -> PathBuf {
            let mut backups = listed(&self.client().join("Interface/.omaddons/backups"));
            assert_eq!(backups.len(), 1, "{backups:?}");
            backups.remove(0)
        }

        /// The argv of the command that recovers: an install of Bar.
        pub fn install_bar(&self) -> Vec<String> {
            self.s.install_args("gh:o/bar", "bar")
        }

        /// The install of Bar, which recovers, with E11's differential around it: what
        /// `diagnose` predicts (its game running when `running`) is held to what it did.
        /// The run, for the scene's own oracle.
        pub fn recover_predicted(
            &self,
            log: &TestLog,
            label: &str,
            running: bool,
        ) -> common::home::Run {
            let games: &[&str] = if running { &["_retail_"] } else { &[] };
            let mut differential = Differential::new(log, &self.s.home, &["_retail_"], games);
            differential.predict(log, label);
            let run = self.s.run(&self.install_bar(), "");
            let caller = differential.commit(&["retail"]);
            let compared = differential.compare(log, &run.envelope(), &self.s.journals(), &caller);
            assert!(compared > 0, "{label}: a journal was compared");
            run
        }

        /// The child running the engine's restore: `engine` armed around the engine call
        /// alone, `env` added to its environment (`OMADDONS_FAILPOINTS` reaches the
        /// preflight too), `bind` (when given) mounted over itself in a user and mount
        /// namespace first. Returns the command and where it writes its outcome.
        fn child(
            &self,
            engine: &str,
            env: &[(&str, &str)],
            bind: Option<&Path>,
        ) -> (Command, PathBuf) {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let out = self.s.home.env.root().join(format!(
                "restore-{}.json",
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let args = [
                "engine_crash::child_restore",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ];
            let mut cmd = match bind {
                None => Command::new(std::env::current_exe().unwrap()),
                Some(dir) => common::mounts::under_bind(dir, std::env::current_exe().unwrap()),
            };
            cmd.args(args);
            self.s.home.env.apply_to(&mut cmd);
            if let Some(proc) = &self.s.home.proc {
                cmd.env("OMADDONS_PROC_ROOT", proc);
            }
            cmd.env("OMADDONS_RESTORE_ROOT", &self.s.home.root)
                .env("OMADDONS_RESTORE_OUT", &out)
                .env("OMADDONS_RESTORE_FAILPOINTS", engine)
                .envs(env.iter().copied())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            (cmd, out)
        }

        /// The engine's restore in a child process (see [`Self::child`]): its outcome, or
        /// `None` when an `abort` killed it.
        pub fn restore(&self, engine: &str, env: &[(&str, &str)]) -> Option<Value> {
            let (mut cmd, out) = self.child(engine, env, None);
            outcome(&cmd.output().unwrap(), &out)
        }

        /// The engine's undo of the newest transaction in a child process (see
        /// [`Self::child`]): its outcome, or `None` when an `abort` killed it.
        pub fn undo(&self, engine: &str, env: &[(&str, &str)]) -> Option<Value> {
            let mut env = env.to_vec();
            env.push(("OMADDONS_RESTORE_MODE", "undo"));
            self.restore(engine, &env)
        }

        /// The engine's recovery alone in a child process (see [`Self::child`]): its
        /// outcome, or `None` when an `abort` killed it.
        pub fn recover(&self, engine: &str) -> Option<Value> {
            self.restore(engine, &[("OMADDONS_RESTORE_MODE", "recover")])
        }

        /// The tree hash of every place a `WTF/` version can be while recovery preserves
        /// one: the live tree, each stage's `WTF/` and assembled `WTF/`, each backup's.
        pub fn versions(&self) -> Vec<String> {
            let meta = self.client().join("Interface/.omaddons");
            let mut trees = vec![self.client().join("WTF")];
            for stage in listed(&meta.join("stage")) {
                trees.push(stage.join("WTF"));
                trees.extend(
                    listed(&stage.join("recovered"))
                        .iter()
                        .map(|a| a.join("WTF")),
                );
            }
            trees.extend(listed(&meta.join("backups")).iter().map(|b| b.join("WTF")));
            trees
                .iter()
                .filter(|tree| tree.is_dir())
                .map(|tree| tree_hash(tree))
                .collect()
        }

        /// The id of the preservation intent the pending journal holds, if any.
        pub fn intent(&self) -> Option<String> {
            self.s.journals().iter().find_map(|journal| {
                fs::read_to_string(journal)
                    .ok()?
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .find(|line| line["type"] == "preserve")
                    .and_then(|line| line["id"].as_str().map(str::to_owned))
            })
        }

        /// A play session on the restored tree: a file rewritten.
        pub fn play_on_restored(&self) {
            let path = self.client().join("WTF").join(PLAYED);
            fs::write(path, "FooDB = { since_the_restore = true }\n").unwrap();
        }

        /// The same with `dir` bind-mounted over itself, so it is a mount point of its own.
        pub fn restore_with_bind(&self, dir: &Path) -> Option<Value> {
            let (mut cmd, out) = self.child("", &[], Some(dir));
            outcome(&cmd.output().unwrap(), &out)
        }

        /// The same in the background, for a scene that acts at a `pause`.
        pub fn spawn_restore(&self, engine: &str, env: &[(&str, &str)]) -> (Child, PathBuf) {
            let (mut cmd, out) = self.child(engine, env, None);
            (cmd.spawn().unwrap(), out)
        }

        /// A spawned restore's outcome, once it ends.
        pub fn finish((child, out): (Child, PathBuf)) -> Option<Value> {
            outcome(&child.wait_with_output().unwrap(), &out)
        }

        /// Moves what a crash before `prepared` leaves (stage dirs, in-flight locks) two
        /// hours back, past the orphan sweep's `ORPHAN_MIN_AGE`.
        pub fn age_leftovers(&self) {
            let past = SystemTime::now() - Duration::from_secs(2 * 3600);
            for dir in [self.stage_root(), self.s.home.state().join("inflight")] {
                for path in listed(&dir) {
                    fs::File::open(&path).unwrap().set_modified(past).unwrap();
                }
            }
        }
    }

    impl Drop for RestoreScene {
        /// The tempdir holds `0500` directories: they are given back first.
        fn drop(&mut self) {
            writable_again(self.s.home.env.root());
        }
    }

    /// A child's outcome: `None` when a signal (an `abort`) killed it.
    fn outcome(output: &Output, out: &Path) -> Option<Value> {
        output.status.code()?;
        assert!(
            output.status.success(),
            "the restore child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(serde_json::from_slice(&fs::read(out).unwrap()).unwrap())
    }

    /// The entries of `dir`, none when it is absent.
    pub fn listed(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|l| l.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }

    /// What one restore run did.
    #[derive(Debug, Clone, Copy)]
    pub struct Restored {
        /// An `abort` killed it.
        pub crashed: bool,
        /// It returned a committed restore.
        pub ok: bool,
        /// `WTF/` was the backup's when it ended.
        pub new_at_crash: bool,
        /// It left a journal for the next command.
        pub pending: bool,
        /// After recovery, `WTF/` is the backup's.
        pub went_new: bool,
        /// The first recovering command died mid-recovery (its `recover` failpoints).
        pub recovering_crashed: bool,
        /// E11: the run left a journal, and its prediction was compared.
        pub with_journal: bool,
        pub compared: bool,
    }

    impl Restored {
        /// One more state of a matrix's E11 tally.
        pub fn tally(&self, tally: &mut Tally) {
            tally.count(self.with_journal, self.compared);
        }
    }

    /// A restore run with `engine` armed (a fresh scene, `live` or without `WTF/`), then
    /// the oracle. `WTF/` is wholly old or wholly new when it ends; a reader reports an
    /// unfinished restore and changes nothing; the next command that takes the lock
    /// recovers (with `recover` armed it may die mid-recovery, keeping the journal, and
    /// the command after it finishes) in the direction the journal's phase says; history
    /// names the restore exactly when it stands, with its old tree kept as
    /// `WTF.previous`; no snapshot of a restore that never committed is left; what a crash
    /// before `prepared` staged goes with the sweep once old; and two more commands that
    /// take the lock change nothing of `WTF/`.
    pub fn crash(log: &TestLog, engine: &str, recover: &str, live: bool) -> Restored {
        crash_op(log, Op::Restore, engine, recover, live)
    }

    /// What a restore scene runs in its child.
    #[derive(Debug, Clone, Copy)]
    pub enum Op {
        /// The restore of the backup over the played-on tree (or onto none).
        Restore,
        /// The undo of that restore, committed first and played on since: the restored
        /// tree goes, the one from before it comes back (or, after a restore onto a client
        /// that had none, no `WTF/`).
        UndoRestore,
    }

    /// What one recovery with a fault armed did.
    #[derive(Debug, Clone, Copy)]
    pub struct Preserving {
        /// It crashed or failed: the journal stayed for the next one.
        pub stopped: bool,
        /// The journal held a preservation intent then.
        pub intent: bool,
        /// E11: a journal was left for the binary, and its prediction was compared.
        pub with_journal: bool,
        pub compared: bool,
    }

    impl Preserving {
        /// One more state of a matrix's E11 tally.
        pub fn tally(&self, tally: &mut Tally) {
            tally.count(self.with_journal, self.compared);
        }
    }

    /// A restore crashed after its exchange, then played on; recovered by a child with
    /// `engine` armed (a crash or a failure at one step of the walk or the protocol), then
    /// by the binary. Whatever happened, both versions (the pre-restore tree and the
    /// played-on one) still exist somewhere, and every backup listed is whole (described,
    /// with its manifest); a failure is `preservation_incomplete` and names no account,
    /// realm or character. The next recovery completes: the pre-restore tree live, exactly
    /// one `recovered` backup holding the played-on tree byte for byte, under the id the
    /// intent reserved when there was one, nothing left in the stage or the journal; two
    /// more commands that take the lock change nothing of that.
    pub fn preserve_crash(log: &TestLog, engine: &str) -> Preserving {
        let label = format!("a played-on restore recovered with {engine:?}, then by the binary");
        log.step(&label);
        let r = RestoreScene::new(true);
        assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
        let config = r.client().join("WTF/Config.wtf");
        fs::write(&config, "SET locale \"enUS\"\nSET played \"1\"\n").unwrap();
        let (pre, played_on) = (r.played.clone().unwrap(), r.s.wtf().unwrap());

        let outcome = r.recover(engine);
        let context = |what: &str| format!("recovery with {engine:?}: {what}\noutcome {outcome:?}");
        let stopped = outcome.as_ref().is_none_or(|o| o["ok"] == false);
        if let Some(o) = outcome.as_ref().filter(|o| o["ok"] == false) {
            common::privacy::assert_private(&o.to_string());
            assert!(
                matches!(
                    o["details"]["reason"].as_str(),
                    Some("preservation_incomplete")
                ) || o["code"] == "io",
                "{}",
                context("a failure stops as preservation_incomplete, or the walk's own error")
            );
        }
        let versions = r.versions();
        for (what, version) in [("pre-restore", &pre), ("played-on", &played_on)] {
            assert!(
                versions.contains(version),
                "{}",
                context(&format!("the {what} tree is gone"))
            );
        }
        let backups = listed(&r.client().join("Interface/.omaddons/backups"));
        for backup in &backups {
            assert!(
                backup.join("backup.json").is_file() && backup.join("manifest.json").is_file(),
                "{}",
                context(&format!(
                    "a half-made backup is listed: {}",
                    backup.display()
                ))
            );
        }
        let intent = r.intent();

        // E11: the armed recovery is no prediction's to match; the binary's is.
        let mut differential = Differential::new(log, &r.s.home, &["_retail_"], &[]);
        differential.predict(log, &label);
        let with_journal = !r.s.journals().is_empty();
        let bar = r.s.run(&r.install_bar(), "");
        let envelope = bar.expect(0);
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &envelope, &r.s.journals(), &caller) > 0;
        common::privacy::assert_private_all(&bar.stdout, &bar.stderr);
        assert!(r.s.journals().is_empty(), "{}", context("journals left"));
        assert_eq!(
            r.s.wtf(),
            Some(pre.clone()),
            "{}",
            context("not rolled back")
        );
        let kept = r.recovered();
        assert_eq!(kept.len(), 1, "{}", context(&format!("recovered {kept:?}")));
        assert_eq!(
            kept[0].1,
            played_on,
            "{}",
            context("not the played-on tree")
        );
        if let Some(id) = &intent {
            assert_eq!(
                &kept[0].0,
                id,
                "{}",
                context("not the id the intent reserved")
            );
        }
        assert!(
            listed(&r.stage_root()).is_empty(),
            "{}",
            context("stage left")
        );
        for args in [["pin", "Bar"], ["unpin", "Bar"]] {
            r.s.run(&r.s.args(&args), "").expect(0);
            assert_eq!(
                r.recovered(),
                kept,
                "{}",
                context("a later recovery changed it")
            );
            assert_eq!(r.s.wtf(), Some(pre.clone()));
        }
        Preserving {
            stopped,
            intent: intent.is_some(),
            with_journal,
            compared,
        }
    }

    /// [`crash`] for `op`, with E11's differential before the recovering command.
    pub fn crash_op(log: &TestLog, op: Op, engine: &str, recover: &str, live: bool) -> Restored {
        let label = format!("{op:?} with {engine:?} (live: {live}), then {recover:?}");
        log.step(&label);
        let r = RestoreScene::new(live);
        let s = &r.s;
        let (old, new, kind) = match op {
            Op::Restore => (r.played.clone(), r.backed_up.clone(), "restore"),
            Op::UndoRestore => {
                let restored = r.restore("", &[]).unwrap();
                assert_eq!(restored["ok"], true, "{restored}");
                r.play_on_restored();
                (s.wtf(), r.played.clone(), "undo")
            }
        };
        let (old_generation, old_history) = (s.generation(), s.history());
        let outcome = match op {
            Op::Restore => r.restore(engine, &[]),
            Op::UndoRestore => r.undo(engine, &[]),
        };
        let journal_text = || -> Vec<String> {
            s.journals()
                .iter()
                .map(|p| fs::read_to_string(p).unwrap_or_default())
                .collect()
        };
        let context = |what: &str| {
            format!(
                "{op:?} with {engine:?}, then {recover:?}: {what}\noutcome {outcome:?}\nold {old:?}\nnew {new:?}\nnow {:?}\njournals {:?}",
                s.wtf(),
                journal_text()
            )
        };
        let crashed = outcome.is_none();
        let ok = outcome.as_ref().is_some_and(|o| o["ok"] == true);
        if !crashed && engine.contains("=abort") {
            assert!(ok, "{}", context("not on this path, so it must succeed"));
        }

        let at_crash = s.wtf();
        assert!(
            at_crash == old || at_crash == new,
            "{}",
            context("WTF/ is neither old nor new")
        );
        let pending_before = support::pending(s);
        let swapped = journal_text()
            .iter()
            .any(|t| t.contains("\"type\":\"swapped\""));

        // A reader reports the unfinished restore and changes nothing.
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

        // E11: what `diagnose` predicts of the recovering command.
        let mut differential = Differential::new(log, &s.home, &["_retail_"], &[]);
        differential.predict(log, &label);

        // The next command that takes the lock recovers; one armed with `recover` may die
        // mid-recovery, and the journal stays for the command after it.
        let install_bar = r.install_bar();
        let mut recovering_crashed = false;
        let mut recovered_by = None;
        if !recover.is_empty() {
            let first = s.run(&install_bar, recover);
            recovering_crashed = first.code == -1;
            if recovering_crashed {
                assert_eq!(
                    support::pending(s),
                    pending_before,
                    "{}",
                    context(&format!(
                        "a recovery that died dropped the journal\n{}",
                        first.context()
                    ))
                );
                let mid = s.wtf();
                assert!(
                    mid == old || mid == new,
                    "{}",
                    context("WTF/ split by a recovery that died")
                );
                differential.predict(log, &format!("{label}, after the recovery that died"));
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &bar, &s.journals(), &caller) > 0;
        assert_eq!(
            warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
            pending_before > 0,
            "{}",
            context("recovery warning")
        );
        assert!(s.journals().is_empty(), "{}", context("journals left"));

        let now = s.wtf();
        let went_new = now == new;
        assert!(went_new || now == old, "{}", context("WTF/ split"));
        if pending_before > 0 {
            assert_eq!(
                went_new,
                swapped,
                "{}",
                context("recovery went against the journal's phase")
            );
        }
        let mut history = old_history.clone();
        if went_new {
            history.push(kind.to_owned());
        }
        history.push("install".to_owned());
        assert_eq!(s.history(), history, "{}", context("history"));
        assert_eq!(
            s.generation(),
            old_generation + u64::from(went_new) + 1,
            "{}",
            context("generation")
        );
        let snapshots = r.client().join("Interface/.omaddons/snapshots");
        if went_new {
            // Both halves named `<tx>.wtf`, which a pre-M3 build's orphan sweep never takes
            // (omad-aepa).
            let tx = s.history_txs()[old_history.len()].clone();
            let named = format!("{tx}.wtf");
            let previous = snapshots.join(&named).join("WTF.previous");
            assert_eq!(
                previous.is_dir().then(|| tree_hash(&previous)),
                old,
                "{}",
                context("WTF.previous is not the tree it replaced")
            );
            let state = s.home.state().join("snapshots");
            assert!(
                state.join(&named).is_dir(),
                "{}",
                context("the restore's state snapshot")
            );
            assert!(
                !snapshots.join(&tx).exists() && !state.join(&tx).exists(),
                "{}",
                context("a snapshot half under the bare name")
            );
        }
        // No snapshot of a restore that never committed is left. A recovering command that
        // died leaves the copies its own install took before the lock, holding no tree.
        let orphans = s.orphan_snapshots();
        assert!(
            orphans.is_empty() || recovering_crashed,
            "{}",
            context(&format!(
                "snapshots of a restore that never committed {orphans:?}"
            ))
        );
        for dir in &orphans {
            assert!(
                !dir.join("WTF.previous").exists(),
                "{}",
                context(&format!("{} holds a WTF/", dir.display()))
            );
        }

        // What a crash left staged or snapshotted goes with the sweep once it is old (a
        // stored `0500` directory included), and recovery run again changes nothing.
        r.age_leftovers();
        s.age_orphan_snapshots();
        s.run(&s.args(&["pin", "Bar"]), "").expect(0);
        let left = listed(&r.stage_root());
        assert!(
            left.is_empty(),
            "{}",
            context(&format!("staged leftovers {left:?}"))
        );
        let orphans = s.orphan_snapshots();
        assert!(
            orphans.is_empty(),
            "{}",
            context(&format!("aged leftovers survived the sweep {orphans:?}"))
        );
        s.run(&s.args(&["unpin", "Bar"]), "").expect(0);
        assert_eq!(s.wtf(), now, "{}", context("a later command changed WTF/"));
        assert!(s.journals().is_empty(), "{}", context("journals after"));

        Restored {
            crashed,
            ok,
            new_at_crash: at_crash == new,
            pending: pending_before > 0,
            went_new,
            recovering_crashed,
            with_journal: !journals.is_empty(),
            compared,
        }
    }
}

/// Every failpoint as a crash of the undo of a restore that was played on since: the
/// restored tree goes and the one from before comes back, wholly or not at all.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_undo_restore() {
    use restore_support::{Op, crash_op};

    let log = tlog!();
    let mut tally = Tally::default();
    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = crash_op(
            &log,
            Op::UndoRestore,
            &format!("{failpoint}=abort"),
            "",
            true,
        );
        done.tally(&mut tally);
        if done.crashed {
            crashed_at.push(*failpoint);
        }
        if matches!(
            *failpoint,
            "engine.restore.exchanged" | "engine.restore.snapshotted"
        ) {
            assert!(
                done.new_at_crash && !done.went_new,
                "{failpoint}: renamed but not swapped must roll back: {done:?}"
            );
        }
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "undo of a restore: crashed at {} of {} failpoints: {crashed_at:?}\n",
            crashed_at.len(),
            FAILPOINTS.len()
        ),
    );
    tally.assert_complete(&log, "UndoRestore");
    for on_path in [
        "engine.restore.staged",
        "engine.lock.acquired",
        "engine.revalidated",
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.history.appended",
        "engine.committed",
    ] {
        assert!(
            crashed_at.contains(&on_path),
            "{on_path} is on an undo of a restore's path: {crashed_at:?}"
        );
    }
}

/// The undo of a restore onto a client that had no `WTF/`: one rename, the restored tree
/// out to the undo's snapshot, crashed at each point of its window and before it.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_undo_restore_onto_no_wtf() {
    use restore_support::{Op, crash_op};

    let log = tlog!();
    let mut tally = Tally::default();
    for failpoint in [
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.committed",
    ] {
        let done = crash_op(
            &log,
            Op::UndoRestore,
            &format!("{failpoint}=abort"),
            "",
            false,
        );
        done.tally(&mut tally);
        assert!(done.crashed, "{failpoint}: {done:?}");
    }
    let mut renames = Vec::new();
    for n in 1..=3 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        let done = crash_op(&log, Op::UndoRestore, &spec, "", false);
        done.tally(&mut tally);
        if !done.crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1], "one rename out");
    tally.assert_complete(&log, "UndoRestore onto no WTF/");
}

/// A crash before each journaled rename of the undo of a restore, and in its recovery:
/// after each rename its rollback undoes, and as a roll-forward starts.
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_every_rename_and_recovery_of_an_undo_of_a_restore() {
    use restore_support::{Op, crash_op};

    let log = tlog!();
    let mut tally = Tally::default();
    let mut renames = Vec::new();
    for n in 1..=4 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        let done = crash_op(&log, Op::UndoRestore, &spec, "", true);
        done.tally(&mut tally);
        if !done.crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1, 2]);
    let mut recoveries = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = crash_op(
            &log,
            Op::UndoRestore,
            "engine.restore.snapshotted=abort",
            &recover,
            true,
        );
        done.tally(&mut tally);
        if !done.recovering_crashed {
            break;
        }
        recoveries.push(k);
    }
    assert_eq!(recoveries, [1, 2]);
    let done = crash_op(
        &log,
        Op::UndoRestore,
        "engine.swapped=abort",
        "engine.recover.forward=abort",
        true,
    );
    done.tally(&mut tally);
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
    tally.assert_complete(&log, "UndoRestore renames and recoveries");
}

/// I/O failures where the undo of a restore writes, each injected once at every hit in
/// turn: `WTF/` stays wholly old or wholly new and what is left recovers. The undo runs
/// its own probing preflight, so its first atomic writes (the probe's files) fail it
/// before anything is staged; the record's, after `swapped`, roll forward.
#[cfg(debug_assertions)]
#[test]
fn io_failures_in_an_undo_of_a_restore_leave_wtf_whole() {
    use restore_support::{Op, crash_op};

    let log = tlog!();
    let mut tally = Tally::default();
    for (failpoint, most) in [
        ("fsx.clone_file.before", 2),
        ("fsx.append_durable.before", 8),
        ("fsx.rename_with.before", 8),
        ("fsx.write_atomic.after", 4),
    ] {
        let mut failed = Vec::new();
        for n in 1..=most {
            let done = crash_op(
                &log,
                Op::UndoRestore,
                &format!("{failpoint}=error@{n}"),
                "",
                true,
            );
            done.tally(&mut tally);
            if !done.ok {
                failed.push((n, done.pending, done.went_new));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("undo of a restore: {failpoint} failed it at {failed:?}\n"),
        );
        assert!(!failed.is_empty(), "{failpoint} is on its path");
    }
    tally.assert_complete(&log, "UndoRestore I/O failures");
}

/// The undo of a restore crashed after its exchange, the game started since: the next
/// command refuses (`game_running`, naming the journal) and moves nothing; once the game
/// exits, the next one rolls it back to the restored tree. E11 predicts both.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_undo_of_a_restore_waits_while_the_game_runs() {
    let log = tlog!();
    let mut r = restore_support::RestoreScene::new(true);
    assert_eq!(r.restore("", &[]).unwrap()["ok"], true);
    r.play_on_restored();
    let restored = r.s.wtf();
    assert!(r.undo("engine.restore.exchanged=abort", &[]).is_none());
    assert_eq!(r.s.wtf(), r.played, "the exchange ran");
    r.s.home.retail_running();
    let mut differential = Differential::new(&log, &r.s.home, &["_retail_"], &["_retail_"]);
    differential.predict(&log, "the game runs");
    let refused = r.s.run(&r.install_bar(), "");
    let error = &refused.envelope()["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    let caller = differential.commit(&["retail"]);
    differential.compare(&log, &refused.envelope(), &r.s.journals(), &caller);
    assert_eq!(r.s.wtf(), r.played, "nothing moves while it runs");
    r.s.home.nothing_running();
    let mut differential = Differential::new(&log, &r.s.home, &["_retail_"], &[]);
    differential.predict(&log, "the game exited");
    let envelope = r.s.run(&r.install_bar(), "").expect(0);
    differential.compare(&log, &envelope, &r.s.journals(), &caller);
    assert_eq!(r.s.wtf(), restored, "rolled back once the game exited");
    assert!(r.s.journals().is_empty());
}

/// The engine's whole restore, as a child process of the restore scenes: the scene's
/// Battle.net root in `OMADDONS_RESTORE_ROOT`, its outcome written to
/// `OMADDONS_RESTORE_OUT`. Run on its own it does nothing.
#[cfg(debug_assertions)]
#[test]
#[ignore = "a child process of the restore scenes"]
fn child_restore() {
    let (Some(root), Some(out)) = (
        std::env::var_os("OMADDONS_RESTORE_ROOT"),
        std::env::var_os("OMADDONS_RESTORE_OUT"),
    ) else {
        return;
    };
    let outcome = restore_support::engine_restore(std::path::Path::new(&root));
    fs::write(out, outcome.to_string()).unwrap();
}

/// Every failpoint as a crash of a restore over a played-on `WTF/`. The two renames
/// having run never selects roll-forward on their own: only `swapped` does.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_restore() {
    let log = tlog!();
    let mut tally = Tally::default();
    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = restore_support::crash(&log, &format!("{failpoint}=abort"), "", true);
        done.tally(&mut tally);
        if done.crashed {
            crashed_at.push(*failpoint);
        }
        if matches!(
            *failpoint,
            "engine.restore.exchanged" | "engine.restore.snapshotted"
        ) {
            assert!(
                done.new_at_crash && !done.went_new,
                "{failpoint}: renamed but not swapped must roll back: {done:?}"
            );
        }
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "restore: crashed at {} of {} failpoints: {crashed_at:?}\n",
            crashed_at.len(),
            FAILPOINTS.len()
        ),
    );
    tally.assert_complete(&log, "Restore");
    for on_path in [
        "engine.restore.staged",
        "engine.lock.acquired",
        "engine.revalidated",
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.record.written",
        "engine.history.appended",
        "engine.committed",
    ] {
        assert!(
            crashed_at.contains(&on_path),
            "{on_path} is on a restore's path: {crashed_at:?}"
        );
    }
}

/// A restore onto a client with no `WTF/` (its first step a `Noreplace` into place),
/// crashed at each point of its window.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_restore_onto_no_wtf() {
    let log = tlog!();
    let mut tally = Tally::default();
    for failpoint in [
        "engine.restore.staged",
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.history.appended",
        "engine.committed",
    ] {
        let done = restore_support::crash(&log, &format!("{failpoint}=abort"), "", false);
        done.tally(&mut tally);
        assert!(done.crashed, "{failpoint}: {done:?}");
    }
    tally.assert_complete(&log, "Restore onto no WTF/");
}

/// A crash before each journaled rename: the exchange and the move to `WTF.previous`
/// (only the first onto a client with no `WTF/`).
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_every_rename_of_a_restore() {
    let log = tlog!();
    let mut tally = Tally::default();
    for (live, renames) in [(true, 2), (false, 1)] {
        let mut crashed = Vec::new();
        for n in 1..=4 {
            let spec = format!("engine.swap.before_rename=abort@{n}");
            let done = restore_support::crash(&log, &spec, "", live);
            done.tally(&mut tally);
            if !done.crashed {
                break;
            }
            crashed.push(n);
        }
        assert_eq!(crashed, (1..=renames).collect::<Vec<_>>(), "live {live}");
    }
    tally.assert_complete(&log, "Restore renames");
}

/// Recovery of a restore crashed too: after each rename its rollback undoes, and as a
/// roll-forward starts. The command after it finishes.
#[cfg(debug_assertions)]
#[test]
fn a_crash_during_recovery_of_a_restore() {
    let log = tlog!();
    let mut tally = Tally::default();
    let mut crashed = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = restore_support::crash(&log, "engine.restore.snapshotted=abort", &recover, true);
        done.tally(&mut tally);
        if !done.recovering_crashed {
            break;
        }
        crashed.push(k);
    }
    assert_eq!(
        crashed,
        [1, 2],
        "both renames rolled back, each crashed after"
    );
    let done = restore_support::crash(
        &log,
        "engine.swapped=abort",
        "engine.recover.forward=abort",
        true,
    );
    done.tally(&mut tally);
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
    tally.assert_complete(&log, "Restore recoveries");
}

/// I/O failures where a restore writes (its staged copy's directories, files and modes,
/// the journal's appends, the two renames, the record), each injected once at every
/// hit in turn: `WTF/` stays wholly old or wholly new and what is left recovers. A
/// failure before `swapped` fails the restore; one after it (the record's writes) is
/// rolled forward in process, so the restore stands, and its failpoint is shown to be
/// on the path by crashing there instead.
#[cfg(debug_assertions)]
#[test]
fn io_failures_in_a_restore_leave_wtf_whole() {
    let log = tlog!();
    let mut tally = Tally::default();
    for (failpoint, most, fails) in [
        ("fsx.create_dir.before", 2, true),
        ("fsx.clone_file.before", 2, true),
        ("fsx.chmod.before", 2, true),
        ("fsx.append_durable.before", 8, true),
        ("fsx.rename_with.before", 8, true),
        ("fsx.write_atomic.before", 4, false),
        ("fsx.write_atomic.after", 4, false),
    ] {
        // Every hit in turn: one a restore tolerates (best-effort writes) is no end.
        let mut failed = Vec::new();
        for n in 1..=most {
            let done = restore_support::crash(&log, &format!("{failpoint}=error@{n}"), "", true);
            done.tally(&mut tally);
            if !done.ok {
                failed.push((n, done.pending, done.went_new));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("restore: {failpoint} failed it at (hit, pending, new) {failed:?}\n"),
        );
        if fails {
            assert!(!failed.is_empty(), "{failpoint} is on a restore's path");
        } else {
            assert!(failed.is_empty(), "{failpoint}: rolled forward: {failed:?}");
            let crashed = restore_support::crash(&log, &format!("{failpoint}=abort@1"), "", true);
            crashed.tally(&mut tally);
            assert!(crashed.crashed, "{failpoint} is on a restore's path");
        }
    }
    tally.assert_complete(&log, "Restore I/O failures");
}

/// A restore crashed after its exchange, with the game started since: the next command
/// on that client refuses (`game_running`, naming the journal) and moves nothing; once
/// the game exits, the next one rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_restore_waits_while_the_game_runs_then_rolls_back() {
    let log = tlog!();
    let mut r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
    assert_eq!(r.s.wtf(), r.backed_up, "the exchange ran");
    r.s.home.retail_running();
    let mut differential = Differential::new(&log, &r.s.home, &["_retail_"], &["_retail_"]);
    differential.predict(&log, "the game runs");
    let refused = r.s.run(&r.install_bar(), "");
    let envelope = refused.envelope();
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    let caller = differential.commit(&["retail"]);
    differential.compare(&log, &envelope, &r.s.journals(), &caller);
    assert!(error["details"]["journal_pending"].is_string(), "{error}");
    assert_eq!(r.s.wtf(), r.backed_up, "nothing moves while it runs");
    assert_eq!(support::pending(&r.s), 1);

    r.s.home.nothing_running();
    let mut differential = Differential::new(&log, &r.s.home, &["_retail_"], &[]);
    differential.predict(&log, "the game exited");
    let envelope = r.s.run(&r.install_bar(), "").expect(0);
    differential.compare(&log, &envelope, &r.s.journals(), &caller);
    assert!(warning_codes(&envelope).contains(&"journal_recovered".to_owned()));
    assert_eq!(r.s.wtf(), r.played, "rolled back once the game exited");
    assert!(r.s.journals().is_empty());
}

/// The game started while the restore staged (the fake `/proc` flipped at
/// `engine.restore.staged`): the last running check, under the lock, refuses it before
/// the journal exists, and the stage goes.
#[cfg(debug_assertions)]
#[test]
fn a_game_started_while_a_restore_stages_is_refused_under_the_lock() {
    use common::pause::Pause;

    let mut r = restore_support::RestoreScene::new(true);
    let proc = r.s.home.env.root().join("proc");
    fs::create_dir_all(&proc).unwrap();
    r.s.home.proc = Some(proc);
    let pause = Pause::new(r.s.home.env.root());
    let child = r.spawn_restore("engine.restore.staged=pause", &[pause.env()]);
    pause.reached("engine.restore.staged");
    r.s.home.retail_running();
    pause.resume("engine.restore.staged");
    let outcome = restore_support::RestoreScene::finish(child).unwrap();
    assert_eq!(
        (&outcome["code"], &outcome["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{outcome}"
    );
    assert!(r.s.journals().is_empty(), "refused before the journal");
    assert_eq!(r.s.wtf(), r.played);
    assert!(restore_support::listed(&r.stage_root()).is_empty());
    assert!(!r.s.history().contains(&"restore".to_owned()));
}

/// The game started after the last running check, and the commit window then failed:
/// in-process recovery checks running afresh, so the restore is left pending, nothing
/// moved after the failure; once the game exits, the next command rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_game_started_before_in_process_recovery_leaves_the_restore_pending() {
    use common::pause::Pause;

    let mut r = restore_support::RestoreScene::new(true);
    let proc = r.s.home.env.root().join("proc");
    fs::create_dir_all(&proc).unwrap();
    r.s.home.proc = Some(proc);
    let pause = Pause::new(r.s.home.env.root());
    let child = r.spawn_restore(
        "engine.restore.exchanged=pause,engine.restore.snapshotted=error",
        &[pause.env()],
    );
    pause.reached("engine.restore.exchanged");
    r.s.home.retail_running();
    pause.resume("engine.restore.exchanged");
    let outcome = restore_support::RestoreScene::finish(child).unwrap();
    assert_eq!(outcome["ok"], false, "{outcome}");
    assert_eq!(
        outcome["details"]["recovery_error"]["code"], "game_running",
        "{outcome}"
    );
    assert!(
        outcome["details"]["journal_pending"].is_string(),
        "{outcome}"
    );
    assert_eq!(
        r.s.wtf(),
        r.backed_up,
        "both renames ran; recovery moved nothing"
    );
    assert_eq!(support::pending(&r.s), 1);

    r.s.home.nothing_running();
    let log = tlog!();
    let envelope = r
        .recover_predicted(&log, "the restore left pending", false)
        .expect(0);
    assert!(warning_codes(&envelope).contains(&"journal_recovered".to_owned()));
    assert_eq!(r.s.wtf(), r.played);
}

/// Preservation (M3-D24 (c) rule 3): a restore crashed after its exchange, then played
/// on. Recovery puts the pre-restore tree back live, gives the played-on one a durable
/// home as a `recovered` backup (byte for byte), says so (`restore_rolled_back`), and only
/// then cleans up. Nothing names an account, realm or character.
#[cfg(debug_assertions)]
#[test]
fn a_restore_played_on_before_recovery_is_preserved() {
    let r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
    let config = r.client().join("WTF/Config.wtf");
    fs::write(&config, "SET locale \"enUS\"\nSET played \"1\"\n").unwrap();
    let played_on = r.s.wtf();

    let log = tlog!();
    let run = r.recover_predicted(&log, "a restore played on", false);
    let envelope = run.expect(0);
    common::privacy::assert_private_all(&run.stdout, &run.stderr);
    let codes = warning_codes(&envelope);
    assert!(
        codes.contains(&"journal_recovered".to_owned())
            && codes.contains(&"restore_rolled_back".to_owned()),
        "{codes:?}"
    );
    assert_eq!(r.s.wtf(), r.played, "the pre-restore tree is live again");
    let kept = r.recovered();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        Some(kept[0].1.clone()),
        played_on,
        "the played-on tree, byte for byte"
    );
    let rolled = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "restore_rolled_back")
        .unwrap();
    assert_eq!(rolled["fix_argv"][3], kept[0].0, "{rolled}");
    assert!(r.s.journals().is_empty());
    assert!(restore_support::listed(&r.stage_root()).is_empty());
}

/// Preservation of a staged copy whose source backup was deleted since: unchanged, but
/// that content's last copy, so it becomes a `recovered` backup rather than pruned.
#[cfg(debug_assertions)]
#[test]
fn a_restore_whose_backup_is_gone_is_preserved() {
    let r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
    let backup = r.backup_dir();
    common::wtf::writable_again(&backup);
    fs::remove_dir_all(&backup).unwrap();

    let log = tlog!();
    let envelope = r
        .recover_predicted(&log, "a restore whose backup is gone", false)
        .expect(0);
    assert!(warning_codes(&envelope).contains(&"restore_rolled_back".to_owned()));
    assert_eq!(r.s.wtf(), r.played, "rolled back");
    let kept = r.recovered();
    assert_eq!(kept.len(), 1);
    assert_eq!(Some(kept[0].1.clone()), r.backed_up);
    assert!(r.s.journals().is_empty());
}

/// Rule 2 (M3-D24 (c)): a restore crashed after both its renames, then its restored
/// live `WTF/` replaced by another directory, as a sync tool would. That directory is
/// left exactly as it is; the pre-restore tree, found where the journal recorded it, is
/// preserved. Neither version is lost, and nothing is attributed.
#[cfg(debug_assertions)]
#[test]
fn another_wtf_live_is_left_and_the_restored_over_tree_preserved() {
    let r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.snapshotted=abort", &[]).is_none());
    let wtf = r.client().join("WTF");
    let aside = r.client().join("WTF.aside");
    fs::rename(&wtf, &aside).unwrap();
    fs::create_dir_all(wtf.join("Account")).unwrap();
    fs::write(wtf.join("Config.wtf"), "SET planted \"1\"\n").unwrap();
    let (planted, moved) = (r.s.wtf(), common::trees::tree_hash(&aside));

    let log = tlog!();
    let envelope = r
        .recover_predicted(&log, "another WTF/ live", false)
        .expect(0);
    assert!(warning_codes(&envelope).contains(&"restore_rolled_back".to_owned()));
    assert_eq!(r.s.wtf(), planted, "the live path is never touched");
    assert_eq!(common::trees::tree_hash(&aside), moved);
    let kept = r.recovered();
    assert_eq!(kept.len(), 1);
    assert_eq!(Some(kept[0].1.clone()), r.played, "the pre-restore tree");
    assert!(r.s.journals().is_empty());
}

/// Preservation crashed or failed at each of its own transitions (the intent written, the
/// items moved, the backup published): the next recovery skips the walk and finishes
/// with the id the intent reserved (M3-D24 (c) rule 3).
#[cfg(debug_assertions)]
#[test]
fn a_preservation_crashed_or_failed_at_each_transition_resumes_with_the_same_id() {
    let log = tlog!();
    let mut tally = Tally::default();
    for point in [
        "engine.preserve.intent",
        "engine.preserve.moved",
        "engine.preserve.published",
    ] {
        for action in ["abort", "error"] {
            let done = restore_support::preserve_crash(&log, &format!("{point}={action}"));
            done.tally(&mut tally);
            assert!(done.stopped && done.intent, "{point}={action}: {done:?}");
        }
    }
    tally.assert_complete(&log, "preservation transitions");
}

/// The fsx writes and renames of a played-on restore's recovery: its walk, the protocol
/// and the cleanup after it. Each kind of write, crashed at every hit in turn.
const RECOVERY_WRITES: &[&str] = &[
    "fsx.append_durable.before",
    "fsx.create_dir.before",
    "fsx.chmod.before",
    "fsx.rename_with.before",
    "fsx.write_atomic.before",
    "fsx.write_atomic.after",
    "fsx.sync_filesystem.before",
];

#[cfg(debug_assertions)]
#[test]
fn a_preservation_crashed_at_each_write_and_rename_is_finished() {
    let log = tlog!();
    let mut tally = Tally::default();
    for name in RECOVERY_WRITES {
        let mut crashed = Vec::new();
        for n in 1..=12 {
            let done = restore_support::preserve_crash(&log, &format!("{name}=abort@{n}"));
            done.tally(&mut tally);
            if !done.stopped {
                break;
            }
            crashed.push((n, done.intent));
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("preservation: {name} crashed it at (hit, intent) {crashed:?}\n"),
        );
        assert!(!crashed.is_empty(), "{name} is on a recovery's path");
    }
    tally.assert_complete(&log, "preservation crashes");
}

/// The same writes failing (an I/O error, a full disk) at every hit in turn: recovery
/// stops with every version in place, and the next one, the fault gone, finishes.
#[cfg(debug_assertions)]
#[test]
fn a_preservation_failing_at_each_write_and_rename_keeps_everything() {
    let log = tlog!();
    let mut tally = Tally::default();
    for name in RECOVERY_WRITES {
        let mut failed = Vec::new();
        for n in 1..=10 {
            let done = restore_support::preserve_crash(&log, &format!("{name}=error@{n}"));
            done.tally(&mut tally);
            if done.stopped {
                failed.push((n, done.intent));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("preservation: {name} failed it at (hit, intent) {failed:?}\n"),
        );
        assert!(!failed.is_empty(), "{name} is on a recovery's path");
    }
    tally.assert_complete(&log, "preservation failures");
}

/// Rule 2's "replaced": a restore onto a client that had no `WTF/`, crashed after it
/// moved its copy into place, whose restored `WTF/` the game then recreated. Nothing of
/// ours is displaced (the copy was the backup's): recovery leaves the game's tree live,
/// finishes, and preserves nothing.
#[cfg(debug_assertions)]
#[test]
fn a_restored_tree_the_game_replaced_is_left_and_nothing_is_preserved() {
    let r = restore_support::RestoreScene::new(false);
    assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
    let wtf = r.client().join("WTF");
    common::wtf::writable_again(&wtf);
    fs::remove_dir_all(&wtf).unwrap();
    fs::create_dir_all(wtf.join("Account")).unwrap();
    fs::write(wtf.join("Config.wtf"), "SET fresh \"1\"\n").unwrap();
    let fresh = r.s.wtf();

    let log = tlog!();
    let envelope = r
        .recover_predicted(&log, "a restored tree the game replaced", false)
        .expect(0);
    let codes = warning_codes(&envelope);
    assert!(
        codes.contains(&"journal_recovered".to_owned())
            && !codes.contains(&"restore_rolled_back".to_owned()),
        "{codes:?}"
    );
    assert_eq!(r.s.wtf(), fresh, "the game's tree stays live");
    assert!(r.recovered().is_empty());
    assert!(r.s.journals().is_empty());
}

/// Rule 2's stop: what a restore moved is gone from where the journal recorded it (moved
/// away by someone between the crash and recovery). Recovery cannot place it, so it
/// stops (`unexpected_state`) with the journal, the stage, the snapshots and every tree
/// as they are, and every later change on that client stops the same way.
#[cfg(debug_assertions)]
#[test]
fn a_restore_recovery_that_cannot_place_what_it_moved_keeps_everything() {
    let r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.snapshotted=abort", &[]).is_none());
    let snapshots = r.client().join("Interface/.omaddons/snapshots");
    let snapshot = restore_support::listed(&snapshots).remove(0);
    let elsewhere = r.client().join("moved");
    fs::rename(snapshot.join("WTF.previous"), &elsewhere).unwrap();
    let (live, moved) = (r.s.wtf(), common::trees::tree_hash(&elsewhere));

    let log = tlog!();
    for attempt in 1..=2 {
        let stopped =
            r.recover_predicted(&log, &format!("what it moved is gone ({attempt})"), false);
        let error = &stopped.envelope()["error"];
        assert_eq!(
            error["details"]["reason"],
            "unexpected_state",
            "{}",
            stopped.context()
        );
        common::privacy::assert_private_all(&stopped.stdout, &stopped.stderr);
    }
    assert_eq!(r.s.wtf(), live, "the live path is never touched");
    assert_eq!(common::trees::tree_hash(&elsewhere), moved);
    assert!(snapshot.is_dir());
    assert_eq!(support::pending(&r.s), 1);
    assert!(r.recovered().is_empty());
}

/// A filesystem that cannot exchange: the probe refuses before anything is copied, and
/// an exchange refused inside the transaction (after the probe passed) rolls back a
/// step that never ran. `filesystem_unsupported` either way; `WTF/` untouched.
#[cfg(debug_assertions)]
#[test]
fn a_restore_without_an_atomic_exchange_changes_nothing() {
    let probe = ("OMADDONS_FAILPOINTS", "fsx.renameat2.einval");
    for (engine, env) in [("", vec![probe]), ("fsx.renameat2.einval", Vec::new())] {
        let r = restore_support::RestoreScene::new(true);
        let outcome = r.restore(engine, &env).unwrap();
        assert_eq!(
            (&outcome["code"], &outcome["details"]["reason"]),
            (
                &json!("filesystem_unsupported"),
                &json!("no_rename_exchange")
            ),
            "{engine:?}: {outcome}"
        );
        assert_eq!(outcome["retryable"], false);
        assert_eq!(r.s.wtf(), r.played, "{engine:?}");
        assert!(r.s.journals().is_empty(), "{engine:?}");
        assert!(
            restore_support::listed(&r.stage_root()).is_empty(),
            "{engine:?}"
        );
        assert!(!r.s.history().contains(&"restore".to_owned()));
    }
}

/// A `WTF/` that is a mount point of its own, or a stage on another mount than the
/// client's, is refused by the preflight before anything is copied. Bind mounts need an
/// unprivileged user namespace: without one the scene is skipped, saying so.
#[cfg(debug_assertions)]
#[test]
fn a_wtf_mount_point_or_a_stage_on_another_mount_is_refused_before_a_copy() {
    for (bind, reason) in [
        ("WTF", "wtf_is_mount_point"),
        ("Interface/.omaddons", "stage_cross_mount"),
    ] {
        let r = restore_support::RestoreScene::new(true);
        let dir = r.client().join(bind);
        if !common::mounts::can_bind_mount(&dir) {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("SKIP {reason}: no unprivileged user and mount namespaces here\n"),
            );
            return;
        }
        let outcome = r.restore_with_bind(&dir).unwrap();
        assert_eq!(
            (&outcome["code"], &outcome["details"]["reason"]),
            (&json!("filesystem_unsupported"), &json!(reason)),
            "{outcome}"
        );
        assert_eq!(r.s.wtf(), r.played);
        assert!(
            restore_support::listed(&r.stage_root()).is_empty(),
            "{reason}"
        );
    }
}

/// An automatic backup crashed where it starts (`engine.autobackup`, before it copies
/// anything and before the change stages): the change is wholly old and no backup is
/// half-made. The next run makes the backup and the change (M3-D31). Off this scene,
/// every other crash scene runs the default `daily` on a filesystem the clone hint calls
/// incapable, so it never reaches this failpoint.
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_the_automatic_backup_leaves_the_change_wholly_old() {
    let s = Scene::new();
    let client = s.home.client("_retail_");
    common::wtf::Wtf::at(client.clone())
        .saved_variables("Foo", 100)
        .build();
    let r = s.run(&s.args(&["config", "set", "auto_backup", "daily-copy"]), "");
    assert_eq!(r.code, 0, "{}", r.context());
    let before = s.folders();
    let r = s.run(&s.install_args("gh:o/foo", "v1"), "engine.autobackup=abort");
    assert_eq!(r.code, -1, "aborted: {}", r.context());
    assert_eq!(s.folders(), before, "wholly old");
    let backups = client.join("Interface/.omaddons/backups");
    let made = |dir: &std::path::Path| fs::read_dir(dir).map_or(0, |entries| entries.count());
    assert_eq!(made(&backups), 0, "no backup, not even half of one");

    s.install("gh:o/foo", "v1");
    assert_ne!(s.folders(), before, "the change, made");
    assert_eq!(made(&backups), 1, "and its backup");
    let kinds: Vec<String> = fs::read_to_string(s.home.state().join("history.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"].to_string())
        .collect();
    assert_eq!(
        kinds,
        ["\"config\"", "\"backup\"", "\"install\""],
        "the setting, then the backup just before the change"
    );
}

// ---------------------------------------------------------------------------------------
// Partial restores (D7, M3-D27): Foo's SavedVariables put back from a backup, one journaled
// rename per file and the stage into the snapshot, in the restore scenes' child process.
// The oracle: each of Foo's files wholly old or wholly new at every crash, and all of them
// on one side once recovered, the side the journal's durable phase says; the rest of
// `WTF/` byte for byte; history naming the restore exactly when it stands, its pre-restore
// files kept in its snapshot for undo; nothing staged left once the sweep has run.

#[cfg(test)]
mod addon_support {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::restore_support::{Preserving, RestoreScene, Restored, listed};
    use super::*;

    /// Foo's account-wide file: rewritten since the backup, so replaced.
    pub const ACCOUNT: &str = "Account/FAKEACCT/SavedVariables/Foo.lua";
    /// Foo's file for FakeChar: deleted since, so put back.
    pub const CHAR: &str = "Account/FAKEACCT/FakeRealm/FakeChar/SavedVariables/Foo.lua";
    /// Foo's file for FakeCharTwo: created since, so moved aside.
    pub const CHAR_TWO: &str = "Account/FAKEACCT/FakeRealm/FakeCharTwo/SavedVariables/Foo.lua";

    /// Foo's three files, each with its bytes, `None` when absent.
    pub type Foo = BTreeMap<&'static str, Option<Vec<u8>>>;

    /// A restore scene (two accounts, three characters, a symlink, a `0500` directory)
    /// whose Foo files were played on since the backup in all three ways.
    pub struct AddonScene {
        pub r: RestoreScene,
        /// Foo's files as backed up: what the restore puts back.
        pub backed_up: Foo,
        /// Foo's files now: what it replaces.
        pub played: Foo,
    }

    impl AddonScene {
        pub fn new() -> Self {
            let r = RestoreScene::new(true);
            let backed_up = foo(&r.backup_dir().join("WTF"));
            let wtf = r.client().join("WTF");
            fs::remove_file(wtf.join(CHAR)).unwrap();
            fs::write(wtf.join(CHAR_TWO), "FooDB = { created_since = true }\n").unwrap();
            let played = foo(&wtf);
            Self {
                r,
                backed_up,
                played,
            }
        }

        pub fn wtf(&self) -> PathBuf {
            self.r.client().join("WTF")
        }

        pub fn foo(&self) -> Foo {
            foo(&self.wtf())
        }

        pub fn rest(&self) -> BTreeMap<String, Vec<u8>> {
            rest(&self.wtf())
        }

        /// The engine's partial restore of Foo in a child process: its outcome, or `None`
        /// when an `abort` killed it.
        pub fn restore(&self, engine: &str, env: &[(&str, &str)]) -> Option<Value> {
            let mut env = env.to_vec();
            env.push(("OMADDONS_RESTORE_MODE", "addon"));
            self.r.restore(engine, &env)
        }

        /// The same in the background, for a scene that acts at a `pause`.
        pub fn spawn_restore(
            &self,
            engine: &str,
            env: &[(&str, &str)],
        ) -> (std::process::Child, PathBuf) {
            let mut env = env.to_vec();
            env.push(("OMADDONS_RESTORE_MODE", "addon"));
            self.r.spawn_restore(engine, &env)
        }

        /// The `recovered` backups, each with its id, whether it is partial, and its
        /// files' bytes.
        pub fn recovered(&self) -> Vec<(String, bool, BTreeMap<String, Vec<u8>>)> {
            listed(&self.r.client().join("Interface/.omaddons/backups"))
                .into_iter()
                .filter_map(|dir| {
                    let meta: Value =
                        serde_json::from_slice(&fs::read(dir.join("backup.json")).ok()?).ok()?;
                    (meta["kind"] == "recovered").then(|| {
                        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
                        (id, meta["partial"] == true, every(&dir.join("WTF"), &[]))
                    })
                })
                .collect()
        }
    }

    pub fn foo(wtf: &Path) -> Foo {
        [ACCOUNT, CHAR, CHAR_TWO]
            .into_iter()
            .map(|rel| (rel, fs::read(wtf.join(rel)).ok()))
            .collect()
    }

    /// Every file and symlink under `wtf` but Foo's three, with its bytes (a symlink's,
    /// its target's text): what a partial restore of Foo leaves byte for byte.
    pub fn rest(wtf: &Path) -> BTreeMap<String, Vec<u8>> {
        every(wtf, &[ACCOUNT, CHAR, CHAR_TWO])
    }

    /// Every file and symlink under `dir` but those at `skip`, with its bytes (a
    /// symlink's, its target's text).
    pub fn every(dir: &Path, skip: &[&str]) -> BTreeMap<String, Vec<u8>> {
        let wtf = dir;
        let mut out = BTreeMap::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            let Ok(listing) = fs::read_dir(wtf.join(&rel)) else {
                continue;
            };
            for entry in listing {
                let entry = entry.unwrap();
                let child = rel.join(entry.file_name());
                let key = child.to_string_lossy().into_owned();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    pending.push(child);
                } else if kind.is_symlink() {
                    let target = fs::read_link(wtf.join(&child)).unwrap();
                    out.insert(key, target.to_string_lossy().into_owned().into_bytes());
                } else if !skip.contains(&key.as_str()) {
                    out.insert(key, fs::read(wtf.join(&child)).unwrap());
                }
            }
        }
        out
    }

    /// What a partial restore scene runs in its child.
    #[derive(Debug, Clone, Copy)]
    pub enum Op {
        /// The partial restore of Foo over the played-on files.
        RestoreAddon,
        /// The undo of that restore, committed first and played on since: Foo's files go
        /// back as they were before it (the moved-aside one back, the restored one moved
        /// out).
        UndoRestoreAddon,
    }

    /// `op` with `engine` armed (a fresh scene), then the oracle: each of Foo's files
    /// wholly old or wholly new and the rest of `WTF/` unchanged when it ends; a reader
    /// reports an unfinished one and changes nothing; the next command that takes the lock
    /// recovers (with `recover` armed it may die mid-recovery, keeping the journal, and the
    /// command after it finishes), all of Foo's files then on one side, the side the
    /// journal's phase says; history names it exactly when it stands; a standing restore's
    /// snapshot holds Foo's files as they were, for undo; no snapshot of one that never
    /// committed is left; what was staged goes with the sweep once old; and two more
    /// commands that take the lock change nothing.
    pub fn crash(log: &TestLog, op: Op, engine: &str, recover: &str) -> Restored {
        let label = format!("{op:?} with {engine:?}, then {recover:?}");
        log.step(&label);
        let a = AddonScene::new();
        let s = &a.r.s;
        let (old, new, kind) = match op {
            Op::RestoreAddon => (a.played.clone(), a.backed_up.clone(), "restore"),
            Op::UndoRestoreAddon => {
                let restored = a.restore("", &[]).unwrap();
                assert_eq!(restored["ok"], true, "{restored}");
                let path = a.wtf().join(ACCOUNT);
                fs::write(path, "FooDB = { since_the_restore = true }\n").unwrap();
                (a.foo(), a.played.clone(), "undo")
            }
        };
        let rest = a.rest();
        let (old_generation, old_history) = (s.generation(), s.history());
        let outcome = match op {
            Op::RestoreAddon => a.restore(engine, &[]),
            Op::UndoRestoreAddon => a.r.undo(engine, &[]),
        };
        let journal_text = || -> Vec<String> {
            s.journals()
                .iter()
                .map(|p| fs::read_to_string(p).unwrap_or_default())
                .collect()
        };
        let context = |what: &str| {
            format!(
                "{op:?} with {engine:?}, then {recover:?}: {what}\noutcome {outcome:?}\nold {old:?}\nnew {new:?}\nnow {:?}\njournals {:?}",
                a.foo(),
                journal_text()
            )
        };
        let crashed = outcome.is_none();
        let ok = outcome.as_ref().is_some_and(|o| o["ok"] == true);
        if !crashed && engine.contains("=abort") {
            assert!(ok, "{}", context("not on this path, so it must succeed"));
        }

        // Each file wholly old or wholly new, the rest untouched.
        let at_crash = a.foo();
        for (rel, now) in &at_crash {
            assert!(
                now == &old[rel] || now == &new[rel],
                "{}",
                context(&format!("{rel} is neither old nor new"))
            );
        }
        assert_eq!(a.rest(), rest, "{}", context("the rest of WTF/ changed"));
        let pending_before = support::pending(s);
        let swapped = journal_text()
            .iter()
            .any(|t| t.contains("\"type\":\"swapped\""));

        // A reader reports the unfinished change and changes nothing.
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

        // E11: what `diagnose` predicts of the recovering command.
        let mut differential = Differential::new(log, &s.home, &["_retail_"], &[]);
        differential.predict(log, &label);

        // The next command that takes the lock recovers.
        let install_bar = a.r.install_bar();
        let mut recovering_crashed = false;
        let mut recovered_by = None;
        if !recover.is_empty() {
            let first = s.run(&install_bar, recover);
            recovering_crashed = first.code == -1;
            if recovering_crashed {
                assert_eq!(
                    support::pending(s),
                    pending_before,
                    "{}",
                    context("a recovery that died dropped the journal")
                );
                for (rel, now) in &a.foo() {
                    assert!(
                        now == &old[rel] || now == &new[rel],
                        "{}",
                        context(&format!("{rel} split by a recovery that died"))
                    );
                }
                differential.predict(log, &format!("{label}, after the recovery that died"));
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &bar, &s.journals(), &caller) > 0;
        assert_eq!(
            warning_codes(&bar).iter().any(|c| c == "journal_recovered"),
            pending_before > 0,
            "{}",
            context("recovery warning")
        );
        assert!(s.journals().is_empty(), "{}", context("journals left"));

        let now = a.foo();
        let went_new = now == new;
        assert!(went_new || now == old, "{}", context("Foo's files split"));
        if pending_before > 0 {
            assert_eq!(
                went_new,
                swapped,
                "{}",
                context("recovery went against the journal's phase")
            );
        }
        assert_eq!(a.rest(), rest, "{}", context("the rest of WTF/ changed"));
        let mut history = old_history.clone();
        if went_new {
            history.push(kind.to_owned());
        }
        history.push("install".to_owned());
        assert_eq!(s.history(), history, "{}", context("history"));
        assert_eq!(
            s.generation(),
            old_generation + u64::from(went_new) + 1,
            "{}",
            context("generation")
        );
        if went_new && matches!(op, Op::RestoreAddon) {
            // The undo data: the pre-restore manifest, and the files as they were, under
            // `<tx>.wtf`, which a pre-M3 build's orphan sweep never takes (omad-aepa).
            let tx = s.history_txs()[old_history.len()].clone();
            let snapshots = a.r.client().join("Interface/.omaddons/snapshots");
            let snapshot = snapshots.join(format!("{tx}.wtf"));
            assert!(
                !snapshots.join(&tx).exists(),
                "{}",
                context("a snapshot under the bare name")
            );
            assert!(
                snapshot.join("WTF.manifest.json").is_file(),
                "{}",
                context("no SavedVariables manifest")
            );
            for rel in [ACCOUNT, CHAR_TWO] {
                assert_eq!(
                    fs::read(snapshot.join("WTF").join(rel)).ok(),
                    old[rel],
                    "{}",
                    context(&format!("the snapshot's {rel}"))
                );
            }
        }
        // A crash before the journal may leave the SavedVariables copies an undo made
        // before the lock, which the sweep takes once old (below).
        let orphans = s.orphan_snapshots();
        assert!(
            orphans.is_empty() || recovering_crashed || pending_before == 0,
            "{}",
            context(&format!(
                "snapshots of a change that never committed {orphans:?}"
            ))
        );

        // What a crash left staged or snapshotted goes with the sweep once it is old, and
        // recovery run again changes nothing.
        a.r.age_leftovers();
        s.age_orphan_snapshots();
        s.run(&s.args(&["pin", "Bar"]), "").expect(0);
        let left = listed(&a.r.stage_root());
        assert!(
            left.is_empty(),
            "{}",
            context(&format!("staged leftovers {left:?}"))
        );
        let orphans = s.orphan_snapshots();
        assert!(
            orphans.is_empty(),
            "{}",
            context(&format!("aged leftovers survived the sweep {orphans:?}"))
        );
        s.run(&s.args(&["unpin", "Bar"]), "").expect(0);
        assert_eq!(
            a.foo(),
            now,
            "{}",
            context("a later command changed Foo's files")
        );
        assert!(s.journals().is_empty(), "{}", context("journals after"));

        Restored {
            crashed,
            ok,
            new_at_crash: at_crash == new,
            pending: pending_before > 0,
            went_new,
            recovering_crashed,
            with_journal: !journals.is_empty(),
            compared,
        }
    }

    /// A partial restore crashed after its renames, then the game wrote Foo's
    /// account-wide file anew (a new file at its path); recovered by a child with
    /// `engine` armed (a crash or a failure at one step of the walk or the protocol), then
    /// by the binary. Whatever happened, both versions of that file (the pre-restore one
    /// and the game's) still exist somewhere, and every backup listed is whole; a failure
    /// is `preservation_incomplete` (or the walk's own error) and names no account, realm
    /// or character. The next recovery completes: the game's file live, the other files
    /// rolled back, exactly one partial `recovered` backup holding the pre-restore file,
    /// under the id the intent reserved when there was one, nothing left in the stage or
    /// the journal; two more commands that take the lock change nothing of that.
    pub fn preserve_crash(log: &TestLog, engine: &str) -> Preserving {
        let label = format!(
            "a partial restore the game wrote over, recovered with {engine:?}, then by the binary"
        );
        log.step(&label);
        let a = AddonScene::new();
        let s = &a.r.s;
        assert!(a.restore("engine.restore.exchanged=abort", &[]).is_none());
        let live = a.wtf().join(ACCOUNT);
        fs::remove_file(&live).unwrap();
        fs::write(&live, "FooDB = { the_game = true }\n").unwrap();
        let game = fs::read(&live).unwrap();
        let pre = a.played[ACCOUNT].clone().unwrap();

        let outcome = a.r.recover(engine);
        let context = |what: &str| format!("recovery with {engine:?}: {what}\noutcome {outcome:?}");
        let stopped = outcome.as_ref().is_none_or(|o| o["ok"] == false);
        if let Some(o) = outcome.as_ref().filter(|o| o["ok"] == false) {
            common::privacy::assert_private(&o.to_string());
            assert!(
                matches!(
                    o["details"]["reason"].as_str(),
                    Some("preservation_incomplete")
                ) || o["code"] == "io",
                "{}",
                context("a failure stops as preservation_incomplete, or the walk's own error")
            );
        }
        // Both versions of the file still exist: the game's live, the pre-restore one in
        // the stage, the assembly or a backup.
        assert_eq!(
            fs::read(&live).unwrap(),
            game,
            "{}",
            context("the game's file moved")
        );
        let meta = a.r.client().join("Interface/.omaddons");
        let mut places: Vec<PathBuf> = Vec::new();
        for stage in listed(&meta.join("stage")) {
            places.push(stage.join("WTF").join(ACCOUNT));
            places.extend(
                listed(&stage.join("recovered"))
                    .iter()
                    .map(|a| a.join("WTF").join(ACCOUNT)),
            );
        }
        places.extend(
            listed(&meta.join("backups"))
                .iter()
                .map(|b| b.join("WTF").join(ACCOUNT)),
        );
        assert!(
            places
                .iter()
                .any(|p| fs::read(p).ok().as_deref() == Some(pre.as_slice())),
            "{}",
            context("the pre-restore file is gone")
        );
        for backup in listed(&meta.join("backups")) {
            assert!(
                backup.join("backup.json").is_file() && backup.join("manifest.json").is_file(),
                "{}",
                context(&format!(
                    "a half-made backup is listed: {}",
                    backup.display()
                ))
            );
        }
        let intent = a.r.intent();

        // E11: the armed recovery is no prediction's to match; the binary's is.
        let mut differential = Differential::new(log, &s.home, &["_retail_"], &[]);
        differential.predict(log, &label);
        let with_journal = !s.journals().is_empty();
        let bar = s.run(&a.r.install_bar(), "");
        let envelope = bar.expect(0);
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &envelope, &s.journals(), &caller) > 0;
        common::privacy::assert_private_all(&bar.stdout, &bar.stderr);
        assert!(s.journals().is_empty(), "{}", context("journals left"));
        let now = a.foo();
        assert_eq!(
            now[ACCOUNT].as_deref(),
            Some(game.as_slice()),
            "{}",
            context("game's")
        );
        assert_eq!(
            (&now[CHAR], &now[CHAR_TWO]),
            (&a.played[CHAR], &a.played[CHAR_TWO]),
            "{}",
            context("the other files rolled back")
        );
        let kept = a.recovered();
        assert_eq!(kept.len(), 1, "{}", context(&format!("recovered {kept:?}")));
        let (id, partial, files) = &kept[0];
        assert!(
            *partial,
            "{}",
            context("a backup of single files is partial")
        );
        assert_eq!(
            files.get(ACCOUNT).map(Vec::as_slice),
            Some(pre.as_slice()),
            "{}",
            context("not the pre-restore file")
        );
        if let Some(reserved) = &intent {
            assert_eq!(
                id,
                reserved,
                "{}",
                context("not the id the intent reserved")
            );
        }
        // When this command recovered it, its warning names the backup and how it restores.
        if let Some(rolled) = envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "restore_rolled_back")
        {
            assert_eq!(
                rolled["fix_argv"],
                json!([
                    "omaddons", "backup", "restore", id, "-t", "retail", "--addon", "Foo"
                ]),
                "{rolled}"
            );
        }
        assert!(
            listed(&a.r.stage_root()).is_empty(),
            "{}",
            context("stage left")
        );
        for args in [["pin", "Bar"], ["unpin", "Bar"]] {
            s.run(&s.args(&args), "").expect(0);
            assert_eq!(
                a.recovered().len(),
                1,
                "{}",
                context("a later recovery changed it")
            );
            assert_eq!(a.foo(), now);
        }
        Preserving {
            stopped,
            intent: intent.is_some(),
            with_journal,
            compared,
        }
    }
}

/// Every failpoint as a crash of a partial restore. Its renames having all run never
/// selects roll-forward on their own: only `swapped` does.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_restore_addon() {
    use addon_support::{Op, crash};

    let log = tlog!();
    let mut tally = Tally::default();
    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = crash(&log, Op::RestoreAddon, &format!("{failpoint}=abort"), "");
        done.tally(&mut tally);
        if done.crashed {
            crashed_at.push(*failpoint);
        }
        if matches!(
            *failpoint,
            "engine.restore.exchanged" | "engine.restore.snapshotted"
        ) {
            assert!(
                done.new_at_crash && !done.went_new,
                "{failpoint}: renamed but not swapped must roll back: {done:?}"
            );
        }
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "partial restore: crashed at {} of {} failpoints: {crashed_at:?}\n",
            crashed_at.len(),
            FAILPOINTS.len()
        ),
    );
    tally.assert_complete(&log, "RestoreAddon");
    for on_path in [
        "engine.restore.staged",
        "engine.lock.acquired",
        "engine.revalidated",
        "engine.journal.prepared",
        "engine.snapshot.sv",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.record.written",
        "engine.history.appended",
        "engine.committed",
    ] {
        assert!(
            crashed_at.contains(&on_path),
            "{on_path} is on a partial restore's path: {crashed_at:?}"
        );
    }
}

/// Every failpoint as a crash of the undo of a partial restore played on since: Foo's
/// files go back as they were before it, wholly or not at all.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_undo_restore_addon() {
    use addon_support::{Op, crash};

    let log = tlog!();
    let mut tally = Tally::default();
    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = crash(
            &log,
            Op::UndoRestoreAddon,
            &format!("{failpoint}=abort"),
            "",
        );
        done.tally(&mut tally);
        if done.crashed {
            crashed_at.push(*failpoint);
        }
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "undo of a partial restore: crashed at {} of {} failpoints: {crashed_at:?}\n",
            crashed_at.len(),
            FAILPOINTS.len()
        ),
    );
    tally.assert_complete(&log, "UndoRestoreAddon");
    for on_path in [
        "engine.snapshot.prestaged",
        "engine.lock.acquired",
        "engine.journal.prepared",
        "engine.snapshot.sv",
        "engine.swapped",
        "engine.history.appended",
        "engine.committed",
    ] {
        assert!(
            crashed_at.contains(&on_path),
            "{on_path} is on the undo of a partial restore's path: {crashed_at:?}"
        );
    }
}

/// A crash before each journaled rename of a partial restore (one per file, then the
/// stage into the snapshot), and in its recovery: after each rename its rollback undoes,
/// and as a roll-forward starts.
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_every_rename_and_recovery_of_a_partial_restore() {
    use addon_support::{Op, crash};

    let log = tlog!();
    let mut tally = Tally::default();
    let mut renames = Vec::new();
    for n in 1..=6 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        let done = crash(&log, Op::RestoreAddon, &spec, "");
        done.tally(&mut tally);
        if !done.crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1, 2, 3, 4], "three files and the stage");
    let mut recoveries = Vec::new();
    for k in 1..=6 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = crash(
            &log,
            Op::RestoreAddon,
            "engine.restore.snapshotted=abort",
            &recover,
        );
        done.tally(&mut tally);
        if !done.recovering_crashed {
            break;
        }
        recoveries.push(k);
    }
    assert_eq!(
        recoveries,
        [1, 2, 3, 4],
        "each step rolled back, crashed after"
    );
    let done = crash(
        &log,
        Op::RestoreAddon,
        "engine.swapped=abort",
        "engine.recover.forward=abort",
    );
    done.tally(&mut tally);
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
    tally.assert_complete(&log, "RestoreAddon renames and recoveries");
}

/// I/O failures where a partial restore writes (its staged copies' folders, files and
/// modes, the journal's appends, the manifest, the renames, the record), each injected once
/// at every hit in turn: each file stays wholly old or wholly new and what is left
/// recovers. Before `swapped` it fails the restore; after it, the record's writes roll
/// forward in process and the restore stands.
#[cfg(debug_assertions)]
#[test]
fn io_failures_in_a_partial_restore_leave_each_file_whole() {
    use addon_support::{Op, crash};

    let log = tlog!();
    let mut tally = Tally::default();
    for (failpoint, most) in [
        ("fsx.create_dir.before", 3),
        ("fsx.clone_file.before", 2),
        ("fsx.chmod.before", 2),
        ("fsx.append_durable.before", 10),
        ("fsx.rename_with.before", 8),
        ("fsx.write_atomic.before", 4),
        ("fsx.write_atomic.after", 4),
    ] {
        let mut failed = Vec::new();
        for n in 1..=most {
            let done = crash(
                &log,
                Op::RestoreAddon,
                &format!("{failpoint}=error@{n}"),
                "",
            );
            done.tally(&mut tally);
            if !done.ok {
                failed.push((n, done.pending, done.went_new));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("partial restore: {failpoint} failed it at {failed:?}\n"),
        );
        let on_path = !failed.is_empty() || {
            let done = crash(&log, Op::RestoreAddon, &format!("{failpoint}=abort@1"), "");
            done.tally(&mut tally);
            done.crashed
        };
        assert!(on_path, "{failpoint} is on a partial restore's path");
    }
    tally.assert_complete(&log, "RestoreAddon I/O failures");
}

/// A partial restore crashed after its renames, the game started since: the next command
/// refuses (`game_running`, naming the journal) and moves nothing; once the game exits,
/// the next one rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_partial_restore_waits_while_the_game_runs_then_rolls_back() {
    let log = tlog!();
    let mut a = addon_support::AddonScene::new();
    let outcome = a.restore("engine.restore.exchanged=abort", &[]);
    assert!(outcome.is_none(), "{outcome:?}");
    assert_eq!(a.foo(), a.backed_up, "the renames ran");
    a.r.s.home.retail_running();
    let mut differential = Differential::new(&log, &a.r.s.home, &["_retail_"], &["_retail_"]);
    differential.predict(&log, "the game runs");
    let refused = a.r.s.run(&a.r.install_bar(), "");
    let envelope = refused.envelope();
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    let caller = differential.commit(&["retail"]);
    differential.compare(&log, &envelope, &a.r.s.journals(), &caller);
    assert!(error["details"]["journal_pending"].is_string(), "{error}");
    assert_eq!(a.foo(), a.backed_up, "nothing moves while it runs");
    a.r.s.home.nothing_running();
    let mut differential = Differential::new(&log, &a.r.s.home, &["_retail_"], &[]);
    differential.predict(&log, "the game exited");
    let envelope = a.r.s.run(&a.r.install_bar(), "").expect(0);
    differential.compare(&log, &envelope, &a.r.s.journals(), &caller);
    assert!(warning_codes(&envelope).contains(&"journal_recovered".to_owned()));
    assert_eq!(a.foo(), a.played, "rolled back once the game exited");
    assert!(a.r.s.journals().is_empty());
}

/// The game started after the last running check, and the commit window then failed:
/// in-process recovery checks running afresh, so the partial restore is left pending with
/// nothing moved after the failure; once the game exits, the next command rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_game_started_before_in_process_recovery_leaves_the_partial_restore_pending() {
    use common::pause::Pause;

    let mut a = addon_support::AddonScene::new();
    let proc = a.r.s.home.env.root().join("proc");
    fs::create_dir_all(&proc).unwrap();
    a.r.s.home.proc = Some(proc);
    let pause = Pause::new(a.r.s.home.env.root());
    let child = a.spawn_restore(
        "engine.restore.exchanged=pause,engine.restore.snapshotted=error",
        &[pause.env()],
    );
    pause.reached("engine.restore.exchanged");
    a.r.s.home.retail_running();
    pause.resume("engine.restore.exchanged");
    let outcome = restore_support::RestoreScene::finish(child).unwrap();
    assert_eq!(outcome["ok"], false, "{outcome}");
    assert_eq!(
        outcome["details"]["recovery_error"]["code"], "game_running",
        "{outcome}"
    );
    assert_eq!(
        a.foo(),
        a.backed_up,
        "every rename ran; recovery moved nothing"
    );
    assert_eq!(support::pending(&a.r.s), 1);
    a.r.s.home.nothing_running();
    let log = tlog!();
    let envelope =
        a.r.recover_predicted(&log, "the partial restore left pending", false)
            .expect(0);
    assert!(warning_codes(&envelope).contains(&"journal_recovered".to_owned()));
    assert_eq!(a.foo(), a.played);
}

/// Rule 2 for single files (M3-D24 (c), D14): a partial restore crashed after its renames,
/// then the game wrote the file it exchanged anew. Recovery leaves the game's file live,
/// rolls the others back, and preserves the displaced pre-restore file as a partial
/// `recovered` backup, which then restores with `--addon` only.
#[cfg(debug_assertions)]
#[test]
fn a_file_the_game_replaced_is_left_and_the_displaced_one_preserved() {
    let log = tlog!();
    let done = addon_support::preserve_crash(&log, "");
    assert!(!done.stopped && !done.intent, "{done:?}");

    // The recovered backup restores Foo's settings, and only with --addon.
    let a = addon_support::AddonScene::new();
    assert!(a.restore("engine.restore.exchanged=abort", &[]).is_none());
    let live = a.wtf().join(addon_support::ACCOUNT);
    fs::remove_file(&live).unwrap();
    fs::write(&live, "FooDB = { the_game = true }\n").unwrap();
    a.r.s.run(&a.r.install_bar(), "").expect(0);
    let (id, partial, _) = a.recovered().remove(0);
    assert!(partial);
    let whole =
        a.r.s
            .run(&a.r.s.args(&["backup", "restore", &id, "-t", "retail"]), "");
    assert_eq!(
        whole.envelope()["error"]["code"],
        "usage",
        "{}",
        whole.context()
    );
    let args = a.r.s.args(&[
        "backup",
        "restore",
        &id,
        "-t",
        "retail",
        "--addon",
        "Foo",
        "--dry-run",
    ]);
    let one = a.r.s.run(&args, "").expect(0);
    assert_eq!(
        one["data"]["addons"][0]["files"]["replaced"], 1,
        "the pre-restore file over the game's: {one:#}"
    );
}

/// Rule 2's "replaced" for a file put where there was none: the game wrote it anew after
/// the crash. Nothing of ours is displaced (the copy was the backup's), so nothing is
/// preserved for it; the game's file stays.
#[cfg(debug_assertions)]
#[test]
fn a_restored_file_the_game_replaced_is_left_and_nothing_is_preserved() {
    let a = addon_support::AddonScene::new();
    assert!(a.restore("engine.restore.exchanged=abort", &[]).is_none());
    let live = a.wtf().join(addon_support::CHAR);
    fs::remove_file(&live).unwrap();
    fs::write(&live, "FooDB = { fresh = true }\n").unwrap();
    let envelope = a.r.s.run(&a.r.install_bar(), "").expect(0);
    let codes = warning_codes(&envelope);
    assert!(
        codes.contains(&"journal_recovered".to_owned())
            && !codes.contains(&"restore_rolled_back".to_owned()),
        "{codes:?}"
    );
    let now = a.foo();
    assert_eq!(
        now[addon_support::CHAR].as_deref(),
        Some(&b"FooDB = { fresh = true }\n"[..])
    );
    assert_eq!(
        (&now[addon_support::ACCOUNT], &now[addon_support::CHAR_TWO]),
        (
            &a.played[addon_support::ACCOUNT],
            &a.played[addon_support::CHAR_TWO]
        ),
        "the others rolled back"
    );
    assert!(a.recovered().is_empty());
    assert!(a.r.s.journals().is_empty());
}

/// That preservation crashed or failed at each of its own transitions, and at every hit of
/// each write it makes: the next recovery finishes with the id the intent reserved, every
/// version kept meanwhile.
#[cfg(debug_assertions)]
#[test]
fn a_partial_preservation_crashed_or_failed_anywhere_is_finished() {
    let log = tlog!();
    let mut tally = Tally::default();
    for point in [
        "engine.preserve.intent",
        "engine.preserve.moved",
        "engine.preserve.published",
    ] {
        for action in ["abort", "error"] {
            let done = addon_support::preserve_crash(&log, &format!("{point}={action}"));
            done.tally(&mut tally);
            assert!(done.stopped && done.intent, "{point}={action}: {done:?}");
        }
    }
    for name in RECOVERY_WRITES {
        for action in ["abort", "error"] {
            let mut stopped = Vec::new();
            for n in 1..=10 {
                let done = addon_support::preserve_crash(&log, &format!("{name}={action}@{n}"));
                done.tally(&mut tally);
                if done.stopped {
                    stopped.push((n, done.intent));
                } else if action == "abort" {
                    break;
                }
            }
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("partial preservation: {name}={action} stopped it at {stopped:?}\n"),
            );
            assert!(!stopped.is_empty(), "{name} is on a recovery's path");
        }
    }
    tally.assert_complete(&log, "partial preservation");
}

// ---------------------------------------------------------------------------------------
// Across features (D12): scenes that need two tasks' work at once, and the sweep of every
// failpoint over the preservation shapes (the restore, undo-of-restore and partial
// restore shapes sweep every failpoint in their own matrices above).

#[cfg(test)]
mod across_support {
    use super::*;

    /// The undo of a restore crashed after its exchange, then played on: the tree it put in
    /// place (the one from before the restore) is changed before recovery. Recovered by a
    /// child with `engine` armed in its recovery, then by the binary: the restored tree is
    /// live again, the played-on one a `recovered` backup byte for byte, under the id the
    /// intent reserved when a crash stopped the first recovery after it; exactly one.
    pub fn undo_preserve_crash(log: &TestLog, engine: &str) -> restore_support::Preserving {
        use restore_support::{RestoreScene, listed};

        let label =
            format!("a played-on undo of a restore recovered with {engine:?}, then by the binary");
        log.step(&label);
        let r = RestoreScene::new(true);
        assert_eq!(r.restore("", &[]).unwrap()["ok"], true);
        r.play_on_restored();
        let restored = r.s.wtf();
        assert!(r.undo("engine.restore.exchanged=abort", &[]).is_none());
        let config = r.client().join("WTF/Config.wtf");
        fs::write(
            &config,
            "SET locale \"enUS\"\nSET played \"after the undo\"\n",
        )
        .unwrap();
        let played_on = r.s.wtf();
        assert_ne!(played_on, restored);

        let outcome = r.recover(engine);
        let context = |what: &str| format!("recovery with {engine:?}: {what}\noutcome {outcome:?}");
        let stopped = outcome.as_ref().is_none_or(|o| o["ok"] == false);
        let versions = r.versions();
        for (what, version) in [("restored", &restored), ("played-on", &played_on)] {
            assert!(
                versions.contains(version.as_ref().unwrap()),
                "{}",
                context(&format!("the {what} tree is gone"))
            );
        }
        let intent = r.intent();
        // E11: the armed recovery is no prediction's to match; the binary's is.
        let mut differential = Differential::new(log, &r.s.home, &["_retail_"], &[]);
        differential.predict(log, &label);
        let with_journal = !r.s.journals().is_empty();
        let bar = r.s.run(&r.install_bar(), "");
        let envelope = bar.expect(0);
        let caller = differential.commit(&["retail"]);
        let compared = differential.compare(log, &envelope, &r.s.journals(), &caller) > 0;
        common::privacy::assert_private_all(&bar.stdout, &bar.stderr);
        assert!(r.s.journals().is_empty(), "{}", context("journals left"));
        assert_eq!(
            r.s.wtf(),
            restored,
            "{}",
            context("not rolled back to the restored tree")
        );
        let kept = r.recovered();
        assert_eq!(kept.len(), 1, "{}", context(&format!("recovered {kept:?}")));
        assert_eq!(
            Some(&kept[0].1),
            played_on.as_ref(),
            "{}",
            context("not the played-on tree")
        );
        if let Some(id) = &intent {
            assert_eq!(
                &kept[0].0,
                id,
                "{}",
                context("not the id the intent reserved")
            );
        }
        assert!(
            listed(&r.stage_root()).is_empty(),
            "{}",
            context("stage left")
        );
        for args in [["pin", "Bar"], ["unpin", "Bar"]] {
            r.s.run(&r.s.args(&args), "").expect(0);
            assert_eq!(
                r.recovered(),
                kept,
                "{}",
                context("a later recovery changed it")
            );
        }
        restore_support::Preserving {
            stopped,
            intent: intent.is_some(),
            with_journal,
            compared,
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn a_played_on_undo_of_a_restore_is_preserved_and_resumes_with_the_same_id() {
    use across_support::undo_preserve_crash;

    let log = tlog!();
    let mut tally = Tally::default();
    let done = undo_preserve_crash(&log, "");
    done.tally(&mut tally);
    assert!(!done.stopped, "{done:?}");
    for point in [
        "engine.preserve.intent",
        "engine.preserve.moved",
        "engine.preserve.published",
    ] {
        let done = undo_preserve_crash(&log, &format!("{point}=abort"));
        done.tally(&mut tally);
        assert!(done.stopped && done.intent, "{point}: {done:?}");
    }
    tally.assert_complete(&log, "undo-of-restore preservation");
}

/// An automatic capture racing a restore paused while it stages: the capture takes the
/// scheduling lock and copies the live, pre-restore tree (or, with `engine.autobackup`
/// aborted, makes none), never a half-made backup listed; the restore, whose reader
/// reservation and journal it never touches, then commits. No deadlock either way.
#[cfg(debug_assertions)]
#[test]
fn an_automatic_capture_racing_a_staging_restore() {
    use common::pause::Pause;
    use restore_support::{RestoreScene, listed};

    for crash in [false, true] {
        let r = RestoreScene::new(true);
        r.s.home
            .run(&["config", "set", "auto_backup", "daily-copy"])
            .expect(0);
        let pause = Pause::new(r.s.home.env.root());
        let child = r.spawn_restore("engine.restore.staged=pause", &[pause.env()]);
        pause.reached("engine.restore.staged");
        let argv = ["backup", "create", "--auto", "-t", "retail", "--json"].map(str::to_owned);
        let failpoints = if crash { "engine.autobackup=abort" } else { "" };
        let capture = r.s.run(&argv, failpoints);
        // Only whole backups are ever listed.
        for backup in listed(&r.client().join("Interface/.omaddons/backups")) {
            assert!(
                backup.join("backup.json").is_file() && backup.join("manifest.json").is_file(),
                "crash {crash}: half a backup listed: {}",
                backup.display()
            );
        }
        let autos: Vec<std::path::PathBuf> =
            listed(&r.client().join("Interface/.omaddons/backups"))
                .into_iter()
                .filter(|b| {
                    fs::read_to_string(b.join("backup.json"))
                        .is_ok_and(|t| t.contains("\"kind\": \"auto\""))
                })
                .collect();
        if crash {
            assert_eq!(capture.code, -1, "{}", capture.context());
            assert!(autos.is_empty(), "no backup from a capture that died");
        } else {
            let made = capture.expect(0);
            assert_eq!(
                made["data"]["targets"][0]["backup"]["kind"], "auto",
                "{made}"
            );
            assert_eq!(autos.len(), 1);
            // File by file (a stored backup gives its folders owner write access).
            assert_eq!(
                addon_support::every(&autos[0].join("WTF"), &[]),
                addon_support::every(&r.client().join("WTF"), &[]),
                "the live tree, from before the restore"
            );
        }
        pause.resume("engine.restore.staged");
        let outcome = RestoreScene::finish(child).unwrap();
        assert_eq!(outcome["ok"], true, "crash {crash}: {outcome}");
        assert_eq!(r.s.wtf(), r.backed_up, "crash {crash}: restored");
        assert!(r.s.journals().is_empty());
    }
}

/// Recovery across clients (M3-D24, M3-D29): two restores crashed after their exchange;
/// retail's client then runs, classic's restored tree is played on. The next command
/// recovers classic, preserving the played-on tree, and leaves retail's journal pending
/// with nothing under its `WTF/` moved; once retail's client exits, the next command rolls
/// it back. Through the binary's own `backup restore`.
#[cfg(debug_assertions)]
#[test]
fn recovery_defers_a_running_client_and_preserves_another() {
    use common::trees::tree_hash;

    let mut home = common::home::Home::new(&["wow", "wow_classic"]);
    let (retail_wtf, classic_wtf) = (
        home.client("_retail_").join("WTF"),
        home.client("_classic_").join("WTF"),
    );
    let wtf = |flavor: &str| {
        if flavor == "_retail_" {
            retail_wtf.clone()
        } else {
            classic_wtf.clone()
        }
    };
    for flavor in ["_retail_", "_classic_"] {
        common::wtf::Wtf::at(home.client(flavor))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 200)
            .build();
    }
    let created = home.run(&["backup", "create"]).expect(0);
    let ids: std::collections::BTreeMap<String, String> = created["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["target"]["id"].as_str().unwrap().to_owned(),
                t["backup"]["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let mut played = std::collections::BTreeMap::new();
    // Retail's restore crashes first, then its client starts: classic's restore, whose
    // commit window recovers first, leaves retail's journal waiting and crashes too.
    for (target, flavor) in [("retail", "_retail_"), ("classic", "_classic_")] {
        let sv = wtf(flavor).join("Account/FAKEACCT/SavedVariables/Foo.lua");
        fs::write(&sv, "FooDB = { played = true }\n").unwrap();
        played.insert(target, tree_hash(&wtf(flavor)));
        let argv = ["backup", "restore", &ids[target], "-t", target, "--json"].map(str::to_owned);
        let crashed = home.exec_with(
            &argv,
            &[("OMADDONS_FAILPOINTS", "engine.restore.exchanged=abort")],
        );
        assert_eq!(crashed.code, -1, "{target}: {}", crashed.context());
        if target == "retail" {
            home.retail_running();
        }
    }
    let retail_restored = tree_hash(&wtf("_retail_"));
    assert_ne!(retail_restored, played["retail"], "retail's exchange ran");
    fs::write(
        wtf("_classic_").join("Config.wtf"),
        "SET locale \"enUS\"\nSET played \"1\"\n",
    )
    .unwrap();
    let classic_played_on = tree_hash(&wtf("_classic_"));

    // E11: an update with nothing to update commits no target: it recovers under the lock
    // alone, knowing which games run.
    let log = tlog!();
    let clients = ["_retail_", "_classic_"];
    let journals = home.state().join("journal");
    let mut differential = Differential::new(&log, &home, &clients, &["_retail_"]);
    differential.predict(&log, "retail's client runs");
    let update = ["update", "--offline", "--json"].map(str::to_owned);
    let r = home.exec(&update);
    let envelope = r.envelope();
    assert_eq!(
        differential.compare(
            &log,
            &envelope,
            &restore_support::listed(&journals),
            &Caller::Under
        ),
        2
    );
    common::privacy::assert_private_all(&r.stdout, &r.stderr);
    let codes = warning_codes(&envelope);
    assert!(
        codes.contains(&"journal_recovered".to_owned())
            && codes.contains(&"restore_rolled_back".to_owned()),
        "classic recovered and preserved: {codes:?}\n{}",
        r.context()
    );
    assert_eq!(
        tree_hash(&wtf("_classic_")),
        played["classic"],
        "classic rolled back"
    );
    let backups = home.client("_classic_").join("Interface/.omaddons/backups");
    let recovered: Vec<String> = restore_support::listed(&backups)
        .into_iter()
        .filter(|b| {
            fs::read_to_string(b.join("backup.json")).is_ok_and(|t| t.contains("\"recovered\""))
        })
        .map(|b| tree_hash(&b.join("WTF")))
        .collect();
    assert_eq!(recovered, [classic_played_on], "the played-on tree, kept");
    assert_eq!(
        tree_hash(&wtf("_retail_")),
        retail_restored,
        "nothing moves under a running client"
    );
    assert_eq!(
        restore_support::listed(&journals).len(),
        1,
        "retail's waits"
    );

    home.nothing_running();
    let mut differential = Differential::new(&log, &home, &clients, &[]);
    differential.predict(&log, "retail's client exited");
    let envelope = home.exec(&update).expect(0);
    assert_eq!(
        differential.compare(
            &log,
            &envelope,
            &restore_support::listed(&journals),
            &Caller::Under
        ),
        1
    );
    assert_eq!(
        tree_hash(&wtf("_retail_")),
        played["retail"],
        "rolled back once it exited"
    );
    assert!(restore_support::listed(&journals).is_empty());
}

/// The sweep (D12): every engine failpoint armed once in the recovery of each preservation
/// shape (a whole restore, its undo and a partial restore, each played on after a
/// crash). On their path a scene stops and the next recovery finishes; off it, it
/// succeeds. Either way each shape's oracle holds.
#[cfg(debug_assertions)]
#[test]
fn every_failpoint_in_every_preservation_shape() {
    let log = tlog!();
    let mut tally = Tally::default();
    let mut stopped = std::collections::BTreeMap::new();
    for failpoint in FAILPOINTS {
        let spec = format!("{failpoint}=abort");
        let shapes = [
            ("restore", restore_support::preserve_crash(&log, &spec)),
            ("undo", across_support::undo_preserve_crash(&log, &spec)),
            ("partial", addon_support::preserve_crash(&log, &spec)),
        ];
        for (shape, done) in shapes {
            done.tally(&mut tally);
            if done.stopped {
                stopped
                    .entry(shape)
                    .or_insert_with(Vec::new)
                    .push(*failpoint);
            }
        }
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("preservation sweep: stopped at {stopped:?}\n"),
    );
    tally.assert_complete(&log, "preservation sweep");
    for shape in ["restore", "undo", "partial"] {
        let on = stopped.get(shape).cloned().unwrap_or_default();
        for point in [
            "engine.recover.step",
            "engine.preserve.intent",
            "engine.preserve.moved",
            "engine.preserve.published",
        ] {
            assert!(
                on.contains(&point),
                "{shape}: {point} is on its path: {on:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// The doctor's repair transactions (E5). No command commits one before `doctor fix` (E6),
// so each kind's repair runs in a child process of this test binary (`child_repair`),
// crashed by a real `abort`; every reader and every recovery is the binary's. The oracle:
// the subject equals its before state when the crash preceded the durable `done`, and its
// after state from `done` on; no repair journal or in-flight lock is left.

#[cfg(test)]
mod repair_support {
    use std::path::PathBuf;

    use serde_json::Value;

    use super::common::repair::RepairScene;
    use super::common::tlog::TestLog;

    /// `omaddons <args> --json --progress json`.
    pub fn json_args(args: &[&str]) -> Vec<String> {
        args.iter()
            .copied()
            .chain(["--json", "--progress", "json"])
            .map(str::to_owned)
            .collect()
    }

    /// The messages of `envelope`'s warnings with `code` that name `tx`.
    pub fn naming<'a>(envelope: &'a Value, code: &str, tx: &str) -> Vec<&'a str> {
        envelope["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|warning| warning["code"] == code)
            .filter_map(|warning| warning["message"].as_str())
            .filter(|message| message.contains(tx))
            .collect()
    }

    /// The trees a repair scene's commands may write, by label.
    pub fn trees(r: &RepairScene) -> [(&'static str, PathBuf); 4] {
        let env = &r.s.home.env;
        [
            ("state", env.state()),
            ("config", env.config()),
            ("cache", env.cache()),
            ("client", r.s.home.client("_retail_")),
        ]
    }

    /// Records every tree of [`trees`].
    pub fn record(log: &TestLog, r: &RepairScene) {
        for (label, root) in trees(r) {
            log.tree(label, &root);
        }
    }

    /// Every tree of [`trees`] is as [`record`] left it.
    pub fn unchanged(log: &TestLog, r: &RepairScene) {
        for (label, root) in trees(r) {
            log.assert_unchanged(label, &root);
        }
    }

    /// What is left in `<state>/journal` and `<state>/inflight`, logged.
    pub fn leftover_journals(log: &TestLog, r: &RepairScene) -> Vec<String> {
        let state = r.s.home.state();
        let mut left = Vec::new();
        for dir in ["journal", "inflight"] {
            let listed: Vec<String> = std::fs::read_dir(state.join(dir))
                .map(|l| {
                    l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            log.note(format!("{dir}/: {listed:?}"));
            left.extend(listed.into_iter().map(|name| format!("{dir}/{name}")));
        }
        left
    }
}

#[test]
fn a_crashed_repair_is_recovered_by_the_next_lock_taking_command() {
    use std::path::PathBuf;

    use common::repair::{Child, CrashPhase, End, Kind, RepairScene, crash_specs};
    use repair_support::{json_args, leftover_journals, naming, record, unchanged};

    let log = tlog!();
    // Every kind's repair commits unarmed, and leaves exactly its predicted after state.
    log.step("reference");
    for kind in Kind::ALL {
        let r = RepairScene::new(kind);
        let Child::Ended(outcome) = r.child(&log, "") else {
            panic!("{}: the unarmed repair crashed", kind.label()); // ubs:ignore — fails the test
        };
        assert_eq!(
            outcome["outcome"],
            "committed",
            "{}: {outcome}",
            kind.label()
        );
        log.json(&format!("{} after", kind.label()), &r.subject());
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        assert_eq!(r.phase(), CrashPhase::NoJournal, "{}", kind.label());
        assert_eq!(r.history_names_tx(), 1, "{}", kind.label());
        assert_eq!(r.leftovers(), Vec::<PathBuf>::new(), "{}", kind.label());
    }

    // A crash before `done`: a reader reports it, and `config set` rolls it back.
    log.step("build");
    let r = RepairScene::new(Kind::FlattenTwo);
    log.env(&r.s);
    log.json("subject before", &r.subject());
    log.step("crash");
    let spec = "doctor.repair.step=abort@2";
    assert!(crash_specs(Kind::FlattenTwo).contains(&spec.to_owned()));
    assert_eq!(r.child(&log, spec), Child::Crashed);
    log.json("journal at crash", &r.journal_lines());
    log.json("actions at crash", &r.actions_lines());
    assert_eq!(r.phase(), CrashPhase::Stepped(2));
    let tx = r.tx().unwrap();
    log.step("reader");
    record(&log, &r);
    let read = log.run(&r.s, &json_args(&["status"]));
    read.ok().assert_private();
    assert_eq!(
        naming(read.envelope(), "journal_pending", &tx).len(),
        1,
        "the reader reports the repair"
    );
    unchanged(&log, &r);
    log.step("recover");
    let ran = log.run(
        &r.s,
        &json_args(&["config", "set", "check_interval_hours", "6"]),
    );
    ran.ok().assert_private();
    assert_eq!(
        naming(ran.envelope(), "journal_recovered", &tx),
        [format!(
            "repair transaction {tx} was interrupted; recovery undid it (rolled back)"
        )]
    );
    log.json("subject after recovery", &r.subject());
    assert_eq!(r.accounted(End::Before), Vec::<String>::new());
    assert_eq!(r.phase(), CrashPhase::NoJournal);
    assert_eq!(r.history_names_tx(), 0);
    assert_eq!(leftover_journals(&log, &r), Vec::<String>::new());
    assert_eq!(r.leftovers(), Vec::<PathBuf>::new());

    // A crash from `done` on: rolled forward, with exactly one history line.
    log.step("crash after done");
    let r = RepairScene::new(Kind::FlattenTwo);
    assert_eq!(r.child(&log, "doctor.repair.done=abort"), Child::Crashed);
    log.json("journal at crash", &r.journal_lines());
    assert_eq!(r.phase(), CrashPhase::Done);
    let tx = r.tx().unwrap();
    log.step("recover after done");
    let ran = log.run(
        &r.s,
        &json_args(&["config", "set", "check_interval_hours", "6"]),
    );
    ran.ok().assert_private();
    assert_eq!(
        naming(ran.envelope(), "journal_recovered", &tx),
        [format!(
            "repair transaction {tx} was interrupted; recovery completed it (rolled forward)"
        )]
    );
    log.json("subject after recovery", &r.subject());
    assert_eq!(r.accounted(End::After), Vec::<String>::new());
    assert_eq!(r.history_names_tx(), 1);
    assert_eq!(leftover_journals(&log, &r), Vec::<String>::new());
    assert_eq!(r.leftovers(), Vec::<PathBuf>::new());
}

#[test]
fn a_crashed_repair_is_recovered_by_a_no_op_change() {
    use common::repair::{Child, CrashPhase, End, Kind, RepairScene};
    use repair_support::{leftover_journals, naming};

    let log = tlog!();
    log.step("crash");
    let r = RepairScene::new(Kind::FlattenTwo);
    log.env(&r.s);
    assert_eq!(r.child(&log, "doctor.repair.step=abort@1"), Child::Crashed);
    assert_eq!(r.phase(), CrashPhase::Stepped(1));
    let tx = r.tx().unwrap();
    // The same install again changes nothing, and so recovers through `recover_waiting`.
    log.step("recover");
    let mut argv = r.s.install_args("gh:o/foo", "v1");
    argv.extend(["--progress".to_owned(), "json".to_owned()]);
    let ran = log.run(&r.s, &argv);
    ran.ok().assert_private();
    assert_eq!(
        ran.data()["items"][0]["status"],
        "unchanged",
        "a no-op change"
    );
    assert_eq!(naming(ran.envelope(), "journal_recovered", &tx).len(), 1);
    assert_eq!(r.accounted(End::Before), Vec::<String>::new());
    assert_eq!(leftover_journals(&log, &r), Vec::<String>::new());
}

#[test]
fn a_stuck_repair_journal_warns_and_leaves_unrelated_commits_alone() {
    use std::os::unix::fs::PermissionsExt;

    use common::repair::{Child, CrashPhase, Kind, RepairScene};
    use repair_support::{json_args, naming};

    let log = tlog!();
    log.step("crash");
    let r = RepairScene::new(Kind::Chmod);
    log.env(&r.s);
    assert_eq!(r.child(&log, "fsx.mutate.executed=abort@1"), Child::Crashed);
    assert_eq!(r.phase(), CrashPhase::Stepped(1));
    let tx = r.tx().unwrap();
    // The chmod ran; then its file changed by hand: step 1 is in neither state.
    let credential = r.s.home.env.config().join("omaddons/credentials/github");
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o640)).unwrap();
    let journal = r.journal_lines();

    // E14: the stuck repair fences only the credential it protects, so an install that
    // writes nothing of it commits, warned once.
    log.step("an unrelated committing command proceeds");
    let mut argv = r.s.install_args("gh:o/foo", "v2");
    argv.extend(["--progress".to_owned(), "json".to_owned()]);
    let installed = log.run(&r.s, &argv);
    installed.ok().assert_private();
    let pending = naming(installed.envelope(), "journal_pending", &tx);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(pending[0].contains("(step_unrecognized)"), "{}", pending[0]);
    assert_eq!(r.s.record("foo")["version"], "2", "the install committed");

    log.step("a settings change only warns");
    let warned = log.run(
        &r.s,
        &json_args(&["config", "set", "check_interval_hours", "6"]),
    );
    warned.ok().assert_private();
    let pending: Vec<&str> = warned.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "journal_pending")
        .filter_map(|w| w["message"].as_str())
        .collect();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(
        pending[0].contains(&tx) && pending[0].contains("(step_unrecognized)"),
        "{}",
        pending[0]
    );

    log.step("a reader reports it");
    let read = log.run(&r.s, &json_args(&["status"]));
    read.ok().assert_private();
    assert_eq!(naming(read.envelope(), "journal_pending", &tx).len(), 1);
    assert_eq!(r.journal_lines(), journal, "the journal is left as it is");
    assert_eq!(
        fs::metadata(&credential).unwrap().permissions().mode() & 0o7777,
        0o640,
        "and the credential"
    );
}

#[test]
fn readers_report_a_pending_repair_without_writing() {
    use common::repair::{Child, Kind, RepairScene};
    use repair_support::{json_args, naming, record, unchanged};

    let log = tlog!();
    log.step("crash");
    let r = RepairScene::new(Kind::FlattenTwo);
    log.env(&r.s);
    assert_eq!(r.child(&log, "doctor.repair.step=abort@2"), Child::Crashed);
    let tx = r.tx().unwrap();
    record(&log, &r);
    for command in [&["status"][..], &["list"], &["history"]] {
        log.step(command[0]);
        let read = log.run(&r.s, &json_args(command));
        read.ok().assert_private();
        assert_eq!(
            naming(read.envelope(), "journal_pending", &tx),
            [format!(
                "repair transaction {tx} did not finish; the next omaddons command that changes addons, settings or targets recovers it (dry runs never do)"
            )],
            "{}",
            command[0]
        );
        unchanged(&log, &r);
    }
}

// ---------------------------------------------------------------------------------------
// The repair family's adversarial matrix (E5 s7, common rule 14): synthetic repairs of
// every fix kind crashed with real `abort`s at every repair failpoint and every primitive
// boundary, failed by injected I/O errors, their recovery crashed at every inverse and
// repeated, recovered by three different commands, and quarantined across mounts. Each
// stop is held to `common::repair::assert_settled`.

#[cfg(test)]
mod repair_matrix {
    use std::collections::BTreeSet;
    use std::time::Instant;

    use super::common::repair::{
        Child, End, Kind, RepairScene, assert_settled, attributed, checked_sweep, config_set,
        crash_points, crash_specs, point_of,
    };
    use super::common::tlog::TestLog;

    /// The reference run: the kind's repair commits unarmed, to exactly its predicted
    /// after state.
    pub fn reference(log: &TestLog, kind: Kind) {
        log.step(&format!("{} unarmed", kind.label()));
        let r = RepairScene::new(kind);
        let Child::Ended(outcome) = r.child(log, "") else {
            panic!("{}: the unarmed repair crashed", kind.label()); // ubs:ignore — fails the test
        };
        assert_eq!(outcome["outcome"], "committed", "{}", kind.label());
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
    }

    /// Every crash of `kind`'s repair, each settled by the oracle; every crash point the
    /// kind has must be hit. Returns how many crash states it settled.
    pub fn run(log: &TestLog, kind: Kind) -> usize {
        let started = Instant::now();
        reference(log, kind);
        let mut exercised = BTreeSet::new();
        let mut states = 0;
        for spec in crash_specs(kind) {
            log.step(&format!("{} at {spec}", kind.label()));
            let r = RepairScene::new(kind);
            assert_eq!(
                r.child(log, &spec),
                Child::Crashed,
                "{}: {spec} is on the repair's path",
                kind.label()
            );
            exercised.extend(point_of(&spec));
            settle(log, &r);
            states += 1;
        }
        for (_, r) in checked_sweep(|| RepairScene::new(kind), log) {
            exercised.extend(attributed(&r));
            settle(log, &r);
            states += 1;
        }
        log.json("crashes exercised", &exercised);
        let missing: Vec<String> = crash_points(kind).difference(&exercised).cloned().collect();
        assert!(
            missing.is_empty(),
            "{}: crash point never hit: {}",
            kind.label(),
            missing.join(", ")
        );
        log.note(format!(
            "{}: {states} crash states, {} crash points, in {:.1} s",
            kind.label(),
            exercised.len(),
            started.elapsed().as_secs_f64()
        ));
        states
    }

    /// The crash's journal and actions logged, then the oracle with `config set`.
    pub fn settle(log: &TestLog, r: &RepairScene) {
        log.json("journal at crash", &r.journal_lines());
        log.json("actions at crash", &r.actions_lines());
        assert_settled(log, r, &config_set());
    }
}

/// One test per kind, so libtest runs the kinds in parallel.
macro_rules! repair_crash_matrix {
    ($($test:ident => $kind:ident),* $(,)?) => {$(
        #[test]
        fn $test() {
            let log = tlog!();
            repair_matrix::run(&log, common::repair::Kind::$kind);
        }
    )*};
}

repair_crash_matrix! {
    repair_crash_matrix_write_over => WriteOver,
    repair_crash_matrix_write_create => WriteCreate,
    repair_crash_matrix_chmod => Chmod,
    repair_crash_matrix_quarantine_state_file => QuarantineStateFile,
    repair_crash_matrix_quarantine_client_dir => QuarantineClientDir,
    repair_crash_matrix_quarantine_temp_symlink => QuarantineTempSymlink,
    repair_crash_matrix_flatten_two => FlattenTwo,
    repair_crash_matrix_flatten_three => FlattenThree,
}

#[test]
fn a_crash_between_a_flattens_renames_recovers_the_whole_before_state() {
    use common::repair::{Child, End, Kind, RepairScene, assert_settled, config_set};

    let log = tlog!();
    let cases = [
        (Kind::FlattenTwo, "doctor.repair.step=abort@2"),
        (Kind::FlattenTwo, "fsx.mutate.logged=abort@1"),
        (Kind::FlattenThree, "fsx.mutate.logged=abort@1"),
        (Kind::FlattenThree, "doctor.repair.step=abort@2"),
        (Kind::FlattenThree, "fsx.mutate.logged=abort@2"),
        (Kind::FlattenThree, "doctor.repair.step=abort@3"),
    ];
    for (kind, spec) in cases {
        log.step(&format!("{} at {spec}", kind.label()));
        let r = RepairScene::new(kind);
        assert_eq!(r.child(&log, spec), Child::Crashed, "{spec}");
        let settled = assert_settled(&log, &r, &config_set());
        assert_eq!(settled.end, End::Before, "{} at {spec}", kind.label());
        let provisional: Vec<String> = fs::read_dir(r.s.addons())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".omaddons-flatten-"))
            .collect();
        assert_eq!(
            provisional,
            Vec::<String>::new(),
            "{} at {spec}",
            kind.label()
        );
    }
}

#[test]
fn a_repair_recovery_crashed_at_every_inverse_finishes() {
    use common::repair::{Child, End, Kind, RepairScene, assert_settled, config_set};

    let log = tlog!();
    for kind in Kind::ALL {
        let n = kind.primitives();
        for j in 1..=n {
            log.step(&format!(
                "{}: recovery crashed at inverse {j} of {n}",
                kind.label()
            ));
            let r = RepairScene::new(kind);
            let spec = format!("fsx.mutate.logged=abort@{n}");
            assert_eq!(r.child(&log, &spec), Child::Crashed, "{}", kind.label());
            let crashed = log
                .cmd(&r.s, &config_set())
                .failpoints(&format!("doctor.recover.step=abort@{j}"))
                .run();
            crashed.aborted();
            crashed.assert_private();
            log.json("journal after the crashed recovery", &r.journal_lines());
            let settled = assert_settled(&log, &r, &config_set());
            assert_eq!(settled.end, End::Before, "{} at inverse {j}", kind.label());
        }
    }
}

#[test]
fn a_repair_recovered_three_times_changes_nothing_after_the_first() {
    use common::repair::{Child, Kind, RepairScene, assert_settled, config_set};

    let log = tlog!();
    for kind in Kind::ALL {
        let before_done = format!("fsx.mutate.logged=abort@{}", kind.primitives());
        for spec in [before_done.as_str(), "doctor.repair.done=abort"] {
            log.step(&format!("{} at {spec}", kind.label()));
            let r = RepairScene::new(kind);
            assert_eq!(r.child(&log, spec), Child::Crashed, "{spec}");
            assert_settled(&log, &r, &config_set());
            for (tree, root) in r.trees() {
                log.tree(tree, &root);
            }
            for again in 1..=3 {
                let ran = log.run(&r.s, &config_set());
                ran.ok().assert_private();
                assert_eq!(
                    ran.warnings(),
                    Vec::<String>::new(),
                    "{} at {spec}: recovery {again} more",
                    kind.label()
                );
                // Taking the global lock rewrites its holder record; nothing else moves.
                for (tree, root) in r.trees() {
                    let diff = log.diff(tree, &root);
                    let moved: Vec<String> = diff
                        .first(diff.total)
                        .into_iter()
                        // ubs:ignore — comparing a tree difference's text, not a secret
                        .filter(|path| path != "~ omaddons/lock (content)")
                        .collect();
                    assert_eq!(
                        moved,
                        Vec::<String>::new(),
                        "{} at {spec}: recovery {again} more wrote {tree}",
                        kind.label()
                    );
                }
            }
        }
    }
}

#[test]
fn io_failures_in_a_repair_leave_the_subject_whole() {
    use common::repair::{Child, End, Kind, RepairScene, assert_settled, config_set};

    let log = tlog!();
    // How many times a crash sweep at `failpoint` crashes `kind`'s repair: the hits of a
    // run that fails nowhere.
    let hits = |kind: Kind, failpoint: &str| -> u64 {
        for n in 1..=64 {
            log.step(&format!("{} counting {failpoint} hits: {n}", kind.label()));
            let r = RepairScene::new(kind);
            if r.child(&log, &format!("{failpoint}=abort@{n}")) != Child::Crashed {
                return n - 1;
            }
        }
        panic!("{failpoint} still hit 64 times"); // ubs:ignore — fails the test
    };
    for kind in Kind::ALL {
        let n = kind.primitives();
        let mut specs: Vec<(String, Option<u32>)> = Vec::new();
        for k in 1..=n {
            // Refused before it ran: the steps before it are undone.
            specs.push((format!("fsx.mutate.backed_up=error@{k}"), Some(k - 1)));
            // Failed after it ran: it is undone too.
            specs.push((format!("fsx.mutate.executed=error@{k}"), Some(k)));
        }
        for failpoint in ["fsx.mutate.checked", "fsx.append_durable.before"] {
            let total = hits(kind, failpoint);
            assert!(
                total > u64::from(n),
                "{}: {failpoint} hit {total} times",
                kind.label()
            );
            specs.extend((1..=total).map(|at| (format!("{failpoint}=error@{at}"), None)));
        }
        for (spec, restored) in specs {
            log.step(&format!("{} failing at {spec}", kind.label()));
            let r = RepairScene::new(kind);
            let Child::Ended(outcome) = r.child(&log, &spec) else {
                panic!("{}: an error crashed the child at {spec}", kind.label()); // ubs:ignore — fails the test
            };
            let settled = assert_settled(&log, &r, &config_set());
            let committed = outcome["outcome"] == "committed";
            assert_eq!(
                settled.end == End::After,
                committed,
                "{} at {spec}: the outcome {outcome} and the end {:?} agree",
                kind.label(),
                settled.end
            );
            if outcome["outcome"] == "rolled_back"
                && let Some(restored) = restored
            {
                assert_eq!(
                    outcome["backups_restored"],
                    restored,
                    "{} at {spec}: {outcome}",
                    kind.label()
                );
            }
            if spec.starts_with("fsx.mutate.executed") {
                let actions = r.actions_lines();
                let last: serde_json::Value =
                    serde_json::from_str(actions.last().unwrap()).unwrap();
                assert_eq!(last["ok"], false, "{} at {spec}: {actions:?}", kind.label());
            }
        }
    }
}

#[test]
fn config_set_install_and_undo_recover_a_repair_identically() {
    use common::repair::{Child, Kind, RepairScene, assert_settled, config_set, json_args};

    let log = tlog!();
    for kind in [Kind::Chmod, Kind::QuarantineClientDir, Kind::FlattenThree] {
        for spec in [
            "doctor.repair.prepared=abort",
            "fsx.mutate.executed=abort@1",
            "doctor.repair.done=abort",
        ] {
            let mut seen = Vec::new();
            for by in ["config set", "install", "undo"] {
                log.step(&format!("{} at {spec}, recovered by {by}", kind.label()));
                let r = RepairScene::new(kind);
                let install_tx = r.install_tx();
                assert_eq!(r.child(&log, spec), Child::Crashed, "{spec}");
                let argv = match by {
                    "config set" => config_set(),
                    "install" => {
                        let mut argv = r.s.install_args("gh:o/foo", "v2");
                        argv.extend(["--progress".to_owned(), "json".to_owned()]);
                        argv
                    }
                    _ => json_args(&["undo", &install_tx]),
                };
                let settled = assert_settled(&log, &r, &argv);
                // Each command did what it does without a repair to recover.
                match by {
                    "install" => assert_eq!(r.s.record("foo")["version"], "2", "{by}"),
                    "undo" => assert_eq!(r.s.record("foo"), serde_json::Value::Null, "{by}"),
                    _ => assert_eq!(r.s.record("foo")["version"], "1", "{by}"),
                }
                seen.push((by, settled.subject, settled.recovered));
            }
            let (first, rest) = seen.split_first().unwrap();
            for other in rest {
                assert_eq!(
                    (&other.1, &other.2),
                    (&first.1, &first.2),
                    "{} at {spec}: {} and {} recovered it differently",
                    kind.label(),
                    first.0,
                    other.0
                );
            }
        }
    }
}

#[test]
fn a_repair_on_a_client_on_another_mount_quarantines_there() {
    use common::repair::{Child, End, Kind, RepairScene};

    let log = tlog!();
    for kind in [Kind::QuarantineClientDir, Kind::FlattenTwo] {
        log.step(&format!("{} on a client mount of its own", kind.label()));
        let r = RepairScene::new(kind);
        let client = r.s.home.client("_retail_");
        let Some(ended) = r.child_bound(&log, "", &client) else {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!(
                    "SKIP a_repair_on_a_client_on_another_mount_quarantines_there: no unprivileged user and mount namespaces here\n"
                ),
            );
            return;
        };
        let Child::Ended(outcome) = ended else {
            panic!("{}: the bound repair crashed", kind.label()); // ubs:ignore — fails the test
        };
        assert_eq!(
            outcome["outcome"],
            "committed",
            "{}: {outcome}",
            kind.label()
        );
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        let run_quarantine =
            r.s.home
                .state()
                .join("doctor/runs")
                .join(&r.run_id)
                .join("quarantine");
        let home_side: Vec<String> = if run_quarantine.is_dir() {
            common::trees::tree_entries(&run_quarantine)
                .into_keys()
                .filter(|rel| rel.contains("Interface"))
                .collect()
        } else {
            Vec::new()
        };
        assert_eq!(home_side, Vec::<String>::new(), "{}", kind.label());
    }
}

#[test]
fn a_config_victim_on_another_mount_than_state_is_refused_before_anything_moves() {
    use common::repair::{Child, Kind, RepairScene};

    let log = tlog!();
    let r = RepairScene::new(Kind::QuarantineConfigFile);
    let credentials = r.s.home.env.config().join("omaddons/credentials");
    let state = r.s.home.state();
    let entries = |dir: &std::path::Path| -> Vec<(String, String)> {
        common::trees::tree_entries(dir)
            .into_iter()
            .filter(|(rel, _)| !rel.starts_with("inflight"))
            .map(|(rel, entry)| (rel, format!("{entry:?}")))
            .collect()
    };
    let (config_before, state_before) = (entries(&r.s.home.env.config()), entries(&state));
    let Some(ended) = r.child_bound(&log, "", &credentials) else {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "SKIP a_config_victim_on_another_mount_than_state_is_refused_before_anything_moves: no unprivileged user and mount namespaces here\n"
            ),
        );
        return;
    };
    let Child::Ended(outcome) = ended else {
        panic!("the bound repair crashed"); // ubs:ignore — fails the test
    };
    assert_eq!(
        (outcome["stage"].as_str(), outcome["refused"].as_str()),
        (Some("lower"), Some("filesystem_mismatch")),
        "{outcome}"
    );
    assert_eq!(
        entries(&r.s.home.env.config()),
        config_before,
        "config unchanged"
    );
    assert_eq!(
        entries(&state),
        state_before,
        "state unchanged but for the transaction's in-flight lock: no journal, no roots"
    );
    assert_eq!(r.journal_lines(), None);
}

#[test]
fn a_repair_never_writes_wtf_through_the_binary() {
    use common::repair::{Child, Kind, RepairScene, assert_settled, config_set};

    let log = tlog!();
    for kind in Kind::ALL {
        log.step(&format!("{} committed", kind.label()));
        let r = RepairScene::new(kind);
        assert!(matches!(r.child(&log, ""), Child::Ended(_)));
        assert_eq!(r.wtf_named(), Vec::<String>::new(), "{}", kind.label());
        log.step(&format!("{} crashed and recovered", kind.label()));
        let r = RepairScene::new(kind);
        let spec = format!("fsx.mutate.logged=abort@{}", kind.primitives());
        assert_eq!(r.child(&log, &spec), Child::Crashed);
        assert_settled(&log, &r, &config_set());
        let history = fs::read_to_string(r.s.home.state().join("history.jsonl")).unwrap();
        for line in history
            .lines()
            .filter(|line| line.contains("\"kind\":\"repair\""))
        {
            assert!(!line.to_ascii_lowercase().contains("/wtf/"), "{line}");
        }
    }
}

// ---------------------------------------------------------------------------------------
// The undo of a repair (E7 s5, the design gate of the undo of repairs): each kind's repair
// committed, then its undo, `omaddons undo <T>` through the binary, crashed with real
// `abort`s at every point on its path (the run directory, the engine's before the
// journal, every repair-journal point and every tracked fsx boundary of each inverse, and
// every `fsx.mutate.checked` hit). Each stop is held to `assert_undo_settled`: before the
// undo's durable `done` the whole repaired state, from it the whole original.

#[cfg(test)]
mod repair_undo_matrix {
    use std::collections::BTreeSet;
    use std::time::Instant;

    use super::common::repair::{
        End, Kind, RepairScene, assert_undo_settled, config_set, log_point, undo_attributed,
        undo_checked_sweep, undo_crash_points, undo_crash_specs, undo_point_of,
    };
    use super::common::tlog::TestLog;

    /// The reference run: the kind's undo commits unarmed, to exactly the original state,
    /// its own preserved originals in place.
    pub fn reference(log: &TestLog, kind: Kind) {
        log.step(&format!("the undo of {} unarmed", kind.label()));
        let r = RepairScene::repaired(log, kind);
        r.undo(log, "").ok();
        assert_eq!(
            r.accounted(End::Before),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        assert_eq!(r.undo_accounted(), Vec::<String>::new(), "{}", kind.label());
    }

    /// Every crash of `kind`'s undo, each settled by the oracle; every crash point the
    /// undo has must be hit. Returns how many crash states it settled.
    pub fn run(log: &TestLog, kind: Kind) -> usize {
        let started = Instant::now();
        reference(log, kind);
        let mut exercised = BTreeSet::new();
        let mut states = 0;
        for spec in undo_crash_specs(kind) {
            log.step(&format!("the undo of {} at {spec}", kind.label()));
            let r = RepairScene::repaired(log, kind);
            r.undo(log, &spec).aborted();
            exercised.extend(undo_point_of(&spec));
            log_point(log, kind, &spec);
            assert_undo_settled(log, &r, &config_set());
            states += 1;
        }
        for (n, r) in undo_checked_sweep(kind, log) {
            exercised.extend(undo_attributed(&r));
            log_point(log, kind, &format!("fsx.mutate.checked=abort@{n}"));
            assert_undo_settled(log, &r, &config_set());
            states += 1;
        }
        log.json("crashes_exercised", &exercised);
        let missing: Vec<String> = undo_crash_points(kind)
            .difference(&exercised)
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "the undo of {}: crash point never hit: {}",
            kind.label(),
            missing.join(", ")
        );
        log.note(format!(
            "the undo of {}: {states} crash states, {} crash points, in {:.1} s",
            kind.label(),
            exercised.len(),
            started.elapsed().as_secs_f64()
        ));
        states
    }
}

/// One test per kind, so libtest runs the kinds in parallel.
macro_rules! repair_undo_crash_matrix {
    ($($test:ident => $kind:ident),* $(,)?) => {$(
        #[test]
        fn $test() {
            let log = tlog!();
            repair_undo_matrix::run(&log, common::repair::Kind::$kind);
        }
    )*};
}

repair_undo_crash_matrix! {
    repair_undo_crash_matrix_write_over => WriteOver,
    repair_undo_crash_matrix_write_create => WriteCreate,
    repair_undo_crash_matrix_chmod => Chmod,
    repair_undo_crash_matrix_quarantine_state_file => QuarantineStateFile,
    repair_undo_crash_matrix_quarantine_client_dir => QuarantineClientDir,
    repair_undo_crash_matrix_quarantine_temp_symlink => QuarantineTempSymlink,
    repair_undo_crash_matrix_flatten_two => FlattenTwo,
    repair_undo_crash_matrix_flatten_three => FlattenThree,
    repair_undo_crash_matrix_suspend => Suspend,
}

/// The undo `U` of a repair, itself undone (a redo), crashed at every point on its path:
/// the oracle swaps again, the whole original `O` before the redo's durable `done`, the
/// whole repaired `R` from it, with `U`'s line kept and the redo's appearing once.
#[test]
fn repair_undo_of_an_undo_crash_matrix() {
    use std::collections::BTreeSet;

    use common::repair::{
        End, Kind, RepairScene, assert_redo_settled, config_set, log_point, redo_checked_sweep,
        undo_attributed, undo_crash_points, undo_crash_specs, undo_point_of,
    };

    let log = tlog!();
    for kind in [Kind::Chmod, Kind::FlattenTwo, Kind::Suspend] {
        log.step(&format!("the undo of the undo of {} unarmed", kind.label()));
        let r = RepairScene::undone(&log, kind);
        r.redo(&log, "").ok();
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );
        assert_eq!(r.redo_history().len(), 1, "{}", kind.label());
        let mut exercised = BTreeSet::new();
        let mut states = 0;
        for spec in undo_crash_specs(kind) {
            log.step(&format!(
                "the undo of the undo of {} at {spec}",
                kind.label()
            ));
            let r = RepairScene::undone(&log, kind);
            r.redo(&log, &spec).aborted();
            exercised.extend(undo_point_of(&spec));
            log_point(&log, kind, &spec);
            assert_redo_settled(&log, &r, &config_set());
            states += 1;
        }
        for (n, r) in redo_checked_sweep(kind, &log) {
            exercised.extend(undo_attributed(&r));
            log_point(&log, kind, &format!("fsx.mutate.checked=abort@{n}"));
            assert_redo_settled(&log, &r, &config_set());
            states += 1;
        }
        log.json("crashes_exercised", &exercised);
        let missing: Vec<String> = undo_crash_points(kind)
            .difference(&exercised)
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "the undo of the undo of {}: crash point never hit: {}",
            kind.label(),
            missing.join(", ")
        );
        log.note(format!(
            "the undo of the undo of {}: {states} crash states, {} crash points",
            kind.label(),
            exercised.len()
        ));
    }
}

/// An undo crashed after its last inverse (before `done`), its recovery crashed at every
/// inverse it runs, then recovered: the repaired state, and three more recoveries change
/// nothing.
#[test]
fn a_repair_undo_recovery_crashed_at_every_inverse_finishes() {
    use common::repair::{End, Kind, RepairScene, assert_undo_settled, config_set};

    let log = tlog!();
    for kind in Kind::ALL.into_iter().chain([Kind::Suspend]) {
        let n = kind.undo_inverses();
        for j in 1..=n {
            log.step(&format!(
                "the undo of {}: recovery crashed at inverse {j} of {n}",
                kind.label()
            ));
            let r = RepairScene::repaired(&log, kind);
            r.undo(&log, &format!("fsx.mutate.logged=abort@{n}"))
                .aborted();
            let crashed = log
                .cmd(&r.s, &config_set())
                .failpoints(&format!("doctor.recover.step=abort@{j}"))
                .run();
            crashed.aborted();
            crashed.assert_private();
            let settled = assert_undo_settled(&log, &r, &config_set());
            assert_eq!(settled.end, End::After, "{} at inverse {j}", kind.label());
            let settled_subject = r.subject();
            for again in 1..=3 {
                log.run(&r.s, &config_set()).ok();
                assert_eq!(
                    r.subject(),
                    settled_subject,
                    "{} at inverse {j}: recovery {again} more changed it",
                    kind.label()
                );
            }
        }
    }
}

/// An injected failure at every inverse rolls the undo back in process: the repaired
/// state, no undo line, the undo's run `DONE_FAILED`; `doctor undo` of the fix run exits 3.
/// A failure after `done` completes the undo instead.
#[test]
fn io_failures_in_a_repair_undo_roll_it_back() {
    use common::repair::{End, Kind, RepairScene};

    let log = tlog!();
    for kind in Kind::ALL.into_iter().chain([Kind::Suspend]) {
        let n = kind.undo_inverses();
        let mut specs: Vec<String> = (1..=n)
            .flat_map(|k| {
                [
                    format!("fsx.mutate.executed=error@{k}"),
                    format!("doctor.repair.step=error@{k}"),
                ]
            })
            .collect();
        if kind == Kind::WriteOver {
            specs.push("fsx.mutate.backed_up=error@1".to_owned());
        }
        for spec in &specs {
            log.step(&format!("the undo of {} with {spec}", kind.label()));
            let r = RepairScene::repaired(&log, kind);
            let ran = r.undo(&log, spec);
            ran.assert_private();
            assert!(ran.run.code > 0, "{} with {spec}: it fails", kind.label());
            assert_eq!(r.accounted(End::After), Vec::<String>::new(), "{spec}");
            assert_eq!(r.undo_history().len(), 0, "{} with {spec}", kind.label());
            let report: serde_json::Value = serde_json::from_slice(
                &fs::read(r.undo_run_dir().unwrap().join("report.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                report["state"],
                "DONE_FAILED",
                "{} with {spec}",
                kind.label()
            );
            assert_eq!(r.leftovers(), Vec::<std::path::PathBuf>::new(), "{spec}");
        }
        log.step(&format!(
            "doctor undo of {} with an inverse failing",
            kind.label()
        ));
        let r = RepairScene::repaired(&log, kind);
        let ran = log
            .cmd(
                &r.s,
                &common::repair::json_args(&["doctor", "undo", &r.run_id]),
            )
            .failpoints("fsx.mutate.executed=error@1")
            .run();
        ran.exit(3);
        assert_eq!(
            r.accounted(End::After),
            Vec::<String>::new(),
            "{}",
            kind.label()
        );

        // E5's `doctor.repair.done` is hit once `done` is durable: from there the undo
        // stands, and a failure is rolled forward in process, as at `history`.
        for spec in ["doctor.repair.done=error", "doctor.repair.history=error"] {
            log.step(&format!(
                "the undo of {} with {spec}, after done",
                kind.label()
            ));
            let r = RepairScene::repaired(&log, kind);
            r.undo(&log, spec);
            assert_eq!(r.accounted(End::Before), Vec::<String>::new(), "{spec}");
            assert_eq!(r.undo_accounted(), Vec::<String>::new(), "{spec}");
            assert_eq!(r.undo_history().len(), 1, "{}: the line once", kind.label());
        }
    }
}

/// An undo whose run directory cannot be created moves nothing: `omaddons undo` fails
/// with `io`, `doctor undo` exits 73, and neither leaves a journal, an in-flight lock or a
/// published run directory (the runtime's own `.<id>.tmp` stage stays, as for every run
/// that fails to start).
#[test]
fn a_repair_undo_that_cannot_create_its_run_moves_nothing() {
    use common::repair::{End, Kind, RepairScene, json_args};

    let log = tlog!();
    let r = RepairScene::repaired(&log, Kind::FlattenTwo);
    let ran = r.undo(&log, "doctor.run.staged=error");
    assert_eq!(ran.error_code(), Some("io"), "omaddons undo");
    let ran = log
        .cmd(&r.s, &json_args(&["doctor", "undo", &r.run_id]))
        .failpoints("doctor.run.staged=error")
        .run();
    ran.exit(73);
    assert_eq!(r.accounted(End::After), Vec::<String>::new());
    assert_eq!(r.undo_tx(), None, "no journal, no in-flight lock");
    assert_eq!(r.undo_run_dir(), None, "no published run directory");
}

/// A crashed undo is recovered the same by `config set`, an `install` and an `undo` of
/// that install: each command does its own work besides, and the subject ends alike.
#[test]
fn config_set_install_and_undo_recover_a_repair_undo_identically() {
    use common::repair::{Kind, RepairScene, assert_undo_settled, config_set, json_args};

    let log = tlog!();
    let kind = Kind::FlattenTwo;
    for spec in ["doctor.repair.step=abort@2", "doctor.repair.done=abort"] {
        let mut seen = Vec::new();
        for by in ["config set", "install", "undo"] {
            log.step(&format!(
                "the undo of {} at {spec}, recovered by {by}",
                kind.label()
            ));
            let r = RepairScene::repaired(&log, kind);
            let install_tx = r.install_tx();
            r.undo(&log, spec).aborted();
            let argv = match by {
                "config set" => config_set(),
                "install" => {
                    let mut argv = r.s.install_args("gh:o/foo", "v2");
                    argv.extend(["--progress".to_owned(), "json".to_owned()]);
                    argv
                }
                _ => json_args(&["undo", &install_tx]),
            };
            let settled = assert_undo_settled(&log, &r, &argv);
            match by {
                "install" => assert_eq!(r.s.record("foo")["version"], "2", "{by}"),
                "undo" => assert_eq!(r.s.record("foo"), serde_json::Value::Null, "{by}"),
                _ => assert_eq!(r.s.record("foo")["version"], "1", "{by}"),
            }
            seen.push((by, r.subject(), settled.end, settled.recovered));
        }
        let (first, rest) = seen.split_first().unwrap();
        for other in rest {
            assert_eq!(
                (&other.1, &other.2, &other.3),
                (&first.1, &first.2, &first.3),
                "the undo of {} at {spec}: {} and {} recovered it differently",
                kind.label(),
                first.0,
                other.0
            );
        }
    }
}
