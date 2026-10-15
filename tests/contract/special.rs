//! The cross-cutting cases: `locked`, a panic, `--offline`, CI/TERM, format spellings,
//! determinism and human-mode failures.

use super::*;
use common::home::wait;
use std::time::{Duration, Instant};

/// Arguments and extra environment of one way to ask for JSON.
type Spelling<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

#[test]
fn a_held_lock_is_locked_and_retryable() {
    let s = Scene::new();
    s.install_foo();
    let zip = s.zip("foo2");
    let argv = [
        "install",
        "gh:o/foo",
        "--file",
        zip.as_str(),
        "-t",
        "retail",
    ];
    let holder = s.home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.swapped=sleep:8000")],
    );
    let journal = s.home.state().join("journal");
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_dir(&journal).map(|l| l.count()).unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "the holder never journaled");
        std::thread::sleep(Duration::from_millis(20));
    }
    let c = s.json(&["remove", "Foo", "-t", "retail"], 5);
    assert_eq!(c.envelope["error"]["code"], "locked");
    assert_eq!(c.envelope["error"]["retryable"], true);
    let done = wait(holder, &argv);
    assert_eq!(done.code, 0, "{}", done.context());
}

#[test]
fn a_panic_is_one_internal_envelope() {
    let s = Scene::new();
    let c = s.json_env(
        &["status"],
        &[("OMADDONS_FAILPOINTS", "cli.panic=panic")],
        1,
    );
    assert_eq!(c.envelope["error"]["code"], "internal");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "status"])
    );
}

#[test]
fn offline_refuses_the_network() {
    let s = Scene::new();
    let c = s.json(&["install", "gh:o/foo", "-t", "retail", "--offline"], 1);
    assert_eq!(c.envelope["error"]["code"], "offline");
}

#[test]
fn ci_and_dumb_terminals_get_ndjson_stderr() {
    let s = Scene::new();
    for env in [("CI", "1"), ("TERM", "dumb")] {
        let c = s.json_env(&["info", "nosuchaddon"], &[env], 3);
        assert_eq!(
            c.run.stderr.lines().count(),
            1,
            "one error event: {}",
            c.run.context()
        );
    }
}

#[test]
fn every_json_spelling_prints_the_same_envelope() {
    let s = Scene::new();
    s.install_foo();
    let spellings: [Spelling<'_>; 4] = [
        (&["list", "--json"], &[]),
        (&["list", "--format", "json"], &[]),
        (&["list", "--robot"], &[]),
        (&["list"], &[("OMADDONS_FORMAT", "json")]),
    ];
    let outputs: Vec<String> = spellings
        .iter()
        .map(|(args, env)| {
            let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let run = s.home.exec_with(&argv, env);
            let checked = support::check(run, 0);
            support::without_meta(&checked.run.stdout)
        })
        .collect();
    assert!(outputs.windows(2).all(|w| w[0] == w[1]), "{outputs:#?}");
}

#[test]
fn repeated_runs_are_byte_identical() {
    let s = Scene::new();
    s.install_foo();
    for (args, code) in [
        (&["list"][..], 0),
        (&["status"][..], 0),
        (&["history"][..], 0),
        (&["info", "nosuchaddon"][..], 3),
        (&["remove", "Nope", "-t", "retail"][..], 3),
    ] {
        let a = s.json(args, code);
        let b = s.json(args, code);
        assert_eq!(
            support::without_meta(&a.run.stdout),
            support::without_meta(&b.run.stdout),
            "{args:?}"
        );
    }
}

#[test]
fn human_failures_leave_stdout_empty() {
    let s = Scene::new();
    for args in [
        &["info", "nosuchaddon"][..],
        &["remove", "Nope", "-t", "retail"][..],
        &["history", "--since", "nonsense"][..],
        &["undo"][..],
    ] {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let run = s.home.exec(&argv);
        assert_ne!(run.code, 0, "{}", run.context());
        assert!(run.stdout.is_empty(), "{}", run.context());
        assert!(!run.stderr.is_empty(), "{}", run.context());
    }
}
