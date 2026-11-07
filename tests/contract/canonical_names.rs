//! Canonical names end to end (M4-D55 rule 5): every `auth` spelling answers as
//! `auth github`, and every bare parent, alias and envelope-only name answers under the
//! name the registry gives it. Each run goes through `support::check`, which asserts the
//! canonical name of a parsed argv and an exit its command declares; the steps then
//! assert their own facts.

use serde_json::{Value, json};

use super::*;
use crate::common::netlog::net_log;
use crate::common::tlog::{Ran, TestLog, tlog};

/// Checks the whole contract on `ran`, with `exit`, or with the run's own exit when the
/// scene decides it (then `check` still asserts the command declares it).
#[track_caller]
fn checked(ran: Ran, exit: Option<i32>) -> Ran {
    let code = exit.unwrap_or(ran.run.code);
    support::check(ran.to_run(), code);
    ran
}

/// The envelope's `command` is `name`.
#[track_caller]
fn names(log: &TestLog, ran: &Ran, name: &str) {
    log.assert_eq("command", &ran.envelope()["command"], &json!(name));
}

/// The error's code and fix argv.
#[track_caller]
fn refused_with(log: &TestLog, ran: &Ran, code: &str, fix_argv: &Value) {
    log.assert_eq("error.code", &ran.envelope()["error"]["code"], &json!(code));
    log.assert_eq(
        "error.fix_argv",
        &ran.envelope()["error"]["fix_argv"],
        fix_argv,
    );
}

#[test]
fn auth_answers_as_auth_github_on_every_path() {
    let s = support::Scene::new();
    let log = tlog!();
    let config = s.home.env.config();
    let credentials = config.join("omaddons/credentials/github");
    log.env(&s.home);
    log.tree("config", &config);
    let run = |argv: &[&str], exit: i32| checked(log.run(&s.home, argv), Some(exit));

    log.step("status, offline");
    let first = run(&["auth", "github", "--offline", "--json"], 0);
    names(&log, &first, "auth github");
    let status = first.data().clone();
    for (key, expected) in [
        ("configured", json!(false)),
        ("source", json!("none")),
        ("rate_limit", Value::Null),
        ("dry_run", json!(false)),
    ] {
        log.assert_eq(&format!("data.{key}"), &status[key], &expected);
    }

    log.step("status spelled out");
    let ran = run(&["auth", "github", "status", "--offline", "--json"], 0);
    names(&log, &ran, "auth github");
    log.assert_eq("data", ran.data(), &status);

    log.step("flags before the command");
    let ran = run(&["--offline", "auth", "github", "--json"], 0);
    names(&log, &ran, "auth github");
    log.assert_eq("data", ran.data(), &status);

    log.step("set with nothing to read");
    let ran = run(&["auth", "github", "set", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(&log, &ran, "usage", &Value::Null);
    log.assert_eq(
        "error.fix",
        &ran.envelope()["error"]["fix"],
        &json!("echo \"$TOKEN\" | omaddons auth github set"),
    );

    log.step("clear with nothing stored");
    let ran = run(&["auth", "github", "clear", "--json"], 0);
    names(&log, &ran, "auth github");
    log.assert_eq("data.configured", &ran.data()["configured"], &json!(false));
    log.assert_eq("credentials file", &credentials.exists(), &false);

    let from_gh = json!(["omaddons", "auth", "github", "set", "--from-gh"]);
    log.step("from-gh with status");
    let ran = run(&["auth", "github", "status", "--from-gh", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(&log, &ran, "usage", &from_gh);

    log.step("from-gh before the service");
    let ran = run(&["auth", "--from-gh", "github", "clear", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(&log, &ran, "usage", &from_gh);

    log.step("bare auth");
    let ran = run(&["auth", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(&log, &ran, "usage", &json!(["omaddons", "help", "auth"]));
    let message = ran.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        "error.message has no newline",
        &message.contains('\n'),
        &false,
    );

    log.step("mistyped service");
    let ran = run(&["auth", "gihub", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(&log, &ran, "usage", &json!(["omaddons", "auth", "github"]));

    log.step("mistyped action");
    let ran = run(&["auth", "github", "st", "--json"], 2);
    names(&log, &ran, "auth github");
    refused_with(
        &log,
        &ran,
        "usage",
        &json!(["omaddons", "help", "auth", "github"]),
    );

    log.step("help of github, three spellings");
    let helps: Vec<Value> = [
        &["help", "auth", "github", "--json"][..],
        &["auth", "github", "--help", "--json"],
        &["auth", "github", "set", "--help", "--json"],
    ]
    .into_iter()
    .map(|argv| {
        let ran = run(argv, 0);
        names(&log, &ran, "help");
        log.assert_eq(
            "data.command",
            &ran.data()["command"],
            &json!("auth github"),
        );
        ran.data().clone()
    })
    .collect();
    log.assert_eq("help auth github --help", &helps[1], &helps[0]);
    log.assert_eq("help auth github set --help", &helps[2], &helps[0]);

    log.step("help of the parent");
    let ran = run(&["help", "auth", "--json"], 0);
    names(&log, &ran, "help");
    log.assert_eq("data.command", &ran.data()["command"], &json!("auth"));
    let human = log.run(&s.home, &["help", "auth"]);
    human.ok();
    log.assert_eq(
        "human help auth names github",
        &human.stdout().contains("github"),
        &true,
    );

    log.step("nothing changed, nothing sent");
    log.assert_unchanged("config", &config);
    log.assert_eq("credentials file", &credentials.exists(), &false);
    let sent = net_log(&s.home);
    log.assert_eq("net.log is empty", &sent.is_empty(), &true);
}

#[test]
fn bare_parents_and_envelope_only_names_answer_under_their_canonical_names() {
    let s = support::Scene::new();
    let log = tlog!();
    log.env(&s.home);
    // For diagnosis only: listing commands may write derived caches.
    log.tree("state", &s.home.env.state());
    log.tree("cache", &s.home.env.cache());

    log.step("bare backup is backup list");
    let ran = log
        .cmd(&s.home, &["backup", "--json"])
        .env("OMADDONS_FS_FREE_MB", "1024")
        .run();
    names(&log, &checked(ran, Some(0)), "backup list");

    log.step("a usage error at backup's level names backup");
    let ran = checked(log.run(&s.home, &["backup", "--bogus", "--json"]), Some(2));
    names(&log, &ran, "backup");

    log.step("bare omarchy is omarchy status");
    let empty = s.home.env.root().join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    let ran = log
        .cmd(&s.home, &["omarchy", "--json"])
        .env("PATH", &empty)
        .run();
    names(&log, &checked(ran, None), "omarchy status");

    for (label, argv, name, exit) in [
        (
            "bare targets is targets",
            &["targets", "--json"][..],
            "targets",
            Some(0),
        ),
        (
            "bare config is config",
            &["config", "--json"],
            "config",
            Some(0),
        ),
        (
            "bare errors is errors",
            &["errors", "--json"],
            "errors",
            None,
        ),
        (
            "bare auth names auth github",
            &["auth", "--json"],
            "auth github",
            Some(2),
        ),
        ("bare omaddons is help", &["--json"], "help", Some(0)),
        ("-V is version", &["-V", "--json"], "version", Some(0)),
        (
            "an unknown command is omaddons",
            &["nosuch", "--json"],
            "omaddons",
            Some(2),
        ),
    ] {
        log.step(label);
        let ran = checked(log.run(&s.home, argv), exit);
        names(&log, &ran, name);
    }

    log.step("an alias answers as its command");
    for (alias, name) in [("ls", "list"), ("log", "history")] {
        let ran = checked(log.run(&s.home, &[alias, "--json"]), None);
        names(&log, &ran, name);
    }

    log.json("observed exits", &support::observed());
}
