//! The GitHub REST fallback through the full network stack: a legacy addon whose
//! releases carry only zip assets (fondlez/GroupBulletinBoard, a 3.3.5a addon) and a
//! repository whose newest release is a prerelease (WowUp/WowUp), replayed from
//! `tests/fixtures/http/github-rest`; and a synthetic rate-limit reserve stop.

mod common;

use std::fs;

use common::env::TestEnv;
use common::home::Home;
use common::http::{fixture, paths, record, replay, scenario};
use common::netlog::{count_host, read_net_log};
use common::trees::CustomRoot;
use omaddons::error::ErrorCode;
use omaddons::model::{AssetLocation, Channel, ExpectedHash, ReleaseSet};
use omaddons::net::Request;
use omaddons::sources::github::{GithubRestFallback, RestReason};
use omaddons::sources::github_rest::GithubRest;
use omaddons::sources::{SourceRegistry, Wanted, fetch_once};
use serde_json::Value;
use support::{GBB, WOWUP, fetch};

#[cfg(test)]
mod support {
    use super::*;

    pub const GBB: &str = "gh:fondlez/GroupBulletinBoard";
    pub const WOWUP: &str = "gh:WowUp/WowUp";

    /// One ref on `channel` through the builtin registry over the recorded scenario.
    pub fn fetch(env: &TestEnv, r: &str, channel: Channel) -> omaddons::error::Result<ReleaseSet> {
        let http = replay(env, &scenario("github-rest"));
        let mut w = Wanted::stable(r.parse().unwrap());
        w.channel = channel;
        fetch_once([w], &SourceRegistry::builtin(), http.as_ref())
            .pop_first()
            .unwrap()
            .1
    }
}

/// Re-records `github-rest` from live GitHub (three REST requests). Delete the scenario
/// first, then trim each API body to the fields the decoder reads, without logins or
/// release notes (fixtures are public).
/// `OMADDONS_RECORD=1 cargo test --test sources_github_rest -- --ignored record`
#[test]
#[ignore = "records live fixtures: OMADDONS_RECORD=1"]
fn record_github_rest() {
    if std::env::var("OMADDONS_RECORD").as_deref() != Ok("1") {
        return;
    }
    let env = TestEnv::new();
    let http = record(&env, &scenario("github-rest"));
    let mut beta = Wanted::stable(WOWUP.parse().unwrap());
    beta.channel = Channel::Beta;
    let wanted = [Wanted::stable(GBB.parse().unwrap()), beta];
    for (w, result) in fetch_once(wanted, &SourceRegistry::builtin(), http.as_ref()) {
        assert!(result.is_ok(), "{}: {result:?}", w.r#ref);
        if w.r#ref.to_string() == GBB {
            let set = result.unwrap();
            let location = &set.releases[0].assets[0].location;
            let url = match location {
                AssetLocation::Url(url) => Some(url.clone()),
                _ => None,
            };
            assert!(url.is_some(), "{location:?}");
            http.send(Request::get(url.unwrap(), "asset").download())
                .unwrap()
                .bytes()
                .unwrap();
        }
    }
}

#[test]
fn a_zip_only_legacy_release_lists_its_assets_through_the_api() {
    let env = TestEnv::new();
    let set = fetch(&env, GBB, Channel::Stable).unwrap();
    let release = &set.releases[0];
    assert_eq!(
        (
            release.release_id.as_str(),
            release.version.as_str(),
            release.channel
        ),
        (
            "gh:fondlez/GroupBulletinBoard@v1.2.3",
            "v1.2.3",
            Channel::Stable
        )
    );
    assert!(release.published_at.is_some());
    assert_eq!(release.assets.len(), 1, "{release:?}");
    let asset = &release.assets[0];
    assert_eq!(asset.filename, "GroupBulletinBoard-1.2.3.zip");
    assert_eq!(
        asset.location,
        AssetLocation::Url(
            "https://github.com/fondlez/GroupBulletinBoard/releases/download/v1.2.3/GroupBulletinBoard-1.2.3.zip"
                .into()
        )
    );
    assert_eq!(
        asset.expected_hash,
        ExpectedHash::sha256("65245d0693125bbea45c4fee2919545d49684d8a2c082ebaa682429a7e16d00c")
    );
    assert!(asset.name_flavors.is_empty() && !asset.nolib && asset.flavors.is_empty());

    let log = read_net_log(&paths(&env));
    let lines: Vec<(&str, &str, Option<u16>)> = log
        .iter()
        .map(|e| (e.host.as_str(), e.path.as_str(), e.status))
        .collect();
    assert_eq!(
        lines,
        [
            (
                "github.com",
                "/fondlez/GroupBulletinBoard/releases/latest",
                Some(302)
            ),
            (
                "github.com",
                "/fondlez/GroupBulletinBoard/releases/download/v1.2.3/release.json",
                Some(404)
            ),
            (
                "api.github.com",
                "/repos/fondlez/GroupBulletinBoard/releases/tags/v1.2.3",
                Some(200)
            ),
        ],
        "{log:#?}"
    );
}

#[test]
fn the_beta_channel_takes_the_newest_prerelease() {
    let env = TestEnv::new();
    let set = fetch(&env, WOWUP, Channel::Beta).unwrap();
    let release = &set.releases[0];
    assert_eq!(
        (release.version.as_str(), release.channel),
        ("v2.24.1-beta.1", Channel::Beta),
        "{release:?}"
    );
    let names: Vec<&str> = release.assets.iter().map(|a| a.filename.as_str()).collect();
    assert_eq!(
        names,
        [
            "WowUp-2.24.1-beta.1-arm64-mac.zip",
            "WowUp-2.24.1-beta.1-mac.zip"
        ],
        "only zips are candidates"
    );
    let log = read_net_log(&paths(&env));
    assert_eq!(count_host(&log, "api.github.com"), 1, "{log:#?}");
    assert_eq!(
        log.len(),
        1,
        "no releases/latest and no release.json: {log:#?}"
    );
}

#[test]
fn the_reserve_stops_the_next_api_request() {
    let env = TestEnv::new();
    let dir = env.root().join("reserve");
    let listing = r#"[{"tag_name": "v1", "prerelease": true, "published_at": "2026-10-01T00:00:00Z",
        "assets": [{"name": "A-v1.zip", "browser_download_url": "https://github.com/o/a/releases/download/v1/A-v1.zip", "size": 10}]}]"#;
    fixture(
        &dir,
        "a",
        "https://api.github.com/repos/o/a/releases?per_page=20",
        200,
        &[
            ("x-ratelimit-remaining", "4"),
            ("x-ratelimit-reset", "1800000000"),
        ],
        Some(listing),
    );
    let http = replay(&env, &dir);
    // A credential rides on the API requests but never into net.log or an error.
    let canary = "omaddons-canary-7f3a";
    let rest = GithubRest::with_token(Some(canary.to_owned()));
    let beta = |r: &str| {
        let mut w = Wanted::stable(r.parse().unwrap());
        w.channel = Channel::Beta;
        w
    };
    let first = rest
        .fetch(&beta("gh:o/a"), &RestReason::Prerelease, http.as_ref())
        .unwrap();
    assert_eq!(first.releases[0].version, "v1");
    assert_eq!(rest.budget().remaining, Some(4));

    let err = rest
        .fetch(&beta("gh:o/b"), &RestReason::Prerelease, http.as_ref())
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert!(err.retryable);
    assert_eq!(err.details["reset_at"], "2027-01-15T08:00:00Z", "{err:?}");
    assert!(err.fix.argv().is_none());
    assert!(
        err.fix.display().contains("OMADDONS_GITHUB_TOKEN"),
        "{}",
        err.fix.display()
    );
    let log = read_net_log(&paths(&env));
    assert_eq!(
        count_host(&log, "api.github.com"),
        1,
        "the second request was not sent"
    );
    let raw = fs::read_to_string(paths(&env).net_log()).unwrap();
    assert!(!raw.contains(canary) && !format!("{err:?}").contains(canary));
}

#[test]
fn a_zip_only_legacy_release_installs_into_a_335_client() {
    let mut home = Home::new(&[]);
    home.http = scenario("github-rest");
    let root = CustomRoot::at(home.env.home().join("Games/wow-335"))
        .version(3, 3, 5, 12_340)
        .build();
    home.run(&[
        "targets",
        "add",
        &root.display().to_string(),
        "--name",
        "wotlk",
    ])
    .expect(0);
    let r = home.run(&["install", GBB, "-t", "wotlk"]);
    let envelope: Value = r.expect(0);
    let item = &envelope["data"]["items"][0];
    assert_eq!(
        (&item["status"], &item["to_version"], &item["name"]),
        (
            &Value::from("installed"),
            &Value::from("v1.2.3"),
            &Value::from("GroupBulletinBoard")
        ),
        "{}",
        r.context()
    );
    let toc =
        fs::read_to_string(root.join("Interface/AddOns/GroupBulletinBoard/GroupBulletinBoard.toc"))
            .unwrap();
    assert!(toc.contains("## Interface: 30300"), "{toc}");
}
