//! Fixture replay through the full network stack: redirects, the allowlist, `net.log`,
//! and unmatched requests.

mod common;

use std::fs;
use std::path::Path;

use common::env::TestEnv;
use common::netlog::{assert_each_url_once, assert_no_github_rest, count_host, read_net_log};
use omaddons::error::ErrorCode;
use omaddons::net::{NetConfig, NetLog, Request, select_transport, stack_with};
use omaddons::state::paths::Paths;
use serde_json::json;
use support::{fixture, stack};

#[cfg(test)]
mod support {
    use super::*;

    /// Writes a one-response fixture for a GET of `url` under `scenario/<host>/`.
    pub fn fixture(
        scenario: &Path,
        name: &str,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) {
        let host = url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap();
        let dir = scenario.join(host);
        fs::create_dir_all(&dir).unwrap();
        let body_file = body.map(|b| {
            fs::write(dir.join(format!("{name}.body")), b).unwrap();
            format!("{name}.body")
        });
        let doc = json!({
            "schema_version": "1.0",
            "request": {"method": "GET", "url": url},
            "responses": [{"status": status, "headers": headers, "body_file": body_file}],
        });
        fs::write(dir.join(format!("{name}.json")), doc.to_string()).unwrap();
    }

    /// The production stack over a replay of `scenario`.
    pub fn stack(env: &TestEnv, scenario: &Path) -> (Box<dyn omaddons::net::Http>, Paths) {
        let paths = Paths::with_roots(env.home(), env.config(), env.state(), env.cache());
        let fixtures = scenario.display().to_string();
        let lookup = move |name: &str| (name == "OMADDONS_HTTP_FIXTURES").then(|| fixtures.clone());
        let transport = select_transport(&lookup, true, &paths);
        let http = stack_with(
            &NetConfig::default(),
            NetLog::new(&paths).unwrap(),
            &paths,
            transport,
        );
        (http, paths)
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
