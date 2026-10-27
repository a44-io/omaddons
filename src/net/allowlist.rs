//! The hard host allowlists (vision §5.7, D13): one for HTTP, one for the git binary,
//! with no host shared by construction. Hosts match exactly and ASCII-case-insensitively;
//! subdomains are never implied.

use std::collections::BTreeSet;

use crate::sources::SourceId;

/// GitHub's hosts: REST, web (feeds, releases), archives and release assets.
pub const GITHUB_HOSTS: &[&str] = &[
    "api.github.com",
    "github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// WoWInterface's metadata and archive hosts.
pub const WOWI_HOSTS: &[&str] = &["api.mmoui.com", "cdn.wowinterface.com"];
/// Tukui's metadata and archive host.
pub const TUKUI_HOSTS: &[&str] = &["api.tukui.org"];
/// WowAce's public Subversion host.
pub const WOWACE_HOSTS: &[&str] = &["repos.wowace.com"];
/// The HTTPS hosts `git:` refs on github.com are read from (`sources::git_github`).
pub const GIT_GITHUB_HOSTS: &[&str] = &[GITHUB_HOSTS[1], GITHUB_HOSTS[2]];

/// `parts` end to end, as an array of exactly `N` hosts (checked at compile time).
const fn joined<const N: usize>(parts: &[&[&'static str]]) -> [&'static str; N] {
    let mut hosts = [""; N];
    let mut part = 0;
    let mut k = 0;
    while part < parts.len() {
        let mut host = 0;
        while host < parts[part].len() {
            hosts[k] = parts[part][host];
            host += 1;
            k += 1;
        }
        part += 1;
    }
    assert!(k == N, "joined host count must match its array length");
    hosts
}

const ALWAYS_HOSTS: [&str; 9] = joined(&[GITHUB_HOSTS, WOWI_HOSTS, TUKUI_HOSTS, WOWACE_HOSTS]);

/// Always allowed: the union of the keyless sources' hosts.
pub const ALWAYS: &[&str] = &ALWAYS_HOSTS;

/// Allowed only while the CurseForge source is enabled (M5).
pub const CURSEFORGE: &[&str] = &[
    "api.curseforge.com",
    "edge.forgecdn.net",
    "mediafilez.forgecdn.net",
];

/// Allowed only while the Wago source is enabled (M5).
pub const WAGO: &[&str] = &["addons.wago.io"];

/// The hosts built into `source`. A `url:` ref may use any always-allowed host; the
/// git binary's hosts are the configured `git_hosts`, which are config, not built in.
pub fn hosts_for(source: SourceId) -> &'static [&'static str] {
    match source {
        SourceId::Github => GITHUB_HOSTS,
        SourceId::Git => GIT_GITHUB_HOSTS,
        SourceId::Url => ALWAYS,
        SourceId::Local => &[],
        SourceId::Wowi => WOWI_HOSTS,
        SourceId::Tukui => TUKUI_HOSTS,
        SourceId::Wowace => WOWACE_HOSTS,
        SourceId::Curseforge => CURSEFORGE,
        SourceId::Wago => WAGO,
    }
}

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
    /// The configured hosts, less any HTTP host ([`is_http_host`]): config validation
    /// refuses those, and a config that bypassed it still never lets git reach one.
    pub fn new(git_hosts: &[String]) -> Self {
        Self {
            hosts: git_hosts
                .iter()
                .filter(|h| !is_http_host(h))
                .map(|h| h.to_ascii_lowercase())
                .collect(),
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
    fn hosts_for_each_source() {
        let cases: [(SourceId, &[&str]); 9] = [
            (
                SourceId::Github,
                &[
                    "api.github.com",
                    "github.com",
                    "codeload.github.com",
                    "objects.githubusercontent.com",
                    "release-assets.githubusercontent.com",
                ],
            ),
            (SourceId::Git, &["github.com", "codeload.github.com"]),
            (
                SourceId::Url,
                &[
                    "api.github.com",
                    "github.com",
                    "codeload.github.com",
                    "objects.githubusercontent.com",
                    "release-assets.githubusercontent.com",
                    "api.mmoui.com",
                    "cdn.wowinterface.com",
                    "api.tukui.org",
                    "repos.wowace.com",
                ],
            ),
            (SourceId::Local, &[]),
            (SourceId::Wowi, &["api.mmoui.com", "cdn.wowinterface.com"]),
            (SourceId::Tukui, &["api.tukui.org"]),
            (
                SourceId::Curseforge,
                &[
                    "api.curseforge.com",
                    "edge.forgecdn.net",
                    "mediafilez.forgecdn.net",
                ],
            ),
            (SourceId::Wago, &["addons.wago.io"]),
            (SourceId::Wowace, &["repos.wowace.com"]),
        ];
        let covered: BTreeSet<_> = cases.iter().map(|(source, _)| *source).collect();
        assert_eq!(
            covered,
            SourceId::ALL.into_iter().collect(),
            "host cases must cover every source: {cases:?}"
        );
        assert_eq!(covered.len(), cases.len(), "duplicate host case: {cases:?}");
        for (source, expected) in cases {
            assert_eq!(hosts_for(source), expected, "{source:?}: built-in hosts");
        }
    }

    #[test]
    fn always_is_the_union_of_the_keyless_sources_hosts() {
        assert_eq!(
            ALWAYS,
            [
                "api.github.com",
                "github.com",
                "codeload.github.com",
                "objects.githubusercontent.com",
                "release-assets.githubusercontent.com",
                "api.mmoui.com",
                "cdn.wowinterface.com",
                "api.tukui.org",
                "repos.wowace.com",
            ],
            "the always-allowed hosts keep their existing order"
        );
        let always: BTreeSet<_> = ALWAYS.iter().copied().collect();
        assert_eq!(
            always.len(),
            ALWAYS.len(),
            "duplicate in ALWAYS: {ALWAYS:?}"
        );
        let keyless: BTreeSet<_> = SourceId::ALL
            .into_iter()
            .filter(|source| !source.requires_credential())
            .flat_map(|source| hosts_for(source).iter().copied())
            .collect();
        assert_eq!(always, keyless, "ALWAYS must be the keyless host union");
    }

    #[test]
    fn every_source_host_is_allowlisted() {
        let off = Allowlist::new(false, false);
        let on = Allowlist::new(true, true);
        let configured: Vec<String> = SourceId::ALL
            .into_iter()
            .flat_map(|source| hosts_for(source).iter().copied())
            .map(str::to_owned)
            .collect();
        let git = GitHosts::new(&configured);
        for source in SourceId::ALL {
            for host in hosts_for(source) {
                assert_eq!(
                    off.permits(host),
                    !source.requires_credential(),
                    "{source:?}: {host} with optional sources disabled"
                );
                assert!(
                    on.permits(host),
                    "{source:?}: {host} with all sources enabled"
                );
                assert!(
                    !git.permits(host),
                    "{source:?}: HTTP host {host} reached git"
                );
            }
        }
    }

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
        // Configured anyway (a config that bypassed validation): never permitted.
        let listed: Vec<String> = ["GitHub.com", "codeberg.org", "api.curseforge.com"]
            .map(str::to_owned)
            .to_vec();
        let git = GitHosts::new(&listed);
        assert!(git.permits("codeberg.org"));
        assert!(!git.permits("github.com") && !git.permits("api.curseforge.com"));
        assert_eq!(
            git.toml_with("git.example.org"),
            "[\"codeberg.org\", \"git.example.org\"]"
        );
    }
}
