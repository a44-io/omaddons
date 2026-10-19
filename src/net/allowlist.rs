//! The hard host allowlists (vision §5.7, D13): one for HTTP, one for the git binary,
//! with no host shared by construction. Hosts match exactly and ASCII-case-insensitively;
//! subdomains are never implied.

use std::collections::BTreeSet;

/// Always allowed.
pub const ALWAYS: &[&str] = &[
    "api.github.com",
    "github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "api.mmoui.com",
    "cdn.wowinterface.com",
    "api.tukui.org",
];

/// Allowed only while the CurseForge source is enabled (M5).
pub const CURSEFORGE: &[&str] = &[
    "api.curseforge.com",
    "edge.forgecdn.net",
    "mediafilez.forgecdn.net",
];

/// Allowed only while the Wago source is enabled (M5).
pub const WAGO: &[&str] = &["addons.wago.io"];

/// The hosts one run's HTTP requests (every redirect hop included) may contact. Never a
/// `git_hosts` host: those are [`GitHosts`]'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    hosts: BTreeSet<String>,
}

impl Allowlist {
    /// The always-list and the optional sources' hosts when enabled.
    pub fn new(curseforge: bool, wago: bool) -> Self {
        let optional = [(curseforge, CURSEFORGE), (wago, WAGO)]
            .into_iter()
            .filter(|(enabled, _)| *enabled)
            .flat_map(|(_, hosts)| hosts.iter().copied());
        let hosts = ALWAYS
            .iter()
            .copied()
            .chain(optional)
            .map(str::to_ascii_lowercase)
            .collect();
        Self { hosts }
    }

    /// Whether HTTP requests may reach `host`.
    pub fn permits(&self, host: &str) -> bool {
        self.hosts.contains(&host.to_ascii_lowercase())
    }
}

/// The hosts the git binary may contact: exactly the `git_hosts` the user configured,
/// never an HTTP host above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHosts {
    hosts: BTreeSet<String>,
}

impl GitHosts {
    pub fn new(git_hosts: &[String]) -> Self {
        Self {
            hosts: git_hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
        }
    }

    /// Whether git subprocesses may reach `host`.
    pub fn permits(&self, host: &str) -> bool {
        self.hosts.contains(&host.to_ascii_lowercase())
    }

    /// These hosts and `host`, as the TOML array `config set git_hosts` takes: a fix
    /// that allows one more host keeps the others.
    pub fn toml_with(&self, host: &str) -> String {
        let mut hosts = self.hosts.clone();
        hosts.insert(host.to_ascii_lowercase());
        let quoted: Vec<String> = hosts
            .iter()
            .map(|h| toml::Value::from(h.as_str()).to_string())
            .collect();
        format!("[{}]", quoted.join(", "))
    }
}

/// Whether `host` is one HTTP may ever reach (the always-list or an optional source's).
pub fn is_http_host(host: &str) -> bool {
    Allowlist::new(true, true).permits(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_case_insensitive_hosts_only() {
        let list = Allowlist::new(false, false);
        for host in ALWAYS {
            assert!(list.permits(host), "{host}");
        }
        assert!(list.permits("GitHub.com"));
        for host in [
            "evil-github.com",
            "github.com.evil",
            "sub.github.com",
            "github.com.",
            "example.org",
            "",
        ] {
            assert!(!list.permits(host), "{host}");
        }
    }

    #[test]
    fn optional_sources() {
        let off = Allowlist::new(false, false);
        for host in CURSEFORGE.iter().chain(WAGO) {
            assert!(!off.permits(host), "{host}");
        }
        let on = Allowlist::new(true, true);
        for host in CURSEFORGE.iter().chain(WAGO) {
            assert!(on.permits(host), "{host}");
        }
    }

    /// HTTP and git never share a host: git gets exactly the configured hosts, HTTP
    /// never one of them.
    #[test]
    fn git_hosts_and_http_hosts_are_disjoint() {
        let configured = ["Codeberg.org".to_owned(), "git.example.org".to_owned()];
        let git = GitHosts::new(&configured);
        assert!(git.permits("codeberg.org") && git.permits("CODEBERG.ORG"));
        assert!(git.permits("git.example.org"));
        for host in ALWAYS.iter().chain(CURSEFORGE).chain(WAGO) {
            assert!(!git.permits(host), "{host}");
            assert!(is_http_host(host), "{host}");
        }
        assert!(!GitHosts::new(&[]).permits("github.com"));
        let http = Allowlist::new(true, true);
        for host in &configured {
            assert!(!http.permits(host), "{host}");
            assert!(!is_http_host(host), "{host}");
        }
    }
}
