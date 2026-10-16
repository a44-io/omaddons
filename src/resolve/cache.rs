//! `ReleaseCache`: the one way a command gets release metadata (M2-D37). Within a run it
//! fetches each wish once, however many engine runs and targets ask (the probe run and
//! the real run of `install` share one cache). Across runs it answers from the release
//! memo as far as the command's [`Freshness`] allows, and remembers what it fetched.
//!
//! It never answers a failed fetch from the memo on its own: a mutating command must not
//! install from metadata the source may have withdrawn. A read-only command that may
//! fall back asks for the remembered answer with [`ReleaseCache::stale`].

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Result, Suggestion, Warning, WarningCode};
use crate::limits::RELEASE_MEMO_MAX_ENTRIES;
use crate::model::{Channel, Ref, ReleaseSet};
use crate::net::Http;
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::sources::{SourceId, SourceRegistry, Wanted, fetch_once};
use crate::state::memo::{self, CandidateFact, FactIndex, MemoEntry, ReleaseMemo, TukuiList};
use crate::state::paths::Paths;

/// How far a command trusts remembered metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Answers from the memo while an entry is younger than this
    /// (`SOURCE_RECHECK_MIN_INTERVAL`): batch and read-only commands.
    Window(Duration),
    /// Never answers from the memo alone: a source's probe may confirm an entry, else it
    /// fetches (`install`: someone installing now expects now's release).
    Revalidate,
    /// Always fetches (`--refresh`).
    Refresh,
    /// Answers from the memo whatever its age, and fetches nothing (`--offline`, which
    /// wins over `--refresh`).
    OfflineOnly,
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A fresh memo entry; no request.
    Memo,
    /// A stale entry a source's cheap probe confirmed.
    Probed,
    /// A full fetch now.
    Fetched,
    /// A memo entry used whatever its age.
    StaleFallback,
}

/// One answer.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub set: Result<ReleaseSet>,
    pub origin: Origin,
    /// When the answer was fetched from the source.
    pub fetched_at: Timestamp,
}

/// What [`ReleaseCache::remember`] hands back: warnings for the outcome, notes for `-v`.
#[derive(Debug, Default)]
pub struct Remembered {
    pub warnings: Vec<Warning>,
    pub notes: Vec<String>,
}

/// What to do for one wish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Answer(Origin),
    Fetch,
}

/// The pure freshness decision for a wish whose memo entry is `entry`, at `now`.
fn decide(entry: Option<&MemoEntry>, policy: Freshness, now: Timestamp) -> Decision {
    match (policy, entry) {
        (Freshness::OfflineOnly, Some(_)) => Decision::Answer(Origin::StaleFallback),
        (Freshness::Window(window), Some(entry)) if entry.fresh(now, window) => {
            Decision::Answer(Origin::Memo)
        }
        // Offline with nothing remembered: the network guard refuses the fetch with
        // `offline`, and nothing reaches the network.
        _ => Decision::Fetch,
    }
}

/// The release metadata of one command run.
pub struct ReleaseCache<'a> {
    /// `None` for a transient cache, which reads and writes no memo.
    paths: Option<Paths>,
    memo: ReleaseMemo,
    /// The memo's candidate facts, for skipping downloads.
    facts: FactIndex,
    http: &'a dyn Http,
    registry: &'a SourceRegistry,
    policy: Freshness,
    now: Timestamp,
    cap: usize,
    /// Every answer of this run, so a later call never fetches the same wish again.
    run: Mutex<BTreeMap<Wanted, Fetched>>,
    /// What this run fetched, for the memo.
    learned: Mutex<ReleaseMemo>,
    /// The oldest remembered answer used whatever its age.
    stale: Mutex<Option<Timestamp>>,
    notes: Vec<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<'a> ReleaseCache<'a> {
    /// A cache over the memo under `paths`, fetching through `http` and `registry`.
    pub fn new(
        paths: &Paths,
        http: &'a dyn Http,
        registry: &'a SourceRegistry,
        policy: Freshness,
    ) -> Self {
        let (memo, note) = memo::load(paths);
        Self {
            paths: Some(paths.clone()),
            facts: FactIndex::new(&memo.candidates),
            memo,
            http,
            registry,
            policy,
            now: Timestamp::now(),
            cap: RELEASE_MEMO_MAX_ENTRIES,
            run: Mutex::new(BTreeMap::new()),
            learned: Mutex::new(ReleaseMemo::default()),
            stale: Mutex::new(None),
            notes: note.into_iter().collect(),
        }
    }

    /// A cache that only de-duplicates within the run: no memo is read or written.
    pub fn transient(http: &'a dyn Http, registry: &'a SourceRegistry) -> Self {
        Self {
            paths: None,
            memo: ReleaseMemo::default(),
            facts: FactIndex::default(),
            http,
            registry,
            policy: Freshness::Refresh,
            now: Timestamp::now(),
            cap: RELEASE_MEMO_MAX_ENTRIES,
            run: Mutex::new(BTreeMap::new()),
            learned: Mutex::new(ReleaseMemo::default()),
            stale: Mutex::new(None),
            notes: Vec::new(),
        }
    }

    /// Decides freshness as of `now` instead of the construction time.
    #[must_use]
    pub fn at(mut self, now: Timestamp) -> Self {
        self.now = now;
        self
    }

    /// Keeps at most `cap` memo entries instead of `RELEASE_MEMO_MAX_ENTRIES`.
    #[must_use]
    pub fn capped(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// Whether `wanted` goes through the memo: a release fetched from a remote source
    /// without a pinned version. Local and `url:` refs are read now (their release is
    /// the file or the URL itself), and a pinned version must see that exact release.
    fn memoized(&self, wanted: &Wanted) -> bool {
        self.paths.is_some()
            && wanted.version.is_none()
            && !matches!(SourceId::of(&wanted.r#ref), SourceId::Local | SourceId::Url)
    }

    /// The memo entry of `wanted`, if it goes through the memo.
    fn remembered(&self, wanted: &Wanted) -> Option<&MemoEntry> {
        if !self.memoized(wanted) {
            return None;
        }
        self.memo
            .entries
            .get(&memo::key(&wanted.r#ref, wanted.channel))
    }

    /// The release sets of `wanted`, each fetched at most once in the run: answered from
    /// the memo as the freshness policy allows, else fetched (one batch per source,
    /// sources in parallel) and remembered. A failed fetch is returned as it failed.
    pub fn get(&self, wanted: &[Wanted]) -> BTreeMap<Wanted, Fetched> {
        let mut answers = BTreeMap::new();
        let mut to_fetch: Vec<Wanted> = Vec::new();
        {
            let run = lock(&self.run);
            for w in wanted {
                if answers.contains_key(w) || to_fetch.contains(w) {
                    continue;
                }
                if let Some(known) = run.get(w) {
                    answers.insert(w.clone(), known.clone());
                    continue;
                }
                let entry = self.remembered(w);
                match (decide(entry, self.policy, self.now), entry) {
                    (Decision::Answer(origin), Some(entry)) => {
                        if origin == Origin::StaleFallback {
                            let mut oldest = lock(&self.stale);
                            if oldest.is_none_or(|o| entry.fetched_at < o) {
                                *oldest = Some(entry.fetched_at);
                            }
                        }
                        let answer = Fetched {
                            set: entry.result(),
                            origin,
                            fetched_at: entry.fetched_at,
                        };
                        answers.insert(w.clone(), answer);
                    }
                    _ => to_fetch.push(w.clone()),
                }
            }
        }
        if !to_fetch.is_empty() {
            let fetched_at = Timestamp::now();
            let results = fetch_once(to_fetch, self.registry, self.http);
            let mut learned = lock(&self.learned);
            for (w, set) in results {
                if self.memoized(&w)
                    && let Some(entry) = MemoEntry::of(&w.r#ref, w.channel, &set, fetched_at)
                {
                    learned
                        .entries
                        .insert(memo::key(&w.r#ref, w.channel), entry);
                }
                let answer = Fetched {
                    set,
                    origin: Origin::Fetched,
                    fetched_at,
                };
                answers.insert(w, answer);
            }
        }
        let mut run = lock(&self.run);
        for (w, answer) in &answers {
            run.entry(w.clone()).or_insert_with(|| answer.clone());
        }
        answers
    }

    /// The remembered answer for `wanted` whatever its age, for a read-only command whose
    /// fetch failed. Never used by a command that changes addons.
    pub fn stale(&self, wanted: &Wanted) -> Option<Fetched> {
        let entry = self.entry(&wanted.r#ref, wanted.channel)?;
        Some(Fetched {
            set: entry.result(),
            origin: Origin::StaleFallback,
            fetched_at: entry.fetched_at,
        })
    }

    /// What earlier commands learned by downloading candidates.
    pub fn facts(&self) -> &FactIndex {
        &self.facts
    }

    /// The Tukui list, for callers without a `tukui:` ref (dependencies, adoption,
    /// search): the one this command already decoded; else the memo's while it is
    /// younger than `SOURCE_RECHECK_MIN_INTERVAL`, with no request (always fetched under
    /// `--refresh`); else one `tukui-list` request, which `fetch` then shares. Offline,
    /// the memo's at any age, with `stale_check`; without one, the network guard's
    /// `offline`. `source_disabled` when Tukui is off.
    pub fn tukui_list(&self) -> Result<TukuiList> {
        let Some(source) = self.registry.tukui() else {
            return Err(self.registry.unavailable(SourceId::Tukui, "the Tukui list"));
        };
        if let Some(decoded) = source.decoded() {
            return Ok(decoded.memo());
        }
        if let Some(list) = &self.memo.tukui_list {
            match self.policy {
                Freshness::OfflineOnly => {
                    let mut oldest = lock(&self.stale);
                    if oldest.is_none_or(|o| list.fetched_at < o) {
                        *oldest = Some(list.fetched_at);
                    }
                    return Ok(list.clone());
                }
                Freshness::Refresh => {}
                Freshness::Window(_) | Freshness::Revalidate => {
                    if list.fresh(self.now, SOURCE_RECHECK_MIN_INTERVAL) {
                        return Ok(list.clone());
                    }
                }
            }
        }
        source.list(self.http).map(|decoded| decoded.memo())
    }

    /// The memo entry of `r` on `channel` as this run knows it: fetched in this run,
    /// else remembered.
    pub fn entry(&self, r: &Ref, channel: Channel) -> Option<MemoEntry> {
        let key = memo::key(r, channel);
        lock(&self.learned)
            .entries
            .get(&key)
            .or_else(|| self.memo.entries.get(&key))
            .cloned()
    }

    /// Writes what this run learned (and `facts`) to the memo, once per command. The
    /// warnings join the outcome: `stale_check` when an answer came from the memo
    /// whatever its age. The notes are for `-v`: a memo ignored or not written.
    pub fn remember(self, facts: &[CandidateFact]) -> Remembered {
        let mut remembered = Remembered {
            warnings: Vec::new(),
            notes: self.notes,
        };
        if let Some(oldest) = self
            .stale
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
        {
            remembered.warnings.push(
                Warning::new(
                    WarningCode::StaleCheck,
                    format!(
                        "offline: answered from release metadata remembered from {oldest} or later; the sources were not checked"
                    ),
                )
                .with_fix(Suggestion::text(
                    "run without --offline and with OMADDONS_OFFLINE unset",
                )),
            );
        }
        let Some(paths) = self.paths else {
            return remembered;
        };
        let mut learned = self
            .learned
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        learned.candidates.extend(facts.iter().cloned());
        // A list decoded in this command replaces the memo's.
        learned.tukui_list = self
            .registry
            .tukui()
            .and_then(crate::sources::tukui::TukuiSource::decoded)
            .map(|decoded| decoded.memo());
        if learned != ReleaseMemo::default() {
            remembered
                .notes
                .extend(memo::write(&paths, learned, self.cap));
        }
        remembered
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::engine::testing::NoNetwork;
    use crate::error::{Error, ErrorCode};
    use crate::model::Release;
    use crate::sources::Source;

    /// Answers every wish with release 1, or fails with `fail`; counts its calls.
    struct Counting {
        id: SourceId,
        calls: Arc<AtomicUsize>,
        fail: Option<ErrorCode>,
    }

    impl Source for Counting {
        fn id(&self) -> SourceId {
            self.id
        }

        fn fetch(&self, wanted: &[Wanted], _: &dyn Http) -> Vec<Result<ReleaseSet>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            wanted
                .iter()
                .map(|w| match self.fail {
                    Some(code) => Err(Error::new(code, "failed", Suggestion::text("x"))),
                    None => Ok(ReleaseSet {
                        r#ref: w.r#ref.clone(),
                        releases: vec![Release {
                            release_id: format!("{}@1", w.r#ref),
                            version: "1".into(),
                            channel: Channel::Stable,
                            published_at: None,
                            assets: Vec::new(),
                            changelog_url: None,
                            notes: None,
                            name_hint: None,
                        }],
                    }),
                })
                .collect()
        }
    }

    struct Scene {
        _dir: tempfile::TempDir,
        paths: Paths,
        calls: Arc<AtomicUsize>,
        registry: SourceRegistry,
    }

    impl Scene {
        fn new(fail: Option<ErrorCode>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().to_path_buf();
            let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
            let calls = Arc::new(AtomicUsize::new(0));
            let mut registry = SourceRegistry::new();
            for id in [SourceId::Github, SourceId::Local] {
                registry.register(Box::new(Counting {
                    id,
                    calls: calls.clone(),
                    fail,
                }));
            }
            Self {
                _dir: dir,
                paths,
                calls,
                registry,
            }
        }

        fn cache(&self, policy: Freshness) -> ReleaseCache<'_> {
            ReleaseCache::new(&self.paths, &NoNetwork, &self.registry, policy)
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// One fetch of `wanted` through a fresh cache that then remembers.
        fn once(&self, policy: Freshness, wanted: &Wanted) -> Fetched {
            let cache = self.cache(policy);
            let answer = cache
                .get(std::slice::from_ref(wanted))
                .remove(wanted)
                .unwrap();
            cache.remember(&[]);
            answer
        }
    }

    const WINDOW: Freshness = Freshness::Window(Duration::from_secs(15 * 60));

    fn foo() -> Wanted {
        Wanted::stable("gh:o/foo".parse().unwrap())
    }

    #[test]
    fn the_freshness_decision_per_policy() {
        let now: Timestamp = "2026-10-15T12:00:00Z".parse().unwrap();
        let entry = |minutes: i64| {
            let set = Ok(ReleaseSet {
                r#ref: "gh:o/foo".parse().unwrap(),
                releases: Vec::new(),
            });
            let at = now - jiff::SignedDuration::from_mins(minutes);
            MemoEntry::of(&"gh:o/foo".parse().unwrap(), Channel::Stable, &set, at).unwrap()
        };
        let (fresh, stale) = (entry(5), entry(20));
        use Decision::{Answer, Fetch};
        for (policy, e, expected) in [
            (WINDOW, Some(&fresh), Answer(Origin::Memo)),
            (WINDOW, Some(&stale), Fetch),
            (WINDOW, None, Fetch),
            (Freshness::Revalidate, Some(&fresh), Fetch),
            (Freshness::Refresh, Some(&fresh), Fetch),
            (
                Freshness::OfflineOnly,
                Some(&stale),
                Answer(Origin::StaleFallback),
            ),
            (Freshness::OfflineOnly, None, Fetch),
        ] {
            assert_eq!(decide(e, policy, now), expected, "{policy:?}");
        }
    }

    #[test]
    fn each_wish_is_fetched_once_per_run() {
        let scene = Scene::new(None);
        let cache = scene.cache(Freshness::Refresh);
        let bar = Wanted::stable("gh:o/bar".parse().unwrap());
        let first = cache.get(&[foo(), foo(), bar.clone()]);
        assert_eq!(
            (first.len(), scene.calls()),
            (2, 1),
            "one batch, deduplicated"
        );
        let again = cache.get(&[foo(), bar]);
        assert_eq!(scene.calls(), 1, "the run already knows them");
        assert_eq!(again[&foo()].origin, Origin::Fetched);
    }

    #[test]
    fn a_window_answers_from_the_memo_and_refresh_fetches() {
        let scene = Scene::new(None);
        assert_eq!(scene.once(WINDOW, &foo()).origin, Origin::Fetched);
        let answer = scene.once(WINDOW, &foo());
        assert_eq!((answer.origin, scene.calls()), (Origin::Memo, 1));
        assert_eq!(answer.set.unwrap().releases[0].version, "1");
        assert_eq!(
            scene.once(Freshness::Refresh, &foo()).origin,
            Origin::Fetched
        );
        assert_eq!(
            scene.once(Freshness::Revalidate, &foo()).origin,
            Origin::Fetched
        );
        assert_eq!(scene.calls(), 3);
    }

    #[test]
    fn pinned_versions_and_local_refs_never_touch_the_memo() {
        let scene = Scene::new(None);
        let mut pinned = foo();
        pinned.version = Some("1".into());
        let local = Wanted::stable("file:/tmp/Foo.zip".parse().unwrap());
        for wanted in [&pinned, &local] {
            scene.once(WINDOW, wanted);
            scene.once(WINDOW, wanted);
        }
        assert_eq!(scene.calls(), 4, "fetched every time");
        assert!(!scene.paths.release_memo().exists(), "nothing written");
    }

    #[test]
    fn offline_answers_from_the_memo_whatever_its_age_with_stale_check() {
        let scene = Scene::new(None);
        scene.once(Freshness::Refresh, &foo());
        let cache = scene
            .cache(Freshness::OfflineOnly)
            .at(Timestamp::now() + jiff::SignedDuration::from_hours(48));
        let answer = cache.get(&[foo()]).remove(&foo()).unwrap();
        assert_eq!((answer.origin, scene.calls()), (Origin::StaleFallback, 1));
        let remembered = cache.remember(&[]);
        let codes: Vec<WarningCode> = remembered.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, [WarningCode::StaleCheck]);
    }

    #[test]
    fn only_failures_retrying_cannot_change_are_answered_from_the_memo() {
        let scene = Scene::new(Some(ErrorCode::NotFound));
        scene.once(WINDOW, &foo());
        let answer = scene.once(WINDOW, &foo());
        assert_eq!(answer.origin, Origin::Memo);
        assert_eq!(answer.set.unwrap_err().code, ErrorCode::NotFound);
        assert_eq!(scene.calls(), 1);

        let scene = Scene::new(Some(ErrorCode::Network));
        scene.once(WINDOW, &foo());
        let answer = scene.once(WINDOW, &foo());
        assert_eq!(answer.origin, Origin::Fetched, "a network error is retried");
        assert_eq!(scene.calls(), 2);
        assert!(!scene.paths.release_memo().exists());
    }

    #[test]
    fn a_failed_fetch_is_never_answered_from_the_memo_unless_asked() {
        let ok = Scene::new(None);
        ok.once(Freshness::Refresh, &foo());
        // The same memo, a source that now fails.
        let failing = Scene::new(Some(ErrorCode::Network));
        std::fs::create_dir_all(failing.paths.cache_dir()).unwrap();
        std::fs::copy(ok.paths.release_memo(), failing.paths.release_memo()).unwrap();
        let cache = failing.cache(Freshness::Refresh);
        let answer = cache.get(&[foo()]).remove(&foo()).unwrap();
        assert_eq!(answer.set.unwrap_err().code, ErrorCode::Network);
        let stale = cache.stale(&foo()).unwrap();
        assert_eq!(stale.origin, Origin::StaleFallback);
        assert!(stale.set.is_ok(), "the read-only fallback, on request");
        assert!(cache.entry(&foo().r#ref, Channel::Stable).is_some());
    }

    #[test]
    fn the_memo_keeps_at_most_its_cap() {
        let scene = Scene::new(None);
        let cache = scene.cache(Freshness::Refresh).capped(1);
        let bar = Wanted::stable("gh:o/bar".parse().unwrap());
        cache.get(&[foo(), bar]);
        cache.remember(&[]);
        let (memo, _) = memo::load(&scene.paths);
        assert_eq!(memo.entries.len(), 1);
    }

    #[test]
    fn a_transient_cache_writes_nothing() {
        let scene = Scene::new(None);
        let cache = ReleaseCache::transient(&NoNetwork, &scene.registry);
        cache.get(&[foo()]);
        let remembered = cache.remember(&[]);
        assert!(remembered.warnings.is_empty() && remembered.notes.is_empty());
        assert!(!scene.paths.release_memo().exists());
    }

    /// Tukui's list (ElvUI), counting requests; refused as the guard refuses offline.
    #[derive(Default)]
    struct TukuiApi {
        sent: AtomicUsize,
        offline: bool,
    }

    impl Http for TukuiApi {
        fn send(&self, req: crate::net::Request) -> Result<crate::net::Response> {
            if self.offline {
                return Err(Error::new(
                    ErrorCode::Offline,
                    "offline",
                    Suggestion::text("x"),
                ));
            }
            self.sent.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.purpose, "tukui-list");
            let body = br#"[{"id":-2,"slug":"elvui","name":"ElvUI","version":"16.00","patch":["12.1.0"],"directories":["ElvUI"],"last_update":"2026-10-13","url":"https://api.tukui.org/v1/download/elvui/T"}]"#;
            Ok(crate::net::Response::new(
                200,
                Vec::new(),
                req.url,
                Box::new(std::io::Cursor::new(body.to_vec())),
            ))
        }
    }

    #[test]
    fn the_tukui_list_is_one_request_per_command_and_remembered() {
        use crate::sources::SourceConfigView;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
        let all = SourceConfigView::all_enabled();
        let api = TukuiApi::default();
        let sent = || api.sent.load(Ordering::SeqCst);

        // Nothing remembered: one request, shared with fetch, then remembered.
        let registry = SourceRegistry::builtin(&all);
        let cache = ReleaseCache::new(&paths, &api, &registry, Freshness::Revalidate);
        assert_eq!(cache.tukui_list().unwrap().entries[0].slug, "elvui");
        let elvui = Wanted::stable("tukui:elvui".parse().unwrap());
        assert!(cache.get(std::slice::from_ref(&elvui))[&elvui].set.is_ok());
        assert_eq!(sent(), 1, "fetch shares the list");
        let remembered = cache.remember(&[]);
        assert!(remembered.notes.is_empty(), "{:?}", remembered.notes);
        let (memo, _) = memo::load(&paths);
        assert_eq!(memo.tukui_list.unwrap().entries.len(), 1);

        // A new command inside the window: the memo answers.
        let registry = SourceRegistry::builtin(&all);
        let cache = ReleaseCache::new(&paths, &api, &registry, Freshness::Revalidate);
        assert!(cache.tukui_list().is_ok());
        assert_eq!(sent(), 1);

        // Past the window: fetched once, however often asked.
        let later = Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let registry = SourceRegistry::builtin(&all);
        let cache = ReleaseCache::new(&paths, &api, &registry, Freshness::Revalidate).at(later);
        assert!(cache.tukui_list().is_ok() && cache.tukui_list().is_ok());
        assert_eq!(sent(), 2);

        // Offline: the memo's list at any age, said with stale_check.
        let offline = TukuiApi {
            offline: true,
            ..TukuiApi::default()
        };
        let much_later = later + jiff::SignedDuration::from_hours(240);
        let registry = SourceRegistry::builtin(&all);
        let cache =
            ReleaseCache::new(&paths, &offline, &registry, Freshness::OfflineOnly).at(much_later);
        assert!(cache.tukui_list().is_ok());
        let codes: Vec<WarningCode> = cache
            .remember(&[])
            .warnings
            .iter()
            .map(|w| w.code)
            .collect();
        assert_eq!(codes, [WarningCode::StaleCheck]);
        // Offline with nothing remembered: offline.
        let empty = tempfile::tempdir().unwrap();
        let nowhere = Paths::with_roots(empty.path(), empty.path(), empty.path(), empty.path());
        let cache = ReleaseCache::new(&nowhere, &offline, &registry, Freshness::OfflineOnly);
        assert_eq!(cache.tukui_list().unwrap_err().code, ErrorCode::Offline);

        // Tukui turned off: source_disabled.
        let sources = [(
            "tukui".to_owned(),
            crate::state::config::SourceConfig {
                enabled: Some(false),
                credential: None,
            },
        )]
        .into();
        let off = SourceRegistry::builtin(&SourceConfigView::from_config(&sources));
        let cache = ReleaseCache::new(&paths, &api, &off, Freshness::Revalidate);
        assert_eq!(
            cache.tukui_list().unwrap_err().code,
            ErrorCode::SourceDisabled
        );
    }
}
