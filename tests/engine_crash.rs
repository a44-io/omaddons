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
