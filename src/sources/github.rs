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
use super::{LookBack, Source, SourceId, Wanted, atom};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::CHANGELOG_MAX_CHARS;
use crate::model::{Asset, AssetLocation, Channel, Ref, Release, ReleaseSet};
use crate::net::{CacheState, Http, Request, run_bounded};
use crate::politeness::{LOOK_BACK_MAX_RELEASES, LOOK_BACK_SAME_TRACK, MAX_CONCURRENT_REQUESTS};

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

    /// Whether GitHub marks release `tag` a prerelease (or a draft): one request.
    fn is_prerelease(&self, wanted: &Wanted, tag: &str, http: &dyn Http) -> Result<bool>;

    /// The repository's newest releases as the API lists them, newest first, drafts
    /// left out: what a beta or alpha look-back walks. The listing this command's fetch
    /// made is reused, so it costs no request then.
    fn recent_releases(&self, wanted: &Wanted, http: &dyn Http) -> Result<Vec<ListedRelease>>;
}

/// A shared fallback, so the registry can lend the same client (and its budget) to
/// `info`'s `about` ([`super::SourceRegistry::github_rest`]).
impl<T: GithubRestFallback + ?Sized> GithubRestFallback for std::sync::Arc<T> {
    fn fetch(&self, wanted: &Wanted, reason: &RestReason, http: &dyn Http) -> Result<ReleaseSet> {
        (**self).fetch(wanted, reason, http)
    }

    fn is_prerelease(&self, wanted: &Wanted, tag: &str, http: &dyn Http) -> Result<bool> {
        (**self).is_prerelease(wanted, tag, http)
    }

    fn recent_releases(&self, wanted: &Wanted, http: &dyn Http) -> Result<Vec<ListedRelease>> {
        (**self).recent_releases(wanted, http)
    }
}

/// One release of a REST listing, for a look-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRelease {
    pub tag: String,
    pub prerelease: bool,
    pub published_at: Option<jiff::Timestamp>,
    /// It has a `release.json` asset.
    pub release_json: bool,
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

    /// Through `releases.atom` (one conditional request), newest first and skipping
    /// `newest`, at most [`LOOK_BACK_MAX_RELEASES`]: each tag's `release.json` (github.com,
    /// no REST) until one declares a build `names_client` accepts. The feed does not mark
    /// prereleases, so the one found is confirmed through the REST API (one request);
    /// a prerelease, or a tag named like one, is passed over. Releases without a usable
    /// `release.json` are passed over too: their assets need the REST API to be listed.
    fn look_back(
        &self,
        wanted: &Wanted,
        newest: &str,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
    ) -> Option<Result<LookBack>> {
        let Ref::GitHub { owner, repo } = &wanted.r#ref else {
            return None;
        };
        if wanted.version.is_some() {
            return None;
        }
        Some(match wanted.channel {
            Channel::Stable => self.look_back_in(wanted, owner, repo, newest, names_client, http),
            Channel::Beta | Channel::Alpha => {
                let rest = self.rest.as_ref()?;
                self.look_back_listed(
                    wanted,
                    rest.as_ref(),
                    owner,
                    repo,
                    newest,
                    names_client,
                    http,
                )
            }
        })
    }
}

impl GithubSource {
    /// [`Source::look_back`] for `owner/repo` on the stable channel: through
    /// `releases.atom`.
    fn look_back_in(
        &self,
        w: &Wanted,
        owner: &str,
        repo: &str,
        newest: &str,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
    ) -> Result<LookBack> {
        let base = format!("https://github.com/{owner}/{repo}");
        let feed = Self::recent_tags(owner, repo, http)?;
        let order: Vec<String> = feed.tags.iter().map(|t| t.tag.clone()).collect();
        let mut ruled_out = Vec::new();
        let older: Vec<Older> = feed
            .tags
            .iter()
            .filter(|t| !same_version(&t.tag, newest))
            .map(|t| Older {
                tag: t.tag.clone(),
                // The feed dates each release; `release.json` does not.
                published: t.updated.as_deref().and_then(|u| u.parse().ok()),
                prerelease: None,
                release_json: true,
                skip: looks_prerelease(&t.tag).then(|| NAMED_PRERELEASE.to_owned()),
            })
            .collect();
        if let Some(set) = self.walk(w, &base, &older, names_client, http, &mut ruled_out)? {
            return Ok(LookBack {
                found: Some(set),
                ruled_out,
                order,
            });
        }
        // Every older release the feed holds is named like a prerelease, so it cannot
        // reach this repository's stable releases (VuhDo during an alpha series): probe
        // the newest stable tag's per-client siblings instead.
        let blind =
            !ruled_out.is_empty() && ruled_out.iter().all(|(_, why)| why == NAMED_PRERELEASE);
        if blind {
            for tag in siblings(newest, &order)
                .into_iter()
                .take(LOOK_BACK_MAX_RELEASES)
            {
                match self.read_release(w, &base, &tag, None, None, names_client, http)? {
                    Read::Found(set) => {
                        return Ok(LookBack {
                            found: Some(set),
                            ruled_out,
                            order,
                        });
                    }
                    Read::RuledOut(why, _) => {
                        ruled_out.push((tag, format!("probed as a sibling of {newest}: {why}")));
                    }
                }
            }
        }
        Ok(LookBack {
            found: None,
            ruled_out,
            order,
        })
    }

    /// [`Source::look_back`] on the beta and alpha channels: through the REST listing
    /// their fetch already made (no further request), newest first. Prereleases count,
    /// as these channels admit them, and the listing says which are.
    #[expect(
        clippy::too_many_arguments,
        reason = "the look-back's request and its REST client"
    )]
    fn look_back_listed(
        &self,
        w: &Wanted,
        rest: &dyn GithubRestFallback,
        owner: &str,
        repo: &str,
        newest: &str,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
    ) -> Result<LookBack> {
        let base = format!("https://github.com/{owner}/{repo}");
        let listed = rest.recent_releases(w, http)?;
        let order: Vec<String> = listed.iter().map(|r| r.tag.clone()).collect();
        let older: Vec<Older> = listed
            .into_iter()
            .filter(|r| !same_version(&r.tag, newest))
            .map(|r| Older {
                tag: r.tag,
                published: r.published_at,
                prerelease: Some(r.prerelease),
                release_json: r.release_json,
                skip: None,
            })
            .collect();
        let mut ruled_out = Vec::new();
        let found = self.walk(w, &base, &older, names_client, http, &mut ruled_out)?;
        Ok(LookBack {
            found,
            ruled_out,
            order,
        })
    }

    /// Reads `older` in order, at most [`LOOK_BACK_MAX_RELEASES`], until one declares a
    /// build for this client, noting each passed over in `ruled_out`. It stops early on
    /// a repository with one track: [`LOOK_BACK_SAME_TRACK`] releases in a row declaring
    /// the same clients, none of them this one. Skipped releases and missing
    /// `release.json` files neither count nor break the run.
    fn walk(
        &self,
        w: &Wanted,
        base: &str,
        older: &[Older],
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
        ruled_out: &mut Vec<(String, String)>,
    ) -> Result<Option<ReleaseSet>> {
        let mut track: (Vec<String>, usize) = (Vec::new(), 0);
        for o in older.iter().take(LOOK_BACK_MAX_RELEASES) {
            if let Some(why) = &o.skip {
                ruled_out.push((o.tag.clone(), why.clone()));
                continue;
            }
            if !o.release_json {
                ruled_out.push((o.tag.clone(), "publishes no release.json".to_owned()));
                continue;
            }
            let read = self.read_release(
                w,
                base,
                &o.tag,
                o.published,
                o.prerelease,
                names_client,
                http,
            )?;
            let (why, flavors) = match read {
                Read::Found(set) => return Ok(Some(set)),
                Read::RuledOut(why, flavors) => (why, flavors),
            };
            if let Some(flavors) = flavors {
                track = if track.0 == flavors {
                    (flavors, track.1 + 1)
                } else {
                    (flavors, 1)
                };
                if track.1 >= LOOK_BACK_SAME_TRACK {
                    ruled_out.push((
                        o.tag.clone(),
                        format!(
                            "{why}, nor did the {} before it (only {}): one track, so the look-back stops",
                            track.1 - 1,
                            track.0.join(", ")
                        ),
                    ));
                    break;
                }
            }
            ruled_out.push((o.tag.clone(), why));
        }
        Ok(None)
    }

    /// One older release of a look-back, `tag` of `base`: its `release.json` (github.com,
    /// no REST), and when that declares a build `names_client` accepts, whether GitHub
    /// marks it a prerelease (the one REST request).
    #[expect(
        clippy::too_many_arguments,
        reason = "one release and what is known of it"
    )]
    fn read_release(
        &self,
        w: &Wanted,
        base: &str,
        tag: &str,
        published: Option<jiff::Timestamp>,
        prerelease: Option<bool>,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
    ) -> Result<Read> {
        let manifest = match release_json(base, tag, http)? {
            Ok(manifest) => manifest,
            Err(None) => return Ok(Read::RuledOut("publishes no release.json".to_owned(), None)),
            Err(Some(why)) => {
                return Ok(Read::RuledOut(
                    format!("has an unusable release.json ({why})"),
                    None,
                ));
            }
        };
        let mut set = release_set(w, base, tag, &manifest);
        for release in &mut set.releases {
            release.published_at = published;
        }
        let flavors = || {
            set.releases
                .iter()
                .flat_map(|r| &r.assets)
                .flat_map(|a| &a.flavors)
        };
        if !flavors().any(names_client) {
            let mut declared: Vec<String> = flavors().map(|f| f.flavor.clone()).collect();
            declared.sort();
            declared.dedup();
            return Ok(Read::RuledOut(
                "its release.json declares no build for this client".to_owned(),
                Some(declared),
            ));
        }
        // A REST listing says it already: the beta and alpha channels admit both.
        if let Some(flagged) = prerelease {
            for release in &mut set.releases {
                release.channel = if flagged {
                    Channel::Beta
                } else {
                    Channel::Stable
                };
            }
            return Ok(Read::Found(set));
        }
        let Some(rest) = &self.rest else {
            return Err(Error::new(
                ErrorCode::NoCompatibleRelease,
                format!(
                    "{}: whether {tag} is a prerelease needs GitHub's release API",
                    w.r#ref
                ),
                Suggestion::command([
                    "omaddons",
                    "install",
                    &w.r#ref.to_string(),
                    "--version",
                    tag,
                ]),
            ));
        };
        if rest.is_prerelease(w, tag, http)? {
            return Ok(Read::RuledOut("a prerelease".to_owned(), None));
        }
        Ok(Read::Found(set))
    }

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
        // GitHub redirects here when no release is the latest one: every release is a
        // prerelease, or there is no published release at all.
        if path.trim_end_matches('/').ends_with("/releases") {
            return Err(Error::new(
                ErrorCode::NoCompatibleRelease,
                format!("{r} has no stable release (only prereleases, or none published)"),
                Suggestion::command(["omaddons", "install", &r.to_string(), "--channel", "beta"]),
            )
            .alternative(Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                format!("git:https://github.com/{}", repo_path(r)),
            ])));
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

/// A tag named like a prerelease (`v1.2-beta`, `2.0.0-rc1`, `v3-alpha`, `1.0-pre`): a
/// word of it is `alpha`, `beta`, `rc` or `pre`, digits allowed after, as WowAce's tags
/// are read. A look-back passes it over without asking the REST API.
fn looks_prerelease(tag: &str) -> bool {
    tag.split(|c: char| !c.is_ascii_alphanumeric())
        .any(prerelease_word)
}

/// `alpha`, `beta`, `rc` or `pre`, in any case, digits allowed after (`beta2`).
fn prerelease_word(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    ["alpha", "beta", "pre", "rc"].iter().any(|mark| {
        lower.starts_with(mark) && lower[mark.len()..].bytes().all(|b| b.is_ascii_digit())
    })
}

/// Why a look-back passes over a tag named like a prerelease.
const NAMED_PRERELEASE: &str = "named like a prerelease";

/// One older release a look-back may read.
struct Older {
    tag: String,
    published: Option<jiff::Timestamp>,
    /// GitHub's flag, from a REST listing; `None` from the feed, which does not say,
    /// so the release found is confirmed through the API.
    prerelease: Option<bool>,
    /// It has a `release.json`; the feed cannot tell, so it is read to find out.
    release_json: bool,
    /// Passed over without a request, and why (a tag named like a prerelease).
    skip: Option<String>,
}

/// One older release a look-back read.
enum Read {
    /// Its `release.json` declares a build for this client, and GitHub marks it stable.
    Found(ReleaseSet),
    /// Passed over, and why; with the clients its `release.json` declares, when it has
    /// one and declares none for this client.
    RuledOut(String, Option<Vec<String>>),
}

/// The tags of the newest stable release's per-client siblings, for a feed that holds
/// only prereleases: its version with each suffix the feed's tags use, a prerelease
/// mark taken off. `v3.218-catac` and `v3.219-mopc-beta-3`, `v3.219-beta-3` give
/// `v3.218-mopc` and `v3.218`. None of the newest's own, or of a tag the feed holds.
fn siblings(newest: &str, feed: &[String]) -> Vec<String> {
    let Some((version, own)) = split_tag(newest) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for tag in feed {
        let Some((_, rest)) = split_tag(tag) else {
            continue;
        };
        let suffix = strip_prerelease(rest);
        let sibling = format!("{version}{suffix}");
        if suffix != own && !feed.contains(&sibling) && !out.contains(&sibling) {
            out.push(sibling);
        }
    }
    out
}

/// A tag as its version and the rest: `v3.218-catac` is `v3.218` and `-catac`,
/// `2.8.3` is `2.8.3` and nothing. `None` for a tag that does not start with one
/// (`DetailsRetail.v9.1.0`).
fn split_tag(tag: &str) -> Option<(&str, &str)> {
    let start = usize::from(tag.starts_with(['v', 'V']));
    if !tag[start..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let end = tag[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or(tag.len(), |i| start + i);
    let version = tag[..end].trim_end_matches('.');
    Some((version, &tag[version.len()..]))
}

/// `rest` up to its first prerelease mark: `-mopc-beta-3` is `-mopc`, `-alpha-18` is
/// empty, `-catac` stays.
fn strip_prerelease(rest: &str) -> &str {
    let cut = rest.char_indices().find(|&(i, c)| {
        matches!(c, '-' | '_' | '.')
            && prerelease_word(
                rest[i + 1..]
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .next()
                    .unwrap_or_default(),
            )
    });
    cut.map_or(rest, |(i, _)| &rest[..i])
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
            revision: None,
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
        // Two hex digits exactly: `from_str_radix` alone also takes a sign (`%+9`).
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
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
        // The redirect cannot tell prereleases from no releases at all, so it claims
        // neither and offers both ways on.
        assert_eq!(
            err.message,
            "gh:o/pre has no stable release (only prereleases, or none published)"
        );
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "install", "gh:o/pre", "--channel", "beta"]
        );
        assert_eq!(
            err.alternatives[0].argv().unwrap(),
            ["omaddons", "install", "git:https://github.com/o/pre"]
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

            fn is_prerelease(&self, _: &Wanted, _: &str, _: &dyn Http) -> Result<bool> {
                unreachable!("no look-back here")
            }

            fn recent_releases(&self, _: &Wanted, _: &dyn Http) -> Result<Vec<ListedRelease>> {
                unreachable!("no look-back here")
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
    fn tags_named_like_prereleases() {
        for tag in [
            "v1.2-beta",
            "2.0.0-rc1",
            "v3-alpha",
            "1.0-pre",
            "v4.0.0-BETA2",
            "v3.216-mists-rc1",
        ] {
            assert!(looks_prerelease(tag), "{tag}");
        }
        for tag in [
            "v3.218-titan",
            "v3.216-bcc",
            "v12.1.1",
            "v2-precise",
            "release-candidate",
            "v1.0-arc",
            "alphabet-1",
        ] {
            assert!(!looks_prerelease(tag), "{tag}");
        }
    }

    #[test]
    fn tags_split_into_version_and_suffix() {
        assert_eq!(split_tag("v3.218-catac"), Some(("v3.218", "-catac")));
        assert_eq!(split_tag("v3.218"), Some(("v3.218", "")));
        assert_eq!(split_tag("2.8.3"), Some(("2.8.3", "")));
        assert_eq!(split_tag("1.47.11-classic"), Some(("1.47.11", "-classic")));
        assert_eq!(split_tag("v3."), Some(("v3", ".")));
        assert_eq!(split_tag("DetailsRetail.v9.1.0"), None);
        assert_eq!(split_tag("Plater-v661"), None);
        assert_eq!(strip_prerelease("-mopc-beta-3"), "-mopc");
        assert_eq!(strip_prerelease("-alpha-18"), "");
        assert_eq!(strip_prerelease("-catac"), "-catac");
        assert_eq!(strip_prerelease("-RC1"), "");
        assert_eq!(strip_prerelease("-precise"), "-precise");
    }

    /// VuhDo on 2026-10-24: `releases/latest` is `v3.218-catac`, and the feed holds only
    /// v3.219 alphas and betas, five per build.
    #[test]
    fn siblings_of_the_newest_stable_tag_from_the_feeds_suffixes() {
        let feed: Vec<String> = [
            "v3.219-vanilla-beta-3",
            "v3.219-vanilla-alpha-18",
            "v3.219-tbcc-beta-3",
            "v3.219-mopc-beta-3",
            "v3.219-catac-beta-3",
            "v3.219-beta-3",
            "v3.219-alpha-18",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            siblings("v3.218-catac", &feed),
            ["v3.218-vanilla", "v3.218-tbcc", "v3.218-mopc", "v3.218"]
        );
        // A newest tag with no version to build on probes nothing.
        assert!(siblings("Plater-v661", &feed).is_empty());
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
        assert_eq!(
            percent_unescape("a%+9b%-1"),
            "a%+9b%-1",
            "a sign is no digit"
        );
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
