//! e2e suite: new errors are tied to the change before them, and old errors are not
//!
//! The regression guard's link (RG-19) through the real binary, in seventeen histories:
//! the regression is linked and graded, the recurrence and the moved line are not,
//! evidence beats recency, and each kind of uncertainty lowers the grade and is named.
//! Five of them are the false-positive corpus that guards every future change to the
//! ledger and the judge: the recurring error (2), the moved line (3), the unrelated newer
//! change (4), another bug of the same shape (15) and an error that "appears" across a
//! restore (16). An eighteenth holds the rule the second half's model found missing: an
//! update replaced by a later one before the error began is no candidate. The second
//! half itself is `tests/errors_ledger_model.rs`'s judge pass.
//!
//! RG-41's scenarios (`r1` to `r6`) run the loop a player goes through: the error, the
//! way back `errors` offers, its preview and apply, the release left skipped, and the
//! next one taken; offline from the cache; through `undo` for a two-addon update whose
//! scoped `unskip` offers the innocent addon again; as an agent; a stale rollback
//! refused at its preview; and every offered command parsed, each rollback and unskip
//! run. Every step's envelope is validated against the published schemas.
//!
//! Changes run here and now, so the game's writes come after them and each error's own
//! unix time is a whole second after the change (`a_second_later`). No scenario sets
//! `OMADDONS_NOW`: the spans (14 days, 24 hours, 30 days) are RG-14's and RG-15's tables.
//! A running client is the fake `/proc` of `Home::retail_running`; one that cannot be
//! told is a `/proc` that cannot be listed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;
use crate::validate;

use std::fs;
use std::time::Duration;

use clap::Parser as _;
use common::buggrabber::{BugDb, BugEntry, TimeKind, Writer, stack_era_c, write_files};
use common::engine::{Scene, folder};
use common::home::Run;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

/// When history says transaction `tx` committed.
fn history_at(s: &Scene, tx: &str) -> Timestamp {
    let text = fs::read_to_string(s.home.state().join("history.jsonl")).unwrap();
    let line = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|l| l["tx"] == tx)
        .unwrap();
    line["at"].as_str().unwrap().parse().unwrap()
}

/// The game saves `db` in FAKEACCT's file; the file is then left past the racy slack.
fn save(s: &Scene, db: &BugDb) {
    write_files(&s.home.client("_retail_").join("WTF"), &[db.file()]);
    std::thread::sleep(Duration::from_millis(60));
}

/// A unix second after every change so far, and 50 ms into it (file times come from a
/// coarse clock).
fn a_second_later() -> i64 {
    let next = Timestamp::now().as_second() + 1;
    let settled = Timestamp::from_second(next).unwrap() + SignedDuration::from_millis(50);
    while Timestamp::now() < settled {
        std::thread::sleep(Duration::from_millis(10));
    }
    next
}

/// `text` at `folder`'s `file:<line>`, last in `session` at the unix time `when`.
fn entry(folder: &str, file: &str, line: u32, text: &str, session: u32, when: i64) -> BugEntry {
    let message = format!("{} {text}", stack_era_c(folder, file, line));
    BugEntry::new(&message, session, (TimeKind::Unix, when))
}

/// Foo's nil index at `core.lua:<line>`, `counter` times.
fn foo_error(line: u32, session: u32, when: i64, counter: u32) -> BugEntry {
    entry(
        "Foo",
        "core.lua",
        line,
        "attempt to index a nil value",
        session,
        when,
    )
    .counter(counter)
}

fn checked(run: &Run, what: &str) -> Value {
    let envelope = run.expect(0);
    if let Err(failures) = validate::validate_envelope(&envelope, &run.stderr) {
        panic!("{what}'s envelope does not validate:\n{failures}"); // ubs:ignore — the test's own failure
    }
    envelope
}

/// `omaddons errors -t retail --json`.
fn errors(s: &Scene) -> Value {
    checked(&s.home.run(&["errors", "-t", "retail"]), "errors")
}

/// `omaddons errors -t retail`, in words.
fn errors_text(s: &Scene) -> String {
    let run = s.home.exec(&["errors", "-t", "retail"].map(str::to_owned));
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

fn status(s: &Scene, cached: bool) -> Value {
    let argv: &[&str] = if cached {
        &["status", "--cached"]
    } else {
        &["status"]
    };
    checked(&s.home.run(argv), "status")
}

fn status_text(s: &Scene) -> String {
    let run = s.home.exec(&["status".to_owned()]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

/// The error at `folder`'s `file:<line>`.
fn at(envelope: &Value, folder: &str, file: &str, line: u32) -> Value {
    let want = format!("{folder}/{file}:{line}");
    envelope["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|a| a["errors"].as_array().unwrap().clone())
        .find(|e| e["location"].as_str().is_some_and(|l| l.ends_with(&want)))
        .unwrap_or_else(|| panic!("no error at {want}: {envelope}")) // ubs:ignore — the test's own failure
}

fn foo_at(envelope: &Value, line: u32) -> Value {
    at(envelope, "Foo", "core.lua", line)
}

/// The link `status` shows for `name`, `Null` for none.
fn shown(envelope: &Value, name: &str) -> Value {
    envelope["data"]["targets"][0]["errors"]["top_addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == name)
        .map_or(Value::Null, |a| a["after_change"].clone())
}

/// `(cached, plain)` links for `name`.
fn both(s: &Scene, name: &str) -> (Value, Value) {
    (
        shown(&status(s, true), name),
        shown(&status(s, false), name),
    )
}

/// The tx of the newest history line of `kind`.
fn last_tx(s: &Scene, kind: &str) -> String {
    let text = fs::read_to_string(s.home.state().join("history.jsonl")).unwrap();
    text.lines()
        .rev()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|l| l["kind"] == kind)
        .unwrap()["tx"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Every history line's time moved back by `by`: changes made that long ago.
fn age_history(s: &Scene, by: SignedDuration) {
    let path = s.home.state().join("history.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut aged = String::new();
    for line in text.lines() {
        let mut e: Value = serde_json::from_str(line).unwrap();
        let at: Timestamp = e["at"].as_str().unwrap().parse().unwrap();
        e["at"] = json!((at - by).to_string());
        aged.push_str(&serde_json::to_string(&e).unwrap());
        aged.push('\n');
    }
    fs::write(&path, aged).unwrap();
}

/// `addons` (`(repo, Folder, deps)`) installed from their v1 releases three weeks ago,
/// then BugGrabber's file saved in session 5 holding `held`, and a first `errors` read.
fn installed(addons: &[(&str, &str, &[&str])], held: &[BugEntry]) -> Scene {
    let s = Scene::new();
    for (repo, name, deps) in addons {
        s.release(repo, "v1", &[folder(name, deps)]);
        s.ok(&["install", &format!("gh:o/{repo}"), "-t", "retail"]);
    }
    age_history(&s, SignedDuration::from_hours(21 * 24));
    let mut db = BugDb::new(Writer::Modern, 5);
    for e in held {
        db = db.entry(e.clone());
    }
    save(&s, &db);
    errors(&s);
    s
}

fn foo_installed(held: &[BugEntry]) -> Scene {
    installed(&[("foo", "Foo", &[])], held)
}

/// `update` takes `repo` (folder `name`) to v2.
fn update(s: &Scene, repo: &str, name: &str, deps: &[&str]) -> String {
    s.release(repo, "v2", &[folder(name, deps)]);
    s.ok(&["update", "-t", "retail", "--refresh"]);
    last_tx(s, "update")
}

/// The game saves session `session` holding `entries`.
fn session(s: &Scene, session: u32, entries: &[BugEntry]) {
    let mut db = BugDb::new(Writer::Modern, session);
    for e in entries {
        db = db.entry(e.clone());
    }
    save(s, &db);
}

fn contains(text: &str, want: &str) {
    assert!(text.contains(want), "{want:?} in:\n{text}");
}

#[test]
fn s01_a_regression_after_an_update_is_linked_high() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    let tx = update(&s, "foo", "Foo", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 7)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["tx"], &link["relation"], &link["confidence"]),
        (&json!(tx), &json!("own"), &json!("high"))
    );
    assert_eq!(
        (
            &link["evidence"]["clean_sessions"],
            &link["evidence"]["unknowns"]
        ),
        (&json!(0), &json!([]))
    );
    assert_eq!(shown(&status(&s, true), "Foo")["tx"], json!(tx));
    let text = errors_text(&s);
    contains(&text, &format!("in the first session after update {tx}"));
    contains(&text, "that update replaced Foo v1 → v2 · likely");
    contains(&status_text(&s), "first seen after an update");
}

#[test]
fn s02_an_error_that_happens_again_after_an_update_is_not_linked() {
    let log = common::tlog::tlog!();
    let first = a_second_later();
    let s = foo_installed(&[foo_error(12, 5, first, 2)]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 9)]);
    let e = foo_at(&errors(&s), 12);
    log.json("the error", &e);
    assert_eq!(
        (&e["first_seen"]["basis"], &e["after_change"]),
        (&json!("baseline"), &Value::Null)
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
    contains(
        &errors_text(&s),
        "already there when omaddons first read this file",
    );
}

#[test]
fn s03_the_same_error_at_a_moved_line_is_not_linked() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[foo_error(12, 5, a_second_later(), 1)]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    session(&s, 6, &[foo_error(14, 6, a_second_later(), 3)]);
    let e = foo_at(&errors(&s), 14);
    assert_eq!(
        (&e["first_seen"]["basis"], &e["after_change"]),
        (&json!("baseline"), &Value::Null)
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
    contains(
        &errors_text(&s),
        "already there when omaddons first read this file",
    );
}

#[test]
fn s04_evidence_beats_recency() {
    let log = common::tlog::tlog!();
    let s = installed(&[("foo", "Foo", &[]), ("bar", "Bar", &[])], &[]);
    log.env(&s);
    log.step("Foo updated, then Bar, just before Foo's new error");
    let foo = update(&s, "foo", "Foo", &[]);
    let bar = update(&s, "bar", "Bar", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(link["tx"], json!(foo), "the older, related update");
    assert_ne!(link["tx"], json!(bar), "never the newer unrelated one");
    assert_eq!(
        (&link["relation"], &link["evidence"]["other_changes"]),
        (&json!("own"), &json!(1))
    );
    contains(
        &errors_text(&s),
        "that update replaced Foo v1 → v2 · likely",
    );
}

#[test]
fn s05_a_plugin_in_the_stack_is_in_stack_and_medium() {
    let log = common::tlog::tlog!();
    let s = installed(&[("foo", "Foo", &[]), ("pack", "FooPack", &["Foo"])], &[]);
    log.env(&s);
    let tx = update(&s, "pack", "FooPack", &["Foo"]);
    let stack = vec![
        format!("{} in function 'Show'", stack_era_c("Foo", "core.lua", 12)),
        format!(
            "{} in function 'Load'",
            stack_era_c("FooPack", "pack.lua", 3)
        ),
    ];
    session(
        &s,
        6,
        &[foo_error(12, 6, a_second_later(), 1).stack(&stack)],
    );
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["tx"], &link["relation"], &link["confidence"]),
        (&json!(tx), &json!("in_stack"), &json!("medium"))
    );
    contains(
        &errors_text(&s),
        "(its code is in this error's stack) · possible",
    );
}

#[test]
fn s06_an_error_in_the_session_running_during_the_update_is_low_and_not_in_status() {
    let log = common::tlog::tlog!();
    let mut s = foo_installed(&[]);
    log.env(&s);
    s.home.retail_running();
    let tx = update(&s, "foo", "Foo", &[]);
    s.home.nothing_running();
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 3)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["confidence"], &link["evidence"]["same_session"]),
        (&json!("low"), &json!(true))
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
    let text = errors_text(&s);
    contains(
        &text,
        &format!("in the session that was running during update {tx}"),
    );
    contains(&text, "it may have begun before the update");
    contains(&text, "· weak");
}

#[test]
fn s07_a_reload_and_a_read_before_the_error_pin_it_high() {
    let log = common::tlog::tlog!();
    let mut s = foo_installed(&[]);
    log.env(&s);
    s.home.retail_running();
    update(&s, "foo", "Foo", &[]);
    log.step("the reload's session saved without the error, and read");
    session(&s, 6, &[]);
    errors(&s);
    s.home.nothing_running();
    session(&s, 7, &[foo_error(12, 7, a_second_later(), 4)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["confidence"], &link["evidence"]["clean_sessions"]),
        (&json!("high"), &json!(0))
    );
    contains(&errors_text(&s), "in the first session after update");
}

#[test]
fn s08_four_clean_sessions_before_the_error_make_it_medium() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    session(&s, 10, &[foo_error(12, 10, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["confidence"], &link["evidence"]["clean_sessions"]),
        (&json!("medium"), &json!(4))
    );
    contains(&errors_text(&s), "4 sessions after update");
}

#[test]
fn s09_a_game_patch_between_the_reads_lowers_the_link_and_is_named() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    log.step("the game patched: .build.info names another version");
    let build_info = s.home.root.join(".build.info");
    let text = fs::read_to_string(&build_info).unwrap();
    assert!(text.contains("12.1.5.70391"), "{text}");
    fs::write(&build_info, text.replace("12.1.5.70391", "12.2.0.71002")).unwrap();
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(link["confidence"], "medium", "one grade lower");
    assert_eq!(
        link["evidence"]["client_update"],
        json!({"from_version": "12.1.5.70391", "to_version": "12.2.0.71002"})
    );
    contains(
        &errors_text(&s),
        "the game also updated since this file was last seen without the error",
    );
    let headline = status_text(&s);
    assert!(
        !headline.contains("first seen after an update"),
        "only a high link says so (design 5.6):\n{headline}"
    );
    assert_eq!(shown(&status(&s, true), "Foo")["confidence"], "medium");
}

#[test]
fn s10_an_unrelated_update_just_before_is_timing_only() {
    let log = common::tlog::tlog!();
    let s = installed(&[("foo", "Foo", &[]), ("bar", "Bar", &[])], &[]);
    log.env(&s);
    update(&s, "bar", "Bar", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    assert_eq!(
        (&link["relation"], &link["confidence"]),
        (&json!("none"), &json!("low"))
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
    contains(
        &errors_text(&s),
        "(only the order in time connects them) · weak",
    );
}

#[test]
fn s11_no_judgment_across_an_update_of_buggrabber_itself() {
    let log = common::tlog::tlog!();
    let s = installed(&[("foo", "Foo", &[]), ("bug", "!BugGrabber", &[])], &[]);
    log.env(&s);
    update(&s, "bug", "!BugGrabber", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 1)]);
    let e = foo_at(&errors(&s), 12);
    log.json("the error", &e);
    assert_eq!(
        (&e["judgment"], &e["after_change"]),
        (
            &json!({"state": "unsupported", "gap": "provider_changed"}),
            &Value::Null
        )
    );
    contains(&errors_text(&s), "BugGrabber itself changed in between");
}

#[test]
fn s12_an_undo_can_be_the_change_before_an_error() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    s.ok(&["undo"]);
    let undo = last_tx(&s, "undo");
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["tx"], &link["kind"], &link["relation"]),
        (&json!(undo), &json!("undo"), &json!("own"))
    );
    contains(&errors_text(&s), "that undo put back Foo v2 → v1");
}

#[test]
fn s13_when_whether_the_game_ran_cannot_be_told_the_link_is_medium_at_most() {
    let log = common::tlog::tlog!();
    let mut s = foo_installed(&[]);
    log.env(&s);
    s.home.running_unknown();
    update(&s, "foo", "Foo", &[]);
    s.home.nothing_running();
    session(&s, 7, &[foo_error(12, 7, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (&link["confidence"], &link["evidence"]["unknowns"]),
        (&json!("medium"), &json!(["running"]))
    );
    contains(
        &errors_text(&s),
        "omaddons could not tell whether the game was running during that update",
    );
}

#[test]
fn s14_a_local_time_string_never_earns_the_exception() {
    let log = common::tlog::tlog!();
    let mut s = foo_installed(&[]);
    log.env(&s);
    s.home.retail_running();
    update(&s, "foo", "Foo", &[]);
    s.home.nothing_running();
    log.step("once, in the mixed session, with a local-time string after the update");
    let when = a_second_later();
    let message = format!(
        "{} attempt to index a nil value",
        stack_era_c("Foo", "core.lua", 12)
    );
    let string_time = BugEntry::new(&message, 6, (TimeKind::LocalString, when));
    session(&s, 6, &[string_time]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(link["confidence"], "low");
    assert!(
        link["evidence"]["unknowns"]
            .as_array()
            .unwrap()
            .contains(&json!("time")),
        "{link}"
    );
    contains(
        &errors_text(&s),
        "this error's own time is not one omaddons can trust",
    );
}

#[test]
fn s15_another_bug_of_the_same_shape_is_related_and_keeps_no_link_alive() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    update(&s, "foo", "Foo", &[]);
    let first = a_second_later();
    session(&s, 6, &[foo_error(12, 6, first, 1)]);
    errors(&s);
    log.step("the same message at another line, beside the first");
    session(
        &s,
        7,
        &[
            foo_error(12, 6, first, 1),
            foo_error(14, 7, a_second_later(), 1),
        ],
    );
    let v = errors(&s);
    let related = foo_at(&v, 14);
    assert_eq!(
        (&related["first_seen"]["basis"], &related["after_change"]),
        (&json!("related"), &Value::Null)
    );
    contains(&errors_text(&s), "known since");
    log.step("the linked error gone, only the related one left: no link");
    session(&s, 8, &[foo_error(14, 8, a_second_later(), 2)]);
    assert_eq!(shown(&status(&s, false), "Foo"), Value::Null);
}

#[test]
fn s16_an_error_that_appears_across_a_restore_is_a_baseline() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    log.step("a backup holding an error, the file read without it, then restored");
    session(&s, 5, &[foo_error(12, 5, a_second_later(), 1)]);
    let created = s.ok(&["backup", "create", "-t", "retail"]);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    session(&s, 5, &[]);
    errors(&s);
    update(&s, "foo", "Foo", &[]);
    s.ok(&["backup", "restore", &id, "-t", "retail"]);
    let e = foo_at(&errors(&s), 12);
    log.json("the error", &e);
    assert_eq!(
        (
            &e["first_seen"]["basis"],
            &e["first_seen"]["reason"],
            &e["after_change"]
        ),
        (&json!("baseline"), &json!("new_generation"), &Value::Null)
    );
    contains(
        &errors_text(&s),
        "already there when this file was reset or put back",
    );
}

#[test]
fn s17_an_error_met_before_the_change_in_a_file_read_in_part_is_not_tied_to_it() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    log.step("an entry that does not decode, and the error, met by errors");
    let met = foo_error(12, 6, a_second_later(), 1);
    let mut degraded = BugDb::new(Writer::Modern, 6).entry(met.clone());
    degraded.tail.push(common::lua::Value::str("not an entry"));
    save(&s, &degraded);
    let e = foo_at(&errors(&s), 12);
    assert_eq!(e["first_seen"]["reason"], "absence_unproven");
    let tx = update(&s, "foo", "Foo", &[]);
    log.step("the malformed entry gone");
    session(&s, 6, &[met]);
    let e = foo_at(&errors(&s), 12);
    log.json("the error", &e);
    assert_eq!(e["first_seen"]["basis"], "observed");
    let onset_by: Timestamp = e["first_seen"]["onset_by_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        onset_by < history_at(&s, &tx),
        "known to have occurred before the update: {e}"
    );
    assert_ne!(
        e["after_change"]["tx"],
        json!(tx),
        "never the update after it"
    );
    let text = errors_text(&s);
    assert!(!text.contains(&format!("update {tx}")), "{text}");
}

/// Decided in RG-19 (its model found the gap): an update whose code a later update of
/// the same addon replaced before the error began is no candidate.
#[test]
fn s18_an_update_replaced_before_the_error_is_no_candidate() {
    let log = common::tlog::tlog!();
    let s = foo_installed(&[]);
    log.env(&s);
    log.step("Foo to v2, a clean session, Foo to v3, then a new error");
    update(&s, "foo", "Foo", &[]);
    session(&s, 6, &[]);
    s.release("foo", "v3", &[folder("Foo", &[])]);
    s.ok(&["update", "-t", "retail", "--refresh"]);
    let v3 = last_tx(&s, "update");
    session(&s, 7, &[foo_error(12, 7, a_second_later(), 1)]);
    let link = foo_at(&errors(&s), 12)["after_change"].clone();
    log.json("the link", &link);
    assert_eq!(
        (
            &link["tx"],
            &link["confidence"],
            &link["evidence"]["other_changes"]
        ),
        (&json!(v3), &json!("high"), &json!(0)),
        "v2's code was gone from session 7: not one of the other changes"
    );
    contains(
        &errors_text(&s),
        "that update replaced Foo v2 → v3 · likely",
    );
}

// ---------------------------------------------------------------------------------------
// RG-41: from the error to the way back to the next release, through the real binary.

/// `omaddons <argv> --json` (`argv` without `omaddons`): its exit code and its envelope,
/// validated against the published schemas whatever the exit.
fn run_json(s: &Scene, argv: &[&str]) -> (i32, Value) {
    let run = s.home.run(argv);
    let envelope: Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("{argv:?}: no envelope ({e}): {}", run.stderr)); // ubs:ignore — the test's own failure
    if let Err(failures) = validate::validate_envelope(&envelope, &run.stderr) {
        panic!("{argv:?}'s envelope does not validate:\n{failures}"); // ubs:ignore — the test's own failure
    }
    (run.code, envelope)
}

/// [`run_json`] of an argv an envelope offered (it starts with `omaddons`), with `extra`.
fn run_offered(s: &Scene, argv: &[String], extra: &[&str]) -> (i32, Value) {
    let mut words: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    words.extend_from_slice(extra);
    run_json(s, &words)
}

/// Every argv an envelope offers: its `commands`, and each error's own
/// `after_change.commands`.
fn offered(v: &Value) -> Vec<(String, Vec<String>)> {
    let argv = |c: &Value| -> (String, Vec<String>) {
        let words = c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_owned())
            .collect();
        (c["action"].as_str().unwrap().to_owned(), words)
    };
    let mut all: Vec<(String, Vec<String>)> = v["commands"]
        .as_array()
        .map(|cs| cs.iter().map(argv).collect())
        .unwrap_or_default();
    for t in v["data"]["targets"].as_array().into_iter().flatten() {
        for a in t["addons"].as_array().into_iter().flatten() {
            for e in a["errors"].as_array().into_iter().flatten() {
                for c in e["after_change"]["commands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    all.push(argv(c));
                }
            }
        }
    }
    all
}

/// The first offered argv of `action`.
fn first_of(v: &Value, action: &str) -> Vec<String> {
    offered(v)
        .into_iter()
        .find(|(a, _)| a == action)
        .unwrap_or_else(|| panic!("no {action} offered: {v}")) // ubs:ignore — the test's own failure
        .1
}

/// Foo at v2 by an update from v1, and a new error in its first session: design 5.1.
/// The scene and the update's transaction.
fn after_regression() -> (Scene, String) {
    let s = foo_installed(&[]);
    let tx = update(&s, "foo", "Foo", &[]);
    session(&s, 6, &[foo_error(12, 6, a_second_later(), 7)]);
    (s, tx)
}

fn foo_version(s: &Scene) -> String {
    s.record("foo")["version"].as_str().unwrap().to_owned()
}

fn foo_marks(s: &Scene) -> Vec<Value> {
    s.record("foo")["skipped_releases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn r1_the_full_loop_from_the_error_to_the_next_release() {
    let log = common::tlog::tlog!();
    let (s, tx) = after_regression();
    log.env(&s);
    log.step("errors offers the rollback, first");
    let v = errors(&s);
    let rollback = first_of(&v, "rollback");
    assert_eq!(v["commands"][0]["action"], "rollback");
    assert_eq!(
        rollback,
        [
            "omaddons", "skip", "Foo", "--back", "--tx", &tx, "-t", "retail"
        ]
        .map(str::to_owned)
    );
    log.step("the offered argv previewed, then its plan applied");
    let (code, preview) = run_offered(&s, &rollback, &["--dry-run"]);
    assert_eq!(code, 0, "{preview}");
    let plan = preview["data"]["plan_id"].as_str().unwrap().to_owned();
    let (code, applied) = run_json(&s, &["apply", &plan]);
    assert_eq!(code, 0, "{applied}");
    assert_eq!(foo_version(&s), "v1");
    log.step("outdated reads skipped_release");
    let (code, out) = run_json(&s, &["outdated", "-t", "retail"]);
    assert_eq!(code, 0);
    assert_eq!(
        out["data"]["targets"][0]["items"][0]["status"],
        "skipped_release"
    );
    log.step("v3 out: update installs it, and the mark on v2 stays");
    s.release("foo", "v3", &[folder("Foo", &[])]);
    let (code, _) = run_json(&s, &["update", "-t", "retail", "--refresh"]);
    assert_eq!(code, 0);
    assert_eq!(foo_version(&s), "v3");
    assert_eq!(foo_marks(&s)[0]["release"]["tag"], "v2");
}

#[test]
fn r2_the_loop_offline_goes_back_from_the_cache() {
    let log = common::tlog::tlog!();
    let (mut s, _) = after_regression();
    log.env(&s);
    log.step("the fixtures answer nothing from here on");
    s.home.scenario("silent");
    let asked = common::netlog::net_log(&s.home).len();
    let v = errors(&s);
    let rollback = first_of(&v, "rollback");
    let (code, preview) = run_offered(&s, &rollback, &["--dry-run"]);
    assert_eq!(code, 0, "{preview}");
    let plan = preview["data"]["plan_id"].as_str().unwrap().to_owned();
    let (code, applied) = run_json(&s, &["apply", &plan]);
    assert_eq!(code, 0, "{applied}");
    assert_eq!(foo_version(&s), "v1");
    assert_eq!(
        common::netlog::net_log(&s.home).len(),
        asked,
        "no request once the fixtures went quiet"
    );
}

#[test]
fn r3_through_undo_both_are_marked_and_the_innocent_one_is_offered_again() {
    use common::wowi::{details, wowi_details, wowi_file};
    let log = common::tlog::tlog!();
    let mut s = Scene::new();
    log.env(&s);
    let bugsack = |scenario: &std::path::Path, version: &str| {
        let toc = common::addons::toc(
            "120105",
            &["## Title: BugSack", &format!("## Version: {version}")],
        );
        let core = format!("-- BugSack {version}");
        let zip = common::archives::zip_text(&[
            ("BugSack/BugSack.toc", &toc),
            ("BugSack/core.lua", &core),
        ]);
        let md5 = wowi_file(scenario, 5995, &zip);
        wowi_details(
            scenario,
            &[5995],
            &[details(5995, "BugSack", version, &md5)],
        );
    };
    log.step("Foo (GitHub) and BugSack (WoWInterface) installed three weeks ago, then read");
    s.release("foo", "v1", &[folder("Foo", &[])]);
    bugsack(&s.home.http, "1.0");
    s.ok(&["install", "gh:o/foo", "wowi:5995", "-t", "retail"]);
    let earlier = s.record("bugsack")["blob_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    age_history(&s, SignedDuration::from_hours(21 * 24));
    session(&s, 5, &[]);
    errors(&s);
    log.step("one update takes both; BugSack throws; its 1.0 archive left the cache");
    let newer = s.home.scenario("newer");
    s.release("foo", "v1", &[folder("Foo", &[])]);
    s.release("foo", "v2", &[folder("Foo", &[])]);
    bugsack(&newer, "1.1");
    common::binding::age_release_memo(&s.home, SignedDuration::from_hours(24));
    let (code, _) = run_json(&s, &["update", "-t", "retail", "--refresh"]);
    assert_eq!(code, 0);
    let tx = last_tx(&s, "update");
    let message = format!(
        "{} attempt to index a nil value",
        stack_era_c("BugSack", "core.lua", 12)
    );
    session(
        &s,
        6,
        &[BugEntry::new(
            &message,
            6,
            (TimeKind::Unix, a_second_later()),
        )],
    );
    fs::remove_file(s.home.env.cache().join("omaddons/blobs").join(&earlier)).unwrap();
    log.step("errors offers the update's undo, which also moves Foo back");
    let v = errors(&s);
    let undo = first_of(&v, "undo");
    assert_eq!(undo, ["omaddons", "undo", tx.as_str()].map(str::to_owned));
    assert!(
        errors_text(&s).contains("undoing that update also moves back Foo"),
        "{}",
        errors_text(&s)
    );
    let (code, undone) = run_offered(&s, &undo, &[]);
    assert_eq!(code, 0, "{undone}");
    assert_eq!(foo_version(&s), "v1");
    assert_eq!(s.record("bugsack")["version"], "1.0");
    log.step("both marked, and the result offers unskip --tx");
    let marked = |addon: &str| {
        s.record(addon)["skipped_releases"]
            .as_array()
            .map_or(0, Vec::len)
    };
    assert_eq!((marked("foo"), marked("bugsack")), (1, 1));
    let unskip = first_of(&undone, "unskip");
    let undo_tx = unskip[unskip.iter().position(|w| w == "--tx").unwrap() + 1].clone();
    log.step("unskip Foo --tx: update takes Foo, BugSack stays skipped");
    let (code, _) = run_json(&s, &["unskip", "Foo", "--tx", &undo_tx, "-t", "retail"]);
    assert_eq!(code, 0);
    let (code, _) = run_json(&s, &["update", "-t", "retail"]);
    assert_eq!(code, 0);
    assert_eq!(foo_version(&s), "v2");
    assert_eq!(s.record("bugsack")["version"], "1.0");
    assert_eq!(marked("bugsack"), 1);
}

#[test]
fn r4_an_agents_path_is_recorded_and_undone_by_its_actor() {
    let log = common::tlog::tlog!();
    let (s, _) = after_regression();
    log.env(&s);
    let (code, v) = run_json(&s, &["errors", "-t", "retail", "--actor", "agent"]);
    assert_eq!(code, 0);
    let rollback = first_of(&v, "rollback");
    let (code, done) = run_offered(
        &s,
        &rollback,
        &["--actor", "agent", "--reason", "the error after the update"],
    );
    assert_eq!(code, 0, "{done}");
    let rolled = done["data"]["targets"][0]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    log.step("history records the agent on the rollback");
    let (_, history) = run_json(&s, &["history", "-t", "retail", "--actor", "agent"]);
    let entry = history["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["tx"] == rolled.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (&entry["actor"], &entry["kind"]),
        (&json!("agent"), &json!("update"))
    );
    log.step("undo --last-by agent reverts it and takes the mark away");
    let (code, _) = run_json(&s, &["undo", "--last-by", "agent", "--actor", "agent"]);
    assert_eq!(code, 0);
    assert_eq!(foo_version(&s), "v2");
    assert!(foo_marks(&s).is_empty());
}

#[test]
fn r5_a_stale_rollback_is_refused_at_its_preview() {
    let log = common::tlog::tlog!();
    let (s, _) = after_regression();
    log.env(&s);
    let rollback = first_of(&errors(&s), "rollback");
    log.step("v3 installed by hand, then the old argv previewed, as the widget would");
    s.release("foo", "v3", &[folder("Foo", &[])]);
    let (code, _) = run_json(&s, &["update", "-t", "retail", "--refresh"]);
    assert_eq!(code, 0);
    let before = (s.record("foo"), s.folders_of(&["Foo"]));
    let (code, stale) = run_offered(&s, &rollback, &["--dry-run"]);
    assert_eq!((code, &stale["error"]["code"]), (1, &json!("plan_stale")));
    assert_eq!(
        (s.record("foo"), s.folders_of(&["Foo"])),
        before,
        "nothing changed"
    );
}

#[test]
fn r6_every_offered_command_parses_and_each_rollback_and_unskip_does_what_it_says() {
    let log = common::tlog::tlog!();
    let (s, tx) = after_regression();
    log.env(&s);
    let mut seen: Vec<(String, Vec<String>)> = Vec::new();
    let v = errors(&s);
    seen.extend(offered(&v));
    log.step("the rollback, run as offered: Foo back at v1, v2 marked by it");
    let rollback = first_of(&v, "rollback");
    assert!(rollback.windows(2).any(|w| w == ["--tx", tx.as_str()]));
    let (code, done) = run_offered(&s, &rollback, &[]);
    assert_eq!(code, 0, "{done}");
    seen.extend(offered(&done));
    let rolled = done["data"]["targets"][0]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(foo_version(&s), "v1");
    let marks = foo_marks(&s);
    assert_eq!(
        (&marks[0]["release"]["tag"], &marks[0]["tx"]),
        (&json!("v2"), &json!(rolled))
    );
    log.step("its unskip --tx, run as offered: exactly that mark is gone");
    let unskip = first_of(&done, "unskip");
    assert!(unskip.windows(2).any(|w| w == ["--tx", rolled.as_str()]));
    let (code, cleared) = run_offered(&s, &unskip, &[]);
    assert_eq!(code, 0, "{cleared}");
    seen.extend(offered(&cleared));
    assert!(foo_marks(&s).is_empty());
    assert_eq!(foo_version(&s), "v1", "an unskip moves no folder");
    seen.extend(offered(&errors(&s)));
    log.step("every command seen parses with the binary's own parser");
    log.json("seen", &seen);
    assert!(seen.iter().any(|(a, _)| a == "rollback") && seen.iter().any(|(a, _)| a == "unskip"));
    for (action, argv) in &seen {
        if let Err(e) = omaddons::cli::Cli::try_parse_from(argv) {
            panic!("{action}: {argv:?} does not parse: {e}"); // ubs:ignore — the test's own failure
        }
    }
}
