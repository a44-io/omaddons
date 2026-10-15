//! HTTP fixture scenarios for tests: write synthetic fixtures, and build the production
//! network stack over a replay (or, for re-recording, a record) of a scenario.

use std::fs;
use std::path::{Path, PathBuf};

use omaddons::net::{Http, NetConfig, NetLog, select_transport, stack_with};
use omaddons::state::paths::Paths;
use serde_json::json;

use super::env::TestEnv;

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

/// The env's paths.
pub fn paths(env: &TestEnv) -> Paths {
    Paths::with_roots(env.home(), env.config(), env.state(), env.cache())
}

/// The production stack over a replay of `scenario`; `net.log` goes to the env.
pub fn replay(env: &TestEnv, scenario: &Path) -> Box<dyn Http> {
    let fixtures = scenario.display().to_string();
    let lookup = move |name: &str| (name == "OMADDONS_HTTP_FIXTURES").then(|| fixtures.clone());
    let paths = paths(env);
    let transport = select_transport(&lookup, true, &paths);
    stack_with(
        &NetConfig::default(),
        NetLog::new(&paths).unwrap(),
        &paths,
        transport,
    )
}

/// The production stack recording into `scenario` (real network).
pub fn record(env: &TestEnv, scenario: &Path) -> Box<dyn Http> {
    fs::create_dir_all(scenario).unwrap();
    let fixtures = scenario.display().to_string();
    let lookup = move |name: &str| match name {
        "OMADDONS_HTTP_FIXTURES" => Some(fixtures.clone()),
        "OMADDONS_RECORD" => Some("1".to_owned()),
        _ => None,
    };
    let paths = paths(env);
    let transport = select_transport(&lookup, true, &paths);
    stack_with(
        &NetConfig::default(),
        NetLog::new(&paths).unwrap(),
        &paths,
        transport,
    )
}

/// A committed scenario under `tests/fixtures/http/`.
pub fn scenario(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/http")
        .join(name)
}
