//! `prune()`: the only deletion path in the program (vision §5.5). Deletion is not an
//! `Op`.
//!
//! Every victim is checked against the locations its reason may delete; one mismatch
//! refuses the whole call before anything moves. Victims are then renamed into their
//! side's `trash/` under the global lock, the lock is released, and the trash entries
//! are deleted, so the lock window stays short and a crash leaves only trash. Each
//! prune also sweeps whatever an earlier, crashed prune left in the same trash dirs.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Component, Path, PathBuf};

use super::lock::{GlobalLock, GlobalLockGuard, InflightLock};
use super::scope::{ScopeTable, Writer};
use super::session::WriteSession;
use super::{Op, RenameMode, ensure_dir_all, failpoint, mutate};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::{PlanId, TxId};

/// Why something is deleted. Each reason may delete only its own locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneReason {
    /// Snapshots (client and state side) and blob cache entries.
    Retention,
    /// Stage dirs, cache temp dirs, committed journals and released in-flight locks.
    PostCommitCleanup,
    /// Stored plans (M4).
    PlanExpiry,
    /// `WTF/` backups (M3).
    BackupPrune,
    /// Old doctor runs (M4).
    DoctorGc,
}

impl PruneReason {
    fn writer(self) -> Writer {
        match self {
            Self::Retention | Self::PostCommitCleanup | Self::PlanExpiry => Writer::Mutating,
            Self::BackupPrune => Writer::Backup,
            Self::DoctorGc => Writer::Doctor,
        }
    }
}

/// Whether `prune` takes the global lock or runs under the caller's.
pub enum LockUse<'a> {
    Acquire,
    Held(&'a GlobalLockGuard),
    /// No global lock: every victim is named for the transaction whose in-flight lock
    /// the caller holds, and nothing else touches a live transaction's artifacts
    /// (recovery and orphan collection wait for a free in-flight lock). Transactions
    /// clean up this way, so planning and `--dry-run` never take the global lock. Trash
    /// left by crashed prunes is swept only under the global lock.
    Own(&'a InflightLock),
}

/// What a prune did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// The victims deleted, by their original paths.
    pub removed: Vec<PathBuf>,
    /// Bytes of regular files deleted.
    pub bytes: u64,
    /// Victims left alone, with the reason (already gone, held, or a delete error;
    /// failed deletes stay in trash for the next prune).
    pub skipped: Vec<(PathBuf, String)>,
}

/// A validated victim.
struct Planned {
    victim: PathBuf,
    trash: PathBuf,
    /// An in-flight lock file, deleted only while nobody holds it.
    inflight: bool,
}

/// Deletes `victims` for `reason`. See the module docs for the steps.
pub fn prune(
    scopes: &ScopeTable,
    reason: PruneReason,
    victims: &[PathBuf],
    lock: LockUse<'_>,
) -> Result<PruneReport> {
    let mut report = PruneReport::default();
    let mut planned = Vec::new();
    for victim in victims {
        match classify(scopes, reason, victim)? {
            Some(plan) => planned.push(plan),
            None => report
                .skipped
                .push((victim.clone(), "already gone".to_owned())),
        }
    }

    let own = match &lock {
        LockUse::Own(inflight) => Some(inflight.tx().to_string()),
        LockUse::Acquire | LockUse::Held(_) => None,
    };
    if let Some(tx) = &own
        && let Some(stranger) = planned.iter().find(|p| !named_for(&p.victim, tx))
    {
        return Err(Error::new(
            ErrorCode::ScopeDenied,
            format!(
                "refusing to prune {} under transaction {tx}'s own lock: it is not that transaction's",
                stranger.victim.display()
            ),
            Suggestion::command(["omaddons", "status"]),
        ));
    }
    let acquired = match lock {
        LockUse::Acquire => Some(GlobalLock::acquire(scopes.paths(), "prune")?),
        LockUse::Held(_) | LockUse::Own(_) => None,
    };
    let prune_id = ulid::Ulid::generate();
    let mut session = WriteSession::untracked(reason.writer(), scopes, "prune");
    let mut trashed = Vec::new();
    for (n, plan) in planned.into_iter().enumerate() {
        // Under `Own` the caller holds the victim's lock itself.
        if plan.inflight && own.is_none() && held(&plan.victim) {
            report
                .skipped
                .push((plan.victim, "held by a live transaction".to_owned()));
            continue;
        }
        ensure_dir_all(&mut session, &plan.trash)?;
        let name = plan
            .victim
            .file_name()
            .map(OsStr::to_string_lossy)
            .unwrap_or_default();
        let entry = plan.trash.join(format!("{prune_id}-{n}-{name}"));
        mutate(
            &mut session,
            &plan.victim,
            Op::Rename {
                to: &entry,
                mode: RenameMode::NoReplace,
            },
        )?;
        trashed.push((plan.victim, entry, plan.trash));
    }
    let fail = |err: io::Error| {
        Error::io(
            scopes.state_dir(),
            &err,
            Suggestion::command(["omaddons", "status"]),
        )
    };
    failpoint::hit("fsx.prune.renamed").map_err(fail)?;
    drop(acquired);
    failpoint::hit("fsx.prune.deleting").map_err(fail)?;

    let mut trash_dirs = BTreeSet::new();
    for (victim, entry, trash) in trashed {
        match delete_entry(&entry) {
            Ok(bytes) => {
                report.removed.push(victim);
                report.bytes = report.bytes.saturating_add(bytes);
            }
            Err(err) => report
                .skipped
                .push((victim, format!("left in trash: {err}"))),
        }
        trash_dirs.insert(trash);
    }
    if own.is_none() {
        for dir in trash_dirs {
            sweep(&dir);
        }
    }
    Ok(report)
}

/// Whether `victim` is named for transaction `tx` (`<tx>`, `<tx>.lock`, `<tx>.jsonl`).
fn named_for(victim: &Path, tx: &str) -> bool {
    victim
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            name.strip_prefix(tx)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
}

/// Checks `victim` against `reason`'s locations; `None` when it is already gone.
fn classify(scopes: &ScopeTable, reason: PruneReason, victim: &Path) -> Result<Option<Planned>> {
    let refuse = |why: &str| {
        Err(Error::new(
            ErrorCode::ScopeDenied,
            format!("refusing to prune {}: {why}", victim.display()),
            Suggestion::command(["omaddons", "status"]),
        ))
    };
    if !victim.is_absolute() || victim.components().any(|c| c == Component::ParentDir) {
        return refuse("not a normalized absolute path");
    }
    let (Some(parent), Some(name)) = (victim.parent(), victim.file_name()) else {
        return refuse("it names no entry");
    };
    // The location is checked even when the victim (or its parent) is gone, so a
    // malformed request is refused rather than quietly skipped.
    let Some(parent) = canonical_lenient(parent) else {
        return refuse("its location cannot be resolved");
    };
    let canonical = parent.join(name);
    let meta = match fs::symlink_metadata(&canonical) {
        Ok(meta) => Some(meta),
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(Error::io(
                &canonical,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    if meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return refuse("symlinks are never pruned or followed");
    }
    let Some((kind, trash)) = location(scopes, &canonical) else {
        return refuse("it is not a prunable location");
    };
    if kind.reason() != reason {
        return refuse(&format!(
            "{kind:?} entries are pruned for {:?}, not {reason:?}",
            kind.reason()
        ));
    }
    Ok(meta.map(|_| Planned {
        victim: canonical,
        trash,
        inflight: kind == Kind::Inflight,
    }))
}

/// The prunable kinds of entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ClientSnapshot,
    ClientStage,
    ClientBackup,
    StateSnapshot,
    Journal,
    Inflight,
    Plan,
    DoctorRun,
    Blob,
    CacheTmp,
}

impl Kind {
    fn reason(self) -> PruneReason {
        match self {
            Self::ClientSnapshot | Self::StateSnapshot | Self::Blob => PruneReason::Retention,
            Self::ClientStage | Self::Journal | Self::Inflight | Self::CacheTmp => {
                PruneReason::PostCommitCleanup
            }
            Self::Plan => PruneReason::PlanExpiry,
            Self::ClientBackup => PruneReason::BackupPrune,
            Self::DoctorRun => PruneReason::DoctorGc,
        }
    }
}

/// The kind of a canonical victim and its side's trash dir, if it is exactly one entry
/// below a prunable category dir with a well-formed name.
fn location(scopes: &ScopeTable, victim: &Path) -> Option<(Kind, PathBuf)> {
    let is_tx = |name: &str| name.parse::<TxId>().is_ok();
    for client in scopes.client_dirs() {
        let Ok(rel) = victim.strip_prefix(client) else {
            continue;
        };
        let c: Vec<&str> = rel.iter().map(|s| s.to_str().unwrap_or("")).collect();
        let [interface, meta, category, name] = c.as_slice() else {
            return None;
        };
        if !interface.eq_ignore_ascii_case("Interface") || !meta.eq_ignore_ascii_case(".omaddons") {
            return None;
        }
        let kind = match category.to_ascii_lowercase().as_str() {
            "snapshots" if is_tx(name) => Kind::ClientSnapshot,
            "stage" if is_tx(name) => Kind::ClientStage,
            "backups" if is_plain(name) => Kind::ClientBackup,
            _ => return None,
        };
        let trash = victim.parent()?.parent()?.join("trash");
        return Some((kind, trash));
    }
    if let Ok(rel) = victim.strip_prefix(scopes.state_dir()) {
        let c: Vec<&str> = rel.iter().map(|s| s.to_str().unwrap_or("")).collect();
        let kind = match c.as_slice() {
            ["snapshots", name] if is_tx(name) => Kind::StateSnapshot,
            ["journal", name] if name.strip_suffix(".jsonl").is_some_and(is_tx) => Kind::Journal,
            ["inflight", name] if name.strip_suffix(".lock").is_some_and(is_tx) => Kind::Inflight,
            ["plans", name]
                if name
                    .strip_suffix(".json")
                    .is_some_and(|id| id.parse::<PlanId>().is_ok()) =>
            {
                Kind::Plan
            }
            ["doctor", "runs", name] if is_plain(name) => Kind::DoctorRun,
            _ => return None,
        };
        return Some((kind, scopes.state_dir().join("trash")));
    }
    if let Ok(rel) = victim.strip_prefix(scopes.cache_dir()) {
        let c: Vec<&str> = rel.iter().map(|s| s.to_str().unwrap_or("")).collect();
        let kind = match c.as_slice() {
            ["blobs", name] if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Kind::Blob
            }
            ["tmp", name] if is_tx(name) => Kind::CacheTmp,
            _ => return None,
        };
        return Some((kind, scopes.cache_dir().join("trash")));
    }
    None
}

/// Canonicalizes `dir` through its deepest existing ancestor, appending the missing rest.
fn canonical_lenient(dir: &Path) -> Option<PathBuf> {
    let mut existing = dir;
    let mut missing = Vec::new();
    loop {
        if let Ok(mut base) = fs::canonicalize(existing) {
            base.extend(missing.iter().rev());
            return Some(base);
        }
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
    }
}

fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name != "latest"
        && !name.contains(['/', '\\'])
}

/// Whether someone holds the lock file at `path`.
fn held(path: &Path) -> bool {
    File::open(path).is_ok_and(|file| match file.try_lock() {
        Ok(()) => {
            drop(file.unlock());
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    })
}

/// Deletes one trash entry; returns the bytes of regular files it held.
fn delete_entry(entry: &Path) -> io::Result<u64> {
    let meta = match fs::symlink_metadata(entry) {
        Ok(meta) => meta,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let bytes = if meta.is_dir() {
        tree_bytes(entry)
    } else {
        meta.len()
    };
    let removed = if meta.is_dir() {
        fs::remove_dir_all(entry)
    } else {
        fs::remove_file(entry)
    };
    match removed {
        // Another prune sweeping the same trash may have got there first.
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(bytes),
    }
}

fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind) if kind.is_file() => {
                    total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
                }
                _ => {}
            }
        }
    }
    total
}

/// Deletes everything left in a trash dir: only `prune()` renames into trash, so any
/// entry is garbage from this or an earlier (possibly crashed) prune. Failures stay for
/// the next sweep.
fn sweep(trash: &Path) {
    let Ok(entries) = fs::read_dir(trash) else {
        return;
    };
    for entry in entries.flatten() {
        drop(delete_entry(&entry.path()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::paths::Paths;

    const TX: &str = "01JB8Q2VA00000000000000000";

    struct Fixture {
        _dir: tempfile::TempDir,
        scopes: ScopeTable,
        client: PathBuf,
        paths: Paths,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let paths = Paths::with_roots(
            &root,
            root.join(".config"),
            root.join(".local/state"),
            root.join(".cache"),
        );
        let client = root.join("wow/_retail_");
        fs::create_dir_all(client.join("Interface/AddOns/Foo")).unwrap();
        fs::create_dir_all(client.join("WTF")).unwrap();
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        Fixture {
            _dir: dir,
            scopes,
            client,
            paths,
        }
    }

    fn make(path: &Path, dir: bool) -> PathBuf {
        if dir {
            fs::create_dir_all(path.join("sub")).unwrap();
            fs::write(path.join("sub/f"), b"12345").unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"123").unwrap();
        }
        path.to_path_buf()
    }

    #[test]
    fn every_reason_reaches_only_its_locations() {
        let f = fixture();
        let meta = f.client.join("Interface/.omaddons");
        let state = f.paths.state_dir().to_path_buf();
        let cache = f.paths.cache_dir().to_path_buf();
        let blob = "ab".repeat(32);
        let cases = [
            (meta.join("snapshots").join(TX), PruneReason::Retention),
            (meta.join("stage").join(TX), PruneReason::PostCommitCleanup),
            (
                meta.join("backups/2026-10-14T21-00-00Z"),
                PruneReason::BackupPrune,
            ),
            (state.join("snapshots").join(TX), PruneReason::Retention),
            (
                state.join("journal").join(format!("{TX}.jsonl")),
                PruneReason::PostCommitCleanup,
            ),
            (
                state.join("inflight").join(format!("{TX}.lock")),
                PruneReason::PostCommitCleanup,
            ),
            (
                state.join("plans").join(format!("{TX}.json")),
                PruneReason::PlanExpiry,
            ),
            (
                state.join("doctor/runs/2026-10-14T21-00-00Z__abc123"),
                PruneReason::DoctorGc,
            ),
            (cache.join("blobs").join(&blob), PruneReason::Retention),
            (cache.join("tmp").join(TX), PruneReason::PostCommitCleanup),
        ];
        let reasons = [
            PruneReason::Retention,
            PruneReason::PostCommitCleanup,
            PruneReason::PlanExpiry,
            PruneReason::BackupPrune,
            PruneReason::DoctorGc,
        ];
        for (victim, own) in &cases {
            let is_file = victim.extension().is_some() || victim.starts_with(cache.join("blobs"));
            make(victim, !is_file);
            for reason in reasons {
                let result = classify(&f.scopes, reason, victim);
                if reason == *own {
                    assert!(
                        result.unwrap().is_some(),
                        "{} for {reason:?}",
                        victim.display()
                    );
                } else {
                    let err = result.err().unwrap();
                    assert_eq!(
                        err.code,
                        ErrorCode::ScopeDenied,
                        "{} for {reason:?}",
                        victim.display()
                    );
                }
            }
        }
    }

    #[test]
    fn forbidden_victims_are_refused_for_every_reason() {
        let f = fixture();
        fs::create_dir_all(f.paths.state_dir()).unwrap();
        fs::write(f.paths.lock_file(), b"").unwrap();
        let meta = f.client.join("Interface/.omaddons");
        fs::create_dir_all(meta.join("snapshots")).unwrap();
        std::os::unix::fs::symlink(
            f.client.join("Interface/AddOns/Foo"),
            meta.join("snapshots").join(TX),
        )
        .unwrap();
        let forbidden = [
            f.paths.lock_file(),
            f.client.join("Interface/AddOns/Foo"),
            f.client.join("Interface/AddOns"),
            f.client.join("WTF"),
            meta.join("snapshots"),
            meta.join("snapshots").join(TX),
            meta.join("stage/not-a-ulid"),
            f.paths.state_dir().join("registry.json"),
            PathBuf::from("relative"),
        ];
        for victim in &forbidden {
            for reason in [
                PruneReason::Retention,
                PruneReason::PostCommitCleanup,
                PruneReason::DoctorGc,
            ] {
                let err = prune(
                    &f.scopes,
                    reason,
                    std::slice::from_ref(victim),
                    LockUse::Acquire,
                )
                .unwrap_err();
                assert_eq!(err.code, ErrorCode::ScopeDenied, "{}", victim.display());
            }
        }
        assert!(f.paths.lock_file().exists());
        assert!(f.client.join("Interface/AddOns/Foo").is_dir());
    }

    #[test]
    fn prune_moves_through_trash_and_deletes() {
        let f = fixture();
        let snapshot = make(
            &f.client.join("Interface/.omaddons/snapshots").join(TX),
            true,
        );
        let state_snapshot = make(&f.paths.state_dir().join("snapshots").join(TX), true);
        let report = prune(
            &f.scopes,
            PruneReason::Retention,
            &[snapshot.clone(), state_snapshot.clone()],
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(report.removed, [snapshot.clone(), state_snapshot.clone()]);
        assert_eq!(report.bytes, 10);
        assert!(!snapshot.exists() && !state_snapshot.exists());
        for trash in [
            f.client.join("Interface/.omaddons/trash"),
            f.paths.state_dir().join("trash"),
        ] {
            assert_eq!(
                fs::read_dir(&trash).unwrap().count(),
                0,
                "{}",
                trash.display()
            );
        }
        let again = prune(
            &f.scopes,
            PruneReason::Retention,
            std::slice::from_ref(&snapshot),
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(again.skipped, [(snapshot, "already gone".to_owned())]);
    }

    #[test]
    fn a_held_guard_is_used_not_reacquired() {
        let f = fixture();
        let victim = make(&f.paths.cache_dir().join("tmp").join(TX), true);
        let guard = GlobalLock::acquire(&f.paths, "install").unwrap();
        let started = std::time::Instant::now();
        prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            std::slice::from_ref(&victim),
            LockUse::Held(&guard),
        )
        .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "it waited for its own lock"
        );
        assert!(!victim.exists());
    }

    #[test]
    fn a_transaction_prunes_its_own_artifacts_without_the_global_lock() {
        let f = fixture();
        let tx: TxId = TX.parse().unwrap();
        let other: TxId = "01JB8Q2VA00000000000000001".parse().unwrap();
        let _someone = GlobalLock::acquire(&f.paths, "someone else").unwrap();
        let lock = super::super::lock::InflightLock::create(&f.paths, &tx).unwrap();
        fs::create_dir_all(f.paths.cache_tmp(&tx).join("part")).unwrap();
        fs::create_dir_all(f.paths.cache_tmp(&other)).unwrap();

        let stranger = [f.paths.cache_tmp(&tx), f.paths.cache_tmp(&other)];
        let err = prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            &stranger,
            LockUse::Own(&lock),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ScopeDenied);
        assert!(
            f.paths.cache_tmp(&tx).exists(),
            "nothing moves on a refusal"
        );

        let mine = [f.paths.inflight_lock(&tx), f.paths.cache_tmp(&tx)];
        let report = prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            &mine,
            LockUse::Own(&lock),
        )
        .unwrap();
        assert_eq!(report.removed.len(), 2, "{report:?}");
        assert!(!f.paths.inflight_lock(&tx).exists() && !f.paths.cache_tmp(&tx).exists());
        assert!(f.paths.cache_tmp(&other).exists());
    }

    #[test]
    fn held_inflight_locks_are_skipped() {
        let f = fixture();
        let tx: TxId = TX.parse().unwrap();
        let lock = super::super::lock::InflightLock::create(&f.paths, &tx).unwrap();
        let victim = f.paths.inflight_lock(&tx);
        let victims = std::slice::from_ref(&victim);
        let report = prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            victims,
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(report.skipped.len(), 1);
        assert!(victim.exists());
        drop(lock);
        let report = prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            victims,
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(report.removed, victims);
        assert!(!victim.exists());
    }

    #[test]
    fn leftovers_in_trash_are_swept() {
        let f = fixture();
        let trash = f.paths.cache_dir().join("trash");
        make(&trash.join("01OLDPRUNE-0-crashed"), true);
        let victim = make(&f.paths.cache_dir().join("tmp").join(TX), true);
        prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            &[victim],
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
    }
}
