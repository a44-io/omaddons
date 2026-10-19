//! Concurrency checkpoints (vision §5.12, §7 M1), with real processes synchronized by
//! `sleep` failpoints and generous margins; assertions are on final state, never on
//! interleavings: a reader during a live swap, recovery only under the lock, concurrent
//! installs of the same and of different addons, and the lock-hold budget.

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::archives::zip_text;
use common::engine::{Scene, shapes, warning_codes};
use common::home::{Run, wait};
use serde_json::Value;
use support::{poll, status};

#[cfg(test)]
mod support {
    use super::*;

    /// Waits until `ready` holds (10 s at most).
    pub fn poll(what: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

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

    /// The retail client's account-wide SavedVariables directory.
    pub fn account_saved_variables(s: &Scene) -> PathBuf {
        s.home
            .client("_retail_")
            .join("WTF/Account/ACCT/SavedVariables")
    }

    /// 16 MiB of Foo's SavedVariables, as a heavy profile holds them: the account's
    /// `Foo.lua` and `Foo.lua.bak` (4 MiB each) and four characters' (1 MiB each).
    pub fn heavy_saved_variables(s: &Scene) -> Vec<PathBuf> {
        let account = account_saved_variables(s);
        let realm = account.parent().unwrap().join("Realm");
        let mut dirs = vec![(account, 4usize << 20)];
        dirs.extend((1..=4).map(|n| (realm.join(format!("Char{n}/SavedVariables")), 1 << 20)));
        let line = "FooDB = { [\"profile\"] = { [\"bars\"] = { 1, 2, 3 } } }\n";
        let mut files = Vec::new();
        for (dir, size) in dirs {
            fs::create_dir_all(&dir).unwrap();
            let text = line.repeat(size / line.len() + 1);
            for name in ["Foo.lua", "Foo.lua.bak"] {
                fs::write(dir.join(name), &text.as_bytes()[..size]).unwrap();
                files.push(dir.join(name));
            }
        }
        files
    }

    /// The lowercase hex SHA-256 of `bytes`.
    pub fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// How long a `-v` run held the global lock, from its `lock released` log line.
    pub fn lock_held_ms(r: &Run) -> Option<u64> {
        r.stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["message"].as_str().map(str::to_owned))
            .find_map(|m| {
                m.strip_prefix("lock released: ")
                    .and_then(|rest| rest.rsplit("lock_held_ms ").next())
                    .and_then(|n| n.trim().parse::<u64>().ok())
            })
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
    let child = s.home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.swapped=sleep:2000")],
    );
    // Swapped and committing nothing yet: the state holds still for two seconds.
    poll("the swap", || support::swapped(&s));
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

    // Another command holds the lock (asleep right after taking it, before recovering).
    let argv = [
        "install",
        "gh:o/bar",
        "--file",
        &s.zip("bar"),
        "-t",
        "retail",
        "--offline",
    ];
    let child = s.home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.lock.acquired=sleep:2000")],
    );
    poll("the lock", || {
        status(&s)["data"]["journal"]["in_progress"] == true
    });
    let envelope = status(&s);
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

/// Runs two installs at once, both pausing after staging so they overlap.
fn concurrently(s: &Scene, a: (&str, &str), b: (&str, &str)) -> (Run, Run) {
    let sleep = [("OMADDONS_FAILPOINTS", "engine.stage.synced=sleep:500")];
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
    let first = s.home.spawn(&argv_a, &sleep);
    let second = s.home.spawn(&argv_b, &sleep);
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
    let inflight = s.home.planning_locks();
    let planning = || fs::read_dir(&inflight).is_ok_and(|mut d| d.next().is_some());
    assert!(!planning(), "nothing in flight yet");

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
    let a = s.home.spawn(
        &argv_a,
        &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:2000")],
    );
    poll("A to start planning", planning);
    std::thread::sleep(Duration::from_millis(500));
    // B pins v3 meanwhile, to completion.
    let mut argv_b = s.install_args("gh:o/foo", "v3");
    argv_b.extend(["--version".to_owned(), "3".to_owned()]);
    let b = s.run(&argv_b, "").expect(0);
    assert_eq!(b["data"]["items"][0]["status"], "updated", "{b}");

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
enum Sleeper {
    /// The pin, once planned; the install runs to completion meanwhile.
    Pin,
    /// The install, once staged; the pin runs to completion meanwhile.
    Install,
}

/// Pins Foo while Bar is installed on the same target, `sleeper` paused in the middle,
/// and checks the final state only: both commit, neither goes stale.
fn pin_races_an_install(sleeper: Sleeper) {
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
    let inflight = s.home.planning_locks();
    let stage = s.home.client("_retail_").join("Interface/.omaddons/stage");
    let busy = |dir: &std::path::Path| fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some());
    let (pinned, installed) = match sleeper {
        Sleeper::Pin => {
            let child = s.home.spawn(
                &pin,
                &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:1500")],
            );
            poll("the pin to plan", || busy(&inflight));
            let installed = s.home.run(&install);
            (wait(child, &pin), installed)
        }
        Sleeper::Install => {
            let child = s.home.spawn(
                &install,
                &[("OMADDONS_FAILPOINTS", "engine.stage.synced=sleep:1500")],
            );
            poll("the install to stage", || busy(&stage));
            let pinned = s.home.run(&pin);
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
        assert!(tx.is_string(), "{sleeper:?}: both commit\n{}", r.context());
    }
    let now = s.record("foo");
    assert_eq!(
        (&now["pinned"], &now["release_id"]),
        (&Value::Bool(true), &before["release_id"]),
        "{sleeper:?}: {now}"
    );
    assert!(!s.record("bar").is_null() && s.addons().join("Bar").is_dir());
    assert_eq!(s.generation(), generation + 2, "{sleeper:?}");
    let mut added = s.history().split_off(history.len());
    added.sort();
    assert_eq!(added, ["install", "pin"], "{sleeper:?}");
    assert_eq!(s.folders(), folders, "{sleeper:?}: Foo's folders untouched");
    assert!(s.journals().is_empty(), "{sleeper:?}");
}

#[test]
fn a_pin_and_an_install_of_another_addon_both_commit_either_way() {
    pin_races_an_install(Sleeper::Pin);
    pin_races_an_install(Sleeper::Install);
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
    let inflight = s.home.planning_locks();
    let child = s.home.spawn(
        &update[..update.len() - 1],
        &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:1500")],
    );
    poll("the update to plan", || {
        fs::read_dir(&inflight).is_ok_and(|mut d| d.next().is_some())
    });
    let install = s.install("gh:o/bar", "bar");
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
    let inflight = s.home.planning_locks();
    let child = s.home.spawn(
        &adopt[..adopt.len() - 1],
        &[("OMADDONS_FAILPOINTS", "engine.plan.done=sleep:1500")],
    );
    poll("the adoption to plan", || {
        fs::read_dir(&inflight).is_ok_and(|mut d| d.next().is_some())
    });
    // The install replaces Echo's files (it is unmanaged until the adoption commits).
    let installed = s
        .home
        .run(&["install", "gh:o/echo", "-t", "retail", "--force"]);
    installed.expect(0);
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
    assert_eq!(memo["schema_version"], "1.0", "{memo}");
    let (_, problem) = omaddons::state::memo::load(&common::http::paths(&s.home.env));
    assert!(problem.is_none(), "{problem:?}");
}

#[test]
fn multi_addon_commits_hold_the_lock_under_50_ms() {
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
        let ms = support::lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what.to_owned(), ms)));
    }
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("lock_held_ms of multi-addon commits: {held:?}\n"),
    );
    assert!(
        held.iter().all(|(_, ms)| *ms < 50),
        "lock hold times {held:?}"
    );
}

#[test]
fn every_commit_holds_the_lock_under_50_ms() {
    let s = Scene::new();
    let mut held: Vec<u64> = Vec::new();
    for i in 0..20 {
        let zip = if i % 2 == 0 { "v1" } else { "v2" };
        let mut args = s.install_args("gh:o/foo", zip);
        args.push("-v".to_owned());
        let r = s.run(&args, "");
        assert_eq!(r.code, 0, "{}", r.context());
        let ms = support::lock_held_ms(&r);
        assert!(ms.is_some(), "no lock log line\n{}", r.context());
        held.extend(ms);
    }
    held.sort_unstable();
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!(
            "lock_held_ms over 20 commits: min {} median {} max {}\n",
            held[0], held[10], held[19]
        ),
    );
    assert!(held.iter().all(|ms| *ms < 50), "lock hold times {held:?}");
}

#[test]
fn large_saved_variables_hold_the_lock_under_50_ms() {
    let s = Scene::new();
    support::heavy_saved_variables(&s);
    let mut held: Vec<(&str, u64)> = Vec::new();
    let mut run = |what: &'static str, mut args: Vec<String>| {
        args.push("-v".to_owned());
        // Byte copies even where the filesystem could reflink: the slow path.
        let r = s.run(&args, "fsx.reflink.unsupported");
        assert_eq!(r.code, 0, "{what}\n{}", r.context());
        let ms = support::lock_held_ms(&r);
        assert!(ms.is_some(), "{what}: no lock log line\n{}", r.context());
        held.extend(ms.map(|ms| (what, ms)));
    };
    run("install", s.install_args("gh:o/foo", "v1"));
    run("update", s.install_args("gh:o/foo", "v2"));
    run("remove", s.args(&["remove", "Foo"]));
    let _ = std::io::Write::write_fmt(
        &mut std::io::stderr(),
        format_args!("lock_held_ms with 16 MiB of SavedVariables: {held:?}\n"),
    );
    assert!(
        held.iter().all(|(_, ms)| *ms < 50),
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
    assert_eq!(copies, 3, "every transaction snapshotted them");
}

#[test]
fn saved_variables_written_after_their_copy_are_snapshotted_as_written() {
    let s = Scene::new();
    s.install("gh:o/foo", "v1");
    let sv = support::account_saved_variables(&s);
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
    let child = s.home.spawn(
        &argv,
        &[(
            "OMADDONS_FAILPOINTS",
            "engine.snapshot.prestaged=sleep:1500",
        )],
    );
    let snapshots = s
        .home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots");
    let copied = || {
        fs::read_dir(&snapshots).into_iter().flatten().any(|e| {
            let dir = e.unwrap().path();
            dir.join("WTF/Account/ACCT/SavedVariables/Foo.lua")
                .is_file()
        })
    };
    poll("the copy made before the lock", copied);
    std::thread::sleep(Duration::from_millis(200));
    let logout = "-- written on logout, after the copy\n";
    fs::write(sv.join("Foo.lua"), logout).unwrap();

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
