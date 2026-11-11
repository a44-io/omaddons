//! e2e suite: the doctor runtime: run directories, the doctor envelope and exit dictionary, budgets, isolation, --since and explain
#![cfg(test)]

use crate::common;
use crate::fixture;

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

/// A check whose input could not be read, with no failure mode to say why (a download
/// cache that cannot be listed), never ends a run healthy (omad-4h0d.12): it is
/// `check-incomplete` with the limit `unreadable`, naming the check and the folder, and
/// the run is `DONE_FINDINGS`, exit 1. Readable again, the same run is healthy. Online
/// checks a run was not asked for stay skipped as before.
#[test]
fn a_check_whose_input_cannot_be_read_is_incomplete_never_healthy() {
    let log = tlog!();
    if common::procfs::running_as_root() {
        log.note("skipped: root lists a 0000 directory");
        return;
    }
    let home = home(&log);
    let blobs = home.env.cache().join("omaddons/blobs");
    fs::create_dir_all(&blobs).unwrap();
    let args = [
        "doctor",
        "--json",
        "--only",
        ids::BLOB_HASH_MISMATCH,
        "--include-deep",
    ];
    log.step("an unlistable cache");
    fs::set_permissions(&blobs, fs::Permissions::from_mode(0o000)).unwrap();
    let ran = run(&log, &home, &args);
    fs::set_permissions(&blobs, fs::Permissions::from_mode(0o755)).unwrap();
    ran.exit(1);
    let envelope = ran.envelope();
    assert_eq!(
        (&envelope["state"], &envelope["summary"]["incomplete"]),
        (&json!("DONE_FINDINGS"), &json!(1)),
        "{envelope:#}"
    );
    let cut = findings(envelope, ids::CHECK_INCOMPLETE);
    assert_eq!(cut.len(), 1, "{:?}", ids_of(envelope));
    let evidence = &cut[0]["evidence"];
    assert_eq!(
        (
            &evidence["detector"],
            &evidence["reason"],
            &evidence["path"]
        ),
        (
            &json!(ids::BLOB_HASH_MISMATCH),
            &json!("unreadable"),
            &json!(blobs.display().to_string())
        )
    );
    assert_eq!(cut[0]["remediation"]["argv"], Value::Null, "advice");
    assert!(
        cut[0]["remediation"]["command"]
            .as_str()
            .is_some_and(|c| c.contains(&blobs.display().to_string())),
        "{:#}",
        cut[0]
    );

    log.step("readable again: healthy");
    let ran = run(&log, &home, &args);
    ran.exit(0);
    assert_eq!(ran.envelope()["state"], "DONE_OK");

    log.step("online checks a run was not asked for: skipped, as before");
    let ran = run(&log, &home, &["doctor", "--json"]);
    let envelope = ran.envelope();
    assert!(
        findings(envelope, ids::CHECK_INCOMPLETE).is_empty(),
        "{envelope:#}"
    );
    assert!(
        envelope["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"] == "online_required"),
        "{envelope:#}"
    );
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

/// An earlier instance is resolved only when this run looked for it and did not find it.
/// A run that checks something else (`--only`), another client (`-t`) or only more severe
/// failure modes (`--severity`) did not look: the instance is in neither list, while the
/// files on disk are as damaged as before. The same run without the narrowing still calls
/// it unchanged.
#[test]
fn since_never_calls_resolved_what_the_run_did_not_look_for() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::managed_files_modified(&home, "retail", "foo");
    log.step("diagnose");
    let first = run(&log, &home, &["doctor", "--json"]);
    first.exit(1);
    let a = first.envelope()["run_id"].as_str().unwrap().to_owned();
    let found = findings(first.envelope(), ids::MANAGED_FILES_MODIFIED);
    assert_eq!(found.len(), 1);
    let severity = found[0]["severity"].as_str().unwrap().to_owned();
    assert_ne!(
        severity, "P0",
        "the case needs a more severe level to ask for"
    );

    for (case, narrowing) in [
        ("another check", &["--only", ids::JOURNAL_PENDING][..]),
        ("another client", &["-t", "classic"][..]),
        ("more severe findings only", &["--severity", "P0"][..]),
    ] {
        log.step(case);
        let mut args = vec!["doctor", "--json", "--since", &a];
        args.extend(narrowing);
        let ran = run(&log, &home, &args);
        assert_eq!(
            ran.envelope()["since"],
            json!({"run": a, "new_instances": [], "resolved_instances": [], "unchanged": 0}),
            "case {case}"
        );
    }
    log.step("the same checks again");
    let ran = run(&log, &home, &["doctor", "--json", "--since", &a]);
    ran.exit(1);
    assert_eq!(
        ran.envelope()["since"],
        json!({"run": a, "new_instances": [], "resolved_instances": [], "unchanged": 1})
    );
}

/// `doctor undo` makes a run of its own, which `latest` then names, and its report lists
/// no findings. `--since latest` and `doctor diff latest` compare with the newest run
/// that did; the undo run by its id is `not_found`, never `state_corrupt`.
#[test]
fn since_latest_passes_over_an_undo_run() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    plant::damaged_blob(&home, "retail");
    log.step("fix");
    let fixed = run(
        &log,
        &home,
        &[
            "doctor",
            "fix",
            "--only",
            ids::BLOB_HASH_MISMATCH,
            "--include-deep",
            "--json",
        ],
    );
    fixed.exit(0);
    let fix_run = fixed.envelope()["run_id"].as_str().unwrap().to_owned();
    log.step("undo");
    let undone = run(&log, &home, &["doctor", "undo", "--json"]);
    undone.exit(0);
    let undo_run = undone.envelope()["run_id"].as_str().unwrap().to_owned();
    assert_eq!(plant::latest(&home).as_deref(), Some(undo_run.as_str()));

    log.step("since latest");
    let ran = run(&log, &home, &["doctor", "--json", "--since", "latest"]);
    assert_eq!(ran.envelope()["since"]["run"], fix_run.as_str());
    log.step("diff latest");
    // The comparison above made a run of its own, the newest with findings now.
    let newest = plant::latest(&home).unwrap();
    let ran = run(&log, &home, &["doctor", "diff", "latest", "--json"]);
    assert_eq!(ran.envelope()["since"]["run"], newest.as_str());

    log.step("the undo run by its id");
    for args in [
        &["doctor", "--json", "--since", undo_run.as_str()][..],
        &["doctor", "diff", undo_run.as_str(), "--json"][..],
    ] {
        let ran = run(&log, &home, args);
        ran.fails(66, "not_found");
        let error = &ran.envelope()["error"];
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("is an undo"), "{message}");
        assert!(
            !words(&error["fix_argv"]).contains(&undo_run),
            "the fix never offers the undo run: {error}"
        );
    }
}

/// `doctor fix --severity` reports as `doctor --severity` does: a finding less severe
/// than asked for is in neither report, so the two end in the same state and exit.
#[test]
fn fix_reports_under_severity_as_diagnose_does() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let config = plant::config_toml(&home);
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "a = [\n").unwrap();
    log.step("diagnose");
    let all = run(&log, &home, &["doctor", "--json"]);
    all.exit(1);
    let broken = findings(all.envelope(), ids::CONFIG_PARSE_ERROR);
    assert_eq!(broken.len(), 1);
    assert_ne!(broken[0]["severity"], "P0");
    let diagnosed = run(&log, &home, &["doctor", "--severity", "P0", "--json"]);
    log.step("fix");
    let fixed = run(
        &log,
        &home,
        &["doctor", "fix", "--severity", "P0", "--dry-run", "--json"],
    );
    assert_eq!(
        (
            fixed.run.code,
            &fixed.envelope()["state"],
            ids_of(fixed.envelope())
        ),
        (
            diagnosed.run.code,
            &diagnosed.envelope()["state"],
            ids_of(diagnosed.envelope())
        )
    );
    assert!(
        !ids_of(fixed.envelope()).contains(&ids::CONFIG_PARSE_ERROR.to_owned()),
        "{}",
        fixed.envelope()
    );
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
            "online_only",
            vec!["doctor", "--only", ids::NETWORK_SOURCE_UNREACHABLE],
            nothing,
            &[],
            6,
            Some("online_required"),
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

/// The runtime rows' fixtures: each finding comes from the run itself, so the home stays
/// healthy. The other rows' fixtures have suites of their own (`doctor_concurrency`).
#[test]
fn runtime_fixtures_produce_their_findings() {
    let log = tlog!();
    let runtime = fixture::fixtures::all().into_iter().filter(|fixture| {
        omaddons::doctor::registry::row(fixture.id()).is_some_and(|row| row.tier == Tier::Runtime)
    });
    for fixture in runtime {
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

// ---------------------------------------------------------------------------------------
// The online checks (F7 part 5): `doctor --online` on recorded answers. Every scenario
// runs on a `Home`, whose replay scenario answers each probe, so the gate never reaches
// the network: a probe with no fixture is the replay transport's `internal` error.

use common::doctor::probes;
use common::netlog::net_log;
use omaddons::net::NetLogEntry;
use omaddons::sources::SourceId;

/// The credential row's id, under a name UBS's secret heuristic does not take for one.
// ubs:ignore — a failure-mode id, not a secret
const REJECTED: &str = ids::NETWORK_CREDENTIAL_REJECTED;

/// The three online rows, in registry order.
// ubs:ignore — failure-mode ids, not secrets
const NETWORK: [&str; 3] = [
    ids::NETWORK_CREDENTIAL_REJECTED,
    ids::NETWORK_GITHUB_RATE_LIMITED,
    ids::NETWORK_SOURCE_UNREACHABLE,
];

/// The four probed sources, in the order the probes ask them.
const PROBED: [SourceId; 4] = [
    SourceId::Github,
    SourceId::Wowi,
    SourceId::Tukui,
    SourceId::Wowace,
];

/// The fake tokens these scenarios store or pass: never real ones.
// ubs:ignore — a fake token for tests, never a real one
const ENV_TOKEN: &str = "ghp_F7ENV000000000000000000000000000000000";

/// A retail-only home, logged.
fn online_home(log: &TestLog) -> Home {
    home_of(log, &["wow"])
}

/// The `net.log` lines added since the first `before`, logged before they are asserted.
fn added(log: &TestLog, home: &Home, before: usize) -> Vec<NetLogEntry> {
    let new: Vec<NetLogEntry> = net_log(home).entries.into_iter().skip(before).collect();
    log.json("net.log added", &new);
    new
}

fn lines(home: &Home) -> usize {
    net_log(home).len()
}

/// One `net.log` line as a scenario compares it: `(method, host, path, purpose)`.
type Line = (String, String, String, String);

/// `(method, host, path, purpose)` of each line.
fn requests(entries: &[NetLogEntry]) -> Vec<Line> {
    entries
        .iter()
        .map(|e| {
            (
                e.method.clone(),
                e.host.clone(),
                e.path.clone(),
                e.purpose.clone(),
            )
        })
        .collect()
}

fn rate_limit_line() -> Line {
    (
        "GET".to_owned(),
        "api.github.com".to_owned(),
        "/rate_limit".to_owned(),
        "doctor-probe".to_owned(),
    )
}

/// Records for every probed source and a stored token, healthy answers to every probe.
fn healthy_online_home(log: &TestLog) -> Home {
    let home = online_home(log);
    log.step("home");
    plant::records_for_sources(&home, &PROBED);
    plant::stored_token(
        &home,
        fixture::fixtures::fm_network_credential_rejected::TOKEN,
    );
    log.step("answers");
    log.json("answers", &probes::serve_healthy(&home));
    home
}

#[test]
fn online_checks_are_skipped_without_online() {
    let log = tlog!();
    let home = healthy_online_home(&log);
    let before = lines(&home);
    log.step("diagnose");
    let ran = run(&log, &home, &["doctor", "--json"]);
    ran.exit(0);
    let envelope = ran.envelope();
    let skipped: Vec<(&str, &str)> = envelope["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["reason"] == "online_required")
        .map(|s| (s["id"].as_str().unwrap(), s["reason"].as_str().unwrap()))
        .collect();
    log.assert_eq(
        "online skips",
        &skipped,
        &NETWORK
            .iter()
            .map(|id| (*id, "online_required"))
            .collect::<Vec<_>>(),
    );
    assert_eq!(envelope["summary"]["online_required"], 3);
    log.step("human");
    let human = run(&log, &home, &["doctor", "-v"]);
    assert!(
        human.stdout().contains("3 online checks skipped"),
        "{}",
        human.stdout()
    );
    assert!(added(&log, &home, before).is_empty(), "no request");
}

#[test]
fn online_only_selection_is_online_required() {
    let log = tlog!();
    let cases: [(&str, &[&str], &[&str]); 3] = [
        (
            "rate_limited",
            &["doctor", "--only", ids::NETWORK_GITHUB_RATE_LIMITED],
            &[ids::NETWORK_GITHUB_RATE_LIMITED],
        ),
        (
            "two_online_rows",
            &[
                "doctor",
                "--only",
                "fm-network-source-unreachable,fm-network-credential-rejected",
            ],
            &[
                ids::NETWORK_CREDENTIAL_REJECTED,
                ids::NETWORK_SOURCE_UNREACHABLE,
            ],
        ),
        (
            "fix",
            &["doctor", "fix", "--only", ids::NETWORK_CREDENTIAL_REJECTED],
            &[ids::NETWORK_CREDENTIAL_REJECTED],
        ),
    ];
    for (name, argv, fms) in cases {
        let log = log.case(name);
        let home = healthy_online_home(&log);
        let before = lines(&home);
        log.step("diagnose");
        let mut args = argv.to_vec();
        args.push("--json");
        let ran = run(&log, &home, &args);
        ran.fails(6, "online_required");
        let error = &ran.envelope()["error"];
        let mut got: Vec<&str> = error["details"]["fms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        got.sort_unstable();
        assert_eq!(got, fms, "{name}");
        let fix = words(&error["fix_argv"]);
        let mut expected: Vec<String> = std::iter::once("omaddons")
            .chain(argv.iter().copied())
            .map(str::to_owned)
            .collect();
        expected.push("--online".to_owned());
        assert_eq!(fix, expected, "{name}: the argv plus --online");
        assert!(!home.state().join("doctor").exists(), "{name}: no run");
        assert!(added(&log, &home, before).is_empty(), "{name}: no request");
        log.step("fix");
        run_argv(&log, &home, &fix).exit(0);
    }
}

#[test]
fn online_mixed_selection_runs_the_offline_rows() {
    let log = tlog!();
    let home = online_home(&log);
    log.step("home");
    plant::records_for_sources(&home, &PROBED);
    plant::credential_file_mode(&home);
    let before = lines(&home);
    log.step("diagnose");
    // ubs:ignore — failure-mode ids, not secrets
    let only = format!(
        "{},{}",
        ids::CREDENTIAL_FILE_MODE,
        ids::NETWORK_SOURCE_UNREACHABLE
    );
    let ran = run(&log, &home, &["doctor", "--json", "--only", &only]);
    ran.exit(1);
    let envelope = ran.envelope();
    assert_eq!(findings(envelope, ids::CREDENTIAL_FILE_MODE).len(), 1);
    assert_eq!(
        envelope["skipped"],
        json!([{"id": ids::NETWORK_SOURCE_UNREACHABLE, "reason": "online_required", "target": null}])
    );
    assert!(added(&log, &home, before).is_empty(), "no request");
}

#[test]
fn online_each_check_sends_exactly_one_request() {
    let log = tlog!();
    let probe_lines = || -> Vec<Line> {
        [
            ("GET", "github.com", "/f7probe/Probe/releases/latest"),
            ("GET", "api.mmoui.com", "/v3/game/WOW/filedetails/5995.json"),
            ("GET", "api.tukui.org", "/v1/addons"),
            ("PROPFIND", "repos.wowace.com", "/wow/bug-grabber/tags/"),
        ]
        .map(|(m, h, p)| {
            (
                m.to_owned(),
                h.to_owned(),
                p.to_owned(),
                "doctor-probe".to_owned(),
            )
        })
        .to_vec()
    };
    let cases: Vec<(&str, Vec<&str>, Vec<Line>)> = vec![
        (
            "rate_limited",
            vec!["--only", ids::NETWORK_GITHUB_RATE_LIMITED],
            vec![rate_limit_line()],
        ),
        (
            "credential_rejected",
            vec!["--only", ids::NETWORK_CREDENTIAL_REJECTED],
            vec![rate_limit_line()],
        ),
        (
            "source_unreachable",
            vec!["--only", ids::NETWORK_SOURCE_UNREACHABLE],
            probe_lines(),
        ),
        (
            "plain_online",
            vec![],
            [
                vec![rate_limit_line()],
                vec![rate_limit_line()],
                probe_lines(),
            ]
            .concat(),
        ),
    ];
    for (name, extra, expected) in cases {
        let log = log.case(name);
        let home = healthy_online_home(&log);
        let before = lines(&home);
        log.step("diagnose");
        let mut args = vec!["doctor", "--json", "--online"];
        args.extend(extra);
        let ran = run(&log, &home, &args);
        ran.exit(0);
        assert_eq!(ran.envelope()["state"], "DONE_OK", "{name}");
        assert_eq!(ran.envelope()["findings"], json!([]), "{name}");
        let new = added(&log, &home, before);
        let mut got = requests(&new);
        let mut want = expected.clone();
        if name == "plain_online" {
            // The two GitHub rows run in registry order; only the multiset is promised.
            got.sort();
            want.sort();
        }
        assert_eq!(got, want, "{name}");
        for line in &new {
            assert!(
                omaddons::net::allowlist::ALWAYS.contains(&line.host.as_str()),
                "{name}: {} is always allowed",
                line.host
            );
            assert!(line.status.is_some(), "{name}: {line:?} has a status");
        }
    }

    // No credential: the credential row sends nothing.
    let log_none = log.case("nothing_configured");
    let home = online_home(&log_none);
    log_none.json("answers", &probes::serve_healthy(&home));
    let before = lines(&home);
    run(
        &log_none,
        &home,
        &[
            "doctor",
            "--json",
            "--online",
            "--only",
            ids::NETWORK_CREDENTIAL_REJECTED,
        ],
    )
    .exit(0);
    assert!(added(&log_none, &home, before).is_empty());

    // Records of no HTTP source (a local archive only): the source row sends nothing.
    let log_file = log.case("file_ref_only");
    let home = online_home(&log_file);
    let zip = plant::local_zip(&home, "LocalOnly");
    let installed = log_file.run(
        &home,
        &[
            "install",
            zip.as_str(),
            "-t",
            "retail",
            "--offline",
            "--json",
        ],
    );
    installed.exit(0);
    let before = lines(&home);
    run(
        &log_file,
        &home,
        &[
            "doctor",
            "--json",
            "--online",
            "--only",
            ids::NETWORK_SOURCE_UNREACHABLE,
        ],
    )
    .exit(0);
    assert!(added(&log_file, &home, before).is_empty());
}

#[test]
fn online_rate_limit_is_reported() {
    let log = tlog!();
    let resets_at = fixture::fixtures::fm_network_github_rate_limited::BUDGET_RESETS_AT;
    let spent_headers = [
        ("x-ratelimit-limit", "5000"),
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "1767225600"),
    ];
    // Each case: a token stored, and the finding's limit, remaining and authenticated.
    type Expected = Option<(u64, u64, bool)>;
    let cases: [(&str, bool, Expected); 3] = [
        ("unauthenticated_nearly_spent", false, Some((60, 2, false))),
        (
            "authenticated_spent_by_headers",
            true,
            Some((5000, 0, true)),
        ),
        ("healthy", false, None),
    ];
    for (name, token, expected) in cases {
        let log = log.case(name);
        let home = online_home(&log);
        log.step("answers");
        if token {
            plant::stored_token(
                &home,
                fixture::fixtures::fm_network_credential_rejected::TOKEN,
            );
        }
        let answer = match name {
            "unauthenticated_nearly_spent" => probes::rate_limit(&home, 60, 2, resets_at),
            "authenticated_spent_by_headers" => {
                probes::rate_limit_status(&home, 403, &spent_headers, None)
            }
            _ => probes::rate_limit(&home, 60, 59, resets_at),
        };
        log.json("answers", &[answer]);
        let before = lines(&home);
        log.step("diagnose");
        let ran = run(
            &log,
            &home,
            &[
                "doctor",
                "--json",
                "--online",
                "--only",
                ids::NETWORK_GITHUB_RATE_LIMITED,
            ],
        );
        let found = findings(ran.envelope(), ids::NETWORK_GITHUB_RATE_LIMITED);
        match expected {
            None => {
                ran.exit(0);
                assert!(found.is_empty(), "{name}");
            }
            Some((limit, remaining, authenticated)) => {
                ran.exit(1);
                assert_eq!(found.len(), 1, "{name}");
                let evidence = &found[0]["evidence"];
                assert_eq!(
                    (
                        &evidence["rate_limit"],
                        &evidence["authenticated"],
                        evidence.get("origin")
                    ),
                    (
                        &json!({"limit": limit, "remaining": remaining, "reset_at": "2026-01-01T00:00:00Z"}),
                        &json!(authenticated),
                        authenticated.then_some(&json!("file"))
                    ),
                    "{name}"
                );
                let advice = found[0]["remediation"]["command"].as_str().unwrap();
                assert!(advice.contains("2026-01-01 00:00 UTC"), "{name}: {advice}");
                assert_eq!(
                    advice.contains("pipe a token to omaddons auth github set"),
                    !authenticated,
                    "{name}: {advice}"
                );
                assert!(found[0]["remediation"]["argv"].is_null());
            }
        }
        assert_eq!(added(&log, &home, before).len(), 1, "{name}: one request");
    }
}

#[test]
fn online_credential_rejected_is_reported_and_cleared() {
    let log = tlog!();
    // ubs:ignore — a command line naming a failure-mode id, not a secret
    let only = [
        "doctor",
        "--json",
        "--online",
        "--only",
        ids::NETWORK_CREDENTIAL_REJECTED,
    ];

    let file = log.case("file");
    let home = online_home(&file);
    file.step("answers");
    let token = plant::stored_token(
        &home,
        fixture::fixtures::fm_network_credential_rejected::TOKEN,
    );
    file.json(
        "answers",
        &[probes::rate_limit_status(&home, 401, &[], None)],
    );
    file.step("diagnose");
    let ran = run(&file, &home, &only);
    ran.exit(1);
    let found = findings(ran.envelope(), REJECTED);
    assert_eq!(found.len(), 1);
    assert_eq!(
        (
            &found[0]["evidence"]["reason"],
            &found[0]["evidence"]["origin"],
            &found[0]["evidence"]["http_status"]
        ),
        (&json!("rejected"), &json!("file"), &json!(401))
    );
    let argv = words(&found[0]["remediation"]["argv"]);
    assert_eq!(argv, ["omaddons", "auth", "github", "clear"]);
    file.step("remedy");
    run_argv(&file, &home, &argv).exit(0);
    assert_eq!(
        fs::read(&token).unwrap(),
        b"",
        "the stored token is cleared"
    );
    file.step("verify");
    let before = lines(&home);
    let ran = run(&file, &home, &only);
    ran.exit(0);
    assert!(findings(ran.envelope(), ids::NETWORK_CREDENTIAL_REJECTED).is_empty());
    assert!(
        added(&file, &home, before).is_empty(),
        "no credential, no request"
    );

    let env = log.case("env");
    let home = online_home(&env);
    env.json(
        "answers",
        &[probes::rate_limit_status(&home, 401, &[], None)],
    );
    env.step("diagnose");
    // ubs:ignore — a run's output, which the test checks holds no token
    let ran = env
        .cmd(&home, &only)
        .env("OMARCHY_PATH", plant::omarchy_path(&home))
        .env("OMADDONS_GITHUB_TOKEN", ENV_TOKEN)
        .run();
    ran.exit(1);
    let found = findings(ran.envelope(), REJECTED);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["evidence"]["origin"], "env");
    assert!(found[0]["remediation"]["argv"].is_null());
    assert_eq!(
        found[0]["remediation"]["command"],
        "replace or unset OMADDONS_GITHUB_TOKEN"
    );

    let unresolvable = log.case("unresolvable");
    let home = online_home(&unresolvable);
    unresolvable.step("config");
    let config = home.env.config().join("omaddons/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        "[sources.github]\ncredential = { env = \"OMADDONS_F7_NO_SUCH_VAR\" }\n",
    )
    .unwrap();
    unresolvable.json(
        "answers",
        &[probes::rate_limit(&home, 60, 59, 1_767_225_600)],
    );
    let before = lines(&home);
    unresolvable.step("diagnose");
    // ubs:ignore — failure-mode ids, not secrets
    let both = format!(
        "{},{}",
        ids::NETWORK_CREDENTIAL_REJECTED,
        ids::NETWORK_GITHUB_RATE_LIMITED
    );
    let ran = run(
        &unresolvable,
        &home,
        &["doctor", "--json", "--online", "--only", &both],
    );
    ran.exit(1);
    let envelope = ran.envelope();
    let found = findings(envelope, REJECTED);
    assert_eq!(found.len(), 1);
    assert_eq!(
        (
            &found[0]["evidence"]["reason"],
            &found[0]["evidence"]["origin"],
            &found[0]["remediation"]["command"]
        ),
        (
            &json!("unresolvable"),
            &json!("config"),
            &json!("correct [sources.github] credential in config.toml")
        )
    );
    let cut = findings(envelope, ids::CHECK_INCOMPLETE);
    assert_eq!(cut.len(), 1, "{:?}", ids_of(envelope));
    assert_eq!(
        (
            &cut[0]["evidence"]["detector"],
            &cut[0]["evidence"]["reason"]
        ),
        (
            &json!(ids::NETWORK_GITHUB_RATE_LIMITED),
            &json!("unanswered")
        )
    );
    assert!(
        added(&unresolvable, &home, before).is_empty(),
        "nothing sent"
    );
}

/// Answers for every source: GitHub 302 (or `github`), WoWInterface 503, Tukui 429,
/// WowAce 207.
fn mixed_answers(home: &Home, github: u16) -> Vec<probes::Answer> {
    vec![
        probes::source(home, SourceId::Github, github),
        probes::source(home, SourceId::Wowi, 503),
        probes::source(home, SourceId::Tukui, 429),
        probes::source(home, SourceId::Wowace, 207),
    ]
}

#[test]
fn online_unreachable_sources_are_reported_per_source() {
    let log = tlog!();
    for (name, github) in [("github_redirects", 302), ("github_project_gone", 404)] {
        let log = log.case(name);
        let home = online_home(&log);
        log.step("home");
        plant::records_for_sources(&home, &PROBED);
        log.json("answers", &mixed_answers(&home, github));
        let before = lines(&home);
        log.step("diagnose");
        let ran = run(
            &log,
            &home,
            &[
                "doctor",
                "--json",
                "--online",
                "--only",
                ids::NETWORK_SOURCE_UNREACHABLE,
            ],
        );
        ran.exit(1);
        let found = findings(ran.envelope(), ids::NETWORK_SOURCE_UNREACHABLE);
        let got: Vec<(&str, &str, &Value, &str, &str, &str)> = found
            .iter()
            .map(|f| {
                (
                    f["instance"].as_str().unwrap(),
                    f["evidence"]["reason"].as_str().unwrap(),
                    &f["evidence"]["http_status"],
                    f["evidence"]["host"].as_str().unwrap(),
                    f["confidence"].as_str().unwrap(),
                    f["label"].as_str().unwrap(),
                )
            })
            .collect();
        let (s503, s429) = (json!(503), json!(429));
        assert_eq!(
            got,
            [
                (
                    "fm-network-source-unreachable@machine:source=tukui",
                    "too_many_requests",
                    &s429,
                    "api.tukui.org",
                    "high",
                    "machine · Tukui"
                ),
                (
                    "fm-network-source-unreachable@machine:source=wowi",
                    "server_error",
                    &s503,
                    "api.mmoui.com",
                    "high",
                    "machine · WoWInterface"
                ),
            ],
            "{name}"
        );
        assert_eq!(added(&log, &home, before).len(), 4, "{name}");
        log.step("explain");
        let before = lines(&home);
        let explained = run(
            &log,
            &home,
            &[
                "doctor",
                "explain",
                "fm-network-source-unreachable@machine:source=wowi",
                "--json",
            ],
        );
        explained.exit(0);
        let envelope = explained.envelope();
        assert_eq!(envelope["instances"], json!([]), "{name}");
        assert_eq!(
            envelope["skipped"],
            json!([{"id": ids::NETWORK_SOURCE_UNREACHABLE, "reason": "online_required", "target": null}]),
            "{name}"
        );
        assert!(
            added(&log, &home, before).is_empty(),
            "{name}: explain sends nothing"
        );
    }
}

#[test]
fn online_unanswered_is_incomplete_never_clean() {
    let log = tlog!();

    let unreadable = log.case("unreadable_rate_limit");
    let home = online_home(&unreadable);
    unreadable.json(
        "answers",
        &[probes::rate_limit_status(&home, 200, &[], Some("not json"))],
    );
    unreadable.step("diagnose");
    let ran = run(
        &unreadable,
        &home,
        &[
            "doctor",
            "--json",
            "--online",
            "--only",
            ids::NETWORK_GITHUB_RATE_LIMITED,
        ],
    );
    ran.exit(1);
    let envelope = ran.envelope();
    assert_eq!(envelope["state"], "DONE_FINDINGS");
    let cut = findings(envelope, ids::CHECK_INCOMPLETE);
    assert_eq!(cut.len(), 1, "{:?}", ids_of(envelope));
    let evidence = &cut[0]["evidence"];
    assert_eq!(
        (&evidence["detector"], &evidence["reason"]),
        (
            &json!(ids::NETWORK_GITHUB_RATE_LIMITED),
            &json!("unanswered")
        )
    );
    assert!(
        evidence.get("deadline_ms").is_none() && evidence.get("max_entries").is_none(),
        "{evidence}"
    );
    let again = words(&cut[0]["remediation"]["argv"]);
    assert_eq!(
        again,
        [
            "omaddons",
            "doctor",
            "--online",
            "--only",
            ids::NETWORK_GITHUB_RATE_LIMITED
        ]
    );
    unreadable.step("ask again");
    let rerun = run_argv(&unreadable, &home, &again);
    rerun.exit(1);
    assert_eq!(findings(rerun.envelope(), ids::CHECK_INCOMPLETE).len(), 1);

    let missing = log.case("missing_fixture");
    let home = online_home(&missing);
    missing.step("home");
    plant::records_for_sources(&home, &[SourceId::Github, SourceId::Wowi]);
    missing.json("answers", &[probes::source(&home, SourceId::Github, 302)]);
    missing.step("diagnose");
    let ran = run(
        &missing,
        &home,
        &[
            "doctor",
            "--json",
            "--online",
            "--only",
            ids::NETWORK_SOURCE_UNREACHABLE,
        ],
    );
    ran.exit(1);
    let envelope = ran.envelope();
    assert!(
        findings(envelope, ids::NETWORK_SOURCE_UNREACHABLE).is_empty(),
        "omaddons' own failure is no unreachable source: {:?}",
        ids_of(envelope)
    );
    let cut = findings(envelope, ids::CHECK_INCOMPLETE);
    assert_eq!(cut.len(), 1);
    assert_eq!(
        (
            &cut[0]["evidence"]["detector"],
            &cut[0]["evidence"]["reason"]
        ),
        (
            &json!(ids::NETWORK_SOURCE_UNREACHABLE),
            &json!("unanswered")
        )
    );
}

#[test]
fn online_probe_respects_the_deadline() {
    let log = tlog!();
    let home = online_home(&log);
    log.json(
        "answers",
        &[probes::rate_limit(&home, 60, 59, 1_767_225_600)],
    );
    let before = lines(&home);
    log.step("diagnose");
    let ran = run_with(
        &log,
        &home,
        &[
            "doctor",
            "--json",
            "--online",
            "--only",
            ids::NETWORK_GITHUB_RATE_LIMITED,
            "--budget",
            "500ms",
        ],
        &[("OMADDONS_FAILPOINTS", "doctor.detector.panic=sleep:600")],
    );
    ran.exit(1);
    let cut = findings(ran.envelope(), ids::CHECK_INCOMPLETE);
    assert_eq!(cut.len(), 1, "{:?}", ids_of(ran.envelope()));
    assert_eq!(cut[0]["evidence"]["reason"], "deadline");
    assert!(
        added(&log, &home, before).is_empty(),
        "an expired budget sends nothing"
    );
}

#[test]
fn online_fix_lists_network_findings_as_manual() {
    let log = tlog!();
    let home = online_home(&log);
    log.step("home");
    let token = plant::credential_file_mode(&home);
    let bytes = fs::read(&token).unwrap();
    log.json(
        "answers",
        &[probes::rate_limit_status(&home, 401, &[], None)],
    );
    log.step("fix");
    let ran = run(&log, &home, &["doctor", "fix", "--online", "--json"]);
    let envelope = ran.envelope();
    assert_eq!(envelope["summary"]["actions_taken"], 1, "{envelope:#}");
    let actions: Vec<(&str, &str)> = envelope["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["fm"].as_str().unwrap(), a["op"].as_str().unwrap()))
        .collect();
    assert_eq!(actions, [(ids::CREDENTIAL_FILE_MODE, "chmod")]);
    assert_eq!(mode(&token), 0o600, "repaired");
    assert_eq!(
        fs::read(&token).unwrap(),
        bytes,
        "chmodded, never rewritten"
    );
    let rejected = findings(envelope, REJECTED);
    assert_eq!(rejected.len(), 1, "{:?}", ids_of(envelope));
    assert_eq!(
        words(&rejected[0]["remediation"]["argv"]),
        ["omaddons", "auth", "github", "clear"]
    );
    assert_eq!(rejected[0]["remediation"]["auto_fixable"], false);
    // Its repairs verified, but findings it does not repair remain: exit 1, never healthy.
    ran.exit(1);
    assert_eq!(envelope["state"], "DONE_FINDINGS", "{envelope:#}");
}

#[test]
fn online_token_never_appears() {
    let log = tlog!();
    let home = online_home(&log);
    let stored = fixture::fixtures::fm_network_credential_rejected::TOKEN;
    let mut outputs = Vec::new();
    log.step("file");
    plant::stored_token(&home, stored);
    probes::rate_limit_status(&home, 401, &[], None);
    // ubs:ignore — a command line naming a failure-mode id, not a secret
    let only = [
        "doctor",
        "--json",
        "--online",
        "--only",
        ids::NETWORK_CREDENTIAL_REJECTED,
    ];
    let ran = run(&log, &home, &only);
    outputs.push((ran.stdout().to_owned(), ran.stderr().to_owned()));
    log.step("env");
    // ubs:ignore — a run's output, which the test checks holds no token
    let ran = log
        .cmd(&home, &only)
        .env("OMARCHY_PATH", plant::omarchy_path(&home))
        .env("OMADDONS_GITHUB_TOKEN", ENV_TOKEN)
        .run();
    outputs.push((ran.stdout().to_owned(), ran.stderr().to_owned()));
    log.step("fix");
    // The stored token exposed, so the fix run repairs it as well.
    fs::set_permissions(plant::token_file(&home), fs::Permissions::from_mode(0o644)).unwrap();
    let ran = run(&log, &home, &["doctor", "fix", "--online", "--json"]);
    outputs.push((ran.stdout().to_owned(), ran.stderr().to_owned()));
    log.step("scan");
    for token in [stored, ENV_TOKEN] {
        for (stdout, stderr) in &outputs {
            assert!(!stdout.contains(token) && !stderr.contains(token));
        }
        for root in [home.state(), log.dir().to_path_buf()] {
            for file in files_below(&root) {
                let text = fs::read(&file).unwrap_or_default();
                assert!(
                    !String::from_utf8_lossy(&text).contains(token),
                    "{} holds a token",
                    file.display()
                );
            }
        }
    }
}

/// Every regular file below `root`.
fn files_below(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out
}

#[test]
fn online_findings_are_repeatable() {
    let log = tlog!();
    let home = online_home(&log);
    log.step("home");
    plant::records_for_sources(&home, &PROBED);
    log.json("answers", &mixed_answers(&home, 302));
    probes::rate_limit(&home, 60, 59, 1_767_225_600);
    log.step("diagnose twice");
    let first = run(&log, &home, &["doctor", "--json", "--online"]);
    let second = run(&log, &home, &["doctor", "--json", "--online"]);
    assert_eq!(
        stable(first.envelope())["findings"],
        stable(second.envelope())["findings"]
    );
    assert_eq!(
        findings(first.envelope(), ids::NETWORK_SOURCE_UNREACHABLE).len(),
        2
    );
}

#[test]
fn online_no_false_positive_on_the_realistic_home() {
    let log = tlog!();
    let dir = tempfile::tempdir().unwrap();
    log.step("home");
    let realistic = common::realistic::realistic_home(&dir.path().join("home"));
    let home = &realistic.home;
    log.env(home);
    log.step("answers");
    let answers = probes::serve_healthy(home);
    log.json("answers", &answers);
    let before = lines(home);
    log.step("diagnose");
    let only = NETWORK.join(",");
    let ran = run(
        &log,
        home,
        &["doctor", "--json", "--online", "--only", &only],
    );
    ran.exit(0);
    assert_eq!(ran.envelope()["findings"], json!([]));
    let new = added(&log, home, before);
    // GitHub is the one probed source; the rate-limit row asks /rate_limit, and the
    // credential row has no credential to check.
    assert_eq!(
        requests(&new),
        [
            rate_limit_line(),
            (
                "GET".to_owned(),
                "github.com".to_owned(),
                "/bench/addon00/releases/latest".to_owned(),
                "doctor-probe".to_owned()
            ),
        ]
    );
}

/// The three network fixtures: registered, online, report-only; `corrupt` yields exactly
/// one finding of its row, and `remedy` makes `assert_healthy` hold.
#[test]
fn online_fixtures_corrupt_and_remedy() {
    let log = tlog!();
    let network: Vec<_> = fixture::fixtures::all()
        .into_iter()
        .filter(|fixture| NETWORK.contains(&fixture.id()))
        .collect();
    let ids: Vec<&str> = network.iter().map(|f| f.id()).collect();
    log.assert_eq("registered", &ids, &NETWORK.to_vec());
    for fixture in network {
        let log = log.case(fixture.id());
        assert_eq!(fixture.run_args(), ["--online"], "{}", fixture.id());
        assert!(!fixture.fixer(), "{}", fixture.id());
        let home = online_home(&log);
        log.step("corrupt");
        fixture.corrupt(&home);
        log.step("diagnose");
        let mut args = vec!["doctor".to_owned(), "--json".to_owned()];
        args.extend(fixture.run_args());
        args.extend(["--only".to_owned(), fixture.id().to_owned()]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = run(&log, &home, &args);
        ran.exit(1);
        let found = findings(ran.envelope(), fixture.id());
        assert_eq!(found.len(), 1, "{:?}", ids_of(ran.envelope()));
        assert_eq!(ids_of(ran.envelope()), [fixture.id()], "only its finding");
        log.step("remedy");
        fixture.remedy(&home);
        log.step("verify");
        fixture.assert_healthy(&home);
    }
}

// ---------------------------------------------------------------------------------------
// `doctor gc` (M4-D36): old runs, 40 days before `OMADDONS_NOW`.

const GC_NOW: (&str, &str) = ("OMADDONS_NOW", "2026-10-15T00:00:00Z");
const GC_FIX_TX: &str = "01M5000000000000000000000T";
const GC_HELD_TX: &str = "01M5000000000000000000000V";

/// What [`gc_home`] seeded.
struct GcSeed {
    /// An old fix run with a backup and a quarantined file, its client half and its
    /// cache quarantine.
    fix: String,
    /// An old diagnose run.
    diagnose: String,
    /// An old fix run whose transaction's in-flight lock the test may hold.
    held: String,
    /// A young run, which `latest` will name.
    young: String,
}

/// A home whose retail client is registered, with [`GcSeed`]'s runs and `latest` naming
/// the old fix run.
fn gc_home(log: &TestLog) -> (Home, GcSeed) {
    let home = home(log);
    log.step("seed");
    common::doctor::test_fixers::register(&home, "retail");
    let fix = plant::seed_run(
        &home,
        "fix",
        "DONE_OK",
        "2026-09-01T00:00:00Z",
        1,
        Some(GC_FIX_TX),
        &["backups/1", "quarantine/state/x"],
    );
    plant::seed_half(&home, "retail", GC_FIX_TX, &["files/1"]);
    plant::seed_cache_quarantine(&home, &fix, &["blobs/ab"]);
    let diagnose = plant::seed_run(
        &home,
        "diagnose",
        "DONE_FINDINGS",
        "2026-09-02T00:00:00Z",
        2,
        None,
        &[],
    );
    let held = plant::seed_run(
        &home,
        "fix",
        "DONE_OK",
        "2026-09-03T00:00:00Z",
        3,
        Some(GC_HELD_TX),
        &[],
    );
    let young = plant::seed_run(
        &home,
        "diagnose",
        "DONE_OK",
        "2026-10-10T00:00:00Z",
        4,
        None,
        &[],
    );
    std::os::unix::fs::symlink(format!("runs/{fix}"), home.state().join("doctor/latest")).unwrap();
    (
        home,
        GcSeed {
            fix,
            diagnose,
            held,
            young,
        },
    )
}

/// `doctor gc <args> --json` at [`GC_NOW`].
fn gc(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = vec!["doctor", "gc"];
    argv.extend(args);
    argv.push("--json");
    run_with(log, home, &argv, &[GC_NOW])
}

/// Runs an envelope's `error.fix_argv` (with `--json`) at [`GC_NOW`].
fn gc_fix(log: &TestLog, home: &Home, envelope: &Value) -> Ran {
    let fix: Vec<String> = envelope["error"]["fix_argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .chain(["--json".to_owned()])
        .collect();
    let fix: Vec<&str> = fix.iter().map(String::as_str).collect();
    run_with(log, home, &fix, &[GC_NOW])
}

/// Every place gc deletes from, by its label. (Taking the global lock rewrites
/// `<state>/lock`'s holder note, so the state directory as a whole is no measure.)
fn gc_roots(home: &Home) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("doctor", home.state().join("doctor")),
        (
            "cache quarantine",
            home.env.cache().join("omaddons/quarantine"),
        ),
        (
            "client snapshots",
            home.client("_retail_")
                .join("Interface/.omaddons/snapshots"),
        ),
    ]
}

fn gc_trees(log: &TestLog, home: &Home) {
    for (label, root) in gc_roots(home) {
        log.tree(label, &root);
    }
}

fn gc_unchanged(log: &TestLog, home: &Home) {
    for (label, root) in gc_roots(home) {
        log.assert_unchanged(label, &root);
    }
}

#[test]
fn gc_cli_requires_before() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    gc_trees(&log, &home);
    log.step("no --before");
    let ran = gc(&log, &home, &[]);
    ran.exit(64);
    assert_eq!(ran.envelope()["error"]["details"]["flag"], "--before");
    log.step("its fix");
    gc_fix(&log, &home, ran.envelope()).exit(0);
    gc_unchanged(&log, &home);
}

#[test]
fn gc_cli_real_run_requires_yes() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    gc_trees(&log, &home);
    log.step("a real run without --yes");
    let ran = gc(&log, &home, &["--before", "30d"]);
    ran.exit(64);
    let error = &ran.envelope()["error"];
    assert_eq!(error["details"]["flag"], "--yes");
    assert_eq!(
        error["fix_argv"].as_array().unwrap().last().unwrap(),
        "--dry-run"
    );
    log.step("its fix: the preview");
    gc_fix(&log, &home, ran.envelope()).exit(0);
    gc_unchanged(&log, &home);
}

#[test]
fn gc_cli_refuses_a_future_or_malformed_before_and_t() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    gc_trees(&log, &home);
    for (case, args, flag) in [
        (
            "future",
            &["--before", "2099-01-01T00:00:00Z", "--dry-run"][..],
            "--before",
        ),
        (
            "malformed",
            &["--before", "yesterday", "--dry-run"],
            "--before",
        ),
        (
            "target",
            &["--before", "30d", "-t", "retail", "--dry-run"],
            "-t",
        ),
    ] {
        let log = log.case(case);
        let ran = gc(&log, &home, args);
        ran.exit(64);
        assert_eq!(ran.envelope()["error"]["details"]["flag"], flag, "{case}");
        gc_fix(&log, &home, ran.envelope()).exit(0);
    }
    gc_unchanged(&log, &home);
}

#[test]
fn gc_dry_run_deletes_nothing_and_takes_no_lock() {
    let log = tlog!();
    let (home, seed) = gc_home(&log);
    log.step("another process holds the global lock");
    let (guard, _) = plant::hold_lock(&home, "test");
    log.tree("the whole home", home.env.root());
    log.step("preview");
    let started = std::time::Instant::now();
    let ran = gc(&log, &home, &["--before", "30d", "--dry-run"]);
    ran.exit(0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "a preview never waits for the lock"
    );
    let report = ran.envelope();
    log.json("plan", report);
    let listed: Vec<&str> = report["deletions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["run_id"].as_str())
        .collect();
    assert!(listed.contains(&seed.fix.as_str()) && listed.contains(&seed.diagnose.as_str()));
    log.assert_unchanged("the whole home", home.env.root());
    drop(guard);
}

#[test]
fn gc_real_run_waits_for_the_lock() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    gc_trees(&log, &home);
    log.step("another process holds the global lock");
    let (guard, _) = plant::hold_lock(&home, "test");
    log.step("a consented run");
    let ran = gc(&log, &home, &["--before", "30d", "--yes"]);
    ran.exit(5);
    assert_eq!(ran.envelope()["error"]["code"], "locked");
    assert!(ran.envelope()["error"]["details"]["holder"].is_object());
    drop(guard);
    gc_unchanged(&log, &home);
}

#[test]
fn gc_deletes_exactly_what_its_preview_listed() {
    let log = tlog!();
    let (home, seed) = gc_home(&log);
    log.step("the held run's transaction is live");
    let paths = common::http::paths(&home.env);
    let held_tx: omaddons::model::TxId = GC_HELD_TX.parse().unwrap();
    let inflight = omaddons::fsx::InflightLock::create(&paths, &held_tx).unwrap();
    log.step("preview");
    let preview = gc(&log, &home, &["--before", "30d", "--dry-run"]);
    preview.exit(0);
    let report = preview.envelope().clone();
    log.json("plan", &report);
    let listed: Vec<PathBuf> = report["deletions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| PathBuf::from(d["path"].as_str().unwrap()))
        .collect();
    let kept: Vec<PathBuf> = report["exclusions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| PathBuf::from(e["path"].as_str().unwrap()))
        .collect();
    assert_eq!(
        listed.len(),
        4,
        "the fix run, its half and quarantine, the diagnose"
    );
    assert_eq!(report["exclusions"][0]["pin"], "active_run", "{report}");
    assert_eq!(
        report["latest"],
        json!({"from": seed.fix, "to": seed.young})
    );
    let doorbell = home.env.cache().join("omaddons/doorbell.json");
    let rang = fs::read(&doorbell).ok();
    log.step("bound run");
    let token = report["precondition"].as_str().unwrap();
    let ran = gc(
        &log,
        &home,
        &["--before", "30d", "--yes", "--if-match", token],
    );
    ran.exit(0);
    log.step("assert");
    let gone: Vec<&PathBuf> = listed.iter().filter(|p| !p.exists()).collect();
    log.json(
        "removed vs listed",
        &json!({"listed": listed, "removed": gone, "kept": kept}),
    );
    assert_eq!(gone.len(), listed.len(), "every listed path was removed");
    assert!(kept.iter().all(|p| p.exists()), "every kept path remains");
    assert!(home.state().join("doctor/runs").join(&seed.young).is_dir());
    assert!(home.state().join("doctor/runs").join(&seed.held).is_dir());
    assert_eq!(
        fs::read_link(home.state().join("doctor/latest")).unwrap(),
        Path::new("runs").join(&seed.young)
    );
    for trash in [
        home.state().join("trash"),
        home.env.cache().join("omaddons/trash"),
        home.client("_retail_").join("Interface/.omaddons/trash"),
    ] {
        let left = fs::read_dir(&trash).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "{} is empty", trash.display());
    }
    assert_ne!(fs::read(&doorbell).ok(), rang, "the doorbell rang");
    drop(inflight);
}

/// A delete that fails once its victim is in trash (omad-4h0d.13). A lasting failure
/// (`fsx.prune.delete=error`, the sweep's deletes too) leaves every planned deletion in
/// trash: none is listed in `deletions`, each is in `skipped` as `left_in_trash` with the
/// error, and the run exits 74 with an `io` error beside its report. A passing one
/// (`@1`, the first delete only) is finished by the same prune's sweep: every deletion
/// is listed, nothing is skipped, exit 0.
#[test]
fn gc_reports_a_deletion_it_could_not_make_as_skipped() {
    let log = tlog!();
    let (home, seed) = gc_home(&log);
    log.step("preview");
    let preview = gc(&log, &home, &["--before", "30d", "--dry-run"]);
    preview.exit(0);
    let planned: Vec<String> = preview.envelope()["deletions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap().to_owned())
        .collect();
    // No transaction is live, so the held run is eligible too.
    assert_eq!(planned.len(), 5, "{}", preview.envelope());
    let trash_entries_in = |home: &Home| -> usize {
        [
            home.state().join("trash"),
            home.env.cache().join("omaddons/trash"),
            home.client("_retail_").join("Interface/.omaddons/trash"),
        ]
        .iter()
        .map(|t| fs::read_dir(t).map(|d| d.count()).unwrap_or(0))
        .sum()
    };

    log.step("every delete fails");
    let ran = run_with(
        &log,
        &home,
        &["doctor", "gc", "--before", "30d", "--yes", "--json"],
        &[GC_NOW, ("OMADDONS_FAILPOINTS", "fsx.prune.delete=error")],
    );
    ran.exit(74);
    let report = ran.envelope().clone();
    log.json("report", &report);
    assert_eq!(report["ok"], false, "{report}");
    assert_eq!(report["deletions"], json!([]), "{report}");
    let skipped = report["skipped"].as_array().unwrap();
    assert_eq!(
        skipped
            .iter()
            .map(|s| s["path"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        planned,
        "every planned deletion is skipped, in order"
    );
    for s in skipped {
        assert_eq!(s["reason"], "left_in_trash", "{s}");
        assert!(
            s["message"]
                .as_str()
                .unwrap()
                .starts_with("left in trash: "),
            "{s}"
        );
    }
    assert_eq!(report["error"]["code"], "io", "{report}");
    assert_eq!(report["error"]["details"]["path"], planned[0].as_str());
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("doctor gc deleted 0 of its 5 planned deletions"),
        "{report}"
    );
    assert!(planned.iter().all(|p| !Path::new(p).exists()));
    assert_eq!(trash_entries_in(&home), 5, "each waits in trash");
    assert_eq!(
        fs::read_link(home.state().join("doctor/latest")).unwrap(),
        Path::new("runs").join(&seed.young),
        "latest moved before the deletes"
    );

    log.step("a passing failure, finished by the sweep");
    let (home, _) = gc_home(&log);
    let ran = run_with(
        &log,
        &home,
        &["doctor", "gc", "--before", "30d", "--yes", "--json"],
        &[GC_NOW, ("OMADDONS_FAILPOINTS", "fsx.prune.delete=error@1")],
    );
    ran.exit(0);
    let report = ran.envelope().clone();
    log.json("report", &report);
    assert_eq!(report["skipped"], json!([]), "{report}");
    let deleted: Vec<&str> = report["deletions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert_eq!(deleted.len(), planned.len(), "{report}");
    assert!(deleted.iter().all(|p| !Path::new(p).exists()));
    assert_eq!(
        trash_entries_in(&home),
        0,
        "the sweep finished the failed one"
    );
    assert_eq!(report.get("error"), None, "{report}");
}

#[test]
fn gc_binds_a_dry_run_too() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    let preview = gc(&log, &home, &["--before", "30d", "--dry-run"]);
    preview.exit(0);
    let token = preview.envelope()["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    log.step("another old run");
    plant::seed_run(
        &home,
        "diagnose",
        "DONE_OK",
        "2026-09-05T00:00:00Z",
        9,
        None,
        &[],
    );
    let ran = gc(
        &log,
        &home,
        &["--before", "30d", "--dry-run", "--if-match", &token],
    );
    ran.exit(4);
    assert_eq!(ran.envelope()["error"]["code"], "plan_stale");
}

#[test]
fn gc_stale_token_deletes_nothing() {
    let log = tlog!();
    let (home, _) = gc_home(&log);
    let preview = gc(&log, &home, &["--before", "30d", "--dry-run"]);
    preview.exit(0);
    let token = preview.envelope()["precondition"]
        .as_str()
        .unwrap()
        .to_owned();
    log.step("another old run");
    plant::seed_run(
        &home,
        "diagnose",
        "DONE_OK",
        "2026-09-05T00:00:00Z",
        9,
        None,
        &[],
    );
    gc_trees(&log, &home);
    log.step("bound run");
    let ran = gc(
        &log,
        &home,
        &["--before", "30d", "--yes", "--if-match", &token],
    );
    ran.exit(4);
    let changed = &ran.envelope()["error"]["details"]["changed"];
    assert!(
        changed.as_array().unwrap().contains(&json!("selection")),
        "{changed}"
    );
    gc_unchanged(&log, &home);
}

#[test]
fn gc_output_is_private() {
    use common::privacy::assert_private_all;
    use common::wtf::Wtf;
    let log = tlog!();
    let (home, _) = gc_home(&log);
    Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 100)
        .build();
    for (case, args) in [
        ("preview", &["--before", "30d", "--dry-run"][..]),
        ("run", &["--before", "30d", "--yes"]),
    ] {
        let log = log.case(case);
        let mut argv = args.to_vec();
        argv.extend(["--progress", "json"]);
        let ran = gc(&log, &home, &argv);
        ran.exit(0);
        assert_private_all(ran.stdout(), ran.stderr());
    }
}

// ---------------------------------------------------------------------------------------
// `status` on the registry (F8, c6cm.6.8.1, M4-D29).

/// The failure-mode ids of `status`'s issues, the machine's and every client's, sorted
/// and once each (a client's journals of one row are one issue).
fn status_issue_ids(data: &Value) -> Vec<String> {
    let clients = data["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["issues"].as_array().into_iter().flatten());
    let mut ids: Vec<String> = data["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(clients)
        .filter_map(|issue| issue["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// A tree hash of each of `home`'s config, state and cache roots (`None` while absent).
fn roots_hash(home: &Home) -> Vec<(PathBuf, Option<String>)> {
    [home.env.config(), home.env.state(), home.env.cache()]
        .into_iter()
        .map(|root| {
            let hash = root.exists().then(|| common::trees::tree_hash(&root));
            (root, hash)
        })
        .collect()
}

/// `status --cached` reports exactly the doctor's `cached`-tier findings, by failure mode,
/// on the credential-mode fixture, a stuck journal, an outdated agent skill and a clean
/// realistic home, and leaves the config, state and cache roots as they were (no doctor
/// run directory is made). Plain `status` lists the skill's drift with its remediation.
#[test]
fn status_and_doctor_agree_on_the_cached_tier() {
    use fixture::Fixture;
    use fixture::fixtures::fm_concurrency_primitives_journal_too_new::JournalTooNew;
    use fixture::fixtures::fm_plugins_skill_version_drift::SkillVersionDrift;
    use fixture::fixtures::fm_secrets_credential_file_mode::CredentialFileMode;
    let log = tlog!();
    let dir = tempfile::tempdir().unwrap();
    let realistic = common::realistic::realistic_home(&dir.path().join("realistic"));
    let planted: [Box<dyn Fixture>; 3] = [
        Box::new(CredentialFileMode),
        Box::new(JournalTooNew::default()),
        Box::new(SkillVersionDrift),
    ];
    let mut homes: Vec<(&str, Option<&str>, Home)> = planted
        .iter()
        .map(|fixture| {
            let home = home(&log);
            fixture.corrupt(&home);
            (fixture.id(), Some(fixture.id()), home)
        })
        .collect();
    homes.push(("realistic_clean", None, realistic.home));
    for (case, expected, home) in &homes {
        let log = log.case(case);
        log.step("status --cached");
        let before = roots_hash(home);
        let ran = run(&log, home, &["status", "--cached", "--json"]);
        ran.ok();
        let from_status = status_issue_ids(ran.data());
        log.json("status", &from_status);
        assert_eq!(roots_hash(home), before, "{case}: status wrote nothing");
        assert!(
            !home.state().join("doctor").exists(),
            "{case}: status made no doctor run"
        );
        log.step("doctor --quick");
        let ran = run(&log, home, &["doctor", "--json", "--quick"]);
        let mut from_doctor: Vec<String> = ran.envelope()["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["id"].as_str())
            .filter(|id| {
                omaddons::doctor::registry::row(id).is_some_and(|row| row.tier == Tier::Cached)
            })
            .map(str::to_owned)
            .collect();
        from_doctor.sort();
        from_doctor.dedup();
        log.json("doctor", &from_doctor);
        assert_eq!(from_status, from_doctor, "{case}");
        match expected {
            Some(id) => assert!(
                from_status.iter().any(|f| f == id),
                "{case}: {from_status:?}"
            ),
            None => assert_eq!(from_status, Vec::<String>::new(), "{case}"),
        }
    }

    // The skill's drift through plain `status`, as `doctor` remediates it.
    let (_, _, home) = &homes[2];
    let log = log.case("skill_version_drift_plain");
    let ran = run(&log, home, &["status", "--json"]);
    ran.ok();
    let skill: Vec<&Value> = ran.data()["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|issue| issue["id"] == ids::SKILL_VERSION_DRIFT)
        .collect();
    assert_eq!(skill.len(), 1, "{skill:?}");
    assert_eq!(skill[0]["severity"], "P3");
    assert_eq!(
        skill[0]["remediation"]["argv"],
        json!(["omaddons", "robot-docs", "install-skill"])
    );
}
