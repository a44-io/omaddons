//! e2e suite: every production doctor fixer round trips through the binary on its registered fixture: plant, fix, a second fix that does nothing, doctor undo, and a fix aborted at its first step, recovered and retried
//!
//! omad-m4-agent-surface-doctor-c6cm.6.11, which replaces the dropped safety harness (c6cm.6.9,
//! M4-D64). One case per production fixer, each on the fixture its failure mode registers
//! (`tests/doctor/fixtures/`):
//!
//! 1. **Plant** the fixture on a fresh home and record its subject (every entry's kind,
//!    bytes, mode and inode) and the trees of `<state>`, `<cache>`, `<config>` and the client.
//! 2. **Fix** with `doctor fix --only <fm>` (and the fixer's own flags): `DONE_OK`, and the
//!    fixture's `assert_healthy` holds.
//! 3. **Again:** a second identical fix takes no action, commits nothing and writes no
//!    history line.
//! 4. **Undo** with `doctor undo <the fix's run>`: the subject is byte for byte as planted,
//!    with its modes and inodes (every fixer here renames or chmods in place), and nothing
//!    outside the doctor's own record changed across the fix and its undo.
//! 5. **Crash:** on a second fresh plant, the fix aborted at its first step leaves the
//!    subject as planted once the next command that recovers ran, and a retried fix reaches
//!    `DONE_OK`.
//!
//! The delegation (`recover`) differs where its protocol does (M4-D37): it commits no repair
//! transaction, so its inverse is never `doctor undo` (whose run of it "changed nothing",
//! as `delegate_doctor_undo_never_reverts_a_delegation` in `tests/doctor_repair/delegate.rs`
//! also checks), and its first step is the engine's own recovery, whose crash at every step
//! `tests/engine_crash.rs` covers (`engine.recover.*`). Its case asserts that the rollback
//! stands through the undo, and aborts the fix after it planned (`doctor.fix.planned`),
//! where the retried fix is itself the command that recovers.

#![cfg(test)]

use crate::common;
use crate::fixture;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use common::doctor::{check_envelope, plant};
use common::home::Home;
use common::privacy::assert_private_all;
use common::tlog::{Cmd, Ran, TestLog, tlog};
use fixture::Fixture;
use fixture::fixtures::{
    fm_caches_blob_hash_mismatch::BlobHashMismatch,
    fm_concurrency_primitives_journal_pending::JournalPending,
    fm_concurrency_primitives_journal_too_new::JournalTooNew,
    fm_secrets_credential_file_mode::CredentialFileMode,
};

const PENDING: &str = "fm-concurrency_primitives-journal-pending";

/// What `doctor undo` of the fix's run does to the subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inverse {
    /// Puts it back as planted.
    Restores,
    /// Nothing: the fix committed no repair (the delegation, M4-D37), so its result stands.
    Stands,
}

/// One production fixer's case.
struct Case {
    /// The fixture its failure mode registers, fresh.
    fixture: fn() -> Box<dyn Fixture>,
    /// `doctor fix --only <fm>`'s further flags.
    flags: &'static [&'static str],
    inverse: Inverse,
    /// The abort armed on the crashed fix.
    crash: &'static str,
}

const CHMOD: Case = Case {
    fixture: || Box::new(CredentialFileMode),
    flags: &[],
    inverse: Inverse::Restores,
    crash: "doctor.repair.step=abort@1",
};

const QUARANTINE: Case = Case {
    fixture: || Box::new(BlobHashMismatch),
    flags: &[],
    inverse: Inverse::Restores,
    crash: "doctor.repair.step=abort@1",
};

const RECOVER: Case = Case {
    fixture: || Box::new(JournalPending::default()),
    flags: &["-t", "retail"],
    inverse: Inverse::Stands,
    crash: "doctor.fix.planned=abort",
};

const SET_ASIDE: Case = Case {
    fixture: || Box::new(JournalTooNew::default()),
    flags: &["-t", "retail", "--force", "--yes"],
    inverse: Inverse::Restores,
    crash: "doctor.repair.step=abort@1",
};

// ---------------------------------------------------------------------------------------
// Running.

/// A home with a retail client, logged.
fn home(log: &TestLog) -> Home {
    let home = Home::new(&["wow"]);
    log.env(&home);
    home
}

/// `omaddons <args> --json --progress json` with this home's own Omarchy, so the widget
/// rows never see this machine's.
fn cmd<'a>(log: &'a TestLog, home: &Home, args: &[&str]) -> Cmd<'a> {
    let mut argv: Vec<&str> = args.to_vec();
    argv.extend(["--json", "--progress", "json"]);
    log.cmd(home, &argv)
        .env("OMARCHY_PATH", plant::omarchy_path(home))
}

/// A finished run: a doctor envelope checked against the doctor's contract, and every
/// channel checked for private names (common rule 9).
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

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// `doctor fix --only <fm>` with the case's flags.
fn fix_args<'a>(case: &'a Case, fm: &'a str) -> Vec<&'a str> {
    let mut args = vec!["doctor", "fix", "--only", fm];
    args.extend(case.flags.iter().copied());
    args
}

/// The ids of `ran`'s findings.
fn finding_ids(ran: &Ran) -> Vec<String> {
    ran.envelope()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["id"].as_str().map(str::to_owned))
        .collect()
}

/// `history.jsonl`'s lines, as JSON.
fn history(home: &Home) -> Vec<Value> {
    fs::read_to_string(home.state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------------------
// What a round trip compares.

/// One entry of a subject: its path below the home's root, its kind, its content (a file's
/// SHA-256, a link's target), its mode and its inode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Entry {
    path: String,
    kind: &'static str,
    content: Option<String>,
    mode: String,
    ino: u64,
}

/// Every entry below each of `roots` (the roots themselves included), sorted; a root that
/// is absent has none.
fn shot(home: &Home, roots: &[PathBuf]) -> Vec<Entry> {
    fn walk(base: &Path, path: &Path, out: &mut Vec<Entry>) {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return;
        };
        let kind = meta.file_type();
        let (kind_name, content) = if kind.is_symlink() {
            let target = fs::read_link(path).unwrap();
            ("symlink", Some(target.display().to_string()))
        } else if kind.is_dir() {
            ("dir", None)
        } else {
            let digest: String = Sha256::digest(fs::read(path).unwrap())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ("file", Some(digest))
        };
        out.push(Entry {
            path: path
                .strip_prefix(base)
                .unwrap_or(path)
                .display()
                .to_string(),
            kind: kind_name,
            content,
            mode: format!("{:04o}", meta.permissions().mode() & 0o7777),
            ino: meta.ino(),
        });
        if kind.is_dir() {
            let mut children: Vec<PathBuf> = fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            children.sort();
            for child in children {
                walk(base, &child, out);
            }
        }
    }
    let base = home.env.root();
    let mut out = Vec::new();
    for root in roots {
        walk(base, root, &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The trees a round trip must leave as they were: `<state>`, `<cache>`, `<config>` and
/// the client.
fn trees(home: &Home) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("state", home.env.state()),
        ("cache", home.env.cache()),
        ("config", home.env.config()),
        ("client", home.client("_retail_")),
    ]
}

/// Whether a changed path (relative to its tree's root) is the doctor's own record or what
/// it derives, which a round trip may leave behind, with why.
fn the_doctors_own(tree: &str, path: &str) -> bool {
    let under = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
    match tree {
        // The directory itself, which a home that had no state yet gains; the runs of the
        // fix and its undo; the repair and undo lines; their journals, in-flight locks and
        // state snapshots while they ran; the global lock file, whose content is its last
        // holder (never renamed or deleted); the staged-deletions folder a locked commit
        // creates.
        "state" => {
            path == "omaddons"
                || under("omaddons/doctor")
                || path == "omaddons/history.jsonl"
                || under("omaddons/journal")
                || under("omaddons/inflight")
                || under("omaddons/snapshots")
                || path == "omaddons/lock"
                || under("omaddons/trash")
        }
        // The directory itself; the cache-side quarantine of the runs (deleted only by
        // `doctor gc`); the history projection a change rebuilds; the widget's doorbell;
        // the in-flight locks; the discovery cache a command that reads the clients
        // refreshes; staged deletions.
        "cache" => {
            path == "omaddons"
                || under("omaddons/quarantine")
                || path == "omaddons/history_tail.json"
                || path == "omaddons/doorbell.json"
                || under("omaddons/inflight")
                || path == "omaddons/discovery.json"
                || under("omaddons/trash")
        }
        // A repair's client half (`snapshots/<tx>.repair/`, deleted only by `doctor gc`)
        // and the client's in-flight locks.
        "client" => {
            let snapshots = "Interface/.omaddons/snapshots";
            (path.starts_with(&format!("{snapshots}/")) && path.contains(".repair"))
                || path == snapshots
                || under("Interface/.omaddons/inflight")
        }
        _ => false,
    }
}

/// What changed in each of [`trees`] since `prefix`'s recording, outside the doctor's own
/// record.
fn changed_outside_the_doctor(log: &TestLog, home: &Home, prefix: &str) -> Vec<String> {
    let mut changed = Vec::new();
    for (tree, root) in trees(home) {
        let diff = log.diff(&format!("{prefix} {tree}"), &root);
        for line in diff.first(diff.total) {
            let path = line[2..].split(" (").next().unwrap_or_default();
            if !the_doctors_own(tree, path) {
                changed.push(format!("{tree}: {line}"));
            }
        }
    }
    changed
}

fn record_trees(log: &TestLog, home: &Home, prefix: &str) {
    for (tree, root) in trees(home) {
        log.tree(&format!("{prefix} {tree}"), &root);
    }
}

// ---------------------------------------------------------------------------------------
// The round trip.

fn round_trip(log: &TestLog, case: &Case) {
    // 1. Plant.
    log.step("plant");
    let home = home(log);
    let fixture = (case.fixture)();
    let fm = fixture.id();
    assert!(fixture.fixer(), "{fm}: a fixer's fixture");
    fixture.corrupt(&home);
    let roots = fixture.subject(&home);
    let planted = shot(&home, &roots);
    log.json("planted", &planted);
    assert!(!planted.is_empty(), "{fm}: the subject exists: {roots:?}");
    record_trees(log, &home, "planted");
    let lines = history(&home).len();

    // 2. Fix.
    log.step("fix");
    let args = fix_args(case, fm);
    let fixed = run(log, &home, &args);
    fixed.ok();
    let report = fixed.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    fixture.assert_healthy(&home);
    let fix_run = text(&report["run_id"]).to_owned();
    assert!(!fix_run.is_empty(), "{fm}: the fix's run: {report}");
    let repaired = shot(&home, &roots);
    log.json("repaired", &repaired);
    assert_ne!(repaired, planted, "{fm}: the fix changed its subject");
    let added: Vec<Value> = history(&home).split_off(lines);
    match case.inverse {
        Inverse::Restores => {
            let tx = text(&report["tx"]).to_owned();
            assert!(!tx.is_empty(), "{fm}: a repair transaction: {report}");
            log.assert_eq(
                "one repair line",
                &added.iter().map(|l| l["kind"].clone()).collect::<Vec<_>>(),
                &vec![json!("repair")],
            );
            log.assert_eq("its run", &added[0]["doctor_run_id"], &json!(fix_run));
        }
        Inverse::Stands => {
            log.assert_eq("no repair transaction", &report["tx"], &Value::Null);
            log.assert_eq("no history line", &added, &Vec::<Value>::new());
            log.assert_eq(
                "rolled back",
                &report["delegated"][0]["outcome"],
                &json!("rolled_back"),
            );
            log.assert_eq("its subject is gone", &repaired, &Vec::<Entry>::new());
        }
    }
    record_trees(log, &home, "fixed");

    // 3. Again.
    log.step("again");
    let lines = history(&home).len();
    let again = run(log, &home, &args);
    again.ok();
    let report = again.envelope();
    log.assert_eq("state", &report["state"], &json!("DONE_OK"));
    log.assert_eq("no action", &report["summary"]["actions_taken"], &json!(0));
    log.assert_eq("no repair", &report["tx"], &Value::Null);
    log.assert_eq("no recovery", &report["delegated"], &json!([]));
    log.assert_eq("no history line", &history(&home).len(), &lines);
    log.assert_eq("the subject", &shot(&home, &roots), &repaired);

    // 4. Undo.
    log.step("undo");
    record_trees(log, &home, "again");
    let undone = run(log, &home, &["doctor", "undo", &fix_run]);
    undone.ok();
    let report = undone.envelope();
    log.json("undo", report);
    match case.inverse {
        Inverse::Restores => {
            log.assert_eq("state", &report["state"], &json!("DONE_OK"));
            assert!(
                !text(&report["undo_tx"]).is_empty(),
                "{fm}: the undo's transaction: {report}"
            );
            // Bytes, kinds, modes and inodes: each of these fixers renames or chmods in
            // place, and its undo renames or chmods back.
            log.assert_eq("the subject as planted", &shot(&home, &roots), &planted);
            log.assert_eq(
                "changed outside the doctor's record across the fix and its undo",
                &changed_outside_the_doctor(log, &home, "planted"),
                &Vec::<String>::new(),
            );
            let found = fixture::fixtures::found_ids(&home, &fixture.run_args());
            assert!(
                found.iter().any(|id| id == fm),
                "{fm} is found again once its repair is undone: {found:?}"
            );
        }
        Inverse::Stands => {
            log.assert_eq("nothing to undo", &report["undo_tx"], &Value::Null);
            log.assert_eq("the subject", &shot(&home, &roots), &repaired);
            log.assert_eq(
                "changed by the undo",
                &changed_outside_the_doctor(log, &home, "again"),
                &Vec::<String>::new(),
            );
            fixture.assert_healthy(&home);
        }
    }

    // 5. Crash once.
    crash_once(log, case);
}

/// Step 5: a fix aborted at `case.crash` on a fresh plant; a reader reports what it left
/// and changes nothing; the next command that recovers settles it, the subject as planted;
/// a retried fix succeeds.
fn crash_once(log: &TestLog, case: &Case) {
    log.step("crash: plant");
    let home = home(log);
    let fixture = (case.fixture)();
    let fm = fixture.id();
    fixture.corrupt(&home);
    let roots = fixture.subject(&home);
    let planted = shot(&home, &roots);
    let args = fix_args(case, fm);

    log.step(&format!("crash at {}", case.crash));
    let lines = history(&home).len();
    checked(cmd(log, &home, &args).failpoints(case.crash).run()).aborted();
    log.assert_eq("no history line", &history(&home).len(), &lines);

    log.step("diagnose");
    record_trees(log, &home, "crashed");
    let diagnosed = run(log, &home, &["doctor"]);
    diagnosed.exit(1);
    let found = finding_ids(&diagnosed);
    log.json("found", &found);
    // A diagnose writes its own run, which the filter allows, and nothing else.
    log.assert_eq(
        "a diagnose changes nothing",
        &changed_outside_the_doctor(log, &home, "crashed"),
        &Vec::<String>::new(),
    );

    match case.inverse {
        Inverse::Restores => {
            assert!(
                found.iter().any(|id| id == PENDING),
                "{fm}: the interrupted repair is pending: {found:?}"
            );
            log.step("recover");
            plant::recover(&home);
            log.assert_eq("the subject as planted", &shot(&home, &roots), &planted);
            assert!(
                !plant::journals(&home)
                    .iter()
                    .any(|(_, path)| path.to_string_lossy().ends_with(".repair.jsonl")),
                "{fm}: no repair journal is left: {:?}",
                plant::journals(&home)
            );
        }
        Inverse::Stands => {
            // Aborted after planning, before the lock: the journal is as planted, and the
            // retried fix is the command that recovers it.
            assert!(
                found.iter().any(|id| id == fm),
                "{fm}: still found: {found:?}"
            );
            log.assert_eq("the subject as planted", &shot(&home, &roots), &planted);
        }
    }

    log.step("retry");
    let retried = run(log, &home, &args);
    retried.ok();
    log.assert_eq("state", &retried.envelope()["state"], &json!("DONE_OK"));
    fixture.assert_healthy(&home);
}

// ---------------------------------------------------------------------------------------
// One case per production fixer.

/// `chmod`: the stored GitHub token's file is 0644; the fix sets 0600 in place, its undo
/// puts 0644 back on the same inode.
#[test]
fn chmod_round_trips() {
    let log = tlog!();
    round_trip(&log, &CHMOD);
}

/// `quarantine`: a damaged cached archive is renamed into the run's quarantine, and its
/// undo renames it back (the cache holds no good copy of it).
#[test]
fn quarantine_round_trips() {
    let log = tlog!();
    round_trip(&log, &QUARANTINE);
}

/// `recover`: the delegation rolls an interrupted install back through the engine's own
/// recovery; `doctor undo` of its run changes nothing (M4-D37).
#[test]
fn recover_round_trips() {
    let log = tlog!();
    round_trip(&log, &RECOVER);
}

/// `set_aside`: a journal a newer omaddons started is suspended into the run's quarantine
/// (forced), and its undo puts the whole set back byte for byte while its guards hold.
#[test]
fn set_aside_round_trips() {
    let log = tlog!();
    round_trip(&log, &SET_ASIDE);
}
