//! Usage errors suggest commands that run: every fix and alternative parses (the
//! harness checks it on every envelope), and a guess never hands over a different
//! command that changes something. The fix of a mistyped word runs the command the
//! user typed (through an alias or a synonym), a command that only reads, or
//! `omaddons help …` (README "Command reference").

use std::collections::BTreeSet;

use super::*;
use crate::common::tlog::tlog;
use clap::{CommandFactory as _, Parser as _};
use omaddons::cli::{Cli, command_path, usage_error, usage_error_with};
use omaddons::error::{Error, UsageFix};
use omaddons::resolve::catalog::Catalog;
use omaddons::vocabulary::{CLIENT_FREE, SELECTS_CLIENT, TargetVocabulary, selects_client};

/// The verb rule on one suggestion: `help …`, the command the user typed (`typed`, its
/// canonical path), or one that only reads.
fn never_another_mutating_command(argv: &[String], typed: Option<&str>, ctx: &str) {
    if argv.get(1).map(String::as_str) == Some("help") {
        return;
    }
    let path = command_path(argv).join(" ");
    let reads = path.is_empty() || !omaddons::app::mutates(&path);
    assert!(
        reads || Some(path.as_str()) == typed,
        "{argv:?} runs {path}, which changes something, for a typo of {typed:?}\n{ctx}"
    );
}

/// Every suggestion of an envelope's error.
fn suggestions(envelope: &Value) -> Vec<Vec<String>> {
    let error = &envelope["error"];
    std::iter::once(&error["fix_argv"])
        .chain(
            error["alternatives"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| &a["argv"]),
        )
        .filter(|argv| !argv.is_null())
        .map(support::strings)
        .collect()
}

fn owned(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|a| (*a).to_owned()).collect()
}

/// Mistyped verbs, nested verbs, flags and values: (argv, the canonical path the typed
/// word means, through an alias or a synonym; `None` for a plain typo).
const CORPUS: &[(&[&str], Option<&str>)] = &[
    (&["instal", "dbm"], None),
    (&["plan"], None),
    (&["unpn", "x"], None),
    (&["statsu"], None),
    (&["lst"], None),
    (&["serch", "x"], None),
    (&["outdate"], None),
    (&["hepl"], None),
    (&["reinstall", "x", "-t", "retail"], Some("install")),
    (&["targets", "rn", "a", "b"], None),
    (&["targets", "ad", "/x"], None),
    (&["targets", "remov", "x"], None),
    (&["targets", "remove", "x"], Some("targets rm")),
    (&["targets", "list"], Some("targets")),
    (&["config", "st", "k", "v"], None),
    (&["config", "gt", "k"], None),
    (&["config", "ls"], Some("config")),
    (&["auth", "github", "st"], None),
    (&["auth", "gihub"], Some("auth github")),
    (&["auth", "github", "show"], Some("auth github")),
    (&["install", "x", "--dry-rn"], Some("install")),
    (&["targets", "--rescn"], Some("targets")),
    (
        &["targets", "add", "/x", "--nme", "foo"],
        Some("targets add"),
    ),
    (&["undo", "--last-bi", "agent"], Some("undo")),
    // A mistyped or refused presentation flag: the fix drops it.
    (&["--jsno"], None),
    (&["status", "--jsno"], Some("status")),
    (&["--format", "jsno", "status"], Some("status")),
    (&["status", "--cachd"], Some("status")),
    (&["history", "--limt", "5"], Some("history")),
    (&["install", "x", "--channel", "betaa"], Some("install")),
    (&["search", "x", "--source", "wowii"], Some("search")),
    (&["undo", "--saved-variables", "restor"], Some("undo")),
    (
        &["undo", "--last-by", "humna", "-t", "retail", "--dry-run"],
        Some("undo"),
    ),
    (
        &["undo", "--last-by", "agnet", "-t", "retail"],
        Some("undo"),
    ),
    (&["undo", "--last-by", "xyzzy"], None),
    (
        &["install", "x", "-t", "retail", "--actor", "humna"],
        Some("install"),
    ),
    (&["install", "x", "--actor", "xyzzy"], None),
];

/// Mistyped values (argv, the value's index): the fix is the typed command with that
/// value corrected and every other argument where it was.
const VALUE_TYPOS: &[(&[&str], usize)] = &[
    (&["install", "x", "--channel", "betaa"], 3),
    (&["search", "x", "--source", "wowii"], 3),
    (
        &[
            "undo",
            "--saved-variables",
            "restor",
            "-t",
            "retail",
            "--dry-run",
        ],
        2,
    ),
    (
        &["undo", "--last-by", "humna", "-t", "retail", "--dry-run"],
        2,
    ),
    (&["undo", "--last-by", "agnet", "-t", "retail"], 2),
    (&["install", "x", "-t", "retail", "--actor", "humna"], 5),
    (&["install", "x", "--actor=agnet", "-t", "retail"], 2),
];

/// The fixes decided by omad-m2-audit-yegz.5.1, exactly.
const DECIDED: &[(&[&str], &[&str])] = &[
    (&["plan"], &["omaddons", "help", "pin"]),
    (&["unpn", "x"], &["omaddons", "help", "unpin"]),
    (&["statsu"], &["omaddons", "status"]),
    (
        &["targets", "rn", "a", "b"],
        &["omaddons", "help", "targets", "rm"],
    ),
    (
        &["targets", "ad", "/x"],
        &["omaddons", "help", "targets", "add"],
    ),
    (
        &["config", "st", "k", "v"],
        &["omaddons", "help", "config", "set"],
    ),
    (
        &["targets", "add", "/x", "--nme", "foo"],
        &["omaddons", "targets", "add", "/x", "--name", "foo"],
    ),
    (
        &["install", "x", "--channel", "betaa"],
        &["omaddons", "install", "x", "--channel", "beta"],
    ),
    (&["auth", "gihub"], &["omaddons", "auth", "github"]),
    (&["status", "--jsno"], &["omaddons", "status"]),
    (&["--format", "jsno", "status"], &["omaddons", "status"]),
    (
        &["reinstall", "x", "-t", "retail"],
        &["omaddons", "install", "x", "-t", "retail", "--reinstall"],
    ),
];

/// The fixes decided by omad-rnk8: a mistyped actor is corrected in place, and one
/// nothing is close to gets help, never a guessed actor (undo can restore
/// SavedVariables).
const DECIDED_ACTORS: &[(&[&str], &[&str])] = &[
    (
        &["undo", "--last-by", "humna", "-t", "retail", "--dry-run"],
        &[
            "omaddons",
            "undo",
            "--last-by",
            "human",
            "-t",
            "retail",
            "--dry-run",
        ],
    ),
    (
        &["undo", "--last-by", "xyzzy"],
        &["omaddons", "help", "undo"],
    ),
    (
        &["install", "x", "-t", "retail", "--actor", "humna"],
        &[
            "omaddons", "install", "x", "-t", "retail", "--actor", "human",
        ],
    ),
    (
        &["install", "x", "--actor", "xyzzy"],
        &["omaddons", "help", "install"],
    ),
];

#[test]
fn a_mistyped_command_line_gets_a_fix_that_runs_and_is_never_another_change() {
    let s = Scene::new();
    for (argv, typed) in CORPUS {
        let checked = s.json(argv, 2);
        let ctx = checked.run.context();
        let found = suggestions(&checked.envelope);
        assert!(!found.is_empty(), "no suggestion\n{ctx}");
        for suggestion in &found {
            never_another_mutating_command(suggestion, *typed, &ctx);
        }
        if let Some((_, fix)) = DECIDED
            .iter()
            .chain(DECIDED_ACTORS)
            .find(|(a, _)| a == argv)
        {
            assert_eq!(found[0], owned(fix), "{ctx}");
        }
    }
    // `unpn x`: no alternative runs `undo`.
    let unpin = s.json(&["unpn", "x"], 2);
    for suggestion in suggestions(&unpin.envelope) {
        assert_ne!(command_path(&suggestion), ["undo"], "{suggestion:?}");
    }
}

/// A value fix changes the mistyped value alone: no argument is dropped or moved.
#[test]
fn a_mistyped_value_is_corrected_in_place_with_every_argument_kept() {
    let s = Scene::new();
    for (argv, at) in VALUE_TYPOS {
        let checked = s.json(argv, 2);
        let ctx = checked.run.context();
        let fix = &suggestions(&checked.envelope)[0];
        let mut typed = vec!["omaddons".to_owned()];
        typed.extend(owned(argv));
        assert_eq!(fix.len(), typed.len(), "{fix:?}\n{ctx}");
        let changed: Vec<usize> = (0..typed.len()).filter(|i| fix[*i] != typed[*i]).collect();
        assert_eq!(changed, [at + 1], "{fix:?}\n{ctx}");
    }
}

/// The verb aliases run their command, and the envelope names it.
#[test]
fn verb_aliases_run_the_canonical_command() {
    let s = Scene::new();
    for (alias, canonical) in [
        ("uninstall", "remove"),
        ("rm", "remove"),
        ("delete", "remove"),
    ] {
        let run = s.home.run(&[alias, "dbm", "-t", "retail"]);
        let envelope = run.envelope();
        assert_eq!(envelope["command"], canonical, "{}", run.context());
        assert_ne!(
            run.code,
            2,
            "{alias} is not a usage error\n{}",
            run.context()
        );
    }
}

/// Every command name, root and nested, misspelled by one dropped letter or two swapped
/// neighbours: every suggestion parses and is never a different command that changes
/// something (in process, so the sweep stays fast). A service under a service parent
/// (`auth github`) is corrected in place: that row is the command the user typed.
#[test]
fn every_one_edit_typo_of_every_command_gets_a_fix_that_runs() {
    let mut root = Cli::command();
    root.build();
    let mut checked = 0;
    for entry in omaddons::app::COMMANDS {
        let path: Vec<&str> = entry.name.split(' ').collect();
        let Some((word, parent)) = path.split_last() else {
            continue;
        };
        let typed = omaddons::cli::SERVICE_PARENTS
            .contains(&parent.join(" ").as_str())
            .then_some(entry.name);
        let level = parent
            .iter()
            .fold(&root, |cmd, name| cmd.find_subcommand(name).unwrap_or(cmd));
        let taken: Vec<String> = level
            .get_subcommands()
            .flat_map(|c| std::iter::once(c.get_name()).chain(c.get_all_aliases()))
            .map(str::to_owned)
            .collect();
        let chars: Vec<char> = word.chars().collect();
        let mut typos: Vec<String> = (0..chars.len())
            .map(|i| {
                let mut c = chars.clone();
                c.remove(i);
                c.into_iter().collect()
            })
            .collect();
        typos.extend((1..chars.len()).map(|i| {
            let mut c = chars.clone();
            c.swap(i - 1, i);
            c.into_iter().collect()
        }));
        for typo in typos {
            if typo.is_empty() || taken.contains(&typo) {
                continue;
            }
            let mut argv = vec!["omaddons".to_owned()];
            argv.extend(parent.iter().map(|p| (*p).to_owned()));
            argv.push(typo);
            let Err(err) = Cli::try_parse_from(&argv) else {
                continue;
            };
            let error = usage_error(&err, &argv);
            let ctx = format!("{argv:?}: {}", error.message);
            for suggestion in std::iter::once(&error.fix).chain(&error.alternatives) {
                let Some(suggested) = suggestion.argv() else {
                    continue;
                };
                support::parses(suggested, &ctx);
                never_another_mutating_command(suggested, typed, &ctx);
            }
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}

// Misplaced clients (M4-D12): a client named where a command or an argument belongs is
// moved into -t, the user's command kept.

/// Where a misplaced client was typed: in a command's place (hook 1) or an argument's
/// (hook 2). It decides the message.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Command,
    Argument,
}

/// (argv after `omaddons`, `fix_argv`, `details.target`, `details.command`, slot).
type Misplaced = (
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
    &'static str,
    Slot,
);

const MISPLACED: &[Misplaced] = &[
    (
        &["retail", "update"],
        &["omaddons", "update", "-t", "retail"],
        "retail",
        "update",
        Slot::Command,
    ),
    (
        &["era", "status"],
        &["omaddons", "status", "-t", "era"],
        "era",
        "status",
        Slot::Command,
    ),
    (
        &["wotlk", "list"],
        &["omaddons", "list", "-t", "wotlk"],
        "wotlk",
        "list",
        Slot::Command,
    ),
    (
        &["sod", "install", "questie"],
        &["omaddons", "install", "questie", "-t", "sod"],
        "sod",
        "install",
        Slot::Command,
    ),
    (
        &["Retail", "upgrade"],
        &["omaddons", "upgrade", "-t", "Retail"],
        "Retail",
        "update",
        Slot::Command,
    ),
    (
        &["--dry-run", "retail", "update"],
        &["omaddons", "--dry-run", "update", "-t", "retail"],
        "retail",
        "update",
        Slot::Command,
    ),
    (
        &["retail", "remove", "--", "dbm"],
        &["omaddons", "remove", "-t", "retail", "--", "dbm"],
        "retail",
        "remove",
        Slot::Command,
    ),
    (
        &["backup", "retail", "list"],
        &["omaddons", "backup", "list", "-t", "retail"],
        "retail",
        "backup list",
        Slot::Command,
    ),
    (
        &["errors", "retail"],
        &["omaddons", "errors", "-t", "retail"],
        "retail",
        "errors",
        Slot::Command,
    ),
    (
        &["status", "retail"],
        &["omaddons", "status", "-t", "retail"],
        "retail",
        "status",
        Slot::Argument,
    ),
    (
        &["list", "era"],
        &["omaddons", "list", "-t", "era"],
        "era",
        "list",
        Slot::Argument,
    ),
    (
        &["outdated", "retail"],
        &["omaddons", "outdated", "-t", "retail"],
        "retail",
        "outdated",
        Slot::Argument,
    ),
    (
        &["info", "dbm", "era"],
        &["omaddons", "info", "dbm", "-t", "era"],
        "era",
        "info",
        Slot::Argument,
    ),
    (
        &["pin", "dbm", "retail"],
        &["omaddons", "pin", "dbm", "-t", "retail"],
        "retail",
        "pin",
        Slot::Argument,
    ),
    (
        &["unpin", "dbm", "retail"],
        &["omaddons", "unpin", "dbm", "-t", "retail"],
        "retail",
        "unpin",
        Slot::Argument,
    ),
    (
        &["pin", "dbm", "hc", "--version", "12.1.14"],
        &["omaddons", "pin", "dbm", "-t", "hc", "--version", "12.1.14"],
        "hc",
        "pin",
        Slot::Argument,
    ),
];

/// Words that must keep the fix they had before M4-D12: no client, a command that never
/// reads -t, a target already given, or nothing left to run.
const NOT_MISPLACED: &[&[&str]] = &[
    &["frobnicate", "update"],
    &["retail"],
    &["retail", "dbm"],
    &["status", "dbm"],
    &["info", "dbm", "questie"],
    &["retail", "update", "-t", "era"],
    &["status", "retail", "--flavor", "era"],
    &["retail", "config"],
    &["retail", "targets"],
    &["retail", "net-log"],
    &["retail", "help"],
    &["retail", "copy", "--from", "era", "--to", "classic"],
    &["config", "retail"],
    &["targets", "retail"],
    &["omarchy", "retail"],
];

/// `omaddons <args> --json` through the contract's checks, the flag before any `--` (after
/// it, it would be a positional).
fn run_json(s: &Scene, args: &[&str], code: i32) -> support::Checked {
    match args.iter().position(|a| *a == "--") {
        Some(at) => {
            let mut argv = owned(args);
            argv.insert(at, "--json".to_owned());
            support::check(s.home.exec(&argv), code)
        }
        None => s.json(args, code),
    }
}

#[test]
fn a_misplaced_client_moves_into_dash_t_and_keeps_the_verb() {
    let s = Scene::new();
    let log = tlog!();
    for (args, fix, target, command, slot) in MISPLACED {
        log.step(&format!("{args:?}"));
        let c = run_json(&s, args, 2);
        log.json("envelope", &c.envelope);
        let ctx = c.run.context();
        let error = &c.envelope["error"];
        assert_eq!(error["code"], "usage", "{ctx}");
        assert_eq!(error["retryable"], false, "{ctx}");
        assert_eq!(support::strings(&error["fix_argv"]), owned(fix), "{ctx}");
        assert_eq!(
            error["details"],
            serde_json::json!({ "target": target, "command": command }),
            "{ctx}"
        );
        let help: Vec<&str> = ["omaddons", "help"]
            .into_iter()
            .chain(command.split(' '))
            .collect();
        assert_eq!(
            error["alternatives"],
            serde_json::json!([{ "display": help.join(" "), "argv": help }]),
            "{ctx}"
        );
        let message = match slot {
            Slot::Command => {
                format!("'{target}' is a client, not a command; the client goes in -t")
            }
            Slot::Argument => format!(
                "'{target}' is a client, not an argument of {command}; the client goes in -t"
            ),
        };
        assert_eq!(error["message"], message, "{ctx}");
        let fixed = owned(fix);
        never_another_mutating_command(&fixed, Some(command), &ctx);
        assert_eq!(command_path(&fixed).join(" "), *command, "{ctx}");
        // A usage error is named by the words it could read.
        match *args {
            ["retail", "update"] => {
                assert_eq!(c.envelope["command"], "omaddons", "{ctx}");
                insta::assert_json_snapshot!(
                    "misplaced_client_before_the_verb",
                    s.stable(&c.envelope)
                );
            }
            ["status", "retail"] => assert_eq!(c.envelope["command"], "status", "{ctx}"),
            ["info", "dbm", "era"] => {
                insta::assert_json_snapshot!(
                    "misplaced_client_after_the_verb",
                    s.stable(&c.envelope)
                );
            }
            _ => {}
        }
    }
}

#[test]
fn a_word_that_names_no_client_keeps_todays_fix() {
    let s = Scene::new();
    let log = tlog!();
    for args in NOT_MISPLACED {
        log.step(&format!("{args:?}"));
        let c = s.json(args, 2);
        log.json("envelope", &c.envelope);
        let ctx = c.run.context();
        let full: Vec<String> = std::iter::once("omaddons")
            .chain(args.iter().copied())
            .chain(["--json"])
            .map(str::to_owned)
            .collect();
        let err = Cli::try_parse_from(&full).unwrap_err();
        let oracle = usage_error_with(&err, &full, &TargetVocabulary::empty, &|_| false);
        let oracle = serde_json::to_value(&oracle).unwrap();
        let error = &c.envelope["error"];
        for field in ["fix_argv", "alternatives", "message"] {
            assert_eq!(error[field], oracle[field], "{field}\n{ctx}");
        }
        assert_eq!(error["details"], serde_json::json!({}), "{ctx}");
        assert_eq!(usage_error(&err, &full).usage_fix, UsageFix::Guess, "{ctx}");
    }
}

/// The error `argv` gets in process with the built-in words, and the one it got before
/// M4-D12 (no words); `None` when `argv` parses.
fn hooked(argv: &[String]) -> Option<(Error, Error)> {
    let err = Cli::try_parse_from(argv).err()?;
    let oracle = usage_error_with(&err, argv, &TargetVocabulary::empty, &|_| false);
    Some((usage_error(&err, argv), oracle))
}

/// An error's fix argv, `details` and `usage_fix`, for assertion messages.
fn shown(error: &Error) -> String {
    format!(
        "fix {:?}, details {}, usage_fix {:?}",
        error.fix.argv(),
        error.details,
        error.usage_fix
    )
}

/// Every built-in word, before `update` and after `status`, is moved; every command path,
/// with `retail` before or after it, is either moved with its verb kept or gets exactly the
/// error it got before M4-D12 (in process, as the one-edit sweep).
#[test]
fn every_client_word_before_or_after_a_command_gets_a_fix_that_keeps_the_verb() {
    let catalog = Catalog::builtin().unwrap();
    let mut words = 0;
    for (spelling, _) in TargetVocabulary::builtin().words() {
        // The addon wins (the catalog guard below covers those words).
        if catalog.entries().iter().any(|e| e.answers_to(spelling)) {
            continue;
        }
        for (typed, fixed) in [
            ([spelling, "update"], ["update", "-t", spelling]),
            (["status", spelling], ["status", "-t", spelling]),
        ] {
            let argv = owned(&[&["omaddons"][..], &typed].concat());
            let (error, oracle) = hooked(&argv).expect("a misplaced client does not parse");
            let ctx = format!(
                "{argv:?}\n got    {}\n oracle {}",
                shown(&error),
                shown(&oracle)
            );
            assert_eq!(error.usage_fix, UsageFix::UserVerb, "{ctx}");
            let fix = error.fix.argv().unwrap_or_default();
            assert_eq!(fix, owned(&[&["omaddons"][..], &fixed].concat()), "{ctx}");
            assert_eq!(error.details["target"], spelling, "{ctx}");
            support::parses(fix, &ctx);
        }
        words += 1;
    }
    assert!(words > 50, "{words} words swept");

    let mut runnable = BTreeSet::new();
    let mut moved = BTreeSet::new();
    for entry in omaddons::app::COMMANDS {
        let path: Vec<&str> = entry.name.split(' ').collect();
        let first = path.first().copied().unwrap_or_default();
        let alone = owned(&[&["omaddons"][..], &path, &["-t", "retail"]].concat());
        // A client-selecting command that runs with -t alone: hook 1 must move a client
        // typed before it.
        let must_move = SELECTS_CLIENT.contains(&first) && Cli::try_parse_from(&alone).is_ok();
        if must_move {
            runnable.insert(entry.name);
        }
        let before = owned(&[&["omaddons", "retail"][..], &path].concat());
        let after = owned(&[&["omaddons"][..], &path, &["retail"]].concat());
        for (argv, hook1) in [(before, true), (after, false)] {
            // Those positionals belong to the command (C2's preflight).
            let Some((error, oracle)) = hooked(&argv) else {
                continue;
            };
            let ctx = format!(
                "{argv:?}\n got    {}\n oracle {}",
                shown(&error),
                shown(&oracle)
            );
            if error.usage_fix == UsageFix::UserVerb {
                let fix = error.fix.argv().unwrap_or_default().to_vec();
                let command = error.details["command"].as_str().unwrap_or_default();
                assert!(
                    selects_client(&owned(&path)),
                    "moved on {}\n{ctx}",
                    entry.name
                );
                support::parses(&fix, &ctx);
                assert_eq!(command_path(&fix).join(" "), command, "{ctx}");
                assert!(
                    fix.windows(2).any(|w| w[0] == "-t" && w[1] == "retail"),
                    "no -t retail\n{ctx}"
                );
                never_another_mutating_command(&fix, Some(command), &ctx);
                // Every other token kept, in order.
                let mut rest = fix.iter();
                // ubs:ignore — comparing command-line words, not secrets
                for token in argv.iter().filter(|a| *a != "retail") {
                    // ubs:ignore — comparing command-line words, not secrets
                    assert!(rest.any(|f| f == token), "{token} dropped or moved\n{ctx}");
                }
                if hook1 {
                    moved.insert(entry.name);
                }
            } else {
                assert_eq!(error, oracle, "{ctx}");
            }
            if CLIENT_FREE.contains(&first) {
                assert_eq!(error.usage_fix, UsageFix::Guess, "{ctx}");
            }
            if hook1 && must_move {
                assert_eq!(error.usage_fix, UsageFix::UserVerb, "a silent miss\n{ctx}");
            }
        }
    }
    assert_eq!(
        moved, runnable,
        "moved before the verb vs runnable with -t alone"
    );
    for expected in [
        "status",
        "list",
        "outdated",
        "update",
        "export",
        "history",
        "errors",
        "backup",
        "backup list",
    ] {
        assert!(runnable.contains(expected), "{expected}: {runnable:?}");
    }
}

/// The addon wins: a catalog name or alias that is also a client word is never moved. No
/// such name exists today; this guards the day one does (an addon called Hardcore exists).
#[test]
fn a_catalog_name_that_is_a_client_word_is_never_moved() {
    let log = tlog!();
    let words = TargetVocabulary::builtin();
    let catalog = Catalog::builtin().unwrap();
    let names: Vec<&str> = catalog
        .entries()
        .iter()
        .flat_map(|e| std::iter::once(&e.name).chain(&e.aliases))
        .map(String::as_str)
        .filter(|name| words.classify(name).is_some())
        .collect();
    log.note(format!(
        "{} catalog names or aliases are client words: {names:?}",
        names.len()
    ));
    for name in names {
        for typed in [[name, "update"], ["status", name]] {
            let argv = owned(&[&["omaddons"][..], &typed].concat());
            let (error, oracle) = hooked(&argv).expect("does not parse");
            assert_eq!(
                error,
                oracle,
                "{argv:?}\n got    {}\n oracle {}",
                shown(&error),
                shown(&oracle)
            );
        }
    }
}

// A client named among the addons of install, update or remove (M4-D12 hook 3).

/// (argv after `omaddons`, `fix_argv` after `omaddons`, `details.target`,
/// `details.command`, the alternatives after the command's help).
type AsAnAddon = (
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
    &'static str,
    &'static [&'static [&'static str]],
);

const MISPLACED_AS_AN_ADDON: &[AsAnAddon] = &[
    (
        &["update", "retail"],
        &["update", "-t", "retail"],
        "retail",
        "update",
        &[],
    ),
    (
        &["update", "foo", "classic"],
        &["update", "foo", "-t", "classic"],
        "classic",
        "update",
        &[],
    ),
    (
        &["remove", "foo", "classic"],
        &["remove", "foo", "-t", "classic"],
        "classic",
        "remove",
        &[],
    ),
    (
        &["rm", "foo", "classic"],
        &["rm", "foo", "-t", "classic"],
        "classic",
        "remove",
        &[],
    ),
    (
        &["upgrade", "foo", "classic", "--dry-run"],
        &["upgrade", "foo", "-t", "classic", "--dry-run"],
        "classic",
        "update",
        &[],
    ),
    (
        &["install", "weakauras", "classic"],
        &["install", "weakauras", "-t", "classic"],
        "classic",
        "install",
        &[],
    ),
    (
        &["install", "weakauras", "mop"],
        &["install", "weakauras", "-t", "mop"],
        "mop",
        "install",
        &[],
    ),
    (
        &["update", "foo", "Retail"],
        &["update", "foo", "-t", "Retail"],
        "Retail",
        "update",
        &[],
    ),
    (
        &["update", "foo", "retail", "classic"],
        &["update", "foo", "-t", "retail"],
        "retail",
        "update",
        &[&["update", "foo", "-t", "classic"]],
    ),
];

fn prefixed(words: &[&str]) -> Vec<String> {
    owned(&[&["omaddons"][..], words].concat())
}

#[test]
fn a_client_named_among_the_addons_moves_into_dash_t() {
    let s = Scene::new();
    let log = tlog!();
    s.install_foo();
    for (args, fix, target, command, others) in MISPLACED_AS_AN_ADDON {
        log.step(&args.join(" "));
        let c = s.json(args, 2);
        log.json("envelope", &c.envelope);
        let ctx = c.run.context();
        let error = &c.envelope["error"];
        assert_eq!(error["code"], "usage", "{ctx}");
        assert_eq!(error["retryable"], false, "{ctx}");
        let fixed = support::strings(&error["fix_argv"]);
        assert_eq!(fixed, prefixed(fix), "{ctx}");
        let help: Vec<&str> = ["help"].into_iter().chain(command.split(' ')).collect();
        let alternatives: Vec<Vec<String>> = error["alternatives"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| support::strings(&a["argv"]))
            .collect();
        let want: Vec<Vec<String>> = std::iter::once(prefixed(&help))
            .chain(others.iter().map(|o| prefixed(o)))
            .collect();
        assert_eq!(alternatives, want, "{ctx}");
        assert_eq!(
            error["details"],
            serde_json::json!({ "target": target, "command": command }),
            "{ctx}"
        );
        let message = match others {
            [] => format!("'{target}' is a client, not an addon; the client goes in -t"),
            _ => {
                let words: Vec<String> = std::iter::once(*target)
                    .chain(others.iter().filter_map(|o| o.last().copied()))
                    .map(|w| format!("'{w}'"))
                    .collect();
                let (last, rest) = words.split_last().expect("two words or more");
                format!(
                    "{} and {last} are clients, not addons; the client goes in -t, one per command",
                    rest.join(", ")
                )
            }
        };
        assert_eq!(error["message"], message, "{ctx}");
        never_another_mutating_command(&fixed, Some(command), &ctx);
        match *args {
            ["remove", "foo", "classic"] => {
                insta::assert_json_snapshot!("misplaced_client_as_an_addon", s.stable(&c.envelope))
            }
            ["update", "foo", "retail", "classic"] => {
                insta::assert_json_snapshot!("misplaced_clients_as_addons", s.stable(&c.envelope))
            }
            _ => {}
        }
    }
}

#[test]
fn a_name_that_is_no_misplaced_client_keeps_todays_error() {
    let s = Scene::new();
    let log = tlog!();
    s.install_foo();
    // A catalog name that is also a mode alias (`mop`): read as the addon, never moved.
    let catalog_d = s.home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-mop.toml"),
        "schema_version = \"1.0\"\n[[addon]]\nname = \"Mop\"\nref = \"gh:o/mop\"\nfamilies = [\"mists\"]\nfolders = [\"Mop\"]\n",
    )
    .unwrap();
    for (args, exit, code) in [
        (&["update", "nosuch"][..], 3, Some("not_managed")),
        (&["remove", "nosuch"], 3, Some("not_managed")),
        (&["install", "nosuchaddon"], 3, Some("not_found")),
        // A target is given: `retail` fails alone, as an unknown name.
        (&["update", "foo", "retail", "-t", "retail"], 0, None),
        (&["update", "mop"], 3, Some("not_managed")),
        // The fix would not parse: the command fails as before.
        (&["remove", "retail"], 3, Some("not_managed")),
    ] {
        log.step(&args.join(" "));
        let c = s.json(args, exit);
        log.json("envelope", &c.envelope);
        let ctx = c.run.context();
        let error = &c.envelope["error"];
        assert_eq!(error["code"].as_str(), code, "{ctx}");
        let details = &error["details"];
        assert!(
            details.get("target").is_none() || details.get("command").is_none(),
            "a misplaced-client error\n{ctx}"
        );
        if exit == 0 {
            let items = &c.envelope["data"]["targets"][0]["items"];
            let retail = items
                .as_array()
                .into_iter()
                .flatten()
                .find(|item| item["request"] == "retail");
            let failed = retail.map(|item| (&item["status"], &item["error"]["code"]));
            assert_eq!(
                failed,
                Some((&Value::from("failed"), &Value::from("not_managed"))),
                "retail fails alone\n{ctx}"
            );
        }
    }
}
