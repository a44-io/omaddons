//! Fixture replay through the full network stack: redirects, the allowlist, `net.log`,
//! and unmatched requests.

use crate::common;

use std::fs;
use std::path::Path;

use common::env::TestEnv;
use common::http::{fixture, paths, replay};
use common::netlog::{assert_each_url_once, assert_no_github_rest, count_host, read_net_log};
use omaddons::error::ErrorCode;
use omaddons::net::{Http, Request};
use omaddons::state::paths::Paths;
use support::stack;

#[cfg(test)]
mod support {
    use super::*;

    /// The production stack over a replay of `scenario`, and the env's paths.
    pub fn stack(env: &TestEnv, scenario: &Path) -> (Box<dyn Http>, Paths) {
        (replay(env, scenario), paths(env))
    }
}

#[test]
fn a_redirect_chain_replays_through_the_stack() {
    let env = TestEnv::new();
    let scenario = env.root().join("scenario");
    fixture(
        &scenario,
        "release-json",
        "https://github.com/WeakAuras/WeakAuras2/releases/download/5.19.10/release.json",
        302,
        &[(
            "location",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1?id=7",
        )],
        None,
    );
    fixture(
        &scenario,
        "asset",
        "https://release-assets.githubusercontent.com/github-production-release-asset/1?id=7",
        200,
        &[("content-type", "application/json")],
        Some("{\"releases\": []}"),
    );
    let (http, paths) = stack(&env, &scenario);
    let url = "https://github.com/WeakAuras/WeakAuras2/releases/download/5.19.10/release.json?X-Amz-Signature=x";
    let body = http
        .send(Request::get(url, "release-json"))
        .unwrap()
        .bytes()
        .unwrap();
    assert_eq!(body, b"{\"releases\": []}");
    let log = read_net_log(&paths);
    assert_eq!(log.len(), 2, "both hops are logged");
    assert_eq!((log[0].status, log[1].status), (Some(302), Some(200)));
    assert_eq!(count_host(&log, "github.com"), 1);
    assert_eq!(count_host(&log, "release-assets.githubusercontent.com"), 1);
    assert_no_github_rest(&log);
    assert_each_url_once(&log);
}

#[test]
fn the_allowlist_still_guards_replayed_hops() {
    let env = TestEnv::new();
    let scenario = env.root().join("scenario");
    fixture(
        &scenario,
        "evil",
        "https://github.com/a/b/releases/latest",
        302,
        &[("location", "https://evil.example.com/x")],
        None,
    );
    let (http, _) = stack(&env, &scenario);
    let err = http
        .send(Request::get(
            "https://github.com/a/b/releases/latest",
            "latest",
        ))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::HostBlocked);
}

#[test]
fn unmatched_requests_name_the_url() {
    let env = TestEnv::new();
    let scenario = env.root().join("scenario");
    fs::create_dir_all(&scenario).unwrap();
    let (http, _) = stack(&env, &scenario);
    let err = http
        .send(Request::get("https://github.com/a/b/releases.atom", "atom"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        err.message
            .contains("no fixture for GET https://github.com/a/b/releases.atom"),
        "{}",
        err.message
    );

    let broken = env.root().join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("x.json"), "{").unwrap();
    let (http, _) = stack(&env, &broken);
    let err = http
        .send(Request::get("https://github.com/a/b", "x"))
        .unwrap_err();
    assert!(
        err.message.contains("replay: cannot load"),
        "{}",
        err.message
    );
}

#[test]
fn a_gzipped_fixture_decodes_and_a_304_returns_the_same_body() {
    use common::http::{Resp, fixture_seq, gzip_bytes};

    let env = TestEnv::new();
    let scenario = env.root().join("scenario");
    let json = br#"[{"UID":"5995","UIName":"BugSack","UIMD5":"683278e9d6e1cbc1514c3066d3d347b2"}]"#
        .repeat(40);
    let gz = gzip_bytes(&json);
    let url = "https://api.mmoui.com/v3/game/WOW/filelist.json";
    let first = Resp::ok(gz.clone())
        .header("content-encoding", "gzip")
        .header("etag", "\"v1\"");
    fixture_seq(
        &scenario,
        "filelist",
        url,
        &[first, Resp::status(304).header("etag", "\"v1\"")],
    );
    let (http, paths) = stack(&env, &scenario);
    let mut req = Request::get(url, "filelist");
    req.validators = true;

    let response = http.send(req.clone()).unwrap();
    assert_eq!(response.header("content-encoding"), None);
    assert_eq!(response.bytes().unwrap(), json);
    // Revalidated: the cached body is the decoded one.
    let again = http.send(req).unwrap();
    assert_eq!(again.bytes().unwrap(), json);

    let log = read_net_log(&paths);
    assert_eq!(log.len(), 2);
    assert_eq!(
        (log[0].status, log[0].bytes, log[0].wire_bytes),
        (Some(200), Some(json.len() as u64), Some(gz.len() as u64))
    );
    assert_eq!(log[1].status, Some(304));
}
