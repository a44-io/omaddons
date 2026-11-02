//! F2 s5 (omad-m4-agent-surface-doctor-c6cm.6.2.5): the set-aside of the six stuck
//! journal states (rows 3-8) end to end, through the binary: its consent gate, what it
//! moves and that it unblocks the client, its preconditions, its guarded reactivation by
//! `doctor undo`, and its attribution without the payload. Every command runs with
//! `--json --progress json` through `checked` (the doctor envelope contract and
//! `assert_private_all`); each row runs on its own fresh home.

use std::collections::BTreeMap;

use super::*;
use crate::common::archives::zip_text;
use crate::common::trees::tree_hash;
use fixture::fixtures::{
    fm_concurrency_primitives_journal_marker_mismatch::JournalMarkerMismatch,
    fm_concurrency_primitives_journal_step_unrecognized::JournalStepUnrecognized,
    fm_concurrency_primitives_journal_too_new::JournalTooNew,
    fm_concurrency_primitives_journal_unattributed::JournalUnattributed,
    fm_concurrency_primitives_journal_unexpected_state::JournalUnexpectedState,
    fm_concurrency_primitives_journal_unreadable::JournalUnreadable,
};

const TOO_NEW: &str = "fm-concurrency_primitives-journal-too-new";
const UNREADABLE: &str = "fm-concurrency_primitives-journal-unreadable";
pub(super) const UNEXPECTED: &str = "fm-concurrency_primitives-journal-unexpected-state";
pub(super) const STEP: &str = "fm-concurrency_primitives-journal-step-unrecognized";
const MISMATCH: &str = "fm-concurrency_primitives-journal-marker-mismatch";
const UNATTRIBUTED: &str = "fm-concurrency_primitives-journal-unattributed";

/// One stuck row: its id, its fixture, the client it is on (`None`: the machine's), and
/// the `details.reason` a change it blocks fails with.
pub(super) struct Row {
    pub(super) fm: &'static str,
    pub(super) fixture: Box<dyn Fixture>,
    pub(super) target: Option<&'static str>,
    reason: &'static str,
}

fn rows() -> Vec<Row> {
    let row = |fm, fixture: Box<dyn Fixture>, target, reason| Row {
        fm,
        fixture,
        target,
        reason,
    };
    vec![
        row(
            TOO_NEW,
            Box::<JournalTooNew>::default(),
            Some("retail"),
            "journal_too_new",
        ),
        row(
            UNREADABLE,
            Box::<JournalUnreadable>::default(),
            Some("retail"),
            "journal_unreadable",
        ),
        row(
            UNEXPECTED,
            Box::<JournalUnexpectedState>::default(),
            Some("retail"),
            "unexpected_state",
        ),
        row(
            STEP,
            Box::<JournalStepUnrecognized>::default(),
            Some("retail"),
            "step_unrecognized",
        ),
        row(
            MISMATCH,
            Box::<JournalMarkerMismatch>::default(),
            Some("retail"),
            "marker_mismatch",
        ),
        row(
            UNATTRIBUTED,
            Box::<JournalUnattributed>::default(),
            None,
            "unattributed",
        ),
    ]
}

/// A home with a retail and a Classic client, as the journal fixtures plant on.
fn two_clients(log: &TestLog) -> Home {
    let home = Home::new(&["wow", "wow_classic"]);
    log.env(&home);
    home
}

/// `row` planted on a fresh home, its set recorded.
pub(super) fn planted(log: &TestLog, row: &Row) -> Home {
    let home = two_clients(log);
    log.step("plant");
    // The machine's row names no client; on a home in use both are registered, so its
    // set-aside guards each of them.
    if row.target.is_none() {
        register(&home, "retail");
        register(&home, "classic");
    }
    row.fixture.corrupt(&home);
    log.json("planted", &plant::take_planted());
    home
}

/// Each path of `paths` by what it holds: a file's mode and bytes, a directory's tree, or
/// nothing.
pub(super) fn hashes(paths: &[PathBuf]) -> BTreeMap<PathBuf, Option<String>> {
    paths
        .iter()
        .map(|path| {
            let held = match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => Some(format!("dir {}", tree_hash(path))),
                Ok(meta) => Some(format!(
                    "file {:o} {:?}",
                    meta.permissions().mode() & 0o7777,
                    fs::read(path).unwrap()
                )),
                Err(_) => None,
            };
            (path.clone(), held)
        })
        .collect()
}

/// `doctor fix --only <fm> [-t <client>] --force --yes` plus `extra`.
fn set_aside(log: &TestLog, home: &Home, row: &Row, extra: &[&str]) -> Ran {
    let mut args = vec!["doctor", "fix", "--only", row.fm];
    if let Some(target) = row.target {
        args.extend(["-t", target]);
    }
    args.extend(["--force", "--yes"]);
    args.extend(extra);
    run(log, home, &args)
}

/// A set-aside that must succeed: its run id.
pub(super) fn set_aside_ok(log: &TestLog, home: &Home, row: &Row) -> String {
    log.step("set-aside");
    let ran = set_aside(log, home, row, &[]);
    ran.ok();
    log.json("actions", &ran.envelope()["actions"]);
    log.assert_eq("state", &ran.envelope()["state"], &json!("DONE_OK"));
    text(&ran.envelope()["run_id"]).to_owned()
}

/// A change on `target`: an install of a local archive, which recovers first.
pub(super) fn change_on(log: &TestLog, home: &Home, target: &str, name: &str) -> Ran {
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let folder = plant::folder(name);
    let zip = dl.join(format!("{name}.zip"));
    let toc = "## Interface: 120105, 50504, 11509\n## Version: v1\n";
    fs::write(&zip, zip_text(&[(&format!("{folder}/{folder}.toc"), toc)])).unwrap();
    let zip = zip.display().to_string();
    let r = format!("gh:o/{name}");
    run(
        log,
        home,
        &["install", &r, "--file", &zip, "-t", target, "--offline"],
    )
}

/// A refusal's error, logged.
fn refusal(log: &TestLog, ran: &Ran) -> Value {
    let error = ran.envelope()["error"].clone();
    log.json("error", &error);
    error
}

/// The clients a row blocks: its own, or for the machine's row every client.
fn blocked(row: &Row) -> Vec<&'static str> {
    match row.target {
        Some(target) => vec![target],
        None => vec!["retail", "classic"],
    }
}

/// What a change on `target` reports: its error's code and `details.reason`.
pub(super) fn stuck_state(log: &TestLog, home: &Home, target: &str) -> (String, String) {
    let ran = change_on(log, home, target, "probe");
    assert_ne!(ran.run.code, 0, "a stuck journal blocks {target}");
    let error = &ran.envelope()["error"];
    (
        text(&error["code"]).to_owned(),
        text(&error["details"]["reason"]).to_owned(),
    )
}

// ---------------------------------------------------------------------------------------
// 1-2. The gate, and the client unblocked.

#[test]
fn set_aside_gate_matrix() {
    let log = tlog!();
    for row in rows() {
        let log = log.case(row.fm);
        let home = planted(&log, &row);
        let set = row.fixture.subject(&home);
        let before = hashes(&set);
        let unchanged = |log: &TestLog, what: &str| {
            log.assert_eq(&format!("{what}: the set"), &hashes(&set), &before);
        };
        let target: Vec<&str> = row.target.iter().flat_map(|t| ["-t", *t]).collect();

        log.step("without --force");
        let mut args = vec!["doctor", "fix", "--only", row.fm];
        args.extend(&target);
        let refused = run(&log, &home, &args);
        refused.fails(4, "doctor_refused");
        let error = refusal(&log, &refused);
        log.assert_eq(
            "reason",
            &error["details"]["reason"],
            &json!("force_required"),
        );
        log.assert_eq("fm", &error["details"]["fm"], &json!(row.fm));
        let fix = &error["fix_argv"];
        log.assert_eq("its fix explains", &fix[2], &json!("explain"));
        let words: Vec<String> = fix.as_array().unwrap()[1..]
            .iter()
            .map(|w| w.as_str().unwrap().to_owned())
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        run(&log, &home, &words).ok();
        unchanged(&log, "force_required");

        log.step("--force without --yes");
        let mut args = vec!["doctor", "fix", "--only", row.fm];
        args.extend(&target);
        args.push("--force");
        let usage = run(&log, &home, &args);
        usage.fails(64, "usage");
        log.assert_eq(
            "flag",
            &refusal(&log, &usage)["details"]["flag"],
            &json!("--force"),
        );
        unchanged(&log, "--force alone");

        log.step("--force --yes without --only");
        let usage = run(&log, &home, &["doctor", "fix", "--force", "--yes"]);
        usage.fails(64, "usage");
        unchanged(&log, "no --only");

        log.step("a default fix");
        let default = run(&log, &home, &["doctor", "fix"]);
        default.exit(1);
        log.assert_eq(
            "nothing done",
            &default.envelope()["summary"]["actions_taken"],
            &json!(0),
        );
        unchanged(&log, "a default fix");

        log.step("a subsystem's --only");
        let subsystem = run(
            &log,
            &home,
            &[
                "doctor",
                "fix",
                "--only",
                "concurrency_primitives",
                "--force",
                "--yes",
            ],
        );
        log.note(format!("exit {}", subsystem.run.code));
        assert_ne!(subsystem.run.code, 0, "a subsystem never enables it");
        unchanged(&log, "a subsystem");

        log.step("its id, --force and --yes");
        set_aside_ok(&log, &home, &row);
    }
}

#[test]
fn set_aside_unblocks_the_client() {
    let log = tlog!();
    for row in rows() {
        let log = log.case(row.fm);
        let home = planted(&log, &row);
        log.step("before");
        for target in blocked(&row) {
            let (code, reason) = stuck_state(&log, &home, target);
            log.note(format!("{target}: {code} ({reason})"));
            log.assert_eq(&format!("{target}'s reason"), &reason.as_str(), &row.reason);
        }
        set_aside_ok(&log, &home, &row);
        log.step("after");
        for target in blocked(&row) {
            change_on(&log, &home, target, &format!("after{target}")).ok();
        }
    }
}

// ---------------------------------------------------------------------------------------
// 10-11. The guarded reactivation.

/// No `<tx>.reactivating.jsonl` is left.
fn no_provisional(home: &Home) {
    let left: Vec<String> = files_in(&home.state().join("journal"))
        .into_iter()
        .filter(|name| name.ends_with(".reactivating.jsonl"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn reactivate_restores_the_set_byte_identical() {
    let log = tlog!();
    for row in rows() {
        let log = log.case(row.fm);
        let home = planted(&log, &row);
        let set = row.fixture.subject(&home);
        log.step("before");
        let before = hashes(&set);
        let states: Vec<(String, String)> = blocked(&row)
            .iter()
            .map(|t| stuck_state(&log, &home, t))
            .collect();
        let issues_before = issue_ids(&log, &home);
        let elsewhere_before = pending_elsewhere(&log, &home);
        assert!(
            !elsewhere_before.is_empty(),
            "a command elsewhere reports the journal pending"
        );
        let run_id = set_aside_ok(&log, &home, &row);
        assert!(
            set.iter().all(|p| fs::symlink_metadata(p).is_err()),
            "set aside"
        );
        log.step("undo");
        run(&log, &home, &["doctor", "undo", &run_id]).ok();
        log.step("after");
        log.assert_eq("the set, byte for byte", &hashes(&set), &before);
        no_provisional(&home);
        let after: Vec<(String, String)> = blocked(&row)
            .iter()
            .map(|t| stuck_state(&log, &home, t))
            .collect();
        log.assert_eq("the same stuck state", &after, &states);
        log.assert_eq("the same issues", &issue_ids(&log, &home), &issues_before);
        log.assert_eq(
            "the same warning elsewhere",
            &pending_elsewhere(&log, &home),
            &elsewhere_before,
        );
    }
}

/// The `journal_pending` warnings of a lock-taking command on no client (`config set`):
/// how a stuck journal is reported away from its own client's commands.
fn pending_elsewhere(log: &TestLog, home: &Home) -> Vec<Value> {
    let ran = run(log, home, &["config", "set", "check_interval_hours", "6"]);
    ran.ok();
    let pending: Vec<Value> = ran.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        // ubs:ignore — comparing warning codes, not secrets
        .filter(|w| w["code"] == "journal_pending")
        .cloned()
        .collect();
    log.json("journal_pending elsewhere", &pending);
    pending
}

/// `status`'s issue ids and instances, per client.
fn issue_ids(log: &TestLog, home: &Home) -> Vec<(String, String)> {
    let ran = run(log, home, &["status"]);
    let mut out = Vec::new();
    for target in ran.ok().data()["targets"].as_array().unwrap() {
        for issue in target["issues"].as_array().unwrap() {
            out.push((
                text(&target["id"]).to_owned(),
                text(&issue["id"]).to_owned(),
            ));
        }
    }
    for issue in ran.data()["issues"].as_array().into_iter().flatten() {
        out.push(("machine".to_owned(), text(&issue["id"]).to_owned()));
    }
    out
}

#[test]
fn reactivate_refuses_after_a_later_install() {
    let log = tlog!();
    for row in rows() {
        let log = log.case(row.fm);
        let home = planted(&log, &row);
        let set = row.fixture.subject(&home);
        let run_id = set_aside_ok(&log, &home, &row);
        let quarantine = home
            .state()
            .join("doctor/runs")
            .join(&run_id)
            .join("quarantine");
        log.tree("quarantine", &quarantine);
        log.step("change");
        let changed = row.target.unwrap_or("retail");
        change_on(&log, &home, changed, "later").ok();
        log.step("undo");
        let refused = run(&log, &home, &["doctor", "undo", &run_id]);
        refused.fails(4, "undo_conflict");
        let error = refusal(&log, &refused);
        log.assert_eq(
            "reason",
            &error["details"]["reason"],
            &json!("changed_since_suspension"),
        );
        assert!(
            error["details"]["changed_guards"]
                .as_array()
                .unwrap()
                .contains(&json!("generation")),
            "{error}"
        );
        log.assert_unchanged("quarantine", &quarantine);
        assert!(
            set.iter().all(|p| fs::symlink_metadata(p).is_err()),
            "still set aside"
        );
        assert!(
            home.addons(plant::client_folder(changed))
                .join("Later")
                .is_dir(),
            "the later install stands"
        );
    }
}

/// The row `fm` among [`rows`].
pub(super) fn row_of(fm: &str) -> Row {
    rows().into_iter().find(|r| r.fm == fm).unwrap()
}

/// A set-aside, then `change`, then `doctor undo`, which refuses naming `guard`; the
/// quarantine unchanged. `change` gets the home and the row.
fn refuses_after(log: &TestLog, fm: &str, guard: &str, change: impl Fn(&TestLog, &Home)) {
    let row = row_of(fm);
    let home = planted(log, &row);
    let run_id = set_aside_ok(log, &home, &row);
    let quarantine = home
        .state()
        .join("doctor/runs")
        .join(&run_id)
        .join("quarantine");
    log.tree("quarantine", &quarantine);
    log.step("change");
    change(log, &home);
    log.step("undo");
    let refused = run(log, &home, &["doctor", "undo", &run_id]);
    refused.fails(4, "undo_conflict");
    let error = refusal(log, &refused);
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("changed_since_suspension"),
    );
    log.assert_eq(
        "the changed guard",
        &error["details"]["changed_guards"],
        &json!([guard]),
    );
    log.assert_unchanged("quarantine", &quarantine);
}

#[test]
fn reactivate_refuses_after_a_marker_change() {
    let log = tlog!();
    refuses_after(&log, STEP, "marker", |_, home| {
        let marker = home
            .client("_retail_")
            .join("Interface/.omaddons/target.json");
        let mut bytes = fs::read(&marker).unwrap();
        bytes.push(b'\n');
        fs::write(&marker, bytes).unwrap();
    });
}

#[test]
fn reactivate_refuses_after_a_folder_epoch_changed() {
    let log = tlog!();
    // Row 6's step renames its staged folder into `AddOns/Unrecognized`: a folder there
    // now is a change to what the journal would recover.
    refuses_after(&log, STEP, "epochs", |_, home| {
        let folder = home.addons("_retail_").join("Unrecognized");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Unrecognized.lua"), "-- played with\n").unwrap();
    });
}

#[test]
fn reactivate_refuses_after_wtf_changed() {
    let log = tlog!();
    refuses_after(&log, UNEXPECTED, "epochs", |_, home| {
        let sv = home
            .client("_retail_")
            .join(crate::common::privacy::ACCOUNT)
            .join("Foo.lua");
        fs::create_dir_all(sv.parent().unwrap()).unwrap();
        fs::write(&sv, "FooDB = { played = true }\n").unwrap();
    });
}

#[test]
fn reactivate_bare_doctor_undo_selects_the_set_aside_after_a_diagnose() {
    let log = tlog!();
    let row = row_of(TOO_NEW);
    let home = planted(&log, &row);
    let set = row.fixture.subject(&home);
    let before = hashes(&set);
    let run_id = set_aside_ok(&log, &home, &row);
    log.step("a diagnose");
    run(&log, &home, &["doctor"]);
    log.step("bare undo");
    let undone = run(&log, &home, &["doctor", "undo"]);
    undone.ok();
    log.assert_eq(
        "its run",
        &undone.envelope()["reverted_run"],
        &json!(run_id),
    );
    log.assert_eq("the set is back", &hashes(&set), &before);
}

// ---------------------------------------------------------------------------------------
// 3-8. What it moves, binding, preconditions, retention, explain, a repair's journal.

/// `(path, the destination's place within its quarantine)` for each primitive of a
/// report's `actions` or `actions_planned`, in order: the run and repair ids differ
/// between a preview and its run, nothing else.
fn moves(actions: &Value) -> Vec<(String, String)> {
    actions
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let to = text(&a["to"]);
            let within = to
                .split_once("/quarantine/")
                .map_or(to, |(_, rest)| rest)
                .to_owned();
            (text(&a["path"]).to_owned(), within)
        })
        .collect()
}

#[test]
fn set_aside_moves_exactly_the_attributed_set() {
    let log = tlog!();
    for fm in [TOO_NEW, UNEXPECTED, STEP, UNATTRIBUTED] {
        let log = log.case(fm);
        let row = row_of(fm);
        let home = planted(&log, &row);
        let set = row.fixture.subject(&home);
        let before = hashes(&set);
        for client in ["_retail_", "_classic_"] {
            log.tree(&format!("{client} AddOns"), &home.addons(client));
        }
        log.step("preview");
        let preview = set_aside(&log, &home, &row, &["--dry-run"]);
        let planned = preview.envelope()["actions_planned"].clone();
        log.json("actions_planned", &planned);
        let run = set_aside(&log, &home, &row, &[]);
        run.ok();
        let report = run.envelope();
        log.json("actions", &report["actions"]);
        log.assert_eq(
            "the preview's moves, in order",
            &moves(&report["actions"]),
            &moves(&planned),
        );
        let actions = report["actions"].as_array().unwrap();
        assert_eq!(actions.len(), set.len(), "one rename per artifact");
        let journal = actions.last().unwrap();
        assert!(
            text(&journal["path"]).contains("/journal/"),
            "the journal moves last: {journal}"
        );
        let repair_tx = text(&report["tx"]).to_owned();
        let run_dir = PathBuf::from(text(&report["run_dir"]));
        for action in actions {
            log.assert_eq("a rename", &action["op"], &json!("rename_noreplace"));
            let (from, to) = (
                PathBuf::from(text(&action["path"])),
                PathBuf::from(text(&action["to"])),
            );
            let state_side = to.starts_with(run_dir.join("quarantine/state"));
            let client_side = to
                .components()
                .any(|c| c.as_os_str() == format!("{repair_tx}.repair").as_str())
                && to
                    .to_string_lossy()
                    .contains("/Interface/.omaddons/snapshots/");
            assert!(
                state_side || client_side,
                "{} lies in no quarantine",
                to.display()
            );
            assert!(
                fs::symlink_metadata(&from).is_err(),
                "{} is moved",
                from.display()
            );
            let was = before.get(&from).cloned().flatten();
            log.assert_eq(
                &format!("{} intact", from.display()),
                &hashes(std::slice::from_ref(&to))[&to],
                &was,
            );
        }
        log.assert_eq(
            "nothing copied",
            &report["summary"]["bytes_backed_up"],
            &json!(0),
        );
        for client in ["_retail_", "_classic_"] {
            log.assert_unchanged(&format!("{client} AddOns"), &home.addons(client));
        }
        let history = run_history(&log, &home);
        let line = history
            .iter()
            .find(|e| e["tx"] == json!(repair_tx))
            .unwrap();
        log.json("history line", line);
        log.assert_eq("kind", &line["kind"], &json!("repair"));
        log.assert_eq("target", &line["target"], &Value::Null);
        let repairs = line["repairs"].as_array().unwrap();
        assert_eq!(repairs.len(), actions.len(), "{line}");
        let guarded: Vec<bool> = repairs
            .iter()
            .map(|r| r["guards"].as_array().is_some_and(|g| !g.is_empty()))
            .collect();
        let mut want = vec![false; guarded.len()];
        *want.last_mut().unwrap() = true;
        log.assert_eq("guards on the journal's primitive alone", &guarded, &want);
    }
}

/// History's entries, as `history --json` lists them.
fn run_history(log: &TestLog, home: &Home) -> Vec<Value> {
    run(log, home, &["history"]).ok().data()["entries"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn set_aside_dry_run_writes_nothing_and_binds() {
    let log = tlog!();
    let row = row_of(STEP);
    let home = planted(&log, &row);
    let set = row.fixture.subject(&home);
    let before = hashes(&set);
    log.tree("state", &home.env.state());
    log.step("preview");
    let preview = set_aside(&log, &home, &row, &["--dry-run"]);
    let token = text(&preview.envelope()["precondition"]).to_owned();
    log.assert_eq(
        "the dry run wrote nothing outside its run",
        &state_changed_outside_doctor(&log, &home),
        &Vec::<String>::new(),
    );
    log.step("the stage changes");
    let stage = set
        .iter()
        .find(|p| p.to_string_lossy().contains("/stage/"))
        .unwrap();
    fs::write(stage.join("added.txt"), "added\n").unwrap();
    let changed = hashes(&set);
    let stale = set_aside(&log, &home, &row, &["--if-match", &token]);
    stale.fails(4, "plan_stale");
    log.assert_eq("nothing moved", &hashes(&set), &changed);
    log.step("a fresh preview binds");
    let fresh = set_aside(&log, &home, &row, &["--dry-run"]);
    let token = text(&fresh.envelope()["precondition"]).to_owned();
    set_aside(&log, &home, &row, &["--if-match", &token]).ok();
    assert_ne!(hashes(&set), before);
}

#[test]
fn set_aside_refused_while_the_game_runs() {
    let log = tlog!();
    // A WTF/ restore's journal (row 5) is diagnosed as waiting for its game while the
    // game runs (row 2, no fixer): its set-aside then selects nothing and moves nothing.
    {
        let log = log.case("a WTF/ restore waits for its game");
        let row = row_of(UNEXPECTED);
        let home = planted(&log, &row);
        let set = row.fixture.subject(&home);
        let before = hashes(&set);
        let proc = plant::running(&home, "retail");
        let args = [
            "doctor", "fix", "--only", UNEXPECTED, "-t", "retail", "--force", "--yes",
        ];
        let ran = checked(
            cmd(&log, &home, &args)
                .env("OMADDONS_PROC_ROOT", proc.root())
                .run(),
        );
        log.assert_eq(
            "nothing selected",
            &ran.envelope()["summary"]["actions_taken"],
            &json!(0),
        );
        log.assert_eq("nothing moved", &hashes(&set), &before);
    }
    // A folder journal's verdict does not depend on the game: the set-aside refuses.
    let log = log.case("a folder journal's set-aside refuses");
    let row = row_of(STEP);
    let home = planted(&log, &row);
    let set = row.fixture.subject(&home);
    let before = hashes(&set);
    let args = [
        "doctor", "fix", "--only", STEP, "-t", "retail", "--force", "--yes",
    ];
    let refused_with = |log: &TestLog, root: &Path| {
        log.note(format!("OMADDONS_PROC_ROOT {}", root.display()));
        let ran = checked(cmd(log, &home, &args).env("OMADDONS_PROC_ROOT", root).run());
        ran.fails(4, "doctor_refused");
        let error = refusal(log, &ran);
        log.assert_eq(
            "reason",
            &error["details"]["reason"],
            &json!("client_running"),
        );
        log.assert_eq("fm", &error["details"]["fm"], &json!(STEP));
        log.assert_eq("nothing moved", &hashes(&set), &before);
    };
    log.step("the game runs");
    let proc = plant::running(&home, "retail");
    refused_with(&log, proc.root());
    log.step("the process table cannot be read");
    let unreadable = home.env.root().join("proc-a-file");
    fs::write(&unreadable, "not a directory\n").unwrap();
    refused_with(&log, &unreadable);
    log.step("the game is gone");
    set_aside_ok(&log, &home, &row);
}

#[test]
fn set_aside_survives_retention_and_the_orphan_sweep() {
    let log = tlog!();
    let row = row_of(TOO_NEW);
    let home = planted(&log, &row);
    let run_id = set_aside_ok(&log, &home, &row);
    let quarantines = [
        home.state()
            .join("doctor/runs")
            .join(&run_id)
            .join("quarantine"),
        home.client("_retail_")
            .join("Interface/.omaddons/snapshots"),
    ];
    for (n, dir) in quarantines.iter().enumerate() {
        log.tree(&format!("quarantine {n}"), dir);
    }
    log.step("age everything past the orphan sweep's threshold");
    let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    for dir in [
        home.client("_retail_").join("Interface/.omaddons/stage"),
        home.client("_retail_")
            .join("Interface/.omaddons/snapshots"),
        home.state().join("snapshots"),
        home.state().join("inflight"),
        home.state()
            .join("doctor/runs")
            .join(&run_id)
            .join("quarantine"),
    ] {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if let Ok(file) = fs::File::open(entry.path()) {
                file.set_modified(past).ok();
            }
        }
    }
    for (n, dir) in quarantines.iter().enumerate() {
        log.tree(&format!("quarantine {n}"), dir);
    }
    log.step("two changes run retention and the sweep");
    change_on(&log, &home, "retail", "first").ok();
    change_on(&log, &home, "retail", "second").ok();
    log.assert_unchanged("quarantine 0", &quarantines[0]);
    let set_aside_half = home
        .client("_retail_")
        .join("Interface/.omaddons/snapshots");
    let kept: Vec<String> = files_in(&set_aside_half)
        .into_iter()
        .filter(|name| name.ends_with(".repair"))
        .collect();
    assert_eq!(kept.len(), 1, "the client quarantine is kept: {kept:?}");
    log.step("its undo refuses now, and the set stays");
    let refused = run(&log, &home, &["doctor", "undo", &run_id]);
    refused.fails(4, "undo_conflict");
    log.assert_unchanged("quarantine 0", &quarantines[0]);
}

#[test]
fn set_aside_explain_names_the_set_and_the_condition() {
    let log = tlog!();
    for fm in [TOO_NEW, MISMATCH] {
        let log = log.case(fm);
        let row = row_of(fm);
        let home = planted(&log, &row);
        let explained = run(&log, &home, &["doctor", "explain", fm, "-t", "retail"]);
        explained.ok();
        let report = explained.envelope();
        let fixer = &report["failure_mode"]["fixer"];
        log.assert_eq("needs force", &fixer["needs_force"], &json!(true));
        log.assert_eq("disabled", &fixer["enabled"], &json!(false));
        let why = text(&report["failure_mode"]["why"]);
        for must in ["quarantine", "deletes nothing", "undo refuses"] {
            assert!(why.contains(must), "{must:?}: {why}");
        }
        if fm == TOO_NEW {
            assert!(why.contains("newer build"), "{why}");
        }
        let evidence = &report["instances"][0]["evidence"];
        assert!(evidence["attribution"].is_string(), "{evidence}");
        assert!(evidence["unattributed_dirs"].is_array(), "{evidence}");
        let human = log
            .cmd(&home, &["doctor", "explain", fm, "-t", "retail"])
            .env(ENV.0, ENV.1)
            .run();
        human.ok().assert_private();
        for line in ["the set:", "may hold the only copy", "attribution:"] {
            assert!(
                human.stdout().contains(line),
                "{line:?} in:\n{}",
                human.stdout()
            );
        }
    }
}

#[test]
fn set_aside_of_an_unreadable_repair_journal() {
    let log = tlog!();
    let home = two_clients(&log);
    log.step("plant");
    register(&home, "retail");
    let tx = omaddons::model::TxId::new().to_string();
    let journal = home.state().join(format!("journal/{tx}.repair.jsonl"));
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(
        &journal,
        "{\"type\":\"prepared\",\"format\":\"1.0\",\"tx\":\"not a ulid\"}\n",
    )
    .unwrap();
    log.step("before");
    let blocked = change_on(&log, &home, "retail", "probe");
    assert_ne!(
        blocked.run.code, 0,
        "an unreadable repair fails every change closed"
    );
    let error = refusal(&log, &blocked);
    log.assert_eq("code", &error["code"], &json!("state_corrupt"));
    log.assert_eq(
        "reason",
        &error["details"]["reason"],
        &json!("journal_unreadable"),
    );
    log.step("diagnose");
    let diagnosed = run(&log, &home, &["doctor"]);
    let found: Vec<Value> = diagnosed.envelope()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["evidence"]["tx"] == json!(tx))
        .cloned()
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    let finding = &found[0];
    log.json("finding", finding);
    log.assert_eq("row 4", &finding["id"], &json!(UNREADABLE));
    log.assert_eq("family", &finding["evidence"]["family"], &json!("repair"));
    log.assert_eq(
        "attribution",
        &finding["evidence"]["attribution"],
        &json!("partial"),
    );
    let runs = home.state().join("doctor/runs").display().to_string();
    assert!(
        finding["evidence"]["unattributed_dirs"]
            .as_array()
            .unwrap()
            .contains(&json!(runs)),
        "{finding}"
    );
    log.step("set-aside");
    let fm = text(&finding["id"]).to_owned();
    run(
        &log,
        &home,
        &["doctor", "fix", "--only", &fm, "--force", "--yes"],
    )
    .ok();
    assert!(!journal.exists(), "set aside");
    log.step("after");
    change_on(&log, &home, "retail", "after").ok();
}

// ---------------------------------------------------------------------------------------
// 9. Privacy.

#[test]
fn set_aside_privacy() {
    let log = tlog!();
    let row = row_of(UNEXPECTED);
    let home = planted(&log, &row);
    let diagnosed = run(&log, &home, &["doctor"]);
    let evidence: Vec<Value> = diagnosed.envelope()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|f| f["evidence"].clone())
        .collect();
    log.json("evidence", &evidence);
    no_path_below_wtf("the findings' evidence", &Value::from(evidence).to_string());
    run(
        &log,
        &home,
        &["doctor", "explain", UNEXPECTED, "-t", "retail"],
    )
    .ok();
    set_aside(&log, &home, &row, &["--dry-run"]);
    let run_id = set_aside_ok(&log, &home, &row);
    run(&log, &home, &["status"]).ok();
    run(&log, &home, &["history"]).ok();
    run(&log, &home, &["doctor", "undo", &run_id]).ok();
    for path in [
        home.state().join("history.jsonl"),
        home.state()
            .join("doctor/runs")
            .join(&run_id)
            .join("report.json"),
    ] {
        let text = fs::read_to_string(&path).unwrap();
        crate::common::privacy::assert_private(&text);
        // The history line's repairs and guards, and the report's evidence and
        // `unattributed_dirs`.
        no_path_below_wtf(&path.display().to_string(), &text);
    }
}

/// No path below a client's `WTF/` in `text` (M3-D45: a settings journal's set is named
/// by its stage and snapshots, never by what it restores).
fn no_path_below_wtf(label: &str, text: &str) {
    assert!(
        !text.contains("/WTF/"),
        "{label} holds a path below WTF/: {text}"
    );
}

// ---------------------------------------------------------------------------------------
// 16-19. Attribution without the payload.

/// `doctor --json`'s finding of `fm`.
fn finding_of(log: &TestLog, home: &Home, fm: &str) -> Value {
    let diagnosed = run(log, home, &["doctor"]);
    let found = diagnosed.envelope()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == fm)
        .cloned()
        .unwrap();
    log.json("evidence", &found["evidence"]);
    found
}

fn canonical(path: &Path) -> String {
    fs::canonicalize(path).unwrap().display().to_string()
}

/// Appends `line` (JSON) to the journal of `fm`'s finding.
fn append_to_journal(home: &Home, finding: &Value, line: &Value) {
    use std::io::Write;
    let tx = text(&finding["evidence"]["tx"]).to_owned();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(plant::journal_file(home, &tx))
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

#[test]
fn attribution_unreadable_payload_naming_another_client() {
    let log = tlog!();
    let row = row_of(UNREADABLE);
    let home = planted(&log, &row);
    let classic = home.client("_classic_");
    let named = classic.join("Interface/AddOns/Named");
    fs::create_dir_all(&named).unwrap();
    fs::write(named.join("Named.toc"), "## Interface: 11509\n").unwrap();
    let before = finding_of(&log, &home, UNREADABLE);
    append_to_journal(&home, &before, &json!({"type": "note", "path": named}));
    log.tree("classic", &classic);
    let found = finding_of(&log, &home, UNREADABLE);
    log.assert_eq(
        "attribution",
        &found["evidence"]["attribution"],
        &json!("partial"),
    );
    log.assert_eq(
        "unattributed",
        &found["evidence"]["unattributed_dirs"],
        &json!([canonical(&classic)]),
    );
    set_aside_ok(&log, &home, &row);
    log.assert_unchanged("classic", &classic);
}

#[test]
fn attribution_marker_mismatch_naming_paths_outside_its_client() {
    let log = tlog!();
    let row = row_of(MISMATCH);
    let home = planted(&log, &row);
    let classic = home.client("_classic_");
    let named = classic.join("Interface/AddOns/Elsewhere");
    fs::create_dir_all(&named).unwrap();
    let before = finding_of(&log, &home, MISMATCH);
    append_to_journal(
        &home,
        &before,
        &json!({"type": "step", "n": 1, "op": "noreplace", "from": named,
            "to": classic.join("Interface/AddOns/Moved"), "purpose": "swap_in",
            "from_id": null, "to_id": null}),
    );
    log.tree("classic", &classic);
    let found = finding_of(&log, &home, MISMATCH);
    log.assert_eq(
        "attribution",
        &found["evidence"]["attribution"],
        &json!("partial"),
    );
    assert!(
        found["evidence"]["unattributed_dirs"]
            .as_array()
            .unwrap()
            .contains(&json!(canonical(&classic))),
        "{found}"
    );
    set_aside_ok(&log, &home, &row);
    log.assert_unchanged("classic", &classic);
}

#[test]
fn attribution_complete_for_a_newer_journal_on_its_own_client() {
    let log = tlog!();
    let row = row_of(TOO_NEW);
    let home = planted(&log, &row);
    let found = finding_of(&log, &home, TOO_NEW);
    log.assert_eq(
        "attribution",
        &found["evidence"]["attribution"],
        &json!("complete"),
    );
    log.assert_eq(
        "unattributed",
        &found["evidence"]["unattributed_dirs"],
        &json!([]),
    );
    let human = log
        .cmd(
            &home,
            &[
                "doctor",
                "fix",
                "--only",
                TOO_NEW,
                "-t",
                "retail",
                "--force",
                "--yes",
                "--dry-run",
            ],
        )
        .env(ENV.0, ENV.1)
        .run();
    assert!(
        human.stdout().contains("attribution complete"),
        "{}",
        human.stdout()
    );
}

#[test]
fn attribution_unverified_client_leftover_is_reported() {
    let log = tlog!();
    let row = row_of(TOO_NEW);
    let home = planted(&log, &row);
    register(&home, "classic");
    let classic = home.client("_classic_");
    fs::remove_file(classic.join("Interface/.omaddons/target.json")).unwrap();
    let tx = text(&finding_of(&log, &home, TOO_NEW)["evidence"]["tx"]).to_owned();
    let leftover = classic.join(format!("Interface/.omaddons/stage/{tx}"));
    fs::create_dir_all(leftover.join("Foo")).unwrap();
    log.tree("leftover", &leftover);
    let found = finding_of(&log, &home, TOO_NEW);
    log.assert_eq(
        "attribution",
        &found["evidence"]["attribution"],
        &json!("partial"),
    );
    assert!(
        found["evidence"]["unattributed_dirs"]
            .as_array()
            .unwrap()
            .contains(&json!(canonical(&classic))),
        "{found}"
    );
    let human = log
        .cmd(
            &home,
            &[
                "doctor",
                "fix",
                "--only",
                TOO_NEW,
                "-t",
                "retail",
                "--force",
                "--yes",
                "--dry-run",
            ],
        )
        .env(ENV.0, ENV.1)
        .run();
    assert!(
        human.stdout().contains("not accounted for:"),
        "{}",
        human.stdout()
    );
    let explained = log
        .cmd(&home, &["doctor", "explain", TOO_NEW, "-t", "retail"])
        .env(ENV.0, ENV.1)
        .run();
    assert!(
        explained.stdout().contains("not protected afterwards"),
        "{}",
        explained.stdout()
    );
    set_aside_ok(&log, &home, &row);
    log.assert_unchanged("leftover", &leftover);
}

// ---------------------------------------------------------------------------------------
// 20. Schemas.

#[test]
fn set_aside_report_validates_against_its_schema() {
    let log = tlog!();
    let row = row_of(TOO_NEW);
    let home = planted(&log, &row);
    let valid = |ran: &Ran| {
        let general = crate::common::doctor::as_general(ran.envelope());
        let validated = super::validate::validate_envelope(&general, ran.stderr());
        assert!(validated.is_ok(), "{}", validated.err().unwrap_or_default());
    };
    valid(&run(&log, &home, &["doctor"]));
    valid(&set_aside(&log, &home, &row, &["--dry-run"]));
    let real = set_aside(&log, &home, &row, &[]);
    real.ok();
    valid(&real);
}
