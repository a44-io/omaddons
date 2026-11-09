//! `ReleaseCache`: the one way a command gets release metadata (M2-D37). Within a run it
//! fetches each wish once, however many engine runs and targets ask (the probe run and
//! the real run of `install` share one cache). Across runs it answers from the release
//! memo as far as the command's [`Freshness`] allows, and remembers what it fetched.
//!
//! It never answers a failed fetch from the memo on its own: a mutating command must not
//! install from metadata the source may have withdrawn. A read-only command that may
//! fall back asks for the remembered answer with [`ReleaseCache::stale`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::select::newest;
use crate::error::{Result, Suggestion, Warning, WarningCode};
use crate::limits::RELEASE_MEMO_MAX_ENTRIES;
use crate::model::{Actor, Channel, Ref, ReleaseSet};
use crate::net::{Http, run_bounded};
use crate::output::{Phase, ProgressSink, Subject, count, trace};
use crate::politeness::{MAX_CONCURRENT_REQUESTS, SOURCE_RECHECK_MIN_INTERVAL};
use crate::sources::{SourceId, SourceRegistry, Wanted, fetch_once_reporting};
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

impl Freshness {
    /// What `force_revalidate` (`apply`, M4-D20) makes of a command's own choice: a memo
    /// window becomes `Revalidate`; `Revalidate`, `Refresh` and `OfflineOnly` stand.
    #[must_use]
    pub fn forced(self, force_revalidate: bool) -> Self {
        match self {
            Self::Window(_) if force_revalidate => Self::Revalidate,
            other => other,
        }
    }
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

impl Origin {
    /// The origin as it serializes.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Memo => "memo",
            Self::Probed => "probed",
            Self::Fetched => "fetched",
            Self::StaleFallback => "stale_fallback",
        }
    }
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
    /// Ask the source's cheap probe whether the remembered set still holds.
    Probe,
    Fetch,
}

/// The pure freshness decision for a wish whose memo entry is `entry`, at `now`.
fn decide(entry: Option<&MemoEntry>, policy: Freshness, now: Timestamp) -> Decision {
    match (policy, entry) {
        (Freshness::OfflineOnly, Some(_)) => Decision::Answer(Origin::StaleFallback),
        (Freshness::Window(window), Some(entry)) if entry.fresh(now, window) => {
            Decision::Answer(Origin::Memo)
        }
        // A stale (or revalidated) set may be confirmed by a probe; a stored failure,
        // or nothing remembered, is fetched (the fetch asks what a probe would anyway).
        (Freshness::Window(_) | Freshness::Revalidate, Some(entry))
            if entry.set.is_some() && entry.probe.is_some() =>
        {
            Decision::Probe
        }
        // Offline with nothing remembered: the network guard refuses the fetch with
        // `offline`, and nothing reaches the network.
        _ => Decision::Fetch,
    }
}

/// The wish a trace line names: the memo's key (`gh:o/foo|stable`), or only its kind
/// for a local or `url:` ref, whose path or query may say more than a trace should.
fn traced(w: &Wanted) -> String {
    match SourceId::of(&w.r#ref) {
        SourceId::Local => "(local ref)".to_owned(),
        SourceId::Url => "(url ref)".to_owned(),
        _ => memo::key(&w.r#ref, w.channel),
    }
}

/// Whole seconds from `at` to `now`; 0 when `at` lies in the future.
fn age_s(at: Timestamp, now: Timestamp) -> i64 {
    now.duration_since(at).as_secs().max(0)
}

/// The `-vv` line of one wish's freshness decision: why it is answered from the memo,
/// probed or fetched. `transient` is a cache without a memo file.
fn decision_line(
    w: &Wanted,
    transient: bool,
    entry: Option<&MemoEntry>,
    policy: Freshness,
    decision: Decision,
    now: Timestamp,
) -> String {
    let head = format!("release memo {}", traced(w));
    if matches!(SourceId::of(&w.r#ref), SourceId::Local | SourceId::Url) {
        return format!("{head}: not memoized; read now");
    }
    if transient {
        return format!("{head}: no memo for this command; fetching");
    }
    if let Some(version) = &w.version {
        return format!("{head}: version {version} pinned, not memoized; fetching");
    }
    let age = |e: &MemoEntry| age_s(e.fetched_at, now);
    let why = match (decision, entry) {
        (Decision::Answer(Origin::StaleFallback), Some(e)) => format!(
            "offline, answered from the memo whatever its age (fetched {} s ago)",
            age(e)
        ),
        (Decision::Answer(_), Some(e)) => {
            let window = match policy {
                Freshness::Window(window) => window.as_secs(),
                _ => 0,
            };
            format!(
                "fresh, answered from the memo (fetched {} s ago, window {window} s); no request",
                age(e)
            )
        }
        (Decision::Probe, Some(e)) if policy == Freshness::Revalidate => format!(
            "revalidating (fetched {} s ago), probing the source",
            age(e)
        ),
        (Decision::Probe, Some(e)) => {
            format!("stale (fetched {} s ago), probing the source", age(e))
        }
        _ if policy == Freshness::Refresh => "--refresh, fetching".to_owned(),
        (_, None) if policy == Freshness::OfflineOnly => {
            "offline and not remembered; the fetch is refused offline".to_owned()
        }
        (_, None) => "not remembered, fetching".to_owned(),
        (_, Some(e)) if e.set.is_none() => {
            format!("remembered a failure (fetched {} s ago), fetching", age(e))
        }
        (_, Some(e)) if policy == Freshness::Revalidate => format!(
            "revalidating, and the source offers no probe (fetched {} s ago); fetching",
            age(e)
        ),
        (_, Some(e)) => format!(
            "stale, and the source offers no probe (fetched {} s ago); fetching",
            age(e)
        ),
    };
    format!("{head}: {why}")
}

/// The `-vv` line of one fetched wish: its newest release, or its failure, and whether
/// the memo keeps it.
fn fetch_line(w: &Wanted, set: &Result<ReleaseSet>, remembered: bool) -> String {
    let kept = if remembered {
        "remembered"
    } else {
        "not remembered"
    };
    let head = format!("release fetch {}", traced(w));
    match set {
        Ok(set) => match newest(set, w.channel) {
            Some(top) => format!(
                "{head}: newest {} ({}), {kept}",
                top.version,
                count(set.releases.len(), "release")
            ),
            None => format!("{head}: no release in this channel, {kept}"),
        },
        Err(error) => format!("{head}: failed ({}), {kept}", error.code.wire()),
    }
}

/// Where a cache reads and writes the memo, and whom a changed memo rings the doorbell
/// for.
struct MemoFile<'a> {
    paths: Paths,
    /// The command, as its envelope names it.
    command: &'a str,
    actor: Actor,
}

/// The release metadata of one command run.
pub struct ReleaseCache<'a> {
    /// `None` for a transient cache, which reads and writes no memo.
    file: Option<MemoFile<'a>>,
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
    /// Where each ref asked of a source is reported as it is answered (`fetch`).
    progress: ProgressSink,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<'a> ReleaseCache<'a> {
    /// A cache over the memo under `paths`, fetching through `http` and `registry`, for
    /// `command` run by `actor`: a memo it changes rings the doorbell for them.
    pub fn new(
        paths: &Paths,
        http: &'a dyn Http,
        registry: &'a SourceRegistry,
        policy: Freshness,
        command: &'a str,
        actor: Actor,
    ) -> Self {
        let (memo, note) = memo::load(paths);
        Self {
            file: Some(MemoFile {
                paths: paths.clone(),
                command,
                actor,
            }),
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
            progress: ProgressSink::none(),
        }
    }

    /// A cache that only de-duplicates within the run: no memo is read or written.
    pub fn transient(http: &'a dyn Http, registry: &'a SourceRegistry) -> Self {
        Self {
            file: None,
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
            progress: ProgressSink::none(),
        }
    }

    /// Reports each ref asked of a source to `progress` as it is answered.
    #[must_use]
    pub fn with_progress(mut self, progress: ProgressSink) -> Self {
        self.progress = progress;
        self
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
        self.file.is_some()
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
        let mut to_probe: Vec<(Wanted, MemoEntry)> = Vec::new();
        {
            let run = lock(&self.run);
            for w in wanted {
                if answers.contains_key(w)
                    || to_fetch.contains(w)
                    || to_probe.iter().any(|(p, _)| p == w)
                {
                    continue;
                }
                if let Some(known) = run.get(w) {
                    trace::emit(|| {
                        format!(
                            "release memo {}: answered earlier in this run ({})",
                            traced(w),
                            known.origin.wire()
                        )
                    });
                    answers.insert(w.clone(), known.clone());
                    continue;
                }
                let entry = self.remembered(w);
                let decision = decide(entry, self.policy, self.now);
                trace::emit(|| {
                    decision_line(
                        w,
                        self.file.is_none(),
                        entry,
                        self.policy,
                        decision,
                        self.now,
                    )
                });
                match (decision, entry) {
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
                    (Decision::Probe, Some(entry)) => to_probe.push((w.clone(), entry.clone())),
                    _ => to_fetch.push(w.clone()),
                }
            }
        }
        to_fetch.extend(self.probe(to_probe, &mut answers));
        if !to_fetch.is_empty() {
            let fetched_at = Timestamp::now();
            // Progress as each ref answers: the phase grows by what goes to a source (a
            // disabled one is asked nothing).
            let asked = to_fetch
                .iter()
                .filter(|w| self.registry.get(SourceId::of(&w.r#ref)).is_some())
                .count() as u64;
            self.progress.grow(Phase::Fetch, asked);
            let report = |w: &Wanted| {
                let r#ref = w.r#ref.to_string();
                self.progress
                    .event(Phase::Fetch, asked, &r#ref, Subject::of_ref(&r#ref));
            };
            let refs = to_fetch.len();
            let sources = to_fetch
                .iter()
                .map(|w| SourceId::of(&w.r#ref))
                .collect::<BTreeSet<_>>()
                .len();
            let started = Instant::now();
            let results = fetch_once_reporting(to_fetch, self.registry, self.http, &report);
            let took = started.elapsed().as_millis();
            trace::emit(|| {
                format!(
                    "release fetch: {} from {} in {took} ms",
                    count(refs, "ref"),
                    count(sources, "source")
                )
            });
            let mut learned = lock(&self.learned);
            for (w, set) in results {
                let entry = self
                    .memoized(&w)
                    .then(|| MemoEntry::of(&w.r#ref, w.channel, &set, fetched_at))
                    .flatten();
                trace::emit(|| fetch_line(&w, &set, entry.is_some()));
                if let Some(entry) = entry {
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

    /// Probes each remembered entry (in bounded parallel, through the same network
    /// stack): an equal token answers with the remembered set and refreshes its
    /// `fetched_at` (`Probed`); a probe error is the fetch's (remembered only when a
    /// fetch error would be); a different token, or a source without a probe, is
    /// returned to be fetched.
    fn probe(
        &self,
        entries: Vec<(Wanted, MemoEntry)>,
        answers: &mut BTreeMap<Wanted, Fetched>,
    ) -> Vec<Wanted> {
        if entries.is_empty() {
            return Vec::new();
        }
        let (registry, http) = (self.registry, self.http);
        // Each worker measures its probe and returns the time; the lines are written
        // here, on the caller's thread, in the order of the wishes.
        let probed = run_bounded(entries, MAX_CONCURRENT_REQUESTS, |(w, entry)| {
            let started = Instant::now();
            let token = registry
                .get(SourceId::of(&w.r#ref))
                .and_then(|source| source.probe(&w, http));
            (w, entry, token, started.elapsed())
        });
        let now = Timestamp::now();
        let mut learned = lock(&self.learned);
        let mut to_fetch = Vec::new();
        for (w, mut entry, token, took) in probed {
            let key = memo::key(&w.r#ref, w.channel);
            let took = took.as_millis();
            match token {
                // ubs:ignore — a probe token is a public release id, not a secret
                Some(Ok(token)) if entry.probe.as_deref() == Some(token.as_str()) => {
                    trace::emit(|| {
                        format!(
                            "release probe {key}: token unchanged, kept the remembered set ({took} ms)"
                        )
                    });
                    entry.fetched_at = now;
                    let answer = Fetched {
                        set: entry.result(),
                        origin: Origin::Probed,
                        fetched_at: now,
                    };
                    learned.entries.insert(key, entry);
                    answers.insert(w, answer);
                }
                Some(Err(error)) => {
                    trace::emit(|| {
                        format!(
                            "release probe {key}: failed ({}), answered with the failure ({took} ms)",
                            error.code.wire()
                        )
                    });
                    let set = Err(error);
                    if let Some(entry) = MemoEntry::of(&w.r#ref, w.channel, &set, now) {
                        learned.entries.insert(key, entry);
                    }
                    let answer = Fetched {
                        set,
                        origin: Origin::Fetched,
                        fetched_at: now,
                    };
                    answers.insert(w, answer);
                }
                Some(Ok(_)) => {
                    trace::emit(|| {
                        format!("release probe {key}: token changed, fetching ({took} ms)")
                    });
                    to_fetch.push(w);
                }
                None => {
                    trace::emit(|| {
                        format!("release probe {key}: no probe answer, fetching ({took} ms)")
                    });
                    to_fetch.push(w);
                }
            }
        }
        to_fetch
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

    /// How far this command trusts remembered metadata.
    pub fn freshness(&self) -> Freshness {
        self.policy
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
    /// The release a look-back finds for `family`'s clients among `wanted`'s releases
    /// older than `newest` (a tag): the memo's answer while it was made against `newest`,
    /// whatever its age, else the source's, remembered. `None` when the source has no
    /// look-back.
    pub fn look_back(
        &self,
        wanted: &Wanted,
        newest: &str,
        family: crate::rules::Family,
        names_client: &(dyn Fn(&crate::model::AssetFlavor) -> bool + Sync),
    ) -> Option<Result<crate::sources::LookBack>> {
        if let Some(remembered) =
            self.remembered_look_back(&wanted.r#ref, wanted.channel, family, newest)
        {
            return Some(Ok(remembered));
        }
        let source = self
            .registry
            .get(crate::sources::SourceId::of(&wanted.r#ref))?;
        let found = source.look_back(wanted, newest, names_client, self.http)?;
        if let Ok(look_back) = &found {
            let key = memo::look_back_key(&wanted.r#ref, wanted.channel, family);
            lock(&self.learned).look_backs.insert(
                key,
                memo::LookBackMemo {
                    newest: newest.to_owned(),
                    look_back: look_back.clone(),
                    at: self.now,
                },
            );
        }
        Some(found)
    }

    /// The look-back remembered for `family`'s clients, made against `newest`: what
    /// update statuses read, with no request.
    pub fn remembered_look_back(
        &self,
        r: &Ref,
        channel: Channel,
        family: crate::rules::Family,
        newest: &str,
    ) -> Option<crate::sources::LookBack> {
        lock(&self.learned)
            .look_back(r, channel, family, newest)
            .or_else(|| self.memo.look_back(r, channel, family, newest))
    }

    pub fn entry(&self, r: &Ref, channel: Channel) -> Option<MemoEntry> {
        let key = memo::key(r, channel);
        lock(&self.learned)
            .entries
            .get(&key)
            .or_else(|| self.memo.entries.get(&key))
            .cloned()
    }

    /// Adds `facts` to what this run learned, for [`ReleaseCache::remember`] to write:
    /// what a plan its binding refused downloaded (M4-D20), so the next run finds those
    /// archives in the cache by their hashes instead of downloading them again.
    pub fn learn(&self, facts: &[CandidateFact]) {
        lock(&self.learned).candidates.extend(facts.iter().cloned());
    }

    /// Writes what this run learned (and `facts`) to the memo, once per command, ringing
    /// the doorbell when that changed the memo. The warnings join the outcome:
    /// `stale_check` when an answer came from the memo whatever its age. The notes are
    /// for `-v`: a memo ignored or not written.
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
        let Some(file) = self.file else {
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
            remembered.notes.extend(memo::write(
                &file.paths,
                learned,
                self.cap,
                file.command,
                file.actor,
            ));
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

    #[test]
    fn forced_turns_only_a_window_into_revalidate() {
        let window = Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL);
        for (name, choice, force, want) in [
            ("unforced/window", window, false, window),
            (
                "unforced/revalidate",
                Freshness::Revalidate,
                false,
                Freshness::Revalidate,
            ),
            (
                "unforced/refresh",
                Freshness::Refresh,
                false,
                Freshness::Refresh,
            ),
            (
                "unforced/offline_only",
                Freshness::OfflineOnly,
                false,
                Freshness::OfflineOnly,
            ),
            ("forced/window", window, true, Freshness::Revalidate),
            (
                "forced/revalidate",
                Freshness::Revalidate,
                true,
                Freshness::Revalidate,
            ),
            (
                "forced/refresh",
                Freshness::Refresh,
                true,
                Freshness::Refresh,
            ),
            (
                "forced/offline_only",
                Freshness::OfflineOnly,
                true,
                Freshness::OfflineOnly,
            ),
        ] {
            assert_eq!(
                choice.forced(force),
                want,
                "{name}: {choice:?}.forced({force})"
            );
        }
    }

    /// What a test source's probe answers.
    #[derive(Clone, Copy)]
    enum Probe {
        /// No probe.
        Without,
        /// The newest release is `<ref>@<this>`.
        Tag(&'static str),
        Fail(ErrorCode),
    }

    /// Answers every wish with release 1, or fails with `fail`; counts its calls, and
    /// its probes.
    struct Counting {
        id: SourceId,
        calls: Arc<AtomicUsize>,
        fail: Option<ErrorCode>,
        probe: Probe,
        probes: Arc<AtomicUsize>,
    }

    impl Source for Counting {
        fn id(&self) -> SourceId {
            self.id
        }

        fn probe(&self, wanted: &Wanted, _: &dyn Http) -> Option<Result<String>> {
            let answer = match self.probe {
                Probe::Without => return None,
                Probe::Tag(tag) => Ok(format!("{}@{tag}", wanted.r#ref)),
                Probe::Fail(code) => Err(Error::new(code, "probe failed", Suggestion::text("x"))),
            };
            self.probes.fetch_add(1, Ordering::SeqCst);
            Some(answer)
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
                            revision: None,
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
        probes: Arc<AtomicUsize>,
        registry: SourceRegistry,
    }

    impl Scene {
        fn new(fail: Option<ErrorCode>) -> Self {
            Self::with(fail, Probe::Without)
        }

        fn with(fail: Option<ErrorCode>, probe: Probe) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().to_path_buf();
            let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
            let calls = Arc::new(AtomicUsize::new(0));
            let probes = Arc::new(AtomicUsize::new(0));
            let mut registry = SourceRegistry::new();
            for id in [SourceId::Github, SourceId::Local] {
                registry.register(Box::new(Counting {
                    id,
                    calls: calls.clone(),
                    fail,
                    probe,
                    probes: probes.clone(),
                }));
            }
            Self {
                _dir: dir,
                paths,
                calls,
                probes,
                registry,
            }
        }

        fn probes(&self) -> usize {
            self.probes.load(Ordering::SeqCst)
        }

        /// [`Scene::once`] as of `minutes` from now.
        fn later(&self, minutes: i64, policy: Freshness, wanted: &Wanted) -> Fetched {
            let at = Timestamp::now() + jiff::SignedDuration::from_mins(minutes);
            let cache = self.cache(policy).at(at);
            let answer = cache
                .get(std::slice::from_ref(wanted))
                .remove(wanted)
                .unwrap();
            cache.remember(&[]);
            answer
        }

        fn cache(&self, policy: Freshness) -> ReleaseCache<'_> {
            ReleaseCache::new(
                &self.paths,
                &NoNetwork,
                &self.registry,
                policy,
                "outdated",
                Actor::Human,
            )
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
    fn a_remembered_set_is_probed_once_stale_or_revalidated() {
        let now: Timestamp = "2026-10-15T12:00:00Z".parse().unwrap();
        let r: Ref = "gh:o/foo".parse().unwrap();
        let entry = |minutes: i64, result: &Result<ReleaseSet>| {
            let at = now - jiff::SignedDuration::from_mins(minutes);
            MemoEntry::of(&r, Channel::Stable, result, at).unwrap()
        };
        let set = Ok(ReleaseSet {
            r#ref: r.clone(),
            releases: vec![Release {
                release_id: "gh:o/foo@1".into(),
                version: "1".into(),
                channel: Channel::Stable,
                published_at: None,
                assets: Vec::new(),
                changelog_url: None,
                notes: None,
                name_hint: None,
                revision: None,
            }],
        });
        let gone = Err(Error::new(
            ErrorCode::NotFound,
            "gone",
            Suggestion::text("x"),
        ));
        let (fresh, stale, failed) = (entry(5, &set), entry(20, &set), entry(20, &gone));
        use Decision::{Answer, Fetch, Probe};
        for (policy, e, expected) in [
            (WINDOW, Some(&fresh), Answer(Origin::Memo)),
            (WINDOW, Some(&stale), Probe),
            (Freshness::Revalidate, Some(&fresh), Probe),
            (WINDOW, Some(&failed), Fetch),
            (Freshness::Refresh, Some(&stale), Fetch),
            (
                Freshness::OfflineOnly,
                Some(&stale),
                Answer(Origin::StaleFallback),
            ),
        ] {
            assert_eq!(decide(e, policy, now), expected, "{policy:?}");
        }
    }

    #[test]
    fn an_equal_token_reuses_the_set_and_a_different_one_fetches() {
        let scene = Scene::with(None, Probe::Tag("1"));
        assert_eq!(scene.once(WINDOW, &foo()).origin, Origin::Fetched);
        assert_eq!(
            (scene.calls(), scene.probes()),
            (1, 0),
            "a cold check fetches"
        );
        let fetched_at = |scene: &Scene| {
            memo::load(&scene.paths).0.entries[&memo::key(&foo().r#ref, Channel::Stable)].fetched_at
        };
        let first = fetched_at(&scene);
        // Twenty minutes on: probed, confirmed, its fetch time moved to the probe's.
        let answer = scene.later(20, WINDOW, &foo());
        assert_eq!(
            (answer.origin, scene.calls(), scene.probes()),
            (Origin::Probed, 1, 1)
        );
        assert_eq!(answer.set.unwrap().releases[0].version, "1");
        assert!(fetched_at(&scene) > first, "fetched_at bumped");
        assert_eq!(scene.once(WINDOW, &foo()).origin, Origin::Memo);
        assert_eq!(scene.probes(), 1);
        // Revalidate probes even a fresh entry.
        assert_eq!(
            scene.once(Freshness::Revalidate, &foo()).origin,
            Origin::Probed
        );
        // Offline never probes.
        scene.later(60, Freshness::OfflineOnly, &foo());
        assert_eq!(scene.probes(), 2);

        let moved = Scene::with(None, Probe::Tag("2"));
        moved.once(WINDOW, &foo());
        let answer = moved.later(20, WINDOW, &foo());
        assert_eq!(
            (answer.origin, moved.calls(), moved.probes()),
            (Origin::Fetched, 2, 1)
        );
    }

    #[test]
    fn a_probe_error_is_the_fetchs_and_remembered_as_one_would_be() {
        for (code, remembered) in [(ErrorCode::NotFound, true), (ErrorCode::Network, false)] {
            let scene = Scene::with(None, Probe::Fail(code));
            scene.once(WINDOW, &foo());
            let answer = scene.later(20, WINDOW, &foo());
            assert_eq!(answer.set.unwrap_err().code, code);
            assert_eq!(scene.calls(), 1, "no fetch after a failed probe");
            // A not_found replaces the remembered set; a network error leaves it.
            let entry = memo::load(&scene.paths).0.entries
                [&memo::key(&foo().r#ref, Channel::Stable)]
                .clone();
            assert_eq!(
                (entry.set.is_none(), entry.failure.is_some()),
                (remembered, remembered),
                "{code:?}"
            );
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

    /// A release set with release `1` (so its entry has a probe token), an empty set
    /// (an entry without one) and a remembered failure, each fetched `minutes` before
    /// `now`.
    fn entries_at(now: Timestamp) -> impl Fn(i64, &str) -> MemoEntry {
        move |minutes, kind| {
            let r: Ref = "gh:o/foo".parse().unwrap();
            let release = Release {
                release_id: "gh:o/foo@1".into(),
                version: "1".into(),
                channel: Channel::Stable,
                published_at: None,
                assets: Vec::new(),
                changelog_url: None,
                notes: None,
                name_hint: None,
                revision: None,
            };
            let result = match kind {
                "set" => Ok(ReleaseSet {
                    r#ref: r.clone(),
                    releases: vec![release],
                }),
                "empty" => Ok(ReleaseSet {
                    r#ref: r.clone(),
                    releases: Vec::new(),
                }),
                _ => Err(Error::new(
                    ErrorCode::NotFound,
                    "no such repository",
                    Suggestion::text("x"),
                )),
            };
            let at = now - jiff::SignedDuration::from_mins(minutes);
            MemoEntry::of(&r, Channel::Stable, &result, at).unwrap()
        }
    }

    /// A decision case: its name, the wish, whether the cache is transient, the memo
    /// entry, the freshness policy and the line expected.
    type DecisionCase<'a> = (
        &'a str,
        &'a Wanted,
        bool,
        Option<&'a MemoEntry>,
        Freshness,
        String,
    );

    #[test]
    fn each_memo_decision_is_traced() {
        let now: Timestamp = "2026-10-15T12:00:00Z".parse().unwrap();
        let entry = entries_at(now);
        let (fresh, stale) = (entry(5, "set"), entry(20, "set"));
        let (no_probe, failed) = (entry(20, "empty"), entry(20, "failure"));
        let future = entry(-5, "set");
        let mut pinned = foo();
        pinned.version = Some("v2".into());
        let local = Wanted::stable("file:/tmp/Foo.zip".parse().unwrap());
        let url = Wanted::stable(
            "url:https://github.com/o/foo/releases/download/v1/Foo.zip"
                .parse()
                .unwrap(),
        );
        let key = "release memo gh:o/foo|stable";
        let cases: [DecisionCase<'_>; 15] = [
            (
                "fresh_window",
                &foo(),
                false,
                Some(&fresh),
                WINDOW,
                format!(
                    "{key}: fresh, answered from the memo (fetched 300 s ago, window 900 s); no request"
                ),
            ),
            (
                "offline_remembered",
                &foo(),
                false,
                Some(&stale),
                Freshness::OfflineOnly,
                format!(
                    "{key}: offline, answered from the memo whatever its age (fetched 1200 s ago)"
                ),
            ),
            (
                "stale_window_probes",
                &foo(),
                false,
                Some(&stale),
                WINDOW,
                format!("{key}: stale (fetched 1200 s ago), probing the source"),
            ),
            (
                "revalidate_probes",
                &foo(),
                false,
                Some(&fresh),
                Freshness::Revalidate,
                format!("{key}: revalidating (fetched 300 s ago), probing the source"),
            ),
            (
                "remembered_failure_fetches",
                &foo(),
                false,
                Some(&failed),
                WINDOW,
                format!("{key}: remembered a failure (fetched 1200 s ago), fetching"),
            ),
            (
                "no_probe_token_fetches",
                &foo(),
                false,
                Some(&no_probe),
                WINDOW,
                format!(
                    "{key}: stale, and the source offers no probe (fetched 1200 s ago); fetching"
                ),
            ),
            (
                "revalidate_without_a_probe_fetches",
                &foo(),
                false,
                Some(&no_probe),
                Freshness::Revalidate,
                format!(
                    "{key}: revalidating, and the source offers no probe (fetched 1200 s ago); fetching"
                ),
            ),
            (
                "refresh_fetches",
                &foo(),
                false,
                Some(&fresh),
                Freshness::Refresh,
                format!("{key}: --refresh, fetching"),
            ),
            (
                "offline_unremembered",
                &foo(),
                false,
                None,
                Freshness::OfflineOnly,
                format!("{key}: offline and not remembered; the fetch is refused offline"),
            ),
            (
                "not_remembered",
                &foo(),
                false,
                None,
                WINDOW,
                format!("{key}: not remembered, fetching"),
            ),
            (
                "pinned_version",
                &pinned,
                false,
                None,
                WINDOW,
                format!("{key}: version v2 pinned, not memoized; fetching"),
            ),
            (
                "transient_cache",
                &foo(),
                true,
                None,
                Freshness::Refresh,
                format!("{key}: no memo for this command; fetching"),
            ),
            (
                "local_ref",
                &local,
                false,
                None,
                WINDOW,
                "release memo (local ref): not memoized; read now".to_owned(),
            ),
            (
                "url_ref",
                &url,
                false,
                None,
                WINDOW,
                "release memo (url ref): not memoized; read now".to_owned(),
            ),
            (
                "future_entry_is_zero_seconds_old",
                &foo(),
                false,
                Some(&future),
                Freshness::OfflineOnly,
                format!(
                    "{key}: offline, answered from the memo whatever its age (fetched 0 s ago)"
                ),
            ),
        ];
        for (case, wanted, transient, e, policy, want) in cases {
            let decision = decide(e, policy, now);
            let line = decision_line(wanted, transient, e, policy, decision, now);
            assert_eq!(line, want, "{case}: {policy:?} decided {decision:?}");
        }
    }

    /// The number in a line's `(<n> ms)` or `in <n> ms`.
    fn millis(line: &str) -> u128 {
        let before = line
            .rsplit_once(" ms")
            .map(|(head, _)| head)
            .unwrap_or_default();
        let digits: String = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("no time in {line:?}"))
    }

    #[test]
    fn probes_and_fetches_are_traced_with_their_time() {
        let stale_line = |lines: &[String]| {
            let line = &lines[0];
            let age: i64 = line
                .strip_prefix("release memo gh:o/foo|stable: stale (fetched ")
                .and_then(|rest| rest.strip_suffix(" s ago), probing the source"))
                .and_then(|age| age.parse().ok())
                .unwrap_or_else(|| panic!("not a stale line: {line:?}"));
            assert!(age >= 1200, "20 minutes later: {line:?}");
        };
        let key = "release probe gh:o/foo|stable";
        let cases = [
            (
                "token_unchanged",
                Probe::Tag("1"),
                vec![format!("{key}: token unchanged, kept the remembered set (")],
            ),
            (
                "token_changed",
                Probe::Tag("2"),
                vec![
                    format!("{key}: token changed, fetching ("),
                    "release fetch: 1 ref from 1 source in ".to_owned(),
                    "release fetch gh:o/foo|stable: newest 1 (1 release), remembered".to_owned(),
                ],
            ),
            (
                "probe_failed",
                Probe::Fail(ErrorCode::Network),
                vec![format!(
                    "{key}: failed (network), answered with the failure ("
                )],
            ),
        ];
        for (case, probe, want) in cases {
            let scene = Scene::with(None, probe);
            scene.once(WINDOW, &foo());
            let (_, lines) = trace::capture(|| scene.later(20, WINDOW, &foo()));
            stale_line(&lines);
            assert_eq!(lines.len(), want.len() + 1, "{case}: {lines:?}");
            for (line, prefix) in lines[1..].iter().zip(&want) {
                assert!(line.starts_with(prefix.as_str()), "{case}: {line:?}");
                if line.ends_with(" ms)") || line.ends_with(" ms") {
                    assert!(millis(line) < 60_000, "{case}: a time in ms: {line:?}");
                }
            }
        }
    }

    #[test]
    fn a_wish_answered_earlier_in_the_run_says_so() {
        let scene = Scene::new(None);
        let cache = scene.cache(WINDOW);
        let (_, first) = trace::capture(|| cache.get(&[foo()]));
        assert_eq!(
            first[0], "release memo gh:o/foo|stable: not remembered, fetching",
            "{first:?}"
        );
        assert_eq!(
            first[2], "release fetch gh:o/foo|stable: newest 1 (1 release), remembered",
            "{first:?}"
        );
        let (_, again) = trace::capture(|| cache.get(&[foo()]));
        assert_eq!(
            again,
            ["release memo gh:o/foo|stable: answered earlier in this run (fetched)"]
        );
    }

    #[test]
    fn traces_never_name_a_local_or_url_refs_path_or_query() {
        let scene = Scene::new(None);
        let local = Wanted::stable("file:/tmp/private-dir/Foo.zip".parse().unwrap());
        let url = Wanted::stable(
            "url:https://github.com/o/foo/releases/download/v1/Foo.zip?sig=s3cret"
                .parse()
                .unwrap(),
        );
        let (_, lines) = trace::capture(|| scene.cache(WINDOW).get(&[local, url]));
        assert!(!lines.is_empty());
        for line in &lines {
            for private in ["private-dir", "s3cret", "Foo.zip"] {
                assert!(!line.contains(private), "{private} in {line:?}");
            }
        }
        assert!(
            lines.iter().any(|l| l.contains("(local ref)"))
                && lines.iter().any(|l| l.contains("(url ref)")),
            "{lines:?}"
        );
    }

    #[test]
    fn origins_serialize_as_their_wire_names() {
        for origin in [
            Origin::Memo,
            Origin::Probed,
            Origin::Fetched,
            Origin::StaleFallback,
        ] {
            assert_eq!(
                serde_json::to_value(origin).unwrap(),
                serde_json::Value::from(origin.wire()),
                "{origin:?}"
            );
        }
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
        let cache = ReleaseCache::new(
            &paths,
            &api,
            &registry,
            Freshness::Revalidate,
            "install",
            Actor::Human,
        );
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
        let cache = ReleaseCache::new(
            &paths,
            &api,
            &registry,
            Freshness::Revalidate,
            "install",
            Actor::Human,
        );
        assert!(cache.tukui_list().is_ok());
        assert_eq!(sent(), 1);

        // Past the window: fetched once, however often asked.
        let later = Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let registry = SourceRegistry::builtin(&all);
        let cache = ReleaseCache::new(
            &paths,
            &api,
            &registry,
            Freshness::Revalidate,
            "install",
            Actor::Human,
        )
        .at(later);
        assert!(cache.tukui_list().is_ok() && cache.tukui_list().is_ok());
        assert_eq!(sent(), 2);

        // Offline: the memo's list at any age, said with stale_check.
        let offline = TukuiApi {
            offline: true,
            ..TukuiApi::default()
        };
        let much_later = later + jiff::SignedDuration::from_hours(240);
        let registry = SourceRegistry::builtin(&all);
        let cache = ReleaseCache::new(
            &paths,
            &offline,
            &registry,
            Freshness::OfflineOnly,
            "install",
            Actor::Human,
        )
        .at(much_later);
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
        let cache = ReleaseCache::new(
            &nowhere,
            &offline,
            &registry,
            Freshness::OfflineOnly,
            "install",
            Actor::Human,
        );
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
        let cache = ReleaseCache::new(
            &paths,
            &api,
            &off,
            Freshness::Revalidate,
            "install",
            Actor::Human,
        );
        assert_eq!(
            cache.tukui_list().unwrap_err().code,
            ErrorCode::SourceDisabled
        );
    }

    /// What a refused plan downloaded, learned during the run, is written with the facts
    /// the command gives `remember` (M4-D20).
    #[test]
    fn facts_learned_during_the_run_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let paths = Paths::with_roots(&base, base.join("c"), base.join("s"), base.join("k"));
        let registry = SourceRegistry::builtin(&crate::sources::SourceConfigView::all_enabled());
        let fact = |release_id: &str| CandidateFact {
            release_id: release_id.to_owned(),
            asset: "Foo.zip".to_owned(),
            sha256: "ab".repeat(32),
            size: 3,
            verdicts: Vec::new(),
            unverified: false,
        };
        for (name, learned, given) in [
            ("learned alone", vec!["gh:o/foo@v3"], vec![]),
            (
                "learned and given",
                vec!["gh:o/foo@v3"],
                vec!["gh:o/bar@v1"],
            ),
            ("given alone", vec![], vec!["gh:o/bar@v1"]),
        ] {
            let cache = ReleaseCache::new(
                &paths,
                &NoNetwork,
                &registry,
                Freshness::Revalidate,
                "apply",
                Actor::Agent,
            );
            let facts = |ids: &[&str]| ids.iter().map(|id| fact(id)).collect::<Vec<_>>();
            cache.learn(&facts(&learned));
            let remembered = cache.remember(&facts(&given));
            assert!(
                remembered.notes.is_empty(),
                "{name}: {:?}",
                remembered.notes
            );
            let (memo, _) = memo::load(&paths);
            let mut ids: Vec<&str> = memo
                .candidates
                .iter()
                .map(|f| f.release_id.as_str())
                .collect();
            ids.sort_unstable();
            let mut want: Vec<&str> = learned.iter().chain(&given).copied().collect();
            want.sort_unstable();
            want.dedup();
            assert!(
                want.iter().all(|id| ids.contains(id)),
                "{name}: memo candidates {ids:?}, want {want:?}"
            );
        }
    }
}
