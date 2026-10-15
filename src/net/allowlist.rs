//! The hard host allowlist (vision §5.7, D13). Hosts match exactly and
//! ASCII-case-insensitively; subdomains are never implied.

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

/// The hosts one run may contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowlist {
    hosts: BTreeSet<String>,
}

impl Allowlist {
    /// The always-list, the optional sources' hosts when enabled, and the git hosts the
    /// user configured.
    pub fn new(curseforge: bool, wago: bool, git_hosts: &[String]) -> Self {
        let optional = [(curseforge, CURSEFORGE), (wago, WAGO)]
            .into_iter()
            .filter(|(enabled, _)| *enabled)
            .flat_map(|(_, hosts)| hosts.iter().copied());
        let hosts = ALWAYS
            .iter()
            .copied()
            .chain(optional)
            .map(str::to_owned)
            .chain(git_hosts.iter().cloned())
            .map(|host| host.to_ascii_lowercase())
            .collect();
        Self { hosts }
    }

    /// Whether requests (and git subprocesses) may reach `host`.
    pub fn permits(&self, host: &str) -> bool {
        self.hosts.contains(&host.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_case_insensitive_hosts_only() {
        let list = Allowlist::new(false, false, &[]);
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
    fn optional_sources_and_git_hosts() {
        let off = Allowlist::new(false, false, &[]);
        for host in CURSEFORGE.iter().chain(WAGO) {
            assert!(!off.permits(host), "{host}");
        }
        let on = Allowlist::new(true, true, &["Codeberg.org".to_owned()]);
        for host in CURSEFORGE.iter().chain(WAGO) {
            assert!(on.permits(host), "{host}");
        }
        assert!(on.permits("codeberg.org"));
        assert!(!off.permits("codeberg.org"));
    }
}
