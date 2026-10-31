//! e2e suite: the journal and lock detectors, and status's journal titles
#![cfg(test)]

mod common;
#[path = "doctor/mod.rs"]
#[allow(dead_code)]
mod fixture;

use serde_json::{Value, json};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::realistic::realistic_home;
use common::tlog::{Ran, TestLog, tlog};
use common::wtf::Wtf;
use fixture::Fixture;
use fixture::fixtures::{self, fm_concurrency_primitives_journal_marker_mismatch::OTHER};
use omaddons::doctor::registry::{FAILURE_MODES, Tier};
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

/// `omaddons <args>` (pass `--json`) with `env` and this home's own Omarchy, checked
/// against the doctor's envelope contract.
fn doctor(log: &TestLog, home: &Home, args: &[&str], env: &[(String, String)]) -> Ran {
    let mut cmd = log
        .cmd(home, args)
        .env("OMARCHY_PATH", plant::omarchy_path(home));
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    let ran = cmd.run();
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

/// The evidence keys and values the state each fixture plants must give (Technical
/// Approach §2), besides `tx`; `pid`, `age_s` and `count` are checked apart.
fn expected_evidence(id: &str, home: &Home) -> Vec<(&'static str, Value)> {
    match id.strip_prefix(PREFIX).unwrap() {
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
        "journal-preservation-incomplete" => vec![("resumable", json!(true))],
        "lock-held-long" => vec![("command", json!("update"))],
        other => panic!("no expected evidence for {other}"), // ubs:ignore — the test's own failure
    }
}

/// Checks the one finding a fixture's `doctor --only` gives: its id, severity, instance
/// and evidence. `tx` is the planted journal's transaction (`None` for the lock).
fn check_finding(id: &str, finding: &Value, home: &Home, tx: Option<&str>) {
    let row = omaddons::doctor::registry::row(id).unwrap();
    assert_eq!(finding["id"], id);
    assert_eq!(finding["severity"], format!("{:?}", row.severity), "{id}");
    let evidence = &finding["evidence"];
    let instance = match (id.strip_prefix(PREFIX).unwrap(), tx) {
        ("journal-unattributed", Some(tx)) => format!("{id}@machine:tx={tx}"),
        (_, Some(tx)) => format!("{id}@{}:tx={tx}", plant::target_key(home, TARGET)),
        (_, None) => format!("{id}@machine"),
    };
    assert_eq!(finding["instance"], instance, "{id}: {finding}");
    if let Some(tx) = tx {
        assert_eq!(evidence["tx"], tx, "{id}: {finding}");
    }
    for (key, want) in expected_evidence(id, home) {
        assert_eq!(evidence[key], want, "{id}: evidence {key}: {finding}");
    }
    match id.strip_prefix(PREFIX).unwrap() {
        "journal-preservation-incomplete" => assert!(
            evidence["count"].as_u64().is_some_and(|n| n >= 1),
            "{id}: count: {finding}"
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

/// Each of the ten fixtures, on a fresh home: planted, found alone by `doctor --only` with
/// its row's severity, instance and evidence, found by a plain `doctor`, then cleared by
/// its remedy.
#[test]
fn every_concurrency_fixture_is_found_and_cleared() {
    let log = tlog!();
    let all = concurrency();
    assert_eq!(all.len(), rows().len(), "one fixture per row");
    for fixture in all {
        let id = fixture.id();
        let log = log.case(id);
        log.step("home");
        let home = home(&log);

        log.step("plant");
        fixture.corrupt(&home);
        log.json("planted", &plant::take_planted());
        let journals = plant::journals(&home);
        log.json("journals", &journals);
        let tx = if id.ends_with("lock-held-long") {
            assert!(journals.is_empty(), "{id}: {journals:?}");
            None
        } else {
            assert_eq!(journals.len(), 1, "{id}: one journal: {journals:?}");
            Some(journals[0].0.clone())
        };
        let env = fixture.run_env();

        log.step("doctor --only");
        let ran = doctor(&log, &home, &["doctor", "--json", "--only", id], &env);
        ran.exit(1);
        let found = findings(ran.envelope());
        log.json("finding", &found);
        assert_eq!(found.len(), 1, "{id}: exactly one finding: {found:?}");
        check_finding(id, found[0], &home, tx.as_deref());

        log.step("doctor");
        let ran = doctor(&log, &home, &["doctor", "--json"], &env);
        ran.exit(1);
        assert!(
            findings(ran.envelope()).iter().any(|f| f["id"] == id),
            "{id} not found by a plain doctor"
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
    for fixture in &fixtures {
        assert!(!fixture.fixer(), "{} has no fixer yet", fixture.id());
    }
}
