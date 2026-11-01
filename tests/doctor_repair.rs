//! e2e suite: doctor fix end to end with the test-only fixers: every terminal state, the dry run's purity, a preview bound across processes, stale runs that touch nothing, concurrent fixers, the force and disabled gates, quarantine roots, declared scopes and privacy
#![cfg(test)]

mod common;
// Shared with `tests/doctor.rs`: this suite uses only the test fixers' fixtures.
#[allow(dead_code)]
#[path = "doctor/mod.rs"]
mod fixture;
// E7 s6: the undo of repairs end to end (`undo::<scenario>`).
#[path = "doctor_repair/undo.rs"]
mod undo;
// F2 s4: the delegation end to end (`delegate::delegate_<scenario>`).
#[path = "doctor_repair/delegate.rs"]
mod delegate;
// The contract suite's strict validator, for the delegation's envelopes.
#[allow(dead_code)]
#[path = "contract/validate.rs"]
mod validate;

/// A child process of E5's synthetic repair scenes, which the undo scenarios reuse (the
/// round trip of every kind, a repair of an installed folder). Run on its own it does
/// nothing.
#[test]
#[ignore = "a child process of the repair scenes"]
fn child_repair() {
    common::repair::child_main();
}

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use common::doctor::test_fixers::{
    ENV, FLATTEN, Kind, QUARANTINE, REPAIRED, Side, WRITE, plant_flatten, plant_missing_write,
    plant_quarantine, plant_write, register, test_dir,
};
use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::pause::Pause;
use common::privacy::{CHARACTER, assert_private_all};
use common::tlog::{Cmd, Ran, TestLog, tlog};
use fixture::Fixture;
use fixture::fixtures::test_fixers;
use omaddons::fsx::{GlobalLock, GlobalLockGuard};
use omaddons::state::paths::Paths;

/// Every test fixer, by id.
const ALL: &str = "fm-test-write,fm-test-quarantine,fm-test-flatten";

/// A home with a retail client, logged.
fn home(log: &TestLog) -> Home {
    let home = Home::new(&["wow"]);
    log.env(&home);
    home
}

/// `omaddons <args> --json --progress json` with the test fixers registered and this
/// home's own Omarchy.
fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    let mut argv: Vec<&str> = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    log.cmd(home, &argv)
        .env(ENV.0, ENV.1)
        .env("OMARCHY_PATH", plant::omarchy_path(home))
}

/// A finished run, a doctor envelope checked against the doctor's contract, every channel
/// checked for private names.
fn checked(ran: Ran) -> Ran {
    let doctor = ran.run.argv.iter().any(|a| a == "doctor");
    if doctor && ran.run.code >= 0 && !ran.stdout().is_empty() {
        check_envelope(ran.stdout(), ran.stderr(), ran.run.code);
    }
    assert_private_all(ran.stdout(), ran.stderr());
    ran
}

fn run(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    checked(cmd(log, home, args).run())
}

/// The run's `report.json`'s directory.
fn run_dir(ran: &Ran) -> PathBuf {
    PathBuf::from(ran.envelope()["run_dir"].as_str().unwrap())
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// The global lock, held by the test as another omaddons process would hold it.
fn hold_lock(home: &Home) -> GlobalLockGuard {
    let paths = Paths::with_roots(
        home.env.home(),
        home.env.config(),
        home.env.state(),
        home.env.cache(),
    );
    GlobalLock::acquire(&paths, "test").unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn ino(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().ino()
}

/// What changed in `<state>` since `log.tree("state", …)` outside the doctor's own record
/// (`omaddons/doctor/`), as `+ path`, `- path` or `~ path (what)` lines.
fn state_changed_outside_doctor(log: &TestLog, home: &Home) -> Vec<String> {
    let changed = log.diff("state", &home.env.state());
    changed
        .first(changed.total)
        .into_iter()
        .filter(|line| {
            let path = &line[2..];
            path != "omaddons/doctor" && !path.starts_with("omaddons/doctor/")
        })
        .collect()
}

/// The files directly in `dir`, sorted.
fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The repair's `actions.jsonl` lines.
fn actions_log(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("actions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Every path an `actions.jsonl` names (and every rename's destination) lies below one
/// of `roots`: the scopes its fixers declare in `writes_to` (rule 17).
fn assert_in_scopes(log: &TestLog, dir: &Path, roots: &[PathBuf]) {
    let mut outside = Vec::new();
    for action in actions_log(dir) {
        for key in ["path", "to"] {
            if let Some(path) = action[key].as_str()
                && !roots.iter().any(|root| Path::new(path).starts_with(root))
            {
                outside.push(path.to_owned());
            }
        }
    }
    log.assert_eq("writes outside the declared scopes", &outside, &Vec::new());
}

/// A token's four parts (`command`, `targets`, `selection`, `observed`).
fn parts(token: &str) -> Vec<String> {
    let hex = token.strip_prefix("pc1:").unwrap();
    (0..4)
        .map(|i| hex[i * 16..(i + 1) * 16].to_owned())
        .collect()
}

fn token_of(log: &TestLog, label: &str, ran: &Ran) -> String {
    let token = text(&ran.envelope()["precondition"]).to_owned();
    log.json(label, &token);
    assert!(token.starts_with("pc1:"), "{label}: {token}");
    token
}

/// E7's two test fixers through the binary: `fm-test-chmod` chmods a config file to 0600
/// and its undo puts the mode back; `fm-test-suspend` (forced) sets a stuck journal aside,
/// guarded, and its undo puts the journal back under its own name, with no provisional
/// name left behind.
#[test]
fn chmod_and_suspend_test_fixers_repair_and_undo() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    register(&home, "retail");
    let file = test_dir(&home, Side::Config).join("a.chmod");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"secret\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    log.step("chmod");
    run(&log, &home, &["doctor", "fix", "--only", "fm-test-chmod"]).ok();
    log.assert_eq("repaired", &mode(&file), &0o600);
    run(&log, &home, &["undo"]).ok();
    log.assert_eq("undone", &mode(&file), &0o644);

    log.step("suspend");
    let tx = "01JB8Q2VA00000000000000009";
    let journal = home.state().join("journal").join(format!("{tx}.jsonl"));
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(&journal, "{\"line\":\"prepared\"}\n").unwrap();
    let request = test_dir(&home, Side::State).join(format!("{tx}.suspend"));
    fs::create_dir_all(request.parent().unwrap()).unwrap();
    let key = plant::target_key(&home, "retail");
    fs::write(&request, format!("{key}\n")).unwrap();
    let journals = home.state().join("journal");
    let set_aside = run(
        &log,
        &home,
        &[
            "doctor",
            "fix",
            "--only",
            "fm-test-suspend",
            "--force",
            "--yes",
        ],
    );
    set_aside.ok();
    log.assert_eq("set aside", &journal.exists(), &false);
    run(&log, &home, &["undo"]).ok();
    log.assert_eq(
        "the journal back under its own name",
        &files_in(&journals),
        &vec![format!("{tx}.jsonl")],
    );
}

/// `doctor undo` (E7 s4): bare, it reverts the newest repair still in effect, also after
/// a diagnose; again, it finds nothing; by the fix run's id, it reports the repair already
/// undone; and the subject is as it was before the repair.
#[test]
fn doctor_undo_smoke() {
    let log = tlog!();
    log.step("home");
    let home = home(&log);
    let file = plant_write(&home, "a.write", b"broken\n");
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let runs = home.state().join("doctor/runs");

    log.step("repair");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", WRITE]);
    fixed.ok();
    let fix_run = text(&fixed.envelope()["run_id"]).to_owned();
    let fix_tx = text(&fixed.envelope()["tx"]).to_owned();
    log.assert_eq("repaired", &fs::read(&file).unwrap(), &REPAIRED.to_vec());

    log.step("diagnose");
    run(&log, &home, &["doctor"]).exit(0);

    log.step("dry_run");
    log.tree("runs", &runs);
    let planned = run(&log, &home, &["doctor", "undo", "--dry-run"]);
    planned.ok();
    let report = planned.envelope();
    log.assert_eq("tx", &report["tx"], &json!(fix_tx));
    log.assert_eq("run_id", &report["run_id"], &Value::Null);
    log.assert_eq(
        "planned",
        &report["actions_planned"][0]["op"],
        &json!("write_file"),
    );
    log.assert_eq(
        "the bindable preview",
        &report["commands"][0]["argv"],
        &json!(["omaddons", "undo", fix_tx, "--dry-run"]),
    );
    log.assert_unchanged("runs", &runs);
    log.assert_eq(
        "still repaired",
        &fs::read(&file).unwrap(),
        &REPAIRED.to_vec(),
    );

    log.step("bare_undo");
    let undone = run(&log, &home, &["doctor", "undo"]);
    undone.ok();
    let report = undone.envelope();
    log.assert_eq("reverted_run", &report["reverted_run"], &json!(fix_run));
    log.assert_eq("tx", &report["tx"], &json!(fix_tx));
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    log.assert_eq("actions_taken", &report["actions_taken"], &json!(1));
    log.assert_eq("already_undone", &report["already_undone"], &json!(false));
    let undo_run = text(&report["run_id"]).to_owned();
    let recorded: Value =
        serde_json::from_slice(&fs::read(runs.join(&undo_run).join("report.json")).unwrap())
            .unwrap();
    log.assert_eq("run kind", &recorded["kind"], &json!("undo"));
    log.assert_eq("its undo_tx", &recorded["undo_tx"], &report["undo_tx"]);

    log.step("again");
    let again = run(&log, &home, &["doctor", "undo"]);
    again.ok();
    log.assert_eq(
        "nothing selected",
        &again.envelope()["reverted_run"],
        &Value::Null,
    );
    log.assert_eq("no run", &again.envelope()["run_id"], &Value::Null);

    log.step("by_run");
    log.tree("runs", &runs);
    let by_run = run(&log, &home, &["doctor", "undo", &fix_run]);
    by_run.ok();
    log.assert_eq(
        "already_undone",
        &by_run.envelope()["already_undone"],
        &json!(true),
    );
    log.assert_eq("run_id", &by_run.envelope()["run_id"], &Value::Null);
    log.assert_eq("undo_tx", &by_run.envelope()["undo_tx"], &report["undo_tx"]);
    log.assert_unchanged("runs", &runs);

    log.step("trees");
    log.assert_unchanged("subject", &subject);
}

/// 1. A write repaired: the report, the backup, history, the hardened record, `undo.sh`.
#[test]
fn fix_repairs_a_write_and_reaches_done_ok() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    let file = plant_write(&home, "a.write", b"broken\n");
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);

    log.step("diagnose");
    let found = run(&log, &home, &["doctor"]);
    found.exit(1);
    let findings = found.envelope()["findings"].as_array().unwrap();
    log.assert_eq("findings", &findings.len(), &1);
    log.assert_eq("id", &findings[0]["id"], &json!(WRITE));
    log.assert_eq(
        "remediation.argv",
        &findings[0]["remediation"]["argv"],
        &json!(["omaddons", "doctor", "fix", "--only", WRITE]),
    );

    log.step("repair");
    let fixed = run(&log, &home, &["doctor", "fix", "--only", WRITE]);
    fixed.ok();
    let report = fixed.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    log.assert_eq("dry_run", &report["dry_run"], &json!(false));
    log.assert_eq(
        "actions_taken",
        &report["summary"]["actions_taken"],
        &json!(1),
    );
    log.assert_eq(
        "bytes_backed_up",
        &report["summary"]["bytes_backed_up"],
        &json!(7),
    );
    assert!(report["tx"].is_string(), "{report}");
    log.assert_eq("file", &fs::read(&file).unwrap(), &REPAIRED.to_vec());

    log.step("verify the record");
    let dir = run_dir(&fixed);
    let run_id = text(&report["run_id"]).to_owned();
    log.assert_eq(
        "backup",
        &fs::read(dir.join("backups/1")).unwrap(),
        &b"broken\n".to_vec(),
    );
    let history = run(&log, &home, &["history"]);
    history.ok();
    let repairs: Vec<&Value> = history.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "repair")
        .collect();
    log.assert_eq("repair lines", &repairs.len(), &1);
    log.assert_eq(
        "doctor_run_id",
        &repairs[0]["doctor_run_id"],
        &json!(run_id),
    );
    log.assert_eq("target", &repairs[0]["target"], &Value::Null);
    let actions = actions_log(&dir);
    log.assert_eq("actions.jsonl lines", &actions.len(), &1);
    assert!(Path::new(text(&actions[0]["path"])).starts_with(home.state()));
    for (name, want) in [
        ("report.json", 0o400),
        ("runtime.jsonl", 0o400),
        ("actions.jsonl", 0o400),
        ("undo.sh", 0o500),
    ] {
        log.assert_eq(&format!("mode of {name}"), &mode(&dir.join(name)), &want);
    }
    log.assert_eq("mode of the run directory", &mode(&dir), &0o700);
    assert!(![0o400, 0o500].contains(&mode(&dir.join("backups"))));

    log.step("undo script");
    let script = fs::read_to_string(dir.join("undo.sh")).unwrap();
    let syntax = std::process::Command::new("sh")
        .arg("-n")
        .arg(dir.join("undo.sh"))
        .output()
        .unwrap();
    assert!(syntax.status.success(), "sh -n: {syntax:?}");
    assert!(
        script.contains(&format!("doctor undo '{run_id}'")),
        "{script}"
    );
    for var in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
    ] {
        assert!(
            script.contains(&format!("export {var}")),
            "{var} in\n{script}"
        );
    }
    log.assert_eq("latest", &plant::latest(&home), &Some(run_id));
    assert_in_scopes(&log, &dir, &[home.state()]);
    TestWrite.assert_healthy(&home);
}

use fixture::fixtures::fm_test_write::TestWrite;

/// 2. A write that creates its file: an absent `pre`, no backup.
#[test]
fn write_that_creates_a_file() {
    let log = tlog!();
    let home = home(&log);
    let file = plant_missing_write(&home, "new.write");
    log.step("preview");
    let preview = run(&log, &home, &["doctor", "fix", "--dry-run"]);
    preview.exit(1);
    let planned = &preview.envelope()["actions_planned"][0];
    log.assert_eq("pre", &planned["pre"], &json!({"kind": "absent"}));
    log.assert_eq("backup", &planned["backup"], &Value::Null);
    log.assert_eq(
        "bytes_to_back_up",
        &preview.envelope()["summary"]["bytes_to_back_up"],
        &json!(0),
    );
    log.step("repair");
    run(&log, &home, &["doctor", "fix"]).ok();
    log.assert_eq("created", &fs::read(&file).unwrap(), &REPAIRED.to_vec());
}

/// 3. A repaired mode still found: `DONE_PARTIAL`, exit 2, `latest` moved.
#[test]
fn residual_is_done_partial() {
    let log = tlog!();
    let home = home(&log);
    plant_write(&home, "x.residual.write", b"broken\n");
    log.step("repair");
    let ran = run(&log, &home, &["doctor", "fix"]);
    ran.exit(2);
    let report = ran.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_PARTIAL"));
    log.assert_eq(
        "findings_after",
        &report["summary"]["findings_after"],
        &json!(1),
    );
    log.assert_eq("findings", &report["findings"][0]["id"], &json!(WRITE));
    log.assert_eq(
        "latest",
        &plant::latest(&home),
        &Some(text(&report["run_id"]).to_owned()),
    );
    assert!(run_dir(&ran).join("undo.sh").is_file());
}

/// 4. The second step fails: the first is rolled back, nothing changed, no record.
#[test]
fn step_error_rolls_back() {
    let log = tlog!();
    let home = home(&log);
    plant_write(&home, "a.write", b"broken a\n");
    plant_write(&home, "b.write", b"broken b\n");
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let before = plant::latest(&home);
    log.step("repair, failing at the second step");
    let ran = checked(
        cmd(&log, &home, &["doctor", "fix"])
            .failpoints("doctor.repair.step=error@2")
            .run(),
    );
    ran.exit(3);
    let report = ran.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_FAILED"));
    log.assert_eq(
        "backups_restored",
        &report["summary"]["backups_restored"],
        &json!(1),
    );
    log.assert_unchanged("subject", &subject);
    let journals: Vec<String> = files_in(&home.state().join("journal"))
        .into_iter()
        .filter(|name| name.ends_with(".repair.jsonl"))
        .collect();
    log.assert_eq("repair journals left", &journals, &Vec::new());
    let history = run(&log, &home, &["history"]);
    let repairs = history.data()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "repair")
        .count();
    log.assert_eq("repair lines", &repairs, &0);
    log.assert_eq("latest", &plant::latest(&home), &before);
    assert!(!run_dir(&ran).join("undo.sh").exists());
}

/// 5. A forced repair named without `--force`: refused before the lock.
#[test]
fn missing_force_is_refused() {
    let log = tlog!();
    let home = home(&log);
    plant_quarantine(&home, Side::State, "b.quarantine", Kind::File);
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let lock = hold_lock(&home);
    log.step("refused, the lock held elsewhere");
    let ran = run(&log, &home, &["doctor", "fix", "--only", QUARANTINE]);
    ran.fails(4, "doctor_refused");
    let error = &ran.envelope()["error"];
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("force_required"),
    );
    log.assert_eq("fm", &error["details"]["fm"], &json!(QUARANTINE));
    log.assert_eq(
        "fix_argv",
        &error["fix_argv"],
        &json!(["omaddons", "doctor", "explain", QUARANTINE]),
    );
    log.assert_eq("state", &ran.envelope()["state"], &json!("REFUSING"));
    assert!(
        ran.elapsed_ms < 4_000,
        "decided before the lock: {} ms",
        ran.elapsed_ms
    );
    drop(lock);
    log.assert_unchanged("subject", &subject);
    run(&log, &home, &["doctor", "explain", QUARANTINE]).ok();
}

/// 6. A directory and its child, planned by two fixers: refused together.
#[test]
fn overlapping_fixes_are_refused_together() {
    let log = tlog!();
    let home = home(&log);
    let dir = plant_quarantine(&home, Side::State, "box.quarantine", Kind::Dir);
    plant_write(&home, "box.quarantine/item.write", b"broken\n");
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let only = format!("{QUARANTINE},{WRITE}");
    let ran = run(
        &log,
        &home,
        &["doctor", "fix", "--only", &only, "--force", "--yes"],
    );
    ran.fails(4, "doctor_refused");
    let details = &ran.envelope()["error"]["details"];
    log.json("details", details);
    log.assert_eq("reason", &details["reason"], &json!("fixer_conflict"));
    let fms: Vec<&str> = details["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| text(&f["fm"]))
        .collect();
    assert!(
        fms.contains(&QUARANTINE) && fms.contains(&WRITE),
        "{details}"
    );
    let resources = details["resources"].to_string();
    let dir = dir.display().to_string();
    assert!(resources.contains(&dir), "{resources}");
    assert!(
        resources.contains(&format!("{dir}/item.write")),
        "{resources}"
    );
    log.assert_unchanged("subject", &subject);
}

/// 7. The lock held past the timeout: `LOCK_LOST`, exit 5; free, the same run repairs.
#[test]
fn lock_held_past_the_timeout_is_lock_lost() {
    let log = tlog!();
    let home = home(&log);
    plant_write(&home, "c.write", b"broken\n");
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let lock = hold_lock(&home);
    log.step("wait for the lock");
    let ran = run(&log, &home, &["doctor", "fix", "--only", WRITE]);
    ran.fails(5, "locked");
    log.note(format!("waited {} ms", ran.elapsed_ms));
    assert!(ran.elapsed_ms >= 5_000, "{} ms", ran.elapsed_ms);
    let report = ran.envelope();
    log.assert_eq(
        "holder",
        &report["error"]["details"]["holder"]["command"],
        &json!("test"),
    );
    log.assert_eq("state", &report["state"], &json!("LOCK_LOST"));
    log.assert_unchanged("subject", &subject);
    drop(lock);
    log.step("the lock free");
    run(&log, &home, &["doctor", "fix", "--only", WRITE]).ok();
}

/// 8. The dry run takes no lock and writes only its run directory.
#[test]
fn dry_run_writes_only_its_run_directory() {
    let log = tlog!();
    let home = home(&log);
    log.step("plant");
    register(&home, "retail");
    plant_write(&home, "a.write", b"broken\n");
    plant_quarantine(&home, Side::State, "b.quarantine", Kind::File);
    plant_quarantine(&home, Side::Client("_retail_"), "c.quarantine", Kind::File);
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    let runs = plant::runs(&home);
    // Taken first: its holder line is the test's own write, and a dry run writes none.
    let lock = hold_lock(&home);
    log.tree("state", &home.env.state());
    log.tree("config", &home.env.config());
    log.tree("cache", &home.env.cache());
    log.tree("client", &home.client("_retail_"));

    log.step("preview, the lock held elsewhere");
    let ran = run(
        &log,
        &home,
        &[
            "doctor",
            "fix",
            "--only",
            ALL,
            "--force",
            "--yes",
            "--dry-run",
        ],
    );
    drop(lock);
    ran.exit(1);
    assert!(
        ran.elapsed_ms < 4_000,
        "no lock wait: {} ms",
        ran.elapsed_ms
    );
    let report = ran.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_FINDINGS"));
    let planned = report["actions_planned"].as_array().unwrap();
    log.json("actions_planned", planned);
    let tos: Vec<&str> = planned.iter().filter_map(|a| a["to"].as_str()).collect();
    let quarantine = home.state().join("doctor/runs/<run>/quarantine/state");
    assert!(
        tos.iter().any(|to| Path::new(to).starts_with(&quarantine)),
        "{tos:?}"
    );
    assert!(
        tos.iter()
            .any(|to| to.contains("/snapshots/<tx>.repair/quarantine/")),
        "{tos:?}"
    );
    assert!(planned.iter().all(|a| a["bytes"].is_u64()));
    assert!(report["summary"]["bytes_to_back_up"].is_u64());
    assert!(text(&report["precondition"]).starts_with("pc1:"));

    log.step("nothing else written");
    log.assert_eq(
        "changed in <state> outside doctor/",
        &state_changed_outside_doctor(&log, &home),
        &Vec::new(),
    );
    log.assert_unchanged("config", &home.env.config());
    log.assert_unchanged("cache", &home.env.cache());
    log.assert_unchanged("client", &home.client("_retail_"));
    let mut after = plant::runs(&home);
    after.retain(|r| !runs.contains(r));
    log.assert_eq("new runs", &after.len(), &1);
    log.assert_eq(
        "its files",
        &files_in(&run_dir(&ran)),
        &vec!["report.json".to_owned(), "runtime.jsonl".to_owned()],
    );
}

/// 9. A preview binds across processes; a changed subject stales the next one.
#[test]
fn preview_binds_across_processes() {
    let log = tlog!();
    let home = home(&log);
    let d = plant_quarantine(&home, Side::State, "d.quarantine", Kind::File);
    let d_ino = ino(&d);
    let args = ["doctor", "fix", "--only", QUARANTINE, "--force", "--yes"];

    log.step("preview");
    let mut dry = args.to_vec();
    dry.push("--dry-run");
    let preview = run(&log, &home, &dry);
    preview.exit(1);
    let token = token_of(&log, "token", &preview);

    log.step("bound run");
    let mut bound = args.to_vec();
    bound.extend(["--if-match", &token]);
    let ran = run(&log, &home, &bound);
    ran.ok();
    let dir = run_dir(&ran);
    let run_id = text(&ran.envelope()["run_id"]).to_owned();
    let tx = text(&ran.envelope()["tx"]).to_owned();
    let planned = preview.envelope()["actions_planned"].as_array().unwrap();
    let done = ran.envelope()["actions"].as_array().unwrap();
    log.assert_eq("actions", &done.len(), &planned.len());
    for (p, d) in planned.iter().zip(done) {
        let resolve = |v: &Value| text(v).replace("<run>", &run_id).replace("<tx>", &tx);
        log.assert_eq("op", &d["op"], &p["op"]);
        log.assert_eq("path", &resolve(&d["path"]), &resolve(&p["path"]));
        log.assert_eq("to", &resolve(&d["to"]), &resolve(&p["to"]));
        log.assert_eq("ok", &d["ok"], &json!(true));
    }
    let moved = dir.join("quarantine/state/doctor-test/d.quarantine");
    log.assert_eq("set aside, same inode", &ino(&moved), &d_ino);

    log.step("a stale preview");
    let e = plant_quarantine(&home, Side::State, "e.quarantine", Kind::File);
    let preview = run(&log, &home, &dry);
    let token = token_of(&log, "second token", &preview);
    // Replaced, not edited in place: a set-aside's predicate is the file's identity.
    let replacement = e.with_extension("new");
    fs::write(&replacement, b"changed\n").unwrap();
    fs::rename(&replacement, &e).unwrap();
    let subject = test_dir(&home, Side::State);
    log.tree("subject", &subject);
    let mut bound = args.to_vec();
    bound.extend(["--if-match", &token]);
    let stale = run(&log, &home, &bound);
    stale.fails(4, "plan_stale");
    let error = &stale.envelope()["error"];
    log.assert_eq("retryable", &error["retryable"], &json!(false));
    assert!(
        error["details"]["changed"]
            .as_array()
            .unwrap()
            .contains(&json!("observed")),
        "{error}"
    );
    let mut fix: Vec<&str> = vec!["omaddons"];
    fix.extend(dry.iter());
    log.assert_eq("fix_argv", &error["fix_argv"], &json!(fix));
    log.assert_unchanged("subject", &subject);
}

/// 10. A journal appearing after the preview stales the bound run before anything moves.
#[test]
fn stale_bound_run_touches_no_journal() {
    let log = tlog!();
    let home = home(&log);
    plant_quarantine(&home, Side::State, "f.quarantine", Kind::File);
    let f = test_dir(&home, Side::State);
    let args = ["doctor", "fix", "--only", QUARANTINE, "--force", "--yes"];
    log.step("preview");
    let mut dry = args.to_vec();
    dry.push("--dry-run");
    let token = token_of(&log, "token", &run(&log, &home, &dry));

    log.step("an install crashes on retail");
    plant::journal_pending(&home, "retail");
    log.tree("subject", &f);
    log.tree("addons", &home.addons("_retail_"));
    log.tree("journal", &home.state().join("journal"));
    log.tree("inflight", &home.state().join("inflight"));
    log.tree(
        "stage",
        &home.client("_retail_").join("Interface/.omaddons/stage"),
    );

    log.step("bound run");
    let mut bound = args.to_vec();
    bound.extend(["--if-match", &token]);
    let stale = run(&log, &home, &bound);
    stale.fails(4, "plan_stale");
    log.json("details", &stale.envelope()["error"]["details"]);
    log.assert_unchanged("subject", &f);
    log.assert_unchanged("addons", &home.addons("_retail_"));
    log.assert_unchanged("journal", &home.state().join("journal"));
    log.assert_unchanged("inflight", &home.state().join("inflight"));
    log.assert_unchanged(
        "stage",
        &home.client("_retail_").join("Interface/.omaddons/stage"),
    );

    log.step("a change command recovers it");
    run(&log, &home, &["config", "set", "check_interval_hours", "6"]).ok();
}

/// 11. Two fixers planned at once serialize: the second repairs nothing.
#[test]
fn concurrent_fixers_serialize() {
    let log = tlog!();
    let home = home(&log);
    let file = plant_write(&home, "g.write", b"broken\n");
    let (a, b) = (
        Pause::new(&home.env.root().join("pause-a")),
        Pause::new(&home.env.root().join("pause-b")),
    );
    let args = ["doctor", "fix", "--only", WRITE];
    log.step("both plan");
    let first = cmd(&log, &home, &args)
        .failpoints("doctor.fix.planned=pause")
        .pause(&a)
        .spawn();
    let second = cmd(&log, &home, &args)
        .failpoints("doctor.fix.planned=pause")
        .pause(&b)
        .spawn();
    log.reached(&a, "doctor.fix.planned");
    log.reached(&b, "doctor.fix.planned");
    log.step("the first repairs");
    log.resume(&a, "doctor.fix.planned");
    let first = checked(first.wait());
    first.ok();
    log.assert_eq(
        "first actions",
        &first.envelope()["summary"]["actions_taken"],
        &json!(1),
    );
    log.step("the second re-plans");
    log.resume(&b, "doctor.fix.planned");
    let second = checked(second.wait());
    second.ok();
    log.assert_eq(
        "second actions",
        &second.envelope()["summary"]["actions_taken"],
        &json!(0),
    );
    assert!(!run_dir(&second).join("actions.jsonl").exists());
    log.assert_eq("file", &fs::read(&file).unwrap(), &REPAIRED.to_vec());
    let backups: usize = plant::runs(&home)
        .iter()
        .map(|r| files_in(&home.state().join("doctor/runs").join(r).join("backups")).len())
        .sum();
    log.assert_eq("backups in total", &backups, &1);
}

/// 11, variant: the first holds the lock mid-repair past the second's timeout.
#[test]
fn second_fixer_times_out() {
    let log = tlog!();
    let home = home(&log);
    let file = plant_write(&home, "g.write", b"broken\n");
    let a = Pause::new(&home.env.root().join("pause-a"));
    let args = ["doctor", "fix", "--only", WRITE];
    log.step("the first stops holding the lock");
    let first = cmd(&log, &home, &args)
        .failpoints("doctor.repair.prepared=pause")
        .pause(&a)
        .spawn();
    log.reached(&a, "doctor.repair.prepared");
    log.step("the second waits, then gives up");
    let second = run(&log, &home, &args);
    second.fails(5, "locked");
    assert!(second.elapsed_ms >= 5_000, "{} ms", second.elapsed_ms);
    log.step("the first finishes");
    log.resume(&a, "doctor.repair.prepared");
    checked(first.wait()).ok();
    log.assert_eq("file", &fs::read(&file).unwrap(), &REPAIRED.to_vec());
}

/// 12. The disabled flatten runs only by id, keeping each folder's identity.
#[test]
fn disabled_flatten_runs_only_by_id() {
    let log = tlog!();
    let home = home(&log);
    register(&home, "retail");
    let addons = home.addons("_retail_");
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    plant_flatten(&home, "_retail_", "TestWrapB", "TestWrapB");
    let inner = ino(&addons.join("TestWrapA/Inner"));
    let nested = ino(&addons.join("TestWrapB/TestWrapB"));
    log.tree("addons", &addons);

    log.step("a default fix leaves them");
    let ran = run(&log, &home, &["doctor", "fix"]);
    ran.exit(1);
    let flatten: Vec<&Value> = ran.envelope()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["id"] == FLATTEN)
        .collect();
    log.assert_eq("listed", &flatten.len(), &2);
    for f in &flatten {
        log.assert_eq(
            "auto_fixable",
            &f["remediation"]["auto_fixable"],
            &json!(false),
        );
        log.assert_eq(
            "remediation",
            &f["remediation"]["argv"],
            &json!([
                "omaddons", "doctor", "fix", "--only", FLATTEN, "-t", "retail"
            ]),
        );
    }
    log.assert_unchanged("addons", &addons);

    log.step("by id");
    let ran = run(&log, &home, &["doctor", "fix", "--only", FLATTEN]);
    ran.ok();
    log.assert_eq("Inner", &ino(&addons.join("Inner")), &inner);
    log.assert_eq("TestWrapB", &ino(&addons.join("TestWrapB")), &nested);
    let tx = text(&ran.envelope()["tx"]).to_owned();
    let quarantine = home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots")
        .join(format!("{tx}.repair/quarantine"));
    let set_aside = files_in(&quarantine);
    assert!(!set_aside.is_empty(), "{}", quarantine.display());
    let actions = ran.envelope()["actions"].as_array().unwrap();
    log.assert_eq("steps (2 + 3)", &actions.len(), &5);
    let dir = run_dir(&ran);
    assert_in_scopes(
        &log,
        &dir,
        &[
            addons.clone(),
            home.client("_retail_").join("Interface/.omaddons"),
        ],
    );
}

/// 12, variant: refused while the client runs.
#[test]
fn flatten_refused_while_running() {
    let log = tlog!();
    let mut home = home(&log);
    register(&home, "retail");
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    let addons = home.addons("_retail_");
    log.tree("addons", &addons);
    home.retail_running();
    let ran = run(&log, &home, &["doctor", "fix", "--only", FLATTEN]);
    ran.fails(4, "doctor_refused");
    log.assert_eq(
        "reason",
        &ran.envelope()["error"]["details"]["reason"],
        &json!("client_running"),
    );
    log.assert_unchanged("addons", &addons);
}

/// 13. Each side's set-aside lands in its own side's root, keeping its identity.
#[test]
fn quarantine_on_each_side() {
    let log = tlog!();
    let home = home(&log);
    register(&home, "retail");
    let planted = [
        plant_quarantine(&home, Side::Config, "c.quarantine", Kind::File),
        plant_quarantine(&home, Side::Cache, "k.quarantine", Kind::Dir),
        plant_quarantine(&home, Side::Client("_retail_"), "r.quarantine", Kind::File),
    ];
    let inodes: Vec<u64> = planted.iter().map(|p| ino(p)).collect();
    let ran = run(
        &log,
        &home,
        &["doctor", "fix", "--only", QUARANTINE, "--force", "--yes"],
    );
    ran.ok();
    let run_id = text(&ran.envelope()["run_id"]).to_owned();
    let tx = text(&ran.envelope()["tx"]).to_owned();
    let dir = run_dir(&ran);
    let meta = home.client("_retail_").join("Interface/.omaddons");
    let roots = [
        dir.join("quarantine/config"),
        home.env.cache().join("omaddons/quarantine").join(&run_id),
        meta.join("snapshots")
            .join(format!("{tx}.repair/quarantine")),
    ];
    for ((path, inode), root) in planted.iter().zip(&inodes).zip(&roots) {
        let action = ran.envelope()["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["path"] == json!(path.display().to_string()))
            .unwrap();
        let to = PathBuf::from(text(&action["to"]));
        assert!(
            to.starts_with(root),
            "{} not below {}",
            to.display(),
            root.display()
        );
        log.assert_eq("same inode", &ino(&to), inode);
        assert!(fs::symlink_metadata(path).is_err());
    }
    assert_in_scopes(
        &log,
        &dir,
        &[
            home.state(),
            home.env.config().join("omaddons"),
            home.env.cache().join("omaddons"),
            meta,
        ],
    );
}

/// 14. `doctor diff` previews and binds, and compares with a run, writing nothing.
#[test]
fn diff_previews_and_binds() {
    let log = tlog!();
    let home = home(&log);
    plant_write(&home, "h.write", b"broken\n");
    log.step("diff");
    log.tree("state", &home.env.state());
    let diff = run(&log, &home, &["doctor", "diff"]);
    diff.exit(1);
    log.assert_unchanged("state", &home.env.state());
    let preview = run(&log, &home, &["doctor", "fix", "--dry-run"]);
    log.assert_eq(
        "actions_planned",
        &diff.envelope()["actions_planned"],
        &preview.envelope()["actions_planned"],
    );
    log.assert_eq("since", &diff.envelope()["since"], &Value::Null);
    let token = token_of(&log, "diff token", &diff);
    log.assert_eq("token", &json!(token), &preview.envelope()["precondition"]);
    let preview_run = text(&preview.envelope()["run_id"]).to_owned();

    log.step("bound to diff's token");
    run(&log, &home, &["doctor", "fix", "--if-match", &token]).ok();

    log.step("since the preview");
    let since = run(&log, &home, &["doctor", "diff", &preview_run]);
    since.ok();
    let resolved = &since.envelope()["since"]["resolved_instances"];
    assert!(resolved.to_string().contains("h.write"), "{resolved}");
    run(&log, &home, &["doctor", "diff", "latest"]).ok();
    let unknown = run(
        &log,
        &home,
        &[
            "doctor",
            "diff",
            "2026-01-01T00-00-00Z__01AAAAAAAAAAAAAAAAAAAAAAAA",
        ],
    );
    unknown.fails(66, "not_found");
}

/// 15. A token moves only with what it covers: content, not times; a directory's stamp.
#[test]
fn tokens_stay_put() {
    let log = tlog!();
    let home = home(&log);
    let file = plant_write(&home, "i.write", b"broken\n");
    let preview = |label: &str, args: &[&str]| {
        let mut argv = vec!["doctor", "fix", "--dry-run"];
        argv.extend(args);
        token_of(&log, label, &run(&log, &home, &argv))
    };
    let first = preview("first", &[]);
    log.assert_eq("again", &preview("again", &[]), &first);
    // ubs:ignore — a file time for a `touch`, not a secret
    let later = std::time::SystemTime::now() + Duration::from_secs(3_600);
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(later)
        .unwrap();
    log.assert_eq("after touch", &preview("after touch", &[]), &first);
    fs::write(&file, b"broken differently\n").unwrap();
    let changed = preview("after a content change", &[]);
    let (was, now) = (parts(&first), parts(&changed));
    log.assert_eq(
        "command, targets, selection",
        &now[..3].to_vec(),
        &was[..3].to_vec(),
    );
    assert_ne!(now[3], was[3], "observed moves");

    log.step("a directory's stamp");
    let dir = plant_quarantine(&home, Side::State, "j.quarantine", Kind::Dir);
    let forced = ["--only", QUARANTINE, "--force", "--yes"];
    let before = preview("directory", &forced);
    fs::File::options()
        .write(true)
        .open(dir.join("inside.txt"))
        .unwrap()
        .set_modified(later)
        .unwrap();
    let after = preview("directory touched", &forced);
    assert_ne!(
        after, before,
        "a directory's stamp covers its entries' times"
    );
}

/// 16. No output names an account, realm or character, and `WTF/` is never touched.
#[test]
fn privacy_on_every_channel() {
    let log = tlog!();
    let home = home(&log);
    register(&home, "retail");
    let wtf = home.client("_retail_").join("WTF");
    let sv = home.client("_retail_").join(CHARACTER).join("x.lua");
    fs::create_dir_all(sv.parent().unwrap()).unwrap();
    fs::write(&sv, b"x = 1\n").unwrap();
    log.tree("wtf", &wtf);
    plant_write(&home, "a.write", b"broken\n");
    plant_flatten(&home, "_retail_", "TestWrapA", "Inner");
    // `run` asserts privacy on stdout and stderr of each.
    run(&log, &home, &["doctor", "fix", "--only", WRITE]).ok();
    run(
        &log,
        &home,
        &[
            "doctor",
            "fix",
            "--only",
            ALL,
            "--force",
            "--yes",
            "--dry-run",
        ],
    );
    run(&log, &home, &["doctor", "fix", "--only", FLATTEN]).ok();
    log.assert_unchanged("wtf", &wtf);
}

/// The test fixers' fixtures round trip: corrupted, found, repaired by their own
/// invocation, healthy.
#[test]
fn test_fixer_fixtures_round_trip() {
    let log = tlog!();
    for fixture in test_fixers() {
        let log = log.case(fixture.id());
        let home = home(&log);
        fixture.corrupt(&home);
        let only = fixture.id();
        let mut args = vec!["doctor", "fix", "--only", only];
        if only == QUARANTINE {
            args.extend(["--force", "--yes"]);
        }
        run(&log, &home, &args).ok();
        fixture.assert_healthy(&home);
    }
}
