//! The cross-cutting cases: `locked`, a panic, `--offline`, CI/TERM, format spellings,
//! determinism and human-mode failures.

use super::*;
use crate::support::check;
use common::home::wait;

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
    // The holder stops inside its commit, the lock held, until resumed.
    let pause = common::pause::Pause::new(s.home.env.root());
    let holder = s.home.spawn(
        &argv,
        &[("OMADDONS_FAILPOINTS", "engine.swapped=pause"), pause.env()],
    );
    pause.reached("engine.swapped");
    let c = s.json(&["remove", "Foo", "-t", "retail"], 5);
    assert_eq!(c.envelope["error"]["code"], "locked");
    assert_eq!(c.envelope["error"]["retryable"], true);
    // The fix reruns the same command once the holder is done.
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "remove", "Foo", "-t", "retail"])
    );
    pause.resume("engine.swapped");
    let done = wait(holder, &argv);
    assert_eq!(done.code, 0, "{}", done.context());
}

/// One internal envelope, whose fix never reruns the command that failed: `status`
/// points to `status --cached`, which in turn leaves only a report.
#[test]
fn a_panic_is_one_internal_envelope() {
    let s = Scene::new();
    let panic = [("OMADDONS_FAILPOINTS", "cli.panic=panic")];
    let error = |args: &[&str]| s.json_env(args, &panic, 1).envelope["error"].clone();
    let status = error(&["status"]);
    assert_eq!(status["code"], "internal");
    assert_eq!(
        status["fix_argv"],
        serde_json::json!(["omaddons", "status", "--cached"])
    );
    let cached = error(&["status", "--cached"]);
    assert_eq!(cached["fix_argv"], Value::Null);
    assert!(
        cached["fix"]
            .as_str()
            .is_some_and(|f| f.starts_with("this is a bug in omaddons")),
        "{cached}"
    );
    assert_eq!(
        error(&["list"])["fix_argv"],
        serde_json::json!(["omaddons", "status"])
    );
}

/// A panic after a transaction committed is one internal envelope, whose commands offer
/// the undo of what stands, and that undo works.
#[test]
fn a_panic_after_commit_offers_undo() {
    let s = Scene::new();
    let zip = s.zip("foo");
    let c = s.json_env(
        &["install", "gh:o/foo", "--file", &zip, "-t", "retail"],
        &[("OMADDONS_FAILPOINTS", "engine.retention=panic")],
        1,
    );
    assert_eq!(c.envelope["error"]["code"], "internal", "{}", c.envelope);
    let undo = &c.envelope["commands"][0];
    assert_eq!(undo["action"], "undo", "{}", c.envelope);
    let argv: Vec<&str> = undo["argv"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(argv[..2], ["omaddons", "undo"]);
    let undone = s.json(&argv[1..], 0);
    assert_eq!(
        undone.envelope["data"]["reverted"]["kind"], "install",
        "{}",
        undone.envelope
    );
}

/// A panic the program recovers from (the credential command's reader thread) prints
/// nothing: the run's own envelope is the only document.
#[test]
fn a_recovered_panic_prints_one_envelope() {
    let s = Scene::new();
    let cred = s.home.env.root().join("cred");
    crate::common::exec::write_executable(
        &cred,
        "#!/bin/sh\necho ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\n",
    );
    fs::create_dir_all(s.home.env.config().join("omaddons")).unwrap();
    fs::write(
        s.home.env.config().join("omaddons/config.toml"),
        format!(
            "[sources.github]\ncredential = {{ command = [\"{}\"] }}\n",
            cred.display()
        ),
    )
    .unwrap();
    let argv = ["auth", "github", "--json"].map(str::to_owned);
    let run = s.home.exec_with(
        &argv,
        &[("OMADDONS_FAILPOINTS", "config.credential.reader=panic")],
    );
    let docs: Vec<Value> = serde_json::Deserializer::from_str(&run.stdout)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(docs.len(), 1, "{}", run.context());
    assert_ne!(docs[0]["error"]["code"], "internal", "{}", run.context());
}

/// A missing or relative HOME is the environment's fault: `usage` (exit 2), with a fix
/// no omaddons command can carry out. `help` needs no HOME.
#[test]
fn a_home_that_is_unset_or_relative_is_a_usage_error() {
    let s = Scene::new();
    let run = |home: Option<&str>, args: &[&str]| {
        let mut cmd = s.home.env.command();
        cmd.args(args).arg("--json");
        match home {
            Some(home) => cmd.env("HOME", home),
            None => cmd.env_remove("HOME"),
        };
        let out = cmd.output().unwrap();
        common::home::Run {
            argv: args.iter().map(|a| (*a).to_owned()).collect(),
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    };
    for (home, args) in [
        (None, &["status"][..]),
        (Some("relative/home"), &["config", "path"][..]),
        (None, &["list"][..]),
    ] {
        let c = check(run(home, args), 2);
        let error = &c.envelope["error"];
        assert_eq!(
            (&error["code"], &error["fix_argv"]),
            (&serde_json::json!("usage"), &Value::Null),
            "{args:?}: {error}"
        );
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("HOME is not set to an absolute path")),
            "{error}"
        );
    }
    check(run(None, &["help"]), 0);
}

#[test]
fn offline_refuses_the_network() {
    let s = Scene::new();
    let c = s.json(&["install", "gh:o/foo", "-t", "retail", "--offline"], 1);
    assert_eq!(c.envelope["error"]["code"], "offline");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "install", "gh:o/foo", "-t", "retail"]),
        "the same install, online"
    );
}

#[test]
fn a_usage_error_reports_an_offline_run_as_offline() {
    let s = Scene::new();
    let offline = |args: &[&str], env: &[(&str, &str)]| {
        let c = s.json_env(args, env, 2);
        assert_eq!(c.envelope["error"]["code"], "usage", "{args:?}");
        c.envelope["meta"]["offline"].clone()
    };
    assert_eq!(offline(&["nosuch"], &[("OMADDONS_OFFLINE", "1")]), true);
    assert_eq!(offline(&["nosuch", "--offline"], &[]), true);
    assert_eq!(offline(&["status", "--frobnicate", "--offline"], &[]), true);
    assert_eq!(offline(&["nosuch"], &[("OMADDONS_OFFLINE", "yes")]), false);
    assert_eq!(offline(&["nosuch"], &[]), false);
}

#[test]
fn an_http_ref_is_fixed_to_https_keeping_every_other_argument() {
    let s = Scene::new();
    let c = s.json(&["install", "http://github.com/o/foo", "-t", "retail"], 2);
    assert_eq!(c.envelope["error"]["code"], "ref_invalid");
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!([
            "omaddons",
            "install",
            "https://github.com/o/foo",
            "-t",
            "retail"
        ])
    );
    let c = s.json(&["info", "http://github.com/o/foo"], 2);
    assert_eq!(
        c.envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "info", "https://github.com/o/foo"]),
        "the command that ran, not install"
    );
}

/// Every entry under `dir` with its bytes (a directory's are empty), but `net.log`,
/// which records each request whatever the run (and the `omaddons/` dir it may create).
fn state_tree(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = path.display().to_string();
            if name.contains("net.log") {
                continue;
            }
            if path == dir.join("omaddons") {
                todo.push(path);
            } else if path.is_dir() {
                todo.push(path);
                out.push((name, Vec::new()));
            } else {
                out.push((name, fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_dry_run_leaves_the_state_directory_as_it_was() {
    let s = Scene::new();
    let state = s.home.env.state();
    let (foo, foo2) = (s.zip("foo"), s.zip("foo2"));
    let dry_runs = |runs: &[&[&str]]| {
        let before = state_tree(&state);
        for args in runs {
            let r = s.home.run(args);
            assert_eq!(r.code, 0, "{args:?}\n{}", r.context());
            assert_eq!(
                state_tree(&state)
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>(),
                before.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "{args:?}"
            );
            assert!(state_tree(&state) == before, "{args:?}: a file changed");
        }
    };
    // A fresh home: nothing registered, nothing installed.
    dry_runs(&[
        &[
            "install",
            "gh:o/foo",
            "--file",
            &foo,
            "-t",
            "retail",
            "--dry-run",
        ],
        &["install", "gh:o/foo", "-t", "retail", "--dry-run"],
        &["install", "weakauras", "-t", "classic", "--dry-run"],
        &["targets", "default", "retail", "--dry-run"],
        &["backup", "create", "--dry-run"],
        &["backup", "prune", "--dry-run"],
    ]);
    // With an install to change, remove, update and undo.
    let tx = s.install_foo();
    s.home
        .write_desired("[family.mainline]\naddons = [\"gh:o/foo\"]\n");
    // A bound dry run compares its token, and writes nothing either: an install's, and
    // the no-op tokens of an update and a sync with nothing to do.
    let bound = |preview: &[&str]| -> Vec<String> {
        let token = s.json(preview, 0).envelope["data"]["precondition"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut argv: Vec<String> = preview.iter().map(|a| (*a).to_owned()).collect();
        argv.extend(["--if-match".to_owned(), token]);
        argv
    };
    let install = [
        "install",
        "gh:o/foo",
        "--file",
        foo2.as_str(),
        "-t",
        "retail",
        "--dry-run",
    ];
    let update = ["update", "--all", "--dry-run"];
    let sync = ["sync", "--dry-run"];
    let owned = [bound(&install), bound(&update), bound(&sync)];
    let [bound_install, bound_update, bound_sync] = owned
        .each_ref()
        .map(|argv| argv.iter().map(String::as_str).collect::<Vec<_>>());
    dry_runs(&[
        &install,
        &bound_install,
        &["remove", "Foo", "-t", "retail", "--dry-run"],
        &update,
        &bound_update,
        &sync,
        &bound_sync,
        &["undo", &tx, "--dry-run"],
        &["undo", "--dry-run"],
        &["pin", "Foo", "-t", "retail", "--dry-run"],
    ]);
}

/// Every command that changes nothing accepts `--dry-run` and `--reason` and ignores
/// them: its envelope is the same with and without them (`meta` aside), and the flagged
/// run writes nothing to state. A run first warms what the command derives anyway (the
/// release memo, the cache), so the two compared runs see the same state.
#[test]
fn read_only_commands_ignore_dry_run_and_reason() {
    let s = Scene::new();
    s.install_foo();
    let state = s.home.env.state();
    let invocation = |name: &str| -> Option<&[&str]> {
        Some(match name {
            "status" => &["status"],
            "list" => &["list"],
            "outdated" => &["outdated"],
            "info" => &["info", "Foo"],
            "search" => &["search", "foo"],
            "errors" => &["errors"],
            "history" => &["history"],
            "net-log" => &["net-log"],
            "targets" => &["targets"],
            "config" => &["config"],
            "config get" => &["config", "get", "default_channel"],
            "config path" => &["config", "path"],
            "backup" => &["backup"],
            "backup list" => &["backup", "list"],
            "omarchy" => &["omarchy"],
            "omarchy status" => &["omarchy", "status"],
            "completions" => &["completions", "bash"],
            "help" => &["help"],
            _ => return None,
        })
    };
    // The omarchy commands run with no Omarchy tool reachable: never this machine's
    // shell.
    let no_tools = s.home.env.root().join("no-tools").display().to_string();
    let run = |name: &str, args: &[&str]| {
        let mut argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        argv.push("--json".to_owned());
        let env: &[(&str, &str)] = if name.starts_with("omarchy") {
            &[("PATH", no_tools.as_str())]
        } else {
            &[]
        };
        s.home.exec_with(&argv, env)
    };
    let read_only: Vec<&str> = omaddons::app::COMMANDS
        .iter()
        .filter(|c| !c.mutates)
        .map(|c| c.name)
        .collect();
    assert!(read_only.len() >= 12, "{read_only:?}");
    for name in read_only {
        let args = invocation(name);
        assert!(args.is_some(), "add a read-only invocation of {name} here");
        let args = args.unwrap_or_default();
        let plain = run(name, args);
        let code = plain.code;
        let plain = run(name, args);
        assert_eq!(plain.code, code, "{args:?}\n{}", plain.context());
        let before = state_tree(&state);
        let mut flagged_args = args.to_vec();
        flagged_args.extend(["--dry-run", "--reason", "x"]);
        let flagged = run(name, &flagged_args);
        assert_eq!(
            flagged.code,
            code,
            "{flagged_args:?}\n{}",
            flagged.context()
        );
        assert_eq!(
            support::without_meta(&flagged.stdout),
            support::without_meta(&plain.stdout),
            "{name}: the flags changed the envelope"
        );
        assert!(
            state_tree(&state) == before,
            "{name}: --dry-run --reason wrote to state"
        );
    }
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

/// A JSON spelling clap refuses still answers with one envelope, whose fix is the
/// command without the refused flag; values are read ignoring case.
#[test]
fn a_refused_json_spelling_still_prints_one_envelope() {
    let s = Scene::new();
    for args in [
        &["status", "--json=1"][..],
        &["status", "--json=true"],
        &["status", "--robot=1"],
        &["--robot=yes", "status"],
        &["status", "--format", "--json"],
        &["status", "--progress", "--json"],
    ] {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let checked = support::check(s.home.exec_with(&argv, &[]), 2);
        assert_eq!(
            checked.envelope["error"]["fix_argv"],
            serde_json::json!(["omaddons", "status"]),
            "{}",
            checked.run.context()
        );
    }
    for args in [
        &["status", "--format", "JSON"][..],
        &["status", "--progress", "JSON", "--json"],
    ] {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        support::check(s.home.exec_with(&argv, &[]), 0);
    }
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
        (&["completions", "bash"][..], 0),
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

/// The M2 outputs, populated (updates, drift, an adoptable folder, an orphan), are the
/// same bytes twice but for `meta`.
#[test]
fn m2_outputs_are_byte_identical() {
    let s = Scene::new();
    s.dependencies();
    s.json(&["install", "littlewigs", "-t", "retail"], 0);
    s.json(&["remove", "LittleWigs", "-t", "retail"], 0);
    s.json(&["install", "weakauras", "-t", "retail"], 0);
    let both = "## Interface: 120105, 50504\n## Version: 4\n";
    let wa: [Asset<'_>; 1] = [(
        "WeakAuras.zip",
        &[("mainline", 120_105), ("mists", 50_504)],
        vec![("WeakAuras/WeakAuras.toc", both)],
    )];
    let base = "https://github.com/WeakAuras/WeakAuras2";
    common::http::gh_release_v(&s.home.http, base, "5.20.0", &wa);
    common::http::gh_latest(&s.home.http, base, "5.20.0");
    s.json(&["outdated", "--refresh"], 0);
    // Drift, and an unmanaged folder the catalog names.
    let toc = s.home.addons("_retail_").join("WeakAuras/WeakAuras.toc");
    fs::write(&toc, "## Interface: 120105\n## Title: edited by hand\n").unwrap();
    fs::create_dir_all(s.home.addons("_retail_").join("Masque")).unwrap();
    fs::write(
        s.home.addons("_retail_").join("Masque/Masque.toc"),
        "## Interface: 120105\n",
    )
    .unwrap();
    let status = s.json(&["status"], 0);
    let t = status.envelope["data"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "retail")
        .unwrap();
    assert!(
        t["updates"]["available"].as_u64() > Some(0)
            && t["adoptable"].as_u64() > Some(0)
            && t["orphans"].as_u64() > Some(0),
        "{t:#}"
    );
    for args in [
        &["outdated"][..],
        &["status"][..],
        &["status", "--deep"][..],
        &["export"][..],
        &["search", "wigs", "--offline"][..],
    ] {
        let a = s.json(args, 0);
        let b = s.json(args, 0);
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

/// Every warning M2 added, and `missing_dependency` (first emitted in M2), is raised once
/// through the harness, which checks its fix: an argv that runs, or a text fix.
#[test]
fn every_m2_warning_carries_a_fix() {
    use std::collections::BTreeSet;

    let s = Scene::new();
    s.dependencies();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut run = |args: &[&str], code: i32| {
        let c = s.json(args, code);
        for w in c.envelope["warnings"].as_array().into_iter().flatten() {
            seen.insert(w["code"].as_str().unwrap_or_default().to_owned());
        }
    };

    // Dependencies: one nothing ships, a bridge, still required, then an orphan.
    run(&["install", "littlewigs", "-t", "retail"], 0);
    run(&["install", "gh:o/plug", "-t", "retail"], 0);
    run(&["remove", "BigWigs", "-t", "retail", "--dry-run"], 0);
    run(&["remove", "LittleWigs", "-t", "retail"], 0);

    // A pinned addon asked for again; then its files changed by hand and reinstalled.
    run(&["install", "weakauras", "-t", "retail"], 0);
    run(&["pin", "weakauras", "-t", "retail"], 0);
    run(&["install", "weakauras", "-t", "retail"], 0);
    let toc = s.home.addons("_retail_").join("WeakAuras/WeakAuras.toc");
    fs::write(&toc, "## Interface: 120105\n## Title: edited\n").unwrap();
    run(&["install", "weakauras", "-t", "retail", "--reinstall"], 0);

    // An answer from the memo while offline.
    run(&["outdated"], 0);
    run(&["outdated", "--offline"], 0);

    // An adopted folder whose catalog name is taken.
    s.install_foo();
    fs::create_dir_all(s.home.addons("_retail_").join("FooTwo")).unwrap();
    fs::write(
        s.home.addons("_retail_").join("FooTwo/FooTwo.toc"),
        "## Interface: 120105\n## Version: 2\n",
    )
    .unwrap();
    run(&["adopt", "FooTwo", "--no-verify"], 0);

    // A desired-state file with an unknown key, an unknown client and an unlocked entry.
    s.home.write_desired(
        "[family.mainline]\naddons = [\"littlewigs\"]\nadons = []\n\n[target.\"elsewhere\"]\naddons = [\"x\"]\n",
    );
    run(&["sync", "--locked", "--dry-run"], 0);

    // An export with a local install, and a client whose directory is gone.
    let both = s.zip("both");
    run(&["install", &format!("file:{both}"), "-t", "classic"], 0);
    fs::rename(
        s.home.client("_classic_"),
        s.home.env.root().join("unplugged"),
    )
    .unwrap();
    run(&["export"], 0);

    let expected = [
        "addon_pinned",
        "still_required",
        "orphaned_dependency",
        "bridge_dependency",
        "local_changes_replaced",
        "stale_check",
        "adopt_name_changed",
        "export_missing_target",
        "export_local_source",
        "desired_unknown_key",
        "desired_unknown_target",
        "not_locked",
        "missing_dependency",
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|c| !seen.contains(**c))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "never raised: {missing:?}; saw {seen:?}"
    );
}
