//! Concurrency checkpoints (vision §5.12, §7 M1), with real processes synchronized by
//! `sleep` failpoints and generous margins; assertions are on final state, never on
//! interleavings: a reader during a live swap, recovery only under the lock, concurrent
//! installs of the same and of different addons, and the lock-hold budget.

mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::engine::{Scene, warning_codes};
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
    s.install("gh:o/baz", "bar"); // registers the target first
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
fn every_commit_holds_the_lock_under_50_ms() {
    let s = Scene::new();
    let mut held: Vec<u64> = Vec::new();
    for i in 0..20 {
        let zip = if i % 2 == 0 { "v1" } else { "v2" };
        let mut args = s.install_args("gh:o/foo", zip);
        args.push("-v".to_owned());
        let r = s.run(&args, "");
        assert_eq!(r.code, 0, "{}", r.context());
        let ms = r
            .stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["message"].as_str().map(str::to_owned))
            .find_map(|m| {
                m.strip_prefix("lock released: ")
                    .and_then(|rest| rest.rsplit("lock_held_ms ").next())
                    .and_then(|n| n.trim().parse::<u64>().ok())
            });
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
