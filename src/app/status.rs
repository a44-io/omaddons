//! `omaddons status [-t T] [--cached] [--check]`: offline triage, the first call for
//! orientation (vision §5.8). Every target with its client, live running state, folder
//! classes and addon counts; unfinished transactions; what needs attention and the
//! commands that address it. Read-only: never writes, never takes the lock, no network.
//!
//! `--cached` (the widget's authority from M3) never scans: it re-stats the discovery
//! cache's roots, or without a cache parses only the registered roots and recommends a
//! full `omaddons status`. Running state is live either way. `--check` turns any
//! attention into exit 6 (`check_failed`) with the data still there.
//!
//! Lua errors (M3-D20): plain `status` reads BugGrabber's files as `errors` does;
//! `--cached` recounts the errors memo and only stats files to tell it is stale. Neither
//! writes the memo.
//!
//! Backups (M3-D37): both modes read each client's block from the backup inventory,
//! validated by listing, and never rebuild it; a client without a recent checkpoint is
//! offered `backup create` whatever `auto_backup` says.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Serialize;

use super::backups_status::{self, BackupSummary};
use super::drift::{DriftItem, needs_reinstall, reinstall_hint};
use super::errors::CoverageState;
use super::errors_status::{self, TargetErrors};
use super::history::ChangeCounts;
use super::issues::{self, Issue, SeverityCounts};
use super::remembered::Remembered;
use super::world::World;
use crate::backup::inventory;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::discover::DiscoveredTarget;
use crate::discover::cache::{CacheStats, DiscoveryMode};
use crate::discover::running::detect_running;
use crate::discover::shared::{SharedAddons, shared_addons};
use crate::doctor;
use crate::doctor::facts::{
    AddonDriftFact, MachineFacts, NewerFile, PendingJournal, StatusFacts, TargetFacts, WidgetDrift,
};
use crate::doctor::registry::Tier;
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{AddonDrift, DriftMode, DriftState};
use crate::engine::journal;
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::errors::observe::{Observe, read_target};
use crate::errors::{ack, memo};
use crate::fsx::{LockState, probe_global};
use crate::inventory::{Counts, Depth, FolderClass, Inventory, inventory_shared};
use crate::limits::ERRORS_MAX_TOTAL_BYTES;
use crate::model::{Actor, AddonState, FolderName, Ref, RunId, Target, TargetKey, TxKind};
use crate::omarchy::status::{self as omarchy_status, WidgetInfo};
use crate::output::{Outcome, Style, table};
use crate::politeness::DEFAULT_CHECK_INTERVAL;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{StatusKind, UncheckedReason, UpdateStatus};
use crate::rules::{ClientVersion, Family, InstallKind, ModeName, RulesTable, TargetKind};
use crate::state::history::{self, HistoryEntry, HistoryFilter, Reach};
use crate::state::history_tail::{self, Answer};
use crate::state::paths::ClientPaths;
use crate::state::records::{TargetRecord, load_record};
use crate::state::registry::RegistryEntry;
use crate::state::store::{VersionStatus, Versioned};

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
    /// Every target at a glance: the bar's badge and urgency (M3-D37).
    pub summary: Summary,
    /// What bare `undo` would revert now; `null` when nothing is undoable, or, with
    /// `last_change_state: "unknown"`, when `--cached` cannot tell (never "nothing").
    pub last_change: Option<LastChange>,
    pub last_change_state: LastChangeState,
    /// The bar widget (E4): installed, on the bar, from this build; `null` without
    /// Omarchy's config directory.
    pub widget: Option<WidgetInfo>,
    /// Issues of no client: the machine's own (today, an outdated bar widget's).
    pub issues: Vec<Issue>,
}

/// Every target at a glance.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Summary {
    pub targets: usize,
    /// The ids of the clients running now.
    pub running: Vec<String>,
    pub updates: SummaryUpdates,
    pub issues: SeverityCounts,
    pub errors: SummaryErrors,
    /// `commands` holds `check_updates`: a check is due.
    pub check_due: bool,
    /// The bar widget serving this omaddons state is from another build: `widget`'s
    /// `outdated`, on an Omarchy desktop, for this run's `HOME` and XDG directories.
    pub widget_outdated: bool,
    pub state: SummaryState,
}

/// Pending updates across the targets.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SummaryUpdates {
    pub available: usize,
    /// The ids of the targets with updates.
    pub targets: Vec<String>,
}

/// New local Lua errors across the targets (received ones never count).
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct SummaryErrors {
    pub new: usize,
    /// The ids of the targets with new errors.
    pub targets: Vec<String>,
}

/// The most pressing state, in this order: a P0 or P1 issue, new Lua errors, updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SummaryState {
    Attention,
    Errors,
    Updates,
    Idle,
}

/// The run bare `undo` would revert.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LastChange {
    /// `null` for a transaction from before M2 (a run by itself).
    pub run: Option<RunId>,
    /// Its newest transaction's kind (`undo` for an undo, which the widget calls a redo).
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: Timestamp,
    pub actor: Actor,
    pub reason: Option<String>,
    /// The ids of the targets it changed, sorted.
    pub targets: Vec<String>,
    pub changes: LastChanges,
    /// The undo that reverts exactly this run, whatever commits after it.
    pub undo: UndoCommand,
}

/// Folders (and, for a restore, files) a run changed, summed over its transactions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LastChanges {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
    pub recorded: usize,
    /// Files a restore brought back (none until restores exist).
    pub restored: usize,
}

/// A command and its argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UndoCommand {
    pub command: String,
    pub argv: Vec<String>,
}

/// Whether `last_change` is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LastChangeState {
    Ok,
    /// `--cached`: the history projection is missing, stale or too short to tell.
    Unknown,
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
    /// `null` when the process table could not be read (`running_unknown`).
    pub running: Option<bool>,
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
    /// Addons installed here under `--any-flavor` (builds their publisher did not make
    /// for this client), sorted ignoring case. A standing choice, never attention, so
    /// `status --check` stays a health check.
    pub any_flavor: Vec<String>,
    pub attention: Vec<Attention>,
    /// The attention as ranked findings (M3-D37): vision D20 failure-mode ids, P0–P3
    /// severities and a remediation each. `--cached` checks drift by presence only, so a
    /// modified addon is not among them there.
    pub issues: Vec<Issue>,
    /// Its Lua errors, from BugGrabber: read live, or under `--cached` recounted from what
    /// `errors` last read (M3-D20).
    pub errors: TargetErrors,
    /// Its `WTF/` backups, from the inventory projection (M3-D37); `null` for a client
    /// whose directory is gone.
    pub backups: Option<BackupSummary>,
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
            writeln!(
                w,
                "No WoW client found. Add one with: omaddons targets add <path>"
            )?;
            if let Some(line) = widget_line(self.data.widget.as_ref()) {
                writeln!(w, "{line}")?;
            }
            // Its recommendations all the same: `status --cached` without a cache yet
            // knows no client, and its next step is a scan.
            for c in &self.commands {
                writeln!(w, "Next: {}", c.command())?;
            }
            return Ok(());
        }
        writeln!(w, "{}", summary_line(&self.data.summary))?;
        let rows: Vec<Vec<String>> = self
            .data
            .targets
            .iter()
            .map(|t| {
                let state = match (t.missing, t.running) {
                    (true, _) => "missing".to_owned(),
                    (false, Some(true)) => "running".to_owned(),
                    (false, None) => "running?".to_owned(),
                    (false, Some(false)) => t.label.clone().unwrap_or_else(|| "-".to_owned()),
                };
                let f = &t.folders;
                let mut counts = format!(
                    "{} managed · {} unmanaged · {} protected",
                    f.managed, f.unmanaged, f.protected
                );
                if f.shared > 0 {
                    counts.push_str(&format!(" · {} shared", f.shared));
                }
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
        // Each target's row, then its issues, its Lua errors and its backups under it.
        let text = table(&rows);
        let now = crate::clock::now();
        for (row, t) in text.lines().zip(&self.data.targets) {
            writeln!(w, "{row}")?;
            for issue in &t.issues {
                let mut line = format!("  {} {}", wire(issue.severity), issue.title);
                if let Some(command) = &issue.remediation.command {
                    line.push_str(&format!(" → {command}"));
                }
                writeln!(w, "{line}")?;
            }
            if let Some(line) = errors_status::human_line(&t.id, &t.errors) {
                writeln!(w, "{line}")?;
            }
            if let Some(backups) = &t.backups {
                writeln!(w, "{}", backups_status::human_line(backups, now))?;
            }
            if !t.any_flavor.is_empty() {
                writeln!(
                    w,
                    "  any flavor: {} (builds not made for this client, by --any-flavor)",
                    t.any_flavor.join(", ")
                )?;
            }
        }
        for line in &self.drift_lines {
            writeln!(w, "{line}")?;
        }
        if let Some(line) = widget_line(self.data.widget.as_ref()) {
            writeln!(w, "{line}")?;
        }
        match (&self.data.last_change, self.data.last_change_state) {
            (_, LastChangeState::Unknown) => {
                writeln!(w, "last change: unknown (run omaddons status for it)")?
            }
            // A repair outside every client names no target.
            (Some(c), LastChangeState::Ok) if c.targets.is_empty() => writeln!(
                w,
                "last change: {} by {} ({}) · undo: {}",
                wire(c.kind),
                c.actor.as_str(),
                super::history::local_time(c.at),
                c.undo.command
            )?,
            (Some(c), LastChangeState::Ok) => writeln!(
                w,
                "last change: {} on {} by {} ({}) · undo: {}",
                wire(c.kind),
                c.targets.join(", "),
                c.actor.as_str(),
                super::history::local_time(c.at),
                c.undo.command
            )?,
            (None, LastChangeState::Ok) => {}
        }
        for c in &self.commands {
            writeln!(w, "Next: {}", c.command())?;
        }
        Ok(())
    }
}

/// The first line: `3 updates on 2 clients · 1 issue (P2) · 4 new Lua errors on 1 client
/// · retail running`.
fn summary_line(s: &Summary) -> String {
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts = Vec::new();
    if s.updates.available > 0 {
        parts.push(format!(
            "{} on {}",
            plural(s.updates.available, "update", "updates"),
            plural(s.updates.targets.len(), "client", "clients")
        ));
    }
    let total = s.issues.total();
    if total > 0 {
        let severities: Vec<&str> = [
            (s.issues.p0, "P0"),
            (s.issues.p1, "P1"),
            (s.issues.p2, "P2"),
            (s.issues.p3, "P3"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(_, name)| name)
        .collect();
        parts.push(format!(
            "{} ({})",
            plural(total, "issue", "issues"),
            severities.join(", ")
        ));
    }
    if s.errors.new > 0 {
        parts.push(format!(
            "{} on {}",
            plural(s.errors.new, "new Lua error", "new Lua errors"),
            plural(s.errors.targets.len(), "client", "clients")
        ));
    }
    if !s.running.is_empty() {
        parts.push(format!("{} running", s.running.join(", ")));
    }
    if parts.is_empty() {
        "Nothing needs doing".to_owned()
    } else {
        parts.join(" · ")
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

/// The bar widget's human line, when there is something to say: an outdated widget, with
/// its fix, or one serving another omaddons state, as a note with no command (M3-D7).
fn widget_line(widget: Option<&WidgetInfo>) -> Option<String> {
    let widget = widget.filter(|w| w.installed)?;
    if widget.needs_reinstall() {
        return Some(
            "bar widget: from another omaddons build → omaddons omarchy install".to_owned(),
        );
    }
    if widget.context_matches == Some(false) {
        let build = if widget.outdated {
            ", from another omaddons build"
        } else {
            ""
        };
        return Some(format!(
            "bar widget: serves another omaddons state (installed under other HOME or XDG \
             directories){build}"
        ));
    }
    None
}

/// Runs `status`.
/// `target_addons_shared` for one group of targets sharing an AddOns directory.
fn shared_warning(group: &SharedAddons) -> Warning {
    let names = match group.targets.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => group.targets.join(""),
    };
    let first = group.targets.first().map_or("", String::as_str);
    Warning::new(
        WarningCode::TargetAddonsShared,
        format!(
            "{names} share one AddOns directory ({}): each keeps its own records, so each \
             sees the others' addons as unmanaged",
            group.path.display()
        ),
    )
    .with_fix(Suggestion::text(format!(
        "manage these addons from one client (-t {first}) and do not adopt them on the \
         others, or stop sharing the directory"
    )))
}

pub fn run(ctx: &Context, args: StatusArgs) -> Result<StatusOutcome> {
    let mode = if args.cached {
        DiscoveryMode::NoScan
    } else {
        DiscoveryMode::Full
    };
    let world = World::load_with(ctx, mode)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();

    let selected: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    // Registered clients whose dir is gone: listed without -t and with -t all.
    let missing: Vec<&RegistryEntry> = match &ctx.target {
        Some(_) if !world.selects_all(ctx) => Vec::new(),
        _ => world.resolved.missing.iter().collect(),
    };
    let found: Vec<Target> = selected.iter().map(|d| d.target.clone()).collect();
    let running = detect_running(&found);
    warnings.extend(running.warning());
    let pending = journal::pending(&world.paths)?;
    let held = matches!(probe_global(&world.paths)?, LockState::Held { .. });
    // Read once: what earlier commands learned about releases.
    let remembered = Remembered::load(&world);
    let mut notes: Vec<String> = remembered.note.iter().cloned().collect();
    // Read once too, and never written: what `errors` last read, and the dismissals.
    let (errors_memo, _) = memo::load(&world.paths);
    let (acks, ack_note) = ack::read(&world.paths);
    notes.extend(ack_note);
    // Read once too, never rebuilt here: what the backups add up to, per client.
    let backup_inventory = inventory::load_file(&world.paths);
    let auto_mode = world.config.auto_backup.unwrap_or_default();
    // Lua errors' ages and backups' are told by the one clock.
    let errors_now = crate::clock::now();
    // Plain `status` reads BugGrabber's files, as `errors` does (local-time strings in
    // this machine's zone, within one run's byte budget).
    let live_errors = (!args.cached).then(TimeZone::system);
    let mut errors_budget = ERRORS_MAX_TOTAL_BYTES;
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

    // Targets sharing one AddOns directory, every group with a selected target in it.
    let discovered = world.resolved.targets.iter().map(|d| &d.target);
    for group in shared_addons(discovered) {
        if group
            .targets
            .iter()
            .any(|id| found.iter().any(|t| t.id == *id))
        {
            warnings.push(shared_warning(&group));
        }
    }

    let mut targets = Vec::new();
    // Each registered target's facts for the failure-mode registry.
    let mut facts = Vec::new();
    for d in &selected {
        let t = &d.target;
        let record = match t.key {
            Some(key) => {
                let loaded = load_record(&world.paths, &key)?;
                let warning = loaded.warning(&world.paths.target_record(&key));
                ctx.notes.warn_all(warning.as_slice());
                warnings.extend(warning);
                Some(loaded)
            }
            None => None,
        };
        // Plain `status` reads the loaded TOCs too: an addon that embeds BugGrabber's
        // database says so in its TOC.
        let depth = if args.cached {
            Depth::Classes
        } else {
            Depth::Full
        };
        let inv = inventory_shared(
            t,
            record.as_ref().map(|l| &l.value),
            &world.sibling_records(t)?,
            &world.table,
            depth,
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
                let (catalog, loaded) = world.catalog(ctx);
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
        let filed_as = t.key.map_or_else(|| t.id.clone(), |k| k.to_string());
        let client = ClientPaths::new(&t.client_dir);
        let errors = match &live_errors {
            Some(tz) => {
                let owner = |folder: &str| {
                    super::errors::owner_of(record.as_ref().map(|l| &l.value), folder)
                };
                let observation = read_target(
                    &Observe {
                        target: &t.id,
                        client: &client,
                        inventory: &inv,
                        owner: &owner,
                        memo: errors_memo.targets.get(&filed_as),
                        ack: acks.targets.get(&filed_as),
                        now: errors_now,
                        tz,
                    },
                    &mut errors_budget,
                );
                errors_status::live(&observation)
            }
            None => {
                let installed = |folder: &str| {
                    inv.folders
                        .iter()
                        .any(|f| f.name.eq_ignore_ascii_case(folder))
                };
                errors_status::cached(
                    &client,
                    &installed,
                    errors_memo.targets.get(&filed_as),
                    acks.targets.get(&filed_as),
                    errors_now,
                )
            }
        };
        let backups = backups_status::summary(
            &backup_inventory,
            &client,
            t.key.as_ref(),
            auto_mode,
            errors_now,
        );
        let target = StatusTarget {
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
            running: running
                .unavailable
                .is_none()
                .then(|| running.clients.contains_key(&t.client_dir)),
            missing: false,
            folders: inv.counts,
            addons,
            updates,
            drift,
            adoptable,
            orphans: orphan_names.len(),
            orphan_names,
            any_flavor: record
                .as_ref()
                .map(|l| any_flavor(&l.value))
                .unwrap_or_default(),
            attention,
            issues: Vec::new(),
            errors,
            backups: Some(backups),
            client_dir: verbose.then(|| t.client_dir.clone()),
            install_root: verbose.then(|| t.install_root.clone()),
            exe_name: if verbose { t.exe_name.clone() } else { None },
        };
        if let Some(key) = target.key {
            let newer_record = record
                .as_ref()
                .filter(|l| l.status == VersionStatus::Newer)
                .map(|l| NewerFile {
                    path: world.paths.target_record(&key),
                    on_disk: l.version,
                    writes: <TargetRecord as Versioned>::CURRENT,
                });
            let missing_folders = |addon: &str| -> Vec<PathBuf> {
                drifted
                    .iter()
                    .filter(|a| a.addon == addon)
                    .flat_map(|a| a.folders.iter().filter(|f| !f.present))
                    .map(|f| client.addons().join(f.folder.as_str()))
                    .collect()
            };
            facts.push(target_facts(
                &target,
                key,
                pending_journals(&pending, Some(key), held),
                newer_record,
                None,
                &missing_folders,
            ));
        }
        targets.push(target);
    }
    for e in &missing {
        let record = load_record(&world.paths, &e.key)?.value;
        let mut attention = vec![Attention::TargetMissing];
        if !held && pending.iter().any(|p| p.target == Some(e.key)) {
            attention.insert(0, Attention::JournalPending);
        }
        let target = StatusTarget {
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
            running: Some(false),
            missing: true,
            folders: Counts::default(),
            addons: counts(&record),
            // A missing client's fix is `targets rm`, not an update or a reinstall.
            updates: UpdateCounts::default(),
            drift: Vec::new(),
            adoptable: 0,
            orphans: 0,
            orphan_names: Vec::new(),
            any_flavor: any_flavor(&record),
            attention,
            issues: Vec::new(),
            // Nothing to read: the directory is gone.
            errors: TargetErrors::uncaptured(CoverageState::Complete),
            backups: None,
            client_dir: verbose.then(|| e.client_dir.clone()),
            install_root: verbose.then(|| e.install_root.clone()),
            exe_name: None,
        };
        facts.push(target_facts(
            &target,
            e.key,
            pending_journals(&pending, Some(e.key), held),
            None,
            Some(e.client_dir.clone()),
            &|_| Vec::new(),
        ));
        targets.push(target);
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));

    // The bar widget: file reads only, no subprocess (E4).
    let widget = omarchy_status::widget(&world.paths);
    let widget_outdated = widget.as_ref().is_some_and(WidgetInfo::needs_reinstall);
    if widget_outdated {
        warnings.push(
            Warning::new(
                WarningCode::WidgetOutdated,
                "the bar widget is from another omaddons build, so it stays read-only",
            )
            .with_fix(Suggestion::command(["omaddons", "omarchy", "install"])),
        );
    }
    // A pending journal that names no client (it cannot be read) is attention too: no
    // target carries it, and it needs manual repair.
    let unattributed = !held && pending.iter().any(|p| p.target.is_none());

    // Each issue is a finding of the failure-mode registry (E1), from the facts gathered
    // above: `--cached` runs the `cached` rows, plain `status` the `quick` ones too.
    let machine = machine_facts(widget.as_ref(), pending_journals(&pending, None, held));
    let view = DoctorView::from_status_facts(
        world.paths.clone(),
        StatusFacts {
            targets: facts,
            machine,
        },
    );
    let max_tier = if args.cached {
        Tier::Cached
    } else {
        Tier::Quick
    };
    let found = doctor::detect(
        &view,
        &Selection::only(issues::STATUS_FAILURE_MODES),
        max_tier,
    );
    debug_assert!(
        found.incomplete.is_empty(),
        "status's rows walk nothing: {:?}",
        found.incomplete
    );
    for t in &mut targets {
        t.issues = t
            .key
            .map(|key| issues::for_target(&found.findings, key))
            .unwrap_or_default();
    }
    let issues = issues::for_machine(&found.findings);

    let (mut commands, mut first_fix) = recommendations(&targets, check_due, widget_outdated);
    if unattributed && first_fix.is_none() {
        first_fix = Some((
            "an interrupted change needs manual repair: its journal cannot be read".to_owned(),
            Suggestion::text(
                "see \"journal_pending that does not clear\" under Troubleshooting in the README",
            ),
        ));
    }
    if let Some(selector) = ctx.target.as_deref() {
        scope_update(&mut commands, selector);
    }
    if args.cached && !world.paths.discovery_cache().exists() {
        commands.push(CommandHint::new(Action::Status, ["omaddons", "status"]));
    }
    let needs_attention = unattributed || targets.iter().any(|t| !t.attention.is_empty());
    let summary = summary(&targets, &issues, &commands, widget_outdated);
    let (last_change, last_change_state) = last_change(&world, args.cached);
    Ok(StatusOutcome {
        data: StatusData {
            cached: args.cached,
            targets,
            journal: JournalState {
                pending: pending.len(),
                in_progress: held,
            },
            needs_attention,
            summary,
            last_change,
            last_change_state,
            widget,
            issues,
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
/// The `adopt` recommendation for one target: the report of what its unmanaged folders
/// are, which changes nothing.
pub(crate) fn adopt_hint(target: &str) -> CommandHint {
    CommandHint::new(Action::Adopt, ["omaddons", "adopt", "-t", target])
}

/// `outdated`'s and `update`'s `adopt` hints: one per target of `targets` that manages
/// nothing yet and has an [`adoptable`] folder. Offline, one class-depth inventory per
/// such target, the catalog loaded only when one has an unmanaged folder.
pub(crate) fn adopt_hints(
    world: &World,
    targets: &[&DiscoveredTarget],
    records: &BTreeMap<String, TargetRecord>,
) -> Vec<CommandHint> {
    let mut catalog: Option<Catalog> = None;
    let mut hints = Vec::new();
    for d in targets {
        let record = records.get(&d.target.id);
        if record.is_some_and(|r| !r.addons.is_empty()) {
            continue;
        }
        let shared = world.sibling_records(&d.target).unwrap_or_default();
        let Ok(inv) = inventory_shared(&d.target, record, &shared, &world.table, Depth::Classes)
        else {
            continue;
        };
        if inv.counts.unmanaged == 0 {
            continue;
        }
        let catalog = catalog.get_or_insert_with(|| Catalog::load(&world.paths).0);
        if adoptable(&inv, record, catalog) > 0 {
            hints.push(adopt_hint(&d.target.id));
        }
    }
    hints
}

/// The unmanaged folders of `inv` that a catalog entry not installed here names: offline,
/// no TOC read, a managed addon's leftovers excluded.
pub(crate) fn adoptable(
    inv: &Inventory,
    record: Option<&TargetRecord>,
    catalog: &Catalog,
) -> usize {
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

/// Every target at a glance, from the targets, the machine's own issues and the
/// recommendations.
fn summary(
    targets: &[StatusTarget],
    machine: &[Issue],
    commands: &[CommandHint],
    widget_outdated: bool,
) -> Summary {
    let mut issues = SeverityCounts::default();
    for t in targets {
        issues.add(&t.issues);
    }
    issues.add(machine);
    let with_updates: Vec<&StatusTarget> =
        targets.iter().filter(|t| t.updates.available > 0).collect();
    let updates = SummaryUpdates {
        available: with_updates.iter().map(|t| t.updates.available).sum(),
        targets: with_updates.iter().map(|t| t.id.clone()).collect(),
    };
    let with_errors: Vec<&StatusTarget> = targets.iter().filter(|t| t.errors.new > 0).collect();
    let errors = SummaryErrors {
        new: with_errors.iter().map(|t| t.errors.new).sum(),
        targets: with_errors.iter().map(|t| t.id.clone()).collect(),
    };
    let state = summary_state(&issues, errors.new, updates.available);
    Summary {
        targets: targets.len(),
        running: targets
            .iter()
            .filter(|t| t.running == Some(true))
            .map(|t| t.id.clone())
            .collect(),
        updates,
        issues,
        errors,
        check_due: commands.iter().any(|c| c.action() == Action::CheckUpdates),
        widget_outdated,
        state,
    }
}

/// The most pressing state: a P0 or P1 issue, then new Lua errors, then updates.
fn summary_state(issues: &SeverityCounts, new_errors: usize, updates: usize) -> SummaryState {
    if issues.urgent() {
        SummaryState::Attention
    } else if new_errors > 0 {
        SummaryState::Errors
    } else if updates > 0 {
        SummaryState::Updates
    } else {
        SummaryState::Idle
    }
}

/// What bare `undo` would revert now. `--cached` asks the history projection and says
/// `unknown` when it cannot tell (M3-D37); plain `status` reads the whole history. Neither
/// writes anything.
fn last_change(world: &World, cached: bool) -> (Option<LastChange>, LastChangeState) {
    let empty = match std::fs::metadata(world.paths.history_file()) {
        Ok(meta) => meta.len() == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    if empty {
        // No history (a fresh home): nothing to undo, known without a projection.
        return (None, LastChangeState::Ok);
    }
    if cached {
        return match history_tail::fresh(&world.paths).map(|tail| tail.answer()) {
            Some(Answer::Found(run)) => {
                let entries: Vec<&HistoryEntry> = run.iter().collect();
                (Some(change_of(world, &entries)), LastChangeState::Ok)
            }
            Some(Answer::Nothing { complete: true }) => (None, LastChangeState::Ok),
            _ => (None, LastChangeState::Unknown),
        };
    }
    let Ok(read) = history::read(&world.paths, &HistoryFilter::default()) else {
        return (None, LastChangeState::Unknown);
    };
    match history::newest_run(&read.entries, None, None) {
        Some(found) => (Some(change_of(world, &found.entries)), LastChangeState::Ok),
        None => (None, LastChangeState::Ok),
    }
}

/// The last change a run's transactions (`entries`, newest first) make up.
fn change_of(world: &World, entries: &[&HistoryEntry]) -> LastChange {
    let mut changes = LastChanges::default();
    let mut targets: Vec<String> = Vec::new();
    for e in entries {
        let c = ChangeCounts::of(&e.ops);
        changes.added += c.added;
        changes.replaced += c.replaced;
        changes.removed += c.removed;
        changes.recorded += c.recorded;
        changes.restored += c.restored;
        // A repair names the clients it touched, or, having touched a client's files
        // without naming it, every registered one.
        let ids: Vec<String> = match e.reach() {
            Reach::Targets(keys) => keys.into_iter().map(|key| world.target_id(key)).collect(),
            Reach::Every => world
                .registry
                .targets
                .iter()
                .map(|t| t.id.clone())
                .collect(),
        };
        for id in ids {
            if !targets.contains(&id) {
                targets.push(id);
            }
        }
    }
    targets.sort();
    let newest = entries.first();
    let run = newest.and_then(|e| e.run);
    let argv: Vec<String> = match (run, newest) {
        (Some(run), _) => ["omaddons", "undo", "--run"]
            .map(str::to_owned)
            .into_iter()
            .chain([run.to_string()])
            .collect(),
        (None, Some(e)) => vec!["omaddons".into(), "undo".into(), e.tx.to_string()],
        (None, None) => vec!["omaddons".into(), "undo".into()],
    };
    LastChange {
        run,
        kind: newest.map_or(TxKind::Undo, |e| e.kind),
        at: newest.map_or_else(crate::clock::now, |e| e.at),
        actor: newest.map_or(Actor::Human, |e| e.actor),
        reason: newest.and_then(|e| e.reason.clone()),
        targets,
        changes,
        undo: UndoCommand {
            command: shell_words::join(&argv),
            argv,
        },
    }
}

/// A registered target's facts for the failure-mode registry (E1), mirroring its
/// `attention` exactly: each fact is kept only when `status` raised the attention item it
/// stands for, so the registry finds what `status` reports. `missing_folders` gives a
/// drifted addon's folders gone from `Interface/AddOns`.
pub(crate) fn target_facts(
    target: &StatusTarget,
    key: TargetKey,
    journals: Vec<PendingJournal>,
    newer_record: Option<NewerFile>,
    vanished: Option<PathBuf>,
    missing_folders: &dyn Fn(&str) -> Vec<PathBuf>,
) -> TargetFacts {
    let raised = |attention| target.attention.contains(&attention);
    TargetFacts {
        id: target.id.clone(),
        key,
        journals: if raised(Attention::JournalPending) {
            journals
        } else {
            Vec::new()
        },
        newer_record: newer_record.filter(|_| raised(Attention::StateVersionNewer)),
        vanished: vanished.filter(|_| raised(Attention::TargetMissing)),
        degraded: if raised(Attention::AddonDegraded) {
            target.addons.degraded
        } else {
            0
        },
        drift: target
            .drift
            .iter()
            .filter(|d| needs_reinstall(d.state))
            .map(|d| AddonDriftFact {
                addon: d.addon.clone(),
                state: d.state,
                files: d.files,
                missing_folders: missing_folders(&d.addon),
            })
            .collect(),
    }
}

/// The pending journals naming `target` (`None`: naming no client), oldest first, while
/// no omaddons process holds the lock; a held lock means a change is committing, not
/// interrupted.
pub(crate) fn pending_journals(
    pending: &[journal::Pending],
    target: Option<TargetKey>,
    held: bool,
) -> Vec<PendingJournal> {
    if held {
        return Vec::new();
    }
    pending
        .iter()
        .filter(|p| p.target == target)
        .map(|p| PendingJournal {
            tx: p.tx,
            unrecoverable: p.unrecoverable,
        })
        .collect()
}

/// The machine's facts: the journals that name no client, and the bar widget when it
/// needs a reinstall.
pub(crate) fn machine_facts(
    widget: Option<&WidgetInfo>,
    unattributed: Vec<PendingJournal>,
) -> MachineFacts {
    MachineFacts {
        unattributed,
        widget: widget.filter(|w| w.needs_reinstall()).map(|w| WidgetDrift {
            version: w.version.clone(),
        }),
    }
}

/// `targets rm <id>`, the fix for a client whose directory is gone.
pub(crate) fn forget_target_hint(target: &str) -> CommandHint {
    CommandHint::new(Action::ForgetTarget, ["omaddons", "targets", "rm", target])
}

/// `list -t <id> -v`, the look at a target's degraded addons.
pub(crate) fn inspect_hint(target: &str) -> CommandHint {
    CommandHint::new(Action::Inspect, ["omaddons", "list", "-t", target, "-v"])
}

/// The commands `status` recommends, in M2's order: (1) journal and state fixes (none
/// have a command), (2) `reinstall` (E3), (3) `update`, one for all targets with
/// updates or `-t` when only one has them, (4) `adopt` (E5.3), (5) `check_updates`
/// when `check_due`, then `check_errors` when a target's Lua errors are stale or
/// unchecked, each target's backup hints, and `reinstall_widget` for an outdated bar
/// widget (M3), (6) `remove_orphans` (C6), (7) the attention fixes (target order, then
/// item order); then a look at unmanaged folders. Also the first attention item's
/// message and fix, for `--check`.
/// The bare `update` hint (several clients have updates) under `-t <selector>`: a hint
/// is never wider than the selection it answers, and bare `update` reaches every client.
pub(super) fn scope_update(commands: &mut [CommandHint], selector: &str) {
    for hint in commands
        .iter_mut()
        .filter(|h| h.action() == Action::Update && h.argv() == ["omaddons", "update"])
    {
        *hint = CommandHint::new(Action::Update, ["omaddons", "update", "-t", selector]);
    }
}

fn recommendations(
    targets: &[StatusTarget],
    check_due: bool,
    widget_outdated: bool,
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
            Action::Update,
            ["omaddons", "update", "-t", one.id.as_str()],
        )),
        _ => commands.push(CommandHint::new(Action::Update, ["omaddons", "update"])),
    }
    // A catalog addon found unmanaged: the adopt report says what it is. Unknown-only
    // folders keep the plain look below.
    for t in targets.iter().filter(|t| t.adoptable > 0) {
        commands.push(adopt_hint(&t.id));
    }
    if check_due {
        commands.push(CommandHint::new(
            Action::CheckUpdates,
            ["omaddons", "outdated"],
        ));
    }
    // Never for a read that was incomplete: reading it again would not complete it.
    if targets.iter().any(|t| t.errors.worth_checking()) {
        commands.push(CommandHint::new(
            Action::CheckErrors,
            ["omaddons", "errors"],
        ));
    }
    for t in targets {
        if let Some(backups) = &t.backups {
            commands.extend(backups_status::hints(&t.id, backups));
        }
    }
    if widget_outdated {
        commands.push(omarchy_status::reinstall_hint());
    }
    for t in targets.iter().filter(|t| !t.orphan_names.is_empty()) {
        let argv = ["omaddons", "remove"]
            .into_iter()
            .chain(t.orphan_names.iter().map(String::as_str))
            .chain(["-t", t.id.as_str()]);
        commands.push(CommandHint::new(Action::RemoveOrphans, argv));
    }
    commands.extend(fixes);
    for t in targets
        .iter()
        .filter(|t| t.folders.unmanaged > 0 && t.adoptable == 0)
    {
        commands.push(CommandHint::new(
            Action::ListUnmanaged,
            ["omaddons", "list", "-t", t.id.as_str(), "--unmanaged"],
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
                    let hint = forget_target_hint(&t.id);
                    (
                        format!("{}'s client directory is gone", t.id),
                        Suggestion::command(hint.argv().to_vec()),
                        Some(hint),
                    )
                }
                Attention::AddonDegraded => {
                    let hint = inspect_hint(&t.id);
                    (
                        format!("{} has {} degraded addon(s)", t.id, t.addons.degraded),
                        Suggestion::command(hint.argv().to_vec()),
                        Some(hint),
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

/// The addons installed under `--any-flavor`, sorted ignoring case.
fn any_flavor(record: &TargetRecord) -> Vec<String> {
    let names = record.addons.values().filter(|a| a.any_flavor);
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

/// A retail-like target with `available` updates, `attention` and `unmanaged` folders,
/// for unit tests.
#[cfg(test)]
pub(crate) fn test_target(
    id: &str,
    available: usize,
    attention: &[Attention],
    unmanaged: u32,
) -> StatusTarget {
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
        running: Some(false),
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
        any_flavor: Vec::new(),
        attention: attention.to_vec(),
        issues: Vec::new(),
        errors: TargetErrors::uncaptured(CoverageState::Complete),
        backups: None,
        client_dir: None,
        install_root: None,
        exe_name: None,
    }
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::test_target as target;
    use super::*;

    #[test]
    fn the_summary_state_takes_the_most_pressing() {
        let counts = |p0, p1, p2| SeverityCounts { p0, p1, p2, p3: 0 };
        assert_eq!(
            summary_state(&counts(1, 0, 0), 2, 3),
            SummaryState::Attention
        );
        assert_eq!(
            summary_state(&counts(0, 1, 0), 0, 0),
            SummaryState::Attention
        );
        assert_eq!(
            summary_state(&counts(0, 0, 4), 2, 3),
            SummaryState::Errors,
            "a P2 issue is not urgent"
        );
        assert_eq!(summary_state(&counts(0, 0, 0), 0, 3), SummaryState::Updates);
        assert_eq!(summary_state(&counts(0, 0, 1), 0, 0), SummaryState::Idle);
    }

    #[test]
    fn the_summary_counts_every_target() {
        let mut gone = target("gone", 0, &[Attention::TargetMissing], 0);
        gone.issues = crate::app::issues::tests::registry_issues(&gone, &[], false);
        let mut retail = target("retail", 2, &[], 0);
        retail.running = Some(true);
        let classic = target("classic", 1, &[], 0);
        let check = CommandHint::new(Action::CheckUpdates, ["omaddons", "outdated"]);
        let s = summary(&[classic, gone, retail], &[], &[check], false);
        assert_eq!(s.targets, 3);
        assert_eq!(s.running, ["retail"]);
        assert_eq!(
            (s.updates.available, s.updates.targets.clone()),
            (3, vec!["classic".to_owned(), "retail".to_owned()])
        );
        assert_eq!((s.issues.p2, s.issues.total()), (1, 1));
        assert!(s.check_due);
        assert_eq!(s.state, SummaryState::Updates);
        assert_eq!(
            summary_line(&s),
            "3 updates on 2 clients · 1 issue (P2) · retail running"
        );
        assert_eq!(
            summary_line(&summary(&[target("retail", 0, &[], 0)], &[], &[], false)),
            "Nothing needs doing"
        );
    }

    /// A target's errors block with `new` new and `received` received errors.
    fn lua(state: errors_status::ErrorsState, new: usize, received: usize) -> TargetErrors {
        TargetErrors {
            capture: crate::app::errors::CaptureState::Buggrabber,
            state,
            new,
            total: new + 1,
            received,
            ..TargetErrors::uncaptured(CoverageState::Complete)
        }
    }

    #[test]
    fn new_local_errors_count_and_received_ones_never_do() {
        use errors_status::ErrorsState;
        let mut retail = target("retail", 2, &[], 0);
        retail.errors = lua(ErrorsState::Ok, 4, 0);
        let mut classic = target("classic", 0, &[], 0);
        classic.errors = lua(ErrorsState::Ok, 0, 3);
        let s = summary(&[classic.clone(), retail], &[], &[], false);
        assert_eq!(
            (s.errors.new, s.errors.targets.clone()),
            (4, vec!["retail".to_owned()])
        );
        assert_eq!(s.state, SummaryState::Errors, "errors outrank updates");
        assert_eq!(
            summary_line(&s),
            "2 updates on 1 client · 4 new Lua errors on 1 client"
        );
        let s = summary(&[classic], &[], &[], false);
        assert_eq!((s.errors.new, s.state), (0, SummaryState::Idle));
        assert_eq!(summary_line(&s), "Nothing needs doing");
    }

    #[test]
    fn errors_are_checked_again_only_when_stale_or_unchecked() {
        use errors_status::ErrorsState;
        let with = |state| {
            let mut t = target("retail", 0, &[], 0);
            t.errors = lua(state, 1, 0);
            recommendations(&[t], true, false).0
        };
        for state in [ErrorsState::Stale, ErrorsState::Unchecked] {
            let commands = with(state);
            assert_eq!(
                actions(&commands),
                ["check_updates", "check_errors"],
                "{state:?}"
            );
            assert_eq!(commands[1].argv(), ["omaddons", "errors"]);
            assert_eq!(
                serde_json::to_value(&commands[1]).unwrap()["destructive"],
                false
            );
        }
        for state in [
            ErrorsState::Ok,
            ErrorsState::Partial,
            ErrorsState::Unreadable,
        ] {
            assert_eq!(actions(&with(state)), ["check_updates"], "{state:?}");
        }
    }

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

    fn actions(commands: &[CommandHint]) -> Vec<String> {
        commands
            .iter()
            .map(|c| c.action().wire().to_owned())
            .collect()
    }

    /// Under `-t`, the update hint for several clients names the same selector; one
    /// already naming its client, and any other hint, is left as it is.
    #[test]
    fn the_update_hint_is_never_wider_than_the_selection() {
        let mut commands = vec![
            CommandHint::new(Action::Update, ["omaddons", "update"]),
            CommandHint::new(Action::Update, ["omaddons", "update", "-t", "era"]),
            CommandHint::new(Action::CheckUpdates, ["omaddons", "outdated"]),
        ];
        scope_update(&mut commands, "vanilla");
        let argvs: Vec<Vec<String>> = commands.iter().map(|c| c.argv().to_vec()).collect();
        assert_eq!(
            argvs,
            [
                vec!["omaddons", "update", "-t", "vanilla"],
                vec!["omaddons", "update", "-t", "era"],
                vec!["omaddons", "outdated"],
            ]
            .map(|argv| argv.into_iter().map(str::to_owned).collect::<Vec<_>>())
        );
    }

    #[test]
    fn recommendations_come_in_m2_order() {
        let targets = [
            target("classic", 1, &[Attention::AddonDegraded], 2),
            target("gone", 0, &[Attention::TargetMissing], 0),
            target("retail", 3, &[], 1),
        ];
        let (commands, first) = recommendations(&targets, true, false);
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
        let (commands, _) = recommendations(&[target("retail", 2, &[], 0)], false, false);
        assert_eq!(actions(&commands), ["update"]);
        assert_eq!(commands[0].argv(), ["omaddons", "update", "-t", "retail"]);
        assert_eq!(
            serde_json::to_value(&commands[0]).unwrap()["destructive"],
            false
        );
        let (commands, first) = recommendations(&[target("retail", 0, &[], 0)], false, false);
        assert!(commands.is_empty() && first.is_none());
    }

    #[test]
    fn an_outdated_widget_is_recommended_after_the_checks_and_counted_once() {
        let mut retail = target("retail", 1, &[Attention::AddonDegraded], 0);
        retail.orphans = 1;
        retail.orphan_names = vec!["Ace".into()];
        let (commands, _) = recommendations(&[retail.clone()], true, true);
        assert_eq!(
            actions(&commands),
            [
                "update",
                "check_updates",
                "reinstall_widget",
                "remove_orphans",
                "inspect"
            ]
        );
        let widget = omarchy_status::WidgetInfo {
            omarchy: true,
            installed: true,
            enabled: true,
            version: Some("0.0.0".into()),
            outdated: true,
            context_matches: Some(true),
        };
        use crate::app::issues::tests::registry_machine_issues;
        let machine = registry_machine_issues(Some(&widget), &[]);
        let s = summary(std::slice::from_ref(&retail), &machine, &commands, true);
        assert_eq!((s.issues.p3, s.widget_outdated), (1, true));
        assert_eq!(s.state, SummaryState::Updates, "a P3 never makes it urgent");
        assert!(registry_machine_issues(None, &[]).is_empty());
        // A pending journal no client can carry is the machine's, and urgent.
        let unreadable = (
            "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap(),
            Some(journal::Unrecoverable::Unreadable),
        );
        let machine = registry_machine_issues(None, &[unreadable]);
        assert_eq!(machine[0].severity, crate::app::issues::Severity::P0);
        let s = summary(&[retail], &machine, &commands, false);
        assert_eq!(s.state, SummaryState::Attention);
    }

    #[test]
    fn orphans_are_recommended_after_the_update_check_and_before_attention() {
        let mut retail = target("retail", 1, &[Attention::AddonDegraded], 0);
        retail.orphans = 2;
        retail.orphan_names = vec!["Ace".into(), "BigWigs".into()];
        let (commands, _) = recommendations(&[retail], true, false);
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
        assert!(recommendations(&[none], false, false).0.is_empty());
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
        let (commands, first) = recommendations(&[retail], false, false);
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

    /// A `status` outcome of `targets` and the machine's own issues, nothing else:
    /// no warning, recommendation, drift line, last change or widget.
    fn outcome_for(targets: Vec<StatusTarget>, machine: Vec<Issue>) -> StatusOutcome {
        let summary = summary(&targets, &machine, &[], false);
        let needs_attention =
            !machine.is_empty() || targets.iter().any(|t| !t.attention.is_empty());
        StatusOutcome {
            data: StatusData {
                cached: false,
                targets,
                journal: JournalState {
                    pending: 0,
                    in_progress: false,
                },
                needs_attention,
                summary,
                last_change: None,
                last_change_state: LastChangeState::Ok,
                widget: None,
                issues: machine,
            },
            warnings: Vec::new(),
            commands: Vec::new(),
            check: false,
            first_fix: None,
            drift_lines: Vec::new(),
            discovery: CacheStats::default(),
            notes: Vec::new(),
        }
    }

    /// The human lines `status` prints for issues (E1 part 1), pinned before the move onto
    /// the failure-mode registry: each target's issues come from `issues_of_case`, so this
    /// snapshot moves with the adapter.
    #[test]
    fn status_issue_lines_are_pinned() {
        use crate::app::issues::tests::{
            issues_of_case, machine_issues_of_case, machine_named, named,
        };
        let target_of = |id: &str, case: &str| {
            let mut case = named(case);
            case.target.id = id.to_owned();
            let mut t = case.target.clone();
            t.issues = issues_of_case(&case);
            t
        };
        let targets = vec![
            target_of("classic", "degraded_one"),
            target_of("gone", "vanished_with_journal"),
            target_of("retail", "everything"),
        ];
        let machine = machine_issues_of_case(&machine_named("machine_both"));
        let outcome = outcome_for(targets, machine);
        let mut out = Vec::new();
        outcome.render_human(&mut out, &Style::default()).unwrap();
        let text = String::from_utf8(out).unwrap();
        insta::assert_snapshot!("status_issue_lines", text);
    }
}
