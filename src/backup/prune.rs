//! Backup retention (M3-D32), reclamation under space pressure (M3-D48), and the
//! deletions both make.
//!
//! Automatic backups are thinned by local day: for each of the newest
//! `limits.backup_auto_keep_days` days that hold any, the day's first pre-change backup,
//! its last post-session backup and every before-restore backup stay. Manual, imported and
//! recovered backups are the user's: only `--keep` or `--older-than`, or naming them,
//! removes them. Nothing younger than [`BACKUP_MIN_AGE`] and never the newest qualifying
//! capture goes automatically. Under space pressure, eligible automatic backups go oldest
//! first until the need is met, never a protected one.
//!
//! Every deletion is reservation-aware: a backup a reader (a restore, an export, a verify)
//! holds with `DirLock::shared` is skipped and reported busy, never waited on. The
//! directory lock is taken before the global lock, which `fsx::prune` takes only for its
//! trash rename.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Serialize;

use super::inventory::{self, Ring};
use super::{BackupKind, BackupMeta, BackupRef, Reflinked, Trigger, fs::free_bytes, list};
use crate::error::{Error, Result, Suggestion};
use crate::fsx::{DirLock, LockUse, PruneReason, ScopeTable, prune};
use crate::limits::BACKUP_MIN_AGE;
use crate::model::{BackupId, TargetKey};
use crate::state::doorbell;
use crate::state::paths::{ClientPaths, Paths};

/// How long a deletion steps around `backup list`'s own instant busy probe before it
/// takes a held reservation for a reader's.
const PROBE_RETRY: Duration = Duration::from_millis(50);

/// What a pass prunes beyond the retention rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneOpts {
    /// `backup prune <id>…`: exactly these, the 24 h protection lifted because the request
    /// is explicit. `keep` and `captured_before` do not apply.
    pub ids: Option<BTreeSet<BackupId>>,
    /// `--keep N`: also every backup beyond the newest N of its kind.
    pub keep: Option<usize>,
    /// `--older-than D`: also every manual, imported or recovered backup captured before
    /// this.
    pub captured_before: Option<Timestamp>,
    /// Ids this pass keeps whatever else it says (a restore's source, M3-D26).
    pub pins: BTreeSet<BackupId>,
}

/// The ids among `backups` that the pass `opts` describes prunes at `now`, local days
/// counted in `tz`, `keep_days` days of automatic backups kept. Pure. Never a victim: the
/// newest qualifying capture or a pinned id; nor, unless named, a backup younger than
/// [`BACKUP_MIN_AGE`] (since it was published here) or one whose metadata does not read.
pub fn select(
    backups: &[BackupRef],
    now: Timestamp,
    tz: &TimeZone,
    keep_days: usize,
    opts: &PruneOpts,
) -> Vec<BackupId> {
    let newest_qualifying = backups
        .iter()
        .filter(|b| b.qualifying)
        .max_by_key(|b| (b.captured_at(), b.id))
        .map(|b| b.id);
    // ubs:ignore — comparing backup ids, not secrets
    let always = |b: &BackupRef| opts.pins.contains(&b.id) || newest_qualifying == Some(b.id);
    if let Some(ids) = &opts.ids {
        return backups
            .iter()
            .filter(|b| ids.contains(&b.id) && !always(b))
            .map(|b| b.id)
            .collect();
    }
    let min_age = i64::try_from(BACKUP_MIN_AGE.as_secs()).unwrap_or(i64::MAX);
    let young = |b: &BackupRef| {
        b.meta
            .as_ref()
            .is_none_or(|m| now.as_second().saturating_sub(m.created_at.as_second()) < min_age)
    };
    let daily = by_day(backups, tz, keep_days);
    let newest = opts.keep.map(|n| newest_of_each_kind(backups, n));
    let beyond_keep = |b: &BackupRef| newest.as_ref().is_some_and(|kept| !kept.contains(&b.id));
    let old = |b: &BackupRef| {
        opts.captured_before
            .is_some_and(|cutoff| b.captured_at().is_some_and(|at| at < cutoff))
    };
    backups
        .iter()
        .filter(|b| !always(b) && !young(b))
        .filter(|b| {
            let Some(meta) = &b.meta else { return false };
            if automatic(meta.kind) {
                !daily.contains(&b.id) || beyond_keep(b)
            } else {
                beyond_keep(b) || old(b)
            }
        })
        .map(|b| b.id)
        .collect()
}

/// Whether retention manages backups of `kind` without being asked (M3-D32): automatic
/// checkpoints and the copies a restore takes first.
fn automatic(kind: BackupKind) -> bool {
    matches!(kind, BackupKind::Auto | BackupKind::BeforeRestore)
}

/// The newest `n` of each kind, by capture.
fn newest_of_each_kind(backups: &[BackupRef], n: usize) -> BTreeSet<BackupId> {
    let mut by_kind: BTreeMap<BackupKind, Vec<&BackupRef>> = BTreeMap::new();
    for b in backups {
        if let Some(meta) = &b.meta {
            by_kind.entry(meta.kind).or_default().push(b);
        }
    }
    by_kind
        .into_values()
        .flat_map(|mut of_kind| {
            of_kind.sort_by_key(|b| std::cmp::Reverse((b.captured_at(), b.id)));
            of_kind.into_iter().take(n).map(|b| b.id)
        })
        .collect()
}

/// The automatic backups the per-day rule keeps: for each of the newest `keep_days` local
/// days (in `tz`) holding any automatic backup, its first pre-change backup, its last
/// post-session backup and every before-restore backup.
fn by_day(backups: &[BackupRef], tz: &TimeZone, keep_days: usize) -> BTreeSet<BackupId> {
    let mut days: BTreeMap<Date, Vec<&BackupRef>> = BTreeMap::new();
    for b in backups {
        let Some(meta) = &b.meta else { continue };
        if automatic(meta.kind) {
            let day = meta.captured_at.to_zoned(tz.clone()).date();
            days.entry(day).or_default().push(b);
        }
    }
    let mut kept = BTreeSet::new();
    for (_, of_day) in days.into_iter().rev().take(keep_days) {
        let auto = |trigger: Trigger| {
            of_day.iter().filter(move |b| {
                b.meta.as_ref().is_some_and(|m| {
                    // ubs:ignore — comparing backup triggers, not secrets
                    matches!(m.kind, BackupKind::Auto) && m.trigger == Some(trigger)
                })
            })
        };
        let at = |b: &&&BackupRef| (b.captured_at(), b.id);
        kept.extend(auto(Trigger::PreChange).min_by_key(at).map(|b| b.id));
        kept.extend(auto(Trigger::PostSession).max_by_key(at).map(|b| b.id));
        kept.extend(
            of_day
                .iter()
                .filter(|b| {
                    b.meta
                        .as_ref()
                        .is_some_and(|m| matches!(m.kind, BackupKind::BeforeRestore))
                })
                .map(|b| b.id),
        );
    }
    kept
}

/// One backup a pass deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Pruned {
    pub id: BackupId,
    /// `None` for a backup whose metadata did not read (only an explicit request
    /// deletes one).
    pub kind: Option<BackupKind>,
    /// Apparent bytes.
    pub bytes: u64,
}

/// What a pass did on one target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub pruned: Vec<Pruned>,
    /// Victims a reader held: skipped, deleted on a later pass.
    pub busy: Vec<BackupId>,
    /// The backups left.
    pub kept: usize,
    /// The deletions came from a reclamation pass under space pressure.
    pub reclaimed_for_space: bool,
}

impl PruneReport {
    fn new(backups: &[BackupRef]) -> Self {
        Self {
            kept: backups.len(),
            ..Self::default()
        }
    }

    fn note(&mut self, b: &BackupRef, deleted: Deleted) {
        match deleted {
            Deleted::Pruned(pruned) => {
                self.pruned.push(pruned);
                self.kept = self.kept.saturating_sub(1);
            }
            Deleted::Busy => self.busy.push(b.id),
            Deleted::Gone => self.kept = self.kept.saturating_sub(1),
        }
    }
}

/// What became of one victim.
enum Deleted {
    Pruned(Pruned),
    /// A reader holds it.
    Busy,
    /// Another pass deleted it first.
    Gone,
}

/// Deletes the backups `victims` of `backups` (none unless listed there), each under an
/// exclusive reservation taken without waiting: one a reader holds is reported busy and
/// kept. Records the deletions in the inventory projection and rings the doorbell for
/// `ring`, also when a deletion fails part way.
pub fn delete(
    paths: &Paths,
    scopes: &ScopeTable,
    client: &ClientPaths,
    key: Option<&TargetKey>,
    backups: &[BackupRef],
    victims: &[BackupId],
    ring: Ring<'_>,
) -> Result<PruneReport> {
    let mut report = PruneReport::new(backups);
    let done = backups
        .iter()
        .filter(|b| victims.contains(&b.id))
        .try_for_each(|b| {
            report.note(b, delete_one(scopes, b)?);
            Ok(())
        });
    recorded(paths, client, key, &report, ring);
    done.map(|()| report)
}

/// Deletes `b` under its reservation: taken before `fsx::prune` takes the global lock for
/// the trash rename (M3-D48's lock order), dropped once the backup is out of `backups/`.
fn delete_one(scopes: &ScopeTable, b: &BackupRef) -> Result<Deleted> {
    let reservation = match reserve(&b.dir)? {
        Reserved::Held(lock) => lock,
        Reserved::Busy => return Ok(Deleted::Busy),
        Reserved::Gone => return Ok(Deleted::Gone),
    };
    let pruned = prune(
        scopes,
        PruneReason::BackupPrune,
        std::slice::from_ref(&b.dir),
        LockUse::Acquire,
    );
    drop(reservation);
    pruned?;
    // Deleted, or left in trash for the next prune to finish: no longer a backup either
    // way. Its reservation kept every other pass from taking it first.
    Ok(Deleted::Pruned(Pruned {
        id: b.id,
        kind: b.meta.as_ref().map(|m| m.kind),
        bytes: b.meta.as_ref().map_or(0, |m| m.bytes),
    }))
}

/// A victim's reservation.
enum Reserved {
    Held(DirLock),
    Busy,
    Gone,
}

/// The exclusive reservation of the backup at `dir`, taken without waiting. Retried once
/// after [`PROBE_RETRY`]: `backup list` holds each backup an instant to probe it, and that
/// is no reader. A backup another pass moved away meanwhile is gone.
fn reserve(dir: &Path) -> Result<Reserved> {
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(PROBE_RETRY);
        }
        match DirLock::try_exclusive(dir) {
            Ok(Some(lock)) if lock.same_dir(dir) => return Ok(Reserved::Held(lock)),
            Ok(Some(_)) => return Ok(Reserved::Gone),
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Reserved::Gone),
            Err(err) => {
                let fix = Suggestion::command(["omaddons", "backup", "list"]);
                return Err(Error::io(dir, &err, fix));
            }
        }
    }
    Ok(Reserved::Busy)
}

/// Records a pass's deletions in `client`'s inventory projection and rings the doorbell
/// once for them: the rebuild rings when it changed the projection, else (an unregistered
/// target, a write that failed) the ring is explicit.
fn recorded(
    paths: &Paths,
    client: &ClientPaths,
    key: Option<&TargetKey>,
    report: &PruneReport,
    ring: Ring<'_>,
) {
    if report.pruned.is_empty() {
        return;
    }
    let rang = key.is_some_and(|key| inventory::rebuild(paths, client, key, ring).changed);
    if !rang {
        doorbell::ring(paths, ring.command, ring.actor);
    }
}

/// The retention pass on `client`'s backups (M3-D32) with `opts`, `keep_days` local days
/// of automatic backups kept: after `backup create`, an automatic backup, a restore's
/// commit (its source pinned), and `backup prune`.
pub fn retain(
    paths: &Paths,
    scopes: &ScopeTable,
    client: &ClientPaths,
    key: Option<&TargetKey>,
    keep_days: usize,
    opts: &PruneOpts,
    ring: Ring<'_>,
) -> Result<PruneReport> {
    let (backups, _) = list(client);
    let victims = select(
        &backups,
        crate::clock::now(),
        &TimeZone::system(),
        keep_days,
        opts,
    );
    delete(paths, scopes, client, key, &backups, &victims, ring)
}

/// What a reclamation pass needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceNeed {
    /// At least this many bytes free beside the backups: the free-space guard.
    Free(u64),
    /// The apparent bytes of `auto` backups that are not wholly reflinked at most this:
    /// `daily-copy`'s byte-copy budget. Only such backups are reclaimed for it.
    CopyBytes(u64),
}

/// What a reclamation pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reclaimed {
    pub report: PruneReport,
    /// The need holds after the pass.
    pub met: bool,
}

/// The reclamation pass (M3-D48): eligible automatic backups (those retention would prune
/// with no day kept, so never one younger than a day, the newest qualifying capture, a
/// user's or a pinned one), oldest capture first, checking `need` before each deletion.
pub fn reclaim(
    paths: &Paths,
    scopes: &ScopeTable,
    client: &ClientPaths,
    key: Option<&TargetKey>,
    need: SpaceNeed,
    pins: &BTreeSet<BackupId>,
    ring: Ring<'_>,
) -> Result<Reclaimed> {
    let (backups, _) = list(client);
    let opts = PruneOpts {
        pins: pins.clone(),
        ..PruneOpts::default()
    };
    let eligible = select(&backups, crate::clock::now(), &TimeZone::system(), 0, &opts);
    let mut order: Vec<&BackupRef> = backups
        .iter()
        .filter(|b| eligible.contains(&b.id))
        .filter(|b| matches!(need, SpaceNeed::Free(_)) || b.meta.as_ref().is_some_and(copied))
        .collect();
    order.sort_by_key(|b| (b.captured_at(), b.id));
    let mut copy_bytes: u64 = backups
        .iter()
        .filter_map(|b| b.meta.as_ref())
        .filter(|m| copied(m))
        .map(|m| m.bytes)
        .sum();
    let met = |copy_bytes: u64| match need {
        SpaceNeed::Free(bytes) => free_bytes(&client.backups()) >= bytes,
        SpaceNeed::CopyBytes(bytes) => copy_bytes <= bytes,
    };
    let mut report = PruneReport::new(&backups);
    let mut done = Ok(());
    for b in order {
        if met(copy_bytes) {
            break;
        }
        match delete_one(scopes, b) {
            Ok(deleted) => {
                if !matches!(deleted, Deleted::Busy) && b.meta.as_ref().is_some_and(copied) {
                    copy_bytes = copy_bytes.saturating_sub(b.meta.as_ref().map_or(0, |m| m.bytes));
                }
                report.note(b, deleted);
            }
            Err(error) => {
                done = Err(error);
                break;
            }
        }
    }
    report.reclaimed_for_space = !report.pruned.is_empty();
    recorded(paths, client, key, &report, ring);
    done.map(|()| Reclaimed {
        met: met(copy_bytes),
        report,
    })
}

/// Whether `meta` counts against the byte-copy budget: an `auto` backup that is not
/// wholly reflinked.
fn copied(meta: &BackupMeta) -> bool {
    matches!(meta.kind, BackupKind::Auto) && !matches!(meta.reflinked, Reflinked::All)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{Consistency, Integrity};
    use crate::model::Actor;
    use crate::rules::Family;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A backup of `kind` captured (and published) at `captured`.
    fn backup(kind: BackupKind, trigger: Option<Trigger>, captured: &str) -> BackupRef {
        let id = BackupId::new();
        let meta = BackupMeta {
            id,
            target: TargetKey::new(),
            target_id: "retail".into(),
            family: Family::Mainline,
            created_at: at(captured),
            captured_at: at(captured),
            kind,
            trigger,
            actor: Actor::Agent,
            reason: None,
            client_version: "12.1.5.70391".into(),
            client_running: false,
            files: 1,
            dirs: 1,
            symlinks: 0,
            skipped: 0,
            bytes: 100,
            reflinked: Reflinked::All,
            unsettled: 0,
            consistency: Consistency::Stable,
            integrity: Integrity::MetadataOnly,
            verified_at: None,
            baseline: None,
            tool_version: "0.1.0".into(),
            source: None,
            partial: false,
            wtf_outside_client: false,
        };
        BackupRef {
            id,
            qualifying: super::super::qualifying(&meta),
            meta: Some(meta),
            dir: std::path::PathBuf::from(format!("/backups/{id}")),
            restorable: true,
            busy: false,
        }
    }

    fn pre(captured: &str) -> BackupRef {
        backup(BackupKind::Auto, Some(Trigger::PreChange), captured)
    }

    fn post(captured: &str) -> BackupRef {
        backup(BackupKind::Auto, Some(Trigger::PostSession), captured)
    }

    /// A pre-change backup made by byte copy.
    fn copy(captured: &str) -> BackupRef {
        let mut b = pre(captured);
        if let Some(meta) = b.meta.as_mut() {
            meta.reflinked = Reflinked::None;
        }
        b
    }

    /// The acceptance scene: ten days of automatic backups (two pre-change, three
    /// post-session a day), and two manual ones.
    fn ten_days() -> Vec<BackupRef> {
        let mut backups = Vec::new();
        for day in 1..=10 {
            let d = format!("2026-10-{day:02}");
            backups.push(pre(&format!("{d}T08:00:00Z")));
            backups.push(pre(&format!("{d}T12:00:00Z")));
            backups.push(post(&format!("{d}T10:00:00Z")));
            backups.push(post(&format!("{d}T14:00:00Z")));
            backups.push(post(&format!("{d}T20:00:00Z")));
        }
        backups.push(backup(BackupKind::Manual, None, "2026-09-01T10:00:00Z"));
        backups.push(backup(BackupKind::Manual, None, "2026-10-05T10:00:00Z"));
        backups
    }

    fn kept(backups: &[BackupRef], victims: &[BackupId]) -> Vec<String> {
        let mut kept: Vec<String> = backups
            .iter()
            .filter(|b| !victims.contains(&b.id))
            .map(|b| {
                let m = b.meta.as_ref().unwrap();
                format!("{} {:?}", m.captured_at, m.trigger)
            })
            .collect();
        kept.sort();
        kept
    }

    #[test]
    fn automatic_backups_are_thinned_by_day_and_the_users_are_kept() {
        let backups = ten_days();
        let now = at("2026-10-10T21:00:00Z");
        let victims = select(&backups, now, &TimeZone::UTC, 7, &PruneOpts::default());
        let mut expected: Vec<String> = Vec::new();
        // Within 24 h: every one of day 10 after 21:00 the day before.
        for (time, trigger) in [
            ("08:00", "Some(PreChange)"),
            ("12:00", "Some(PreChange)"),
            ("10:00", "Some(PostSession)"),
            ("14:00", "Some(PostSession)"),
            ("20:00", "Some(PostSession)"),
        ] {
            expected.push(format!("2026-10-10T{time}:00Z {trigger}"));
        }
        // Days 4 to 9: the first pre-change and the last post-session.
        for day in 4..=9 {
            expected.push(format!("2026-10-{day:02}T08:00:00Z Some(PreChange)"));
            expected.push(format!("2026-10-{day:02}T20:00:00Z Some(PostSession)"));
        }
        expected.push("2026-09-01T10:00:00Z None".into());
        expected.push("2026-10-05T10:00:00Z None".into());
        expected.sort();
        assert_eq!(kept(&backups, &victims), expected);
    }

    #[test]
    fn the_newest_qualifying_capture_and_a_pin_survive_every_pass() {
        let pinned = pre("2026-09-01T08:00:00Z");
        let newest = pre("2026-09-03T08:00:00Z");
        let mut later = post("2026-09-04T08:00:00Z");
        // Best effort: not a checkpoint, so `newest` is still the newest qualifying one.
        if let Some(meta) = later.meta.as_mut() {
            meta.consistency = Consistency::BestEffort;
        }
        later.qualifying = false;
        let backups = vec![pinned.clone(), newest, later.clone()];
        let now = at("2026-10-10T00:00:00Z");
        let opts = PruneOpts {
            pins: BTreeSet::from([pinned.id]),
            keep: Some(0),
            captured_before: Some(now),
            ids: None,
        };
        let victims = select(&backups, now, &TimeZone::UTC, 0, &opts);
        assert_eq!(
            victims,
            [later.id],
            "the newest qualifying capture and the pin stay"
        );
        let named = PruneOpts {
            ids: Some(backups.iter().map(|b| b.id).collect()),
            ..opts
        };
        let victims = select(&backups, now, &TimeZone::UTC, 0, &named);
        assert_eq!(victims, [later.id], "naming them does not lift either");
    }

    #[test]
    fn named_backups_go_whatever_their_age_and_only_they() {
        let now = at("2026-10-10T12:00:00Z");
        let fresh = post("2026-10-10T11:00:00Z");
        let older = pre("2026-10-01T08:00:00Z");
        let mut unread = pre("2026-10-02T08:00:00Z");
        unread.meta = None;
        let newest = backup(BackupKind::Manual, None, "2026-10-10T11:30:00Z");
        let backups = vec![fresh.clone(), older.clone(), unread.clone(), newest];
        let retention = select(&backups, now, &TimeZone::UTC, 7, &PruneOpts::default());
        assert!(
            retention.is_empty(),
            "a day of automatic backups, one younger than a day, one unread: {retention:?}"
        );
        let named = PruneOpts {
            ids: Some(BTreeSet::from([fresh.id, unread.id])),
            ..PruneOpts::default()
        };
        let mut victims = select(&backups, now, &TimeZone::UTC, 7, &named);
        victims.sort();
        let mut expected = vec![fresh.id, unread.id];
        expected.sort();
        assert_eq!(
            victims, expected,
            "only the named, the young and the unread too"
        );
    }

    #[test]
    fn keep_adds_to_retention_and_counts_each_kind_alone() {
        let now = at("2026-10-10T12:00:00Z");
        // Two automatic backups on one old day: the later pre-change one is an extra.
        let first = pre("2026-10-01T08:00:00Z");
        let extra = pre("2026-10-01T09:00:00Z");
        let restore = backup(BackupKind::BeforeRestore, None, "2026-10-02T08:00:00Z");
        let manual = backup(BackupKind::Manual, None, "2026-10-03T08:00:00Z");
        let backups = vec![first.clone(), extra.clone(), restore.clone(), manual];
        let keep = |n| PruneOpts {
            keep: Some(n),
            ..PruneOpts::default()
        };
        let victims = |opts: &PruneOpts| {
            let mut victims = select(&backups, now, &TimeZone::UTC, 7, opts);
            victims.sort();
            victims
        };
        assert_eq!(
            victims(&keep(5)),
            [extra.id],
            "within the newest five, the day's extra still goes"
        );
        let mut expected = vec![first.id, extra.id, restore.id];
        expected.sort();
        assert_eq!(
            victims(&keep(0)),
            expected,
            "every kind, the manual one the newest checkpoint"
        );
    }

    #[test]
    fn keep_and_older_than_reach_the_users_backups_but_never_the_last_day() {
        let manual = |at: &str| backup(BackupKind::Manual, None, at);
        let backups = vec![
            manual("2026-10-01T10:00:00Z"),
            manual("2026-10-05T10:00:00Z"),
            manual("2026-10-09T10:00:00Z"),
            backup(BackupKind::Imported, None, "2026-08-01T10:00:00Z"),
            backup(BackupKind::Recovered, None, "2026-10-10T10:00:00Z"),
        ];
        let now = at("2026-10-10T12:00:00Z");
        let ids = |victims: Vec<BackupId>| -> Vec<String> {
            backups
                .iter()
                .filter(|b| victims.contains(&b.id))
                .map(|b| b.captured_at().unwrap().to_string())
                .collect()
        };
        let keep = PruneOpts {
            keep: Some(1),
            ..PruneOpts::default()
        };
        assert_eq!(
            ids(select(&backups, now, &TimeZone::UTC, 7, &keep)),
            ["2026-10-01T10:00:00Z", "2026-10-05T10:00:00Z"],
            "the newest manual (also the newest checkpoint) and the only import stay"
        );
        let older = PruneOpts {
            captured_before: Some(now),
            ..PruneOpts::default()
        };
        assert_eq!(
            ids(select(&backups, now, &TimeZone::UTC, 7, &older)),
            [
                "2026-10-01T10:00:00Z",
                "2026-10-05T10:00:00Z",
                "2026-08-01T10:00:00Z"
            ],
            "the recovered one is younger than a day"
        );
        assert!(
            select(&backups, now, &TimeZone::UTC, 7, &PruneOpts::default()).is_empty(),
            "without either, the user's backups are kept"
        );
    }

    /// A client with `backups` published under its `<meta>/backups/`, each with a `WTF/`
    /// holding a `0500` directory, as a byte-copied backup may.
    struct OnDisk {
        _dir: tempfile::TempDir,
        paths: Paths,
        client: ClientPaths,
        scopes: ScopeTable,
    }

    fn on_disk(backups: &[BackupRef]) -> OnDisk {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let client_dir = root.join("_retail_");
        std::fs::create_dir_all(client_dir.join("Interface/AddOns")).unwrap();
        let paths = Paths::with_roots(&root, root.join("c"), root.join("s"), root.join("k"));
        let client = ClientPaths::new(&client_dir);
        for b in backups {
            let dir = crate::backup::backup_dir(&client, &b.id);
            let sealed = dir.join("WTF/Account/Sealed");
            std::fs::create_dir_all(&sealed).unwrap();
            std::fs::write(sealed.join("S.lua"), "S = 1").unwrap();
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o500)).unwrap();
            let meta = b.meta.clone().unwrap();
            let manifest = crate::backup::Manifest {
                entries: Vec::new(),
            };
            let write = |name: &str, json: Vec<u8>| std::fs::write(dir.join(name), json).unwrap();
            write("backup.json", crate::state::store::encode(&meta).unwrap());
            write(
                "manifest.json",
                crate::state::store::encode(&manifest).unwrap(),
            );
        }
        let scopes = ScopeTable::new(&paths, std::slice::from_ref(&client_dir), None).unwrap();
        OnDisk {
            _dir: dir,
            paths,
            client,
            scopes,
        }
    }

    const RING: Ring<'static> = Ring {
        command: "backup prune",
        actor: Actor::Agent,
    };

    impl Drop for OnDisk {
        /// The `0500` directories left are given back, so the tempdir goes whole.
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let mut pending = vec![self.client.client_dir().to_path_buf()];
            while let Some(dir) = pending.pop() {
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        pending.push(entry.path());
                    }
                }
            }
        }
    }

    impl OnDisk {
        fn ids(&self) -> Vec<BackupId> {
            let mut ids: Vec<BackupId> = list(&self.client).0.iter().map(|b| b.id).collect();
            ids.sort();
            ids
        }
    }

    #[test]
    fn a_backup_a_reader_holds_is_skipped_busy_and_pruned_once_free() {
        let (a, b) = (pre("2025-01-01T08:00:00Z"), pre("2025-01-02T08:00:00Z"));
        let gone = pre("2025-01-03T08:00:00Z");
        let disk = on_disk(&[a.clone(), b.clone(), gone.clone()]);
        let (backups, _) = list(&disk.client);
        let reader = DirLock::shared(&crate::backup::backup_dir(&disk.client, &a.id)).unwrap();
        // Another pass deleted this one since the listing.
        let gone_dir = crate::backup::backup_dir(&disk.client, &gone.id);
        let writable = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::Permissions::from_mode(0o700)
        };
        std::fs::set_permissions(gone_dir.join("WTF/Account/Sealed"), writable).unwrap();
        std::fs::remove_dir_all(&gone_dir).unwrap();
        let victims = [a.id, b.id, gone.id];
        let report = delete(
            &disk.paths,
            &disk.scopes,
            &disk.client,
            None,
            &backups,
            &victims,
            RING,
        )
        .unwrap();
        assert_eq!(report.busy, [a.id]);
        assert_eq!(
            report.pruned.iter().map(|p| p.id).collect::<Vec<_>>(),
            [b.id]
        );
        assert_eq!(report.pruned[0].kind, Some(BackupKind::Auto));
        assert_eq!(report.kept, 1, "the busy one; the gone one is neither");
        assert_eq!(disk.ids(), [a.id], "a 0500 directory is no obstacle");
        assert!(
            disk.paths.doorbell_file().is_file(),
            "an unregistered target's deletion rings too"
        );
        drop(reader);
        let (backups, _) = list(&disk.client);
        let report = delete(
            &disk.paths,
            &disk.scopes,
            &disk.client,
            None,
            &backups,
            &[a.id],
            RING,
        )
        .unwrap();
        assert!(report.busy.is_empty());
        assert!(disk.ids().is_empty());
    }

    /// `backup list` probes each backup by taking its lock for an instant: a deletion
    /// that meets one steps around it instead of reporting the backup busy.
    #[test]
    fn an_instant_probe_is_no_reader() {
        let victim = pre("2025-01-01T08:00:00Z");
        let disk = on_disk(std::slice::from_ref(&victim));
        let (backups, _) = list(&disk.client);
        let dir = crate::backup::backup_dir(&disk.client, &victim.id);
        let (taken, held) = std::sync::mpsc::channel();
        let probe = std::thread::spawn(move || {
            let lock = DirLock::try_exclusive(&dir).unwrap();
            taken.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drop(lock);
        });
        held.recv().unwrap();
        let report = delete(
            &disk.paths,
            &disk.scopes,
            &disk.client,
            None,
            &backups,
            &[victim.id],
            RING,
        )
        .unwrap();
        probe.join().unwrap();
        assert!(report.busy.is_empty(), "{report:?}");
        assert_eq!(report.pruned.len(), 1);
    }

    #[test]
    fn reclaim_takes_the_oldest_eligible_first_and_stops_when_met() {
        let copies: Vec<BackupRef> = (1..=4)
            .map(|day| copy(&format!("2025-01-0{day}T08:00:00Z")))
            .collect();
        let reflinked = pre("2024-12-15T08:00:00Z");
        let manual = backup(BackupKind::Manual, None, "2024-12-01T08:00:00Z");
        let mut all = copies.clone();
        all.extend([reflinked.clone(), manual.clone()]);
        let disk = on_disk(&all);
        let reclaim = |need, pins: &[BackupId]| {
            reclaim(
                &disk.paths,
                &disk.scopes,
                &disk.client,
                None,
                need,
                &pins.iter().copied().collect(),
                RING,
            )
            .unwrap()
        };
        let ids = |r: &Reclaimed| r.report.pruned.iter().map(|p| p.id).collect::<Vec<_>>();
        // 400 bytes of copies; the newest is the newest checkpoint, kept. The budget never
        // takes the reflinked one, older as it is.
        let pass = reclaim(SpaceNeed::CopyBytes(250), &[]);
        assert_eq!(ids(&pass), [copies[0].id, copies[1].id], "oldest first");
        assert!(pass.met && pass.report.reclaimed_for_space);
        assert_eq!(pass.report.kept, 4);
        let pass = reclaim(SpaceNeed::CopyBytes(0), &[copies[2].id]);
        assert!(
            ids(&pass).is_empty() && !pass.met,
            "only protected copies left"
        );
        assert!(!pass.report.reclaimed_for_space);
        let pass = reclaim(SpaceNeed::Free(0), &[]);
        assert!(
            ids(&pass).is_empty() && pass.met,
            "met before the first deletion"
        );
        let pass = reclaim(SpaceNeed::Free(u64::MAX), &[]);
        assert_eq!(ids(&pass), [reflinked.id, copies[2].id]);
        assert!(!pass.met);
        let mut left = vec![copies[3].id, manual.id];
        left.sort();
        assert_eq!(
            disk.ids(),
            left,
            "the newest checkpoint and the user's backup"
        );
    }

    /// Days are local: a day that lasts 25 hours (the end of summer time) is one day.
    #[test]
    fn local_days_survive_a_daylight_saving_change() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        // 2026-10-25 is 25 hours long in Berlin. 00:30 and 23:30 local are both that day.
        let early = pre("2026-10-24T22:30:00Z");
        let late = pre("2026-10-25T22:30:00Z");
        // A newer checkpoint, so neither is kept as the newest qualifying capture.
        let newer = backup(BackupKind::Manual, None, "2026-11-01T10:00:00Z");
        let backups = vec![early.clone(), late.clone(), newer];
        let now = at("2026-11-10T00:00:00Z");
        let victims = select(&backups, now, &berlin, 7, &PruneOpts::default());
        assert_eq!(victims, [late.id], "one day: its first pre-change is kept");
        let utc = select(&backups, now, &TimeZone::UTC, 7, &PruneOpts::default());
        assert!(utc.is_empty(), "two UTC days");
    }
}
