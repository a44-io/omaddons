//! The glue's slots placed in the live command tree, and the shell text built from them.
#![cfg(test)]

use std::collections::BTreeSet;

use super::*;
use crate::app::completions::command;

/// The value names of arguments that take a client, an addon or an id omaddons' files
/// hold: each such argument must be a slot, or be listed in [`NOT_SLOTS`] with why.
const ID_VALUE_NAMES: &[&str] = &["TARGET", "ADDON", "PLAN_ID", "TX", "RUN", "ID", "IDS"];

/// Arguments with such a value name that completion leaves alone: (path, arg id, why).
const NOT_SLOTS: &[(&str, &str, &str)] = &[];

fn built_tree() -> Tree {
    match tree(&command()) {
        Ok(tree) => tree,
        Err(e) => panic!("the slots do not fit the command tree: {e}"), // ubs:ignore — fails the test
    }
}

/// Every (path, arg id) of `cmd` and below whose value name is in [`ID_VALUE_NAMES`].
fn id_arguments(cmd: &clap::Command, path: &str, out: &mut BTreeSet<(String, String)>) {
    for arg in cmd.get_arguments().filter(|arg| !arg.is_hide_set()) {
        let named = arg
            .get_value_names()
            .unwrap_or_default()
            .iter()
            .any(|name| ID_VALUE_NAMES.contains(&name.as_str()));
        // A global argument is declared once, at the root.
        if named && (path.is_empty() || !arg.is_global_set()) {
            out.insert((path.to_owned(), arg.get_id().to_string()));
        }
    }
    for sub in cmd.get_subcommands() {
        id_arguments(sub, &join(path, sub.get_name()), out);
    }
}

#[test]
fn every_slot_fits_the_live_command_tree() {
    let tree = built_tree();
    assert_eq!(tree.target, ["-t", "--target", "--flavor"]);
    assert_eq!(tree.from, ["--from"]);
    assert_eq!(
        tree.global.len() + tree.options.len() + tree.positionals.len(),
        SLOTS.len()
    );
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let glue = glue(shell, &command());
        assert!(glue.is_ok(), "{shell:?}: {glue:?}");
    }
}

#[test]
fn every_argument_that_takes_an_id_or_a_name_is_a_slot() {
    let mut found = BTreeSet::new();
    id_arguments(&command(), "", &mut found);
    for (path, arg, why) in NOT_SLOTS {
        assert!(!why.is_empty(), "{path} {arg}: say why");
        assert!(
            found.remove(&((*path).to_owned(), (*arg).to_owned())),
            "{path} {arg} is listed as no slot but takes no id"
        );
    }
    let slots: BTreeSet<(String, String)> = SLOTS
        .iter()
        .map(|s| (s.path.to_owned(), s.arg.to_owned()))
        .collect();
    let missing: Vec<_> = found.difference(&slots).collect();
    let extra: Vec<_> = slots.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "arguments that take an id or a name but complete none: {missing:?}; add a slot, or a NOT_SLOTS row saying why"
    );
    assert!(
        extra.is_empty(),
        "slots whose argument's value name is not in ID_VALUE_NAMES: {extra:?}"
    );
}

#[test]
fn the_walk_knows_every_command_path_by_every_name() {
    let tree = built_tree();
    let mut expected = 0;
    let mut stack = vec![command()];
    while let Some(cmd) = stack.pop() {
        for sub in cmd.get_subcommands() {
            expected += 1;
            stack.push(sub.clone());
        }
    }
    assert_eq!(tree.children.len(), expected);
    let reached: BTreeSet<(&str, &str, &str)> = tree
        .children
        .iter()
        .flat_map(|c| {
            c.words
                .iter()
                .map(move |w| (c.parent.as_str(), w.as_str(), c.path.as_str()))
        })
        .collect();
    for (case, parent, word, path) in [
        ("a command", "", "install", "install"),
        ("a visible alias", "", "add", "install"),
        ("another alias", "", "rm", "remove"),
        ("a nested command", "backup", "restore", "backup restore"),
        ("a doctor verb", "doctor", "explain", "doctor explain"),
    ] {
        assert!(reached.contains(&(parent, word, path)), "{case}");
    }
    for option in [
        "-t", "--target", "--flavor", "--from", "--addon", "--run", "--only",
    ] {
        assert!(tree.value_options.contains(option), "{option}");
    }
    for flag in ["--json", "--dry-run", "-v", "--all", "--quick"] {
        assert!(!tree.value_options.contains(flag), "{flag} takes no value");
    }
}

#[test]
fn positional_slots_take_their_place_and_arity_from_clap() {
    let tree = built_tree();
    let of = |path: &str| -> Vec<(usize, bool, Kind)> {
        tree.positionals
            .iter()
            .filter(|p| p.path == path)
            .map(|p| (p.index, p.many, p.kind))
            .collect()
    };
    for (path, expected) in [
        ("update", vec![(0, true, Kind::Addon)]),
        ("pin", vec![(0, false, Kind::Addon)]),
        ("apply", vec![(0, false, Kind::Plan)]),
        ("undo", vec![(0, false, Kind::Tx)]),
        ("backup prune", vec![(0, true, Kind::Backup)]),
        ("backup export", vec![(0, false, Kind::Backup)]),
        ("targets rename", vec![(0, false, Kind::Target)]),
        ("doctor explain", vec![(0, false, Kind::Fm)]),
    ] {
        assert_eq!(of(path), expected, "{path}");
    }
}

#[test]
fn the_glue_calls_omaddons_only_as_its_reader() {
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let glue = glue(shell, &command()).unwrap_or_default();
        let calls: Vec<&str> = glue
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter(|line| line.contains("omaddons __") || line.contains("(omaddons"))
            .collect();
        assert!(!calls.is_empty(), "{shell:?}: no call");
        for line in calls {
            assert!(
                line.contains(&format!("omaddons {WORD} ")),
                "{shell:?}: {line}"
            );
        }
        assert!(!glue.contains("COMPLETE="), "{shell:?}");
        assert!(!glue.contains('\x1b'), "{shell:?}");
    }
}

#[test]
fn fish_offers_files_beside_client_ids_and_never_beside_other_values() {
    let glue = glue(Shell::Fish, &command()).unwrap_or_default();
    let rule = |flag: &str| -> String {
        glue.lines()
            .find(|line| line.starts_with("complete ") && line.contains(flag))
            .unwrap_or_default()
            .to_owned()
    };
    for (flag, files) in [
        ("-s t -l target -l flavor ", true),
        ("-l from ", true),
        ("-l addon ", false),
        ("-l run ", false),
        ("-l since ", false),
        ("-l only ", false),
    ] {
        let line = rule(flag);
        assert!(!line.is_empty(), "no rule for {flag}");
        assert_eq!(!line.contains(" -f "), files, "{line}");
    }
    assert!(glue.contains("complete -c omaddons -n '__fish_omaddons_at positional' -f "));
}

#[test]
fn a_word_with_a_shell_character_is_refused() {
    for (word, ok) in [
        ("install", true),
        ("net-log", true),
        ("robot-docs", true),
        ("x2", true),
        ("", false),
        ("Install", false),
        ("a b", false),
        ("a*", false),
        ("$(x)", false),
        ("a'b", false),
    ] {
        assert_eq!(plain(word), ok, "{word:?}");
    }
}
