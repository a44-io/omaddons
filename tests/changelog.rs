//! `info --changelog` and `outdated --changelog` end to end: GitHub notes from
//! `releases.atom` (one request, never REST) newer than the installed tag, cut where the
//! feed ends; one changelog per ref for `outdated`, sliced per item and attached to
//! updates only; WoWInterface notes with no request, offline too; Tukui's one
//! changelog request; commit titles for git on GitHub; the latest release's notes for
//! an identified adoption; and the reasons when there is none. Request counts come from
//! `net.log`.

use crate::common;

use common::archives::zip_text;
use common::home::Home;
use common::http::{Resp, fixture_seq, gh_feed};
use common::netlog::{assert_no_github_rest, net_log};
use common::tukui::{elvui, tukui_changelog, tukui_list};
use common::wowi::{details, wowi_details, wowi_file};
use serde_json::{Value, json};
use support::{
    BAZ, FOO, WA, age_memo, changelog_versions, fetch_progress, from_changelog, item, publish,
    stable,
};

#[cfg(test)]
mod support {
    use std::path::Path;

    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::{Asset, gh_latest, gh_release_v, paths};
    use super::common::netlog::NetLogView;

    pub const WA: &str = "https://github.com/WeakAuras/WeakAuras2";
    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAZ: &str = "https://github.com/o/baz";

    const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];

    /// Release `tag` of the repository at `base` as its latest: one asset `<name>.zip`
    /// with `<name>/<name>.toc` for retail and era.
    pub fn publish(scenario: &Path, base: &str, name: &str, tag: &str) {
        let toc = toc(
            "120105, 11509",
            &[&format!("## Title: {name}"), &format!("## Version: {tag}")],
        );
        let core = format!("-- {name} {tag}");
        let (file, toc_path, core_path) = (
            format!("{name}.zip"),
            format!("{name}/{name}.toc"),
            format!("{name}/core.lua"),
        );
        let assets: [Asset<'_>; 1] = [(
            file.as_str(),
            FLAVORS,
            vec![
                (toc_path.as_str(), toc.as_str()),
                (core_path.as_str(), core.as_str()),
            ],
        )];
        gh_release_v(scenario, base, tag, &assets);
        gh_latest(scenario, base, tag);
    }

    /// Ages every remembered check by a day, so the next run asks the source again.
    pub fn age_memo(home: &Home) {
        let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
        for entry in memo.entries.values_mut() {
            entry.fetched_at -= jiff::SignedDuration::from_hours(24);
        }
        home.seed_memo(&memo);
    }

    /// The versions of a `changelog` object.
    pub fn changelog_versions(changelog: &Value) -> Vec<String> {
        changelog["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["version"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The `outdated` item `name` on `target`.
    pub fn item<'a>(envelope: &'a Value, target: &str, name: &str) -> &'a Value {
        let found = envelope["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["target"]["id"] == target)
            .and_then(|t| {
                t["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["name"] == name)
            });
        assert!(found.is_some(), "no {name} on {target}: {envelope}");
        found.unwrap()
    }

    /// `(current, total)` of each `fetch` progress event in `stderr`.
    pub fn fetch_progress(stderr: &str) -> Vec<(u64, u64)> {
        stderr
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["event"] == "progress" && e["phase"] == "fetch")
            .map(|e| (e["current"].as_u64().unwrap(), e["total"].as_u64().unwrap()))
            .collect()
    }

    /// The human output from its `changelog:` line on.
    pub fn from_changelog(stdout: &str) -> String {
        stdout
            .lines()
            .skip_while(|l| l.trim() != "changelog:")
            .map(|l| format!("{l}\n"))
            .collect()
    }

    /// The envelope without what changes between runs.
    pub fn stable(envelope: &Value) -> Value {
        let mut text = envelope.to_string();
        if let Some(targets) = envelope["data"]["targets"].as_array() {
            for t in targets {
                if let Some(key) = t["target"]["key"].as_str() {
                    let id = t["target"]["id"].as_str().unwrap();
                    text = text.replace(key, &format!("<key:{id}>"));
                }
            }
        }
        let mut stable: Value = serde_json::from_str(&text).unwrap();
        if !stable["data"]["checked_at"].is_null() {
            stable["data"]["checked_at"] = Value::from("<checked_at>");
        }
        stable["meta"] = Value::from("<meta>");
        stable["tool_version"] = Value::from("<version>");
        stable
    }

    /// The new `net.log` lines with `purpose`.
    pub fn new_with_purpose(log: &NetLogView, before: usize, purpose: &str) -> usize {
        log.entries
            .iter()
            .skip(before)
            .filter(|e| e.purpose == purpose)
            .count()
    }
}

#[test]
fn info_shows_the_github_notes_newer_than_the_installed_tag_with_no_rest() {
    let home = Home::new(&["wow"]);
    publish(&home.http, WA, "WeakAuras", "5.19.1");
    home.run(&["install", "gh:WeakAuras/WeakAuras2", "-t", "retail"])
        .expect(0);
    publish(&home.http, WA, "WeakAuras", "5.19.3");
    gh_feed(
        &home.http,
        WA,
        &[
            (
                "5.19.3",
                "<h2>Fixes</h2><ul><li>Faster <b>loading</b></li><li>Fewer errors</li></ul>",
            ),
            ("5.19.2", "<p>Mists support &amp; more</p>"),
            ("5.19.1", "<p>Old news</p>"),
            ("5.19.0", "<p>Older news</p>"),
        ],
        None,
    );
    age_memo(&home);

    // Without the flag: no changelog keys and no feed request.
    let before = net_log(&home).len();
    let plain = home.run(&["info", "weakauras"]).expect(0);
    assert!(plain["data"].get("changelog").is_none(), "{plain}");
    assert!(plain["data"].get("changelog_unavailable").is_none());
    let log = net_log(&home);
    assert_eq!(support::new_with_purpose(&log, before, "releases-atom"), 0);

    let before = net_log(&home).len();
    let r = home.run(&["info", "weakauras", "--changelog"]);
    let envelope = r.expect(0);
    let changelog = &envelope["data"]["changelog"];
    assert_eq!(
        changelog_versions(changelog),
        ["5.19.3", "5.19.2"],
        "{}",
        r.context()
    );
    assert_eq!(changelog["source"], "github");
    assert_eq!(changelog["truncated"], false);
    assert_eq!(changelog["more_url"], format!("{WA}/releases"));
    let text = changelog["entries"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Faster loading") && text.contains("Fewer errors") && !text.contains('<'),
        "{text}"
    );
    assert_eq!(changelog["entries"][1]["text"], "Mists support & more");
    assert!(envelope["data"].get("changelog_unavailable").is_none());
    assert_eq!(envelope["partial"], false);
    // The keys come last, after `offline`.
    let wire = r.stdout.find("\"offline\"").unwrap();
    assert!(wire < r.stdout.find("\"changelog\"").unwrap());

    let log = net_log(&home);
    assert_eq!(
        support::new_with_purpose(&log, before, "releases-atom"),
        1,
        "{}",
        log.context()
    );
    assert_no_github_rest(&log.entries);
    insta::assert_json_snapshot!("info_changelog", stable(&envelope)["data"]["changelog"]);

    let human = home.exec(&["info".into(), "weakauras".into(), "--changelog".into()]);
    assert_eq!(human.code, 0, "{}", human.context());
    insta::assert_snapshot!("info_changelog_human", from_changelog(&human.stdout));
}

#[test]
fn an_installed_tag_older_than_the_feed_is_cut_with_the_releases_page() {
    let home = Home::new(&["wow"]);
    publish(&home.http, FOO, "Foo", "v1");
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    publish(&home.http, FOO, "Foo", "v4");
    // The feed's history ends at v3: v2 and v1's successor are not in it.
    gh_feed(
        &home.http,
        FOO,
        &[("v4", "<p>four</p>"), ("v3", "<p>three</p>")],
        None,
    );
    age_memo(&home);
    let r = home.run(&["info", "gh:o/foo", "--changelog"]);
    let envelope = r.expect(0);
    let changelog = &envelope["data"]["changelog"];
    assert_eq!(
        changelog_versions(changelog),
        ["v4", "v3"],
        "{}",
        r.context()
    );
    assert_eq!(changelog["truncated"], true);
    assert_eq!(changelog["more_url"], format!("{FOO}/releases"));
    let human = home.exec(&["info".into(), "gh:o/foo".into(), "--changelog".into()]);
    assert!(
        human
            .stdout
            .contains("    older releases on GitHub: https://github.com/o/foo/releases\n"),
        "{}",
        human.context()
    );
}

#[test]
fn outdated_attaches_one_changelog_per_ref_to_updates_only() {
    let home = Home::new(&["wow", "wow_classic_era"]);
    publish(&home.http, FOO, "Foo", "v1");
    publish(&home.http, BAZ, "Baz", "v1");
    for target in ["retail", "era"] {
        home.run(&["install", "gh:o/baz", "-t", target]).expect(0);
    }
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    publish(&home.http, FOO, "Foo", "v2");
    age_memo(&home);
    home.run(&["install", "gh:o/foo", "-t", "era"]).expect(0);
    publish(&home.http, FOO, "Foo", "v3");
    gh_feed(
        &home.http,
        FOO,
        &[
            ("v3", "<p>Third: <b>faster</b></p>"),
            ("v2", "<ul><li>Second</li></ul>"),
            ("v1", "<p>First</p>"),
        ],
        None,
    );
    age_memo(&home);

    let before = net_log(&home).len();
    let run = home.run(&["outdated", "--changelog", "--progress", "json"]);
    let envelope = run.expect(0);
    // Retail has v1, era v2: one changelog from v1, each item its own slice.
    let retail = item(&envelope, "retail", "Foo");
    let era = item(&envelope, "era", "Foo");
    assert_eq!(
        changelog_versions(&retail["latest"]["changelog"]),
        ["v3", "v2"],
        "{}",
        run.context()
    );
    assert_eq!(changelog_versions(&era["latest"]["changelog"]), ["v3"]);
    assert_eq!(
        era["latest"]["changelog"]["entries"][0]["text"],
        "Third: faster"
    );
    assert_eq!(envelope["partial"], false);
    let log = net_log(&home);
    assert_eq!(
        support::new_with_purpose(&log, before, "releases-atom"),
        1,
        "{}",
        log.context()
    );
    assert_no_github_rest(&log.entries);
    // Baz's probe confirmed it (no fetch event); Foo's changelog is part of its fetch:
    // one event, not two.
    assert_eq!(fetch_progress(&run.stderr), [(1, 1)], "{}", run.context());
    insta::assert_json_snapshot!("outdated_changelog", stable(&envelope));

    // Current items get nothing, even listed.
    let all = home.run(&["outdated", "--changelog", "-v"]).expect(0);
    let baz = item(&all, "retail", "Baz");
    assert_eq!(baz["status"], "current");
    assert!(baz["latest"].get("changelog").is_none(), "{baz}");
    assert!(item(&all, "retail", "Foo")["latest"]["changelog"].is_object());

    // Without the flag: no changelog keys and no feed request.
    let before = net_log(&home).len();
    let plain = home.run(&["outdated"]).expect(0);
    assert!(
        item(&plain, "retail", "Foo")["latest"]
            .get("changelog")
            .is_none()
    );
    let log = net_log(&home);
    assert_eq!(support::new_with_purpose(&log, before, "releases-atom"), 0);

    let human = home.exec(&["outdated".into(), "--changelog".into()]);
    assert_eq!(human.code, 0, "{}", human.context());
    insta::assert_snapshot!("outdated_changelog_human", human.stdout);
}

#[test]
fn wowinterface_notes_need_no_request_offline_too() {
    let mut home = Home::new(&["wow"]);
    let bugsack = |scenario: &std::path::Path, version: &str| {
        let toc = common::addons::toc(
            "120105",
            &["## Title: BugSack", &format!("## Version: {version}")],
        );
        let core = format!("-- BugSack {version}");
        let zip = zip_text(&[("BugSack/BugSack.toc", &toc), ("BugSack/core.lua", &core)]);
        let md5 = wowi_file(scenario, 5995, &zip);
        wowi_details(
            scenario,
            &[5995],
            &[details(5995, "BugSack", version, &md5)],
        );
    };
    bugsack(&home.http, "1.0");
    home.run(&["install", "wowi:5995", "-t", "retail"])
        .expect(0);
    let newer = home.scenario("newer");
    bugsack(&newer, "1.1");
    age_memo(&home);

    let before = net_log(&home).len();
    let plain = home.run(&["outdated"]).expect(0);
    let plain_requests = net_log(&home).len() - before;
    assert_eq!(
        item(&plain, "retail", "BugSack")["status"],
        "update_available"
    );
    age_memo(&home);
    let before = net_log(&home).len();
    let r = home.run(&["outdated", "--changelog"]);
    let envelope = r.expect(0);
    let changelog = &item(&envelope, "retail", "BugSack")["latest"]["changelog"];
    assert_eq!(changelog_versions(changelog), ["1.1"], "{}", r.context());
    assert_eq!(changelog["source"], "wowi");
    let text = changelog["entries"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("1.1: fixed things") && !text.contains("[b]"),
        "{text}"
    );
    assert_eq!(
        net_log(&home).len() - before,
        plain_requests,
        "the notes cost nothing"
    );

    // Offline, from the memo, with no request at all.
    let before = net_log(&home).len();
    let r = home.run(&["info", "wowi:5995", "--changelog", "--offline"]);
    let offline = r.expect(0);
    assert_eq!(
        changelog_versions(&offline["data"]["changelog"]),
        ["1.1"],
        "{}",
        r.context()
    );
    assert_eq!(net_log(&home).len(), before);
}

#[test]
fn tukui_makes_one_changelog_request() {
    let home = Home::new(&["wow"]);
    tukui_list(&home.http, &[elvui()]);
    tukui_changelog(
        &home.http,
        "elvui",
        "<h3>16.00</h3><ul><li>Fixed the <strong>bags</strong></li></ul>",
    );
    let before = net_log(&home).len();
    let r = home.run(&["info", "tukui:elvui", "--changelog"]);
    let envelope = r.expect(0);
    let changelog = &envelope["data"]["changelog"];
    assert_eq!(changelog_versions(changelog), ["16.00"], "{}", r.context());
    assert_eq!(changelog["source"], "tukui");
    let text = changelog["entries"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Fixed the bags") && !text.contains('<'),
        "{text}"
    );
    let log = net_log(&home);
    assert_eq!(
        support::new_with_purpose(&log, before, "tukui-changelog"),
        1,
        "{}",
        log.context()
    );
}

#[test]
fn git_on_github_lists_commit_titles_since_the_installed_commit() {
    let home = Home::new(&["wow"]);
    let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    let feed = |commits: &[(&str, &str)]| {
        let entries: String = commits
            .iter()
            .enumerate()
            .map(|(i, (sha, title))| {
                format!(
                    "<entry><id>tag:github.com,2008:Grit::Commit/{sha}</id>\
                     <updated>2026-10-{:02}T12:00:00Z</updated><title>{title}</title></entry>",
                    20 - i
                )
            })
            .collect();
        format!("<?xml version=\"1.0\"?><feed>{entries}</feed>")
    };
    let serve = |commits: &[(&str, &str)]| {
        fixture_seq(
            &home.http,
            "commits-main",
            "https://github.com/o/r/commits/main.atom",
            &[Resp::ok(feed(commits))],
        );
        let head = commits[0].0;
        let toc = common::addons::toc("120105", &["## Title: R"]);
        let zip = zip_text(&[(&format!("r-{head}/R/R.toc"), &toc)]);
        fixture_seq(
            &home.http,
            &format!("zip-{head}"),
            &format!("https://codeload.github.com/o/r/zip/{head}"),
            &[Resp::ok(zip)],
        );
    };
    serve(&[(&a, "First")]);
    let git = "git:https://github.com/o/r#main";
    home.run(&["install", git, "-t", "retail"]).expect(0);
    serve(&[(&c, "Third &amp; last"), (&b, "Second"), (&a, "First")]);
    age_memo(&home);
    let r = home.run(&["outdated", "--changelog"]);
    let envelope = r.expect(0);
    let changelog = &item(&envelope, "retail", "R")["latest"]["changelog"];
    assert_eq!(
        changelog_versions(changelog),
        ["ccccccc", "bbbbbbb"],
        "{}",
        r.context()
    );
    assert_eq!(changelog["source"], "git");
    assert_eq!(changelog["entries"][0]["text"], "Third & last");
    assert_eq!(changelog["truncated"], false);
    assert_no_github_rest(&net_log(&home).entries);
}

#[test]
fn an_identified_adoption_gets_the_latest_notes_only() {
    let home = Home::new(&["wow"]);
    common::addons::unpack(
        &home.addons("_retail_"),
        &[(
            "Foo/Foo.toc",
            &common::addons::toc("120105", &["## Title: Foo", "## Version: local"]),
        )],
    );
    publish(&home.http, FOO, "Foo", "v2");
    gh_feed(
        &home.http,
        FOO,
        &[("v2", "<p>two</p>"), ("v1", "<p>one</p>")],
        None,
    );
    let r = home.run(&["adopt", "Foo", "--ref", "gh:o/foo", "--no-verify"]);
    r.expect(0);
    let record = common::state::record(&home, "retail");
    assert_eq!(record.addons["foo"].release_id, None, "{}", r.context());
    let r = home.run(&["info", "gh:o/foo", "--changelog"]);
    let envelope = r.expect(0);
    assert_eq!(
        changelog_versions(&envelope["data"]["changelog"]),
        ["v2"],
        "{}",
        r.context()
    );
    let r = home.run(&["outdated", "--changelog"]);
    let envelope = r.expect(0);
    let foo = item(&envelope, "retail", "Foo");
    assert_eq!(foo["status"], "update_available", "{foo}");
    assert_eq!(
        changelog_versions(&foo["latest"]["changelog"]),
        ["v2"],
        "{foo}"
    );
}

#[test]
fn no_changelog_says_why_and_never_makes_the_run_partial() {
    let home = Home::new(&["wow"]);
    // A local archive has none.
    let toc = common::addons::toc("120105", &["## Title: Loc"]);
    let zip = home.env.root().join("Loc.zip");
    std::fs::write(&zip, zip_text(&[("Loc/Loc.toc", &toc)])).unwrap();
    let file = format!("file:{}", zip.display());
    let r = home.run(&["info", &file, "--changelog"]);
    let local = r.expect(0);
    assert_eq!(
        (
            &local["data"]["changelog"],
            &local["data"]["changelog_unavailable"]
        ),
        (&Value::Null, &json!("not_supported")),
        "{}",
        r.context()
    );

    // GitHub offline: nothing asked.
    publish(&home.http, FOO, "Foo", "v1");
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    publish(&home.http, FOO, "Foo", "v2");
    age_memo(&home);
    home.run(&["outdated"]).expect(0);
    let r = home.run(&["info", "gh:o/foo", "--changelog", "--offline"]);
    let offline = r.expect(0);
    assert_eq!(
        offline["data"]["changelog_unavailable"],
        "offline",
        "{}",
        r.context()
    );

    // A failing feed: a reason, not a warning, and the run is whole.
    fixture_seq(
        &home.http,
        "feed-broken",
        &format!("{FOO}/releases.atom"),
        &[Resp::status(500)],
    );
    let r = home.run(&["outdated", "--changelog"]);
    let failed = r.expect(0);
    let latest = &item(&failed, "retail", "Foo")["latest"];
    assert_eq!(
        (&latest["changelog"], &latest["changelog_unavailable"]),
        (&Value::Null, &json!("fetch_failed")),
        "{}",
        r.context()
    );
    assert_eq!(failed["partial"], false);
    assert_eq!(failed["warnings"], json!([]));
    let human = home.exec(&["outdated".into(), "--changelog".into()]);
    assert!(
        human
            .stdout
            .contains("      no changelog (it could not be fetched)\n"),
        "{}",
        human.context()
    );
}
