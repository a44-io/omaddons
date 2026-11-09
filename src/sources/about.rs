//! What an addon is, as its source describes it: `info`'s `about`
//! (omad-widget-user-first-1n2p.3). A ref that names a project on GitHub, WoWInterface,
//! Tukui or WowAce has a project page, derived from the ref alone (never from a TOC,
//! whose `X-Website` is the author's untrusted text). Its source is asked once for what
//! it says of the project, through the projection `search` uses for the same source:
//!
//! - GitHub (`gh:`, and `git:` on github.com): `/repos/{owner}/{repo}` on the REST API,
//!   read as a search hit (`github_search::decode_repo`): description, owner, stars and
//!   last push. One request, against the release path's budget.
//! - WoWInterface: the file details the release path reads (`wowi::details`): author,
//!   downloads, the file's date. Its description is a BBCode page, often opening with
//!   images and donation links, that no line can stand for, so it gives no summary, as
//!   in `search`.
//! - Tukui: its project list (`TukuiSource::list`, the one this command's fetch decoded
//!   when it asked): `small_desc`, author, last update.
//! - WowAce: its Subversion repository describes no project, so only the page.
//!
//! The answers are kept in the about memo (`state::about_memo`), so a page opened again
//! costs no request.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{SourceId, SourceRegistry, git_github, tukui, wowi};
use crate::error::{Error, Result};
use crate::model::Ref;
use crate::net::Http;
use crate::resolve::search::{Popularity, cap_summary};

/// The project a ref names, on a source with project pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Project<'a> {
    GitHub { owner: &'a str, repo: &'a str },
    Wowi { id: u64 },
    Tukui { slug: &'a str },
    WowAce { slug: &'a str },
}

impl<'a> Project<'a> {
    /// The project `r` names: `None` for a source with no project pages (`url:`,
    /// `file:`, `git:` off GitHub) and for CurseForge and Wago, which need a key.
    pub fn of(r: &'a Ref) -> Option<Self> {
        match r {
            Ref::GitHub { owner, repo } => Some(Self::GitHub { owner, repo }),
            Ref::Git { url, .. } => {
                git_github::repository(url).map(|(owner, repo)| Self::GitHub { owner, repo })
            }
            Ref::Wowi { id } => Some(Self::Wowi { id: *id }),
            Ref::Tukui { slug } => Some(Self::Tukui { slug }),
            Ref::WowAce { slug, .. } => Some(Self::WowAce { slug }),
            Ref::CurseForge { .. } | Ref::Wago { .. } | Ref::Url { .. } | Ref::File { .. } => None,
        }
    }

    /// The source that describes it.
    pub fn source(self) -> SourceId {
        match self {
            Self::GitHub { .. } => SourceId::Github,
            Self::Wowi { .. } => SourceId::Wowi,
            Self::Tukui { .. } => SourceId::Tukui,
            Self::WowAce { .. } => SourceId::Wowace,
        }
    }

    /// Its key in the about memo: one per project, whatever ref or channel named it
    /// (GitHub's owner and repository lowercased, as GitHub compares them).
    pub fn key(self) -> String {
        match self {
            Self::GitHub { owner, repo } => format!(
                "github:{}/{}",
                owner.to_ascii_lowercase(),
                repo.to_ascii_lowercase()
            ),
            Self::Wowi { id } => format!("wowi:{id}"),
            Self::Tukui { slug } => format!("tukui:{}", slug.to_ascii_lowercase()),
            Self::WowAce { slug } => format!("wowace:{}", slug.to_ascii_lowercase()),
        }
    }

    /// Its page on the source's site, built from the ref: on `github.com`,
    /// `www.wowinterface.com`, `tukui.org` or `www.wowace.com`, the hosts the bar
    /// widget opens. Every part comes from a ref whose grammar allows only letters,
    /// digits, `-`, `_` and `.` there; anything else gives `None`.
    pub fn homepage(self) -> Option<String> {
        let plain = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                && s != "."
                && s != ".."
        };
        match self {
            Self::GitHub { owner, repo } if plain(owner) && plain(repo) => {
                Some(format!("https://github.com/{owner}/{repo}"))
            }
            Self::Wowi { id } => Some(format!("https://www.wowinterface.com/downloads/info{id}")),
            Self::Tukui { slug } if plain(slug) => Some(format!("https://tukui.org/{slug}")),
            Self::WowAce { slug } if plain(slug) => {
                Some(format!("https://www.wowace.com/projects/{slug}"))
            }
            Self::GitHub { .. } | Self::Tukui { .. } | Self::WowAce { .. } => None,
        }
    }

    /// Whether its source describes projects at all (WowAce does not).
    pub fn described(self) -> bool {
        !matches!(self, Self::WowAce { .. })
    }
}

/// What a source says of a project, made safe to show; what the about memo keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AboutFacts {
    /// One line about it, at most `SEARCH_SUMMARY_MAX_CHARS`.
    pub summary: Option<String>,
    pub author: Option<String>,
    /// When it last changed, as its source says: GitHub's last push, WoWInterface's
    /// file date, Tukui's last update.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub updated_at: Option<Timestamp>,
    /// WoWInterface's downloads or GitHub's stars.
    pub popularity: Option<Popularity>,
}

/// Asks `project`'s source what it says of it: `Ok(None)` when the source knows no such
/// project (GitHub's 404, an id WoWInterface does not have, a slug Tukui does not list).
/// A source this registry does not have is `source_disabled`; WowAce, which describes
/// nothing, is never asked (`internal`).
pub fn fetch(
    project: Project<'_>,
    registry: &SourceRegistry,
    http: &dyn Http,
) -> Result<Option<AboutFacts>> {
    let subject = || format!("about {}", project.key());
    match project {
        Project::GitHub { owner, repo } => {
            let rest = registry
                .github_rest()
                .ok_or_else(|| registry.unavailable(SourceId::Github, &subject()))?;
            Ok(rest.repository(owner, repo, http)?.map(|hit| AboutFacts {
                summary: hit.description.as_deref().map(cap_summary),
                author: Some(hit.owner).filter(|o| !o.is_empty()),
                updated_at: hit.pushed_at,
                popularity: Some(Popularity::Stars(hit.stars)),
            }))
        }
        Project::Wowi { id } => {
            if registry.get(SourceId::Wowi).is_none() {
                return Err(registry.unavailable(SourceId::Wowi, &subject()));
            }
            Ok(wowi::details(id, http)?.map(|d| AboutFacts {
                summary: None,
                author: Some(d.author).filter(|a| !a.is_empty()),
                updated_at: d
                    .date_ms
                    .and_then(|ms| i64::try_from(ms).ok())
                    .and_then(|ms| Timestamp::from_millisecond(ms).ok()),
                popularity: d.downloads.map(Popularity::Downloads),
            }))
        }
        Project::Tukui { slug } => {
            let source = registry
                .tukui()
                .ok_or_else(|| registry.unavailable(SourceId::Tukui, &subject()))?;
            let list = source.list(http)?;
            Ok(list
                .addons
                .iter()
                .find(|a| a.slug.eq_ignore_ascii_case(slug))
                .map(|a| AboutFacts {
                    summary: Some(a.summary.clone()).filter(|s| !s.is_empty()),
                    author: Some(a.author.clone()).filter(|s| !s.is_empty()),
                    updated_at: tukui::updated_at(a),
                    popularity: None,
                }))
        }
        Project::WowAce { .. } => Err(Error::internal(format!(
            "{} has no project metadata to ask for",
            subject()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(r: &str) -> Option<(String, Option<String>)> {
        let r: Ref = r.parse().unwrap();
        Project::of(&r).map(|p| (p.key(), p.homepage()))
    }

    #[test]
    fn a_project_page_comes_from_the_ref_alone() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            (
                "gh:WeakAuras/WeakAuras2",
                Some((
                    "github:weakauras/weakauras2",
                    "https://github.com/WeakAuras/WeakAuras2",
                )),
            ),
            (
                "gh:tullamods/OmniCC",
                Some((
                    "github:tullamods/omnicc",
                    "https://github.com/tullamods/OmniCC",
                )),
            ),
            (
                "git:https://github.com/shagu/pfUI#master",
                Some(("github:shagu/pfui", "https://github.com/shagu/pfUI")),
            ),
            (
                "git:https://github.com/fondlez/InFlight-Taxi-Timer.git",
                Some((
                    "github:fondlez/inflight-taxi-timer",
                    "https://github.com/fondlez/InFlight-Taxi-Timer",
                )),
            ),
            (
                "wowi:7032",
                Some((
                    "wowi:7032",
                    "https://www.wowinterface.com/downloads/info7032",
                )),
            ),
            (
                "tukui:elvui",
                Some(("tukui:elvui", "https://tukui.org/elvui")),
            ),
            (
                "wowace:bug-grabber#r152",
                Some((
                    "wowace:bug-grabber",
                    "https://www.wowace.com/projects/bug-grabber",
                )),
            ),
            (
                "url:https://github.com/o/r/releases/download/v1/x.zip",
                None,
            ),
            ("file:/tmp/Foo.zip", None),
            ("cf:61284", None),
        ];
        for (input, want) in cases {
            let want = want.map(|(k, h)| (k.to_owned(), Some(h.to_owned())));
            assert_eq!(project(input), want, "{input}");
        }
    }

    /// A part a ref would never hold still gives no page, never a URL with it.
    #[test]
    fn a_part_outside_the_grammar_gives_no_page() {
        for p in [
            Project::GitHub {
                owner: "o",
                repo: "r/../x",
            },
            Project::GitHub {
                owner: "o@evil.example",
                repo: "r",
            },
            Project::GitHub {
                owner: "o",
                repo: "..",
            },
            Project::Tukui { slug: "elv ui" },
            Project::WowAce { slug: "" },
        ] {
            assert_eq!(p.homepage(), None, "{p:?}");
        }
    }

    #[test]
    fn only_wowace_describes_nothing() {
        assert!(!Project::WowAce { slug: "x" }.described());
        assert!(Project::Wowi { id: 1 }.described());
        assert_eq!(Project::Tukui { slug: "x" }.source(), SourceId::Tukui);
    }
}
