//! GitHub's repository search for `search` (M2 §5.4): one request per run, the only
//! request that carries the query (M2-D21). GitHub counts search on its own rate limit
//! (`x-ratelimit-resource: search`, 10 requests a minute unauthenticated, 30 with a
//! token), so it keeps a budget of its own, apart from [`super::github_rest`]'s.

use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use serde::Deserialize;

use super::github_rest::Budget;
use super::github_token::{GithubToken, TokenSource};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::Ref;
use crate::net::{Http, Request};
use crate::politeness::GITHUB_SEARCH_PER_PAGE;

/// One repository found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoHit {
    pub r#ref: Ref,
    pub name: String,
    pub owner: String,
    pub description: Option<String>,
    pub stars: u64,
    pub pushed_at: Option<Timestamp>,
}

/// The search client; its budget is the search resource's, never the core API's.
pub struct GithubSearch {
    token: Arc<GithubToken>,
    budget: Mutex<Budget>,
    /// `--offline`: a search is refused before the token is resolved.
    offline: bool,
}

impl std::fmt::Debug for GithubSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubSearch")
            .field("token", &self.token.source())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Page {
    items: Vec<Item>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Item {
    name: String,
    owner: Owner,
    description: Option<String>,
    topics: Vec<String>,
    stargazers_count: u64,
    pushed_at: Option<String>,
}

/// What marks a repository as WoW's, anywhere in its name, description or a topic
/// (ignoring case); `wow` also does at the start of a word ([`wow_evidence`]).
const WOW_MARKERS: &[&str] = &["warcraft", "azeroth", "addon"];

/// Whether any of `texts` says WoW: a [`WOW_MARKERS`] word anywhere, or `wow` at the
/// start or after a character that is not a letter or digit (`wow-addon`,
/// `world-of-warcraft`), so `slowwork` says nothing.
fn wow_evidence(texts: &[&str]) -> bool {
    texts.iter().any(|text| {
        let text = text.to_ascii_lowercase();
        WOW_MARKERS.iter().any(|m| text.contains(m))
            || text.match_indices("wow").any(|(at, _)| {
                text[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_ascii_alphanumeric())
            })
    })
}

/// What a search found: the repositories that say WoW, and how many it dropped for
/// saying nothing of it (GitHub does not reliably honour the query's group).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub repos: Vec<RepoHit>,
    pub dropped: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Owner {
    login: String,
}

impl GithubSearch {
    /// With `token` as if from the environment (tests).
    pub fn with_token(token: Option<String>) -> Self {
        let source = if token.is_some() {
            TokenSource::Env
        } else {
            TokenSource::None
        };
        Self::with_github_token(GithubToken::fixed(source, token))
    }

    /// With the command's resolved token.
    pub fn with_github_token(token: Arc<GithubToken>) -> Self {
        Self {
            token,
            budget: Mutex::new(Budget::default()),
            offline: false,
        }
    }

    /// For a command run `--offline` (or not).
    #[must_use]
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// The search budget as last reported.
    pub fn budget(&self) -> Budget {
        self.budget.lock().map_or_else(|p| *p.into_inner(), |b| *b)
    }

    /// The request for `query`: addon-ish repositories (`wow`, `warcraft` or `addon` in
    /// the name, description or topics), by stars.
    pub fn request(&self, query: &str) -> Request {
        let mut req = Request::get(request_url(query), "github-search")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Ok(Some(token)) = self.token.value() {
            // ubs:ignore — an outbound request header; github_token::valid rejects controls and spaces
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req
    }

    /// The repositories GitHub finds for `query`, best first, kept only when their name,
    /// description or topics say WoW ([`wow_evidence`]). A spent search budget, or a 403
    /// or 429 with no requests left, is `rate_limited` with its reset time.
    pub fn search(&self, query: &str, http: &dyn Http) -> Result<Found> {
        // Offline, nothing is resolved: a credential command never runs.
        if self.offline {
            return Err(crate::net::offline_refusal(&request_url(query)));
        }
        let budget = self.budget();
        if budget.remaining == Some(0) {
            let reset = budget.reset.unwrap_or_else(Timestamp::now);
            return Err(Error::rate_limited(
                reset,
                format!("GitHub's search allows no more requests before {reset}"),
                Suggestion::text(format!(
                    "wait until {reset}, or store a GitHub token with omaddons auth github set for 30 searches a minute"
                )),
            ));
        }
        // A configured credential that cannot be resolved stops the search.
        let token = self.token.value()?.is_some();
        let resp = match http.send(self.request(query)) {
            Ok(resp) => resp,
            // The retry layer answered a spent search for us: remember it is spent.
            Err(error) if matches!(error.code, ErrorCode::RateLimited) => {
                if let Ok(mut b) = self.budget.lock() {
                    b.remaining = Some(0);
                    b.reset = error.details["reset_at"]
                        .as_str()
                        .and_then(|t| t.parse().ok());
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if let Ok(mut b) = self.budget.lock() {
            b.update(&resp);
        }
        if resp.status == 401 && token {
            return Err(self.token.rejected());
        }
        let body = resp.error_for_status("GitHub's search API")?.bytes()?;
        let page: Page = serde_json::from_slice(&body).map_err(|e| {
            Error::new(
                ErrorCode::Network,
                format!("GitHub's search API answered something unreadable ({e})"),
                Suggestion::text("retry later; if it persists, GitHub changed its API"),
            )
            .retryable(true)
        })?;
        // Only what says WoW: GitHub returns database tools for `dbm`.
        let total = page.items.len();
        let says_wow: Vec<Item> = page
            .items
            .into_iter()
            .filter(|item| {
                let mut texts = vec![item.name.as_str()];
                texts.extend(item.description.as_deref());
                texts.extend(item.topics.iter().map(String::as_str));
                wow_evidence(&texts)
            })
            .collect();
        let dropped = total - says_wow.len();
        let repos = says_wow.into_iter().filter_map(repo_hit).collect();
        Ok(Found { repos, dropped })
    }
}

/// One repository object, as a search page's items and `/repos/{owner}/{repo}` both
/// send it: `None` when its owner and name make no `gh:` ref. Its text is untrusted and
/// made safe at once (names are restricted already; this makes it hold whatever GitHub
/// sends).
fn repo_hit(item: Item) -> Option<RepoHit> {
    let r#ref: Ref = format!("gh:{}/{}", item.owner.login, item.name)
        .parse()
        .ok()?;
    let safe = |text: &str| {
        crate::text::source_line(
            text,
            crate::limits::SOURCE_TEXT_MAX_CHARS,
            crate::text::Entities::Keep,
        )
    };
    Some(RepoHit {
        r#ref,
        name: safe(&item.name),
        owner: safe(&item.owner.login),
        description: item.description.map(|d| safe(&d)).filter(|d| !d.is_empty()),
        stars: item.stargazers_count,
        pushed_at: item.pushed_at.and_then(|t| t.parse().ok()),
    })
}

/// The repository `/repos/{owner}/{repo}` describes, read as a search hit is
/// ([`repo_hit`]): `info`'s `about` and `search` share one projection. A body that is not
/// a repository object is a retryable `network` error.
pub fn decode_repo(body: &[u8]) -> Result<RepoHit> {
    let unreadable = |why: String| {
        Error::new(
            ErrorCode::Network,
            format!("GitHub's API answered something that is not a repository ({why})"),
            Suggestion::text("retry later; if it persists, GitHub changed its API"),
        )
        .retryable(true)
    };
    let item: Item = serde_json::from_slice(body).map_err(|e| unreadable(e.to_string()))?;
    repo_hit(item).ok_or_else(|| unreadable("no owner and name".into()))
}

/// The search URL for `query`: addon-ish repositories (`wow`, `warcraft` or `addon` in
/// the name, description or topics), by stars.
fn request_url(query: &str) -> String {
    let q = format!("{query} (wow OR warcraft OR addon) in:name,description,topics");
    format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&per_page={GITHUB_SEARCH_PER_PAGE}",
        query_component(&q)
    )
}

/// `s` as a URL query value: unreserved characters kept, spaces as `+`, the rest
/// percent-encoded.
fn query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_encoded_with_the_addon_qualifier() {
        let req = GithubSearch::with_token(None).request("damage meter");
        assert_eq!(
            req.url,
            "https://api.github.com/search/repositories?q=damage+meter+%28wow+OR+warcraft+OR+addon%29+in%3Aname%2Cdescription%2Ctopics&sort=stars&per_page=20"
        );
        assert_eq!(req.purpose, "github-search");
        // ubs:ignore — a header name, not its value
        assert!(req.headers.iter().all(|(k, _)| k != "Authorization"));
        let with = GithubSearch::with_token(Some("t".into())).request("x");
        assert!(
            with.headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer t")
        );
        assert_eq!(query_component("é&/"), "%C3%A9%26%2F");
    }

    /// Answers every request with one page of `items`.
    struct OnePage(String);

    impl Http for OnePage {
        fn send(&self, req: Request) -> Result<crate::net::Response> {
            Ok(crate::net::Response::new(
                200,
                Vec::new(),
                req.url,
                Box::new(std::io::Cursor::new(self.0.clone().into_bytes())),
            ))
        }
    }

    /// `/repos/{owner}/{repo}`'s object reads as a search item does, made safe; a body
    /// that is no repository is a retryable `network` error.
    #[test]
    fn one_repository_reads_as_a_search_hit() {
        let body = serde_json::json!({
            "name": "WeakAuras2",
            "owner": {"login": "WeakAuras"},
            "description": "Auras\u{202e} for\u{7} WoW",
            "stargazers_count": 1234,
            "pushed_at": "2026-10-28T09:30:00Z",
            "archived": false,
        });
        let hit = decode_repo(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            hit,
            RepoHit {
                r#ref: "gh:WeakAuras/WeakAuras2".parse().unwrap(),
                name: "WeakAuras2".into(),
                owner: "WeakAuras".into(),
                description: Some("Auras for WoW".into()),
                stars: 1234,
                pushed_at: Some("2026-10-28T09:30:00Z".parse().unwrap()),
            }
        );
        for bad in [
            &b"[]"[..],
            b"{}",
            b"<html>",
            br#"{"name":"x","owner":{"login":"a b"}}"#,
        ] {
            let error = decode_repo(bad).unwrap_err();
            assert_eq!(
                (error.code, error.retryable),
                (ErrorCode::Network, true),
                "{bad:?}"
            );
        }
    }

    /// GitHub answers `dbm` with database tools: only what says WoW is kept, and the
    /// rest is counted.
    #[test]
    fn only_repositories_that_say_wow_are_kept() {
        let item = |owner: &str, name: &str, description: &str, topics: &[&str]| {
            serde_json::json!({
                "name": name,
                "owner": {"login": owner},
                "description": description,
                "topics": topics,
                "stargazers_count": 1,
            })
        };
        let page = serde_json::json!({"items": [
            item("amacneil", "dbmate", "A lightweight, framework-agnostic database migration tool.", &["database"]),
            item("holistics", "dbml", "Database Markup Language", &[]),
            item("o", "Raids", "Timers", &["world-of-warcraft"]),
            item("o", "Bars", "A WoW addon for bars", &[]),
            item("o", "slowwork", "Background jobs", &["worker"]),
            item("DeadlyBossMods", "DeadlyBossMods", "Raid encounter alerts for World of Warcraft", &[]),
        ]});
        let http = OnePage(page.to_string());
        let found = GithubSearch::with_token(None).search("dbm", &http).unwrap();
        let names: Vec<&str> = found.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Raids", "Bars", "DeadlyBossMods"]);
        assert_eq!(found.dropped, 3);
        // `wow` counts at a word's start only.
        assert!(wow_evidence(&["wow-addon"]) && wow_evidence(&["Shows WoW stats"]));
        assert!(!wow_evidence(&["slowwork"]) && !wow_evidence(&["knowwow"]));
        assert!(wow_evidence(&["Azeroth maps"]));
    }
}
