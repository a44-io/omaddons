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
    stargazers_count: u64,
    pushed_at: Option<String>,
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
        }
    }

    /// The search budget as last reported.
    pub fn budget(&self) -> Budget {
        self.budget.lock().map_or_else(|p| *p.into_inner(), |b| *b)
    }

    /// The request for `query`: addon-ish repositories (`wow`, `warcraft` or `addon` in
    /// the name, description or topics), by stars.
    pub fn request(&self, query: &str) -> Request {
        let q = format!("{query} (wow OR warcraft OR addon) in:name,description,topics");
        let mut req = Request::get(
            format!(
                "https://api.github.com/search/repositories?q={}&sort=stars&per_page={GITHUB_SEARCH_PER_PAGE}",
                query_component(&q)
            ),
            "github-search",
        )
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
        if let Ok(Some(token)) = self.token.value() {
            // ubs:ignore — an outbound request header; github_token::valid rejects controls and spaces
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req
    }

    /// The repositories GitHub finds for `query`, best first. A spent search budget, or a
    /// 403 or 429 with no requests left, is `rate_limited` with its reset time.
    pub fn search(&self, query: &str, http: &dyn Http) -> Result<Vec<RepoHit>> {
        // A configured credential that cannot be resolved stops the search.
        let token = self.token.value()?.is_some();
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
        Ok(page
            .items
            .into_iter()
            .filter_map(|item| {
                let r#ref: Ref = format!("gh:{}/{}", item.owner.login, item.name)
                    .parse()
                    .ok()?;
                Some(RepoHit {
                    r#ref,
                    name: item.name,
                    owner: item.owner.login,
                    description: item.description.filter(|d| !d.trim().is_empty()),
                    stars: item.stargazers_count,
                    pushed_at: item.pushed_at.and_then(|t| t.parse().ok()),
                })
            })
            .collect())
    }
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
}
