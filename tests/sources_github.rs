//! The GitHub source through the full network stack: six real `release.json`
//! publishers replayed from `tests/fixtures/http/github-stable`, and synthetic
//! scenarios for the failure paths.

mod common;

use common::env::TestEnv;
use common::http::{fixture, paths, record, replay, scenario};
use common::netlog::{assert_each_url_once, assert_no_github_rest, count_host, read_net_log};
use omaddons::error::ErrorCode;
use omaddons::model::{AssetLocation, ReleaseSet};
use omaddons::sources::github::GithubSource;
use omaddons::sources::{SourceRegistry, Wanted, fetch_once};
use support::{fetch_one, want};

/// Six `release.json` publishers (owner/repo confirmed against GitHub on 2026-10-14).
const PUBLISHERS: [&str; 6] = [
    "gh:WeakAuras/WeakAuras2",
    "gh:DeadlyBossMods/DeadlyBossMods",
    "gh:BigWigsMods/BigWigs",
    "gh:Tercioo/Plater-Nameplates",
    "gh:Nevcairiel/Bartender4",
    "gh:Questie/Questie",
];

#[cfg(test)]
mod support {
    use super::*;

    pub fn want(r: &str) -> Wanted {
        Wanted::stable(r.parse().unwrap())
    }

    /// One ref through the builtin registry over a replay of `scenario`.
    pub fn fetch_one(
        env: &TestEnv,
        scenario: &std::path::Path,
        w: Wanted,
    ) -> omaddons::error::Result<ReleaseSet> {
        let http = replay(env, scenario);
        let mut results = fetch_once([w], &SourceRegistry::builtin(), http.as_ref());
        results.pop_first().unwrap().1
    }
}

/// Re-records `github-stable` from live GitHub. Delete the scenario directory first,
/// then review the diff: fixtures are public.
/// `OMADDONS_RECORD=1 cargo test --test sources_github -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_github_stable() {
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("github-stable"));
    let results = fetch_once(
        PUBLISHERS.map(want),
        &SourceRegistry::builtin(),
        http.as_ref(),
    );
    for (w, result) in results {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        let set = result.unwrap();
        assert!(!set.releases[0].assets.is_empty(), "{}: {set:?}", w.r#ref);
    }
}

#[test]
fn six_publishers_for_three_targets_without_rest() {
    let env = TestEnv::new();
    let http = replay(&env, &scenario("github-stable"));
    let wanted = (0..3).flat_map(|_| PUBLISHERS.map(want));
    let results = fetch_once(wanted, &SourceRegistry::builtin(), http.as_ref());
    assert_eq!(results.len(), 6, "three targets, one fetch per addon");

    let set = |r: &str| -> &ReleaseSet {
        let result = &results[&want(r)];
        assert!(result.is_ok(), "{r}: {result:?}");
        result.as_ref().unwrap()
    };
    let expected = [
        (
            "gh:WeakAuras/WeakAuras2",
            "5.22.1",
            "5.22.1",
            "WeakAuras-5.22.1.zip",
        ),
        (
            "gh:DeadlyBossMods/DeadlyBossMods",
            "12.1.14",
            "12.1.14",
            "DBM-Core-12.1.14.zip",
        ),
        (
            "gh:BigWigsMods/BigWigs",
            "v427.6",
            "v427.6",
            "BigWigs-v427.6.zip",
        ),
        (
            "gh:Tercioo/Plater-Nameplates",
            "Plater-v660",
            "Plater-v660",
            "Plater-v660.zip",
        ),
        (
            "gh:Nevcairiel/Bartender4",
            "4.18.3.1",
            "4.18.3.1",
            "Bartender4-4.18.3.1.zip",
        ),
        (
            "gh:Questie/Questie",
            "bundle/v12.1.0+v1.0.6",
            "bundle/v12.1.0+v1.0.6",
            "Questie-v12.1.0+v1.0.6.zip",
        ),
    ];
    for (r, tag, version, first_file) in expected {
        let release = &set(r).releases[0];
        assert_eq!(release.release_id, format!("{r}@{tag}"), "{release:?}");
        assert_eq!(release.version, version, "{release:?}");
        assert_eq!(release.assets[0].filename, first_file, "{release:?}");
        for asset in &release.assets {
            let download = format!("https://github.com/{}/releases/download/", &r[3..]);
            assert!(
                matches!(&asset.location, AssetLocation::Url(url) if url.starts_with(&download)),
                "{r}: {asset:?}"
            );
            assert!(
                !asset.flavors.is_empty(),
                "{r}: {} has metadata",
                asset.filename
            );
        }
    }

    let weakauras = &set("gh:WeakAuras/WeakAuras2").releases[0].assets[0];
    assert!(
        !weakauras.flavors.iter().any(|f| f.flavor == "mainline"),
        "WeakAuras' manifest lists no mainline, and none is invented"
    );
    let bigwigs = &set("gh:BigWigsMods/BigWigs").releases[0].assets[0];
    assert!(
        bigwigs
            .flavors
            .iter()
            .any(|f| f.flavor == "mainline" && f.interface.is_some())
    );
    let questie = &set("gh:Questie/Questie").releases[0].assets[0];
    assert!(
        matches!(&questie.location, AssetLocation::Url(u) if u.ends_with("/bundle/v12.1.0%2Bv1.0.6/Questie-v12.1.0%2Bv1.0.6.zip"))
    );

    let log = read_net_log(&paths(&env));
    let purpose = |p: &str| log.iter().filter(|e| e.purpose == p).count();
    assert_eq!(purpose("release-latest"), 6);
    assert_eq!(
        count_host(&log, "github.com"),
        12,
        "6 releases/latest + 6 release.json"
    );
    assert_eq!(log.len(), 18, "plus one asset-host hop per release.json");
    assert_each_url_once(&log);
    assert_no_github_rest(&log);
}

#[test]
fn failure_paths_through_the_stack() {
    let env = TestEnv::new();
    let dir = env.root().join("synthetic");
    fixture(
        &dir,
        "none",
        "https://github.com/o/none/releases/latest",
        404,
        &[],
        Some("Not Found"),
    );
    fixture(
        &dir,
        "pre",
        "https://github.com/o/pre/releases/latest",
        302,
        &[("location", "https://github.com/o/pre/releases")],
        None,
    );
    fixture(
        &dir,
        "plain",
        "https://github.com/o/plain/releases/latest",
        302,
        &[("location", "https://github.com/o/plain/releases/tag/v2")],
        None,
    );
    fixture(
        &dir,
        "plain-json",
        "https://github.com/o/plain/releases/download/v2/release.json",
        404,
        &[],
        Some("Not Found"),
    );
    fixture(
        &dir,
        "plain-rest",
        "https://api.github.com/repos/o/plain/releases/tags/v2",
        200,
        &[("x-ratelimit-remaining", "59")],
        Some(
            r#"{"tag_name": "v2", "assets": [{"name": "Plain-v2.zip", "browser_download_url": "https://github.com/o/plain/releases/download/v2/Plain-v2.zip"}]}"#,
        ),
    );

    let err = fetch_one(&env, &dir, want("gh:o/none")).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(
        err.fix.argv().unwrap(),
        ["omaddons", "install", "git:https://github.com/o/none"]
    );

    let err = fetch_one(&env, &dir, want("gh:o/pre")).unwrap_err();
    assert_eq!(err.code, ErrorCode::NoCompatibleRelease);
    assert_eq!(
        err.fix.argv().unwrap(),
        ["omaddons", "install", "gh:o/pre", "--channel", "beta"]
    );

    assert_no_github_rest(&read_net_log(&paths(&env)));

    // Without a release.json, the REST fallback lists the release's zips (the
    // no-fallback messages are github.rs unit tests).
    let set = fetch_one(&env, &dir, want("gh:o/plain")).unwrap();
    assert_eq!(set.releases[0].release_id, "gh:o/plain@v2");
    assert_eq!(set.releases[0].assets[0].filename, "Plain-v2.zip");
    assert_eq!(count_host(&read_net_log(&paths(&env)), "api.github.com"), 1);
}

#[test]
fn recent_tags_revalidate() {
    let env = TestEnv::new();
    let dir = env.root().join("atom");
    let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>a</id><title>v2</title><updated>2026-10-01T00:00:00Z</updated>
        <link rel="alternate" type="text/html" href="https://github.com/o/a/releases/tag/v2"/></entry></feed>"#;
    fixture(
        &dir,
        "atom",
        "https://github.com/o/a/releases.atom",
        200,
        &[("etag", "W/\"abc\"")],
        Some(feed),
    );
    let http = replay(&env, &dir);
    let first = GithubSource::recent_tags("o", "a", http.as_ref()).unwrap();
    assert_eq!(
        (first.tags[0].tag.as_str(), first.revalidated),
        ("v2", false)
    );
    let lines = read_net_log(&paths(&env));
    assert_eq!(lines[0].purpose, "releases-atom");
}
