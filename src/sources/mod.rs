//! Addon sources. `fetch` does the network work once per distinct ref per run;
//! selection and verification are pure and live in `resolve`.
//!
//! [`fetch_once`] deduplicates refs (D28: requests never multiply by target count),
//! groups them by [`SourceId`], and calls each registered source's [`Source::fetch`]
//! exactly once, groups in parallel. Errors are per ref. Refs of sources this build does
//! not have (CurseForge and Wago arrive in M5), or that config.toml turns
//! off (`[sources.<id>] enabled = false`), fail with `source_disabled` without touching
//! the network (DEC-12).

pub mod atom;
pub mod changelog;
pub mod git;
pub mod git_github;
pub mod git_hosts;
pub mod github;
pub mod github_rest;
pub mod github_search;
pub mod github_token;
pub mod local;
pub mod refs;
pub mod release_json;
pub mod tukui;
pub mod url;
pub mod wowace;
pub mod wowi;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Channel, ExpectedHash, HashFromHeader, Ref, Release, ReleaseSet};
use crate::net::{Http, run_bounded};
use crate::politeness::MAX_CONCURRENT_REQUESTS;
use crate::state::config::SourceConfig;

/// A source a command could not consult, and why (`sources_skipped` in outputs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SourceSkipped {
    pub source: SourceId,
    pub reason: SourceSkipReason,
    /// When a rate limit lifts; omitted unless `reason` is `rate_limited` and the reset
    /// time is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub reset_at: Option<jiff::Timestamp>,
}

impl SourceSkipped {
    /// What a failed read of `source` leaves: `offline`, `rate_limited` (with when it
    /// lifts, from the error's `reset_at`), else `no_index`.
    pub fn for_error(source: SourceId, error: &Error) -> Self {
        let reason = match error.code {
            ErrorCode::Offline => SourceSkipReason::Offline,
            ErrorCode::RateLimited => SourceSkipReason::RateLimited,
            _ => SourceSkipReason::NoIndex,
        };
        Self {
            source,
            reason,
            reset_at: error.details["reset_at"]
                .as_str()
                .and_then(|t| t.parse().ok()),
        }
    }
}

/// Why a source was not consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceSkipReason {
    Offline,
    RateLimited,
    Disabled,
    /// Its index (WoWInterface's file list) is neither stored nor fetchable.
    NoIndex,
}

/// Which source a ref belongs to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceId {
    Github,
    Git,
    Url,
    Local,
    Wowi,
    Tukui,
    Curseforge,
    Wago,
    Wowace,
}

impl SourceId {
    pub const ALL: [Self; 9] = [
        Self::Github,
        Self::Git,
        Self::Url,
        Self::Local,
        Self::Wowi,
        Self::Tukui,
        Self::Curseforge,
        Self::Wago,
        Self::Wowace,
    ];

    pub fn of(r: &Ref) -> Self {
        match r {
            Ref::GitHub { .. } => Self::Github,
            Ref::Git { .. } => Self::Git,
            Ref::Url { .. } => Self::Url,
            Ref::File { .. } => Self::Local,
            Ref::Wowi { .. } => Self::Wowi,
            Ref::Tukui { .. } => Self::Tukui,
            Ref::CurseForge { .. } => Self::Curseforge,
            Ref::Wago { .. } => Self::Wago,
            Ref::WowAce { .. } => Self::Wowace,
        }
    }

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Git => "git",
            Self::Url => "url",
            Self::Local => "local",
            Self::Wowi => "wowi",
            Self::Tukui => "tukui",
            Self::Curseforge => "curseforge",
            Self::Wago => "wago",
            Self::Wowace => "wowace",
        }
    }

    /// The id config.toml names (`[sources.<wire>]`).
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.wire() == name)
    }

    /// The name people know the source by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Git => "git",
            Self::Url => "url",
            Self::Local => "local file",
            Self::Wowi => "WoWInterface",
            Self::Tukui => "Tukui",
            Self::Curseforge => "CurseForge",
            Self::Wago => "Wago",
            Self::Wowace => "WowAce",
        }
    }

    /// Whether this source needs a credential to work at all (`capabilities.sources[]`):
    /// CurseForge and Wago (M5). GitHub's token is optional, so it does not.
    pub const fn requires_credential(self) -> bool {
        match self {
            Self::Curseforge | Self::Wago => true,
            Self::Github
            | Self::Git
            | Self::Url
            | Self::Local
            | Self::Wowi
            | Self::Tukui
            | Self::Wowace => false,
        }
    }

    /// The milestone a source this build lacks arrives in.
    const fn later(self) -> Option<&'static str> {
        match self {
            Self::Curseforge | Self::Wago => Some("M5"),
            Self::Github
            | Self::Git
            | Self::Url
            | Self::Local
            | Self::Wowi
            | Self::Tukui
            | Self::Wowace => None,
        }
    }
}

/// Which sources config.toml turns off (`[sources.<id>] enabled = false`). Every
/// source is on unless turned off; names that are not sources are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceConfigView {
    disabled: BTreeSet<SourceId>,
    /// The GitHub token REST requests carry; none unless given.
    github: TokenSlot,
    /// `--offline`: the GitHub clients refuse before resolving a credential, so a
    /// credential command never runs.
    offline: bool,
}

/// The token a view hands to the GitHub clients; views compare by where it comes from.
#[derive(Debug, Clone, Default)]
struct TokenSlot(Option<Arc<github_token::GithubToken>>);

impl PartialEq for TokenSlot {
    fn eq(&self, other: &Self) -> bool {
        let source = |s: &Self| s.0.as_ref().map(|t| t.source());
        source(self) == source(other)
    }
}

impl Eq for TokenSlot {}

impl SourceConfigView {
    pub fn from_config(sources: &BTreeMap<String, SourceConfig>) -> Self {
        Self {
            disabled: sources
                .iter()
                .filter(|(_, config)| matches!(config.enabled, Some(false)))
                .filter_map(|(name, _)| SourceId::from_wire(name))
                .collect(),
            github: TokenSlot::default(),
            offline: false,
        }
    }

    /// Every source on (tests).
    pub fn all_enabled() -> Self {
        Self::default()
    }

    /// With the command's resolved GitHub token.
    #[must_use]
    pub fn with_github_token(mut self, token: Arc<github_token::GithubToken>) -> Self {
        self.github = TokenSlot(Some(token));
        self
    }

    /// For a command run `--offline` (or not).
    #[must_use]
    pub fn with_offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    pub fn offline(&self) -> bool {
        self.offline
    }

    /// The GitHub token (none when the view was given none).
    pub fn github_token(&self) -> Arc<github_token::GithubToken> {
        self.github
            .0
            .clone()
            .unwrap_or_else(github_token::GithubToken::none)
    }

    pub fn enabled(&self, id: SourceId) -> bool {
        !self.disabled.contains(&id)
    }
}

/// One addon source (vision §5.4).
pub trait Source: Send + Sync {
    fn id(&self) -> SourceId;
    /// Network; batched; called once per run with every distinct wish for this source.
    /// Result `i` belongs to `wanted[i]`.
    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>>;

    /// A cheap freshness token for `wanted`, when one costs less than a fetch: the
    /// newest release's id, compared with the memo's (`MemoEntry.probe`). `None` when the
    /// source has no probe for this wish. A probe's error is the fetch's for that wish.
    fn probe(&self, wanted: &Wanted, http: &dyn Http) -> Option<Result<String>> {
        let _ = (wanted, http);
        None
    }

    /// The concrete URL and hash rule of a `Deferred` asset this source made for `key`,
    /// fetched right before each download attempt. Only sources whose assets are
    /// deferred implement it.
    fn resolve_asset(&self, key: &str, http: &dyn Http) -> Result<ResolvedAsset> {
        let _ = (key, http);
        Err(Error::internal(format!(
            "the {} source cannot resolve deferred assets",
            self.id().wire()
        )))
    }

    /// The newest release older than `newest` (a tag) whose own metadata declares a build
    /// for the client `names_client` accepts: GitHub's look-back through its recent
    /// releases, for repositories that publish one release per client. `None` when the
    /// source has no look-back.
    fn look_back(
        &self,
        wanted: &Wanted,
        newest: &str,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
        http: &dyn Http,
    ) -> Option<Result<LookBack>> {
        let _ = (wanted, newest, names_client, http);
        None
    }

    /// [`Source::fetch`], calling `answered` as each wish is answered (fetch progress).
    /// By default every wish is reported once the batch is back; a source that answers
    /// wishes one at a time reports each as it comes.
    fn fetch_reporting(
        &self,
        wanted: &[Wanted],
        http: &dyn Http,
        answered: &(dyn Fn(&Wanted) + Sync),
    ) -> Vec<Result<ReleaseSet>> {
        let sets = self.fetch(wanted, http);
        for w in wanted {
            answered(w);
        }
        sets
    }

    /// Release notes for the releases of `r` newer than `since` (an installed release's
    /// identity; exclusive), newest first, at most `limit`; `latest` is the newest
    /// release, for a source that serves only its notes. `None` when the source has
    /// none ([`changelog::changelog_for`] says `not_supported`).
    fn changelog(
        &self,
        r: &Ref,
        latest: &Release,
        since: Option<&str>,
        limit: usize,
        http: &dyn Http,
    ) -> Option<Result<changelog::Changelog>> {
        let _ = (r, latest, since, limit, http);
        None
    }
}

/// A shared source is the source (the registry keeps a typed handle to Tukui's list).
impl<S: Source> Source for std::sync::Arc<S> {
    fn id(&self) -> SourceId {
        S::id(self)
    }

    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>> {
        S::fetch(self, wanted, http)
    }

    fn fetch_reporting(
        &self,
        wanted: &[Wanted],
        http: &dyn Http,
        answered: &(dyn Fn(&Wanted) + Sync),
    ) -> Vec<Result<ReleaseSet>> {
        S::fetch_reporting(self, wanted, http, answered)
    }

    fn probe(&self, wanted: &Wanted, http: &dyn Http) -> Option<Result<String>> {
        S::probe(self, wanted, http)
    }

    fn resolve_asset(&self, key: &str, http: &dyn Http) -> Result<ResolvedAsset> {
        S::resolve_asset(self, key, http)
    }

    fn changelog(
        &self,
        r: &Ref,
        latest: &Release,
        since: Option<&str>,
        limit: usize,
        http: &dyn Http,
    ) -> Option<Result<changelog::Changelog>> {
        S::changelog(self, r, latest, since, limit, http)
    }
}

/// What a [`Source::look_back`] found: the release declared for the client, as a set of
/// its own, the releases ruled out on the way (tag, why), and every recent tag, newest
/// first, so an installed release can be placed before or after the one found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LookBack {
    pub found: Option<ReleaseSet>,
    pub ruled_out: Vec<(String, String)>,
    pub order: Vec<String>,
}

/// Where a `Deferred` asset's bytes are now. Used for one download attempt and never
/// stored: plans show `canonical_url` instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAsset {
    pub url: String,
    pub expected_hash: Option<ExpectedHash>,
    /// A hash the download response's header carries (Tukui's `ETag` MD5).
    pub hash_from_header: Option<HashFromHeader>,
    /// The stable metadata URL plans show, e.g. `https://api.tukui.org/v1/addon/elvui`.
    pub canonical_url: Option<String>,
}

/// What a run wants from a ref: its channel, and a pinned version (`--version`, or a
/// release URL's tag). Equal wishes are fetched once, however many targets share them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wanted {
    pub r#ref: Ref,
    pub channel: Channel,
    pub version: Option<String>,
}

impl Wanted {
    /// The latest stable release of `r`.
    pub fn stable(r: Ref) -> Self {
        Self {
            r#ref: r,
            channel: Channel::Stable,
            version: None,
        }
    }
}

/// The sources of this build.
#[derive(Default)]
pub struct SourceRegistry {
    sources: BTreeMap<SourceId, Box<dyn Source>>,
    /// The git runner, when the command layer provides one: the git source resolves
    /// non-GitHub hosts with it and the engine checks them out with it.
    git: Option<std::sync::Arc<git_hosts::GitRunner>>,
    /// The Tukui source, when registered: its list serves `ReleaseCache::tukui_list`.
    tukui: Option<std::sync::Arc<tukui::TukuiSource>>,
    /// Sources this build has that config.toml turned off.
    off: BTreeSet<SourceId>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every source this build ships that `config` leaves on.
    pub fn builtin(config: &SourceConfigView) -> Self {
        let tukui = std::sync::Arc::new(tukui::TukuiSource::default());
        let shipped: [Box<dyn Source>; 7] = [
            Box::new(github::GithubSource::with_rest(Box::new(
                github_rest::GithubRest::with_github_token(config.github_token())
                    .offline(config.offline()),
            ))),
            Box::new(git::GitSource::default()),
            Box::new(local::LocalSource),
            Box::new(url::UrlSource::new()),
            Box::new(wowi::WowiSource),
            Box::new(std::sync::Arc::clone(&tukui)),
            Box::new(wowace::WowAceSource),
        ];
        let mut registry = Self::new();
        for source in shipped {
            if config.enabled(source.id()) {
                registry.register(source);
            } else {
                registry.off.insert(source.id());
            }
        }
        if config.enabled(SourceId::Tukui) {
            registry.tukui = Some(tukui);
        }
        registry
    }

    /// The Tukui source, if registered.
    pub fn tukui(&self) -> Option<&tukui::TukuiSource> {
        self.tukui.as_deref()
    }

    /// With `runner` for `git:` refs to configured non-GitHub hosts; unchanged when
    /// config.toml turned git off.
    #[must_use]
    pub fn with_git(mut self, runner: git_hosts::GitRunner) -> Self {
        if self.off.contains(&SourceId::Git) {
            return self;
        }
        let runner = std::sync::Arc::new(runner);
        self.register(Box::new(git::GitSource {
            runner: Some(runner.clone()),
        }));
        self.git = Some(runner);
        self
    }

    /// The git runner, if this run has one.
    pub fn git(&self) -> Option<&git_hosts::GitRunner> {
        self.git.as_deref()
    }

    /// Adds a source, replacing one with the same id.
    pub fn register(&mut self, source: Box<dyn Source>) {
        self.sources.insert(source.id(), source);
    }

    pub fn get(&self, id: SourceId) -> Option<&dyn Source> {
        self.sources.get(&id).map(Box::as_ref)
    }

    /// The ids of the sources registered: the ones that are on.
    pub fn enabled(&self) -> BTreeSet<SourceId> {
        self.sources.keys().copied().collect()
    }

    /// `source_disabled` for a ref whose source is not registered: turned off in
    /// config.toml (the fix turns it back on), or not in this build.
    pub fn disabled(&self, r: &Ref) -> Error {
        self.unavailable(SourceId::of(r), &r.to_string())
    }

    /// `source_disabled` for `subject` (a ref, or a release whose download the source
    /// must resolve) when source `id` is not registered.
    pub fn unavailable(&self, id: SourceId, subject: &str) -> Error {
        if self.off.contains(&id) {
            let key = format!("sources.{}.enabled", id.wire());
            return Error::new(
                ErrorCode::SourceDisabled,
                format!(
                    "{subject}: the {} source is disabled in config.toml",
                    id.name()
                ),
                Suggestion::command(["omaddons", "config", "set", &key, "true"]),
            );
        }
        let message = match id.later() {
            Some(milestone) => format!("{subject}: {} arrives in {milestone}", id.name()),
            None => format!("{subject}: the {} source is not available", id.name()),
        };
        Error::new(
            ErrorCode::SourceDisabled,
            message,
            Suggestion::command(["omaddons", "help", "install"]),
        )
    }
}

/// Fetches every distinct ref once: one `fetch` call per source, sources in parallel.
pub fn fetch_once(
    wanted: impl IntoIterator<Item = Wanted>,
    registry: &SourceRegistry,
    http: &dyn Http,
) -> BTreeMap<Wanted, Result<ReleaseSet>> {
    fetch_once_reporting(wanted, registry, http, &|_| {})
}

/// [`fetch_once`], calling `answered` as each wish a source handles is answered (a
/// disabled source's wishes are not reported: nothing was asked).
pub fn fetch_once_reporting(
    wanted: impl IntoIterator<Item = Wanted>,
    registry: &SourceRegistry,
    http: &dyn Http,
    answered: &(dyn Fn(&Wanted) + Sync),
) -> BTreeMap<Wanted, Result<ReleaseSet>> {
    let mut groups: BTreeMap<SourceId, Vec<Wanted>> = BTreeMap::new();
    for w in wanted {
        let group = groups.entry(SourceId::of(&w.r#ref)).or_default();
        if !group.contains(&w) {
            group.push(w);
        }
    }
    let mut results = BTreeMap::new();
    let mut work: Vec<(&dyn Source, Vec<Wanted>)> = Vec::new();
    for (id, group) in groups {
        match registry.get(id) {
            Some(source) => work.push((source, group)),
            None => {
                for w in group {
                    let error = registry.disabled(&w.r#ref);
                    results.insert(w, Err(error));
                }
            }
        }
    }
    let fetched = run_bounded(work, MAX_CONCURRENT_REQUESTS, |(source, group)| {
        let sets = source.fetch_reporting(&group, http, answered);
        (source.id(), group, sets)
    });
    for (id, group, sets) in fetched {
        let mut sets = sets.into_iter();
        for w in group {
            let set = sets.next().unwrap_or_else(|| {
                Err(Error::internal(format!(
                    "the {} source returned no result for {}",
                    id.wire(),
                    w.r#ref
                )))
            });
            results.insert(w, set);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    use super::*;
    use crate::net::{Request, Response};

    /// Counts calls and refs; fails refs named in `failing`.
    struct Counting {
        id: SourceId,
        calls: Arc<Mutex<Vec<Vec<Ref>>>>,
        barrier: Option<Arc<Barrier>>,
        failing: Vec<Ref>,
    }

    impl Source for Counting {
        fn id(&self) -> SourceId {
            self.id
        }

        fn fetch(&self, wanted: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
            let refs: Vec<Ref> = wanted.iter().map(|w| w.r#ref.clone()).collect();
            self.calls.lock().unwrap().push(refs.clone());
            if let Some(barrier) = &self.barrier {
                barrier.wait();
            }
            refs.iter()
                .map(|r| {
                    if self.failing.contains(r) {
                        Err(Error::new(
                            ErrorCode::NotFound,
                            "gone",
                            Suggestion::text("x"),
                        ))
                    } else {
                        Ok(ReleaseSet {
                            r#ref: r.clone(),
                            releases: Vec::new(),
                        })
                    }
                })
                .collect()
        }
    }

    #[derive(Default)]
    struct NoNetwork(AtomicUsize);

    impl Http for NoNetwork {
        fn send(&self, req: Request) -> Result<Response> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Response::new(
                200,
                Vec::new(),
                req.url,
                Box::new(std::io::empty()),
            ))
        }
    }

    fn r(s: &str) -> Ref {
        s.parse().unwrap()
    }

    fn w(s: &str) -> Wanted {
        Wanted::stable(r(s))
    }

    #[test]
    fn each_answered_wish_is_reported_once_and_disabled_sources_never() {
        let mut registry = SourceRegistry::new();
        registry.register(Box::new(Counting {
            id: SourceId::Github,
            calls: Arc::new(Mutex::new(Vec::new())),
            barrier: None,
            failing: vec![r("gh:a/gone")],
        }));
        let reported = Mutex::new(Vec::new());
        let wishes = ["gh:a/one", "gh:a/two", "gh:a/gone", "cf:1"].map(w);
        let results = fetch_once_reporting(wishes, &registry, &NoNetwork::default(), &|w| {
            reported.lock().unwrap().push(w.r#ref.to_string());
        });
        assert_eq!(results.len(), 4);
        let mut reported = reported.into_inner().unwrap();
        reported.sort();
        // Failures are answers too; CurseForge (not registered) is asked nothing.
        assert_eq!(reported, ["gh:a/gone", "gh:a/one", "gh:a/two"]);
    }

    #[test]
    fn each_ref_is_fetched_once() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut registry = SourceRegistry::new();
        registry.register(Box::new(Counting {
            id: SourceId::Github,
            calls: Arc::clone(&calls),
            barrier: None,
            failing: vec![r("gh:a/gone")],
        }));
        let http = NoNetwork::default();
        let wanted = [
            "gh:WeakAuras/WeakAuras2",
            "gh:weakauras/weakauras2",
            "gh:WEAKAURAS/WeakAuras2",
            "gh:a/gone",
            "wago:aNDmy96o",
            "cf:1",
        ];
        let mut wishes: Vec<Wanted> = wanted.iter().map(|s| w(s)).collect();
        wishes.push(Wanted {
            channel: crate::model::Channel::Beta,
            ..w("gh:a/gone")
        });
        let results = fetch_once(wishes, &registry, &http);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [vec![
                r("gh:WeakAuras/WeakAuras2"),
                r("gh:a/gone"),
                r("gh:a/gone")
            ]],
            "one call, deduplicated by ref and channel"
        );
        assert_eq!(results.len(), 5);
        assert!(results[&w("gh:weakauras/weakauras2")].is_ok());
        assert_eq!(
            results[&w("gh:a/gone")].as_ref().unwrap_err().code,
            ErrorCode::NotFound,
            "per-ref failure"
        );
        let wago = results[&w("wago:aNDmy96o")].as_ref().unwrap_err();
        assert_eq!(
            (wago.code, wago.exit().code()),
            (ErrorCode::SourceDisabled, 1)
        );
        assert!(
            wago.message.contains("Wago arrives in M5"),
            "{}",
            wago.message
        );
        assert!(
            results[&w("cf:1")]
                .as_ref()
                .unwrap_err()
                .message
                .contains("M5")
        );
        assert_eq!(
            http.0.load(Ordering::SeqCst),
            0,
            "disabled sources never touch the network"
        );
    }

    #[test]
    fn a_source_without_deferred_assets_cannot_resolve_one() {
        let source = Counting {
            id: SourceId::Github,
            calls: Arc::new(Mutex::new(Vec::new())),
            barrier: None,
            failing: Vec::new(),
        };
        let error = source
            .resolve_asset("x", &NoNetwork::default())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(
            error.message,
            "the github source cannot resolve deferred assets"
        );
    }

    #[test]
    fn sources_run_concurrently() {
        let barrier = Arc::new(Barrier::new(2));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut registry = SourceRegistry::new();
        for id in [SourceId::Github, SourceId::Url] {
            registry.register(Box::new(Counting {
                id,
                calls: Arc::clone(&calls),
                barrier: Some(Arc::clone(&barrier)),
                failing: Vec::new(),
            }));
        }
        let results = fetch_once(
            [w("gh:a/b"), w("url:https://example.org/x.zip")],
            &registry,
            &NoNetwork::default(),
        );
        assert_eq!(
            results.len(),
            2,
            "both groups met at the barrier, so they ran at once"
        );
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn wire_names() {
        assert_eq!(
            serde_json::to_value(SourceId::Curseforge).unwrap(),
            "curseforge"
        );
        assert_eq!(SourceId::of(&r("file:/x")).wire(), "local");
        for id in SourceId::ALL {
            assert_eq!(serde_json::to_value(id).unwrap(), id.wire());
            assert_eq!(SourceId::from_wire(id.wire()), Some(id));
        }
        assert_eq!(SourceId::from_wire("wowinterface"), None);
        assert_eq!(SourceId::ALL.len(), 9);
        let wowace = SourceId::of(&r("wowace:bug-grabber#r152"));
        assert_eq!((wowace.wire(), wowace.name()), ("wowace", "WowAce"));
    }

    #[test]
    fn only_curseforge_and_wago_require_a_credential() {
        let cases = [
            (SourceId::Github, false),
            (SourceId::Git, false),
            (SourceId::Url, false),
            (SourceId::Local, false),
            (SourceId::Wowi, false),
            (SourceId::Tukui, false),
            (SourceId::Curseforge, true),
            (SourceId::Wago, true),
            (SourceId::Wowace, false),
        ];
        let covered: BTreeSet<_> = cases.iter().map(|(source, _)| *source).collect();
        assert_eq!(
            covered,
            SourceId::ALL.into_iter().collect(),
            "credential cases must cover every source exactly once: {cases:?}"
        );
        assert_eq!(
            covered.len(),
            cases.len(),
            "duplicate credential case: {cases:?}"
        );
        for (source, expected) in cases {
            assert_eq!(
                source.requires_credential(),
                expected,
                "{source:?}: requires_credential should be {expected}"
            );
        }
    }

    /// Until its source is registered, a `wowace:` ref is honestly unavailable.
    #[test]
    fn an_unregistered_wowace_source_is_not_available() {
        let registry = SourceRegistry::builtin(&config(&[]));
        let error = registry.unavailable(SourceId::Wowace, "wowace:bug-grabber");
        assert_eq!(error.code, ErrorCode::SourceDisabled);
        assert_eq!(
            error.message,
            "wowace:bug-grabber: the WowAce source is not available"
        );
    }

    fn config(entries: &[(&str, Option<bool>)]) -> SourceConfigView {
        let sources = entries
            .iter()
            .map(|(name, enabled)| {
                let config = SourceConfig {
                    enabled: *enabled,
                    credential: None,
                };
                ((*name).to_owned(), config)
            })
            .collect();
        SourceConfigView::from_config(&sources)
    }

    #[test]
    fn config_turns_sources_off_and_ignores_unknown_names() {
        let view = config(&[
            ("wowi", Some(false)),
            ("github", Some(true)),
            ("url", None),
            ("nonsense", Some(false)),
        ]);
        let off: Vec<SourceId> = SourceId::ALL
            .into_iter()
            .filter(|id| !view.enabled(*id))
            .collect();
        assert_eq!(off, [SourceId::Wowi]);
        assert!(
            SourceId::ALL
                .iter()
                .all(|id| SourceConfigView::all_enabled().enabled(*id))
        );
    }

    #[test]
    fn builtin_registers_what_config_leaves_on() {
        let all = SourceRegistry::builtin(&SourceConfigView::all_enabled());
        assert_eq!(
            all.enabled(),
            [
                SourceId::Github,
                SourceId::Git,
                SourceId::Url,
                SourceId::Local,
                SourceId::Wowi,
                SourceId::Tukui,
                SourceId::Wowace
            ]
            .into(),
            "curseforge and wago are not in this build"
        );
        assert!(all.tukui().is_some());
        // Turning a source this build lacks off changes nothing.
        let view = config(&[
            ("wowi", Some(false)),
            ("git", Some(false)),
            ("tukui", Some(false)),
            ("wowace", Some(false)),
            ("curseforge", Some(false)),
        ]);
        let some = SourceRegistry::builtin(&view);
        assert!(some.tukui().is_none());
        assert_eq!(
            some.enabled(),
            [SourceId::Github, SourceId::Url, SourceId::Local].into()
        );

        let wowi = some.disabled(&r("wowi:5995"));
        assert_eq!(
            (wowi.code, wowi.exit().code()),
            (ErrorCode::SourceDisabled, 1)
        );
        assert_eq!(
            wowi.message,
            "wowi:5995: the WoWInterface source is disabled in config.toml"
        );
        assert_eq!(
            wowi.fix.argv(),
            Some(
                ["omaddons", "config", "set", "sources.wowi.enabled", "true"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
        let cf = some.disabled(&r("cf:1"));
        assert_eq!(cf.message, "cf:1: CurseForge arrives in M5");
        assert_eq!(
            cf.fix.argv(),
            Some(
                ["omaddons", "help", "install"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
    }

    #[test]
    fn a_disabled_source_is_refused_without_the_network() {
        let view = config(&[("wowi", Some(false)), ("tukui", Some(false))]);
        let registry = SourceRegistry::builtin(&view);
        let http = NoNetwork::default();
        let results = fetch_once(
            [w("wowi:5995"), w("tukui:elvui"), w("wago:aNDmy96o")],
            &registry,
            &http,
        );
        let wowi = results[&w("wowi:5995")].as_ref().unwrap_err();
        assert!(
            wowi.message.contains("disabled in config.toml"),
            "{}",
            wowi.message
        );
        let tukui = results[&w("tukui:elvui")].as_ref().unwrap_err();
        assert_eq!(
            tukui.message,
            "tukui:elvui: the Tukui source is disabled in config.toml"
        );
        let wago = results[&w("wago:aNDmy96o")].as_ref().unwrap_err();
        assert_eq!(wago.message, "wago:aNDmy96o: Wago arrives in M5");
        assert_eq!(http.0.load(Ordering::SeqCst), 0);
    }
}
