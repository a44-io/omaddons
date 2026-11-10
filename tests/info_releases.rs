//! e2e suite: `info --releases`: an addon's recent releases from one listing request, each with its date, channel, release id, archives and a verdict per client, each installable as listed.
//!
//! A GitHub repository publishes a beta, a stable release for retail and Mists Classic
//! (its latest) and an older stable release for Mists Classic only; GitHub's REST
//! listing names them. `info --releases` lists them newest first from that one request,
//! judges each on each client as `latest.compat` judges the latest, caps the list at
//! `--limit`, and every listed version previews with `install --version` under the same
//! release id. WowAce lists its stable tags from its recorded tag list. Sources that
//! serve only their latest release, `--offline` and a bad `--limit` each say so.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::Path;

use common::addons::toc;
use common::home::Home;
use common::http::{Asset, fixture, gh_latest, gh_release_v};
use common::netlog::net_log;
use common::tlog::{Ran, TestLog, tlog};
use serde_json::{Value, json};

const FOO: &str = "https://github.com/o/foo";
const REF: &str = "gh:o/foo";

/// One release: its tag, whether GitHub marks it a prerelease, when it was published
/// (minutes after midnight), its zip's name and the flavors its `release.json` declares.
struct Rel {
    tag: &'static str,
    prerelease: bool,
    minutes: u32,
    zip: &'static str,
    flavors: &'static [(&'static str, u32)],
}

const RELEASES: [Rel; 3] = [
    Rel {
        tag: "v3",
        prerelease: true,
        minutes: 180,
        zip: "Foo-v3.zip",
        flavors: &[("mainline", 120_105)],
    },
    Rel {
        tag: "v2",
        prerelease: false,
        minutes: 120,
        zip: "Foo-v2.zip",
        flavors: &[("mainline", 120_105), ("mists", 50_504)],
    },
    Rel {
        tag: "v1",
        prerelease: false,
        minutes: 60,
        zip: "Foo-v1-mists.zip",
        flavors: &[("mists", 50_504)],
    },
];

/// Serves every release's `release.json` and zip, `v2` as the latest, and the REST
/// listing naming all three.
fn publish(scenario: &Path) {
    let mut listed = Vec::new();
    for r in &RELEASES {
        let interfaces: Vec<String> = r.flavors.iter().map(|(_, i)| i.to_string()).collect();
        let text = toc(&interfaces.join(", "), &[&format!("## Version: {}", r.tag)]);
        let assets: [Asset<'_>; 1] = [(r.zip, r.flavors, vec![("Foo/Foo.toc", text.as_str())])];
        gh_release_v(scenario, FOO, r.tag, &assets);
        let download = format!("{FOO}/releases/download/{}", r.tag);
        listed.push(json!({
            "tag_name": r.tag,
            "prerelease": r.prerelease,
            "draft": false,
            "published_at": format!("2026-11-01T{:02}:{:02}:00Z", r.minutes / 60, r.minutes % 60),
            "assets": [
                {"name": "release.json", "browser_download_url": format!("{download}/release.json")},
                {"name": r.zip, "browser_download_url": format!("{download}/{}", r.zip)},
            ],
        }));
    }
    gh_latest(scenario, FOO, "v2");
    fixture(
        scenario,
        "rest-listing",
        "https://api.github.com/repos/o/foo/releases?per_page=20",
        200,
        &[("x-ratelimit-remaining", "59")],
        Some(&Value::from(listed).to_string()),
    );
}

/// `omaddons <args> --json`, both streams checked for private names.
fn json(log: &TestLog, home: &Home, args: &[&str]) -> Ran {
    let mut argv = args.to_vec();
    argv.push("--json");
    let ran = log.run(home, &argv);
    ran.assert_private();
    ran
}

/// `install -t <client> --version <version> --dry-run -- <r>` as JSON: the release id its
/// plan names (`--json` before `--`, which ends the options).
fn previewed_release_id(log: &TestLog, home: &Home, client: &str, version: &str, r: &str) -> Value {
    let argv = [
        "install",
        "-t",
        client,
        "--version",
        version,
        "--dry-run",
        "--json",
        "--",
        r,
    ];
    let ran = log.run(home, &argv);
    ran.ok().assert_private();
    ran.data()["approval"]["selection"][0]["release_id"].clone()
}

/// What `releases` lists, as `(version, channel, release id, assets, compat)`.
fn listed(data: &Value) -> Vec<Value> {
    data["releases"]
        .as_array()
        .unwrap_or_else(|| panic!("releases is a list: {data}")) // ubs:ignore — the test's own failure
        .iter()
        .map(|r| {
            json!([
                r["version"],
                r["channel"],
                r["release_id"],
                r["assets"],
                r["compat"]
            ])
        })
        .collect()
}

#[test]
fn github_releases_are_listed_from_one_request_judged_per_client_and_installable_as_listed() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow", "wow_classic"]);
    publish(&home.http);

    log.step("info --releases");
    let ran = json(&log, &home, &["info", REF, "--releases"]);
    ran.ok();
    let data = ran.data();
    log.json("releases", &data["releases"]);
    log.assert_eq(
        "latest's own judgment",
        &data["latest"]["compat"],
        &json!({"classic": "declared", "retail": "declared"}),
    );
    log.assert_eq(
        "newest first: the beta, the latest with latest's judgment, the older mists build",
        &json!(listed(data)),
        &json!([
            ["v3", "beta", "gh:o/foo@v3", ["Foo-v3.zip"], {"classic": "likely", "retail": "likely"}],
            ["v2", "stable", "gh:o/foo@v2", ["Foo-v2.zip"], {"classic": "declared", "retail": "declared"}],
            ["v1", "stable", "gh:o/foo@v1", ["Foo-v1-mists.zip"], {"classic": "likely", "retail": "unlikely"}],
        ]),
    );
    log.assert_eq(
        "dates",
        &json!(
            data["releases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["published_at"].clone())
                .collect::<Vec<_>>()
        ),
        &json!([
            "2026-11-01T03:00:00Z",
            "2026-11-01T02:00:00Z",
            "2026-11-01T01:00:00Z"
        ]),
    );
    assert!(data.get("releases_unavailable").is_none(), "{data}");
    let requests = net_log(&home);
    log.assert_eq(
        "one listing request",
        &requests.count("api.github.com", "rest-releases"),
        &1,
    );

    log.step("info without --releases leaves both keys out");
    let plain = json(&log, &home, &["info", REF]);
    plain.ok();
    for key in ["releases", "releases_unavailable"] {
        assert!(plain.data().get(key).is_none(), "{key}: {}", plain.data());
    }

    log.step("--limit caps the list");
    let capped = json(&log, &home, &["info", REF, "--releases", "--limit", "2"]);
    capped.ok();
    let versions: Vec<&Value> = capped.data()["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["version"])
        .collect();
    log.assert_eq("two, newest first", &json!(versions), &json!(["v3", "v2"]));

    log.step("each listed release previews under its own release id");
    for (version, client) in [("v3", "retail"), ("v2", "retail"), ("v1", "classic")] {
        log.assert_eq(
            &format!("{version} on {client}"),
            &previewed_release_id(&log, &home, client, version, REF),
            &json!(format!("{REF}@{version}")),
        );
    }

    log.step("the human view");
    let human = log.run(&home, &["info", REF, "--releases"]);
    human.ok().assert_private();
    for expected in [
        "  releases:",
        "v3  2026-11-01",
        "beta",
        "likely on classic, retail",
        "likely on classic · not on retail",
    ] {
        assert!(
            human.stdout().contains(expected),
            "{expected:?} missing: {}",
            human.stdout()
        );
    }

    log.step("--offline asks nothing");
    let before = net_log(&home).len();
    let offline = json(&log, &home, &["info", REF, "--releases", "--offline"]);
    offline.ok();
    log.assert_eq("releases", &offline.data()["releases"], &Value::Null);
    log.assert_eq(
        "why",
        &offline.data()["releases_unavailable"],
        &json!({"reason": "offline"}),
    );
    log.assert_eq("no request", &net_log(&home).len(), &before);
}

#[test]
fn sources_that_serve_their_latest_only_and_bad_limits_say_so() {
    let log = tlog!();
    let home = Home::new(&["wow"]);
    for r in [
        "wowi:7032",
        "tukui:elvui",
        "url:https://github.com/o/foo/releases/download/v1/Foo.zip",
    ] {
        log.step(r);
        let ran = json(&log, &home, &["info", r, "--releases", "--offline"]);
        ran.ok();
        log.assert_eq(
            &format!("{r}: latest only, whatever the network"),
            &ran.data()["releases_unavailable"],
            &json!({"reason": "source_lists_latest_only"}),
        );
        log.assert_eq(
            &format!("{r}: releases"),
            &ran.data()["releases"],
            &Value::Null,
        );
    }

    log.step("a disabled source");
    let set = json(
        &log,
        &home,
        &["config", "set", "sources.github.enabled", "false"],
    );
    set.ok();
    let ran = json(&log, &home, &["info", REF, "--releases", "--offline"]);
    ran.ok();
    log.assert_eq(
        "github off",
        &ran.data()["releases_unavailable"],
        &json!({"reason": "source_disabled"}),
    );

    log.step("--limit");
    for (limit, bound) in [("0", "1"), ("21", "20")] {
        let ran = json(&log, &home, &["info", REF, "--releases", "--limit", limit]);
        ran.fails(2, "usage");
        log.assert_eq(
            &format!("--limit {limit}: the fix uses the bound"),
            &ran.envelope()["error"]["fix_argv"],
            &json!(["omaddons", "info", REF, "--releases", "--limit", bound]),
        );
    }
    let ran = json(&log, &home, &["info", REF, "--limit", "5"]);
    ran.fails(2, "usage");
}

#[test]
fn wowace_lists_its_stable_tags_newest_first_each_installable_as_listed() {
    let log = tlog!();
    log.step("home");
    let home = Home::new(&["wow"]);
    common::wowace::serve_recorded(&home);

    log.step("info --releases");
    let ran = json(
        &log,
        &home,
        &["info", "wowace:bug-grabber", "--releases", "--limit", "3"],
    );
    ran.ok();
    let data = ran.data();
    log.json("releases", &data["releases"]);
    let releases = data["releases"].as_array().unwrap();
    log.assert_eq("three", &releases.len(), &3);
    log.assert_eq(
        "the first is the latest",
        &releases[0]["release_id"],
        &data["latest"]["release_id"],
    );
    for r in releases {
        log.assert_eq("stable", &r["channel"], &json!("stable"));
        log.assert_eq("undated", &r["published_at"], &Value::Null);
        assert!(
            r["release_id"]
                .as_str()
                .unwrap()
                .starts_with("wowace:bug-grabber@"),
            "{r}"
        );
    }
    log.assert_eq(
        "one tag list",
        &net_log(&home).count("repos.wowace.com", "wowace-tags"),
        &1,
    );

    log.step("the second previews under its own release id");
    let version = releases[1]["version"].as_str().unwrap();
    log.assert_eq(
        "release id",
        &previewed_release_id(&log, &home, "retail", version, "wowace:bug-grabber"),
        &releases[1]["release_id"],
    );

    log.step("a revision peg names one tree");
    let pegged = json(
        &log,
        &home,
        &["info", "wowace:bug-grabber#r152", "--releases", "--offline"],
    );
    pegged.ok();
    log.assert_eq(
        "latest only",
        &pegged.data()["releases_unavailable"],
        &json!({"reason": "source_lists_latest_only"}),
    );
}
