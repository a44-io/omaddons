//! e2e suite: omaddons completions (A7, M4-D16, checkpoint criterion 15) and the values its scripts ask `omaddons __complete` for (omad-launch-week-fixes-bmcx.10). Every multi-command scenario is a TestLog scenario.
//!
//! Each shell's script is snapshot-tested through the real binary. bash checks its own
//! script (`bash -n`) and runs its completion function over the whole command tree in
//! every gate, and never skips. Generating a script is pure (no state read, no write, no
//! request, no home needed) and gives the same bytes on every terminal. The bash, zsh and
//! fish scripts call back into the binary only as `omaddons __complete`, which offers
//! client ids, addon names and ids from omaddons' own files, reads nothing else, writes
//! nothing, takes no lock and prints nothing it cannot offer; bash completes them here
//! from a home with clients, addons, plans, a backup, history and a doctor run. zsh's and
//! fish's syntax checks are checkpoint tests (`--ignored`), which skip only where their
//! shell is missing.

use crate::common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use clap::CommandFactory;
use common::archives::zip_text;
use common::checkpoint;
use common::env::TestEnv;
use common::home::Home;
use common::http::paths;
use common::netlog::net_log;
use common::privacy::assert_private_all;
use common::shells::{self, Checker};
use common::tlog::{Ran, TestLog, tlog};
use common::wtf::Wtf;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SHELLS: [&str; 5] = ["bash", "zsh", "fish", "elvish", "powershell"];

const RE_ACCEPT: &str = "the completion script of this build; any change to a command, flag, alias, about text or fixed value changes it: re-accept with INSTA_UPDATE=always cargo test --test integration completions::, then review git diff tests/snapshots/";

/// `omaddons completions <shell>` on `env`, which must succeed with nothing on stderr.
#[track_caller]
fn script_of(log: &TestLog, env: &TestEnv, shell: &str) -> String {
    let ran = log.run(env, &["completions", shell]);
    ran.ok();
    log.assert_eq("stderr", &ran.stderr(), &"");
    ran.stdout().to_owned()
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The envelope without `meta` (its elapsed time differs between runs).
fn without_meta(ran: &Ran) -> Value {
    let mut envelope = ran.envelope().clone();
    if let Some(map) = envelope.as_object_mut() {
        map.remove("meta");
    }
    envelope
}

// ---------------------------------------------------------------------------------------
// 1. The five snapshots.

fn snapshot_case(log: &TestLog, shell: &str) {
    let env = TestEnv::new();
    log.step("generate");
    let script = script_of(log, &env, shell);
    log.note(format!(
        "{shell}: {} bytes, {} lines, sha256 {}",
        script.len(),
        script.lines().count(),
        sha256(&script)
    ));
    insta::with_settings!({ description => RE_ACCEPT, omit_expression => true }, {
        insta::assert_snapshot!(shell, script);
    });
}

#[test]
fn bash_script_matches_its_snapshot() {
    snapshot_case(&tlog!(), "bash");
}

#[test]
fn zsh_script_matches_its_snapshot() {
    snapshot_case(&tlog!(), "zsh");
}

#[test]
fn fish_script_matches_its_snapshot() {
    snapshot_case(&tlog!(), "fish");
}

#[test]
fn elvish_script_matches_its_snapshot() {
    snapshot_case(&tlog!(), "elvish");
}

#[test]
fn powershell_script_matches_its_snapshot() {
    snapshot_case(&tlog!(), "powershell");
}

// ---------------------------------------------------------------------------------------
// 2. The JSON form.

#[test]
fn the_json_form_carries_the_same_script() {
    let log = tlog!();
    let env = TestEnv::new();
    for shell in SHELLS {
        log.step(shell);
        let human = script_of(&log, &env, shell);
        let ran = log.run(&env, &["completions", shell, "--json"]);
        ran.ok();
        let envelope = ran.envelope();
        for (key, expected) in [
            ("ok", json!(true)),
            ("command", json!("completions")),
            ("exit_code", json!(0)),
            ("schema_version", json!(omaddons::output::SCHEMA_VERSION)),
            ("warnings", json!([])),
            ("commands", json!([])),
        ] {
            log.assert_eq(key, &envelope[key], &expected);
        }
        let order = format!(r#""data":{{"shell":"{shell}","script":"#);
        log.assert_eq(
            "data's keys in order",
            &ran.stdout().contains(&order),
            &true,
        );
        let keys: BTreeSet<String> = ran
            .data()
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let two = BTreeSet::from(["script".to_owned(), "shell".to_owned()]);
        log.assert_eq("data's keys", &keys, &two);
        let script = ran.data()["script"].as_str().unwrap_or_default();
        log.assert_eq(
            "data.script is the human output, byte for byte",
            &(script == human),
            &true,
        );
        let json_form = without_meta(&ran);
        for spelling in [&["--robot"][..], &["--format", "json"]] {
            let argv: Vec<&str> = ["completions", shell]
                .iter()
                .chain(spelling)
                .copied()
                .collect();
            let other = log.run(&env, &argv);
            other.ok();
            log.assert_eq(
                &format!("{spelling:?} gives the same envelope"),
                &(without_meta(&other) == json_form),
                &true,
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3 and 4. bash's syntax check.

#[test]
fn bash_accepts_its_script() {
    let log = tlog!();
    let env = TestEnv::new();
    let bash = shells::find("bash");
    assert!(
        bash.is_some(),
        "bash is required by this test and is never skipped (M4 checkpoint criterion 15)"
    );
    log.step("generate");
    let script = script_of(&log, &env, "bash");
    log.step("bash -n");
    let dir = env.root().join("bash-check");
    fs::create_dir_all(&dir).unwrap();
    let checked = shells::syntax_check(Checker::Bash, &script, &dir);
    let run = match &checked {
        Ok(run) | Err(run) => run,
    };
    log.json("bash -n", run);
    log.assert_eq("bash -n exit", &run.code, &Some(0));
}

#[test]
fn the_syntax_check_rejects_a_broken_script() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("generate");
    let script = script_of(&log, &env, "bash");
    let lines: Vec<&str> = script.lines().collect();
    let half = lines[..lines.len() / 2].join("\n") + "\n";
    for (case, text) in [
        ("if then fi", "if then fi\n".to_owned()),
        ("half the script", half),
    ] {
        log.step(case);
        let dir = env
            .root()
            .join(format!("broken-{}", case.replace(' ', "-")));
        fs::create_dir_all(&dir).unwrap();
        let checked = shells::syntax_check(Checker::Bash, &text, &dir);
        let run = match &checked {
            Ok(run) | Err(run) => run,
        };
        log.json("bash -n", run);
        log.assert_eq("rejected", &checked.is_err(), &true);
        log.assert_eq(
            "a non-zero exit",
            &matches!(run.code, Some(c) if c != 0),
            &true,
        );
        log.assert_eq(
            "stderr names a syntax error",
            &run.stderr.contains("syntax error"),
            &true,
        );
    }
}

// ---------------------------------------------------------------------------------------
// 5. bash's completion function over the whole command tree.

/// One command line to complete, and what its offers must hold.
#[derive(serde::Serialize)]
struct Case {
    words: Vec<String>,
    expected: Vec<String>,
    /// The offers must equal `expected`, not only include it.
    exact: bool,
    offered: Vec<String>,
}

fn case(words: &[&str], expected: &[&str], exact: bool) -> Case {
    Case {
        words: words.iter().map(|w| (*w).to_owned()).collect(),
        expected: expected.iter().map(|w| (*w).to_owned()).collect(),
        exact,
        offered: Vec::new(),
    }
}

/// Every case the bead names, then the walk of the clap tree: every command path, every
/// visible top-level alias, every global long flag and every fixed value.
fn completion_cases() -> Vec<Case> {
    let shells: Vec<&str> = SHELLS.to_vec();
    let mut cases = vec![
        case(&["st"], &["status"], false),
        case(&["comp"], &["completions"], false),
        case(&["completions", ""], &shells, false),
        case(&["--format", ""], &["json", "human"], false),
        case(&["--progress", ""], &["json", "human", "none"], false),
        case(&["backup", "re"], &["restore"], false),
        case(&["auth", ""], &["github"], false),
        case(&["auth", "github", ""], &["status", "set", "clear"], false),
        case(
            &["search", "x", "--source", ""],
            &["catalog", "wowi", "tukui", "github"],
            false,
        ),
        case(
            &["omarchy", "install", "--section", ""],
            &["left", "center", "right"],
            false,
        ),
        case(&["install", "--dry"], &["--dry-run"], false),
        // No client is registered yet: the one the discovery cache holds, by the id `-t`
        // resolves for it (omad-ivbj), then file names, and the directory is empty.
        case(&["status", "-t", ""], &["retail"], true),
        // The reader is no command: nothing offers it.
        case(&["__"], &[], true),
    ];

    for row in omaddons::app::COMMANDS {
        let words: Vec<&str> = row.name.split(' ').collect();
        let Some((last, parent)) = words.split_last() else {
            continue;
        };
        let mut typed = parent.to_vec();
        typed.push("");
        cases.push(case(&typed, &[*last], false));
    }

    let mut root = omaddons::cli::Cli::command();
    root.build();
    for sub in root.get_subcommands() {
        for alias in sub.get_visible_aliases() {
            cases.push(case(&[""], &[alias], false));
        }
    }
    let globals: Vec<String> = root
        .get_arguments()
        .filter(|a| a.is_global_set() && !a.is_hide_set())
        .filter_map(|a| a.get_long().map(|long| format!("--{long}")))
        .collect();
    let globals: Vec<&str> = globals.iter().map(String::as_str).collect();
    cases.push(case(&["status", "--"], &globals, false));

    let mut fixed = Vec::new();
    walk_fixed_values(&root, &mut Vec::new(), &mut fixed);
    for (words, values) in fixed {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let values: Vec<&str> = values.iter().map(String::as_str).collect();
        cases.push(case(&words, &values, false));
    }
    cases
}

/// For every argument with possible values in `cmd` (at `path`) and below: the words
/// that complete them (`<path> --<long> ""` for an option, `<path> ""` for the first
/// positional), and the values.
fn walk_fixed_values(
    cmd: &clap::Command,
    path: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, Vec<String>)>,
) {
    for arg in cmd.get_arguments().filter(|a| !a.is_hide_set()) {
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| v.get_name().to_owned())
            .collect();
        if values.is_empty() {
            continue;
        }
        let mut words = path.clone();
        if arg.is_positional() {
            if arg.get_index() != Some(1) {
                continue;
            }
        } else if let Some(long) = arg.get_long() {
            words.push(format!("--{long}"));
        } else {
            continue;
        }
        words.push(String::new());
        out.push((words, values));
    }
    for sub in cmd.get_subcommands() {
        path.push(sub.get_name().to_owned());
        walk_fixed_values(sub, path, out);
        path.pop();
    }
}

#[test]
fn bash_completes_commands_flags_and_values() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    log.step("a registered client");
    log.run(&home, &["status", "--json"]).ok();
    log.step("generate");
    let script = script_of(&log, &home.env, "bash");
    let dir = home.env.root().join("bash-completion");
    let cwd = home.env.root().join("bash-completion-cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = dir.join("omaddons.bash");
    fs::write(&file, &script).unwrap();

    log.step("complete");
    let mut cases = completion_cases();
    let words: Vec<Vec<String>> = cases.iter().map(|c| c.words.clone()).collect();
    let (run, offers) = shells::bash_complete(&file, &words, &cwd, Some(&home.env));
    log.json("bash completion", &run);
    log.note(format!("{} cases in one bash process", cases.len()));
    let mut failing = Vec::new();
    for (case, offered) in cases.iter_mut().zip(offers) {
        case.offered = offered;
        let offered: BTreeSet<&String> = case.offered.iter().collect();
        let held = if case.exact {
            case.offered == case.expected
        } else {
            case.expected.iter().all(|w| offered.contains(w))
        };
        if !held {
            failing.push(format!(
                "{:?}: expected {:?} {} {:?}",
                case.words,
                case.expected,
                if case.exact { "exactly, got" } else { "among" },
                case.offered
            ));
        }
    }
    log.json("cases", &cases);
    log.assert_eq("failing cases", &failing, &Vec::<String>::new());
}

// ---------------------------------------------------------------------------------------
// 6. Calling back only as the reader, and registered for omaddons only.

/// The reader's invocation, the one way a script may run omaddons.
const READER: &str = "omaddons __complete ";

/// Where `line` starts omaddons other than as [`READER`]: through `COMPLETE=` (clap's
/// dynamic engine, never used), or `omaddons` right after `$(`, a backtick, `& `, or `(`
/// and before a space (fish's command substitution; zsh's `(omaddons)` is a case label).
fn calls_other_than_the_reader(line: &str) -> bool {
    line.contains("COMPLETE=")
        || ["$(omaddons", "`omaddons", "(omaddons ", "& omaddons"]
            .iter()
            .any(|opener| {
                let to_name = opener.find("omaddons").unwrap_or_default();
                line.match_indices(opener)
                    .any(|(at, _)| !line[at + to_name..].starts_with(READER))
            })
}

#[test]
fn a_call_back_other_than_the_reader_is_caught() {
    for (line, caught) in [
        (
            "mapfile -t COMPREPLY < <(omaddons __complete \"$kind\")",
            false,
        ),
        ("found=(${(f)\"$(omaddons __complete $kind)\"})", false),
        ("        omaddons __complete $slot[1] 2>/dev/null", false),
        ("x=$(omaddons status --json)", true),
        ("x=`omaddons list`", true),
        ("complete -a '(omaddons list)'", true),
        ("true & omaddons update", true),
        ("COMPLETE=bash omaddons", true),
        ("x=$(omaddons __completes)", true),
        ("    (omaddons)", false),
    ] {
        assert_eq!(calls_other_than_the_reader(line), caught, "{line}");
    }
}

#[test]
fn the_scripts_call_back_only_through_the_reader_and_register_only_omaddons() {
    let log = tlog!();
    let env = TestEnv::new();
    let root = env.root().display().to_string();
    let forbidden = [
        "\x1b",
        root.as_str(),
        env!("CARGO_BIN_EXE_omaddons"),
        env!("CARGO_TARGET_TMPDIR"),
    ];
    for shell in SHELLS {
        log.step(shell);
        let script = script_of(&log, &env, shell);
        let registered = match shell {
            // clap's registration of its function, then the glue's of the one wrapping it.
            "bash" => {
                let lines = |function: &str| -> Vec<&str> {
                    script
                        .lines()
                        .filter(|l| {
                            l.trim_start()
                                .starts_with(&format!("complete -F {function} "))
                        })
                        .collect()
                };
                let (clap, glue) = (lines("_omaddons"), lines("_omaddons_dynamic"));
                let last = script
                    .lines()
                    .rfind(|l| l.trim_start().starts_with("complete -F "))
                    .unwrap_or_default();
                clap.len() == 2
                    && glue.len() == 2
                    && clap.iter().chain(&glue).all(|l| l.ends_with(" omaddons"))
                    && last
                        .trim_start()
                        .starts_with("complete -F _omaddons_dynamic ")
            }
            "zsh" => script.lines().next() == Some("#compdef omaddons"),
            "fish" => script
                .lines()
                .filter(|l| l.starts_with("complete "))
                .all(|l| l.starts_with("complete -c omaddons ")),
            "elvish" => {
                script
                    .matches("edit:completion:arg-completer[omaddons]")
                    .count()
                    == 1
            }
            _ => script.matches("-CommandName 'omaddons'").count() == 1,
        };
        log.assert_eq(&format!("{shell} registers omaddons"), &registered, &true);
        let mut found: Vec<String> = forbidden
            .iter()
            .filter_map(|needle| {
                script.lines().position(|l| l.contains(needle)).map(|n| {
                    format!(
                        "{shell}: {}-byte script contains {needle:?} at line {}",
                        script.len(),
                        n + 1
                    )
                })
            })
            .collect();
        found.extend(
            script
                .lines()
                .enumerate()
                .filter(|(_, l)| calls_other_than_the_reader(l))
                .map(|(n, l)| format!("{shell}: line {} runs omaddons: {l}", n + 1)),
        );
        log.assert_eq("forbidden text", &found, &Vec::<String>::new());
        let calls = script.matches(READER).count();
        let expected = match shell {
            "elvish" | "powershell" => 0,
            _ => 1,
        };
        log.assert_eq(
            &format!("{shell}: calls of the reader"),
            &calls.min(1),
            &expected,
        );
    }
}

// ---------------------------------------------------------------------------------------
// 7. The same bytes on every terminal.

#[test]
fn a_script_is_the_same_bytes_on_every_terminal() {
    let log = tlog!();
    let env = TestEnv::new();
    let variations: [(&str, &[(&str, &str)]); 4] = [
        ("COLUMNS unset", &[]),
        ("COLUMNS=40", &[("COLUMNS", "40")]),
        ("COLUMNS=250", &[("COLUMNS", "250")]),
        (
            "TERM=xterm-256color COLUMNS=80",
            &[("TERM", "xterm-256color"), ("COLUMNS", "80")],
        ),
    ];
    let run = |argv: &[&str], vars: &[(&str, &str)]| {
        let mut cmd = log.cmd(&env, argv);
        if vars.is_empty() {
            cmd = cmd.env_remove("COLUMNS");
        }
        for (key, value) in vars {
            cmd = cmd.env(key, value);
        }
        let ran = cmd.run();
        ran.ok();
        ran
    };
    for shell in SHELLS {
        log.step(shell);
        let mut scripts = BTreeMap::new();
        let mut envelopes = BTreeMap::new();
        for (label, vars) in variations {
            if vars.is_empty() {
                log.note("COLUMNS removed from the child's environment");
            }
            for attempt in 1..=2 {
                let human = run(&["completions", shell], vars);
                scripts.insert(format!("{label} #{attempt}"), sha256(human.stdout()));
                let json = run(&["completions", shell, "--json"], vars);
                envelopes.insert(format!("{label} #{attempt}"), without_meta(&json));
            }
        }
        log.json("script sha256 per run", &scripts);
        let distinct: BTreeSet<&String> = scripts.values().collect();
        log.assert_eq("distinct scripts", &distinct.len(), &1);
        let distinct: Vec<&Value> = envelopes.values().collect();
        log.assert_eq(
            "every envelope equals the first",
            &distinct.iter().all(|e| *e == distinct[0]),
            &true,
        );
    }
}

// ---------------------------------------------------------------------------------------
// 8. Pure: nothing read, written or sent, no home needed.

#[test]
fn completions_reads_and_writes_nothing() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let retail = home.client("_retail_");
    Wtf::at(&retail).saved_variables("Foo", 64).build();
    let zip = home.env.root().join("dl/Foo.zip");
    fs::create_dir_all(zip.parent().unwrap()).unwrap();
    fs::write(
        &zip,
        zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n## Version: 1\n")]),
    )
    .unwrap();
    log.step("a managed addon");
    log.run(
        &home,
        &[
            "install",
            &zip.display().to_string(),
            "-t",
            "retail",
            "--json",
        ],
    )
    .ok();

    log.step("before");
    let trees = [
        ("state", home.env.state()),
        ("cache", home.env.cache()),
        ("config", home.env.config()),
        ("home", home.env.home()),
    ];
    for (label, root) in &trees {
        log.tree(label, root);
    }
    let requests = net_log(&home).len();
    log.note(format!("net.log: {requests} lines"));

    let mut bash = String::new();
    for shell in SHELLS {
        log.step(shell);
        for argv in [
            &["completions", shell][..],
            &["completions", shell, "--json", "--progress", "json"],
        ] {
            let ran = log.run(&home, argv);
            ran.ok();
            assert_private_all(ran.stdout(), ran.stderr());
            if argv.len() == 2 && shell == "bash" {
                bash = ran.stdout().to_owned();
            }
        }
    }

    log.step("after");
    for (label, root) in &trees {
        log.assert_unchanged(label, root);
    }
    log.assert_eq("net.log lines", &net_log(&home).len(), &requests);

    log.step("no home");
    let ran = log
        .cmd(&home, &["completions", "bash"])
        .env("HOME", "relative/home")
        .run();
    ran.ok();
    log.assert_eq(
        "the same bytes without a home",
        &(ran.stdout() == bash),
        &true,
    );
    let status = log
        .cmd(&home, &["status", "--json"])
        .env("HOME", "relative/home")
        .run();
    status.fails(2, "usage");
    let message = status.envelope()["error"]["message"]
        .as_str()
        .unwrap_or_default();
    log.assert_eq(
        "status refuses the relative HOME",
        &message.starts_with("HOME is not set to an absolute path"),
        &true,
    );
}

// ---------------------------------------------------------------------------------------
// 9. Every wrong invocation gets a fix that runs.

#[test]
fn wrong_shell_invocations_get_fixes_that_run() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let token = format!("pc1:{}", "0".repeat(64));
    let rows: [(&[&str], &[&str], &[&str]); 7] = [
        (
            &["completions", "bsh"],
            &["omaddons", "completions", "bash"],
            &["omaddons completions zsh", "omaddons completions fish"],
        ),
        (
            &["completions", "PowerShell"],
            &["omaddons", "completions", "powershell"],
            &[],
        ),
        (
            &["completions", "BASH"],
            &["omaddons", "help", "completions"],
            &[],
        ),
        (
            &["completion", "bash"],
            &["omaddons", "completions", "bash"],
            &[],
        ),
        (&["completions"], &["omaddons", "help", "completions"], &[]),
        (
            &["completions", "bash", "zsh"],
            &["omaddons", "help", "completions"],
            &[],
        ),
        (
            &["completions", "bash", "--if-match", &token],
            &["omaddons", "completions", "bash"],
            &[],
        ),
    ];
    for (wrong, fix, alternatives) in rows {
        log.step(&wrong.join(" "));
        log.tree("state", &home.env.state());
        log.tree("home", &home.env.home());
        let mut argv = wrong.to_vec();
        argv.push("--json");
        let ran = log.run(&home, &argv);
        ran.fails(2, "usage");
        let error = &ran.envelope()["error"];
        log.assert_eq("fix_argv", &error["fix_argv"], &json!(fix));
        let displays: Vec<&str> = error["alternatives"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x["display"].as_str()).collect())
            .unwrap_or_default();
        log.assert_eq("alternatives", &displays, &alternatives.to_vec());
        log.assert_unchanged("state", &home.env.state());
        log.assert_unchanged("home", &home.env.home());

        log.step("fix");
        let mut fix_args: Vec<&str> = fix[1..].to_vec();
        if fix_args.first() == Some(&"help") {
            fix_args.push("--json");
            let ran = log.run(&home, &fix_args);
            ran.ok();
            log.assert_eq(
                "data.command",
                &ran.data()["command"],
                &json!("completions"),
            );
            log.assert_eq(
                "data.args[0].name",
                &ran.data()["args"][0]["name"],
                &json!("shell"),
            );
            log.assert_eq(
                "data.args[0].required",
                &ran.data()["args"][0]["required"],
                &json!(true),
            );
        } else {
            let shell = fix_args[1];
            let ran = log.run(&home, &fix_args);
            ran.ok();
            let marker = if shell == "bash" {
                "complete -F _omaddons"
            } else {
                "Register-ArgumentCompleter -Native -CommandName 'omaddons'"
            };
            log.assert_eq(
                &format!("the {shell} script"),
                &ran.stdout().contains(marker),
                &true,
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 10. Help.

#[test]
fn help_describes_completions() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("three spellings");
    let texts: Vec<String> = [
        &["completions", "--help"][..],
        &["help", "completions"],
        &["help", "completions", "bash"],
    ]
    .into_iter()
    .map(|argv| {
        let ran = log.run(&env, argv);
        ran.ok();
        ran.stdout().to_owned()
    })
    .collect();
    log.assert_eq("help completions", &texts[1], &texts[0]);
    log.assert_eq("help completions bash", &texts[2], &texts[0]);
    for needle in [
        "Usage: omaddons completions [OPTIONS] <SHELL>",
        "[possible values: bash, zsh, fish, elvish, powershell]",
        "Examples:",
    ] {
        log.assert_eq(needle, &texts[0].contains(needle), &true);
    }
    log.step("json");
    let ran = log.run(&env, &["help", "completions", "--json"]);
    ran.ok();
    log.assert_eq(
        "data.command",
        &ran.data()["command"],
        &json!("completions"),
    );
}

// ---------------------------------------------------------------------------------------
// 11. Checkpoint only: zsh and fish check their scripts where they are installed.

fn shell_accepts_its_script(log: &TestLog, test: &str, checker: Checker) {
    let program = checker.program();
    let Some(shell) = shells::find(program) else {
        checkpoint::skip(test, &format!("{program} is not installed"));
        return;
    };
    let env = TestEnv::new();
    log.step("version");
    let version = std::process::Command::new(&shell) // ubs:ignore — zsh or fish, found on PATH
        .arg("--version")
        .env_clear()
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    log.note(format!("{program} --version: {version}"));
    log.step("generate");
    let script = script_of(log, &env, program);
    log.step("check");
    let dir = env.root().join(format!("{program}-check"));
    let made = fs::create_dir_all(&dir);
    assert!(made.is_ok(), "{}: {made:?}", dir.display());
    let checked = shells::syntax_check(checker, &script, &dir);
    let run = match &checked {
        Ok(run) | Err(run) => run,
    };
    log.json(&format!("{program} check"), run);
    log.assert_eq(
        &format!("{program} accepts its script"),
        &run.code,
        &Some(0),
    );
}

#[test]
#[ignore = "checkpoint: zsh -n over the generated script"]
fn zsh_accepts_its_script() {
    shell_accepts_its_script(&tlog!(), "zsh_accepts_its_script", Checker::Zsh);
}

#[test]
#[ignore = "checkpoint: fish --no-execute over the generated script"]
fn fish_accepts_its_script() {
    shell_accepts_its_script(&tlog!(), "fish_accepts_its_script", Checker::Fish);
}

// ---------------------------------------------------------------------------------------
// 12. The reader: values from omaddons' own files, and nothing else.

/// Every kind `__complete` takes.
const KINDS: [&str; 8] = [
    "target",
    "addon",
    "plan",
    "backup",
    "tx",
    "run",
    "doctor-run",
    "fm",
];

/// A home with two registered clients (three addons on retail, none on era), two stored
/// plans, a backup of retail, a history projection and a doctor run; and the values each
/// kind must offer, read from the envelopes and directories that made them.
struct Fixture {
    home: Home,
    plans: Vec<String>,
    backups: Vec<String>,
    txs: Vec<String>,
    runs: Vec<String>,
    doctor_runs: Vec<String>,
}

/// The names of `dir`'s directories that do not start with a dot, newest first (their
/// names start with their time).
fn dirs_newest_first(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| !name.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names
}

fn fixture(log: &TestLog) -> Fixture {
    let home = Home::new(&["wow", "wow_classic_era"]);
    let retail = home.client("_retail_");
    Wtf::at(&retail).saved_variables("Foo", 64).build();
    let dl = home.env.root().join("dl");
    let made = fs::create_dir_all(&dl);
    assert!(made.is_ok(), "{}: {made:?}", dl.display());
    let zip = |name: &str| {
        let path = dl.join(format!("{name}.zip"));
        let toc = format!("{name}/{name}.toc");
        let written = fs::write(
            &path,
            zip_text(&[(&toc, "## Interface: 120105\n## Version: 1\n")]),
        );
        assert!(written.is_ok(), "{}: {written:?}", path.display());
        path.display().to_string()
    };
    let zips = [zip("Foo"), zip("Bar"), zip("Baz")];

    log.step("three addons on retail");
    log.run(
        &home,
        &[
            "install", &zips[0], &zips[1], &zips[2], "-t", "retail", "--json",
        ],
    )
    .ok();
    log.step("era registered");
    log.run(&home, &["targets", "default", "era", "--json"])
        .ok();
    log.step("two stored plans");
    let mut plans = Vec::new();
    for argv in [
        &["remove", "Foo", "-t", "retail", "--dry-run", "--json"][..],
        &["remove", "Bar", "-t", "retail", "--dry-run", "--json"],
    ] {
        let ran = log.run(&home, argv);
        ran.ok();
        plans.push(
            ran.data()["plan_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        );
    }
    plans.sort_unstable_by(|a, b| b.cmp(a));
    log.step("a backup of retail");
    log.run(&home, &["backup", "create", "-t", "retail", "--json"])
        .ok();
    let backups = dirs_newest_first(&retail.join("Interface/.omaddons/backups"));
    log.step("a doctor run");
    let doctor = log.run(&home, &["doctor", "--quick", "--json"]);
    log.json("doctor exit", &doctor.run.code);
    let doctor_runs = dirs_newest_first(&home.env.state().join("omaddons/doctor/runs"));
    log.step("the history");
    let history = log.run(&home, &["history", "--json"]);
    history.ok();
    let entries = history.data()["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let field = |entry: &Value, key: &str| entry[key].as_str().map(str::to_owned);
    let mut txs: Vec<String> = entries
        .iter()
        .filter(|e| e["kind"] == "install")
        .filter_map(|e| field(e, "tx"))
        .collect();
    txs.sort_unstable_by(|a, b| b.cmp(a));
    let mut runs: Vec<String> = entries
        .iter()
        .filter_map(|e| field(e, "run"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    runs.sort_unstable_by(|a, b| b.cmp(a));
    let fixture = Fixture {
        home,
        plans,
        backups,
        txs,
        runs,
        doctor_runs,
    };
    for (what, values) in [
        ("plans", &fixture.plans),
        ("backups", &fixture.backups),
        ("txs", &fixture.txs),
        ("runs", &fixture.runs),
        ("doctor runs", &fixture.doctor_runs),
    ] {
        log.json(what, values);
        assert!(
            values.iter().all(|v| !v.is_empty()) && !values.is_empty(),
            "the fixture made no {what}: {values:?}"
        );
    }
    assert_eq!(fixture.plans.len(), 2, "two stored plans");
    fixture
}

/// `omaddons __complete <args>` on `home`: exit 0, nothing on stderr, and its lines.
#[track_caller]
fn complete(log: &TestLog, home: &Home, args: &[&str]) -> (Vec<String>, u64) {
    let argv: Vec<&str> = std::iter::once("__complete")
        .chain(args.iter().copied())
        .collect();
    let ran = log.run(home, &argv);
    ran.exit(0);
    log.assert_eq("stderr", &ran.stderr(), &"");
    let lines = ran.stdout().lines().map(str::to_owned).collect();
    (lines, ran.elapsed_ms)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}

fn strs(values: &[String]) -> Vec<&str> {
    values.iter().map(String::as_str).collect()
}

fn failure_modes() -> Vec<String> {
    omaddons::doctor::registry::FAILURE_MODES
        .iter()
        .map(|fm| fm.id.to_owned())
        .collect()
}

#[test]
fn the_reader_offers_each_kind_from_omaddons_files() {
    let log = tlog!();
    let f = fixture(&log);
    let home = &f.home;
    let mut latest = vec!["latest".to_owned()];
    latest.extend(f.doctor_runs.iter().cloned());
    let plan_prefix = f.plans[0][..10].to_ascii_lowercase();
    let cases: Vec<(&str, Vec<&str>, Vec<String>)> = vec![
        (
            "every registered client",
            vec!["target"],
            strings(&["era", "retail"]),
        ),
        (
            "a prefix, ignoring case",
            vec!["target", "--prefix", "R"],
            strings(&["retail"]),
        ),
        (
            "one client's addons",
            vec!["addon", "--target", "retail"],
            strings(&["Bar", "Baz", "Foo"]),
        ),
        (
            "a client with none",
            vec!["addon", "--target", "era"],
            Vec::new(),
        ),
        (
            "every client's addons",
            vec!["addon"],
            strings(&["Bar", "Baz", "Foo"]),
        ),
        (
            "addons by prefix",
            vec!["addon", "--target=retail", "--prefix=b"],
            strings(&["Bar", "Baz"]),
        ),
        (
            "the stored plans, newest first",
            vec!["plan"],
            f.plans.clone(),
        ),
        (
            "a plan by a lowercase prefix",
            vec!["plan", "--prefix", &plan_prefix],
            vec![f.plans[0].clone()],
        ),
        (
            "retail's backups",
            vec!["backup", "--target", "retail"],
            f.backups.clone(),
        ),
        (
            "era has none",
            vec!["backup", "--target", "era"],
            Vec::new(),
        ),
        ("what undo can revert", vec!["tx"], f.txs.clone()),
        ("the runs", vec!["run"], f.runs.clone()),
        ("latest, then the doctor's runs", vec!["doctor-run"], latest),
        ("every failure mode", vec!["fm"], failure_modes()),
    ];
    for (case, args, expected) in cases {
        log.step(case);
        let (values, _) = complete(&log, home, &args);
        log.assert_eq(case, &values, &expected);
    }
}

#[test]
fn the_reader_reads_only_takes_no_lock_and_fails_silently() {
    let log = tlog!();
    let f = fixture(&log);
    let home = &f.home;

    log.step("before");
    let trees = [
        ("state", home.env.state()),
        ("cache", home.env.cache()),
        ("config", home.env.config()),
        ("home", home.env.home()),
    ];
    for (label, root) in &trees {
        log.tree(label, root);
    }
    let requests = net_log(home).len();

    log.step("every kind, with and without a client and a prefix");
    let mut slowest = 0;
    for kind in KINDS {
        for extra in [&[][..], &["--target", "retail"], &["--prefix", "x"]] {
            let args: Vec<&str> = std::iter::once(kind).chain(extra.iter().copied()).collect();
            let argv: Vec<&str> = std::iter::once("__complete")
                .chain(args.iter().copied())
                .collect();
            let ran = log.run(home, &argv);
            ran.exit(0);
            assert_private_all(ran.stdout(), ran.stderr());
            slowest = slowest.max(ran.elapsed_ms);
        }
    }
    log.note(format!(
        "the slowest __complete took {slowest} ms (a debug build)"
    ));
    log.assert_eq("no Tab press waits seconds", &(slowest < 2_000), &true);

    log.step("what it cannot read or parse: nothing, exit 0");
    for args in [
        &[][..],
        &["game"],
        &["target", "addon"],
        &["target", "--json"],
        &["target", "--help"],
        &["--version"],
    ] {
        let (values, _) = complete(&log, home, args);
        log.assert_eq(&format!("{args:?}"), &values, &Vec::<String>::new());
    }

    log.step("after: nothing written, nothing sent");
    for (label, root) in &trees {
        log.assert_unchanged(label, root);
    }
    log.assert_eq("net.log lines", &net_log(home).len(), &requests);

    log.step("while another command holds the global lock");
    let guard = omaddons::fsx::GlobalLock::acquire(&paths(&home.env), "test holder");
    assert!(guard.is_ok(), "{guard:?}");
    let (values, ms) = complete(&log, home, &["target"]);
    log.assert_eq(
        "values under the lock",
        &values,
        &strings(&["era", "retail"]),
    );
    log.note(format!("answered in {ms} ms while the lock was held"));
    log.assert_eq("never waited for the lock", &(ms < 2_000), &true);
    drop(guard);

    log.step("an environment that fails every other command");
    for (case, key, value) in [
        ("a JSON format", "OMADDONS_FORMAT", "json"),
        ("an unknown actor", "OMADDONS_ACTOR", "nobody"),
    ] {
        let ran = log
            .cmd(home, &["__complete", "target"])
            .env(key, value)
            .run();
        ran.exit(0);
        log.assert_eq(case, &ran.stdout(), &"era\nretail\n");
        log.assert_eq("stderr", &ran.stderr(), &"");
    }
    let ran = log
        .cmd(home, &["__complete", "target"])
        .env("HOME", "relative/home")
        .run();
    ran.exit(0);
    log.assert_eq("no usable HOME", &ran.stdout(), &"");
    log.assert_eq("stderr", &ran.stderr(), &"");

    log.step("an empty home");
    let empty = Home::new(&["wow"]);
    for kind in KINDS.iter().filter(|k| **k != "fm") {
        let (values, _) = complete(&log, &empty, &[kind]);
        log.assert_eq(kind, &values, &Vec::<String>::new());
    }
}

#[test]
fn bash_completes_values_from_omaddons_files() {
    let log = tlog!();
    let f = fixture(&log);
    let home = &f.home;
    log.step("generate");
    let script = script_of(&log, &home.env, "bash");
    let dir = home.env.root().join("bash-completion");
    let cwd = home.env.root().join("bash-completion-cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = dir.join("omaddons.bash");
    fs::write(&file, &script).unwrap();

    let fms = failure_modes();
    let fm_refs: Vec<&str> = fms.iter().map(String::as_str).collect();
    let caches: Vec<String> = fms
        .iter()
        .filter(|id| id.starts_with("fm-caches-"))
        .map(|id| format!("{},{id}", fms[0]))
        .collect();
    let caches: Vec<&str> = caches.iter().map(String::as_str).collect();
    let only_prefix = format!("{},fm-caches-", fms[0]);
    let mut latest = vec!["latest".to_owned()];
    latest.extend(f.doctor_runs.iter().cloned());
    let (plans, txs, backups, latest, runs) = (
        strs(&f.plans),
        strs(&f.txs),
        strs(&f.backups),
        strs(&latest),
        strs(&f.runs),
    );
    let addons = ["Bar", "Baz", "Foo"];
    let clients = ["era", "retail"];
    let mut cases = vec![
        case(&["status", "-t", ""], &clients, true),
        case(&["-t", "r"], &["retail"], true),
        case(&["--target", ""], &clients, true),
        case(&["status", "--target", "=", ""], &clients, true),
        case(&["remove", "-t", "retail", ""], &addons, true),
        case(
            &["remove", "-t", "retail", "Foo", "B"],
            &["Bar", "Baz"],
            true,
        ),
        case(&["update", "--target", "=", "retail", ""], &addons, true),
        case(
            &[
                "update",
                "--dry-run",
                "-t",
                "retail",
                "--channel",
                "beta",
                "",
            ],
            &addons,
            true,
        ),
        case(&["pin", ""], &addons, true),
        case(&["pin", "Foo", "B"], &[], true),
        case(
            &["copy", "--from", "retail", "--to", "era", ""],
            &addons,
            true,
        ),
        case(&["copy", "--from", ""], &clients, true),
        case(&["errors", "--addon", ""], &addons, true),
        case(&["apply", ""], &plans, true),
        case(&["plans", "show", ""], &plans, true),
        case(&["undo", ""], &txs, true),
        case(&["revert", ""], &txs, true),
        case(&["undo", "--run", ""], &runs, true),
        case(&["history", "--run", ""], &runs, true),
        case(&["backup", "restore", "-t", "retail", ""], &backups, true),
        case(&["backup", "restore", "x", "--addon", ""], &addons, true),
        case(&["doctor", "explain", ""], &fm_refs, true),
        case(&["doctor", "--only", &only_prefix], &caches, true),
        case(&["doctor", "diff", ""], &latest, true),
        case(&["doctor", "--since", ""], &latest, true),
        case(&["targets", "rm", ""], &clients, true),
        // A path, after `--`, an option, or a word that names no slot: clap's offers
        // (its flags, which no letter starts, or the empty directory's file names).
        case(&["targets", "rename", "retail", "r"], &[], true),
        case(&["-t", "./"], &[], true),
        case(&["--", "-t", ""], &[], true),
        case(&["remove", "-"], &["--help"], false),
        case(&["list", "B"], &[], true),
    ];
    log.step("complete");
    let words: Vec<Vec<String>> = cases.iter().map(|c| c.words.clone()).collect();
    let (run, offers) = shells::bash_complete(&file, &words, &cwd, Some(&home.env));
    log.json("bash completion", &run);
    let mut failing = Vec::new();
    for (case, offered) in cases.iter_mut().zip(offers) {
        case.offered = offered;
        let offered: BTreeSet<&String> = case.offered.iter().collect();
        let held = if case.exact {
            case.offered == case.expected
        } else {
            case.expected.iter().all(|w| offered.contains(w))
        };
        if !held {
            failing.push(format!(
                "{:?}: expected {:?} {} {:?}",
                case.words,
                case.expected,
                if case.exact { "exactly, got" } else { "among" },
                case.offered
            ));
        }
    }
    log.json("cases", &cases);
    log.assert_eq("failing cases", &failing, &Vec::<String>::new());
}

/// A shell's `--version`, for the log.
fn version_of(shell: &Path) -> String {
    std::process::Command::new(shell) // ubs:ignore — zsh or fish, found on PATH
        .arg("--version")
        .env_clear()
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

/// The fixture's script for `shell`, written to a folder of its own, and the empty
/// working directory its file-name fallbacks list.
fn script_file(
    log: &TestLog,
    home: &Home,
    shell: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    log.step("generate");
    let script = script_of(log, &home.env, shell);
    let dir = home.env.root().join(format!("{shell}-completion"));
    let cwd = home.env.root().join(format!("{shell}-completion-cwd"));
    for folder in [&dir, &cwd] {
        let made = fs::create_dir_all(folder);
        assert!(made.is_ok(), "{}: {made:?}", folder.display());
    }
    let file = dir.join(format!("omaddons.{shell}"));
    let written = fs::write(&file, &script);
    assert!(written.is_ok(), "{}: {written:?}", file.display());
    (file, cwd)
}

/// One case of another shell than bash: its words after `omaddons`, what it must give,
/// and what it gave.
#[derive(serde::Serialize)]
struct ShellCase<T> {
    words: Vec<String>,
    expected: T,
    got: Option<T>,
}

fn owned(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// omad-pede: zsh's `_omaddons` over the home bash completes from. At each slot it hands
/// `compadd` the values unfiltered (zsh filters them by the word itself), a
/// `--target=…` word keeps its option through `compset -P`, and where no slot is, or the
/// word is a path, it falls back to clap's static part. Checkpoint only: it skips by name
/// where zsh is missing; a container with zsh 5.8 or newer counts.
#[test]
#[ignore = "checkpoint: zsh completes values from omaddons' files"]
fn zsh_completes_values_from_omaddons_files() {
    let log = tlog!();
    let Some(zsh) = shells::find("zsh") else {
        checkpoint::skip(
            "zsh_completes_values_from_omaddons_files",
            "zsh is not installed",
        );
        return;
    };
    log.note(format!("zsh --version: {}", version_of(&zsh)));
    let f = fixture(&log);
    let (file, cwd) = script_file(&log, &f.home, "zsh");
    let fms = failure_modes();
    let mut latest = vec!["latest".to_owned()];
    latest.extend(f.doctor_runs.iter().cloned());
    let (plans, txs, backups, latest, runs, fms) = (
        strs(&f.plans),
        strs(&f.txs),
        strs(&f.backups),
        strs(&latest),
        strs(&f.runs),
        strs(&fms),
    );
    let addons = ["Bar", "Baz", "Foo"];
    let clients = ["era", "retail"];
    let offer = |values: &[&str], iprefix: &str| shells::ZshOffer {
        offered: owned(values),
        iprefix: iprefix.to_owned(),
        fell_back: false,
    };
    let fallback = shells::ZshOffer {
        offered: Vec::new(),
        iprefix: String::new(),
        fell_back: true,
    };
    let table: Vec<(&[&str], shells::ZshOffer)> = vec![
        (&["status", "-t", ""], offer(&clients, "")),
        (&["-t", "r"], offer(&clients, "")),
        (&["--target", ""], offer(&clients, "")),
        (&["status", "--target="], offer(&clients, "--target=")),
        (&["remove", "-t", "retail", ""], offer(&addons, "")),
        (&["remove", "-tretail", ""], offer(&addons, "")),
        (&["update", "--target=retail", ""], offer(&addons, "")),
        (&["pin", ""], offer(&addons, "")),
        (
            &["copy", "--from", "retail", "--to", "era", ""],
            offer(&addons, ""),
        ),
        (&["copy", "--from", ""], offer(&clients, "")),
        (&["errors", "--addon", ""], offer(&addons, "")),
        (&["apply", ""], offer(&plans, "")),
        (&["undo", ""], offer(&txs, "")),
        (&["undo", "--run", ""], offer(&runs, "")),
        (
            &["backup", "restore", "-t", "retail", ""],
            offer(&backups, ""),
        ),
        (&["doctor", "explain", ""], offer(&fms, "")),
        (&["doctor", "--only", ""], offer(&fms, "")),
        (&["doctor", "diff", ""], offer(&latest, "")),
        (&["targets", "rm", ""], offer(&clients, "")),
        // No slot, a path, or a word after `--`: clap's static part.
        (&["list", "B"], fallback.clone()),
        (&["targets", "rename", "retail", "r"], fallback.clone()),
        (&["-t", "./"], fallback.clone()),
        (&["--", "-t", ""], fallback.clone()),
    ];
    let mut cases: Vec<ShellCase<shells::ZshOffer>> = table
        .into_iter()
        .map(|(words, expected)| ShellCase {
            words: owned(words),
            expected,
            got: None,
        })
        .collect();
    log.step("complete");
    let words: Vec<Vec<String>> = cases.iter().map(|c| c.words.clone()).collect();
    let (run, offers) = shells::zsh_complete(&zsh, &file, &words, &cwd, Some(&f.home.env));
    log.json("zsh completion", &run);
    let mut failing = Vec::new();
    for (case, got) in cases.iter_mut().zip(offers) {
        if got != case.expected {
            failing.push(format!(
                "{:?}: expected {:?}, got {got:?}",
                case.words, case.expected
            ));
        }
        case.got = Some(got);
    }
    log.json("cases", &cases);
    log.assert_eq("failing cases", &failing, &Vec::<String>::new());
}

/// omad-pede: fish's rules over the home bash completes from, as `complete -C` completes
/// each command line. fish filters by the word itself and keeps an `--option=` in front
/// of each value; where no slot is, or the word is a path, the empty working directory
/// leaves the static part nothing to offer. Checkpoint only: it skips by name where fish
/// is missing; a container with fish 3.6 or newer counts.
#[test]
#[ignore = "checkpoint: fish completes values from omaddons' files"]
fn fish_completes_values_from_omaddons_files() {
    let log = tlog!();
    let Some(fish) = shells::find("fish") else {
        checkpoint::skip(
            "fish_completes_values_from_omaddons_files",
            "fish is not installed",
        );
        return;
    };
    log.note(format!("fish --version: {}", version_of(&fish)));
    let f = fixture(&log);
    let (file, cwd) = script_file(&log, &f.home, "fish");
    let fms = failure_modes();
    let mut latest = vec!["latest".to_owned()];
    latest.extend(f.doctor_runs.iter().cloned());
    let (plans, txs, backups, latest, runs, fms) = (
        strs(&f.plans),
        strs(&f.txs),
        strs(&f.backups),
        strs(&latest),
        strs(&f.runs),
        strs(&fms),
    );
    let addons = ["Bar", "Baz", "Foo"];
    let clients = ["era", "retail"];
    let table: Vec<(&[&str], &[&str])> = vec![
        (&["status", "-t", ""], &clients),
        (&["-t", "r"], &["retail"]),
        (&["--target", ""], &clients),
        (
            &["status", "--target="],
            &["--target=era", "--target=retail"],
        ),
        (&["remove", "-t", "retail", ""], &addons),
        (&["remove", "-tretail", ""], &addons),
        (&["remove", "-t", "retail", "Foo", "B"], &["Bar", "Baz"]),
        (&["update", "--target=retail", ""], &addons),
        (&["pin", ""], &addons),
        (&["copy", "--from", "retail", "--to", "era", ""], &addons),
        (&["copy", "--from", ""], &clients),
        (&["errors", "--addon", ""], &addons),
        (&["apply", ""], &plans),
        (&["undo", ""], &txs),
        (&["undo", "--run", ""], &runs),
        (&["backup", "restore", "-t", "retail", ""], &backups),
        (&["doctor", "explain", ""], &fms),
        (&["doctor", "--only", ""], &fms),
        (&["doctor", "diff", ""], &latest),
        (&["targets", "rm", ""], &clients),
        // No slot, a path: the static part, which finds nothing in the empty directory.
        (&["list", "B"], &[]),
        (&["targets", "rename", "retail", "r"], &[]),
        (&["-t", "./"], &[]),
    ];
    let sorted = |values: &[String]| -> Vec<String> {
        let mut values = values.to_vec();
        values.sort();
        values
    };
    let mut cases: Vec<ShellCase<Vec<String>>> = table
        .into_iter()
        .map(|(words, expected)| ShellCase {
            words: owned(words),
            expected: sorted(&owned(expected)),
            got: None,
        })
        .collect();
    log.step("complete");
    let words: Vec<Vec<String>> = cases.iter().map(|c| c.words.clone()).collect();
    let (run, offers) = shells::fish_complete(&fish, &file, &words, &cwd, Some(&f.home.env));
    log.json("fish completion", &run);
    let mut failing = Vec::new();
    for (case, got) in cases.iter_mut().zip(offers) {
        let got = sorted(&got);
        if got != case.expected {
            failing.push(format!(
                "{:?}: expected {:?}, got {got:?}",
                case.words, case.expected
            ));
        }
        case.got = Some(got);
    }
    log.json("cases", &cases);
    log.assert_eq("failing cases", &failing, &Vec::<String>::new());
}

/// omad-ivbj: on a fresh install, before any change registers a client, `-t` completes
/// the clients the last plain `status` discovered, by the ids `-t` resolves for them, and
/// each of those ids works. `targets rm`, which removes a registration, offers none; `targets
/// default`, which registers, offers them. Once a first change registers one, it comes
/// first and the other still follows.
#[test]
fn a_fresh_install_completes_the_clients_status_discovered() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic_era"]);
    log.env(&home);

    log.step("before any status: no cache, no registry, nothing to offer");
    let (values, _) = complete(&log, &home, &["client"]);
    log.assert_eq("client", &values, &Vec::<String>::new());

    log.step("a plain status discovers both clients and registers neither");
    log.run(&home, &["status", "--json"]).ok();
    let (registered, _) = complete(&log, &home, &["target"]);
    log.assert_eq(
        "target: nothing registered",
        &registered,
        &Vec::<String>::new(),
    );
    let (clients, _) = complete(&log, &home, &["client"]);
    let mut sorted = clients.clone();
    sorted.sort();
    log.assert_eq(
        "client: the discovered ones",
        &sorted,
        &strings(&["era", "retail"]),
    );
    for id in &clients {
        log.run(&home, &["status", "-t", id, "--json"]).ok();
    }

    log.step("bash");
    let script = script_of(&log, &home.env, "bash");
    let dir = home.env.root().join("bash-completion");
    let cwd = home.env.root().join("bash-completion-cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = dir.join("omaddons.bash");
    fs::write(&file, &script).unwrap();
    let words: Vec<Vec<String>> = [
        &["status", "-t", ""][..],
        &["targets", "rm", ""],
        &["targets", "default", ""],
    ]
    .iter()
    .map(|w| strings(w))
    .collect();
    let (run, offers) = shells::bash_complete(&file, &words, &cwd, Some(&home.env));
    log.json("bash completion", &run);
    log.assert_eq("status -t", &offers[0], &clients);
    // Registered ones only, and there are none: the script falls back to clap's own
    // offers (its flags), never a discovered client's id.
    log.assert_eq(
        "targets rm: no client id",
        &offers[1].iter().any(|offer| clients.contains(offer)),
        &false,
    );
    log.assert_eq("targets default", &offers[2], &clients);

    log.step("a first change registers one");
    log.run(&home, &["targets", "default", "retail", "--json"])
        .ok();
    let (after, _) = complete(&log, &home, &["client"]);
    log.assert_eq(
        "client: the registered one first",
        &after,
        &strings(&["retail", "era"]),
    );
    let (registered, _) = complete(&log, &home, &["target"]);
    log.assert_eq("target", &registered, &strings(&["retail"]));
}

#[test]
fn no_help_capabilities_or_guide_names_the_reader() {
    let log = tlog!();
    let env = TestEnv::new();
    for argv in [
        &["--help"][..],
        &["help", "--json"],
        &["capabilities", "--json"],
        &["capabilities"],
        &["robot-docs", "guide"],
        &["robot-docs", "schema", "--json"],
        &["robot-docs", "errors", "--json"],
        &["completions", "--help"],
    ] {
        log.step(&argv.join(" "));
        let ran = log.run(&env, argv);
        ran.ok();
        log.assert_eq(
            "names __complete",
            &(ran.stdout().contains("__complete") || ran.stderr().contains("__complete")),
            &false,
        );
    }
    log.step("a typo of a command never offers the reader");
    let ran = log.run(&env, &["complete", "--json"]);
    ran.fails(2, "usage");
    log.assert_eq(
        "names __complete",
        &ran.stdout().contains("__complete"),
        &false,
    );
}

// ---------------------------------------------------------------------------------------
// 11. install-completions: the script written where each shell looks (bmcx.11).

/// The marker line `install-completions` writes.
const MARKER: &str = omaddons::app::install_completions::MARKER;

/// What `install-completions <shell>` writes: `completions <shell>`' script with the
/// marker line first, or second after zsh's `#compdef omaddons`.
fn marked(log: &TestLog, env: &TestEnv, shell: &str) -> String {
    let script = script_of(log, env, shell);
    match script.strip_prefix("#compdef omaddons\n") {
        Some(rest) if shell == "zsh" => format!("#compdef omaddons\n{MARKER}\n{rest}"),
        _ => format!("{MARKER}\n{script}"),
    }
}

/// Each shell's file, under `env`'s home and XDG folders.
fn completion_file(env: &TestEnv, shell: &str) -> std::path::PathBuf {
    match shell {
        "bash" => env.data().join("bash-completion/completions/omaddons"),
        "zsh" => env.home().join(".zfunc/_omaddons"),
        _ => env.config().join("fish/completions/omaddons.fish"),
    }
}

/// The kinds of `history --json`'s lines, newest first.
fn history_kinds(log: &TestLog, env: &TestEnv) -> Vec<String> {
    let ran = log.run(env, &["history", "--json"]);
    ran.ok();
    ran.data()["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|line| line["kind"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn install_completions_writes_each_shells_file_once() {
    let log = tlog!();
    let env = TestEnv::new();
    for shell in ["bash", "zsh", "fish"] {
        let file = completion_file(&env, shell);
        let want = marked(&log, &env, shell);
        log.step(&format!("{shell}: dry run"));
        log.tree("home", &env.home());
        log.tree("config", &env.config());
        let ran = log.run(&env, &["install-completions", shell, "--dry-run", "--json"]);
        ran.ok();
        log.assert_eq("dry run written", &ran.data()["written"], &json!("fresh"));
        log.assert_eq(
            "dry run dir_created",
            &ran.data()["dir_created"],
            &json!(true),
        );
        log.assert_eq("dry run bytes", &ran.data()["bytes"], &json!(want.len()));
        log.assert_unchanged("home", &env.home());
        log.assert_unchanged("config", &env.config());

        log.step(&format!("{shell}: install"));
        let ran = log.run(&env, &["install-completions", shell]);
        ran.ok();
        log.note(ran.stdout());
        log.assert_eq(
            "the human line names the file",
            &ran.stdout().contains(&file.display().to_string()),
            &true,
        );
        log.assert_eq(
            "zsh's output says the fpath line",
            &ran.stdout().contains("fpath+=(\"$HOME/.zfunc\")"),
            &(shell == "zsh"),
        );
        log.assert_eq(
            "the file is the script with its marker",
            &(fs::read_to_string(&file).unwrap_or_default() == want),
            &true,
        );
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&file).unwrap().permissions());
        log.assert_eq("mode", &format!("{:o}", mode & 0o7777), &"644".to_owned());
        if shell == "zsh" {
            let text = fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().take(2).collect();
            log.assert_eq(
                "compinit's line first, the marker second",
                &lines,
                &vec!["#compdef omaddons", MARKER],
            );
        }

        log.step(&format!("{shell}: again, unchanged"));
        log.tree("home", &env.home());
        log.tree("config", &env.config());
        let ran = log.run(&env, &["install-completions", shell, "--json"]);
        ran.ok();
        log.assert_eq("written", &ran.data()["written"], &json!("unchanged"));
        log.assert_eq("dir_created", &ran.data()["dir_created"], &json!(false));
        log.assert_eq(
            "file",
            &ran.data()["file"],
            &json!(file.display().to_string()),
        );
        log.assert_unchanged("home", &env.home());
        log.assert_unchanged("config", &env.config());
    }
    log.step("like backup export, it records nothing: no history line, no state");
    log.assert_eq("history", &history_kinds(&log, &env), &Vec::<String>::new());
    log.assert_eq(
        "the state folder",
        &env.state().join("omaddons").exists(),
        &false,
    );
}

#[test]
fn install_completions_replaces_only_a_file_it_wrote() {
    let log = tlog!();
    let env = TestEnv::new();
    let file = completion_file(&env, "bash");
    let want = marked(&log, &env, "bash");
    fs::create_dir_all(file.parent().unwrap()).unwrap();

    log.step("a script without the marker is refused, dry run too");
    fs::write(&file, b"complete -F _mine omaddons\n").unwrap();
    for extra in [&[][..], &["--dry-run"]] {
        let mut argv = vec!["install-completions", "bash", "--json"];
        argv.extend_from_slice(extra);
        let ran = log.run(&env, &argv);
        ran.fails(4, "file_exists");
        let error = &ran.envelope()["error"];
        log.assert_eq(
            "details",
            &error["details"],
            &json!({"file": file.display().to_string()}),
        );
        log.assert_eq("a prose fix", &error["fix_argv"], &Value::Null);
        let mut retry = vec!["omaddons", "install-completions", "bash"];
        retry.extend_from_slice(extra);
        retry.push("--force");
        log.assert_eq(
            "the alternative adds --force, the rest kept",
            &error["alternatives"][0]["argv"],
            &json!(retry),
        );
        log.assert_eq(
            "left as it is",
            &fs::read(&file).unwrap(),
            &b"complete -F _mine omaddons\n".to_vec(),
        );
    }

    log.step("--force replaces it");
    let ran = log.run(&env, &["install-completions", "bash", "--force", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("replaced"));
    log.assert_eq(
        "ours now",
        &(fs::read_to_string(&file).unwrap() == want),
        &true,
    );

    log.step("an older script it wrote is replaced without --force");
    fs::write(&file, format!("{MARKER}\n# an older build's script\n")).unwrap();
    let ran = log.run(&env, &["install-completions", "bash", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("replaced"));
    log.assert_eq(
        "current",
        &(fs::read_to_string(&file).unwrap() == want),
        &true,
    );

    log.step("a symlink is refused, even with --force");
    let dotfiles = env.home().join("dotfiles/omaddons.bash");
    fs::create_dir_all(dotfiles.parent().unwrap()).unwrap();
    fs::write(&dotfiles, b"my dotfiles copy\n").unwrap();
    fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&dotfiles, &file).unwrap();
    for extra in [&[][..], &["--force"]] {
        let mut argv = vec!["install-completions", "bash", "--json"];
        argv.extend_from_slice(extra);
        let ran = log.run(&env, &argv);
        ran.fails(4, "scope_denied");
        log.assert_eq(
            "reason",
            &ran.envelope()["error"]["details"]["reason"],
            &json!("symlink"),
        );
        log.assert_eq(
            "the link stays",
            &fs::symlink_metadata(&file)
                .unwrap()
                .file_type()
                .is_symlink(),
            &true,
        );
        log.assert_eq(
            "its target is untouched",
            &fs::read(&dotfiles).unwrap(),
            &b"my dotfiles copy\n".to_vec(),
        );
    }
}

#[test]
fn install_completions_never_replaces_a_file_created_meanwhile() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("the file's folder exists, so the first write fsx checks is the file's");
    let file = completion_file(&env, "bash");
    fs::create_dir_all(file.parent().unwrap()).unwrap();

    log.step("paused after its checks, another program creates the file");
    let pause = common::pause::Pause::new(env.root());
    let running = log
        .cmd(&env, &["install-completions", "bash", "--json"])
        .failpoints("fsx.mutate.checked=pause@1")
        .pause(&pause)
        .spawn();
    log.reached(&pause, "fsx.mutate.checked");
    fs::write(&file, b"another program's script\n").unwrap();
    log.resume(&pause, "fsx.mutate.checked");
    let ran = running.wait();
    ran.fails(4, "file_exists");
    log.assert_eq(
        "the other program's file stays",
        &fs::read(&file).unwrap(),
        &b"another program's script\n".to_vec(),
    );
    log.assert_eq(
        "a retry is the same command",
        &ran.envelope()["error"]["alternatives"][0]["argv"],
        &json!(["omaddons", "install-completions", "bash"]),
    );
}

#[test]
fn install_completions_refusals_change_nothing() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    let env = &home.env;

    // The cache is left out: every mutating command rebuilds the derived history
    // projection as it exits (M3-D37), a refusal too.
    let trees = [
        ("home", env.home()),
        ("config", env.config()),
        ("state", env.state()),
    ];
    for (shell, rc) in [("elvish", "rc.elv"), ("powershell", "$PROFILE")] {
        log.step(&format!("{shell}: usage, writing nothing"));
        for (label, root) in &trees {
            log.tree(label, root);
        }
        let ran = log.run(&home, &["install-completions", shell, "--json"]);
        ran.fails(2, "usage");
        let error = &ran.envelope()["error"];
        log.assert_eq(
            "names the rc file",
            &error["message"].as_str().unwrap_or_default().contains(rc),
            &true,
        );
        log.assert_eq("a prose fix", &error["fix_argv"], &Value::Null);
        log.assert_eq(
            "the alternative prints the script",
            &error["alternatives"][0]["argv"],
            &json!(["omaddons", "completions", shell]),
        );
        for (label, root) in &trees {
            log.assert_unchanged(label, root);
        }
    }

    log.step("a missing config folder: not_found, nothing created");
    let missing = env.root().join("no-config");
    let ran = log
        .cmd(&home, &["install-completions", "fish", "--json"])
        .env("XDG_CONFIG_HOME", &missing)
        .run();
    ran.fails(3, "not_found");
    log.assert_eq(
        "details.path",
        &ran.envelope()["error"]["details"]["path"],
        &json!(missing.display().to_string()),
    );
    log.assert_eq("nothing created", &missing.exists(), &false);

    log.step("a data folder inside a registered client: scope_denied");
    log.run(&home, &["targets", "default", "retail", "--json"])
        .ok();
    let retail = home.client("_retail_");
    let inside = retail.join("share");
    fs::create_dir_all(&inside).unwrap();
    log.tree("client", &retail);
    let ran = log
        .cmd(&home, &["install-completions", "bash", "--json"])
        .env("XDG_DATA_HOME", &inside)
        .run();
    ran.fails(4, "scope_denied");
    log.assert_eq(
        "names the client",
        &ran.envelope()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("inside the game client"),
        &true,
    );
    log.assert_unchanged("client", &retail);
    assert_private_all(ran.stdout(), ran.stderr());
}

#[test]
fn the_installed_bash_script_completes() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("install");
    log.run(&env, &["install-completions", "bash"]).ok();
    let installed = fs::read_to_string(completion_file(&env, "bash")).unwrap();
    log.step("complete with it");
    let dir = env.root().join("bash-installed");
    let cwd = env.root().join("bash-installed-cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let file = dir.join("omaddons.bash");
    fs::write(&file, installed).unwrap();
    let cases = vec![
        vec!["sta".to_owned()],
        vec!["install-comp".to_owned()],
        vec!["install-completions".to_owned(), String::new()],
    ];
    let (run, offers) = shells::bash_complete(&file, &cases, &cwd, Some(&env));
    log.json("bash completion", &run);
    log.json("offers", &offers);
    log.assert_eq("sta", &offers[0].contains(&"status".to_owned()), &true);
    log.assert_eq(
        "the command itself",
        &offers[1].contains(&"install-completions".to_owned()),
        &true,
    );
    for shell in ["bash", "zsh", "fish"] {
        log.assert_eq(
            &format!("its shells: {shell}"),
            &offers[2].contains(&shell.to_owned()),
            &true,
        );
    }
}
