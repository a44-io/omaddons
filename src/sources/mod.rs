//! Addon sources. `fetch` does the network work once per distinct ref per run;
//! selection and verification are pure and live in `resolve`.
//!
//! [`fetch_once`] deduplicates refs (D28: requests never multiply by target count),
//! groups them by [`SourceId`], and calls each registered source's [`Source::fetch`]
//! exactly once, groups in parallel. Errors are per ref. Refs of sources this build does
//! not have (WoWInterface and Tukui arrive in M2, CurseForge and Wago in M5) fail with
//! `source_disabled` without touching the network (DEC-12).

pub mod atom;
pub mod github;
pub mod refs;
pub mod release_json;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{Channel, Ref, ReleaseSet};
use crate::net::{Http, run_bounded};
use crate::politeness::MAX_CONCURRENT_REQUESTS;

/// Which source a ref belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
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
}

impl SourceId {
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
        }
    }

    /// The source's name and the milestone it arrives in, for sources this build lacks.
    const fn later(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Wowi => Some(("WoWInterface", "M2")),
            Self::Tukui => Some(("Tukui", "M2")),
            Self::Curseforge => Some(("CurseForge", "M5")),
            Self::Wago => Some(("Wago", "M5")),
            Self::Github | Self::Git | Self::Url | Self::Local => None,
        }
    }
}

/// One addon source (vision §5.4).
pub trait Source: Send + Sync {
    fn id(&self) -> SourceId;
    /// Network; batched; called once per run with every distinct wish for this source.
    /// Result `i` belongs to `wanted[i]`.
    fn fetch(&self, wanted: &[Wanted], http: &dyn Http) -> Vec<Result<ReleaseSet>>;
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
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every source this build ships.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(github::GithubSource::new()));
        registry
    }

    /// Adds a source, replacing one with the same id.
    pub fn register(&mut self, source: Box<dyn Source>) {
        self.sources.insert(source.id(), source);
    }

    pub fn get(&self, id: SourceId) -> Option<&dyn Source> {
        self.sources.get(&id).map(Box::as_ref)
    }
}

/// `source_disabled` for a ref whose source this build does not have.
pub fn disabled(r: &Ref) -> Error {
    let id = SourceId::of(r);
    let message = match id.later() {
        Some((name, milestone)) => format!("{r}: {name} arrives in {milestone}"),
        None => format!("{r}: the {} source is not available", id.wire()),
    };
    Error::new(
        ErrorCode::SourceDisabled,
        message,
        Suggestion::command(["omaddons", "help", "install"]),
    )
}

/// Fetches every distinct ref once: one `fetch` call per source, sources in parallel.
pub fn fetch_once(
    wanted: impl IntoIterator<Item = Wanted>,
    registry: &SourceRegistry,
    http: &dyn Http,
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
                    let error = disabled(&w.r#ref);
                    results.insert(w, Err(error));
                }
            }
        }
    }
    let fetched = run_bounded(work, MAX_CONCURRENT_REQUESTS, |(source, group)| {
        let sets = source.fetch(&group, http);
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
            "wowi:5108",
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
        let wowi = results[&w("wowi:5108")].as_ref().unwrap_err();
        assert_eq!(
            (wowi.code, wowi.exit().code()),
            (ErrorCode::SourceDisabled, 1)
        );
        assert!(wowi.message.contains("M2"), "{}", wowi.message);
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
    }
}
