//! E14's design gate (omad-m4-agent-surface-doctor-c6cm.5.14.5): recovery in barrier
//! order, crashed at every boundary. A real folder journal F (an update killed mid-swap,
//! or after `swapped`) is set aside by a real repair and reactivated by another, each
//! committed in a child of this test binary (`child_repair`) and killed at every primitive
//! boundary and every repair failpoint; recovery is run by three lock-taking commands,
//! crashed and repeated, and failed on I/O; an unreadable repair and a repair waiting
//! behind an unresolved folder journal complete the matrix. After each, every state is
//! wholly before or wholly after the repair, never a mixture, and F is never recovered
//! ahead of the repair that exposed it.
//!
//! A suite of its own, apart from `tests/engine_crash.rs`, so its matrices run in
//! parallel with the engine's.
//!
//! Debug builds only: failpoints exist only there (DEC-7).
//!
//! e2e suite: the activation barrier's crash matrix, with E11's differential: before each
//! recovering command, `journal::diagnose` predicts what it will do to each journal in
//! recovery's three passes, and the prediction is held to what it did.
#![cfg(debug_assertions)]

use crate::common;

// ---------------------------------------------------------------------------------------
// E14: recovery in barrier order, crashed at every boundary (the leaf's design gate,
// omad-m4-agent-surface-doctor-c6cm.5.14.5).

#[cfg(test)]
mod barrier_matrix {
    use std::collections::BTreeSet;

    use serde_json::Value;

    use super::common::barrier::{
        AFTER_A_COMMIT, BarrierScene, ERA, FolderCrash, PAIR_AFTER_A_COMMIT, PairScene, RETAIL,
        Recovering, SuspendOrder, Trees, only,
    };
    use super::common::differential::{Caller, Differential};
    use super::common::repair::point_of;
    use super::common::repairs::{self, Child};
    use super::common::tlog::{TestLog, tlog};

    /// The most `fsx.mutate.checked` hits a sweep tries before it calls the repair
    /// unbounded (as `common::repair`'s sweep).
    const CHECKED_SWEEP_MAX: usize = 64;

    /// Every addressed crash spec of a repair of `n` primitives: the doctor's abort points
    /// and the tracked fsx boundaries of each primitive (`checked` is swept instead: it is
    /// hit by untracked writes too).
    fn specs(n: usize) -> Vec<String> {
        let mut specs = vec!["doctor.repair.prepared=abort".to_owned()];
        specs.extend((1..=n).map(|k| format!("doctor.repair.step=abort@{k}")));
        for boundary in ["backed_up", "executed", "logged"] {
            specs.extend((1..=n).map(|k| format!("fsx.mutate.{boundary}=abort@{k}")));
        }
        specs.extend(
            [
                "doctor.repair.done=abort",
                "doctor.repair.history=abort",
                "doctor.repair.committed=abort",
            ]
            .map(str::to_owned),
        );
        specs
    }

    /// Every crash point a repair of `n` primitives has.
    fn points(n: usize) -> BTreeSet<String> {
        let mut points: BTreeSet<String> = ["prepared", "done", "history", "committed"]
            .map(str::to_owned)
            .into();
        for k in 1..=n {
            for boundary in ["step", "checked", "backed_up", "executed", "logged"] {
                points.insert(format!("{boundary}@{k}"));
            }
        }
        points
    }

    /// The reactivation's key boundaries: after every artifact is back, right before the
    /// activation runs, and between the activation and `done`.
    fn key_specs(n: usize) -> Vec<String> {
        vec![
            format!("fsx.mutate.logged=abort@{}", n - 1),
            format!("doctor.repair.step=abort@{n}"),
            format!("fsx.mutate.logged=abort@{n}"),
            "doctor.repair.done=abort".to_owned(),
        ]
    }

    /// A reader changes nothing it compares, and reports a pending repair.
    fn status_changes_nothing(log: &TestLog, scene: &BarrierScene, pending: bool) {
        log.step("status changes nothing");
        let trees = scene.trees();
        let ran = log.run(&scene.home, &["status", "--json", "--progress", "json"]);
        ran.assert_private();
        assert_eq!(scene.trees(), trees, "status wrote nothing a case compares");
        if pending {
            assert!(
                ran.warnings().iter().any(|code| code == "journal_pending"),
                "status reports the pending repair: {:?}",
                ran.warnings()
            );
        }
    }

    /// The recovering command `by`, between the differential's prediction and its
    /// comparison; its envelope. `exit` is the code it must end with.
    fn recover(log: &TestLog, scene: &BarrierScene, by: Recovering, exit: i32) -> Value {
        recover_armed(log, scene, by, exit, "")
    }

    fn recover_armed(
        log: &TestLog,
        scene: &BarrierScene,
        by: Recovering,
        exit: i32,
        failpoints: &str,
    ) -> Value {
        log.step(&format!("recover with {} {failpoints}", by.label()));
        let mut differential = Differential::new(log, &scene.home, &[RETAIL, ERA], &[]);
        differential.predict(log, by.label());
        let ran = log
            .cmd(&scene.home, &by.argv(scene))
            .failpoints(failpoints)
            .run();
        ran.assert_private();
        ran.exit(exit);
        let envelope = ran.envelope().clone();
        // E11's rule: an injected failure is not predicted.
        if failpoints.is_empty() {
            let caller = match by {
                Recovering::ConfigSet => Caller::Under,
                other => differential.commit(other.own()),
            };
            differential.compare(log, &envelope, &repairs::journals(&scene.home), &caller);
        }
        envelope
    }

    /// The messages of the envelope's warnings of `code` that name `tx`.
    fn about(envelope: &Value, code: &str, tx: &str) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|w| w["code"] == code)
            .filter_map(|w| w["message"].as_str())
            .filter(|message| message.contains(tx))
            .map(str::to_owned)
            .collect()
    }

    /// The index of the first warning whose message names `tx` as its subject.
    fn position(envelope: &Value, tx: &str) -> Option<usize> {
        envelope["warnings"].as_array()?.iter().position(|w| {
            w["message"].as_str().is_some_and(|m| {
                m.starts_with(&format!("transaction {tx}"))
                    || m.starts_with(&format!("repair transaction {tx}"))
            })
        })
    }

    /// Whole before: the trees as they were before the repair that crashed (all of them
    /// under `config set`, which commits nothing; under a command that commits, the ones
    /// it does not touch), with no journal of F, no repair journal and no history line
    /// of `repair`.
    fn assert_before(
        log: &TestLog,
        scene: &BarrierScene,
        before: &Trees,
        by: Recovering,
        repair: Option<&str>,
        context: &str,
    ) {
        log.step("assert whole before");
        let now = scene.trees();
        let mut keys: Vec<&str> = match by {
            // `config set` records a line of its own in history.
            Recovering::ConfigSet => before.keys().copied().filter(|k| *k != "history").collect(),
            _ => AFTER_A_COMMIT.to_vec(),
        };
        // A repair killed before its `prepared` line leaves its in-flight lock alone, which
        // the orphan sweep takes once it is old: that lock, and nothing else, is new.
        let inflight = scene.home.env.state().join("omaddons/inflight");
        if let Some(repair) = repair.filter(|r| inflight.join(format!("{r}.lock")).exists()) {
            keys.retain(|k| *k != "state.inflight");
            let mut left: Vec<String> = std::fs::read_dir(&inflight)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            assert_eq!(
                left,
                [format!("{repair}.lock")],
                "{context}: only its own lock is new"
            );
            log.note(format!(
                "{context}: {repair} never prepared; its lock is the sweep's"
            ));
        }
        if only(&now, &keys) != only(before, &keys) {
            log.json("before", before);
            log.json("now", &now);
        }
        assert_eq!(
            only(&now, &keys),
            only(before, &keys),
            "{context}: whole before, never a mixture"
        );
        assert!(scene.f_absent(), "{context}: F stays set aside");
        assert!(
            scene.repair_lines().is_empty(),
            "{context}: no repair journal left"
        );
        if let Some(repair) = repair {
            assert!(
                !scene.history().iter().any(|tx| tx == repair),
                "{context}: no history line of {repair}"
            );
        }
    }

    /// Whole after: `repair`'s history line once, F recovered by the same command
    /// (`journal_recovered` naming it, after any warning naming `repair`), F's journal, lock
    /// and stage gone, and retail's `foo` as F's own recovery leaves it.
    fn assert_after(
        log: &TestLog,
        scene: &BarrierScene,
        envelope: &Value,
        repair: &str,
        context: &str,
    ) {
        log.step("assert whole after");
        let history = scene.history();
        assert_eq!(
            history.iter().filter(|tx| *tx == repair).count(),
            1,
            "{context}: the repair's history line, once"
        );
        let recovered = about(envelope, "journal_recovered", &scene.f);
        assert_eq!(
            recovered.len(),
            1,
            "{context}: F recovered once: {envelope}"
        );
        let f_at = position(envelope, &scene.f);
        if let Some(repair_at) = position(envelope, repair) {
            assert!(
                f_at > Some(repair_at),
                "{context}: F never ahead of its repair: {envelope}"
            );
        }
        assert!(scene.f_absent(), "{context}: F's journal pruned");
        assert!(
            scene.repair_lines().is_empty(),
            "{context}: no repair journal left"
        );
        let state = scene.home.env.state().join("omaddons");
        assert!(
            !state.join(format!("inflight/{}.lock", scene.f)).exists(),
            "{context}: F's lock pruned"
        );
        assert!(
            !scene
                .retail
                .join(format!("Interface/.omaddons/stage/{}", scene.f))
                .exists(),
            "{context}: F's stage pruned"
        );
        let addons = scene.home.addons(RETAIL);
        match scene.crash {
            FolderCrash::MidSwap => {
                assert_eq!(
                    scene.foo_now(),
                    scene.v1_foo,
                    "{context}: F rolled back to v1"
                );
                assert!(recovered[0].contains("rolled back"), "{context}");
            }
            FolderCrash::Swapped => {
                let toc = std::fs::read_to_string(addons.join("Foo/Foo.toc")).unwrap();
                assert!(
                    toc.contains("Version: 2"),
                    "{context}: F rolled forward to v2"
                );
                assert!(!addons.join("FooOld").exists(), "{context}");
                assert!(recovered[0].contains("rolled forward"), "{context}");
                assert_eq!(
                    history.iter().filter(|tx| **tx == scene.f).count(),
                    1,
                    "{context}: F's history line once"
                );
                let f = history.iter().position(|tx| *tx == scene.f);
                let r = history.iter().position(|tx| tx == repair);
                assert!(f > r, "{context}: F's line after its repair's");
            }
        }
    }

    /// One reactivation case: a fresh scene with F set aside, the reactivation crashed at
    /// `spec`, a reader, the recovering command `by`, and the whole-state oracle. Whether
    /// the child crashed, and the point that fired.
    fn reactivation_case(
        log: &TestLog,
        crash: FolderCrash,
        spec: &str,
        by: Recovering,
    ) -> (bool, Option<String>) {
        let scene = BarrierScene::new(log, crash);
        scene.set_aside_whole(log);
        let before = scene.trees();
        let child = scene.reactivate(log, spec);
        let crashed = matches!(child, Child::Crashed);
        let repair = match &child {
            Child::Ended(outcome) => outcome["tx"].as_str().map(str::to_owned),
            Child::Crashed => scene.repair_tx(),
        };
        let done = repair.as_deref().is_some_and(|tx| scene.done(tx));
        let fired = match (crashed, spec.starts_with("fsx.mutate.checked")) {
            (false, _) => None,
            (true, true) => scene.checked_point(),
            (true, false) => point_of(spec),
        };
        log.note(format!(
            "{spec}: crashed {crashed}, done {done}, fired {fired:?}, repair {repair:?}"
        ));
        log.json("repair journal", &scene.repair_lines());
        status_changes_nothing(log, &scene, scene.repair_pending());
        let envelope = recover(log, &scene, by, 0);
        let context = format!("{} at {spec} ({crash:?})", by.label());
        if done {
            assert_after(log, &scene, &envelope, repair.as_deref().unwrap(), &context);
        } else {
            assert_before(log, &scene, &before, by, repair.as_deref(), &context);
            assert!(
                about(&envelope, "journal_recovered", &scene.f).is_empty(),
                "{context}: F is never recovered while its repair rolls back"
            );
        }
        (crashed, fired)
    }

    /// E7's reactivation case (omad-m4-agent-surface-doctor-c6cm.5.7.5): [`reactivation_case`]
    /// with the reactivation the real undo of the set-aside, `omaddons undo <set-aside>`
    /// through the binary, crashed at `spec`. The same oracle: before the undo's `done`,
    /// the scene as the set-aside left it and F never recovered; from `done`, F recovered
    /// once by the folder pass, after the undo.
    fn undo_reactivation_case(
        log: &TestLog,
        crash: FolderCrash,
        spec: &str,
        by: Recovering,
    ) -> (bool, Option<String>) {
        let scene = BarrierScene::new(log, crash);
        scene.set_aside_whole(log);
        let before = scene.trees();
        let ran = scene.undo_set_aside(log, spec);
        ran.assert_private();
        let crashed = ran.run.code < 0;
        let repair = if crashed {
            scene.repair_tx()
        } else {
            ran.ok();
            ran.envelope()["data"]["tx"].as_str().map(str::to_owned)
        };
        let done = repair.as_deref().is_some_and(|tx| scene.done(tx));
        let fired = match (crashed, spec.starts_with("fsx.mutate.checked")) {
            (false, _) => None,
            (true, true) => scene.checked_point(),
            (true, false) => super::common::repair::undo_point_of(spec),
        };
        log.note(format!(
            "{spec}: crashed {crashed}, done {done}, fired {fired:?}, undo {repair:?}"
        ));
        log.json("undo journal", &scene.repair_lines());
        status_changes_nothing(log, &scene, scene.repair_pending());
        let envelope = recover(log, &scene, by, 0);
        let context = format!("{} at {spec} ({crash:?}), the undo for real", by.label());
        if done {
            assert_after(log, &scene, &envelope, repair.as_deref().unwrap(), &context);
        } else {
            assert_before(log, &scene, &before, by, repair.as_deref(), &context);
            assert!(
                about(&envelope, "journal_recovered", &scene.f).is_empty(),
                "{context}: F is never recovered while its undo rolls back"
            );
        }
        (crashed, fired)
    }

    /// E7's design gate, the reactivation through the real undo: crashed at every point on
    /// its path (the run directory's and the engine's before its journal, every repair
    /// point and tracked boundary of each inverse, every `checked` hit), recovered by
    /// `config set`; behind a journal that reached `swapped`, at the key boundaries.
    #[test]
    fn a_reactivation_crashed_anywhere_settles_before_the_folder_pass() {
        let log = tlog!();
        let n = {
            let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
            scene.set_aside_whole(&log);
            scene.reactivation_len()
        };
        let mut all = vec![
            "doctor.run.staged=abort".to_owned(),
            "doctor.run.published=abort".to_owned(),
            "engine.snapshot.prestaged=abort".to_owned(),
            "engine.lock.acquired=abort".to_owned(),
            "engine.revalidated=abort".to_owned(),
        ];
        all.extend(specs(n));
        let mut fired = BTreeSet::new();
        let mut cases = 0;
        for spec in all {
            let case = log.case(&spec);
            let (crashed, point) =
                undo_reactivation_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            assert!(crashed, "{spec} is on the undo's path");
            fired.extend(point);
            cases += 1;
        }
        for k in 1..=CHECKED_SWEEP_MAX {
            let spec = format!("fsx.mutate.checked=abort@{k}");
            let case = log.case(&spec);
            let (crashed, point) =
                undo_reactivation_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            fired.extend(point);
            cases += 1;
            if !crashed {
                break;
            }
        }
        for spec in key_specs(n) {
            let case = log.case(&format!("swapped {spec}"));
            undo_reactivation_case(&case, FolderCrash::Swapped, &spec, Recovering::ConfigSet);
            cases += 1;
        }
        let mut expected = points(n);
        expected.extend(
            [
                "run.staged",
                "run.published",
                "snapshot.prestaged",
                "lock.acquired",
                "revalidated",
            ]
            .map(str::to_owned),
        );
        log.json("crashed_at", &fired);
        log.note(format!(
            "{cases} cases, {} boundaries fired of {}",
            fired.len(),
            expected.len()
        ));
        let missing: Vec<String> = expected.difference(&fired).cloned().collect();
        assert!(
            missing.is_empty(),
            "boundaries that never fired: {missing:?}"
        );
    }

    // -----------------------------------------------------------------------------------
    // omad-i2c5: the undo of one set-aside of two journals, F on retail and G on era, an
    // activation each, both last; crashed anywhere, including between the activations.

    /// The pair's key boundaries: every artifact back (before the first activation),
    /// between the two activations, the second recorded and not run, run and not logged,
    /// both logged, and `done`.
    fn pair_key_specs(n: usize) -> Vec<String> {
        vec![
            format!("fsx.mutate.logged=abort@{}", n - 2),
            format!("fsx.mutate.logged=abort@{}", n - 1),
            format!("doctor.repair.step=abort@{n}"),
            format!("fsx.mutate.executed=abort@{n}"),
            format!("fsx.mutate.logged=abort@{n}"),
            "doctor.repair.done=abort".to_owned(),
        ]
    }

    /// [`status_changes_nothing`] for the pair: era's trees too.
    fn pair_status_changes_nothing(log: &TestLog, scene: &PairScene, pending: bool) {
        log.step("status changes nothing");
        let trees = scene.trees();
        let ran = log.run(
            &scene.base.home,
            &["status", "--json", "--progress", "json"],
        );
        ran.assert_private();
        assert_eq!(scene.trees(), trees, "status wrote nothing a case compares");
        if pending {
            assert!(
                ran.warnings().iter().any(|code| code == "journal_pending"),
                "status reports the pending undo: {:?}",
                ran.warnings()
            );
        }
    }

    /// Whole before, for the pair: every tree as the set-aside left it (under a command
    /// that commits, those it does not touch), F and G both set aside and neither
    /// recovered, no repair journal, no history line of `undo`.
    fn pair_before(
        log: &TestLog,
        scene: &PairScene,
        before: &Trees,
        by: Recovering,
        envelope: &Value,
        undo: Option<&str>,
        context: &str,
    ) {
        log.step("assert whole before");
        let now = scene.trees();
        let mut keys: Vec<&str> = match by {
            Recovering::ConfigSet => before.keys().copied().filter(|k| *k != "history").collect(),
            _ => PAIR_AFTER_A_COMMIT.to_vec(),
        };
        // An undo killed before its `prepared` line leaves its in-flight lock alone, which
        // the orphan sweep takes once it is old: that lock, and nothing else, is new.
        let inflight = scene.base.home.env.state().join("omaddons/inflight");
        if let Some(undo) = undo.filter(|u| inflight.join(format!("{u}.lock")).exists()) {
            keys.retain(|k| *k != "state.inflight");
            let left: Vec<String> = std::fs::read_dir(&inflight)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                left,
                [format!("{undo}.lock")],
                "{context}: only its own lock is new"
            );
        }
        if only(&now, &keys) != only(before, &keys) {
            log.json("before", before);
            log.json("now", &now);
        }
        assert_eq!(
            only(&now, &keys),
            only(before, &keys),
            "{context}: whole before, never a mixture"
        );
        assert!(scene.f_absent(), "{context}: F stays set aside");
        assert!(scene.g_absent(), "{context}: G stays set aside");
        assert!(
            scene.base.repair_lines().is_empty(),
            "{context}: no repair journal left"
        );
        if let Some(undo) = undo {
            assert!(
                !scene.base.history().iter().any(|tx| tx == undo),
                "{context}: no history line of {undo}"
            );
        }
        for tx in [&scene.base.f, &scene.g.tx] {
            assert!(
                about(envelope, "journal_recovered", tx).is_empty(),
                "{context}: {tx} is never recovered while the undo rolls back"
            );
        }
    }

    /// Whole after, for the pair: the undo's history line once, and each of F and G
    /// recovered once by the same command after any warning naming the undo, its journal,
    /// lock and stage gone, and its client's `foo` as its own recovery leaves it.
    fn pair_after(log: &TestLog, scene: &PairScene, envelope: &Value, undo: &str, context: &str) {
        log.step("assert whole after");
        let history = scene.base.history();
        assert_eq!(
            history.iter().filter(|tx| *tx == undo).count(),
            1,
            "{context}: the undo's history line, once"
        );
        let undo_at = position(envelope, undo);
        let state = scene.base.home.env.state().join("omaddons");
        let journals = [
            (
                "F",
                &scene.base.f,
                &scene.base.retail,
                RETAIL,
                &scene.base.v1_foo,
                scene.base.foo_now(),
            ),
            (
                "G",
                &scene.g.tx,
                &scene.g.dir,
                ERA,
                &scene.g.v1_foo,
                scene.g_foo_now(),
            ),
        ];
        for (name, tx, client, folder, v1_foo, foo) in journals {
            let recovered = about(envelope, "journal_recovered", tx);
            assert_eq!(
                recovered.len(),
                1,
                "{context}: {name} recovered once: {envelope}"
            );
            if let Some(undo_at) = undo_at {
                assert!(
                    position(envelope, tx) > Some(undo_at),
                    "{context}: {name} never ahead of the undo: {envelope}"
                );
            }
            assert!(
                !state.join(format!("inflight/{tx}.lock")).exists(),
                "{context}: {name}'s lock pruned"
            );
            assert!(
                !client
                    .join(format!("Interface/.omaddons/stage/{tx}"))
                    .exists(),
                "{context}: {name}'s stage pruned"
            );
            let addons = scene.base.home.addons(folder);
            match scene.base.crash {
                FolderCrash::MidSwap => {
                    assert_eq!(&foo, v1_foo, "{context}: {name} rolled back to v1");
                    assert!(recovered[0].contains("rolled back"), "{context}");
                }
                FolderCrash::Swapped => {
                    let toc = std::fs::read_to_string(addons.join("Foo/Foo.toc")).unwrap();
                    assert!(
                        toc.contains("Version: 2"),
                        "{context}: {name} rolled forward to v2"
                    );
                    assert!(!addons.join("FooOld").exists(), "{context}");
                    assert!(recovered[0].contains("rolled forward"), "{context}");
                    let lines: Vec<usize> = history
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| *line == tx)
                        .map(|(at, _)| at)
                        .collect();
                    assert_eq!(lines.len(), 1, "{context}: {name}'s history line once");
                    let u = history.iter().position(|line| line == undo);
                    assert!(
                        Some(lines[0]) > u,
                        "{context}: {name}'s line after the undo's"
                    );
                }
            }
        }
        assert!(
            scene.f_absent() && scene.g_absent(),
            "{context}: both journals pruned"
        );
        assert!(
            scene.base.repair_lines().is_empty(),
            "{context}: no repair journal left"
        );
    }

    /// One pair case: a fresh pair set aside, its undo through the binary crashed at
    /// `spec`, a reader, the recovering command `by`, and the pair's whole-state oracle.
    /// Whether the undo crashed, and the point that fired.
    fn pair_case(
        log: &TestLog,
        crash: FolderCrash,
        spec: &str,
        by: Recovering,
    ) -> (bool, Option<String>) {
        let scene = PairScene::new(log, crash);
        scene.set_aside_both(log);
        let before = scene.trees();
        let ran = scene.base.undo_set_aside(log, spec);
        ran.assert_private();
        let crashed = ran.run.code < 0;
        let undo = if crashed {
            scene.repair_tx()
        } else {
            ran.ok();
            ran.envelope()["data"]["tx"].as_str().map(str::to_owned)
        };
        let done = undo.as_deref().is_some_and(|tx| scene.base.done(tx));
        let fired = match (crashed, spec.starts_with("fsx.mutate.checked")) {
            (false, _) => None,
            (true, true) => scene.base.checked_point(),
            (true, false) => super::common::repair::undo_point_of(spec),
        };
        log.note(format!(
            "{spec}: crashed {crashed}, done {done}, fired {fired:?}, undo {undo:?}"
        ));
        log.json("undo journal", &scene.base.repair_lines());
        pair_status_changes_nothing(log, &scene, scene.base.repair_pending());
        let envelope = recover(log, &scene.base, by, 0);
        let context = format!("{} at {spec} ({crash:?}), two journals", by.label());
        if done {
            pair_after(log, &scene, &envelope, undo.as_deref().unwrap(), &context);
        } else {
            pair_before(
                log,
                &scene,
                &before,
                by,
                &envelope,
                undo.as_deref(),
                &context,
            );
        }
        (crashed, fired)
    }

    /// The undo of the pair's set-aside, run whole: both provisional restores ahead of
    /// every other inverse, the two activations last, one per journal, and both journals
    /// back under their own names for the next lock-taking command, which recovers each.
    #[test]
    fn two_journals_set_aside_together_come_back_with_an_activation_each() {
        let log = tlog!();
        let scene = PairScene::new(&log, FolderCrash::MidSwap);
        scene.set_aside_both(&log);
        let n = scene.undo_len();
        let ran = scene.base.undo_set_aside(&log, "");
        ran.assert_private();
        let data = ran.ok().data().clone();
        log.json("repairs", &data["repairs"]);
        let repairs = data["repairs"].as_array().unwrap();
        assert_eq!(repairs.len(), n, "{data}");
        let activations: Vec<usize> = repairs
            .iter()
            .enumerate()
            .filter(|(_, op)| op["activation"] == true)
            .map(|(at, _)| at + 1)
            .collect();
        assert_eq!(activations, [n - 1, n], "the activations are last: {data}");
        let restores: Vec<usize> = repairs
            .iter()
            .enumerate()
            .filter(|(_, op)| {
                op["path"].as_str().is_some_and(|p| p.ends_with(".jsonl"))
                    && op["activation"] != true
            })
            .map(|(at, _)| at + 1)
            .collect();
        assert_eq!(restores, [1, 2], "both provisional restores first: {data}");
        assert!(
            scene.base.journal().exists(),
            "F is back under its own name"
        );
        assert!(scene.g_journal().exists(), "G is back under its own name");
        let envelope = recover(&log, &scene.base, Recovering::ConfigSet, 0);
        pair_after(
            &log,
            &scene,
            &envelope,
            data["tx"].as_str().unwrap(),
            "the undo run whole",
        );
    }

    /// omad-i2c5's design gate: the undo of one set-aside of two journals, through the
    /// binary, crashed at every point on its path (the run directory's and the engine's
    /// before its journal, every repair point and tracked boundary of each inverse, every
    /// `checked` hit), between the two activations included, recovered by `config set`;
    /// at the key boundaries also by an install on either journal's client; behind two
    /// journals that reached `swapped`, at the key boundaries. Neither journal is ever
    /// recovered ahead of the undo, and every state is wholly before or wholly after.
    #[test]
    fn two_journals_reactivated_crashed_anywhere_settle_before_the_folder_pass() {
        let log = tlog!();
        let n = {
            let scene = PairScene::new(&log, FolderCrash::MidSwap);
            scene.set_aside_both(&log);
            scene.undo_len()
        };
        let mut all = vec![
            "doctor.run.staged=abort".to_owned(),
            "doctor.run.published=abort".to_owned(),
            "engine.snapshot.prestaged=abort".to_owned(),
            "engine.lock.acquired=abort".to_owned(),
            "engine.revalidated=abort".to_owned(),
        ];
        all.extend(specs(n));
        let mut fired = BTreeSet::new();
        let mut cases = 0;
        for spec in all {
            let case = log.case(&spec);
            let (crashed, point) =
                pair_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            assert!(crashed, "{spec} is on the undo's path");
            fired.extend(point);
            cases += 1;
        }
        for k in 1..=CHECKED_SWEEP_MAX {
            let spec = format!("fsx.mutate.checked=abort@{k}");
            let case = log.case(&spec);
            let (crashed, point) =
                pair_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            fired.extend(point);
            cases += 1;
            if !crashed {
                break;
            }
        }
        for by in [Recovering::InstallOwn, Recovering::InstallOther] {
            for spec in pair_key_specs(n) {
                let case = log.case(&format!("{} {spec}", by.label()));
                pair_case(&case, FolderCrash::MidSwap, &spec, by);
                cases += 1;
            }
        }
        for spec in pair_key_specs(n) {
            let case = log.case(&format!("swapped {spec}"));
            pair_case(&case, FolderCrash::Swapped, &spec, Recovering::ConfigSet);
            cases += 1;
        }
        let mut expected = points(n);
        expected.extend(
            [
                "run.staged",
                "run.published",
                "snapshot.prestaged",
                "lock.acquired",
                "revalidated",
            ]
            .map(str::to_owned),
        );
        log.json("crashed_at", &fired);
        log.note(format!(
            "{cases} cases, {} boundaries fired of {}",
            fired.len(),
            expected.len()
        ));
        let missing: Vec<String> = expected.difference(&fired).cloned().collect();
        assert!(
            missing.is_empty(),
            "boundaries that never fired: {missing:?}"
        );
    }

    /// The pair's undo killed between its two activations, and after both, then its
    /// recovery crashed at every inverse and repeated three times more: every state is
    /// whole before, and neither journal is recovered. The rollback hides the journal
    /// activated last first (it inverts newest first).
    #[test]
    fn two_journal_recovery_crashed_at_every_inverse_then_repeated() {
        let log = tlog!();
        let n = {
            let scene = PairScene::new(&log, FolderCrash::MidSwap);
            scene.set_aside_both(&log);
            scene.undo_len()
        };
        let mut crashed_inverses = 0;
        for at in [n - 1, n] {
            let spec = format!("fsx.mutate.logged=abort@{at}");
            for j in 1..=n + 1 {
                let case = log.case(&format!("{spec} then recover.step@{j}"));
                let scene = PairScene::new(&case, FolderCrash::MidSwap);
                scene.set_aside_both(&case);
                let before = scene.trees();
                let ran = scene.base.undo_set_aside(&case, &spec);
                assert!(ran.run.code < 0, "{spec} kills the undo");
                let undo = scene.repair_tx();
                let ran = case
                    .cmd(&scene.base.home, &Recovering::ConfigSet.argv(&scene.base))
                    .failpoints(&format!("doctor.recover.step=abort@{j}"))
                    .run();
                if ran.signal.is_none() {
                    // Past the last inverse: this recovery finished on its own.
                    let envelope = ran.envelope().clone();
                    pair_before(
                        &case,
                        &scene,
                        &before,
                        Recovering::ConfigSet,
                        &envelope,
                        undo.as_deref(),
                        &spec,
                    );
                    break;
                }
                crashed_inverses += 1;
                for round in 0..4 {
                    let envelope = recover(&case, &scene.base, Recovering::ConfigSet, 0);
                    let context = format!("{spec}, recovery crashed at inverse {j}, round {round}");
                    pair_before(
                        &case,
                        &scene,
                        &before,
                        Recovering::ConfigSet,
                        &envelope,
                        undo.as_deref(),
                        &context,
                    );
                    if round > 0 {
                        assert!(
                            envelope["warnings"].as_array().is_none_or(|w| w.is_empty()),
                            "{context}: nothing left to say: {envelope}"
                        );
                    }
                }
            }
        }
        log.note(format!("{crashed_inverses} recoveries crashed"));
        assert!(crashed_inverses >= 2 * (n - 1), "every inverse crashed");
    }

    #[test]
    fn the_scene_builds_a_valid_reactivation() {
        let log = tlog!();
        let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
        let artifacts = scene.artifacts(SuspendOrder::JournalLast).len();
        scene.set_aside_whole(&log);
        assert_eq!(scene.reactivation_len(), artifacts + 1);
        log.note(format!(
            "{artifacts} artifacts, N = {}",
            scene.reactivation_len()
        ));
        let child = scene.reactivate(&log, "");
        assert!(
            matches!(&child, Child::Ended(outcome) if outcome["ok"] == true),
            "the reactivation passes barrier::validate and commits: {child:?}"
        );
        assert!(scene.journal().exists(), "F is back under its own name");
        assert!(!scene.provisional().exists());
    }

    /// Item 2: the reactivation crashed at every boundary, recovered by `config set`; at
    /// the key boundaries also by an install on F's own client and on the other one.
    #[test]
    fn barrier_reactivation_crashed_at_every_boundary() {
        let log = tlog!();
        let n = {
            let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
            scene.set_aside_whole(&log);
            scene.reactivation_len()
        };
        let mut fired = BTreeSet::new();
        let mut cases = 0;
        for spec in specs(n) {
            let case = log.case(&spec);
            let (_, point) =
                reactivation_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            fired.extend(point);
            cases += 1;
        }
        for k in 1..=CHECKED_SWEEP_MAX {
            let spec = format!("fsx.mutate.checked=abort@{k}");
            let case = log.case(&spec);
            let (crashed, point) =
                reactivation_case(&case, FolderCrash::MidSwap, &spec, Recovering::ConfigSet);
            fired.extend(point);
            cases += 1;
            if !crashed {
                break;
            }
        }
        for by in [Recovering::InstallOwn, Recovering::InstallOther] {
            for spec in key_specs(n) {
                let case = log.case(&format!("{} {spec}", by.label()));
                reactivation_case(&case, FolderCrash::MidSwap, &spec, by);
                cases += 1;
            }
        }
        log.json("crashed_at", &fired);
        log.note(format!(
            "{cases} cases, {} boundaries fired of {}",
            fired.len(),
            points(n).len()
        ));
        let missing: Vec<String> = points(n).difference(&fired).cloned().collect();
        assert!(
            missing.is_empty(),
            "boundaries that never fired: {missing:?}"
        );
    }

    /// Item 3: behind a journal that reached `swapped`, F rolls forward once its repair
    /// settles, its history line once and after its repair's.
    #[test]
    fn barrier_reactivation_behind_a_swapped_journal() {
        let log = tlog!();
        let n = {
            let scene = BarrierScene::new(&log, FolderCrash::Swapped);
            scene.set_aside_whole(&log);
            scene.reactivation_len()
        };
        let mut specs = key_specs(n);
        specs.extend(
            [
                "doctor.repair.history=abort",
                "doctor.repair.committed=abort",
            ]
            .map(str::to_owned),
        );
        for spec in specs {
            let case = log.case(&spec);
            reactivation_case(&case, FolderCrash::Swapped, &spec, Recovering::ConfigSet);
        }
    }

    /// One suspension case: F as the crash left it, set aside with `order` and crashed at
    /// `spec`, then `config set`. Without `done` the set-aside rolls back and F is
    /// recovered in the same command; from `done` on, F stays set aside. The point fired.
    fn suspension_case(log: &TestLog, order: SuspendOrder, spec: &str) -> (bool, Option<String>) {
        let scene = BarrierScene::new(log, FolderCrash::MidSwap);
        let before = scene.trees();
        let child = scene.set_aside(log, order, spec);
        let crashed = matches!(child, Child::Crashed);
        let set_aside = scene.set_aside_tx();
        let done = scene.done(&set_aside);
        let fired = match (crashed, spec.starts_with("fsx.mutate.checked")) {
            (false, _) => None,
            (true, true) => scene.checked_point(),
            (true, false) => point_of(spec),
        };
        log.note(format!(
            "{spec}: crashed {crashed}, done {done}, fired {fired:?}"
        ));
        status_changes_nothing(log, &scene, scene.repair_pending());
        let envelope = recover(log, &scene, Recovering::ConfigSet, 0);
        let context = format!("set-aside ({order:?}) at {spec}");
        let history = scene.history();
        if done {
            assert_eq!(
                history.iter().filter(|tx| **tx == set_aside).count(),
                1,
                "{context}: rolled forward, its line once"
            );
            assert!(scene.f_absent(), "{context}: F stays set aside");
            assert!(
                about(&envelope, "journal_recovered", &scene.f).is_empty(),
                "{context}: F is not recovered"
            );
            assert_eq!(
                scene.trees()["addons"],
                before["addons"],
                "{context}: retail's AddOns as F's crash left them"
            );
        } else {
            assert!(!history.contains(&set_aside), "{context}: rolled back");
            let recovered = about(&envelope, "journal_recovered", &scene.f);
            assert_eq!(
                recovered.len(),
                1,
                "{context}: F recovered in the same command: {envelope}"
            );
            if let Some(at) = position(&envelope, &set_aside) {
                assert!(
                    position(&envelope, &scene.f) > Some(at),
                    "{context}: F after its set-aside"
                );
            }
            assert_eq!(
                scene.foo_now(),
                scene.v1_foo,
                "{context}: F rolled back to v1"
            );
            assert!(scene.f_absent(), "{context}: F's journal pruned");
        }
        assert!(
            scene.repair_lines().is_empty(),
            "{context}: no repair journal left"
        );
        (crashed, fired)
    }

    /// Item 4: the set-aside crashed at every boundary, journal first and journal last.
    #[test]
    fn barrier_suspension_crashed_at_every_boundary() {
        let log = tlog!();
        for order in [SuspendOrder::JournalLast, SuspendOrder::JournalFirst] {
            let n = BarrierScene::new(&log, FolderCrash::MidSwap)
                .artifacts(order)
                .len();
            let mut fired = BTreeSet::new();
            let mut cases = 0;
            for spec in specs(n) {
                let case = log.case(&format!("{order:?} {spec}"));
                fired.extend(suspension_case(&case, order, &spec).1);
                cases += 1;
            }
            for k in 1..=CHECKED_SWEEP_MAX {
                let spec = format!("fsx.mutate.checked=abort@{k}");
                let case = log.case(&format!("{order:?} {spec}"));
                let (crashed, point) = suspension_case(&case, order, &spec);
                fired.extend(point);
                cases += 1;
                if !crashed {
                    break;
                }
            }
            log.json(&format!("crashed_at {order:?}"), &fired);
            log.note(format!(
                "{order:?}: {cases} cases, {} boundaries fired",
                fired.len()
            ));
            let missing: Vec<String> = points(n).difference(&fired).cloned().collect();
            assert!(missing.is_empty(), "{order:?}: never fired: {missing:?}");
        }
    }

    /// Item 5: the reactivation's rollback crashed at every inverse, then recovered, then
    /// recovered three more times: whole before every time.
    #[test]
    fn barrier_recovery_crashed_at_every_inverse_then_repeated() {
        let log = tlog!();
        let n = {
            let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
            scene.set_aside_whole(&log);
            scene.reactivation_len()
        };
        let mut crashed_inverses = 0;
        for at in [n - 1, n] {
            let spec = format!("fsx.mutate.logged=abort@{at}");
            for j in 1..=n + 1 {
                let case = log.case(&format!("{spec} then recover.step@{j}"));
                let scene = BarrierScene::new(&case, FolderCrash::MidSwap);
                scene.set_aside_whole(&case);
                let before = scene.trees();
                assert!(matches!(scene.reactivate(&case, &spec), Child::Crashed));
                let repair = scene.repair_tx();
                let ran = case
                    .cmd(&scene.home, &Recovering::ConfigSet.argv(&scene))
                    .failpoints(&format!("doctor.recover.step=abort@{j}"))
                    .run();
                if ran.signal.is_none() {
                    // Past the last inverse: this recovery finished on its own.
                    assert_before(
                        &case,
                        &scene,
                        &before,
                        Recovering::ConfigSet,
                        repair.as_deref(),
                        &spec,
                    );
                    break;
                }
                crashed_inverses += 1;
                for round in 0..4 {
                    let envelope = recover(&case, &scene, Recovering::ConfigSet, 0);
                    let context = format!("{spec}, recovery crashed at inverse {j}, round {round}");
                    assert_before(
                        &case,
                        &scene,
                        &before,
                        Recovering::ConfigSet,
                        repair.as_deref(),
                        &context,
                    );
                    if round > 0 {
                        assert!(
                            envelope["warnings"].as_array().is_none_or(|w| w.is_empty()),
                            "{context}: nothing left to say: {envelope}"
                        );
                    }
                }
            }
        }
        log.note(format!("{crashed_inverses} recoveries crashed"));
        assert!(crashed_inverses >= 2 * (n - 1), "every inverse crashed");
    }

    /// Item 6: an I/O failure injected into the reactivation's rollback leaves it
    /// unsettled, and F is never recovered ahead of it; the next command settles it.
    /// `doctor.recover.step` fires after an inverse has run, so the rollback's first
    /// inverse (the activation's) has already put F back under its provisional name: no
    /// pass lists F while its repair is unsettled, whichever client the command commits.
    /// (A journal held at its own name behind a repair whose recovery failed on I/O, and
    /// the `io` error it gives its own client's command, are proved in process by
    /// `engine::journal`'s `a_behind_journal_of_the_callers_target_fails_it_with_the_owners_stop`.)
    #[test]
    fn barrier_io_failure_in_recovery_leaves_the_journal_behind() {
        let log = tlog!();
        for by in [
            Recovering::InstallOwn,
            Recovering::InstallOther,
            Recovering::ConfigSet,
        ] {
            let case = log.case(by.label());
            let scene = BarrierScene::new(&case, FolderCrash::MidSwap);
            scene.set_aside_whole(&case);
            let before = scene.trees();
            let n = scene.reactivation_len();
            assert!(matches!(
                scene.reactivate(&case, &format!("fsx.mutate.logged=abort@{n}")),
                Child::Crashed
            ));
            assert!(scene.journal().exists(), "activated, no done");
            let repair = scene.repair_tx().unwrap();
            let foo = scene.foo_now();
            let envelope = recover_armed(&case, &scene, by, 0, "doctor.recover.step=error@1");
            let context = format!("{} with an I/O failure in recovery", by.label());
            assert!(
                about(&envelope, "journal_pending", &repair)
                    .iter()
                    .any(|m| m.contains("its recovery failed (io)")),
                "{context}: the repair is unsettled: {envelope}"
            );
            assert!(
                scene.provisional().exists(),
                "{context}: F back under its provisional name"
            );
            assert!(
                !scene.journal().exists(),
                "{context}: F is listed by no pass"
            );
            assert!(
                about(&envelope, "journal_recovered", &scene.f).is_empty(),
                "{context}: F is never recovered ahead of its repair"
            );
            assert_eq!(scene.foo_now(), foo, "{context}: retail's foo untouched");
            assert!(
                !scene.repair_lines().is_empty(),
                "{context}: the repair's journal kept"
            );
            // The next command settles the repair: whole before.
            let _ = recover(&case, &scene, Recovering::ConfigSet, 0);
            assert_before(&case, &scene, &before, by, Some(&repair), &context);
        }
    }

    /// Item 7: a repair whose `prepared` line does not decode holds every journal: F is
    /// never recovered while it exists, every committing command is refused, `status`
    /// runs, and once it is moved away F is recovered. A committing command stages before
    /// it takes the lock, so the closed set refuses it at that first write, before its
    /// recovery runs: the error is the planted journal's own (its `journal_pending` and
    /// `tx`). `config set` recovers first: F waits behind the planted journal (a
    /// `journal_pending` warning with its reason), then the write is refused.
    #[test]
    fn barrier_an_unknowable_repair_holds_every_journal() {
        let log = tlog!();
        let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
        let unknowable = repairs::plant_undecodable_prepared(&scene.home);
        let f = scene.f.clone();
        for by in [Recovering::InstallOwn, Recovering::InstallOther] {
            let envelope = recover(&log, &scene, by, 1);
            let error = &envelope["error"];
            assert_eq!(error["code"], "state_corrupt", "{}: {envelope}", by.label());
            assert_eq!(error["details"]["reason"], "journal_unreadable");
            assert_eq!(error["details"]["tx"], unknowable.as_str());
            assert_eq!(
                error["details"]["resource"],
                format!("journal:{unknowable}")
            );
            assert!(
                about(&envelope, "journal_recovered", &f).is_empty(),
                "{}: F is never recovered",
                by.label()
            );
            assert!(scene.journal().exists(), "{}: F untouched", by.label());
        }
        {
            let by = Recovering::ConfigSet;
            let envelope = recover(&log, &scene, by, 1);
            assert_eq!(
                envelope["error"]["code"],
                "state_corrupt",
                "{}: refused",
                by.label()
            );
            assert!(
                about(&envelope, "journal_pending", &f)
                    .iter()
                    .any(|m| m.contains("(journal_unreadable)")),
                "{}: F waits behind it: {envelope}",
                by.label()
            );
            assert!(scene.journal().exists(), "{}: F untouched", by.label());
        }
        log.run(&scene.home, &["status", "--json"]).assert_private();
        // Standing in for its set-aside.
        let planted = repairs::journal_dir(&scene.home).join(format!("{unknowable}.repair.jsonl"));
        std::fs::rename(&planted, scene.home.env.root().join("set-aside.jsonl")).unwrap();
        let envelope = recover(&log, &scene, Recovering::ConfigSet, 0);
        assert_eq!(
            about(&envelope, "journal_recovered", &f).len(),
            1,
            "{envelope}"
        );
        assert_eq!(scene.foo_now(), scene.v1_foo);
    }

    /// Item 8: a pending repair whose writes overlap a folder journal pass 2 left
    /// unresolved waits behind it, fenced with that journal's stop; a disjoint one is
    /// recovered.
    #[test]
    fn barrier_a_repair_waits_behind_an_unresolved_folder_journal() {
        use super::common::repairs::{FixSpec, Root};
        let log = tlog!();
        let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
        // F made unrecoverable: its step's recorded source inode is not what is there.
        let journal = scene.journal();
        let text = std::fs::read_to_string(&journal).unwrap();
        let mut edited = false;
        let lines: Vec<String> = text
            .lines()
            .map(|line| {
                let mut value: Value = serde_json::from_str(line).unwrap();
                if !edited
                    && value["type"] == "step"
                    && let Some(ino) = value["from_id"]["ino"].as_u64()
                {
                    value["from_id"]["ino"] = (ino + 1_000_000).into();
                    edited = true;
                }
                value.to_string()
            })
            .collect();
        assert!(edited, "F has a step: {text}");
        std::fs::write(&journal, format!("{}\n", lines.join("\n"))).unwrap();
        let run = repairs::doctor_run(&scene.home);
        let clients = [scene.retail.clone()];
        let foo = std::fs::canonicalize(scene.home.addons(RETAIL).join("Foo")).unwrap();
        let waits = repairs::run_child(
            &scene.home,
            &log,
            &run,
            &clients,
            &[FixSpec::Quarantine {
                path: foo.clone(),
                root: Root::Client(scene.retail.clone()),
            }],
            "fsx.mutate.executed=abort@1",
        );
        assert!(matches!(waits, Child::Crashed), "{waits:?}");
        let waiting = repairs::repair_txs(&scene.home);
        let test_file = scene.home.env.state().join("omaddons/test-file");
        std::fs::write(&test_file, "x").unwrap();
        let disjoint = repairs::run_child(
            &scene.home,
            &log,
            &run,
            &clients,
            &[FixSpec::Chmod {
                path: std::fs::canonicalize(&test_file).unwrap(),
                mode: 0o600,
            }],
            "fsx.mutate.executed=abort@1",
        );
        assert!(matches!(disjoint, Child::Crashed), "{disjoint:?}");
        let r3 = waiting[0].clone();
        let r4 = repairs::repair_txs(&scene.home)
            .into_iter()
            .find(|tx| *tx != r3)
            .unwrap();
        let envelope = recover(&log, &scene, Recovering::ConfigSet, 0);
        assert!(
            about(&envelope, "journal_pending", &scene.f)
                .iter()
                .any(|m| m.contains("(step_unrecognized)")),
            "F reported: {envelope}"
        );
        let behind = about(&envelope, "journal_pending", &r3);
        assert!(
            behind.iter().any(|m| m.starts_with(&format!(
                "repair transaction {r3} waits behind transaction {}",
                scene.f
            )) && m.contains("(step_unrecognized)")),
            "R3 waits behind F: {envelope}"
        );
        assert!(
            repairs::repair_txs(&scene.home).contains(&r3),
            "R3 is not recovered"
        );
        assert_eq!(
            about(&envelope, "journal_recovered", &r4).len(),
            1,
            "R4 rolled back"
        );
        let paths = omaddons::state::paths::Paths::with_roots(
            scene.home.env.home(),
            scene.home.env.config(),
            scene.home.env.state(),
            scene.home.env.cache(),
        );
        let fences = omaddons::engine::fence::load(&paths, &clients);
        let fence = fences.covering(&foo);
        assert!(fence.is_some(), "R3's writes are fenced");
        let stop = &fence.unwrap().stop;
        assert_eq!(stop.tx.map(|tx| tx.to_string()), Some(scene.f.clone()));
        assert_eq!(stop.reason, Some("step_unrecognized"));
    }

    /// F at its own name behind a set-aside recovery cannot settle: a command on F's own
    /// client fails with the set-aside's stop (`details.journal_pending` F, `details.tx`
    /// the set-aside, `details.resource` `journal:<F>`) and changes nothing; one on the
    /// other client commits, F fenced; `config set` warns and goes on. F is never
    /// recovered while its owner is stuck.
    #[test]
    fn barrier_a_journal_behind_a_stuck_set_aside() {
        let log = tlog!();
        let scene = BarrierScene::new(&log, FolderCrash::MidSwap);
        let owner = scene.stuck_set_aside(&log);
        let f = scene.f.clone();
        let foo = scene.foo_now();
        let envelope = recover(&log, &scene, Recovering::InstallOwn, 1);
        let error = &envelope["error"];
        assert_eq!(error["code"], "internal", "{envelope}");
        assert_eq!(error["details"]["reason"], "step_unrecognized");
        assert_eq!(error["details"]["journal_pending"], f.as_str());
        assert_eq!(error["details"]["tx"], owner.as_str());
        assert_eq!(error["details"]["resource"], format!("journal:{f}"));
        assert_eq!(scene.foo_now(), foo, "nothing changed on retail");
        for by in [Recovering::InstallOther, Recovering::ConfigSet] {
            let envelope = recover(&log, &scene, by, 0);
            let behind = about(&envelope, "journal_pending", &f);
            assert!(
                behind.iter().any(|m| m.starts_with(&format!("transaction {f} on target"))
                    && m.contains(&format!("waits behind repair {owner}, which recovery could not settle (step_unrecognized)"))),
                "{}: {envelope}",
                by.label()
            );
            assert!(
                about(&envelope, "journal_recovered", &f).is_empty(),
                "{}: F is never recovered behind a stuck owner",
                by.label()
            );
            assert!(scene.journal().exists(), "{}: F left as it is", by.label());
        }
        assert_eq!(scene.foo_now(), foo, "retail's foo as F's crash left it");
    }
}
