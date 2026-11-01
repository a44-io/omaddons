//! Fences (M4-D62): the paths a stuck journal protects, which no write may touch until
//! that journal is settled or set aside. They are enforced here, at the one writer: a
//! command loads its fences once into its [`ScopeTable`](super::ScopeTable), `mutate()`
//! refuses an operation that overlaps one, and `prune()` skips a victim one covers.
//!
//! This module decides and never writes. It knows nothing of journals: the engine turns
//! what a stuck journal protects into fences of canonical paths, each with the stop the
//! refused command reports.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::error::{ErrorCode, Suggestion};
use crate::model::{TargetKey, TxId};

/// What a write refused by a fence reports: the fencing journal's own stop (M4-D62).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceStop {
    /// The fencing journal; `None` only for a journal directory that cannot be listed.
    pub tx: Option<TxId>,
    /// That journal's target; `None` for a repair.
    pub target: Option<TargetKey>,
    /// Its stop's code: `internal`, `state_too_new`, `state_corrupt`, `scope_denied`,
    /// `io` or `game_running`.
    pub code: ErrorCode,
    /// Its stop's `details.reason` (`step_unrecognized`, …); `None` for a recovery that
    /// failed on I/O.
    pub reason: Option<&'static str>,
    /// What stopped, as a sentence: "repair transaction 01M9… did not finish, and recovery
    /// could not settle it (step_unrecognized)".
    pub summary: String,
    /// What to do: `omaddons doctor explain <fm>`, filled by the engine.
    pub fix: Suggestion,
}

/// One protected canonical path: itself and, when a directory, everything below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub path: PathBuf,
    /// Its text for `details.resource`.
    pub resource: String,
    pub stop: FenceStop,
}

/// The fences a command honours, sorted by path, or closed: every operation refused (a
/// stuck journal whose protected paths cannot be known).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FenceSet {
    /// Sorted by `(path, stop.tx)`, without duplicates.
    fences: Vec<Fence>,
    closed: Option<(String, FenceStop)>,
}

/// What `mutate()` is about to do, in the terms a fence cares about.
#[derive(Debug, Clone, Copy)]
pub enum FencedOp<'a> {
    CreateDir(&'a Path),
    /// `WriteFile`, `AppendFile`, `CloneFile` (its target; the source is only read),
    /// `Chmod` and `SymlinkAtomic`.
    Write(&'a Path),
    Rename {
        from: &'a Path,
        to: &'a Path,
    },
}

/// A refusal: the fenced resource (never the attempted path) and the stop to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fenced {
    pub resource: String,
    pub stop: FenceStop,
    /// The set was closed: every operation is refused.
    pub closed: bool,
}

impl Fenced {
    /// The refused command's message. It names the fenced resource, never the path the
    /// command tried to write.
    pub fn message(&self) -> String {
        if self.closed {
            format!(
                "{}; every change is refused until it is set aside",
                self.stop.summary
            )
        } else {
            format!(
                "{}; it protects {}, so omaddons does not write it",
                self.stop.summary, self.resource
            )
        }
    }
}

/// Whether `a` and `b` overlap by whole components: one is the other, or below it
/// (E13's `Resource::overlaps` for paths, restated on canonical paths).
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl FenceSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a fence. `path` must be canonical and absolute; `resource` is its text for
    /// `details.resource`. The same path fenced twice by one journal counts once.
    pub fn fence(&mut self, path: PathBuf, resource: String, stop: FenceStop) {
        debug_assert!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
            "a fence's path is canonical: {}",
            path.display()
        );
        let key = |f: &Fence| (f.path.clone(), f.stop.tx);
        let wanted = (path.clone(), stop.tx);
        if let Err(at) = self.fences.binary_search_by(|f| key(f).cmp(&wanted)) {
            self.fences.insert(
                at,
                Fence {
                    path,
                    resource,
                    stop,
                },
            );
        }
    }

    /// Closes the set: every operation is refused. Closed twice, the stop with the lowest
    /// `tx` wins, so the refusal is the same whatever the order.
    pub fn close(&mut self, resource: String, stop: FenceStop) {
        if self
            .closed
            .as_ref()
            .is_none_or(|(_, kept)| stop.tx < kept.tx)
        {
            self.closed = Some((resource, stop));
        }
    }

    /// No fence, and not closed: it refuses nothing.
    pub fn is_empty(&self) -> bool {
        self.fences.is_empty() && self.closed.is_none()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_some()
    }

    /// Every fencing journal, the closing one included.
    pub fn txs(&self) -> BTreeSet<TxId> {
        self.fences
            .iter()
            .filter_map(|f| f.stop.tx)
            .chain(self.closed.as_ref().and_then(|(_, stop)| stop.tx))
            .collect()
    }

    /// The first fence, in path order, that overlaps `path`.
    pub fn covering(&self, path: &Path) -> Option<&Fence> {
        self.fences.iter().find(|f| overlaps(&f.path, path))
    }

    /// Refuses `op` when the set is closed, or when it would write what a fence protects
    /// (the first such fence in path order is named). A `CreateDir` strictly above a fence
    /// is allowed: fsx creates only a missing directory, so the protected path below it is
    /// absent too, and stays so.
    pub fn check(&self, op: FencedOp<'_>) -> Result<(), Box<Fenced>> {
        if let Some((resource, stop)) = &self.closed {
            return Err(Box::new(Fenced {
                resource: resource.clone(),
                stop: stop.clone(),
                closed: true,
            }));
        }
        let hit = match op {
            FencedOp::CreateDir(path) => self.fences.iter().find(|f| path.starts_with(&f.path)),
            FencedOp::Write(path) => self.covering(path),
            FencedOp::Rename { from, to } => self
                .fences
                .iter()
                .find(|f| overlaps(&f.path, from) || overlaps(&f.path, to)),
        };
        match hit {
            Some(fence) => Err(Box::new(Fenced {
                resource: fence.resource.clone(),
                stop: fence.stop.clone(),
                closed: false,
            })),
            None => Ok(()),
        }
    }

    /// The same set without the fences and the close of the journals in `txs`: what a
    /// set-aside of those journals leaves.
    pub fn excusing(&self, txs: &BTreeSet<TxId>) -> Self {
        let excused = |stop: &FenceStop| stop.tx.is_some_and(|tx| txs.contains(&tx));
        Self {
            fences: self
                .fences
                .iter()
                .filter(|f| !excused(&f.stop))
                .cloned()
                .collect(),
            closed: self.closed.clone().filter(|(_, stop)| !excused(stop)),
        }
    }

    /// The fences held, for test logs. A closed set may hold none and still refuse
    /// everything: [`FenceSet::is_empty`] says whether it refuses nothing.
    pub fn len(&self) -> usize {
        self.fences.len()
    }

    /// The fences, in path order.
    pub fn iter(&self) -> impl Iterator<Item = &Fence> {
        self.fences.iter()
    }

    /// What closed the set, when it is closed: the resource text and the stop.
    pub fn closed(&self) -> Option<(&str, &FenceStop)> {
        self.closed
            .as_ref()
            .map(|(resource, stop)| (resource.as_str(), stop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::resource::{PathKind, Resource};

    const TX1: &str = "01JB8Q2VA00000000000000001";
    const TX2: &str = "01JB8Q2VA00000000000000002";

    fn tx(text: &str) -> TxId {
        text.parse().unwrap()
    }

    fn stop(tx: Option<TxId>) -> FenceStop {
        FenceStop {
            tx,
            target: None,
            code: ErrorCode::Internal,
            reason: Some("step_unrecognized"),
            summary: "repair transaction did not finish, and recovery could not settle it \
                      (step_unrecognized)"
                .to_owned(),
            fix: Suggestion::text("see doctor explain"),
        }
    }

    fn set(fences: &[(&str, &str)]) -> FenceSet {
        let mut set = FenceSet::new();
        for (path, by) in fences {
            set.fence(PathBuf::from(path), format!("{path}/"), stop(Some(tx(by))));
        }
        set
    }

    fn shown(set: &FenceSet) -> Vec<String> {
        set.fences
            .iter()
            .map(|f| format!("{} by {:?}", f.path.display(), f.stop.tx))
            .collect()
    }

    #[test]
    fn overlap_matches_the_resource_model() {
        let cases = [
            ("same_path", "/r/a/b", "/r/a/b", true),
            ("ancestor", "/r/a", "/r/a/b", true),
            ("descendant", "/r/a/b/c", "/r/a/b", true),
            ("sibling_prefix", "/r/a/b", "/r/a/bc", false),
            ("root_and_child", "/", "/r", true),
            ("unrelated", "/r/a", "/s/a", false),
        ];
        for (name, fenced, path, expected) in cases {
            let ours = set(&[(fenced, TX1)]).covering(Path::new(path)).is_some();
            let resource = |p: &str| Resource::from_canonical(Path::new(p), PathKind::Subtree);
            let theirs = resource(fenced).unwrap().overlaps(&resource(path).unwrap());
            assert_eq!(
                (ours, theirs),
                (expected, expected),
                "{name}: a fence on {fenced}, {path}: covering {ours}, Resource::overlaps {theirs}"
            );
        }
    }

    #[test]
    fn check_refuses_each_operation_kind() {
        let fenced = set(&[("/r/a/b", TX1)]);
        let p = Path::new;
        let cases = [
            (
                "create_dir_at_fence",
                FencedOp::CreateDir(p("/r/a/b")),
                true,
            ),
            (
                "create_dir_below_fence",
                FencedOp::CreateDir(p("/r/a/b/c")),
                true,
            ),
            (
                "create_dir_strictly_above_fence",
                FencedOp::CreateDir(p("/r/a")),
                false,
            ),
            ("write_at_fence", FencedOp::Write(p("/r/a/b")), true),
            ("write_above_fence", FencedOp::Write(p("/r/a")), true),
            (
                "write_below_fence",
                FencedOp::Write(p("/r/a/b/c.lua")),
                true,
            ),
            ("write_sibling", FencedOp::Write(p("/r/a/bc")), false),
            (
                "rename_from_fenced",
                FencedOp::Rename {
                    from: p("/r/a/b"),
                    to: p("/r/trash/x"),
                },
                true,
            ),
            (
                "rename_to_fenced",
                FencedOp::Rename {
                    from: p("/r/stage/x"),
                    to: p("/r/a/b/x"),
                },
                true,
            ),
            (
                "rename_neither",
                FencedOp::Rename {
                    from: p("/r/stage/x"),
                    to: p("/r/a/bc"),
                },
                false,
            ),
        ];
        for (name, op, refused) in cases {
            let result = fenced.check(op);
            assert_eq!(
                result.is_err(),
                refused,
                "{name}: {op:?} against {:?}: {result:?}",
                shown(&fenced)
            );
            if let Err(f) = result {
                assert_eq!(
                    (f.resource.as_str(), f.closed),
                    ("/r/a/b/", false),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn a_closed_set_refuses_every_operation() {
        let mut closed = FenceSet::new();
        closed.close("every change".to_owned(), stop(Some(tx(TX1))));
        let p = Path::new;
        for op in [
            FencedOp::CreateDir(p("/r/new")),
            FencedOp::Write(p("/r/file")),
            FencedOp::Rename {
                from: p("/r/x"),
                to: p("/r/y"),
            },
        ] {
            let refused = closed.check(op).unwrap_err();
            assert!(refused.closed, "{op:?}: {refused:?}");
            assert_eq!(refused.resource, "every change", "{op:?}");
        }
        assert!(closed.is_closed() && !closed.is_empty());
        assert_eq!(
            closed.covering(Path::new("/")),
            None,
            "closed, with no fence"
        );
    }

    #[test]
    fn closing_twice_keeps_the_lowest_tx() {
        for order in [[TX1, TX2], [TX2, TX1]] {
            let mut closed = FenceSet::new();
            for by in order {
                closed.close(format!("closed by {by}"), stop(Some(tx(by))));
            }
            let refused = closed.check(FencedOp::Write(Path::new("/r"))).unwrap_err();
            assert_eq!(refused.stop.tx, Some(tx(TX1)), "{order:?}");
        }
    }

    #[test]
    fn the_first_fence_in_path_order_is_reported() {
        let wide = ("/r/a", TX2);
        let narrow = ("/r/a/b", TX1);
        for fences in [[wide, narrow], [narrow, wide]] {
            let set = set(&fences);
            let refused = set
                .check(FencedOp::Write(Path::new("/r/a/b/c")))
                .unwrap_err();
            assert_eq!(refused.resource, "/r/a/", "{:?}", shown(&set));
        }
    }

    #[test]
    fn insertion_deduplicates() {
        let twice = set(&[("/r/a", TX1), ("/r/a", TX1)]);
        assert_eq!(twice.len(), 1, "{:?}", shown(&twice));
        let two_journals = set(&[("/r/a", TX1), ("/r/a", TX2)]);
        assert_eq!(two_journals.len(), 2, "{:?}", shown(&two_journals));
    }

    #[test]
    fn excusing_lifts_only_the_named_journals() {
        let mut both = set(&[("/r/one", TX1), ("/r/two", TX2)]);
        both.close("closed".to_owned(), stop(Some(tx(TX1))));
        let left = both.excusing(&BTreeSet::from([tx(TX1)]));
        assert_eq!(left.txs(), BTreeSet::from([tx(TX2)]), "{:?}", shown(&left));
        assert!(!left.is_closed());
        assert!(left.check(FencedOp::Write(Path::new("/r/one"))).is_ok());
        assert!(left.check(FencedOp::Write(Path::new("/r/two"))).is_err());
        assert_eq!(both.excusing(&BTreeSet::new()), both, "excusing nothing");
    }

    #[test]
    fn an_empty_set_refuses_nothing() {
        let empty = FenceSet::new();
        assert!(empty.is_empty() && !empty.is_closed());
        for op in [
            FencedOp::CreateDir(Path::new("/")),
            FencedOp::Write(Path::new("/r/file")),
            FencedOp::Rename {
                from: Path::new("/a"),
                to: Path::new("/b"),
            },
        ] {
            assert!(empty.check(op).is_ok(), "{op:?}");
        }
        assert_eq!(empty.covering(Path::new("/")), None);
    }

    #[test]
    fn txs_lists_every_fencing_journal() {
        let mut fences = set(&[("/r/one", TX1), ("/r/two", TX1)]);
        fences.fence(PathBuf::from("/r/three"), "/r/three".to_owned(), stop(None));
        fences.close("closed".to_owned(), stop(Some(tx(TX2))));
        assert_eq!(fences.txs(), BTreeSet::from([tx(TX1), tx(TX2)]));
    }

    #[test]
    fn a_refusal_names_the_resource_never_the_path() {
        let fenced = set(&[("/r/a", TX1)]);
        let refused = fenced
            .check(FencedOp::Write(Path::new("/r/a/FAKEACCT/x")))
            .unwrap_err();
        let message = refused.message();
        assert!(
            message.contains("it protects /r/a/") && !message.contains("FAKEACCT"),
            "{message}"
        );
        let mut closed = FenceSet::new();
        closed.close("every change".to_owned(), stop(Some(tx(TX1))));
        let message = closed
            .check(FencedOp::Write(Path::new("/r/x")))
            .unwrap_err()
            .message();
        assert!(
            message.ends_with("every change is refused until it is set aside"),
            "{message}"
        );
    }
}
