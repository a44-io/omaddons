//! e2e suite: every storable command's applied plan reconciles from history: after apply, plans show says completed with exactly the transactions apply reported and history lists, except backup prune, which records no history line and stays unknown.
#![cfg(test)]

mod common;

use std::collections::BTreeMap;
use std::fs;

use clap::Parser as _;
use serde_json::{Value, json};

use common::archives::zip_text;
use common::home::Home;
use common::http::{Asset, gh_release};
use common::plans::{committed, evidence_txs, history_txs_for, plan_id, show, show_human};
use common::tlog::{Ran, TestLog, tlog};
use common::wtf::Wtf;
use omaddons::cli::Cli;

const FOO: &str = "https://github.com/o/foo";

/// Foo's flavors in its `release.json`: retail's, Classic Era's and Anniversary's.
const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509), ("bcc", 20_505)];

/// Foo `v1` and `v2`: one folder whose TOC every client of these homes loads, naming its
/// repository so that `adopt` identifies it.
const FOO_V1: &[(&str, &str)] = &[
    (
        "Foo/Foo.toc",
        "## Interface: 120105, 11509, 20505\n## Title: Foo\n## Version: v1\n## X-Website: https://github.com/o/foo\nFoo.lua\n",
    ),
    ("Foo/Foo.lua", "local version = 1\n"),
];
const FOO_V2: &[(&str, &str)] = &[
    (
        "Foo/Foo.toc",
        "## Interface: 120105, 11509, 20505\n## Title: Foo\n## Version: v2\n## X-Website: https://github.com/o/foo\nFoo.lua\n",
    ),
    ("Foo/Foo.lua", "local version = 2\n"),
];

fn foo(entries: &'static [(&'static str, &'static str)]) -> [Asset<'static>; 1] {
    [("Foo.zip", FLAVORS, entries.to_vec())]
}

/// What `plans show` must say after the case's `apply`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// `completed`, with exactly `apply`'s committed transactions as its evidence.
    Completed,
    /// `unknown`: the command appends no history line (`backup prune`).
    UnknownNoHistory,
}

/// A case's home, and the values its preview argv names by placeholder (`{zip}`, `{tx}`,
/// `{backup}`).
struct Built {
    home: Home,
    fill: BTreeMap<&'static str, String>,
}

impl Built {
    fn new(home: Home, fill: &[(&'static str, String)]) -> Self {
        Self {
            home,
            fill: fill.iter().cloned().collect(),
        }
    }
}

/// One storable command: its path, the home it needs, the change it previews (without
/// `--dry-run`) and what `plans show` says once that preview is applied.
struct Case {
    name: &'static str,
    command: &'static str,
    build: fn(&TestLog) -> Built,
    preview: &'static [&'static str],
    expect: Expect,
}

const CASES: &[Case] = &[
    Case {
        name: "install",
        command: "install",
        build: zip_on_retail,
        preview: &["install", "gh:o/foo", "--file", "{zip}", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "update",
        command: "update",
        build: behind_on_retail,
        preview: &["update"],
        expect: Expect::Completed,
    },
    Case {
        name: "pin",
        command: "pin",
        build: installed_on_retail,
        preview: &["pin", "foo", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "unpin",
        command: "unpin",
        build: pinned_on_retail,
        preview: &["unpin", "foo", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "adopt",
        command: "adopt",
        build: unmanaged_on_retail,
        preview: &["adopt", "--apply", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "sync",
        command: "sync",
        build: desired_on_retail,
        preview: &["sync"],
        expect: Expect::Completed,
    },
    Case {
        name: "copy",
        command: "copy",
        build: installed_on_era,
        preview: &["copy", "--from", "era", "--to", "anniversary"],
        expect: Expect::Completed,
    },
    Case {
        name: "remove",
        command: "remove",
        build: installed_on_retail,
        preview: &["remove", "foo", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "undo",
        command: "undo",
        build: installed_on_retail,
        preview: &["undo", "{tx}"],
        expect: Expect::Completed,
    },
    Case {
        name: "backup_restore",
        command: "backup restore",
        build: backed_up_then_played,
        preview: &["backup", "restore", "{backup}", "-t", "retail"],
        expect: Expect::Completed,
    },
    Case {
        name: "backup_restore_addon",
        command: "backup restore",
        build: backed_up_then_played,
        preview: &[
            "backup", "restore", "{backup}", "-t", "retail", "--addon", "Foo",
        ],
        expect: Expect::Completed,
    },
    Case {
        name: "backup_prune",
        command: "backup prune",
        build: two_backups_on_retail,
        preview: &["backup", "prune", "-t", "retail", "--", "{backup}"],
        expect: Expect::UnknownNoHistory,
    },
];

// ---------------------------------------------------------------------------------------
// Homes.

/// `products`, with Foo v1 served as `gh:o/foo`.
fn served(log: &TestLog, products: &[&str]) -> Home {
    let home = Home::new(products);
    log.env(&home);
    gh_release(&home.http, FOO, "v1", &foo(FOO_V1));
    home
}

/// `omaddons <args>` with `--json --progress json` (before any `--`, after which every
/// word is a positional), which must succeed and keep every name private.
fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    let at = argv
        .iter()
        .position(|word| *word == "--")
        .unwrap_or(argv.len());
    argv.splice(at..at, ["--json", "--progress", "json"]);
    let ran = log.run(home, &argv);
    ran.ok().assert_private();
    ran
}

fn zip_on_retail(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    let zip = home.env.root().join("foo.zip");
    fs::write(&zip, zip_text(FOO_V1)).unwrap();
    Built::new(home, &[("{zip}", zip.display().to_string())])
}

/// Foo v1 installed on retail while v2 is the latest release, the memo a day old.
fn behind_on_retail(log: &TestLog) -> Built {
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    home.installed_behind(FOO, ("v1", &foo(FOO_V1)), ("v2", &foo(FOO_V2)), &["retail"]);
    Built::new(home, &[])
}

/// Foo v1 installed on retail; `{tx}` is that install.
fn installed_on_retail(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    let installed = run(log, &home, &["install", "gh:o/foo", "-t", "retail"]);
    let tx = installed.data()["tx"].as_str().unwrap().to_owned();
    Built::new(home, &[("{tx}", tx)])
}

fn pinned_on_retail(log: &TestLog) -> Built {
    let built = installed_on_retail(log);
    run(log, &built.home, &["pin", "foo", "-t", "retail"]);
    built
}

/// Foo v1's folder unpacked into retail's AddOns by hand: no record names it.
fn unmanaged_on_retail(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    common::addons::unpack(&home.addons("_retail_"), FOO_V1);
    Built::new(home, &[])
}

/// The default `addons.toml` asks for Foo on retail.
fn desired_on_retail(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    home.write_desired("schema_version = \"1.0\"\n[target.\"retail\"]\naddons = [\"gh:o/foo\"]\n");
    Built::new(home, &[])
}

/// Classic Era and Anniversary, Foo v1 installed on Classic Era from its source.
fn installed_on_era(log: &TestLog) -> Built {
    let home = served(log, &["wow_classic_era", "wow_anniversary"]);
    run(log, &home, &["install", "gh:o/foo", "-t", "era"]);
    Built::new(home, &[])
}

/// Foo installed on retail, its SavedVariables (account-wide and the character's, under
/// the fake names) backed up as `{backup}`, then its account-wide file rewritten, as a
/// play session would.
fn backed_up_then_played(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    run(log, &home, &["install", "gh:o/foo", "-t", "retail"]);
    Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 64)
        .build();
    let id = backup(log, &home);
    let played = home
        .client("_retail_")
        .join(common::privacy::ACCOUNT)
        .join("Foo.lua");
    fs::write(played, "FooDB = { [\"played\"] = true }\n").unwrap();
    Built::new(home, &[("{backup}", id)])
}

/// Two manual backups of retail's `WTF/`; `{backup}` is the older, never the newest
/// checkpoint, which `backup prune` refuses.
fn two_backups_on_retail(log: &TestLog) -> Built {
    let home = served(log, &["wow"]);
    Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 64)
        .build();
    let older = backup(log, &home);
    backup(log, &home);
    Built::new(home, &[("{backup}", older)])
}

/// `backup create -t retail`; returns the backup's id.
fn backup(log: &TestLog, home: &Home) -> String {
    let made = run(log, home, &["backup", "create", "-t", "retail"]);
    made.data()["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

// ---------------------------------------------------------------------------------------
// The check.

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|word| word.as_str().map(str::to_owned))
        .collect()
}

/// `log.assert_eq` under a label that names the case first, so a failure reads
/// `assert "case <name>: <what>" …: expected …, got …`, then the case's transcript.
#[track_caller]
fn expect_eq<T: PartialEq + std::fmt::Debug + serde::Serialize>(
    log: &TestLog,
    case: &Case,
    what: &str,
    got: &T,
    expected: &T,
) {
    log.assert_eq(&format!("case {}: {what}", case.name), got, expected);
}

#[test]
fn every_storable_command_reconciles_from_history() {
    let log = tlog!();
    for case in CASES {
        check(&log.case(case.name), case);
    }
}

fn check(log: &TestLog, case: &Case) {
    log.step("build");
    let built = (case.build)(log);
    let home = &built.home;
    log.tree("home", home.env.root());

    log.step("preview");
    let mut argv: Vec<&str> = case
        .preview
        .iter()
        .map(|word| built.fill.get(word).map_or(*word, String::as_str))
        .collect();
    let at = argv
        .iter()
        .position(|word| *word == "--")
        .unwrap_or(argv.len());
    argv.insert(at, "--dry-run");
    let previewed = run(log, home, &argv);
    let id = plan_id(&previewed);
    log.json("approval", &previewed.data()["approval"]);
    let hint = previewed
        .commands()
        .into_iter()
        .find(|c| c.action == "apply")
        .map(|c| c.argv);
    expect_eq(
        log,
        case,
        "the apply hint",
        &hint,
        &Some(strings(&["omaddons", "apply", &id])),
    );
    if case.expect == Expect::UnknownNoHistory {
        log.step("plans show before apply");
        let shown = show(log, home, &[&id, "--progress", "json"], &[]);
        shown.ok().assert_private();
        expect_eq(
            log,
            case,
            "state",
            &shown.data()["state"],
            &json!("unknown"),
        );
        let display = words(&shown.data()["intent"]["display_argv"]);
        no_history_advice(log, case, &built, &id, &display, false);
    }

    log.step("apply");
    let applied = run(log, home, &["apply", &id]);
    log.json("outcomes", &applied.data()["outcomes"]);
    let commits = committed(&applied);
    assert!(
        !commits.is_empty(),
        "case {}: apply committed nothing: {}",
        case.name,
        applied.data()
    );
    let intent = applied.data()["applied"].clone();
    log.json("applied", &intent);
    expect_eq(
        log,
        case,
        "the command applied",
        &intent["command"],
        &json!(case.command),
    );

    log.step("plans show");
    let shown = show(log, home, &[&id, "--progress", "json"], &[]);
    shown.ok().assert_private();
    let data = shown.data();
    log.json("plans show", data);
    expect_eq(log, case, "stored", &data["stored"], &json!(true));
    expect_eq(log, case, "readable", &data["readable"], &json!(true));
    expect_eq(log, case, "the intent apply ran", &data["intent"], &intent);
    let evidence = evidence_txs(&shown);
    match case.expect {
        Expect::Completed => {
            expect_eq(log, case, "state", &data["state"], &json!("completed"));
            expect_eq(
                log,
                case,
                "the whole window read",
                &data["evidence"]["complete"],
                &json!(true),
            );
            expect_eq(log, case, "evidence", &evidence, &commits);
            let mut expected = Vec::new();
            if commits.len() > 1 {
                expected.push("undo_run".to_owned());
            }
            expected.extend(commits.iter().map(|_| "undo".to_owned()));
            let hints = shown.commands();
            let actions: Vec<String> = hints.iter().map(|c| c.action.clone()).collect();
            expect_eq(log, case, "the undo hints", &actions, &expected);
            let mut undone: Vec<String> = hints
                .iter()
                .filter(|c| c.action == "undo")
                .map(|c| c.argv[2].clone())
                .collect();
            let mut txs: Vec<String> = commits.iter().map(|(_, tx)| tx.clone()).collect();
            undone.sort();
            txs.sort();
            expect_eq(log, case, "each undo names a commit", &undone, &txs);
        }
        Expect::UnknownNoHistory => {
            expect_eq(log, case, "state", &data["state"], &json!("unknown"));
            expect_eq(log, case, "no evidence", &evidence, &Vec::new());
            let display = words(&data["intent"]["display_argv"]);
            no_history_advice(log, case, &built, &id, &display, true);
        }
    }

    log.step("history");
    let history = history_txs_for(log, home, &id);
    let expected = match case.expect {
        Expect::Completed => evidence,
        Expect::UnknownNoHistory => Vec::new(),
    };
    expect_eq(
        log,
        case,
        "history's lines carrying the plan",
        &history,
        &expected,
    );
}

/// The human view of a `backup prune` plan, which records no history line: the reason,
/// and advice to preview again (`--dry-run` before the plan's `--`, if any) that parses
/// and answers whether it ran. Before the prune ran, the advice is a dry run that would
/// delete `{backup}`; after, the backup it names is gone, and the re-preview says so
/// (`not_found` for that id), which is the answer.
fn no_history_advice(
    log: &TestLog,
    case: &Case,
    built: &Built,
    id: &str,
    display: &[String],
    ran: bool,
) {
    let home = &built.home;
    let human = show_human(log, home, &[id, "--progress", "json"], &[]);
    human.ok().assert_private();
    let says = |text: &str| {
        assert!(
            human.stdout().contains(text),
            "case {}: the human view does not say {text:?}:\n{}",
            case.name,
            human.stdout()
        );
    };
    says("backup prune records no history line");
    let mut advice = display.to_vec();
    let at = advice
        .iter()
        .position(|word| word == "--")
        .unwrap_or(advice.len());
    advice.insert(at, "--dry-run".to_owned());
    log.json("advice", &advice);
    says(&format!("see what is left: {}", shell_words::join(&advice)));
    if let Err(err) = Cli::try_parse_from(&advice) {
        panic!("case {}: {advice:?} does not parse: {err}", case.name); // ubs:ignore — the assertion
    }
    let backup = &built.fill["{backup}"];
    let mut argv: Vec<&str> = advice[1..].iter().map(String::as_str).collect();
    if !ran {
        let again = run(log, home, &argv);
        expect_eq(
            log,
            case,
            "a dry run",
            &again.data()["dry_run"],
            &json!(true),
        );
        let pruned: Vec<String> = again.data()["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|t| t["pruned"].as_array().into_iter().flatten())
            .filter_map(|b| b["id"].as_str().map(str::to_owned))
            .collect();
        expect_eq(log, case, "it would prune", &pruned, &vec![backup.clone()]);
        return;
    }
    let at = argv
        .iter()
        .position(|word| *word == "--")
        .unwrap_or(argv.len());
    argv.splice(at..at, ["--json", "--progress", "json"]);
    let again = log.run(home, &argv);
    again.fails(3, "not_found").assert_private();
    expect_eq(
        log,
        case,
        "the backup it names is gone",
        &again.envelope()["error"]["details"]["prefix"],
        &json!(backup),
    );
}
