//! The GitHub REST API path behind [`GithubRestFallback`] (vision §5.4): the beta and alpha
//! channels, which need a release's `prerelease` flag, and releases without a usable
//! `release.json`, whose zip assets only the API lists.
//!
//! - `beta` takes the newest non-draft release, prerelease or not; GitHub has no separate
//!   alpha, so `alpha` is the same. `stable` would take the newest non-prerelease.
//! - A chosen release with a `release.json` asset is read from github.com like the web
//!   path. Otherwise its `.zip` assets are candidates, annotated with flavor hints from
//!   their names; TOC verification decides compatibility.
//! - Unauthenticated REST allows 60 requests an hour, so a run stops sending once
//!   `X-RateLimit-Remaining` is at or below [`GITHUB_REST_RESERVE`] and returns
//!   `rate_limited` with the reset time. The first request of a run is always sent.
//! - `OMADDONS_GITHUB_TOKEN`, when set, is sent as a bearer token to the API host only;
//!   `GITHUB_TOKEN` is never read (D12). The net layer logs no headers.

use std::sync::Mutex;

use jiff::Timestamp;
use serde::Deserialize;

use super::Wanted;
use super::github::{GithubRestFallback, RestReason, encode_segment, release_set};
use super::release_json;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{
    Asset, AssetLocation, Channel, ExpectedHash, FlavorHint, Ref, Release, ReleaseSet,
};
use crate::net::{Http, Request, Response};
use crate::politeness::GITHUB_REST_RESERVE;

const API: &str = "https://api.github.com";
/// The only variable a token is read from.
// ubs:ignore — the variable's name, not a secret value
pub const TOKEN_VAR: &str = "OMADDONS_GITHUB_TOKEN";
/// Releases listed per request; a stable release older than this many prereleases is
/// not found.
const PER_PAGE: u32 = 20;

/// The REST fallback; one per run, so its rate-limit budget is shared by every fetch.
pub struct GithubRest {
    token: Option<String>,
    budget: Mutex<Budget>,
}

impl std::fmt::Debug for GithubRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubRest")
            .field("token", &self.token.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

/// The last rate-limit state the API reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Budget {
    pub remaining: Option<u32>,
    pub reset: Option<Timestamp>,
}

impl Budget {
    /// Whether another request may be sent; unknown means yes.
    pub fn allows(&self) -> bool {
        self.remaining.is_none_or(|n| n > GITHUB_REST_RESERVE)
    }

    /// Takes `X-RateLimit-Remaining` and `X-RateLimit-Reset` (epoch seconds) when present.
    pub fn update(&mut self, resp: &Response) {
        if let Some(n) = resp
            .header("x-ratelimit-remaining")
            .and_then(|v| v.trim().parse().ok())
        {
            self.remaining = Some(n);
        }
        if let Some(t) = resp
            .header("x-ratelimit-reset")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .and_then(|s| Timestamp::from_second(s).ok())
        {
            self.reset = Some(t);
        }
    }
}

impl GithubRest {
    /// With the token from the process environment, if any.
    pub fn from_env() -> Self {
        Self::with_token(token_from(|name| std::env::var(name).ok()))
    }

    pub fn with_token(token: Option<String>) -> Self {
        Self {
            token,
            budget: Mutex::new(Budget::default()),
        }
    }

    /// The budget as last reported.
    pub fn budget(&self) -> Budget {
        self.budget.lock().map_or_else(|p| *p.into_inner(), |b| *b)
    }

    /// A GET of `path` on the API host, with the API headers and the token.
    pub fn api_request(&self, path: &str) -> Request {
        let mut req = Request::get(format!("{API}{path}"), "rest-releases")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            // ubs:ignore — an outbound request header; token_from rejects CR, LF and other controls
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req
    }

    /// Sends an API request unless the budget is spent, then records the new budget.
    fn send(&self, req: Request, http: &dyn Http) -> Result<Response> {
        let budget = self.budget();
        if !budget.allows() {
            let reset = budget.reset.unwrap_or_else(Timestamp::now);
            return Err(Error::rate_limited(
                reset,
                format!(
                    "GitHub's API allows {} more requests before {reset}; omaddons keeps the last {GITHUB_REST_RESERVE} in reserve",
                    budget.remaining.unwrap_or(0)
                ),
                Suggestion::text(format!(
                    "wait until {reset}, or export {TOKEN_VAR}=<a GitHub token> for 5000 requests an hour"
                )),
            ));
        }
        let resp = http.send(req)?;
        if let Ok(mut b) = self.budget.lock() {
            b.update(&resp);
        }
        Ok(resp)
    }

    fn list(&self, w: &Wanted, owner: &str, repo: &str, http: &dyn Http) -> Result<ReleaseSet> {
        let mut req = self.api_request(&format!(
            "/repos/{owner}/{repo}/releases?per_page={PER_PAGE}"
        ));
        req.validators = true;
        let resp = self.send(req, http)?;
        if resp.status == 404 {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("{} does not exist on GitHub, or it is private", w.r#ref),
                Suggestion::command(["omaddons", "help", "install"]),
            ));
        }
        let body = resp.error_for_status("GitHub's releases API")?.bytes()?;
        let releases: Vec<ApiRelease> = from_json(&body, &w.r#ref)?;
        let Some(release) = choose(&releases, w.channel) else {
            return Err(Error::new(
                ErrorCode::NoCompatibleRelease,
                format!(
                    "{}: no published {} release among the newest {PER_PAGE}",
                    w.r#ref,
                    channel_name(w.channel)
                ),
                Suggestion::command([
                    "omaddons",
                    "install",
                    &w.r#ref.to_string(),
                    "--channel",
                    "beta",
                ]),
            ));
        };
        self.release(w, owner, repo, release, true, http)
    }

    fn tagged(
        &self,
        w: &Wanted,
        owner: &str,
        repo: &str,
        tag: &str,
        http: &dyn Http,
    ) -> Result<ReleaseSet> {
        let path = format!("/repos/{owner}/{repo}/releases/tags/{}", encode_path(tag));
        let resp = self.send(self.api_request(&path), http)?;
        if resp.status == 404 {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("{} has no release tagged {tag:?}", w.r#ref),
                Suggestion::command(["omaddons", "install", &w.r#ref.to_string()]),
            ));
        }
        let body = resp.error_for_status("GitHub's releases API")?.bytes()?;
        let release: ApiRelease = from_json(&body, &w.r#ref)?;
        // The web path already found no usable release.json for this tag.
        self.release(w, owner, repo, &release, false, http)
    }

    /// One release: from its `release.json` when it has one (and `manifest` allows),
    /// else from its zip assets.
    fn release(
        &self,
        w: &Wanted,
        owner: &str,
        repo: &str,
        release: &ApiRelease,
        manifest: bool,
        http: &dyn Http,
    ) -> Result<ReleaseSet> {
        let channel = if release.prerelease {
            Channel::Beta
        } else {
            Channel::Stable
        };
        let published_at = release.published_at.as_deref().and_then(|s| s.parse().ok());
        let json = release
            .assets
            .iter()
            .find(|a| a.name.eq("release.json"))
            .filter(|_| manifest);
        if let Some(json) = json
            && let Some(parsed) = manifest_at(&json.browser_download_url, http)?
        {
            let base = format!("https://github.com/{owner}/{repo}");
            let mut set = release_set(w, &base, &release.tag_name, &parsed);
            for r in &mut set.releases {
                r.channel = channel;
                r.published_at = published_at;
            }
            return Ok(set);
        }
        let assets: Vec<Asset> = release.assets.iter().filter_map(zip_asset).collect();
        if assets.is_empty() {
            return Err(Error::new(
                ErrorCode::NoCompatibleRelease,
                format!(
                    "{}@{} has no zip assets and no release.json",
                    w.r#ref, release.tag_name
                ),
                Suggestion::text("download the addon, then: omaddons install <path>"),
            )
            .alternative(Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                format!("git:https://github.com/{owner}/{repo}#{}", release.tag_name),
            ])));
        }
        Ok(ReleaseSet {
            r#ref: w.r#ref.clone(),
            releases: vec![Release {
                release_id: format!("{}@{}", w.r#ref, release.tag_name),
                version: release.tag_name.clone(),
                channel,
                published_at,
                assets,
                changelog_url: release.html_url.clone(),
            }],
        })
    }
}

impl GithubRestFallback for GithubRest {
    fn fetch(&self, w: &Wanted, reason: &RestReason, http: &dyn Http) -> Result<ReleaseSet> {
        let Ref::GitHub { owner, repo } = &w.r#ref else {
            return Err(Error::internal(format!("the GitHub API got {}", w.r#ref)));
        };
        match reason {
            RestReason::Prerelease => self.list(w, owner, repo, http),
            RestReason::NoReleaseJson { tag, .. } => self.tagged(w, owner, repo, tag, http),
        }
    }
}

/// `OMADDONS_GITHUB_TOKEN` when set, not blank and free of control characters (a CR or
/// LF would split the request header); nothing else is ever read.
pub fn token_from(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup(TOKEN_VAR)
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
}

/// A release as the API lists it; unknown fields are ignored and missing ones default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiRelease {
    pub tag_name: String,
    pub prerelease: bool,
    pub draft: bool,
    /// RFC 3339.
    pub published_at: Option<String>,
    pub html_url: Option<String>,
    pub assets: Vec<ApiAsset>,
}

/// A release asset as the API lists it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: Option<u64>,
    /// `sha256:<hex>` on assets uploaded since mid-2025.
    pub digest: Option<String>,
}

fn from_json<T: serde::de::DeserializeOwned>(body: &[u8], r: &Ref) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| {
        Error::new(
            ErrorCode::Network,
            format!("{r}: GitHub's releases API answered something unreadable ({e})"),
            Suggestion::text("retry later; if it persists, GitHub changed its API"),
        )
        .retryable(true)
    })
}

/// The newest release `channel` admits, skipping drafts: by `published_at` when it can
/// be read, else the API's order (newest first).
pub fn choose(releases: &[ApiRelease], channel: Channel) -> Option<&ApiRelease> {
    let when = |r: &ApiRelease| {
        r.published_at
            .as_deref()
            .and_then(|s| s.parse::<Timestamp>().ok())
    };
    let mut best: Option<&ApiRelease> = None;
    for r in releases {
        let stable_only = matches!(channel, Channel::Stable);
        if r.draft || (stable_only && r.prerelease) || r.tag_name.is_empty() {
            continue;
        }
        if best.is_none_or(|b| when(r) > when(b)) {
            best = Some(r);
        }
    }
    best
}

/// A release's `release.json`, read from its github.com download URL (no REST); `None`
/// when it is missing or unusable, so the zips are used instead.
fn manifest_at(url: &str, http: &dyn Http) -> Result<Option<release_json::Manifest>> {
    let resp = http.send(Request::get(url, "release-json"))?;
    if resp.status == 404 {
        return Ok(None);
    }
    let body = resp.error_for_status("release.json")?.bytes()?;
    Ok(release_json::parse(&body).ok())
}

/// A `.zip` asset as a candidate, with the hints its name carries.
fn zip_asset(a: &ApiAsset) -> Option<Asset> {
    if !a.name.to_ascii_lowercase().ends_with(".zip") || a.browser_download_url.is_empty() {
        return None;
    }
    let (name_flavors, nolib) = name_hints(&a.name);
    Some(Asset {
        filename: a.name.clone(),
        location: AssetLocation::Url(a.browser_download_url.clone()),
        size: a.size,
        expected_hash: a
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .and_then(ExpectedHash::sha256),
        flavors: Vec::new(),
        name_flavors,
        nolib,
    })
}

/// Flavor hints and the nolib flag from an asset's file name. Words are matched whole
/// (`Foo-classic`, `Foo_tbc`, `Foo-nolib`); client versions (`3.3.5`, `335`, `1.12`) map
/// to legacy families. `classic` alone, without a more specific word, is any classic.
pub fn name_hints(file_name: &str) -> (Vec<FlavorHint>, bool) {
    let lower = file_name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".zip").unwrap_or(&lower);
    let words: Vec<&str> = stem
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let has = |names: &[&str]| words.iter().any(|w| names.contains(w));
    let mut hints = Vec::new();
    let mut add = |hint: FlavorHint, yes: bool| {
        if yes && !hints.contains(&hint) {
            hints.push(hint);
        }
    };
    add(FlavorHint::Mainline, has(&["mainline", "retail"]));
    add(FlavorHint::Classic, has(&["vanilla", "era"]));
    add(FlavorHint::Bcc, has(&["bcc", "tbc"]));
    add(FlavorHint::Wrath, has(&["wrath", "wotlk"]));
    add(FlavorHint::Cata, has(&["cata"]));
    add(FlavorHint::Mists, has(&["mists", "mop"]));
    add(FlavorHint::Titan, has(&["titan"]));
    add(FlavorHint::Forever, has(&["forever"]));
    add(FlavorHint::VanillaLegacy, version_in(stem, &["1.12"]));
    add(FlavorHint::TbcLegacy, version_in(stem, &["2.4.3"]));
    add(
        FlavorHint::WrathLegacy,
        version_in(stem, &["3.3.5"]) || has(&["335", "335a"]),
    );
    add(FlavorHint::CataLegacy, version_in(stem, &["4.3.4"]));
    add(FlavorHint::MistsLegacy, version_in(stem, &["5.4.8"]));
    let specific_classic = hints.iter().any(|h| {
        matches!(
            h,
            FlavorHint::Classic
                | FlavorHint::Bcc
                | FlavorHint::Wrath
                | FlavorHint::Cata
                | FlavorHint::Mists
                | FlavorHint::Titan
                | FlavorHint::Forever
        )
    });
    if has(&["classic"]) && !specific_classic {
        hints.push(FlavorHint::ClassicAny);
    }
    (hints, has(&["nolib"]))
}

/// Whether `version` occurs in `stem` not inside a longer number (`3.3.5` in `v3.3.5a`,
/// not `1.12` in `11.12.0` or `1.120`).
fn version_in(stem: &str, versions: &[&str]) -> bool {
    versions.iter().any(|v| {
        stem.match_indices(v).any(|(i, _)| {
            let before = stem[..i].chars().next_back();
            let after = stem[i + v.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_digit() || c == '.')
                && !after.is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// A tag's path segments, each percent-encoded.
fn encode_path(tag: &str) -> String {
    tag.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

const fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
        Channel::Alpha => "alpha",
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn release(tag: &str, prerelease: bool, draft: bool, published: Option<&str>) -> ApiRelease {
        ApiRelease {
            tag_name: tag.to_owned(),
            prerelease,
            draft,
            published_at: published.map(str::to_owned),
            ..ApiRelease::default()
        }
    }

    #[test]
    fn channel_selection() {
        let list = [
            release("v3-draft", false, true, Some("2026-10-05T00:00:00Z")),
            release("v3-beta", true, false, Some("2026-10-04T00:00:00Z")),
            release("v2", false, false, Some("2026-10-01T00:00:00Z")),
            release("v1", false, false, Some("2026-09-01T00:00:00Z")),
        ];
        let tag = |c| choose(&list, c).map(|r| r.tag_name.as_str());
        assert_eq!(tag(Channel::Stable), Some("v2"));
        assert_eq!(tag(Channel::Beta), Some("v3-beta"));
        assert_eq!(tag(Channel::Alpha), Some("v3-beta"));
        // A stable release published after the prerelease wins beta too.
        let later = [
            release("v4", false, false, Some("2026-10-06T00:00:00Z")),
            release("v3-beta", true, false, Some("2026-10-04T00:00:00Z")),
        ];
        assert_eq!(
            choose(&later, Channel::Beta).map(|r| r.tag_name.as_str()),
            Some("v4")
        );
        // Without dates, the API's order decides.
        let undated = [
            release("b", true, false, None),
            release("a", false, false, None),
        ];
        assert_eq!(
            choose(&undated, Channel::Beta).map(|r| r.tag_name.as_str()),
            Some("b")
        );
        assert_eq!(
            choose(&undated, Channel::Stable).map(|r| r.tag_name.as_str()),
            Some("a")
        );
        let drafts = [release("d", false, true, None)];
        assert!(choose(&drafts, Channel::Beta).is_none());
    }

    #[test]
    fn asset_name_heuristics() {
        use FlavorHint::*;
        #[rustfmt::skip]
        let cases: &[(&str, &[FlavorHint], bool)] = &[
            ("ElvUI-3.3.5.zip",                 &[WrathLegacy],              false),
            ("ElvUI_v3.3.5a.zip",               &[WrathLegacy],              false),
            ("pfQuest-wotlk-335.zip",           &[Wrath, WrathLegacy],       false),
            ("Foo-1.12.1.zip",                  &[VanillaLegacy],            false),
            ("Foo-11.12.0.zip",                 &[],                         false),
            ("Foo-1.120.zip",                   &[],                         false),
            ("Foo-2.4.3.zip",                   &[TbcLegacy],                false),
            ("Foo-4.3.4.zip",                   &[CataLegacy],               false),
            ("Foo-5.4.8.zip",                   &[MistsLegacy],              false),
            ("Foo-classic.zip",                 &[ClassicAny],               false),
            ("Foo-Classic-Era.zip",             &[Classic],                  false),
            ("Foo-vanilla.zip",                 &[Classic],                  false),
            ("Foo-bcc-nolib.zip",               &[Bcc],                      true),
            ("Foo_TBC.zip",                     &[Bcc],                      false),
            ("Foo-Mainline.zip",                &[Mainline],                 false),
            ("Foo-retail-nolib.zip",            &[Mainline],                 true),
            ("Foo-wrath.zip",                   &[Wrath],                    false),
            ("Foo-cata.zip",                    &[Cata],                     false),
            ("Foo-catalog.zip",                 &[],                         false),
            ("Foo-mists.zip",                   &[Mists],                    false),
            ("Foo-MoP.zip",                     &[Mists],                    false),
            ("Foo-titan.zip",                   &[Titan],                    false),
            ("Foo-forever.zip",                 &[Forever],                  false),
            ("Foo_v1.2.zip",                    &[],                         false),
            ("Foo-camera.zip",                  &[],                         false),
            ("Foo-classic-tbc.zip",             &[Bcc],                      false),
            ("Foo-1.2.3-classic-nolib.zip",     &[ClassicAny],               true),
            ("Foo-12.1.5.zip",                  &[],                         false),
            ("GroupBulletinBoard-1.2.3.zip",    &[],                         false),
            ("FOO-NOLIB.ZIP",                   &[],                         true),
        ];
        for (name, hints, nolib) in cases {
            assert_eq!(name_hints(name), (hints.to_vec(), *nolib), "{name}");
        }
    }

    #[test]
    fn only_zips_become_candidates_with_their_digests() {
        let digest = format!("sha256:{}", "AB".repeat(32));
        let asset = |name: &str| ApiAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://github.com/o/r/releases/download/v1/{name}"),
            size: Some(3),
            digest: Some(digest.clone()),
        };
        let zip = zip_asset(&asset("Foo-1.0.zip")).unwrap();
        assert_eq!(
            zip.expected_hash,
            Some(ExpectedHash::Sha256("ab".repeat(32)))
        );
        assert_eq!(zip.size, Some(3));
        assert!(zip_asset(&asset("Foo.AppImage")).is_none());
        assert!(zip_asset(&asset("Foo.zip.blockmap")).is_none());
    }

    fn response(headers: &[(&str, &str)]) -> Response {
        let headers = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Response::new(200, headers, format!("{API}/x"), Box::new(std::io::empty()))
    }

    #[test]
    fn budget_states() {
        let mut b = Budget::default();
        assert!(b.allows(), "unknown allows the first request");
        b.update(&response(&[
            ("X-RateLimit-Remaining", "6"),
            ("X-RateLimit-Reset", "1800000000"),
        ]));
        assert!(b.allows());
        assert_eq!(b.reset, Timestamp::from_second(1_800_000_000).ok());
        b.update(&response(&[("x-ratelimit-remaining", "5")]));
        assert!(!b.allows(), "the reserve is kept");
        assert_eq!(
            b.reset,
            Timestamp::from_second(1_800_000_000).ok(),
            "reset carried"
        );
        b.update(&response(&[]));
        assert_eq!(
            b.remaining,
            Some(5),
            "a response without the headers changes nothing"
        );
    }

    #[test]
    fn the_token_comes_only_from_its_own_variable_and_goes_only_to_the_api() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(token_from(env(&[("GITHUB_TOKEN", "ghp_x")])), None);
        assert_eq!(token_from(env(&[(TOKEN_VAR, "  ")])), None);
        assert_eq!(token_from(env(&[(TOKEN_VAR, "a\r\nX-Evil: 1")])), None);
        assert_eq!(
            token_from(env(&[(TOKEN_VAR, "ghp_y")])).as_deref(),
            Some("ghp_y")
        );

        let auth = |rest: &GithubRest| {
            let req = rest.api_request("/repos/o/r/releases");
            assert!(req.url.starts_with("https://api.github.com/"));
            req.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.clone())
        };
        let anonymous = GithubRest::with_token(token_from(env(&[("GITHUB_TOKEN", "ghp_x")])));
        assert_eq!(auth(&anonymous), None);
        let authed = GithubRest::with_token(Some("ghp_y".into()));
        assert_eq!(auth(&authed).as_deref(), Some("Bearer ghp_y"));
        assert!(
            !format!("{authed:?}").contains("ghp_y"),
            "Debug hides the token"
        );
    }

    proptest! {
        #[test]
        fn any_name_gives_hints(name in ".{0,64}") {
            let _ = name_hints(&name);
        }
    }
}
