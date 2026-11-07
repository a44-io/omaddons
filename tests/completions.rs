//! e2e suite: omaddons completions (A7, M4-D16, checkpoint criterion 15). Every multi-command scenario is a TestLog scenario.
//!
//! Each shell's script is snapshot-tested through the real binary. bash checks its own
//! script (`bash -n`) and runs its completion function over the whole command tree in
//! every gate, and never skips. The scripts are static (no call back into the binary, no
//! client id offered), the same bytes on every terminal, and pure: no state read, no
//! write, no request, no home needed. zsh's and fish's syntax checks are checkpoint tests
//! (`--ignored`), which skip only where their shell is missing.

use crate::common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use clap::CommandFactory;
use common::archives::zip_text;
use common::checkpoint;
use common::env::TestEnv;
use common::home::Home;
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
            ("schema_version", json!("1.0")),
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
        // Static (M4-D16): no client id, only file names, and the directory is empty.
        case(&["status", "-t", ""], &[], true),
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
    let (run, offers) = shells::bash_complete(&file, &words, &cwd);
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
// 6. Static, and registered for omaddons only.

#[test]
fn the_scripts_are_static_and_register_only_omaddons() {
    let log = tlog!();
    let env = TestEnv::new();
    let root = env.root().display().to_string();
    let forbidden = [
        "COMPLETE=",
        "$(omaddons",
        "`omaddons",
        "(omaddons ",
        "& omaddons",
        "\x1b",
        root.as_str(),
        env!("CARGO_BIN_EXE_omaddons"),
        env!("CARGO_TARGET_TMPDIR"),
    ];
    for shell in SHELLS {
        log.step(shell);
        let script = script_of(&log, &env, shell);
        let registered = match shell {
            "bash" => {
                let lines: Vec<&str> = script
                    .lines()
                    .filter(|l| l.trim_start().starts_with("complete -F _omaddons"))
                    .collect();
                lines.len() == 2 && lines.iter().all(|l| l.ends_with(" omaddons"))
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
        let found: Vec<String> = forbidden
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
        log.assert_eq("forbidden text", &found, &Vec::<String>::new());
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
