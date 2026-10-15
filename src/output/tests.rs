//! `emit` and the envelope, with in-memory stdout and stderr.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Instant;

use schemars::JsonSchema;
use serde::Serialize;

use super::*;
use crate::error::{ErrorCode, Suggestion, WarningCode};

#[derive(Debug, Serialize, JsonSchema)]
struct DemoData {
    zeta: u32,
    alpha: String,
    map: BTreeMap<String, u32>,
}

struct Demo {
    data: DemoData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    partial: bool,
    assertion: Option<Error>,
}

impl Outcome for Demo {
    type Data = DemoData;

    fn command(&self) -> &'static str {
        "update"
    }

    fn data(&self) -> &DemoData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn assertion_failure(&self) -> Option<Error> {
        self.assertion.clone()
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> io::Result<()> {
        writeln!(w, "{} {}", style.bold(&self.data.alpha), self.data.zeta)
    }
}

fn demo() -> Demo {
    Demo {
        data: DemoData {
            zeta: 1,
            alpha: "a".into(),
            map: BTreeMap::from([("b".into(), 2), ("a".into(), 1)]),
        },
        warnings: vec![
            Warning::new(WarningCode::ReloadRequired, "retail needs a /reload")
                .with_fix(Suggestion::text("/reload in game")),
        ],
        commands: vec![CommandHint::new(
            "undo",
            ["omaddons", "undo", "01JB8Q2VA00000000000000000"],
            false,
        )],
        partial: false,
        assertion: None,
    }
}

fn ambiguous() -> Error {
    Error::new(
        ErrorCode::TargetAmbiguous,
        "weakauras is compatible with 2 targets",
        Suggestion::command(["omaddons", "install", "weakauras", "-t", "retail"]),
    )
    .alternative(Suggestion::command([
        "omaddons",
        "install",
        "weakauras",
        "-t",
        "forever",
    ]))
    .details(serde_json::json!({ "candidates": ["retail", "forever"] }))
}

fn check_failed() -> Error {
    Error::new(
        ErrorCode::CheckFailed,
        "2 updates are pending",
        Suggestion::command(["omaddons", "update", "--all"]),
    )
}

/// Top-level keys in serialized order (serde_json::Value would sort them).
fn top_level_keys(text: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let mut keys: Vec<(usize, String)> = value
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (text.find(&format!("\"{k}\":")).unwrap(), k.clone()))
        .collect();
    keys.sort();
    keys.into_iter().map(|(_, k)| k).collect()
}

struct Run {
    exit: Exit,
    stdout: String,
    stderr: String,
}

fn run(cfg: OutputConfig, result: Result<Demo, Error>) -> Run {
    let mut stdout = Vec::new();
    let mut reporter = Reporter::new(Vec::new(), &cfg);
    let exit = emit_to(
        &cfg,
        &mut stdout,
        &mut reporter,
        "update",
        Instant::now(),
        false,
        result,
    );
    Run {
        exit,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(reporter.into_sink()).unwrap(),
    }
}

fn human() -> OutputConfig {
    OutputConfig {
        output: OutputMode::Human,
        ..OutputConfig::machine()
    }
}

const SUCCESS_KEYS: [&str; 10] = [
    "ok",
    "schema_version",
    "tool_version",
    "command",
    "exit_code",
    "partial",
    "data",
    "warnings",
    "commands",
    "meta",
];
const FAILURE_KEYS: [&str; 9] = [
    "ok",
    "schema_version",
    "tool_version",
    "command",
    "exit_code",
    "error",
    "warnings",
    "commands",
    "meta",
];

#[test]
fn success_envelopes_keep_key_order_and_data_order() {
    let r = run(OutputConfig::machine(), Ok(demo()));
    assert_eq!(r.exit, Exit::Ok);
    assert_eq!(r.stdout.lines().count(), 1);
    assert!(r.stdout.ends_with('\n'));
    assert_eq!(top_level_keys(&r.stdout), SUCCESS_KEYS);
    assert!(
        r.stdout
            .contains(r#""data":{"zeta":1,"alpha":"a","map":{"a":1,"b":2}}"#),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains(r#""tool_version":""#));
}

#[test]
fn failure_envelopes_carry_the_error() {
    let r = run(OutputConfig::machine(), Err(ambiguous()));
    assert_eq!(r.exit, Exit::NotFound);
    assert_eq!(r.stdout.lines().count(), 1);
    assert_eq!(top_level_keys(&r.stdout), FAILURE_KEYS);
    let envelope: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(
        (envelope["ok"].as_bool(), envelope["exit_code"].as_u64()),
        (Some(false), Some(3))
    );
    assert_eq!(envelope["command"], "update");
    assert_eq!(envelope["error"]["fix_argv"][4], "retail");
}

#[test]
fn check_failed_keeps_the_data() {
    let outcome = Demo {
        assertion: Some(check_failed()),
        ..demo()
    };
    let r = run(OutputConfig::machine(), Ok(outcome));
    assert_eq!(r.exit, Exit::CheckFailed);
    assert_eq!(r.stdout.lines().count(), 1);
    let envelope: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(
        (envelope["ok"].as_bool(), envelope["exit_code"].as_u64()),
        (Some(false), Some(6))
    );
    assert!(
        envelope["data"].is_object()
            && envelope["error"].is_object()
            && envelope["partial"].is_boolean()
    );
    assert_eq!(envelope["error"]["code"], "check_failed");
}

#[test]
fn human_output_is_empty_on_failure() {
    let r = run(human(), Err(ambiguous()));
    assert_eq!((r.exit, r.stdout.as_str()), (Exit::NotFound, ""));
    let r = run(human(), Ok(demo()));
    assert_eq!(r.stdout, "a 1\n");
    let r = run(
        human(),
        Ok(Demo {
            assertion: Some(check_failed()),
            ..demo()
        }),
    );
    assert_eq!((r.exit, r.stdout.as_str()), (Exit::CheckFailed, "a 1\n"));
}

#[test]
fn ndjson_stderr_mirrors_each_error_once() {
    for (cfg, result) in [
        (OutputConfig::machine(), Err(ambiguous())),
        (human(), Err(ambiguous())),
        (
            OutputConfig::machine(),
            Ok(Demo {
                assertion: Some(check_failed()),
                ..demo()
            }),
        ),
    ] {
        let r = run(cfg, result);
        let events: Vec<serde_json::Value> = r
            .stderr
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        for event in &events {
            assert!(
                matches!(
                    event["event"].as_str(),
                    Some("progress" | "log" | "warning" | "error")
                ),
                "{event}"
            );
        }
        assert_eq!(
            events.iter().filter(|e| e["event"] == "error").count(),
            1,
            "{}",
            r.stderr
        );
    }
    let r = run(OutputConfig::machine(), Ok(demo()));
    assert_eq!(r.stderr.lines().count(), 1, "the warning, mirrored");
}

#[test]
fn a_closed_stdout_is_not_an_error() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }
    let cfg = OutputConfig::machine();
    let mut reporter = Reporter::new(Vec::new(), &cfg);
    let exit = emit_to(
        &cfg,
        &mut Closed,
        &mut reporter,
        "update",
        Instant::now(),
        false,
        Ok(demo()),
    );
    assert_eq!(exit, Exit::Ok);
    assert!(stdout_written());
}

/// Envelope shapes. `meta` is fixed and the crate version masked, so neither timing nor
/// a version bump churns the snapshots.
#[test]
fn envelope_snapshots() {
    fn stable<D: Serialize>(mut envelope: Envelope<'_, D>) -> Envelope<'_, D> {
        envelope.tool_version = "<version>";
        envelope
    }
    let meta = Meta {
        elapsed_ms: 0,
        offline: false,
    };
    let d = demo();
    let partial = Envelope::success("update", &d.data, true, &d.warnings, &d.commands, meta);
    insta::assert_json_snapshot!("envelope_partial_success", stable(partial));
    let success = Envelope::success("update", &d.data, false, &d.warnings, &d.commands, meta);
    insta::assert_json_snapshot!("envelope_success", stable(success));
    let failure = Envelope::failure("install", &ambiguous(), &[], &[], meta);
    insta::assert_json_snapshot!("envelope_failure", stable(failure));
    let failed =
        Envelope::check_failed("outdated", &d.data, false, &check_failed(), &[], &[], meta);
    insta::assert_json_snapshot!("envelope_check_failed", stable(failed));
}
