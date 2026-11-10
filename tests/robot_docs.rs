//! e2e suite: robot-docs
//!
//! `omaddons robot-docs schema` and `robot-docs errors` end to end, through the real
//! binary: the listing complete and equal to the library's schemas; one path's entry and
//! every path's own schema command; an unknown path's fix runs; the catalog equal to the
//! registries; the published schemas, fetched from the binary, validating live output;
//! byte-identical repeats in other environments; the human forms; no reads, writes or
//! requests, and no `HOME` needed; bare `robot-docs` as its help until the guide exists.
//! And `robot-docs install-skill` into a throwaway `HOME`: a dry run writes nothing, the
//! install is the committed skill byte for byte with its `skill` history line and
//! projection folder, undo steps over it, and a second run changes nothing.
//!
//! A schema entry runs to hundreds of kilobytes, so large values are compared as the list
//! of what differs, expected empty: the transcript then names the entry, not the bytes.

use crate::common;
use crate::validate;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::archives::zip_text;
use common::env::TestEnv;
use common::guide;
use common::home::Home;
use common::netlog::net_log;
use common::tlog::{Ran, Runner, TestLog, tlog};
use omaddons::app::{COMMANDS, command_entry};
use omaddons::contract::events::EventName;
use omaddons::contract::schema::{self, command_schema, envelope_schema, error_variant};
use omaddons::doctor::exit::exit_for;
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
            ("doctor_exit", json!(exit_for(*code).code())),
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

/// A home with one discovered client and one installed addon (`Foo` from a local zip).
fn managed_home(log: &TestLog) -> Home {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    let zip = dl.join("foo.zip");
    let written = std::fs::create_dir_all(&dl).and_then(|()| {
        std::fs::write(&zip, zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n")]))
    });
    assert!(written.is_ok(), "{}: {written:?}", zip.display());
    let zip = zip.display().to_string();
    log.run(
        &home,
        &[
            "install", "gh:o/foo", "--file", &zip, "-t", "retail", "--json",
        ],
    )
    .ok();
    home
}

/// `ran` printed exactly `baseline`, else the step fails at the first differing line.
fn same_guide(log: &TestLog, case: &str, baseline: &str, ran: &Ran) {
    ran.ok();
    // Any change moves the stamp (line 5), so the content's first difference is reported,
    // and the stamp's only when nothing else differs.
    let without_stamp = |text: &str| {
        let mut lines: Vec<&str> = text.split('\n').collect();
        if lines.len() > 4 {
            lines.remove(4);
        }
        lines.join("\n")
    };
    let difference =
        guide::first_difference(&without_stamp(baseline), &without_stamp(ran.stdout()))
            .map(|mut d| {
                if d.line >= 5 {
                    d.line += 1;
                }
                d
            })
            .or_else(|| guide::first_difference(baseline, ran.stdout()));
    log.assert_eq(
        &format!("{case}: the first line that differs from the baseline"),
        &difference.map(|d| {
            format!(
                "line {}: {:?} instead of {:?}",
                d.line, d.rendered, d.committed
            )
        }),
        &None,
    );
}

#[test]
fn guide_bare_robot_docs_is_the_guide() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("human");
    let bare = log.run(&env, &["robot-docs"]);
    let guide_run = log.run(&env, &["robot-docs", "guide"]);
    same_guide(&log, "bare", guide_run.stdout(), &bare);
    log.step("JSON");
    let bare = log.run(&env, &["robot-docs", "--json"]);
    let guide_run = log.run(&env, &["robot-docs", "guide", "--json"]);
    for ran in [&bare, &guide_run] {
        ran.ok();
        log.assert_eq(
            "command",
            &ran.envelope()["command"],
            &json!("robot-docs guide"),
        );
    }
    log.assert_eq(
        "the envelopes, meta removed",
        &(without_meta(&bare) == without_meta(&guide_run)),
        &true,
    );
}

#[test]
fn guide_human_bytes_are_the_json_content() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("both forms");
    let human = log.run(&env, &["robot-docs", "guide"]);
    let json_run = log.run(&env, &["robot-docs", "guide", "--json"]);
    human.ok();
    json_run.ok();
    let envelope = json_run.envelope();
    log.assert_eq(
        "the human bytes are data.content",
        &(envelope["data"]["content"].as_str() == Some(human.stdout())),
        &true,
    );
    let frame = json!({
        "format": envelope["data"]["format"],
        "ok": envelope["ok"],
        "exit_code": envelope["exit_code"],
        "warnings": envelope["warnings"],
        "commands": envelope["commands"],
        "partial": envelope["partial"],
    });
    log.assert_eq(
        "the frame",
        &frame,
        &json!({"format": "markdown", "ok": true, "exit_code": 0, "warnings": [], "commands": [], "partial": false}),
    );
    for ran in [&human, &json_run] {
        log.assert_eq("stderr", &ran.stderr(), &"");
    }
}

#[test]
fn guide_is_the_same_on_every_machine() {
    let log = tlog!();
    log.step("baseline");
    let baseline = guide::rendered(&log, &TestEnv::new());
    let guide_argv = ["robot-docs", "guide"];

    let case = log.case("broken_config");
    let env = TestEnv::new();
    std::fs::create_dir_all(env.config().join("omaddons")).unwrap();
    std::fs::write(env.config().join("omaddons/config.toml"), "limits = [\n").unwrap();
    same_guide(
        &case,
        "broken_config",
        &baseline,
        &case.run(&env, &guide_argv),
    );

    let case = log.case("config_with_overrides");
    let env = TestEnv::new();
    std::fs::create_dir_all(env.config().join("omaddons")).unwrap();
    std::fs::write(
        env.config().join("omaddons/config.toml"),
        "default_channel = \"beta\"\ncheck_interval_hours = 1\ngit_hosts = [\"git.example.org\"]\n",
    )
    .unwrap();
    // The control: the config is valid, so it is read rather than refused.
    case.run(&env, &["config", "get", "default_channel", "--json"])
        .ok();
    same_guide(
        &case,
        "config_with_overrides",
        &baseline,
        &case.run(&env, &guide_argv),
    );

    let case = log.case("managed_home");
    let home = managed_home(&case);
    same_guide(
        &case,
        "managed_home",
        &baseline,
        &case.run(&home, &guide_argv),
    );

    let case = log.case("relative_home");
    let env = TestEnv::new();
    case.cmd(&env, &["status", "--json"])
        .env("HOME", "relative/home")
        .run()
        .fails(2, "usage");
    let ran = case
        .cmd(&env, &guide_argv)
        .env("HOME", "relative/home")
        .run();
    same_guide(&case, "relative_home", &baseline, &ran);

    for (name, columns) in [("narrow_terminal", "40"), ("wide_terminal", "200")] {
        let case = log.case(name);
        let env = TestEnv::new();
        let ran = case.cmd(&env, &guide_argv).env("COLUMNS", columns).run();
        same_guide(&case, name, &baseline, &ran);
    }

    for (name, flag) in [("verbose", "-v"), ("very_verbose", "-vv")] {
        let case = log.case(name);
        let env = TestEnv::new();
        let ran = case.run(&env, &["robot-docs", "guide", flag]);
        same_guide(&case, name, &baseline, &ran);
    }

    let case = log.case("progress_json");
    let env = TestEnv::new();
    let ran = case.run(&env, &["robot-docs", "guide", "--progress", "json"]);
    same_guide(&case, "progress_json", &baseline, &ran);
    case.assert_eq("stderr", &ran.stderr(), &"");

    let case = log.case("other_working_directory");
    let env = TestEnv::new();
    let home_dir = env.root().join("home");
    std::fs::create_dir_all(&home_dir).unwrap();
    for dir in [home_dir, env.root().to_path_buf()] {
        let ran = case.cmd(&env, &guide_argv).cwd(&dir).run();
        same_guide(&case, "other_working_directory", &baseline, &ran);
    }

    let case = log.case("target_flag");
    let env = TestEnv::new();
    let ran = case.run(&env, &["robot-docs", "guide", "-t", "retail"]);
    same_guide(&case, "target_flag", &baseline, &ran);
}

#[test]
fn guide_reads_and_writes_nothing() {
    let log = tlog!();
    log.step("a managed home");
    let home = managed_home(&log);
    let root = home.env.root().to_path_buf();
    log.tree("root", &root);
    let requests = net_log(&home).entries.len();
    log.step("every form");
    for argv in [
        &["robot-docs", "guide"][..],
        &["robot-docs", "guide", "--json"],
        &["robot-docs"],
        &[
            "robot-docs",
            "guide",
            "--dry-run",
            "--reason",
            "x",
            "--json",
        ],
    ] {
        log.run(&home, argv).ok();
    }
    log.assert_unchanged("root", &root);
    log.assert_eq("net.log entries", &net_log(&home).entries.len(), &requests);
}

#[test]
fn guide_repeats_byte_for_byte() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("human");
    let (a, b) = (
        log.run(&env, &["robot-docs", "guide"]),
        log.run(&env, &["robot-docs", "guide"]),
    );
    same_guide(&log, "a human repeat", a.stdout(), &b);
    log.step("JSON");
    let (a, b) = (
        log.run(&env, &["robot-docs", "guide", "--json"]),
        log.run(&env, &["robot-docs", "guide", "--json"]),
    );
    a.ok();
    b.ok();
    log.assert_eq(
        "the bytes, meta removed",
        &{
            let (first, second) = (without_meta(&a).to_string(), without_meta(&b).to_string());
            first == second
        },
        &true,
    );
}

#[test]
fn guide_help_paths() {
    let log = tlog!();
    let env = TestEnv::new();
    log.step("help robot-docs");
    let help = log.run(&env, &["help", "robot-docs"]);
    help.ok();
    log.assert_eq("it lists guide", &help.stdout().contains("guide"), &true);
    log.step("help robot-docs guide");
    let help = log.run(&env, &["help", "robot-docs", "guide", "--json"]);
    help.ok();
    log.assert_eq("command", &help.envelope()["command"], &json!("help"));
    log.step("the help flags");
    for argv in [
        &["robot-docs", "guide", "--help"][..],
        &["robot-docs", "guide", "-h"],
    ] {
        log.run(&env, argv).ok();
    }
    let parent = log.run(&env, &["robot-docs", "--help"]);
    parent.ok();
    log.assert_eq(
        "robot-docs --help lists guide",
        &parent.stdout().contains("guide"),
        &true,
    );
}

#[test]
fn guide_usage_fix_runs() {
    let log = tlog!();
    let env = TestEnv::new();
    for (argv, fix) in [
        (&["robot-docs", "guide", "extra", "--json"][..], None),
        (
            &["robot-docs", "gide", "--json"],
            Some(json!(["omaddons", "robot-docs", "guide"])),
        ),
    ] {
        log.step(&argv.join(" "));
        let ran = log.run(&env, argv);
        ran.fails(2, "usage");
        let fix_argv = ran.envelope()["error"]["fix_argv"].clone();
        if let Some(fix) = fix {
            log.assert_eq("fix_argv", &fix_argv, &fix);
        }
        let Some(words) = fix_argv.as_array() else {
            continue;
        };
        let words: Vec<&str> = words.iter().skip(1).filter_map(Value::as_str).collect();
        log.run(&env, &words).ok();
    }
}

/// `robot-docs install-skill` into a throwaway `HOME` that has used Claude Code (M4-D64):
/// a dry run changes no file anywhere; the install writes the committed skill byte for
/// byte, mode 0644, records a `skill` line naming its folder, which the history
/// projection keeps and bare `undo` steps over; a second run changes nothing; a pristine
/// older guide is replaced without a copy, and an edited file is kept as `SKILL.md.bak`,
/// a later edit as `SKILL.md.bak.1` (never over the first), the same bytes again not at
/// all, and the doctor's skill check reads `SKILL.md` alone (omad-syc2).
#[test]
fn install_skill_round_trip_on_a_throwaway_home() {
    use std::os::unix::fs::PermissionsExt;
    let log = tlog!();
    let env = TestEnv::new();
    let home = env.home();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let committed =
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/omaddons/SKILL.md"))
            .unwrap();

    log.step("a dry run writes nothing");
    log.tree("root", env.root());
    let ran = log.run(
        &env,
        &["robot-docs", "install-skill", "--dry-run", "--json"],
    );
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("fresh"));
    log.assert_unchanged("root", env.root());

    log.step("the install");
    let ran = log.run(&env, &["robot-docs", "install-skill", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("fresh"));
    let dir = std::fs::canonicalize(home.join(".claude/skills/omaddons")).unwrap();
    let file = dir.join("SKILL.md");
    assert!(
        std::fs::read(&file).unwrap() == committed,
        "the installed skill is the committed one, byte for byte"
    );
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    log.assert_eq("mode", &mode, &0o644);
    log.assert_eq("bytes", &ran.data()["bytes"], &json!(committed.len()));

    log.step("its history line and the projection");
    let ran = log.run(&env, &["history", "--json"]);
    ran.ok();
    let entry = &ran.data()["entries"][0];
    log.json("entry", entry);
    assert_eq!(entry["kind"], "skill", "{entry}");
    assert_eq!(entry["target"], Value::Null, "{entry}");
    assert_eq!(entry["dir"], json!(dir.display().to_string()), "{entry}");
    let tail: Value = serde_json::from_slice(
        &std::fs::read(env.cache().join("omaddons/history_tail.json")).unwrap(),
    )
    .unwrap();
    log.assert_eq(
        "skill_dirs",
        &tail["skill_dirs"],
        &json!([dir.display().to_string()]),
    );
    let ran = log.run(&env, &["undo", "--json"]);
    ran.fails(3, "not_found");

    log.step("a second run changes nothing");
    log.tree("root", env.root());
    let ran = log.run(&env, &["robot-docs", "install-skill", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("unchanged"));
    log.assert_unchanged("root", env.root());

    log.step("a pristine older guide is replaced without a copy");
    let older = {
        use omaddons::contract::guide::stamp::{digest_without_stamp, hex, line};
        let zeros = "0".repeat(64);
        let provisional = format!(
            "---\nname: omaddons\n---\n{}\nan older guide\n",
            line(&zeros)
        );
        let digest = digest_without_stamp(&provisional).unwrap();
        provisional.replacen(&zeros, &hex(&digest), 1)
    };
    std::fs::write(&file, &older).unwrap();
    let ran = log.run(&env, &["robot-docs", "install-skill", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("replaced"));
    assert!(
        std::fs::read(&file).unwrap() == committed,
        "the guide again"
    );
    let copy = dir.join("SKILL.md.bak");
    assert!(!copy.exists(), "an older guide's own bytes are not kept");

    log.step("an edited copy is kept as SKILL.md.bak, byte for byte");
    let edited = b"my own notes on omaddons\n";
    std::fs::write(&file, edited).unwrap();
    let ran = log.run(&env, &["robot-docs", "install-skill", "--json"]);
    ran.ok();
    log.assert_eq("written", &ran.data()["written"], &json!("replaced"));
    assert!(
        std::fs::read(&file).unwrap() == committed,
        "the guide again"
    );
    log.assert_eq("kept", &std::fs::read(&copy).unwrap(), &edited.to_vec());
    let ran = log.run(&env, &["history", "--json"]);
    let skill_lines = ran.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "skill")
        .count();
    log.assert_eq("skill lines", &skill_lines, &3);

    // omad-syc2: a second edit never replaces the first one's backup.
    log.step("a second edit goes to SKILL.md.bak.1; the first stays in SKILL.md.bak");
    let second = b"my second notes\n";
    std::fs::write(&file, second).unwrap();
    let ran = log.run(&env, &["robot-docs", "install-skill"]);
    ran.ok();
    let numbered = dir.join("SKILL.md.bak.1");
    assert!(
        ran.stdout().contains(&format!(
            "your edited copy is kept as {}",
            numbered.display()
        )),
        "{}",
        ran.stdout()
    );
    log.assert_eq(
        "(SKILL.md.bak, SKILL.md.bak.1)",
        &(
            std::fs::read(&copy).unwrap(),
            std::fs::read(&numbered).unwrap(),
        ),
        &(edited.to_vec(), second.to_vec()),
    );

    log.step("the same edit again is kept already: no new copy");
    std::fs::write(&file, second).unwrap();
    let ran = log.run(&env, &["robot-docs", "install-skill"]);
    ran.ok();
    assert!(
        ran.stdout().contains(&format!(
            "your edited copy is already kept as {}",
            numbered.display()
        )),
        "{}",
        ran.stdout()
    );
    assert!(!dir.join("SKILL.md.bak.2").exists(), "no duplicate");
    assert!(
        std::fs::read(&file).unwrap() == committed,
        "the guide again"
    );

    log.step("the doctor judges SKILL.md alone, the backups beside it aside");
    let ran = log.run(
        &env,
        &[
            "doctor",
            "--only",
            "fm-plugins-skill-version-drift",
            "--json",
        ],
    );
    ran.ok();
    log.assert_eq("findings", &ran.envelope()["findings"], &json!([]));
}
