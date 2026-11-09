//! `net.log` assertions for tests: what was requested, from which host, how often.

use omaddons::net::{NetLog, NetLogEntry};
use omaddons::state::paths::Paths;

use super::home::Home;
use super::http::paths;

/// Every line of `<state>/omaddons/net.log`.
pub fn read_net_log(paths: &Paths) -> Vec<NetLogEntry> {
    NetLog::new(paths).unwrap().read(None).unwrap()
}

/// The home's `net.log` so far.
pub fn net_log(home: &Home) -> NetLogView {
    NetLogView {
        entries: read_net_log(&paths(&home.env)),
    }
}

/// `net.log` entries, counted the way request-economy assertions need. (Not `NetLog`:
/// that is `omaddons::net::NetLog`.)
#[derive(Debug, Clone)]
pub struct NetLogView {
    pub entries: Vec<NetLogEntry>,
}

impl NetLogView {
    /// Requests to `host` for `purpose` (`("github.com", "release-latest")`).
    pub fn count(&self, host: &str, purpose: &str) -> usize {
        self.entries
            .iter()
            .filter(|e| e.host == host && e.purpose == purpose)
            .count()
    }

    /// Requests for `purpose` (`"asset"`) to any host.
    pub fn count_purpose(&self, purpose: &str) -> usize {
        self.entries.iter().filter(|e| e.purpose == purpose).count()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One line per request, for assertion messages.
    pub fn context(&self) -> String {
        self.entries
            .iter()
            .map(|e| format!("{} {}{} ({})", e.method, e.host, e.path, e.purpose))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// How many requests went to `host`.
pub fn count_host(entries: &[NetLogEntry], host: &str) -> usize {
    entries.iter().filter(|e| e.host == host).count()
}

/// Fails, listing them, if any request went to the GitHub REST API.
pub fn assert_no_github_rest(entries: &[NetLogEntry]) {
    let rest: Vec<String> = entries
        .iter()
        .filter(|e| e.host == "api.github.com")
        .map(|e| format!("{} {}{} ({})", e.method, e.host, e.path, e.purpose))
        .collect();
    assert!(
        rest.is_empty(),
        "GitHub REST requests:\n{}",
        rest.join("\n")
    );
}

/// [`assert_no_github_rest`] for an `info` run: its `about` asks the REST API what a
/// repository is (`rest-repo`, `tests/info_about.rs`), and nothing else may.
pub fn assert_no_github_rest_beside_about(entries: &[NetLogEntry]) {
    let others: Vec<NetLogEntry> = entries
        .iter()
        .filter(|e| e.purpose != "rest-repo")
        .cloned()
        .collect();
    assert_no_github_rest(&others);
}

/// Fails, listing them, if any method + host + path was requested more than once.
pub fn assert_each_url_once(entries: &[NetLogEntry]) {
    let mut seen = std::collections::BTreeMap::new();
    for e in entries {
        *seen
            .entry(format!("{} {}{}", e.method, e.host, e.path))
            .or_insert(0usize) += 1;
    }
    let repeated: Vec<String> = seen
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(url, n)| format!("{url} x{n}"))
        .collect();
    assert!(
        repeated.is_empty(),
        "requested more than once:\n{}",
        repeated.join("\n")
    );
}
