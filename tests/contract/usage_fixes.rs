//! Usage errors suggest commands that run: every fix and alternative parses (the
//! harness checks it on every envelope), and a guess never hands over a different
//! command that changes something. The fix of a mistyped word runs the command the
//! user typed (through an alias or a synonym), a command that only reads, or
//! `omaddons help …` (README "Command reference").

use super::*;
use clap::{CommandFactory as _, Parser as _};
use omaddons::cli::{Cli, command_path, usage_error};

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
