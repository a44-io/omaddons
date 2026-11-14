//! e2e suite: `errors` records what it reads in the errors ledger and says when each error began (RG-11)
//!
//! Design 7.1.7 (phases 1 and 2) and 7.3.1 (`first_seen`): every `errors` run sights the
//! whole observation of each client (`SightMode::Full`), whatever its filters, and each
//! error reads what the ledger knows of it: `baseline` when it was there at the first
//! read, `observed` (with its bounds) when a proved read without it was followed by one
//! with it, `related` for another error of a shape already watched appear, `unknown`
//! when no read vouched for its absence or the ledger could not record this read. A
//! dismissal sights nothing. The binary keeps its own clock: each scenario places the
//! game's writes from one epoch (`common::epoch`), and its timeline is checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::addons::{addon_folder, toc};
use common::buggrabber::{BugDb, BugEntry, FILE, TimeKind, Writer, stack_era_c, write_files};
use common::epoch::Epoch;
use common::home::Home;
use common::http::paths;
use common::lua::Value as Lua;
use common::tlog::{Ran, TestLog, tlog};
use jiff::{SignedDuration, Timestamp};
use omaddons::errors::ledger::{ErrorsLedger, FamilyFirst};
use omaddons::fsx::ScopeTable;
use serde_json::{Value, json};

fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n * 60)
}

fn minutes(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n)
}

/// Retail, with BugGrabber and Foo and Bar installed by hand.
fn home(log: &TestLog) -> Home {
    log.step("retail, with BugGrabber, Foo and Bar");
    let home = Home::new(&["wow"]);
    log.env(&home);
    let addons = home.addons("_retail_");
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &addons,
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    for folder in ["Foo", "Bar"] {
        let file = format!("{folder}.toc");
        addon_folder(&addons, folder, &[(&file, &toc("120105", &[]))], &[]);
    }
    home
}

/// The FAKEACCT database's file.
fn db_path(home: &Home) -> PathBuf {
    home.client("_retail_")
        .join("WTF/Account/FAKEACCT/SavedVariables")
        .join(FILE)
}

/// An error in `folder`'s `core.lua` at `line`, last occurred in `session` at the epoch
/// plus `occurred`, `counter` times.
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

/// The game saves `db` at the epoch plus `written`.
fn save(home: &Home, epoch: &Epoch, db: &BugDb, written: SignedDuration) {
    write_files(&home.client("_retail_").join("WTF"), &[db.file()]);
    epoch.written(&db_path(home), written);
}

/// `omaddons errors <args> --json` (the binary's own clock).
fn errors(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = vec!["errors"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let ran = log.run(home, &argv);
    ran.ok();
    ran
}

/// Every error of the first client, local and received.
fn listed(ran: &Ran) -> Vec<Value> {
    ran.data()["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["errors"].as_array().unwrap().clone())
        .collect()
}

/// The error whose location ends `…/core.lua:<line>` in `folder`.
fn at_line(ran: &Ran, folder: &str, line: u32) -> Value {
    let want = format!("{folder}/core.lua:{line}");
    listed(ran)
        .into_iter()
        .find(|e| e["location"].as_str().is_some_and(|l| l.ends_with(&want)))
        .unwrap_or_else(|| panic!("no error at {want}: {:?}", listed(ran))) // ubs:ignore — the test's own failure
}

/// `(basis, reason)` of an error.
fn basis(e: &Value) -> (Value, Value) {
    (
        e["first_seen"]["basis"].clone(),
        e["first_seen"]["reason"].clone(),
    )
}

fn time(e: &Value, key: &str) -> Option<Timestamp> {
    e["first_seen"][key].as_str().map(|t| t.parse().unwrap())
}

fn ledger_bytes(home: &Home) -> Option<Vec<u8>> {
    fs::read(paths(&home.env).errors_ledger()).ok()
}

/// A first read finds every error already there: `baseline`, `first_read`. When the
/// game then saves a new error in a later session, the next read watched it appear:
/// `observed`, absent from the file written before, present by its own trusted time
/// (which fits between the two writes), found by this read; the old one stays a baseline.
#[test]
fn the_first_read_is_a_baseline_and_a_later_error_is_watched_appear() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );

    log.step("the game saves A in session 4");
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a.clone()),
        hours(1),
    );
    let first = errors(&log, &home, &[]);
    let found = at_line(&first, "Foo", 12);
    log.assert_eq(
        "A at the first read",
        &basis(&found),
        &(json!("baseline"), json!("first_read")),
    );
    let first_at = time(&found, "at").unwrap();

    log.step("the game saves B in session 5, beside A");
    let b = error(
        &epoch,
        "Foo",
        40,
        "attempt to call a nil value",
        5,
        minutes(90),
        1,
    );
    let db = BugDb::new(Writer::Modern, 5).entry(a).entry(b);
    save(&home, &epoch, &db, hours(2));
    let before = Timestamp::now();
    let second = errors(&log, &home, &[]);
    let after = Timestamp::now();
    let b = at_line(&second, "Foo", 40);
    log.assert_eq("B", &basis(&b), &(json!("observed"), Value::Null));
    log.assert_eq(
        "absent from the file written before",
        &time(&b, "onset_after_at"),
        &Some(epoch.at(hours(1))),
    );
    log.assert_eq(
        "present by its trusted time",
        &time(&b, "onset_by_at"),
        &Some(epoch.at(minutes(90))),
    );
    let observed = time(&b, "observed_at").unwrap();
    assert!(
        before <= observed && observed <= after,
        "found by this read: {observed}"
    );
    log.assert_eq(
        "A stays a baseline, from the first read",
        &(
            basis(&at_line(&second, "Foo", 12)),
            time(&at_line(&second, "Foo", 12), "at"),
        ),
        &((json!("baseline"), json!("first_read")), Some(first_at)),
    );

    log.step("in words: B's one occurrence, at its time");
    let human = log.run(&home, &["errors"]);
    human.ok();
    log.assert_eq(
        "first occurred",
        &human.stdout().contains("first occurred "),
        &true,
    );
    epoch.assert_consistent(&home);
}

/// A bug that moved: the same error rewritten at another line, in a later session and
/// more often, is still of a family that was there at the first read: a baseline, never
/// a newly appeared error.
#[test]
fn a_bug_that_moved_lines_stays_a_baseline() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let text = "attempt to index a nil value";
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(error(&epoch, "Foo", 12, text, 4, minutes(30), 1)),
        hours(1),
    );
    errors(&log, &home, &[]);

    log.step("the same error, at line 14 now, in session 5, three times");
    let moved =
        BugDb::new(Writer::Modern, 5).entry(error(&epoch, "Foo", 14, text, 5, minutes(90), 3));
    save(&home, &epoch, &moved, hours(2));
    let ran = errors(&log, &home, &[]);
    let e = at_line(&ran, "Foo", 14);
    log.assert_eq(
        "still a baseline",
        &basis(&e),
        &(json!("baseline"), json!("first_read")),
    );
    log.assert_eq("it is new", &e["new"], &json!(true));
    let human = log.run(&home, &["errors"]);
    human.ok();
    log.assert_eq(
        "said in words",
        &human
            .stdout()
            .contains("already there when omaddons first read this file ("),
        &true,
    );
    epoch.assert_consistent(&home);
}

/// After an error was watched appear, another error of its shape (the same message at
/// another line) is `related`, pointing at the first, never a second appearance.
#[test]
fn another_error_of_an_observed_shape_is_related() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );
    let b = error(
        &epoch,
        "Bar",
        40,
        "attempt to call a nil value",
        5,
        minutes(90),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a.clone()),
        hours(1),
    );
    errors(&log, &home, &[]);
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 5)
            .entry(a.clone())
            .entry(b.clone()),
        hours(2),
    );
    let watched = errors(&log, &home, &[]);
    let first = at_line(&watched, "Bar", 40);
    log.assert_eq(
        "B appeared",
        &basis(&first),
        &(json!("observed"), Value::Null),
    );

    log.step("the same message at another line of Bar");
    let again = error(
        &epoch,
        "Bar",
        77,
        "attempt to call a nil value",
        6,
        minutes(150),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 6).entry(a).entry(b).entry(again),
        hours(3),
    );
    let ran = errors(&log, &home, &[]);
    let related = at_line(&ran, "Bar", 77);
    log.assert_eq(
        "related",
        &basis(&related),
        &(json!("related"), Value::Null),
    );
    log.assert_eq(
        "pointing at the first",
        &related["first_seen"]["related_fingerprint"],
        &first["fingerprint"],
    );
    log.assert_eq(
        "no bounds of its own",
        &related["first_seen"]["onset_by_at"],
        &Value::Null,
    );
    epoch.assert_consistent(&home);
}

/// A read that holds an entry that does not decode proves nothing: an error it meets
/// that the ledger does not know is `unknown`, `absence_unproven`. Once the next read is
/// whole, that error appeared after the last proved read, and by the time the degraded
/// read first met it.
#[test]
fn a_read_that_proves_nothing_leaves_the_next_proved_read_to_decide() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );
    // C's own time cannot be trusted (it is before the read without it), so the reads
    // alone bound it.
    let c = error(
        &epoch,
        "Bar",
        40,
        "attempt to call a nil value",
        5,
        minutes(10),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a.clone()),
        hours(1),
    );
    errors(&log, &home, &[]);

    log.step("C saved beside an entry that does not decode");
    let mut degraded = BugDb::new(Writer::Modern, 5)
        .entry(a.clone())
        .entry(c.clone());
    degraded.tail.push(Lua::str("not an entry"));
    save(&home, &epoch, &degraded, hours(2));
    let met = errors(&log, &home, &[]);
    log.assert_eq(
        "C, unplaced",
        &basis(&at_line(&met, "Bar", 40)),
        &(json!("unknown"), json!("absence_unproven")),
    );
    log.assert_eq(
        "A, as the ledger knows it",
        &basis(&at_line(&met, "Foo", 12)),
        &(json!("baseline"), json!("first_read")),
    );

    log.step("the file whole again");
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 5).entry(a).entry(c),
        hours(3),
    );
    let ran = errors(&log, &home, &[]);
    let c = at_line(&ran, "Bar", 40);
    log.assert_eq("C appeared", &basis(&c), &(json!("observed"), Value::Null));
    log.assert_eq(
        "after the last proved read",
        &time(&c, "onset_after_at"),
        &Some(epoch.at(hours(1))),
    );
    log.assert_eq(
        "by the degraded read's file",
        &time(&c, "onset_by_at"),
        &Some(epoch.at(hours(2))),
    );
    epoch.assert_consistent(&home);
}

/// What one client's ledger learned, without the times and ids each run's clock makes:
/// its databases, each family's basis, and each appearance's members.
fn learned(home: &Home) -> Value {
    let ledger: ErrorsLedger =
        serde_json::from_slice(&ledger_bytes(home).expect("a ledger")).unwrap();
    let target = ledger.targets.values().next().expect("one client");
    let families: Vec<(String, &str)> = target
        .families
        .iter()
        .map(|(key, f)| {
            let first = match &f.first {
                FamilyFirst::Baseline { .. } => "baseline",
                FamilyFirst::Observed { .. } => "observed",
                FamilyFirst::Pending(_) => "pending",
            };
            (key.clone(), first)
        })
        .collect();
    let clusters: BTreeSet<Vec<String>> = target
        .clusters
        .values()
        .map(|c| c.fingerprints.clone())
        .collect();
    json!({"databases": target.databases.len(), "families": families, "clusters": clusters})
}

/// `--addon`, `--since` and `--limit` shape the view, never what the ledger learns: a
/// run that filters every error out of view records exactly what an unfiltered run does.
#[test]
fn a_filtered_run_records_what_an_unfiltered_one_does() {
    let log = tlog!();
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );
    let b = error(
        &epoch,
        "Foo",
        40,
        "attempt to call a nil value",
        5,
        minutes(90),
        1,
    );
    let filters: [&[&str]; 2] = [&["--addon", "Bar", "--since", "1m", "--limit", "1"], &[]];
    let mut seen = Vec::new();
    for filter in filters {
        let home = home(&log);
        save(
            &home,
            &epoch,
            &BugDb::new(Writer::Modern, 4).entry(a.clone()),
            hours(1),
        );
        errors(&log, &home, filter);
        save(
            &home,
            &epoch,
            &BugDb::new(Writer::Modern, 5)
                .entry(a.clone())
                .entry(b.clone()),
            hours(2),
        );
        let ran = errors(&log, &home, filter);
        if !filter.is_empty() {
            log.assert_eq("nothing in view", &listed(&ran).len(), &0);
        }
        seen.push(learned(&home));
    }
    log.assert_eq("the same ledger", &seen[0], &seen[1]);
    log.assert_eq(
        "B's appearance among it",
        &seen[1]["clusters"].as_array().map(Vec::len),
        &Some(1),
    );
}

/// A dismissal changes what is new, not what has been seen: `errors dismiss` and its
/// preview leave the ledger byte for byte.
#[test]
fn a_dismissal_records_nothing_in_the_ledger() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a.clone()),
        hours(1),
    );
    errors(&log, &home, &[]);
    let b = error(
        &epoch,
        "Foo",
        40,
        "attempt to call a nil value",
        5,
        minutes(90),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 5).entry(a).entry(b),
        hours(2),
    );
    let before = ledger_bytes(&home);
    for argv in [
        &["errors", "dismiss", "--dry-run", "--json"][..],
        &["errors", "dismiss", "--json"],
    ] {
        log.run(&home, argv).ok();
        log.assert_eq(
            &format!("{}: the ledger, byte for byte", argv.join(" ")),
            &(ledger_bytes(&home) == before),
            &true,
        );
    }
}

/// While another process holds the evidence lock, `errors` still answers: what the
/// ledger knows reads as it has it, the rest `unknown` (`ledger_busy`), and it writes no
/// ledger.
#[test]
fn a_busy_ledger_answers_from_the_file_and_records_nothing() {
    let log = tlog!();
    let home = home(&log);
    let epoch = Epoch::ago(hours(48));
    let a = error(
        &epoch,
        "Foo",
        12,
        "attempt to index a nil value",
        4,
        minutes(30),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 4).entry(a.clone()),
        hours(1),
    );
    errors(&log, &home, &[]);
    let b = error(
        &epoch,
        "Foo",
        40,
        "attempt to call a nil value",
        5,
        minutes(90),
        1,
    );
    save(
        &home,
        &epoch,
        &BugDb::new(Writer::Modern, 5).entry(a).entry(b),
        hours(2),
    );
    let before = ledger_bytes(&home);

    log.step("errors while this test holds the evidence lock");
    let paths = paths(&home.env);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let held = omaddons::errors::ledger::open(&paths, &scopes, "the test").unwrap();
    let ran = errors(&log, &home, &[]);
    drop(held);
    log.assert_eq(
        "A, as the ledger has it",
        &basis(&at_line(&ran, "Foo", 12)),
        &(json!("baseline"), json!("first_read")),
    );
    log.assert_eq(
        "B, not recorded",
        &basis(&at_line(&ran, "Foo", 40)),
        &(json!("unknown"), json!("ledger_busy")),
    );
    log.assert_eq("no ledger written", &(ledger_bytes(&home) == before), &true);

    log.step("once the lock is free, the next read records B, which appeared");
    let ran = errors(&log, &home, &[]);
    log.assert_eq(
        "B",
        &basis(&at_line(&ran, "Foo", 40)),
        &(json!("observed"), Value::Null),
    );
}
