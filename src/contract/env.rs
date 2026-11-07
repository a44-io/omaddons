//! Environment facts shared by capabilities and the guide, held complete by scans.

use schemars::JsonSchema;
use serde::Serialize;

/// When a variable is read (M4-D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvKind {
    /// Read by every build.
    Release,
    /// Read by debug builds only; release builds ignore it.
    Debug,
    /// Read only by the test harness, never by the binary.
    Harness,
}

/// One environment variable, described without ever reading its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EnvVar {
    pub name: &'static str,
    pub kind: EnvKind,
    /// One line of plain text, no trailing period.
    pub description: &'static str,
    /// The global flag it stands for, if any.
    pub mirrors: Option<&'static str>,
}

const fn var(
    name: &'static str,
    kind: EnvKind,
    description: &'static str,
    mirrors: Option<&'static str>,
) -> EnvVar {
    EnvVar {
        name,
        kind,
        description,
        mirrors,
    }
}

/// Every variable omaddons reads: release rows, then debug, then harness.
pub const ENV_VARS: &[EnvVar] = &[
    var(
        "OMADDONS_ACTOR",
        EnvKind::Release,
        "Default --actor: human, agent or widget",
        Some("--actor"),
    ),
    var(
        "OMADDONS_OFFLINE",
        EnvKind::Release,
        "1 (exactly) means --offline",
        Some("--offline"),
    ),
    var(
        "OMADDONS_FORMAT",
        EnvKind::Release,
        "json means --json",
        Some("--json"),
    ),
    var(
        "OMADDONS_GITHUB_TOKEN",
        EnvKind::Release,
        "Bearer token sent to api.github.com only",
        None,
    ),
    var(
        "NO_COLOR",
        EnvKind::Release,
        "Set and non-empty: no color",
        None,
    ),
    var(
        "CI",
        EnvKind::Release,
        "Set and non-empty: no color, and NDJSON on stderr",
        None,
    ),
    var(
        "TERM",
        EnvKind::Release,
        "dumb: no color, and NDJSON on stderr",
        None,
    ),
    var(
        "HOME",
        EnvKind::Release,
        "Required and absolute: every path omaddons derives starts here",
        None,
    ),
    var(
        "XDG_CONFIG_HOME",
        EnvKind::Release,
        "Moves omaddons' config when absolute; also the Lutris and Heroic config lookups",
        None,
    ),
    var(
        "XDG_STATE_HOME",
        EnvKind::Release,
        "Moves omaddons' state when absolute",
        None,
    ),
    var(
        "XDG_CACHE_HOME",
        EnvKind::Release,
        "Moves omaddons' cache when absolute",
        None,
    ),
    var(
        "XDG_DATA_HOME",
        EnvKind::Release,
        "Moves the Lutris, Steam and Bottles data lookups; omaddons keeps no files there",
        None,
    ),
    var(
        "OMARCHY_PATH",
        EnvKind::Release,
        "Where omarchy install also looks for Omarchy's shell ($OMARCHY_PATH/shell)",
        None,
    ),
    var(
        "PATH",
        EnvKind::Release,
        "Where gh, git and omarchy-shell are found; git keeps only PATH from the caller's environment",
        None,
    ),
    var(
        "OMADDONS_FAILPOINTS",
        EnvKind::Debug,
        "Failpoints to arm: name or name=action[@N], comma-separated",
        None,
    ),
    var(
        "OMADDONS_FAILPOINT_DIR",
        EnvKind::Debug,
        "Absolute directory where pause failpoints meet the test that resumes them",
        None,
    ),
    var(
        "OMADDONS_HTTP_FIXTURES",
        EnvKind::Debug,
        "Directory of recorded HTTP responses to replay instead of the network",
        None,
    ),
    var(
        "OMADDONS_RECORD",
        EnvKind::Debug,
        "1 with OMADDONS_HTTP_FIXTURES: record real responses into it",
        None,
    ),
    var(
        "OMADDONS_PROC_ROOT",
        EnvKind::Debug,
        "A fake /proc that running-client detection reads",
        None,
    ),
    var(
        "OMADDONS_NOW",
        EnvKind::Debug,
        "An RFC 3339 time that freezes the clock",
        None,
    ),
    var(
        "OMADDONS_FS_CLONE",
        EnvKind::Debug,
        "yes or no: whether the filesystem under a client clones files",
        None,
    ),
    var(
        "OMADDONS_FS_FREE_MB",
        EnvKind::Debug,
        "The free space, in MiB, that backups see",
        None,
    ),
    var(
        "OMADDONS_WIDGET_ID",
        EnvKind::Debug,
        "A suffix that installs a separate copy of the bar widget",
        None,
    ),
    var(
        "OMADDONS_TEST_FIXERS",
        EnvKind::Debug,
        "1 registers the doctor's three test-only fixers, in debug builds",
        None,
    ),
    var(
        "OMADDONS_ALLOW_SKIP",
        EnvKind::Harness,
        "Checkpoint tests allowed to print SKIP, as a comma list of test names",
        None,
    ),
    var(
        "OMADDONS_REAL_MACHINE",
        EnvKind::Harness,
        "1 runs the real-machine tests",
        None,
    ),
    var(
        "OMADDONS_LIVE",
        EnvKind::Harness,
        "1 runs the live network probes",
        None,
    ),
    var(
        "OMADDONS_REAL_CUSTOM_ROOT",
        EnvKind::Harness,
        "A real custom client root for the custom-discovery checkpoint test",
        None,
    ),
    var(
        "OMADDONS_WRITE_SEEDS",
        EnvKind::Harness,
        "1 rewrites the fuzz seeds",
        None,
    ),
    var(
        "OMADDONS_WIDGET_OUT",
        EnvKind::Harness,
        "The widget pipeline's output directory, set by scripts/widget-test.sh",
        None,
    ),
    var(
        "BENCH_HOME",
        EnvKind::Harness,
        "Where argc bench builds its home",
        None,
    ),
    var(
        "LIVE_HOME",
        EnvKind::Harness,
        "Where the live run's home is built",
        None,
    ),
    var(
        "COVERAGE_HOME",
        EnvKind::Harness,
        "Where the coverage study's home is built",
        None,
    ),
    var(
        "OMADDONS_TEST_LOG",
        EnvKind::Harness,
        "verbose echoes each test-log record to stderr; unset writes logs only (tests/common/tlog.rs)",
        None,
    ),
    var(
        "OMADDONS_UPDATE_SKILL",
        EnvKind::Harness,
        "1 makes tests/skill.rs rewrite skills/omaddons/SKILL.md from the rendered guide",
        None,
    ),
    var(
        "OMADDONS_UPDATE_WIRE_NAMES",
        EnvKind::Harness,
        "1 makes tests/contract/wire_names.rs regenerate the contract's current golden",
        None,
    ),
    var(
        "OMADDONS_ADVANCE_RELEASED_BASELINE",
        EnvKind::Harness,
        "A contract version: the release step writes tests/contract/released/<version>.json",
        None,
    ),
];

/// The rows capabilities publishes.
pub fn release() -> impl Iterator<Item = &'static EnvVar> {
    ENV_VARS.iter().filter(|row| row.kind == EnvKind::Release)
}

/// The row named `name`.
pub fn get(name: &str) -> Option<&'static EnvVar> {
    ENV_VARS.iter().find(|row| row.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;
    use std::sync::OnceLock;

    use clap::CommandFactory;

    use super::*;
    use crate::cli::Cli;
    use crate::source_scan::{Literals, production_code, sources};

    const LOOKUP_SEAMS: &[(&str, &str)] = &[
        (
            "src/cli.rs",
            "the run's env lookup, passed to Context::new and offline_from_argv",
        ),
        ("src/output/term.rs", "TermEnv::from_process's var"),
        ("src/net/mod.rs", "the transport's env lookup"),
        (
            "src/state/paths.rs",
            "Paths::from_env's lookup, and xdg over it",
        ),
        (
            "src/sources/github_token.rs",
            "the token lookup; the name is github_rest::TOKEN_VAR",
        ),
        (
            "src/state/config.rs",
            "a credential variable named by config.toml: a user's choice, never a registry row",
        ),
    ];

    #[derive(Debug)]
    struct Scanned {
        reads: Vec<(String, usize)>,
        non_literal: Vec<usize>,
        literals: Vec<(String, usize)>,
        debug_gated: bool,
    }

    fn ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn env_name(s: &str) -> bool {
        s.starts_with(|c: char| c.is_ascii_uppercase())
            && s.bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    }

    fn spaces(code: &[char], mut at: usize) -> usize {
        while code.get(at).is_some_and(|c| c.is_whitespace()) {
            at += 1;
        }
        at
    }

    fn word_end(code: &[char], mut at: usize) -> usize {
        while code.get(at).is_some_and(|c| ident(*c)) {
            at += 1;
        }
        at
    }

    /// A scan of both production views in char indices: a non-ASCII literal changes
    /// byte offsets when blanked, but never these offsets. Quotes in strings and
    /// comments cannot become calls because calls are found in the blanked view.
    fn scan(text: &str) -> Option<Scanned> {
        let keep: Vec<char> = production_code(text, Literals::Keep)?.chars().collect();
        let blank: Vec<char> = production_code(text, Literals::Blank)?.chars().collect();
        assert_eq!(
            keep.len(),
            blank.len(),
            "production views have aligned char indices"
        );
        let debug_gated = blank
            .iter()
            .collect::<String>()
            .contains("debug_assertions");
        let mut lines = Vec::with_capacity(blank.len());
        let mut line = 1;
        for c in &blank {
            lines.push(line);
            if *c == '\n' {
                line += 1;
            }
        }
        let mut literals = BTreeMap::new();
        let mut opening = None;
        for (i, c) in blank.iter().enumerate() {
            if *c != '"' {
                continue;
            }
            if let Some(start) = opening.take() {
                literals.insert(
                    start,
                    (keep[start + 1..i].iter().collect::<String>(), lines[start]),
                );
            } else {
                opening = Some(i);
            }
        }
        let mut reads = BTreeSet::new();
        let mut non_literal = BTreeSet::new();
        // Each #[arg(...)] range, including nested delimiters but no string contents.
        let mut arg_ranges = Vec::new();
        for i in 0..blank.len() {
            if blank[i] != '#' {
                continue;
            }
            let bracket = spaces(&blank, i + 1);
            if blank.get(bracket) != Some(&'[') {
                continue;
            }
            let start = spaces(&blank, bracket + 1);
            let end = word_end(&blank, start);
            if blank[start..end].iter().collect::<String>() != "arg" {
                continue;
            }
            let mut depth = 1;
            for (at, c) in blank.iter().enumerate().skip(end) {
                if *c == '[' {
                    depth += 1;
                }
                if *c == ']' {
                    depth -= 1;
                }
                if depth == 0 {
                    arg_ranges.push((start, at));
                    break;
                }
            }
        }
        for i in 0..blank.len() {
            if !ident(blank[i]) || (i > 0 && ident(blank[i - 1])) {
                continue;
            }
            let end = word_end(&blank, i);
            let word: String = blank[i..end].iter().collect();
            let mut after = spaces(&blank, end);
            let mut direct = false;
            if word == "env" && blank.get(after..after + 2) == Some(&[':', ':'][..]) {
                let name_start = spaces(&blank, after + 2);
                let name_end = word_end(&blank, name_start);
                let name: String = blank[name_start..name_end].iter().collect();
                if name == "var" || name == "var_os" {
                    direct = true;
                    after = spaces(&blank, name_end);
                }
            }
            let before = i.checked_sub(1).and_then(|at| blank.get(at)).copied();
            let lookup = matches!(word.as_str(), "env" | "var" | "lookup" | "xdg")
                && !before.is_some_and(|c| ident(c) || c == '.' || c == ':');
            if (direct || lookup) && blank.get(after) == Some(&'(') {
                let argument = spaces(&blank, after + 1);
                if let Some((name, _)) = literals.get(&argument) {
                    reads.insert((name.clone(), lines[i]));
                } else if direct {
                    non_literal.insert(lines[i]);
                }
            }
            if word == "env"
                && blank.get(after) == Some(&'=')
                && arg_ranges
                    .iter()
                    .any(|(start, end)| *start <= i && i <= *end)
            {
                let argument = spaces(&blank, after + 1);
                if let Some((name, _)) = literals.get(&argument) {
                    reads.insert((name.clone(), lines[i]));
                }
            }
        }
        for (at, (name, line)) in &literals {
            if name.len() <= "OMADDONS_".len() || !name.starts_with("OMADDONS_") || !env_name(name)
            {
                continue;
            }
            let before: String = blank[..*at].iter().filter(|c| !c.is_whitespace()).collect();
            if !before.ends_with("env!(") && !before.ends_with("option_env!(") {
                reads.insert((name.clone(), *line));
            }
        }
        Some(Scanned {
            reads: reads.into_iter().collect(),
            non_literal: non_literal.into_iter().collect(),
            literals: literals.into_values().collect(),
            debug_gated,
        })
    }

    fn tree() -> &'static [(String, Scanned)] {
        static TREE: OnceLock<Vec<(String, Scanned)>> = OnceLock::new();
        TREE.get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            sources(&root.join("src"))
                .into_iter()
                .filter(|(path, _)| *path != root.join("src/contract/env.rs"))
                .filter_map(|(path, text)| {
                    scan(&text).map(|scanned| {
                        (
                            path.strip_prefix(root)
                                .unwrap()
                                .to_string_lossy()
                                .into_owned(),
                            scanned,
                        )
                    })
                })
                .collect()
        })
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum Fault {
        Unregistered,
        Harness,
        Seam,
    }

    /// The same checks run on planted cases and on every production source file.
    fn findings(path: &str, scanned: &Scanned) -> BTreeSet<(Fault, String, usize)> {
        let mut found = BTreeSet::new();
        for (name, line) in &scanned.reads {
            if get(name).is_none() {
                found.insert((Fault::Unregistered, name.clone(), *line));
            }
        }
        for (name, line) in &scanned.literals {
            if get(name).is_some_and(|row| row.kind == EnvKind::Harness) {
                found.insert((Fault::Harness, name.clone(), *line));
            }
        }
        if !LOOKUP_SEAMS.iter().any(|(seam, _)| *seam == path) {
            for line in &scanned.non_literal {
                found.insert((Fault::Seam, path.to_owned(), *line));
            }
        }
        found
    }

    #[test]
    fn the_tree_reads_only_registered_variables() {
        for (path, scanned) in tree() {
            for (fault, name, line) in findings(path, scanned) {
                assert_ne!(
                    fault,
                    Fault::Unregistered,
                    "{path}:{line} reads {name}, which ENV_VARS does not list: add a row (release, debug or harness) to src/contract/env.rs"
                );
            }
        }
    }

    #[test]
    fn planted_reads_are_found_or_ignored() {
        let cases = [
            (
                "direct_var_literal",
                "fn f() { let _ = std::env::var(\"OMADDONS_X\"); }",
                Some((Fault::Unregistered, "OMADDONS_X", 1)),
            ),
            (
                "direct_var_os_literal",
                "std::env::var_os(\"OMADDONS_X\")",
                Some((Fault::Unregistered, "OMADDONS_X", 1)),
            ),
            (
                "lookup_closure_env",
                "fn f(env: &dyn Fn(&str) -> Option<String>) { env(\"SOME_VAR\"); }",
                Some((Fault::Unregistered, "SOME_VAR", 1)),
            ),
            (
                "name_in_a_constant",
                "pub const V: &str = \"OMADDONS_X\";",
                Some((Fault::Unregistered, "OMADDONS_X", 1)),
            ),
            (
                "clap_env_attribute",
                "#[arg(long, env = \"OMADDONS_X\")] x: bool,",
                Some((Fault::Unregistered, "OMADDONS_X", 1)),
            ),
            (
                "after_a_non_ascii_literal",
                "\n\nlet _ = \"…\"; let _ = std::env::var(\"OMADDONS_X\");",
                Some((Fault::Unregistered, "OMADDONS_X", 3)),
            ),
            (
                "registered_name",
                "std::env::var_os(\"OMADDONS_NOW\")",
                None,
            ),
            (
                "setting_a_child_variable",
                "cmd.env(\"GIT_TERMINAL_PROMPT\", \"0\");",
                None,
            ),
            ("in_a_comment", "// std::env::var(\"OMADDONS_X\")", None),
            (
                "in_a_message",
                "Suggestion::text(\"unset OMADDONS_X, or set it\")",
                None,
            ),
            (
                "in_test_code",
                "#[cfg(test)] mod tests { fn t() { let _ = std::env::var(\"OMADDONS_X\"); } }",
                None,
            ),
            (
                "compile_time_value",
                "const B: &str = env!(\"OMADDONS_BUILD_ID\");",
                None,
            ),
            (
                "harness_name_in_the_binary",
                "let _ = std::env::var(\"OMADDONS_LIVE\");",
                Some((Fault::Harness, "OMADDONS_LIVE", 1)),
            ),
            (
                "non_literal_read_outside_a_seam",
                "fn f(name: &str) { let _ = std::env::var(name); }",
                Some((Fault::Seam, "src/new.rs", 1)),
            ),
        ];
        for (case, text, expected) in cases {
            let actual = scan(text)
                .map(|s| findings("src/new.rs", &s))
                .unwrap_or_default();
            let expected: BTreeSet<_> = expected
                .into_iter()
                .map(|(kind, name, line)| (kind, name.to_owned(), line))
                .collect();
            assert_eq!(
                actual, expected,
                "{case}: src/new.rs input {text:?}; add a registry row or declare the lookup seam with its reason"
            );
        }
        let text = "var(\"A_VAR\"); lookup(\"B_VAR\"); xdg(\"C_VAR\", \".x\");";
        let actual = findings("src/new.rs", &scan(text).unwrap());
        let expected = ["A_VAR", "B_VAR", "C_VAR"]
            .map(|name| (Fault::Unregistered, name.to_owned(), 1))
            .into_iter()
            .collect();
        assert_eq!(actual, expected, "lookup_closures_var_lookup_xdg: {text}");
        assert!(
            scan("#![cfg(test)]\nfn f() { std::env::var(\"OMADDONS_X\"); }").is_none(),
            "whole test-only files are excluded"
        );
        assert!(
            findings(
                "src/new.rs",
                &scan("const B: &str = option_env!(\"OMADDONS_BUILD_ID\");").unwrap()
            )
            .is_empty(),
            "option_env is also compile time"
        );
    }

    #[test]
    fn harness_names_never_appear_in_production_code() {
        for (path, scanned) in tree() {
            for (fault, name, line) in findings(path, scanned) {
                assert_ne!(
                    fault,
                    Fault::Harness,
                    "harness variable {name} appears in production code at {path}:{line}: the binary must never read it; if it does, make it a debug row"
                );
            }
        }
    }

    #[test]
    fn lookup_seams_are_declared() {
        for (path, scanned) in tree() {
            for (fault, _, line) in findings(path, scanned) {
                assert_ne!(
                    fault,
                    Fault::Seam,
                    "{path}:{line} reads a variable whose name is not a literal: declare it in LOOKUP_SEAMS with its reason, and add the lookup's name to the scan if new"
                );
            }
        }
        for (path, reason) in LOOKUP_SEAMS {
            assert!(!reason.is_empty(), "{path}: explain the lookup seam");
            assert!(
                tree()
                    .iter()
                    .any(|(found, scanned)| found == path && !scanned.non_literal.is_empty()),
                "{path} is a stale LOOKUP_SEAMS entry: remove it"
            );
        }
    }

    #[test]
    fn no_row_is_stale() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut harness = sources(&root.join("tests"))
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n");
        harness.push_str(&std::fs::read_to_string(root.join("Argcfile.sh")).unwrap());
        let mut pending = vec![root.join("scripts")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    harness.push_str(&std::fs::read_to_string(path).unwrap());
                }
            }
        }
        for row in ENV_VARS {
            let read = if row.kind == EnvKind::Harness {
                harness.contains(row.name)
            } else {
                tree()
                    .iter()
                    .any(|(_, scan)| scan.reads.iter().any(|(name, _)| name == row.name))
            };
            assert!(
                read,
                "ENV_VARS lists {} but nothing in {} reads it: remove the row",
                row.name,
                if row.kind == EnvKind::Harness {
                    "tests/ or the harness scripts"
                } else {
                    "src/"
                }
            );
        }
    }

    /// Heuristic: a file containing a debug read has a debug gate. Per-variable
    /// behavior tests prove that the gate covers the read, not this source scan.
    #[test]
    fn debug_rows_are_read_behind_debug_assertions() {
        for (path, scanned) in tree() {
            for (name, line) in &scanned.reads {
                if get(name).is_some_and(|row| row.kind == EnvKind::Debug) {
                    assert!(
                        scanned.debug_gated,
                        "{path}:{line} reads debug variable {name} with no debug_assertions gate: release builds must ignore it"
                    );
                }
            }
        }
    }

    #[test]
    fn rows_are_well_formed() {
        let mut command = Cli::command();
        command.build();
        let flags: BTreeSet<_> = command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
            .collect();
        let mut seen = BTreeSet::new();
        let mut previous = 0;
        for row in ENV_VARS {
            assert!(
                env_name(row.name) && seen.insert(row.name),
                "invalid or duplicate name: {row:?}"
            );
            let group = match row.kind {
                EnvKind::Release => 0,
                EnvKind::Debug => 1,
                EnvKind::Harness => 2,
            };
            assert!(group >= previous, "row kind out of order: {row:?}");
            previous = group;
            assert!(
                !row.description.is_empty()
                    && row.description.chars().count() <= 120
                    && !row.description.chars().any(char::is_control)
                    && !row.description.contains('`')
                    && !row.description.ends_with('.'),
                "invalid description: {row:?}"
            );
            if let Some(flag) = row.mirrors {
                assert_eq!(
                    row.kind,
                    EnvKind::Release,
                    "only release rows mirror flags: {row:?}"
                );
                assert!(
                    flags.contains(flag),
                    "{} mirrors unknown global flag {flag}",
                    row.name
                );
            }
        }
    }

    #[test]
    fn readme_lists_exactly_the_registered_variables() {
        let readme = include_str!("../../README.md");
        let section = readme
            .split_once("### Environment variables\n")
            .unwrap()
            .1
            .lines()
            .take_while(|line| !line.starts_with("---") && !line.starts_with("## "))
            .collect::<Vec<_>>()
            .join("\n");
        let documented: BTreeSet<_> = section
            .split('`')
            .skip(1)
            .step_by(2)
            .map(|span| span.split('=').next().unwrap())
            .filter(|name| name.len() > 1 && env_name(name))
            .collect();
        let registered: BTreeSet<_> = ENV_VARS.iter().map(|row| row.name).collect();
        assert_eq!(
            documented,
            registered,
            "Environment variables section disagrees with ENV_VARS; missing from README: {:?}; extra in README: {:?}",
            registered.difference(&documented).collect::<Vec<_>>(),
            documented.difference(&registered).collect::<Vec<_>>()
        );
    }

    #[test]
    fn release_rows_are_what_capabilities_publishes() {
        let names: Vec<_> = release().map(|row| row.name).collect();
        assert_eq!(
            names,
            [
                "OMADDONS_ACTOR",
                "OMADDONS_OFFLINE",
                "OMADDONS_FORMAT",
                "OMADDONS_GITHUB_TOKEN",
                "NO_COLOR",
                "CI",
                "TERM",
                "HOME",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
                "XDG_DATA_HOME",
                "OMARCHY_PATH",
                "PATH"
            ],
            "release rows in contract order"
        );
        assert_eq!(get("OMADDONS_OFFLINE").unwrap().mirrors, Some("--offline"));
        assert_eq!(get("NOPE"), None);
    }

    #[test]
    fn rows_serialize_as_capabilities_shows_them() {
        assert_eq!(
            serde_json::to_value(get("OMADDONS_OFFLINE")).unwrap(),
            serde_json::json!({"name":"OMADDONS_OFFLINE", "kind":"release", "description":"1 (exactly) means --offline", "mirrors":"--offline"})
        );
    }
}
