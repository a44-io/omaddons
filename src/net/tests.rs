//! The stack over a scripted transport: no network.
// Test-only file (the single-writer scanner skips files marked like this).
#![cfg(test)]

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;

use super::guard::{parse_target, resolve};
use super::*;
use crate::politeness::MAX_REDIRECTS;
use crate::state::paths::Paths;

type Canned = Result<(u16, Vec<(String, String)>, Vec<u8>)>;

/// Replays canned responses in order and records every request it receives.
struct Script {
    responses: Mutex<VecDeque<Canned>>,
    seen: std::sync::Arc<Mutex<Vec<Request>>>,
}

impl Http for Script {
    fn send(&self, req: Request) -> Result<Response> {
        self.seen.lock().unwrap().push(req.clone());
        let next = self.responses.lock().unwrap().pop_front();
        let (status, headers, body) = next.expect("the script ran out of responses")?;
        Ok(Response::new(
            status,
            headers,
            req.url,
            Box::new(Cursor::new(body)),
        ))
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    paths: Paths,
    http: Box<dyn Http>,
    seen: std::sync::Arc<Mutex<Vec<Request>>>,
    log: NetLog,
}

fn harness(config: NetConfig, responses: Vec<Canned>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    // The tempdir is the (existing) home; fsx never creates home itself.
    let home = dir.path().to_path_buf();
    let paths = Paths::with_roots(
        &home,
        home.join(".config"),
        home.join(".local/state"),
        home.join(".cache"),
    );
    let log = NetLog::new(&paths).unwrap();
    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let script = Script {
        responses: Mutex::new(responses.into()),
        seen: std::sync::Arc::clone(&seen),
    };
    let http = stack_with(&config, log.clone(), Box::new(script));
    Harness {
        _dir: dir,
        paths,
        http,
        seen,
        log,
    }
}

fn redirect(to: &str) -> Canned {
    Ok((302, vec![("Location".into(), to.into())], Vec::new()))
}

fn ok(body: &[u8]) -> Canned {
    Ok((200, vec![], body.to_vec()))
}

fn log_text(paths: &Paths) -> String {
    std::fs::read_to_string(paths.net_log()).unwrap_or_default()
}

#[test]
fn a_redirect_to_the_asset_host_is_followed_and_both_hops_logged() {
    let h = harness(
        NetConfig::default(),
        vec![
            redirect("https://release-assets.githubusercontent.com/x/y.zip?sig=abc&token=def"),
            ok(b"zip"),
        ],
    );
    let response = h
        .http
        .send(Request::get("https://github.com/o/r/releases/download/v1/y.zip", "asset").download())
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.bytes().unwrap(), b"zip");
    let lines = h.log.read(None).unwrap();
    let summary: Vec<(&str, &str, Option<u16>, Option<u64>)> = lines
        .iter()
        .map(|l| (l.host.as_str(), l.path.as_str(), l.status, l.bytes))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "github.com",
                "/o/r/releases/download/v1/y.zip",
                Some(302),
                Some(0)
            ),
            (
                "release-assets.githubusercontent.com",
                "/x/y.zip",
                Some(200),
                Some(3)
            ),
        ]
    );
    assert!(
        lines
            .iter()
            .all(|l| l.purpose == "asset" && l.method == "GET" && l.via == Via::Http)
    );
    let text = log_text(&h.paths);
    for secret in ["sig=", "abc", "token", "def"] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into net.log: {text}"
        );
    }
}

#[test]
fn a_hop_to_an_unlisted_host_is_blocked_after_logging_the_earlier_hop() {
    let h = harness(
        NetConfig::default(),
        vec![redirect("https://example.org/payload"), ok(b"never")],
    );
    let err = h
        .http
        .send(Request::get("https://github.com/o/r", "release-json"))
        .unwrap_err();
    assert_eq!((err.code, err.exit().code()), (ErrorCode::HostBlocked, 4));
    assert!(err.message.contains("redirect hop 1"), "{}", err.message);
    assert_eq!(
        h.seen.lock().unwrap().len(),
        1,
        "nothing was sent to example.org"
    );
    assert_eq!(h.log.read(None).unwrap().len(), 1);
}

#[test]
fn unlisted_and_non_https_urls_are_refused_before_sending() {
    let h = harness(NetConfig::default(), vec![]);
    for url in [
        "https://example.org/",
        "http://github.com/",
        "ftp://github.com/x",
        "https://github.com:8443/x",
        "https://user:pass@github.com/x",
        "https://evil-github.com/x",
        "not a url",
    ] {
        let err = h.http.send(Request::get(url, "probe")).unwrap_err();
        assert_eq!(err.code, ErrorCode::HostBlocked, "{url}");
    }
    assert!(h.seen.lock().unwrap().is_empty());
    assert!(!h.paths.net_log().exists());
}

#[test]
fn offline_fails_every_request_without_logging() {
    let config = NetConfig {
        offline: true,
        ..NetConfig::default()
    };
    let h = harness(config, vec![ok(b"x")]);
    let err = h
        .http
        .send(Request::get("https://github.com/", "probe"))
        .unwrap_err();
    assert_eq!(
        (err.code, err.exit().code(), err.retryable),
        (ErrorCode::Offline, 1, false)
    );
    assert!(h.seen.lock().unwrap().is_empty());
    assert!(!h.paths.net_log().exists());
}

#[test]
fn the_size_cap_applies_to_announced_and_streamed_bodies() {
    let config = NetConfig {
        download_max_bytes: 10,
        ..NetConfig::default()
    };
    let announced = Ok((
        200,
        vec![("Content-Length".into(), "100".into())],
        vec![0; 100],
    ));
    let h = harness(config.clone(), vec![announced]);
    let err = h
        .http
        .send(Request::get("https://github.com/big.zip", "asset"))
        .unwrap_err();
    assert_eq!(
        (err.code, err.exit().code()),
        (ErrorCode::ArchiveTooLarge, 4)
    );
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(
        (line.status, line.error.as_deref()),
        (Some(200), Some("archive_too_large"))
    );

    let h = harness(config, vec![ok(&[0; 100])]);
    let response = h
        .http
        .send(Request::get("https://github.com/big.zip", "asset"))
        .unwrap();
    let err = response.bytes().unwrap_err();
    assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(line.error.as_deref(), Some("archive_too_large"));
}

#[test]
fn manual_mode_returns_the_redirect() {
    let h = harness(
        NetConfig::default(),
        vec![redirect("https://github.com/o/r/releases/tag/v5.19.10")],
    );
    let response = h
        .http
        .send(
            Request::get("https://github.com/o/r/releases/latest", "release-latest")
                .manual_redirects(),
        )
        .unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(
        response.location(),
        Some("https://github.com/o/r/releases/tag/v5.19.10")
    );
    drop(response);
    assert_eq!(
        h.log.read(None).unwrap().len(),
        1,
        "dropping the response logs it"
    );
}

#[test]
fn follow_stops_after_max_redirects() {
    let loop_back: Vec<Canned> = (0..=MAX_REDIRECTS + 1)
        .map(|_| redirect("/again"))
        .collect();
    let h = harness(NetConfig::default(), loop_back);
    let err = h
        .http
        .send(Request::get("https://github.com/start", "probe"))
        .unwrap_err();
    assert_eq!((err.code, err.retryable), (ErrorCode::Network, false));
    assert_eq!(h.seen.lock().unwrap().len(), MAX_REDIRECTS + 1);
    assert_eq!(h.log.read(None).unwrap().len(), MAX_REDIRECTS + 1);
}

#[test]
fn credentials_never_follow_a_redirect_to_another_host() {
    let h = harness(
        NetConfig::default(),
        vec![
            redirect("/repos/o/r/releases/assets/1"),
            redirect("https://objects.githubusercontent.com/blob"),
            ok(b"zip"),
        ],
    );
    let req = Request::get(
        "https://api.github.com/repos/o/r/releases/assets/1",
        "rest-asset",
    )
    .header("Authorization", "Bearer s3cr3t")
    .header("Accept", "application/octet-stream");
    h.http.send(req).unwrap().bytes().unwrap();
    let seen = h.seen.lock().unwrap();
    let auth = |r: &Request| {
        r.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
    };
    assert!(auth(&seen[0]) && auth(&seen[1]), "same-host hops keep it");
    assert!(!auth(&seen[2]), "a cross-host hop drops it");
    // ubs:ignore — a header name, not a secret
    assert!(seen[2].headers.iter().any(|(k, _)| k == "Accept"));
    assert!(!log_text(&h.paths).contains("s3cr3t"));
}

#[test]
fn transport_errors_are_logged_without_a_status() {
    let failure = Err(Error::new(
        ErrorCode::Network,
        "connection reset",
        Suggestion::text("retry"),
    ));
    let h = harness(NetConfig::default(), vec![failure]);
    let err = h
        .http
        .send(Request::get("https://api.github.com/x", "probe"))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Network);
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(
        (line.status, line.bytes, line.error.as_deref()),
        (None, None, Some("network"))
    );
}

#[test]
fn error_for_status_maps_to_http_status() {
    let h = harness(
        NetConfig::default(),
        vec![Ok((503, vec![], vec![])), Ok((404, vec![], vec![]))],
    );
    let err = h
        .http
        .send(Request::get("https://api.github.com/x", "probe"))
        .unwrap()
        .error_for_status("x")
        .unwrap_err();
    assert_eq!((err.code, err.retryable), (ErrorCode::HttpStatus, true));
    let err = h
        .http
        .send(Request::get("https://api.github.com/x", "probe"))
        .unwrap()
        .error_for_status("x")
        .unwrap_err();
    assert!(!err.retryable);
}

#[test]
fn locations_resolve_against_the_request_url() {
    let base = "https://github.com/a/b/c?x=1";
    assert_eq!(resolve(base, "d"), "https://github.com/a/b/d");
    assert_eq!(resolve(base, "/z"), "https://github.com/z");
    assert_eq!(
        resolve(base, "//objects.githubusercontent.com/p"),
        "https://objects.githubusercontent.com/p"
    );
    assert_eq!(
        resolve(base, "https://codeload.github.com/q"),
        "https://codeload.github.com/q"
    );
    assert_eq!(resolve("https://github.com", "x"), "https://github.com/x");
}

#[test]
fn targets_and_redaction() {
    let target = parse_target("https://GitHub.com/a/b?c=d", 0).unwrap();
    assert_eq!(
        (target.host.as_str(), target.path.as_str()),
        ("github.com", "/a/b")
    );
    assert_eq!(
        redact("https://u:p@github.com/a?token=x#f"),
        "https://github.com/a"
    );
    assert_eq!(redact("https://github.com"), "https://github.com");
}

#[test]
#[ignore = "live network probe: OMADDONS_LIVE=1 cargo test -- --ignored live_github_request"]
fn live_github_request() {
    if std::env::var_os("OMADDONS_LIVE").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let paths = Paths::with_roots(
        &home,
        home.join(".config"),
        home.join(".local/state"),
        home.join(".cache"),
    );
    let log = NetLog::new(&paths).unwrap();
    let http = stack(&NetConfig::default(), log.clone());
    let response = http
        .send(Request::get("https://github.com/", "live-probe"))
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(!response.bytes().unwrap().is_empty());
    let line = log.read(None).unwrap().pop().unwrap();
    assert_eq!((line.host.as_str(), line.status), ("github.com", Some(200)));
    assert!(Path::new(log.path()).exists());
}
