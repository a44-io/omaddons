//! e2e suite: the errors ledger's light read around every change (RG-12)
//!
//! Design 7.1.8: every command that may move code or put settings back looks at the
//! client's BugGrabber files before the engine call and records what it saw after it,
//! with a change mark naming the run. A dry run and a request made only of record edits
//! leave the ledger byte for byte; an evidence lock another process holds costs the
//! change its mark and never fails or noticeably delays it; a restore ends the lifetime
//! of the databases it replaced. The tables of the pure parts are unit tests in
//! `src/app/errors_ledger.rs`; RG-13 proves the ledger end to end.
#![cfg(test)]

use crate::common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::buggrabber::{BugDb, BugEntry, NOW, TimeKind, Writer, stack_era_c, write_files};
use common::engine::Scene;
use omaddons::errors::ledger::{ErrorsLedger, TargetLedger};
use omaddons::fsx::ScopeTable;
use omaddons::limits::LEDGER_LOCK_TIMEOUT;
use omaddons::state::paths::Paths;
use serde_json::Value;

/// The scene's paths, as the binary computes them from the scene's environment.
fn paths(scene: &Scene) -> Paths {
    let env = &scene.home.env;
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

fn ledger_file(scene: &Scene) -> PathBuf {
    paths(scene).errors_ledger()
}

/// Retail's one database in the ledger, by the client's key.
fn retail(scene: &Scene) -> TargetLedger {
    let bytes = fs::read(ledger_file(scene)).unwrap();
    let ledger: ErrorsLedger = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(ledger.targets.len(), 1, "one client: {ledger:?}");
    ledger.targets.into_values().next().unwrap()
}

/// FAKEACCT's `!BugGrabber.lua` saved in `session` with one error, then past the
/// timestamp slack, so the next look reads it as a proved state.
fn save(scene: &Scene, session: u32) {
    let message = format!(
        "{} attempt to index a nil value",
        stack_era_c("Foo", "core.lua", 12)
    );
    let db = BugDb::new(Writer::Modern, session).entry(BugEntry::new(
        &message,
        session,
        (TimeKind::Unix, NOW),
    ));
    write_files(&scene.home.client("_retail_").join("WTF"), &[db.file()]);
    std::thread::sleep(Duration::from_millis(30));
}

/// The `run` of the newest history line.
fn last_run(scene: &Scene) -> String {
    let text = fs::read_to_string(scene.home.state().join("history.jsonl")).unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    last["run"].as_str().unwrap().to_owned()
}

/// Foo installed (which registers retail), then its BugGrabber file saved.
fn scene() -> Scene {
    let scene = Scene::new();
    scene.install("gh:o/foo", "v1");
    save(&scene, 5);
    scene
}

#[test]
fn an_install_and_its_undo_each_leave_a_mark_naming_their_run() {
    let log = common::tlog::tlog!();
    let scene = scene();
    log.env(&scene);

    log.step("bar installed from a local zip");
    log.cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run()
        .ok();
    let install = last_run(&scene);
    let ledger = retail(&scene);
    log.json("the ledger's retail", &ledger);
    let db = ledger.databases.values().next().unwrap();
    let mark = db.marks.last().unwrap();
    log.assert_eq(
        "the mark: its run, generation, stamp, session, settled",
        &(
            mark.run.to_string(),
            &mark.generation,
            &mark.stamp,
            mark.session,
            mark.settled,
        ),
        &(
            install,
            &db.generation,
            &db.reads.last().unwrap().stamp,
            Some(5),
            true,
        ),
    );
    log.assert_eq(
        "running: the scene's game never runs",
        &mark.running,
        &Some(false),
    );

    log.step("the install undone");
    log.cmd(&scene, &scene.args(&["undo"])).run().ok();
    let undo = last_run(&scene);
    let ledger = retail(&scene);
    let db = ledger.databases.values().next().unwrap();
    log.assert_eq(
        "one more mark, the undo's",
        &db.marks
            .iter()
            .skip(1)
            .map(|m| m.run.to_string())
            .collect::<Vec<_>>(),
        &vec![undo],
    );
}

#[test]
fn the_change_that_registers_a_client_files_its_mark_under_the_key_it_was_given() {
    let log = common::tlog::tlog!();
    let scene = Scene::new();
    log.env(&scene);
    save(&scene, 5);

    log.step("foo installed on retail, unregistered until this commit");
    log.cmd(&scene, &scene.install_args("gh:o/foo", "v1"))
        .run()
        .ok();
    let key = omaddons::discover::registry::read_marker(&scene.home.client("_retail_")).unwrap();
    let bytes = fs::read(ledger_file(&scene)).unwrap();
    let ledger: ErrorsLedger = serde_json::from_slice(&bytes).unwrap();
    log.assert_eq(
        "the ledger's clients: retail's key alone",
        &ledger.targets.keys().cloned().collect::<Vec<_>>(),
        &vec![key.to_string()],
    );
    let db = ledger.targets[&key.to_string()]
        .databases
        .values()
        .next()
        .unwrap()
        .clone();
    log.assert_eq(
        "its one mark names the install",
        &db.marks
            .iter()
            .map(|m| m.run.to_string())
            .collect::<Vec<_>>(),
        &vec![last_run(&scene)],
    );
}

#[test]
fn a_dry_run_and_a_request_made_only_of_record_edits_leave_the_ledger_byte_for_byte() {
    let log = common::tlog::tlog!();
    let scene = scene();
    log.env(&scene);
    log.cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run()
        .ok();
    let before = fs::read(ledger_file(&scene)).unwrap();

    log.step("an install's dry run: foo's next release");
    let mut dry = scene.install_args("gh:o/foo", "v2");
    dry.push("--dry-run".to_owned());
    log.cmd(&scene, &dry).run().ok();
    log.assert_eq(
        "the ledger after the dry run",
        &fs::read(ledger_file(&scene)).unwrap(),
        &before,
    );

    log.step("a plain pin, and its unpin");
    log.cmd(&scene, &scene.args(&["pin", "foo"])).run().ok();
    log.cmd(&scene, &scene.args(&["unpin", "foo"])).run().ok();
    log.assert_eq(
        "the ledger after the pin and the unpin",
        &fs::read(ledger_file(&scene)).unwrap(),
        &before,
    );
}

#[test]
fn an_evidence_lock_another_process_holds_costs_the_change_only_its_mark() {
    let log = common::tlog::tlog!();
    let scene = scene();
    log.env(&scene);
    log.cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run()
        .ok();
    log.cmd(&scene, &scene.args(&["remove", "bar"])).run().ok();
    let before = fs::read(ledger_file(&scene)).unwrap();

    log.step("bar installed again, unhindered");
    let started = Instant::now();
    let unhindered = log
        .cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run();
    let free = started.elapsed();
    unhindered.ok();
    log.cmd(&scene, &scene.args(&["remove", "bar"])).run().ok();
    let marked = fs::read(ledger_file(&scene)).unwrap();
    assert_ne!(marked, before, "the free runs left their marks");

    log.step("bar installed again while this test holds the evidence lock");
    let paths = paths(&scene);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let held = omaddons::errors::ledger::open(&paths, &scopes, "the test").unwrap();
    let started = Instant::now();
    let ran = log
        .cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run();
    let busy = started.elapsed();
    drop(held);
    ran.ok();
    log.assert_eq(
        "the same warnings as unhindered: none for the lost mark",
        &ran.envelope()["warnings"],
        &unhindered.envelope()["warnings"],
    );
    log.assert_eq(
        "the ledger, as the free runs left it",
        &fs::read(ledger_file(&scene)).unwrap(),
        &marked,
    );
    log.json(
        "the two installs' times (ms)",
        &(free.as_millis(), busy.as_millis()),
    );
    assert!(
        busy < free + LEDGER_LOCK_TIMEOUT + Duration::from_secs(2),
        "the held lock cost {busy:?} against {free:?}"
    );
}

#[test]
fn a_restore_ends_the_lifetime_of_the_databases_it_replaced() {
    let log = common::tlog::tlog!();
    let scene = scene();
    log.env(&scene);
    log.cmd(&scene, &scene.install_args("gh:o/bar", "bar"))
        .run()
        .ok();

    log.step("a backup of WTF/, then a later session");
    let created = log.cmd(&scene, &scene.args(&["backup", "create"])).run();
    created.ok();
    let id = created.data()["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = retail(&scene).databases.into_values().next().unwrap();
    save(&scene, 6);

    log.step("the backup restored: its light read finds its own history line");
    log.cmd(&scene, &scene.args(&["backup", "restore", &id]))
        .run()
        .ok();
    let after = retail(&scene).databases.into_values().next().unwrap();
    log.json("the database after the restore", &after);
    assert!(
        after.invalidated_at.is_some() || after.generation != before.generation,
        "the restore ended the lifetime: {after:?}"
    );
    log.assert_eq(
        "no mark of the restore: the file it looked at was replaced",
        &after.marks.len(),
        &before.marks.len(),
    );
}
