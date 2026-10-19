//! GitHub releases on the stable channel without the REST API (D28, vision §4.5):
//!
//! 1. `releases/latest` answers with a redirect to `/releases/tag/<tag>`; the tag is read
//!    from `Location` without following it.
//! 2. `releases/download/<tag>/release.json`, the packager's manifest, lists every zip
//!    with its flavors.
//!
//! That is two github.com web requests per repository, which carry no REST rate limit.
//! A pinned version skips step 1. Releases without a usable `release.json`, and the
//! beta and alpha channels (only the API knows which releases are prereleases), go to
//! the [`GithubRestFallback`] seam; without one they fail with exact next steps.
//! `releases.atom` is decoded for [`GithubSource::recent_tags`]; installs do not need it.
//!
//! Step 1 alone is the probe (M2-D2, tag-first): an unchanged tag lets the release memo
//! answer with one request. A changed one is fetched without asking `releases/latest`
//! again: the probe's answer is kept for the run.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use super::changelog::{Changelog, ChangelogEntry, same_version};
use super::release_json::{self, Manifest};
use super::{Source, SourceId, Wanted, atom};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::CHANGELOG_MAX_CHARS;
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::{CacheState, Http, Request, run_bounded};
use crate::politeness::MAX_CONCURRENT_REQUESTS;

/// Why the web path handed a ref to the fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestReason {
    /// The release tagged `tag` has no usable `release.json` (`why` says what was wrong
    /// when one was there).
    NoReleaseJson { tag: String, why: Option<String> },
    /// The beta or alpha channel, which needs the prerelease flag.
    Prerelease,
}

/// The GitHub API path (source-github-rest): asset listings and prereleases.
pub trait GithubRestFallback: Send + Sync {
    fn fetch(&self, wanted: &Wanted, reason: &RestReason, http: &dyn Http) -> Result<ReleaseSet>;
}

/// The `gh:` source.
#[derive(Default)]
pub struct GithubSource {
    pub rest: Option<Box<dyn GithubRestFallback>>,
    /// What this run's probes found, `(repository URL, tag)` by ref identity: a fetch
    /// after a probe reuses it instead of asking `releases/latest` again (the registry,
    /// and so this, lives one command).
    probed: Mutex<BTreeMap<String, (String, String)>>,
}

/// A recent release from `releases.atom`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTag {
    pub tag: String,
    /// RFC 3339.
    pub updated: Option<String>,
    /// The release notes, as the feed's `<content>` holds them (HTML).
    pub notes: Option<String>,
}

/// `releases.atom`, newest first, and whether it was unchanged since the last run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTags {
    pub tags: Vec<RecentTag>,
    pub revalidated: bool,
}

impl Source for GithubSource {
    fn id(&self) -> SourceId {
        SourceId::Github
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        self.fetch_reporting(wanted, http, &|_| {})
    }

    /// Each repository is fetched on its own, so each is reported as it answers.
    fn fetch_reporting(
        &self,
        wanted: &[Wanted],
        http: &dyn Http,
        answered: &(dyn Fn(&Wanted) + Sync),
    ) -> Vec<Result<ReleaseSet>> {
        run_bounded(wanted.to_vec(), MAX_CONCURRENT_REQUESTS, |w| {
            let set = self.fetch_one(&w, http);
            answered(&w);
            set
        })
    }

    /// From `releases.atom` (one conditional request, never the REST API): the notes of
    /// each tag newer than `since`, which the feed's ten entries may not reach
    /// (`truncated`, the releases page as `more_url`).
    fn changelog(
        &self,
        r: &Ref,
        _latest: &Release,
        since: Option<&str>,
        limit: usize,
        http: &dyn Http,
    ) -> Option<Result<Changelog>> {
        let Ref::GitHub { owner, repo } = r else {
            return None;
        };
        Some(Self::recent_tags(owner, repo, http).map(|feed| {
            let mut entries = Vec::new();
            let mut reached = since.is_none();
            let mut cut = false;
            for t in feed.tags {
                if since.is_some_and(|s| same_version(&t.tag, s)) {
                    reached = true;
                    break;
                }
                if entries.len() == limit {
                    cut = true;
                    break;
                }
                entries.push(ChangelogEntry {
                    version: t.tag,
                    published_at: t.updated.and_then(|u| u.parse().ok()),
                    text: t
                        .notes
                        .map(|n| crate::text::html_to_text(&n, CHANGELOG_MAX_CHARS))
                        .unwrap_or_default(),
                });
            }
            Changelog {
                source: SourceId::Github,
                entries,
                truncated: since.is_some() && (cut || !reached),
                more_url: Some(format!("https://github.com/{owner}/{repo}/releases")),
            }
        }))
    }

    /// The stable channel's newest release id from `releases/latest` alone (one request,
    /// no REST); `None` for prereleases and pinned versions.
    fn probe(&self, wanted: &Wanted, http: &dyn Http) -> Option<Result<String>> {
        let Ref::GitHub { owner, repo } = &wanted.r#ref else {
            return None;
        };
        if wanted.channel != Channel::Stable || wanted.version.is_some() {
            return None;
        }
        let base = format!("https://github.com/{owner}/{repo}");
        Some(latest_tag(wanted, &base, http).map(|(base, tag)| {
            let token = format!("{}@{tag}", wanted.r#ref);
            self.probed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(wanted.r#ref.identity(), (base, tag));
            token
        }))
    }
}

impl GithubSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// With the API path for what the web path cannot answer.
    pub fn with_rest(rest: Box<dyn GithubRestFallback>) -> Self {
        Self {
            rest: Some(rest),
            ..Self::default()
        }
    }

    fn fetch_one(&self, w: &Wanted, http: &dyn Http) -> Result<ReleaseSet> {
        let Ref::GitHub { owner, repo } = &w.r#ref else {
            return Err(Error::internal(format!(
                "the github source got {}",
                w.r#ref
            )));
        };
        let base = format!("https://github.com/{owner}/{repo}");
        if let Some(version) = &w.version {
            return self.pinned(w, &base, version, http);
        }
        if w.channel != Channel::Stable {
            return self.fallback(w, RestReason::Prerelease, http);
        }
        // A probe of this run already asked `releases/latest`.
        let probed = self
            .probed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&w.r#ref.identity());
        let (base, tag) = match probed {
            Some(found) => found,
            None => latest_tag(w, &base, http)?,
        };
        match release_json(&base, &tag, http)? {
            Ok(manifest) => Ok(release_set(w, &base, &tag, &manifest)),
            Err(why) => self.fallback(w, RestReason::NoReleaseJson { tag, why }, http),
        }
    }

    /// An explicit version is the tag; `1.2.3` falls back once to `v1.2.3`.
    fn pinned(&self, w: &Wanted, base: &str, version: &str, http: &dyn Http) -> Result<ReleaseSet> {
        if !valid_tag(version) {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("{} has no release tagged {version:?}", w.r#ref),
                Suggestion::command(["omaddons", "install", &w.r#ref.to_string()]),
            ));
        }
        let mut found = release_json(base, version, http)?;
        let mut tag = version.to_owned();
        if matches!(found, Err(None)) && !version.starts_with('v') {
            let prefixed = format!("v{version}");
            let again = release_json(base, &prefixed, http)?;
            if !matches!(again, Err(None)) {
                (found, tag) = (again, prefixed);
            }
        }
        match found {
            Ok(manifest) => Ok(release_set(w, base, &tag, &manifest)),
            Err(why) => self.fallback(w, RestReason::NoReleaseJson { tag, why }, http),
        }
    }

    fn fallback(&self, w: &Wanted, reason: RestReason, http: &dyn Http) -> Result<ReleaseSet> {
        if let Some(rest) = &self.rest {
            return rest.fetch(w, &reason, http);
        }
        let r = &w.r#ref;
        let git = |rev: Option<&str>| {
            let mut url = format!("git:https://github.com/{}", repo_path(r));
            if let Some(rev) = rev {
                url = format!("{url}#{rev}");
            }
            Suggestion::command(["omaddons".to_owned(), "install".to_owned(), url])
        };
        Err(match reason {
            RestReason::Prerelease => Error::new(
                ErrorCode::NoCompatibleRelease,
                format!(
                    "{r}: the {} channel needs GitHub's release API, which this build does not use yet",
                    channel_name(w.channel)
                ),
                Suggestion::command(["omaddons", "install", &r.to_string(), "--channel", "stable"]),
            ),
            RestReason::NoReleaseJson { tag, why } => {
                let detail = why.map_or_else(
                    || "publishes no release.json".to_owned(),
                    |why| format!("has an unusable release.json ({why})"),
                );
                let pinned = w.version.is_some().then_some(tag.as_str());
                Error::new(
                    ErrorCode::NoCompatibleRelease,
                    format!("{r}@{tag} {detail}, so its assets cannot be listed without GitHub's release API"),
                    Suggestion::text("download a release zip, then: omaddons install <path>"),
                )
                .alternative(git(pinned))
            }
        })
    }

    /// `releases.atom` through a conditional GET.
    pub fn recent_tags(owner: &str, repo: &str, http: &dyn Http) -> Result<RecentTags> {
        let mut req = Request::get(
            format!("https://github.com/{owner}/{repo}/releases.atom"),
            "releases-atom",
        );
        req.validators = true;
        let resp = http.send(req)?.error_for_status("releases.atom")?;
        let revalidated = resp.cache == CacheState::Revalidated;
        let body = resp.bytes()?;
        let tags = atom::parse(&String::from_utf8_lossy(&body))
            .into_iter()
            .filter_map(|e| {
                let tag = e
                    .link
                    .as_deref()
                    .and_then(|l| l.split_once("/releases/tag/"))
                    .map(|(_, t)| percent_unescape(t.split(['?', '#']).next().unwrap_or(t)))
                    .unwrap_or(e.title);
                (!tag.is_empty()).then_some(RecentTag {
                    tag,
                    updated: e.updated,
                    notes: e.content,
                })
            })
            .collect();
        Ok(RecentTags { tags, revalidated })
    }
}

/// The tag `releases/latest` redirects to, and the repository's current URL (a renamed
/// repository first redirects to its new `releases/latest`).
fn latest_tag(w: &Wanted, base: &str, http: &dyn Http) -> Result<(String, String)> {
    let r = &w.r#ref;
    let mut base = base.to_owned();
    for _ in 0..3 {
        let resp = http.send(
            Request::get(format!("{base}/releases/latest"), "release-latest").manual_redirects(),
        )?;
        if resp.status == 404 {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("{r} has no GitHub releases (or the repository does not exist)"),
                Suggestion::command([
                    "omaddons".to_owned(),
                    "install".to_owned(),
                    format!("git:https://github.com/{}", repo_path(r)),
                ]),
            ));
        }
        if !(300..400).contains(&resp.status) {
            let status = resp.status;
            resp.error_for_status("releases/latest")?;
            return Err(Error::http_status(
                status,
                format!("{r}: releases/latest answered {status} instead of a redirect"),
                Suggestion::text(
                    "retry later; if it persists, GitHub changed how latest releases redirect",
                ),
            ));
        }
        let location = resp.location().unwrap_or_default();
        let path = location.split(['?', '#']).next().unwrap_or_default();
        if let Some((_, tag)) = path.split_once("/releases/tag/") {
            let tag = percent_unescape(tag);
            if !valid_tag(&tag) {
                return Err(Error::internal(format!(
                    "{r}: releases/latest redirected to an unusable tag {tag:?}"
                )));
            }
            return Ok((base, tag));
        }
        if path.trim_end_matches('/').ends_with("/releases") {
            return Err(Error::new(
                ErrorCode::NoCompatibleRelease,
                format!("{r} has releases but no stable one (only prereleases)"),
                Suggestion::command(["omaddons", "install", &r.to_string(), "--channel", "beta"]),
            ));
        }
        match path.strip_suffix("/releases/latest") {
            Some(moved) if moved.starts_with("https://github.com/") => base = moved.to_owned(),
            Some(moved) if moved.starts_with('/') => base = format!("https://github.com{moved}"),
            _ => {
                return Err(Error::internal(format!(
                    "{r}: releases/latest redirected somewhere unexpected ({location:?})"
                )));
            }
        }
    }
    Err(Error::internal(format!(
        "{r}: releases/latest kept redirecting"
    )))
}

/// `Ok(manifest)`, or `Err(None)` when there is no `release.json` and `Err(Some(why))`
/// when there is one that cannot be used.
fn release_json(
    base: &str,
    tag: &str,
    http: &dyn Http,
) -> Result<std::result::Result<Manifest, Option<String>>> {
    let url = format!("{base}/releases/download/{}/release.json", encode_tag(tag));
    let resp = http.send(Request::get(url, "release-json"))?;
    if resp.status == 404 {
        return Ok(Err(None));
    }
    let body = resp.error_for_status("release.json")?.bytes()?;
    Ok(release_json::parse(&body).map_err(Some))
}

/// One stable release with an asset per manifest entry.
pub fn release_set(w: &Wanted, base: &str, tag: &str, manifest: &Manifest) -> ReleaseSet {
    let download = format!("{base}/releases/download/{}", encode_tag(tag));
    let assets = manifest
        .entries
        .iter()
        .map(|e| Asset {
            filename: e.filename.clone(),
            location: AssetLocation::Url(format!("{download}/{}", encode_segment(&e.filename))),
            size: None,
            expected_hash: None,
            flavors: e.flavors.clone(),
            name_flavors: Vec::new(),
            nolib: e.nolib,
        })
        .collect();
    let version = manifest
        .entries
        .iter()
        .find_map(|e| e.version.clone())
        .unwrap_or_else(|| tag.to_owned());
    ReleaseSet {
        r#ref: w.r#ref.clone(),
        releases: vec![Release {
            release_id: format!("{}@{tag}", w.r#ref),
            version,
            channel: Channel::Stable,
            published_at: None,
            assets,
            changelog_url: Some(format!("{base}/releases/tag/{}", encode_tag(tag))),
            notes: None,
            name_hint: None,
        }],
    }
}

fn repo_path(r: &Ref) -> String {
    match r {
        Ref::GitHub { owner, repo } => format!("{owner}/{repo}"),
        other => other.to_string(),
    }
}

const fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
        Channel::Alpha => "alpha",
    }
}

/// A tag usable in a download path: no empty, `.` or `..` segment, no control characters.
pub(crate) fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.chars().any(char::is_control)
        && tag
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Decodes `%XX` escapes; anything else, `+` included, is literal.
pub fn percent_unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A tag for a URL path: each `/`-separated segment encoded, the slashes kept.
fn encode_tag(tag: &str) -> String {
    tag.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::net::Response;

    /// Canned responses by URL; records every request.
    #[derive(Default)]
    struct Canned {
        responses: HashMap<String, Reply>,
        seen: Mutex<Vec<String>>,
    }

    /// (status, headers, body)
    type Reply = (u16, Vec<(String, String)>, String);

    impl Canned {
        fn on(mut self, url: &str, status: u16, location: Option<&str>, body: &str) -> Self {
            let headers = location
                .map(|l| vec![("location".to_owned(), l.to_owned())])
                .unwrap_or_default();
            self.responses
                .insert(url.to_owned(), (status, headers, body.to_owned()));
            self
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Http for Canned {
        fn send(&self, req: Request) -> Result<Response> {
            self.seen.lock().unwrap().push(req.url.clone());
            let (status, headers, body) =
                self.responses
                    .get(&req.url)
                    .cloned()
                    .unwrap_or((404, Vec::new(), String::new()));
            Ok(Response::new(
                status,
                headers,
                req.url,
                Box::new(std::io::Cursor::new(body.into_bytes())),
            ))
        }
    }

    const MANIFEST: &str = r#"{"releases":[{"name":"A","version":"1.2.3","filename":"A-1.2.3.zip","nolib":false,"metadata":[{"flavor":"mainline","interface":120100}]},{"filename":"A-1.2.3-nolib.zip","nolib":true,"metadata":[]}]}"#;

    fn wanted(r: &str) -> Wanted {
        Wanted::stable(r.parse().unwrap())
    }

    fn fetch(http: &Canned, w: Wanted) -> Result<ReleaseSet> {
        GithubSource::new().fetch(&[w], http).pop().unwrap()
    }

    #[test]
    fn stable_release_from_redirect_and_manifest() {
        let http = Canned::default()
            .on(
                "https://github.com/o/a/releases/latest",
                302,
                Some("https://github.com/o/a/releases/tag/1.2.3"),
                "",
            )
            .on(
                "https://github.com/o/a/releases/download/1.2.3/release.json",
                200,
                None,
                MANIFEST,
            );
        let set = fetch(&http, wanted("gh:o/a")).unwrap();
        let release = &set.releases[0];
        assert_eq!(
            (
                release.release_id.as_str(),
                release.version.as_str(),
                release.channel
            ),
            ("gh:o/a@1.2.3", "1.2.3", Channel::Stable)
        );
        assert_eq!(
            release.changelog_url.as_deref(),
            Some("https://github.com/o/a/releases/tag/1.2.3")
        );
        assert_eq!(release.assets.len(), 2);
        assert_eq!(
            release.assets[0].location,
            AssetLocation::Url("https://github.com/o/a/releases/download/1.2.3/A-1.2.3.zip".into())
        );
        assert_eq!(release.assets[0].flavors[0].flavor, "mainline");
        assert!(release.assets[1].nolib && release.assets[1].flavors.is_empty());
        assert_eq!(http.seen().len(), 2);
    }

    #[test]
    fn tags_with_slashes_and_plus_signs() {
        let http = Canned::default()
            .on(
                "https://github.com/o/q/releases/latest",
                302,
                Some("/o/q/releases/tag/bundle/v1.0%2Bv2"),
                "",
            )
            .on(
                "https://github.com/o/q/releases/download/bundle/v1.0%2Bv2/release.json",
                200,
                None,
                r#"{"releases":[{"filename":"Q v1+2.zip"}]}"#,
            );
        let set = fetch(&http, wanted("gh:o/q")).unwrap();
        let release = &set.releases[0];
        assert_eq!(
            release.version, "bundle/v1.0+v2",
            "no version in the manifest: the tag"
        );
        assert_eq!(
            release.assets[0].location,
            AssetLocation::Url(
                "https://github.com/o/q/releases/download/bundle/v1.0%2Bv2/Q%20v1%2B2.zip".into()
            )
        );
    }

    #[test]
    fn a_renamed_repository_is_followed() {
        let http = Canned::default()
            .on(
                "https://github.com/old/a/releases/latest",
                301,
                Some("https://github.com/new/a/releases/latest"),
                "",
            )
            .on(
                "https://github.com/new/a/releases/latest",
                302,
                Some("https://github.com/new/a/releases/tag/1.2.3"),
                "",
            )
            .on(
                "https://github.com/new/a/releases/download/1.2.3/release.json",
                200,
                None,
                MANIFEST,
            );
        let set = fetch(&http, wanted("gh:old/a")).unwrap();
        assert_eq!(set.r#ref.to_string(), "gh:old/a");
        assert!(
            matches!(&set.releases[0].assets[0].location, AssetLocation::Url(u) if u.starts_with("https://github.com/new/a/"))
        );
    }

    #[test]
    fn the_probe_is_the_tag_and_a_fetch_after_it_asks_no_second_time() {
        let http = Canned::default()
            .on(
                "https://github.com/old/a/releases/latest",
                301,
                Some("https://github.com/new/a/releases/latest"),
                "",
            )
            .on(
                "https://github.com/new/a/releases/latest",
                302,
                Some("https://github.com/new/a/releases/tag/1.2.3"),
                "",
            )
            .on(
                "https://github.com/new/a/releases/download/1.2.3/release.json",
                200,
                None,
                MANIFEST,
            );
        let source = GithubSource::new();
        let w = wanted("gh:old/a");
        let token = source.probe(&w, &http).unwrap().unwrap();
        assert_eq!(
            token, "gh:old/a@1.2.3",
            "the release id, through the rename"
        );
        let set = source
            .fetch(std::slice::from_ref(&w), &http)
            .pop()
            .unwrap()
            .unwrap();
        assert_eq!(set.releases[0].release_id, token);
        let latest = http
            .seen()
            .iter()
            .filter(|u| u.ends_with("/releases/latest"))
            .count();
        assert_eq!(latest, 2, "the old and the renamed one, once each");
        assert!(
            http.seen()
                .iter()
                .any(|u| u.starts_with("https://github.com/new/a/releases/download")),
            "fetched from the renamed repository"
        );
        // Prereleases and pinned versions have no probe.
        for w in [
            Wanted {
                channel: Channel::Beta,
                ..wanted("gh:o/a")
            },
            Wanted {
                channel: Channel::Alpha,
                ..wanted("gh:o/a")
            },
            Wanted {
                version: Some("1.0".into()),
                ..wanted("gh:o/a")
            },
        ] {
            assert!(source.probe(&w, &http).is_none(), "{w:?}");
        }
    }

    #[test]
    fn no_releases_and_prerelease_only() {
        let err = fetch(&Canned::default(), wanted("gh:o/none")).unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::NotFound, 3));
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "install", "git:https://github.com/o/none"]
        );

        let http = Canned::default().on(
            "https://github.com/o/pre/releases/latest",
            302,
            Some("https://github.com/o/pre/releases"),
            "",
        );
        let err = fetch(&http, wanted("gh:o/pre")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoCompatibleRelease);
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "install", "gh:o/pre", "--channel", "beta"]
        );
    }

    #[test]
    fn missing_or_malformed_manifests_without_a_fallback() {
        let latest = |repo: &str| {
            (
                format!("https://github.com/o/{repo}/releases/latest"),
                format!("https://github.com/o/{repo}/releases/tag/2.0"),
            )
        };
        let (url, tag) = latest("plain");
        let http = Canned::default().on(&url, 302, Some(&tag), "");
        let err = fetch(&http, wanted("gh:o/plain")).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoCompatibleRelease);
        assert!(
            err.message.contains("publishes no release.json"),
            "{}",
            err.message
        );
        assert_eq!(err.fix.argv(), None);
        assert_eq!(
            err.alternatives[0].argv().unwrap(),
            ["omaddons", "install", "git:https://github.com/o/plain"]
        );

        let (url, tag) = latest("broken");
        let http = Canned::default().on(&url, 302, Some(&tag), "").on(
            "https://github.com/o/broken/releases/download/2.0/release.json",
            200,
            None,
            "{",
        );
        let err = fetch(&http, wanted("gh:o/broken")).unwrap_err();
        assert!(
            err.message
                .contains("unusable release.json (release.json is not JSON"),
            "{}",
            err.message
        );
    }

    #[test]
    fn pinned_versions_retry_with_a_v_prefix() {
        let http = Canned::default().on(
            "https://github.com/o/a/releases/download/v1.2.3/release.json",
            200,
            None,
            MANIFEST,
        );
        let w = Wanted {
            version: Some("1.2.3".into()),
            ..wanted("gh:o/a")
        };
        let set = fetch(&http, w.clone()).unwrap();
        assert_eq!(set.releases[0].release_id, "gh:o/a@v1.2.3");
        assert_eq!(
            http.seen(),
            [
                "https://github.com/o/a/releases/download/1.2.3/release.json",
                "https://github.com/o/a/releases/download/v1.2.3/release.json"
            ],
            "releases/latest skipped"
        );

        let err = fetch(&Canned::default(), w).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoCompatibleRelease);
        assert_eq!(
            err.alternatives[0].argv().unwrap()[2],
            "git:https://github.com/o/a#1.2.3"
        );

        let err = fetch(
            &Canned::default(),
            Wanted {
                version: Some("../x".into()),
                ..wanted("gh:o/a")
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn prerelease_channels_need_the_fallback() {
        let http = Canned::default();
        let err = fetch(
            &http,
            Wanted {
                channel: Channel::Beta,
                ..wanted("gh:o/a")
            },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NoCompatibleRelease);
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "install", "gh:o/a", "--channel", "stable"]
        );
        assert!(http.seen().is_empty(), "no request without a fallback");

        struct Fallback;
        impl GithubRestFallback for Fallback {
            fn fetch(&self, w: &Wanted, reason: &RestReason, _: &dyn Http) -> Result<ReleaseSet> {
                assert_eq!(*reason, RestReason::Prerelease);
                Ok(ReleaseSet {
                    r#ref: w.r#ref.clone(),
                    releases: Vec::new(),
                })
            }
        }
        let source = GithubSource::with_rest(Box::new(Fallback));
        let w = Wanted {
            channel: Channel::Alpha,
            ..wanted("gh:o/a")
        };
        assert!(source.fetch(&[w], &http).pop().unwrap().is_ok());
    }

    #[test]
    fn recent_tags_from_the_feed() {
        let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>x</id><title>bundle/v1+v2</title><updated>2026-10-12T20:29:01Z</updated>
            <link rel="alternate" href="https://github.com/o/a/releases/tag/bundle%2Fv1%2Bv2"/></entry>
            <entry><title>no link</title></entry></feed>"#;
        let http = Canned::default().on("https://github.com/o/a/releases.atom", 200, None, feed);
        let recent = GithubSource::recent_tags("o", "a", &http).unwrap();
        assert!(!recent.revalidated);
        assert_eq!(
            recent.tags[0],
            RecentTag {
                tag: "bundle/v1+v2".into(),
                updated: Some("2026-10-12T20:29:01Z".into()),
                notes: None,
            }
        );
        assert_eq!(recent.tags[1].tag, "no link");
    }

    #[test]
    fn percent_coding() {
        assert_eq!(percent_unescape("a%2Fb%2bc+d%zz%"), "a/b+c+d%zz%");
        assert_eq!(encode_segment("A b+c/é"), "A%20b%2Bc%2F%C3%A9");
        assert_eq!(encode_tag("bundle/v1+2"), "bundle/v1%2B2");
        assert!(!valid_tag("a//b") && !valid_tag("..") && !valid_tag("") && valid_tag("bundle/v1"));
    }

    /// The web path never touches the REST API.
    #[test]
    fn no_rest_host_in_the_web_path() {
        let host = concat!("api", ".github", ".com");
        for (name, text) in [
            ("github.rs", include_str!("github.rs")),
            ("release_json.rs", include_str!("release_json.rs")),
            ("atom.rs", include_str!("atom.rs")),
        ] {
            assert!(!text.contains(host), "{name} names the REST host");
        }
    }
}
