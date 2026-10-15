//! `net.log` assertions for tests: what was requested, from which host, how often.

use omaddons::net::{NetLog, NetLogEntry};
use omaddons::state::paths::Paths;

/// Every line of `<state>/omaddons/net.log`.
pub fn read_net_log(paths: &Paths) -> Vec<NetLogEntry> {
    NetLog::new(paths).unwrap().read(None).unwrap()
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
