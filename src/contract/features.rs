//! Stable feature words, checked against the surfaces this build actually provides.

/// Each word names a surface this build has. New words go in canonical order and
/// remain once their surface ships.
pub const FEATURES: &[&str] = &[
    "json_envelope",
    "ndjson_stderr",
    "dry_run_everywhere",
    "preconditions",
    "offline",
    "progress_ndjson",
    "undo",
    "backups",
    "lua_errors",
    "omarchy_widget",
    "completions",
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::*;
    use crate::app::COMMANDS;
    use crate::cli::Cli;

    const CANONICAL: [&str; 14] = [
        "json_envelope",
        "ndjson_stderr",
        "dry_run_everywhere",
        "preconditions",
        "stored_plans",
        "doctor",
        "offline",
        "progress_ndjson",
        "undo",
        "backups",
        "lua_errors",
        "omarchy_widget",
        "completions",
        "agent_skill",
    ];

    enum Evidence {
        Flag(&'static str),
        Command(&'static str),
        Builtin(&'static str),
    }

    const EVIDENCE: &[(&str, Evidence)] = &[
        ("json_envelope", Evidence::Flag("json")),
        (
            "ndjson_stderr",
            Evidence::Builtin("StderrMode::Ndjson is selected whenever stderr is not a terminal"),
        ),
        ("dry_run_everywhere", Evidence::Flag("dry-run")),
        ("preconditions", Evidence::Flag("if-match")),
        ("stored_plans", Evidence::Command("apply")),
        ("doctor", Evidence::Command("doctor")),
        ("offline", Evidence::Flag("offline")),
        ("progress_ndjson", Evidence::Flag("progress")),
        ("undo", Evidence::Command("undo")),
        ("backups", Evidence::Command("backup create")),
        ("lua_errors", Evidence::Command("errors")),
        ("omarchy_widget", Evidence::Command("omarchy install")),
        ("completions", Evidence::Command("completions")),
        ("agent_skill", Evidence::Command("robot-docs install-skill")),
    ];

    #[test]
    fn features_are_unique_snake_case_and_canonically_ordered() {
        let mut seen = BTreeSet::new();
        let mut previous = None;
        for word in FEATURES {
            assert!(
                word.starts_with(|c: char| c.is_ascii_lowercase())
                    && word
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "invalid feature word {word:?}"
            );
            assert!(seen.insert(word), "duplicate feature word {word}");
            let position = CANONICAL
                .iter()
                .position(|w| w == word)
                .unwrap_or_else(|| panic!("feature word {word} is absent from CANONICAL")); // ubs:ignore — test-only failure identifies a missing registry entry
            assert!(
                previous.is_none_or(|p| position > p),
                "{word} belongs at canonical position {position}, after {:?} and before {:?}",
                position.checked_sub(1).map(|p| CANONICAL[p]),
                CANONICAL.get(position + 1)
            );
            previous = Some(position);
        }
    }

    #[test]
    fn each_feature_word_is_listed_exactly_when_its_surface_exists() {
        assert_eq!(
            EVIDENCE.iter().map(|(word, _)| *word).collect::<Vec<_>>(),
            CANONICAL,
            "one evidence row per canonical feature"
        );
        let mut command = Cli::command();
        command.build();
        for (word, evidence) in EVIDENCE {
            let (exists, surface) = match evidence {
                Evidence::Flag(flag) => (
                    command
                        .get_arguments()
                        .any(|arg| arg.get_long() == Some(flag) && arg.is_global_set()),
                    format!("the --{flag} global flag"),
                ),
                Evidence::Command(path) => (
                    COMMANDS.iter().any(|c| c.name == *path),
                    format!("the {path} command"),
                ),
                Evidence::Builtin(reason) => {
                    assert!(!reason.is_empty(), "{word}: explain the built-in evidence");
                    (true, (*reason).to_owned())
                }
            };
            let listed = FEATURES.contains(word);
            assert!(
                !exists || listed,
                "{surface} exists: add the feature word {word} to src/contract/features.rs at its place in the canonical order"
            );
            assert!(
                !listed || exists,
                "FEATURES lists {word}, but {surface} does not exist: a feature word names a surface this build has"
            );
        }
    }
}
