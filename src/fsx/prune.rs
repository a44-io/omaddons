//! `prune()`: the only deletion path in the program (vision §5.5). Deletion is not an
//! `Op`.
//!
//! Every victim is checked against the locations its reason may delete; one mismatch
//! refuses the whole call before anything moves. Victims are then renamed into their
//! side's `trash/` under the global lock, the lock is released, and the trash entries
//! are deleted, so the lock window stays short and a crash leaves only trash. Each
//! prune under the global lock also sweeps whatever an earlier, crashed prune left in
//! the same trash dirs. Two modes take no global lock: a transaction cleaning up its own
//! artifacts, and the plan store deleting expired plans.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;

use super::lock::{GlobalLock, GlobalLockGuard, InflightLock, Lease, LeaseGuard};
use super::scope::{ScopeTable, Writer};
use super::session::WriteSession;
use super::{FsxError, Op, RenameMode, ensure_dir_all, failpoint, mutate};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::PLAN_MAX_BYTES;
use crate::model::{PlanId, TxId};

/// Why something is deleted. Each reason may delete only its own locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneReason {
    /// Snapshots (client and state side) and blob cache entries.
    Retention,
    /// Stage dirs, cache temp dirs, journals and released in-flight locks. Under a
    /// transaction's own lock, only a journal that is committed or never started.
    PostCommitCleanup,
    /// Stored plans and their leases (M4-D24), only as expired, under
    /// [`LockUse::Expired`].
    PlanExpiry,
    /// `WTF/` backups (M3).
    BackupPrune,
    /// Old doctor runs (M4).
    DoctorGc,
    /// The Omarchy widget's hidden stages and parked old widgets (M3-D38): a published
    /// install's replaced tree, and the stage of an install whose process is gone.
    WidgetStage,
}

impl PruneReason {
    fn writer(self) -> Writer {
        match self {
            Self::Retention | Self::PostCommitCleanup => Writer::Mutating,
            Self::PlanExpiry => Writer::Derived,
            Self::BackupPrune => Writer::Backup,
            Self::DoctorGc => Writer::Doctor,
            Self::WidgetStage => Writer::Omarchy,
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
    /// No global lock, for the plan store's sweeps (M4-D24): only `<cache>/plans/<id>.json`
    /// files whose `expires_at` (read here, bounded by `PLAN_MAX_BYTES`) is not after
    /// `now`, and `<id>.lease` files of such plans, or of plans already gone, that fsx takes
    /// itself. Any other victim refuses the whole call with `scope_denied` before anything
    /// moves.
    Expired {
        now: Timestamp,
    },
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
    kind: Kind,
    /// An in-flight lock file, deleted only while nobody holds it.
    inflight: bool,
    /// A transaction journal.
    journal: bool,
}

/// Deletes `victims` for `reason`. See the module docs for the steps.
pub fn prune(
    scopes: &ScopeTable,
    reason: PruneReason,
    victims: &[PathBuf],
    lock: LockUse<'_>,
) -> Result<PruneReport> {
    // Plan victims go only as expired, and `Expired` deletes nothing else, so no caller
    // can delete an unexpired plan under any lock.
    let expired = matches!(lock, LockUse::Expired { .. });
    if expired != (reason == PruneReason::PlanExpiry) {
        return Err(Error::new(
            ErrorCode::ScopeDenied,
            "plan victims are pruned only as expired, without the global lock",
            Suggestion::command(["omaddons", "status"]),
        ));
    }
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
    // Held until every victim is in trash, so an applier never holds a deleted lease.
    let _leases = match &lock {
        LockUse::Expired { now } => only_expired(scopes, &planned, *now)?,
        LockUse::Acquire | LockUse::Held(_) | LockUse::Own(_) => Vec::new(),
    };

    let own = match &lock {
        LockUse::Own(inflight) => Some(inflight.tx().to_string()),
        LockUse::Acquire | LockUse::Held(_) | LockUse::Expired { .. } => None,
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
    // A writer cleaning up after itself never deletes the journal recovery still needs:
    // only the next global lock holder finishes or rolls back an unfinished one.
    if own.is_some()
        && let Some(open) = planned.iter().find(|p| p.journal && unfinished(&p.victim))
    {
        return Err(Error::new(
            ErrorCode::ScopeDenied,
            format!(
                "refusing to prune an unfinished journal ({}): the next omaddons command that commits a change recovers it",
                open.victim.display()
            ),
            Suggestion::command(["omaddons", "status"]),
        ));
    }
    let lock_held = matches!(lock, LockUse::Held(_));
    // Trash a crashed prune left is swept only under the global lock.
    let sweeps = matches!(lock, LockUse::Acquire | LockUse::Held(_));
    let acquired = match lock {
        LockUse::Acquire => Some(GlobalLock::acquire(scopes.paths(), "prune")?),
        LockUse::Held(_) | LockUse::Own(_) | LockUse::Expired { .. } => None,
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
        let into_trash = |mode| Op::Rename { to: &entry, mode };
        match mutate(
            &mut session,
            &plan.victim,
            into_trash(RenameMode::NoReplace),
        ) {
            // Without renameat2 flags (NTFS via ntfs-3g, some FUSE): the entry's name is
            // unique to this prune, so once it is confirmed absent a plain rename cannot
            // overwrite anything.
            Err(FsxError::Unsupported) if fs::symlink_metadata(&entry).is_err() => {
                mutate(&mut session, &plan.victim, into_trash(RenameMode::Replace))?;
            }
            other => {
                other?;
            }
        }
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
    if sweeps {
        for dir in &trash_dirs {
            sweep(dir);
        }
        drop_foreign_trash(scopes, &trash_dirs, lock_held);
    }
    Ok(report)
}

/// The widget's trash sits in Omarchy's plugins directory, which omaddons does not own,
/// so it goes once empty: under the global lock, which every rename into it holds, so
/// it never disappears under another prune's rename. A busy lock leaves it for the next
/// prune.
fn drop_foreign_trash(scopes: &ScopeTable, dirs: &BTreeSet<PathBuf>, lock_held: bool) {
    let plugins = scopes.omarchy_plugins_dir();
    let foreign: Vec<&PathBuf> = dirs
        .iter()
        .filter(|dir| dir.parent() == Some(plugins))
        .collect();
    if foreign.is_empty() {
        return;
    }
    let _guard = if lock_held {
        None
    } else {
        match GlobalLock::acquire(scopes.paths(), "prune") {
            Ok(guard) => Some(guard),
            Err(_) => return,
        }
    };
    for dir in foreign {
        // Only an empty directory is removed.
        let _ = fs::remove_dir(dir);
    }
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
        kind,
        inflight: kind == Kind::Inflight,
        journal: kind == Kind::Journal,
    }))
}

/// `LockUse::Expired`'s check, before anything moves: every victim is an expired plan,
/// or the lease of one (or of a plan already gone) that nobody holds. Returns the leases,
/// taken, for the caller to hold until its victims are in trash.
fn only_expired(
    scopes: &ScopeTable,
    planned: &[Planned],
    now: Timestamp,
) -> Result<Vec<LeaseGuard>> {
    let refuse = |victim: &Path, why: &str| {
        Err(Error::new(
            ErrorCode::ScopeDenied,
            format!("refusing to prune {}: {why}", victim.display()),
            Suggestion::command(["omaddons", "status"]),
        ))
    };
    let plans: BTreeSet<&Path> = planned
        .iter()
        .filter(|p| p.kind == Kind::Plan)
        .map(|p| p.victim.as_path())
        .collect();
    for p in planned {
        match p.kind {
            Kind::Plan if !expired_plan(&p.victim, now) => {
                return refuse(&p.victim, "not an expired plan");
            }
            Kind::Plan | Kind::PlanLease => {}
            _ => {
                return refuse(
                    &p.victim,
                    "only expired plans and their leases go without the global lock",
                );
            }
        }
    }
    let mut lease_paths: BTreeSet<PathBuf> = plans
        .iter()
        .map(|plan| plan.with_extension("lease"))
        .collect();
    for p in planned.iter().filter(|p| p.kind == Kind::PlanLease) {
        // An orphan lease guards nothing: no plan of its id exists or will again.
        let plan = p.victim.with_extension("json");
        if !plans.contains(plan.as_path()) && fs::symlink_metadata(&plan).is_ok() {
            return refuse(&p.victim, "its plan is not pruned with it");
        }
        lease_paths.insert(p.victim.clone());
    }
    let mut leases = Vec::new();
    for path in lease_paths {
        // Guard every plan, even if its lease appeared after the caller listed it.
        // Creating a missing lease also prevents an applier from entering mid-prune.
        match Lease::try_acquire(scopes, &path, "prune") {
            Ok(Some(guard)) => leases.push(guard),
            Ok(None) => return refuse(&path, "a held lease is never unlinked"),
            Err(err) => return refuse(&path, &format!("its lease cannot be taken: {err}")),
        }
    }
    Ok(leases)
}

/// Whether the stored plan at `path` has expired by `now`: a regular file, read without
/// following a symlink and at most `PLAN_MAX_BYTES`, whose `expires_at` is not after
/// `now`. Only that key is decoded, so fsx does not depend on the plan's type.
fn expired_plan(path: &Path, now: Timestamp) -> bool {
    use rustix::fs::{Mode, OFlags};
    #[derive(Deserialize)]
    struct Expiry {
        expires_at: Timestamp,
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(fd) = rustix::fs::open(path, flags, Mode::empty()) else {
        return false;
    };
    let file = File::from(fd);
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= PLAN_MAX_BYTES)
    {
        return false;
    }
    let mut bytes = Vec::new();
    if file
        .take(PLAN_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > PLAN_MAX_BYTES
    {
        return false;
    }
    serde_json::from_slice::<Expiry>(&bytes).is_ok_and(|plan| plan.expires_at <= now)
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
    /// `<cache>/plans/<id>.json`
    Plan,
    /// `<cache>/plans/<id>.lease`
    PlanLease,
    DoctorRun,
    Blob,
    CacheTmp,
    /// `<plugins>/.omaddons.stage-<op>`
    OmarchyStage,
    /// `<plugins>/.omaddons.old-<op>`
    OmarchyOld,
}

impl Kind {
    fn reason(self) -> PruneReason {
        match self {
            Self::ClientSnapshot | Self::StateSnapshot | Self::Blob => PruneReason::Retention,
            Self::ClientStage | Self::Journal | Self::Inflight | Self::CacheTmp => {
                PruneReason::PostCommitCleanup
            }
            Self::Plan | Self::PlanLease => PruneReason::PlanExpiry,
            Self::ClientBackup => PruneReason::BackupPrune,
            Self::DoctorRun => PruneReason::DoctorGc,
            Self::OmarchyStage | Self::OmarchyOld => PruneReason::WidgetStage,
        }
    }
}

/// The kind of a canonical victim and its side's trash dir, if it is exactly one entry
/// below a prunable category dir with a well-formed name.
fn location(scopes: &ScopeTable, victim: &Path) -> Option<(Kind, PathBuf)> {
    let is_tx = |name: &str| name.parse::<TxId>().is_ok();
    let is_plan = |name: &str| name.parse::<PlanId>().is_ok();
    // A stage, journal or snapshot may also be named `<tx>.wtf` (`state::paths::tx_named`).
    let is_tx_named = |name: &str| crate::state::paths::tx_named(name).is_some();
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
            "snapshots" if is_tx_named(name) => Kind::ClientSnapshot,
            "stage" if is_tx_named(name) => Kind::ClientStage,
            "backups" if is_plain(name) => Kind::ClientBackup,
            _ => return None,
        };
        let trash = victim.parent()?.parent()?.join("trash");
        return Some((kind, trash));
    }
    if let Ok(rel) = victim.strip_prefix(scopes.state_dir()) {
        let c: Vec<&str> = rel.iter().map(|s| s.to_str().unwrap_or("")).collect();
        let kind = match c.as_slice() {
            ["snapshots", name] if is_tx_named(name) => Kind::StateSnapshot,
            ["journal", name] if name.strip_suffix(".jsonl").is_some_and(is_tx_named) => {
                Kind::Journal
            }
            ["inflight", name] if name.strip_suffix(".lock").is_some_and(is_tx) => Kind::Inflight,
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
            ["inflight", name] if name.strip_suffix(".lock").is_some_and(is_tx) => Kind::Inflight,
            // The store's `.lock` and its temporary files are never victims.
            ["plans", name] if name.strip_suffix(".json").is_some_and(is_plan) => Kind::Plan,
            ["plans", name] if name.strip_suffix(".lease").is_some_and(is_plan) => Kind::PlanLease,
            _ => return None,
        };
        return Some((kind, scopes.cache_dir().join("trash")));
    }
    if victim.parent() == Some(scopes.omarchy_plugins_dir()) {
        let paths = scopes.paths();
        let name = victim.file_name()?.to_str()?;
        let kind = if paths.omarchy_stage_op(name).is_some() {
            Kind::OmarchyStage
        } else if paths.omarchy_old_op(name).is_some() {
            Kind::OmarchyOld
        } else {
            return None;
        };
        let trash = paths.omarchy_trash();
        return Some((kind, scopes.omarchy_plugins_dir().join(trash.file_name()?)));
    }
    None
}

/// Whether the journal at `path` belongs to a transaction that started but did not
/// finish: its first complete line is `prepared` and its last is not `committed`. A torn
/// or missing `prepared` line means nothing ran. An unreadable journal counts as
/// unfinished. The format is the engine's (`engine::journal`); fsx reads only the tags.
fn unfinished(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return true;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut complete: Vec<&str> = text.lines().collect();
    if !text.ends_with('\n') {
        complete.pop();
    }
    let tag = |line: Option<&&str>| {
        line.and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .and_then(|v| v.get("type")?.as_str().map(str::to_owned))
    };
    tag(complete.first()).as_deref() == Some("prepared")
        && tag(complete.last()).as_deref() != Some("committed")
}

/// Canonicalizes `dir` through its deepest existing ancestor, appending the missing rest.
pub(super) fn canonical_lenient(dir: &Path) -> Option<PathBuf> {
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
    if meta.is_dir() {
        grant_owner(entry);
    }
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

/// Gives every directory under `dir` (itself included) the owner access a delete needs.
/// A stored tree keeps exact modes (a restore's stage, a `WTF.previous`, an imported
/// backup), and a `0500` directory cannot be emptied otherwise. Top down, so a directory
/// that could not be listed is listed once granted; symlinks are never followed. Best
/// effort: the delete reports what still fails.
fn grant_owner(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let mode = meta.permissions().mode() & 0o7777;
        if mode & 0o700 != 0o700 {
            let _ = super::sys::chmod(&dir, mode | 0o700);
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                pending.push(entry.path());
            }
        }
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

    /// A widget stage or parked old widget goes through `<plugins>/.omaddons.trash`, for
    /// `WidgetStage` only; the widget itself, other plugins and malformed names are not
    /// prunable.
    #[test]
    fn widget_stages_and_old_trees_are_pruned_for_widget_stage_only() {
        let f = fixture();
        let op: TxId = TxId::new();
        let stage = make(&f.paths.omarchy_stage(&op), true);
        let old = make(&f.paths.omarchy_old(&op), true);
        for victim in [&stage, &old] {
            let err = prune(
                &f.scopes,
                PruneReason::PostCommitCleanup,
                std::slice::from_ref(victim),
                LockUse::Acquire,
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::ScopeDenied, "{victim:?}");
        }
        let report = prune(
            &f.scopes,
            PruneReason::WidgetStage,
            &[stage.clone(), old.clone()],
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(report.removed, vec![stage.clone(), old.clone()]);
        assert!(!stage.exists() && !old.exists());
        // Omarchy's directory is not omaddons': the emptied trash goes too.
        assert!(!f.paths.omarchy_trash().exists());
        let plugins = f.paths.omarchy_plugins_dir();
        for stranger in [
            make(&f.paths.omarchy_plugin_dir(), true),
            make(&plugins.join("beads"), true),
            make(&plugins.join(".omaddons.stage"), true),
            make(&plugins.join(".omaddons.stage-notaulid"), true),
        ] {
            let err = prune(
                &f.scopes,
                PruneReason::WidgetStage,
                std::slice::from_ref(&stranger),
                LockUse::Acquire,
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::ScopeDenied, "{stranger:?}");
            assert!(stranger.exists());
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
            (
                meta.join("snapshots").join(format!("{TX}.wtf")),
                PruneReason::Retention,
            ),
            (meta.join("stage").join(TX), PruneReason::PostCommitCleanup),
            (
                meta.join("stage").join(format!("{TX}.wtf")),
                PruneReason::PostCommitCleanup,
            ),
            (
                meta.join("backups/2026-10-14T21-00-00Z"),
                PruneReason::BackupPrune,
            ),
            (state.join("snapshots").join(TX), PruneReason::Retention),
            (
                state.join("snapshots").join(format!("{TX}.wtf")),
                PruneReason::Retention,
            ),
            (
                state.join("journal").join(format!("{TX}.jsonl")),
                PruneReason::PostCommitCleanup,
            ),
            (
                state.join("journal").join(format!("{TX}.wtf.jsonl")),
                PruneReason::PostCommitCleanup,
            ),
            (
                state.join("inflight").join(format!("{TX}.lock")),
                PruneReason::PostCommitCleanup,
            ),
            (
                state.join("doctor/runs/2026-10-14T21-00-00Z__abc123"),
                PruneReason::DoctorGc,
            ),
            (cache.join("blobs").join(&blob), PruneReason::Retention),
            (cache.join("tmp").join(TX), PruneReason::PostCommitCleanup),
            (
                cache.join("inflight").join(format!("{TX}.lock")),
                PruneReason::PostCommitCleanup,
            ),
            (
                cache.join("plans").join(format!("{TX}.json")),
                PruneReason::PlanExpiry,
            ),
            (
                cache.join("plans").join(format!("{TX}.lease")),
                PruneReason::PlanExpiry,
            ),
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
            meta.join("stage").join(format!("{TX}.old")),
            f.paths
                .state_dir()
                .join("journal")
                .join(format!("{TX}.old.jsonl")),
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

    #[cfg(debug_assertions)]
    #[test]
    fn without_renameat2_flags_victims_still_reach_the_trash() {
        let f = fixture();
        let tx: TxId = TX.parse().unwrap();
        let victim = f.paths.cache_tmp(&tx);
        fs::create_dir_all(victim.join("part")).unwrap();
        let victims = std::slice::from_ref(&victim);
        let report = failpoint::with_spec("fsx.renameat2.einval=error", || {
            prune(
                &f.scopes,
                PruneReason::PostCommitCleanup,
                victims,
                LockUse::Acquire,
            )
        })
        .unwrap();
        assert_eq!(report.removed, victims);
        assert!(!victim.exists());
    }

    #[test]
    fn a_tree_kept_with_exact_modes_is_deleted_whole() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let tx: TxId = TX.parse().unwrap();
        let client = crate::state::paths::ClientPaths::new(&f.client);
        // A restore's stage, a backup and a snapshot's `WTF.previous`: each a stored
        // 0500 directory holding a 0000 one.
        let backup = client.backups().join("01M6Q4ZZ000000000000000000");
        for (victim, reason) in [
            (client.stage(&tx), PruneReason::PostCommitCleanup),
            (backup, PruneReason::BackupPrune),
            (client.snapshots(&tx), PruneReason::Retention),
        ] {
            let locked = victim.join("WTF/Account/Locked");
            fs::create_dir_all(locked.join("Sealed")).unwrap();
            fs::write(locked.join("Sealed/S.lua"), b"S = 1").unwrap();
            fs::write(locked.join("L.lua"), b"L = 1").unwrap();
            for (dir, mode) in [(locked.join("Sealed"), 0o000), (locked.clone(), 0o500)] {
                fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
            }
            let victims = std::slice::from_ref(&victim);
            let report = prune(&f.scopes, reason, victims, LockUse::Acquire).unwrap();
            assert!(report.skipped.is_empty(), "{:?}", report.skipped);
            assert_eq!(report.removed, victims);
            assert_eq!(report.bytes, 10, "measured through the sealed directory");
            assert!(!victim.exists(), "{reason:?}");
        }
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
        drop(lock);

        // A scratch lock (planning, a dry run) lives in the cache and is pruned through
        // the cache's trash: the state directory is never touched.
        let scratch: TxId = "01JB8Q2VA00000000000000002".parse().unwrap();
        let lock = super::super::lock::InflightLock::create_scratch(&f.paths, &scratch).unwrap();
        assert!(super::super::lock::InflightLock::is_live(&f.paths, &scratch).unwrap());
        fs::create_dir_all(f.paths.cache_tmp(&scratch)).unwrap();
        let state_before = fs::read_dir(f.paths.state_dir())
            .map(|d| d.count())
            .unwrap_or(0);
        let mine = [
            f.paths.cache_inflight_lock(&scratch),
            f.paths.cache_tmp(&scratch),
        ];
        let report = prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            &mine,
            LockUse::Own(&lock),
        )
        .unwrap();
        assert_eq!(report.removed.len(), 2, "{report:?}");
        let state_after = fs::read_dir(f.paths.state_dir())
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(state_before, state_after, "no state trash");
        drop(lock);
        // A child another test spawns holds a copy of the lock's descriptor until it execs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while super::super::lock::InflightLock::is_live(&f.paths, &scratch).unwrap()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!super::super::lock::InflightLock::is_live(&f.paths, &scratch).unwrap());
    }

    #[test]
    fn a_transaction_never_prunes_its_own_unfinished_journal() {
        let f = fixture();
        let tx: TxId = TX.parse().unwrap();
        let lock = super::super::lock::InflightLock::create(&f.paths, &tx).unwrap();
        let journal = f.paths.journal(&tx);
        let write = |text: &str| {
            fs::create_dir_all(journal.parent().unwrap()).unwrap();
            fs::write(&journal, text).unwrap();
        };
        let prune_own = || {
            prune(
                &f.scopes,
                PruneReason::PostCommitCleanup,
                std::slice::from_ref(&journal),
                LockUse::Own(&lock),
            )
        };
        let prepared = "{\"type\":\"prepared\",\"tx\":\"x\"}\n";
        let step = "{\"type\":\"step\",\"n\":1}\n";
        let swapped = "{\"type\":\"swapped\"}\n";

        // Started and not committed (a torn `committed` line is not one): kept.
        for open in [
            format!("{prepared}{step}"),
            format!("{prepared}{step}{swapped}"),
            format!("{prepared}{step}{swapped}{{\"type\":\"committed\""),
        ] {
            write(&open);
            let err = prune_own().unwrap_err();
            assert_eq!(err.code, ErrorCode::ScopeDenied, "{open}");
            assert!(
                err.message.contains("unfinished journal"),
                "{}",
                err.message
            );
            assert!(journal.exists(), "{open}");
        }
        // Committed, or never started (torn or empty): pruned.
        for done in [
            format!("{prepared}{step}{swapped}{{\"type\":\"committed\"}}\n"),
            "{\"type\":\"prep".to_owned(),
            String::new(),
        ] {
            write(&done);
            assert_eq!(prune_own().unwrap().removed.len(), 1, "{done}");
            assert!(!journal.exists(), "{done}");
        }
        // Recovery, under the global lock, prunes what it finished or rolled back.
        write(&format!("{prepared}{step}"));
        let guard = GlobalLock::acquire(&f.paths, "test").unwrap();
        prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            std::slice::from_ref(&journal),
            LockUse::Held(&guard),
        )
        .unwrap();
        assert!(!journal.exists());
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

    // ---- Stored plans (M4-D24): only as expired, without the global lock -------------

    const NOW: &str = "2026-10-24T12:00:00Z";
    const OTHER: &str = "01JB8Q2VA00000000000000001";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    /// `<cache>/plans/<id>.json` holding `text`.
    fn plan_text(f: &Fixture, id: &str, text: &str) -> PathBuf {
        let path = f.paths.plan(&id.parse().unwrap());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// A stored plan of `id` that expires at `expires_at`.
    fn plan(f: &Fixture, id: &str, expires_at: &str) -> PathBuf {
        let text =
            format!(r#"{{"schema_version":"1.0","plan_id":"{id}","expires_at":"{expires_at}"}}"#);
        plan_text(f, id, &text)
    }

    /// `<cache>/plans/<id>.lease`, made as an applier makes it.
    fn lease(f: &Fixture, id: &str) -> PathBuf {
        let path = f.paths.plan_lease(&id.parse().unwrap());
        Lease::acquire(&f.scopes, &path, "apply", std::time::Duration::from_secs(1))
            .unwrap()
            .release();
        path
    }

    /// The lease at `path`, held by an applier.
    fn held_lease(f: &Fixture, path: &Path) -> LeaseGuard {
        Lease::acquire(&f.scopes, path, "apply", std::time::Duration::from_secs(1)).unwrap()
    }

    /// The entries of `paths` that exist, for messages.
    fn listing(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .filter(|p| fs::symlink_metadata(p).is_ok())
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// Runs `run` once for each lock mode, each with its own lock.
    fn each_lock_mode(f: &Fixture, mut run: impl FnMut(&str, LockUse<'_>)) {
        run("acquire", LockUse::Acquire);
        {
            let guard = GlobalLock::acquire(&f.paths, "test").unwrap();
            run("held", LockUse::Held(&guard));
        }
        {
            let lock = InflightLock::create(&f.paths, &TxId::new()).unwrap();
            run("own", LockUse::Own(&lock));
        }
        run("expired", LockUse::Expired { now: now() });
    }

    const REASONS: [PruneReason; 6] = [
        PruneReason::Retention,
        PruneReason::PostCommitCleanup,
        PruneReason::PlanExpiry,
        PruneReason::BackupPrune,
        PruneReason::DoctorGc,
        PruneReason::WidgetStage,
    ];

    #[test]
    fn the_state_plans_dir_is_not_prunable() {
        let f = fixture();
        let victim = make(
            &f.paths.state_dir().join("plans").join(format!("{TX}.json")),
            false,
        );
        for reason in REASONS {
            each_lock_mode(&f, |mode, lock| {
                let result = prune(&f.scopes, reason, std::slice::from_ref(&victim), lock);
                assert_eq!(
                    result.as_ref().err().map(|e| e.code),
                    Some(ErrorCode::ScopeDenied),
                    "{reason:?} under {mode}: expected scope_denied, got {result:?}"
                );
                assert!(victim.exists(), "{reason:?} under {mode}: the file is kept");
            });
        }
    }

    /// What one `Expired` call must do with its victims.
    enum Outcome {
        Removed,
        /// `scope_denied`, its message holding this, and every victim kept.
        Refused(&'static str),
    }

    #[test]
    fn expired_admits_only_expired_plans_and_free_leases() {
        use Outcome::{Refused, Removed};
        type Setup = fn(&Fixture) -> (Vec<PathBuf>, Vec<LeaseGuard>);
        // A plan that would be expired, one byte past the bound.
        let oversized: Setup = |f| {
            let head = r#"{"expires_at":"2026-10-24T11:00:00Z","pad":""#;
            let pad = usize::try_from(PLAN_MAX_BYTES).unwrap() + 1 - head.len() - 2;
            let text = format!("{head}{}\"}}", "x".repeat(pad));
            assert_eq!(text.len() as u64, PLAN_MAX_BYTES + 1);
            (vec![plan_text(f, TX, &text)], vec![])
        };
        let rows: Vec<(&str, Setup, Outcome)> = vec![
            (
                "expired plan",
                |f| (vec![plan(f, TX, "2026-10-24T11:00:00Z")], vec![]),
                Removed,
            ),
            (
                "expired plan with its free lease",
                |f| {
                    let plan = plan(f, TX, "2026-10-24T11:00:00Z");
                    (vec![plan, lease(f, TX)], vec![])
                },
                Removed,
            ),
            (
                "plan expiring exactly now",
                |f| (vec![plan(f, TX, NOW)], vec![]),
                Removed,
            ),
            (
                "unexpired plan",
                |f| (vec![plan(f, TX, "2026-10-24T13:00:00Z")], vec![]),
                Refused("not an expired plan"),
            ),
            (
                "plan without expires_at",
                |f| {
                    let text = format!(r#"{{"schema_version":"1.0","plan_id":"{TX}"}}"#);
                    (vec![plan_text(f, TX, &text)], vec![])
                },
                Refused("not an expired plan"),
            ),
            (
                "truncated plan",
                |f| (vec![plan_text(f, TX, r#"{"plan_id":"#)], vec![]),
                Refused("not an expired plan"),
            ),
            ("oversized plan", oversized, Refused("not an expired plan")),
            (
                "expired plan with a held lease",
                |f| {
                    let plan = plan(f, TX, "2026-10-24T11:00:00Z");
                    let lease = lease(f, TX);
                    let held = held_lease(f, &lease);
                    (vec![plan, lease], vec![held])
                },
                Refused("a held lease is never unlinked"),
            ),
            (
                "expired plan with a held lease omitted from victims",
                |f| {
                    let plan = plan(f, TX, "2026-10-24T11:00:00Z");
                    let held = held_lease(f, &lease(f, TX));
                    (vec![plan], vec![held])
                },
                Refused("a held lease is never unlinked"),
            ),
            (
                "orphan free lease",
                |f| (vec![lease(f, TX)], vec![]),
                Removed,
            ),
            (
                "orphan held lease",
                |f| {
                    let lease = lease(f, TX);
                    let held = held_lease(f, &lease);
                    (vec![lease], vec![held])
                },
                Refused("a held lease is never unlinked"),
            ),
            (
                "the lease of a plan not pruned with it",
                |f| {
                    plan(f, TX, "2026-10-24T11:00:00Z");
                    (vec![lease(f, TX)], vec![])
                },
                Refused("its plan is not pruned with it"),
            ),
            (
                ".lock",
                |f| (vec![make(&f.paths.plans_lock(), false)], vec![]),
                Refused("not a prunable location"),
            ),
            (
                "a blob",
                |f| {
                    let blob = f.paths.blobs_dir().join("ab".repeat(32));
                    (vec![make(&blob, false)], vec![])
                },
                Refused("entries are pruned for Retention"),
            ),
            (
                "a target record",
                |f| {
                    let record = f
                        .paths
                        .state_dir()
                        .join("targets")
                        .join(format!("{TX}.json"));
                    (vec![make(&record, false)], vec![])
                },
                Refused("not a prunable location"),
            ),
            (
                "a journal",
                |f| {
                    (
                        vec![make(&f.paths.journal(&TX.parse().unwrap()), false)],
                        vec![],
                    )
                },
                Refused("entries are pruned for PostCommitCleanup"),
            ),
            (
                "a cache in-flight lock",
                |f| {
                    let lock = f.paths.cache_inflight_lock(&TX.parse().unwrap());
                    (vec![make(&lock, false)], vec![])
                },
                Refused("entries are pruned for PostCommitCleanup"),
            ),
            (
                "an expired plan beside an unexpired one in one call",
                |f| {
                    let expired = plan(f, TX, "2026-10-24T11:00:00Z");
                    (
                        vec![expired, plan(f, OTHER, "2026-10-24T13:00:00Z")],
                        vec![],
                    )
                },
                Refused("not an expired plan"),
            ),
        ];
        for (row, setup, outcome) in rows {
            let f = fixture();
            let (victims, _held) = setup(&f);
            let result = prune(
                &f.scopes,
                PruneReason::PlanExpiry,
                &victims,
                LockUse::Expired { now: now() },
            );
            let now_there = listing(&victims);
            match outcome {
                Removed => {
                    let removed = result.as_ref().ok().map(|r| &r.removed);
                    assert_eq!(
                        removed,
                        Some(&victims),
                        "{row}: expected removed, got {result:?}; victims now {now_there:?}"
                    );
                    assert!(
                        now_there.is_empty(),
                        "{row}: expected removed; victims now {now_there:?}"
                    );
                }
                Refused(why) => {
                    let refusal = result
                        .as_ref()
                        .err()
                        .map(|e| (e.code, e.message.contains(why)));
                    assert_eq!(
                        refusal,
                        Some((ErrorCode::ScopeDenied, true)),
                        "{row}: expected scope_denied ({why}), got {result:?}; victims now {now_there:?}"
                    );
                    assert_eq!(
                        now_there.len(),
                        victims.len(),
                        "{row}: expected every victim kept; victims now {now_there:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn plan_expiry_pairs_only_with_expired() {
        let f = fixture();
        let victim = plan(&f, TX, "2026-10-24T11:00:00Z");
        each_lock_mode(&f, |mode, lock| {
            if mode == "expired" {
                return;
            }
            let result = prune(
                &f.scopes,
                PruneReason::PlanExpiry,
                std::slice::from_ref(&victim),
                lock,
            );
            let refusal = result
                .as_ref()
                .err()
                .map(|e| (e.code, e.message.contains("only as expired")));
            assert_eq!(
                refusal,
                Some((ErrorCode::ScopeDenied, true)),
                "{mode}: expected refused, got {result:?}"
            );
            assert!(victim.exists(), "{mode}: the plan is kept");
        });
        let blob = make(&f.paths.blobs_dir().join("ab".repeat(32)), false);
        let err = prune(
            &f.scopes,
            PruneReason::Retention,
            std::slice::from_ref(&blob),
            LockUse::Expired { now: now() },
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ScopeDenied, "{}", err.message);
        assert!(blob.exists());
    }

    #[test]
    fn expired_takes_no_global_lock() {
        let f = fixture();
        let victim = plan(&f, TX, "2026-10-24T11:00:00Z");
        let _someone = GlobalLock::acquire(&f.paths, "someone else").unwrap();
        let started = std::time::Instant::now();
        let report = prune(
            &f.scopes,
            PruneReason::PlanExpiry,
            std::slice::from_ref(&victim),
            LockUse::Expired { now: now() },
        )
        .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "it waited for the global lock"
        );
        assert_eq!(report.removed, std::slice::from_ref(&victim));
        assert!(!victim.exists());
    }

    #[test]
    fn expired_guards_a_lease_that_did_not_exist() {
        let f = fixture();
        let victim = plan(&f, TX, "2026-10-24T11:00:00Z");
        let lease = f.paths.plan_lease(&TX.parse().unwrap());
        assert!(!lease.exists());
        let planned = classify(&f.scopes, PruneReason::PlanExpiry, &victim)
            .unwrap()
            .unwrap();
        let guards = only_expired(&f.scopes, &[planned], now()).unwrap();
        assert!(lease.exists());
        assert!(
            Lease::try_acquire(&f.scopes, &lease, "apply")
                .unwrap()
                .is_none()
        );
        drop(guards);
        assert!(
            Lease::try_acquire(&f.scopes, &lease, "apply")
                .unwrap()
                .is_some()
        );
        assert!(victim.exists());
    }

    #[test]
    fn expired_leaves_foreign_trash_alone() {
        let f = fixture();
        let trash = f.paths.cache_trash();
        let crashed = make(&trash.join("01OLDPRUNE-0-crashed"), true);
        let victim = plan(&f, TX, "2026-10-24T11:00:00Z");
        prune(
            &f.scopes,
            PruneReason::PlanExpiry,
            std::slice::from_ref(&victim),
            LockUse::Expired { now: now() },
        )
        .unwrap();
        assert!(!victim.exists());
        assert!(
            crashed.exists(),
            "only a prune under the global lock sweeps it"
        );
        let tmp = make(&f.paths.cache_dir().join("tmp").join(TX), true);
        prune(
            &f.scopes,
            PruneReason::PostCommitCleanup,
            &[tmp],
            LockUse::Acquire,
        )
        .unwrap();
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
    }
}
