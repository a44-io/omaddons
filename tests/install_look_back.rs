//! GitHub look-back end to end (omad-author-built-loading-5po6.4): a repository that
//! publishes one release per client, as VuhDo does (`v3.218-titan`, `v3.217-mists`,
//! `v3.216-bcc`), installs on each client the newest release whose own `release.json`
//! declares it. The look-back reads `releases.atom`, then each older tag's
//! `release.json`, and asks GitHub's API once whether the release found is a
//! prerelease. It is remembered against the newest tag, never downgrades, and never runs
//! for `--version` or a pasted asset link. (No Cata client exists any more, so Titan
//! plays the newest release's part.)

mod common;

use common::home::Home;
use common::netlog::net_log;
use serde_json::Value;
use support::{BCC, MISTS, REF, Rel, TITAN, alpha_series, codes, listed, serve};

#[cfg(test)]
mod support {
    use std::path::Path;

    use common::addons::toc;
    use common::http::{Asset, fixture, gh_feed, gh_latest, gh_release_v};
    use serde_json::{Value, json};

    use super::*;

    pub const VUHDO: &str = "https://github.com/o/vuhdo";
    pub const REF: &str = "gh:o/vuhdo";

    /// One release: its tag, the flavor its `release.json` declares and that flavor's
    /// interface, which its TOC lists alone.
    pub type Rel = (&'static str, &'static str, u32);

    pub const TITAN: Rel = ("v3.218-titan", "titan", 38_002);
    pub const MISTS: Rel = ("v3.217-mists", "mists", 50_504);
    pub const BCC: Rel = ("v3.216-bcc", "bcc", 20_506);

    /// Serves `releases` (newest first, the first as latest) in `scenario`: each tag's
    /// `release.json` and `VuhDo-<tag>.zip`, the API's answer for each tag (a prerelease
    /// when named in `prereleases`), and `releases.atom` listing them all.
    pub fn serve(scenario: &Path, releases: &[Rel], prereleases: &[&str]) {
        for &release in releases {
            publish(scenario, release, prereleases.contains(&release.0));
        }
        gh_latest(scenario, VUHDO, releases[0].0);
        let feed: Vec<(&str, &str)> = releases.iter().map(|r| (r.0, "notes")).collect();
        gh_feed(scenario, VUHDO, &feed, None);
    }

    /// One release's `release.json`, `VuhDo-<tag>.zip` and the API's answer for its tag,
    /// without touching `releases/latest` or the feed.
    pub fn publish(scenario: &Path, (tag, flavor, interface): Rel, prerelease: bool) {
        let files = [
            (
                "VuhDo/VuhDo.toc".to_owned(),
                toc(&interface.to_string(), &[&format!("## Version: {tag}")]),
            ),
            ("VuhDo/VuhDo.lua".to_owned(), "-- VuhDo\n".to_owned()),
        ];
        let entries: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        let file = format!("VuhDo-{tag}.zip");
        let flavors = [(flavor, interface)];
        let assets: [Asset<'_>; 1] = [(file.as_str(), &flavors, entries)];
        gh_release_v(scenario, VUHDO, tag, &assets);
        let api = json!({
            "tag_name": tag,
            "prerelease": prerelease,
            "draft": false,
            "assets": [],
        });
        fixture(
            scenario,
            &format!("rest-{tag}"),
            &format!("https://api.github.com/repos/o/vuhdo/releases/tags/{tag}"),
            200,
            &[("x-ratelimit-remaining", "59")],
            Some(&api.to_string()),
        );
    }

    /// VuhDo on 2026-10-24: `releases/latest` is `v3.218-catac`, the feed holds only
    /// v3.219 alphas and betas, and the stable siblings in `stable` are published. A
    /// sibling named in `missing` answers 404, as GitHub does for no such release.
    pub fn alpha_series(scenario: &Path, stable: &[Rel], missing: &[&str]) {
        publish(scenario, ("v3.218-catac", "cata", 40_402), false);
        for &release in stable {
            publish(scenario, release, false);
        }
        for tag in missing {
            fixture(
                scenario,
                &format!("missing-{tag}"),
                &format!("{VUHDO}/releases/download/{tag}/release.json"),
                404,
                &[],
                Some("Not Found"),
            );
        }
        gh_latest(scenario, VUHDO, "v3.218-catac");
        let feed: Vec<(&str, &str)> = [
            "v3.219-vanilla-beta-3",
            "v3.219-vanilla-alpha-18",
            "v3.219-tbcc-beta-3",
            "v3.219-tbcc-alpha-18",
            "v3.219-mopc-beta-3",
            "v3.219-mopc-alpha-18",
            "v3.219-catac-beta-3",
            "v3.219-catac-alpha-18",
            "v3.219-beta-3",
            "v3.219-alpha-18",
        ]
        .iter()
        .map(|t| (*t, "notes"))
        .collect();
        gh_feed(scenario, VUHDO, &feed, None);
    }

    /// The warning codes of an envelope.
    pub fn codes(envelope: &Value) -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `list -t <id>`'s first addon.
    pub fn listed(home: &Home, id: &str) -> Value {
        let list = home.run(&["list", "-t", id]).expect(0);
        list["data"]["targets"][0]["addons"][0].clone()
    }
}

/// Requests whose path names `tag`'s `release.json`.
fn manifest_requests(home: &Home, tag: &str) -> usize {
    let wanted = format!("/download/{tag}/release.json");
    net_log(home)
        .entries
        .iter()
        .filter(|e| e.path.ends_with(&wanted))
        .count()
}

/// Requests to GitHub's REST API.
fn rest_requests(home: &Home) -> usize {
    common::netlog::count_host(&net_log(home).entries, "api.github.com")
}

/// Each client gets the newest release declared for it: Titan the newest with no
/// look-back and no API request, Mists and Anniversary an older one with
/// `older_release_for_client` and one API request each, Era none, naming every release
/// it looked at.
#[test]
fn each_client_installs_the_newest_release_declared_for_it() {
    let home = Home::new(&[
        "wow_classic_titan",
        "wow_classic",
        "wow_anniversary",
        "wow_classic_era",
    ]);
    serve(&home.http, &[TITAN, MISTS, BCC], &[]);

    let r = home.run(&["install", REF, "-t", "titan"]);
    let envelope = r.expect(0);
    assert!(
        !codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "titan")["version"],
        TITAN.0,
        "{}",
        r.context()
    );
    assert_eq!(net_log(&home).count("github.com", "releases-atom"), 0);
    assert_eq!(rest_requests(&home), 0, "{}", net_log(&home).context());

    let r = home.run(&["install", REF, "-t", "classic"]);
    let envelope = r.expect(0);
    assert!(
        codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    let warning = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "older_release_for_client")
        .unwrap();
    let message = warning["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("newest release {} has no build", TITAN.0))
            && message.contains(&format!("installed {} (2026-10-27)", MISTS.0)),
        "{message}"
    );
    assert_eq!(
        listed(&home, "classic")["version"],
        MISTS.0,
        "{}",
        r.context()
    );
    assert!(home.addons("_classic_").join("VuhDo/VuhDo.toc").is_file());
    assert_eq!(rest_requests(&home), 1, "{}", net_log(&home).context());
    // Mists stopped at its own release.
    assert_eq!(manifest_requests(&home, BCC.0), 0);

    let r = home.run(&["install", REF, "-t", "anniversary"]);
    let envelope = r.expect(0);
    assert!(
        codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "anniversary")["version"],
        BCC.0,
        "{}",
        r.context()
    );
    assert_eq!(rest_requests(&home), 2, "{}", net_log(&home).context());

    let r = home.run(&["install", REF, "-t", "era"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let tried: Vec<String> = error["details"]["tried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["asset"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        tried,
        [
            format!("VuhDo-{}.zip", TITAN.0),
            format!("{}/release.json", MISTS.0),
            format!("{}/release.json", BCC.0),
        ],
        "{}",
        r.context()
    );
    let reasons = error["details"]["tried"].to_string();
    assert!(reasons.contains("look_back: "), "{reasons}");
    // Nothing found asks the API nothing.
    assert_eq!(rest_requests(&home), 2, "{}", net_log(&home).context());
    assert!(!home.addons("_classic_era_").join("VuhDo").exists());
}

/// What a look-back found is remembered against the newest tag: while it is unchanged,
/// a refreshed check fetches no older `release.json`, no feed and no API, and the client's
/// release is current.
#[test]
fn an_unchanged_newest_release_reuses_the_look_back() {
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &[TITAN, MISTS, BCC], &[]);
    home.run(&["install", REF, "-t", "classic"]).expect(0);
    let before = net_log(&home);
    assert_eq!(manifest_requests(&home, MISTS.0), 1, "{}", before.context());

    let r = home.run(&["outdated", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["current"], 1, "{}", r.context());
    let after = net_log(&home);
    assert_eq!(manifest_requests(&home, MISTS.0), 1, "{}", after.context());
    assert_eq!(
        after.count("github.com", "releases-atom"),
        before.count("github.com", "releases-atom"),
        "{}",
        after.context()
    );
    assert_eq!(rest_requests(&home), 1, "{}", after.context());

    // `update` agrees: nothing to do, and still no look-back request.
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["version"],
        MISTS.0,
        "{}",
        r.context()
    );
    assert_eq!(
        manifest_requests(&home, MISTS.0),
        1,
        "{}",
        net_log(&home).context()
    );
    assert_eq!(rest_requests(&home), 1, "{}", net_log(&home).context());

    // Installing again plans again: the newest is refused, and the look-back is the
    // memo's. (A reinstall would take the recorded archive from the download cache.)
    home.run(&["remove", "VuhDo", "-t", "classic"]).expect(0);
    let r = home.run(&["install", REF, "-t", "classic"]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["version"],
        MISTS.0,
        "{}",
        r.context()
    );
    let log = net_log(&home);
    assert_eq!(manifest_requests(&home, MISTS.0), 1, "{}", log.context());
    assert_eq!(
        log.count("github.com", "releases-atom"),
        before.count("github.com", "releases-atom"),
        "{}",
        log.context()
    );
    assert_eq!(rest_requests(&home), 1, "{}", log.context());
}

/// An update moves to the newer release declared for the client, with the warning; once
/// there, it is current.
#[test]
fn an_update_moves_to_the_newer_release_declared_for_the_client() {
    const OLD: Rel = ("v3.215-mists", "mists", 50_504);
    let mut home = Home::new(&["wow_classic"]);
    serve(&home.http, &[OLD], &[]);
    home.run(&["install", REF, "-t", "classic"]).expect(0);
    assert_eq!(listed(&home, "classic")["version"], OLD.0);

    let later = home.scenario("later");
    serve(&later, &[TITAN, MISTS, OLD], &[]);
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    assert!(
        codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "classic")["version"],
        MISTS.0,
        "{}",
        r.context()
    );

    let r = home.run(&["outdated", "-t", "classic"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["current"], 1, "{}", r.context());
}

/// A release the feed does not place (deleted, or out of its reach) is never replaced by
/// what a look-back found: that could be a downgrade. (A tag versions cannot order, so
/// the update is planned and the planner decides.)
#[test]
fn a_look_back_never_downgrades() {
    const GONE: Rel = ("mists-hotfix", "mists", 50_504);
    let mut home = Home::new(&["wow_classic"]);
    serve(&home.http, &[GONE], &[]);
    home.run(&["install", REF, "-t", "classic"]).expect(0);

    let later = home.scenario("later");
    serve(&later, &[TITAN, MISTS, BCC], &[]);
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    assert!(
        !codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "classic")["version"],
        GONE.0,
        "{}",
        r.context()
    );
    // It looked back, found Mists' release, and left the installed one alone.
    assert_eq!(
        manifest_requests(&home, MISTS.0),
        1,
        "{}",
        net_log(&home).context()
    );
    let downloads = net_log(&home)
        .entries
        .iter()
        .filter(|e| e.path.ends_with(&format!("VuhDo-{}.zip", MISTS.0)))
        .count();
    assert_eq!(downloads, 0, "{}", net_log(&home).context());
}

/// An installed release newer in the feed than the look-back's answer stays: Titan's
/// build installed on Mists under `--any-flavor`, the setting then cleared, is not
/// replaced by Mists' older release once a newer Titan release is out, by `update` or
/// in `outdated`.
#[test]
fn an_installed_release_newer_than_the_look_back_stays() {
    const NEWER: Rel = ("v3.219-titan", "titan", 38_002);
    let mut home = Home::new(&["wow_classic"]);
    serve(&home.http, &[TITAN, MISTS, BCC], &[]);
    let r = home.run(&["install", REF, "-t", "classic", "--any-flavor"]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["version"],
        TITAN.0,
        "{}",
        r.context()
    );
    let r = home.run(&["install", REF, "-t", "classic", "--no-any-flavor"]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["any_flavor"],
        false,
        "{}",
        r.context()
    );

    let later = home.scenario("later");
    serve(&later, &[NEWER, TITAN, MISTS, BCC], &[]);
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    let envelope = r.expect(0);
    assert!(
        !codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "classic")["version"],
        TITAN.0,
        "{}",
        r.context()
    );
    // It looked back and found Mists' release, older than the one installed.
    assert_eq!(
        manifest_requests(&home, MISTS.0),
        1,
        "{}",
        net_log(&home).context()
    );
    let r = home.run(&["outdated", "-t", "classic"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["current"], 1, "{}", r.context());
}

/// A release GitHub marks a prerelease, or one named like a prerelease, is passed over:
/// the look-back continues to the next stable release declared for the client.
#[test]
fn a_prerelease_is_passed_over() {
    const RC: Rel = ("v3.216-mists-rc1", "mists", 50_504);
    const STABLE: Rel = ("v3.215-mists", "mists", 50_504);
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &[TITAN, MISTS, RC, STABLE], &[MISTS.0]);

    let r = home.run(&["install", REF, "-t", "classic"]);
    let envelope = r.expect(0);
    assert!(
        codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(
        listed(&home, "classic")["version"],
        STABLE.0,
        "{}",
        r.context()
    );
    // One API request for the prerelease, one for the release installed; none for the
    // tag named like a prerelease.
    assert_eq!(rest_requests(&home), 2, "{}", net_log(&home).context());
    assert_eq!(
        manifest_requests(&home, RC.0),
        0,
        "{}",
        net_log(&home).context()
    );
}

/// With GitHub's API budget spent after the first prerelease check, the look-back
/// stops: nothing installs and the refusal says why, and nothing is remembered.
#[test]
fn a_spent_api_budget_stops_the_look_back() {
    const STABLE: Rel = ("v3.215-mists", "mists", 50_504);
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &[TITAN, MISTS, STABLE], &[MISTS.0]);
    let api = serde_json::json!({ "tag_name": MISTS.0, "prerelease": true, "draft": false, "assets": [] });
    common::http::fixture(
        &home.http,
        &format!("rest-{}", MISTS.0),
        &format!(
            "https://api.github.com/repos/o/vuhdo/releases/tags/{}",
            MISTS.0
        ),
        200,
        &[
            ("x-ratelimit-remaining", "5"),
            ("x-ratelimit-reset", "1800000000"),
        ],
        Some(&api.to_string()),
    );

    let r = home.run(&["install", REF, "-t", "classic"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let tried = error["details"]["tried"].as_array().unwrap();
    let last = tried.last().unwrap();
    assert_eq!(last["asset"], "look_back", "{}", r.context());
    assert!(
        last["reason"]
            .as_str()
            .unwrap()
            .starts_with("rate_limited: "),
        "{last}"
    );
    assert_eq!(rest_requests(&home), 1, "{}", net_log(&home).context());
    assert!(!home.addons("_classic_").join("VuhDo").exists());
    let memo = std::fs::read_to_string(home.memo()).unwrap_or_default();
    assert!(!memo.contains("look_backs"), "{memo}");
}

/// A newest release whose own `release.json` declares the client has a build for it,
/// one that does not load: no look-back, and the refusal names that build. (OmniCC on
/// retail 12.1.5, 2026-10-24: 11.2.8 and 11.2.7 both declare mainline below the floor.)
#[test]
fn a_newest_release_declaring_the_client_never_looks_back() {
    const NEWER: Rel = ("v2", "mainline", 110_207);
    const OLDER: Rel = ("v1", "mainline", 110_205);
    let home = Home::new(&["wow"]);
    serve(&home.http, &[NEWER, OLDER], &[]);

    let r = home.run(&["install", REF, "-t", "retail"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("VuhDo-v2.zip"), "{message}");
    let tried = error["details"]["tried"].to_string();
    assert!(tried.contains("below_client_floor"), "{tried}");
    assert!(!tried.contains("look_back"), "{tried}");
    let log = net_log(&home);
    assert_eq!(
        log.count("github.com", "releases-atom"),
        0,
        "{}",
        log.context()
    );
    assert_eq!(manifest_requests(&home, OLDER.0), 0, "{}", log.context());
    assert_eq!(rest_requests(&home), 0, "{}", log.context());
}

/// A repository with one track (every release retail-only) is not read to the end of
/// its feed: after three older releases declaring the same other clients the
/// look-back stops, and the refusal says why.
#[test]
fn a_single_track_repository_stops_the_look_back_early() {
    const TRACK: [Rel; 6] = [
        ("v6", "mainline", 120_105),
        ("v5", "mainline", 120_105),
        ("v4", "mainline", 120_105),
        ("v3", "mainline", 120_105),
        ("v2", "mainline", 120_105),
        ("v1", "mainline", 120_105),
    ];
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &TRACK, &[]);
    let r = home.run(&["install", REF, "-t", "classic"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let reasons: Vec<String> = error["details"]["tried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        reasons.last().is_some_and(|r| r.contains("one track")),
        "{reasons:?}"
    );
    for (tag, reads) in [("v5", 1), ("v4", 1), ("v3", 1), ("v2", 0), ("v1", 0)] {
        assert_eq!(
            manifest_requests(&home, tag),
            reads,
            "{tag}: {}",
            net_log(&home).context()
        );
    }
}

/// Another client's release in between breaks the run, so a per-client release behind
/// more than three others is still found when the track changes on the way.
#[test]
fn another_clients_release_resets_the_track() {
    const MIXED: [Rel; 6] = [
        ("v6", "mainline", 120_105),
        ("v5", "mainline", 120_105),
        ("v4", "mainline", 120_105),
        ("v3", "bcc", 20_506),
        ("v2", "mainline", 120_105),
        ("v1", "mists", 50_504),
    ];
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &MIXED, &[]);
    let r = home.run(&["install", REF, "-t", "classic"]);
    let envelope = r.expect(0);
    assert!(
        codes(&envelope).contains(&"older_release_for_client".to_owned()),
        "{}",
        r.context()
    );
    assert_eq!(listed(&home, "classic")["version"], "v1", "{}", r.context());
}

/// A feed of prereleases only cannot reach the stable releases, so the newest stable
/// tag's siblings are probed: VuhDo's shape on 2026-10-24. Each client installs its own
/// `v3.218-<suffix>` with one API request; a sibling never published (`-tbcc` here) is
/// passed over by its 404, and that client is refused naming each probe.
#[test]
fn an_alpha_series_probes_the_newest_stable_tags_siblings() {
    let home = Home::new(&["wow_classic", "wow_classic_era", "wow_anniversary"]);
    alpha_series(
        &home.http,
        &[
            ("v3.218-mopc", "mists", 50_504),
            ("v3.218-vanilla", "classic", 11_509),
            ("v3.218", "mainline", 120_105),
        ],
        &["v3.218-tbcc"],
    );

    for (id, tag) in [("classic", "v3.218-mopc"), ("era", "v3.218-vanilla")] {
        let before = rest_requests(&home);
        let r = home.run(&["install", REF, "-t", id]);
        let envelope = r.expect(0);
        assert!(
            codes(&envelope).contains(&"older_release_for_client".to_owned()),
            "{id}: {}",
            r.context()
        );
        assert_eq!(listed(&home, id)["version"], tag, "{}", r.context());
        assert_eq!(
            rest_requests(&home),
            before + 1,
            "{}",
            net_log(&home).context()
        );
    }
    // No `release.json` of a prerelease in the feed was fetched.
    assert_eq!(manifest_requests(&home, "v3.219-mopc-beta-3"), 0);

    let r = home.run(&["install", REF, "-t", "anniversary"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());
    let tried = error["details"]["tried"].to_string();
    assert!(
        tried.contains("v3.218-tbcc/release.json")
            && tried.contains("probed as a sibling of v3.218-catac: publishes no release.json"),
        "{tried}"
    );
}

/// An installed sibling the feed does not hold is placed by its version: `v3.217-mopc`
/// updates to `v3.218-mopc`, and `v3.218-mopc` stays when the probe finds it again.
#[test]
fn a_probed_sibling_updates_by_version() {
    let mut home = Home::new(&["wow_classic"]);
    serve(&home.http, &[("v3.217-mopc", "mists", 50_504)], &[]);
    home.run(&["install", REF, "-t", "classic"]).expect(0);
    assert_eq!(listed(&home, "classic")["version"], "v3.217-mopc");

    let later = home.scenario("later");
    alpha_series(
        &later,
        &[
            ("v3.218-mopc", "mists", 50_504),
            ("v3.218-vanilla", "classic", 11_509),
        ],
        &["v3.218-tbcc", "v3.218"],
    );
    let r = home.run(&["update", "-t", "classic", "--refresh"]);
    r.expect(0);
    assert_eq!(
        listed(&home, "classic")["version"],
        "v3.218-mopc",
        "{}",
        r.context()
    );
    let r = home.run(&["outdated", "-t", "classic"]);
    let envelope = r.expect(0);
    assert_eq!(envelope["data"]["summary"]["current"], 1, "{}", r.context());
}

/// `--version` and a pasted asset link name one release: no look-back.
#[test]
fn a_named_release_never_looks_back() {
    let home = Home::new(&["wow_classic"]);
    serve(&home.http, &[TITAN, MISTS, BCC], &[]);

    let r = home.run(&["install", REF, "--version", TITAN.0, "-t", "classic"]);
    let error = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());

    let link = format!(
        "https://github.com/o/vuhdo/releases/download/{0}/VuhDo-{0}.zip",
        TITAN.0
    );
    let r = home.run(&["install", &link, "-t", "classic"]);
    let error: &Value = &r.expect(3)["error"];
    assert_eq!(error["code"], "no_compatible_release", "{}", r.context());

    let log = net_log(&home);
    assert_eq!(
        log.count("github.com", "releases-atom"),
        0,
        "{}",
        log.context()
    );
    assert_eq!(manifest_requests(&home, MISTS.0), 0, "{}", log.context());
    assert_eq!(rest_requests(&home), 0, "{}", log.context());
}
