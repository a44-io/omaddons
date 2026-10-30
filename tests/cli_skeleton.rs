//! The first commands an agent guesses: bare help, `help`, `-V`, usage errors and the
//! panic hook, run as a real process with HOME and XDG_* in a tempdir (DEC-6). Every
//! test checks that each captured stderr line is an NDJSON event.

mod common;

use serde_json::Value;
use support::{run, run_env};

#[cfg(test)]
mod support {
    use std::path::Path;

    use assert_cmd::Command;
    use serde_json::Value;

    pub struct Run {
        args: Vec<String>,
        pub code: i32,
        pub stdout: String,
        pub stderr: String,
    }

    impl Run {
        /// Everything about the run, for assertion messages.
        pub fn context(&self) -> String {
            format!(
                "argv: omaddons {:?}\nexit: {}\nstdout:\n{}\nstderr:\n{}",
                self.args, self.code, self.stdout, self.stderr
            )
        }

        /// The single JSON document on stdout.
        pub fn envelope(&self) -> Value {
            assert_eq!(
                self.stdout.lines().count(),
                1,
                "one document\n{}",
                self.context()
            );
            assert!(self.stdout.ends_with('\n'), "{}", self.context());
            self.parse(&self.stdout)
        }

        /// Every stderr line, each parsed as one JSON object.
        pub fn events(&self) -> Vec<Value> {
            self.stderr.lines().map(|line| self.parse(line)).collect()
        }

        fn parse(&self, text: &str) -> Value {
            let parsed = serde_json::from_str::<Value>(text);
            assert!(parsed.is_ok(), "{parsed:?}\n{}", self.context());
            parsed.unwrap_or_default()
        }
    }

    fn isolated(home: &Path) -> Command {
        let mut cmd = Command::cargo_bin("omaddons").unwrap();
        cmd.env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            // An empty `/proc`: no running client of this machine is discovered.
            .env("OMADDONS_PROC_ROOT", home.join("no-proc"));
        // Coverage runs: the instrumented binary writes its profile where
        // cargo-llvm-cov collects it, not into the crate root.
        for (key, value) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name == "LLVM_PROFILE_FILE" || name.starts_with("CARGO_LLVM_COV") {
                cmd.env(&key, value);
            }
        }
        cmd
    }

    /// Runs omaddons in an isolated HOME with extra environment, checking that every
    /// stderr line is an NDJSON event and that nothing was written.
    pub fn run_env(args: &[&str], env: &[(&str, &str)]) -> Run {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = isolated(home.path());
        cmd.args(args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let output = cmd.output().unwrap();
        let run = Run {
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        };
        for event in run.events() {
            assert!(
                event["event"].is_string(),
                "every stderr line has an event\n{}",
                run.context()
            );
        }
        let created: Vec<_> = std::fs::read_dir(home.path()).unwrap().collect();
        assert!(
            created.is_empty(),
            "help and usage errors write nothing\n{}",
            run.context()
        );
        run
    }

    pub fn run(args: &[&str]) -> Run {
        run_env(args, &[])
    }
}

type Spelling<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

#[test]
fn bare_omaddons_prints_help_and_exits_0() {
    let r = run(&[]);
    assert_eq!(r.code, 0, "{}", r.context());
    assert!(r.stdout.contains("Usage: omaddons"), "{}", r.context());
    assert!(
        r.stdout.contains("\nLook      status")
            && r.stdout.contains("\nChange    install")
            && r.stdout.contains(
                "\nSetup     targets  config  auth  omarchy  completions  robot-docs  help\n"
            ),
        "one line per group\n{}",
        r.context()
    );
    for name in [
        "ok",
        "failed",
        "usage",
        "not_found",
        "refused",
        "locked",
        "check_failed",
    ] {
        assert!(r.stdout.contains(name), "exit code {name}\n{}", r.context());
    }
    assert_eq!(r.stderr, "", "{}", r.context());
    insta::assert_snapshot!("bare_help_human", r.stdout);
}

#[test]
fn every_json_spelling_gives_one_help_envelope() {
    let spellings: [Spelling<'_>; 5] = [
        (&["--json"], &[]),
        (&["--robot"], &[]),
        (&["--format", "json"], &[]),
        (&["--format=json"], &[]),
        (&[], &[("OMADDONS_FORMAT", "json")]),
    ];
    for (args, env) in spellings {
        let r = run_env(args, env);
        assert_eq!(r.code, 0, "{}", r.context());
        let envelope = r.envelope();
        assert_eq!(
            (&envelope["ok"], &envelope["command"]),
            (&Value::Bool(true), &Value::from("help"))
        );
        let codes: Vec<u64> = envelope["data"]["exit_codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["code"].as_u64().unwrap())
            .collect();
        assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6], "{}", r.context());
        assert_eq!(r.stderr, "", "{}", r.context());
    }
    let mut envelope = run(&["--json"]).envelope();
    envelope["meta"]["elapsed_ms"] = Value::from(0);
    envelope["tool_version"] = Value::from("<version>");
    envelope["data"]["version"] = Value::from("<version>");
    insta::assert_json_snapshot!("bare_help_json", envelope);
}

#[test]
fn help_for_a_command() {
    let r = run(&["help", "help"]);
    assert_eq!(r.code, 0, "{}", r.context());
    assert!(r.stdout.contains("Usage: omaddons help"), "{}", r.context());
    assert!(r.stdout.contains("Global options:"), "{}", r.context());
    let r = run(&["help", "help", "--json"]);
    assert_eq!(r.code, 0, "{}", r.context());
    let envelope = r.envelope();
    assert_eq!(envelope["data"]["command"], "help");
    assert_eq!(envelope["data"]["args"][0]["name"], "command");
    for args in [
        &["help", "-h"][..],
        &["-h", "help"][..],
        &["help", "--help", "--json"][..],
    ] {
        let r = run(args);
        assert_eq!(r.code, 0, "{}", r.context());
        assert!(r.stdout.contains("omaddons help"), "{}", r.context());
    }
    let r = run(&["-h"]);
    assert!(
        r.stdout.contains("Exit codes:"),
        "bare -h is bare help\n{}",
        r.context()
    );
}

#[test]
fn help_walks_a_nested_path() {
    // A command whose bare form runs a default action is still the command's help.
    for command in ["backup", "omarchy"] {
        let via_help = run(&["help", command]);
        let flag = run(&[command, "--help"]);
        assert_eq!(via_help.code, 0, "{}", via_help.context());
        assert_eq!(via_help.stdout, flag.stdout, "{command}");
    }
    for path in [
        &["targets", "add"][..],
        &["targets", "rm"],
        &["targets", "rename"],
        &["targets", "default"],
        &["config", "get"],
        &["config", "set"],
        &["config", "path"],
        &["plans", "show"],
        &["robot-docs", "guide"],
    ] {
        let mut asked = vec!["help"];
        asked.extend(path);
        let via_help = run(&asked);
        let mut direct = path.to_vec();
        direct.push("--help");
        let flag = run(&direct);
        assert_eq!(via_help.code, 0, "{}", via_help.context());
        assert_eq!(via_help.stdout, flag.stdout, "{path:?}");
        asked.push("--json");
        let envelope = run(&asked).envelope();
        // The envelope is help's; the data names the subcommand, as `help "targets add"`.
        assert_eq!(
            (&envelope["command"], &envelope["data"]["command"]),
            (&Value::from("help"), &Value::from(path[1])),
            "{envelope}"
        );
    }
    // An unknown word names the subcommands of the command it follows.
    let r = run(&["help", "targets", "nosuch", "--json"]);
    assert_eq!(r.code, 2, "{}", r.context());
    let error = &r.envelope()["error"];
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.contains("add, rm, rename, default")),
        "{error}"
    );
    assert_eq!(
        error["fix_argv"],
        serde_json::json!(["omaddons", "help", "targets"])
    );
    let r = run(&["help", "config", "st", "--json"]);
    assert_eq!(
        r.envelope()["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help", "config", "set"])
    );
}

/// `auth github` names `auth` with its service (a positional value, not a subcommand):
/// every spelling README shows for a command's help resolves to `auth`'s.
#[test]
fn help_for_auth_and_its_github_subcommand() {
    // `auth`'s help lists its one subcommand.
    let auth = run(&["help", "auth"]);
    assert_eq!(auth.code, 0, "{}", auth.context());
    assert!(auth.stdout.contains("github"), "{}", auth.context());
    let flag = run(&["auth", "--help"]);
    assert_eq!(flag.stdout, auth.stdout, "auth --help");
    let envelope = run(&["help", "auth", "--json"]).envelope();
    assert_eq!(envelope["data"]["command"], "auth", "{envelope}");
    // `github`'s help, however it is asked for; a value of its ACTION stays on it.
    let github = run(&["help", "auth", "github"]);
    assert_eq!(github.code, 0, "{}", github.context());
    for asked in [
        &["help", "auth", "github"][..],
        &["auth", "github", "--help"],
        &["auth", "github", "set", "--help"],
        &["help", "auth", "github", "set"],
    ] {
        let r = run(asked);
        assert_eq!(r.code, 0, "{}", r.context());
        assert_eq!(r.stdout, github.stdout, "{asked:?}");
        let mut json = asked.to_vec();
        json.push("--json");
        let envelope = run(&json).envelope();
        assert_eq!(
            (&envelope["command"], &envelope["data"]["command"]),
            (&Value::from("help"), &Value::from("github")),
            "{envelope}"
        );
    }
    // A misspelled service is still a usage error, with a whole sentence and a fix that
    // resolves.
    let r = run(&["help", "auth", "gihub", "--json"]);
    assert_eq!(r.code, 2, "{}", r.context());
    let error = &r.envelope()["error"];
    assert_eq!(
        error["message"],
        "there is no command \"auth gihub\": auth has github"
    );
    assert_eq!(
        error["fix_argv"],
        serde_json::json!(["omaddons", "help", "auth", "github"])
    );
}

#[test]
fn version() {
    let r = run(&["-V"]);
    assert_eq!(
        (r.code, r.stdout.trim()),
        (0, concat!("omaddons ", env!("CARGO_PKG_VERSION"))),
        "{}",
        r.context()
    );
    let envelope = run(&["--version", "--json"]).envelope();
    assert_eq!(envelope["command"], "version");
    assert_eq!(envelope["data"]["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn unknown_subcommands_suggest_the_closest() {
    let r = run(&["hepl", "--json"]);
    assert_eq!(r.code, 2, "{}", r.context());
    let envelope = r.envelope();
    assert_eq!(envelope["error"]["code"], "usage");
    assert_eq!(envelope["exit_code"], 2);
    assert!(!envelope["error"]["fix"].as_str().unwrap().is_empty());
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help"])
    );
    let errors = r.events();
    assert_eq!(errors.len(), 1, "the error, mirrored once\n{}", r.context());
    assert_eq!(errors[0]["event"], "error");

    let envelope = run(&["help", "hepl", "--json"]).envelope();
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help", "help"])
    );
}

/// The verb aliases (vision §5.10) run their command: the same answer, under the
/// canonical `command`. `uninstall dbm` removes, it never suggests installing.
#[test]
fn verb_aliases_answer_as_their_command() {
    for (alias, canonical) in [
        (&["ls", "--json"][..], &["list", "--json"][..]),
        (&["log", "--json"], &["history", "--json"]),
        (
            &["show", "weakauras", "--offline", "--json"],
            &["info", "weakauras", "--offline", "--json"],
        ),
        (
            &["uninstall", "dbm", "--json"],
            &["remove", "dbm", "--json"],
        ),
        (&["rm", "dbm", "--json"], &["remove", "dbm", "--json"]),
        (&["delete", "dbm", "--json"], &["remove", "dbm", "--json"]),
        (
            &["add", "dbm", "--offline", "--json"],
            &["install", "dbm", "--offline", "--json"],
        ),
        (&["revert", "--json"], &["undo", "--json"]),
        (&["rollback", "--json"], &["undo", "--json"]),
    ] {
        let home = common::home::Home::new(&["wow"]);
        let exec =
            |args: &[&str]| home.exec(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
        let (a, b) = (exec(alias), exec(canonical));
        let (x, y) = (a.envelope(), b.envelope());
        assert_eq!(a.code, b.code, "{}\n{}", a.context(), b.context());
        for key in ["command", "ok", "exit_code", "data"] {
            assert_eq!(x[key], y[key], "{key}: {}\n{}", a.context(), b.context());
        }
        assert_eq!(x["error"]["code"], y["error"]["code"], "{}", a.context());
    }
}

#[test]
fn unknown_flags_suggest_the_closest() {
    let r = run(&["--jsno"]);
    assert_eq!(r.code, 2, "{}", r.context());
    assert_eq!(
        r.stdout,
        "",
        "human mode: stdout empty on error\n{}",
        r.context()
    );
    let events = r.events();
    assert_eq!(events.len(), 1, "{}", r.context());
    assert_eq!(events[0]["code"], "usage");
    // The closest flag is a presentation one: the command without it, which the
    // caller reruns with its own presentation flags.
    assert_eq!(events[0]["fix"], "omaddons", "{}", r.context());

    let r = run(&["help", "--jsno", "--format", "json"]);
    let envelope = r.envelope();
    assert_eq!(envelope["command"], "help");
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "help"])
    );

    let r = run(&["--format", "jsno"]);
    assert_eq!((r.code, r.stdout.as_str()), (2, ""), "{}", r.context());
}

#[test]
fn a_panic_is_one_internal_envelope() {
    if !cfg!(debug_assertions) {
        return;
    }
    let r = run_env(&["--json"], &[("OMADDONS_FAILPOINTS", "cli.panic=panic")]);
    assert_eq!(r.code, 1, "{}", r.context());
    let envelope = r.envelope();
    assert_eq!(envelope["error"]["code"], "internal");
    assert_eq!(
        envelope["error"]["fix_argv"],
        serde_json::json!(["omaddons", "status"])
    );
    let events = r.events();
    assert_eq!(events.len(), 1, "{}", r.context());
    assert_eq!(
        (&events[0]["event"], &events[0]["code"]),
        (&Value::from("error"), &Value::from("internal"))
    );

    let r = run_env(&[], &[("OMADDONS_FAILPOINTS", "cli.panic=panic")]);
    assert_eq!((r.code, r.stdout.as_str()), (1, ""), "{}", r.context());
    assert_eq!(r.events().len(), 1, "{}", r.context());
}

#[test]
fn attribution_flags_are_validated() {
    let reason = "é".repeat(201);
    let r = run(&["--reason", &reason, "--json"]);
    assert_eq!(r.code, 2, "{}", r.context());
    let envelope = r.envelope();
    assert_eq!(envelope["error"]["code"], "usage");
    let fixed = envelope["error"]["fix_argv"][2].as_str().unwrap();
    assert_eq!(fixed.chars().count(), 200, "{}", r.context());
    assert_eq!(run(&["--reason", &"é".repeat(200)]).code, 0);

    let r = run(&["--actor", "robot", "help"]);
    assert_eq!((r.code, r.stdout.as_str()), (2, ""), "{}", r.context());
    assert_eq!(run(&["help", "--actor", "widget"]).code, 0);
    assert_eq!(run_env(&["--json"], &[("OMADDONS_ACTOR", "robot")]).code, 2);
}

#[test]
fn global_flags_go_before_or_after_the_subcommand() {
    for args in [
        &["--json", "-t", "retail", "--offline", "-vv", "help"][..],
        &[
            "help",
            "--json",
            "--flavor",
            "retail",
            "--dry-run",
            "--quiet",
        ][..],
        &[
            "--while-running",
            "help",
            "help",
            "--no-color",
            "--no-progress",
            "--progress",
            "none",
            "--json",
        ][..],
    ] {
        let r = run(args);
        assert_eq!(r.code, 0, "{}", r.context());
        assert_eq!(r.envelope()["ok"], true);
    }
}
