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

mod common;

use std::collections::BTreeMap;
use std::fs;

use common::archives::zip_text;
use common::engine::{Scene, shapes, warning_codes};
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
    }

    /// `op` run with the failpoints `spec` armed, then the oracle. Each folder, on its
    /// own, is wholly old or wholly new at every instant (or absent, between the two
    /// renames of the `fallback`), and SavedVariables are wholly old or wholly new; a
    /// reader reports the unfinished transaction and changes nothing; the next command
    /// that takes the lock recovers (with `recover` armed it may die mid-recovery,
    /// keeping the journal, and the command after it finishes); then folders and
    /// SavedVariables sit on one side, consistent with the generation and history, with
    /// nothing stray in AddOns and the same lock file.
    pub fn crash(op: Op, spec: &str, recover: &str, new: &Foo, fallback: bool) -> Crash {
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
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
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
        }
    }

    /// Every failpoint as a crash of `op`. Some crash before `prepared` leaves
    /// SavedVariables copies, which the sweep takes.
    pub fn matrix(op: Op) {
        let new = expected_new(op);
        let mut swept = false;
        let crashed_at: Vec<&str> = FAILPOINTS
            .iter()
            .copied()
            .filter(|failpoint| {
                let done = crash(op, &format!("{failpoint}=abort"), "", &new, false);
                swept |= done.swept;
                done.crashed
            })
            .collect();
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
    /// rename must leave the folders mixed until recovery: the window is reached.
    pub fn renames(what: &str, crash: impl Fn(&str, bool) -> Crash) {
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
    }

    /// Every journaled rename of `op` as a crash.
    pub fn rename_matrix(op: Op) {
        let new = expected_new(op);
        renames(&format!("{op:?}"), |spec, fallback| {
            crash(op, spec, "", &new, fallback)
        });
    }

    /// Every crash of `op` that leaves a journal, recovered by a command that dies in
    /// that recovery too: after each step a rollback undoes (`engine.recover.step=abort@K`
    /// for every K it reaches), as a roll-forward starts (`engine.recover.forward`), and
    /// before an undo's SavedVariables restore (`engine.recover.sv`). The command after it
    /// finishes, and the oracle holds then. Each kind of recovery crash must happen.
    pub fn recovery_matrix(op: Op, kinds: &[&str]) {
        let new = expected_new(op);
        let mut crashed: Vec<(&str, String)> = Vec::new();
        for failpoint in FAILPOINTS {
            let spec = format!("{failpoint}=abort");
            let mut recovers: Vec<String> = Vec::new();
            for k in 1..=8 {
                let recover = format!("engine.recover.step=abort@{k}");
                let done = crash(op, &spec, &recover, &new, false);
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
                if crash(op, &spec, &recover, &new, false).recovering_crashed {
                    crashed.push((failpoint, recover));
                }
            }
        }
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
    pub fn crash_shape(shape: Shape, spec: &str, new: &Side, fallback: bool) -> Crash {
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

        // The next command that takes the lock recovers.
        let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
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
        }
    }

    /// Every journaled rename of `shape` as a crash.
    pub fn shape_rename_matrix(shape: Shape) {
        let new = reference(shape);
        renames(&format!("{shape:?}"), |spec, fallback| {
            crash_shape(shape, spec, &new, fallback)
        });
    }

    /// Every failpoint as a crash of `shape`. Unless it is records-only, some crash
    /// before `prepared` leaves SavedVariables copies, which the sweep takes.
    pub fn shape_matrix(shape: Shape) {
        let new = reference(shape);
        let mut swept = false;
        let crashed_at: Vec<&str> = FAILPOINTS
            .iter()
            .copied()
            .filter(|failpoint| {
                let done = crash_shape(shape, &format!("{failpoint}=abort"), &new, false);
                swept |= done.swept;
                done.crashed
            })
            .collect();
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
    pub fn record_matrix(op: RecordsOp) {
        let mut crashed_at = Vec::new();
        for failpoint in FAILPOINTS {
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

            // The next command that takes the lock recovers.
            let bar = s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
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
fn crash_matrix_undo() {
    matrix(Op::Undo);
}

#[test]
fn crash_matrix_undo_keeping_saved_variables() {
    matrix(Op::UndoKeep);
}

#[test]
fn crash_matrix_undo_while_the_client_runs() {
    matrix(Op::UndoRunning);
}

#[test]
fn a_crash_at_every_rename_of_an_undo() {
    rename_matrix(Op::Undo);
}

#[test]
fn a_crash_during_recovery_of_an_install() {
    recovery_matrix(
        Op::Install,
        &["engine.recover.step", "engine.recover.forward"],
    );
}

#[test]
fn a_crash_during_recovery_of_an_update() {
    recovery_matrix(
        Op::Update,
        &["engine.recover.step", "engine.recover.forward"],
    );
}

#[test]
fn a_crash_during_recovery_of_an_undo() {
    recovery_matrix(
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
    let new = expected_new(Op::Update);
    let mut crashed = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = support::crash(
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
    rename_matrix(Op::Install);
}

#[test]
fn a_crash_at_every_rename_of_an_update() {
    rename_matrix(Op::Update);
}

#[test]
fn a_crash_at_every_rename_of_a_remove() {
    rename_matrix(Op::Remove);
}

#[test]
fn a_crash_at_every_rename_of_an_update_of_three() {
    shape_rename_matrix(Shape::UpdateMany);
}

#[test]
fn a_crash_at_every_rename_of_a_handover() {
    shape_rename_matrix(Shape::Handover);
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
    for failpoint in ["engine.swap.before_rename", "engine.swapped"] {
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

        let bar = s.install("gh:o/bar", "bar");
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

    let r = s.home.run(&["config", "set", "running_policy", "refuse"]);
    let envelope = r.expect(0);
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
        let probe = client.stage(&tx).join("probe");
        let probe = Probe {
            scopes: &scopes,
            dir: &probe,
        };
        let topology = match preflight(&client, &backup.wtf(), Some(probe)) {
            Ok(topology) => topology,
            Err(e) => {
                let left: Vec<PathBuf> = [client.stage(&tx), paths.inflight_lock(&tx)]
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
                "child_restore",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ];
            let mut cmd = match bind {
                None => Command::new(std::env::current_exe().unwrap()),
                Some(dir) => {
                    let mut cmd = Command::new("unshare");
                    cmd.args([
                        "-r",
                        "-m",
                        "sh",
                        "-c",
                        "mount --bind \"$0\" \"$0\" && exec \"$@\"",
                    ])
                    .arg(dir)
                    .arg(std::env::current_exe().unwrap());
                    cmd
                }
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

    /// Whether this machine lets an unprivileged process bind-mount in a user and mount
    /// namespace (`unshare -rm`), tried on `dir`.
    pub fn can_bind_mount(dir: &Path) -> bool {
        Command::new("unshare")
            .args(["-r", "-m", "mount", "--bind"])
            .arg(dir)
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
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
    pub fn crash(engine: &str, recover: &str, live: bool) -> Restored {
        crash_op(Op::Restore, engine, recover, live)
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
    pub fn preserve_crash(engine: &str) -> Preserving {
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

        let bar = r.s.run(&r.install_bar(), "");
        bar.expect(0);
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
        }
    }

    /// [`crash`] for `op`.
    pub fn crash_op(op: Op, engine: &str, recover: &str, live: bool) -> Restored {
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
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
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
            let tx = s.history_txs()[old_history.len()].clone();
            let previous = snapshots.join(&tx).join("WTF.previous");
            assert_eq!(
                previous.is_dir().then(|| tree_hash(&previous)),
                old,
                "{}",
                context("WTF.previous is not the tree it replaced")
            );
            assert!(
                s.home.state().join("snapshots").join(&tx).is_dir(),
                "{}",
                context("the restore's state snapshot")
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
        }
    }
}

/// Every failpoint as a crash of the undo of a restore that was played on since: the
/// restored tree goes and the one from before comes back, wholly or not at all.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_undo_restore() {
    use restore_support::{Op, crash_op};

    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = crash_op(Op::UndoRestore, &format!("{failpoint}=abort"), "", true);
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

    for failpoint in [
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.committed",
    ] {
        let done = crash_op(Op::UndoRestore, &format!("{failpoint}=abort"), "", false);
        assert!(done.crashed, "{failpoint}: {done:?}");
    }
    let mut renames = Vec::new();
    for n in 1..=3 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        if !crash_op(Op::UndoRestore, &spec, "", false).crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1], "one rename out");
}

/// A crash before each journaled rename of the undo of a restore, and in its recovery:
/// after each rename its rollback undoes, and as a roll-forward starts.
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_every_rename_and_recovery_of_an_undo_of_a_restore() {
    use restore_support::{Op, crash_op};

    let mut renames = Vec::new();
    for n in 1..=4 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        if !crash_op(Op::UndoRestore, &spec, "", true).crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1, 2]);
    let mut recoveries = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = crash_op(
            Op::UndoRestore,
            "engine.restore.snapshotted=abort",
            &recover,
            true,
        );
        if !done.recovering_crashed {
            break;
        }
        recoveries.push(k);
    }
    assert_eq!(recoveries, [1, 2]);
    let done = crash_op(
        Op::UndoRestore,
        "engine.swapped=abort",
        "engine.recover.forward=abort",
        true,
    );
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
}

/// I/O failures where the undo of a restore writes, each injected once at every hit in
/// turn: `WTF/` stays wholly old or wholly new and what is left recovers. The undo runs
/// its own probing preflight, so its first atomic writes (the probe's files) fail it
/// before anything is staged; the record's, after `swapped`, roll forward.
#[cfg(debug_assertions)]
#[test]
fn io_failures_in_an_undo_of_a_restore_leave_wtf_whole() {
    use restore_support::{Op, crash_op};

    for (failpoint, most) in [
        ("fsx.clone_file.before", 2),
        ("fsx.append_durable.before", 8),
        ("fsx.rename_with.before", 8),
        ("fsx.write_atomic.after", 4),
    ] {
        let mut failed = Vec::new();
        for n in 1..=most {
            let done = crash_op(Op::UndoRestore, &format!("{failpoint}=error@{n}"), "", true);
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
}

/// The undo of a restore crashed after its exchange, the game started since: the next
/// command refuses (`game_running`, naming the journal) and moves nothing; once the game
/// exits, the next one rolls it back to the restored tree.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_undo_of_a_restore_waits_while_the_game_runs() {
    let mut r = restore_support::RestoreScene::new(true);
    assert_eq!(r.restore("", &[]).unwrap()["ok"], true);
    r.play_on_restored();
    let restored = r.s.wtf();
    assert!(r.undo("engine.restore.exchanged=abort", &[]).is_none());
    assert_eq!(r.s.wtf(), r.played, "the exchange ran");
    r.s.home.retail_running();
    let refused = r.s.run(&r.install_bar(), "");
    let error = &refused.envelope()["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    assert_eq!(r.s.wtf(), r.played, "nothing moves while it runs");
    r.s.home.nothing_running();
    r.s.run(&r.install_bar(), "").expect(0);
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
    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = restore_support::crash(&format!("{failpoint}=abort"), "", true);
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
    for failpoint in [
        "engine.restore.staged",
        "engine.journal.prepared",
        "engine.restore.exchanged",
        "engine.restore.snapshotted",
        "engine.swapped",
        "engine.history.appended",
        "engine.committed",
    ] {
        let done = restore_support::crash(&format!("{failpoint}=abort"), "", false);
        assert!(done.crashed, "{failpoint}: {done:?}");
    }
}

/// A crash before each journaled rename: the exchange and the move to `WTF.previous`
/// (only the first onto a client with no `WTF/`).
#[cfg(debug_assertions)]
#[test]
fn a_crash_at_every_rename_of_a_restore() {
    for (live, renames) in [(true, 2), (false, 1)] {
        let mut crashed = Vec::new();
        for n in 1..=4 {
            let spec = format!("engine.swap.before_rename=abort@{n}");
            if !restore_support::crash(&spec, "", live).crashed {
                break;
            }
            crashed.push(n);
        }
        assert_eq!(crashed, (1..=renames).collect::<Vec<_>>(), "live {live}");
    }
}

/// Recovery of a restore crashed too: after each rename its rollback undoes, and as a
/// roll-forward starts. The command after it finishes.
#[cfg(debug_assertions)]
#[test]
fn a_crash_during_recovery_of_a_restore() {
    let mut crashed = Vec::new();
    for k in 1..=4 {
        let recover = format!("engine.recover.step=abort@{k}");
        if !restore_support::crash("engine.restore.snapshotted=abort", &recover, true)
            .recovering_crashed
        {
            break;
        }
        crashed.push(k);
    }
    assert_eq!(
        crashed,
        [1, 2],
        "both renames rolled back, each crashed after"
    );
    let done = restore_support::crash("engine.swapped=abort", "engine.recover.forward=abort", true);
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
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
            let done = restore_support::crash(&format!("{failpoint}=error@{n}"), "", true);
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
            let crashed = restore_support::crash(&format!("{failpoint}=abort@1"), "", true);
            assert!(crashed.crashed, "{failpoint} is on a restore's path");
        }
    }
}

/// A restore crashed after its exchange, with the game started since: the next command
/// on that client refuses (`game_running`, naming the journal) and moves nothing; once
/// the game exits, the next one rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_restore_waits_while_the_game_runs_then_rolls_back() {
    let mut r = restore_support::RestoreScene::new(true);
    assert!(r.restore("engine.restore.exchanged=abort", &[]).is_none());
    assert_eq!(r.s.wtf(), r.backed_up, "the exchange ran");
    r.s.home.retail_running();
    let refused = r.s.run(&r.install_bar(), "");
    let envelope = refused.envelope();
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    assert!(error["details"]["journal_pending"].is_string(), "{error}");
    assert_eq!(r.s.wtf(), r.backed_up, "nothing moves while it runs");
    assert_eq!(support::pending(&r.s), 1);

    r.s.home.nothing_running();
    let envelope = r.s.run(&r.install_bar(), "").expect(0);
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
    let envelope = r.s.run(&r.install_bar(), "").expect(0);
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

    let run = r.s.run(&r.install_bar(), "");
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

    let envelope = r.s.run(&r.install_bar(), "").expect(0);
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

    let envelope = r.s.run(&r.install_bar(), "").expect(0);
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
    for point in [
        "engine.preserve.intent",
        "engine.preserve.moved",
        "engine.preserve.published",
    ] {
        for action in ["abort", "error"] {
            let done = restore_support::preserve_crash(&format!("{point}={action}"));
            assert!(done.stopped && done.intent, "{point}={action}: {done:?}");
        }
    }
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
    for name in RECOVERY_WRITES {
        let mut crashed = Vec::new();
        for n in 1..=12 {
            let done = restore_support::preserve_crash(&format!("{name}=abort@{n}"));
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
}

/// The same writes failing (an I/O error, a full disk) at every hit in turn: recovery
/// stops with every version in place, and the next one, the fault gone, finishes.
#[cfg(debug_assertions)]
#[test]
fn a_preservation_failing_at_each_write_and_rename_keeps_everything() {
    for name in RECOVERY_WRITES {
        let mut failed = Vec::new();
        for n in 1..=10 {
            let done = restore_support::preserve_crash(&format!("{name}=error@{n}"));
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

    let envelope = r.s.run(&r.install_bar(), "").expect(0);
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

    for _ in 0..2 {
        let stopped = r.s.run(&r.install_bar(), "");
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
        if !restore_support::can_bind_mount(&dir) {
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
    pub fn crash(op: Op, engine: &str, recover: &str) -> Restored {
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
            } else {
                recovered_by = Some(first.expect(0));
            }
        }
        let bar = recovered_by.unwrap_or_else(|| s.run(&install_bar, "").expect(0));
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
            // The undo data: the pre-restore manifest, and the files as they were.
            let tx = s.history_txs()[old_history.len()].clone();
            let snapshot = a.r.client().join("Interface/.omaddons/snapshots").join(&tx);
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
    pub fn preserve_crash(engine: &str) -> Preserving {
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

        let bar = s.run(&a.r.install_bar(), "");
        let envelope = bar.expect(0);
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
        }
    }
}

/// Every failpoint as a crash of a partial restore. Its renames having all run never
/// selects roll-forward on their own: only `swapped` does.
#[cfg(debug_assertions)]
#[test]
fn crash_matrix_restore_addon() {
    use addon_support::{Op, crash};

    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        let done = crash(Op::RestoreAddon, &format!("{failpoint}=abort"), "");
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

    let mut crashed_at = Vec::new();
    for failpoint in FAILPOINTS {
        if crash(Op::UndoRestoreAddon, &format!("{failpoint}=abort"), "").crashed {
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

    let mut renames = Vec::new();
    for n in 1..=6 {
        let spec = format!("engine.swap.before_rename=abort@{n}");
        if !crash(Op::RestoreAddon, &spec, "").crashed {
            break;
        }
        renames.push(n);
    }
    assert_eq!(renames, [1, 2, 3, 4], "three files and the stage");
    let mut recoveries = Vec::new();
    for k in 1..=6 {
        let recover = format!("engine.recover.step=abort@{k}");
        let done = crash(
            Op::RestoreAddon,
            "engine.restore.snapshotted=abort",
            &recover,
        );
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
        Op::RestoreAddon,
        "engine.swapped=abort",
        "engine.recover.forward=abort",
    );
    assert!(done.recovering_crashed && done.went_new, "{done:?}");
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
            let done = crash(Op::RestoreAddon, &format!("{failpoint}=error@{n}"), "");
            if !done.ok {
                failed.push((n, done.pending, done.went_new));
            }
        }
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("partial restore: {failpoint} failed it at {failed:?}\n"),
        );
        let on_path = !failed.is_empty()
            || crash(Op::RestoreAddon, &format!("{failpoint}=abort@1"), "").crashed;
        assert!(on_path, "{failpoint} is on a partial restore's path");
    }
}

/// A partial restore crashed after its renames, the game started since: the next command
/// refuses (`game_running`, naming the journal) and moves nothing; once the game exits,
/// the next one rolls it back.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_partial_restore_waits_while_the_game_runs_then_rolls_back() {
    let mut a = addon_support::AddonScene::new();
    let outcome = a.restore("engine.restore.exchanged=abort", &[]);
    assert!(outcome.is_none(), "{outcome:?}");
    assert_eq!(a.foo(), a.backed_up, "the renames ran");
    a.r.s.home.retail_running();
    let refused = a.r.s.run(&a.r.install_bar(), "");
    let envelope = refused.envelope();
    let error = &envelope["error"];
    assert_eq!(
        (&error["code"], &error["details"]["reason"]),
        (&json!("game_running"), &json!("saved_variables")),
        "{}",
        refused.context()
    );
    assert!(error["details"]["journal_pending"].is_string(), "{error}");
    assert_eq!(a.foo(), a.backed_up, "nothing moves while it runs");
    a.r.s.home.nothing_running();
    let envelope = a.r.s.run(&a.r.install_bar(), "").expect(0);
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
    let envelope = a.r.s.run(&a.r.install_bar(), "").expect(0);
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
    let done = addon_support::preserve_crash("");
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
    for point in [
        "engine.preserve.intent",
        "engine.preserve.moved",
        "engine.preserve.published",
    ] {
        for action in ["abort", "error"] {
            let done = addon_support::preserve_crash(&format!("{point}={action}"));
            assert!(done.stopped && done.intent, "{point}={action}: {done:?}");
        }
    }
    for name in RECOVERY_WRITES {
        for action in ["abort", "error"] {
            let mut stopped = Vec::new();
            for n in 1..=10 {
                let done = addon_support::preserve_crash(&format!("{name}={action}@{n}"));
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
}
