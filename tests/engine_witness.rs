//! e2e suite: the adversarial matrix of the approval under the commit lock: every guarded rename refused and rolled back, the residual window, bound crash matrices, repeated recovery and injected failures.
//!
//! D8's design gate (`omad-m4-agent-surface-doctor-c6cm.4.8`, common rule 14). D8 adds no
//! failpoint and no write of its own, so its matrix is about the journaled steps it now
//! guards: each swap rename compares the identities its journal line recorded with the
//! ones the commit window's checks saw, and carries fsx endpoint predicates for the
//! instant after. Every crash is a real `abort` in a child; every interleaving is a
//! `pause` failpoint. `tests/engine_crash.rs` and `tests/engine_concurrency.rs` are
//! rerun unchanged: their unbound swaps carry the same predicates.
#![cfg(test)]
#![cfg(debug_assertions)]

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use common::binding as b;
use common::engine::{Scene, folder, shapes, warning_codes};
use common::http::{Asset, gh_release};
use common::pause::Pause;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::{copy_tree_exact, tree_hash};
use omaddons::engine::FAILPOINTS;

/// The pause before any step: the journal holds `prepared` and no step line yet.
const PREPARED: &str = "engine.journal.prepared";
/// Hit once per journaled rename, after its line is written and compared, before it runs.
const BEFORE_RENAME: &str = "engine.swap.before_rename";

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `args` with `--json --progress json`, so both streams are machine-read and checked.
fn json_args(args: &[String]) -> Vec<String> {
    let mut argv = args.to_vec();
    argv.extend(strings(&["--json", "--progress", "json"]));
    argv
}

/// `omaddons <args> --json` on the scene, which must succeed.
fn ok(log: &TestLog, s: &Scene, args: &[&str]) -> Ran {
    let ran = log.run(s, &json_args(&strings(args)));
    ran.ok();
    ran
}

/// A seeded scene, the run it binds (no presentation flags), the folders the oracle
/// judges, and an addon a later records-only command can pin.
struct Seeded {
    s: Scene,
    args: Vec<String>,
    folders: &'static [&'static str],
    pin: &'static str,
    /// The child runs with `fsx.renameat2.einval` (the two-rename fallback).
    fallback: bool,
}

impl Seeded {
    /// The preview's token.
    fn preview(&self, log: &TestLog) -> String {
        let mut args = self.args.clone();
        args.push("--dry-run".to_owned());
        let ran = log.run(&self.s, &json_args(&args));
        let token = ran.ok().assert_private().data()["precondition"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        log.json("token", &token);
        token
    }

    /// The run bound to `token`, or unbound when `None`.
    fn run_args(&self, token: Option<&str>) -> Vec<String> {
        let mut args = self.args.clone();
        if let Some(token) = token {
            args.extend(strings(&["--if-match", token]));
        }
        json_args(&args)
    }

    /// The failpoint spec `spec`, with the fallback's switch when the shape needs it.
    fn spec(&self, spec: &str) -> String {
        if self.fallback {
            format!("fsx.renameat2.einval=error,{spec}")
        } else {
            spec.to_owned()
        }
    }

    fn addons(&self) -> PathBuf {
        self.s.addons()
    }

    fn folders(&self) -> BTreeMap<String, Option<String>> {
        self.s.folders_of(self.folders)
    }

    /// The run stopped at `point` (`@N` allowed), `act` done while it waits, resumed.
    fn paused(&self, log: &TestLog, run: &[String], point: &str, act: impl FnOnce()) -> Ran {
        let name = point.split('@').next().unwrap_or(point);
        log.step(&format!("run, paused at {point}"));
        let pause = Pause::new(self.s.home.env.root());
        let child = log
            .cmd(&self.s, run)
            .failpoints(&self.spec(&format!("{name}=pause{}", at_suffix(point))))
            .pause(&pause)
            .spawn();
        log.reached(&pause, name);
        act();
        log.step("resume");
        log.resume(&pause, name);
        child.wait()
    }

    /// The journal files' lines, for the record.
    fn journal(&self) -> Vec<Vec<Value>> {
        self.s
            .journals()
            .iter()
            .map(|p| {
                fs::read_to_string(p)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            })
            .collect()
    }
}

/// `@N` of a point, or nothing.
fn at_suffix(point: &str) -> String {
    point
        .split_once('@')
        .map(|(_, n)| format!("@{n}"))
        .unwrap_or_default()
}

/// Release `tag` of `gh:o/<repo>` as its latest: `<name>/<name>.toc` and `f00.lua` …
/// `f11.lua`, loading on retail.
fn publish(s: &Scene, repo: &str, name: &str, tag: &str) {
    let mut files = vec![(
        format!("{name}/{name}.toc"),
        format!("## Interface: 120105\n## Version: {tag}\n"),
    )];
    files.extend((0..12).map(|i| (format!("{name}/f{i:02}.lua"), format!("-- {tag} {i}\n"))));
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let file = format!("{name}.zip");
    let assets: [Asset<'_>; 1] = [(file.as_str(), &[], borrowed)];
    gh_release(
        &s.home.http,
        &format!("https://github.com/o/{repo}"),
        tag,
        &assets,
    );
}

// ---- the shapes ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Alpha, Beta and Gamma at v1, v2 released: three exchanges, then three old
    /// versions to the snapshot.
    UpdateOfThree,
    /// Foo and Bar installed and removed: two removals, Bar's first (key order).
    RemoveOfTwo,
    /// Alpha and Beta released, not installed: two new folders going live.
    InstallOfTwoNew,
    /// Alpha and Beta at v1, v2 released, with no `RENAME_EXCHANGE`: each folder's
    /// live version to the snapshot, then its new one live.
    UpdateInTheFallback,
}

impl Shape {
    fn seed(self, log: &TestLog) -> Seeded {
        log.step("seed");
        let s = Scene::new();
        log.env(&s);
        let (args, folders, pin, fallback): (Vec<String>, &'static [&'static str], _, _) =
            match self {
                Self::UpdateOfThree => {
                    let s = &s;
                    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta"), ("gamma", "Gamma")] {
                        s.release(repo, "v1", &[folder(name, &[])]);
                    }
                    ok(
                        log,
                        s,
                        &[
                            "install",
                            "gh:o/alpha",
                            "gh:o/beta",
                            "gh:o/gamma",
                            "-t",
                            "retail",
                        ],
                    );
                    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta"), ("gamma", "Gamma")] {
                        s.release(repo, "v2", &[folder(name, &[])]);
                    }
                    let args = strings(&["update", "-t", "retail", "--refresh"]);
                    (args, shapes::UPDATE_MANY, "Gamma", false)
                }
                Self::RemoveOfTwo => {
                    for (repo, name) in [("foo", "Foo"), ("bar", "Bar")] {
                        s.release(repo, "v1", &[folder(name, &[])]);
                    }
                    ok(
                        log,
                        &s,
                        &["install", "gh:o/foo", "gh:o/bar", "-t", "retail"],
                    );
                    let args = strings(&["remove", "Foo", "Bar", "-t", "retail"]);
                    (args, &["Bar", "Foo"][..], "Bar", false)
                }
                Self::InstallOfTwoNew => {
                    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
                        s.release(repo, "v1", &[folder(name, &[])]);
                    }
                    let args = strings(&["install", "gh:o/alpha", "gh:o/beta", "-t", "retail"]);
                    (args, &["Alpha", "Beta"][..], "", false)
                }
                Self::UpdateInTheFallback => {
                    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
                        s.release(repo, "v1", &[folder(name, &[])]);
                    }
                    ok(
                        log,
                        &s,
                        &["install", "gh:o/alpha", "gh:o/beta", "-t", "retail"],
                    );
                    for (repo, name) in [("alpha", "Alpha"), ("beta", "Beta")] {
                        s.release(repo, "v2", &[folder(name, &[])]);
                    }
                    let args = strings(&["update", "-t", "retail", "--refresh"]);
                    (args, &["Alpha", "Beta"][..], "Alpha", true)
                }
            };
        Seeded {
            s,
            args,
            folders,
            pin,
            fallback,
        }
    }

    /// The journaled steps the matrix tampers with, each with the folder it moves. In the
    /// fallback, step 1 is Alpha's exchange attempt (journaled, then refused by the
    /// filesystem, which switches the swap to plain renames): Alpha's live-to-snapshot
    /// rename is step 2 and its stage-to-live step 3, Beta's live-to-snapshot step 4.
    fn tampered(self) -> &'static [(usize, &'static str)] {
        match self {
            Self::UpdateOfThree => &[(1, "Alpha"), (2, "Beta"), (3, "Gamma")],
            Self::RemoveOfTwo => &[(1, "Bar"), (2, "Foo")],
            Self::InstallOfTwoNew => &[(1, "Alpha"), (2, "Beta")],
            Self::UpdateInTheFallback => &[(1, "Alpha"), (4, "Beta")],
        }
    }
}

// ---- tampering ----------------------------------------------------------------------------

/// The identity (device, inode) of the entry at `path`, never following a link.
fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::symlink_metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// `AddOns/<name>` moved aside and replaced by an exact copy: the same names, bytes, modes
/// and mtimes under another identity. Fails at once unless exactly that changed.
fn replace_by_copy(log: &TestLog, seeded: &Seeded, name: &str) {
    let live = seeded.addons().join(name);
    let aside = seeded.s.home.env.root().join(format!("{name}-aside"));
    let (before, hash) = (identity(&live), tree_hash(&live));
    fs::rename(&live, &aside).unwrap();
    copy_tree_exact(&aside, &live);
    log.note(format!(
        "{name} replaced by an exact copy; the original kept aside"
    ));
    assert!(
        identity(&live) != before && tree_hash(&live) == hash,
        "the tamper must change {name}'s identity and nothing else"
    );
}

/// A folder `name` made by hand where the plan creates one.
fn made_by_hand(log: &TestLog, seeded: &Seeded, name: &str) {
    let dir = seeded.addons().join(name);
    assert!(!dir.exists(), "{name} must not exist before the tamper");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join(format!("{name}.toc")), "## Interface: 120105\n").unwrap();
    log.note(format!("{name} made by hand"));
}

fn tamper(log: &TestLog, shape: Shape, seeded: &Seeded, name: &str) {
    log.step("tamper");
    if shape == Shape::InstallOfTwoNew {
        made_by_hand(log, seeded, name);
    } else {
        replace_by_copy(log, seeded, name);
    }
}

/// Where a run pauses so that `step`'s line is not written yet: before the first step,
/// or right before the previous step's rename.
fn before_step(step: usize) -> String {
    if step == 1 {
        PREPARED.to_owned()
    } else {
        format!("{BEFORE_RENAME}@{}", step - 1)
    }
}

/// Entries of `dir`, sorted (nothing when it is absent).
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|l| {
            l.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// `status --json`'s warnings: whether one says a journal waits.
fn status_pending(log: &TestLog, s: &Scene) -> bool {
    let ran = log.run(s, &json_args(&strings(&["status"])));
    warning_codes(ran.ok().assert_private().envelope())
        .iter()
        .any(|c| c == "journal_pending")
}

// ---- 1. a refused rename at every journaled step ------------------------------------------

/// Step `step` (moving `name`) of `shape` refused: its folder tampered with before its
/// journal line, the run resumed. It refuses before the rename, rolls every earlier step
/// back, and leaves nothing.
fn refused_at(log: &TestLog, shape: Shape, step: usize, name: &str, bound: bool) {
    let seeded = shape.seed(log);
    let token = bound.then(|| {
        log.step("preview");
        seeded.preview(log)
    });
    let (generation, history) = (seeded.s.generation(), seeded.s.history_txs());
    // What a clean rollback leaves: the content before the run, since an exact copy
    // changes no content (by the time step n is reached, steps before n - 1 have run, so
    // the tree at the pause is mid-swap); for a folder made by hand, the tree right after
    // it was made, which no rename of this shape has touched yet.
    let made = shape == Shape::InstallOfTwoNew;
    if !made {
        log.tree("addons", &seeded.addons());
    }
    let run = seeded.run_args(token.as_deref());
    let ran = seeded.paused(log, &run, &before_step(step), || {
        tamper(log, shape, &seeded, name);
        if made {
            log.tree("addons", &seeded.addons());
        }
    });

    log.step("outcome");
    if bound {
        b::assert_stale(
            log,
            &ran,
            &["observed"],
            &b::preview_argv(&strs(&seeded.args)),
        );
    } else {
        let error = &ran.fails(1, "plan_stale").envelope()["error"];
        let mut rerun = strings(&["omaddons"]);
        rerun.extend(seeded.args.iter().cloned());
        log.assert_eq(
            "(retryable, fix_argv: the same command again)",
            &(&error["retryable"], &error["fix_argv"]),
            &(&json!(true), &json!(rerun)),
        );
    }

    log.step("oracle");
    log.assert_unchanged("addons", &seeded.addons());
    let meta = seeded.s.home.client("_retail_").join("Interface/.omaddons");
    log.assert_eq(
        "(journals, stage entries, generation, transactions)",
        &(
            seeded.s.journals().len(),
            entries(&meta.join("stage")),
            seeded.s.generation(),
            seeded.s.history_txs(),
        ),
        &(0, Vec::<String>::new(), generation, history),
    );
    log.assert_eq(
        "status reports no journal",
        &status_pending(log, &seeded.s),
        &false,
    );
}

fn strs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Every tampered step of `shape`, bound, and unbound too when asked.
fn per_step(log: &TestLog, shape: Shape, unbound: bool) {
    for &(step, name) in shape.tampered() {
        for bound in [true, false] {
            if !bound && !unbound {
                continue;
            }
            let case = log.case(&format!(
                "step {step} {name} {}",
                if bound { "bound" } else { "unbound" }
            ));
            refused_at(&case, shape, step, name, bound);
        }
    }
}

#[test]
fn a_refused_rename_at_every_step_of_an_update_of_three() {
    per_step(&tlog!(), Shape::UpdateOfThree, true);
}

#[test]
fn a_refused_rename_at_every_step_of_a_remove_of_two() {
    per_step(&tlog!(), Shape::RemoveOfTwo, false);
}

#[test]
fn a_refused_rename_at_every_step_of_an_install_of_two_new() {
    per_step(&tlog!(), Shape::InstallOfTwoNew, false);
}

#[test]
fn a_refused_rename_at_every_step_of_an_update_in_the_fallback() {
    per_step(&tlog!(), Shape::UpdateInTheFallback, false);
}

// ---- 2. the residual window, pinned -------------------------------------------------------

/// Step 2's folder replaced after its journal line was written and compared: the rename's
/// own predicate refuses, so nothing moves at that step, but the step reads in neither
/// state and recovery stops for repair, losing nothing (M4-D60's stated scope).
fn residual(log: &TestLog, shape: Shape) {
    let seeded = shape.seed(log);
    log.step("preview");
    let token = seeded.preview(log);
    let (step1, step2) = (shape.tampered()[0].1, shape.tampered()[1].1);
    let run = seeded.run_args(Some(&token));
    let ran = seeded.paused(log, &run, &format!("{BEFORE_RENAME}@2"), || {
        tamper(log, shape, &seeded, step2);
    });

    log.step("outcome: stopped for repair");
    log.json("refusal", &ran.envelope()["error"]);
    let journal = seeded.journal();
    log.json("journal", &journal);
    let error = &ran.envelope()["error"];
    let tx = error["details"]["journal_pending"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    log.assert_eq(
        "(exit 1, journal_pending names a journal left, recovery_error.code)",
        &(
            ran.run.code,
            journal.len(),
            seeded
                .s
                .journals()
                .iter()
                .any(|p| p.to_string_lossy().contains(&tx) && !tx.is_empty()),
            &error["details"]["recovery_error"]["code"],
        ),
        &(1, 1, true, &json!("internal")),
    );

    log.step("oracle: each folder wholly one version, nothing lost");
    let addons = seeded.addons();
    let meta = seeded.s.home.client("_retail_").join("Interface/.omaddons");
    let aside = seeded.s.home.env.root().join(format!("{step2}-aside"));
    log.json("folders", &seeded.folders());
    match shape {
        Shape::UpdateOfThree => {
            log.assert_eq(
                "(Alpha at v2, Beta the copy at v1, the original aside, Beta v2 still staged)",
                &(
                    toc_version(&addons.join(step1), step1),
                    toc_version(&addons.join(step2), step2),
                    toc_version(&aside, step2),
                    toc_version(&meta.join("stage").join(&tx).join(step2), step2),
                ),
                &(v("v2"), v("v1"), v("v1"), v("v2")),
            );
        }
        _ => {
            log.assert_eq(
                "(Bar removed into the snapshot, Foo the copy, the original aside)",
                &(
                    addons.join(step1).exists(),
                    toc_version(
                        &meta.join("snapshots").join(&tx).join("AddOns").join(step1),
                        step1,
                    ),
                    toc_version(&addons.join(step2), step2),
                    toc_version(&aside, step2),
                ),
                &(false, v("v1"), v("v1"), v("v1")),
            );
        }
    }
    log.assert_eq(
        "status reports the journal",
        &status_pending(log, &seeded.s),
        &true,
    );

    log.step("recover: the next lock-taking commands stop the same way");
    // Each a real change, so it reaches the stop: the one-client `install`, and the
    // commands that report per client, whose one error is their target's (omad-alz5).
    // Delta is published for `sync` to install, and Echo and Fox unpacked, in the
    // catalog, for `adopt` to record.
    seeded.s.release("delta", "v1", &[folder("Delta", &[])]);
    seeded.s.home.write_desired(
        "schema_version = \"1.0\"\n\n[family.mainline]\naddons = [\"gh:o/delta\"]\n",
    );
    let adopt = shapes::adopt(&seeded.s);
    log.tree("addons", &addons);
    let mut install = seeded.s.install_args("gh:o/bar", "bar");
    install.extend(strings(&["--progress", "json"]));
    let mut commands = vec![
        install,
        json_args(&strings(&["pin", seeded.pin, "-t", "retail"])),
        json_args(&strings(&["sync", "-t", "retail"])),
        json_args(&adopt[..adopt.len() - 1]),
    ];
    if shape == Shape::UpdateOfThree {
        commands.push(json_args(&strings(&["update", "-t", "retail"])));
    }
    for argv in commands {
        let ran = log.run(&seeded.s, &argv);
        let error = &ran.envelope()["error"];
        log.json(&format!("{}: error", argv[0]), error);
        log.assert_eq(
            &format!(
                "{}: (exit, code, names step 2, details.reason, details.journal_pending)",
                argv[0]
            ),
            &(
                ran.run.code,
                &error["code"],
                error["message"].as_str().is_some_and(|m| {
                    m.starts_with("journal step 2 ")
                        && m.contains("matches neither its before nor its after state")
                }),
                &error["details"]["reason"],
                &error["details"]["journal_pending"],
            ),
            &(
                1,
                &json!("internal"),
                true,
                &json!("step_unrecognized"),
                &json!(tx),
            ),
        );
    }
    log.assert_unchanged("addons", &addons);
    log.assert_eq("the journal is kept", &seeded.journal(), &journal);
}

/// The `## Version` of `<dir>/<name>.toc`, `None` when there is none.
fn toc_version(dir: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(dir.join(format!("{name}.toc"))).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("## Version: "))
        .map(str::to_owned)
}

fn v(version: &str) -> Option<String> {
    Some(version.to_owned())
}

#[test]
fn residual_after_the_step_line_update_of_three() {
    residual(&tlog!(), Shape::UpdateOfThree);
}

#[test]
fn residual_after_the_step_line_remove_of_two() {
    residual(&tlog!(), Shape::RemoveOfTwo);
}

// ---- 3. bound crash matrices --------------------------------------------------------------

/// Foo (and FooOld) at v1, and the update to v2 from its fixture archive, as
/// `tests/engine_crash.rs`'s update cell builds it, with SavedVariables of both under the
/// fake account, realm and character, so every run's output is checked for them.
fn foo_update(log: &TestLog) -> Seeded {
    log.step("seed");
    let s = Scene::new();
    let mut wtf = common::wtf::Wtf::at(s.home.client("_retail_"));
    for folder in common::engine::FOLDERS {
        wtf = wtf.saved_variables(folder, 256);
    }
    wtf.build();
    log.env(&s);
    log.run(
        &s,
        &json_args(&strings(&[
            "install",
            "gh:o/foo",
            "--file",
            &s.zip("v1"),
            "-t",
            "retail",
            "--offline",
        ])),
    )
    .ok()
    .assert_private();
    let v2 = s.zip("v2");
    let args = strings(&[
        "install",
        "gh:o/foo",
        "--file",
        &v2,
        "-t",
        "retail",
        "--offline",
    ]);
    Seeded {
        s,
        args,
        folders: common::engine::FOLDERS,
        pin: "Foo",
        fallback: false,
    }
}

/// Journals that never reached `committed`.
fn pending(s: &Scene) -> usize {
    s.journals()
        .iter()
        .filter(|p| {
            !fs::read_to_string(p)
                .unwrap_or_default()
                .contains("\"type\":\"committed\"")
        })
        .count()
}

/// One bound run crashed at `spec`, then the oracle: at the crash each folder is wholly
/// old or wholly new and SavedVariables untouched; a reader reports the unfinished
/// transaction and changes nothing; the next lock-taking command (an install of Bar)
/// recovers it;
/// then every folder sits on one side, consistent with the generation and history.
/// Returns whether the run crashed.
fn crash_bound(log: &TestLog, seed: fn(&TestLog) -> Seeded, spec: &str, kind: &str) -> bool {
    let reference = seed(&log.case("reference"));
    let old = reference.folders();
    log.run(&reference.s, &reference.run_args(None))
        .ok()
        .assert_private();
    let new = reference.folders();

    let seeded = seed(log);
    log.step("preview");
    let token = seeded.preview(log);
    let (generation, history, wtf) = (seeded.s.generation(), seeded.s.history(), seeded.s.wtf());
    log.step(&format!("bound run, armed with {spec}"));
    let ran = log
        .cmd(&seeded.s, &seeded.run_args(Some(&token)))
        .failpoints(&seeded.spec(spec))
        .run();
    let crashed = ran.signal.is_some();
    if crashed {
        ran.aborted();
    } else {
        ran.ok();
    }
    ran.assert_private();

    log.step("oracle at the crash");
    let at_crash = seeded.folders();
    log.json("folders", &at_crash);
    for (name, now) in &at_crash {
        assert!(
            *now == old[name] || *now == new[name],
            "{spec}: {name} is neither old nor new"
        );
    }
    log.assert_eq("SavedVariables untouched", &seeded.s.wtf(), &wtf);
    let waiting = pending(&seeded.s);
    let before = seeded.s.snapshot();
    log.assert_eq(
        "a reader reports the journal",
        &status_pending(log, &seeded.s),
        &(waiting > 0),
    );
    log.assert_eq("a reader changes nothing", &seeded.s.snapshot(), &before);

    // An install of Bar, which plans nothing the crashed run touched: a pin of one of its
    // addons would plan against the record recovery may then roll forward, and be stale.
    log.step("recover");
    let mut install_bar = seeded.s.install_args("gh:o/bar", "bar");
    install_bar.extend(strings(&["--progress", "json"]));
    let bar = log.run(&seeded.s, &install_bar);
    let recovered = warning_codes(bar.ok().assert_private().envelope())
        .iter()
        .any(|c| c == "journal_recovered");
    log.step("oracle after recovery");
    let now = seeded.folders();
    log.json("folders", &now);
    let went_new = now == new;
    let mut expected = history.clone();
    if went_new {
        expected.push(kind.to_owned());
    }
    expected.push("install".to_owned());
    log.assert_eq(
        "(recovered when a journal waited, no journal left, one side, generation, history)",
        &(
            recovered,
            seeded.s.journals().len(),
            went_new || now == old,
            seeded.s.generation(),
            seeded.s.history(),
        ),
        &(
            waiting > 0,
            0,
            true,
            generation + u64::from(went_new) + 1,
            expected,
        ),
    );
    crashed
}

/// One test per engine failpoint, each saying whether a bound update reaches it, and the
/// list held to `engine::FAILPOINTS`. Off the path: recovery, restores and their
/// preservation, the automatic backup (cloning is off in tests), a download (`--file`
/// downloads nothing) and creating AddOns (it exists).
macro_rules! bound_crash_rows {
    ($($test:ident => $name:literal, $on_path:literal;)*) => {
        const CRASH_ROWS: &[&str] = &[$($name),*];
        $(
            #[test]
            fn $test() {
                let log = tlog!();
                let crashed = crash_bound(&log, foo_update, concat!($name, "=abort"), "update");
                log.assert_eq("crashed: the bound update reaches it", &crashed, &$on_path);
            }
        )*
    };
}

bound_crash_rows! {
    crash_bound_update_engine_plan_fetched => "engine.plan.fetched", true;
    crash_bound_update_engine_plan_downloaded => "engine.plan.downloaded", false;
    crash_bound_update_engine_plan_done => "engine.plan.done", true;
    crash_bound_update_engine_stage_begin => "engine.stage.begin", true;
    crash_bound_update_engine_stage_file => "engine.stage.file", true;
    crash_bound_update_engine_stage_synced => "engine.stage.synced", true;
    crash_bound_update_engine_snapshot_prestaged => "engine.snapshot.prestaged", true;
    crash_bound_update_engine_lock_acquired => "engine.lock.acquired", true;
    crash_bound_update_engine_recovered => "engine.recovered", true;
    crash_bound_update_engine_registered => "engine.registered", true;
    crash_bound_update_engine_revalidated => "engine.revalidated", true;
    crash_bound_update_engine_journal_prepared => "engine.journal.prepared", true;
    crash_bound_update_engine_addons_dir_created => "engine.addons_dir.created", false;
    crash_bound_update_engine_snapshot_sv => "engine.snapshot.sv", true;
    crash_bound_update_engine_snapshot_state => "engine.snapshot.state", true;
    crash_bound_update_engine_swap_before_rename => "engine.swap.before_rename", true;
    crash_bound_update_engine_swap_after_exchange => "engine.swap.after_exchange", true;
    crash_bound_update_engine_swapped => "engine.swapped", true;
    crash_bound_update_engine_record_written => "engine.record.written", true;
    crash_bound_update_engine_history_appended => "engine.history.appended", true;
    crash_bound_update_engine_committed => "engine.committed", true;
    crash_bound_update_engine_unlocked => "engine.unlocked", true;
    crash_bound_update_engine_cleanup_done => "engine.cleanup.done", true;
    crash_bound_update_engine_retention => "engine.retention", true;
    crash_bound_update_engine_recover_step => "engine.recover.step", false;
    crash_bound_update_engine_recover_sv => "engine.recover.sv", false;
    crash_bound_update_engine_recover_forward => "engine.recover.forward", false;
    crash_bound_update_engine_restore_staged => "engine.restore.staged", false;
    crash_bound_update_engine_restore_exchanged => "engine.restore.exchanged", false;
    crash_bound_update_engine_restore_snapshotted => "engine.restore.snapshotted", false;
    crash_bound_update_engine_preserve_intent => "engine.preserve.intent", false;
    crash_bound_update_engine_preserve_moved => "engine.preserve.moved", false;
    crash_bound_update_engine_preserve_published => "engine.preserve.published", false;
    crash_bound_update_engine_autobackup => "engine.autobackup", false;
}

#[test]
fn the_bound_crash_matrix_covers_every_engine_failpoint() {
    let log = tlog!();
    log.assert_eq("the rows are engine::FAILPOINTS", &CRASH_ROWS, &FAILPOINTS);
}

/// Alpha, Beta and Gamma updated bound, crashed before each of its six journaled renames.
#[test]
fn a_crash_at_every_rename_of_a_bound_update_of_three() {
    let log = tlog!();
    let seed: fn(&TestLog) -> Seeded = |log| Shape::UpdateOfThree.seed(log);
    let crashed: Vec<bool> = (1..=6)
        .map(|n| {
            let case = log.case(&format!("rename {n}"));
            crash_bound(&case, seed, &format!("{BEFORE_RENAME}=abort@{n}"), "update")
        })
        .collect();
    log.assert_eq("every rename was reached", &crashed, &vec![true; 6]);
}

// ---- 4. repeated recovery after a refused rename -------------------------------------------

/// Beta replaced while the bound run waits before Alpha's exchange: Beta's step refuses,
/// and the rollback (armed to die at its step) dies; the next command's recovery dies
/// too; the third finishes it and commits its own pin; a fourth finds nothing.
#[test]
fn a_refused_renames_crashed_rollback_is_finished_by_later_commands() {
    let log = tlog!();
    let seeded = Shape::UpdateOfThree.seed(&log);
    log.step("preview");
    let token = seeded.preview(&log);
    let (generation, history) = (seeded.s.generation(), seeded.s.history());
    let records = seeded.s.view();
    let pause = Pause::new(seeded.s.home.env.root());
    log.step("bound run, paused before Alpha's exchange, its rollback armed to die");
    let child = log
        .cmd(&seeded.s, &seeded.run_args(Some(&token)))
        .failpoints(&format!(
            "{BEFORE_RENAME}=pause@1,engine.recover.step=abort"
        ))
        .pause(&pause)
        .spawn();
    log.reached(&pause, BEFORE_RENAME);
    tamper(&log, Shape::UpdateOfThree, &seeded, "Beta");
    log.tree("addons", &seeded.addons());
    log.resume(&pause, BEFORE_RENAME);
    child.wait().aborted();
    log.json("journal", &seeded.journal());
    log.assert_eq("a journal waits", &pending(&seeded.s), &1);

    let pin = json_args(&strings(&["pin", "Gamma", "-t", "retail"]));
    log.step("recover 1: the next command's recovery dies too");
    log.cmd(&seeded.s, &pin)
        .failpoints("engine.recover.step=abort")
        .run()
        .aborted();
    log.assert_eq("the journal still waits", &pending(&seeded.s), &1);

    log.step("recover 2: the third command finishes it, then pins");
    let third = log.run(&seeded.s, &pin);
    log.assert_eq(
        "journal_recovered",
        &warning_codes(third.ok().envelope())
            .iter()
            .any(|c| c == "journal_recovered"),
        &true,
    );
    log.assert_unchanged("addons", &seeded.addons());
    let mut pinned = records.clone();
    if let Some(gamma) = pinned.get_mut("gamma") {
        gamma["pinned"] = json!(true);
    }
    let mut expected = history.clone();
    expected.push("pin".to_owned());
    log.assert_eq(
        "(journals, records: Gamma's pin alone, generation, history: the pin's line)",
        &(
            seeded.s.journals().len(),
            seeded.s.view(),
            seeded.s.generation(),
            seeded.s.history(),
        ),
        &(0, pinned, generation + 1, expected),
    );

    log.step("recover 3: a fourth command finds nothing to recover");
    let fourth = log.run(
        &seeded.s,
        &json_args(&strings(&["unpin", "Gamma", "-t", "retail"])),
    );
    log.assert_eq(
        "no journal_recovered",
        &warning_codes(fourth.ok().envelope())
            .iter()
            .any(|c| c == "journal_recovered"),
        &false,
    );
}

// ---- 5. injected failures -----------------------------------------------------------------

/// An I/O error injected right before the second rename's syscall, after its predicate
/// passed: Beta's exchange. The run fails `io`, Alpha's exchange is rolled back, and every
/// folder is wholly v1.
#[test]
fn an_injected_io_error_after_a_predicated_check_rolls_back() {
    let log = tlog!();
    let seeded = Shape::UpdateOfThree.seed(&log);
    log.step("preview");
    let token = seeded.preview(&log);
    log.tree("addons", &seeded.addons());
    let (generation, history) = (seeded.s.generation(), seeded.s.history());
    log.step("bound run, the second rename failing");
    let ran = log
        .cmd(&seeded.s, &seeded.run_args(Some(&token)))
        .failpoints("fsx.rename_with.before=error@2")
        .run();
    let error = &ran.fails(1, "io").envelope()["error"];
    log.json("error", error);
    let message = error["message"].as_str().unwrap_or_default();
    log.assert_eq(
        "the failed rename is Beta's exchange (from its staged folder)",
        &(message.contains("/stage/")
            && message.contains("/Beta: failpoint fsx.rename_with.before")),
        &true,
    );
    log.step("oracle");
    log.assert_unchanged("addons", &seeded.addons());
    log.assert_eq(
        "(journals, generation, history)",
        &(
            seeded.s.journals().len(),
            seeded.s.generation(),
            seeded.s.history(),
        ),
        &(0, generation, history),
    );
}

/// Foo at v1 (12 files), v2 published: `update Foo -t retail --refresh`.
fn foo_twelve(log: &TestLog) -> Seeded {
    log.step("seed");
    let s = Scene::new();
    log.env(&s);
    publish(&s, "foo", "Foo", "v1");
    ok(log, &s, &["install", "gh:o/foo", "-t", "retail"]);
    publish(&s, "foo", "Foo", "v2");
    Seeded {
        s,
        args: strings(&["update", "Foo", "-t", "retail", "--refresh"]),
        folders: &["Foo"],
        pin: "Foo",
        fallback: false,
    }
}

/// The bound run of `seeded`, with `act` done while it waits before its lock (`act`
/// records the `addons` tree the run must leave, or it is recorded before the run); it
/// must be refused as `observed` saying `why`, moving nothing. `restore` runs before the
/// tree is compared.
fn stale_under_the_lock(
    log: &TestLog,
    seeded: &Seeded,
    why: &str,
    act: impl FnOnce(),
    restore: impl FnOnce(),
) {
    log.step("preview");
    let token = seeded.preview(log);
    let history = seeded.s.history();
    let ran = seeded.paused(
        log,
        &seeded.run_args(Some(&token)),
        "engine.snapshot.prestaged",
        act,
    );
    log.step("outcome");
    b::assert_stale(
        log,
        &ran,
        &["observed"],
        &b::preview_argv(&strs(&seeded.args)),
    );
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        &format!("the refusal says: {why}"),
        &message.contains(why),
        &true,
    );
    restore();
    log.assert_unchanged("addons", &seeded.addons());
    log.assert_eq(
        "(journals, history)",
        &(seeded.s.journals().len(), seeded.s.history()),
        &(0, history),
    );
}

/// A witnessed file unreadable under the lock fails closed. As root, mode bits do not stop
/// a read, so the run commits instead, and the test says why.
#[test]
fn an_unreadable_witnessed_file_under_the_lock_is_stale() {
    let log = tlog!();
    let seeded = foo_twelve(&log);
    let file = seeded.addons().join("Foo/f03.lua");
    let root = {
        let probe = seeded.s.home.env.root().join("probe");
        fs::write(&probe, "x").unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
        fs::read(&probe).is_ok()
    };
    if root {
        log.note("running as root: mode 000 does not stop a read, so the run commits");
        log.step("preview");
        let token = seeded.preview(&log);
        let ran = seeded.paused(
            &log,
            &seeded.run_args(Some(&token)),
            "engine.snapshot.prestaged",
            || {
                fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
            },
        );
        ran.ok();
        return;
    }
    // An unreadable file cannot be hashed: what the run must leave is the tree before it,
    // compared once the mode is back (the tree hash reads modes and bytes, not mtimes).
    log.tree("addons", &seeded.addons());
    stale_under_the_lock(
        &log,
        &seeded,
        "foo cannot be read now",
        || {
            log.step("f03.lua made unreadable, its mtime moved");
            let handle = fs::File::options().write(true).open(&file).unwrap();
            handle
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
                .unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        },
        || fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap(),
    );
}

/// A witnessed folder grown past `limits.archive_max_entries` under the lock fails closed.
#[test]
fn a_witnessed_folder_grown_past_the_limits_under_the_lock_is_stale() {
    let log = tlog!();
    let seeded = foo_twelve(&log);
    let config = seeded.s.home.env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "[limits]\narchive_max_entries = 40\n").unwrap();
    log.note("limits.archive_max_entries = 40: Foo's 13 entries fit");
    let foo = seeded.addons().join("Foo");
    stale_under_the_lock(
        &log,
        &seeded,
        "foo grew past the archive limits",
        || {
            log.step("40 files added to Foo");
            for i in 0..40 {
                fs::write(foo.join(format!("extra{i:02}.lua")), "-- added\n").unwrap();
            }
            log.tree("addons", &seeded.addons());
        },
        || {},
    );
}
