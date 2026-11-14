//! e2e suite: `errors` judges each newly appeared error and names the change before it (RG-16)
//!
//! Design 7.1.7 (phases 3 and 4) and 7.3.1: after recording what it read, `errors` judges
//! every watched appearance still owed a judgment, outside any lock, reading history once
//! and only when one is owed, and stores each verdict in a second session. An error then
//! carries `after_change` (the change, its grade, its evidence and what is true now) and
//! `judgment` (where the judgment stands); its addon carries the strongest current
//! `high` or `medium` link. A complete judgment is kept as made; a deferred one is tried
//! again. A verdict reaches only the exact fingerprints of its cluster.
//!
//! The changes run here and now, so the game's writes are real ones made after them, and
//! each error's own unix time is a whole second after the change (`a_second_later`). The
//! judge's tables are unit tests in `src/errors/suspect/judge_tests.rs`; the adapter's
//! and the view's are in `src/app/errors_ledger.rs`; RG-19 owns the scenario suite.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;
use crate::validate;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use common::addons::{addon_folder, toc};
use common::buggrabber::{BugDb, BugEntry, TimeKind, Writer, stack_era_c, write_files};
use common::engine::{Scene, folder};
use common::epoch::Epoch;
use common::home::Run;
use jiff::{SignedDuration, Timestamp};
use omaddons::errors::ledger::{ErrorsLedger, TargetLedger};
use omaddons::errors::sanitize::{self, normalized, strict_message};
use omaddons::state::paths::{ClientPaths, Paths};
use serde_json::{Value, json};

fn paths(s: &Scene) -> Paths {
    let env = &s.home.env;
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

/// The client's one ledger entry.
fn retail(s: &Scene) -> TargetLedger {
    let bytes = fs::read(paths(s).errors_ledger()).unwrap();
    let ledger: ErrorsLedger = serde_json::from_slice(&bytes).unwrap();
    ledger.targets.into_values().next().unwrap()
}

/// The game saves `db`; the file is then left past the ledger's racy slack.
fn save(s: &Scene, db: &BugDb) {
    write_files(&s.home.client("_retail_").join("WTF"), &[db.file()]);
    std::thread::sleep(Duration::from_millis(60));
}

/// A unix second after every change made so far: an error's own time, which BugGrabber
/// keeps in whole seconds, must not fall before the change that preceded it. It returns
/// 50 ms into that second, so the file the game then writes is stamped after it too:
/// file times come from a coarse clock that can trail the precise one by milliseconds.
fn a_second_later() -> i64 {
    let next = Timestamp::now().as_second() + 1;
    let settled = Timestamp::from_second(next).unwrap() + SignedDuration::from_millis(50);
    while Timestamp::now() < settled {
        std::thread::sleep(Duration::from_millis(10));
    }
    next
}

/// `attempt to index a nil value` at `folder`'s `core.lua:<line>`, last in `session` at
/// the unix time `when`, `counter` times.
fn error(folder: &str, line: u32, session: u32, when: i64, counter: u32) -> BugEntry {
    let message = format!(
        "{} attempt to index a nil value",
        stack_era_c(folder, "core.lua", line)
    );
    BugEntry::new(&message, session, (TimeKind::Unix, when)).counter(counter)
}

/// `omaddons errors -t retail <extra> --json`, which must succeed with an envelope valid
/// against its schemas.
fn errors(s: &Scene, extra: &[&str]) -> (Value, Run) {
    let mut argv = vec!["errors", "-t", "retail"];
    argv.extend_from_slice(extra);
    let run = s.home.run(&argv);
    let envelope = run.expect(0);
    if let Err(failures) = validate::validate_envelope(&envelope, &run.stderr) {
        panic!("errors' envelope does not validate:\n{failures}"); // ubs:ignore — the test's own failure
    }
    (envelope, run)
}

/// The human output of `omaddons errors -t retail`.
fn human(s: &Scene) -> String {
    let argv: Vec<String> = ["errors", "-t", "retail"].map(str::to_owned).to_vec();
    let run = s.home.exec(&argv);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

fn addons(envelope: &Value) -> Vec<Value> {
    envelope["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .clone()
}

/// The error at `folder`'s `core.lua:<line>`.
fn at_line(envelope: &Value, folder: &str, line: u32) -> Value {
    let want = format!("{folder}/core.lua:{line}");
    addons(envelope)
        .iter()
        .flat_map(|a| a["errors"].as_array().unwrap().clone())
        .find(|e| e["location"].as_str().is_some_and(|l| l.ends_with(&want)))
        .unwrap_or_else(|| panic!("no error at {want}: {envelope}")) // ubs:ignore — the test's own failure
}

/// The row of `addon`.
fn addon(envelope: &Value, name: &str) -> Value {
    addons(envelope)
        .into_iter()
        .find(|a| a["addon"] == name)
        .unwrap_or_else(|| panic!("no addon {name}: {envelope}")) // ubs:ignore — the test's own failure
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

/// Moves every history line's time back by `by`: changes made that long ago.
fn age_history(s: &Scene, by: SignedDuration) {
    let path = s.home.state().join("history.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut aged = String::new();
    for line in text.lines() {
        let mut entry: Value = serde_json::from_str(line).unwrap();
        let at: Timestamp = entry["at"].as_str().unwrap().parse().unwrap();
        entry["at"] = json!((at - by).to_string());
        aged.push_str(&serde_json::to_string(&entry).unwrap());
        aged.push('\n');
    }
    fs::write(&path, aged).unwrap();
}

/// Foo 1 installed from its release three weeks ago, past the judge's window; then
/// BugGrabber's file saved in session 5 with nothing in it and read by a first `errors`:
/// the ledger's first read.
fn foo_installed() -> Scene {
    let s = Scene::new();
    s.release("foo", "v1", &[folder("Foo", &[])]);
    s.ok(&["install", "gh:o/foo", "-t", "retail"]);
    age_history(&s, SignedDuration::from_hours(21 * 24));
    save(&s, &BugDb::new(Writer::Modern, 5));
    errors(&s, &[]);
    s
}

/// `update` takes Foo to `tag`, its light read leaving a mark of session 5.
fn update_foo(s: &Scene, tag: &str) {
    s.release("foo", tag, &[folder("Foo", &[])]);
    s.ok(&["update", "-t", "retail", "--refresh"]);
}

/// Foo updated to v2 with the game closed, then an error Foo never threw, seven times in
/// the first session after: design 5.1. Returns the error's own time.
fn regression(s: &Scene) -> i64 {
    update_foo(s, "v2");
    let when = a_second_later();
    save(
        s,
        &BugDb::new(Writer::Modern, 6).entry(error("Foo", 12, 6, when, 7)),
    );
    when
}

/// `link` with what changes from run to run in place of its values.
fn shape(link: &Value) -> Value {
    let mut link = link.clone();
    for key in ["tx", "run", "at", "judged_at"] {
        if link[key].is_string() {
            link[key] = json!(format!("<{key}>"));
        }
    }
    if link["evidence"]["gap_s"].is_u64() {
        link["evidence"]["gap_s"] = json!("<gap_s>");
    }
    link
}

#[test]
fn a_direct_update_followed_by_a_new_error_names_the_update_and_keeps_the_judgment() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    log.step("Foo updated to v2, then a new error in its first session (design 5.1)");
    let when = regression(&s);
    let update = last_tx(&s, "update");
    let (first, _) = errors(&s, &[]);
    let e = at_line(&first, "Foo", 12);
    log.json("the error", &e);
    assert_eq!(e["first_seen"]["basis"], "observed");
    assert_eq!(
        e["first_seen"]["onset_by_at"],
        json!(Timestamp::from_second(when).unwrap().to_string())
    );
    assert_eq!(e["judgment"], json!({"state": "complete", "gap": null}));
    let link = &e["after_change"];
    assert_eq!(link["tx"], update.as_str());
    assert_eq!(
        shape(link),
        json!({
            "tx": "<tx>", "run": "<run>", "kind": "update", "at": "<at>", "actor": "agent",
            "relation": "own", "confidence": "high", "state": "current", "algorithm": 1,
            "judged_at": "<judged_at>",
            "changed": [{
                "addon": "Foo", "change": "updated", "from_version": "v1", "to_version": "v2",
                "from_release": {"source": "gh:o/foo", "tag": "v1"},
                "to_release": {"source": "gh:o/foo", "tag": "v2"},
                "still_installed": true,
            }],
            "evidence": {
                "gap_s": "<gap_s>", "exact": false, "same_session": false, "clean_sessions": 0,
                "client_update": null, "other_changes": 0, "unknowns": [],
            },
            "commands": [],
        }),
        "design 5.1's shape, without commands (RG-40)"
    );
    assert_eq!(
        addon(&first, "Foo")["after_change"],
        json!({
            "tx": update, "kind": "update", "at": link["at"], "addon": "Foo",
            "relation": "own", "confidence": "high",
        })
    );
    let text = human(&s);
    log.json("human", &text);
    for want in [
        format!("in the first session after update {update}"),
        "that update replaced Foo v1 → v2 · likely".to_owned(),
    ] {
        assert!(text.contains(&want), "{want:?} in:\n{text}");
    }

    log.step("a second run reads no history and returns the same link");
    let (second, run) = errors(&s, &["-vv"]);
    assert!(
        run.stderr.contains("errors ledger: judged 0"),
        "{}",
        run.stderr
    );
    assert_eq!(at_line(&second, "Foo", 12)["after_change"], *link);

    log.step("Foo moves on to v3: the link stays, its release is no longer installed");
    update_foo(&s, "v3");
    let (moved, _) = errors(&s, &[]);
    let kept = &at_line(&moved, "Foo", 12)["after_change"];
    assert_eq!(
        (&kept["judged_at"], &kept["changed"][0]["still_installed"]),
        (&link["judged_at"], &json!(false))
    );
}

#[test]
fn an_error_no_change_preceded_has_no_link() {
    let log = common::tlog::tlog!();
    let s = Scene::new();
    log.env(&s);
    addon_folder(&s.addons(), "Foo", &[("Foo.toc", &toc("120105", &[]))], &[]);
    save(&s, &BugDb::new(Writer::Modern, 5));
    errors(&s, &[]);
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 6).entry(error("Foo", 12, 6, when, 1)),
    );
    let (v, _) = errors(&s, &[]);
    let e = at_line(&v, "Foo", 12);
    log.json("the error", &e);
    assert_eq!(e["first_seen"]["basis"], "observed");
    assert_eq!(e["judgment"], json!({"state": "complete", "gap": null}));
    assert_eq!(e["after_change"], Value::Null);
    assert_eq!(addon(&v, "Foo")["after_change"], Value::Null);
}

#[test]
fn a_judgment_waits_for_an_unreadable_history_and_outlives_a_pruned_snapshot() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    let update = last_tx(&s, "update");
    log.step("Bar installed after the error: its light read records the appearance");
    s.run(&s.install_args("gh:o/bar", "bar"), "").expect(0);
    let cluster = retail(&s).clusters.into_values().next().unwrap();
    assert!(cluster.judgment.is_none(), "{cluster:?}");

    log.step("history.jsonl unreadable: the judgment is deferred, the run succeeds");
    let history = s.home.state().join("history.jsonl");
    fs::set_permissions(&history, fs::Permissions::from_mode(0o000)).unwrap();
    let (deferred, _) = errors(&s, &[]);
    let text = human(&s);
    fs::set_permissions(&history, fs::Permissions::from_mode(0o644)).unwrap();
    let e = at_line(&deferred, "Foo", 12);
    log.json("the error", &e);
    assert_eq!(
        (&e["judgment"], &e["after_change"]),
        (
            &json!({"state": "deferred", "gap": "history_unreadable"}),
            &Value::Null
        )
    );
    assert!(
        text.contains("it looks for the change before this at the next read"),
        "{text}"
    );

    log.step("the update's snapshot pruned; history readable again: judged from its line");
    let snapshots = ClientPaths::new(s.home.client("_retail_")).snapshots_dir();
    let mut pruned = 0;
    for dir in fs::read_dir(&snapshots).unwrap() {
        let dir = dir.unwrap().path();
        if dir.to_string_lossy().contains(&update) {
            fs::remove_dir_all(&dir).unwrap();
            pruned += 1;
        }
    }
    assert_eq!(
        pruned,
        1,
        "the update's snapshot in {}",
        snapshots.display()
    );
    let (judged, _) = errors(&s, &[]);
    let link = &at_line(&judged, "Foo", 12)["after_change"];
    log.json("the link", link);
    assert_eq!(
        (&link["tx"], &link["confidence"]),
        (&json!(update), &json!("high"))
    );
    assert_eq!(
        (
            &link["changed"][0]["from_version"],
            &link["changed"][0]["to_version"]
        ),
        (&json!("v1"), &json!("v2")),
        "the versions the update's history line recorded"
    );
}

/// Foo requires Lib; both installed at v1, then the file's first read; then Lib updated
/// to v2 and a new error in Foo in the first session after.
fn library_regression() -> Scene {
    let s = Scene::new();
    s.release("lib", "v1", &[folder("Lib", &[])]);
    s.release("foo", "v1", &[folder("Foo", &["Lib"])]);
    s.ok(&["install", "gh:o/lib", "gh:o/foo", "-t", "retail"]);
    save(&s, &BugDb::new(Writer::Modern, 5));
    errors(&s, &[]);
    s.release("lib", "v2", &[folder("Lib", &[])]);
    s.ok(&["update", "-t", "retail", "--refresh"]);
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 6).entry(error("Foo", 12, 6, when, 3)),
    );
    s
}

#[test]
fn a_dependency_updated_again_since_is_not_asserted_from_the_inventory_now() {
    let log = common::tlog::tlog!();
    log.step("control: Lib's update is the dependency Foo's error followed");
    let control = library_regression();
    log.env(&control);
    let (v, _) = errors(&control, &[]);
    let link = &at_line(&v, "Foo", 12)["after_change"];
    log.json("the link", link);
    assert_eq!(
        (
            &link["relation"],
            &link["confidence"],
            &link["changed"][0]["addon"]
        ),
        (&json!("dependency"), &json!("medium"), &json!("Lib"))
    );

    log.step("Lib updated again before the first errors run: the edge is not asserted");
    let s = library_regression();
    s.release("lib", "v3", &[folder("Lib", &[])]);
    s.ok(&["update", "-t", "retail", "--refresh"]);
    let (v, _) = errors(&s, &[]);
    let link = &at_line(&v, "Foo", 12)["after_change"];
    log.json("the link", link);
    assert_ne!(link["relation"], "dependency");
    let unknowns: BTreeSet<&str> = link["evidence"]["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u.as_str().unwrap())
        .collect();
    assert!(unknowns.contains("dependencies"), "{unknowns:?}");
}

#[test]
fn another_error_of_the_shape_inherits_nothing() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    let when = regression(&s);
    errors(&s, &[]);
    log.step("the same bug two lines down: one family, another fingerprint");
    save(
        &s,
        &BugDb::new(Writer::Modern, 7)
            .entry(error("Foo", 12, 6, when, 7))
            .entry(error("Foo", 14, 7, a_second_later(), 1)),
    );
    let (v, _) = errors(&s, &[]);
    let (a, b) = (at_line(&v, "Foo", 12), at_line(&v, "Foo", 14));
    log.json("the second error", &b);
    assert_eq!(b["first_seen"]["basis"], "related");
    assert_eq!(b["first_seen"]["related_fingerprint"], a["fingerprint"]);
    assert_eq!(
        (&b["after_change"], &b["judgment"]),
        (&Value::Null, &Value::Null)
    );
    assert!(a["after_change"].is_object());
}

#[test]
fn a_cluster_does_not_stretch_past_its_members() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    update_foo(&s, "v2");
    log.step("seventeen errors of one family appear at once");
    // A cluster keeps the first sixteen fingerprints in sorted order: the one left out is
    // the greatest. It occurs most often, so the view (ten per addon, most frequent
    // first) shows it. Its fingerprint is worked out as `observe` makes one.
    let fingerprint = |line: u32| {
        let entry = error("Foo", line, 6, 0, 1);
        let message = &entry.message;
        let location = strict_message(message).location.map(|l| normalized(&l));
        sanitize::fingerprint(&normalized(message), location.as_deref())
    };
    let left_out = (1..=17).max_by_key(|line| fingerprint(*line)).unwrap();
    let when = a_second_later();
    let mut db = BugDb::new(Writer::Modern, 6);
    for line in 1..=17 {
        let counter = if line == left_out { 9 } else { 1 };
        db = db.entry(error("Foo", line, 6, when, counter));
    }
    save(&s, &db);
    let (v, _) = errors(&s, &[]);
    let cluster = retail(&s).clusters.into_values().next().unwrap();
    assert_eq!((cluster.fingerprints.len(), cluster.overflowed), (16, true));
    let shown: Vec<Value> = addon(&v, "Foo")["errors"].as_array().unwrap().clone();
    let (members, others): (Vec<&Value>, Vec<&Value>) = shown.iter().partition(|e| {
        cluster
            .fingerprints
            .iter()
            .any(|f| e["fingerprint"] == f.as_str())
    });
    log.json("the errors not in the cluster", &others);
    assert_eq!(
        others.len(),
        1,
        "the seventeenth is among the {} shown",
        shown.len()
    );
    for e in members {
        assert_eq!(e["after_change"]["confidence"], "high", "{e}");
    }
    for e in &others {
        assert_eq!(e["first_seen"]["basis"], "related", "{e}");
        assert_eq!(
            (&e["after_change"], &e["judgment"]),
            (&Value::Null, &Value::Null)
        );
    }
}

#[test]
fn a_link_past_its_freshness_or_its_file_is_history_only() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);

    log.step("the ledger aged 31 days: the link is expired, and off the addon's row");
    Epoch::ago(SignedDuration::ZERO).age_ledger(&s.home, SignedDuration::from_hours(31 * 24));
    let (v, _) = errors(&s, &[]);
    let e = at_line(&v, "Foo", 12);
    assert_eq!(e["after_change"]["state"], "expired");
    assert_eq!(addon(&v, "Foo")["after_change"], Value::Null);
    assert!(human(&s).contains("more than 30 days ago"));
}

#[test]
fn a_link_whose_file_was_put_back_is_history_only() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    log.step("WTF/ backed up, then put back: the file's lifetime ends");
    let created = s.ok(&["backup", "create", "-t", "retail"]);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    s.ok(&["backup", "restore", &id, "-t", "retail"]);
    let (v, _) = errors(&s, &[]);
    let e = at_line(&v, "Foo", 12);
    log.json("the error", &e);
    assert_eq!(e["after_change"]["state"], "database_replaced");
    assert_eq!(addon(&v, "Foo")["after_change"], Value::Null);
}

#[test]
fn a_weak_link_stays_off_the_addon_s_row() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    addon_folder(&s.addons(), "Bar", &[("Bar.toc", &toc("120105", &[]))], &[]);
    errors(&s, &[]);
    update_foo(&s, "v2");
    log.step("an error in Bar, which no change touched, right after Foo's update");
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 6).entry(error("Bar", 3, 6, when, 1)),
    );
    let (v, _) = errors(&s, &[]);
    let link = &at_line(&v, "Bar", 3)["after_change"];
    log.json("the link", link);
    assert_eq!(
        (&link["relation"], &link["confidence"]),
        (&json!("none"), &json!("low"))
    );
    assert_eq!(addon(&v, "Bar")["after_change"], Value::Null);
    let text = human(&s);
    assert!(
        text.contains("(only the order in time connects them) · weak"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------------------
// `status` shows a link from the errors memo, checked before it shows (RG-17, design
// 7.3.2). Here rather than in `tests/status_errors.rs`, whose frozen clock cannot run a
// change before an error the ledger judges.

/// `omaddons status <extra> --json`, valid against its schemas.
fn status(s: &Scene, extra: &[&str]) -> Value {
    let mut argv = vec!["status"];
    argv.extend_from_slice(extra);
    let run = s.home.run(&argv);
    let envelope = run.expect(0);
    if let Err(failures) = validate::validate_envelope(&envelope, &run.stderr) {
        panic!("status's envelope does not validate:\n{failures}"); // ubs:ignore — the test's own failure
    }
    envelope
}

/// The link `status` shows for `name` on the first client, `Null` for none.
fn shown(envelope: &Value, name: &str) -> Value {
    envelope["data"]["targets"][0]["errors"]["top_addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == name)
        .map_or(Value::Null, |a| a["after_change"].clone())
}

/// What both forms of `status` show for `name`: `(cached, plain)`.
fn both(s: &Scene, name: &str) -> (Value, Value) {
    (
        shown(&status(s, &["--cached"]), name),
        shown(&status(s, &[]), name),
    )
}

fn status_human(s: &Scene, extra: &[&str]) -> String {
    let mut argv: Vec<String> = vec!["status".to_owned()];
    argv.extend(extra.iter().map(|a| (*a).to_owned()));
    let run = s.home.exec(&argv);
    assert_eq!(run.code, 0, "{}", run.stderr);
    run.stdout
}

fn memo(s: &Scene) -> Value {
    serde_json::from_slice(&fs::read(paths(s).errors_memo()).unwrap()).unwrap()
}

/// Every link the memo holds, per database (in its order), for `addon`.
fn memo_links(s: &Scene, addon: &str) -> Vec<usize> {
    let memo = memo(s);
    let target = memo["targets"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    target["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|db| {
            db["addons"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["addon"] == addon)
                .map(|a| a["links"].as_array().map_or(0, Vec::len))
                .sum()
        })
        .collect()
}

#[test]
fn status_shows_a_strong_link_in_both_forms_and_says_so_in_words() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    let update = last_tx(&s, "update");
    errors(&s, &[]);
    log.step("both forms of status show the update, the cached one from the memo alone");
    let (cached, plain) = both(&s, "Foo");
    log.json("cached", &cached);
    assert_eq!(cached, plain);
    assert_eq!(
        (
            &cached["tx"],
            &cached["kind"],
            &cached["relation"],
            &cached["confidence"],
            &cached["addon"]
        ),
        (
            &json!(update),
            &json!("update"),
            &json!("own"),
            &json!("high"),
            &json!("Foo")
        )
    );
    let text = status_human(&s, &["--cached"]);
    log.json("human", &text);
    assert!(text.contains("first seen after an update"), "{text}");
    assert!(
        text.contains("Foo 1, likely since its update today"),
        "{text}"
    );
}

#[test]
fn a_medium_link_shows_without_the_headline_and_a_low_one_never_shows() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    log.step("Foo's error once in its second session after the update: medium");
    update_foo(&s, "v2");
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 7).entry(error("Foo", 12, 7, when, 1)),
    );
    errors(&s, &[]);
    let (cached, plain) = both(&s, "Foo");
    assert_eq!(
        (&cached["confidence"], &plain["confidence"]),
        (&json!("medium"), &json!("medium"))
    );
    let text = status_human(&s, &[]);
    assert!(!text.contains("first seen after an update"), "{text}");
    assert!(text.contains("possible since its update"), "{text}");

    log.step("an error in Bar, which nothing touched: a low link, kept out of status");
    let s = foo_installed();
    addon_folder(&s.addons(), "Bar", &[("Bar.toc", &toc("120105", &[]))], &[]);
    errors(&s, &[]);
    update_foo(&s, "v2");
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 6).entry(error("Bar", 3, 6, when, 1)),
    );
    let (v, _) = errors(&s, &[]);
    assert_eq!(at_line(&v, "Bar", 3)["after_change"]["confidence"], "low");
    assert_eq!(both(&s, "Bar"), (Value::Null, Value::Null));
    assert_eq!(
        memo_links(&s, "Bar"),
        vec![0],
        "a low link is never projected"
    );
}

#[test]
fn a_dismissal_hides_the_link_at_once_and_keeps_it_in_the_memo() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    log.step("errors dismiss: no errors run after it");
    s.ok(&["errors", "dismiss", "-t", "retail"]);
    assert_eq!(
        memo_links(&s, "Foo"),
        vec![1],
        "the dismissal kept the unchanged file's link"
    );
    assert_eq!(shown(&status(&s, &["--cached"]), "Foo"), Value::Null);
    log.step("a later errors run does not bring it back while the error is not new");
    errors(&s, &[]);
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
}

#[test]
fn a_reset_file_shows_no_link_in_either_form_before_any_errors_run() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    assert!(shown(&status(&s, &["--cached"]), "Foo").is_object());
    log.step("BugGrabber's file reset: session 1, the error again");
    save(
        &s,
        &BugDb::new(Writer::Modern, 1).entry(error("Foo", 12, 1, a_second_later(), 1)),
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
}

#[test]
fn a_restore_whose_light_read_never_committed_still_hides_the_link() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    log.step("a backup holding the error in a later session");
    save(
        &s,
        &BugDb::new(Writer::Modern, 9).entry(error("Foo", 12, 9, a_second_later(), 2)),
    );
    let created = s.ok(&["backup", "create", "-t", "retail"]);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    log.step("restored while another process holds the evidence lock");
    let scopes = omaddons::fsx::ScopeTable::new(&paths(&s), &[], None).unwrap();
    let held = omaddons::errors::ledger::open(&paths(&s), &scopes, "test").unwrap();
    s.ok(&["backup", "restore", &id, "-t", "retail"]);
    drop(held);
    assert_eq!(
        memo_links(&s, "Foo"),
        vec![1],
        "the light read never committed"
    );
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
}

#[test]
fn two_accounts_at_one_session_show_the_link_once_from_its_own_database() {
    let log = common::tlog::tlog!();
    let s = Scene::new();
    log.env(&s);
    s.release("foo", "v1", &[folder("Foo", &[])]);
    s.ok(&["install", "gh:o/foo", "-t", "retail"]);
    age_history(&s, SignedDuration::from_hours(21 * 24));
    // Foo's known error: another file and text, so another family than the new one.
    let known = |session: u32, when: i64| {
        let message = format!(
            "{} attempt to call a nil value",
            stack_era_c("Foo", "util.lua", 40)
        );
        BugEntry::new(&message, session, (TimeKind::Unix, when))
    };
    let old = known(5, a_second_later());
    let second = |db: &BugDb| {
        write_files(
            &s.home.client("_retail_").join("WTF"),
            &[db.file_of("SECONDACCT")],
        );
        std::thread::sleep(Duration::from_millis(60));
    };
    save(&s, &BugDb::new(Writer::Modern, 5).entry(old.clone()));
    second(&BugDb::new(Writer::Modern, 5));
    errors(&s, &[]);
    update_foo(&s, "v2");
    log.step("account 1: a new error; account 2, also at session 6: Foo's known one");
    let when = a_second_later();
    save(
        &s,
        &BugDb::new(Writer::Modern, 6)
            .entry(old.clone())
            .entry(error("Foo", 12, 6, when, 1)),
    );
    second(&BugDb::new(Writer::Modern, 6).entry(known(6, when)));
    errors(&s, &[]);
    let links = memo_links(&s, "Foo");
    log.json("links per database", &links);
    assert_eq!(
        links.iter().sum::<usize>(),
        1,
        "one link, in one database: {links:?}"
    );
    let (cached, plain) = both(&s, "Foo");
    assert_eq!(
        (&cached["confidence"], &plain["confidence"]),
        (&json!("high"), &json!("high"))
    );
}

#[test]
fn a_memo_projected_from_a_newer_ledger_is_never_overwritten() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    log.step("the memo on disk says it was projected from a far newer revision");
    let mut newer = memo(&s);
    for target in newer["targets"].as_object_mut().unwrap().values_mut() {
        target["ledger_revision"] = json!(1_000_000);
    }
    let bytes = serde_json::to_vec_pretty(&newer).unwrap();
    fs::write(paths(&s).errors_memo(), &bytes).unwrap();
    errors(&s, &[]);
    assert_eq!(fs::read(paths(&s).errors_memo()).unwrap(), bytes);
}

#[test]
fn a_link_whose_error_is_gone_is_not_lent_to_another_new_error() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    log.step("the linked error is gone; Foo has another new error in the next session");
    let message = format!(
        "{} attempt to call a nil value",
        stack_era_c("Foo", "util.lua", 40)
    );
    let other = BugEntry::new(&message, 7, (TimeKind::Unix, a_second_later()));
    save(&s, &BugDb::new(Writer::Modern, 7).entry(other));
    let plain = status(&s, &[]);
    let foo = plain["data"]["targets"][0]["errors"]["top_addons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["addon"] == "Foo")
        .unwrap()
        .clone();
    log.json("Foo in plain status", &foo);
    assert_eq!(
        (&foo["new"], &foo["after_change"]),
        (&json!(1), &Value::Null)
    );
}

#[test]
fn a_restore_s_light_read_removes_the_links_of_the_file_it_replaced() {
    let log = common::tlog::tlog!();
    let s = foo_installed();
    log.env(&s);
    regression(&s);
    errors(&s, &[]);
    assert_eq!(memo_links(&s, "Foo"), vec![1]);
    let created = s.ok(&["backup", "create", "-t", "retail"]);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    log.step("restored: its light read ends the file's lifetime and drops its links");
    s.ok(&["backup", "restore", &id, "-t", "retail"]);
    assert_eq!(memo_links(&s, "Foo"), vec![0]);
    let bell: Value = serde_json::from_slice(
        &fs::read(s.home.env.cache().join("omaddons/doorbell.json")).unwrap(),
    )
    .unwrap();
    log.json("the doorbell", &bell);
    assert!(bell["at"].is_string(), "the memo write rang it");
    assert_eq!(both(&s, "Foo"), (Value::Null, Value::Null));
}
