//! Release notes ("explain pending updates with changelogs", M2): the types, and the one
//! helper `info --changelog` and `outdated --changelog` share, so they cannot diverge. A
//! changelog is opt-in decoration: one that cannot be had is a reason in the output,
//! never a warning, and never makes a run partial.
//!
//! Where notes come from: a release's own `notes` (WoWInterface, no request); else the
//! source's [`super::Source::changelog`] (GitHub's `releases.atom`, git on GitHub's
//! commits feed, Tukui's changelog URL); else nothing (`not_supported`). Nothing here
//! calls GitHub's REST API.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::{SourceId, SourceRegistry};
use crate::error::ErrorCode;
use crate::limits::{CHANGELOG_MAX_CHARS, CHANGELOG_MAX_ENTRIES};
use crate::model::{Release, ReleaseSet};
use crate::net::Http;

/// The notes of the releases newer than the installed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Changelog {
    pub source: SourceId,
    /// Newest first.
    pub entries: Vec<ChangelogEntry>,
    /// Newer releases exist that are not here: cut at `CHANGELOG_MAX_ENTRIES`, or older
    /// than the source's history reaches.
    pub truncated: bool,
    /// Where the rest is (a project's releases page).
    pub more_url: Option<String>,
}

/// One release's notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ChangelogEntry {
    pub version: String,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub published_at: Option<Timestamp>,
    /// Plain text, at most `CHANGELOG_MAX_CHARS`.
    pub text: String,
}

/// Why there is no changelog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangelogUnavailable {
    /// The source has none (`url:`, `file:`, `git:` off GitHub).
    NotSupported,
    Offline,
    /// The source could not be read now; the update status itself is unaffected.
    FetchFailed,
    /// No release to explain.
    NoReleases,
}

/// What [`changelog_for`] found: a changelog, or why not (and, for a fetch that failed,
/// what failed, for `-v`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notes {
    pub changelog: Option<Changelog>,
    pub unavailable: Option<ChangelogUnavailable>,
    pub diagnostic: Option<String>,
    /// The source was asked over the network (for progress).
    pub asked: bool,
}

impl Notes {
    fn unavailable(reason: ChangelogUnavailable) -> Self {
        Self {
            changelog: None,
            unavailable: Some(reason),
            diagnostic: None,
            asked: false,
        }
    }

    fn found(changelog: Changelog, asked: bool) -> Self {
        Self {
            changelog: Some(changelog),
            unavailable: None,
            diagnostic: None,
            asked,
        }
    }
}

/// The identity a `release_id` ends with: what follows its last `@` (a GitHub tag, a git
/// sha, a Tukui version).
pub fn identity(release_id: &str) -> &str {
    release_id.rsplit_once('@').map_or(release_id, |(_, id)| id)
}

/// Whether two versions or tags name one release, ignoring case and a leading `v`; a
/// commit sha matches its abbreviation.
pub fn same_version(a: &str, b: &str) -> bool {
    let bare = |s: &str| {
        let s = s.trim();
        s.strip_prefix(['v', 'V']).unwrap_or(s).to_ascii_lowercase()
    };
    let (a, b) = (bare(a), bare(b));
    let sha = |s: &str| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit());
    a == b || (sha(&a) && sha(&b) && (a.starts_with(&b) || b.starts_with(&a)))
}

/// The notes of `set`'s releases newer than `since` (an installed release's identity;
/// `None`: the latest only), newest first, at most `CHANGELOG_MAX_ENTRIES`, never above
/// `latest` (a stable record's feed may list newer prereleases). `since` equal to
/// `latest` is an empty changelog, with no request.
pub fn changelog_for(
    registry: &SourceRegistry,
    set: &ReleaseSet,
    latest: &Release,
    since: Option<&str>,
    http: &dyn Http,
) -> Notes {
    let source = SourceId::of(&set.r#ref);
    let latest_id = identity(&latest.release_id);
    let is_latest = |v: &str| same_version(v, &latest.version) || same_version(v, latest_id);
    let limit = if since.is_some() {
        CHANGELOG_MAX_ENTRIES
    } else {
        1
    };
    let local = |entries: Vec<ChangelogEntry>| {
        Notes::found(
            Changelog {
                source,
                entries,
                truncated: false,
                more_url: latest.changelog_url.clone(),
            },
            false,
        )
    };
    if since.is_some_and(is_latest) {
        return local(Vec::new());
    }
    // The release's own notes (WoWInterface's current file): no request.
    if let Some(notes) = &latest.notes {
        return local(vec![ChangelogEntry {
            version: latest.version.clone(),
            published_at: latest.published_at,
            text: notes.clone(),
        }]);
    }
    let Some(found) = registry.get(source) else {
        return Notes::unavailable(ChangelogUnavailable::NotSupported);
    };
    match found.changelog(&set.r#ref, latest, since, CHANGELOG_MAX_ENTRIES, http) {
        None => Notes::unavailable(ChangelogUnavailable::NotSupported),
        Some(Err(error)) if matches!(error.code, ErrorCode::Offline) => {
            Notes::unavailable(ChangelogUnavailable::Offline)
        }
        Some(Err(error)) => Notes {
            diagnostic: Some(format!("{}: the changelog: {}", set.r#ref, error.message)),
            asked: true,
            ..Notes::unavailable(ChangelogUnavailable::FetchFailed)
        },
        Some(Ok(mut changelog)) => {
            match changelog.entries.iter().position(|e| is_latest(&e.version)) {
                Some(at) => {
                    changelog.entries.drain(..at);
                }
                // `latest` is not in the feed (a stable record's feed full of newer
                // prereleases): what is dated after it goes; with no date to tell, no
                // entry is known to be at most `latest`.
                None => match latest.published_at {
                    Some(at) => changelog
                        .entries
                        .retain(|e| e.published_at.is_some_and(|p| p <= at)),
                    None => {
                        changelog.truncated = !changelog.entries.is_empty();
                        changelog.entries.clear();
                    }
                },
            }
            if changelog.entries.len() > limit {
                changelog.entries.truncate(limit);
                changelog.truncated = since.is_some();
            }
            if since.is_none() {
                changelog.truncated = false;
            }
            if changelog.entries.is_empty() && since.is_none() {
                return Notes {
                    asked: true,
                    ..Notes::unavailable(ChangelogUnavailable::NoReleases)
                };
            }
            Notes::found(changelog, true)
        }
    }
}

/// One installed release, for [`oldest_since`].
#[derive(Debug, Clone, Copy)]
pub struct Installed<'a> {
    pub release_id: &'a str,
    pub version: &'a str,
    pub installed_at: Timestamp,
}

/// The identity of the oldest of `installed`, in B4's downgrade-guard order: the
/// `published_at` `set` knows, then dotted-numeric versions, then `installed_at`. `None`
/// when there is none (records without a release take no part).
pub fn oldest_since(installed: &[Installed<'_>], set: &ReleaseSet) -> Option<String> {
    let published = |id: &str| {
        set.releases
            .iter()
            .find(|r| r.release_id == id)
            .and_then(|r| r.published_at)
    };
    installed
        .iter()
        .min_by(|a, b| {
            match (published(a.release_id), published(b.release_id)) {
                (Some(x), Some(y)) if x != y => return x.cmp(&y),
                _ => {}
            }
            crate::resolve::updates::compare_versions(a.version, b.version)
                .filter(|o| o.is_ne())
                .unwrap_or_else(|| a.installed_at.cmp(&b.installed_at))
        })
        .map(|i| identity(i.release_id).to_owned())
}

/// `changelog` cut to the entries newer than `since` (`None`: unchanged), for an item
/// whose installed release is newer than the one the changelog was fetched for.
pub fn newer_than(changelog: &Changelog, since: Option<&str>) -> Changelog {
    let Some(since) = since else {
        return changelog.clone();
    };
    match changelog
        .entries
        .iter()
        .position(|e| same_version(&e.version, since))
    {
        Some(at) => Changelog {
            entries: changelog.entries[..at].to_vec(),
            truncated: false,
            ..changelog.clone()
        },
        None => changelog.clone(),
    }
}

/// A changelog (or why there is none) as text lines, for `info` and `outdated` to
/// indent: each release as `5.22.1 (2026-10-12)` with its notes under it, and where the
/// rest is when it was cut.
pub fn human_lines(
    changelog: Option<&Changelog>,
    unavailable: Option<ChangelogUnavailable>,
) -> Vec<String> {
    let Some(c) = changelog else {
        let why = match unavailable {
            Some(ChangelogUnavailable::NotSupported) | None => "its source has none",
            Some(ChangelogUnavailable::Offline) => "offline",
            Some(ChangelogUnavailable::FetchFailed) => "it could not be fetched",
            Some(ChangelogUnavailable::NoReleases) => "no release",
        };
        return vec![format!("no changelog ({why})")];
    };
    let mut lines = Vec::new();
    if c.entries.is_empty() {
        lines.push("no newer release notes".to_owned());
    }
    for e in &c.entries {
        lines.push(match e.published_at {
            Some(at) => format!("{} ({})", e.version, at.strftime("%Y-%m-%d")),
            None => e.version.clone(),
        });
        lines.extend(e.text.lines().map(|l| format!("  {l}")));
    }
    if let (true, Some(url)) = (c.truncated, &c.more_url) {
        let place = match c.source {
            SourceId::Github | SourceId::Git => "on GitHub",
            SourceId::Tukui => "on Tukui",
            SourceId::Wowi => "on WoWInterface",
            _ => "at",
        };
        lines.push(format!("older releases {place}: {url}"));
    }
    lines
}

/// Notes as plain text: HTML when it has tags, else text with CRLF made LF; capped.
pub fn notes_text(body: &str) -> String {
    let looks_html = body.contains('<') && body.contains('>');
    if looks_html {
        crate::text::html_to_text(body, CHANGELOG_MAX_CHARS)
    } else {
        let text = body.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim();
        if text.chars().count() <= CHANGELOG_MAX_CHARS {
            text.to_owned()
        } else {
            let kept: String = text.chars().take(CHANGELOG_MAX_CHARS - 1).collect();
            format!("{}…", kept.trim_end())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, Result, Suggestion};
    use crate::model::{Channel, Ref};
    use crate::sources::{Source, Wanted};

    /// A GitHub source whose changelog is canned: entries by version, or an error.
    struct Fake(std::result::Result<Vec<&'static str>, ErrorCode>);

    impl Source for Fake {
        fn id(&self) -> SourceId {
            SourceId::Github
        }

        fn fetch(&self, _: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
            Vec::new()
        }

        fn changelog(
            &self,
            _: &Ref,
            _: &Release,
            _: Option<&str>,
            limit: usize,
            _: &dyn Http,
        ) -> Option<Result<Changelog>> {
            Some(match &self.0 {
                Ok(versions) => Ok(Changelog {
                    source: SourceId::Github,
                    entries: versions
                        .iter()
                        .take(limit)
                        .map(|v| ChangelogEntry {
                            version: (*v).to_owned(),
                            published_at: None,
                            text: format!("notes of {v}"),
                        })
                        .collect(),
                    truncated: false,
                    more_url: Some("https://github.com/o/a/releases".into()),
                }),
                Err(code) => Err(Error::new(*code, "nope", Suggestion::text("x"))),
            })
        }
    }

    struct NoHttp;

    impl Http for NoHttp {
        fn send(&self, _: crate::net::Request) -> Result<crate::net::Response> {
            Err(Error::internal("no request expected"))
        }
    }

    fn release(version: &str, notes: Option<&str>) -> Release {
        Release {
            release_id: format!("gh:o/a@{version}"),
            version: version.to_owned(),
            channel: Channel::Stable,
            published_at: None,
            changelog_url: None,
            notes: notes.map(str::to_owned),
            assets: Vec::new(),
            name_hint: None,
            revision: None,
        }
    }

    fn run(fake: Fake, latest: &Release, since: Option<&str>) -> Notes {
        let mut registry = SourceRegistry::new();
        registry.register(Box::new(fake));
        let set = ReleaseSet {
            r#ref: "gh:o/a".parse().unwrap(),
            releases: vec![latest.clone()],
        };
        changelog_for(&registry, &set, latest, since, &NoHttp)
    }

    fn versions(notes: &Notes) -> Vec<&str> {
        notes
            .changelog
            .iter()
            .flat_map(|c| &c.entries)
            .map(|e| e.version.as_str())
            .collect()
    }

    #[test]
    fn the_source_gives_the_releases_newer_than_since_never_above_latest() {
        // A prerelease above the stable latest is skipped.
        let feed = Fake(Ok(vec!["v4.0-beta", "v3.0", "v2.0", "v1.0"]));
        let notes = run(feed, &release("3.0", None), Some("1.0"));
        assert_eq!(
            versions(&notes),
            ["v3.0", "v2.0", "v1.0"],
            "the fake ignores since"
        );
        // Without an installed release: the latest's notes alone.
        let notes = run(Fake(Ok(vec!["v3.0", "v2.0"])), &release("v3.0", None), None);
        assert_eq!(versions(&notes), ["v3.0"]);
        assert!(!notes.changelog.unwrap().truncated);
        // Since is the latest: nothing to say, and no call at all.
        let notes = run(
            Fake(Err(ErrorCode::Network)),
            &release("3.0", None),
            Some("v3.0"),
        );
        assert_eq!(versions(&notes), Vec::<&str>::new());
        assert!(notes.unavailable.is_none());
        // The latest beyond the feed's window (newer prereleases fill it): no entry is
        // known to be at most it, so no prerelease is shown as its changelog.
        let betas = Fake(Ok(vec!["v4.0-beta3", "v4.0-beta2", "v4.0-beta1"]));
        let notes = run(betas, &release("3.0", None), Some("1.0"));
        assert_eq!(versions(&notes), Vec::<&str>::new());
        assert!(
            notes.changelog.is_some_and(|c| c.truncated),
            "the page links the rest"
        );
        let notes = run(Fake(Ok(vec!["v4.0-beta1"])), &release("3.0", None), None);
        assert!(versions(&notes).is_empty());
    }

    #[test]
    fn a_releases_own_notes_need_no_request() {
        let notes = run(
            Fake(Err(ErrorCode::Network)),
            &release("2.0", Some("fixed things")),
            Some("1.0"),
        );
        let c = notes.changelog.unwrap();
        assert_eq!(
            (c.entries.len(), c.entries[0].text.as_str()),
            (1, "fixed things")
        );
    }

    #[test]
    fn errors_map_to_reasons_and_never_warn() {
        let reason = |code| run(Fake(Err(code)), &release("2.0", None), Some("1.0"));
        let offline = reason(ErrorCode::Offline);
        assert_eq!(offline.unavailable, Some(ChangelogUnavailable::Offline));
        let failed = reason(ErrorCode::Network);
        assert_eq!(failed.unavailable, Some(ChangelogUnavailable::FetchFailed));
        assert!(failed.diagnostic.is_some_and(|d| d.contains("nope")));
        // A source without changelogs.
        let registry = SourceRegistry::new();
        let set = ReleaseSet {
            r#ref: "url:https://x.org/a.zip".parse().unwrap(),
            releases: Vec::new(),
        };
        let none = changelog_for(&registry, &set, &release("1", None), None, &NoHttp);
        assert_eq!(none.unavailable, Some(ChangelogUnavailable::NotSupported));
    }

    #[test]
    fn versions_match_with_or_without_v_and_shas_by_prefix() {
        assert!(same_version("v1.2.3", "1.2.3") && same_version("V2", "v2"));
        assert!(!same_version("1.2.3", "1.2.30"));
        assert!(same_version(
            "abc1234",
            "abc1234def5678900000000000000000000000000"
        ));
        assert!(!same_version("abc", "abcdef0"), "too short to be a sha");
        assert_eq!(identity("gh:o/a@v1.0"), "v1.0");
        assert_eq!(identity("git:https://x/y@abc@def"), "def");
    }

    #[test]
    fn the_oldest_installed_release_comes_by_date_then_version_then_install_time() {
        let at = |s: &str| -> Timestamp { s.parse().unwrap() };
        let mut set = ReleaseSet {
            r#ref: "gh:o/a".parse().unwrap(),
            releases: vec![release("1.10", None), release("1.9", None)],
        };
        let installed = |id: &'static str, version: &'static str, when: &str| Installed {
            release_id: id,
            version,
            installed_at: at(when),
        };
        let both = [
            installed("gh:o/a@1.10", "1.10", "2026-01-01T00:00:00Z"),
            installed("gh:o/a@1.9", "1.9", "2026-02-01T00:00:00Z"),
        ];
        // No dates: numerically, 1.9 < 1.10.
        assert_eq!(oldest_since(&both, &set).as_deref(), Some("1.9"));
        // Dates win.
        set.releases[0].published_at = Some(at("2025-01-01T00:00:00Z"));
        set.releases[1].published_at = Some(at("2025-06-01T00:00:00Z"));
        assert_eq!(oldest_since(&both, &set).as_deref(), Some("1.10"));
        assert_eq!(oldest_since(&[], &set), None);
    }

    #[test]
    fn an_item_keeps_the_entries_newer_than_its_own_release() {
        let c = Changelog {
            source: SourceId::Github,
            entries: ["v3", "v2", "v1"]
                .iter()
                .map(|v| ChangelogEntry {
                    version: (*v).into(),
                    published_at: None,
                    text: String::new(),
                })
                .collect(),
            truncated: true,
            more_url: None,
        };
        let mine = newer_than(&c, Some("2"));
        let kept: Vec<&str> = mine.entries.iter().map(|e| e.version.as_str()).collect();
        assert_eq!((kept, mine.truncated), (vec!["v3"], false));
        assert_eq!(
            newer_than(&c, Some("0.9")),
            c,
            "older than the changelog: as it is"
        );
    }

    #[test]
    fn notes_become_text() {
        let html = notes_text("<ul><li>alpha</li><li>beta</li></ul>");
        assert!(html.contains("alpha") && html.contains("beta") && !html.contains('<'));
        assert_eq!(notes_text("line one\r\nline two\r\n"), "line one\nline two");
        let long = "x".repeat(CHANGELOG_MAX_CHARS + 10);
        assert_eq!(notes_text(&long).chars().count(), CHANGELOG_MAX_CHARS);
    }
}
