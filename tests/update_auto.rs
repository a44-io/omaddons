//! e2e suite: an unattended update takes only releases that have settled
//!
//! Slice 3 of the regression guard (RG-55, design 7.6) through the real binary: `update
//! --auto` takes a release only once this machine first saw it at least
//! `update_min_age_hours` ago (default 6 hours), with no exception. The fifteen
//! scenarios carry RG-55's numbers: too new then settled (1), superseded (2), churn (3),
//! the wait off (4), a skipped and a too-new release together (5), the widget's sequence
//! (7), an entry from before first sightings (8), the real carry (9) and merge (10)
//! paths, a look-back's choice (11), a too-new dependency and a shared one (12), new
//! errors that do not waive the wait (13), a stored plan gone stale (14) and the re-tag
//! lifecycle (15). Scenario 6 runs inside 1 to 5, 11 and 12: [`ready`] holds `status`'s
//! `auto_ready` to what `update --auto --dry-run` updates, equal where the offered
//! release is the one the planner takes, higher where planning learns more.
//!
//! Time is placed by the scenario's epoch (`common::epoch`, RG-52b), never by
//! `OMADDONS_NOW`; scenarios 9 and 10 edit no time at all. Every `update --auto` runs
//! through [`auto`], which holds each release it updated or installed to its first sight:
//! at least the wait before the run began. Beyond the fifteen: a channel switch, bytes
//! replaced under a tag, what the read commands show (RG-53), the requests `--auto` sends,
//! and a source check that the deciding modules never name the errors ledger or memo.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use common::binding::age_release_memo;
use common::epoch::Epoch;
use common::graph::{Folder, Node, release_graph};
use common::home::Home;
use common::http::paths;
use common::netlog::net_log;
use common::pause::Pause;
use common::tlog::{Ran, TestLog, tlog};
use common::trees::tree_hash;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

const RETAIL: &str = "120105";
const FOO: &str = "https://github.com/o/foo";
/// The default wait (`DEFAULT_UPDATE_MIN_AGE`).
const WAIT: SignedDuration = SignedDuration::from_hours(6);

fn hours(h: i64) -> SignedDuration {
    SignedDuration::from_hours(h)
}

/// A retail folder requiring `deps`.
const fn folder<'a>(name: &'a str, deps: &'a [&'a str]) -> Folder<'a> {
    Folder {
        name,
        interface: RETAIL,
        deps,
        optional: &[],
    }
}

/// Publishes `gh:o/<repo>` at `tag` in `scenario`: one retail folder `name` requiring
/// `deps`, and the release made latest.
fn publish(scenario: &Path, name: &str, repo: &str, tag: &str, deps: &[&str]) {
    let base = format!("https://github.com/o/{repo}");
    release_graph(
        scenario,
        &[Node {
            base: &base,
            tag,
            folders: &[folder(name, deps)],
        }],
    );
}

/// Writes `catalog.d/50-auto.toml`: one entry per (name, repo under `o/`), its one folder
/// the name, so a TOC's dependency resolves.
fn catalog(home: &Home, entries: &[(&str, &str)]) {
    let mut text = String::from("schema_version = \"1.0\"\n");
    for (name, repo) in entries {
        text.push_str(&format!(
            "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{name:?}]\n"
        ));
    }
    let dir = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("50-auto.toml"), text).unwrap();
}

/// `omaddons <args> --json`.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.push("--json");
    log.run(home, &argv)
}

/// `omaddons <args>` as a person reads it.
fn human(log: &TestLog, home: &Home, args: &[&str]) -> String {
    let ran = log.run(home, args);
    ran.exit(0);
    ran.stdout().to_owned()
}

/// One installed addon of [`behind`]: its name, its repo under `o/`, and what its v2's
/// folder requires.
type Behind<'a> = (&'a str, &'a str, &'a [&'a str]);

/// Retail with each of `addons` installed at v1, then each one's v2 published; `extra`
/// (name, repo) published at v1 and not installed. Fetched once (`outdated --refresh`),
/// so the memo first saw every v2 just now.
fn behind(log: &TestLog, addons: &[Behind<'_>], extra: &[(&str, &str)]) -> Home {
    let home = Home::new(&["wow"]);
    log.env(&home);
    let all: Vec<(&str, &str)> = addons
        .iter()
        .map(|(name, repo, _)| (*name, *repo))
        .chain(extra.iter().copied())
        .collect();
    catalog(&home, &all);
    for (name, repo, _) in addons {
        publish(&home.http, name, repo, "v1", &[]);
        json(
            log,
            &home,
            &["install", &format!("gh:o/{repo}"), "-t", "retail"],
        )
        .ok();
    }
    for (name, repo) in extra {
        publish(&home.http, name, repo, "v1", &[]);
    }
    for (name, repo, requires) in addons {
        publish(&home.http, name, repo, "v2", requires);
    }
    json(log, &home, &["outdated", "--refresh"]).ok();
    home
}

/// `(name, status, skip_reason)` of the first target's items in `data`.
fn items_of(data: &Value) -> Vec<(String, String, String)> {
    data["targets"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["name"].as_str().unwrap_or("-").to_owned(),
                i["status"].as_str().unwrap().to_owned(),
                i["skip_reason"].as_str().unwrap_or("-").to_owned(),
            )
        })
        .collect()
}

fn items(ran: &Ran) -> Vec<(String, String, String)> {
    items_of(ran.data())
}

fn item(name: &str, status: &str, reason: &str) -> (String, String, String) {
    (name.to_owned(), status.to_owned(), reason.to_owned())
}

/// The first target's item named `name`.
fn item_of(ran: &Ran, name: &str) -> Value {
    ran.data()["targets"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no item {name} in {}", ran.data())) // ubs:ignore — the test's own failure
}

fn version(home: &Home, addon: &str) -> Option<String> {
    common::state::record(home, "retail")
        .addons
        .values()
        .find(|a| a.name == addon)
        .map(|a| a.version.clone())
}

/// Every first sight the release memo holds of `r`'s release `tag` (any revision).
fn sights(home: &Home, r: &str, tag: &str) -> Vec<Timestamp> {
    let (memo, _) = omaddons::state::memo::load(&paths(&home.env));
    memo.entries
        .values()
        .filter(|e| e.wanted.r#ref.to_string() == r)
        .flat_map(|e| &e.seen_releases)
        .filter(|s| s.release.tag == tag)
        .map(|s| s.first_seen_at)
        .collect()
}

/// RG-55's guard over every unattended run: each item of `data` that updated or
/// installed a release names one this machine first saw at least `wait` before `began`
/// (when the run began, so before its own `now`); every sighting of that tag counts, so a
/// re-tag seen recently fails it even beside an old revision's.
fn settled(log: &TestLog, home: &Home, data: &Value, began: Timestamp, wait: SignedDuration) {
    for t in data["targets"].as_array().unwrap() {
        for i in t["items"].as_array().unwrap() {
            if !matches!(i["status"].as_str(), Some("updated" | "installed")) {
                continue;
            }
            let (r, tag) = (
                i["ref"].as_str().unwrap(),
                i["to_version"].as_str().unwrap(),
            );
            let seen = sights(home, r, tag);
            log.assert_eq(
                &format!("{r}@{tag}: first seen at least the wait before the run"),
                &(!seen.is_empty() && seen.iter().all(|s| *s <= began - wait)),
                &true,
            );
        }
    }
}

/// `omaddons update --auto <args> --json`, exit 0, held to [`settled`].
fn auto(log: &TestLog, home: &Home, args: &[&str], wait: SignedDuration) -> Ran {
    let began = Timestamp::now();
    let mut argv = vec!["update", "--auto"];
    argv.extend_from_slice(args);
    let ran = json(log, home, &argv);
    ran.ok();
    settled(log, home, ran.data(), began, wait);
    ran
}

/// Scenario 6: `status`'s `summary.updates.auto_ready` and how many items `update --auto
/// --dry-run` would update, in that order.
fn ready(log: &TestLog, home: &Home, wait: SignedDuration) -> (u64, usize) {
    let status = json(log, home, &["status"]);
    status.ok();
    let expected = status.data()["summary"]["updates"]["auto_ready"]
        .as_u64()
        .unwrap();
    let dry = auto(log, home, &["--dry-run"], wait);
    let taken = items(&dry)
        .iter()
        .filter(|(_, status, _)| status == "updated")
        .count();
    (expected, taken)
}

/// Scenario 6 where the offered release is the one taken: `auto_ready` is exactly what
/// the dry run updates.
fn ready_equals(log: &TestLog, home: &Home, wait: SignedDuration, n: usize) {
    let (expected, taken) = ready(log, home, wait);
    log.assert_eq(
        "auto_ready, and what the dry run updates",
        &(expected, taken),
        &(n as u64, n),
    );
}

/// Scenario 6 where planning learns more than the memo: `status` expects more than the
/// dry run takes, never fewer.
fn ready_exceeds(log: &TestLog, home: &Home, wait: SignedDuration) {
    let (expected, taken) = ready(log, home, wait);
    log.assert_eq(
        "auto_ready exceeds what the dry run updates",
        &(expected > taken as u64),
        &true,
    );
}

/// The requests `omaddons update <args> --dry-run` adds on `home`, sorted by path.
fn requests(log: &TestLog, home: &Home, args: &[&str]) -> Vec<String> {
    let before = net_log(home).entries.len();
    let mut argv = vec!["update"];
    argv.extend_from_slice(args);
    argv.push("--dry-run");
    json(log, home, &argv).ok();
    let mut paths: Vec<String> = net_log(home).entries[before..]
        .iter()
        .map(|e| e.path.clone())
        .collect();
    paths.sort();
    paths
}

/// RG-55's request bound: on twin homes from `twin`, `update --auto <args> --dry-run`
/// sends no request that `update <args> --dry-run` does not.
fn no_extra_request(log: &TestLog, twin: impl Fn() -> Home, args: &[&str]) {
    let attended = requests(log, &twin(), args);
    let mut auto_args = vec!["--auto"];
    auto_args.extend_from_slice(args);
    let mut extra = requests(log, &twin(), &auto_args);
    for path in &attended {
        if let Some(i) = extra.iter().position(|p| p == path) {
            extra.remove(i);
        }
    }
    log.assert_eq("requests --auto adds", &extra, &Vec::<String>::new());
}

/// The envelope without what changes between runs; `auto_eligible_at` as `<time>`.
fn stable(envelope: &Value) -> Value {
    let mut text = envelope.to_string();
    if let Some(run) = envelope["data"]["run"].as_str() {
        text = text.replace(run, "<run>");
    }
    for t in envelope["data"]["targets"].as_array().unwrap() {
        for (v, what) in [(&t["target"]["key"], "<key>"), (&t["tx"], "<tx>")] {
            if let Some(v) = v.as_str() {
                text = text.replace(v, what);
            }
        }
        for i in t["items"].as_array().unwrap() {
            if let Some(at) = i["auto_eligible_at"].as_str() {
                text = text.replace(at, "<time>");
            }
        }
    }
    let mut stable: Value = serde_json::from_str(&text).unwrap();
    stable["meta"] = Value::from("<meta>");
    stable["tool_version"] = Value::from("<version>");
    stable
}

/// Runs `runs` with the errors ledger and the errors memo replaced by FIFOs, and returns
/// which of them a process opened: a reader opening one is seen by the non-blocking
/// writer kept trying here, which then lets it go (EOF). Each stays a FIFO.
fn opened_during(home: &Home, runs: impl FnOnce()) -> Vec<String> {
    let p = paths(&home.env);
    let fifos: [PathBuf; 2] = [p.errors_ledger(), p.errors_memo()];
    for fifo in &fifos {
        fs::create_dir_all(fifo.parent().unwrap()).unwrap();
        let _ = fs::remove_file(fifo);
        rustix::fs::mknodat(
            rustix::fs::CWD,
            fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
    }
    let done = AtomicBool::new(false);
    let opened = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            let mut opened = Vec::new();
            while !done.load(Ordering::Acquire) {
                for fifo in &fifos {
                    if let Ok(fd) = rustix::fs::open(
                        fifo,
                        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                        rustix::fs::Mode::empty(),
                    ) {
                        drop(fd);
                        opened.push(fifo.display().to_string());
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            opened
        });
        runs();
        done.store(true, Ordering::Release);
        watcher.join().unwrap()
    });
    for fifo in &fifos {
        let kind = fs::symlink_metadata(fifo).unwrap().file_type();
        assert!(
            std::os::unix::fs::FileTypeExt::is_fifo(&kind),
            "{} was replaced",
            fifo.display()
        );
    }
    opened
}

/// (1) Too new, then settled (design 5.5): first seen an hour ago, Foo waits, says until
/// when, and nothing commits; `--strict` and a named Foo are gated the same; an attended
/// update would take it at once. First seen seven hours ago, it installs.
#[test]
fn scenario_1_too_new_then_settled() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    let epoch = Epoch::ago(hours(1));
    epoch.seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    epoch.assert_consistent(&home);
    let addons = home.addons("_retail_");
    let (before, lines) = (tree_hash(&addons), common::state::history(&home).len());

    log.step("update --auto leaves Foo: too new, eligible six hours after its first sight");
    for args in [&[][..], &["Foo"], &["--strict"]] {
        let ran = auto(&log, &home, args, WAIT);
        log.assert_eq(
            "held",
            &items(&ran),
            &vec![item("Foo", "skipped", "too_new")],
        );
        let foo = item_of(&ran, "Foo");
        let at: Timestamp = foo["auto_eligible_at"].as_str().unwrap().parse().unwrap();
        log.assert_eq("eligible", &at, &epoch.at(WAIT));
        log.assert_eq("to", &foo["to_version"], &json!("v2"));
        log.assert_eq(
            "nothing committed",
            &(&ran.data()["run"], &ran.data()["targets"][0]["tx"]),
            &(&Value::Null, &Value::Null),
        );
    }
    let ran = auto(&log, &home, &[], WAIT);
    insta::assert_json_snapshot!("update_auto_too_new", stable(ran.envelope()));
    log.assert_eq("no folder moved", &tree_hash(&addons), &before);
    log.assert_eq(
        "no transaction",
        &common::state::history(&home).len(),
        &lines,
    );
    let text = human(&log, &home, &["update", "--auto"]);
    log.assert_eq(
        "the human line",
        &text.contains("Foo left at v1 · v2 is too new for an automatic update (after "),
        &true,
    );
    ready_equals(&log, &home, WAIT, 0);

    log.step("an attended preview takes it at once and says nothing of a wait");
    let preview = json(&log, &home, &["update", "--dry-run"]);
    preview.ok();
    log.assert_eq(
        "taken",
        &items(&preview),
        &vec![item("Foo", "updated", "-")],
    );
    log.assert_eq(
        "no auto_eligible_at",
        &item_of(&preview, "Foo").get("auto_eligible_at").is_some(),
        &false,
    );

    log.step("first seen seven hours ago: update --auto installs it");
    Epoch::ago(hours(7)).seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    ready_equals(&log, &home, WAIT, 1);
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq("updated", &items(&ran), &vec![item("Foo", "updated", "-")]);
    log.assert_eq(
        "no auto_eligible_at",
        &item_of(&ran, "Foo").get("auto_eligible_at").is_some(),
        &false,
    );
    log.assert_eq("installed", &version(&home, "Foo"), &Some("v2".to_owned()));
}

/// (2) Superseded: v2 first seen seven hours ago, v3 an hour ago. v3 waits, and v2, which
/// has stood the wait, is never installed in its place; once v3 has stood it, v3
/// installs.
#[test]
fn scenario_2_a_superseded_release_never_installs() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    publish(&home.http, "Foo", "foo", "v3", &[]);
    json(&log, &home, &["outdated", "--refresh"]).ok();
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    epoch.seen(&home, "gh:o/foo", "v3", hours(6));

    log.step("v3 waits; v2 is not taken instead");
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );
    log.assert_eq("v3", &item_of(&ran, "Foo")["to_version"], &json!("v3"));
    log.assert_eq("still v1", &version(&home, "Foo"), &Some("v1".to_owned()));
    ready_equals(&log, &home, WAIT, 0);

    log.step("v3 settled: v3 installs");
    epoch.seen(&home, "gh:o/foo", "v3", SignedDuration::ZERO);
    ready_equals(&log, &home, WAIT, 1);
    auto(&log, &home, &[], WAIT);
    log.assert_eq("v3", &version(&home, "Foo"), &Some("v3".to_owned()));
}

/// (3) Churn never bypasses the wait (the removed burst bypass): eight releases, each
/// first seen seconds after the last and the newest a moment ago, wait; the same a day
/// apart, the newest a moment ago, wait; the newest seen seven hours ago is taken.
#[test]
fn scenario_3_churn_never_bypasses_the_wait() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    let tags: Vec<String> = (2..=9).map(|n| format!("v{n}")).collect();
    for tag in &tags[1..] {
        publish(&home.http, "Foo", "foo", tag, &[]);
    }
    json(&log, &home, &["outdated", "--refresh"]).ok();

    log.step("eight releases a second apart, the newest a moment ago: v9 waits");
    let epoch = Epoch::ago(SignedDuration::from_mins(1));
    for (i, tag) in tags.iter().enumerate() {
        epoch.seen(&home, "gh:o/foo", tag, SignedDuration::from_secs(i as i64));
    }
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );
    log.assert_eq("v9", &item_of(&ran, "Foo")["to_version"], &json!("v9"));

    log.step("the same a day apart, the newest a moment ago: v9 waits");
    let epoch = Epoch::ago(hours(7 * 24) + SignedDuration::from_mins(1));
    for (i, tag) in tags.iter().enumerate() {
        epoch.seen(&home, "gh:o/foo", tag, hours(24 * i as i64));
    }
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );
    log.assert_eq("still v1", &version(&home, "Foo"), &Some("v1".to_owned()));

    log.step("the newest seen seven hours ago: v9 installs");
    Epoch::ago(hours(7)).seen(&home, "gh:o/foo", "v9", SignedDuration::ZERO);
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq("updated", &items(&ran), &vec![item("Foo", "updated", "-")]);
    log.assert_eq("v9", &version(&home, "Foo"), &Some("v9".to_owned()));
}

/// (4) The wait off (`update_min_age_hours = 0`): a release first seen a moment ago is
/// taken.
#[test]
fn scenario_4_the_wait_off_takes_a_release_at_once() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    json(&log, &home, &["config", "set", "update_min_age_hours", "0"]).ok();
    ready_equals(&log, &home, SignedDuration::ZERO, 1);
    let ran = auto(&log, &home, &[], SignedDuration::ZERO);
    log.assert_eq("updated", &items(&ran), &vec![item("Foo", "updated", "-")]);
    log.assert_eq("v2", &version(&home, "Foo"), &Some("v2".to_owned()));
}

/// (5) A skipped release and a too-new one on one client: each says its own reason.
#[test]
fn scenario_5_a_skipped_and_a_too_new_release_on_one_client() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[]), ("Bar", "bar", &[])], &[]);
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", hours(6));
    epoch.seen(&home, "gh:o/bar", "v2", SignedDuration::ZERO);
    json(&log, &home, &["skip", "bar", "-t", "retail"]).ok();
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "reasons",
        &items(&ran),
        &vec![
            item("Bar", "skipped", "skipped_release"),
            item("Foo", "skipped", "too_new"),
        ],
    );
    ready_equals(&log, &home, WAIT, 0);
}

/// (7) The widget's sequence: `list -v` (which says what waits), `--dry-run update
/// --auto`, then `apply` of the plan it stored, which runs as `--auto`.
#[test]
fn scenario_7_the_widget_s_sequence() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[]), ("Bar", "bar", &[])], &[]);
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", hours(6));
    epoch.seen(&home, "gh:o/bar", "v2", SignedDuration::ZERO);

    log.step("list -v");
    let list = json(&log, &home, &["list", "-v"]);
    list.ok();
    let wait_of = |name: &str| -> Value {
        list.data()["targets"][0]["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .map(|a| a["latest"]["auto_eligible_at"].clone())
            .unwrap()
    };
    log.assert_eq("Bar has settled", &wait_of("Bar"), &Value::Null);
    let at: Timestamp = wait_of("Foo").as_str().unwrap().parse().unwrap();
    log.assert_eq("Foo waits", &at, &epoch.at(hours(12)));

    log.step("--dry-run update --auto");
    let began = Timestamp::now();
    let preview = json(&log, &home, &["--dry-run", "update", "--auto"]);
    preview.ok();
    settled(&log, &home, preview.data(), began, WAIT);
    log.assert_eq(
        "preview",
        &items(&preview),
        &vec![
            item("Bar", "updated", "-"),
            item("Foo", "skipped", "too_new"),
        ],
    );
    let plan = preview.data()["plan_id"].as_str().unwrap().to_owned();

    log.step("apply");
    let began = Timestamp::now();
    let applied = json(&log, &home, &["apply", &plan]);
    applied.ok();
    let result = &applied.data()["result"];
    settled(&log, &home, result, began, WAIT);
    log.assert_eq(
        "applied as --auto",
        &items_of(result),
        &vec![
            item("Bar", "updated", "-"),
            item("Foo", "skipped", "too_new"),
        ],
    );
    log.assert_eq(
        "versions",
        &(version(&home, "Foo"), version(&home, "Bar")),
        &(Some("v1".to_owned()), Some("v2".to_owned())),
    );
}

/// (8) An entry from before first sightings (release memo 1.1, the helper's `unseen`):
/// the first `outdated` that asks the source seeds it; a second, an hour later by the
/// helper, keeps that first sight.
#[test]
fn scenario_8_an_entry_from_before_first_sightings_is_seeded_once() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    Epoch::ago(hours(1)).unseen(&home, "gh:o/foo");
    log.assert_eq("unseen", &sights(&home, "gh:o/foo", "v2").len(), &0);

    log.step("the first check seeds the newest release's first sight");
    let before = Timestamp::now();
    json(&log, &home, &["outdated", "--refresh"]).ok();
    let seeded = sights(&home, "gh:o/foo", "v2");
    log.assert_eq(
        "seeded now",
        &(seeded.len() == 1 && seeded[0] >= before),
        &true,
    );

    log.step("an hour later, a second check keeps it");
    age_release_memo(&home, hours(1));
    json(&log, &home, &["outdated"]).ok();
    log.assert_eq(
        "kept",
        &sights(&home, "gh:o/foo", "v2"),
        &vec![seeded[0] - hours(1)],
    );
}

/// (9) The real carry path: two checks that each ask the source (`--refresh`), no time
/// edited between them: the release keeps the first check's first sight.
#[test]
fn scenario_9_the_real_carry_path_keeps_the_first_sight() {
    let log = tlog!();
    let home = behind(&log, &[("Foo", "foo", &[])], &[]);
    let first = sights(&home, "gh:o/foo", "v2");
    log.assert_eq("one sight", &first.len(), &1);
    let asked = net_log(&home).entries.len();
    json(&log, &home, &["outdated", "--refresh"]).ok();
    log.assert_eq(
        "asked again",
        &(net_log(&home).entries.len() > asked),
        &true,
    );
    log.assert_eq("carried", &sights(&home, "gh:o/foo", "v2"), &first);
}

/// (10) The real merge path: two checks fetch at once and each stops just before its
/// memo write (`memo.write.before`), then they write in either order. The earlier
/// sight survives both: a later writer merges with the file the other wrote.
#[test]
fn scenario_10_the_real_merge_path_keeps_the_earlier_sight() {
    let log = tlog!();
    for earlier_writes_last in [true, false] {
        log.step(if earlier_writes_last {
            "the earlier fetch writes last"
        } else {
            "the earlier fetch writes first"
        });
        let home = Home::new(&["wow"]);
        log.env(&home);
        publish(&home.http, "Foo", "foo", "v1", &[]);
        json(&log, &home, &["install", "gh:o/foo", "-t", "retail"]).ok();
        publish(&home.http, "Foo", "foo", "v2", &[]);
        let point = "memo.write.before";
        let start = |pause: &Pause| {
            log.cmd(&home, &["outdated", "--refresh", "--json"])
                .failpoints(&format!("{point}=pause"))
                .pause(pause)
                .spawn()
        };
        let (a, b) = (Pause::new(home.env.root()), Pause::new(home.env.root()));
        let earlier = start(&a);
        log.reached(&a, point);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let later = start(&b);
        log.reached(&b, point);
        let (first, second) = if earlier_writes_last {
            ((&b, later), (&a, earlier))
        } else {
            ((&a, earlier), (&b, later))
        };
        log.resume(first.0, point);
        first.1.wait().ok();
        let written = sights(&home, "gh:o/foo", "v2");
        log.resume(second.0, point);
        second.1.wait().ok();
        let merged = sights(&home, "gh:o/foo", "v2");
        log.assert_eq("one sight", &merged.len(), &1);
        if earlier_writes_last {
            log.assert_eq(
                "earlier than the later fetch's",
                &(merged[0] < written[0]),
                &true,
            );
        } else {
            log.assert_eq("the first writer's kept", &merged, &written);
        }
    }
}

/// (11) A look-back's choice waits though the prediction saw only the newest release, a
/// day old: that release has no build for the client, and the planner's look-back finds
/// one first seen when it ran, just now. `status` expects one update; the dry run takes
/// none.
#[test]
fn scenario_11_a_look_back_choice_waits() {
    use crate::install_look_back::support::{MISTS, REF, Rel, TITAN, serve};
    const OLD: Rel = ("v3.215-mists", "mists", 50_504);
    let log = tlog!();
    let scene = || {
        let mut home = Home::new(&["wow_classic"]);
        log.env(&home);
        serve(&home.http, &[OLD], &[]);
        json(&log, &home, &["install", REF, "-t", "classic"]).ok();
        let later = home.scenario("later");
        serve(&later, &[TITAN, MISTS, OLD], &[]);
        let outdated = json(&log, &home, &["outdated", "-t", "classic", "--refresh"]);
        outdated.ok();
        log.assert_eq(
            "the status knows only the newest",
            &outdated.data()["targets"][0]["items"][0]["latest"]["version"],
            &json!(TITAN.0),
        );
        Epoch::ago(hours(24)).seen(&home, REF, TITAN.0, SignedDuration::ZERO);
        home
    };
    no_extra_request(&log, scene, &["-t", "classic"]);

    let home = scene();
    // Before any planning: once a plan has looked back, the memo remembers the choice,
    // and `status` then knows it too.
    let feeds = |home: &Home| net_log(home).count("github.com", "releases-atom");
    let before = feeds(&home);
    ready_exceeds(&log, &home, WAIT);
    log.assert_eq(
        "the dry run's planner looked back",
        &(feeds(&home) - before),
        &1,
    );
    let ran = auto(&log, &home, &["-t", "classic"], WAIT);
    let vuhdo = &ran.data()["targets"][0]["items"][0];
    log.assert_eq(
        "held",
        &(
            &vuhdo["status"],
            &vuhdo["skip_reason"],
            &vuhdo["to_version"],
        ),
        &(&json!("skipped"), &json!("too_new"), &json!(MISTS.0)),
    );
    let at: Timestamp = vuhdo["auto_eligible_at"].as_str().unwrap().parse().unwrap();
    let from_now = at.duration_since(Timestamp::now());
    log.assert_eq(
        "six hours after the look-back",
        &(from_now > hours(5) && from_now <= WAIT),
        &true,
    );
    let record = common::state::record(&home, "classic");
    log.assert_eq(
        "still the old release",
        &record.addons.values().next().unwrap().version,
        &OLD.0.to_owned(),
    );
}

/// (12) A too-new dependency, and a shared one: two updates that newly need BigWigs,
/// first seen when the planner fetched it, both wait (`dependency_held`) while an unrelated
/// update in the same run installs; once BigWigs has stood the wait, all of it installs.
#[test]
fn scenario_12_a_too_new_dependency_holds_every_requester() {
    let log = tlog!();
    let scene = || {
        let home = behind(
            &log,
            &[
                ("LittleWigs", "littlewigs", &["BigWigs"]),
                ("Bars", "bars", &["BigWigs"]),
                ("Solo", "solo", &[]),
            ],
            &[("BigWigs", "bigwigs")],
        );
        let epoch = Epoch::ago(hours(7));
        for repo in ["littlewigs", "bars", "solo"] {
            epoch.seen(&home, &format!("gh:o/{repo}"), "v2", SignedDuration::ZERO);
        }
        home
    };
    no_extra_request(&log, scene, &[]);

    let home = scene();
    ready_exceeds(&log, &home, WAIT);
    log.step("update --auto: both requesters wait, Solo updates");
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "items",
        &items(&ran),
        &vec![
            item("Bars", "skipped", "dependency_held"),
            item("LittleWigs", "skipped", "dependency_held"),
            item("Solo", "updated", "-"),
        ],
    );
    for name in ["Bars", "LittleWigs"] {
        let held = item_of(&ran, name);
        log.assert_eq("to v2", &held["to_version"], &json!("v2"));
        log.assert_eq(
            "no auto_eligible_at",
            &held.get("auto_eligible_at").is_some(),
            &false,
        );
    }
    insta::assert_json_snapshot!("update_auto_dependency_held", stable(ran.envelope()));
    log.assert_eq("the dependency waits", &version(&home, "BigWigs"), &None);
    let text = human(&log, &home, &["update", "--auto"]);
    log.assert_eq(
        "the human line",
        &text.contains(
            "LittleWigs left at v1 · it needs BigWigs v1, which is too new for an automatic update",
        ),
        &true,
    );

    log.step("BigWigs settled: the requesters update and it installs with them");
    Epoch::ago(hours(7)).seen(&home, "gh:o/bigwigs", "v1", SignedDuration::ZERO);
    let ran = auto(&log, &home, &[], WAIT);
    log.assert_eq(
        "items",
        &items(&ran),
        &vec![
            item("Bars", "updated", "-"),
            item("BigWigs", "installed", "-"),
            item("LittleWigs", "updated", "-"),
        ],
    );
}

/// (13) New errors do not waive the wait (the removed waiver): Foo updated, then an error
/// in its first session, so `errors` reports it new with a `high` link to that update;
/// Foo's newest release, first seen a moment ago, waits. `update --auto` opens neither
/// the errors memo nor the ledger.
#[test]
fn scenario_13_new_errors_do_not_waive_the_wait() {
    use crate::errors_after_change::{addon, errors, foo_installed, regression};
    let log = tlog!();
    let s = foo_installed();
    log.env(&s.home);
    regression(&s);
    let (envelope, _) = errors(&s, &[]);
    let foo = addon(&envelope, "Foo");
    log.json("Foo in errors", &foo);
    log.assert_eq(
        "a high link naming Foo",
        &(
            &foo["after_change"]["confidence"],
            &foo["after_change"]["addon"],
        ),
        &(&json!("high"), &json!("Foo")),
    );

    log.step("Foo v3, first seen now");
    s.release("foo", "v3", &[common::engine::folder("Foo", &[])]);
    json(&log, &s.home, &["outdated", "-t", "retail", "--refresh"]).ok();
    let ran = auto(&log, &s.home, &["-t", "retail"], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );

    log.step("update --auto opens neither the errors memo nor the ledger");
    let opened = opened_during(&s.home, || {
        let ran = auto(&log, &s.home, &["-t", "retail"], WAIT);
        log.assert_eq(
            "held",
            &items(&ran),
            &vec![item("Foo", "skipped", "too_new")],
        );
        auto(&log, &s.home, &["-t", "retail", "--dry-run"], WAIT);
    });
    log.assert_eq("opened", &opened, &Vec::<String>::new());
}

/// (14) A stored `update --auto` plan applies as `--auto`, and is `plan_stale` when,
/// between the preview and `apply`, Foo gains a skip mark, `update_min_age_hours`
/// changes, or Foo's time comes.
#[test]
fn scenario_14_a_stored_auto_plan_goes_stale_when_its_world_moves() {
    let log = tlog!();
    let scene = || {
        let home = behind(&log, &[("Foo", "foo", &[]), ("Bar", "bar", &[])], &[]);
        let epoch = Epoch::ago(hours(30));
        epoch.seen(&home, "gh:o/foo", "v2", hours(29));
        epoch.seen(&home, "gh:o/bar", "v2", SignedDuration::ZERO);
        home
    };
    let previewed = |home: &Home| -> String {
        let ran = auto(&log, home, &["--dry-run"], WAIT);
        log.assert_eq(
            "preview",
            &items(&ran),
            &vec![
                item("Bar", "updated", "-"),
                item("Foo", "skipped", "too_new"),
            ],
        );
        ran.data()["plan_id"].as_str().unwrap().to_owned()
    };
    let stale = |home: &Home, plan: &str| {
        json(&log, home, &["apply", plan]).fails(1, "plan_stale");
        log.assert_eq(
            "nothing applied",
            &version(home, "Bar"),
            &Some("v1".to_owned()),
        );
    };

    log.step("applied as --auto");
    let home = scene();
    let plan = previewed(&home);
    let applied = json(&log, &home, &["apply", &plan]);
    applied.ok();
    log.assert_eq(
        "applied",
        &items_of(&applied.data()["result"]),
        &vec![
            item("Bar", "updated", "-"),
            item("Foo", "skipped", "too_new"),
        ],
    );

    log.step("a skip mark on Foo in between");
    let home = scene();
    let plan = previewed(&home);
    json(&log, &home, &["skip", "foo", "-t", "retail"]).ok();
    stale(&home, &plan);

    log.step("another wait in between, which changes nothing else");
    let home = scene();
    let plan = previewed(&home);
    json(
        &log,
        &home,
        &["config", "set", "update_min_age_hours", "12"],
    )
    .ok();
    stale(&home, &plan);

    log.step("Foo's time comes in between");
    let home = scene();
    let plan = previewed(&home);
    Epoch::ago(hours(30)).seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    stale(&home, &plan);
}

/// (15) The re-tag lifecycle (RG-35's WowAce tag, `v12.1.1` at revisions 1390 and 1402):
/// the re-tag is first seen when a check finds it, whatever the age of the revision
/// installed, and has its own age: `update --auto` holds it, and takes it once it has
/// settled. `tests/skip.rs` runs the rest of the lifecycle on the same tag.
#[test]
fn scenario_15_a_re_tag_has_its_own_age() {
    use common::wowace::{serve_recorded, tag_at};
    const R: &str = "wowace:bug-grabber";
    let log = tlog!();
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    serve_recorded(&home);
    tag_at(&home.http, 1390);
    json(&log, &home, &["install", R, "-t", "retail"]).ok();
    age_release_memo(&home, hours(24));
    let revision = |home: &Home| {
        common::state::record(home, "retail")
            .addons
            .values()
            .next()
            .unwrap()
            .revision
    };
    log.assert_eq("1390", &revision(&home), &Some(1390));

    log.step("v12.1.1 re-tagged at revision 1402, found by a check");
    let dir = home.scenario("revision-1402");
    serve_recorded(&home);
    tag_at(&dir, 1402);
    let before = Timestamp::now();
    json(&log, &home, &["outdated", "-t", "retail"]).ok();
    let (memo, _) = omaddons::state::memo::load(&paths(&home.env));
    let sight = |rev: u32| {
        memo.entries
            .values()
            .flat_map(|e| &e.seen_releases)
            .filter(|s| s.release.revision == Some(rev))
            .map(|s| s.first_seen_at)
            .min()
    };
    log.assert_eq(
        "1390 first seen a day ago, 1402 now",
        &(
            sight(1390).is_some_and(|t| t < before - hours(23)),
            sight(1402).is_some_and(|t| t >= before),
        ),
        &(true, true),
    );

    log.step("update --auto holds the re-tag");
    let ran = auto(&log, &home, &["-t", "retail"], WAIT);
    log.assert_eq(
        "held",
        &ran.data()["targets"][0]["items"][0]["skip_reason"],
        &json!("too_new"),
    );
    log.assert_eq("still 1390", &revision(&home), &Some(1390));

    log.step("seven hours on, it takes it");
    Epoch::ago(hours(7)).seen_revision(&home, R, "v12.1.1", Some(1402), SignedDuration::ZERO);
    let ran = auto(&log, &home, &["-t", "retail"], WAIT);
    log.assert_eq(
        "updated",
        &ran.data()["targets"][0]["items"][0]["status"],
        &json!("updated"),
    );
    log.assert_eq("1402", &revision(&home), &Some(1402));
}

/// Beyond the fifteen: a channel switch under `--auto` is gated. The beta build the
/// planner fetches now is too new, and the addon keeps its channel.
#[test]
fn a_channel_switch_under_auto_is_gated() {
    let log = tlog!();
    let mut home = Home::new(&["wow"]);
    log.env(&home);
    publish(&home.http, "Foo", "foo", "v1", &[]);
    json(&log, &home, &["install", "gh:o/foo", "-t", "retail"]).ok();
    let scenario = home.scenario("beta");
    publish(&scenario, "Foo", "foo", "v2-beta", &[]);
    publish(&scenario, "Foo", "foo", "v1", &[]);
    let list = json!([
        {"tag_name": "v2-beta", "prerelease": true, "draft": false,
         "published_at": "2026-10-15T00:00:00Z",
         "assets": [{"name": "foo.zip",
                     "browser_download_url": format!("{FOO}/releases/download/v2-beta/foo.zip")}]},
        {"tag_name": "v1", "prerelease": false, "draft": false,
         "published_at": "2026-10-01T00:00:00Z", "assets": []}
    ])
    .to_string();
    common::http::fixture(
        &scenario,
        "api-releases",
        "https://api.github.com/repos/o/foo/releases?per_page=20",
        200,
        &[("content-type", "application/json")],
        Some(&list),
    );
    let ran = auto(&log, &home, &["foo", "--channel", "beta"], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );
    log.assert_eq(
        "v2-beta",
        &item_of(&ran, "Foo")["to_version"],
        &json!("v2-beta"),
    );
    let record = common::state::record(&home, "retail");
    let foo = &record.addons["foo"];
    log.assert_eq(
        "the channel and release kept",
        &(foo.channel, foo.version.as_str()),
        &(omaddons::model::Channel::Stable, "v1"),
    );
}

/// Beyond the fifteen: bytes replaced under a tag first seen seven hours ago are new
/// content, and wait again from when they were seen.
#[test]
fn replaced_bytes_under_a_seven_hour_old_tag_wait_again() {
    let log = tlog!();
    let mut home = behind(&log, &[("Foo", "foo", &[])], &[]);
    // A preview downloads v2, so the memo knows its bytes.
    json(&log, &home, &["update", "--dry-run"]).ok();
    Epoch::ago(hours(7)).seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    let rebuilt = home.next_scenario();
    let toc = common::graph::folder_toc(&folder("Foo", &[]), "v2");
    let assets: [common::http::Asset<'_>; 1] = [(
        "foo.zip",
        &[],
        vec![
            ("Foo/Foo.toc", toc.as_str()),
            ("Foo/rebuilt.lua", "-- rebuilt"),
        ],
    )];
    common::http::gh_release(&rebuilt, FOO, "v2", &assets);
    let blobs = home.env.cache().join("omaddons/blobs");
    if blobs.exists() {
        fs::remove_dir_all(&blobs).unwrap();
    }
    let ran = auto(&log, &home, &["--refresh"], WAIT);
    log.assert_eq(
        "held",
        &items(&ran),
        &vec![item("Foo", "skipped", "too_new")],
    );
    log.assert_eq("still v1", &version(&home, "Foo"), &Some("v1".to_owned()));
}

/// Beyond the fifteen (RG-53): the read commands show what waits. One settled and one
/// too-new update, and a skipped release: `outdated` and `list` give the too-new one
/// `auto_eligible_at` and the others `null`; `status` counts `available: 2`, `too_new:
/// 1`, `auto_ready: 1` (the skipped release in none of them), `--cached` the same with no
/// request; with no wait, nothing is too new. The human lines are design 5.5's.
#[test]
fn the_read_commands_show_what_waits_and_status_predicts_what_is_ready() {
    let log = tlog!();
    let home = behind(
        &log,
        &[
            ("Foo", "foo", &[]),
            ("Bar", "bar", &[]),
            ("Baz", "baz", &[]),
        ],
        &[],
    );
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", hours(6));
    epoch.seen(&home, "gh:o/bar", "v2", SignedDuration::ZERO);
    epoch.seen(&home, "gh:o/baz", "v2", SignedDuration::ZERO);
    json(&log, &home, &["skip", "baz", "-t", "retail"]).ok();

    let outdated = json(&log, &home, &["outdated", "-v"]);
    outdated.ok();
    let wait_of = |ran: &Ran, name: &str| item_of(ran, name)["auto_eligible_at"].clone();
    let at: Timestamp = wait_of(&outdated, "Foo").as_str().unwrap().parse().unwrap();
    log.assert_eq("first sight plus six hours", &at, &epoch.at(hours(12)));
    log.assert_eq("settled", &wait_of(&outdated, "Bar"), &Value::Null);
    log.assert_eq("skipped", &wait_of(&outdated, "Baz"), &Value::Null);

    let list = json(&log, &home, &["list", "-v"]);
    list.ok();
    let latest = |name: &str| -> Value {
        list.data()["targets"][0]["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .map(|a| a["latest"]["auto_eligible_at"].clone())
            .unwrap()
    };
    log.assert_eq("list agrees", &latest("Foo"), &wait_of(&outdated, "Foo"));
    log.assert_eq(
        "and on the others",
        &(latest("Bar"), latest("Baz")),
        &(Value::Null, Value::Null),
    );

    let counts = |ran: &Ran| {
        let t = &ran.data()["targets"][0]["updates"];
        let s = &ran.data()["summary"]["updates"];
        (
            t["available"].clone(),
            t["too_new"].clone(),
            t["skipped"].clone(),
            s["available"].clone(),
            s["auto_ready"].clone(),
        )
    };
    let status = json(&log, &home, &["status"]);
    status.ok();
    log.assert_eq(
        "status",
        &counts(&status),
        &(json!(2), json!(1), json!(1), json!(2), json!(1)),
    );
    let asked = net_log(&home).entries.len();
    let cached = json(&log, &home, &["status", "--cached"]);
    cached.ok();
    log.assert_eq("--cached agrees", &counts(&cached), &counts(&status));
    log.assert_eq(
        "--cached asks nothing",
        &net_log(&home).entries.len(),
        &asked,
    );

    let text = human(&log, &home, &["outdated"]);
    let line = |name: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap()
            .to_owned()
    };
    log.assert_eq(
        "Foo's note",
        &(line("Foo").contains("· first seen ")
            && line("Foo").contains(" · automatic updates take it after ")),
        &true,
    );
    log.assert_eq("Bar's has none", &line("Bar").contains("automatic"), &false);
    let text = human(&log, &home, &["status"]);
    log.assert_eq(
        "the client line",
        &text.contains("2 updates (1 waits to settle)"),
        &true,
    );

    json(&log, &home, &["config", "set", "update_min_age_hours", "0"]).ok();
    let status = json(&log, &home, &["status"]);
    status.ok();
    log.assert_eq(
        "no wait",
        &counts(&status),
        &(json!(2), json!(0), json!(1), json!(2), json!(2)),
    );
}

/// Beyond the fifteen: a run the prediction holds whole and a preview the planner holds
/// decide without opening the errors ledger or memo (a run that changes a client's code
/// keeps the ledger's light read around the change, design 7.1.8, whose result reaches
/// only the read after it).
#[test]
fn deciding_opens_neither_the_errors_ledger_nor_the_errors_memo() {
    let log = tlog!();
    let home = behind(
        &log,
        &[
            ("Foo", "foo", &[]),
            ("LittleWigs", "littlewigs", &["BigWigs"]),
        ],
        &[("BigWigs", "bigwigs")],
    );
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", hours(6));
    epoch.seen(&home, "gh:o/littlewigs", "v2", SignedDuration::ZERO);
    let opened = opened_during(&home, || {
        let held = auto(&log, &home, &["Foo"], WAIT);
        log.assert_eq(
            "held",
            &items(&held),
            &vec![item("Foo", "skipped", "too_new")],
        );
        let preview = auto(&log, &home, &["--dry-run"], WAIT);
        log.assert_eq(
            "preview",
            &items(&preview),
            &vec![
                item("Foo", "skipped", "too_new"),
                item("LittleWigs", "skipped", "dependency_held"),
            ],
        );
        json(&log, &home, &["update", "--dry-run"]).ok();
    });
    log.assert_eq("opened", &opened, &Vec::<String>::new());
}

/// Beyond the fifteen: what decides an automatic update, the gate and its prediction in
/// `update` and the planner's hold, never names the errors ledger or the errors memo.
#[test]
fn the_deciding_modules_never_name_the_errors_ledger_or_memo() {
    let log = tlog!();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "src/app/update.rs",
        "src/engine/plan.rs",
        "src/resolve/updates.rs",
    ] {
        let text = fs::read_to_string(root.join(file)).unwrap();
        for word in [
            "errors_ledger",
            "errors::ledger",
            "errors_memo",
            "ErrorsLedger",
            "ErrorsMemo",
        ] {
            log.assert_eq(
                &format!("{file} names {word}"),
                &text.contains(word),
                &false,
            );
        }
    }
}
