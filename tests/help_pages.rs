//! e2e suite: every command path's help pages, the net under the `--help` overhaul (launch-week fixes, help subtask 1). Every multi-command scenario is a TestLog scenario.
//!
//! For every visible command path (bare `omaddons`, each command and each nested one such
//! as `backup create`, `doctor fix` or `auth github`), the binary's `-h` and `--help` pages
//! are snapshotted together, one insta snapshot per path
//! (`tests/snapshots/integration__help_pages__<path>.snap`), rendered as a terminal 100
//! columns wide with color off (`COLUMNS=100`, `NO_COLOR=1`) in an isolated home. Bare
//! `omaddons`'s snapshot holds its bare page too. `help <path>` must print exactly what
//! `<path> --help` prints.
//!
//! Three checks run over every page, each with a shrink-only allowlist holding today's
//! violators ([`ALLOWLIST`], one `<check> | <command line>` per line):
//!
//! - `width`: no line over 100 columns, and none over 80 on bare help (`omaddons` and
//!   `omaddons -h`);
//! - `ansi`: no ESC byte, since color is off;
//! - `hygiene`: no developer note (a bead id, a decision id, `TODO`, a source path,
//!   `unwrap`) on a page, or in any about, long about, before or after text, argument help
//!   or value help of the command.
//!
//! A new violator fails the test, and so does an entry that no longer violates: the list
//! only shrinks. A path with no accepted snapshot fails naming the path, and a snapshot
//! file no path renders any more fails naming the file.

use crate::common;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use common::env::TestEnv;
use common::tlog::{TestLog, tlog};

/// The terminal width the pages are rendered at, and the widest line a page may have.
const PAGE_MAX_COLUMNS: usize = 100;
/// The widest line bare help (`omaddons`, `omaddons -h`) may have.
const BARE_MAX_COLUMNS: usize = 80;
/// The allowlist of today's violators, relative to the crate root.
const ALLOWLIST: &str = "tests/help_pages_allowlist.txt";
/// What every snapshot file of this suite starts with: the test binary's module path.
const SNAPSHOT_PREFIX: &str = "integration__help_pages__";
/// The snapshot directory, relative to the crate root.
const SNAPSHOT_DIR: &str = "tests/snapshots";
/// Developer notes a page must never show, as plain substrings: a bead id, a decision id
/// of the older form, a note to self, a source path, and the panic helpers. Decision ids
/// of the milestone form (`M4-D55`) are found by [`decision_id`].
const LEAKS: &[&str] = &["omad-", "DEC-", "TODO", "FIXME", "src/", "unwrap"];
/// The checks an allowlist line may name.
const CHECKS: &[&str] = &["width", "ansi", "hygiene"];

const RE_ACCEPT: &str = "a command's -h and --help pages as this build renders them at 100 columns without color; any change to an about, a flag, a value, a heading or the layout changes them: review with cargo insta review, or re-accept with INSTA_UPDATE=always cargo test --test integration help_pages:: and review git diff tests/snapshots/";

/// Every visible command path, depth first in the command tree's order: `[]` (bare
/// `omaddons`) first.
fn command_paths() -> Vec<Vec<String>> {
    fn walk(cmd: &clap::Command, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            path.push(sub.get_name().to_owned());
            out.push(path.clone());
            walk(sub, path, out);
            path.pop();
        }
    }
    let mut root = omaddons::cli::Cli::command();
    root.build();
    let mut out = vec![Vec::new()];
    walk(&root, &mut Vec::new(), &mut out);
    out
}

/// The command at `path` in the built tree.
fn command_at(root: &clap::Command, path: &[String]) -> clap::Command {
    let mut cmd = root.clone();
    for word in path {
        let Some(sub) = cmd.find_subcommand(word).cloned() else {
            panic!("{word:?} of {path:?} is not in the command tree"); // ubs:ignore — the walk's own paths
        };
        cmd = sub;
    }
    cmd
}

/// `omaddons <path>`, as an allowlist line and a message name it.
fn command_line(path: &[String]) -> String {
    std::iter::once("omaddons")
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The snapshot's name: the path's words joined by `_` (no command name holds one), and
/// `omaddons` for bare help (no command is called that).
fn snapshot_name(path: &[String]) -> String {
    if path.is_empty() {
        "omaddons".to_owned()
    } else {
        path.join("_")
    }
}

/// One page a path's snapshot holds, and the widest line it may have.
struct Page {
    /// The arguments after `omaddons`.
    argv: Vec<String>,
    text: String,
    max_columns: usize,
}

/// The pages of `path`: bare `omaddons`, `-h` and `--help` for the root, `-h` and
/// `--help` for every command.
fn page_argvs(path: &[String]) -> Vec<(Vec<String>, usize)> {
    let with = |flag: &str| {
        let mut argv = path.to_vec();
        argv.push(flag.to_owned());
        argv
    };
    if path.is_empty() {
        vec![
            (Vec::new(), BARE_MAX_COLUMNS),
            (with("-h"), BARE_MAX_COLUMNS),
            (with("--help"), PAGE_MAX_COLUMNS),
        ]
    } else {
        vec![
            (with("-h"), PAGE_MAX_COLUMNS),
            (with("--help"), PAGE_MAX_COLUMNS),
        ]
    }
}

/// A path's snapshot: each page under a marker line naming the command line that printed
/// it.
fn snapshot_text(pages: &[Page]) -> String {
    let mut text = String::new();
    for page in pages {
        let _ = writeln!(text, "==> {} <==", command_line(&page.argv));
        text.push_str(&page.text);
    }
    text
}

/// Runs `omaddons <argv>` as a 100-column terminal without color would, and returns
/// stdout; a run that fails or writes to stderr is a problem.
fn render(log: &TestLog, env: &TestEnv, argv: &[String], problems: &mut Vec<String>) -> String {
    render_at(log, env, argv, PAGE_MAX_COLUMNS, problems)
}

/// [`render`] as a terminal `columns` wide.
fn render_at(
    log: &TestLog,
    env: &TestEnv,
    argv: &[String],
    columns: usize,
    problems: &mut Vec<String>,
) -> String {
    let ran = log
        .cmd(env, argv)
        .env("COLUMNS", columns.to_string())
        .env("NO_COLOR", "1")
        .run();
    if ran.run.code != 0 || !ran.stderr().is_empty() {
        problems.push(format!(
            "{}: exit {}, stderr {:?}",
            command_line(argv),
            ran.run.code,
            ran.stderr()
        ));
    }
    ran.stdout().to_owned()
}

/// The lines of `text` wider than `max` columns, as `(line number, columns)`. A column is
/// a character: help text holds no wide character, and `·` is one column, two bytes.
fn wide_lines(text: &str, max: usize) -> Vec<(usize, usize)> {
    text.lines()
        .enumerate()
        .map(|(at, line)| (at + 1, line.chars().count()))
        .filter(|(_, columns)| *columns > max)
        .collect()
}

/// Whether `text` holds an ESC byte.
fn has_escape(text: &str) -> bool {
    text.contains('\u{1b}')
}

/// The first decision id of the milestone form in `text`: `M`, digits, `-D`, a digit
/// (`M4-D55`).
fn decision_id(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for (at, _) in text.match_indices('M') {
        let digits = bytes[at + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let rest = &bytes[at + 1 + digits..];
        if digits > 0 && rest.len() > 2 && rest.starts_with(b"-D") && rest[2].is_ascii_digit() {
            let end = at + 1 + digits + 2;
            let id_digits = bytes[end..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            return Some(text[at..end + id_digits].to_owned());
        }
    }
    None
}

/// The developer notes in `text`, each as the substring found.
fn leaks(text: &str) -> Vec<String> {
    let mut found: Vec<String> = LEAKS
        .iter()
        .filter(|leak| text.contains(*leak))
        .map(|leak| (*leak).to_owned())
        .collect();
    found.extend(decision_id(text));
    found
}

/// Every text the command's own definition carries: its abouts, before and after texts,
/// and each argument's help and each possible value's help (hidden ones included: a
/// developer note is one wherever it sits).
fn definition_texts(cmd: &clap::Command) -> Vec<String> {
    let mut texts: Vec<String> = [
        cmd.get_about(),
        cmd.get_long_about(),
        cmd.get_before_help(),
        cmd.get_before_long_help(),
        cmd.get_after_help(),
        cmd.get_after_long_help(),
    ]
    .into_iter()
    .flatten()
    .map(ToString::to_string)
    .collect();
    for arg in cmd.get_arguments() {
        texts.extend(
            [arg.get_help(), arg.get_long_help()]
                .into_iter()
                .flatten()
                .map(ToString::to_string),
        );
        texts.extend(
            arg.get_possible_values()
                .iter()
                .filter_map(|value| value.get_help().map(ToString::to_string)),
        );
    }
    texts
}

/// The checks `path`'s pages and definition fail, as allowlist lines (`<check> | <command
/// line>`), each with what it found for the failure message.
fn violations(path: &[String], pages: &[Page], cmd: &clap::Command) -> Vec<(String, String)> {
    let line = command_line(path);
    let mut found = Vec::new();
    let wide: Vec<String> = pages
        .iter()
        .flat_map(|page| {
            wide_lines(&page.text, page.max_columns)
                .into_iter()
                .map(move |(at, columns)| {
                    format!(
                        "`{}` line {at}: {columns} columns, over {}",
                        command_line(&page.argv),
                        page.max_columns
                    )
                })
        })
        .collect();
    if !wide.is_empty() {
        found.push((format!("width | {line}"), wide.join("; ")));
    }
    let escaped: Vec<String> = pages
        .iter()
        .filter(|page| has_escape(&page.text))
        .map(|page| format!("`{}` holds an ESC byte", command_line(&page.argv)))
        .collect();
    if !escaped.is_empty() {
        found.push((format!("ansi | {line}"), escaped.join("; ")));
    }
    let mut leaked = BTreeSet::new();
    for text in pages
        .iter()
        .map(|page| page.text.clone())
        .chain(definition_texts(cmd))
    {
        leaked.extend(leaks(&text));
    }
    if !leaked.is_empty() {
        let leaked: Vec<String> = leaked.into_iter().collect();
        found.push((
            format!("hygiene | {line}"),
            format!("developer notes: {}", leaked.join(", ")),
        ));
    }
    found
}

/// The allowlist's entries, or every malformed line: an entry is `<check> | <command
/// line>`, the check one of [`CHECKS`] and the command line starting with `omaddons`.
/// Comments (`#`) and blank lines are skipped; an entry listed twice is malformed.
fn parse_allowlist(text: &str) -> Result<BTreeSet<String>, Vec<String>> {
    let mut entries = BTreeSet::new();
    let mut problems = Vec::new();
    for (at, raw) in text.lines().enumerate() {
        let entry = raw.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let well_formed = entry.split_once(" | ").is_some_and(|(check, line)| {
            CHECKS.contains(&check)
                && (line == "omaddons" || line.starts_with("omaddons "))
                && line.trim() == line
        });
        if !well_formed {
            problems.push(format!(
                "line {}: `{raw}` is not `<check> | omaddons <path>` with a check among {CHECKS:?}",
                at + 1
            ));
        } else if !entries.insert(entry.to_owned()) {
            problems.push(format!("line {}: `{raw}` is listed twice", at + 1));
        }
    }
    if problems.is_empty() {
        Ok(entries)
    } else {
        Err(problems)
    }
}

/// Compares what the checks found (`(entry, detail)`) with the allowlist: `Err` names
/// every malformed line, every new violator and every entry that no longer violates.
fn compare(found: &[(String, String)], allowlist: &str) -> Result<(), String> {
    let allowed = parse_allowlist(allowlist).map_err(|problems| problems.join("\n"))?;
    let violating: BTreeSet<&str> = found.iter().map(|(entry, _)| entry.as_str()).collect();
    let mut problems: Vec<String> = found
        .iter()
        .filter(|(entry, _)| !allowed.contains(entry))
        .map(|(entry, detail)| {
            format!("new violation `{entry}`: {detail}; fix the page, never list it in {ALLOWLIST}")
        })
        .collect();
    problems.extend(
        allowed
            .iter()
            .filter(|entry| !violating.contains(entry.as_str()))
            .map(|entry| {
                format!("`{entry}` no longer violates: delete its line from {ALLOWLIST} (the list only shrinks)")
            }),
    );
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// The snapshot files of this suite, by snapshot name, or why `dir` could not be listed.
fn snapshot_files(dir: &Path) -> Result<BTreeSet<String>, String> {
    let unreadable = |error: std::io::Error| format!("listing {}: {error}", dir.display());
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let name = entry.map_err(unreadable)?.file_name();
        let name = name.to_string_lossy();
        if let Some(snapshot) = name
            .strip_prefix(SNAPSHOT_PREFIX)
            .and_then(|rest| rest.strip_suffix(".snap"))
        {
            names.insert(snapshot.to_owned());
        }
    }
    Ok(names)
}

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn every_command_path_s_help_pages() {
    let log = tlog!();
    let env = TestEnv::new();
    log.env(&env);
    log.tree("before", env.root());
    let mut root = omaddons::cli::Cli::command();
    root.build();
    let paths = command_paths();
    log.note(format!("{} command paths", paths.len()));

    let mut problems = Vec::new();
    let mut changed = Vec::new();
    let mut unequal = Vec::new();
    let mut found = Vec::new();
    for path in &paths {
        let line = command_line(path);
        log.step(&line);
        let pages: Vec<Page> = page_argvs(path)
            .into_iter()
            .map(|(argv, max_columns)| Page {
                text: render(&log, &env, &argv, &mut problems),
                argv,
                max_columns,
            })
            .collect();
        log.note(
            pages
                .iter()
                .map(|page| {
                    format!(
                        "`{}`: {} lines, {} bytes",
                        command_line(&page.argv),
                        page.text.lines().count(),
                        page.text.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(" · "),
        );

        // `help <path>` is `<path> --help`, byte for byte.
        let mut asked = vec!["help".to_owned()];
        asked.extend(path.iter().cloned());
        let via_help = render(&log, &env, &asked, &mut problems);
        let Some(long) = pages
            .iter()
            .find(|page| page.argv.last().is_some_and(|f| f == "--help"))
        else {
            panic!("{line}: no --help page"); // ubs:ignore — page_argvs always adds one
        };
        if via_help != long.text {
            unequal.push(format!(
                "`omaddons help {}` differs from `{line} --help`",
                path.join(" ")
            ));
        }

        found.extend(violations(path, &pages, &command_at(&root, path)));

        let name = snapshot_name(path);
        let text = snapshot_text(&pages);
        let accepted = catch_unwind(AssertUnwindSafe(|| {
            insta::with_settings!({ description => RE_ACCEPT, omit_expression => true }, {
                insta::assert_snapshot!(name.as_str(), text);
            });
        }));
        if accepted.is_err() {
            changed.push(line);
        }
    }

    // Every section is reported at once, so a change that moves pages and breaks a check
    // shows both in one run.
    let mut failures = Vec::new();
    log.step("every page rendered");
    log.json("runs that failed or wrote to stderr", &problems);
    if !problems.is_empty() {
        failures.push(format!("help runs that failed:\n{}", problems.join("\n")));
    }
    log.json("help <path> differing from <path> --help", &unequal);
    if !unequal.is_empty() {
        failures.push(unequal.join("\n"));
    }
    log.json("paths whose snapshot is new or changed", &changed);
    if !changed.is_empty() {
        failures.push(format!(
            "the help pages of these paths are new or changed (each diff is above): {}\n{RE_ACCEPT}",
            changed.join(", ")
        ));
    }

    log.step("snapshot files");
    let expected: BTreeSet<String> = paths.iter().map(|path| snapshot_name(path)).collect();
    let mut files = Vec::new();
    match snapshot_files(&repo(SNAPSHOT_DIR)) {
        Err(unreadable) => files.push(unreadable),
        Ok(on_disk) => {
            for path in &paths {
                if !on_disk.contains(&snapshot_name(path)) {
                    files.push(format!(
                        "`{}` has no accepted help snapshot ({SNAPSHOT_DIR}/{SNAPSHOT_PREFIX}{}.snap): {RE_ACCEPT}",
                        command_line(path),
                        snapshot_name(path)
                    ));
                }
            }
            for orphan in on_disk.difference(&expected) {
                files.push(format!(
                    "{SNAPSHOT_DIR}/{SNAPSHOT_PREFIX}{orphan}.snap: no command path renders this page any more; delete it"
                ));
            }
        }
    }
    log.json("snapshot files missing or orphaned", &files);
    if !files.is_empty() {
        failures.push(files.join("\n"));
    }

    log.step("width, ansi and hygiene against the allowlist");
    let entries: Vec<&String> = found.iter().map(|(entry, _)| entry).collect();
    log.json("violations", &entries);
    match std::fs::read_to_string(repo(ALLOWLIST)) {
        Ok(allowlist) => {
            if let Err(message) = compare(&found, &allowlist) {
                failures.push(message);
            }
        }
        Err(error) => failures.push(format!("reading {ALLOWLIST}: {error}")),
    }

    log.step("help wrote nothing");
    log.assert_unchanged("before", env.root());
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A command's pages wrap to the terminal, never wider than 100 columns: on a terminal
/// 200 columns wide no line passes 100, and on one 60 wide every line fits, the examples'
/// descriptions and the doctor's exit meanings continuing under their own column. Bare
/// help keeps its fixed 80-column layout, so it is left out.
#[test]
fn pages_wrap_to_the_terminal_capped_at_100() {
    let log = tlog!();
    let env = TestEnv::new();
    let mut problems = Vec::new();
    let mut wide = Vec::new();
    for path in command_paths().into_iter().filter(|path| !path.is_empty()) {
        log.step(&command_line(&path));
        for flag in ["-h", "--help"] {
            let mut argv = path.clone();
            argv.push(flag.to_owned());
            for (columns, max) in [(200, PAGE_MAX_COLUMNS), (60, 60)] {
                let text = render_at(&log, &env, &argv, columns, &mut problems);
                let lines: Vec<&str> = text.lines().collect();
                for (at, width) in wide_lines(&text, max) {
                    // One word wider than the terminal (a long path) cannot break: it
                    // stands on a line of its own, as clap leaves it.
                    let line = lines.get(at - 1).copied().unwrap_or_default();
                    if columns == 60 && line.split_whitespace().count() == 1 {
                        continue;
                    }
                    wide.push(format!(
                        "`{}` at COLUMNS={columns}: line {at} is {width} columns, over {max}",
                        command_line(&argv)
                    ));
                }
            }
        }
    }
    log.json("runs that failed", &problems);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    log.json("lines over the width", &wide);
    assert!(wide.is_empty(), "{}", wide.join("\n"));

    log.step("hanging indents at 60 columns");
    let mut narrow = |argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        render_at(&log, &env, &argv, 60, &mut problems)
    };
    let status = narrow(&["status", "-h"]);
    let examples: Vec<&str> = status
        .split("Examples:\n")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take_while(|line| !line.is_empty())
        .collect();
    assert!(
        examples.len() > 3,
        "status -h's examples wrap at 60:\n{status}"
    );
    for line in &examples {
        assert!(line.starts_with("  "), "{line:?} hangs:\n{status}");
    }
    let doctor = narrow(&["doctor", "--help"]);
    let table: Vec<&str> = doctor
        .split("The doctor exits with its own codes:\n")
        .nth(1)
        .unwrap_or_default()
        .lines()
        .take_while(|line| !line.is_empty())
        .collect();
    assert!(table.len() > 11, "the exit table wraps at 60:\n{doctor}");
    for line in &table {
        assert!(line.starts_with("  "), "{line:?} hangs:\n{doctor}");
    }
}

/// Each check catches a planted fault, and passes the same page without it.
#[test]
fn each_check_catches_a_planted_fault() {
    let path = vec!["status".to_owned()];
    let cmd = clap::Command::new("status").about("Triage every client");
    let page = |text: &str, max_columns: usize| Page {
        argv: vec!["status".to_owned(), "--help".to_owned()],
        text: text.to_owned(),
        max_columns,
    };
    let clean = "Triage every client\n\nUsage: omaddons status [OPTIONS]\n";
    let wide = format!("{clean}{}\n", "x".repeat(101));
    let cases: [(&str, String, usize, &[&str]); 9] = [
        ("a clean page", clean.to_owned(), 100, &[]),
        (
            "101 columns",
            wide.clone(),
            100,
            &["width | omaddons status"],
        ),
        ("100 columns", format!("{}\n", "x".repeat(100)), 100, &[]),
        (
            "81 columns on bare help",
            format!("{}\n", "x".repeat(81)),
            80,
            &["width | omaddons status"],
        ),
        (
            "a two-byte dot is one column",
            format!("{}\n", "·".repeat(100)),
            100,
            &[],
        ),
        (
            "an ESC byte",
            format!("\u{1b}[1mUsage\u{1b}[0m\n{clean}"),
            100,
            &["ansi | omaddons status"],
        ),
        (
            "a bead id",
            format!("{clean}see omad-xyz\n"),
            100,
            &["hygiene | omaddons status"],
        ),
        (
            "a decision id",
            format!("{clean}(M4-D55)\n"),
            100,
            &["hygiene | omaddons status"],
        ),
        (
            "a source path",
            format!("{clean}src/cli.rs\n"),
            100,
            &["hygiene | omaddons status"],
        ),
    ];
    for (case, text, max_columns, expected) in cases {
        let got: Vec<String> = violations(&path, &[page(&text, max_columns)], &cmd)
            .into_iter()
            .map(|(entry, _)| entry)
            .collect();
        assert_eq!(got, expected, "{case}: page {text:?}");
    }
    // A note in the command's definition fails though no page shows it.
    let noted = clap::Command::new("status")
        .about("Triage every client")
        .arg(
            clap::Arg::new("deep")
                .long("deep")
                .hide(true)
                .help("TODO: drop after M4-D12"),
        );
    let got: Vec<(String, String)> = violations(&path, &[page(clean, 100)], &noted);
    assert_eq!(
        got,
        [(
            "hygiene | omaddons status".to_owned(),
            "developer notes: M4-D12, TODO".to_owned()
        )],
        "a hidden argument's help"
    );
}

#[test]
fn decision_ids_are_found_by_shape() {
    for (text, expected) in [
        ("see M4-D55 for why", Some("M4-D55")),
        ("M12-D3", Some("M12-D3")),
        ("Mists of Pandaria", None),
        ("M4-Dx", None),
        ("M-D4", None),
        ("ends in M4-D", None),
        ("MM4-D1", Some("M4-D1")),
    ] {
        assert_eq!(decision_id(text).as_deref(), expected, "{text:?}");
    }
}

/// What the checks found: `(allowlist entry, detail)` each.
type Found = Vec<(String, String)>;

/// The allowlist only shrinks: a new violator, an entry that no longer violates and a
/// malformed line each fail, naming themselves.
#[test]
fn the_allowlist_comparison_names_every_difference() {
    let found = |entries: &[&str]| -> Found {
        entries
            .iter()
            .map(|entry| ((*entry).to_owned(), "detail".to_owned()))
            .collect()
    };
    // (case, what the checks found, the allowlist, the parts of the failure, or None).
    let cases: [(&str, Found, &str, Option<&[&str]>); 7] = [
        (
            "equal",
            found(&["width | omaddons doctor"]),
            "# c\n\nwidth | omaddons doctor\n",
            None,
        ),
        ("both empty", found(&[]), "# only comments\n", None),
        (
            "a new violator",
            found(&["ansi | omaddons status"]),
            "",
            Some(&["new violation `ansi | omaddons status`: detail"]),
        ),
        (
            "a stale entry",
            found(&[]),
            "hygiene | omaddons doctor fix\n",
            Some(&["`hygiene | omaddons doctor fix` no longer violates"]),
        ),
        (
            "an unknown check",
            found(&[]),
            "colour | omaddons status\n",
            Some(&["line 1: `colour | omaddons status` is not"]),
        ),
        (
            "no command line",
            found(&[]),
            "width | status\n",
            Some(&["line 1: `width | status` is not"]),
        ),
        (
            "listed twice",
            found(&["width | omaddons"]),
            "width | omaddons\nwidth | omaddons\n",
            Some(&["line 2: `width | omaddons` is listed twice"]),
        ),
    ];
    for (case, found, allowlist, expected) in cases {
        let result = compare(&found, allowlist);
        match expected {
            None => assert_eq!(result, Ok(()), "{case}"),
            Some(parts) => {
                let message = result.expect_err(case);
                for part in parts {
                    assert!(message.contains(part), "{case}: {message:?} lacks {part:?}");
                }
            }
        }
    }
}

/// The walk reaches the root, every command, the nested commands and the parents of
/// nested commands, once each.
#[test]
fn the_walk_reaches_every_command_path() {
    let paths: Vec<String> = command_paths()
        .iter()
        .map(|path| command_line(path))
        .collect();
    let unique: BTreeSet<&String> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len(), "each path once: {paths:?}");
    for expected in [
        "omaddons",
        "omaddons status",
        "omaddons backup",
        "omaddons backup create",
        "omaddons doctor fix",
        "omaddons auth",
        "omaddons auth github",
        "omaddons plans",
        "omaddons plans show",
        "omaddons targets rm",
        "omaddons robot-docs install-skill",
        "omaddons help",
    ] {
        assert!(
            paths.iter().any(|p| p == expected),
            "{expected} in {paths:?}"
        );
    }
    // Every command the registry names is a path the walk reaches.
    for row in omaddons::app::COMMANDS {
        let line = format!("omaddons {}", row.name);
        assert!(paths.contains(&line), "{line} (COMMANDS) in {paths:?}");
    }
}
