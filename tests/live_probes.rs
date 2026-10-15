//! Live probes of the keyless M1 endpoints, run at milestone checkpoints only:
//! `OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored`. Each probe makes one
//! request (two for the conditional atom check) through the production stack against
//! `gh:WeakAuras/WeakAuras2` and checks raw shape only; source beads add decoder-level
//! probes here.

mod common;

use std::io::Read;

use common::env::TestEnv;
use common::http::paths;
use common::netlog::{assert_no_github_rest, read_net_log};
use omaddons::net::{CacheState, Http, NetConfig, NetLog, Request, stack};
use omaddons::sources::git::GitSource;
use omaddons::sources::github::GithubSource;
use omaddons::sources::{Source, Wanted};
use omaddons::state::paths::Paths;
use support::{latest_tag, live};

const REPO: &str = "https://github.com/WeakAuras/WeakAuras2";

#[cfg(test)]
mod support {
    use super::*;

    /// The production stack in an isolated environment, when `OMADDONS_LIVE=1`.
    pub fn live() -> Option<(TestEnv, Box<dyn Http>)> {
        if std::env::var("OMADDONS_LIVE").as_deref() != Ok("1") {
            return None;
        }
        let env = TestEnv::new();
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let http = stack(&NetConfig::default(), NetLog::new(&paths).unwrap(), &paths);
        Some((env, http))
    }

    /// The tag `releases/latest` redirects to.
    pub fn latest_tag(http: &dyn Http) -> String {
        let resp = http
            .send(Request::get(format!("{REPO}/releases/latest"), "probe").manual_redirects())
            .unwrap();
        assert_eq!(resp.status, 302);
        let location = resp.location().unwrap().to_owned();
        assert!(location.contains("/releases/tag/"), "{location}");
        location.rsplit('/').next().unwrap().to_owned()
    }
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn latest_release_redirects_to_a_tag() {
    let Some((_env, http)) = live() else { return };
    assert!(!latest_tag(http.as_ref()).is_empty());
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn the_releases_feed_revalidates() {
    let Some((_env, http)) = live() else { return };
    let mut req = Request::get(format!("{REPO}/releases.atom"), "probe");
    req.validators = true;
    let first = http.send(req.clone()).unwrap();
    assert_eq!(first.status, 200);
    assert!(first.header("etag").is_some(), "the feed carries an ETag");
    assert!(
        String::from_utf8(first.bytes().unwrap())
            .unwrap()
            .contains("<feed")
    );
    let second = http.send(req).unwrap();
    assert_eq!(
        (second.status, second.cache),
        (200, CacheState::Revalidated)
    );
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn release_json_decodes() {
    let Some((_env, http)) = live() else { return };
    let tag = latest_tag(http.as_ref());
    let resp = http
        .send(Request::get(
            format!("{REPO}/releases/download/{tag}/release.json"),
            "probe",
        ))
        .unwrap();
    assert_eq!(resp.status, 200);
    let json: serde_json::Value = serde_json::from_slice(&resp.bytes().unwrap()).unwrap();
    assert!(json["releases"].is_array(), "{json}");
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn commits_feed_and_codeload_zip() {
    let Some((_env, http)) = live() else { return };
    let feed = http
        .send(Request::get(format!("{REPO}/commits/main.atom"), "probe"))
        .unwrap();
    assert_eq!(feed.status, 200);
    let text = String::from_utf8(feed.bytes().unwrap()).unwrap();
    let sha: String = text
        .split("Grit::Commit/")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    assert_eq!(sha.len(), 40, "{sha}");
    let zip = http
        .send(
            Request::get(
                format!("https://codeload.github.com/WeakAuras/WeakAuras2/zip/{sha}"),
                "probe",
            )
            .download(),
        )
        .unwrap();
    assert_eq!(zip.status, 200);
    let mut magic = [0u8; 4];
    zip.into_reader().read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"PK\x03\x04");
}

/// source-github-stable: the production decoder on WeakAuras2's live `release.json`,
/// with zero REST requests.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn github_source_decodes_live() {
    let Some((env, http)) = live() else { return };
    let wanted = Wanted::stable("gh:WeakAuras/WeakAuras2".parse().unwrap());
    let set = GithubSource::new()
        .fetch(&[wanted], http.as_ref())
        .pop()
        .unwrap()
        .unwrap();
    let release = &set.releases[0];
    assert!(!release.assets.is_empty(), "{release:?}");
    assert!(
        release.assets.iter().all(|a| !a.flavors.is_empty()),
        "{release:?}"
    );
    assert_no_github_rest(&read_net_log(&paths(&env)));

    let recent = GithubSource::recent_tags("WeakAuras", "WeakAuras2", http.as_ref()).unwrap();
    assert!(
        recent
            .tags
            .iter()
            .any(|t| release.release_id.ends_with(&format!("@{}", t.tag))),
        "{recent:?}"
    );
    let again = GithubSource::recent_tags("WeakAuras", "WeakAuras2", http.as_ref()).unwrap();
    assert!(again.revalidated && again.tags == recent.tags);
}

/// git refs for GitHub repositories: the default branch through `commits.atom`, a tag
/// through its own commits feed, both pinned to a commit on codeload; zero REST.
#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1"]
fn git_source_decodes_live() {
    let Some((env, http)) = live() else { return };
    let wanted = [
        "git:https://github.com/shagu/pfUI",
        "git:https://github.com/fondlez/GroupBulletinBoard#v1.2.3",
    ]
    .map(|r| Wanted::stable(r.parse().unwrap()));
    for set in GitSource.fetch(&wanted, http.as_ref()) {
        let release = set.unwrap().releases.remove(0);
        let (_, sha) = release.release_id.rsplit_once('@').unwrap();
        assert!(
            sha.len() == 40 && release.version == sha[..7],
            "{release:?}"
        );
        assert_eq!(
            release.assets[0].location,
            omaddons::model::AssetLocation::Url(format!(
                "{}/zip/{sha}",
                release
                    .release_id
                    .trim_start_matches("git:https://github.com")
                    .split('@')
                    .next()
                    .map(|path| format!("https://codeload.github.com{path}"))
                    .unwrap()
            )),
        );
    }
    assert_no_github_rest(&read_net_log(&paths(&env)));
}
