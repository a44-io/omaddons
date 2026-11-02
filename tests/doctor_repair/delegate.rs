//! F2 s4 (omad-m4-agent-surface-doctor-c6cm.6.2.4): the delegation (M4-D37) end to end,
//! through the binary. `doctor fix` has the engine's own recovery settle exactly the
//! journals its preview disclosed: rolled back, rolled forward, a repair's, a restore's
//! once its game exits, a preservation once its cause is gone; a bound run is stale when
//! the journals changed; two fixers serialize; a failed recovery makes the run partial;
//! `doctor undo` never reverts a delegation. Every command runs with `--json --progress
//! json` through `checked` (the doctor envelope contract and `assert_private_all`).

use super::*;
use crate::common::archives::zip_text;
use crate::common::engine::{BAR, V1, V2};
use crate::common::privacy::ACCOUNT;

const PENDING: &str = "fm-concurrency_primitives-journal-pending";
const PRESERVATION: &str = "fm-concurrency_primitives-journal-preservation-incomplete";
const WAITS: &str = "fm-concurrency_primitives-journal-waits-for-game";

// ---------------------------------------------------------------------------------------
// Planting.

/// A home with a retail client, its SavedVariables under private names, and the fixture
/// archives `v1`, `v2` and `bar` in `<root>/dl`.
fn scene(log: &TestLog) -> Home {
    let home = home(log);
    for dir in [ACCOUNT, CHARACTER] {
        let dir = home.client("_retail_").join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Foo.lua"), "FooDB = { seen = true }\n").unwrap();
    }
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    for (name, entries) in [("v1", V1), ("v2", V2), ("bar", BAR)] {
        fs::write(dl.join(format!("{name}.zip")), zip_text(entries)).unwrap();
    }
    home
}

/// `install <r> --file <root>/dl/<zip>.zip -t retail --offline`.
fn install_args(home: &Home, r: &str, zip: &str) -> Vec<String> {
    let zip = home.env.root().join(format!("dl/{zip}.zip"));
    let zip = zip.display().to_string();
    ["install", r, "--file", &zip, "-t", "retail", "--offline"]
        .map(str::to_owned)
        .to_vec()
}

fn words(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

/// `args` killed at `point` (an abort): the one journal it left.
fn crash(log: &TestLog, home: &Home, args: &[String], point: &str) -> String {
    let before = plant::journals(home);
    cmd(log, home, &words(args))
        .failpoints(&format!("{point}=abort"))
        .run()
        .aborted();
    let new: Vec<String> = plant::journals(home)
        .into_iter()
        .filter(|entry| !before.contains(entry))
        .map(|(tx, _)| tx)
        .collect();
    assert_eq!(
        new.len(),
        1,
        "{args:?} at {point}: one new journal: {new:?}"
    );
    let tx = new.into_iter().next().unwrap();
    log.note(format!(
        "journal {tx}: {}",
        fs::read_to_string(plant::journal_file(home, &tx)).unwrap_or_default()
    ));
    tx
}

/// `foo` at v1, the AddOns tree recorded as `addons`, then an install of `bar` killed
/// after its exchange (`engine.swap.after_exchange`): a prepared journal with a step,
/// which recovery rolls back.
fn interrupted_install(log: &TestLog, home: &Home) -> String {
    log.step("plant");
    run(log, home, &words(&install_args(home, "gh:o/foo", "v1"))).ok();
    log.tree("addons", &home.addons("_retail_"));
    crash(
        log,
        home,
        &install_args(home, "gh:o/bar", "bar"),
        "engine.swap.after_exchange",
    )
}

/// `foo` at v1, the AddOns tree recorded as `addons`, then its update to v2 killed after
/// its record was written (`engine.record.written`): a swapped journal, which recovery
/// rolls forward.
fn swapped_update(log: &TestLog, home: &Home) -> String {
    log.step("plant");
    run(log, home, &words(&install_args(home, "gh:o/foo", "v1"))).ok();
    log.tree("addons", &home.addons("_retail_"));
    crash(
        log,
        home,
        &install_args(home, "gh:o/foo", "v2"),
        "engine.record.written",
    )
}

// ---------------------------------------------------------------------------------------
// Reading.

fn delegated(ran: &Ran) -> Vec<Value> {
    ran.envelope()["delegated"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn warnings_of(ran: &Ran, code: &str) -> Vec<Value> {
    ran.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == code)
        .cloned()
        .collect()
}

/// The findings of `fm` in a doctor envelope.
fn findings_of(ran: &Ran, fm: &str) -> Vec<Value> {
    ran.envelope()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"] == fm)
        .cloned()
        .collect()
}

/// History's kinds, newest first.
fn history_kinds(log: &TestLog, home: &Home) -> Vec<String> {
    run(log, home, &["history"]).ok().data()["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["kind"].as_str().map(str::to_owned))
        .collect()
}

fn journal_left(home: &Home, tx: &str) -> bool {
    plant::journals(home).iter().any(|(t, _)| t == tx)
}

/// `doctor fix`'s `--json` envelope validates, strictly, against its published schemas:
/// the report against `robot-docs schema "doctor fix"`, the frame, each stderr event.
fn schema_valid(log: &TestLog, ran: &Ran) {
    let general = crate::common::doctor::as_general(ran.envelope());
    let validated = super::validate::validate_envelope(&general, ran.stderr());
    if let Err(failures) = &validated {
        log.json("envelope", ran.envelope());
        log.note(failures.clone());
    }
    assert!(validated.is_ok(), "{}", validated.err().unwrap_or_default());
}

/// The delegated outcome `[{fm, tx, family, outcome}]` with no reason.
fn settled(fm: &str, tx: &str, family: &str, outcome: &str) -> Value {
    json!([{"fm": fm, "tx": tx, "family": family, "outcome": outcome}])
}

// ---------------------------------------------------------------------------------------
// 1-3. Rolled back, rolled forward, part of a default fix.

/// An interrupted install, its `doctor` finding and `status` issue, its preview, and its
/// rollback through the delegation: the client as before the install, no repair line.
/// The home after it, with the transaction.
fn rolled_back(log: &TestLog) -> (Home, String) {
    let home = scene(log);
    let tx = interrupted_install(log, &home);
    let remedy: Value = json!([
        "omaddons", "doctor", "fix", "--only", PENDING, "-t", "retail"
    ]);

    log.step("diagnose");
    let diagnosed = run(log, &home, &["doctor"]);
    diagnosed.exit(1);
    let found = findings_of(&diagnosed, PENDING);
    log.json("finding", &found);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        text(&found[0]["instance"]).ends_with(&format!(":tx={tx}")),
        "{}",
        found[0]["instance"]
    );
    log.assert_eq("remediation", &found[0]["remediation"]["argv"], &remedy);
    log.assert_eq(
        "auto-fixable",
        &found[0]["remediation"]["auto_fixable"],
        &json!(true),
    );
    let status = run(log, &home, &["status"]);
    let issue = status.ok().data()["targets"][0]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == PENDING)
        .cloned()
        .unwrap();
    log.assert_eq(
        "status's remediation",
        &issue["remediation"]["argv"],
        &remedy,
    );

    log.step("preview");
    for (label, root) in [
        ("state", home.env.state()),
        ("cache", home.env.cache()),
        ("client", home.client("_retail_")),
    ] {
        log.tree(label, &root);
    }
    let preview = run(log, &home, &["doctor", "fix", "--dry-run"]);
    let report = preview.envelope();
    log.json("delegated_planned", &report["delegated_planned"]);
    log.assert_eq(
        "delegated_planned",
        &report["delegated_planned"],
        &json!([{"tx": tx, "family": "folder", "phase": "prepared", "direction": "roll_back"}]),
    );
    log.assert_eq("actions_planned", &report["actions_planned"], &json!([]));
    assert!(
        text(&report["precondition"]).starts_with("pc1:"),
        "{report}"
    );
    log.assert_eq(
        "the dry run moved nothing in <state>",
        &state_changed_outside_doctor(log, &home),
        &Vec::<String>::new(),
    );
    log.assert_unchanged("cache", &home.env.cache());
    log.assert_unchanged("client", &home.client("_retail_"));

    log.step("fix");
    let fixed = run(log, &home, &["doctor", "fix"]);
    fixed.ok();
    schema_valid(log, &fixed);
    let report = fixed.envelope();
    log.json("delegated", &report["delegated"]);
    log.json("warnings", &report["warnings"]);
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    log.assert_eq(
        "delegated",
        &report["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_back"),
    );
    log.assert_eq("no repair transaction", &report["tx"], &Value::Null);
    let delegated_warnings = warnings_of(&fixed, "delegated_recovery");
    assert_eq!(delegated_warnings.len(), 1, "{delegated_warnings:?}");
    assert!(text(&delegated_warnings[0]["message"]).contains(&tx));
    log.assert_eq(
        "its fix",
        &delegated_warnings[0]["fix_argv"],
        &json!(["omaddons", "history", "--limit", "5"]),
    );
    assert!(
        warnings_of(&fixed, "journal_recovered")
            .iter()
            .all(|w| !text(&w["message"]).contains(&tx)),
        "one event, one warning"
    );

    log.step("check");
    assert!(!journal_left(&home, &tx), "no journal");
    log.assert_unchanged("addons", &home.addons("_retail_"));
    let kinds = history_kinds(log, &home);
    assert!(!kinds.iter().any(|k| k == "repair"), "{kinds:?}");
    run(log, &home, &["doctor"]).ok();
    (home, tx)
}

#[test]
fn delegate_rolls_back_an_interrupted_install() {
    let log = tlog!();
    rolled_back(&log);
}

/// A swapped update, rolled forward by the delegation, whose warning's fix (`omaddons
/// undo <tx>`) then puts v1 back. The home before that undo, with the transaction and
/// the fix's run.
fn rolled_forward(log: &TestLog) -> (Home, String, String) {
    let home = scene(log);
    let tx = swapped_update(log, &home);
    log.step("preview");
    let preview = run(log, &home, &["doctor", "fix", "--dry-run"]);
    log.assert_eq(
        "delegated_planned",
        &preview.envelope()["delegated_planned"],
        &json!([{"tx": tx, "family": "folder", "phase": "swapped", "direction": "roll_forward"}]),
    );
    log.step("fix");
    let fixed = run(log, &home, &["doctor", "fix"]);
    // The half-swapped update also showed as Foo's files changed on disk before the fix;
    // verification checks that mode again, so the settled state reports nothing left.
    fixed.ok();
    assert_eq!(fixed.envelope()["state"], "DONE_OK");
    assert_eq!(fixed.envelope()["findings"], json!([]));
    assert_eq!(fixed.envelope()["summary"]["findings_after"], 0);
    log.json("warnings", &fixed.envelope()["warnings"]);
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_forward"),
    );
    let foo = home.addons("_retail_").join("Foo");
    let toc = fs::read_to_string(foo.join("Foo.toc")).unwrap();
    assert!(toc.contains("Version: 2"), "v2's folders: {toc}");
    assert!(!home.addons("_retail_").join("FooOld").exists());
    let warning = warnings_of(&fixed, "delegated_recovery");
    assert_eq!(warning.len(), 1, "{warning:?}");
    log.assert_eq(
        "its fix undoes it",
        &warning[0]["fix_argv"],
        &json!(["omaddons", "undo", tx]),
    );
    let run_id = text(&fixed.envelope()["run_id"]).to_owned();
    (home, tx, run_id)
}

#[test]
fn delegate_rolls_forward_a_swapped_update_and_points_at_undo() {
    let log = tlog!();
    let (home, tx, _) = rolled_forward(&log);
    log.step("undo");
    run(&log, &home, &["undo", &tx]).ok();
    log.assert_unchanged("addons", &home.addons("_retail_"));
}

#[test]
fn delegate_is_part_of_a_default_fix() {
    let log = tlog!();
    let home = scene(&log);
    let tx = interrupted_install(&log, &home);
    log.step("fix");
    let fixed = run(&log, &home, &["doctor", "fix"]);
    fixed.ok();
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_back"),
    );
}

// ---------------------------------------------------------------------------------------
// 4-6. Binding.

/// What a stale bound fix must leave: every tree but `<state>/omaddons/doctor/`.
fn record_all(log: &TestLog, home: &Home) {
    for (label, root) in [
        ("state", home.env.state()),
        ("config", home.env.config()),
        ("cache", home.env.cache()),
        ("client", home.client("_retail_")),
    ] {
        log.tree(label, &root);
    }
}

fn nothing_moved(log: &TestLog, home: &Home) {
    // The global lock's file holds its holder's record, which every lock-taking command
    // rewrites; it is no subject a fix moves.
    let changed: Vec<String> = state_changed_outside_doctor(log, home)
        .into_iter()
        .filter(|line| &line[2..] != "omaddons/lock (content)")
        .collect();
    log.assert_eq(
        "<state> outside the run's directory and the lock's record",
        &changed,
        &Vec::<String>::new(),
    );
    for (label, root) in [
        ("config", home.env.config()),
        ("cache", home.env.cache()),
        ("client", home.client("_retail_")),
    ] {
        log.assert_unchanged(label, &root);
    }
}

#[test]
fn delegate_bound_fix_is_stale_when_a_journal_appeared() {
    let log = tlog!();
    let home = scene(&log);
    run(&log, &home, &words(&install_args(&home, "gh:o/foo", "v1"))).ok();
    let only = format!("fm-test-write,{PENDING}");
    log.step("preview");
    plant_write(&home, "a.write", b"broken\n");
    let preview = run(
        &log,
        &home,
        &["doctor", "fix", "--only", &only, "--dry-run"],
    );
    let token = text(&preview.envelope()["precondition"]).to_owned();
    log.assert_eq(
        "nothing delegated yet",
        &preview.envelope()["delegated_planned"],
        &json!([]),
    );
    let tx = crash(
        &log,
        &home,
        &install_args(&home, "gh:o/bar", "bar"),
        "engine.swap.after_exchange",
    );
    record_all(&log, &home);
    log.step("fix");
    let bound = run(
        &log,
        &home,
        &["doctor", "fix", "--only", &only, "--if-match", &token],
    );
    bound.fails(4, "plan_stale");
    let changed = &bound.envelope()["error"]["details"]["changed"];
    log.json("changed", changed);
    assert!(
        changed.as_array().unwrap().contains(&json!("selection")),
        "{changed}"
    );
    log.step("check");
    nothing_moved(&log, &home);
    assert!(journal_left(&home, &tx), "the journal is left");
}

#[test]
fn delegate_bound_fix_is_stale_when_its_journal_was_recovered_meanwhile() {
    let log = tlog!();
    let home = scene(&log);
    interrupted_install(&log, &home);
    log.step("preview");
    let preview = run(&log, &home, &["doctor", "fix", "--dry-run"]);
    let token = text(&preview.envelope()["precondition"]).to_owned();
    log.step("recover by another command");
    run(&log, &home, &["config", "set", "check_interval_hours", "6"]).ok();
    record_all(&log, &home);
    log.step("fix");
    let bound = run(&log, &home, &["doctor", "fix", "--if-match", &token]);
    bound.fails(4, "plan_stale");
    log.json("changed", &bound.envelope()["error"]["details"]["changed"]);
    nothing_moved(&log, &home);
}

#[test]
fn delegate_bound_fix_runs_exactly_the_disclosed_recovery() {
    let log = tlog!();
    let home = scene(&log);
    let tx = interrupted_install(&log, &home);
    log.step("preview");
    let preview = run(&log, &home, &["doctor", "fix", "--dry-run"]);
    let token = text(&preview.envelope()["precondition"]).to_owned();
    log.step("fix");
    let bound = run(&log, &home, &["doctor", "fix", "--if-match", &token]);
    bound.ok();
    log.assert_eq(
        "delegated",
        &bound.envelope()["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_back"),
    );
}

// ---------------------------------------------------------------------------------------
// 7-8. Concurrency and failure.

#[test]
fn delegate_two_fixers_serialize() {
    let log = tlog!();
    let home = scene(&log);
    let tx = interrupted_install(&log, &home);
    let (a, b) = (
        Pause::new(&home.env.root().join("pause-a")),
        Pause::new(&home.env.root().join("pause-b")),
    );
    log.step("pause");
    let first = cmd(&log, &home, &["doctor", "fix"])
        .failpoints("doctor.fix.planned=pause")
        .pause(&a)
        .spawn();
    let second = cmd(&log, &home, &["doctor", "fix"])
        .failpoints("doctor.fix.planned=pause")
        .pause(&b)
        .spawn();
    log.reached(&a, "doctor.fix.planned");
    log.reached(&b, "doctor.fix.planned");
    log.step("resume");
    log.resume(&a, "doctor.fix.planned");
    log.resume(&b, "doctor.fix.planned");
    let (first, second) = (checked(first.wait()), checked(second.wait()));
    let runs = [&first, &second];
    for ran in runs {
        ran.ok();
        log.json("delegated", &ran.envelope()["delegated"]);
    }
    let recovered: Vec<Vec<Value>> = runs.iter().map(|r| delegated(r)).collect();
    let mut sorted = recovered.clone();
    sorted.sort_by_key(Vec::len);
    log.assert_eq(
        "one recovers, the other takes no action",
        &json!(sorted),
        &json!([[], settled(PENDING, &tx, "folder", "rolled_back")]),
    );
    for ran in runs {
        if delegated(ran).is_empty() {
            log.assert_eq(
                "no action",
                &ran.envelope()["summary"]["actions_taken"],
                &json!(0),
            );
        }
    }
    let warnings: usize = runs
        .iter()
        .map(|r| warnings_of(r, "delegated_recovery").len())
        .sum();
    log.assert_eq("one delegated_recovery in all", &warnings, &1);
    run(&log, &home, &["doctor"]).ok();
}

/// A delegated recovery that fails leaves its journal and makes the run partial; the
/// next fix recovers it. Both envelopes validate against their schema.
#[test]
fn delegate_failed_recovery_makes_the_run_partial() {
    let log = tlog!();
    let home = scene(&log);
    let tx = interrupted_install(&log, &home);
    log.step("fix, its recovery failing");
    let failed = checked(
        cmd(&log, &home, &["doctor", "fix"])
            .failpoints("engine.recover.step=error")
            .run(),
    );
    failed.exit(2);
    schema_valid(&log, &failed);
    let report = failed.envelope();
    log.json("delegated", &report["delegated"]);
    log.assert_eq("state", &report["state"], &json!("DONE_PARTIAL"));
    let items = delegated(&failed);
    assert_eq!(items.len(), 1, "{items:?}");
    log.assert_eq("outcome", &items[0]["outcome"], &json!("failed"));
    let reason = text(&items[0]["reason"]);
    log.note(format!("the injected failure's reason: {reason}"));
    assert!(!reason.is_empty(), "{items:?}");
    assert!(journal_left(&home, &tx), "the journal is left");
    assert_eq!(findings_of(&failed, PENDING).len(), 1, "still found");
    log.step("fix again");
    let fixed = run(&log, &home, &["doctor", "fix"]);
    fixed.ok();
    schema_valid(&log, &fixed);
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_back"),
    );
    log.assert_unchanged("addons", &home.addons("_retail_"));
}

// ---------------------------------------------------------------------------------------
// 9. A repair's journal.

#[test]
fn delegate_recovers_a_repair_family_journal() {
    let log = tlog!();
    let home = scene(&log);
    let file = plant_write(&home, "a.write", b"broken\n");
    log.tree("subject", file.parent().unwrap());
    log.step("a repair killed after its first step line");
    cmd(&log, &home, &["doctor", "fix", "--only", "fm-test-write"])
        .failpoints("doctor.repair.step=abort@1")
        .run()
        .aborted();
    let pending: Vec<String> = files_in(&home.state().join("journal"))
        .into_iter()
        .filter(|name| name.ends_with(".repair.jsonl"))
        .collect();
    assert_eq!(pending.len(), 1, "{pending:?}");
    let tx = pending[0].trim_end_matches(".repair.jsonl").to_owned();
    log.step("diagnose");
    let diagnosed = run(&log, &home, &["doctor"]);
    let found = findings_of(&diagnosed, PENDING);
    assert_eq!(found.len(), 1, "{found:?}");
    log.assert_eq(
        "its family",
        &found[0]["evidence"]["family"],
        &json!("repair"),
    );
    log.step("fix");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", PENDING]);
    fixed.ok();
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PENDING, &tx, "repair", "rolled_back"),
    );
    log.assert_unchanged("subject", file.parent().unwrap());
    assert!(
        files_in(&home.state().join("journal"))
            .iter()
            .all(|name| !name.ends_with(".repair.jsonl")),
        "no repair journal"
    );
}

// ---------------------------------------------------------------------------------------
// 10-12. Restores and preservation.

/// A restore's journal waits while its game runs (no fixer, nothing moves) and is
/// delegated once it exits: rolled back, the settings as before the restore.
#[test]
fn delegate_finishes_a_restore_only_after_the_game_exits() {
    let log = tlog!();
    let home = scene(&log);
    run(&log, &home, &words(&install_args(&home, "gh:o/foo", "v1"))).ok();
    log.step("plant");
    let (tx, proc) = plant::journal_waits_for_game(&home, "retail");
    log.json("planted", &plant::take_planted());
    let wtf = home.client("_retail_").join("WTF");
    log.tree("wtf", &wtf);
    log.tree("journal", &home.state().join("journal"));
    let running = |args: &[&str]| {
        checked(
            cmd(&log, &home, args)
                .env("OMADDONS_PROC_ROOT", proc.root())
                .run(),
        )
    };
    log.step("diagnose while the game runs");
    let diagnosed = running(&["doctor"]);
    assert_eq!(
        findings_of(&diagnosed, WAITS).len(),
        1,
        "waits for the game"
    );
    assert!(findings_of(&diagnosed, PENDING).is_empty(), "not delegated");
    log.step("fix while the game runs");
    let fixed = running(&["doctor", "fix"]);
    schema_valid(&log, &fixed);
    log.assert_eq("nothing delegated", &json!(delegated(&fixed)), &json!([]));
    log.assert_unchanged("wtf", &wtf);
    log.assert_unchanged("journal", &home.state().join("journal"));
    log.step("the game exits");
    let diagnosed = run(&log, &home, &["doctor"]);
    assert_eq!(findings_of(&diagnosed, PENDING).len(), 1, "delegable now");
    let fixed = run(&log, &home, &["doctor", "fix"]);
    fixed.ok();
    schema_valid(&log, &fixed);
    log.json("warnings", &fixed.envelope()["warnings"]);
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PENDING, &tx, "folder", "rolled_back"),
    );
    let config = fs::read_to_string(wtf.join("Config.wtf")).unwrap();
    assert!(
        config.contains("after the backup"),
        "the settings as before the restore: {config}"
    );
    assert!(!journal_left(&home, &tx));
}

#[test]
fn delegate_preservation_resumes_once_the_cause_is_gone() {
    let log = tlog!();
    let home = scene(&log);
    run(&log, &home, &words(&install_args(&home, "gh:o/foo", "v1"))).ok();
    log.step("plant");
    let tx = plant::journal_preservation_incomplete(&home, "retail");
    log.json("planted", &plant::take_planted());
    log.step("diagnose");
    let diagnosed = run(&log, &home, &["doctor"]);
    let found = findings_of(&diagnosed, PRESERVATION);
    assert_eq!(found.len(), 1, "{found:?}");
    log.assert_eq(
        "remediation",
        &found[0]["remediation"]["argv"],
        &json!([
            "omaddons",
            "doctor",
            "fix",
            "--only",
            PRESERVATION,
            "-t",
            "retail"
        ]),
    );
    log.step("fix");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", PRESERVATION]);
    fixed.ok();
    log.json("warnings", &fixed.envelope()["warnings"]);
    log.assert_eq(
        "delegated",
        &fixed.envelope()["delegated"],
        &settled(PRESERVATION, &tx, "folder", "rolled_back"),
    );
    assert_eq!(
        warnings_of(&fixed, "restore_rolled_back").len(),
        1,
        "the engine's own warning passes through"
    );
    let recovered = plant::recovered_backups(&home, "retail");
    assert_eq!(recovered.len(), 1, "a recovered backup: {recovered:?}");
}

#[test]
fn delegate_preservation_predicted_stop_offers_only_explain() {
    let log = tlog!();
    let home = scene(&log);
    run(&log, &home, &words(&install_args(&home, "gh:o/foo", "v1"))).ok();
    log.step("plant");
    let (_tx, _moved) = plant::journal_preservation_stops(&home, "retail");
    log.json("planted", &plant::take_planted());
    log.tree("journal", &home.state().join("journal"));
    log.step("diagnose");
    let diagnosed = run(&log, &home, &["doctor"]);
    let found = findings_of(&diagnosed, PRESERVATION);
    assert_eq!(found.len(), 1, "{found:?}");
    let remediation = &found[0]["remediation"];
    log.json("remediation", remediation);
    log.assert_eq(
        "explain only",
        &remediation["argv"],
        &json!([
            "omaddons",
            "doctor",
            "explain",
            PRESERVATION,
            "-t",
            "retail"
        ]),
    );
    log.assert_eq(
        "not auto-fixable",
        &remediation["auto_fixable"],
        &json!(false),
    );
    log.step("fix");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", PRESERVATION]);
    fixed.exit(1);
    log.assert_eq("nothing delegated", &json!(delegated(&fixed)), &json!([]));
    log.assert_eq(
        "no action",
        &fixed.envelope()["summary"]["actions_taken"],
        &json!(0),
    );
    log.assert_unchanged("journal", &home.state().join("journal"));
}

// ---------------------------------------------------------------------------------------
// 13-15. What a delegation leaves.

/// The doctor's undo never reverts a delegation: the delegation-only run committed no
/// repair, bare `doctor undo` selects nothing, and `doctor undo <run>` moves nothing but
/// names the roll-forward's own undo (`omaddons undo <tx>`, in `commands[]` and the human
/// output).
#[test]
fn delegate_doctor_undo_never_reverts_a_delegation() {
    let log = tlog!();
    let (home, tx, fix_run) = rolled_forward(&log);
    let report: Value = serde_json::from_slice(
        &fs::read(
            home.state()
                .join("doctor/runs")
                .join(&fix_run)
                .join("report.json"),
        )
        .unwrap(),
    )
    .unwrap();
    log.assert_eq("no repair transaction", &report["tx"], &Value::Null);
    log.tree("addons", &home.addons("_retail_"));
    log.step("bare doctor undo");
    let bare = run(&log, &home, &["doctor", "undo"]);
    bare.ok();
    log.assert_eq(
        "nothing selected",
        &bare.envelope()["reverted_run"],
        &Value::Null,
    );
    log.step("doctor undo <run>");
    let named = run(&log, &home, &["doctor", "undo", &fix_run]);
    named.ok();
    log.json("commands", &named.envelope()["commands"]);
    log.assert_eq("no undo", &named.envelope()["undo_tx"], &Value::Null);
    let argvs: Vec<&Value> = named.envelope()["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["argv"])
        .collect();
    assert!(
        argvs.contains(&&json!(["omaddons", "undo", tx])),
        "it names the roll-forward's undo: {argvs:?}"
    );
    let human = log
        .cmd(&home, &["doctor", "undo", &fix_run])
        .env(ENV.0, ENV.1)
        .run();
    human.ok().assert_private();
    assert!(human.stdout().contains(&tx), "{}", human.stdout());
    log.assert_unchanged("addons", &home.addons("_retail_"));
}

#[test]
fn delegate_second_fix_takes_no_action() {
    let log = tlog!();
    let (home, _) = rolled_back(&log);
    log.step("fix again");
    let again = run(&log, &home, &["doctor", "fix"]);
    again.ok();
    log.assert_eq("nothing delegated", &json!(delegated(&again)), &json!([]));
    log.assert_eq(
        "no action",
        &again.envelope()["summary"]["actions_taken"],
        &json!(0),
    );
}

/// The envelopes of a preview, a failed delegation and a settled one validate strictly
/// against `robot-docs schema "doctor fix"` and the envelope's schema: `delegated[]`
/// items with and without `reason`, and the `delegated_recovery` warning.
#[test]
fn delegate_report_validates_against_its_schema() {
    let log = tlog!();
    let home = scene(&log);
    interrupted_install(&log, &home);
    log.step("preview");
    schema_valid(&log, &run(&log, &home, &["doctor", "fix", "--dry-run"]));
    log.step("a failed delegation");
    let failed = checked(
        cmd(&log, &home, &["doctor", "fix"])
            .failpoints("engine.recover.step=error")
            .run(),
    );
    schema_valid(&log, &failed);
    assert!(delegated(&failed)[0]["reason"].is_string());
    log.step("a settled one");
    let fixed = run(&log, &home, &["doctor", "fix"]);
    fixed.ok();
    schema_valid(&log, &fixed);
    assert!(delegated(&fixed)[0].get("reason").is_none());
    assert_eq!(warnings_of(&fixed, "delegated_recovery").len(), 1);
}
