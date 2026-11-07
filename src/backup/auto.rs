//! Automatic backups (M3-D30, M3-D31, M3-D48): a pre-change checkpoint before the day's
//! first change that moves folders or writes SavedVariables, and a post-session one after
//! each play session that changed settings (`backup create --auto`).
//!
//! Both skip when the live `WTF/` is what the newest qualifying capture holds, run one at
//! a time per target under the scheduling lock (a `try_exclusive` on `<meta>/backups/`,
//! never waited on), reclaim eligible automatic backups before the disk runs low or skip
//! saying so, and record what they did in the inventory projection. `daily` clones only:
//! where the filesystem cannot clone files, none is made and nothing is written.
//! Neither trigger ever fails a change: a failure is a warning.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::create::{self, CreateError, CreateRequest, Created, DIR_MODE};
use super::inventory::{self, AutoLast, Capability, Change, Ring};
use super::prune::{self, SpaceNeed};
use super::tree::walk;
use super::{BackupKind, BackupRef, EntryKind, Manifest, ManifestEntry, Reflinked, Trigger};
use crate::discover::DiscoveredTarget;
use crate::error::{Error, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{
    ClonePolicy, DirLock, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, failpoint, mutate,
};
use crate::limits::{BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES, EffectiveLimits};
use crate::model::{Actor, Target};
use crate::output::ProgressSink;
use crate::state::config::AutoBackup;
use crate::state::paths::{ClientPaths, Paths};
use crate::state::store;

/// Why an automatic backup made none: a success, with its reason. It serializes as its
/// [`Skip::wire`] name (`backup create`'s `skipped.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// `auto_backup = "off"`.
    Off,
    /// The client has no `WTF/` yet.
    NoWtf,
    /// After play, while the client runs or may: it writes `WTF/` at logout.
    Running,
    /// Before a change: today's checkpoint was made.
    DoneToday,
    /// `WTF/` is what the newest qualifying capture holds.
    Unchanged,
    /// `daily` where the filesystem cannot clone files.
    NoReflink,
    /// Too little space, even after reclaiming eligible automatic backups.
    SpaceLow,
    /// Another automatic backup of this client runs.
    InProgress,
}

impl Skip {
    /// Every skip, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Off,
        Self::NoWtf,
        Self::Running,
        Self::DoneToday,
        Self::Unchanged,
        Self::NoReflink,
        Self::SpaceLow,
        Self::InProgress,
    ];

    /// The wire name (`skipped.reason`, `auto.last.result`).
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::NoWtf => "no_wtf",
            Self::Running => "running",
            Self::DoneToday => "done_today",
            Self::Unchanged => "unchanged",
            Self::NoReflink => "no_reflink",
            Self::SpaceLow => "space_low",
            Self::InProgress => "in_progress",
        }
    }
}

/// `auto.last.result` of an attempt that made a backup.
pub const LAST_CREATED: &str = "created";
/// `auto.last.result` of an attempt that failed.
pub const LAST_FAILED: &str = "failed";

/// The schema of `auto.last.result`: [`LAST_CREATED`], a [`Skip`]'s wire name, or
/// [`LAST_FAILED`], the only values [`note`] records. The inventory keeps it a string, so
/// a value a newer build records still reads.
pub fn last_result_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let values: Vec<&str> = std::iter::once(LAST_CREATED)
        .chain(Skip::ALL.iter().map(|skip| skip.wire()))
        .chain(std::iter::once(LAST_FAILED))
        .collect();
    schemars::json_schema!({"type": "string", "enum": values})
}

/// What [`decide`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Skip(Skip),
    /// Back up, cloning only (`daily`) or copying where cloning fails (`daily-copy`).
    Go(ClonePolicy),
}

/// What [`decide`] reads about a target, all but the stat walk up front.
pub struct DueFacts<'a> {
    pub mode: AutoBackup,
    pub trigger: Trigger,
    /// The client has a `WTF/`.
    pub wtf: bool,
    /// The client runs, or whether it does cannot be told.
    pub running: bool,
    pub backups: &'a [BackupRef],
    pub now: Timestamp,
    pub tz: &'a TimeZone,
    /// The filesystem holding `<meta>` clones files (the `statfs` hint).
    pub clones: bool,
    /// What the last attempt observed.
    pub capability: Capability,
}

/// Whether an automatic backup is due (M3-D31), checking in order: the mode, a `WTF/`,
/// the client running (after play only), today's checkpoint (before a change only), an
/// unchanged tree (`unchanged`, asked only when the earlier checks pass: it is a stat
/// walk), and for `daily` a filesystem that clones. Reads only.
pub fn decide(f: &DueFacts<'_>, unchanged: &dyn Fn() -> bool) -> Decision {
    if matches!(f.mode, AutoBackup::Off) {
        return Decision::Skip(Skip::Off);
    }
    if !f.wtf {
        return Decision::Skip(Skip::NoWtf);
    }
    let after_play = matches!(f.trigger, Trigger::PostSession);
    if after_play && f.running {
        return Decision::Skip(Skip::Running);
    }
    if !after_play && done_today(f.backups, f.now, f.tz) {
        return Decision::Skip(Skip::DoneToday);
    }
    if unchanged() {
        return Decision::Skip(Skip::Unchanged);
    }
    match f.mode {
        AutoBackup::Daily if !f.clones || matches!(f.capability, Capability::Unsupported) => {
            Decision::Skip(Skip::NoReflink)
        }
        AutoBackup::Daily => Decision::Go(ClonePolicy::Required),
        AutoBackup::DailyCopy | AutoBackup::Off => Decision::Go(ClonePolicy::Preferred),
    }
}

/// Whether a qualifying pre-change backup, or a restore's `before_restore` one, was
/// captured since local midnight: today's checkpoint before a change.
fn done_today(backups: &[BackupRef], now: Timestamp, tz: &TimeZone) -> bool {
    let Ok(midnight) = now.to_zoned(tz.clone()).start_of_day() else {
        return false;
    };
    let midnight = midnight.timestamp();
    backups.iter().filter(|b| b.qualifying).any(|b| {
        b.meta.as_ref().is_some_and(|m| {
            let checkpoint = match m.kind {
                BackupKind::Auto => matches!(m.trigger, Some(Trigger::PreChange)),
                BackupKind::BeforeRestore => true,
                BackupKind::Manual | BackupKind::Imported | BackupKind::Recovered => false,
            };
            checkpoint && m.captured_at >= midnight
        })
    })
}

/// Whether `client`'s live `WTF/` is, entry by entry (path, kind, size and time), what the
/// newest qualifying capture's manifest recorded. Never true without one, or when either
/// does not read.
pub fn unchanged(client: &ClientPaths, backups: &[BackupRef]) -> bool {
    let Some(newest) = backups
        .iter()
        .filter(|b| b.qualifying)
        .max_by_key(|b| (b.captured_at(), b.id))
    else {
        return false;
    };
    let Ok(Some(manifest)) = store::load::<Manifest>(&newest.dir.join("manifest.json")) else {
        return false;
    };
    let Ok(live) = walk(
        &client.wtf(),
        BACKUP_TREE_MAX_ENTRIES,
        BACKUP_TREE_MAX_DEPTH,
    ) else {
        return false;
    };
    shape(&manifest.value.entries) == shape(&live.entries)
}

/// Entries by path: kind, size and modification time.
fn shape(entries: &[ManifestEntry]) -> BTreeMap<&str, (EntryKind, u64, i64)> {
    entries
        .iter()
        .map(|e| (e.path.as_str(), (e.kind, e.size, e.mtime_ns)))
        .collect()
}

/// Automatic backups as configured (M3-D30), for the engine's pre-change trigger.
#[derive(Debug, Clone, Copy)]
pub struct AutoPolicy<'a> {
    pub mode: AutoBackup,
    pub limits: &'a EffectiveLimits,
}

/// An automatic attempt on one target.
pub struct AutoRun<'a> {
    pub paths: &'a Paths,
    pub scopes: &'a ScopeTable,
    pub target: &'a DiscoveredTarget,
    pub trigger: Trigger,
    pub mode: AutoBackup,
    pub limits: &'a EffectiveLimits,
    /// The client runs, or whether it does cannot be told.
    pub running: bool,
    pub actor: Actor,
    pub reason: Option<String>,
    /// What the projection's writes ring the doorbell for.
    pub command: &'a str,
    pub progress: &'a ProgressSink,
    /// Whether a skip decided before anything else is recorded (`backup create --auto`).
    /// The pre-change hook writes nothing for one, so a change that makes no backup
    /// writes nothing more than it did.
    pub record_skips: bool,
}

/// What an automatic attempt did.
#[derive(Debug)]
pub enum AutoResult {
    Created(Box<Created>),
    Skipped(Skip),
    /// The attempt failed: before a change, only [`failed`]'s warning; asked for, the
    /// command's item error.
    Failed(Error),
}

/// An automatic attempt's result, its warnings, and the target as registered when it got
/// that far.
#[derive(Debug)]
pub struct AutoOutcome {
    pub result: AutoResult,
    pub warnings: Vec<Warning>,
    pub target: Option<Target>,
}

/// Makes the automatic backup `req` describes, when due. `register` registers the
/// client (under the global lock) when it has no key, which only a backup that is due
/// asks for. A failure is recorded and returned, never raised.
pub fn run(req: &AutoRun<'_>, register: &dyn Fn() -> Result<Target>) -> AutoOutcome {
    let mut outcome = AutoOutcome {
        result: AutoResult::Skipped(Skip::Off),
        warnings: Vec::new(),
        target: None,
    };
    outcome.result = match attempt(req, register, &mut outcome) {
        Ok(result) => result,
        Err(error) => {
            // Recorded however early it failed (before registering, too): `status`
            // must never keep saying a failing schedule works. A client with no key
            // records nothing.
            let target = outcome.target.as_ref().unwrap_or(&req.target.target);
            note(req, target, LAST_FAILED, None);
            AutoResult::Failed(error)
        }
    };
    outcome
}

/// [`run`]'s steps; an error is the attempt's failure.
fn attempt(
    req: &AutoRun<'_>,
    register: &dyn Fn() -> Result<Target>,
    outcome: &mut AutoOutcome,
) -> Result<AutoResult> {
    let discovered = &req.target.target;
    let client = ClientPaths::new(&discovered.client_dir);
    let backups_dir = client.backups();
    // Where there is no `<meta>/backups/` yet, a skip creates none: decide first.
    let mut lock = None;
    if backups_dir.is_dir() {
        match reserve(&backups_dir)? {
            Some(held) => lock = Some(held),
            None => return Ok(AutoResult::Skipped(Skip::InProgress)),
        }
    }
    let policy = match due(req, &client) {
        Decision::Go(policy) => policy,
        Decision::Skip(skip) => return Ok(skipped(req, discovered, skip)),
    };
    if lock.is_none() {
        let mut s = WriteSession::untracked(Writer::Backup, req.scopes, "auto-backup");
        ensure_dir_all(&mut s, &backups_dir)?;
        mutate(&mut s, &backups_dir, Op::Chmod { mode: DIR_MODE })?;
        match reserve(&backups_dir)? {
            Some(held) => lock = Some(held),
            None => return Ok(AutoResult::Skipped(Skip::InProgress)),
        }
        // Under the lock, as M3-D31 requires.
        if let Decision::Skip(skip) = due(req, &client) {
            return Ok(skipped(req, discovered, skip));
        }
    }
    let target = match discovered.key {
        Some(_) => discovered.clone(),
        None => register()?,
    };
    outcome.target = Some(target.clone());
    if let Some(skip) = make_room(req, &client, &target, policy, outcome)? {
        note(req, &target, skip.wire(), None);
        return Ok(AutoResult::Skipped(skip));
    }
    failpoint::hit("engine.autobackup").map_err(|e| {
        Error::io(
            &backups_dir,
            &e,
            Suggestion::command(["omaddons", "status"]),
        )
    })?;
    let created = create::create(
        req.paths,
        req.scopes,
        &CreateRequest {
            target: &target,
            kind: BackupKind::Auto,
            trigger: Some(req.trigger),
            actor: req.actor,
            reason: req.reason.clone(),
            policy,
            verify: false,
            client_running: req.running,
            command: req.command,
        },
        req.progress,
    );
    drop(lock);
    match created {
        Ok(created) => {
            let capability = match created.meta.reflinked {
                Reflinked::All | Reflinked::Some => Capability::Supported,
                Reflinked::None => Capability::Unsupported,
            };
            note(req, &target, LAST_CREATED, Some(capability));
            outcome.warnings.extend(created.warnings.iter().cloned());
            Ok(AutoResult::Created(Box::new(created)))
        }
        Err(CreateError::NotReflinkable) => {
            note(
                req,
                &target,
                Skip::NoReflink.wire(),
                Some(Capability::Unsupported),
            );
            Ok(AutoResult::Skipped(Skip::NoReflink))
        }
        Err(CreateError::Failed(error)) => Err(error),
    }
}

/// The scheduling lock on `<meta>/backups/`, or `None` while another attempt holds it.
fn reserve(dir: &std::path::Path) -> Result<Option<DirLock>> {
    DirLock::try_exclusive(dir)
        .map_err(|e| Error::io(dir, &e, Suggestion::command(["omaddons", "status"])))
}

/// [`decide`] for `req`'s target as it is now. Reads only: a dry run's answer.
pub fn due(req: &AutoRun<'_>, client: &ClientPaths) -> Decision {
    let (backups, _) = super::list(client);
    let tz = TimeZone::system();
    let now = crate::clock::now();
    // Capability is observed, not assumed: an `unsupported` recorded on an earlier local
    // day is tried again, as what stopped a clone (a `WTF/` linked onto another
    // filesystem, a byte copy made under `daily-copy`) may be gone. Within its day it
    // stands, so a clone that keeps failing is tried once a day.
    let midnight = now
        .to_zoned(tz.clone())
        .start_of_day()
        .map(|z| z.timestamp())
        .ok();
    let capability = req.target.target.key.map_or(Capability::Unknown, |key| {
        let auto = inventory::auto_state(req.paths, &key);
        let stale = auto
            .last
            .as_ref()
            .is_none_or(|last| midnight.is_none_or(|m| last.at < m));
        match auto.capability {
            Capability::Unsupported if stale => Capability::Unknown,
            observed => observed,
        }
    });
    let facts = DueFacts {
        mode: req.mode,
        trigger: req.trigger,
        wtf: client.wtf().is_dir(),
        running: req.running,
        backups: &backups,
        now,
        tz: &tz,
        clones: matches!(
            super::fs::clone_hint(&client.meta_dir()),
            super::fs::CloneHint::Capable
        ),
        capability,
    };
    decide(&facts, &|| unchanged(client, &backups))
}

/// A skip decided before anything else: recorded only where `req` records them, never
/// for `no_reflink` (its whole point is to write nothing) or for a target without a key.
fn skipped(req: &AutoRun<'_>, target: &Target, skip: Skip) -> AutoResult {
    if req.record_skips && !matches!(skip, Skip::NoReflink | Skip::InProgress) {
        note(req, target, skip.wire(), None);
    }
    AutoResult::Skipped(skip)
}

/// Records `result` as the last automatic attempt on `target` (best effort), and the
/// capability it observed.
fn note(req: &AutoRun<'_>, target: &Target, result: &str, capability: Option<Capability>) {
    let Some(key) = target.key else {
        return;
    };
    let client = ClientPaths::new(&target.client_dir);
    let mut auto = inventory::auto_state(req.paths, &key);
    auto.capability = capability.unwrap_or(auto.capability);
    auto.last = Some(AutoLast {
        at: crate::clock::now(),
        result: result.to_owned(),
    });
    let ring = Ring {
        command: req.command,
        actor: req.actor,
    };
    inventory::record(req.paths, &client, &key, Change::Auto(auto), ring);
}

/// The space guard (M3-D48), then `daily-copy`'s byte budget: eligible automatic backups
/// reclaimed oldest first while too little is free or too much is byte-copied; a skip
/// when that is not enough. `backup_space_low` says so.
fn make_room(
    req: &AutoRun<'_>,
    client: &ClientPaths,
    target: &Target,
    policy: ClonePolicy,
    outcome: &mut AutoOutcome,
) -> Result<Option<Skip>> {
    let ring = Ring {
        command: req.command,
        actor: req.actor,
    };
    let key = target.key.as_ref();
    let pins = std::collections::BTreeSet::new();
    let min_free = req.limits.backup_min_free;
    if min_free > 0 && super::fs::free_bytes(&client.backups()) < min_free {
        let done = prune::reclaim(
            req.paths,
            req.scopes,
            client,
            key,
            SpaceNeed::Free(min_free),
            &pins,
            ring,
        )?;
        if !done.met {
            outcome.warnings.push(space_low(&target.id, min_free));
            return Ok(Some(Skip::SpaceLow));
        }
    }
    let budget = req.limits.backup_copy_max;
    // The budget counts byte copies: only where one is expected.
    let clones = matches!(
        super::fs::clone_hint(&client.meta_dir()),
        super::fs::CloneHint::Capable
    );
    let unsupported = key.is_some_and(|key| {
        matches!(
            inventory::auto_state(req.paths, key).capability,
            Capability::Unsupported
        )
    });
    let copies = matches!(policy, ClonePolicy::Preferred) && (!clones || unsupported);
    if budget > 0 && copies {
        let tree = walk(
            &client.wtf(),
            BACKUP_TREE_MAX_ENTRIES,
            BACKUP_TREE_MAX_DEPTH,
        )
        .map(|w| {
            w.entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::File))
                .map(|e| e.size)
                .sum::<u64>()
        })
        .unwrap_or(u64::MAX);
        let met = match budget.checked_sub(tree) {
            Some(room) => {
                prune::reclaim(
                    req.paths,
                    req.scopes,
                    client,
                    key,
                    SpaceNeed::CopyBytes(room),
                    &pins,
                    ring,
                )?
                .met
            }
            None => false,
        };
        if !met {
            outcome.warnings.push(budget_low(&target.id, budget));
            return Ok(Some(Skip::SpaceLow));
        }
    }
    Ok(None)
}

/// `backup_space_low`: an automatic backup skipped, too little free beside the backups.
fn space_low(target: &str, min_free: u64) -> Warning {
    Warning::new(
        WarningCode::BackupSpaceLow,
        format!(
            "{target}: no automatic backup: less than limits.backup_min_free_mb ({} MiB) is free beside its backups, even after pruning old automatic ones",
            min_free >> 20
        ),
    )
    .with_fix(Suggestion::text(
        "free some space, or lower limits.backup_min_free_mb in config.toml",
    ))
}

/// `backup_space_low`: an automatic byte copy skipped, over the byte-copy budget.
fn budget_low(target: &str, budget: u64) -> Warning {
    Warning::new(
        WarningCode::BackupSpaceLow,
        format!(
            "{target}: no automatic backup: byte copies of WTF/ would pass limits.backup_copy_max_mb ({} MiB), even after pruning old automatic ones",
            budget >> 20
        ),
    )
    .with_fix(Suggestion::text(
        "raise limits.backup_copy_max_mb in config.toml, or set auto_backup to daily",
    ))
}

/// `auto_backup_failed`: the change goes ahead without its automatic backup.
pub fn failed(target: &str, error: &Error) -> Warning {
    Warning::new(
        WarningCode::AutoBackupFailed,
        format!("{target}: the automatic backup failed: {}", error.message),
    )
    .with_fix(Suggestion::command([
        "omaddons", "backup", "create", "-t", target,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::{BackupMeta, Consistency, Integrity};
    use crate::model::{BackupId, TargetKey};
    use crate::rules::Family;

    /// `auto.last.result` publishes a closed set (rule 4): created, failed and every skip,
    /// whose list is complete because it equals the skips `Skip`'s own schema derives.
    #[test]
    fn last_results_are_a_closed_set_of_every_skip() {
        let published = last_result_schema(&mut schemars::SchemaGenerator::default());
        let published = published.as_value();
        let values: Vec<&str> = published["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        let derived = serde_json::to_value(schemars::schema_for!(Skip)).unwrap();
        let mut skips: Vec<String> = Vec::new();
        crate::contract::schema::walk(&derived, &mut |visit| {
            if let crate::contract::schema::Visit::Position { node, .. } = visit {
                skips.extend(
                    node.get("const")
                        .and_then(|c| c.as_str())
                        .map(str::to_owned),
                );
                skips.extend(
                    node.get("enum")
                        .and_then(|e| e.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned)),
                );
            }
        });
        let mut expected: Vec<&str> = vec![LAST_CREATED];
        expected.extend(skips.iter().map(String::as_str));
        expected.push(LAST_FAILED);
        assert_eq!(values, expected, "auto.last.result's published values");
        assert_eq!(
            skips.len(),
            Skip::ALL.len(),
            "Skip::ALL lists every skip: {skips:?}"
        );
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn every_skip_serializes_as_its_wire_name() {
        for skip in [
            Skip::Off,
            Skip::NoWtf,
            Skip::Running,
            Skip::DoneToday,
            Skip::Unchanged,
            Skip::NoReflink,
            Skip::SpaceLow,
            Skip::InProgress,
        ] {
            // An exhaustive match makes a new reason join this list.
            match skip {
                Skip::Off
                | Skip::NoWtf
                | Skip::Running
                | Skip::DoneToday
                | Skip::Unchanged
                | Skip::NoReflink
                | Skip::SpaceLow
                | Skip::InProgress => {}
            }
            assert_eq!(serde_json::to_value(skip).unwrap(), skip.wire(), "{skip:?}");
        }
    }

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
            qualifying: crate::backup::qualifying(&meta),
            meta: Some(meta),
            dir: std::path::PathBuf::from("/nowhere"),
            restorable: true,
            busy: false,
        }
    }

    struct Case {
        mode: AutoBackup,
        trigger: Trigger,
        wtf: bool,
        running: bool,
        backups: Vec<BackupRef>,
        unchanged: bool,
        clones: bool,
        capability: Capability,
    }

    impl Default for Case {
        fn default() -> Self {
            Self {
                mode: AutoBackup::Daily,
                trigger: Trigger::PreChange,
                wtf: true,
                running: false,
                backups: Vec::new(),
                unchanged: false,
                clones: true,
                capability: Capability::Unknown,
            }
        }
    }

    const NOW: &str = "2026-10-21T10:00:00Z";

    fn decided(case: Case) -> Decision {
        let facts = DueFacts {
            mode: case.mode,
            trigger: case.trigger,
            wtf: case.wtf,
            running: case.running,
            backups: &case.backups,
            now: at(NOW),
            tz: &TimeZone::UTC,
            clones: case.clones,
            capability: case.capability,
        };
        decide(&facts, &|| case.unchanged)
    }

    fn skip(skip: Skip) -> Decision {
        Decision::Skip(skip)
    }

    #[test]
    fn each_check_in_its_order() {
        let pre_today = || {
            backup(
                BackupKind::Auto,
                Some(Trigger::PreChange),
                "2026-10-21T08:00:00Z",
            )
        };
        for (case, expected) in [
            (Case::default(), Decision::Go(ClonePolicy::Required)),
            (
                Case {
                    mode: AutoBackup::DailyCopy,
                    clones: false,
                    ..Case::default()
                },
                Decision::Go(ClonePolicy::Preferred),
            ),
            // Off wins over everything.
            (
                Case {
                    mode: AutoBackup::Off,
                    wtf: false,
                    running: true,
                    ..Case::default()
                },
                skip(Skip::Off),
            ),
            (
                Case {
                    wtf: false,
                    unchanged: true,
                    ..Case::default()
                },
                skip(Skip::NoWtf),
            ),
            // Running stops only the after-play trigger.
            (
                Case {
                    trigger: Trigger::PostSession,
                    running: true,
                    ..Case::default()
                },
                skip(Skip::Running),
            ),
            (
                Case {
                    running: true,
                    ..Case::default()
                },
                Decision::Go(ClonePolicy::Required),
            ),
            // Today's checkpoint stops only the before-change trigger.
            (
                Case {
                    backups: vec![pre_today()],
                    unchanged: true,
                    ..Case::default()
                },
                skip(Skip::DoneToday),
            ),
            (
                Case {
                    trigger: Trigger::PostSession,
                    backups: vec![pre_today()],
                    ..Case::default()
                },
                Decision::Go(ClonePolicy::Required),
            ),
            (
                Case {
                    unchanged: true,
                    clones: false,
                    ..Case::default()
                },
                skip(Skip::Unchanged),
            ),
            (
                Case {
                    clones: false,
                    ..Case::default()
                },
                skip(Skip::NoReflink),
            ),
            (
                Case {
                    capability: Capability::Unsupported,
                    ..Case::default()
                },
                skip(Skip::NoReflink),
            ),
        ] {
            assert_eq!(decided(case), expected);
        }
    }

    /// Yesterday's, an after-play one, a manual one and a best-effort one are no
    /// checkpoint before today's first change; a restore's `before_restore` is.
    #[test]
    fn only_a_qualifying_pre_change_or_before_restore_backup_since_midnight_is_today() {
        let mut best_effort = backup(
            BackupKind::Auto,
            Some(Trigger::PreChange),
            "2026-10-21T09:00:00Z",
        );
        if let Some(meta) = best_effort.meta.as_mut() {
            meta.consistency = Consistency::BestEffort;
        }
        best_effort.qualifying = false;
        let not_today = vec![
            backup(
                BackupKind::Auto,
                Some(Trigger::PreChange),
                "2026-10-20T23:59:59Z",
            ),
            backup(
                BackupKind::Auto,
                Some(Trigger::PostSession),
                "2026-10-21T09:00:00Z",
            ),
            backup(BackupKind::Manual, None, "2026-10-21T09:00:00Z"),
            best_effort,
        ];
        let go = Decision::Go(ClonePolicy::Required);
        assert_eq!(
            decided(Case {
                backups: not_today.clone(),
                ..Case::default()
            }),
            go
        );
        let mut today = not_today;
        today.push(backup(
            BackupKind::BeforeRestore,
            None,
            "2026-10-21T00:00:00Z",
        ));
        assert_eq!(
            decided(Case {
                backups: today,
                ..Case::default()
            }),
            skip(Skip::DoneToday)
        );
    }

    /// Local midnight decides: a checkpoint at 23:30 in Berlin is yesterday's at 00:30
    /// there, though both are the same UTC day.
    #[test]
    fn today_is_the_local_day() {
        let berlin = TimeZone::get("Europe/Berlin").unwrap();
        let backups = vec![backup(
            BackupKind::Auto,
            Some(Trigger::PreChange),
            "2026-10-20T21:30:00Z",
        )];
        let now = at("2026-10-20T22:30:00Z");
        assert!(
            done_today(&backups, now, &TimeZone::UTC),
            "the same UTC day"
        );
        assert!(
            !done_today(&backups, now, &berlin),
            "23:30 yesterday in Berlin"
        );
    }
}
