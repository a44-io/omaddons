//! e2e suite: the errors ledger records appearances, never recurrences
//!
//! The ledger's one claim (design 7.1): a cluster is recorded if and only if omaddons
//! proved a database free of an error and then found it there, within one lifetime of
//! that database. Each scenario plays the game by rewriting BugGrabber's SavedVariables
//! and runs the real binary: `errors` (the full read), changes (whose light read looks
//! too), `backup restore`. The game's writes are placed from one epoch in the past
//! (`common::epoch`); no scenario sets `OMADDONS_NOW`. RG-13b checks the same claim
//! against an independent model; RG-11's suite checks `first_seen`'s fields.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use common::addons::{addon_folder, toc};
use common::buggrabber::{
    BugDb, BugEntry, FILE, SvFile, TimeKind, Writer, stack_era_c, without_global, write_files,
};
use common::engine::Scene;
use common::epoch::Epoch;
use common::lua::Value as Lua;
use common::pause::Pause;
use common::privacy::{ACCOUNT2_NAME, PRIVATE};
use common::tlog::{Ran, TestLog, tlog};
use jiff::{SignedDuration, Timestamp};
use omaddons::errors::ledger::{ErrorsLedger, FamilyFirst, TargetLedger};
use omaddons::state::paths::Paths;
use serde_json::{Value, json};

const ACCOUNT: &str = "FAKEACCT";

fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n * 60)
}

fn minutes(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n)
}

/// Retail with Alpha and Beta unpacked by hand (no history); BugGrabber's files not
/// written yet. The scene's zips install Foo (`v1`) and Bar (`bar`), apart from these.
fn scene(log: &TestLog) -> Scene {
    let s = Scene::new();
    log.env(&s);
    for folder in ["Alpha", "Beta"] {
        let file = format!("{folder}.toc");
        addon_folder(&s.addons(), folder, &[(&file, &toc("120105", &[]))], &[]);
    }
    s
}

/// `account`'s BugGrabber file.
fn db_path(s: &Scene, account: &str) -> PathBuf {
    s.home
        .client("_retail_")
        .join("WTF/Account")
        .join(account)
        .join("SavedVariables")
        .join(FILE)
}

/// `text` at `folder`'s `core.lua:<line>`, last in `session` at the epoch plus
/// `occurred`, `counter` times.
fn error(
    epoch: &Epoch,
    folder: &str,
    line: u32,
    text: &str,
    session: u32,
    occurred: SignedDuration,
    counter: u32,
) -> BugEntry {
    let message = format!("{} {text}", stack_era_c(folder, "core.lua", line));
    BugEntry::new(
        &message,
        session,
        (TimeKind::Unix, epoch.at(occurred).as_second()),
    )
    .counter(counter)
}

/// A: Alpha's nil index, the motivating kind of error.
fn a(epoch: &Epoch, session: u32, counter: u32) -> BugEntry {
    error(
        epoch,
        "Alpha",
        12,
        "attempt to index a nil value",
        session,
        minutes(30),
        counter,
    )
}

/// B: a different error, in Beta.
fn b(epoch: &Epoch, session: u32, occurred: SignedDuration) -> BugEntry {
    error(
        epoch,
        "Beta",
        40,
        "attempt to call a nil value",
        session,
        occurred,
        1,
    )
}

/// The game saves `db` in `account`'s file at the epoch plus `written`.
fn save_in(s: &Scene, epoch: &Epoch, account: &str, db: &BugDb, written: SignedDuration) {
    write_files(
        &s.home.client("_retail_").join("WTF"),
        &[db.file_of(account)],
    );
    epoch.written(&db_path(s, account), written);
}

fn save(s: &Scene, epoch: &Epoch, db: &BugDb, written: SignedDuration) {
    save_in(s, epoch, ACCOUNT, db, written);
}

/// `omaddons errors --json`, which must succeed.
fn errors(log: &TestLog, s: &Scene) -> Ran {
    let ran = log.run(s, &["errors", "--json"]);
    ran.ok();
    ran
}

/// Every local error listed, every addon's.
fn listed(ran: &Ran) -> Vec<Value> {
    ran.data()["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["errors"].as_array().unwrap().clone())
        .filter(|e| e["origin"] == "local")
        .collect()
}

/// The (first) error at `folder`'s `core.lua:<line>`.
fn at_line(ran: &Ran, folder: &str, line: u32) -> Value {
    let want = format!("{folder}/core.lua:{line}");
    listed(ran)
        .into_iter()
        .find(|e| e["location"].as_str().is_some_and(|l| l.ends_with(&want)))
        .unwrap_or_else(|| panic!("no error at {want}: {:?}", listed(ran))) // ubs:ignore — the test's own failure
}

fn basis(e: &Value) -> (Value, Value) {
    (
        e["first_seen"]["basis"].clone(),
        e["first_seen"]["reason"].clone(),
    )
}

fn time(e: &Value, key: &str) -> Option<Timestamp> {
    e["first_seen"][key].as_str().map(|t| t.parse().unwrap())
}

/// How many listed errors are `observed`.
fn observed(ran: &Ran) -> usize {
    listed(ran)
        .iter()
        .filter(|e| e["first_seen"]["basis"] == "observed")
        .count()
}

fn paths(s: &Scene) -> Paths {
    let env = &s.home.env;
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

fn ledger_bytes(s: &Scene) -> Option<Vec<u8>> {
    fs::read(paths(s).errors_ledger()).ok()
}

/// The client's ledger entry, as it is filed now: under its key once a change registered
/// it. What an `errors` run recorded while it was unregistered stays under its id, as
/// the errors memo's does, and is no longer read.
fn client(s: &Scene) -> TargetLedger {
    let ledger: ErrorsLedger = serde_json::from_slice(&ledger_bytes(s).unwrap()).unwrap();
    let mut targets = ledger.targets;
    let registered = targets.keys().find(|k| *k != "retail").cloned();
    targets
        .remove(registered.as_deref().unwrap_or("retail"))
        .expect("the client's entry")
}

/// The members of every cluster the client's ledger holds.
fn clusters(s: &Scene) -> BTreeSet<Vec<String>> {
    client(s)
        .clusters
        .values()
        .map(|c| c.fingerprints.clone())
        .collect()
}

/// What the ledger learned, without times, ids and database keys: the databases, each
/// family's basis and reason, and each cluster's members.
fn learned(s: &Scene) -> Value {
    let target = client(s);
    let families: BTreeSet<(String, String)> = target
        .families
        .iter()
        .map(|(key, f)| {
            let first = match &f.first {
                FamilyFirst::Baseline { reason, .. } => format!("baseline {reason:?}"),
                FamilyFirst::Observed { .. } => "observed".to_owned(),
                FamilyFirst::Pending(_) => "pending".to_owned(),
            };
            (key.clone(), first)
        })
        .collect();
    let sessions: BTreeSet<Vec<Option<u32>>> = target
        .databases
        .values()
        .map(|db| db.reads.iter().map(|r| r.session).collect())
        .collect();
    json!({
        "databases": target.databases.len(),
        "reads": sessions,
        "families": families,
        "clusters": clusters(s),
    })
}

/// The `run` of the newest history line.
fn last_run(s: &Scene) -> String {
    let text = fs::read_to_string(s.home.state().join("history.jsonl")).unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    last["run"].as_str().unwrap().to_owned()
}

/// Installs Bar from its zip: a change, whose light read looks at BugGrabber's files.
fn install_bar(log: &TestLog, s: &Scene) -> String {
    log.cmd(s, &s.install_args("gh:o/bar", "bar")).run().ok();
    last_run(s)
}

/// The scene's first read: A in session 4, saved an hour after the epoch.
fn first_read(log: &TestLog, s: &Scene, epoch: &Epoch) -> Ran {
    save(
        s,
        epoch,
        &BugDb::new(Writer::Modern, 4).entry(a(epoch, 4, 1)),
        hours(1),
    );
    errors(log, s)
}

#[test]
fn s01_the_first_read_baselines_everything_there() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 4)
            .entry(a(&epoch, 4, 3))
            .entry(b(&epoch, 3, minutes(20))),
        hours(1),
    );
    let ran = errors(&log, &s);
    for e in listed(&ran) {
        log.assert_eq(
            "every error at the first read",
            &basis(&e),
            &(json!("baseline"), json!("first_read")),
        );
    }
    log.assert_eq("two errors", &listed(&ran).len(), &2);
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
    epoch.assert_consistent(&s.home);
}

#[test]
fn s02_an_error_the_game_adds_in_a_later_session_is_observed_alone() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("the game plays: session 5 adds B");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    let ran = errors(&log, &s);
    let found = at_line(&ran, "Beta", 40);
    log.assert_eq("B", &basis(&found), &(json!("observed"), Value::Null));
    log.assert_eq("only B", &observed(&ran), &1);
    log.assert_eq(
        "one cluster, of B",
        &clusters(&s),
        &[vec![found["fingerprint"].as_str().unwrap().to_owned()]].into(),
    );
    epoch.assert_consistent(&s.home);
}

#[test]
fn s03_an_old_error_that_happens_again_after_a_change_stays_a_baseline() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("an unrelated addon installed");
    install_bar(&log, &s);
    log.step("A happens again: a later session, a higher counter");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 6).entry(a(&epoch, 6, 9)),
        hours(3),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "A",
        &basis(&at_line(&ran, "Alpha", 12)),
        &(json!("baseline"), json!("first_read")),
    );
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
    log.assert_eq(
        "no link",
        &at_line(&ran, "Alpha", 12)["after_change"],
        &Value::Null,
    );
    epoch.assert_consistent(&s.home);
}

#[test]
fn s04_a_bug_that_moved_lines_stays_a_baseline() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("A's line moves from 12 to 14, in a later session");
    let moved = error(
        &epoch,
        "Alpha",
        14,
        "attempt to index a nil value",
        5,
        minutes(80),
        2,
    );
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5).entry(moved),
        hours(2),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "the moved A",
        &basis(&at_line(&ran, "Alpha", 14)),
        &(json!("baseline"), json!("first_read")),
    );
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
}

#[test]
fn s05_a_light_read_between_two_changes_records_the_appearance_and_both_marks() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("change 1, then the game adds B, then change 2: no errors run between");
    let one = install_bar(&log, &s);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    log.cmd(&s, &s.install_args("gh:o/foo", "v1")).run().ok();
    let two = last_run(&s);
    let target = client(&s);
    log.json("the client's ledger", &target);
    let db = target.databases.values().next().unwrap();
    log.assert_eq(
        "both changes left a mark",
        &db.marks
            .iter()
            .map(|m| m.run.to_string())
            .collect::<Vec<_>>(),
        &vec![one, two],
    );
    log.assert_eq("the light read recorded B", &target.clusters.len(), &1);
    let ran = errors(&log, &s);
    log.assert_eq(
        "B",
        &basis(&at_line(&ran, "Beta", 40)),
        &(json!("observed"), Value::Null),
    );
    let cluster = target.clusters.values().next().unwrap();
    log.assert_eq(
        "found by the second change's look",
        &time(&at_line(&ran, "Beta", 40), "observed_at"),
        &Some(cluster.appearance.found.observed_at),
    );
}

#[test]
fn s06_a_change_on_a_client_errors_never_read_baselines_it() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a(&epoch, 4, 1)),
        hours(1),
    );
    log.step("no errors run, no memo: a change's light read is the first read");
    install_bar(&log, &s);
    let target = client(&s);
    log.assert_eq(
        "A, a baseline of the first read",
        &target
            .families
            .values()
            .map(|f| matches!(f.first, FamilyFirst::Baseline { .. }))
            .collect::<Vec<_>>(),
        &vec![true],
    );
    log.step("the game adds B");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "A",
        &basis(&at_line(&ran, "Alpha", 12)),
        &(json!("baseline"), json!("first_read")),
    );
    log.assert_eq(
        "B",
        &basis(&at_line(&ran, "Beta", 40)),
        &(json!("observed"), Value::Null),
    );
}

#[test]
fn s07_an_old_error_back_in_a_wiped_database_is_no_appearance() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("the database wiped: session 1, no entries");
    save(&s, &epoch, &BugDb::new(Writer::Modern, 1), hours(2));
    errors(&log, &s);
    log.step("A returns in session 2");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 2).entry(a(&epoch, 2, 1)),
        hours(3),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "A",
        &at_line(&ran, "Alpha", 12)["first_seen"]["basis"],
        &json!("baseline"),
    );
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
}

/// Two accounts: the first holds A, the second nothing; then A in the second.
fn two_accounts(log: &TestLog, s: &Scene, epoch: &Epoch, first: &str, second: &str) -> Ran {
    save_in(
        s,
        epoch,
        first,
        &BugDb::new(Writer::Modern, 4).entry(a(epoch, 4, 1)),
        hours(1),
    );
    save_in(s, epoch, second, &BugDb::new(Writer::Modern, 9), hours(1));
    errors(log, s);
    log.step("A in the other account's database");
    save_in(
        s,
        epoch,
        second,
        &BugDb::new(Writer::Modern, 10).entry(a(epoch, 10, 1)),
        hours(2),
    );
    errors(log, s)
}

#[test]
fn s08_an_error_known_in_one_account_is_no_appearance_in_another() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    let ran = two_accounts(&log, &s, &epoch, ACCOUNT, ACCOUNT2_NAME);
    log.assert_eq("both As listed", &listed(&ran).len(), &2);
    log.assert_eq("neither observed", &observed(&ran), &0);
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
}

#[test]
fn s09_a_deleted_cache_baselines_everything_again() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    log.assert_eq("B observed", &observed(&errors(&log, &s)), &1);
    log.step("the cache deleted");
    fs::remove_dir_all(s.home.env.cache()).unwrap();
    let ran = errors(&log, &s);
    for e in listed(&ran) {
        log.assert_eq(
            "every error",
            &basis(&e),
            &(json!("baseline"), json!("first_read")),
        );
    }
    log.assert_eq("no cluster", &clusters(&s).len(), &0);
}

#[test]
fn s10_the_ledger_holds_no_name_and_no_message() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    let ran = two_accounts(&log, &s, &epoch, ACCOUNT, ACCOUNT2_NAME);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(150))),
        hours(3),
    );
    errors(&log, &s);
    let text = String::from_utf8(ledger_bytes(&s).unwrap())
        .unwrap()
        .to_lowercase();
    let mut forbidden: Vec<String> = PRIVATE.iter().map(|n| n.to_lowercase()).collect();
    // An addon's name it may hold (the one an error is filed under); never text of the
    // error, nor where in a file it happened.
    forbidden.extend(
        [
            "attempt to index",
            "attempt to call",
            "nil value",
            "core.lua",
        ]
        .map(str::to_owned),
    );
    for word in forbidden {
        assert!(!text.contains(&word), "the ledger holds {word:?}");
    }
    log.assert_eq("an appearance was recorded", &clusters(&s).len(), &1);
    drop(ran);
}

#[test]
fn s11_a_file_without_the_database_proves_nothing() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    log.step("a complete file without BugGrabberDB");
    write_files(
        &s.home.client("_retail_").join("WTF"),
        &[SvFile::account(
            ACCOUNT,
            FILE,
            without_global(Writer::Modern),
        )],
    );
    epoch.written(&db_path(&s, ACCOUNT), hours(1));
    errors(&log, &s);
    log.step("then the real database, with A");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a(&epoch, 4, 1)),
        hours(2),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "A",
        &at_line(&ran, "Alpha", 12)["first_seen"]["basis"],
        &json!("baseline"),
    );
    log.assert_eq("nothing appeared", &clusters(&s).len(), &0);
}

#[test]
fn s12_a_dropped_entry_leaves_the_decision_to_the_next_proved_read() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("B saved beside an entry that does not decode");
    // B's own time is before the proved read: only the reads bound it.
    let b = b(&epoch, 5, minutes(10));
    let mut degraded = BugDb::new(Writer::Modern, 5)
        .entry(a(&epoch, 4, 1))
        .entry(b.clone());
    degraded.tail.push(Lua::str("not an entry"));
    save(&s, &epoch, &degraded, hours(2));
    let met = errors(&log, &s);
    log.assert_eq(
        "B",
        &basis(&at_line(&met, "Beta", 40)),
        &(json!("unknown"), json!("absence_unproven")),
    );
    log.step("the malformed entry gone");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b),
        hours(3),
    );
    let ran = errors(&log, &s);
    let found = at_line(&ran, "Beta", 40);
    log.assert_eq("B", &basis(&found), &(json!("observed"), Value::Null));
    log.assert_eq(
        "after the last proved read",
        &time(&found, "onset_after_at"),
        &Some(epoch.at(hours(1))),
    );
    log.assert_eq(
        "by the degraded read that first met it",
        &time(&found, "onset_by_at"),
        &Some(epoch.at(hours(2))),
    );
    log.assert_eq(
        "A, in the last proved read, is not",
        &basis(&at_line(&ran, "Alpha", 12)),
        &(json!("baseline"), json!("first_read")),
    );
    epoch.assert_consistent(&s.home);
}

#[test]
fn s13_a_database_past_the_entry_cap_proves_no_absence() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    log.step("5,001 entries, B among them");
    let mut db = BugDb::new(Writer::Modern, 5)
        .entry(a(&epoch, 4, 1))
        .entry(b(&epoch, 5, minutes(90)));
    for i in 0..4_999 {
        let line = u32::try_from(i).unwrap() + 1;
        db = db.entry(error(
            &epoch,
            "Flood",
            line,
            "attempt to index a nil value",
            5,
            minutes(100),
            1,
        ));
    }
    save(&s, &epoch, &db, hours(2));
    let ran = errors(&log, &s);
    // The oldest entry past the cap is dropped (A), so the read held B and could not
    // vouch for anything's absence.
    log.assert_eq(
        "B, read but unplaced",
        &basis(&at_line(&ran, "Beta", 40)),
        &(json!("unknown"), json!("absence_unproven")),
    );
    log.assert_eq("nothing observed", &observed(&ran), &0);
    log.assert_eq("nothing appeared", &clusters(&s).len(), &0);
}

#[test]
fn s14_a_file_replaced_during_the_read_records_nothing_new_until_the_next() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    first_read(&log, &s, &epoch);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    log.step("the file replaced while errors reads it");
    let replaced = log
        .cmd(&s, &["errors", "--json"])
        .failpoints("errors.read.between=error")
        .run();
    replaced.ok();
    log.assert_eq("B not observed", &observed(&replaced), &0);
    log.assert_eq("nothing appeared", &clusters(&s).len(), &0);
    log.step("the next run reads it whole");
    let ran = errors(&log, &s);
    log.assert_eq(
        "B",
        &basis(&at_line(&ran, "Beta", 40)),
        &(json!("observed"), Value::Null),
    );
}

#[test]
fn s15_an_unreadable_second_account_keeps_new_errors_baselines_until_it_reads() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    save_in(
        &s,
        &epoch,
        ACCOUNT,
        &BugDb::new(Writer::Modern, 4).entry(a(&epoch, 4, 1)),
        hours(1),
    );
    save_in(
        &s,
        &epoch,
        ACCOUNT2_NAME,
        &BugDb::new(Writer::Modern, 9),
        hours(1),
    );
    errors(&log, &s);
    log.step("account 2's file unreadable; B new in account 1");
    let second = db_path(&s, ACCOUNT2_NAME);
    fs::set_permissions(&second, fs::Permissions::from_mode(0o000)).unwrap();
    save_in(
        &s,
        &epoch,
        ACCOUNT,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "B",
        &basis(&at_line(&ran, "Beta", 40)),
        &(json!("baseline"), json!("coverage_incomplete")),
    );
    log.step("account 2 readable again; C new in account 1");
    fs::set_permissions(&second, fs::Permissions::from_mode(0o644)).unwrap();
    errors(&log, &s);
    let c = error(
        &epoch,
        "Alpha",
        60,
        "bad argument #1 to 'pairs' (table expected, got nil)",
        6,
        minutes(200),
        1,
    );
    save_in(
        &s,
        &epoch,
        ACCOUNT,
        &BugDb::new(Writer::Modern, 6)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90)))
            .entry(c),
        hours(4),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "C",
        &basis(&at_line(&ran, "Alpha", 60)),
        &(json!("observed"), Value::Null),
    );
}

/// `backup create` of the client's `WTF/`; its id.
fn backup(log: &TestLog, s: &Scene) -> String {
    let created = log.cmd(s, &s.args(&["backup", "create"])).run();
    created.ok();
    created.data()["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn restore(log: &TestLog, s: &Scene, id: &str) {
    log.cmd(s, &s.args(&["backup", "restore", id])).run().ok();
}

#[test]
fn s16_a_restore_with_an_equal_or_higher_counter_starts_a_new_lifetime() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    install_bar(&log, &s);
    let c = |session| {
        error(
            &epoch,
            "Beta",
            70,
            "bad argument #1 to 'ipairs' (table expected, got nil)",
            session,
            minutes(20),
            1,
        )
    };
    let d = |session| {
        error(
            &epoch,
            "Alpha",
            90,
            "attempt to compare number with nil",
            session,
            minutes(25),
            1,
        )
    };

    log.step("equal: a backup holds C in session 5; the file read without it; restored");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(c(5)),
        hours(1),
    );
    let equal = backup(&log, &s);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5).entry(a(&epoch, 4, 1)),
        hours(2),
    );
    errors(&log, &s);
    restore(&log, &s, &equal);
    let ran = errors(&log, &s);
    log.assert_eq(
        "C, across the restore",
        &basis(&at_line(&ran, "Beta", 70)),
        &(json!("baseline"), json!("new_generation")),
    );
    log.assert_eq("no cluster", &clusters(&s).len(), &0);

    log.step("higher: a backup holds D in session 8; the file read at 6 without it");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 8)
            .entry(a(&epoch, 4, 1))
            .entry(c(5))
            .entry(d(8)),
        hours(3),
    );
    let higher = backup(&log, &s);
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 6)
            .entry(a(&epoch, 4, 1))
            .entry(c(5)),
        hours(4),
    );
    errors(&log, &s);
    restore(&log, &s, &higher);
    let ran = errors(&log, &s);
    log.assert_eq(
        "D, across the restore",
        &basis(&at_line(&ran, "Alpha", 90)),
        &(json!("baseline"), json!("new_generation")),
    );
    log.assert_eq("still no cluster", &clusters(&s).len(), &0);
}

#[test]
fn s17_a_late_writer_does_not_carry_a_lifetime_past_a_restore() {
    let log = tlog!();
    let s = scene(&log);
    let epoch = Epoch::ago(hours(48));
    install_bar(&log, &s);
    first_read(&log, &s, &epoch);
    let id = backup(&log, &s);
    log.step("errors looks at session 5 and holds its sightings, under the ledger's lock");
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(b(&epoch, 5, minutes(90))),
        hours(2),
    );
    let pause = Pause::new(s.home.env.root());
    let argv = ["errors", "--json"];
    let child = s.home.spawn(
        &argv,
        &[
            ("OMADDONS_FAILPOINTS", "errors.ledger.loaded=pause"),
            pause.env(),
        ],
    );
    pause.reached("errors.ledger.loaded");
    log.step("meanwhile the backup is restored: its light read finds the lock taken");
    restore(&log, &s, &id);
    pause.resume("errors.ledger.loaded");
    let late = common::home::wait(child, &argv).expect(0);
    let b_late = late["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["errors"].as_array().unwrap().clone())
        .find(|e| {
            e["location"]
                .as_str()
                .is_some_and(|l| l.ends_with("Beta/core.lua:40"))
        })
        .unwrap();
    log.assert_eq(
        "the late run recorded what it saw before the restore",
        &b_late["first_seen"]["basis"],
        &json!("observed"),
    );
    log.step("the game writes D after the restore");
    let d = error(
        &epoch,
        "Alpha",
        90,
        "attempt to compare number with nil",
        5,
        minutes(200),
        1,
    );
    save(
        &s,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a(&epoch, 4, 1))
            .entry(d),
        hours(4),
    );
    let ran = errors(&log, &s);
    log.assert_eq(
        "D, the first read of the restored file's lifetime",
        &basis(&at_line(&ran, "Alpha", 90)),
        &(json!("baseline"), json!("new_generation")),
    );
}

#[test]
fn s18_the_order_accounts_are_found_in_changes_nothing() {
    let log = tlog!();
    let epoch = Epoch::ago(hours(48));
    let s = scene(&log);
    two_accounts(&log, &s, &epoch, "AAAACCT", "ZZZZACCT");
    let first = learned(&s);
    log.json("A known first in the account listed first", &first);
    let other = scene(&log.case("other order"));
    two_accounts(&log, &other, &epoch, "ZZZZACCT", "AAAACCT");
    let second = learned(&other);
    log.assert_eq(
        "the same ledger, apart from times and keys",
        &second,
        &first,
    );
}
