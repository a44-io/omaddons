//! Concurrency checkpoints (vision §5.12, §7 M1), with real processes synchronized by
//! `pause` failpoints: each run stops at a named step until the test resumes it, so the
//! interleaving a test names is the one that runs. A reader during a live swap, recovery
//! only under the lock, no lock while planning and staging, concurrent installs of the
//! same and of different addons, and the lock hold of commits, measured and printed (its
//! 50 ms budget is a release-build check in `tests/perf.rs`).

mod common;

use std::fs;

use common::archives::zip_text;
use common::engine::{Scene, lock_held_ms, shapes, warning_codes};
use common::home::{Run, wait};
use common::pause::Pause;
use serde_json::Value;
use support::{backed_up, backup, play, status};

#[cfg(test)]
mod support {
    use super::*;

    pub fn status(s: &Scene) -> Value {
        s.run(&["status".to_owned(), "--json".to_owned()], "")
            .expect(0)
    }

    /// Whether any journal has reached `swapped`.
    pub fn swapped(s: &Scene) -> bool {
        s.journals().iter().any(|p| {
            fs::read_to_string(p)
                .unwrap_or_default()
                .contains("\"type\":\"swapped\"")
        })
    }

    /// The lowercase hex SHA-256 of `bytes`.
    pub fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A scene with Foo installed and a `WTF/` of two addons' settings backed up, then
    /// played on; the backup's id.
    pub fn backed_up() -> (Scene, String) {
        let s = Scene::new();
        s.install("gh:o/foo", "v1");
        common::wtf::Wtf::at(s.home.client("_retail_"))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 300)
            .saved_variables("Bar", 120)
            .build();
        let id = backup(&s);
        play(&s);
        (s, id)
    }

    /// A manual backup of retail; its id.
    pub fn backup(s: &Scene) -> String {
        let created = s.ok(&["backup", "create", "-t", "retail"]);
        created["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A play session: Foo's account-wide settings rewritten.
    pub fn play(s: &Scene) {
        let sv = s
            .home
            .client("_retail_")
            .join("WTF/Account/FAKEACCT/SavedVariables/Foo.lua");
        let mut text = fs::read_to_string(&sv).unwrap();
        text.push_str("FooDB.played = (FooDB.played or 0) + 1\n");
        fs::write(sv, text).unwrap();
    }
}

#[test]
fn a_reader_during_a_live_swap_reports_it_and_writes_nothing() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let inode = s.lock_inode();
    let argv = [
        "install",
        "gh:o/foo",
        "--file",
        &s.zip("v2"),
        "-t",
        "retail",
        "--offline",
    ];
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.swapped=pause"), pause.env()],
    );
    // Swapped and committing nothing until resumed: the state holds still.
    pause.reached("engine.swapped");
    assert!(support::swapped(&s));
    let before = s.snapshot();
    let envelope = status(&s);
    assert!(
        warning_codes(&envelope)
            .iter()
            .any(|c| c == "tx_in_progress"),
        "{envelope}"
    );
    assert_eq!(envelope["data"]["journal"]["in_progress"], true);
    assert_eq!(s.snapshot(), before, "the reader wrote nothing");
    assert!(support::swapped(&s), "the reader did not recover");

    pause.resume("engine.swapped");
    let done = wait(child, &argv);
    assert_eq!(done.code, 0, "{}", done.context());
    let core = fs::read_to_string(s.addons().join("Foo/core.lua")).unwrap();
    assert_eq!(core, "-- v2\n");
    assert!(s.journals().is_empty());
    assert_eq!(s.lock_inode(), inode);
}

#[test]
fn recovery_happens_only_under_the_lock() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let crashed = s.run(
        &s.install_args("gh:o/foo", "v2"),
        "engine.journal.prepared=abort",
    );
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    let journals = s.journals();
    assert_eq!(journals.len(), 1);

    // Another command holds the lock (paused right after taking it, before recovering).
    let argv = [
        "install",
        "gh:o/bar",
        "--file",
        &s.zip("bar"),
        "-t",
        "retail",
        "--offline",
    ];
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "engine.lock.acquired=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.lock.acquired");
    let envelope = status(&s);
    assert_eq!(
        envelope["data"]["journal"]["in_progress"], true,
        "{envelope}"
    );
    assert!(
        warning_codes(&envelope)
            .iter()
            .any(|c| c == "tx_in_progress"),
        "{envelope}"
    );
    assert_eq!(
        s.journals(),
        journals,
        "untouched while another process holds the lock"
    );

    pause.resume("engine.lock.acquired");
    let done = wait(child, &argv);
    let envelope = done.expect(0);
    assert!(
        warning_codes(&envelope)
            .iter()
            .any(|c| c == "journal_recovered"),
        "the lock holder recovered it\n{}",
        done.context()
    );
    assert!(s.journals().is_empty());
    let core = fs::read_to_string(s.addons().join("Foo/core.lua")).unwrap();
    assert_eq!(core, "-- v1\n", "rolled back: it crashed before the swap");
}

/// README's "Planning and staging take no lock", whatever the machine's speed: a run
/// stopped once planned, and again once staged, leaves the global lock free, and then
/// commits.
#[test]
fn planning_and_staging_take_no_lock() {
    use omaddons::fsx::{LockState, probe_global};
    use omaddons::state::paths::Paths;

    let s = Scene::new();
    s.install("gh:o/foo", "v1"); // registers the target and creates the lock file
    let env = &s.home.env;
    let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
    let generation = s.generation();
    let v2 = s.zip("v2");
    let argv = [
        "install",
        "gh:o/foo",
        "--file",
        &v2,
        "-t",
        "retail",
        "--offline",
    ];
    let pause = Pause::new(env.root());
    let armed = "engine.plan.done=pause,engine.stage.synced=pause";
    let child = s
        .home
        .spawn(&argv, &[("OMADDONS_FAILPOINTS", armed), pause.env()]);
    for step in ["engine.plan.done", "engine.stage.synced"] {
        pause.reached(step);
        assert_eq!(probe_global(&paths).unwrap(), LockState::Free, "{step}");
        pause.resume(step);
    }
    wait(child, &argv).expect(0);
    assert_eq!(s.generation(), generation + 1);
}

/// Runs two installs at once: both stop once staged, and go on together, so they
/// overlap.
fn concurrently(s: &Scene, a: (&str, &str), b: (&str, &str)) -> (Run, Run) {
    let (pa, pb) = (Pause::new(s.home.env.root()), Pause::new(s.home.env.root()));
    let armed = ("OMADDONS_FAILPOINTS", "engine.stage.synced=pause");
    let za = s.zip(a.1);
    let zb = s.zip(b.1);
    let argv_a = [
        "install",
        a.0,
        "--file",
        za.as_str(),
        "-t",
        "retail",
        "--offline",
    ];
    let argv_b = [
        "install",
        b.0,
        "--file",
        zb.as_str(),
        "-t",
        "retail",
        "--offline",
    ];
    let first = s.home.spawn(&argv_a, &[armed, pa.env()]);
    let second = s.home.spawn(&argv_b, &[armed, pb.env()]);
    for p in [&pa, &pb] {
        p.reached("engine.stage.synced");
    }
    for p in [&pa, &pb] {
        p.resume("engine.stage.synced");
    }
    (wait(first, &argv_a), wait(second, &argv_b))
}

#[test]
fn two_installs_of_the_same_addon_at_once() {
    let s = Scene::new();
    s.install("gh:o/bar", "bar"); // registers the target and creates the lock file
    let inode = s.lock_inode();
    let (a, b) = concurrently(&s, ("gh:o/foo", "v1"), ("gh:o/foo", "v1"));
    let (ea, eb) = (a.expect(0), b.expect(0));
    for e in [&ea, &eb] {
        assert_ne!(e["error"]["code"], "locked", "{e}");
    }
    let commits = [&ea, &eb]
        .iter()
        .filter(|e| !e["data"]["tx"].is_null())
        .count();
    assert_eq!(
        commits, 1,
        "one commits, the other re-plans to a no-op\n{ea}\n{eb}"
    );
    let installs = s.history().iter().filter(|k| *k == "install").count();
    assert_eq!(installs, 2, "bar and foo, once each");
    assert_eq!(s.generation(), 2, "the generation counts the commits");
    let list = s
        .run(&["list".to_owned(), "--json".to_owned()], "")
        .expect(0);
    let names: Vec<&str> = list["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Bar", "Foo"], "no lost or duplicated record");
    assert_eq!(s.lock_inode(), inode);
}

/// The run that re-plans to a no-op committed nothing, so it reports none of the
/// warnings its plan gave the work it dropped; the run that commits reports them.
#[test]
fn a_work_dropped_at_revalidation_reports_none_of_its_plan_warnings() {
    let s = Scene::new();
    s.install("gh:o/bar", "bar"); // registers the target and creates the lock file
    fs::write(
        s.dl.join("old.zip"),
        zip_text(&[("Old/Old.toc", "## Interface: 120000\n## Version: 1\n")]),
    )
    .unwrap();
    let (a, b) = concurrently(&s, ("gh:o/old", "old"), ("gh:o/old", "old"));
    let (ea, eb) = (a.expect(0), b.expect(0));
    let (committed, dropped) = if ea["data"]["tx"].is_null() {
        (&eb, &ea)
    } else {
        (&ea, &eb)
    };
    assert!(
        committed["data"]["tx"].is_string() && dropped["data"]["tx"].is_null(),
        "one commits, the other re-plans to a no-op\n{ea}\n{eb}"
    );
    assert!(
        warning_codes(committed).contains(&"out_of_date".to_owned()),
        "{committed}"
    );
    assert!(
        !warning_codes(dropped).contains(&"out_of_date".to_owned()),
        "{dropped}"
    );
}

#[test]
fn two_installs_of_different_addons_at_once() {
    let s = Scene::new();
    // Baz first, which registers the target (an addon of its own: another ref of Bar's
    // archive would be a takeover of Bar).
    fs::write(
        s.dl.join("baz.zip"),
        zip_text(&[("Baz/Baz.toc", "## Interface: 120105\n")]),
    )
    .unwrap();
    s.install("gh:o/baz", "baz");
    let (a, b) = concurrently(&s, ("gh:o/foo", "v1"), ("gh:o/bar", "bar"));
    let (ea, eb) = (a.expect(0), b.expect(0));
    assert!(
        !ea["data"]["tx"].is_null() && !eb["data"]["tx"].is_null(),
        "{ea}\n{eb}"
    );
    assert_eq!(s.generation(), 3);
    assert!(s.addons().join("Foo").is_dir() && s.addons().join("FooOld").is_dir());
    let stage = s.home.client("_retail_").join("Interface/.omaddons/stage");
    let leftovers = fs::read_dir(&stage).map(|l| l.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "each stage dir was its own and is gone");
}

#[test]
fn a_pin_that_commits_while_an_update_plans_wins() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    fs::write(
        s.dl.join("v3.zip"),
        zip_text(&[
            ("Foo/Foo.toc", "## Interface: 120105\n## Version: 3\n"),
            ("Foo/core.lua", "-- v3\n"),
        ]),
    )
    .unwrap();

    // A updates without a version and pauses once it has planned against v1.
    let v2 = s.zip("v2");
    let argv_a = [
        "install",
        "gh:o/foo",
        "--file",
        &v2,
        "-t",
        "retail",
        "--offline",
    ];
    let pause = Pause::new(s.home.env.root());
    let a = s.home.spawn(
        &argv_a,
        &[
            ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.plan.done");
    // B pins v3 meanwhile, to completion.
    let mut argv_b = s.install_args("gh:o/foo", "v3");
    argv_b.extend(["--version".to_owned(), "3".to_owned()]);
    let b = s.run(&argv_b, "").expect(0);
    assert_eq!(b["data"]["items"][0]["status"], "updated", "{b}");

    pause.resume("engine.plan.done");
    let a = wait(a, &argv_a);
    let ea = a.expect(0);
    assert_eq!(
        (&ea["data"]["tx"], &ea["data"]["items"][0]["status"]),
        (&Value::Null, &Value::from("unchanged")),
        "{}",
        a.context()
    );
    assert!(
        warning_codes(&ea).iter().any(|c| c == "addon_pinned"),
        "{}",
        a.context()
    );
    let core = fs::read_to_string(s.addons().join("Foo/core.lua")).unwrap();
    assert_eq!(core, "-- v3\n", "the pinned release stays");
    let list = s
        .run(&["list".to_owned(), "--json".to_owned()], "")
        .expect(0);
    let foo = &list["data"]["targets"][0]["addons"][0];
    assert_eq!(
        (&foo["name"], &foo["version"], &foo["pinned"]),
        (&Value::from("Foo"), &Value::from("3"), &Value::Bool(true)),
        "{list}"
    );
    assert_eq!(s.history(), ["install", "update"], "two commits, none by A");
}

/// Which side of the records-only race pauses.
#[derive(Debug, Clone, Copy)]
enum Paused {
    /// The pin, once planned; the install runs to completion meanwhile.
    Pin,
    /// The install, once staged; the pin runs to completion meanwhile.
    Install,
}

/// Pins Foo while Bar is installed on the same target, `paused` stopped in the middle,
/// and checks the final state only: both commit, neither goes stale.
fn pin_races_an_install(paused: Paused) {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let (folders, before) = (s.folders(), s.record("foo"));
    let (generation, history) = (s.generation(), s.history());
    let pin = ["pin", "Foo", "-t", "retail", "--offline"];
    let bar = s.zip("bar");
    let install = [
        "install",
        "gh:o/bar",
        "--file",
        bar.as_str(),
        "-t",
        "retail",
        "--offline",
    ];
    let pause = Pause::new(s.home.env.root());
    let (pinned, installed) = match paused {
        Paused::Pin => {
            let child = s.home.spawn(
                &pin,
                &[
                    ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
                    pause.env(),
                ],
            );
            pause.reached("engine.plan.done");
            let installed = s.home.run(&install);
            pause.resume("engine.plan.done");
            (wait(child, &pin), installed)
        }
        Paused::Install => {
            let child = s.home.spawn(
                &install,
                &[
                    ("OMADDONS_FAILPOINTS", "engine.stage.synced=pause"),
                    pause.env(),
                ],
            );
            pause.reached("engine.stage.synced");
            let pinned = s.home.run(&pin);
            pause.resume("engine.stage.synced");
            (pinned, wait(child, &install))
        }
    };
    // install reports its transaction in `data.tx`, pin per target.
    for (r, tx) in [
        (&pinned, &["data", "targets", "0", "tx"][..]),
        (&installed, &["data", "tx"][..]),
    ] {
        let envelope = r.expect(0);
        let tx = tx.iter().fold(&envelope, |v, k| match k.parse::<usize>() {
            Ok(i) => &v[i],
            Err(_) => &v[*k],
        });
        assert!(tx.is_string(), "{paused:?}: both commit\n{}", r.context());
    }
    let now = s.record("foo");
    assert_eq!(
        (&now["pinned"], &now["release_id"]),
        (&Value::Bool(true), &before["release_id"]),
        "{paused:?}: {now}"
    );
    assert!(!s.record("bar").is_null() && s.addons().join("Bar").is_dir());
    assert_eq!(s.generation(), generation + 2, "{paused:?}");
    let mut added = s.history().split_off(history.len());
    added.sort();
    assert_eq!(added, ["install", "pin"], "{paused:?}");
    assert_eq!(s.folders(), folders, "{paused:?}: Foo's folders untouched");
    assert!(s.journals().is_empty(), "{paused:?}");
}

#[test]
fn a_pin_and_an_install_of_another_addon_both_commit_either_way() {
    pin_races_an_install(Paused::Pin);
    pin_races_an_install(Paused::Install);
}

/// Every code an envelope's error and items' errors carry.
fn error_codes(envelope: &Value) -> Vec<String> {
    let top = envelope["error"]["code"].as_str().map(str::to_owned);
    let items = envelope["data"]["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["items"].as_array().into_iter().flatten())
        .filter_map(|i| i["error"]["code"].as_str().map(str::to_owned));
    top.into_iter().chain(items).collect()
}

#[test]
fn an_update_and_an_install_of_another_addon_both_commit() {
    let s = Scene::new();
    let update = shapes::update_many(&s);
    let update: Vec<&str> = update.iter().map(String::as_str).collect();
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &update[..update.len() - 1],
        &[
            ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.plan.done");
    let install = s.install("gh:o/bar", "bar");
    pause.resume("engine.plan.done");
    let updated = wait(child, &update);
    let envelope = updated.expect(0);
    assert!(
        envelope["data"]["targets"][0]["tx"].is_string() && install["data"]["tx"].is_string(),
        "{}",
        updated.context()
    );
    let mut kinds = s.history();
    kinds.sort();
    assert_eq!(
        kinds,
        ["install", "install", "update"],
        "baseline, Bar, the update"
    );
    assert!(s.view().values().all(|r| r["version"] == "v2"));
    assert!(!s.record("bar").is_null() && s.journals().is_empty());
}

#[test]
fn an_adoption_racing_an_install_of_its_folder_ends_clean() {
    let s = Scene::new();
    let adopt = shapes::adopt(&s);
    let adopt: Vec<&str> = adopt.iter().map(String::as_str).collect();
    s.release("echo", "v2", &[common::engine::folder("Echo", &[])]);
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &adopt[..adopt.len() - 1],
        &[
            ("OMADDONS_FAILPOINTS", "engine.plan.done=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.plan.done");
    // The install replaces Echo's files (it is unmanaged until the adoption commits).
    let installed = s
        .home
        .run(&["install", "gh:o/echo", "-t", "retail", "--force"]);
    installed.expect(0);
    pause.resume("engine.plan.done");
    let adopted = wait(child, &adopt);
    let codes = error_codes(&adopted.envelope());
    assert!(
        codes
            .iter()
            .any(|c| c == "plan_stale" || c == "ownership_conflict"),
        "the adoption saw the change\n{}",
        adopted.context()
    );
    // Nothing half-applied: Echo has one owner, whose index matches its files.
    let owners: Vec<String> = s
        .view()
        .into_iter()
        .filter(|(_, r)| {
            r["folders"]
                .as_array()
                .is_some_and(|f| f.iter().any(|x| x == "Echo"))
        })
        .map(|(k, _)| k)
        .collect();
    assert_eq!(owners, ["echo"], "{}", adopted.context());
    assert_eq!(s.record("echo")["version"], "v2");
    let status = status(&s);
    assert_eq!(
        status["data"]["targets"][0]["drift"],
        serde_json::json!([]),
        "{status}"
    );
    // A stale adoption records nothing of its target: Fox stays unmanaged.
    assert_eq!(codes, ["plan_stale"], "{}", adopted.context());
    assert!(!s.history().contains(&"adopt".to_owned()));
    assert!(!s.view().contains_key("fox"), "{}", adopted.context());
    assert!(s.journals().is_empty());
}

#[test]
fn two_outdated_runs_at_once_leave_a_valid_memo() {
    let s = Scene::new();
    shapes::update_many(&s);
    let argv = ["outdated", "-t", "retail", "--refresh"];
    let a = s.home.spawn(&argv, &[]);
    let b = s.home.spawn(&argv, &[]);
    for r in [wait(a, &argv), wait(b, &argv)] {
        assert_eq!(r.code, 0, "{}", r.context());
    }
    let memo: Value = serde_json::from_slice(&fs::read(s.home.memo()).unwrap()).unwrap();
    assert_eq!(memo["schema_version"], "1.1", "{memo}");
    let (_, problem) = omaddons::state::memo::load(&common::http::paths(&s.home.env));
    assert!(problem.is_none(), "{problem:?}");
}

/// A sanity ceiling on a commit's lock hold in a debug build under parallel test load:
/// the 50 ms budget is measured on a release build in `tests/perf.rs`, never here.
const LOCK_HOLD_CEILING_MS: u64 = 2_000;

/// The lock hold of multi-addon commits, measured and printed (checkpoint evidence);
/// only a sanity ceiling fails here.
#[test]
fn multi_addon_commits_report_their_lock_hold() {
    let mut held: Vec<(String, u64)> = Vec::new();
    for (what, prepare) in [
        (
            "update of three",
            shapes::update_many as fn(&Scene) -> Vec<String>,
        ),
        ("install with dependencies", shapes::install_with_dep),
        ("mixed sync", shapes::sync_mixed),
        ("handover", shapes::handover),
    ] {
        let s = Scene::new();
        let mut args = prepare(&s);
        args.push("-v".to_owned());
        let r = s.run(&args, "");
        assert_eq!(r.code, 0, "{}", r.context());
        let ms = lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what.to_owned(), ms)));
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("lock_held_ms of multi-addon commits: {held:?}\n"),
    );
    assert!(
        held.iter().all(|(_, ms)| *ms < LOCK_HOLD_CEILING_MS),
        "lock hold times {held:?}"
    );
}

/// The lock hold with 16 MiB of SavedVariables, copied byte by byte, measured and
/// printed; the snapshots and the undo are checked, and only a sanity ceiling fails on
/// time.
#[test]
fn large_saved_variables_report_their_lock_hold() {
    let s = Scene::new();
    let files = s.heavy_saved_variables();
    let sizes: Vec<u64> = files
        .iter()
        .map(|f| fs::metadata(f).unwrap().len())
        .collect();
    let mut held: Vec<(&str, u64)> = Vec::new();
    let mut run = |what: &'static str, mut args: Vec<String>| {
        args.push("-v".to_owned());
        // Byte copies even where the filesystem could reflink: the slow path.
        let r = s.run(&args, "fsx.reflink.unsupported");
        assert_eq!(r.code, 0, "{what}\n{}", r.context());
        let ms = lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what, ms)));
    };
    run("install", s.install_args("gh:o/foo", "v1"));
    run("update", s.install_args("gh:o/foo", "v2"));
    // A play session: every file changed, so the undo restores all ten.
    for file in &files {
        let mut f = fs::OpenOptions::new().append(true).open(file).unwrap();
        std::io::Write::write_all(&mut f, b"FooDB.played = true\n").unwrap();
    }
    run("undo", s.args(&["undo"]));
    let restored: Vec<u64> = files
        .iter()
        .map(|f| fs::metadata(f).unwrap().len())
        .collect();
    assert_eq!(restored, sizes, "the undo restored them");
    run("remove", s.args(&["remove", "Foo"]));
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("lock_held_ms with 16 MiB of SavedVariables: {held:?}\n"),
    );
    assert!(
        held.iter().all(|(_, ms)| *ms < LOCK_HOLD_CEILING_MS),
        "lock hold times {held:?}"
    );
    let snapshots = s
        .home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots");
    let copies = fs::read_dir(&snapshots)
        .unwrap()
        .filter(|e| {
            let dir = e.as_ref().unwrap().path();
            dir.join("WTF/Account/ACCT/SavedVariables/Foo.lua")
                .is_file()
        })
        .count();
    assert_eq!(copies, 4, "every transaction snapshotted them");
}

/// The lock hold with a 64 MiB folder (a copy put in AddOns by hand, then two
/// releases), measured and printed; the undo is checked, and only a sanity ceiling fails
/// on time.
#[test]
fn large_folders_report_their_lock_hold() {
    let s = Scene::new();
    let [one, two] = s.big_folders();
    let mut held: Vec<(&str, u64)> = Vec::new();
    let mut run = |what: &'static str, args: &[&str]| {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["-t", "retail", "--offline", "--json", "-v"].map(str::to_owned));
        let r = s.run(&argv, "");
        assert_eq!(r.code, 0, "{what}\n{}", r.context());
        let ms = lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what, ms)));
    };
    // The unmanaged copy has no files index: it is hashed before the lock.
    run(
        "install --force",
        &["install", "gh:o/big", "--file", &one, "--force"],
    );
    run("update", &["install", "gh:o/big", "--file", &two]);
    run("undo", &["undo"]);
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("lock_held_ms with a 64 MiB folder: {held:?}\n"),
    );
    assert!(
        held.iter().all(|(_, ms)| *ms < LOCK_HOLD_CEILING_MS),
        "lock hold times {held:?}"
    );
    let toc = fs::read_to_string(s.addons().join("Big/Big.toc")).unwrap();
    assert!(toc.contains("big1"), "the undo put release 1 back: {toc}");
}

#[test]
fn saved_variables_written_after_their_copy_are_snapshotted_as_written() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let sv = s.account_saved_variables();
    fs::create_dir_all(&sv).unwrap();
    fs::write(sv.join("Foo.lua"), "-- before logout\n").unwrap();
    let v2 = s.zip("v2");
    let argv = [
        "install",
        "gh:o/foo",
        "--file",
        &v2,
        "-t",
        "retail",
        "--offline",
    ];
    // Paused between the copy made before the lock and the lock.
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "engine.snapshot.prestaged=pause"),
            pause.env(),
        ],
    );
    let snapshots = s
        .home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots");
    pause.reached("engine.snapshot.prestaged");
    let copied = fs::read_dir(&snapshots).into_iter().flatten().any(|e| {
        let dir = e.unwrap().path();
        dir.join("WTF/Account/ACCT/SavedVariables/Foo.lua")
            .is_file()
    });
    assert!(copied, "the copy made before the lock");
    let logout = "-- written on logout, after the copy\n";
    fs::write(sv.join("Foo.lua"), logout).unwrap();

    pause.resume("engine.snapshot.prestaged");
    let done = wait(child, &argv);
    let envelope = done.expect(0);
    let snapshot = snapshots.join(envelope["data"]["tx"].as_str().unwrap());
    let copy = snapshot.join("WTF/Account/ACCT/SavedVariables/Foo.lua");
    assert_eq!(
        fs::read_to_string(copy).unwrap(),
        logout,
        "{}",
        done.context()
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(snapshot.join("WTF.manifest.json")).unwrap()).unwrap();
    let entry = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == "Account/ACCT/SavedVariables/Foo.lua")
        .unwrap();
    assert_eq!(
        entry["sha256"],
        support::sha256_hex(logout.as_bytes()),
        "{manifest}"
    );
}

// ---------------------------------------------------------------------------------------
// Backups and restores (M3-D31, M3-D48): readers of a backup (restore, export, verify)
// hold it shared, prune and the automatic-capture scheduler take theirs exclusively
// without waiting, and none of them stops a change while it copies or stages. Their
// scene helpers are `support::{backed_up, backup, play}`.

/// A restore staging (paused at `engine.restore.staged`, no global lock held) lets an
/// install commit; the restore then finds the records moved under it and replans
/// (`plan_stale`, retryable) with nothing changed, and run again it restores.
#[test]
fn an_install_commits_while_a_restore_stages() {
    let (s, id) = backed_up();
    let played = s.wtf();
    let argv = ["backup", "restore", id.as_str(), "-t", "retail"];
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "engine.restore.staged=pause"),
            pause.env(),
        ],
    );
    pause.reached("engine.restore.staged");
    s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
    pause.resume("engine.restore.staged");
    let done = wait(child, &argv);
    let envelope = done.envelope();
    assert_eq!(
        (&envelope["error"]["code"], &envelope["error"]["retryable"]),
        (&Value::from("plan_stale"), &Value::from(true)),
        "{}",
        done.context()
    );
    assert_eq!(s.wtf(), played, "nothing changed");
    assert!(s.journals().is_empty());
    s.ok(&["backup", "restore", &id, "-t", "retail"]);
    assert_ne!(s.wtf(), played, "restored when run again");
}

/// A backup paused mid-copy (it holds no global lock) blocks no change.
#[test]
fn a_backup_paused_mid_copy_blocks_no_change() {
    let (s, _) = backed_up();
    let argv = ["backup", "create", "-t", "retail"];
    let pause = Pause::new(s.home.env.root());
    let child = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.copied.1=pause"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.copied.1");
    s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
    pause.resume("backup.capture.copied.1");
    let done = wait(child, &argv).expect(0);
    assert!(
        done["data"]["targets"][0]["backup"]["id"].is_string(),
        "{done}"
    );
}

/// Two automatic captures of one client at once (the widget's after-play trigger fired
/// twice): the second finds the scheduling lock held and skips (`in_progress`), the
/// first makes the one backup.
#[test]
fn two_automatic_captures_at_once_make_one_backup() {
    let (s, _) = backed_up();
    s.ok(&["config", "set", "auto_backup", "daily-copy"]);
    let argv = ["backup", "create", "--auto", "-t", "retail"];
    let pause = Pause::new(s.home.env.root());
    let first = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "backup.capture.copied.1=pause"),
            pause.env(),
        ],
    );
    pause.reached("backup.capture.copied.1");
    let second = s.ok(&argv);
    assert_eq!(
        second["data"]["targets"][0]["skipped"]["reason"], "in_progress",
        "{second}"
    );
    pause.resume("backup.capture.copied.1");
    let first = wait(first, &argv).expect(0);
    assert_eq!(
        first["data"]["targets"][0]["backup"]["kind"], "auto",
        "{first}"
    );
    let listed = s.ok(&["backup", "list", "-t", "retail"]);
    let autos = listed["data"]["targets"][0]["backups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["kind"] == "auto")
        .count();
    assert_eq!(autos, 1, "{listed}");
}

/// A prune of two backups another process is reading (one exported, one restored, each
/// paused while it holds its reservation) reports both busy and deletes neither; both
/// readers then finish.
#[test]
fn a_prune_finds_a_paused_export_and_a_paused_restore_busy() {
    let (s, exported) = backed_up();
    let restored = backup(&s);
    play(&s);
    backup(&s); // the newest checkpoint, which prune always keeps
    let usb = s.home.env.root().join("usb");
    fs::create_dir_all(&usb).unwrap();
    let file = usb.join("retail.tar.zst").display().to_string();
    let export = ["backup", "export", exported.as_str(), file.as_str()];
    let restore = ["backup", "restore", restored.as_str(), "-t", "retail"];
    let (pe, pr) = (Pause::new(s.home.env.root()), Pause::new(s.home.env.root()));
    let exporting = s.home.spawn(
        &export,
        &[
            ("OMADDONS_FAILPOINTS", "fsx.write_atomic.after=pause"),
            pe.env(),
        ],
    );
    let restoring = s.home.spawn(
        &restore,
        &[
            ("OMADDONS_FAILPOINTS", "engine.restore.staged=pause"),
            pr.env(),
        ],
    );
    pe.reached("fsx.write_atomic.after");
    pr.reached("engine.restore.staged");
    let pruned = s.ok(&["backup", "prune", &exported, &restored, "-t", "retail"]);
    let mut busy: Vec<&str> = pruned["data"]["targets"][0]["busy"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    busy.sort_unstable();
    let mut expected = [exported.as_str(), restored.as_str()];
    expected.sort_unstable();
    assert_eq!(busy, expected, "{pruned}");
    assert_eq!(
        pruned["data"]["targets"][0]["pruned"],
        serde_json::json!([])
    );
    pe.resume("fsx.write_atomic.after");
    pr.resume("engine.restore.staged");
    wait(exporting, &export).expect(0);
    wait(restoring, &restore).expect(0);
    let listed = s.ok(&["backup", "list", "-t", "retail"]).to_string();
    assert!(listed.contains(&exported) && listed.contains(&restored));
}

/// A verify and a prune of the same backup: the verify first (paused while it holds its
/// reservation), the prune finds it busy; the prune first (paused after moving it out
/// while still holding it), the verify finds it gone. Never a crash, never half a backup.
#[test]
fn a_verify_racing_a_prune_of_its_backup_ends_busy_or_gone() {
    let (s, id) = backed_up();
    backup(&s); // the newest checkpoint
    let verify = ["backup", "verify", id.as_str(), "-t", "retail"];
    let pause = Pause::new(s.home.env.root());
    let verifying = s.home.spawn(
        &verify,
        &[
            ("OMADDONS_FAILPOINTS", "fsx.write_atomic.before=pause"),
            pause.env(),
        ],
    );
    pause.reached("fsx.write_atomic.before");
    let pruned = s.ok(&["backup", "prune", &id, "-t", "retail"]);
    assert_eq!(
        pruned["data"]["targets"][0]["busy"],
        serde_json::json!([{"id": id}]),
        "{pruned}"
    );
    pause.resume("fsx.write_atomic.before");
    let verified = wait(verifying, &verify).expect(0);
    assert_eq!(
        verified["data"]["targets"][0]["backups"][0]["integrity"],
        "verified"
    );

    let prune = ["backup", "prune", id.as_str(), "-t", "retail"];
    let pause = Pause::new(s.home.env.root());
    let pruning = s.home.spawn(
        &prune,
        &[
            ("OMADDONS_FAILPOINTS", "fsx.prune.renamed=pause"),
            pause.env(),
        ],
    );
    pause.reached("fsx.prune.renamed");
    let gone = s.run(&s.args(&["backup", "verify", &id]), "");
    assert_eq!(
        gone.envelope()["error"]["code"],
        "not_found",
        "{}",
        gone.context()
    );
    pause.resume("fsx.prune.renamed");
    wait(pruning, &prune).expect(0);
    let meta = s.home.client("_retail_").join("Interface/.omaddons");
    assert!(!meta.join("backups").join(&id).exists(), "wholly deleted");
    let trash: Vec<_> = fs::read_dir(meta.join("trash"))
        .map(|l| l.collect())
        .unwrap_or_default();
    assert!(trash.is_empty(), "nothing half-deleted left in trash");
}
