//! e2e suite: the journal and lock detectors, and status's journal titles
#![cfg(test)]

mod common;
#[path = "doctor/mod.rs"]
#[allow(dead_code)]
mod fixture;

use std::collections::BTreeSet;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::privacy::assert_private;
use common::realistic::realistic_home;
use common::tlog::{Ran, TestLog, tlog};
use common::wtf::Wtf;
use fixture::Fixture;
use fixture::fixtures::fm_concurrency_primitives_journal_pending::JournalPending;
use fixture::fixtures::fm_concurrency_primitives_journal_preservation_incomplete::{
    JournalPreservationIncomplete, JournalPreservationStops,
};
use fixture::fixtures::fm_concurrency_primitives_journal_too_new::JournalTooNew;
use fixture::fixtures::fm_concurrency_primitives_journal_unexpected_state::JournalUnexpectedState;
use fixture::fixtures::fm_concurrency_primitives_journal_waits_for_game::JournalWaitsForGame;
use fixture::fixtures::{self, fm_concurrency_primitives_journal_marker_mismatch::OTHER};
use omaddons::contract::VOLATILE;
use omaddons::doctor::registry::{FAILURE_MODES, Tier, ids};
use omaddons::limits::LOCK_HELD_LONG;

/// The ten rows' common prefix (Appendix B rows 1-10).
const PREFIX: &str = "fm-concurrency_primitives-";

/// Every journal fixture plants on retail.
const TARGET: &str = "retail";

/// The concurrency rows' fixtures, in id order.
fn concurrency() -> Vec<Box<dyn Fixture>> {
    fixtures::all()
        .into_iter()
        .filter(|fixture| fixture.id().starts_with(PREFIX))
        .collect()
}

/// The ten rows' ids, in registry order.
fn rows() -> Vec<&'static str> {
    FAILURE_MODES
        .iter()
        .filter(|row| row.id.starts_with(PREFIX))
        .map(|row| row.id)
        .collect()
}

/// Retail and the classic client (row 7's other client), logged.
fn home(log: &TestLog) -> Home {
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    home
}

/// `omaddons <args>` with `env` and this home's own Omarchy.
fn run(log: &TestLog, home: &Home, args: &[&str], env: &[(String, String)]) -> Ran {
    let mut cmd = log
        .cmd(home, args)
        .env("OMARCHY_PATH", plant::omarchy_path(home));
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    cmd.run()
}

/// [`run`] of a `doctor` command (pass `--json`), checked against the doctor's envelope
/// contract.
fn doctor(log: &TestLog, home: &Home, args: &[&str], env: &[(String, String)]) -> Ran {
    let ran = run(log, home, args, env);
    check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
    ran
}

/// The findings of `envelope`.
fn findings(envelope: &Value) -> Vec<&Value> {
    envelope["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .collect()
}

/// Each concurrency fixture labelled by its id, then each variant, labelled `<id>
/// (stopping)`: the states the surfaces must name.
fn cases() -> Vec<(String, Box<dyn Fixture>)> {
    let mut cases: Vec<(String, Box<dyn Fixture>)> = concurrency()
        .into_iter()
        .map(|fixture| (fixture.id().to_owned(), fixture))
        .collect();
    cases.extend(
        fixtures::variants()
            .into_iter()
            .map(|fixture| (format!("{} (stopping)", fixture.id()), fixture)),
    );
    cases
}

/// The evidence keys and values the state each case plants must give (Technical Approach
/// §2), besides `tx`; `pid`, `age_s` and `count` are checked apart.
fn expected_evidence(label: &str, home: &Home) -> Vec<(&'static str, Value)> {
    match label.strip_prefix(PREFIX).unwrap() {
        "journal-pending" => vec![("phase", json!("prepared")), ("kind", json!("install"))],
        "journal-waits-for-game" => vec![
            ("reason", json!("saved_variables")),
            ("kind", json!("restore")),
        ],
        "journal-too-new" => vec![
            ("reason", json!("journal_too_new")),
            ("format", json!("9.0")),
        ],
        "journal-unreadable" => vec![("reason", json!("journal_unreadable"))],
        "journal-unexpected-state" => {
            vec![("reason", json!("unexpected_state")), ("step", json!(1))]
        }
        "journal-step-unrecognized" => {
            vec![("reason", json!("step_unrecognized")), ("step", json!(1))]
        }
        "journal-marker-mismatch" => vec![
            ("reason", json!("marker_mismatch")),
            (
                "path",
                json!(
                    home.client(plant::client_folder(OTHER))
                        .display()
                        .to_string()
                ),
            ),
            ("expected", json!(plant::target_key(home, TARGET))),
            ("observed", json!(plant::target_key(home, OTHER))),
        ],
        "journal-unattributed" => vec![("cause", json!("unreadable"))],
        "journal-preservation-incomplete" => vec![
            ("reason", json!("preservation_incomplete")),
            ("resumable", json!(true)),
        ],
        "journal-preservation-incomplete (stopping)" => vec![
            ("reason", json!("preservation_incomplete")),
            ("resumable", json!(false)),
        ],
        "lock-held-long" => vec![("command", json!("update"))],
        other => panic!("no expected evidence for {other}"), // ubs:ignore — the test's own failure
    }
}

/// The planted journal's transaction: the one journal a case leaves (`None` for the
/// lock, which plants none).
fn planted_tx(label: &str, home: &Home) -> Option<String> {
    let journals = plant::journals(home);
    if label.ends_with("lock-held-long") {
        assert!(journals.is_empty(), "{label}: {journals:?}");
        None
    } else {
        assert_eq!(journals.len(), 1, "{label}: one journal: {journals:?}");
        Some(journals[0].0.clone())
    }
}

/// Checks the one finding a case's `doctor --only` gives: its id, severity, instance and
/// evidence. `tx` is the planted journal's transaction (`None` for the lock).
fn check_finding(label: &str, id: &str, finding: &Value, home: &Home, tx: Option<&str>) {
    let row = omaddons::doctor::registry::row(id).unwrap();
    assert_eq!(finding["id"], id);
    assert_eq!(
        finding["severity"],
        format!("{:?}", row.severity),
        "{label}"
    );
    let evidence = &finding["evidence"];
    let instance = match (id.strip_prefix(PREFIX).unwrap(), tx) {
        ("journal-unattributed", Some(tx)) => format!("{id}@machine:tx={tx}"),
        (_, Some(tx)) => format!("{id}@{}:tx={tx}", plant::target_key(home, TARGET)),
        (_, None) => format!("{id}@machine"),
    };
    assert_eq!(finding["instance"], instance, "{label}: {finding}");
    if let Some(tx) = tx {
        assert_eq!(evidence["tx"], tx, "{label}: {finding}");
    }
    for (key, want) in expected_evidence(label, home) {
        assert_eq!(evidence[key], want, "{label}: evidence {key}: {finding}");
    }
    match id.strip_prefix(PREFIX).unwrap() {
        "journal-preservation-incomplete" => assert!(
            evidence["count"].as_u64().is_some_and(|n| n >= 1),
            "{label}: count: {finding}"
        ),
        "lock-held-long" => {
            assert_eq!(evidence["pid"], std::process::id(), "{id}: {finding}");
            let at_least = LOCK_HELD_LONG.as_secs() + 60;
            assert!(
                evidence["age_s"]
                    .as_u64()
                    .is_some_and(|age| (at_least..=at_least + 5).contains(&age)),
                "{id}: age_s about {at_least}: {finding}"
            );
        }
        _ => {}
    }
}

/// Each of the ten fixtures (and the stopping variant), on a fresh home: planted, found
/// alone by `doctor --only` with its row's severity, instance and evidence, found by a
/// plain `doctor`, then cleared by its remedy.
#[test]
fn every_concurrency_fixture_is_found_and_cleared() {
    let log = tlog!();
    assert_eq!(concurrency().len(), rows().len(), "one fixture per row");
    for (label, fixture) in cases() {
        let id = fixture.id();
        let log = log.case(&label);
        log.step("home");
        let home = home(&log);

        log.step("plant");
        fixture.corrupt(&home);
        log.json("planted", &plant::take_planted());
        log.json("journals", &plant::journals(&home));
        let tx = planted_tx(&label, &home);
        let env = fixture.run_env();

        log.step("doctor --only");
        let ran = doctor(&log, &home, &["doctor", "--json", "--only", id], &env);
        ran.exit(1);
        let found = findings(ran.envelope());
        log.json("finding", &found);
        assert_eq!(found.len(), 1, "{label}: exactly one finding: {found:?}");
        check_finding(&label, id, found[0], &home, tx.as_deref());

        log.step("doctor");
        let ran = doctor(&log, &home, &["doctor", "--json"], &env);
        ran.exit(1);
        assert!(
            findings(ran.envelope()).iter().any(|f| f["id"] == id),
            "{label}: not found by a plain doctor"
        );

        log.step("remedy");
        fixture.remedy(&home);
        log.json("remedied", &plant::take_planted());

        log.step("healthy");
        fixture.assert_healthy(&home);
    }
}

/// No row fires on a home whose every change completed: the realistic home (30 installs
/// over three clients), plus a whole `WTF/` restore on retail and its undo, which leave
/// `.wtf` snapshots behind for undo. Nor while a young lock is held.
#[test]
fn no_concurrency_detector_fires_on_a_clean_home() {
    let log = tlog!();
    log.step("home");
    let dir = tempfile::tempdir().unwrap();
    let realistic = realistic_home(&dir.path().join("home"));
    let home = &realistic.home;
    log.env(home);

    log.step("a completed restore, and its undo");
    let wtf = Wtf::at(home.client("_retail_"))
        .saved_variables("Foo", 64)
        .build();
    let made = log.run(home, &["backup", "create", "-t", TARGET, "--json"]);
    made.ok();
    let id = made.data()["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(wtf.join("Config.wtf"), "SET edited \"after the backup\"\n").unwrap();
    log.run(home, &["backup", "restore", &id, "-t", TARGET, "--json"])
        .ok();
    log.run(home, &["undo", "--json"]).ok();
    assert_eq!(plant::journals(home), [], "every change completed");

    let only = rows().join(",");
    log.step("doctor --only, the lock free");
    let ran = doctor(&log, home, &["doctor", "--json", "--only", &only], &[]);
    ran.ok();
    assert_eq!(findings(ran.envelope()), Vec::<&Value>::new());

    log.step("doctor --only, a young lock held");
    let (held, _) = plant::hold_lock(home, "update");
    let ran = doctor(&log, home, &["doctor", "--json", "--only", &only], &[]);
    ran.ok();
    assert_eq!(findings(ran.envelope()), Vec::<&Value>::new());
    drop(held);
}

/// `fixtures::all()` holds exactly one fixture per concurrency row, in id order; every
/// such row is a cached check, and none has a fixer yet.
#[test]
fn the_concurrency_fixtures_are_registered() {
    let log = tlog!();
    let rows = rows();
    let fixtures = concurrency();
    let ids: Vec<&str> = fixtures.iter().map(|fixture| fixture.id()).collect();
    log.json("rows", &rows);
    log.json("fixtures", &ids);
    assert_eq!(rows.len(), 10, "Appendix B rows 1-10");
    assert_eq!(ids, rows, "one fixture per row, in id order");
    for row in FAILURE_MODES
        .iter()
        .filter(|row| row.id.starts_with(PREFIX))
    {
        assert_eq!(row.tier, Tier::Cached, "{}", row.id);
    }
    // Rows 1 and 9 delegate (F2); a fixture declares its row's fixer exactly.
    for fixture in &fixtures {
        let row = FAILURE_MODES.iter().find(|r| r.id == fixture.id()).unwrap();
        assert_eq!(fixture.fixer(), row.fix.is_some(), "{}", fixture.id());
    }
}

// ---------------------------------------------------------------------------------------
// Both surfaces (part 4): `status` and `doctor` name each state alike.

/// One row's report on one surface: its id, scope (a client's key, or `machine`),
/// severity and title.
type Reported = (String, String, String, String);

/// The client folders of [`home`].
const CLIENTS: [&str; 2] = ["_retail_", "_classic_"];

/// A finding or issue of the ten rows.
fn ours(value: &Value) -> bool {
    value["id"]
        .as_str()
        .is_some_and(|id| id.starts_with(PREFIX))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// The findings of the ten rows in a doctor envelope.
fn our_findings(envelope: &Value) -> Vec<&Value> {
    findings(envelope).into_iter().filter(|f| ours(f)).collect()
}

/// What `doctor` reports of the ten rows.
fn doctor_reported(envelope: &Value) -> BTreeSet<Reported> {
    our_findings(envelope)
        .into_iter()
        .map(|f| {
            let scope = f["target"]["key"].as_str().unwrap_or("machine");
            (
                text(&f["id"]),
                scope.to_owned(),
                text(&f["severity"]),
                text(&f["title"]),
            )
        })
        .collect()
}

/// `status`'s issues of the ten rows, each with its scope: a client's key, or `machine`
/// for `data.issues`.
fn status_issues(envelope: &Value) -> Vec<(String, &Value)> {
    let data = &envelope["data"];
    let mut issues = Vec::new();
    for target in data["targets"].as_array().into_iter().flatten() {
        let scope = text(&target["key"]);
        for issue in target["issues"].as_array().into_iter().flatten() {
            if ours(issue) {
                issues.push((scope.clone(), issue));
            }
        }
    }
    for issue in data["issues"].as_array().into_iter().flatten() {
        if ours(issue) {
            issues.push(("machine".to_owned(), issue));
        }
    }
    issues
}

/// What `status` reports of the ten rows.
fn status_reported(envelope: &Value) -> BTreeSet<Reported> {
    status_issues(envelope)
        .into_iter()
        .map(|(scope, issue)| {
            (
                text(&issue["id"]),
                scope,
                text(&issue["severity"]),
                text(&issue["title"]),
            )
        })
        .collect()
}

/// The ids of the clients whose `attention` holds `journal_pending`.
fn journal_attention(envelope: &Value) -> Vec<String> {
    envelope["data"]["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| {
            t["attention"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a == "journal_pending")
        })
        .map(|t| text(&t["id"]))
        .collect()
}

/// Records the trees a reader must leave as they were: `<state>` (bar `doctor/`, checked
/// by [`assert_trees`]), the config, the cache and each client dir.
fn record_trees(log: &TestLog, home: &Home) {
    log.step("trees");
    log.tree("state", &home.state());
    log.tree("config", &home.env.config());
    log.tree("cache", &home.env.cache());
    for client in CLIENTS {
        log.tree(client, &home.client(client));
    }
}

/// The paths `diff` added, removed or changed.
fn diffed(diff: &common::trees::TreeDiff) -> Vec<&String> {
    diff.added
        .iter()
        .chain(&diff.removed)
        .chain(diff.changed.iter().map(|(path, _)| path))
        .collect()
}

/// The discovery cache, below `<cache>`: plain `status`'s scan refreshes it when a
/// client changed since it was written (it is what `status --cached` reads); the doctor
/// never writes it.
const DISCOVERY_CACHE: &str = "omaddons/discovery.json";

/// Nothing but `<state>/doctor/` changed since [`record_trees`], and, once plain `status`
/// ran (`status_ran`), the discovery cache it refreshes.
fn assert_trees(log: &TestLog, home: &Home, status_ran: bool) {
    log.step("check trees");
    let diff = log.diff("state", &home.state());
    let outside: Vec<&String> = diffed(&diff)
        .into_iter()
        .filter(|path| !path.starts_with("doctor"))
        .collect();
    assert!(outside.is_empty(), "changed outside doctor/: {outside:?}");
    let diff = log.diff("cache", &home.env.cache());
    // On a home where nothing ran yet, the refresh also creates `<cache>/omaddons/`,
    // holding that file alone.
    let only_discovery = || {
        let dir = home.env.cache().join("omaddons");
        let names: Vec<String> = std::fs::read_dir(dir)
            .map(|listing| {
                listing
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names == ["discovery.json"]
    };
    let cache: Vec<&String> = diffed(&diff)
        .into_iter()
        .filter(|path| {
            !(status_ran
                && (path.as_str() == DISCOVERY_CACHE
                    || (path.as_str() == "omaddons" && only_discovery())))
        })
        .collect();
    assert!(cache.is_empty(), "the cache changed: {cache:?}");
    log.assert_unchanged("config", &home.env.config());
    for client in CLIENTS {
        log.assert_unchanged(client, &home.client(client));
    }
}

/// Runs `argv`, a remediation an output offers for `finding`: a `doctor explain` as
/// [`explain_holds`]; a `doctor fix` (the delegation's, F2) as its preview, which must
/// disclose the recovery of the finding's transaction and nothing else.
fn remediation_holds(
    log: &TestLog,
    home: &Home,
    argv: &[Value],
    env: &[(String, String)],
    finding: &Value,
) {
    let words: Vec<&str> = argv[1..]
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    if words[..2] != ["doctor", "fix"] {
        return explain_holds(log, home, argv, env, &finding["instance"]);
    }
    let forced = words.contains(&"--force");
    let mut args = words;
    args.extend(["--dry-run", "--json"]);
    let ran = doctor(log, home, &args, env);
    assert!(
        matches!(ran.run.code, 0 | 1),
        "{argv:?}: exit {}",
        ran.run.code
    );
    if forced {
        // A set-aside (F2): renames only, the journal's own last, nothing delegated.
        let actions = ran.envelope()["actions_planned"]
            .as_array()
            .unwrap()
            .clone();
        assert!(!actions.is_empty(), "{argv:?}: a set-aside moves its set");
        assert!(
            actions.iter().all(|a| a["op"] == "rename_noreplace"),
            "{argv:?}: {actions:?}"
        );
        let last = text(&actions.last().unwrap()["path"]);
        assert!(
            last.contains(&format!("/journal/{}", text(&finding["evidence"]["tx"]))),
            "{argv:?}: the journal moves last: {last}"
        );
        assert_eq!(ran.envelope()["delegated_planned"], json!([]), "{argv:?}");
        return;
    }
    let planned = &ran.envelope()["delegated_planned"];
    let txs: Vec<&Value> = planned
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["tx"])
        .collect();
    assert_eq!(txs, [&finding["evidence"]["tx"]], "{argv:?}: {planned}");
    assert_eq!(
        ran.envelope()["actions_planned"],
        json!([]),
        "{argv:?}: a delegation runs no primitive"
    );
}

/// Runs `argv` (a `doctor explain` an output offers) with `--json`: exit 0, and its
/// `instances` hold `instance`.
fn explain_holds(
    log: &TestLog,
    home: &Home,
    argv: &[Value],
    env: &[(String, String)],
    instance: &Value,
) {
    let mut args: Vec<&str> = argv[1..]
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    assert_eq!(args[..2], ["doctor", "explain"], "{argv:?}");
    args.push("--json");
    let ran = doctor(log, home, &args, env);
    ran.ok();
    let instances: Vec<&Value> = ran.envelope()["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| &f["instance"])
        .collect();
    assert!(
        instances.contains(&instance),
        "{argv:?}: {instance} not in {instances:?}"
    );
}

/// Removes from `value` every key `contract::VOLATILE` declares (`*`: every key or
/// element; `**`: any depth).
fn strip_volatile(value: &mut Value) {
    for (pattern, _) in VOLATILE {
        let path: Vec<&str> = pattern.split('.').collect();
        strip(value, &path);
    }
}

fn strip(value: &mut Value, path: &[&str]) {
    match path {
        [] => {}
        ["*"] => match value {
            Value::Object(map) => map.clear(),
            Value::Array(items) => items.clear(),
            _ => {}
        },
        ["**", rest @ ..] => {
            strip(value, rest);
            strip_each(value, path);
        }
        ["*", rest @ ..] => strip_each(value, rest),
        [key] => {
            if let Some(map) = value.as_object_mut() {
                map.remove(*key);
            }
        }
        [key, rest @ ..] => {
            if let Some(child) = value.get_mut(*key) {
                strip(child, rest);
            }
        }
    }
}

/// [`strip`] of `path` on each child of `value`.
fn strip_each(value: &mut Value, path: &[&str]) {
    match value {
        Value::Object(map) => map.values_mut().for_each(|child| strip(child, path)),
        Value::Array(items) => items.iter_mut().for_each(|child| strip(child, path)),
        _ => {}
    }
}

/// Each state, on a fresh home: `doctor`, `status` and `status --cached` report it under
/// the same id, scope, severity and title; attention, `--check` and `summary.state` follow
/// the row; every remediation and explain argv runs; and nothing outside
/// `<state>/doctor/` changes.
#[test]
fn status_and_doctor_name_each_state_alike() {
    let log = tlog!();
    for (label, fixture) in cases() {
        let id = fixture.id();
        let short = id.strip_prefix(PREFIX).unwrap();
        let log = log.case(&label);
        log.step("home");
        let home = home(&log);
        log.step("plant");
        fixture.corrupt(&home);
        log.json("planted", &plant::take_planted());
        let env = fixture.run_env();
        record_trees(&log, &home);

        log.step("doctor");
        let ran = doctor(&log, &home, &["doctor", "--json"], &env);
        ran.exit(1);
        let doctor_envelope = ran.envelope().clone();
        let from_doctor = doctor_reported(&doctor_envelope);
        log.json("doctor", &from_doctor);
        assert_trees(&log, &home, false);

        log.step("status");
        let ran = run(&log, &home, &["status", "--json"], &env);
        ran.ok();
        let status_envelope = ran.envelope().clone();
        let from_status = status_reported(&status_envelope);
        log.json("status", &from_status);

        log.step("status --cached");
        let ran = run(&log, &home, &["status", "--cached", "--json"], &env);
        ran.ok();
        let from_cached = status_reported(ran.envelope());
        log.json("status --cached", &from_cached);

        assert_eq!(from_doctor.len(), 1, "{label}: {from_doctor:?}");
        assert!(from_doctor.iter().all(|(found, ..)| found == id), "{label}");
        assert_eq!(from_status, from_doctor, "{label}: status against doctor");
        assert_eq!(
            from_cached, from_doctor,
            "{label}: status --cached against doctor"
        );

        log.step("attention");
        let attention = journal_attention(&status_envelope);
        let data = &status_envelope["data"];
        match short {
            "lock-held-long" => assert_eq!(attention, Vec::<String>::new(), "{label}"),
            "journal-unattributed" => {
                assert_eq!(attention, Vec::<String>::new(), "{label}");
                assert_eq!(data["needs_attention"], true, "{label}");
            }
            _ => assert_eq!(attention, [TARGET], "{label}"),
        }
        let check = run(&log, &home, &["status", "--check", "--json"], &env);
        if short == "lock-held-long" {
            check.ok();
        } else {
            check.fails(6, "check_failed");
            let error = &check.envelope()["error"];
            let issues = status_issues(&status_envelope);
            let remediation = &issues[0].1["remediation"];
            if short == "journal-pending" {
                // A journal recovery settles keeps today's message and fix, byte for byte.
                assert_eq!(
                    (&error["message"], &error["fix"], &error["fix_argv"]),
                    (
                        &json!("an interrupted transaction for retail awaits recovery"),
                        &json!(
                            "run any omaddons command that changes addons (install, remove, undo); it recovers the transaction first"
                        ),
                        &Value::Null,
                    ),
                    "{label}: {error}"
                );
            } else if remediation["argv"].is_null() {
                assert_eq!(error["fix"], remediation["command"], "{label}: {error}");
                assert!(error["fix_argv"].is_null(), "{label}: {error}");
            } else if remediation["argv"]
                .as_array()
                .is_some_and(|argv| argv.contains(&json!("--force")))
            {
                // A forced set-aside is read before it is consented to: `--check`'s fix is
                // its `doctor explain`, never an argv with `--force --yes`.
                let explain = &error["fix_argv"];
                assert_eq!(&explain[1], "doctor", "{label}: {error}");
                assert_eq!(&explain[2], "explain", "{label}: {error}");
                assert_eq!(explain[3], issues[0].1["id"], "{label}: {error}");
            } else {
                assert_eq!(error["fix_argv"], remediation["argv"], "{label}: {error}");
            }
        }

        log.step("summary");
        let severity = &from_doctor.iter().next().unwrap().2;
        let state = &data["summary"]["state"];
        if severity == "P0" || severity == "P1" {
            assert_eq!(state, "attention", "{label}");
        } else {
            assert_ne!(state, "attention", "{label}");
        }

        log.step("explain each remediation");
        let found = our_findings(&doctor_envelope);
        for (scope, issue) in status_issues(&status_envelope) {
            if let Some(argv) = issue["remediation"]["argv"].as_array() {
                let finding = found
                    .iter()
                    .find(|f| {
                        f["id"] == issue["id"]
                            && f["target"]["key"].as_str().unwrap_or("machine") == scope
                    })
                    .unwrap();
                remediation_holds(&log, &home, argv, &env, finding);
            }
        }
        for finding in &found {
            let remediation = &finding["remediation"];
            for argv in [&remediation["argv"], &remediation["explain_argv"]] {
                if let Some(argv) = argv.as_array() {
                    remediation_holds(&log, &home, argv, &env, finding);
                }
            }
        }
        assert_trees(&log, &home, true);
    }
}

/// A recoverable folder journal keeps the published id, title and remediation, byte for
/// byte, and recovering it leaves nothing of the ten rows.
#[test]
fn a_recoverable_journal_keeps_its_published_id_and_meaning() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    log.step("plant");
    JournalPending::default().corrupt(&home);
    log.json("planted", &plant::take_planted());

    log.step("status");
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    let issues = status_issues(ran.envelope());
    log.json("issues", &issues);
    assert_eq!(issues.len(), 1, "{issues:?}");
    let issue = issues[0].1;
    assert_eq!(issue["id"], "fm-concurrency_primitives-journal-pending");
    assert_eq!(issue["severity"], "P0");
    assert_eq!(issue["title"], "An interrupted change waits to be finished");
    // Its remediation is the doctor's delegation (F2, M4-D53), as `doctor` shows it.
    assert_eq!(
        issue["remediation"],
        json!({
            "command": "omaddons doctor fix --only fm-concurrency_primitives-journal-pending -t retail",
            "argv": [
                "omaddons", "doctor", "fix", "--only",
                "fm-concurrency_primitives-journal-pending", "-t", "retail",
            ],
            "destructive": false,
        })
    );

    log.step("doctor");
    let ran = doctor(&log, &home, &["doctor", "--json"], &[]);
    ran.exit(1);
    let found = our_findings(ran.envelope());
    assert_eq!(found.len(), 1, "{found:?}");
    let evidence = &found[0]["evidence"];
    assert_eq!(evidence["family"], "folder");
    assert_eq!(evidence["phase"], "prepared");
    assert!(evidence.get("reason").is_none(), "{evidence}");

    log.step("recover");
    plant::recover(&home);
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    assert_eq!(status_reported(ran.envelope()), BTreeSet::new());
    let ran = doctor(&log, &home, &["doctor", "--json"], &[]);
    assert_eq!(doctor_reported(ran.envelope()), BTreeSet::new());
}

/// Two too-new journals on retail are two instances, one plural `status` issue; once one
/// is gone, `doctor --since` reports it resolved and the other unchanged, and `status`
/// names the one left.
#[test]
fn two_stuck_journals_on_one_client_are_two_instances_and_since_tells_them_apart() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    log.step("plant");
    let (a, _) = plant::journal_too_new(&home, TARGET);
    let b = plant::copy_journal(&home, &a);
    log.json("planted", &plant::take_planted());
    let journals = plant::journals(&home);
    log.json("journals", &journals);
    assert_eq!(journals.len(), 2, "{journals:?}");
    let key = plant::target_key(&home, TARGET);
    let instance = |tx: &str| format!("{}@{key}:tx={tx}", ids::JOURNAL_TOO_NEW);

    log.step("run A");
    let ran = doctor(&log, &home, &["doctor", "--json"], &[]);
    ran.exit(1);
    let run_a = text(&ran.envelope()["run_id"]);
    let found = our_findings(ran.envelope());
    log.json("findings", &found);
    let instances: BTreeSet<String> = found.iter().map(|f| text(&f["instance"])).collect();
    assert_eq!(instances, BTreeSet::from([instance(&a), instance(&b)]));
    let labels: BTreeSet<String> = found.iter().map(|f| text(&f["label"])).collect();
    assert_eq!(labels.len(), 2, "{labels:?}");

    log.step("status");
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    let issues = status_issues(ran.envelope());
    log.json("issues", &issues);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(
        issues[0].1["title"],
        "2 interrupted changes were started by a newer omaddons"
    );

    log.step("settle one");
    let aside = plant::aside(&home);
    for (n, path) in plant::journal_set(&home, &a, Some(TARGET))
        .into_iter()
        .enumerate()
    {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::rename(&path, aside.join(format!("{n}-{name}"))).unwrap();
    }
    assert_eq!(plant::journals(&home).len(), 1);

    log.step("since");
    let ran = doctor(&log, &home, &["doctor", "--json", "--since", &run_a], &[]);
    ran.exit(1);
    let since = &ran.envelope()["since"];
    log.json("since", since);
    assert_eq!(
        since["resolved_instances"],
        json!([instance(&a)]),
        "{since}"
    );
    assert_eq!(since["new_instances"], json!([]), "{since}");
    assert_eq!(since["unchanged"], 1, "{since}");
    let left = our_findings(ran.envelope());
    assert_eq!(left.len(), 1, "{left:?}");
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    let issues = status_issues(ran.envelope());
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].1["title"], left[0]["title"], "the instance title");
}

/// `OMADDONS_NOW` set `after` past `taken`, as a run's environment.
fn now_after(taken: Timestamp, after: SignedDuration) -> Vec<(String, String)> {
    vec![(
        "OMADDONS_NOW".to_owned(),
        taken.checked_add(after).unwrap().to_string(),
    )]
}

/// While another process holds the lock, neither surface reports a journal; held past
/// `LOCK_HELD_LONG`, both report the holder instead; released, the journal is back.
#[test]
fn another_holder_hides_journals_until_it_releases() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    log.step("plant");
    JournalTooNew::default().corrupt(&home);
    log.json("planted", &plant::take_planted());
    let journal_ids = |reported: &BTreeSet<Reported>| -> Vec<String> {
        reported
            .iter()
            .filter(|(id, ..)| id.as_str() != ids::LOCK_HELD_LONG)
            .map(|(id, ..)| id.clone())
            .collect()
    };
    let lock_held_long = |reported: &BTreeSet<Reported>| -> bool {
        reported
            .iter()
            .any(|(id, scope, ..)| id == ids::LOCK_HELD_LONG && scope == "machine")
    };

    log.step("hold");
    let (held, taken) = plant::hold_lock(&home, "update");
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    let envelope = ran.envelope();
    assert_eq!(
        journal_ids(&status_reported(envelope)),
        Vec::<String>::new()
    );
    assert_eq!(envelope["data"]["journal"]["in_progress"], true);
    // A journal this build cannot read is never another process's work in progress: it
    // keeps its own warning (`tx_in_progress` is for one this build recovers).
    assert_eq!(ran.warnings(), ["journal_pending"]);
    assert!(
        text(&envelope["warnings"][0]["message"]).contains("(journal_too_new)"),
        "{envelope}"
    );
    let ran = doctor(&log, &home, &["doctor", "--json"], &[]);
    ran.ok();
    assert_eq!(doctor_reported(ran.envelope()), BTreeSet::new());

    log.step("hold long");
    let long = now_after(taken, SignedDuration::from_mins(11));
    let ran = run(&log, &home, &["status", "--json"], &long);
    ran.ok();
    let reported = status_reported(ran.envelope());
    log.json("status", &reported);
    assert!(lock_held_long(&reported), "{reported:?}");
    assert_eq!(journal_ids(&reported), Vec::<String>::new());
    let ran = doctor(&log, &home, &["doctor", "--json"], &long);
    ran.exit(1);
    let reported = doctor_reported(ran.envelope());
    log.json("doctor", &reported);
    assert!(lock_held_long(&reported), "{reported:?}");
    assert_eq!(journal_ids(&reported), Vec::<String>::new());

    log.step("release");
    drop(held);
    for reported in [
        status_reported(run(&log, &home, &["status", "--json"], &long).envelope()),
        doctor_reported(doctor(&log, &home, &["doctor", "--json"], &long).envelope()),
    ] {
        log.json("reported", &reported);
        assert!(!lock_held_long(&reported), "{reported:?}");
        assert_eq!(journal_ids(&reported), [ids::JOURNAL_TOO_NEW]);
    }
}

/// The boundary of `LOCK_HELD_LONG`: unset and 9 min 59 s are no finding, 10 min 1 s is,
/// with its evidence, title and advice; frozen at one time two runs agree, at two times
/// they differ only in `age_s`; and no read touches the lock file.
#[test]
fn a_young_lock_is_no_finding_and_an_old_one_is() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    let (held, taken) = plant::hold_lock(&home, "update");
    log.tree("state", &home.state());
    let pid = std::process::id();
    let only = ["doctor", "--json", "--only", ids::LOCK_HELD_LONG];
    let cases: [(&str, Option<SignedDuration>, bool); 3] = [
        ("unset", None, false),
        ("9 min 59 s", Some(SignedDuration::from_secs(599)), false),
        ("10 min 1 s", Some(SignedDuration::from_secs(601)), true),
    ];
    for (name, after, found) in cases {
        let log = log.case(name);
        log.step("doctor");
        let env = after.map(|a| now_after(taken, a)).unwrap_or_default();
        let ran = doctor(&log, &home, &only, &env);
        let lock = findings(ran.envelope());
        log.json("findings", &lock);
        if !found {
            ran.ok();
            assert_eq!(lock.len(), 0, "{name}: {lock:?}");
            continue;
        }
        ran.exit(1);
        assert_eq!(lock.len(), 1, "{name}: {lock:?}");
        let finding = lock[0];
        assert_eq!(
            finding["evidence"],
            json!({"pid": pid, "command": "update", "age_s": 601}),
            "{finding}"
        );
        assert_eq!(finding["confidence"], "medium");
        assert_eq!(
            finding["title"],
            format!(
                "An omaddons process (pid {pid}, update) has held the lock for more than 10 minutes"
            )
        );
        let remediation = &finding["remediation"];
        assert!(remediation["argv"].is_null(), "advice: {remediation}");
        assert!(
            text(&remediation["command"]).contains(&format!("kill {pid}")),
            "{remediation}"
        );
    }

    log.step("repeatable");
    assert!(
        VOLATILE
            .iter()
            .any(|(path, _)| *path == "findings.*.evidence.age_s")
    );
    let at = |secs: i64| now_after(taken, SignedDuration::from_secs(secs));
    let stable = |env: &[(String, String)]| {
        let mut envelope = doctor(&log, &home, &only, env).envelope().clone();
        strip_volatile(&mut envelope);
        envelope
    };
    assert_eq!(stable(&at(601)), stable(&at(601)), "one frozen time");
    let ages: Vec<Value> = [601, 900]
        .iter()
        .map(|secs| {
            let ran = doctor(&log, &home, &only, &at(*secs));
            findings(ran.envelope())[0]["evidence"]["age_s"].clone()
        })
        .collect();
    assert_eq!(ages, [json!(601), json!(900)]);
    assert_eq!(stable(&at(601)), stable(&at(900)), "only age_s differs");

    log.step("check trees");
    let diff = log.diff("state", &home.state());
    let outside: Vec<&String> = diff
        .added
        .iter()
        .chain(&diff.removed)
        .chain(diff.changed.iter().map(|(path, _)| path))
        .filter(|path| !path.starts_with("doctor"))
        .collect();
    assert!(
        outside.is_empty(),
        "the lock file and state untouched: {outside:?}"
    );
    drop(held);
}

/// Each state diagnosed twice gives equal envelopes once the declared volatile keys are
/// stripped, `status` twice the same envelope (bar `meta`), and nothing outside
/// `<state>/doctor/` changes.
#[test]
fn the_detectors_are_repeatable_and_change_nothing() {
    let log = tlog!();
    for (label, fixture) in cases() {
        let log = log.case(&label);
        log.step("home");
        let home = home(&log);
        log.step("plant");
        fixture.corrupt(&home);
        log.json("planted", &plant::take_planted());
        let env = fixture.run_env();
        record_trees(&log, &home);

        log.step("doctor twice");
        let twice: Vec<Value> = (0..2)
            .map(|_| {
                let mut envelope = doctor(&log, &home, &["doctor", "--json"], &env)
                    .envelope()
                    .clone();
                strip_volatile(&mut envelope);
                envelope
            })
            .collect();
        assert_eq!(twice[0].to_string(), twice[1].to_string(), "{label}");
        assert_trees(&log, &home, false);

        log.step("status twice");
        let twice: Vec<Value> = (0..2)
            .map(|_| {
                let mut envelope = run(&log, &home, &["status", "--json"], &env)
                    .envelope()
                    .clone();
                strip_volatile(&mut envelope);
                envelope
            })
            .collect();
        assert_eq!(twice[0].to_string(), twice[1].to_string(), "{label}");
        assert_trees(&log, &home, true);
    }
}

/// The `WTF/` fixtures' outputs name no account, realm or character: `status` and
/// `doctor`, JSON and human (`-v`), both streams with `--progress json`, and the run's
/// `report.json`; and every evidence path is the state's or a client dir.
#[test]
fn journal_findings_name_no_account_realm_or_character() {
    let log = tlog!();
    let wtf: [(&str, Box<dyn Fixture>); 4] = [
        (
            "journal-waits-for-game",
            Box::new(JournalWaitsForGame::default()),
        ),
        (
            "journal-unexpected-state",
            Box::new(JournalUnexpectedState::default()),
        ),
        (
            "journal-preservation-incomplete",
            Box::new(JournalPreservationIncomplete::default()),
        ),
        (
            "journal-preservation-incomplete (stopping)",
            Box::new(JournalPreservationStops::default()),
        ),
    ];
    for (label, fixture) in wtf {
        let log = log.case(label);
        log.step("home");
        let home = home(&log);
        log.step("plant");
        fixture.corrupt(&home);
        log.json("planted", &plant::take_planted());
        let env = fixture.run_env();

        log.step("outputs");
        for args in [
            &["status", "--json", "--progress", "json"][..],
            &["status", "--progress", "json"],
            &["doctor", "-v", "--progress", "json"],
        ] {
            run(&log, &home, args, &env).assert_private();
        }
        let ran = doctor(
            &log,
            &home,
            &["doctor", "--json", "--progress", "json"],
            &env,
        );
        ran.exit(1).assert_private();
        let envelope = ran.envelope();
        let report = Path::new(envelope["run_dir"].as_str().unwrap()).join("report.json");
        assert_private(&std::fs::read_to_string(report).unwrap());

        log.step("evidence paths");
        let clients: Vec<String> = CLIENTS
            .iter()
            .map(|c| home.client(c).display().to_string())
            .collect();
        let state = home.state().display().to_string();
        for finding in our_findings(envelope) {
            for key in ["journal", "path"] {
                if let Some(path) = finding["evidence"][key].as_str() {
                    assert!(
                        path.starts_with(&state) || clients.iter().any(|c| c == path),
                        "{label}: evidence {key} {path}"
                    );
                }
            }
        }
    }
}

/// Human `status` prints each stuck journal's title and its `doctor explain` under its
/// client's row: a too-new journal on retail, an unrecognized step on classic (whose
/// install proceeds: retail's journal only warns another client).
#[test]
fn status_prints_each_stuck_title_and_its_command() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    log.step("plant");
    plant::journal_too_new(&home, "retail");
    plant::journal_step_unrecognized(&home, "classic");
    log.json("planted", &plant::take_planted());

    log.step("status --json");
    let ran = run(&log, &home, &["status", "--json"], &[]);
    ran.ok();
    let mut expected: Vec<(String, String)> = Vec::new();
    for target in ran.envelope()["data"]["targets"].as_array().unwrap() {
        let client = text(&target["id"]);
        for issue in target["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| ours(i))
        {
            // Each stuck journal's command is its forced set-aside (F2).
            let line = format!(
                "  P0 {} → omaddons doctor fix --only {} -t {client} --force --yes",
                text(&issue["title"]),
                text(&issue["id"])
            );
            expected.push((client.clone(), line));
        }
    }
    log.json("expected", &expected);
    let clients: BTreeSet<&str> = expected.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        clients,
        BTreeSet::from(["classic", "retail"]),
        "{expected:?}"
    );

    log.step("status");
    let ran = run(&log, &home, &["status"], &[]);
    ran.ok();
    let lines: Vec<&str> = ran.stdout().lines().collect();
    for (client, line) in &expected {
        let at = lines.iter().position(|l| l == line);
        assert!(at.is_some(), "no `{line}` in:\n{}", ran.stdout());
        let row = lines[..at.unwrap()]
            .iter()
            .rev()
            .find(|l| !l.starts_with(' '))
            .unwrap();
        assert!(
            row.starts_with(client.as_str()),
            "`{line}` is under `{row}`, not {client}'s row"
        );
    }
}

/// A child process of the repair scenes (`common::repair::child_main`). Run on its own it
/// does nothing.
#[cfg(debug_assertions)]
#[test]
#[ignore = "a child process of the repair scenes"]
fn child_repair() {
    common::repair::child_main();
}

/// E5's crashed repair (a chmod, killed after its step ran): `doctor` reports it as
/// `journal-pending` with `family: repair`, and `status` lists it at the same scope,
/// severity and title.
#[cfg(debug_assertions)]
#[test]
fn a_crashed_repair_is_journal_pending_with_family_repair() {
    use common::repair::{Child, Kind, RepairScene};

    let log = tlog!();
    log.step("crash a repair");
    let r = RepairScene::new(Kind::Chmod);
    let spec = format!("fsx.mutate.executed=abort@{}", Kind::Chmod.primitives());
    assert_eq!(r.child(&log, &spec), Child::Crashed);
    let home = &r.s.home;
    log.json("journal", &r.journal_lines());

    log.step("doctor");
    let ran = doctor(&log, home, &["doctor", "--json"], &[]);
    ran.exit(1);
    let found = our_findings(ran.envelope());
    log.json("findings", &found);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["id"], ids::JOURNAL_PENDING);
    assert_eq!(found[0]["evidence"]["family"], "repair");
    assert_eq!(found[0]["evidence"]["tx"], r.tx().unwrap());
    let from_doctor = doctor_reported(ran.envelope());

    log.step("status");
    let ran = run(&log, home, &["status", "--json"], &[]);
    ran.ok();
    let from_status = status_reported(ran.envelope());
    log.json("status", &from_status);
    assert_eq!(from_status, from_doctor);
}
