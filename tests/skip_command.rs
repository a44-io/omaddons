//! e2e suite: the commands skip and unskip, records-only marks a preview binds to the release it marks.
//!
//! RG-33. `skip` marks the release the release memo says is waiting (or one named by id
//! or by label), `unskip` clears marks (all of an addon's, one release's, one label's, or
//! one transaction's). Both are records-only transactions of kind `skip`: they touch no
//! folder, run while the client runs, make no request, and `undo` reverts them. A
//! preview binds what it marks: a stored plan whose waiting release moved in the memo
//! before `apply` is `plan_stale` and writes nothing. The scenarios of the regression
//! guard's slice (a release that was left stays skipped) are `tests/skip.rs` (RG-35).

use crate::common;

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::addons::toc;
use common::home::Home;
use common::http::{Asset, paths};
use common::netlog::net_log;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::tree_hash;
use omaddons::model::{SkipOrigin, SkippedRelease};
use proptest::prelude::*;
use serde_json::{Value, json};

const FOO: &str = "https://github.com/o/foo";
const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

/// One release of Foo for retail: its TOC and a `core.lua` that differs per tag.
struct Rel {
    toc: String,
    lua: String,
}

impl Rel {
    fn new(tag: &str) -> Self {
        Self {
            toc: toc("120105", &["## Title: Foo", &format!("## Version: {tag}")]),
            lua: format!("-- Foo {tag}"),
        }
    }

    fn assets(&self) -> [Asset<'_>; 1] {
        [(
            "Foo.zip",
            MAINLINE,
            vec![
                ("Foo/Foo.toc", self.toc.as_str()),
                ("Foo/core.lua", self.lua.as_str()),
            ],
        )]
    }
}

/// `omaddons <args> --json`, logged.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv: Vec<&str> = args.to_vec();
    argv.push("--json");
    log.run(home, &argv)
}

/// Retail with Foo v1 installed while v2 is out, and an `outdated` that saw it: the
/// release memo offers v2.
fn behind(log: &TestLog) -> Home {
    log.step("Foo v1 installed, v2 out and checked");
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    let (v1, v2) = (Rel::new("v1"), Rel::new("v2"));
    home.installed_behind(FOO, ("v1", &v1.assets()), ("v2", &v2.assets()), &["retail"]);
    json(log, &home, &["outdated", "-t", "retail"]).ok();
    home
}

/// Foo's marks on retail, as its record holds them.
fn marks(home: &Home) -> Vec<SkippedRelease> {
    common::state::record(home, "retail").addons["foo"]
        .skipped_releases
        .clone()
}

/// What each mark selects, as `skips_marked` and the record's key spell it.
#[cfg(test)]
fn selected(marks: &[SkippedRelease]) -> Vec<Value> {
    marks
        .iter()
        .map(|m| serde_json::to_value(m.selects()).unwrap())
        .collect()
}

/// Rewrites only the release memo, so that Foo's newest release is `tag`.
fn memo_offers(home: &Home, tag: &str) {
    let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
    for entry in memo.entries.values_mut() {
        if let Some(set) = &mut entry.set {
            for release in &mut set.releases {
                release.release_id = format!("gh:o/foo@{tag}");
                release.version = tag.to_owned();
            }
        }
        entry.probe = Some(format!("gh:o/foo@{tag}"));
    }
    home.seed_memo(&memo);
}

/// The record file of retail, as bytes.
#[cfg(test)]
fn record_bytes(home: &Home) -> Vec<u8> {
    let key = common::state::key_of(home, "retail");
    fs::read(paths(&home.env).target_record(&key)).unwrap()
}

#[test]
fn a_skip_marks_the_waiting_release_and_undo_reverts_it() {
    let log = tlog!();
    let mut home = behind(&log);
    home.retail_running();
    let addons = tree_hash(&home.addons("_retail_"));
    let requests = net_log(&home).len();

    log.step("skip Foo while the client runs");
    let ran = json(&log, &home, &["skip", "foo", "-t", "retail"]);
    ran.ok();
    let data = ran.data();
    let target = &data["targets"][0];
    log.assert_eq("kind", &target["kind"], &json!("skip"));
    log.assert_eq("item", &target["items"][0]["status"], &json!("skip_marked"));
    log.assert_eq(
        "the mark it made",
        &target["skips_marked"],
        &json!([{"addon": "Foo", "version": "v2", "release": {"source": "gh:o/foo", "tag": "v2"}}]),
    );
    let tx = target["tx"].as_str().unwrap().to_owned();
    let made = marks(&home);
    log.assert_eq("one mark", &made.len(), &1);
    log.assert_eq(
        "by its exact key",
        &selected(&made),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v2"}})],
    );
    log.assert_eq("made by hand", &made[0].origin, &SkipOrigin::Manual);
    log.assert_eq("by this transaction", &made[0].tx.to_string(), &tx);
    log.assert_eq(
        "no folder moved",
        &tree_hash(&home.addons("_retail_")),
        &addons,
    );
    log.assert_eq("no request", &net_log(&home).len(), &requests);
    let history = common::state::history(&home);
    let last = history.last().unwrap();
    log.assert_eq(
        "its history line",
        &serde_json::to_value(last.kind).unwrap(),
        &json!("skip"),
    );
    let unskip: Vec<Vec<String>> = ran
        .commands()
        .into_iter()
        .filter(|c| c.action == "unskip")
        .map(|c| c.argv)
        .collect();
    log.assert_eq(
        "offered again by exactly this transaction",
        &unskip,
        &vec![vec![
            "omaddons".to_owned(),
            "unskip".to_owned(),
            "--tx".to_owned(),
            tx.clone(),
            "-t".to_owned(),
            "retail".to_owned(),
        ]],
    );

    log.step("the human line names the release");
    let human = log.run(&home, &["skip", "foo", "-t", "retail"]);
    human.ok();
    assert!(
        human
            .stdout()
            .contains("retail: Foo: v2 is skipped already"),
        "{}",
        human.stdout()
    );

    log.step("undo takes the mark back");
    json(&log, &home, &["undo", &tx]).ok();
    log.assert_eq("no mark", &marks(&home).len(), &0);
}

#[test]
fn a_pinned_addon_with_a_newer_release_has_that_release_marked() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["pin", "foo", "-t", "retail"]).ok();
    log.step("skip the pinned Foo");
    let ran = json(&log, &home, &["skip", "foo", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "its newer release",
        &selected(&marks(&home)),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v2"}})],
    );
}

#[test]
fn nothing_waiting_is_unchanged_with_no_transaction() {
    let log = tlog!();
    let home = behind(&log);

    log.step("v2 installed: nothing is waiting");
    json(&log, &home, &["update", "foo", "-t", "retail"]).ok();
    let lines = common::state::history(&home).len();
    let ran = json(&log, &home, &["skip", "foo", "-t", "retail", "-v"]);
    ran.ok();
    let target = &ran.data()["targets"][0];
    log.assert_eq(
        "unchanged",
        &target["items"][0]["status"],
        &json!("unchanged"),
    );
    log.assert_eq("no transaction", &target["tx"], &Value::Null);
    log.assert_eq(
        "no history line",
        &common::state::history(&home).len(),
        &lines,
    );
    let said = ran.events().iter().any(|e| {
        e["message"]
            .as_str()
            .is_some_and(|m| m.contains("Foo unchanged: v2 is the newest release"))
    });
    assert!(said, "a -v note says why: {:?}", ran.events());

    log.step("the memo forgotten: run outdated first");
    fs::remove_file(home.memo()).unwrap();
    let human = log.run(&home, &["skip", "foo", "-t", "retail"]);
    human.ok();
    assert!(
        human.stdout().contains(
            "nothing is remembered about its releases: run `omaddons outdated -t retail` first"
        ),
        "{}",
        human.stdout()
    );
    log.assert_eq(
        "still no history line",
        &common::state::history(&home).len(),
        &lines,
    );
}

#[test]
fn release_and_label_marks() {
    let log = tlog!();
    let home = behind(&log);

    log.step("--release marks exactly that release, whatever the memo offers");
    let ran = json(
        &log,
        &home,
        &["skip", "foo", "--release", "gh:o/foo@v7", "-t", "retail"],
    );
    ran.ok();
    log.assert_eq(
        "an exact mark on v7",
        &selected(&marks(&home)),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v7"}})],
    );

    log.step("--release with --revision marks that revision");
    json(
        &log,
        &home,
        &[
            "skip",
            "foo",
            "--release",
            "gh:O/Foo@v8",
            "--revision",
            "12",
            "-t",
            "retail",
        ],
    )
    .ok();
    log.assert_eq(
        "its revision is kept, the source spelled canonically",
        &selected(&marks(&home))[1],
        &json!({"release": {"source": "gh:o/foo", "tag": "v8", "revision": 12}}),
    );

    log.step("--version makes a label mark, and says so");
    let human = log.run(&home, &["skip", "foo", "--version", "v9", "-t", "retail"]);
    human.ok();
    assert!(
        human
            .stdout()
            .contains("every release labelled v9 (a label mark) is skipped"),
        "{}",
        human.stdout()
    );
    log.assert_eq(
        "a label",
        &selected(&marks(&home))[2],
        &json!({"label": "9"}),
    );

    log.step("refusals");
    json(
        &log,
        &home,
        &["skip", "foo", "--release", "gh:o/other@v1", "-t", "retail"],
    )
    .fails(2, "usage");
    json(
        &log,
        &home,
        &["skip", "foo", "--release", "v1", "-t", "retail"],
    )
    .fails(2, "usage");
    json(
        &log,
        &home,
        &["skip", "foo", "bar", "--version", "1", "-t", "retail"],
    )
    .fails(2, "usage");
    json(
        &log,
        &home,
        &["skip", "foo", "--revision", "3", "-t", "retail"],
    )
    .fails(2, "usage");
    log.assert_eq("three marks still", &marks(&home).len(), &3);
}

#[test]
fn unskip_clears_exactly_what_it_names() {
    let log = tlog!();
    let home = behind(&log);
    log.step("three marks: the waiting v2, a label 3.0, and v4 by id");
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    json(
        &log,
        &home,
        &["skip", "foo", "--version", "3.0", "-t", "retail"],
    )
    .ok();
    let v4 = json(
        &log,
        &home,
        &["skip", "foo", "--release", "gh:o/foo@v4", "-t", "retail"],
    );
    v4.ok();
    let v4_tx = v4.data()["targets"][0]["tx"].as_str().unwrap().to_owned();
    log.assert_eq("three marks", &marks(&home).len(), &3);

    log.step("--release of the label's version clears the label mark that holds it");
    let ran = json(
        &log,
        &home,
        &["unskip", "foo", "--release", "gh:o/foo@3.0", "-t", "retail"],
    );
    ran.ok();
    log.assert_eq(
        "the label mark went",
        &ran.data()["targets"][0]["skips_cleared"],
        &json!([{"addon": "Foo", "version": "3.0", "release": null}]),
    );
    log.assert_eq(
        "the others stay",
        &selected(&marks(&home)),
        &vec![
            json!({"release": {"source": "gh:o/foo", "tag": "v2"}}),
            json!({"release": {"source": "gh:o/foo", "tag": "v4"}}),
        ],
    );

    log.step("--tx clears the marks that transaction made");
    let ran = json(&log, &home, &["unskip", "--tx", &v4_tx, "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "v4 went",
        &selected(&marks(&home)),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v2"}})],
    );

    log.step("--version clears the marks labelled so");
    json(
        &log,
        &home,
        &["unskip", "foo", "--version", "2", "-t", "retail"],
    )
    .ok();
    log.assert_eq("none left", &marks(&home).len(), &0);

    log.step("no mark matches: unchanged");
    let ran = json(&log, &home, &["unskip", "foo", "-t", "retail"]);
    ran.ok();
    log.assert_eq("unchanged", &ran.data()["summary"]["unchanged"], &json!(1));
    log.assert_eq(
        "no transaction",
        &ran.data()["targets"][0]["tx"],
        &Value::Null,
    );
    let human = log.run(&home, &["unskip", "foo", "-t", "retail"]);
    human.ok();
    assert!(
        human
            .stdout()
            .contains("retail: Foo: no release is skipped"),
        "{}",
        human.stdout()
    );

    log.step("a bare unskip names nothing to clear");
    json(&log, &home, &["unskip", "-t", "retail"]).fails(2, "usage");
    json(&log, &home, &["unskip", "--tx", "nope", "-t", "retail"]).fails(2, "usage");

    log.step("plain unskip clears every mark, and says what update would install");
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    json(
        &log,
        &home,
        &["skip", "foo", "--version", "5", "-t", "retail"],
    )
    .ok();
    let human = log.run(&home, &["unskip", "foo", "-t", "retail"]);
    human.ok();
    log.assert_eq("every mark went", &marks(&home).len(), &0);
    assert!(
        human
            .stdout()
            .contains("retail: `omaddons update Foo -t retail` installs v2"),
        "{}",
        human.stdout()
    );
}

#[test]
fn the_257th_mark_is_usage_and_names_unskip() {
    let log = tlog!();
    let home = behind(&log);
    log.step("256 label marks on Foo's record");
    let key = common::state::key_of(&home, "retail");
    let path = paths(&home.env).target_record(&key);
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let full: Vec<SkippedRelease> = (0..256)
        .map(|n| SkippedRelease {
            release: None,
            version: format!("1.{n}"),
            at: "2026-11-01T00:00:00Z".parse().unwrap(),
            origin: SkipOrigin::Manual,
            tx: "01JB8Q2VA00000000000000001".parse().unwrap(),
        })
        .collect();
    record["addons"]["foo"]["skipped_releases"] = serde_json::to_value(&full).unwrap();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    log.step("a 257th is refused before it commits");
    let lines = common::state::history(&home).len();
    let ran = json(&log, &home, &["skip", "foo", "-t", "retail"]);
    ran.fails(2, "usage");
    let error = &ran.envelope()["error"];
    assert!(
        error["fix"]
            .as_str()
            .is_some_and(|f| f.starts_with("omaddons unskip Foo --release <release-id>")),
        "{error}"
    );
    log.assert_eq("no command to run as is", &error["fix_argv"], &Value::Null);
    log.assert_eq(
        "nothing committed",
        &common::state::history(&home).len(),
        &lines,
    );
    log.assert_eq("every mark kept", &marks(&home).len(), &256);

    log.step("after an unskip, it goes through");
    json(
        &log,
        &home,
        &["unskip", "foo", "--version", "1.0", "-t", "retail"],
    )
    .ok();
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    log.assert_eq("256 again", &marks(&home).len(), &256);
}

#[test]
fn an_addon_installed_nowhere_is_not_managed() {
    let log = tlog!();
    let home = behind(&log);
    log.step("skip an addon that is not installed");
    let ran = json(&log, &home, &["skip", "Nope", "-t", "retail"]);
    ran.fails(3, "not_managed");
    log.assert_eq(
        "the install fix",
        &ran.envelope()["error"]["fix_argv"],
        &json!(["omaddons", "install", "Nope"]),
    );
    json(&log, &home, &["unskip", "Nope", "-t", "retail"]).fails(3, "not_managed");
}

#[test]
fn a_preview_binds_the_release_it_marks() {
    let log = tlog!();
    let home = behind(&log);

    log.step("preview a skip while the memo offers v2");
    let preview = json(&log, &home, &["skip", "foo", "-t", "retail", "--dry-run"]);
    preview.ok();
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
    let token = preview.data()["precondition"].as_str().unwrap().to_owned();
    log.assert_eq(
        "it would mark v2",
        &preview.data()["targets"][0]["skips_marked"][0]["version"],
        &json!("v2"),
    );

    log.step("only the memo moves: it offers v3");
    memo_offers(&home, "v3");
    let before = record_bytes(&home);
    let applied = json(&log, &home, &["apply", &plan]);
    applied.fails(1, "plan_stale");
    log.assert_eq("no mark", &marks(&home).len(), &0);
    log.assert_eq("the record as it was", &record_bytes(&home), &before);
    json(
        &log,
        &home,
        &["skip", "foo", "-t", "retail", "--if-match", &token],
    )
    .fails(1, "plan_stale");
    log.assert_eq("still no mark", &marks(&home).len(), &0);

    log.step("the memo offers v2 again: apply marks v2, at its own time");
    memo_offers(&home, "v2");
    let started = jiff::Timestamp::now();
    let applied = json(&log, &home, &["apply", &plan]);
    applied.ok();
    let tx = applied.data()["outcomes"][0]["tx"]
        .as_str()
        .unwrap()
        .to_owned();
    let made = marks(&home);
    log.assert_eq(
        "v2 marked",
        &selected(&made),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v2"}})],
    );
    log.assert_eq("by the apply's transaction", &made[0].tx.to_string(), &tx);
    assert!(
        made[0].at >= started,
        "made at the apply, not the preview: {:?}",
        made[0].at
    );
}

#[test]
fn a_stored_unskip_plan_is_stale_once_its_mark_is_gone() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    log.step("preview offering v2 again");
    let preview = json(
        &log,
        &home,
        &[
            "unskip",
            "foo",
            "--release",
            "gh:o/foo@v2",
            "-t",
            "retail",
            "--dry-run",
        ],
    );
    preview.ok();
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
    log.step("the mark is cleared in between");
    json(&log, &home, &["unskip", "foo", "-t", "retail"]).ok();
    let before = record_bytes(&home);
    json(&log, &home, &["apply", &plan]).fails(1, "plan_stale");
    log.assert_eq("the record as it was", &record_bytes(&home), &before);
}

/// What [`warning`] answers for a code the envelope does not carry.
static NO_WARNING: Value = Value::Null;

/// The warning of `code` in `ran`'s envelope; `null` when there is none, so a check of
/// one of its fields fails naming that field.
fn warning<'r>(ran: &'r Ran, code: &str) -> &'r Value {
    ran.envelope()["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|w| w["code"] == code)
        .unwrap_or(&NO_WARNING)
}

/// Whether `ran` warns `code`.
fn warns(ran: &Ran, code: &str) -> bool {
    ran.warnings().iter().any(|w| w == code)
}

/// RG-31: a mark is honored. `outdated`, `status`, `list`, `info`, `update` and an
/// `install` without a version all pass over the release it holds, and a newer release
/// is an ordinary update, the mark staying on the record.
#[test]
fn a_mark_is_honored_until_a_newer_release() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    let addons = tree_hash(&home.addons("_retail_"));

    log.step("outdated shows the release skipped, and --check passes");
    let ran = json(&log, &home, &["outdated", "-t", "retail"]);
    ran.ok();
    let item = &ran.data()["targets"][0]["items"][0];
    log.assert_eq("status", &item["status"], &json!("skipped_release"));
    log.assert_eq("latest", &item["latest"]["version"], &json!("v2"));
    log.assert_eq(
        "the mark",
        &item["skipped_release"]["release"],
        &json!({"source": "gh:o/foo", "tag": "v2"}),
    );
    log.assert_eq(
        "counted apart",
        &(
            &ran.data()["summary"]["update_available"],
            &ran.data()["summary"]["skipped_release"],
        ),
        &(&json!(0), &json!(1)),
    );
    json(&log, &home, &["outdated", "-t", "retail", "--check"]).ok();

    log.step("status counts it skipped, never available");
    let ran = json(&log, &home, &["status", "-t", "retail"]);
    ran.ok();
    let updates = &ran.data()["targets"][0]["updates"];
    log.assert_eq(
        "available, skipped",
        &(&updates["available"], &updates["skipped"]),
        &(&json!(0), &json!(1)),
    );

    log.step("list -v and info name the mark");
    let ran = json(&log, &home, &["list", "-t", "retail", "-v"]);
    ran.ok();
    let listed = &ran.data()["targets"][0]["addons"][0];
    log.assert_eq(
        "latest.status",
        &listed["latest"]["status"],
        &json!("skipped_release"),
    );
    log.assert_eq(
        "skipped_releases",
        &listed["skipped_releases"].as_array().map(Vec::len),
        &Some(1),
    );
    let ran = json(&log, &home, &["info", "foo", "-t", "retail", "--offline"]);
    ran.ok();
    log.assert_eq(
        "info's marks",
        &ran.data()["installed"][0]["skipped_releases"][0]["version"],
        &json!("v2"),
    );

    log.step("bare update passes over it, without a warning");
    let ran = json(&log, &home, &["update"]);
    ran.ok();
    let target = &ran.data()["targets"][0];
    log.assert_eq("no transaction", &target["tx"], &Value::Null);
    log.assert_eq("skipped", &target["items"][0]["status"], &json!("skipped"));
    log.assert_eq(
        "why",
        &target["items"][0]["skip_reason"],
        &json!("skipped_release"),
    );
    log.assert_eq(
        "the release passed over",
        &target["items"][0]["to_version"],
        &json!("v2"),
    );
    log.assert_eq(
        "no release_skipped",
        &warns(&ran, "release_skipped"),
        &false,
    );

    log.step("update Foo warns, with the scoped unskip as its fix");
    let ran = json(&log, &home, &["update", "foo", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "fix_argv",
        &warning(&ran, "release_skipped")["fix_argv"],
        &json!([
            "omaddons",
            "unskip",
            "Foo",
            "--release",
            "gh:o/foo@v2",
            "-t",
            "retail"
        ]),
    );
    log.assert_eq(
        "about retail",
        &warning(&ran, "release_skipped")["targets"],
        &json!(["retail"]),
    );

    log.step("install Foo without a version is unchanged, with the warning");
    let ran = json(&log, &home, &["install", "foo", "-t", "retail"]);
    ran.ok();
    log.assert_eq(
        "unchanged",
        &ran.data()["items"][0]["status"],
        &json!("unchanged"),
    );
    log.assert_eq("release_skipped", &warns(&ran, "release_skipped"), &true);
    log.assert_eq(
        "no folder moved",
        &tree_hash(&home.addons("_retail_")),
        &addons,
    );

    log.step("the human lines say it");
    for (args, says) in [
        (&["outdated", "-t", "retail"][..], "skipped v2 (marked "),
        (&["outdated", "-t", "retail"][..], " · 1 skipped"),
        (
            &["update", "-t", "retail"][..],
            "Foo left at v1 · v2 is skipped",
        ),
        (&["status", "-t", "retail"][..], " · 1 skipped"),
        (
            &["info", "foo", "-t", "retail", "--offline"][..],
            "skipped: v2",
        ),
    ] {
        let human = log.run(&home, args);
        human.ok();
        log.assert_eq(
            &format!("{} says {says:?}", args.join(" ")),
            &human.stdout().contains(says),
            &true,
        );
    }

    log.step("v3 is out: an ordinary update, and the mark on v2 stays");
    let v3 = Rel::new("v3");
    common::http::gh_release_v(&home.http, FOO, "v3", &v3.assets());
    common::http::gh_latest(&home.http, FOO, "v3");
    let ran = json(&log, &home, &["outdated", "-t", "retail", "--refresh"]);
    ran.ok();
    log.assert_eq(
        "v3 waits",
        &(
            &ran.data()["targets"][0]["items"][0]["status"],
            &ran.data()["targets"][0]["items"][0]["latest"]["version"],
        ),
        &(&json!("update_available"), &json!("v3")),
    );
    json(&log, &home, &["update", "-t", "retail"]).ok();
    let record = common::state::record(&home, "retail");
    log.assert_eq("installed", &record.addons["foo"].version, &"v3".to_owned());
    log.assert_eq(
        "the mark on v2 is still there",
        &selected(&marks(&home)),
        &vec![json!({"release": {"source": "gh:o/foo", "tag": "v2"}})],
    );
}

/// RG-31: naming a skipped release installs it and lifts its marks, each said with
/// `skip_lifted` and taken back by the install's undo; `release_skipped`'s fix, run,
/// clears that release's mark and no other.
#[test]
fn naming_a_skipped_release_lifts_its_mark_and_the_fix_clears_only_its_own() {
    let log = tlog!();
    let home = behind(&log);
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    json(
        &log,
        &home,
        &["skip", "foo", "--version", "9.9", "-t", "retail"],
    )
    .ok();

    log.step("release_skipped's fix offers v2 again and keeps the label mark");
    let ran = json(&log, &home, &["update", "foo", "-t", "retail"]);
    ran.ok();
    let argv: Vec<String> =
        serde_json::from_value(warning(&ran, "release_skipped")["fix_argv"].clone()).unwrap();
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    json(&log, &home, &args).ok();
    log.assert_eq(
        "only the label mark is left",
        &selected(&marks(&home)),
        &vec![json!({"label": "9.9"})],
    );

    log.step("skip v2 again, then install it by name");
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    let ran = json(
        &log,
        &home,
        &["install", "foo", "--version", "v2", "-t", "retail"],
    );
    ran.ok();
    log.assert_eq(
        "installed v2",
        &ran.data()["items"][0]["to_version"],
        &json!("v2"),
    );
    let tx = ran.data()["tx"].as_str().unwrap().to_owned();
    log.assert_eq(
        "skip_lifted's fix undoes the install",
        &warning(&ran, "skip_lifted")["fix_argv"],
        &json!(["omaddons", "undo", tx]),
    );
    log.assert_eq(
        "the v2 mark is gone, the label mark stays",
        &selected(&marks(&home)),
        &vec![json!({"label": "9.9"})],
    );

    log.step("the install's undo puts the mark back");
    json(&log, &home, &["undo", &tx]).ok();
    log.assert_eq("both marks again", &selected(&marks(&home)).len(), &2);

    log.step("a lock file naming v2 lifts it the same way");
    home.write_desired(
        "schema_version = \"1.0\"\n\n[[target.\"retail\".addon]]\nref = \"gh:o/foo\"\nversion = \"v2\"\n",
    );
    let ran = json(&log, &home, &["sync", "--locked"]);
    ran.ok();
    log.assert_eq("skip_lifted", &warns(&ran, "skip_lifted"), &true);
    log.assert_eq(
        "installed v2",
        &common::state::record(&home, "retail").addons["foo"].version,
        &"v2".to_owned(),
    );
    log.assert_eq(
        "the label mark stays",
        &selected(&marks(&home)),
        &vec![json!({"label": "9.9"})],
    );
}

/// What happens between a preview and its `apply`.
#[derive(Debug, Clone, Copy)]
enum Between {
    /// Only the memo moves to v3, or back to v2.
    MemoV3,
    MemoV2,
    /// Another change commits on the client's record.
    SkipALabel,
    UnskipAll,
    Pin,
    Nothing,
}

fn between() -> impl Strategy<Value = Between> {
    prop_oneof![
        Just(Between::MemoV3),
        Just(Between::MemoV2),
        Just(Between::SkipALabel),
        Just(Between::UnskipAll),
        Just(Between::Pin),
        Just(Between::Nothing),
    ]
}

/// A case's own log, named by a counter: proptest runs one test body many times.
fn case_log(what: &str) -> TestLog {
    static CASES: AtomicUsize = AtomicUsize::new(0);
    let n = CASES.fetch_add(1, Ordering::Relaxed);
    TestLog::new("skip_command", &format!("{what}_case_{n}"))
}

/// Runs `between` on `home`; returns whether a change committed on the record.
fn intervene(log: &TestLog, home: &Home, steps: &[Between], label: &str) -> bool {
    let mut committed = false;
    for (n, step) in steps.iter().enumerate() {
        match step {
            Between::MemoV3 => memo_offers(home, "v3"),
            Between::MemoV2 => memo_offers(home, "v2"),
            Between::SkipALabel => {
                let label = format!("{label}{n}");
                let ran = json(
                    log,
                    home,
                    &["skip", "foo", "--version", &label, "-t", "retail"],
                );
                ran.ok();
                committed |= !ran.data()["targets"][0]["tx"].is_null();
            }
            Between::UnskipAll => {
                let ran = json(log, home, &["unskip", "foo", "-t", "retail"]);
                ran.ok();
                committed |= !ran.data()["targets"][0]["tx"].is_null();
            }
            Between::Pin => {
                let ran = json(log, home, &["pin", "foo", "-t", "retail"]);
                ran.ok();
                committed |= !ran.data()["targets"][0]["tx"].is_null();
            }
            Between::Nothing => {}
        }
    }
    committed
}

/// Whether the memo offers v2 after `steps` (it starts so).
fn offers_v2(steps: &[Between]) -> bool {
    steps
        .iter()
        .rev()
        .find_map(|s| match s {
            Between::MemoV3 => Some(false),
            Between::MemoV2 => Some(true),
            _ => None,
        })
        .unwrap_or(true)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// A stored `skip` plan does exactly what its preview showed, or answers `plan_stale`
    /// and writes nothing: whatever moves in the memo or commits on the record between.
    #[test]
    fn a_skip_plan_marks_what_it_showed_or_nothing(steps in proptest::collection::vec(between(), 0..4)) {
        let log = case_log("skip");
        log.json("between", &format!("{steps:?}"));
        let home = behind(&log);
        let preview = json(&log, &home, &["skip", "foo", "-t", "retail", "--dry-run"]);
        preview.ok();
        let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
        let committed = intervene(&log, &home, &steps, "l");
        let before = record_bytes(&home);
        let marks_before = marks(&home);
        let applied = json(&log, &home, &["apply", &plan]);
        if offers_v2(&steps) && !committed {
            applied.ok();
            let mut after = selected(&marks(&home));
            let mut expected = selected(&marks_before);
            expected.push(json!({"release": {"source": "gh:o/foo", "tag": "v2"}}));
            after.sort_by_key(Value::to_string);
            expected.sort_by_key(Value::to_string);
            prop_assert_eq!(after, expected);
        } else {
            applied.fails(1, "plan_stale");
            prop_assert_eq!(record_bytes(&home), before);
        }
    }

    /// A stored `unskip --release` plan clears exactly what its preview showed, or answers
    /// `plan_stale` and writes nothing. What it clears does not depend on the memo.
    #[test]
    fn an_unskip_plan_clears_what_it_showed_or_nothing(steps in proptest::collection::vec(between(), 0..4)) {
        let log = case_log("unskip");
        log.json("between", &format!("{steps:?}"));
        let home = behind(&log);
        json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
        let preview = json(&log, &home, &["unskip", "foo", "--release", "gh:o/foo@v2", "-t", "retail", "--dry-run"]);
        preview.ok();
        let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();
        let committed = intervene(&log, &home, &steps, "u");
        let before = record_bytes(&home);
        let applied = json(&log, &home, &["apply", &plan]);
        if committed {
            applied.fails(1, "plan_stale");
            prop_assert_eq!(record_bytes(&home), before);
        } else {
            applied.ok();
            prop_assert!(marks(&home).is_empty());
        }
    }
}
