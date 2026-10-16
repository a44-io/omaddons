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
    harness_capped(config, None, responses)
}

/// [`harness`] with the API response cap at `api_max_bytes` instead of
/// `API_RESPONSE_MAX_BYTES` (the stack is then built here, as `core_stack_with` does).
fn harness_capped(
    config: NetConfig,
    api_max_bytes: Option<u64>,
    responses: Vec<Canned>,
) -> Harness {
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
    let http = match api_max_bytes {
        None => core_stack_with(&config, log.clone(), Box::new(script)),
        Some(cap) => Box::new(Redirecting::new(Guarded::new(
            Box::new(decode::Decoding::new(Box::new(script))),
            config.allowlist(),
            log.clone(),
            config.offline,
            config.download_max_bytes,
            cap,
        ))),
    };
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
        .send(Request::get("https://github.com/big.zip", "asset").download())
        .unwrap_err();
    assert_eq!(
        (err.code, err.exit().code()),
        (ErrorCode::ArchiveTooLarge, 4)
    );
    assert!(err.message.contains("download limit"), "{}", err.message);
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(
        (line.status, line.error.as_deref()),
        (Some(200), Some("archive_too_large"))
    );

    let h = harness(config, vec![ok(&[0; 100])]);
    let response = h
        .http
        .send(Request::get("https://github.com/big.zip", "asset").download())
        .unwrap();
    let err = response.bytes().unwrap_err();
    assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(line.error.as_deref(), Some("archive_too_large"));
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn gzipped(body: &[u8]) -> Canned {
    Ok((
        200,
        vec![("Content-Encoding".into(), "gzip".into())],
        gzip(body),
    ))
}

#[test]
fn api_responses_and_downloads_each_have_their_own_cap() {
    let config = NetConfig {
        download_max_bytes: 1_000,
        ..NetConfig::default()
    };
    // An API response past the API cap: its own wording, no config fix.
    let h = harness_capped(config.clone(), Some(10), vec![ok(&[b'x'; 100])]);
    let response = h
        .http
        .send(Request::get("https://api.github.com/x", "api"))
        .unwrap();
    let err = response.bytes().unwrap_err();
    assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
    assert!(
        err.message
            .contains("from api.github.com is larger than the 10-byte API response limit"),
        "{}",
        err.message
    );
    assert!(
        err.fix.display().starts_with("retry later"),
        "{}",
        err.fix.display()
    );
    assert_eq!(err.details["max_bytes"], 10);
    // A download of the same size is under its own cap.
    let h = harness_capped(config, Some(10), vec![ok(&[b'x'; 100])]);
    let response = h
        .http
        .send(Request::get("https://github.com/x.zip", "asset").download())
        .unwrap();
    assert_eq!(response.bytes().unwrap().len(), 100);
}

#[test]
fn a_gzip_bomb_stops_at_the_api_cap() {
    let bomb = vec![0u8; 8 * 1024 * 1024];
    let h = harness_capped(NetConfig::default(), Some(64 * 1024), vec![gzipped(&bomb)]);
    let response = h
        .http
        .send(Request::get(
            "https://api.mmoui.com/v3/game/WOW/filelist.json",
            "api",
        ))
        .unwrap();
    let err = response.bytes().unwrap_err();
    assert_eq!(err.code, ErrorCode::ArchiveTooLarge);
    assert!(
        err.message.contains("API response limit"),
        "{}",
        err.message
    );
    let line = h.log.read(None).unwrap().pop().unwrap();
    assert_eq!(line.error.as_deref(), Some("archive_too_large"));
    // Reading stopped at the cap, not at the end of the bomb.
    assert!(line.bytes.unwrap() <= 64 * 1024 + 64 * 1024, "{line:?}");
}

#[test]
fn gzip_api_responses_decode_and_log_their_wire_size() {
    let json = br#"{"UIDir":["BugSack"],"UIMD5":"683278e9d6e1cbc1514c3066d3d347b2"}"#.repeat(50);
    let h = harness(NetConfig::default(), vec![gzipped(&json), ok(b"plain")]);
    let url = "https://api.mmoui.com/v3/game/WOW/filelist.json";
    let response = h.http.send(Request::get(url, "api")).unwrap();
    assert_eq!(response.header("content-encoding"), None);
    assert_eq!(response.bytes().unwrap(), json);
    let plain = h.http.send(Request::get(url, "api")).unwrap();
    assert_eq!(plain.bytes().unwrap(), b"plain");
    let lines = h.log.read(None).unwrap();
    assert_eq!(
        (lines[0].bytes, lines[0].wire_bytes),
        (Some(json.len() as u64), Some(gzip(&json).len() as u64))
    );
    assert_eq!((lines[1].bytes, lines[1].wire_bytes), (Some(5), None));
    // The API requests asked for gzip.
    let seen = h.seen.lock().unwrap();
    assert!(seen.iter().all(|r| {
        r.headers
            .iter()
            .any(|(k, v)| k == "Accept-Encoding" && v == "gzip")
    }));
}

#[test]
fn downloads_never_ask_for_gzip() {
    let h = harness(NetConfig::default(), vec![ok(b"zip")]);
    let response = h
        .http
        .send(Request::get("https://github.com/x.zip", "asset").download())
        .unwrap();
    assert_eq!(response.bytes().unwrap(), b"zip");
    let seen = h.seen.lock().unwrap();
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(k, _)| !k.eq_ignore_ascii_case("accept-encoding"))
    );
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

    // Tukui download tokens never reach net.log or a message.
    let link = "https://api.tukui.org/v1/download/elvui/SYNTHETIC-not-a-real-one";
    assert_eq!(parse_target(link, 0).unwrap().path, "/v1/download/elvui/…");
    assert_eq!(redact(link), "https://api.tukui.org/v1/download/elvui/…");
    assert_eq!(
        redact("https://API.tukui.org:443/v1/download/elvui/x/y"),
        "https://API.tukui.org:443/v1/download/elvui/…"
    );
    // Other paths, and other hosts, are kept.
    for kept in [
        "https://api.tukui.org/v1/addon/elvui",
        "https://api.tukui.org/v1/download/elvui",
        "https://example.org/v1/download/elvui/abc",
    ] {
        assert_eq!(redact(kept), kept);
    }
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
    let http = stack(&NetConfig::default(), log.clone(), &paths);
    let response = http
        .send(Request::get("https://github.com/", "live-probe"))
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(!response.bytes().unwrap().is_empty());
    let line = log.read(None).unwrap().pop().unwrap();
    assert_eq!((line.host.as_str(), line.status), ("github.com", Some(200)));
    assert!(Path::new(log.path()).exists());
}

#[test]
fn transport_selection_truth_table() {
    let env = |fixtures: Option<&'static str>, record: Option<&'static str>| {
        move |name: &str| match name {
            "OMADDONS_HTTP_FIXTURES" => fixtures.map(str::to_owned),
            "OMADDONS_RECORD" => record.map(str::to_owned),
            _ => None,
        }
    };
    let cases = [
        (None, None, true, TransportChoice::Real),
        (Some("/f"), None, true, TransportChoice::Replay("/f".into())),
        (
            Some("/f"),
            Some("0"),
            true,
            TransportChoice::Replay("/f".into()),
        ),
        (
            Some("/f"),
            Some("1"),
            true,
            TransportChoice::Record("/f".into()),
        ),
        (None, Some("1"), true, TransportChoice::Real),
        (Some(""), None, true, TransportChoice::Real),
        (Some("/f"), None, false, TransportChoice::Real),
        (Some("/f"), Some("1"), false, TransportChoice::Real),
    ];
    for (fixtures, record, debug, expected) in cases {
        assert_eq!(
            choose_transport(&env(fixtures, record), debug),
            expected,
            "{fixtures:?} {record:?} {debug}"
        );
    }
}
