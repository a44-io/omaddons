//! Snapshot retention and blob eviction (vision §5.5 "Retention"), after every commit.
//!
//! Per target: snapshots younger than 24 h and the most recent transaction's are never
//! pruned; beyond that the newest 10 within 7 days stay (protected ones count toward
//! the 10), then the oldest unprotected ones go until the target is under
//! `SNAPSHOT_MAX_BYTES` (apparent sizes, both the client-dir and the state-dir halves).
//! Protection beats the cap; if protected snapshots alone exceed it,
//! `snapshot_cap_exceeded`. History entries stay; undo of a pruned transaction reports
//! the snapshot gone.
//!
//! Also swept: stage dirs and in-flight lock files of dead transactions (lock free, no
//! journal, older than `ORPHAN_MIN_AGE`), which nothing else removes in M1. Then the
//! blob cache is evicted LRU to `BLOB_CACHE_MAX_BYTES`, sparing the blobs of installed
//! releases. Every removal goes through `fsx::prune`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::blobs::BlobCache;
use crate::error::{Result, Suggestion, Warning, WarningCode};
use crate::fsx::{self, InflightLock, LockUse, PruneReason, ScopeTable};
use crate::limits::{
    BLOB_CACHE_MAX_BYTES, EffectiveLimits, ORPHAN_MIN_AGE, SNAPSHOT_MAX_BYTES, TX_RETENTION,
    TxRetention,
};
use crate::model::{TargetKey, TxId};
use crate::state::history::{self, HistoryFilter};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::load_record;
use crate::state::registry::load_registry;

/// What retention keeps (config-overridable, vision §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionLimits {
    pub tx: TxRetention,
    pub snapshot_max_bytes: u64,
    pub blob_cache_max_bytes: u64,
}

impl Default for RetentionLimits {
    fn default() -> Self {
        Self {
            tx: TX_RETENTION,
            snapshot_max_bytes: SNAPSHOT_MAX_BYTES,
            blob_cache_max_bytes: BLOB_CACHE_MAX_BYTES,
        }
    }
}

impl From<&EffectiveLimits> for RetentionLimits {
    fn from(limits: &EffectiveLimits) -> Self {
        Self {
            tx: limits.tx_retention,
            snapshot_max_bytes: limits.snapshot_max_bytes,
            blob_cache_max_bytes: limits.blob_cache_max_bytes,
        }
    }
}

/// One transaction with snapshots on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotTx {
    pub tx: TxId,
    pub at: Timestamp,
    pub bytes: u64,
    /// The target's most recent transaction.
    pub most_recent: bool,
}

/// What the policy decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub prune: Vec<TxId>,
    pub kept: usize,
    /// The kept snapshots' bytes, when protected ones alone exceed the cap.
    pub cap_exceeded: Option<u64>,
}

/// What one retention run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RetentionReport {
    pub pruned_tx: usize,
    pub kept_tx: usize,
    pub cap_exceeded: bool,
    pub orphans_swept: usize,
    pub blobs_evicted: usize,
    #[serde(skip)]
    pub warnings: Vec<Warning>,
}

/// The policy over one target's snapshot-bearing transactions (any order) at `now`.
pub fn select(txs: &[SnapshotTx], now: Timestamp, limits: &RetentionLimits) -> Selection {
    let age = |t: &SnapshotTx| now.as_second().saturating_sub(t.at.as_second());
    let secs = |d: std::time::Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
    let protected = |t: &SnapshotTx| t.most_recent || age(t) < secs(limits.tx.min_age);
    let mut newest_first: Vec<&SnapshotTx> = txs.iter().collect();
    newest_first.sort_by(|a, b| b.at.cmp(&a.at).then(b.tx.cmp(&a.tx)));

    let mut kept: Vec<&SnapshotTx> = Vec::new();
    let mut selection = Selection::default();
    for t in newest_first {
        let within = age(t) < secs(limits.tx.window) && kept.len() < limits.tx.keep_last;
        if protected(t) || within {
            kept.push(t);
        } else {
            selection.prune.push(t.tx);
        }
    }
    // The size cap: oldest unprotected first.
    let mut total: u64 = kept.iter().map(|t| t.bytes).sum();
    while total > limits.snapshot_max_bytes {
        let Some(at) = kept.iter().rposition(|t| !protected(t)) else {
            selection.cap_exceeded = Some(total);
            break;
        };
        let dropped = kept.remove(at);
        total = total.saturating_sub(dropped.bytes);
        selection.prune.push(dropped.tx);
    }
    selection.kept = kept.len();
    selection
}

/// Retention for the target `key` (whose client dir `client` must be in `scopes`) at
/// `now`, sparing the blobs in `pins`.
pub fn run(
    paths: &Paths,
    scopes: &ScopeTable,
    key: TargetKey,
    client: &ClientPaths,
    limits: &RetentionLimits,
    now: Timestamp,
    pins: &BTreeSet<String>,
) -> Result<RetentionReport> {
    let filter = HistoryFilter {
        target: Some(key),
        ..HistoryFilter::default()
    };
    let entries = history::read(paths, &filter)?.entries;
    let latest = entries.last().map(|e| e.tx);
    let txs: Vec<SnapshotTx> = entries
        .iter()
        .filter_map(|e| {
            let halves = [client.snapshots(&e.tx), paths.state_snapshot(&e.tx)];
            halves.iter().any(|p| p.exists()).then(|| SnapshotTx {
                tx: e.tx,
                at: e.at,
                bytes: halves.iter().map(|p| apparent_size(p)).sum(),
                most_recent: Some(e.tx) == latest,
            })
        })
        .collect();
    let selection = select(&txs, now, limits);
    let mut report = RetentionReport {
        pruned_tx: selection.prune.len(),
        kept_tx: selection.kept,
        cap_exceeded: selection.cap_exceeded.is_some(),
        ..RetentionReport::default()
    };
    let victims: Vec<PathBuf> = selection
        .prune
        .iter()
        .flat_map(|tx| [client.snapshots(tx), paths.state_snapshot(tx)])
        .filter(|p| p.exists())
        .collect();
    if !victims.is_empty() {
        fsx::prune(scopes, PruneReason::Retention, &victims, LockUse::Acquire)?;
    }
    if let Some(bytes) = selection.cap_exceeded {
        report.warnings.push(
            Warning::new(
                WarningCode::SnapshotCapExceeded,
                format!(
                    "the last day's snapshots of target {key} take {bytes} bytes, over the {} byte cap; they are kept until they are a day old",
                    limits.snapshot_max_bytes
                ),
            )
            .with_fix(Suggestion::text(
                "raise limits.snapshot_max_bytes in config.toml, or wait 24 h",
            )),
        );
    }

    let orphans = orphans(paths, client, now)?;
    if !orphans.is_empty() {
        fsx::prune(
            scopes,
            PruneReason::PostCommitCleanup,
            &orphans,
            LockUse::Acquire,
        )?;
        report.orphans_swept = orphans.len();
    }
    let evicted = BlobCache::new(paths).evict(scopes, pins, limits.blob_cache_max_bytes)?;
    report.blobs_evicted = evicted.evicted.len();
    Ok(report)
}

/// The blobs installed releases came from, across every registered target.
pub fn pinned_blobs(paths: &Paths) -> Result<BTreeSet<String>> {
    let mut pins = BTreeSet::new();
    for entry in load_registry(paths)?.value.targets {
        let record = load_record(paths, &entry.key)?.value;
        pins.extend(record.addons.values().filter_map(|a| a.blob_sha256.clone()));
    }
    Ok(pins)
}

/// Stage dirs and in-flight lock files of dead transactions: lock free, no journal,
/// older than `ORPHAN_MIN_AGE`.
fn orphans(paths: &Paths, client: &ClientPaths, now: Timestamp) -> Result<Vec<PathBuf>> {
    let old = |path: &Path| {
        std::fs::symlink_metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| Timestamp::try_from(t).ok())
            .is_some_and(|t| {
                let floor = i64::try_from(ORPHAN_MIN_AGE.as_secs()).unwrap_or(i64::MAX);
                now.as_second().saturating_sub(t.as_second()) >= floor
            })
    };
    let dead = |tx: &TxId| -> Result<bool> {
        Ok(!InflightLock::is_live(paths, tx)? && !paths.journal(tx).exists())
    };
    let mut found = Vec::new();
    for (name, path) in listing(&client.meta_dir().join("stage")) {
        if let Ok(tx) = name.parse::<TxId>()
            && dead(&tx)?
            && old(&path)
        {
            found.push(path);
        }
    }
    for (name, path) in listing(&paths.inflight_dir()) {
        if let Some(tx) = name
            .strip_suffix(".lock")
            .and_then(|t| t.parse::<TxId>().ok())
            && dead(&tx)?
            && old(&path)
        {
            found.push(path);
        }
    }
    Ok(found)
}

/// (name, path) of each entry in `dir`; nothing when it cannot be read.
fn listing(dir: &Path) -> Vec<(String, PathBuf)> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e.path())))
                .collect()
        })
        .unwrap_or_default()
}

/// Bytes of the regular files under `path` (apparent size; symlinks not followed).
fn apparent_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    listing(path)
        .iter()
        .map(|(_, child)| apparent_size(child))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::{WriteSession, Writer};
    use crate::model::{Actor, TxKind};
    use crate::state::history::{HistoryEntry, TxResult};

    const HOUR: i64 = 3_600;
    const DAY: i64 = 24 * HOUR;

    fn now() -> Timestamp {
        "2026-10-14T12:00:00Z".parse().unwrap()
    }

    fn ago(secs: i64) -> Timestamp {
        Timestamp::from_second(now().as_second() - secs).unwrap()
    }

    fn tx(n: u64, age: i64, bytes: u64) -> SnapshotTx {
        SnapshotTx {
            tx: format!("01JB8Q2VA000000000000000{n:02}").parse().unwrap(),
            at: ago(age),
            bytes,
            most_recent: false,
        }
    }

    fn limits(cap: u64) -> RetentionLimits {
        RetentionLimits {
            snapshot_max_bytes: cap,
            ..RetentionLimits::default()
        }
    }

    fn pruned(selection: &Selection) -> Vec<String> {
        let mut out: Vec<String> = selection.prune.iter().map(|t| t.to_string()).collect();
        out.sort();
        out
    }

    #[test]
    fn young_and_most_recent_snapshots_are_kept_even_over_the_cap() {
        let mut latest = tx(1, 10 * DAY, 500);
        latest.most_recent = true;
        let txs = [tx(2, HOUR, 500), tx(3, 2 * HOUR, 500), latest];
        let selection = select(&txs, now(), &limits(100));
        assert!(selection.prune.is_empty(), "{selection:?}");
        assert_eq!(selection.cap_exceeded, Some(1_500));
        assert_eq!(selection.kept, 3);
    }

    #[test]
    fn the_newest_ten_within_seven_days_stay() {
        // 15 transactions between one and six days old, newest first by number.
        let txs: Vec<SnapshotTx> = (1..=15)
            .map(|n| tx(n, DAY + n as i64 * 8 * HOUR, 1))
            .collect();
        let selection = select(&txs, now(), &limits(u64::MAX));
        assert_eq!(selection.kept, 10);
        let expected: Vec<String> = (11..=15).map(|n| tx(n, 0, 0).tx.to_string()).collect();
        assert_eq!(pruned(&selection), expected);

        // Older than seven days goes, unless it is the most recent.
        let mut old_latest = tx(1, 9 * DAY, 1);
        old_latest.most_recent = true;
        let selection = select(&[old_latest, tx(2, 8 * DAY, 1)], now(), &limits(u64::MAX));
        assert_eq!(pruned(&selection), [tx(2, 0, 0).tx.to_string()]);
    }

    #[test]
    fn the_cap_prunes_the_oldest_unprotected_first() {
        let txs = [
            tx(1, HOUR, 100),
            tx(2, 2 * DAY, 100),
            tx(3, 3 * DAY, 100),
            tx(4, 4 * DAY, 100),
        ];
        let selection = select(&txs, now(), &limits(250));
        assert_eq!(
            pruned(&selection),
            [tx(3, 0, 0).tx.to_string(), tx(4, 0, 0).tx.to_string()],
            "newest first: tx 2 stays with the protected tx 1"
        );
        assert_eq!(selection.cap_exceeded, None);
    }

    /// A state dir, a client dir and a scope table over them.
    struct Fixture {
        _dir: tempfile::TempDir,
        paths: Paths,
        client: ClientPaths,
        scopes: ScopeTable,
        key: TargetKey,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let client_dir = root.join("_retail_");
        std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::create_dir_all(paths.cache_dir()).unwrap();
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client_dir), None).unwrap();
        Fixture {
            _dir: dir,
            client: ClientPaths::new(&client_dir),
            paths,
            scopes,
            key: TargetKey::new(),
        }
    }

    impl Fixture {
        /// A committed transaction `age` seconds old with `bytes` in each snapshot half.
        fn commit(&self, age: i64, bytes: usize) -> TxId {
            let tx = TxId::new();
            let entry = HistoryEntry {
                schema_version: history::CURRENT,
                tx,
                kind: TxKind::Install,
                at: ago(age),
                target: self.key,
                actor: Actor::Agent,
                reason: None,
                argv: vec!["omaddons".into()],
                ops: Vec::new(),
                result: TxResult::Committed,
                reverts: None,
                sv_deferred: false,
                created_addons_dir: false,
            };
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            history::append(&mut s, &self.paths, &entry).unwrap();
            for dir in [
                self.client.snapshots(&tx).join("AddOns/Foo"),
                self.paths.state_snapshot(&tx),
            ] {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("data"), vec![b'x'; bytes]).unwrap();
            }
            tx
        }

        fn run(&self, limits: &RetentionLimits, pins: &BTreeSet<String>) -> RetentionReport {
            // Two hours on: every orphan made now is past ORPHAN_MIN_AGE.
            let later = Timestamp::from_second(Timestamp::now().as_second() + 2 * HOUR).unwrap();
            run(
                &self.paths,
                &self.scopes,
                self.key,
                &self.client,
                limits,
                later,
                pins,
            )
            .unwrap()
        }

        fn has_snapshot(&self, tx: &TxId) -> (bool, bool) {
            (
                self.client.snapshots(tx).exists(),
                self.paths.state_snapshot(tx).exists(),
            )
        }
    }

    #[test]
    fn pruning_removes_both_halves_and_keeps_history() {
        let f = fixture();
        let old = f.commit(10 * DAY, 10);
        let kept = f.commit(2 * DAY, 10);
        let latest = f.commit(9 * DAY, 10); // the most recent by history order
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!((report.pruned_tx, report.kept_tx), (1, 2), "{report:?}");
        assert_eq!(f.has_snapshot(&old), (false, false));
        assert_eq!(f.has_snapshot(&kept), (true, true));
        assert_eq!(
            f.has_snapshot(&latest),
            (true, true),
            "the most recent is protected"
        );
        let entries = history::read(&f.paths, &HistoryFilter::default())
            .unwrap()
            .entries;
        assert_eq!(entries.len(), 3, "history is append-only");
    }

    #[test]
    fn dead_orphans_are_swept_and_live_or_journaled_ones_kept() {
        let f = fixture();
        f.commit(HOUR, 1);
        let stage = |tx: &TxId| f.client.stage(tx);
        let (dead, held, journaled) = (TxId::new(), TxId::new(), TxId::new());
        for tx in [&dead, &held, &journaled] {
            std::fs::create_dir_all(stage(tx)).unwrap();
        }
        std::fs::create_dir_all(f.paths.inflight_dir()).unwrap();
        std::fs::write(f.paths.inflight_lock(&dead), b"").unwrap();
        let _live = InflightLock::create(&f.paths, &held).unwrap();
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        std::fs::write(f.paths.journal(&journaled), b"").unwrap();

        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 2, "{report:?}");
        assert!(!stage(&dead).exists() && !f.paths.inflight_lock(&dead).exists());
        assert!(stage(&held).exists() && f.paths.inflight_lock(&held).exists());
        assert!(stage(&journaled).exists(), "journal recovery owns it");
    }

    #[test]
    fn a_retention_failure_never_fails_the_commit() {
        let scene = crate::engine::testing::TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", "## Interface: 120105\n")]);
        let done = crate::fsx::failpoint::with_spec("engine.retention=error", || {
            scene.run(vec![scene.install("gh:o/foo", &zip)])
        })
        .unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        let outcome = &done.outcomes[0];
        assert!(outcome.tx.is_some(), "committed");
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.code == WarningCode::RetentionFailed),
            "{:?}",
            outcome.warnings
        );
        assert!(scene.client.addons().join("Foo").is_dir());
    }

    #[test]
    fn pinned_blobs_survive_eviction() {
        let f = fixture();
        f.commit(HOUR, 1);
        let sha = |c: char| c.to_string().repeat(64);
        std::fs::create_dir_all(f.paths.blobs_dir()).unwrap();
        for c in ['a', 'b', 'c'] {
            std::fs::write(f.paths.blob(&sha(c)), vec![0u8; 100]).unwrap();
        }
        let pins = BTreeSet::from([sha('b')]);
        let limits = RetentionLimits {
            blob_cache_max_bytes: 0,
            ..RetentionLimits::default()
        };
        let report = f.run(&limits, &pins);
        assert_eq!(report.blobs_evicted, 2, "{report:?}");
        assert!(f.paths.blob(&sha('b')).exists());
        assert!(!f.paths.blob(&sha('a')).exists() && !f.paths.blob(&sha('c')).exists());
    }
}
