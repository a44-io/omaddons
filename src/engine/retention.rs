//! Snapshot retention and blob eviction (vision §5.5 "Retention"), after every commit.
//!
//! Per target: snapshots younger than 24 h, the most recent transaction's, and those a
//! deferred SavedVariables restore still needs are never pruned; beyond that the newest
//! 10 within 7 days stay (protected ones count toward the 10), then the oldest
//! unprotected ones go until the target is under `SNAPSHOT_MAX_BYTES` (apparent sizes,
//! both the client-dir and the state-dir halves). Protection beats the cap; if
//! protected snapshots alone exceed it, `snapshot_cap_exceeded`. History entries stay;
//! undo of a pruned transaction reports the snapshot gone.
//!
//! Also swept: stage dirs, in-flight lock files and snapshot dirs of dead transactions
//! (lock free, no journal, older than `ORPHAN_MIN_AGE`), a snapshot dir only when no
//! history line names its transaction, which never committed. Rollback and cleanup
//! remove a dead transaction's snapshots at once; the sweep takes what a crash or a
//! failed prune left. A client snapshot whose `AddOns/` still holds a folder is never
//! removed as garbage: it may be the only copy of an addon. Then the blob cache is
//! evicted LRU to `BLOB_CACHE_MAX_BYTES`, sparing installed releases and unexpired stored
//! plans. Expired plans are swept first, in a bounded batch through the plan store's
//! own sweep. Every removal goes through `fsx::prune`.
//!
//! Never touched, however old (M4-D36): a doctor repair's `<tx>.repair` halves and the
//! cache's `quarantine/`, which only `doctor gc` deletes.
//!
//! A commit's retention reaches only its own client, so [`sweep`] also passes over every
//! other registered client discovery found, at most once per `RETENTION_SWEEP_INTERVAL`,
//! after a commit or a backup: a client nobody changes any more is pruned too.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::blobs::BlobCache;
use crate::error::{Error, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{self, InflightLock, LockUse, PruneReason, ScopeTable, failpoint};
use crate::limits::{
    BLOB_CACHE_MAX_BYTES, EffectiveLimits, ORPHAN_MIN_AGE, RETENTION_SWEEP_INTERVAL,
    SNAPSHOT_MAX_BYTES, TX_RETENTION, TxRetention,
};
use crate::model::{Target, TargetKey, TxId, TxKind};
use crate::state::history::{self, HistoryEntry, HistoryFilter};
use crate::state::paths::{ClientPaths, Paths, REPAIR_SUFFIX};
use crate::state::plans::{self, PlanPins};
use crate::state::records::load_record;
use crate::state::registry::load_registry;
use crate::state::retention_sweep;

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
    /// A deferred SavedVariables restore still needs it ([`super::undo`]'s follow-up).
    pub pinned: bool,
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
    pub plans_swept: usize,
    pub plan_leases_swept: usize,
    pub plan_sweep_failed: bool,
    #[serde(skip)]
    pub plan_pins: Option<PlanPins>,
    #[serde(skip)]
    pub warnings: Vec<Warning>,
}

/// Snapshots and orphans use system-clock history and mtimes. Stored plans use the
/// plan clock, frozen by `OMADDONS_NOW` in debug builds; release builds use real time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionClock {
    pub system: Timestamp,
    pub plans: Timestamp,
}

impl RetentionClock {
    pub fn current() -> Self {
        Self {
            system: Timestamp::now(),
            plans: crate::clock::now(),
        }
    }
}

impl From<Timestamp> for RetentionClock {
    fn from(now: Timestamp) -> Self {
        Self {
            system: now,
            plans: now,
        }
    }
}

/// The policy over one target's snapshot-bearing transactions (any order) at `now`.
pub fn select(txs: &[SnapshotTx], now: Timestamp, limits: &RetentionLimits) -> Selection {
    let age = |t: &SnapshotTx| now.as_second().saturating_sub(t.at.as_second());
    let secs = |d: std::time::Duration| i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
    let protected = |t: &SnapshotTx| t.most_recent || t.pinned || age(t) < secs(limits.tx.min_age);
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
/// `now`, sparing the blobs `pins` names (read under the lock, when eviction needs it):
/// the plan sweep, that client's part ([`prune_client`]), then the blob cache.
pub fn run(
    paths: &Paths,
    scopes: &ScopeTable,
    key: TargetKey,
    client: &ClientPaths,
    limits: &RetentionLimits,
    now: impl Into<RetentionClock>,
    pins: &dyn Fn() -> Result<BTreeSet<String>>,
) -> Result<RetentionReport> {
    let clock = now.into();
    let mut report = RetentionReport::default();
    sweep_plans(paths, clock.plans, &mut report);
    let history = HistoryView::read(paths)?;
    prune_client(
        paths,
        scopes,
        key,
        client,
        limits,
        clock.system,
        &history,
        &mut report,
    )?;
    let seen = std::cell::RefCell::new(None);
    let with_plans = || -> Result<BTreeSet<String>> {
        let mut all = pins()?;
        let found = plans::unexpired_blob_pins(paths, clock.plans);
        all.extend(found.blobs.iter().cloned());
        *seen.borrow_mut() = Some(found);
        Ok(all)
    };
    let evicted = BlobCache::new(paths).evict(scopes, &with_plans, limits.blob_cache_max_bytes)?;
    report.blobs_evicted = evicted.evicted.len();
    report.plan_pins = seen.into_inner();
    Ok(report)
}

/// The history a pass reads once: the raw text (the orphan sweep looks for transaction
/// ids in it, torn lines included), its entries, and the snapshots a deferred
/// SavedVariables restore still needs, whose conflict check spans targets.
struct HistoryView {
    text: String,
    entries: Vec<HistoryEntry>,
    pinned: BTreeSet<TxId>,
}

impl HistoryView {
    fn read(paths: &Paths) -> Result<Self> {
        let text = history::text(paths)?;
        let entries = history::parse(&text, &HistoryFilter::default()).entries;
        let pinned = super::undo::pending_sv_restores(&entries);
        Ok(Self {
            text,
            entries,
            pinned,
        })
    }
}

/// One client's part of retention at `now`: the snapshot policy over the target `key`'s
/// transactions, then that client's orphan sweep. Its counts and warnings go to
/// `report`.
#[allow(clippy::too_many_arguments)]
fn prune_client(
    paths: &Paths,
    scopes: &ScopeTable,
    key: TargetKey,
    client: &ClientPaths,
    limits: &RetentionLimits,
    now: Timestamp,
    history: &HistoryView,
    report: &mut RetentionReport,
) -> Result<()> {
    let HistoryView {
        text,
        entries: all,
        pinned,
    } = history;
    let entries: Vec<&HistoryEntry> = all.iter().filter(|e| e.touches_target(key)).collect();
    // A backup line has no snapshot: the most recent transaction is the last other one.
    // Nor has a target-less line (a repair): it keeps its halves as `<tx>.repair`, which
    // retention never prunes, so it has no `<tx>` snapshot to protect.
    let latest = entries
        .iter()
        .rev()
        .find(|e| e.kind != TxKind::Backup && e.target.is_some())
        .map(|e| e.tx);
    let txs: Vec<SnapshotTx> = entries
        .iter()
        .filter_map(|e| {
            let halves = snapshot_dirs(paths, client, &e.tx);
            // A restore's `WTF.previous` is undo data the size cap never counts, so a few
            // large restores never evict AddOns snapshots (M3-D48).
            let wtf_previous = apparent_size(&snapshot_of(client, &e.tx).join(WTF_PREVIOUS));
            halves.iter().any(|p| p.exists()).then(|| SnapshotTx {
                tx: e.tx,
                at: e.at,
                bytes: halves
                    .iter()
                    .map(|p| apparent_size(p))
                    .sum::<u64>()
                    .saturating_sub(wtf_previous),
                most_recent: Some(e.tx) == latest,
                pinned: pinned.contains(&e.tx),
            })
        })
        .collect();
    let selection = select(&txs, now, limits);
    report.pruned_tx = selection.prune.len();
    report.kept_tx = selection.kept;
    report.cap_exceeded = selection.cap_exceeded.is_some();
    let victims: Vec<PathBuf> = selection
        .prune
        .iter()
        .flat_map(|tx| snapshot_dirs(paths, client, tx))
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

    let orphans = orphans(paths, client, now, text)?;
    if !orphans.scratch.is_empty() {
        fsx::prune(
            scopes,
            PruneReason::PostCommitCleanup,
            &orphans.scratch,
            LockUse::Acquire,
        )?;
    }
    if !orphans.snapshots.is_empty() {
        fsx::prune(
            scopes,
            PruneReason::Retention,
            &orphans.snapshots,
            LockUse::Acquire,
        )?;
    }
    report.orphans_swept = orphans.scratch.len() + orphans.snapshots.len();
    Ok(())
}

/// What one pass over the other clients did ([`sweep`]).
#[derive(Debug, Default)]
pub struct SweepReport {
    /// Whether a pass ran: there were clients to reach and the last pass was at least
    /// [`RETENTION_SWEEP_INTERVAL`] ago.
    pub ran: bool,
    /// Each client the pass pruned, with what it did there.
    pub targets: Vec<(TargetKey, RetentionReport)>,
    /// One `retention_failed` per client that failed, naming it.
    pub warnings: Vec<Warning>,
}

/// Snapshot retention for every registered client in `targets` (the clients discovery
/// found) but `except`, at most once per [`RETENTION_SWEEP_INTERVAL`]: a client whose
/// last change is long past is never reached by its own commits' retention, so without
/// this pass it would keep every snapshot. Each client gets its own fenced scope table
/// and its own prunes, which take the global lock briefly as a commit's retention does.
/// Best effort: a client that fails is a `retention_failed` warning naming it, and the
/// others are still pruned. The stamp is written only when every client was pruned, so a
/// failed pass is tried again by the next command. Each client's snapshot cap warning
/// stays with that client's own commits.
pub fn sweep(
    paths: &Paths,
    writer: fsx::Writer,
    targets: &[Target],
    except: Option<TargetKey>,
    limits: &RetentionLimits,
    now: Timestamp,
) -> SweepReport {
    let mut report = SweepReport::default();
    let mut reached: Vec<(TargetKey, &Target)> = Vec::new();
    for target in targets {
        if let Some(key) = target.key
            && Some(key) != except
            && !reached.iter().any(|(k, _)| *k == key)
        {
            reached.push((key, target));
        }
    }
    // A home with one client never reads or writes the stamp.
    if reached.is_empty() {
        return report;
    }
    if !retention_sweep::due(paths, now, RETENTION_SWEEP_INTERVAL) {
        return report;
    }
    report.ran = true;
    let history = match HistoryView::read(paths) {
        Ok(history) => history,
        Err(error) => {
            report
                .warnings
                .push(sweep_failed("other clients' snapshots", &error));
            return report;
        }
    };
    let mut failed = false;
    for (key, target) in reached {
        let client = ClientPaths::new(&target.client_dir);
        let one = || -> Result<RetentionReport> {
            failpoint::hit("engine.retention.sweep")
                .map_err(|e| Error::io(paths.state_dir(), &e, sweep_fix()))?;
            let scopes =
                super::fence::table(paths, std::slice::from_ref(&target.client_dir), None)?;
            let mut pruned = RetentionReport::default();
            prune_client(
                paths,
                &scopes,
                key,
                &client,
                limits,
                now,
                &history,
                &mut pruned,
            )?;
            Ok(pruned)
        };
        match one() {
            Ok(pruned) => report.targets.push((key, pruned)),
            Err(error) => {
                failed = true;
                let what = format!("{}'s snapshots", target.id);
                report.warnings.push(sweep_failed(&what, &error));
            }
        }
    }
    if !failed {
        // Best effort, as the doorbell: a stamp that cannot be written only makes the next
        // command's pass due again.
        if let Ok(scopes) = super::fence::table(paths, &[], None) {
            let mut s = fsx::WriteSession::untracked(writer, &scopes, "retention sweep");
            let _ = retention_sweep::write(&mut s, paths, now);
        }
    }
    report
}

/// The `retention_failed` warning for `what` (a client's snapshots, or all of them).
fn sweep_failed(what: &str, error: &crate::error::Error) -> Warning {
    Warning::new(
        WarningCode::RetentionFailed,
        format!("pruning {what} failed: {}", error.message),
    )
    .with_fix(sweep_fix())
}

fn sweep_fix() -> Suggestion {
    Suggestion::command(["omaddons", "status"])
}

fn sweep_plans(paths: &Paths, now: Timestamp, report: &mut RetentionReport) {
    // A home that never previewed must not acquire a plan directory or its lock.
    // ubs:ignore — an io::ErrorKind, not a secret
    if matches!(std::fs::symlink_metadata(paths.plans_dir()), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
    {
        return;
    }
    match plans::sweep(paths, now) {
        Ok(swept) => {
            report.plans_swept = swept.removed;
            report.plan_leases_swept = swept.leases_removed;
        }
        Err(_) => report.plan_sweep_failed = true,
    }
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

/// `tx`'s client snapshot under whichever of its two names it has (`<tx>.wtf` for a
/// transaction that puts settings back, `<tx>` for any other or a restore from before
/// omad-aepa); the bare name when neither exists.
pub(crate) fn snapshot_of(client: &ClientPaths, tx: &TxId) -> PathBuf {
    either_name(client.snapshots_wtf(tx), client.snapshots(tx))
}

/// `tx`'s state snapshot under whichever of its two names it has, as [`snapshot_of`].
pub(crate) fn state_snapshot_of(paths: &Paths, tx: &TxId) -> PathBuf {
    either_name(paths.state_snapshot_wtf(tx), paths.state_snapshot(tx))
}

fn either_name(wtf: PathBuf, plain: PathBuf) -> PathBuf {
    if std::fs::symlink_metadata(&wtf).is_ok() {
        wtf
    } else {
        plain
    }
}

/// Both halves of `tx`'s snapshots under both of their names: `<tx>`, and `<tx>.wtf` for
/// a transaction that puts settings back. Of each half, at most one exists.
fn snapshot_dirs(paths: &Paths, client: &ClientPaths, tx: &TxId) -> [PathBuf; 4] {
    [
        client.snapshots(tx),
        client.snapshots_wtf(tx),
        paths.state_snapshot(tx),
        paths.state_snapshot_wtf(tx),
    ]
}

/// The snapshot dirs of `tx`, a transaction that never committed and never will, that
/// may go.
#[derive(Debug, Default)]
pub(super) struct DeadSnapshots {
    /// The state half, and the client half unless it is kept; each only if it exists.
    pub victims: Vec<PathBuf>,
    /// The client half, kept because its `AddOns/` still holds this many entries.
    pub kept: Option<(PathBuf, usize)>,
}

/// What of `tx`'s snapshots may go once it is known never to commit: the state half,
/// and the client half (when `client` is known) unless its `AddOns/` still holds a
/// folder. A rollback brings every folder back out of it, so one left there is what a
/// recovery bug left, and may be the only copy of an addon. Each half under either of
/// its names.
pub(super) fn dead_snapshots(
    paths: &Paths,
    client: Option<&ClientPaths>,
    tx: &TxId,
) -> DeadSnapshots {
    let mut dead = DeadSnapshots::default();
    if let Some(client) = client {
        for snapshot in [client.snapshots(tx), client.snapshots_wtf(tx)] {
            if snapshot.exists() {
                match snapshot_folders(&snapshot) {
                    0 => dead.victims.push(snapshot),
                    n => dead.kept = Some((snapshot, n)),
                }
            }
        }
    }
    for state in [paths.state_snapshot(tx), paths.state_snapshot_wtf(tx)] {
        if state.exists() {
            dead.victims.push(state);
        }
    }
    dead
}

/// The entries in a client snapshot's `AddOns/`, plus its `WTF.previous` when present:
/// none when both are absent, one for an `AddOns/` that cannot be read (kept, as if it
/// held a folder).
pub(crate) fn snapshot_folders(client_snapshot: &Path) -> usize {
    let addons = match std::fs::read_dir(client_snapshot.join("AddOns")) {
        Ok(listing) => listing.count(),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => 1,
    };
    // A restore's old `WTF/` may be the only copy of someone's settings too.
    let wtf_previous = std::fs::symlink_metadata(client_snapshot.join(WTF_PREVIOUS)).is_ok();
    addons + usize::from(wtf_previous)
}

/// Where a whole restore keeps the `WTF/` it replaced, in its client snapshot.
pub const WTF_PREVIOUS: &str = "WTF.previous";

/// Whether history `text` names `tx` anywhere. The raw text, not the parsed entries: a
/// committed transaction whose line is torn or merged into the next still counts, so
/// its snapshot is never taken for garbage.
pub(super) fn in_history(text: &str, tx: &TxId) -> bool {
    text.contains(&tx.to_string())
}

/// What the orphan sweep removes, by the reason `fsx::prune` takes for each.
struct Orphans {
    /// Stage dirs and in-flight lock files.
    scratch: Vec<PathBuf>,
    /// Snapshot dirs, both halves.
    snapshots: Vec<PathBuf>,
}

/// Stage dirs, in-flight lock files (the state's and the cache's scratch locks) and
/// snapshot dirs of dead transactions: lock free, no journal, older than
/// `ORPHAN_MIN_AGE`. A snapshot dir also needs no mention in history `text`, and a
/// client one an empty `AddOns/`.
fn orphans(paths: &Paths, client: &ClientPaths, now: Timestamp, text: &str) -> Result<Orphans> {
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
    // A journal under its provisional name belongs to an undo of a set-aside that has not
    // settled (E14): whatever that undo has put back around it is never an orphan.
    let dead = |tx: &TxId| -> Result<bool> {
        Ok(!InflightLock::is_live(paths, tx)?
            && super::journal::existing(paths, tx).is_none()
            && std::fs::symlink_metadata(super::repair::provisional_journal(paths, tx)).is_err())
    };
    let mut found = Vec::new();
    // Age first: probing a lock takes it for a moment, which must never meet a lock file
    // its transaction has just created and not yet locked.
    for (name, path) in listing(&client.meta_dir().join("stage")) {
        // The repair family's halves are deleted only by `doctor gc` (M4-D36); the suffix
        // is matched to skip, never parsed.
        if name.ends_with(REPAIR_SUFFIX) {
            continue;
        }
        if let Some(tx) = crate::state::paths::tx_named(&name)
            && old(&path)
            && dead(&tx)?
        {
            found.push(path);
        }
    }
    let locks = listing(&paths.inflight_dir())
        .into_iter()
        .chain(listing(&paths.cache_inflight_dir()));
    for (name, path) in locks {
        if let Some(tx) = name
            .strip_suffix(".lock")
            .and_then(|t| t.parse::<TxId>().ok())
            && old(&path)
            && dead(&tx)?
        {
            found.push(path);
        }
    }
    let mut snapshots = Vec::new();
    // The state half is every target's; the client half is this client's.
    let roots = [
        (client.meta_dir().join("snapshots"), true),
        (paths.state_snapshots_dir(), false),
    ];
    for (root, client_side) in roots {
        for (name, path) in listing(&root) {
            // As for stages: a `.repair` half is `doctor gc`'s alone (M4-D36).
            if name.ends_with(REPAIR_SUFFIX) {
                continue;
            }
            if let Some(tx) = crate::state::paths::tx_named(&name)
                && !in_history(text, &tx)
                && dead(&tx)?
                && old(&path)
                && !(client_side && snapshot_folders(&path) > 0)
            {
                snapshots.push(path);
            }
        }
    }
    Ok(Orphans {
        scratch: found,
        snapshots,
    })
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
    use crate::model::Actor;
    use crate::state::history::{BackupNote, HistoryEntry, TxResult};

    mod plan_tests;

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
            pinned: false,
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
    fn a_pinned_snapshot_is_protected_like_the_most_recent() {
        let mut pinned = tx(1, 10 * DAY, 500);
        pinned.pinned = true;
        let selection = select(&[pinned, tx(2, 9 * DAY, 1)], now(), &limits(100));
        assert_eq!(pruned(&selection), [tx(2, 0, 0).tx.to_string()]);
        assert_eq!(
            (selection.kept, selection.cap_exceeded),
            (1, Some(500)),
            "kept over the cap, which it counts toward"
        );
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
        /// The instant retention runs at, two hours on (every orphan made now is past
        /// ORPHAN_MIN_AGE); commits are aged from it too, so ages hold on any date.
        clock: Timestamp,
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
            clock: Timestamp::from_second(Timestamp::now().as_second() + 2 * HOUR).unwrap(),
        }
    }

    impl Fixture {
        /// A committed transaction `age` seconds old with `bytes` in each snapshot half.
        fn commit(&self, age: i64, bytes: usize) -> TxId {
            self.commit_as(self.key, age, bytes)
        }

        /// The same, committed by target `key` in this client dir.
        fn commit_as(&self, key: TargetKey, age: i64, bytes: usize) -> TxId {
            self.commit_on(&self.client, key, age, bytes)
        }

        /// The same, committed by target `key` in `client`'s dir.
        fn commit_on(&self, client: &ClientPaths, key: TargetKey, age: i64, bytes: usize) -> TxId {
            let tx = TxId::new();
            let entry = HistoryEntry {
                schema_version: history::CURRENT,
                tx,
                kind: TxKind::Install,
                at: Timestamp::from_second(self.clock.as_second() - age).unwrap(),
                target: Some(key),
                targets: Vec::new(),
                actor: Actor::Agent,
                reason: None,
                argv: vec!["omaddons".into()],
                ops: Vec::new(),
                result: TxResult::Committed,
                reverts: None,
                sv_deferred: false,
                sv_restored: None,
                created_addons_dir: false,
                run: None,
                backup: None,
                plan: None,
                doctor_run_id: None,
                repairs: Vec::new(),
            };
            let mut s = WriteSession::untracked(Writer::Mutating, &self.scopes, "test");
            history::append(&mut s, &self.paths, &entry).unwrap();
            for dir in [
                client.snapshots(&tx).join("AddOns/Foo"),
                self.paths.state_snapshot(&tx),
            ] {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("data"), vec![b'x'; bytes]).unwrap();
            }
            tx
        }

        fn run(&self, limits: &RetentionLimits, pins: &BTreeSet<String>) -> RetentionReport {
            run(
                &self.paths,
                &self.scopes,
                self.key,
                &self.client,
                limits,
                self.clock,
                &|| Ok(pins.clone()),
            )
            .unwrap()
        }

        fn has_snapshot(&self, tx: &TxId) -> (bool, bool) {
            (
                self.client.snapshots(tx).exists(),
                self.paths.state_snapshot(tx).exists(),
            )
        }

        /// Another registered client beside this one, in `flavor`, named `id`.
        fn other(&self, flavor: &str, id: &str) -> Other {
            let dir = self.client.client_dir().parent().unwrap().join(flavor);
            std::fs::create_dir_all(dir.join("Interface/AddOns")).unwrap();
            let mut target = crate::engine::testing::target(&dir);
            target.key = Some(TargetKey::new());
            target.id = id.into();
            Other {
                client: ClientPaths::new(&dir),
                target,
            }
        }

        /// The pass over `targets` but `except`, at `self.clock` plus `later` seconds.
        fn sweep(&self, targets: &[Target], except: Option<TargetKey>, later: i64) -> SweepReport {
            let at = Timestamp::from_second(self.clock.as_second() + later).unwrap();
            sweep(
                &self.paths,
                Writer::Mutating,
                targets,
                except,
                &RetentionLimits::default(),
                at,
            )
        }

        /// The fixture's own client as a target, `retail`.
        fn own_target(&self) -> Target {
            let mut target = crate::engine::testing::target(self.client.client_dir());
            target.key = Some(self.key);
            target
        }
    }

    /// A client beside the fixture's.
    struct Other {
        client: ClientPaths,
        target: Target,
    }

    impl Other {
        fn key(&self) -> TargetKey {
            self.target.key.unwrap()
        }
    }

    /// Ages of a run of commits: 26 hours old, then 14 hours older each, so the newest 11
    /// are inside the 7-day window and the twelfth is past it.
    fn burst_ages() -> Vec<i64> {
        (0..12).map(|i| 26 * HOUR + i * 14 * HOUR).collect()
    }

    /// Commits `ages` on `client` as `key`, oldest first, so the youngest is the most
    /// recent: (age, tx) newest first.
    fn burst(f: &Fixture, client: &ClientPaths, key: TargetKey, ages: &[i64]) -> Vec<(i64, TxId)> {
        let mut made: Vec<(i64, TxId)> = ages
            .iter()
            .rev()
            .map(|age| (*age, f.commit_on(client, key, *age, 10)))
            .collect();
        made.reverse();
        made
    }

    /// The ages of `made` whose client and state halves are both still there.
    fn surviving(f: &Fixture, client: &ClientPaths, made: &[(i64, TxId)]) -> Vec<i64> {
        made.iter()
            .filter(|(_, tx)| client.snapshots(tx).exists() && f.paths.state_snapshot(tx).exists())
            .map(|(age, _)| *age)
            .collect()
    }

    #[test]
    fn the_pass_prunes_another_client_as_its_own_retention_would() {
        // Its own retention, on a twin home.
        let twin = fixture();
        let own = burst(&twin, &twin.client, twin.key, &burst_ages());
        twin.run(&RetentionLimits::default(), &BTreeSet::new());
        let expected = surviving(&twin, &twin.client, &own);
        assert_eq!(
            expected,
            burst_ages()[..10].to_vec(),
            "the newest 10 inside the window stay"
        );

        // The pass, from a commit on the fixture's own client.
        let f = fixture();
        let era = f.other("_classic_era_", "era");
        let made = burst(&f, &era.client, era.key(), &burst_ages());
        let latest = f.commit(HOUR, 10);
        let report = f.sweep(&[f.own_target(), era.target.clone()], Some(f.key), 0);
        assert!(report.ran && report.warnings.is_empty(), "{report:?}");
        assert_eq!(
            report.targets.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [era.key()],
            "only the other client: {report:?}"
        );
        assert_eq!(surviving(&f, &era.client, &made), expected);
        assert_eq!(
            f.has_snapshot(&latest),
            (true, true),
            "the committed client"
        );
        let entries = history::read(&f.paths, &HistoryFilter::default())
            .unwrap()
            .entries;
        assert_eq!(entries.len(), 13, "history is append-only");
    }

    #[test]
    fn the_pass_runs_at_most_once_per_interval() {
        let f = fixture();
        let era = f.other("_classic_era_", "era");
        let targets = [f.own_target(), era.target.clone()];
        let first = burst(&f, &era.client, era.key(), &burst_ages());
        let ran = f.sweep(&targets, Some(f.key), 0);
        assert!(ran.ran, "no stamp yet: {ran:?}");
        assert_eq!(surviving(&f, &era.client, &first).len(), 10);
        assert!(f.paths.retention_sweep_file().is_file(), "stamped");

        // More old commits on era: an hour on, the pass is not due and walks nothing.
        let second = burst(&f, &era.client, era.key(), &burst_ages());
        let skipped = f.sweep(&targets, Some(f.key), HOUR);
        assert!(!skipped.ran && skipped.targets.is_empty(), "{skipped:?}");
        assert_eq!(surviving(&f, &era.client, &second).len(), 12, "untouched");

        // A day on, it is due again.
        let again = f.sweep(&targets, Some(f.key), DAY);
        assert!(again.ran && again.warnings.is_empty(), "{again:?}");
        assert!(surviving(&f, &era.client, &second).len() < 12, "pruned");
    }

    #[test]
    fn the_pass_reaches_only_other_registered_clients() {
        let f = fixture();
        let era = f.other("_classic_era_", "era");
        let made = burst(&f, &era.client, era.key(), &burst_ages());
        let own = burst(&f, &f.client, f.key, &burst_ages());

        // Alone, or beside an unregistered client: nothing to reach, no stamp read or made.
        let mut unregistered = f.other("_anniversary_", "anniversary").target;
        unregistered.key = None;
        for (case, targets) in [
            ("alone", vec![f.own_target()]),
            (
                "beside an unregistered one",
                vec![f.own_target(), unregistered],
            ),
        ] {
            let report = f.sweep(&targets, Some(f.key), 0);
            assert!(!report.ran, "{case}: {report:?}");
            assert!(!f.paths.retention_sweep_file().exists(), "{case}");
        }
        assert_eq!(surviving(&f, &f.client, &own).len(), 12, "the excepted one");

        // From a backup, nothing is excepted: both clients are pruned.
        let report = f.sweep(&[f.own_target(), era.target.clone()], None, 0);
        assert_eq!(report.targets.len(), 2, "{report:?}");
        assert_eq!(surviving(&f, &f.client, &own).len(), 10);
        assert_eq!(surviving(&f, &era.client, &made).len(), 10);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_client_that_fails_is_named_and_the_next_pass_converges() {
        use crate::fsx::failpoint;

        let cases = [
            ("before the client", "engine.retention.sweep=error@1"),
            ("an I/O error mid-prune", "fsx.prune.renamed=error@1"),
        ];
        for (case, spec) in cases {
            let f = fixture();
            let era = f.other("_classic_era_", "era");
            let anniversary = f.other("_anniversary_", "anniversary");
            let targets = [
                f.own_target(),
                era.target.clone(),
                anniversary.target.clone(),
            ];
            let on_era = burst(&f, &era.client, era.key(), &burst_ages());
            let on_anniversary = burst(&f, &anniversary.client, anniversary.key(), &burst_ages());
            let failed = failpoint::with_spec(spec, || f.sweep(&targets, Some(f.key), 0));
            let codes: Vec<(WarningCode, bool)> = failed
                .warnings
                .iter()
                .map(|w| {
                    (
                        w.code,
                        w.message.starts_with("pruning era's snapshots failed"),
                    )
                })
                .collect();
            assert_eq!(
                codes,
                [(WarningCode::RetentionFailed, true)],
                "{case}: {:?}",
                failed.warnings
            );
            assert_eq!(
                surviving(&f, &anniversary.client, &on_anniversary).len(),
                10,
                "{case}: the other client is still pruned"
            );
            assert!(
                !f.paths.retention_sweep_file().exists(),
                "{case}: a failed pass is not stamped"
            );

            // The next command's pass is due, and ends where an uninterrupted one would.
            let retried = f.sweep(&targets, Some(f.key), 0);
            assert!(
                retried.ran && retried.warnings.is_empty(),
                "{case}: {retried:?}"
            );
            assert_eq!(
                surviving(&f, &era.client, &on_era),
                burst_ages()[..10].to_vec(),
                "{case}: converged"
            );
            assert!(f.paths.retention_sweep_file().is_file(), "{case}: stamped");
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
    fn a_backup_line_never_takes_most_recent_protection() {
        let f = fixture();
        let old = f.commit(10 * DAY, 10);
        let last = f.commit(9 * DAY, 10);
        // A backup published after the last install: a history line with no snapshot.
        let entry = HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind: TxKind::Backup,
            at: Timestamp::from_second(f.clock.as_second() - HOUR).unwrap(),
            target: Some(f.key),
            targets: Vec::new(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into(), "backup".into(), "create".into()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: Some(BackupNote {
                id: crate::model::BackupId::new(),
                kind: crate::backup::BackupKind::Manual,
                files: 1,
                bytes: 1,
            }),
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        };
        let mut s = WriteSession::untracked(Writer::Backup, &f.scopes, "test");
        history::append(&mut s, &f.paths, &entry).unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!((report.pruned_tx, report.kept_tx), (1, 1), "{report:?}");
        assert_eq!(f.has_snapshot(&old), (false, false));
        assert_eq!(
            f.has_snapshot(&last),
            (true, true),
            "the last install stays the most recent"
        );
    }

    /// A repair line (M4-D31): target-less, its client in `targets`.
    fn repair_line(key: TargetKey, at: Timestamp) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: TxId::new(),
            kind: TxKind::Repair,
            at,
            target: None,
            targets: vec![key],
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into(), "doctor".into(), "fix".into()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(crate::model::RunId::new()),
            backup: None,
            plan: None,
            doctor_run_id: Some("2026-10-25T12:00:00Z__01M9Z00000000000000000000".into()),
            repairs: Vec::new(),
        }
    }

    #[test]
    fn a_repair_line_never_takes_most_recent_protection() {
        let f = fixture();
        let old = f.commit(10 * DAY, 10);
        let last = f.commit(9 * DAY, 10);
        // A repair after the last install: on this target, with no `<tx>` snapshot.
        let at = Timestamp::from_second(f.clock.as_second() - HOUR).unwrap();
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        history::append(&mut s, &f.paths, &repair_line(f.key, at)).unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!((report.pruned_tx, report.kept_tx), (1, 1), "{report:?}");
        assert_eq!(f.has_snapshot(&old), (false, false));
        assert_eq!(
            f.has_snapshot(&last),
            (true, true),
            "the last install stays the most recent"
        );
    }

    #[test]
    fn a_repair_lines_tx_counts_as_in_history() {
        let f = fixture();
        let repair = repair_line(f.key, Timestamp::now());
        let mut s = WriteSession::untracked(Writer::Mutating, &f.scopes, "test");
        history::append(&mut s, &f.paths, &repair).unwrap();
        let text = history::text(&f.paths).unwrap();
        assert!(in_history(&text, &repair.tx), "{text}");
    }

    /// A repair's client-side half as the repair leaves it: a reserved backup and a
    /// quarantined folder, with no `AddOns/` (M4-D36).
    fn plant_repair_half(f: &Fixture, tx: &TxId) -> PathBuf {
        let half = f.client.snapshots_repair(tx);
        for (rel, text) in [
            ("files/1", "a file a repair overwrote\n"),
            (
                "quarantine/Interface/AddOns/X/X.toc",
                "## Interface: 120105\n",
            ),
        ] {
            let path = half.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        half
    }

    #[test]
    fn retention_never_touches_repair_halves() {
        let f = fixture();
        let old = f.commit(10 * DAY, 10);
        f.commit(9 * DAY, 10);
        // Unnamed by history, past every bound (two hours on), its lock free, its
        // `AddOns/` empty: everything that makes an ordinary half an orphan.
        let repair = TxId::new();
        let half = plant_repair_half(&f, &repair);
        let state_half = f
            .paths
            .state_snapshots_dir()
            .join(format!("{repair}.repair"));
        std::fs::create_dir_all(&state_half).unwrap();
        let orphan = TxId::new();
        std::fs::create_dir_all(f.client.snapshots(&orphan)).unwrap();
        let before = crate::engine::testing::tree_hash(&half);
        let limits = RetentionLimits {
            blob_cache_max_bytes: 0,
            ..RetentionLimits::default()
        };
        let report = f.run(&limits, &BTreeSet::new());
        assert_eq!(
            f.has_snapshot(&old),
            (false, false),
            "the run pruned: {report:?}"
        );
        assert!(
            !f.client.snapshots(&orphan).exists(),
            "and swept: {report:?}"
        );
        assert_eq!(
            crate::engine::testing::tree_hash(&half),
            before,
            "the repair half"
        );
        assert!(state_half.is_dir(), "a .repair name on the state side too");
    }

    #[test]
    fn a_repair_half_named_for_a_pruned_transaction_survives() {
        let f = fixture();
        let old = f.commit(10 * DAY, 10);
        f.commit(9 * DAY, 10);
        let half = plant_repair_half(&f, &old);
        let before = crate::engine::testing::tree_hash(&half);
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(f.has_snapshot(&old), (false, false), "{report:?}");
        assert_eq!(
            crate::engine::testing::tree_hash(&half),
            before,
            "retention names only <tx> and <tx>.wtf halves"
        );
        assert_eq!(
            snapshot_dirs(&f.paths, &f.client, &old).map(|p| p.ends_with(format!("{old}.repair"))),
            [false; 4],
            "snapshot_dirs never names a .repair half"
        );
    }

    #[test]
    fn the_orphan_sweep_skips_repair_names_by_suffix() {
        let f = fixture();
        let (repair, plain) = (TxId::new(), TxId::new());
        let repaired = [
            f.client.meta_dir().join(format!("stage/{repair}.repair")),
            f.client
                .meta_dir()
                .join(format!("snapshots/{repair}.repair")),
            f.paths
                .state_snapshots_dir()
                .join(format!("{repair}.repair")),
        ];
        for dir in repaired.iter().chain([&f.client.stage(&plain)]) {
            std::fs::create_dir_all(dir).unwrap();
        }
        let found = orphans(&f.paths, &f.client, f.clock, "").unwrap();
        assert_eq!(found.scratch, [f.client.stage(&plain)], "the sweep runs");
        for dir in &repaired {
            assert!(
                !found.scratch.contains(dir) && !found.snapshots.contains(dir),
                "{} is doctor gc's alone",
                dir.display()
            );
        }
    }

    /// A journal under its provisional name keeps its transaction alive (E14): an aged
    /// stage, in-flight lock and state snapshot of `tx` are no orphans while
    /// `<tx>.reactivating.jsonl` exists (an undo of a set-aside put them back), and are
    /// without it.
    #[test]
    fn a_provisional_journal_keeps_its_transaction_alive() {
        let f = fixture();
        let tx = TxId::new();
        let stage = f.client.stage(&tx);
        let snapshot = f.paths.state_snapshot(&tx);
        let inflight = f.paths.inflight_lock(&tx);
        for dir in [&stage, &snapshot] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(f.paths.inflight_dir()).unwrap();
        std::fs::write(&inflight, b"").unwrap();
        let provisional = crate::engine::repair::provisional_journal(&f.paths, &tx);
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        std::fs::write(&provisional, b"{}\n").unwrap();
        let named = |found: &Orphans, path: &PathBuf| {
            found.scratch.contains(path) || found.snapshots.contains(path)
        };

        let kept = orphans(&f.paths, &f.client, f.clock, "").unwrap();
        for path in [&stage, &inflight, &snapshot] {
            assert!(!named(&kept, path), "{} is kept", path.display());
        }
        std::fs::remove_file(&provisional).unwrap();
        let swept = orphans(&f.paths, &f.client, f.clock, "").unwrap();
        for path in [&stage, &inflight, &snapshot] {
            assert!(named(&swept, path), "{} is an orphan", path.display());
        }
    }

    #[test]
    fn cache_quarantine_is_never_visited() {
        let f = fixture();
        f.commit(HOUR, 1);
        let sha = "ab".repeat(32);
        let blob = f.paths.blob(&sha);
        let quarantined = f.paths.cache_quarantine("R").join("blobs").join(&sha);
        for path in [&blob, &quarantined] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![b'x'; 64]).unwrap();
        }
        let limits = RetentionLimits {
            blob_cache_max_bytes: 0,
            ..RetentionLimits::default()
        };
        let report = f.run(&limits, &BTreeSet::new());
        assert!(!blob.exists(), "eviction ran: {report:?}");
        assert!(
            quarantined.is_file(),
            "the cache quarantine is doctor gc's alone"
        );
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
        // Scratch locks (planning, dry runs) live in the cache: a dead one is swept, a
        // held one keeps its cache scratch.
        let (dead_scratch, held_scratch) = (TxId::new(), TxId::new());
        std::fs::create_dir_all(f.paths.cache_inflight_dir()).unwrap();
        std::fs::write(f.paths.cache_inflight_lock(&dead_scratch), b"").unwrap();
        let _scratch = InflightLock::create_scratch(&f.paths, &held_scratch).unwrap();
        std::fs::create_dir_all(f.paths.cache_tmp(&held_scratch)).unwrap();

        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 3, "{report:?}");
        assert!(!stage(&dead).exists() && !f.paths.inflight_lock(&dead).exists());
        assert!(stage(&held).exists() && f.paths.inflight_lock(&held).exists());
        assert!(stage(&journaled).exists(), "journal recovery owns it");
        assert!(!f.paths.cache_inflight_lock(&dead_scratch).exists());
        assert!(f.paths.cache_inflight_lock(&held_scratch).exists());
        assert!(
            f.paths.cache_tmp(&held_scratch).exists(),
            "live scratch is kept"
        );
    }

    /// A restore's stage and journal carry `.wtf` (hidden from pre-M3 builds): the same
    /// rules, a dead one swept and a journaled one kept for recovery.
    #[test]
    fn restore_stages_are_swept_by_the_same_rules() {
        let f = fixture();
        f.commit(HOUR, 1);
        let (dead, journaled) = (TxId::new(), TxId::new());
        for tx in [&dead, &journaled] {
            std::fs::create_dir_all(f.client.stage_wtf(tx).join("WTF")).unwrap();
        }
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        std::fs::write(f.paths.journal_wtf(&journaled), b"").unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 1, "{report:?}");
        assert!(!f.client.stage_wtf(&dead).exists());
        assert!(
            f.client.stage_wtf(&journaled).join("WTF").is_dir(),
            "journal recovery owns it"
        );
    }

    /// A settings transaction's snapshot halves carry `.wtf` too (omad-aepa), and follow
    /// the same rules: a committed restore's go by age with their transaction, its
    /// `WTF.previous` left out of the cap; a dead one's state half goes and its client half,
    /// holding `WTF.previous`, stays; a dead one holding nothing goes whole; a journaled
    /// one stays for recovery.
    #[test]
    fn restore_snapshots_named_wtf_follow_the_same_rules() {
        let f = fixture();
        let wtf_named = |tx: &TxId| {
            std::fs::rename(f.client.snapshots(tx), f.client.snapshots_wtf(tx)).unwrap();
            std::fs::rename(f.paths.state_snapshot(tx), f.paths.state_snapshot_wtf(tx)).unwrap();
        };
        let has = |tx: &TxId| {
            (
                f.client.snapshots_wtf(tx).exists(),
                f.paths.state_snapshot_wtf(tx).exists(),
            )
        };
        let aged = f.commit(10 * DAY, 10);
        let restored = f.commit(2 * DAY, 10);
        f.commit(HOUR, 10);
        for tx in [&aged, &restored] {
            wtf_named(tx);
        }
        let previous = f.client.snapshots_wtf(&restored).join(WTF_PREVIOUS);
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join("Config.wtf"), vec![b'x'; 1000]).unwrap();
        let report = f.run(&limits(100), &BTreeSet::new());
        assert_eq!(report.pruned_tx, 1, "40 bytes count, not 1040: {report:?}");
        assert_eq!(has(&aged), (false, false), "pruned by age");
        assert_eq!(has(&restored), (true, true));

        let [holding, empty, journaled] = [(); 3].map(|()| TxId::new());
        for tx in [&holding, &empty, &journaled] {
            std::fs::create_dir_all(f.client.snapshots_wtf(tx)).unwrap();
            std::fs::create_dir_all(f.paths.state_snapshot_wtf(tx)).unwrap();
        }
        for tx in [&holding, &journaled] {
            std::fs::create_dir_all(f.client.snapshots_wtf(tx).join(WTF_PREVIOUS)).unwrap();
        }
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        std::fs::write(f.paths.journal_wtf(&journaled), b"").unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 3, "{report:?}");
        assert_eq!(has(&holding), (true, false), "its WTF.previous");
        assert_eq!(has(&empty), (false, false));
        assert_eq!(has(&journaled), (true, true), "recovery owns it");
    }

    #[test]
    fn snapshots_of_transactions_that_never_committed_are_swept_once_old() {
        let f = fixture();
        let committed = f.commit(HOUR, 1);
        // Two targets can share one client dir: another target's commit is no orphan.
        let shared = f.commit_as(TargetKey::new(), HOUR, 1);
        let [dead, held, journaled, holding, torn] = [(); 5].map(|()| TxId::new());
        for tx in [&dead, &held, &journaled, &holding, &torn] {
            for half in [
                f.client.snapshots(tx).join("WTF"),
                f.paths.state_snapshot(tx),
            ] {
                std::fs::create_dir_all(&half).unwrap();
                std::fs::write(half.join("data"), b"x").unwrap();
            }
        }
        let _live = InflightLock::create(&f.paths, &held).unwrap();
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        std::fs::write(f.paths.journal(&journaled), b"").unwrap();
        // A folder a recovery bug left behind may be the addon's only copy.
        std::fs::create_dir_all(f.client.snapshots(&holding).join("AddOns/Foo")).unwrap();
        // A committed transaction whose history line was cut short is still named.
        let mut text = std::fs::read_to_string(f.paths.history_file()).unwrap();
        text.push_str(&format!(
            "{{\"schema_version\":\"1.0\",\"tx\":\"{torn}\",\"ki"
        ));
        std::fs::write(f.paths.history_file(), text).unwrap();

        let young = run(
            &f.paths,
            &f.scopes,
            f.key,
            &f.client,
            &RetentionLimits::default(),
            Timestamp::now(),
            &|| Ok(BTreeSet::new()),
        )
        .unwrap();
        assert_eq!(young.orphans_swept, 0, "younger than ORPHAN_MIN_AGE");
        assert_eq!(f.has_snapshot(&dead), (true, true));

        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 3, "{report:?}");
        assert_eq!(f.has_snapshot(&dead), (false, false));
        assert_eq!(f.has_snapshot(&held), (true, true), "a live transaction's");
        assert_eq!(f.has_snapshot(&journaled), (true, true), "recovery owns it");
        assert_eq!(
            f.has_snapshot(&holding),
            (true, false),
            "its AddOns/ holds a folder"
        );
        assert_eq!(f.has_snapshot(&torn), (true, true), "history names it");
        assert_eq!(f.has_snapshot(&committed), (true, true));
        assert_eq!(f.has_snapshot(&shared), (true, true), "another target's");
    }

    /// A stage holding a preservation in progress (its assembly) belongs to a journal whose
    /// `preserve` line has no `preserved` yet: the sweep never takes it, however old.
    #[test]
    fn a_stage_with_a_preservation_in_progress_is_never_an_orphan() {
        let f = fixture();
        f.commit(HOUR, 1);
        let tx = TxId::new();
        let assembly = f
            .client
            .stage(&tx)
            .join("recovered/01M6X00000000000000000000A");
        std::fs::create_dir_all(assembly.join("WTF")).unwrap();
        std::fs::create_dir_all(f.paths.journal_dir()).unwrap();
        let intent = format!(
            "{{\"type\":\"preserve\",\"id\":\"01M6X00000000000000000000A\",\"items\":[],\"assembly\":{:?}}}\n",
            assembly.display().to_string()
        );
        std::fs::write(f.paths.journal(&tx), intent).unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(report.orphans_swept, 0, "{report:?}");
        assert!(assembly.join("WTF").is_dir());
    }

    #[test]
    fn a_restores_old_wtf_is_left_out_of_the_cap_and_never_an_orphan() {
        let f = fixture();
        // Past 7 days: pruned with its transaction, its old WTF/ too.
        let aged = f.commit(10 * DAY, 10);
        std::fs::create_dir_all(f.client.snapshots(&aged).join(WTF_PREVIOUS)).unwrap();
        // Past the 24 h protection: only the cap could prune it.
        let restored = f.commit(2 * DAY, 10);
        let previous = f.client.snapshots(&restored).join(WTF_PREVIOUS);
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join("Config.wtf"), vec![b'x'; 1000]).unwrap();
        f.commit(HOUR, 10);
        let report = f.run(&limits(100), &BTreeSet::new());
        assert_eq!(report.pruned_tx, 1, "40 bytes count, not 1040: {report:?}");
        assert_eq!(f.has_snapshot(&aged), (false, false), "pruned by age");
        assert!(!report.cap_exceeded);
        assert!(previous.join("Config.wtf").exists());
        // A dead restore's old WTF/ may be the only copy of those settings: the orphan
        // sweep keeps its client half (recovery owns it), the state half goes.
        let dead = TxId::new();
        std::fs::create_dir_all(f.client.snapshots(&dead).join(WTF_PREVIOUS)).unwrap();
        std::fs::create_dir_all(f.paths.state_snapshot(&dead)).unwrap();
        let report = f.run(&RetentionLimits::default(), &BTreeSet::new());
        assert_eq!(f.has_snapshot(&dead), (true, false), "{report:?}");
    }

    #[cfg(debug_assertions)]
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
