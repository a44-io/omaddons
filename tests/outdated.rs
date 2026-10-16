//! `omaddons outdated` (alias `check`) end to end: statuses across three clients from one
//! fetch per distinct ref, the free re-check inside the window, `--check`, `--offline`,
//! static installs, remembered and fresh failures, `--verify`, and that it changes
//! nothing but the memo, the blob cache and `net.log`. Request counts come from
//! `net.log`, never from timing.

mod common;

use std::fs;

use common::archives::zip_text;
use common::home::Home;
use common::http::{fixture, gh_release};
use common::netlog::{NetLogView, assert_no_github_rest, net_log};
use serde_json::{Value, json};
use support::{
    ALL, BAR, BAZ, EVERY, FOO, MAINLINE, QUX, Rel, items, only_new, stable, state_files,
    three_clients,
};

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::Home;
    use super::common::http::{Asset, gh_latest, gh_release, gh_release_v};
    use super::common::netlog::NetLogView;

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";
    pub const BAZ: &str = "https://github.com/o/baz";
    pub const QUX: &str = "https://github.com/o/qux";
    pub const PIN: &str = "https://github.com/o/pin";

    /// Every client of [`three_clients`]: era, classic (Mists) and retail.
    pub const EVERY: &str = "11509, 50504, 120105";
    pub const ALL: &[(&str, u32)] = &[
        ("mainline", 120_105),
        ("classic", 11_509),
        ("mists", 50_504),
    ];
    pub const MAINLINE: &[(&str, u32)] = &[("mainline", 120_105)];

    /// A release's one asset: `<Name>.zip` with `<Name>/<Name>.toc` (`interfaces`,
    /// `## Version: <tag>`) and a `core.lua` that differs per tag.
    pub struct Rel {
        file: String,
        flavors: &'static [(&'static str, u32)],
        toc_path: String,
        toc: String,
        core_path: String,
        core: String,
    }

    impl Rel {
        pub fn new(
            name: &str,
            tag: &str,
            interfaces: &str,
            flavors: &'static [(&'static str, u32)],
        ) -> Self {
            let title = format!("## Title: {name}");
            let version = format!("## Version: {tag}");
            Self {
                file: format!("{name}.zip"),
                flavors,
                toc_path: format!("{name}/{name}.toc"),
                toc: toc(interfaces, &[&title, &version]),
                core_path: format!("{name}/core.lua"),
                core: format!("-- {name} {tag}"),
            }
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                self.file.as_str(),
                self.flavors,
                vec![
                    (self.toc_path.as_str(), self.toc.as_str()),
                    (self.core_path.as_str(), self.core.as_str()),
                ],
            )]
        }
    }

    /// Three clients (retail, era, classic) and five managed addons: Foo and Bar on every
    /// client one release behind, Baz on every client and Qux on retail current, and Pin
    /// on retail pinned at v1 while v2 is out. Every remembered check is a day old.
    pub fn three_clients() -> Home {
        let mut home = Home::new(&["wow", "wow_classic_era", "wow_classic"]);
        let baz = Rel::new("Baz", "v1", EVERY, ALL);
        let qux = Rel::new("Qux", "v1", "120105", MAINLINE);
        let pin = Rel::new("Pin", "v1", "120105", MAINLINE);
        gh_release(&home.http, BAZ, "v1", &baz.assets());
        gh_release(&home.http, QUX, "v1", &qux.assets());
        gh_release(&home.http, PIN, "v1", &pin.assets());
        for target in ["retail", "era", "classic"] {
            home.run(&["install", "gh:o/baz", "-t", target]).expect(0);
        }
        home.run(&["install", "gh:o/qux", "-t", "retail"]).expect(0);
        home.run(&["install", "gh:o/pin", "--version", "v1", "-t", "retail"])
            .expect(0);
        for (base, name) in [(FOO, "Foo"), (BAR, "Bar")] {
            let (old, new) = (
                Rel::new(name, "v1", EVERY, ALL),
                Rel::new(name, "v2", EVERY, ALL),
            );
            home.installed_behind(
                base,
                ("v1", &old.assets()),
                ("v2", &new.assets()),
                &["retail", "era", "classic"],
            );
        }
        let pin2 = Rel::new("Pin", "v2", "120105", MAINLINE);
        gh_release_v(&home.http, PIN, "v2", &pin2.assets());
        gh_latest(&home.http, PIN, "v2");
        home
    }

    /// `(target, name, status)` per listed item.
    pub fn items(envelope: &Value) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            for i in t["items"].as_array().unwrap() {
                out.push((
                    t["target"]["id"].as_str().unwrap().to_owned(),
                    i["name"].as_str().unwrap().to_owned(),
                    i["status"].as_str().unwrap().to_owned(),
                ));
            }
        }
        out
    }

    /// The envelope without what changes between runs: target keys, `checked_at`,
    /// `meta` and the tool version.
    pub fn stable(envelope: &Value) -> Value {
        let mut text = envelope.to_string();
        for t in envelope["data"]["targets"].as_array().unwrap() {
            if let Some(key) = t["target"]["key"].as_str() {
                let id = t["target"]["id"].as_str().unwrap();
                text = text.replace(key, &format!("<key:{id}>"));
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

    /// The `net.log` lines after the first `before`.
    pub fn only_new(log: NetLogView, before: usize) -> NetLogView {
        NetLogView {
            entries: log.entries.into_iter().skip(before).collect(),
        }
    }

    /// Every file under the state dir but `net.log` (and its rotation), with its bytes.
    pub fn state_files(home: &Home) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for item in std::fs::read_dir(dir).unwrap() {
                let path = item.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                if rel.starts_with("net.log") {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&home.state(), &home.state(), &mut out);
        out
    }
}

fn triples(expected: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    expected
        .iter()
        .map(|(t, n, s)| ((*t).to_owned(), (*n).to_owned(), (*s).to_owned()))
        .collect()
}

#[test]
fn lists_updates_across_three_clients_from_one_fetch_per_ref() {
    let home = three_clients();
    let before = net_log(&home).len();
    let r = home.run(&["outdated"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[
            ("classic", "Bar", "update_available"),
            ("classic", "Foo", "update_available"),
            ("era", "Bar", "update_available"),
            ("era", "Foo", "update_available"),
            ("retail", "Bar", "update_available"),
            ("retail", "Foo", "update_available"),
            ("retail", "Pin", "pinned"),
        ]),
        "{}",
        r.context()
    );
    let summary = &envelope["data"]["summary"];
    assert_eq!(
        summary,
        &json!({"update_available": 6, "current": 4, "pinned": 1, "ahead": 0, "blocked": 0,
                "failed": 0, "unchecked": 0, "static": 0})
    );
    // The two refs whose tag did not move are confirmed by their probe alone (M2-D2).
    assert_eq!(
        envelope["data"]["requests"],
        json!({"fetched": 3, "probed": 2, "reused": 0})
    );
    assert_eq!(envelope["partial"], false);
    let pin = &envelope["data"]["targets"][2]["items"][2];
    assert_eq!(
        (&pin["pinned"], &pin["latest"]["version"]),
        (&Value::Bool(true), &Value::from("v2"))
    );

    // One `releases/latest` per distinct ref, a manifest only for the moved tags, none of
    // it through the GitHub REST API.
    let log = only_new(net_log(&home), before);
    assert_eq!(
        log.count("github.com", "release-latest"),
        5,
        "{}",
        log.context()
    );
    assert_eq!(log.count_purpose("release-json"), 3, "{}", log.context());
    assert_eq!(log.count_purpose("asset"), 0, "{}", log.context());
    common::netlog::assert_each_url_once(&log.entries);
    assert_no_github_rest(&log.entries);

    // Several clients have updates: one bare `update`.
    let hints: Vec<&str> = envelope["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["command"].as_str().unwrap())
        .collect();
    assert_eq!(hints, ["omaddons update"]);
    insta::assert_json_snapshot!("outdated_three_clients", stable(&envelope));
    // Snapshots sort keys; the wire keeps the documented order.
    for keys in [
        &[
            "\"checked_at\"",
            "\"requests\"",
            "\"targets\"",
            "\"summary\"",
        ][..],
        &[
            "\"name\"",
            "\"ref\"",
            "\"channel\"",
            "\"pinned\"",
            "\"installed\"",
            "\"latest\"",
            "\"status\"",
            "\"confidence\"",
            "\"blocked_reason\"",
            "\"unchecked_reason\"",
            "\"error\"",
        ][..],
    ] {
        let at: Vec<usize> = keys.iter().map(|k| r.stdout.find(k).unwrap()).collect();
        assert!(at.is_sorted(), "{keys:?} out of order\n{}", r.stdout);
    }

    // Inside the window: no request, the same data.
    let before = net_log(&home).len();
    let warm = home.run(&["outdated"]).expect(0);
    assert_eq!(net_log(&home).len(), before, "the re-check is free");
    assert_eq!(
        warm["data"]["requests"],
        json!({"fetched": 0, "probed": 0, "reused": 5})
    );
    let without_requests = |e: &Value| {
        let mut data = e["data"].clone();
        data["requests"] = Value::Null;
        data
    };
    assert_eq!(without_requests(&warm), without_requests(&envelope));

    // --check fails with the data kept; `check` is `outdated`.
    let r = home.run(&["outdated", "--check"]);
    let checked = r.expect(6);
    assert_eq!(checked["error"]["code"], "check_failed", "{}", r.context());
    assert_eq!(checked["data"]["summary"]["update_available"], 6);
    let alias = home.run(&["check"]).expect(0);
    assert_eq!(alias["command"], "outdated");
    assert_eq!(alias["data"], warm["data"]);

    // -v lists every item, current ones included.
    let all = home.run(&["outdated", "-v"]).expect(0);
    assert_eq!(items(&all).len(), 11);
    assert!(items(&all).contains(&("retail".into(), "Qux".into(), "current".into())));

    let human = home.exec(&["outdated".to_owned()]);
    assert_eq!(human.code, 0, "{}", human.context());
    insta::assert_snapshot!("outdated_three_clients_human", human.stdout);
}

#[test]
fn refresh_asks_again_inside_the_window() {
    let home = three_clients();
    home.run(&["outdated"]).expect(0);
    let before = net_log(&home).len();
    let run = home.run(&["outdated", "--refresh", "--progress", "json"]);
    let r = run.expect(0);
    assert_eq!(
        r["data"]["requests"],
        json!({"fetched": 5, "probed": 0, "reused": 0})
    );
    let log = only_new(net_log(&home), before);
    assert_eq!(
        log.count("github.com", "release-latest"),
        5,
        "{}",
        log.context()
    );
    // One fetch event per ref fetched, counting to 5.
    let fetches: Vec<(String, u64, u64)> = run
        .stderr
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["event"] == "progress" && e["phase"] == "fetch")
        .map(|e| {
            (
                e["message"].as_str().unwrap().to_owned(),
                e["current"].as_u64().unwrap(),
                e["total"].as_u64().unwrap(),
            )
        })
        .collect();
    let counts: Vec<(u64, u64)> = fetches.iter().map(|(_, c, t)| (*c, *t)).collect();
    assert_eq!(
        counts,
        [(1, 5), (2, 5), (3, 5), (4, 5), (5, 5)],
        "{}",
        run.context()
    );
}

#[test]
fn offline_answers_from_the_memo_or_says_unchecked() {
    let home = three_clients();
    let online = home.run(&["outdated"]).expect(0);
    let before = net_log(&home).len();

    let r = home.run(&["outdated", "--offline"]);
    let offline = r.expect(0);
    assert_eq!(items(&offline), items(&online), "{}", r.context());
    let codes: Vec<&str> = offline["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&"stale_check"), "{}", r.context());

    fs::remove_file(home.memo()).unwrap();
    let r = home.run(&["outdated", "--offline", "-v"]);
    let empty = r.expect(0);
    assert_eq!(empty["data"]["summary"]["unchecked"], 11, "{}", r.context());
    assert_eq!(empty["data"]["checked_at"], Value::Null);
    for t in empty["data"]["targets"].as_array().unwrap() {
        for i in t["items"].as_array().unwrap() {
            assert_eq!(i["unchecked_reason"], "offline", "{i}");
        }
    }
    assert_eq!(net_log(&home).len(), before, "offline makes no request");
}

#[test]
fn static_installs_make_no_request() {
    let home = Home::new(&["wow"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("Loc.zip");
    fs::write(&zip, zip_text(&[("Loc/Loc.toc", "## Interface: 120105\n")])).unwrap();
    let foo = dl.join("Foo.zip");
    fs::write(&foo, zip_text(&[("Foo/Foo.toc", "## Interface: 120105\n")])).unwrap();
    home.run(&["install", &zip.display().to_string(), "-t", "retail"])
        .expect(0);
    home.run(&[
        "install",
        "gh:o/foo",
        "--file",
        &foo.display().to_string(),
        "-t",
        "retail",
    ])
    .expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["outdated", "-v"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "Foo", "static"), ("retail", "Loc", "static")]),
        "{}",
        r.context()
    );
    assert_eq!(net_log(&home).len(), before);
    assert_eq!(envelope["data"]["checked_at"], Value::Null);
    let r = home.exec(&["outdated".to_owned()]);
    assert!(
        r.stdout
            .contains("No updates on 1 client · nothing checked"),
        "{}",
        r.context()
    );
}

#[test]
fn a_removed_project_is_failed_with_the_remove_fix() {
    use omaddons::error::{Error, ErrorCode, Suggestion};
    use omaddons::model::Channel;
    use omaddons::state::memo::{self, MemoEntry};

    let home = Home::new(&["wow"]);
    let foo = Rel::new("Foo", "v1", "120105", MAINLINE);
    gh_release(&home.http, FOO, "v1", &foo.assets());
    home.run(&["install", "gh:o/foo", "-t", "retail"]).expect(0);
    let r: omaddons::model::Ref = "gh:o/foo".parse().unwrap();
    let gone = Err(Error::new(
        ErrorCode::NotFound,
        "gh:o/foo has no GitHub releases (or the repository does not exist)",
        Suggestion::text("check the ref"),
    ));
    let entry = MemoEntry::of(&r, Channel::Stable, &gone, jiff::Timestamp::now()).unwrap();
    home.seed_memo(&common::memo::of(vec![(
        memo::key(&r, Channel::Stable),
        entry,
    )]));

    let before = net_log(&home).len();
    let run = home.run(&["outdated"]);
    let envelope = run.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "Foo", "failed")]),
        "{}",
        run.context()
    );
    let error = &envelope["data"]["targets"][0]["items"][0]["error"];
    assert_eq!(error["code"], "not_found");
    assert_eq!(
        error["fix_argv"],
        json!(["omaddons", "remove", "Foo", "-t", "retail"])
    );
    assert_eq!(envelope["partial"], true);
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert_eq!(codes, ["not_found"]);
    assert_eq!(
        net_log(&home).len(),
        before,
        "a fresh failure is remembered"
    );

    // A partial run with an update still fails --check.
    let bar = Rel::new("Bar", "v1", "120105", MAINLINE);
    gh_release(&home.http, BAR, "v1", &bar.assets());
    home.run(&["install", "gh:o/bar", "-t", "retail"]).expect(0);
    let url = "https://github.com/o/bar/releases/download/v2/Bar.zip";
    let newer = common::memo::entry_with_asset("gh:o/bar", "v2", jiff::Timestamp::now(), url);
    let entry = MemoEntry::of(&r, Channel::Stable, &gone, jiff::Timestamp::now()).unwrap();
    home.seed_memo(&common::memo::of(vec![
        (memo::key(&r, Channel::Stable), entry),
        newer,
    ]));
    let run = home.run(&["outdated", "--check"]);
    let envelope = run.expect(6);
    assert_eq!(
        (
            &envelope["partial"],
            &envelope["data"]["summary"]["update_available"]
        ),
        (&Value::Bool(true), &Value::from(1)),
        "{}",
        run.context()
    );
}

#[test]
fn a_failed_fetch_uses_remembered_metadata_or_fails_the_item() {
    let mut home = Home::new(&["wow"]);
    let (foo, bar) = (
        Rel::new("Foo", "v1", "120105", MAINLINE),
        Rel::new("Bar", "v1", "120105", MAINLINE),
    );
    gh_release(&home.http, FOO, "v1", &foo.assets());
    gh_release(&home.http, BAR, "v1", &bar.assets());
    home.run(&["install", "gh:o/foo", "gh:o/bar", "-t", "retail"])
        .expect(0);
    // Foo's check is a day old; Bar's was never remembered.
    let paths = common::http::paths(&home.env);
    let (mut memo, _) = omaddons::state::memo::load(&paths);
    memo.entries.retain(|key, _| !key.contains("o/bar"));
    for entry in memo.entries.values_mut() {
        entry.fetched_at -= jiff::SignedDuration::from_hours(24);
    }
    assert_eq!(memo.entries.len(), 1);
    home.seed_memo(&memo);
    let broken = home.scenario("broken");
    for base in [FOO, BAR] {
        let name = format!("latest-503-{}", base.rsplit('/').next().unwrap());
        let url = format!("{base}/releases/latest");
        fixture(&broken, &name, &url, 503, &[("retry-after", "0")], None);
    }

    let r = home.run(&["outdated", "-v", "--check"]);
    let envelope = r.expect(0);
    assert_eq!(
        items(&envelope),
        triples(&[("retail", "Bar", "failed"), ("retail", "Foo", "current")]),
        "{}",
        r.context()
    );
    assert_eq!(envelope["partial"], true);
    let bar_error = &envelope["data"]["targets"][0]["items"][0]["error"];
    assert_eq!(
        (&bar_error["code"], &bar_error["retryable"]),
        (&Value::from("http_status"), &Value::Bool(true))
    );
    let messages: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "http_status")
        .filter_map(|w| w["message"].as_str())
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("gh:o/foo") && m.contains("using its release metadata")),
        "{messages:?}"
    );
    // Nothing retryable is remembered: the memo still has Foo's day-old entry only.
    let (memo, _) = omaddons::state::memo::load(&paths);
    assert_eq!(memo.entries.len(), 1);
}

#[test]
fn verify_downloads_unverified_updates_only_and_remembers_the_verdicts() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let (old, new) = (
        Rel::new("Foo", "v1", EVERY, ALL),
        Rel::new("Foo", "v2", EVERY, ALL),
    );
    home.installed_behind(
        FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail", "era"],
    );
    let confidences = |e: &Value| -> Vec<String> {
        e["data"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|t| t["items"].as_array().unwrap().clone())
            .map(|i| i["confidence"].as_str().unwrap_or("-").to_owned())
            .collect()
    };
    let plain = home.run(&["outdated"]).expect(0);
    assert_eq!(confidences(&plain), ["declared", "declared"]);

    let state = state_files(&home);
    let before = net_log(&home).len();
    let r = home.run(&["outdated", "--verify"]);
    let verified = r.expect(0);
    assert_eq!(
        confidences(&verified),
        ["verified", "verified"],
        "{}",
        r.context()
    );
    let log: NetLogView = only_new(net_log(&home), before);
    assert!(log.count_purpose("asset") >= 1, "{}", log.context());
    assert_eq!(
        log.count("github.com", "release-latest"),
        0,
        "{}",
        log.context()
    );
    assert_eq!(
        state_files(&home),
        state,
        "records, history and the rest unchanged"
    );
    let (memo, _) = omaddons::state::memo::load(&common::http::paths(&home.env));
    assert!(
        memo.candidates
            .iter()
            .any(|f| f.release_id == "gh:o/foo@v2" && !f.verdicts.is_empty())
    );

    // Verified now: a second --verify downloads nothing.
    let before = net_log(&home).len();
    let again = home.run(&["outdated", "--verify"]).expect(0);
    assert_eq!(confidences(&again), ["verified", "verified"]);
    assert_eq!(net_log(&home).len(), before);
}

#[test]
fn outdated_changes_nothing_but_the_memo_and_net_log() {
    let home = three_clients();
    let state = state_files(&home);
    let addons = common::trees::tree_hash(&home.root);
    home.run(&["outdated", "--refresh"]).expect(0);
    home.run(&["outdated", "--offline"]).expect(0);
    assert_eq!(state_files(&home), state);
    assert_eq!(common::trees::tree_hash(&home.root), addons);
}

#[test]
fn changelog_without_updates_asks_for_none() {
    let home = Home::new(&["wow"]);
    let baz = Rel::new("Baz", "v1", EVERY, ALL);
    gh_release(&home.http, BAZ, "v1", &baz.assets());
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    let before = net_log(&home).len();
    let r = home.run(&["outdated", "--changelog", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["update_available"], 0);
    let log = only_new(net_log(&home), before);
    assert_eq!(log.count_purpose("releases-atom"), 0, "{}", log.context());
}

#[test]
fn one_client_with_updates_names_it() {
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let baz = Rel::new("Baz", "v1", EVERY, ALL);
    gh_release(&home.http, BAZ, "v1", &baz.assets());
    home.run(&["install", "gh:o/baz", "-t", "era"]).expect(0);
    let (old, new) = (
        Rel::new("Qux", "v1", "120105", MAINLINE),
        Rel::new("Qux", "v2", "120105", MAINLINE),
    );
    home.installed_behind(
        QUX,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail"],
    );
    let envelope = home.run(&["outdated"]).expect(0);
    let hints: Vec<&Value> = envelope["commands"].as_array().unwrap().iter().collect();
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints[0]["argv"],
        json!(["omaddons", "update", "-t", "retail"])
    );
    assert_eq!(hints[0]["action"], "update");
    // The hint works: after it, nothing is out of date.
    let argv: Vec<&str> = hints[0]["argv"].as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    home.run(&argv).expect(0);
    let after = home.run(&["outdated", "--check"]).expect(0);
    assert_eq!(after["data"]["summary"]["update_available"], 0);
    assert_eq!(after["commands"], json!([]));
}
