//! e2e suite: the doctor runtime: run directories, the doctor envelope and exit dictionary, budgets, isolation, --since and explain
#![cfg(test)]

mod common;
#[path = "doctor/mod.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::tlog::{Ran, TestLog, tlog};
use fixture::fixtures::fm_state_files_check_incomplete::CheckIncomplete;
use omaddons::doctor::registry::{FAILURE_MODES, Tier, ids};

/// The keys a new run changes besides `meta` (`contract::VOLATILE`'s doctor rows, with
/// `summary.duration_ms`).
const VOLATILE: [&str; 4] = ["run_id", "run_dir", "started_at", "finished_at"];

/// A home with retail and the classic client, logged.
fn home(log: &TestLog) -> Home {
    home_of(log, &["wow", "wow_classic"])
}

fn home_of(log: &TestLog, products: &[&str]) -> Home {
    let home = Home::new(products);
    log.env(&home);
    home
}

/// `omaddons <args>` (pass `--json`), given this home's own Omarchy; a doctor envelope is
/// checked against the doctor's envelope contract.
fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    run_with(log, home, args, &[])
}

fn run_with(log: &TestLog, home: &Home, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let mut cmd = log
        .cmd(home, args)
        .env("OMARCHY_PATH", plant::omarchy_path(home));
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    let ran = cmd.run();
    let json = args.contains(&"--json");
    if args.first() == Some(&"doctor") && json && ran.run.code >= 0 && !ran.stdout().is_empty() {
        check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
    }
    ran
}

/// `envelope` without `meta` and the keys a new run changes.
fn stable(envelope: &Value) -> Value {
    let mut stable = envelope.clone();
    if let Some(map) = stable.as_object_mut() {
        map.remove("meta");
        for key in VOLATILE {
            map.remove(key);
        }
        if let Some(summary) = map.get_mut("summary").and_then(Value::as_object_mut) {
            summary.remove("duration_ms");
        }
    }
    stable
}

/// The findings of `id` in `envelope`.
fn findings<'a>(envelope: &'a Value, id: &str) -> Vec<&'a Value> {
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"] == id)
        .collect()
}

/// The finding ids of `envelope`, deduplicated in order.
fn ids_of(envelope: &Value) -> Vec<String> {
    let mut ids: Vec<String> = envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect();
    ids.dedup();
    ids
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn run_dir(home: &Home, run_id: &str) -> PathBuf {
    home.state().join("doctor/runs").join(run_id)
}

/// The trees a diagnose must leave as they were: `<state>` (but `doctor/`), the config,
/// the cache and each client directory.
fn record_trees(log: &TestLog, home: &Home, clients: &[&str]) {
    log.tree("state", &home.state());
    log.tree("config", &home.env.config());
    log.tree("cache", &home.env.cache());
    for client in clients {
        log.tree(client, &home.client(client));
    }
}

/// Nothing but `<state>/doctor/` changed since [`record_trees`].
fn assert_trees(log: &TestLog, home: &Home, clients: &[&str]) {
    log.step("check trees");
    let diff = log.diff("state", &home.state());
    let outside: Vec<&String> = diff
        .added
        .iter()
        .chain(&diff.removed)
        .chain(diff.changed.iter().map(|(path, _)| path))
        .filter(|path| !path.starts_with("doctor"))
        .collect();
    assert!(outside.is_empty(), "changed outside doctor/: {outside:?}");
    log.assert_unchanged("config", &home.env.config());
    log.assert_unchanged("cache", &home.env.cache());
    for client in clients {
        log.assert_unchanged(client, &home.client(client));
    }
}

/// Runs every `doctor explain` the report offers: each exits 0 and holds the instances of
/// its failure mode the report found.
fn explain_each(log: &TestLog, home: &Home, envelope: &Value) {
    log.step("explain each command");
    let commands = envelope["commands"].as_array().unwrap();
    assert_eq!(
        commands.len(),
        ids_of(envelope).len(),
        "one per failure mode"
    );
    for command in commands {
        assert_eq!(command["action"], "doctor_explain");
        let argv: Vec<&str> = command["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        let mut args = argv[1..].to_vec();
        args.push("--json");
        let explained = run(log, home, &args);
        explained.ok();
        let fm = argv[3];
        let instances: Vec<&Value> = explained.envelope()["instances"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| &f["instance"])
            .collect();
        for finding in findings(envelope, fm) {
            assert!(
                instances.contains(&&finding["instance"]),
                "{fm}: {} not explained in {instances:?}",
                finding["instance"]
            );
        }
    }
}

#[test]
fn a_clean_home_is_healthy_and_leaves_one_run() {
    let log = tlog!();
    let home = home(&log);
    log.step("home");
    plant::install(&home, "foo", "retail");
    let clients = ["_retail_", "_classic_"];
    record_trees(&log, &home, &clients);

    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json", "--progress", "json"]);
    ran.ok();
    let envelope = ran.envelope();
    for (key, want) in [
        ("command", json!("doctor diagnose")),
        ("state", json!("DONE_OK")),
        ("view", json!("full")),
        ("doctor_contract_version", json!("1.0")),
        ("findings", json!([])),
        ("commands", json!([])),
        ("next_steps", json!([])),
    ] {
        assert_eq!(envelope[key], want, "{key}");
    }
    assert_eq!(envelope["summary"]["total_findings"], 0);

    let run_id = envelope["run_id"].as_str().unwrap();
    assert_eq!(plant::runs(&home), [run_id]);
    let dir = run_dir(&home, run_id);
    assert_eq!(envelope["run_dir"], dir.display().to_string());
    let mut listing: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    listing.sort();
    let modes = [
        mode(&dir),
        mode(&dir.join("report.json")),
        mode(&dir.join("runtime.jsonl")),
    ];
    log.json(
        "run directory",
        &json!({"listing": listing, "modes": modes}),
    );
    assert_eq!(listing, ["report.json", "runtime.jsonl"]);
    assert_eq!(modes, [0o700, 0o400, 0o400]);
    assert_eq!(
        fs::read_link(home.state().join("doctor/latest")).unwrap(),
        PathBuf::from(format!("runs/{run_id}"))
    );

    // The record is the envelope printed, `kind` after `command` and no `meta`.
    let text = fs::read_to_string(dir.join("report.json")).unwrap();
    let mut want = envelope.clone();
    want.as_object_mut().unwrap().remove("meta");
    want["kind"] = json!("diagnose");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), want);
    let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap();
    assert!(
        at("command") < at("kind") && at("kind") < at("exit_code"),
        "{text}"
    );

    // One line per write, in order, all under `<state>/doctor/`.
    let lines: Vec<Value> = fs::read_to_string(dir.join("runtime.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    log.json("runtime.jsonl", &lines);
    let doctor = home.state().join("doctor").display().to_string();
    for (at, line) in lines.iter().enumerate() {
        assert_eq!(line["seq"], at + 1, "{line}");
        assert!(
            line["path"].as_str().unwrap().starts_with(&doctor),
            "{line}"
        );
    }
    // The stage made (`create_dir`…) and hardened, published, then the record written,
    // `latest` moved and both files made read-only.
    let ops: Vec<&str> = lines.iter().map(|l| l["op"].as_str().unwrap()).collect();
    let made = ops.iter().take_while(|op| **op == "create_dir").count();
    assert!(made >= 1, "{ops:?}");
    assert_eq!(
        ops[made..],
        [
            "chmod",
            "rename_noreplace",
            "write_file",
            "symlink_atomic",
            "chmod",
            "chmod"
        ],
        "{ops:?}"
    );
    // `<state>` outside `doctor/` unchanged covers `net.log`: no request.
    assert_trees(&log, &home, &clients);
}

#[test]
fn bare_doctor_is_doctor_diagnose() {
    let log = tlog!();
    let home = home(&log);
    log.step("diagnose");
    let bare = run(&log, &home, &["doctor", "--json"]);
    let diagnose = run(&log, &home, &["doctor", "diagnose", "--json"]);
    bare.ok();
    diagnose.ok();
    assert_eq!(bare.envelope()["command"], "doctor diagnose");
    assert_eq!(stable(bare.envelope()), stable(diagnose.envelope()));
}

/// One published failure mode: its id, its severity, and how it is planted.
type Published = (&'static str, &'static str, fn(&Home));

/// The seven failure modes `status` publishes, each planted on retail (target-vanished on
/// the classic client).
const PUBLISHED: [Published; 7] = [
    (ids::JOURNAL_PENDING, "P0", |h| {
        plant::journal_pending(h, "retail")
    }),
    (ids::STATE_VERSION_NEWER, "P0", |h| {
        plant::state_version_newer(h, "retail")
    }),
    (ids::RECORD_FOLDER_MISSING, "P1", |h| {
        plant::record_folder_missing(h, "retail", "foo");
    }),
    (ids::MANAGED_FILES_MODIFIED, "P2", |h| {
        plant::managed_files_modified(h, "retail", "foo");
    }),
    (ids::OWNERSHIP_TRANSFERRED, "P2", |h| {
        plant::ownership_transferred(h, "retail")
    }),
    (ids::TARGET_VANISHED, "P2", |h| {
        plant::target_vanished(h, "classic");
    }),
    (ids::WIDGET_VERSION_DRIFT, "P3", plant::widget_version_drift),
];

/// A diagnose of `home` exits 1 with each of `expected` at its severity, each instance
/// keyed by its id; every `doctor explain` it offers runs.
fn diagnose_finds(log: &TestLog, home: &Home, expected: &[(&str, &str)]) -> Value {
    log.step("diagnose");
    let ran = run(log, home, &["doctor", "--json"]);
    ran.exit(1);
    let envelope = ran.envelope().clone();
    assert_eq!(envelope["state"], "DONE_FINDINGS");
    for (id, severity) in expected {
        let found = findings(&envelope, id);
        assert!(!found.is_empty(), "{id} in {:?}", ids_of(&envelope));
        for finding in found {
            assert_eq!(finding["severity"], *severity, "{id}");
            let instance = finding["instance"].as_str().unwrap();
            assert!(instance.starts_with(&format!("{id}@")), "{instance}");
        }
    }
    explain_each(log, home, &envelope);
    envelope
}

#[test]
fn the_seven_published_findings_and_their_explain_commands() {
    let log = tlog!();
    for (id, severity, planting) in PUBLISHED {
        let log = log.case(id);
        let home = home(&log);
        log.step("plant");
        planting(&home);
        diagnose_finds(&log, &home, &[(id, severity)]);
    }

    // All seven on one home: the addon states on retail, the newer record on the classic
    // client, the era client gone, the widget, and the journal last.
    let log = log.case("all-seven");
    let home = home_of(&log, &["wow", "wow_classic", "wow_classic_era"]);
    log.step("plant");
    plant::managed_files_modified(&home, "retail", "edited");
    plant::record_folder_missing(&home, "retail", "missing");
    plant::ownership_transferred(&home, "retail");
    plant::state_version_newer(&home, "classic");
    plant::target_vanished(&home, "era");
    plant::widget_version_drift(&home);
    plant::journal_pending(&home, "retail");
    let expected: Vec<(&str, &str)> = PUBLISHED.iter().map(|(id, sev, _)| (*id, *sev)).collect();
    diagnose_finds(&log, &home, &expected);
}

/// `argv` as owned words.
fn words(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap().to_owned())
        .collect()
}

/// Runs `argv` (with `omaddons` first) with `--json`.
fn run_argv(log: &TestLog, home: &Home, argv: &[String]) -> Ran {
    let mut args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    args.push("--json");
    run(log, home, &args)
}

#[test]
fn a_panicking_detector_becomes_a_finding_and_the_run_completes() {
    let log = tlog!();
    // Two rows in registry order: A finds the planted edit, B finds nothing here.
    let (a, b) = (ids::MANAGED_FILES_MODIFIED, ids::OWNERSHIP_TRANSFERRED);
    let at = |id| FAILURE_MODES.iter().position(|row| row.id == id).unwrap();
    assert!(at(a) < at(b), "A runs before B");
    let only = format!("{a},{b}");
    for (hit, crashed, survivor) in [("@2", b, Some(a)), ("@1", a, None)] {
        let log = log.case(hit);
        let home = home(&log);
        log.step("plant");
        plant::managed_files_modified(&home, "retail", "foo");
        log.step("diagnose");
        let spec = format!("doctor.detector.panic=panic{hit}");
        let ran = run_with(
            &log,
            &home,
            &["doctor", "--json", "--only", &only],
            &[("OMADDONS_FAILPOINTS", &spec)],
        );
        ran.exit(1);
        let envelope = ran.envelope();
        let crash = findings(envelope, ids::DETECTOR_CRASHED);
        assert_eq!(crash.len(), 1, "{:?}", ids_of(envelope));
        assert_eq!(crash[0]["severity"], "P1");
        assert_eq!(crash[0]["evidence"]["detector"], crashed);
        let location = crash[0]["evidence"]["location"].as_str().unwrap();
        assert!(location.starts_with("src/"), "{location}");
        assert_eq!(
            !findings(envelope, a).is_empty(),
            survivor.is_some(),
            "A's findings"
        );
        let run_id = envelope["run_id"].as_str().unwrap();
        let dir = run_dir(&home, run_id);
        assert!(dir.join("report.json").is_file());
        assert_eq!(plant::latest(&home).as_deref(), Some(run_id));
        for text in [
            ran.stdout().to_owned(),
            ran.stderr().to_owned(),
            fs::read_to_string(dir.join("report.json")).unwrap(),
            fs::read_to_string(dir.join("runtime.jsonl")).unwrap(),
        ] {
            assert!(!text.contains("failpoint doctor.detector.panic"), "{text}");
        }
    }
}

#[test]
fn a_detector_past_its_deadline_is_incomplete_never_healthy() {
    let log = tlog!();
    let home = home(&log);
    let row = CheckIncomplete::row();
    let budget = CheckIncomplete::budget_ms();
    let only = ["--only", row.id, "--budget"];
    let budget_arg = format!("{budget}ms");
    let sleep = [("OMADDONS_FAILPOINTS", "doctor.detector.panic=sleep:50")];
    for (case, severity) in [("alone", None), ("with_severity", Some("P2"))] {
        let log = log.case(case);
        log.step("diagnose");
        let mut args = vec!["doctor", "--json"];
        args.extend(only);
        args.push(&budget_arg);
        args.extend(severity.map(|s| ["--severity", s]).into_iter().flatten());
        let ran = run_with(&log, &home, &args, &sleep);
        ran.exit(1);
        let envelope = ran.envelope();
        assert_eq!(envelope["state"], "DONE_FINDINGS");
        assert_eq!(envelope["summary"]["incomplete"], 1);
        let cut = findings(envelope, ids::CHECK_INCOMPLETE);
        assert_eq!(cut.len(), 1, "{:?}", ids_of(envelope));
        let evidence = &cut[0]["evidence"];
        assert_eq!(
            (
                &evidence["detector"],
                &evidence["reason"],
                &evidence["deadline_ms"]
            ),
            (&json!(row.id), &json!("deadline"), &json!(budget))
        );
        assert!(evidence.get("max_entries").is_none(), "{evidence}");
        let fix = words(&cut[0]["remediation"]["argv"]);
        let ten = format!("{}ms", budget * 10);
        assert_eq!(
            fix,
            ["omaddons", "doctor", "--only", row.id, "--budget", &ten]
        );
        if severity.is_none() {
            run_argv(&log, &home, &fix).ok();
        }
    }
}

/// The declared cost of a plain diagnose's selection: every row of the default tiers.
fn default_cost() -> u32 {
    FAILURE_MODES
        .iter()
        .filter(|row| matches!(row.tier, Tier::Cached | Tier::Quick | Tier::Default))
        .filter(|row| !row.online)
        .map(|row| row.cost_ms)
        .sum()
}

#[test]
fn an_over_budget_selection_refuses_before_starting() {
    let log = tlog!();
    let home = home(&log);
    let cost = default_cost();
    assert!(
        cost >= 2,
        "a meaningful case needs a cost of at least 2 ms: {cost}"
    );
    log.step("diagnose");
    let budget = format!("{}ms", cost - 1);
    let ran = run(&log, &home, &["doctor", "--json", "--budget", &budget]);
    ran.fails(4, "doctor_refused");
    let error = &ran.envelope()["error"];
    assert_eq!(
        error["details"],
        json!({"reason": "over_budget", "budget_ms": cost - 1, "estimated_ms": cost})
    );
    assert!(!home.state().join("doctor").exists(), "nothing created");
    let fixed = run_argv(&log, &home, &words(&error["fix_argv"]));
    assert!(matches!(fixed.run.code, 0 | 1), "{}", fixed.run.code);
}

#[test]
fn a_hundred_concurrent_runs_get_a_hundred_run_ids_under_a_frozen_clock() {
    let log = tlog!();
    let home = home(&log);
    let now = "2026-10-30T18:02:11Z";
    log.step("diagnose");
    let started = std::time::Instant::now();
    let running: Vec<_> = (0..100)
        .map(|_| {
            log.cmd(&home, &["doctor", "--json"])
                .env("OMARCHY_PATH", plant::omarchy_path(&home))
                .env("OMADDONS_NOW", now)
                .spawn()
        })
        .collect();
    let ran: Vec<Ran> = running.into_iter().map(|r| r.wait()).collect();
    let elapsed = started.elapsed();
    log.note(format!("100 runs in {} ms", elapsed.as_millis()));
    let mut run_ids = std::collections::BTreeSet::new();
    for (n, ran) in ran.iter().enumerate() {
        ran.ok();
        let envelope = ran.envelope();
        let run_id = envelope["run_id"].as_str().unwrap().to_owned();
        log.case(&format!("run-{n}")).note(&run_id);
        assert!(run_id.starts_with("2026-10-30T18-02-11Z__"), "{run_id}");
        assert_eq!(envelope["started_at"], now, "{run_id}");
        assert!(run_dir(&home, &run_id).join("report.json").is_file());
        run_ids.insert(run_id);
    }
    assert_eq!(run_ids.len(), 100, "distinct run ids");
    assert_eq!(plant::runs(&home).len(), 100);
    let latest = plant::latest(&home).unwrap();
    assert!(run_ids.contains(&latest), "{latest}");
    assert!(elapsed.as_secs() < 60, "{elapsed:?}");
}

#[test]
fn since_reports_new_resolved_and_unchanged_instances() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::managed_files_modified(&home, "retail", "foo");
    plant::managed_files_modified(&home, "retail", "bar");
    log.step("diagnose");
    let first = run(&log, &home, &["doctor", "--json"]);
    first.exit(1);
    let a = first.envelope()["run_id"].as_str().unwrap().to_owned();
    let found = findings(first.envelope(), ids::MANAGED_FILES_MODIFIED);
    assert_eq!(found.len(), 2);
    let foo = found
        .iter()
        .map(|f| f["instance"].clone())
        .find(|key| key.as_str().is_some_and(|key| key.ends_with(":addon=foo")))
        .unwrap();

    log.step("restore one");
    plant::restore_file(&home, "retail", "foo");
    log.step("since");
    let ran = run(&log, &home, &["doctor", "--json", "--since", &a]);
    ran.exit(1);
    assert_eq!(
        ran.envelope()["since"],
        json!({"run": a, "new_instances": [], "resolved_instances": [foo], "unchanged": 1})
    );
    let before = plant::latest(&home).unwrap();
    let ran = run(&log, &home, &["doctor", "--json", "--since", "latest"]);
    assert_eq!(ran.envelope()["since"]["run"], before.as_str());

    let unknown = "2026-01-01T00-00-00Z__01M8R000000000000000000000";
    let ran = run(&log, &home, &["doctor", "--json", "--since", unknown]);
    ran.fails(66, "not_found");
    let mut newest = plant::runs(&home);
    newest.reverse();
    let error = &ran.envelope()["error"];
    assert_eq!(
        words(&error["fix_argv"]),
        ["omaddons", "doctor", "--since", &newest[0]]
    );
    let alternatives: Vec<Vec<String>> = error["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| words(&a["argv"]))
        .collect();
    assert_eq!(
        alternatives[0],
        ["omaddons", "doctor", "--since", &newest[1]]
    );

    log.step("crash");
    let crashed = run_with(
        &log,
        &home,
        &["doctor", "--json"],
        &[("OMADDONS_FAILPOINTS", "doctor.run.published=abort")],
    );
    crashed.aborted();
    let interrupted = plant::runs(&home)
        .into_iter()
        .find(|run| !run_dir(&home, run).join("report.json").exists())
        .unwrap();
    let ran = run(&log, &home, &["doctor", "--json", "--since", &interrupted]);
    ran.fails(66, "not_found");
    let message = ran.envelope()["error"]["message"].as_str().unwrap();
    assert!(message.contains("interrupted"), "{message}");
}

#[test]
fn a_crash_while_staging_or_after_publishing_leaves_no_report() {
    let log = tlog!();
    {
        let log = log.case("staged");
        let home = home(&log);
        record_trees(&log, &home, &[]);
        log.step("crash");
        let crashed = run_with(
            &log,
            &home,
            &["doctor", "--json"],
            &[("OMADDONS_FAILPOINTS", "doctor.run.staged=abort")],
        );
        crashed.aborted();
        let runs = home.state().join("doctor/runs");
        let left: Vec<String> = fs::read_dir(&runs)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        log.json("runs/ after the crash", &left);
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(
            left[0].starts_with('.') && left[0].ends_with(".tmp"),
            "{left:?}"
        );
        assert_eq!(mode(&runs.join(&left[0])), 0o700);
        assert!(plant::runs(&home).is_empty());
        assert_eq!(plant::latest(&home), None);
        assert_trees(&log, &home, &[]);
        log.step("recover");
        run(&log, &home, &["status", "--json"]).ok();
        let ran = run(&log, &home, &["doctor", "--json"]);
        ran.ok();
        assert_eq!(plant::runs(&home).len(), 1, "one published run");
        assert!(runs.join(&left[0]).exists(), "the stage is left as it was");
    }
    {
        let log = log.case("published");
        let home = home(&log);
        log.step("crash");
        let crashed = run_with(
            &log,
            &home,
            &["doctor", "--json"],
            &[("OMADDONS_FAILPOINTS", "doctor.run.published=abort")],
        );
        crashed.aborted();
        let runs = plant::runs(&home);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let dir = run_dir(&home, &runs[0]);
        assert!(dir.join("runtime.jsonl").is_file() && !dir.join("report.json").exists());
        assert_eq!(
            plant::latest(&home),
            None,
            "latest never moves to a crashed run"
        );
        log.step("recover");
        run(&log, &home, &["doctor", "--json"]).ok();
        assert_eq!(plant::runs(&home).len(), 2);
    }
}

#[test]
fn an_unwritable_runs_directory_exits_73() {
    let log = tlog!();
    if common::procfs::running_as_root() {
        log.note("skipped: root writes to a 0500 directory");
        return;
    }
    let home = home(&log);
    let runs = home.state().join("doctor/runs");
    fs::create_dir_all(&runs).unwrap();
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o500)).unwrap();
    record_trees(&log, &home, &[]);
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o700)).unwrap();
    ran.fails(73, "io");
    let envelope = ran.envelope();
    let path = envelope["error"]["details"]["path"].as_str().unwrap();
    assert!(
        path.starts_with(&home.state().join("doctor").display().to_string()),
        "{path}"
    );
    for key in ["run_id", "findings", "state"] {
        assert!(envelope.get(key).is_none(), "no report: {key}");
    }
    assert_trees(&log, &home, &[]);
}

#[test]
fn a_panic_in_the_doctor_tree_exits_74() {
    let log = tlog!();
    let home = home(&log);
    for (case, spec) in [("error", "cli.panic=error"), ("panic", "cli.panic=panic")] {
        let log = log.case(case);
        log.step("diagnose");
        let ran = run_with(
            &log,
            &home,
            &["doctor", "--json"],
            &[("OMADDONS_FAILPOINTS", spec)],
        );
        ran.fails(74, "internal");
        assert_eq!(ran.envelope()["exit_code"], 74);
    }
    // The general dictionary is unchanged.
    let ran = run_with(
        &log,
        &home,
        &["status", "--json"],
        &[("OMADDONS_FAILPOINTS", "cli.panic=panic")],
    );
    ran.fails(1, "internal");
}

/// How an exit-site row prepares its home; returns values its argv names (`{run}`).
type Setup = fn(&TestLog, &Home) -> Option<String>;

/// One exit site: its name, the argv, its setup, the run's environment, then the exit and
/// the error code (`None` for a report).
type ExitSite<'a> = (
    &'a str,
    Vec<&'a str>,
    Setup,
    &'a [(&'a str, &'a str)],
    i32,
    Option<&'a str>,
);

fn nothing(_: &TestLog, _: &Home) -> Option<String> {
    None
}

fn edited(_: &TestLog, home: &Home) -> Option<String> {
    plant::managed_files_modified(home, "retail", "foo");
    None
}

/// A finished run whose `report.json` is then replaced by `bytes`.
fn rewritten_report(log: &TestLog, home: &Home, bytes: &[u8]) -> Option<String> {
    let ran = run(log, home, &["doctor", "--json"]);
    let run_id = ran.envelope()["run_id"].as_str().unwrap().to_owned();
    let report = run_dir(home, &run_id).join("report.json");
    fs::set_permissions(&report, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&report, bytes).unwrap();
    Some(run_id)
}

fn newer_report(log: &TestLog, home: &Home) -> Option<String> {
    rewritten_report(log, home, br#"{"schema_version":"2.0","findings":[]}"#)
}

fn corrupt_report(log: &TestLog, home: &Home) -> Option<String> {
    rewritten_report(log, home, b"{")
}

fn unwritable_runs(_: &TestLog, home: &Home) -> Option<String> {
    let runs = home.state().join("doctor/runs");
    fs::create_dir_all(&runs).unwrap();
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o500)).unwrap();
    None
}

#[test]
fn every_doctor_exit_site_uses_the_doctor_dictionary() {
    let log = tlog!();
    let budget = format!("{}ms", default_cost() - 1);
    let if_match = format!("pc1:{}", "0".repeat(64));
    let cheapest = CheckIncomplete::row().id;
    let unknown_run = "2026-01-01T00-00-00Z__01M8R000000000000000000000";
    let rows: Vec<ExitSite> = vec![
        ("clean", vec!["doctor"], nothing, &[], 0, None),
        ("findings", vec!["doctor"], edited, &[], 1, None),
        (
            "over_budget",
            vec!["doctor", "--budget", &budget],
            nothing,
            &[],
            4,
            Some("doctor_refused"),
        ),
        (
            "newer_report",
            vec!["doctor", "--since", "{run}"],
            newer_report,
            &[],
            4,
            Some("state_too_new"),
        ),
        (
            "clap_flag",
            vec!["doctor", "--bogus"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "fix_hint",
            vec!["doctor", "--fix"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "unknown_id",
            vec!["doctor", "--only", "fm-nope"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "bad_actor",
            vec!["doctor", "--actor", "robot"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "if_match",
            vec!["doctor", "--if-match", &if_match],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "online_and_offline",
            vec!["doctor", "--online", "--offline"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "explain_unknown",
            vec!["doctor", "explain", "fm-nope"],
            nothing,
            &[],
            64,
            Some("usage"),
        ),
        (
            "unknown_target",
            vec!["doctor", "-t", "nosuch"],
            nothing,
            &[],
            66,
            Some("target_not_found"),
        ),
        (
            "unknown_run",
            vec!["doctor", "--since", unknown_run],
            nothing,
            &[],
            66,
            Some("not_found"),
        ),
        (
            "explain_unknown_target",
            vec!["doctor", "explain", ids::TARGET_VANISHED, "-t", "nosuch"],
            nothing,
            &[],
            66,
            Some("target_not_found"),
        ),
        (
            "cannot_create",
            vec!["doctor"],
            unwritable_runs,
            &[],
            73,
            Some("io"),
        ),
        (
            "failpoint_error",
            vec!["doctor"],
            nothing,
            &[("OMADDONS_FAILPOINTS", "cli.panic=error")],
            74,
            Some("internal"),
        ),
        (
            "panic",
            vec!["doctor"],
            nothing,
            &[("OMADDONS_FAILPOINTS", "cli.panic=panic")],
            74,
            Some("internal"),
        ),
        (
            "explain_panic",
            vec!["doctor", "explain", cheapest],
            nothing,
            &[("OMADDONS_FAILPOINTS", "doctor.detector.panic=panic")],
            74,
            Some("internal"),
        ),
        (
            "corrupt_report",
            vec!["doctor", "--since", "{run}"],
            corrupt_report,
            &[],
            74,
            Some("state_corrupt"),
        ),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for (name, argv, setup, env, exit, code) in rows {
        if name == "cannot_create" && common::procfs::running_as_root() {
            log.case(name)
                .note("skipped: root writes to a 0500 directory");
            continue;
        }
        let log = log.case(name);
        let home = home(&log);
        log.step("plant");
        let filled = setup(&log, &home);
        let mut args: Vec<String> = argv
            .iter()
            .map(|word| match (*word, &filled) {
                ("{run}", Some(run)) => run.clone(),
                _ => (*word).to_owned(),
            })
            .collect();
        args.push("--json".to_owned());
        log.step("diagnose");
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = run_with(&log, &home, &args, env);
        let runs = home.state().join("doctor/runs");
        if runs.exists() {
            fs::set_permissions(&runs, fs::Permissions::from_mode(0o700)).unwrap();
        }
        match code {
            Some(code) => ran.fails(exit, code),
            None => ran.exit(exit),
        };
        let command = ran.envelope()["command"].as_str().unwrap().to_owned();
        let declared = omaddons::app::command_entry(&command).unwrap().exits;
        assert!(
            declared.contains(&u8::try_from(exit).unwrap()),
            "{command}: {exit} declared"
        );
        seen.insert(exit);
    }
    log.json("exits seen", &seen);
}

#[test]
fn explain_answers_by_id_by_instance_and_by_target_and_writes_nothing() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::managed_files_modified(&home, "retail", "foo");
    plant::managed_files_modified(&home, "retail", "bar");
    plant::managed_files_modified(&home, "classic", "baz");
    log.tree("state-all", &home.state());
    log.step("explain");
    let fm = ids::MANAGED_FILES_MODIFIED;
    let explain = |args: &[&str]| {
        let mut argv = vec!["doctor", "explain"];
        argv.extend(args);
        argv.push("--json");
        run(&log, &home, &argv)
    };
    let instances = |ran: &Ran| -> Vec<String> {
        ran.envelope()["instances"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["instance"].as_str().unwrap().to_owned())
            .collect()
    };
    let all = explain(&[fm]);
    all.ok();
    let every = instances(&all);
    assert_eq!(every.len(), 3, "{every:?}");
    assert_eq!(instances(explain(&[fm, "-t", "retail"]).ok()).len(), 2);
    let one = &every[0];
    assert_eq!(
        instances(explain(&[one]).ok()),
        std::slice::from_ref(one),
        "by its key"
    );
    let foo = every
        .iter()
        .find(|key| key.ends_with(":addon=foo"))
        .unwrap()
        .clone();
    plant::restore_file(&home, "retail", "foo");
    log.tree("state-all", &home.state());
    assert!(
        instances(explain(&[&foo]).ok()).is_empty(),
        "a vanished instance"
    );
    let typo = explain(&["fm-external_artifacts-managed-files-modifed"]);
    typo.fails(64, "usage");
    assert_eq!(
        words(&typo.envelope()["error"]["fix_argv"]),
        ["omaddons", "doctor", "explain", fm]
    );
    let runtime = explain(&[ids::CHECK_INCOMPLETE]);
    runtime.ok();
    assert_eq!(runtime.envelope()["failure_mode"]["tier"], "runtime");
    assert_eq!(runtime.envelope()["instances"], json!([]));
    log.step("check trees");
    log.assert_unchanged("state-all", &home.state());
}

#[test]
fn a_diagnose_changes_nothing_outside_its_run_directory() {
    let log = tlog!();
    let home = home(&log);
    log.step("home");
    common::wtf::Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 64)
        .build();
    plant::managed_files_modified(&home, "retail", "foo");
    let clients = ["_retail_", "_classic_"];
    record_trees(&log, &home, &clients);
    log.step("diagnose");
    let json = run(&log, &home, &["doctor", "--json", "--progress", "json"]);
    json.exit(1).assert_private();
    let human = run(&log, &home, &["doctor", "-v", "--progress", "json"]);
    human.exit(1).assert_private();
    for run_id in plant::runs(&home) {
        let dir = run_dir(&home, &run_id);
        for file in ["report.json", "runtime.jsonl"] {
            common::privacy::assert_private(&fs::read_to_string(dir.join(file)).unwrap());
        }
    }
    assert_trees(&log, &home, &clients);
}

#[test]
fn two_diagnoses_of_one_state_give_identical_findings() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::managed_files_modified(&home, "retail", "foo");
    plant::target_vanished(&home, "classic");
    log.step("diagnose");
    let first = run(&log, &home, &["doctor", "--json"]);
    let second = run(&log, &home, &["doctor", "--json"]);
    first.exit(1);
    second.exit(1);
    assert_eq!(first.envelope()["findings"], second.envelope()["findings"]);
    assert_eq!(
        stable(first.envelope()).to_string(),
        stable(second.envelope()).to_string()
    );
}

/// The test fixers' rows exist only with `OMADDONS_TEST_FIXERS=1` (and in debug builds):
/// each fixture's subject is found with the switch, and never without it.
#[test]
fn test_fixer_fixtures_need_the_switch() {
    let log = tlog!();
    for fixture in fixture::fixtures::test_fixers() {
        let log = log.case(fixture.id());
        let home = home(&log);
        log.step("plant");
        fixture.corrupt(&home);
        assert!(fixture.fixer() && !fixture.subject(&home).is_empty());
        log.step("diagnose without the switch");
        let plain = run(&log, &home, &["doctor", "--json"]);
        assert!(
            !ids_of(plain.envelope())
                .iter()
                .any(|id| id.starts_with("fm-test-")),
            "{:?}",
            ids_of(plain.envelope())
        );
        log.step("diagnose with it");
        let env = fixture.run_env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let ran = run_with(&log, &home, &["doctor", "--json"], &env);
        ran.exit(1);
        assert_eq!(findings(ran.envelope(), fixture.id()).len(), 1);
    }
}

#[test]
fn runtime_fixtures_produce_their_findings() {
    let log = tlog!();
    for fixture in fixture::fixtures::all() {
        let log = log.case(fixture.id());
        let home = home(&log);
        log.step("plant");
        fixture.corrupt(&home);
        log.step("diagnose");
        let mut args = vec!["doctor".to_owned(), "--json".to_owned()];
        args.extend(fixture.run_args());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let env = fixture.run_env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let ran = run_with(&log, &home, &args, &env);
        ran.exit(1);
        let found = findings(ran.envelope(), fixture.id());
        assert_eq!(found.len(), 1, "{:?}", ids_of(ran.envelope()));
        let row = omaddons::doctor::registry::row(fixture.id()).unwrap();
        assert_eq!(found[0]["severity"], format!("{:?}", row.severity));
        assert!(found[0]["evidence"]["detector"].is_string());
        assert!(fixture.subject(&home).is_empty() && !fixture.fixer());
        log.step("check trees");
        fixture.assert_healthy(&home);
    }
}
