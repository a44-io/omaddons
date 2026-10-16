//! `omaddons status [-t T] [--cached] [--check]`: offline triage, the first call for
//! orientation (vision §5.8). Every target with its client, live running state, folder
//! classes and addon counts; unfinished transactions; what needs attention and the
//! commands that address it. Read-only: never writes, never takes the lock, no network.
//!
//! `--cached` (the widget's authority from M3) never scans: it re-stats the discovery
//! cache's roots, or without a cache parses only the registered roots and recommends a
//! full `omaddons status`. Running state is live either way. `--check` turns any
//! attention into exit 6 (`check_failed`) with the data still there.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::drift::{DriftItem, needs_reinstall, reinstall_hint};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::cache::{CacheStats, DiscoveryMode};
use crate::discover::running::detect_running;
use crate::drift::{AddonDrift, DriftMode, DriftState};
use crate::engine::journal;
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{LockState, probe_global};
use crate::inventory::{Counts, Depth, FolderClass, Inventory, inventory};
use crate::model::{AddonRecord, AddonState, Channel, FolderName, Ref, Target, TargetKey};
use crate::output::{Outcome, Style, table};
use crate::politeness::DEFAULT_CHECK_INTERVAL;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{
    StatusKind, StatusRow, UncheckedReason, UpdateStatus, installed_primary_interface, statuses,
};
use crate::rules::{ClientVersion, Family, InstallKind, ModeName, RulesTable, TargetKind};
use crate::sources::{SourceId, SourceRegistry};
use crate::state::memo::{self, FactIndex, ReleaseMemo};
use crate::state::records::{TargetRecord, load_record};
use crate::state::registry::RegistryEntry;
use crate::state::store::VersionStatus;

/// `status`'s own flags.
#[derive(Debug, Clone, Copy, Default, clap::Args)]
pub struct StatusArgs {
    /// Local state only, no scan (fast; the widget's view)
    #[arg(long)]
    pub cached: bool,
    /// Exit 6 when anything needs attention
    #[arg(long)]
    pub check: bool,
    /// Re-hash every managed file, not only those whose size or time changed
    #[arg(long, conflicts_with = "cached")]
    pub deep: bool,
}

/// Reinstall recommendations at most, across targets.
const REINSTALL_HINTS_MAX: usize = 5;
/// Changed paths a human line names per addon, unless `-v`.
const DRIFT_PATHS_SHOWN: usize = 5;

/// `status`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusData {
    pub cached: bool,
    /// Sorted by id.
    pub targets: Vec<StatusTarget>,
    pub journal: JournalState,
    pub needs_attention: bool,
}

/// One target.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusTarget {
    pub id: String,
    /// `null` until the target is registered.
    pub key: Option<TargetKey>,
    pub registered: bool,
    pub default: bool,
    pub family: Family,
    pub kind: TargetKind,
    pub install_kind: InstallKind,
    pub product: Option<String>,
    /// The game modes the client runs, which share its AddOns folder (Classic Era,
    /// Hardcore and Season of Discovery); `-t` takes their ids.
    pub modes: Vec<ModeName>,
    pub version: ClientVersion,
    pub interface: u32,
    pub label: Option<String>,
    pub running: bool,
    /// Registered, but its client dir is gone.
    pub missing: bool,
    pub folders: Counts,
    pub addons: AddonCounts,
    /// Pending updates, from the release memo (no request).
    pub updates: UpdateCounts,
    /// Managed addons whose files changed on disk, whose folders are gone, or that are
    /// not indexed, by name ignoring case (`--cached`: missing folders only).
    pub drift: Vec<DriftItem>,
    /// Unmanaged folders a catalog entry not installed here names.
    pub adoptable: usize,
    /// Addons installed as dependencies that nothing requires any more (from the
    /// record, `--cached` too). Reported and recommended, never removed by themselves.
    pub orphans: usize,
    /// Their names, sorted ignoring case, for the `remove_orphans` recommendation.
    #[serde(skip)]
    pub orphan_names: Vec<String>,
    pub attention: Vec<Attention>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dir: Option<PathBuf>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_root: Option<PathBuf>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe_name: Option<String>,
}

/// Managed addons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AddonCounts {
    pub total: usize,
    pub degraded: usize,
}

/// What the release memo says about a target's addons (M2-D1): never fetched by
/// `status`, and never attention (M2-D27).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UpdateCounts {
    pub available: usize,
    pub pinned: usize,
    pub blocked: usize,
    pub failed: usize,
    pub unchecked: usize,
    /// The oldest check of its checkable addons; `null` when one was never checked.
    #[schemars(with = "Option<String>")]
    pub checked_at: Option<Timestamp>,
    /// `checked_at` is older than `check_interval_hours` (or in the future).
    pub stale: bool,
}

/// Why a target needs attention (M1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// An unrecovered transaction touches it (the lock is free).
    JournalPending,
    /// Its record is newer than this binary, so writes are refused.
    StateVersionNewer,
    /// Registered, but the client dir is gone.
    TargetMissing,
    /// An addon lost a folder to another (`--force`).
    AddonDegraded,
    /// A managed addon's files changed on disk.
    AddonModified,
    /// A managed addon's folder is gone (or replaced by a symlink).
    AddonFolderMissing,
}

/// Unfinished transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JournalState {
    pub pending: usize,
    /// Another omaddons process holds the lock right now.
    pub in_progress: bool,
}

/// `status`'s outcome.
#[derive(Debug)]
pub struct StatusOutcome {
    data: StatusData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    check: bool,
    /// The first attention item's fix, for `--check`.
    first_fix: Option<(String, Suggestion)>,
    /// One human line per drifted addon, with its paths.
    drift_lines: Vec<String>,
    /// What discovery did, for the `-v` log line.
    pub discovery: CacheStats,
    /// For `-v` log lines (a release memo that could not be read).
    pub notes: Vec<String>,
}

impl Outcome for StatusOutcome {
    type Data = StatusData;

    fn command(&self) -> &'static str {
        "status"
    }

    fn data(&self) -> &StatusData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn assertion_failure(&self) -> Option<Error> {
        if !(self.check && self.data.needs_attention) {
            return None;
        }
        let (message, fix) = self.first_fix.clone().unwrap_or_else(|| {
            (
                "a target needs attention".to_owned(),
                Suggestion::command(["omaddons", "status"]),
            )
        });
        Some(Error::new(ErrorCode::CheckFailed, message, fix))
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(
                w,
                "No WoW client found. Add one with: omaddons targets add <path>"
            );
        }
        let rows: Vec<Vec<String>> = self
            .data
            .targets
            .iter()
            .map(|t| {
                let state = if t.missing {
                    "missing".to_owned()
                } else if t.running {
                    "running".to_owned()
                } else {
                    t.label.clone().unwrap_or_else(|| "-".to_owned())
                };
                let f = &t.folders;
                let mut counts = format!(
                    "{} managed · {} unmanaged · {} protected",
                    f.managed, f.unmanaged, f.protected
                );
                if t.updates.available > 0 {
                    let n = t.updates.available;
                    let s = if n == 1 { "" } else { "s" };
                    counts.push_str(&format!(" · {n} update{s}"));
                }
                if t.adoptable > 0 {
                    counts.push_str(&format!(" · {} adoptable", t.adoptable));
                }
                if t.orphans > 0 {
                    let n = t.orphans;
                    let s = if n == 1 { "" } else { "s" };
                    counts.push_str(&format!(" · {n} orphan{s}"));
                }
                if !t.attention.is_empty() {
                    let names: Vec<String> =
                        t.attention.iter().map(|a| attention_text(*a, t)).collect();
                    counts.push_str(&format!(" · attention: {}", names.join(", ")));
                }
                vec![
                    t.id.clone(),
                    modes_or_family(&t.modes, t.family),
                    t.version.to_string(),
                    wire(t.kind),
                    state,
                    counts,
                ]
            })
            .collect();
        write!(w, "{}", table(&rows))?;
        for line in &self.drift_lines {
            writeln!(w, "{line}")?;
        }
        for c in &self.commands {
            writeln!(w, "Next: {}", c.command())?;
        }
        Ok(())
    }
}

/// An attention item in the target row: drift as a count (`1 addon modified`), the
/// rest by wire name.
fn attention_text(a: Attention, t: &StatusTarget) -> String {
    let count = |state: DriftState| t.drift.iter().filter(|d| d.state == state).count();
    let addons = |n: usize| if n == 1 { "addon" } else { "addons" };
    match a {
        Attention::AddonModified => {
            let n = count(DriftState::Modified);
            format!("{n} {} modified", addons(n))
        }
        Attention::AddonFolderMissing => {
            let n = count(DriftState::Missing);
            format!("{n} {} missing folders", addons(n))
        }
        _ => wire(a),
    }
}

/// `  Plater on retail: 2 files changed (Plater/Plater.lua, …)`: what drifted, with at
/// most `DRIFT_PATHS_SHOWN` paths unless `verbose`.
fn drift_line(target: &str, addon: &AddonDrift, verbose: bool) -> String {
    let name = &addon.addon;
    match addon.state {
        DriftState::Missing => {
            let gone: Vec<&str> = addon
                .folders
                .iter()
                .filter(|f| !f.present)
                .map(|f| f.folder.as_str())
                .collect();
            format!("  {name} on {target}: missing {}", gone.join(", "))
        }
        DriftState::Unindexed => format!("  {name} on {target}: not indexed, so not checked"),
        DriftState::Modified | DriftState::Clean => {
            let n = super::drift::files(addon);
            let mut paths: Vec<String> = addon
                .folders
                .iter()
                .flat_map(|f| {
                    f.modified
                        .iter()
                        .chain(&f.deleted)
                        .chain(&f.added)
                        .map(move |p| format!("{}/{p}", f.folder.as_str()))
                })
                .collect();
            paths.sort();
            let more = if !verbose && paths.len() > DRIFT_PATHS_SHOWN {
                paths.truncate(DRIFT_PATHS_SHOWN);
                ", …"
            } else {
                ""
            };
            let files = if n == 1 { "file" } else { "files" };
            format!(
                "  {name} on {target}: {n} {files} changed ({}{more})",
                paths.join(", ")
            )
        }
    }
}

/// Runs `status`.
pub fn run(ctx: &Context, args: StatusArgs) -> Result<StatusOutcome> {
    let mode = if args.cached {
        DiscoveryMode::NoScan
    } else {
        DiscoveryMode::Full
    };
    let world = World::load_with(mode)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();

    let selected: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    let missing: Vec<&RegistryEntry> = match &ctx.target {
        Some(_) => Vec::new(),
        None => world.resolved.missing.iter().collect(),
    };
    let found: Vec<Target> = selected.iter().map(|d| d.target.clone()).collect();
    let running = detect_running(&found);
    let pending = journal::pending(&world.paths)?;
    let held = matches!(probe_global(&world.paths)?, LockState::Held { .. });
    // Read once: what earlier commands learned about releases.
    let remembered = Remembered::load(&world);
    let notes: Vec<String> = remembered.note.iter().cloned().collect();
    let interval = world
        .config
        .effective_limits()
        .map_or(DEFAULT_CHECK_INTERVAL, |l| l.default_check_interval);
    let now = Timestamp::now();
    let mut catalog: Option<Catalog> = None;
    let mut check_due = false;
    // `--cached` stays a presence check (M2-D15); plain `status` checks stat-first.
    let drift_mode = if args.cached {
        DriftMode::Presence
    } else if args.deep {
        DriftMode::Deep
    } else {
        DriftMode::Stat
    };
    let mut drift_lines = Vec::new();

    let mut targets = Vec::new();
    for d in &selected {
        let t = &d.target;
        let record = match t.key {
            Some(key) => {
                let loaded = load_record(&world.paths, &key)?;
                warnings.extend(loaded.warning(&world.paths.target_record(&key)));
                Some(loaded)
            }
            None => None,
        };
        let inv = inventory(
            t,
            record.as_ref().map(|l| &l.value),
            &world.table,
            Depth::Classes,
        )?;
        let addons = record
            .as_ref()
            .map_or_else(AddonCounts::default, |l| counts(&l.value));
        let mut attention = Vec::new();
        if !held && t.key.is_some() && pending.iter().any(|p| p.target == t.key) {
            attention.push(Attention::JournalPending);
        }
        if record
            .as_ref()
            .is_some_and(|l| l.status == VersionStatus::Newer)
        {
            attention.push(Attention::StateVersionNewer);
        }
        if addons.degraded > 0 {
            attention.push(Attention::AddonDegraded);
        }
        let report = record
            .as_ref()
            .map(|l| super::drift::report(&world.paths, t, &l.value, drift_mode));
        let drifted: Vec<&AddonDrift> = report
            .iter()
            .flat_map(|r| &r.addons)
            .filter(|a| match drift_mode {
                DriftMode::Presence => a.state == DriftState::Missing,
                _ => a.state != DriftState::Clean,
            })
            .collect();
        if drifted.iter().any(|a| a.state == DriftState::Modified) {
            attention.push(Attention::AddonModified);
        }
        if drifted.iter().any(|a| a.state == DriftState::Missing) {
            attention.push(Attention::AddonFolderMissing);
        }
        drift_lines.extend(drifted.iter().map(|a| drift_line(&t.id, a, verbose)));
        let drift: Vec<DriftItem> = drifted.iter().map(|a| DriftItem::of(a, verbose)).collect();
        let (updates, due) = record
            .as_ref()
            .map_or((UpdateCounts::default(), false), |l| {
                update_counts(&remembered, t, &l.value, &world.table, now, interval)
            });
        check_due |= due;
        let adoptable = if inv.counts.unmanaged > 0 {
            let catalog = catalog.get_or_insert_with(|| {
                let (catalog, loaded) = Catalog::load(&world.paths);
                warnings.extend(loaded);
                catalog
            });
            adoptable(&inv, record.as_ref().map(|l| &l.value), catalog)
        } else {
            0
        };
        let orphan_names = record
            .as_ref()
            .map(|l| orphans(&l.value))
            .unwrap_or_default();
        targets.push(StatusTarget {
            id: t.id.clone(),
            key: t.key,
            registered: t.key.is_some(),
            // ubs:ignore — a target key is a public ULID identifier, not a secret
            default: t.key.is_some() && t.key == world.registry.default,
            family: t.family,
            kind: t.kind,
            install_kind: t.install_kind,
            product: t.product.clone(),
            modes: world.table.mode_names(t.product.as_deref()),
            version: t.version,
            interface: t.interface,
            label: t.label.clone(),
            running: running.contains_key(&t.client_dir),
            missing: false,
            folders: inv.counts,
            addons,
            updates,
            drift,
            adoptable,
            orphans: orphan_names.len(),
            orphan_names,
            attention,
            client_dir: verbose.then(|| t.client_dir.clone()),
            install_root: verbose.then(|| t.install_root.clone()),
            exe_name: if verbose { t.exe_name.clone() } else { None },
        });
    }
    for e in &missing {
        let record = load_record(&world.paths, &e.key)?.value;
        let mut attention = vec![Attention::TargetMissing];
        if !held && pending.iter().any(|p| p.target == Some(e.key)) {
            attention.insert(0, Attention::JournalPending);
        }
        targets.push(StatusTarget {
            id: e.id.clone(),
            key: Some(e.key),
            registered: true,
            // ubs:ignore — a target key is a public ULID identifier, not a secret
            default: world.registry.default == Some(e.key),
            family: Family::Unknown,
            kind: TargetKind::Live,
            install_kind: if e.flavor_dir.is_some() {
                InstallKind::Battlenet
            } else {
                InstallKind::Custom
            },
            product: e.product.clone(),
            modes: world.table.mode_names(e.product.as_deref()),
            version: ClientVersion::new(0, 0, 0),
            interface: 0,
            label: None,
            running: false,
            missing: true,
            folders: Counts::default(),
            addons: counts(&record),
            // A missing client's fix is `targets rm`, not an update or a reinstall.
            updates: UpdateCounts::default(),
            drift: Vec::new(),
            adoptable: 0,
            orphans: 0,
            orphan_names: Vec::new(),
            attention,
            client_dir: verbose.then(|| e.client_dir.clone()),
            install_root: verbose.then(|| e.install_root.clone()),
            exe_name: None,
        });
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));

    let (mut commands, first_fix) = recommendations(&targets, check_due);
    if args.cached && !world.paths.discovery_cache().exists() {
        commands.push(CommandHint::new("status", ["omaddons", "status"], false));
    }
    let needs_attention = targets.iter().any(|t| !t.attention.is_empty());
    Ok(StatusOutcome {
        data: StatusData {
            cached: args.cached,
            targets,
            journal: JournalState {
                pending: pending.len(),
                in_progress: held,
            },
            needs_attention,
        },
        warnings,
        commands,
        check: args.check,
        first_fix,
        drift_lines,
        discovery: world.discovery,
        notes,
    })
}

/// The release memo as read-only commands use it: read once, never written, never
/// fetched; a memo that cannot be read is empty (`note` says why, for `-v`).
pub(super) struct Remembered {
    memo: ReleaseMemo,
    facts: FactIndex,
    enabled: BTreeSet<SourceId>,
    pub note: Option<String>,
}

impl Remembered {
    pub(super) fn load(world: &World) -> Self {
        let (memo, note) = memo::load(&world.paths);
        let facts = FactIndex::new(&memo.candidates);
        Self {
            memo,
            facts,
            enabled: SourceRegistry::builtin(&world.sources()).enabled(),
            note,
        }
    }

    /// What the memo says about each of `record`'s addons on `target`, from the shared
    /// evaluator (the statuses `outdated` shows).
    pub(super) fn statuses(
        &self,
        target: &Target,
        record: &TargetRecord,
        table: &RulesTable,
    ) -> Vec<StatusRow> {
        let entry = |r: &Ref, c: Channel| self.memo.entries.get(&memo::key(r, c)).cloned();
        statuses(
            &[(target, record)],
            &entry,
            &self.facts,
            &self.enabled,
            &|t, a| installed_primary_interface(t, a, table),
        )
    }

    fn fetched_at(&self, a: &AddonRecord) -> Option<Timestamp> {
        self.memo
            .entries
            .get(&memo::key(&a.r#ref, a.channel))
            .map(|e| e.fetched_at)
    }
}

/// A target's update counts, and whether it is due a check: it has checkable addons
/// (neither static nor from a source this build lacks) and one of them was never
/// checked, or the oldest check is stale.
fn update_counts(
    remembered: &Remembered,
    target: &Target,
    record: &TargetRecord,
    table: &RulesTable,
    now: Timestamp,
    interval: Duration,
) -> (UpdateCounts, bool) {
    let mut counts = UpdateCounts::default();
    let (mut checkable, mut never) = (false, false);
    let mut oldest: Option<Timestamp> = None;
    for row in remembered.statuses(target, record, table) {
        match row.status.kind() {
            StatusKind::UpdateAvailable => counts.available += 1,
            StatusKind::Pinned => counts.pinned += 1,
            StatusKind::Blocked => counts.blocked += 1,
            StatusKind::Failed => counts.failed += 1,
            StatusKind::Unchecked => counts.unchecked += 1,
            StatusKind::Current | StatusKind::Ahead | StatusKind::Static => {}
        }
        let disabled = matches!(
            row.status,
            UpdateStatus::Unchecked {
                reason: UncheckedReason::SourceDisabled
            }
        );
        if disabled || row.status.kind() == StatusKind::Static {
            continue;
        }
        let Some(a) = record.addons.get(&TargetRecord::addon_key(&row.addon)) else {
            continue;
        };
        checkable = true;
        match remembered.fetched_at(a) {
            Some(at) => oldest = Some(oldest.map_or(at, |o| o.min(at))),
            None => never = true,
        }
    }
    counts.checked_at = oldest.filter(|_| !never);
    counts.stale = is_stale(counts.checked_at, now, interval);
    (counts, checkable && (never || counts.stale))
}

/// Whether a check made at `checked_at` is too old at `now`: older than `interval`, or
/// in the future (clock skew). Never stale without a check.
fn is_stale(checked_at: Option<Timestamp>, now: Timestamp, interval: Duration) -> bool {
    checked_at.is_some_and(|at| at > now || now.duration_since(at).unsigned_abs() > interval)
}

/// The unmanaged folders a catalog entry names whose ref has no record on the target:
/// leftovers of addons managed here, and unknown folders, never count.
fn adoptable(inv: &Inventory, record: Option<&TargetRecord>, catalog: &Catalog) -> usize {
    let installed = |r: &Ref| {
        record.is_some_and(|rec| {
            rec.addons
                .values()
                .any(|a| a.r#ref.identity() == r.identity())
        })
    };
    inv.folders
        .iter()
        .filter(|e| e.class == FolderClass::Unmanaged)
        .filter(|e| {
            FolderName::try_from(e.name.clone()).is_ok_and(|name| {
                catalog
                    .by_folder(&name)
                    .iter()
                    .any(|entry| !installed(&entry.r#ref))
            })
        })
        .count()
}

/// The commands `status` recommends, in M2's order: (1) journal and state fixes (none
/// have a command), (2) `reinstall` (E3), (3) `update`, one for all targets with
/// updates or `-t` when only one has them, (4) `adopt` (E5.3), (5) `check_updates`
/// when `check_due`, (6) `remove_orphans` (C6), (7) the attention fixes (target order,
/// then item order); then a look at unmanaged folders. Also the first attention item's
/// message and fix, for `--check`.
fn recommendations(
    targets: &[StatusTarget],
    check_due: bool,
) -> (Vec<CommandHint>, Option<(String, Suggestion)>) {
    let (fixes, first) = attention_fixes(targets);
    let mut commands: Vec<CommandHint> = targets
        .iter()
        .flat_map(|t| {
            t.drift
                .iter()
                .filter(|d| needs_reinstall(d.state))
                .map(|d| reinstall_hint(&d.addon, &t.id))
        })
        .take(REINSTALL_HINTS_MAX)
        .collect();
    let with_updates: Vec<&StatusTarget> =
        targets.iter().filter(|t| t.updates.available > 0).collect();
    match with_updates.as_slice() {
        [] => {}
        [one] => commands.push(CommandHint::new(
            "update",
            ["omaddons", "update", "-t", one.id.as_str()],
            false,
        )),
        _ => commands.push(CommandHint::new("update", ["omaddons", "update"], false)),
    }
    if check_due {
        commands.push(CommandHint::new(
            "check_updates",
            ["omaddons", "outdated"],
            false,
        ));
    }
    for t in targets.iter().filter(|t| !t.orphan_names.is_empty()) {
        let argv = ["omaddons", "remove"]
            .into_iter()
            .chain(t.orphan_names.iter().map(String::as_str))
            .chain(["-t", t.id.as_str()]);
        commands.push(CommandHint::new("remove_orphans", argv, true));
    }
    commands.extend(fixes);
    for t in targets.iter().filter(|t| t.folders.unmanaged > 0) {
        commands.push(CommandHint::new(
            "list_unmanaged",
            ["omaddons", "list", "-t", t.id.as_str(), "--unmanaged"],
            false,
        ));
    }
    (commands, first)
}

/// The commands that address each target's attention items (target order, then item
/// order), and the first item's message and fix.
fn attention_fixes(targets: &[StatusTarget]) -> (Vec<CommandHint>, Option<(String, Suggestion)>) {
    let mut commands = Vec::new();
    let mut first: Option<(String, Suggestion)> = None;
    for t in targets {
        for a in &t.attention {
            let (message, fix, hint) = match a {
                Attention::JournalPending => (
                    format!("an interrupted transaction for {} awaits recovery", t.id),
                    Suggestion::text(
                        "run any omaddons command that changes addons (install, remove, undo); it recovers the transaction first",
                    ),
                    None,
                ),
                Attention::StateVersionNewer => (
                    format!("{}'s record was written by a newer omaddons", t.id),
                    Suggestion::text("upgrade omaddons"),
                    None,
                ),
                Attention::TargetMissing => {
                    let argv = ["omaddons", "targets", "rm", t.id.as_str()];
                    (
                        format!("{}'s client directory is gone", t.id),
                        Suggestion::command(argv),
                        Some(CommandHint::new("forget_target", argv, true)),
                    )
                }
                Attention::AddonDegraded => {
                    let argv = ["omaddons", "list", "-t", t.id.as_str(), "-v"];
                    (
                        format!("{} has {} degraded addon(s)", t.id, t.addons.degraded),
                        Suggestion::command(argv),
                        Some(CommandHint::new("inspect", argv, false)),
                    )
                }
                // Their `reinstall` hints come first among the recommendations.
                Attention::AddonModified | Attention::AddonFolderMissing => {
                    let (state, what) = if matches!(a, Attention::AddonModified) {
                        (DriftState::Modified, "modified")
                    } else {
                        (DriftState::Missing, "missing folders")
                    };
                    let drifted: Vec<&DriftItem> =
                        t.drift.iter().filter(|d| d.state == state).collect();
                    let fix = drifted.first().map_or_else(
                        || Suggestion::command(["omaddons", "status", "-t", t.id.as_str()]),
                        |d| Suggestion::command(reinstall_hint(&d.addon, &t.id).argv().to_vec()),
                    );
                    (
                        format!("{}: {} addon(s) {what}", t.id, drifted.len()),
                        fix,
                        None,
                    )
                }
            };
            if first.is_none() {
                first = Some((message, fix));
            }
            commands.extend(hint);
        }
    }
    (commands, first)
}

/// The names of `record`'s orphans, sorted ignoring case.
fn orphans(record: &TargetRecord) -> Vec<String> {
    let names = record.addons.values().filter(|a| a.is_orphan());
    crate::engine::edit::sorted_names(names.map(|a| a.name.clone()))
}

fn counts(record: &TargetRecord) -> AddonCounts {
    AddonCounts {
        total: record.addons.len(),
        degraded: record
            .addons
            .values()
            .filter(|a| a.state == AddonState::Degraded)
            .count(),
    }
}

/// A unit enum's wire name.
fn wire<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What a client is, as a column: the names of its game modes when it runs several
/// (`Classic Era · Hardcore · Season of Discovery`), else its family.
pub(super) fn modes_or_family(modes: &[ModeName], family: Family) -> String {
    if modes.len() > 1 {
        let names: Vec<&str> = modes.iter().map(|m| m.name.as_str()).collect();
        return names.join(" · ");
    }
    wire(family)
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    #[test]
    fn staleness_is_age_past_the_interval_or_a_future_check() {
        let now = Timestamp::now();
        let six_hours = Duration::from_secs(6 * 3600);
        let ago = |hours: i64| now - SignedDuration::from_hours(hours);
        assert!(
            !is_stale(None, now, six_hours),
            "never checked is not stale"
        );
        assert!(!is_stale(Some(ago(1)), now, six_hours));
        assert!(is_stale(Some(ago(7)), now, six_hours));
        assert!(!is_stale(Some(ago(7)), now, Duration::from_secs(48 * 3600)));
        assert!(is_stale(
            Some(now + SignedDuration::from_mins(5)),
            now,
            six_hours
        ));
    }

    fn target(id: &str, available: usize, attention: &[Attention], unmanaged: u32) -> StatusTarget {
        StatusTarget {
            id: id.to_owned(),
            key: None,
            registered: true,
            default: false,
            family: Family::Mainline,
            kind: TargetKind::Live,
            install_kind: InstallKind::Battlenet,
            product: None,
            modes: Vec::new(),
            version: ClientVersion::new(12, 1, 5),
            interface: 120_105,
            label: None,
            running: false,
            missing: attention.contains(&Attention::TargetMissing),
            folders: Counts {
                unmanaged,
                ..Counts::default()
            },
            addons: AddonCounts {
                total: 1,
                degraded: usize::from(attention.contains(&Attention::AddonDegraded)),
            },
            updates: UpdateCounts {
                available,
                ..UpdateCounts::default()
            },
            drift: Vec::new(),
            adoptable: 0,
            orphans: 0,
            orphan_names: Vec::new(),
            attention: attention.to_vec(),
            client_dir: None,
            install_root: None,
            exe_name: None,
        }
    }

    fn actions(commands: &[CommandHint]) -> Vec<String> {
        commands.iter().map(|c| c.action().to_owned()).collect()
    }

    #[test]
    fn recommendations_come_in_m2_order() {
        let targets = [
            target("classic", 1, &[Attention::AddonDegraded], 2),
            target("gone", 0, &[Attention::TargetMissing], 0),
            target("retail", 3, &[], 1),
        ];
        let (commands, first) = recommendations(&targets, true);
        assert_eq!(
            actions(&commands),
            [
                "update",
                "check_updates",
                "inspect",
                "forget_target",
                "list_unmanaged",
                "list_unmanaged"
            ]
        );
        assert_eq!(commands[0].argv(), ["omaddons", "update"], "every target");
        assert!(
            first.is_some_and(|(m, _)| m.contains("classic")),
            "first fix from attention"
        );

        // One target with updates is named; no check due, no check hint.
        let (commands, _) = recommendations(&[target("retail", 2, &[], 0)], false);
        assert_eq!(actions(&commands), ["update"]);
        assert_eq!(commands[0].argv(), ["omaddons", "update", "-t", "retail"]);
        assert_eq!(
            serde_json::to_value(&commands[0]).unwrap()["destructive"],
            false
        );
        let (commands, first) = recommendations(&[target("retail", 0, &[], 0)], false);
        assert!(commands.is_empty() && first.is_none());
    }

    #[test]
    fn orphans_are_recommended_after_the_update_check_and_before_attention() {
        let mut retail = target("retail", 1, &[Attention::AddonDegraded], 0);
        retail.orphans = 2;
        retail.orphan_names = vec!["Ace".into(), "BigWigs".into()];
        let (commands, _) = recommendations(&[retail], true);
        assert_eq!(
            actions(&commands),
            ["update", "check_updates", "remove_orphans", "inspect"]
        );
        assert_eq!(
            commands[2].argv(),
            ["omaddons", "remove", "Ace", "BigWigs", "-t", "retail"]
        );
        assert_eq!(
            serde_json::to_value(&commands[2]).unwrap()["destructive"],
            true
        );
        let none = target("retail", 0, &[], 0);
        assert!(recommendations(&[none], false).0.is_empty());
    }

    #[test]
    fn reinstalls_come_first_and_at_most_five() {
        let item = |addon: &str, state| DriftItem {
            addon: addon.to_owned(),
            state,
            files: 1,
            folders: None,
        };
        let mut retail = target("retail", 1, &[Attention::AddonModified], 0);
        retail.drift = (0..6)
            .map(|i| item(&format!("A{i}"), DriftState::Modified))
            .chain([item("Z", DriftState::Unindexed)])
            .collect();
        let (commands, first) = recommendations(&[retail], false);
        assert_eq!(
            actions(&commands),
            [
                "reinstall",
                "reinstall",
                "reinstall",
                "reinstall",
                "reinstall",
                "update"
            ]
        );
        assert_eq!(
            commands[0].argv(),
            ["omaddons", "install", "A0", "-t", "retail", "--reinstall"]
        );
        let (message, fix) = first.unwrap();
        assert_eq!(message, "retail: 6 addon(s) modified");
        assert_eq!(fix.argv().unwrap(), commands[0].argv());
    }
}
