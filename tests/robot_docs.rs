//! e2e suite: robot-docs
//!
//! `omaddons robot-docs schema` and `robot-docs errors` end to end, through the real
//! binary: the listing complete and equal to the library's schemas; one path's entry and
//! every path's own schema command; an unknown path's fix runs; the catalog equal to the
//! registries; the published schemas, fetched from the binary, validating live output;
//! byte-identical repeats in other environments; the human forms; no reads, writes or
//! requests, and no `HOME` needed; bare `robot-docs` as its help until the guide exists.
//!
//! A schema entry runs to hundreds of kilobytes, so large values are compared as the list
//! of what differs, expected empty: the transcript then names the entry, not the bytes.

mod common;
// The contract suite's validator; this suite uses only part of it.
#[path = "contract/validate.rs"]
#[allow(dead_code)]
mod validate;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::env::TestEnv;
use common::home::Home;
use common::netlog::net_log;
use common::tlog::{Ran, Runner, TestLog, tlog};
use omaddons::app::{COMMANDS, command_entry};
use omaddons::contract::events::EventName;
use omaddons::contract::schema::{self, command_schema, envelope_schema, error_variant};
use omaddons::error::{ErrorCode, WarningCode};
use omaddons::state::store::KINDS;
use serde_json::{Value, json};
use validate::Validator;

/// A test bound on the full listing, far above its size today.
const LISTING_MAX_BYTES: usize = 8 << 20;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// The most validation failures a step logs and fails with.
const FAILURES_SHOWN: usize = 10;

const FIX_LINE: &str = "      fix: ";

/// `argv` with `--json` appended.
fn json_argv<'a>(argv: &[&'a str]) -> Vec<&'a str> {
    let mut out = argv.to_vec();
    out.push("--json");
    out
}

/// The full listing's `data`, from one `robot-docs schema --json` run.
fn listing(log: &TestLog, env: &impl Runner) -> Value {
    let ran = log.run(env, &["robot-docs", "schema", "--json"]);
    ran.ok();
    ran.data().clone()
}

/// The envelope a run printed, `meta` removed.
fn without_meta(ran: &Ran) -> Value {
    let mut value = ran.envelope().clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("meta");
    }
    value
}

/// A path's words.
fn words(path: &str) -> Vec<&str> {
    path.split(' ').collect()
}

/// An object's keys, sorted.
fn keys(value: &Value) -> Vec<String> {
    let mut out: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn sorted<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Vec<String> {
    let mut out: Vec<String> = items.into_iter().map(Into::into).collect();
    out.sort();
    out
}

const NONE: &[String] = &[];

#[test]
fn schema_lists_every_path_the_envelope_every_event_and_every_file() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("full listing");
    let ran = log.run(&env, &["robot-docs", "schema", "--json"]);
    ran.ok();
    let data = ran.data();
    log.assert_eq(
        "data keys",
        &keys(data),
        &sorted(["commands", "envelope", "events", "files"]),
    );
    log.assert_eq(
        "commands",
        &keys(&data["commands"]),
        &sorted(COMMANDS.iter().map(|row| row.name)),
    );
    log.assert_eq(
        "events",
        &keys(&data["events"]),
        &sorted(EventName::ALL.iter().map(|name| name.wire())),
    );
    let files = schema::file_schemas();
    log.assert_eq(
        "files",
        &keys(&data["files"]),
        &sorted(files.keys().copied()),
    );
    let missing: Vec<String> = KINDS
        .iter()
        .map(|row| row.kind)
        .chain(["journal", "desired_state"])
        .filter(|kind| data["files"].get(kind).is_none())
        .map(str::to_owned)
        .collect();
    log.assert_eq("file kinds missing", &missing, &NONE.to_vec());

    log.step("every entry is the library's");
    let mut expected: Vec<(String, &Value, Value)> = vec![(
        "envelope".to_owned(),
        &data["envelope"],
        envelope_schema().to_value(),
    )];
    for row in COMMANDS {
        expected.push((
            format!("command {}", row.name),
            &data["commands"][row.name],
            command_schema(row).to_value(),
        ));
    }
    for (name, library) in schema::event_schemas() {
        expected.push((
            format!("event {name}"),
            &data["events"][name],
            library.to_value(),
        ));
    }
    for (kind, library) in files {
        expected.push((
            format!("file {kind}"),
            &data["files"][kind],
            library.to_value(),
        ));
    }
    let not_draft: Vec<String> = expected
        .iter()
        .filter(|(_, entry, _)| entry["$schema"] != DRAFT_2020_12)
        .map(|(label, _, _)| label.clone())
        .collect();
    log.assert_eq("entries not on draft 2020-12", &not_draft, &NONE.to_vec());
    let differing: Vec<String> = expected
        .iter()
        .filter(|(_, entry, library)| *entry != library)
        .map(|(label, _, _)| label.clone())
        .collect();
    log.assert_eq(
        "entries differing from the library's",
        &differing,
        &NONE.to_vec(),
    );
    let bytes = ran.stdout().len();
    log.json(
        "counts",
        &json!({
            "commands": keys(&data["commands"]).len(),
            "events": keys(&data["events"]).len(),
            "files": keys(&data["files"]).len(),
            "entries compared": expected.len(),
            "bytes": bytes,
        }),
    );
    log.assert_eq(
        "under LISTING_MAX_BYTES",
        &(bytes < LISTING_MAX_BYTES),
        &true,
    );
}

#[test]
fn schema_of_one_command_gives_exactly_one_entry() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("listing");
    let all = listing(&log, &env);
    for (argv, path) in [
        (&["robot-docs", "schema", "install"][..], "install"),
        (&["robot-docs", "schema", "add"], "install"),
        (
            &["robot-docs", "schema", "backup", "restore"],
            "backup restore",
        ),
        (
            &["robot-docs", "schema", "backup restore"],
            "backup restore",
        ),
        (
            &["robot-docs", "schema", "robot-docs", "schema"],
            "robot-docs schema",
        ),
        (&["robot-docs", "schema", "backup"], "backup"),
    ] {
        log.step(&argv.join(" "));
        let ran = log.run(&env, &json_argv(argv));
        ran.ok();
        let data = ran.data();
        log.assert_eq(
            "the one key",
            &keys(&data["commands"]),
            &vec![path.to_owned()],
        );
        let not_null: Vec<&str> = ["envelope", "events", "files"]
            .into_iter()
            .filter(|key| !data[key].is_null())
            .collect();
        log.assert_eq("keys not null", &not_null, &Vec::new());
        log.assert_eq(
            "the entry is the listing's",
            &(data["commands"][path] == all["commands"][path]),
            &true,
        );
    }
}

#[test]
fn every_command_path_has_a_runnable_schema_command() {
    let log = tlog!();
    let env = TestEnv::new();
    for row in COMMANDS {
        log.step(row.name);
        let mut argv = vec!["robot-docs", "schema"];
        argv.extend(words(row.name));
        let ran = log.run(&env, &json_argv(&argv));
        ran.ok();
        log.assert_eq(
            "the one key",
            &keys(&ran.data()["commands"]),
            &vec![row.name.to_owned()],
        );
    }
}

#[test]
fn an_unknown_path_is_usage_with_the_closest_path_and_the_fix_runs() {
    let log = tlog!();
    let env = TestEnv::new();
    log.tree("home", env.root());
    let mut cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "instal",
            vec!["omaddons", "robot-docs", "schema", "install"],
        ),
        (
            "backup restor",
            vec!["omaddons", "robot-docs", "schema", "backup", "restore"],
        ),
        (
            "install extra",
            vec!["omaddons", "robot-docs", "schema", "install"],
        ),
        ("nosuch", vec!["omaddons", "help", "robot-docs", "schema"]),
    ];
    // While `auth` is a parent with no row of its own, its first row is the fix.
    if command_entry("auth").is_none() {
        cases.push((
            "auth",
            vec!["omaddons", "robot-docs", "schema", "auth", "github"],
        ));
    }
    for (typed, fix) in cases {
        log.step(typed);
        let mut argv = vec!["robot-docs", "schema"];
        argv.extend(typed.split(' '));
        let ran = log.run(&env, &json_argv(&argv));
        ran.fails(2, "usage");
        let error = &ran.envelope()["error"];
        log.assert_eq("retryable", &error["retryable"], &json!(false));
        log.assert_eq("details", &error["details"], &json!({}));
        log.assert_eq("fix_argv", &error["fix_argv"], &json!(fix));
        log.assert_unchanged("home", env.root());
        let fix_argv: Vec<&str> = error["fix_argv"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .map(|word| word.as_str().unwrap())
            .collect();
        log.run(&env, &json_argv(&fix_argv)).ok();
    }
}

#[test]
fn errors_lists_every_code_with_its_facts() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("the catalog");
    let ran = log.run(&env, &["robot-docs", "errors", "--json"]);
    ran.ok();
    let data = ran.data();
    let errors = data["error_codes"].as_array().unwrap();
    let warnings = data["warning_codes"].as_array().unwrap();
    let code_of = |row: &Value| row["code"].as_str().unwrap().to_owned();
    log.assert_eq(
        "error codes in order",
        &errors.iter().map(code_of).collect::<Vec<_>>(),
        &ErrorCode::ALL
            .iter()
            .map(|code| code.wire().to_owned())
            .collect(),
    );
    log.assert_eq(
        "warning codes in order",
        &warnings.iter().map(code_of).collect::<Vec<_>>(),
        &WarningCode::ALL
            .iter()
            .map(|code| code.wire().to_owned())
            .collect(),
    );

    log.step("every field from its accessor");
    // `code: field` for every published fact that differs from the library's.
    let mut differing: Vec<String> = Vec::new();
    for (row, code) in errors.iter().zip(ErrorCode::ALL) {
        let union_details = error_variant(*code).to_value()["properties"]["details"].clone();
        let facts = [
            ("exit", json!(code.exit().code())),
            (
                "retryable",
                serde_json::to_value(code.retryability()).unwrap(),
            ),
            ("meaning", json!(code.meaning())),
            ("details_schema", code.details_schema()),
            ("details_schema (the error union's)", union_details),
            ("fix_pattern", json!(code.fix_pattern())),
        ];
        for (field, expected) in facts {
            let published = &row[field.split(' ').next().unwrap()];
            if *published != expected {
                differing.push(format!("{code}: {field}"));
            }
        }
        // The doctor dictionary (E3) is not on main yet: no row publishes a doctor exit.
        if row.get("doctor_exit").is_some() {
            differing.push(format!("{code}: doctor_exit before the doctor dictionary"));
        }
    }
    for (row, code) in warnings.iter().zip(WarningCode::ALL) {
        for (field, expected) in [
            ("meaning", code.meaning()),
            ("fix_pattern", code.fix_pattern()),
        ] {
            if row[field] != expected {
                differing.push(format!("{code}: {field}"));
            }
        }
    }
    log.assert_eq(
        "facts differing from the registries",
        &differing,
        &NONE.to_vec(),
    );

    log.step("the conditional codes");
    let conditional: BTreeSet<String> = errors
        .iter()
        .filter(|row| row["retryable"] == "conditional")
        .map(code_of)
        .collect();
    log.json("conditional", &conditional);
    log.assert_eq(
        "the conditional codes",
        &conditional,
        &ErrorCode::CONDITIONALLY_RETRYABLE
            .iter()
            .map(|code| code.wire().to_owned())
            .collect(),
    );
    log.json(
        "counts",
        &json!({"error_codes": errors.len(), "warning_codes": warnings.len()}),
    );
}

/// Validates `instance` against `schema` as published and in strict form, logging each
/// form's failures with the document; fails with the first ten.
fn validates(log: &TestLog, label: &str, schema: &Value, instance: &Value) {
    for strict in [false, true] {
        let validator = Validator::new(schema, strict);
        log.assert_eq(
            &format!("{label}: a validator (strict: {strict})"),
            &validator.as_ref().err(),
            &None,
        );
        // The assertion above failed the step on an error.
        let Ok(validator) = validator else { return };
        let failures: Vec<String> = validator
            .validate(instance)
            .iter()
            .take(FAILURES_SHOWN)
            .map(ToString::to_string)
            .collect();
        if !failures.is_empty() {
            log.json("document", instance);
        }
        log.assert_eq(
            &format!("{label}: failures (strict: {strict})"),
            &failures,
            &NONE.to_vec(),
        );
    }
}

#[test]
fn published_schemas_validate_live_output() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    log.step("listing");
    let published = listing(&log, &home);
    for argv in [
        &["status"][..],
        &["list"],
        &["help"],
        &["robot-docs", "errors"],
        &["robot-docs", "schema"],
        &["robot-docs", "schema", "install"],
    ] {
        log.step(&argv.join(" "));
        let ran = log.run(&home, &json_argv(argv));
        ran.ok();
        let envelope = ran.envelope();
        let command = envelope["command"].as_str().unwrap();
        validates(
            &log,
            &format!("{command} data"),
            &published["commands"][command],
            &envelope["data"],
        );
        let mut frame = envelope.clone();
        if let Some(object) = frame.as_object_mut() {
            object.remove("data");
            object.remove("error");
        }
        validates(
            &log,
            &format!("{command} frame"),
            &published["envelope"],
            &frame,
        );
    }

    log.step("status -v events");
    let ran = log.run(&home, &["status", "-v", "--progress", "json", "--json"]);
    ran.ok();
    let events = ran.events();
    let names: BTreeSet<&str> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    log.json("events", &json!({"lines": events.len(), "names": names}));
    log.assert_eq("status -v writes events", &events.is_empty(), &false);
    for event in &events {
        let name = event["event"].as_str().unwrap();
        validates(
            &log,
            &format!("event {name}"),
            &published["events"][name],
            event,
        );
    }

    log.step("a usage failure");
    let ran = log.run(&home, &["robot-docs", "schema", "nosuch", "--json"]);
    ran.fails(2, "usage");
    validates(
        &log,
        "a failure's envelope",
        &published["envelope"],
        ran.envelope(),
    );
}

#[test]
fn outputs_are_byte_identical_across_runs_and_environments() {
    let log = tlog!();
    let env = TestEnv::new();
    let elsewhere = env.root().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    for (argv, json) in [
        (&["robot-docs", "schema", "--json"][..], true),
        (&["robot-docs", "schema", "install", "--json"], true),
        (&["robot-docs", "errors", "--json"], true),
        (&["robot-docs", "schema", "install"], false),
        (&["robot-docs", "errors"], false),
    ] {
        log.step(&argv.join(" "));
        let runs: Vec<Ran> = vec![
            log.run(&env, argv),
            log.run(&env, argv),
            log.cmd(&env, argv)
                .env("TZ", "Asia/Tokyo")
                .env("LANG", "C")
                .cwd(&elsewhere)
                .run(),
        ];
        for ran in &runs {
            ran.ok();
        }
        let shown: Vec<Value> = runs
            .iter()
            .map(|ran| {
                if json {
                    without_meta(ran)
                } else {
                    json!(ran.stdout())
                }
            })
            .collect();
        let identical = (shown[0] == shown[1], shown[0] == shown[2]);
        log.json(
            "runs",
            &json!({
                "lengths": runs.iter().map(|ran| ran.stdout().len()).collect::<Vec<_>>(),
                "compared": if json { "meta removed" } else { "byte for byte" },
                "repeat identical": identical.0,
                "TZ, LANG and directory changed, identical": identical.1,
            }),
        );
        log.assert_eq("identical", &identical, &(true, true));
    }
}

#[test]
fn human_forms_are_pretty_json_and_a_table() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("schema install, human and JSON");
    let human = log.run(&env, &["robot-docs", "schema", "install"]);
    human.ok();
    let json_run = log.run(&env, &["robot-docs", "schema", "install", "--json"]);
    json_run.ok();
    let parsed: Value = serde_json::from_str(human.stdout()).unwrap();
    log.assert_eq(
        "the human form is the data",
        &(parsed == *json_run.data()),
        &true,
    );
    log.assert_eq(
        "pretty, not one line",
        &(human.stdout().lines().count() > 1),
        &true,
    );

    log.step("errors, plain and -v");
    let plain = log.run(&env, &["robot-docs", "errors"]);
    plain.ok();
    let verbose = log.run(&env, &["robot-docs", "errors", "-v"]);
    verbose.ok();
    let text = plain.stdout();
    log.assert_eq(
        "first line",
        &text.lines().next(),
        &Some(format!("Error codes ({})", ErrorCode::ALL.len()).as_str()),
    );
    let heading = format!("Warning codes ({})", WarningCode::ALL.len());
    log.assert_eq(
        "the warnings heading",
        &text.lines().any(|line| line == heading),
        &true,
    );
    let not_one_line: Vec<String> = ErrorCode::ALL
        .iter()
        .map(|code| code.wire())
        .chain(WarningCode::ALL.iter().map(|code| code.wire()))
        .filter(|code| {
            text.lines()
                .filter(|line| line.split_whitespace().next() == Some(*code))
                .count()
                != 1
        })
        .map(str::to_owned)
        .collect();
    log.assert_eq(
        "codes without exactly one line",
        &not_one_line,
        &NONE.to_vec(),
    );
    log.assert_eq(
        "ANSI escapes",
        &(text.contains('\u{1b}') || verbose.stdout().contains('\u{1b}')),
        &false,
    );
    let (fixes, rest): (Vec<&str>, Vec<&str>) = verbose
        .stdout()
        .lines()
        .partition(|line| line.starts_with(FIX_LINE));
    log.assert_eq(
        "fix lines under -v",
        &fixes.len(),
        &(ErrorCode::ALL.len() + WarningCode::ALL.len()),
    );
    log.assert_eq(
        "-v leaves the other lines unchanged",
        &rest,
        &text.lines().collect::<Vec<_>>(),
    );
    // Each fix line follows its code's line.
    let lines: Vec<&str> = verbose.stdout().lines().collect();
    let misplaced: Vec<&str> = lines
        .windows(2)
        .filter(|pair| pair[1].starts_with(FIX_LINE))
        .filter(|pair| {
            pair[0].starts_with(FIX_LINE) || pair[0].starts_with(' ') || pair[0].is_empty()
        })
        .map(|pair| pair[1])
        .collect();
    log.assert_eq("fix lines not after a code line", &misplaced, &Vec::new());
    for ran in [&human, &json_run, &plain, &verbose] {
        log.assert_eq("stderr", &ran.stderr(), &"");
    }
}

#[test]
fn robot_docs_reads_nothing_writes_nothing_and_needs_no_home() {
    let log = tlog!();
    let home = Home::new(&["wow", "wow_classic"]);
    let env = &home.env;
    let trees: Vec<(&str, PathBuf)> = vec![
        ("state", env.state()),
        ("cache", env.cache()),
        ("config", env.config()),
        ("retail", home.client("_retail_")),
        ("classic", home.client("_classic_")),
    ];
    for (label, root) in &trees {
        log.tree(label, root);
    }
    let mut runs: Vec<Ran> = Vec::new();

    log.step("both commands");
    let argvs = [
        &["robot-docs", "schema", "--json"][..],
        &["robot-docs", "errors", "--json"],
    ];
    for argv in argvs {
        let ran = log.run(&home, argv);
        ran.ok();
        runs.push(ran);
    }
    for (label, root) in &trees {
        log.assert_unchanged(label, root);
    }
    log.assert_eq("net.log entries", &net_log(&home).entries.len(), &0);
    let expected: Vec<Value> = runs.iter().map(without_meta).collect();

    for (label, home_var) in [
        ("HOME relative", Some("relative/home")),
        ("HOME unset", None),
    ] {
        log.step(label);
        if home_var.is_none() {
            log.note("every run of this step removes HOME from its environment");
        }
        let with_home = |argv: &[&str]| {
            let cmd = log.cmd(&home, argv);
            match home_var {
                Some(value) => cmd.env("HOME", value),
                None => cmd.env_remove("HOME"),
            }
            .run()
        };
        // The control: a command that needs HOME refuses this one.
        with_home(&["status", "--json"]).fails(2, "usage");
        for (argv, earlier) in argvs.iter().zip(&expected) {
            let ran = with_home(argv);
            ran.ok();
            log.assert_eq("the same output", &(without_meta(&ran) == *earlier), &true);
            runs.push(ran);
        }
    }

    log.step("a config.toml that does not parse");
    let config = env.config().join("omaddons");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "[[[\n").unwrap();
    // The control: a command that reads the config refuses this one.
    log.run(&home, &["status", "--json"])
        .fails(1, "state_corrupt");
    for (argv, earlier) in argvs.iter().zip(&expected) {
        let ran = log.run(&home, argv);
        ran.ok();
        log.assert_eq("the same output", &(without_meta(&ran) == *earlier), &true);
        runs.push(ran);
    }
    log.assert_eq("net.log entries", &net_log(&home).entries.len(), &0);

    let root = env.root().display().to_string();
    let naming_the_home: Vec<String> = runs
        .iter()
        .filter(|ran| ran.stdout().contains(&root) || ran.stderr().contains(&root))
        .map(|ran| format!("run {}", ran.seq))
        .collect();
    log.assert_eq(
        "runs whose output names the test home",
        &naming_the_home,
        &NONE.to_vec(),
    );
}

#[test]
fn bare_robot_docs_is_its_help_until_the_guide_exists() {
    let log = tlog!();
    let env = TestEnv::new();
    if command_entry("robot-docs guide").is_some() {
        log.note("robot-docs guide exists: bare robot-docs is the guide, which B1 tests");
        let ran = log.run(&env, &["robot-docs", "--json"]);
        ran.ok();
        log.assert_eq(
            "command",
            &ran.envelope()["command"],
            &json!("robot-docs guide"),
        );
        return;
    }
    log.note("no robot-docs guide yet: bare robot-docs is its help");
    log.step("bare, as JSON");
    let ran = log.run(&env, &["robot-docs", "--json"]);
    ran.ok();
    log.assert_eq("command", &ran.envelope()["command"], &json!("help"));
    log.assert_eq("data.command", &ran.data()["command"], &json!("robot-docs"));

    log.step("the three spellings");
    let texts: Vec<String> = [
        &["robot-docs"][..],
        &["help", "robot-docs"],
        &["robot-docs", "--help"],
    ]
    .iter()
    .map(|argv| {
        let ran = log.run(&env, argv);
        ran.ok();
        ran.stdout().to_owned()
    })
    .collect();
    log.assert_eq("help robot-docs", &texts[1], &texts[0]);
    log.assert_eq("robot-docs --help", &texts[2], &texts[0]);
    log.assert_eq(
        "names schema and errors",
        &(texts[0].contains("schema") && texts[0].contains("errors")),
        &true,
    );
}
