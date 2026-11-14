//! `omaddons update --auto` end to end (RG-52d, design 7.6): an unattended update takes
//! only releases this machine first saw at least `update_min_age_hours` ago. Each case
//! fetches first (`outdated --refresh`), so the release memo holds the sightings, then
//! places them with the scenario's epoch (RG-52b) and runs `update --auto`: the first
//! choice, a new dependency and a shared one, bytes replaced under a tag, a look-back's
//! choice, a stored plan and its staleness, a channel switch, a named addon, `--strict`,
//! the requests sent, and the errors ledger and memo it never opens.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use common::epoch::Epoch;
use common::graph::{Folder, Node, release_graph};
use common::home::Home;
use common::netlog::net_log;
use common::trees::tree_hash;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

const RETAIL: &str = "120105";
const FOO: &str = "https://github.com/o/foo";

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

/// One installed addon of [`behind`]: its name, its repo under `o/`, and what its v2's
/// folder requires.
type Behind<'a> = (&'a str, &'a str, &'a [&'a str]);

/// Retail with each of `addons` installed at v1, then each one's v2 published; `extra`
/// (name, repo) published at v1 and not installed. Fetched once, so the memo first saw
/// every v2 just now.
fn behind(addons: &[Behind<'_>], extra: &[(&str, &str)]) -> Home {
    let home = Home::new(&["wow"]);
    let all: Vec<(&str, &str)> = addons
        .iter()
        .map(|(name, repo, _)| (*name, *repo))
        .chain(extra.iter().copied())
        .collect();
    catalog(&home, &all);
    for (name, repo, _) in addons {
        let base = format!("https://github.com/o/{repo}");
        release_graph(
            &home.http,
            &[Node {
                base: &base,
                tag: "v1",
                folders: &[folder(name, &[])],
            }],
        );
        home.run(&["install", &format!("gh:o/{repo}"), "-t", "retail"])
            .expect(0);
    }
    for (name, repo) in extra {
        let base = format!("https://github.com/o/{repo}");
        release_graph(
            &home.http,
            &[Node {
                base: &base,
                tag: "v1",
                folders: &[folder(name, &[])],
            }],
        );
    }
    for (name, repo, requires) in addons {
        let base = format!("https://github.com/o/{repo}");
        release_graph(
            &home.http,
            &[Node {
                base: &base,
                tag: "v2",
                folders: &[folder(name, requires)],
            }],
        );
    }
    home.run(&["outdated", "--refresh"]).expect(0);
    home
}

/// `(name, status, skip_reason)` of retail's items.
fn items(envelope: &Value) -> Vec<(String, String, String)> {
    envelope["data"]["targets"][0]["items"]
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

fn item(name: &str, status: &str, reason: &str) -> (String, String, String) {
    (name.to_owned(), status.to_owned(), reason.to_owned())
}

/// Retail's item named `name`.
fn item_of<'a>(envelope: &'a Value, name: &str) -> &'a Value {
    envelope["data"]["targets"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == name)
        .unwrap_or_else(|| panic!("no item {name} in {envelope}")) // ubs:ignore — the test's own failure
}

fn version(home: &Home, addon: &str) -> Option<String> {
    common::state::record(home, "retail")
        .addons
        .values()
        .find(|a| a.name == addon)
        .map(|a| a.version.clone())
}

/// `omaddons <args>` as a person reads it.
fn human(home: &Home, args: &[&str]) -> String {
    let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let r = home.exec(&argv);
    assert_eq!(r.code, 0, "{}", r.context());
    r.stdout
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

/// The first choice: first seen an hour ago, it waits, says until when, and nothing
/// commits; first seen seven hours ago, it installs. `--strict` passes a wait as it passes
/// a pin, and naming the addon is gated the same.
#[test]
fn a_release_seen_an_hour_ago_waits_and_one_seen_seven_hours_ago_installs() {
    let home = behind(&[("Foo", "foo", &[])], &[]);
    let epoch = Epoch::ago(hours(1));
    epoch.seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    epoch.assert_consistent(&home);
    let addons = home.addons("_retail_");
    let (before, lines) = (tree_hash(&addons), common::state::history(&home).len());

    for argv in [
        &["update", "--auto"][..],
        &["update", "--auto", "Foo"],
        &["update", "--auto", "--strict"],
    ] {
        let r = home.run(argv);
        let envelope = r.expect(0);
        assert_eq!(
            items(&envelope),
            [item("Foo", "skipped", "too_new")],
            "{}",
            r.context()
        );
        let foo = item_of(&envelope, "Foo");
        assert_eq!(foo["to_version"], "v2");
        let at: Timestamp = foo["auto_eligible_at"].as_str().unwrap().parse().unwrap();
        assert_eq!(at, epoch.at(hours(6)), "first sight plus the default wait");
        assert_eq!(
            (
                &envelope["data"]["run"],
                &envelope["data"]["targets"][0]["tx"]
            ),
            (&Value::Null, &Value::Null),
            "nothing committed"
        );
    }
    insta::assert_json_snapshot!(
        "update_auto_too_new",
        stable(&home.run(&["update", "--auto"]).expect(0))
    );
    assert_eq!(tree_hash(&addons), before, "no folder moved");
    assert_eq!(common::state::history(&home).len(), lines, "no transaction");
    assert_eq!(version(&home, "Foo").as_deref(), Some("v1"));
    let text = human(&home, &["update", "--auto"]);
    assert!(
        text.contains("Foo left at v1 · v2 is too new for an automatic update (after "),
        "{text}"
    );

    // An attended preview takes it at once, and says nothing of a wait.
    let preview = home.run(&["update", "--dry-run"]).expect(0);
    assert_eq!(items(&preview), [item("Foo", "updated", "-")]);
    assert!(item_of(&preview, "Foo").get("auto_eligible_at").is_none());

    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    let r = home.run(&["update", "--auto"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [item("Foo", "updated", "-")],
        "{}",
        r.context()
    );
    assert!(item_of(&envelope, "Foo").get("auto_eligible_at").is_none());
    assert_eq!(version(&home, "Foo").as_deref(), Some("v2"));
}

/// RG-53: the read commands show what waits. One settled and one too-new update, and a
/// skipped release: `outdated` and `list` give the too-new one `auto_eligible_at` and the
/// others `null`; `status` counts `available: 2`, `too_new: 1`, `auto_ready: 1` (the
/// skipped release in none of them), `--cached` the same with no request; with no wait,
/// nothing is too new. The human lines are design 5.5's.
#[test]
fn the_read_commands_show_what_waits_and_status_predicts_what_is_ready() {
    let home = behind(
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
    home.run(&["skip", "baz", "-t", "retail"]).expect(0);
    let eligible = epoch.at(hours(12));

    let outdated = home.run(&["outdated", "-v"]).expect(0);
    let wait_of = |envelope: &Value, name: &str| -> Value {
        envelope["data"]["targets"][0]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .map(|i| i["auto_eligible_at"].clone())
            .unwrap()
    };
    let at: Timestamp = wait_of(&outdated, "Foo").as_str().unwrap().parse().unwrap();
    assert_eq!(at, eligible, "first sight plus six hours");
    assert_eq!(wait_of(&outdated, "Bar"), Value::Null, "settled");
    assert_eq!(wait_of(&outdated, "Baz"), Value::Null, "skipped");

    let list = home.run(&["list", "-v"]).expect(0);
    let latest = |name: &str| -> Value {
        list["data"]["targets"][0]["addons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .map(|a| a["latest"]["auto_eligible_at"].clone())
            .unwrap()
    };
    assert_eq!(latest("Foo"), wait_of(&outdated, "Foo"));
    assert_eq!((latest("Bar"), latest("Baz")), (Value::Null, Value::Null));

    let counts = |envelope: &Value| {
        let t = &envelope["data"]["targets"][0]["updates"];
        let s = &envelope["data"]["summary"]["updates"];
        (
            t["available"].clone(),
            t["too_new"].clone(),
            t["skipped"].clone(),
            s["available"].clone(),
            s["auto_ready"].clone(),
        )
    };
    let status = home.run(&["status"]).expect(0);
    assert_eq!(
        counts(&status),
        (json!(2), json!(1), json!(1), json!(2), json!(1))
    );
    let requests = net_log(&home).entries.len();
    let cached = home.run(&["status", "--cached"]).expect(0);
    assert_eq!(counts(&cached), counts(&status));
    assert_eq!(
        net_log(&home).entries.len(),
        requests,
        "--cached asks nothing"
    );

    let text = human(&home, &["outdated"]);
    let foo = text
        .lines()
        .find(|l| l.trim_start().starts_with("Foo"))
        .unwrap();
    assert!(
        foo.contains("· first seen ") && foo.contains(" · automatic updates take it after "),
        "{text}"
    );
    let bar = text
        .lines()
        .find(|l| l.trim_start().starts_with("Bar"))
        .unwrap();
    assert!(!bar.contains("automatic"), "{text}");
    let text = human(&home, &["status"]);
    assert!(text.contains("2 updates (1 waits to settle)"), "{text}");

    home.run(&["config", "set", "update_min_age_hours", "0"])
        .expect(0);
    let status = home.run(&["status"]).expect(0);
    assert_eq!(
        counts(&status),
        (json!(2), json!(0), json!(1), json!(2), json!(2))
    );
    let outdated = home.run(&["outdated"]).expect(0);
    assert_eq!(wait_of(&outdated, "Foo"), Value::Null, "no wait");
}

/// Without `--auto` a release first seen an hour ago installs at once.
#[test]
fn without_auto_a_release_seen_an_hour_ago_installs_at_once() {
    let home = behind(&[("Foo", "foo", &[])], &[]);
    Epoch::ago(hours(1)).seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    let envelope = home.run(&["update"]).expect(0);
    assert_eq!(items(&envelope), [item("Foo", "updated", "-")]);
    assert!(item_of(&envelope, "Foo").get("auto_eligible_at").is_none());
    assert_eq!(version(&home, "Foo").as_deref(), Some("v2"));
}

/// A new dependency the planner fetches now is too new: every addon whose new release
/// needs it waits (`dependency_held`, naming it), and nothing installs with them, while
/// an unrelated update in the same run installs; once the dependency has stood the wait,
/// the requesters update and it installs with them.
#[test]
fn a_new_dependency_too_new_holds_every_requester_until_it_has_stood_the_wait() {
    let home = behind(
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

    let r = home.run(&["update", "--auto"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            item("Bars", "skipped", "dependency_held"),
            item("LittleWigs", "skipped", "dependency_held"),
            item("Solo", "updated", "-"),
        ],
        "{}",
        r.context()
    );
    assert_eq!(
        version(&home, "Solo").as_deref(),
        Some("v2"),
        "the unrelated update"
    );
    for name in ["Bars", "LittleWigs"] {
        let held = item_of(&envelope, name);
        assert_eq!(held["to_version"], "v2");
        assert!(held.get("auto_eligible_at").is_none(), "{held}");
    }
    insta::assert_json_snapshot!("update_auto_dependency_held", stable(&envelope));
    assert_eq!(version(&home, "BigWigs"), None, "the dependency waits too");
    assert_eq!(version(&home, "LittleWigs").as_deref(), Some("v1"));
    let text = human(&home, &["update", "--auto"]);
    assert!(
        text.contains(
            "LittleWigs left at v1 · it needs BigWigs v1, which is too new for an automatic update"
        ),
        "{text}"
    );

    // The planner's fetch of BigWigs is remembered: seven hours on, all of it updates.
    epoch.seen(&home, "gh:o/bigwigs", "v1", SignedDuration::ZERO);
    let r = home.run(&["update", "--auto"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [
            item("Bars", "updated", "-"),
            item("BigWigs", "installed", "-"),
            item("LittleWigs", "updated", "-"),
        ],
        "{}",
        r.context()
    );
}

/// `--auto` sends no request the same step without it would not: a plain preview and an
/// automatic one of twin homes, the dependency held in the second.
#[test]
fn auto_sends_no_request_a_plain_update_would_not() {
    let twin = || {
        let home = behind(
            &[("LittleWigs", "littlewigs", &["BigWigs"])],
            &[("BigWigs", "bigwigs")],
        );
        Epoch::ago(hours(7)).seen(&home, "gh:o/littlewigs", "v2", SignedDuration::ZERO);
        home
    };
    let requests = |home: &Home, argv: &[&str]| {
        let before = net_log(home).entries.len();
        home.run(argv).expect(0);
        let mut paths: Vec<String> = net_log(home).entries[before..]
            .iter()
            .map(|e| e.path.clone())
            .collect();
        paths.sort();
        paths
    };
    let (plain, auto) = (twin(), twin());
    let attended = requests(&plain, &["update", "--dry-run"]);
    let unattended = requests(&auto, &["update", "--auto", "--dry-run"]);
    let mut extra = unattended.clone();
    for path in &attended {
        if let Some(i) = extra.iter().position(|p| p == path) {
            extra.remove(i);
        }
    }
    assert!(extra.is_empty(), "--auto added {extra:?} to {attended:?}");
}

/// Bytes replaced under a tag first seen seven hours ago are new content: the release
/// waits again from when they were seen.
#[test]
fn replaced_bytes_under_a_seven_hour_old_tag_wait_again() {
    let mut home = behind(&[("Foo", "foo", &[])], &[]);
    // A preview downloads v2, so the memo knows its bytes.
    home.run(&["update", "--dry-run"]).expect(0);
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
    let r = home.run(&["update", "--auto", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [item("Foo", "skipped", "too_new")],
        "{}",
        r.context()
    );
    assert_eq!(version(&home, "Foo").as_deref(), Some("v1"));
}

/// A stored `update --auto` plan applies as `--auto` (Foo still waits, Bar updates), and
/// is `plan_stale` when, between the preview and `apply`, Foo gains a skip mark,
/// `update_min_age_hours` changes, or Foo's time comes.
#[test]
fn a_stored_auto_plan_applies_as_auto_and_goes_stale_when_its_world_moves() {
    let previewed = |home: &Home| -> String {
        let r = home.run(&["update", "--auto", "--dry-run"]);
        let envelope = r.expect(0);
        assert_eq!(
            items(&envelope),
            [
                item("Bar", "updated", "-"),
                item("Foo", "skipped", "too_new")
            ],
            "{}",
            r.context()
        );
        envelope["data"]["plan_id"].as_str().unwrap().to_owned()
    };
    let scene = || {
        let home = behind(&[("Foo", "foo", &[]), ("Bar", "bar", &[])], &[]);
        let epoch = Epoch::ago(hours(30));
        epoch.seen(&home, "gh:o/foo", "v2", hours(29));
        epoch.seen(&home, "gh:o/bar", "v2", SignedDuration::ZERO);
        home
    };

    let home = scene();
    let plan = previewed(&home);
    let r = home.run(&["apply", &plan]);
    let envelope = r.expect(0);
    let result = &envelope["data"]["result"];
    assert_eq!(
        items(&json!({"data": result})),
        [
            item("Bar", "updated", "-"),
            item("Foo", "skipped", "too_new")
        ],
        "{}",
        r.context()
    );
    assert_eq!(
        (
            version(&home, "Foo").as_deref(),
            version(&home, "Bar").as_deref()
        ),
        (Some("v1"), Some("v2"))
    );

    let stale = |home: &Home, plan: &str| {
        let r = home.run(&["apply", plan]);
        let envelope = r.envelope();
        assert_eq!(envelope["error"]["code"], "plan_stale", "{}", r.context());
        assert_eq!(
            version(home, "Bar").as_deref(),
            Some("v1"),
            "nothing applied"
        );
    };
    // A skip mark on Foo.
    let home = scene();
    let plan = previewed(&home);
    home.run(&["skip", "foo", "-t", "retail"]).expect(0);
    stale(&home, &plan);
    // Another wait that changes nothing else: Foo still waits, Bar still updates.
    let home = scene();
    let plan = previewed(&home);
    home.run(&["config", "set", "update_min_age_hours", "12"])
        .expect(0);
    stale(&home, &plan);
    // Foo's time comes.
    let home = scene();
    let plan = previewed(&home);
    Epoch::ago(hours(30)).seen(&home, "gh:o/foo", "v2", SignedDuration::ZERO);
    stale(&home, &plan);
}

/// A look-back's choice is held though the prediction saw only the newest release, a day
/// old: that release has no build for the client, and the release the planner's
/// look-back finds there was first seen when the look-back ran, just now.
#[test]
fn a_look_back_found_now_waits_though_the_newest_is_a_day_old() {
    use crate::install_look_back::support::{MISTS, REF, Rel, TITAN, serve};
    const OLD: Rel = ("v3.215-mists", "mists", 50_504);
    let mut home = Home::new(&["wow_classic"]);
    serve(&home.http, &[OLD], &[]);
    home.run(&["install", REF, "-t", "classic"]).expect(0);
    let later = home.scenario("later");
    serve(&later, &[TITAN, MISTS, OLD], &[]);
    let outdated = home
        .run(&["outdated", "-t", "classic", "--refresh"])
        .expect(0);
    let latest = &outdated["data"]["targets"][0]["items"][0]["latest"];
    assert_eq!(
        latest["version"], TITAN.0,
        "the status knows only the newest"
    );
    Epoch::ago(hours(24)).seen(&home, REF, TITAN.0, SignedDuration::ZERO);

    let feeds = |home: &Home| net_log(home).count("github.com", "releases-atom");
    let before = feeds(&home);
    let r = home.run(&["update", "-t", "classic", "--auto"]);
    let envelope = r.expect(0);
    assert_eq!(
        feeds(&home) - before,
        1,
        "the planner looked back: {}",
        r.context()
    );
    let vuhdo = &envelope["data"]["targets"][0]["items"][0];
    assert_eq!(
        (
            &vuhdo["status"],
            &vuhdo["skip_reason"],
            &vuhdo["to_version"]
        ),
        (&json!("skipped"), &json!("too_new"), &json!(MISTS.0)),
        "{}",
        r.context()
    );
    let at: Timestamp = vuhdo["auto_eligible_at"].as_str().unwrap().parse().unwrap();
    let from_now = at.duration_since(Timestamp::now());
    assert!(
        from_now > hours(5) && from_now <= hours(6),
        "eligible six hours after the look-back: {at}"
    );
    let record = common::state::record(&home, "classic");
    assert_eq!(record.addons.values().next().unwrap().version, OLD.0);
}

/// A channel switch under `--auto` is gated: the beta build the planner fetches now is
/// too new, and the addon keeps its channel.
#[test]
fn a_channel_switch_under_auto_is_gated() {
    let mut home = Home::new(&["wow"]);
    release_graph(
        &home.http,
        &[Node {
            base: FOO,
            tag: "v1",
            folders: &[folder("Foo", &[])],
        }],
    );
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let scenario = home.scenario("beta");
    release_graph(
        &scenario,
        &[
            Node {
                base: FOO,
                tag: "v2-beta",
                folders: &[folder("Foo", &[])],
            },
            Node {
                base: FOO,
                tag: "v1",
                folders: &[folder("Foo", &[])],
            },
        ],
    );
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
    let r = home.run(&["update", "foo", "--channel", "beta", "--auto"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        [item("Foo", "skipped", "too_new")],
        "{}",
        r.context()
    );
    assert_eq!(item_of(&envelope, "Foo")["to_version"], "v2-beta");
    let record = common::state::record(&home, "retail");
    let foo = &record.addons["foo"];
    assert_eq!(
        (foo.channel, foo.version.as_str()),
        (omaddons::model::Channel::Stable, "v1")
    );
}

/// Nothing in the errors ledger or the errors memo can waive the wait: `update` decides
/// without opening either. Each is a FIFO here, and a reader opening one would be seen
/// by the non-blocking writer this test keeps trying, which then lets the reader go.
/// Covered: a run the prediction holds whole, and previews, which decide exactly as
/// `apply` and a real run do (a run that changes a client's code keeps the ledger's light
/// read around the change, design 7.1.8, whose result reaches only the read after it;
/// [`the_deciding_modules_never_name_the_errors_ledger_or_memo`] keeps it out of the
/// decision).
#[test]
fn update_decides_without_opening_the_errors_ledger_or_the_errors_memo() {
    let home = behind(
        &[
            ("Foo", "foo", &[]),
            ("LittleWigs", "littlewigs", &["BigWigs"]),
        ],
        &[("BigWigs", "bigwigs")],
    );
    let epoch = Epoch::ago(hours(7));
    epoch.seen(&home, "gh:o/foo", "v2", hours(6));
    epoch.seen(&home, "gh:o/littlewigs", "v2", SignedDuration::ZERO);
    let paths = common::http::paths(&home.env);
    let fifos = [paths.errors_ledger(), paths.errors_memo()];
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
    let opened: Vec<String> = std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            let mut opened = Vec::new();
            while !done.load(Ordering::Acquire) {
                for fifo in &fifos {
                    if let Ok(fd) = rustix::fs::open(
                        fifo,
                        rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                        rustix::fs::Mode::empty(),
                    ) {
                        // A reader has it open: closing the writer gives it EOF.
                        drop(fd);
                        opened.push(fifo.display().to_string());
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            opened
        });
        // Foo waits by the prediction; LittleWigs's new dependency waits in the planner.
        let held = home.run(&["update", "--auto", "Foo"]).expect(0);
        assert_eq!(items(&held), [item("Foo", "skipped", "too_new")]);
        let preview = home.run(&["update", "--auto", "--dry-run"]).expect(0);
        assert_eq!(
            items(&preview),
            [
                item("Foo", "skipped", "too_new"),
                item("LittleWigs", "skipped", "dependency_held"),
            ]
        );
        home.run(&["update", "--dry-run"]).expect(0);
        done.store(true, Ordering::Release);
        watcher.join().unwrap()
    });
    assert!(opened.is_empty(), "opened: {opened:?}");
    for fifo in &fifos {
        let kind = fs::symlink_metadata(fifo).unwrap().file_type();
        assert!(
            std::os::unix::fs::FileTypeExt::is_fifo(&kind),
            "{} was replaced",
            fifo.display()
        );
    }
}

/// What decides an automatic update, the gate and its prediction in `update` and the
/// planner's hold, never names the errors ledger or the errors memo, so neither can
/// reach the decision.
#[test]
fn the_deciding_modules_never_name_the_errors_ledger_or_memo() {
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
            assert!(!text.contains(word), "{file} names {word}");
        }
    }
}
