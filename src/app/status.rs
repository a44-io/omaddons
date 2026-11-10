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
//!
//! Issues (F8, M4-D29): the doctor's own detector loop over every `cached` row
//! (`--cached`) or every `cached` and `quick` row, on a view seeded with what `status`
//! read itself (its world, journals, lock, widget and drift), so nothing is discovered
//! or checked twice and `status` and `doctor` never disagree.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Serialize;

use super::backups_status::{self, BackupSummary, UndoSnapshots};
use super::drift::{DriftItem, needs_reinstall, reinstall_hint};
use super::errors::CoverageState;
use super::errors_status::{self, TargetErrors};
use super::history::ChangeCounts;
use super::issues::{self, Issue, JOURNAL_ROWS, SeverityCounts, ids};
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
use crate::doctor::registry::Tier;
use crate::doctor::selection::{Selection, Skipped};
use crate::doctor::view::{DoctorView, Journals, Seed, ViewDiscovery, ViewOptions};
use crate::drift::{AddonDrift, DriftMode, DriftReport, DriftState};
use crate::engine::journal;
use crate::engine::retention::{self, RetentionLimits};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::errors::observe::{Observe, read_target};
use crate::errors::{ack, memo};
use crate::fsx::{LockState, probe_global};
use crate::inventory::{Counts, Depth, FolderClass, Inventory, inventory_shared};
use crate::limits::{DOCTOR_SCAN_MAX_ENTRIES, ERRORS_MAX_TOTAL_BYTES};
use crate::model::{Actor, AddonState, FolderName, Ref, RunId, Target, TargetKey, TxKind};
use crate::omarchy::status::{self as omarchy_status, WidgetInfo};
use crate::output::{Outcome, Style, table};
use crate::politeness::DEFAULT_CHECK_INTERVAL;
use crate::resolve::catalog::Catalog;
use crate::resolve::updates::{StatusKind, UncheckedReason, UpdateStatus};
use crate::rules::{ClientVersion, Family, InstallKind, ModeName, RulesTable, TargetKind};
use crate::sources::github_token::{GithubToken, TokenSource};
use crate::state::history::{self, HistoryEntry, HistoryFilter, HistoryRead, Reach};
use crate::state::history_tail::{self, Answer};
use crate::state::paths::ClientPaths;
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
    /// Every target at a glance: the bar's badge and urgency (M3-D37).
    pub summary: Summary,
    /// What bare `undo` would revert now; `null` when nothing is undoable, or, with
    /// `last_change_state: "unknown"`, when `--cached` cannot tell (never "nothing").
    pub last_change: Option<LastChange>,
    pub last_change_state: LastChangeState,
    /// The bar widget (E4): installed, on the bar, from this build; `null` without
    /// Omarchy's config directory.
    pub widget: Option<WidgetInfo>,
    /// The GitHub token, as `auth github --offline` sees it.
    pub github: GithubState,
    /// Issues of no client: the machine's own (today, an outdated bar widget's).
    pub issues: Vec<Issue>,
}

/// The GitHub token as `auth github --offline` sees it (the widget's settings sheet,
/// omad-r0to): whether one is configured and where it comes from, resolved with no
/// request and no credential command run (a `command` credential counts as configured
/// unrun), never the token itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GithubState {
    pub configured: bool,
    pub source: TokenSource,
}

impl GithubState {
    /// The token `paths` and `sources` configure, by `GithubToken::resolve`'s order.
    fn of(
        paths: &crate::state::paths::Paths,
        sources: &BTreeMap<String, crate::state::config::SourceConfig>,
    ) -> Self {
        let token = GithubToken::resolve(paths, sources);
        Self {
            configured: token.configured(),
            source: token.source(),
        }
    }
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
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    pub actor: Actor,
    #[schemars(schema_with = "crate::contract::schema::reason_text")]
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
    /// The name a player knows the client by ("Classic Era", "Wrath 3.3.5a private
    /// server"): `targets.<id>.name` from config.toml, else omaddons' rule; unique in the
    /// envelope (a shared one adds the id). `-t` still takes the id.
    pub display_name: String,
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
    /// `status --check` stays a health check. (`list`'s and `info`'s `any_flavor` is the
    /// state of one addon.)
    pub any_flavor_addons: Vec<String>,
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
    /// Its undo snapshots, as retention counts them, sized under `-v`; `null` under
    /// `--cached` (which reads no history), when the history cannot be read, and for a
    /// client whose directory is gone.
    pub undo_snapshots: Option<UndoSnapshots>,
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
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
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

impl StatusOutcome {
    /// The human lines of the machine's issues (`data.issues`), in order, but the bar
    /// widget's, which has its own `bar widget:` line.
    fn machine_issue_lines(&self) -> Vec<String> {
        self.data
            .issues
            .iter()
            .filter(|issue| issue.id != ids::WIDGET_VERSION_DRIFT)
            .map(issue_line)
            .collect()
    }
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
            for line in self.machine_issue_lines() {
                writeln!(w, "{line}")?;
            }
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
                // Addons and folders both named: a folder count alone reads as addons.
                let mut counts = format!(
                    "{} · {} unmanaged · {}",
                    crate::output::addons_in_folders(t.addons.total, f.managed),
                    f.unmanaged,
                    crate::output::count(f.protected as usize, "protected folder")
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
                writeln!(w, "{}", issue_line(issue))?;
            }
            if let Some(line) = errors_status::human_line(&t.id, &t.errors) {
                writeln!(w, "{line}")?;
            }
            if let Some(backups) = &t.backups {
                let undo = t
                    .undo_snapshots
                    .as_ref()
                    .and_then(|u| backups_status::undo_phrase(u, Timestamp::now()))
                    .unwrap_or_default();
                writeln!(w, "{}{undo}", backups_status::human_line(backups, now))?;
            }
            if !t.any_flavor_addons.is_empty() {
                writeln!(
                    w,
                    "  any flavor: {} (builds not made for this client, by --any-flavor)",
                    t.any_flavor_addons.join(", ")
                )?;
            }
        }
        // The issues of no client, after every client's block.
        for line in self.machine_issue_lines() {
            writeln!(w, "{line}")?;
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
    // Shared with the doctor's view below, which reads it as its Discovery layer.
    let world = Rc::new(World::load_with(ctx, mode)?);
    let verbose = ctx.output.style.detail();
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
    let lock = probe_global(&world.paths)?;
    // What the journal and lock rows read (F1), seeded into the doctor's view: every
    // client's journals diagnosed, a settings restore's game judged by the selected
    // clients' running state (an unselected client's counts as possibly running).
    let mut client_dirs: Vec<PathBuf> = world
        .resolved
        .targets
        .iter()
        .map(|d| d.target.client_dir.clone())
        .chain(world.resolved.missing.iter().map(|e| e.client_dir.clone()))
        .collect();
    client_dirs.sort();
    client_dirs.dedup();
    let sv_writable = |dir: &Path| {
        running.unavailable.is_none()
            && found.iter().any(|t| t.client_dir == dir)
            && !running.clients.contains_key(dir)
    };
    let journals = Journals::read(
        &world.paths,
        &client_dirs,
        &sv_writable,
        DOCTOR_SCAN_MAX_ENTRIES,
    )?;
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
    // Plain `status` reads the whole history once: for each client's undo snapshots, the
    // last change, and the doctor's view. `--cached` reads none of it.
    let history = if args.cached {
        None
    } else {
        history::read(&world.paths, &HistoryFilter::default()).ok()
    };
    let pinned = history
        .as_ref()
        .map(|h| retention::pinned_by(&h.entries))
        .unwrap_or_default();
    let retention_limits = world.config.effective_limits().map_or_else(
        |_| RetentionLimits::default(),
        |l| RetentionLimits::from(&l),
    );
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
    // Each registered client's drift report, for the doctor's view to answer from: its
    // drift rows then check no file again, and `drift[]` and the issues agree.
    let mut drift_seed: BTreeMap<TargetKey, DriftReport> = BTreeMap::new();
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
        // `journal_pending` comes first, from the journal rows' findings (below).
        let mut attention = Vec::new();
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
        // Retention's clock: the system's, as its snapshot ages are.
        let undo_snapshots = history.as_ref().map(|h| match t.key {
            Some(key) => backups_status::undo_snapshots(
                &world.paths,
                &client,
                key,
                &h.entries,
                &pinned,
                &retention_limits,
                now,
                verbose,
            ),
            // Nothing committed on an unregistered client.
            None => UndoSnapshots {
                count: 0,
                oldest_at: None,
                prunable: 0,
                bytes: verbose.then_some(0),
                truncated: false,
            },
        });
        let modes = world.table.mode_names(t.product.as_deref());
        let target = StatusTarget {
            id: t.id.clone(),
            display_name: super::naming::display_name(&super::naming::Client {
                id: &t.id,
                family: t.family,
                kind: t.kind,
                install_kind: t.install_kind,
                product: t.product.as_deref(),
                modes: &modes,
                version: t.version,
                legacy: t.rules.legacy,
                chosen: super::naming::chosen(&world.config, &t.id, t.key.as_ref()),
            }),
            key: t.key,
            registered: t.key.is_some(),
            // ubs:ignore — a target key is a public ULID identifier, not a secret
            default: t.key.is_some() && t.key == world.registry.default,
            family: t.family,
            kind: t.kind,
            install_kind: t.install_kind,
            product: t.product.clone(),
            modes,
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
            any_flavor_addons: record
                .as_ref()
                .map(|l| any_flavor(&l.value))
                .unwrap_or_default(),
            attention,
            issues: Vec::new(),
            errors,
            backups: Some(backups),
            undo_snapshots,
            client_dir: verbose.then(|| t.client_dir.clone()),
            install_root: verbose.then(|| t.install_root.clone()),
            exe_name: if verbose { t.exe_name.clone() } else { None },
        };
        if let (Some(key), Some(report)) = (target.key, report) {
            drift_seed.insert(key, report);
        }
        targets.push(target);
    }
    for e in &missing {
        let record = load_record(&world.paths, &e.key)?.value;
        let attention = vec![Attention::TargetMissing];
        let install_kind = if e.flavor_dir.is_some() {
            InstallKind::Battlenet
        } else {
            InstallKind::Custom
        };
        let modes = world.table.mode_names(e.product.as_deref());
        let target = StatusTarget {
            id: e.id.clone(),
            // A missing client's family is unknown: its product's mode names it, else
            // its id does.
            display_name: super::naming::display_name(&super::naming::Client {
                id: &e.id,
                family: Family::Unknown,
                kind: TargetKind::Live,
                install_kind,
                product: e.product.as_deref(),
                modes: &modes,
                version: ClientVersion::new(0, 0, 0),
                legacy: false,
                chosen: super::naming::chosen(&world.config, &e.id, Some(&e.key)),
            }),
            key: Some(e.key),
            registered: true,
            // ubs:ignore — a target key is a public ULID identifier, not a secret
            default: world.registry.default == Some(e.key),
            family: Family::Unknown,
            kind: TargetKind::Live,
            install_kind,
            product: e.product.clone(),
            modes,
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
            any_flavor_addons: any_flavor(&record),
            attention,
            issues: Vec::new(),
            // Nothing to read: the directory is gone.
            errors: TargetErrors::uncaptured(CoverageState::Complete),
            backups: None,
            undo_snapshots: None,
            client_dir: verbose.then(|| e.client_dir.clone()),
            install_root: verbose.then(|| e.install_root.clone()),
            exe_name: None,
        };
        targets.push(target);
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));
    // Display names are unique in one envelope: a shared one adds the client's id.
    let mut names: Vec<(String, String)> = targets
        .iter()
        .map(|t| (t.id.clone(), t.display_name.clone()))
        .collect();
    super::naming::disambiguate(&mut names);
    for (target, (_, name)) in targets.iter_mut().zip(names) {
        target.display_name = name;
    }

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
    // Each issue is a finding of the failure-mode registry (F8, M4-D29): the doctor's own
    // detector loop over its `cached` rows (`--cached`) or its `cached` and `quick` rows,
    // on a view seeded with what `status` read above, so nothing is discovered or checked
    // twice. `-t` narrows the per-client rows as `doctor -t` does; `-t all` lists every
    // client, a vanished one too, as no `-t` does.
    let in_progress = matches!(lock, LockState::Held { .. });
    let view = DoctorView::seeded(
        ViewOptions {
            discovery: if args.cached {
                ViewDiscovery::Cached
            } else {
                ViewDiscovery::Scan
            },
            ..ViewOptions::default()
        },
        Seed {
            world: Rc::clone(&world),
            journals,
            lock,
            widget: widget.clone(),
            drift: drift_seed,
            history,
        },
    );
    let scope = if world.selects_all(ctx) {
        None
    } else {
        ctx.target.as_deref()
    };
    let found = doctor::detect(
        &view,
        &Selection::parse(scope, &[], &[], &[])?,
        status_tiers(&args),
    )?;
    // A check `status` could not make is the doctor's `skipped[]`; here a `-v` note.
    notes.extend(skipped_notes(&found.skipped));
    let listed: BTreeSet<TargetKey> = targets.iter().filter_map(|t| t.key).collect();
    let (mut per_client, issues) = issues::assign(&found.findings, &listed, ctx.target.is_some());
    for t in &mut targets {
        t.issues = t
            .key
            .and_then(|key| per_client.remove(&key))
            .unwrap_or_default();
        // An interrupted change on the client: its first attention item.
        if t.issues.iter().any(|i| JOURNAL_ROWS.contains(&i.id)) {
            t.attention.insert(0, Attention::JournalPending);
        }
    }
    // An interrupted change no listed client carries is attention too.
    let unattributed = issues.iter().any(|i| JOURNAL_ROWS.contains(&i.id));

    let (mut commands, attention_fix) = recommendations(&targets, check_due, widget_outdated);
    let first_fix = check_fix(&targets, &issues, attention_fix);
    if let Some(selector) = ctx.target.as_deref() {
        scope_update(&mut commands, selector);
    }
    if args.cached && !world.paths.discovery_cache().exists() {
        commands.push(CommandHint::new(Action::Status, ["omaddons", "status"]));
    }
    let needs_attention = needs_attention(&targets, &issues, unattributed);
    let summary = summary(&targets, &issues, &commands, widget_outdated);
    let read = if args.cached { None } else { view.history() };
    let (last_change, last_change_state) = last_change(&world, args.cached, read);
    Ok(StatusOutcome {
        data: StatusData {
            cached: args.cached,
            targets,
            journal: JournalState {
                pending: pending.len(),
                in_progress,
            },
            needs_attention,
            summary,
            last_change,
            last_change_state,
            widget,
            github: GithubState::of(&world.paths, &world.config.sources),
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
fn last_change(
    world: &World,
    cached: bool,
    read: Option<&HistoryRead>,
) -> (Option<LastChange>, LastChangeState) {
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
    let Some(read) = read else {
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

/// The tiers `status` runs (M4-D29): the `cached` rows under `--cached`, else the
/// `cached` and `quick` rows. `--deep` runs the same rows: its depth reaches the issues
/// through the drift report it seeds.
pub(crate) fn status_tiers(args: &StatusArgs) -> &'static [Tier] {
    if args.cached {
        &[Tier::Cached]
    } else {
        &[Tier::Cached, Tier::Quick]
    }
}

/// Each check `status` could not make (a layer it reads could not be loaded), as a `-v`
/// note: `status: <fm id> not checked (<reason>)`, with ` on <client>` for one client.
/// Never a data key or an issue: `doctor` reports each in `skipped[]` (§8.1 rule 2).
pub(crate) fn skipped_notes(skipped: &[Skipped]) -> Vec<String> {
    skipped
        .iter()
        .map(|s| {
            let mut note = format!("status: {} not checked ({})", s.id, wire(s.reason));
            if let Some(target) = &s.target {
                note.push_str(&format!(" on {}", target.id));
            }
            note
        })
        .collect()
}

/// One issue as the human view prints it, under its client or after them all:
/// `  P2 <title> → <remediation>`. A forced repair shows its explain command first, so a
/// person reads the evidence before consenting (M4-D38): `  P0 <title> → <explain> (read
/// it before the forced repair: <remediation>)`.
fn issue_line(issue: &Issue) -> String {
    let mut line = format!("  {} {}", wire(issue.severity), issue.title);
    match &issue.remediation.command {
        Some(command) if issue.remediation.forced() => line.push_str(&format!(
            " → {} (read it before the forced repair: {command})",
            issue.explain.command
        )),
        Some(command) => line.push_str(&format!(" → {command}")),
        None => {}
    }
    line
}

/// Whether `status --check` fails (M4-D53): an attention item on a client, an interrupted
/// change no listed client carries (`unattributed`), or a P0 or P1 issue, a client's or
/// the machine's. P2 and P3 issues alone never do: `--check` stays a health check.
/// `data.needs_attention` is this value, so it holds whenever `summary.state` is
/// `attention` for an urgent issue.
fn needs_attention(targets: &[StatusTarget], machine: &[Issue], unattributed: bool) -> bool {
    unattributed
        || targets
            .iter()
            .any(|t| !t.attention.is_empty() || t.issues.iter().any(Issue::urgent))
        || machine.iter().any(Issue::urgent)
}

/// The `--check` failure's message and fix, the first that applies: the first attention
/// item's (`attention`, as [`recommendations`] gives it), then the interrupted change no
/// listed client carries ([`journal_fix`]), then the most urgent P0 or P1 issue's
/// ([`urgent_fix`]).
fn check_fix(
    targets: &[StatusTarget],
    machine: &[Issue],
    attention: Option<(String, Suggestion)>,
) -> Option<(String, Suggestion)> {
    attention
        .or_else(|| journal_fix(machine, None))
        .or_else(|| urgent_fix(targets, machine))
}

/// The most urgent P0 or P1 issue's `--check` message and fix: by severity, then the
/// clients in `status`'s order with the machine's issues last, then the issue's id. A
/// client's issue reads `<client id>: <title>`, the machine's `<title>`; its fix is
/// [`issue_fix`].
fn urgent_fix(targets: &[StatusTarget], machine: &[Issue]) -> Option<(String, Suggestion)> {
    let on_clients = targets.iter().enumerate().flat_map(|(at, t)| {
        t.issues
            .iter()
            .map(move |issue| (at, Some(t.id.as_str()), issue))
    });
    let on_machine = machine.iter().map(|issue| (targets.len(), None, issue));
    let (_, client, issue) = on_clients
        .chain(on_machine)
        .filter(|(_, _, issue)| issue.urgent())
        .min_by_key(|(at, _, issue)| (issue.severity, *at, issue.id))?;
    let message = match client {
        Some(id) => format!("{id}: {}", issue.title),
        None => issue.title.clone(),
    };
    Some((message, issue_fix(issue)))
}

/// An issue's `--check` fix: its remediation's argv, or its advice. A forced repair is
/// read before it is consented to (M4-D38): its `doctor explain`, never an argv with
/// `--force --yes` that a caller might run as is.
fn issue_fix(issue: &Issue) -> Suggestion {
    match &issue.remediation.argv {
        Some(_) if issue.remediation.forced() => Suggestion::command(issue.explain.argv.clone()),
        Some(argv) => Suggestion::command(argv.clone()),
        None => Suggestion::text(issue.remediation.command.clone().unwrap_or_default()),
    }
}

/// The `--check` fix for a scope's interrupted changes: its most blocking journal issue
/// (by severity, `journal-pending` last within one). A settleable change keeps today's
/// message and advice; a stuck one names its state, with its `doctor explain` (or its
/// advice). `target` is the client's id; `None` for the machine's.
fn journal_fix(issues: &[Issue], target: Option<&str>) -> Option<(String, Suggestion)> {
    let issue = issues
        .iter()
        .filter(|i| JOURNAL_ROWS.contains(&i.id))
        .min_by_key(|i| (i.severity, i.id == ids::JOURNAL_PENDING))?;
    if issue.id == ids::JOURNAL_PENDING
        && let Some(id) = target
    {
        return Some((
            format!("an interrupted transaction for {id} awaits recovery"),
            Suggestion::text(
                "run any omaddons command that changes addons (install, remove, undo); it recovers the transaction first",
            ),
        ));
    }
    // A stuck journal's set-aside is forced: its `doctor explain`.
    let fix = issue_fix(issue);
    let message = match target {
        Some(id) => format!("{id}: {}", issue.title),
        None => issue.title.clone(),
    };
    Some((message, fix))
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
                Attention::JournalPending => {
                    let (message, fix) = journal_fix(&t.issues, Some(&t.id)).unwrap_or_else(|| {
                        (
                            format!("an interrupted transaction for {} awaits recovery", t.id),
                            Suggestion::text(
                                "run any omaddons command that changes addons (install, remove, undo); it recovers the transaction first",
                            ),
                        )
                    });
                    (message, fix, None)
                }
                Attention::StateVersionNewer => (
                    format!("{}'s record was written by a newer omaddons", t.id),
                    Suggestion::text("upgrade omaddons"),
                    None,
                ),
                Attention::TargetMissing => {
                    let hint = forget_target_hint(&t.id);
                    (
                        format!("{}'s client is gone or no longer installed", t.id),
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
        display_name: "Retail".to_owned(),
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
        any_flavor_addons: Vec::new(),
        attention: attention.to_vec(),
        issues: Vec::new(),
        errors: TargetErrors::uncaptured(CoverageState::Complete),
        backups: None,
        undo_snapshots: None,
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
    /// no warning, recommendation, drift line, last change or widget. `needs_attention`
    /// and the `--check` fix are `run`'s own rules over them.
    fn outcome_for(targets: Vec<StatusTarget>, machine: Vec<Issue>) -> StatusOutcome {
        let summary = summary(&targets, &machine, &[], false);
        let unattributed = machine.iter().any(|i| JOURNAL_ROWS.contains(&i.id));
        let needs_attention = needs_attention(&targets, &machine, unattributed);
        let first_fix = check_fix(
            &targets,
            &machine,
            recommendations(&targets, false, false).1,
        );
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
                github: GithubState {
                    configured: false,
                    source: TokenSource::None,
                },
                issues: machine,
            },
            warnings: Vec::new(),
            commands: Vec::new(),
            check: false,
            first_fix,
            drift_lines: Vec::new(),
            discovery: CacheStats::default(),
            notes: Vec::new(),
        }
    }

    /// `omaddons doctor explain <id> [-t <target>]`, as the doctor builds it.
    fn explain_of(id: &str, target: Option<&str>) -> issues::ExplainCommand {
        let mut argv: Vec<String> = ["omaddons", "doctor", "explain", id]
            .map(str::to_owned)
            .to_vec();
        if let Some(target) = target {
            argv.extend(["-t".to_owned(), target.to_owned()]);
        }
        issues::ExplainCommand {
            command: argv.join(" "),
            argv,
        }
    }

    /// An issue of `id` remediated by `argv`, or by advice when it has none; on retail.
    fn issue_with(
        id: &'static str,
        severity: issues::Severity,
        title: &str,
        argv: Option<&[&str]>,
    ) -> Issue {
        Issue {
            id,
            severity,
            title: title.to_owned(),
            addon: None,
            remediation: issues::Remediation {
                command: Some(argv.map_or_else(|| "advice".to_owned(), |a| a.join(" "))),
                argv: argv.map(|a| a.iter().map(|w| (*w).to_owned()).collect()),
                destructive: false,
            },
            auto_fixable: false,
            explain: explain_of(id, Some("retail")),
        }
    }

    /// The human lines `status` prints for issues (E1 part 1), pinned before the move onto
    /// `--check`'s journal fix takes the most blocking state: a stuck one names itself and
    /// gives its `doctor explain`; a settleable one keeps today's text byte for byte; the
    /// game-waiting one gives its advice; the machine's names its title.
    #[test]
    fn check_fix_prefers_the_blocking_state() {
        let issue = issue_with;
        let pending = issue(
            ids::JOURNAL_PENDING,
            issues::Severity::P0,
            "An interrupted change waits to be finished",
            None,
        );
        let too_new_argv = [
            "omaddons",
            "doctor",
            "explain",
            ids::JOURNAL_TOO_NEW,
            "-t",
            "retail",
        ];
        let too_new = issue(
            ids::JOURNAL_TOO_NEW,
            issues::Severity::P0,
            "An interrupted change was started by a newer omaddons",
            Some(&too_new_argv),
        );
        let waits = issue(
            ids::JOURNAL_WAITS_FOR_GAME,
            issues::Severity::P2,
            "An interrupted change to WTF/ waits for the game to exit",
            None,
        );
        let (message, fix) =
            journal_fix(&[pending.clone(), too_new.clone()], Some("retail")).unwrap();
        assert_eq!(
            message,
            "retail: An interrupted change was started by a newer omaddons"
        );
        assert_eq!(fix.argv().unwrap(), too_new_argv);
        let (message, fix) = journal_fix(std::slice::from_ref(&pending), Some("retail")).unwrap();
        assert_eq!(
            message,
            "an interrupted transaction for retail awaits recovery"
        );
        assert_eq!(
            (fix.display(), fix.argv()),
            (
                "run any omaddons command that changes addons (install, remove, undo); it recovers the transaction first",
                None
            )
        );
        let (message, fix) = journal_fix(&[waits], Some("retail")).unwrap();
        assert_eq!(
            message,
            "retail: An interrupted change to WTF/ waits for the game to exit"
        );
        assert_eq!((fix.display(), fix.argv()), ("advice", None));
        let unattributed_argv = ["omaddons", "doctor", "explain", ids::JOURNAL_UNATTRIBUTED];
        let unattributed = issue(
            ids::JOURNAL_UNATTRIBUTED,
            issues::Severity::P0,
            "An interrupted change cannot be attributed to any client, so every change is refused",
            Some(&unattributed_argv),
        );
        let (message, fix) = journal_fix(&[unattributed], None).unwrap();
        assert_eq!(
            message,
            "An interrupted change cannot be attributed to any client, so every change is refused"
        );
        assert_eq!(fix.argv().unwrap(), unattributed_argv);
        assert!(journal_fix(&[], Some("retail")).is_none());
    }

    #[test]
    fn status_tiers_by_flags() {
        let args = |cached, deep| StatusArgs {
            cached,
            check: false,
            deep,
        };
        for (case, flags, tiers) in [
            ("cached", args(true, false), &[Tier::Cached][..]),
            (
                "plain",
                args(false, false),
                &[Tier::Cached, Tier::Quick][..],
            ),
            ("deep", args(false, true), &[Tier::Cached, Tier::Quick][..]),
        ] {
            assert_eq!(status_tiers(&flags), tiers, "{case}: {flags:?}");
        }
    }

    /// The machine's issues print after every client's block, the widget's not doubled
    /// (it has its `bar widget:` line); with no client, after the "No WoW client" line.
    #[test]
    fn machine_issues_print_after_the_clients() {
        let credential = issue_with(
            ids::CREDENTIAL_FILE_MODE,
            issues::Severity::P2,
            "the GitHub token file is readable by other users",
            Some(&[
                "omaddons",
                "doctor",
                "fix",
                "--only",
                ids::CREDENTIAL_FILE_MODE,
            ]),
        );
        let drift = issue_with(
            ids::WIDGET_VERSION_DRIFT,
            issues::Severity::P3,
            "The bar widget is from another omaddons build",
            Some(&["omaddons", "omarchy", "install"]),
        );
        let line = "  P2 the GitHub token file is readable by other users → omaddons doctor fix --only fm-secrets-credential-file-mode";
        let render = |outcome: StatusOutcome| {
            let mut out = Vec::new();
            outcome.render_human(&mut out, &Style::default()).unwrap();
            String::from_utf8(out).unwrap()
        };
        let mut retail = target("retail", 0, &[Attention::AddonDegraded], 0);
        retail.issues = vec![Issue {
            title: "1 addon lost a folder to another addon".to_owned(),
            id: ids::OWNERSHIP_TRANSFERRED,
            ..credential.clone()
        }];
        let text = render(outcome_for(
            vec![retail],
            vec![credential.clone(), drift.clone()],
        ));
        let lines: Vec<&str> = text.lines().collect();
        let client = lines
            .iter()
            .position(|l| l.contains("lost a folder"))
            .unwrap();
        // ubs:ignore — compares a rendered human line in a test, not a secret
        let machine = lines.iter().position(|l| *l == line).unwrap();
        assert!(client < machine, "with_clients: {text}");
        assert!(
            !text.contains("The bar widget is from another omaddons build"),
            "with_clients: the widget's issue keeps its own line: {text}"
        );

        let text = render(outcome_for(Vec::new(), vec![credential, drift]));
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("No WoW client found"),
            "no_client: {text}"
        );
        assert_eq!(lines[1], line, "no_client: {text}");
    }

    /// A forced repair's line shows its explain command first and the forced command after
    /// it; an auto-fixable and a manual one show their remediation. The machine's lines
    /// follow the same rule.
    #[test]
    fn a_forced_remediation_prints_the_explain_command_first() {
        let forced_argv = [
            "omaddons",
            "doctor",
            "fix",
            "--only",
            ids::JOURNAL_TOO_NEW,
            "-t",
            "retail",
            "--force",
            "--yes",
        ];
        let forced = issue_with(
            ids::JOURNAL_TOO_NEW,
            issues::Severity::P0,
            "An interrupted change was started by a newer omaddons",
            Some(&forced_argv),
        );
        let fixable = Issue {
            auto_fixable: true,
            ..issue_with(
                ids::CREDENTIAL_FILE_MODE,
                issues::Severity::P2,
                "The GitHub token file is readable by other users",
                Some(&[
                    "omaddons",
                    "doctor",
                    "fix",
                    "--only",
                    ids::CREDENTIAL_FILE_MODE,
                ]),
            )
        };
        let manual = issue_with(
            ids::RECORD_FOLDER_MISSING,
            issues::Severity::P1,
            "Details's folder is gone",
            Some(&[
                "omaddons",
                "install",
                "Details",
                "-t",
                "retail",
                "--reinstall",
            ]),
        );
        // `--force` after `--` is an argument, never the flag.
        let argument = issue_with(
            ids::RECORD_FOLDER_MISSING,
            issues::Severity::P1,
            "--force's folder is gone",
            Some(&["omaddons", "install", "-t", "retail", "--", "--force"]),
        );
        for (case, issue, line) in [
            (
                "forced",
                &forced,
                "  P0 An interrupted change was started by a newer omaddons → omaddons doctor explain fm-concurrency_primitives-journal-too-new -t retail (read it before the forced repair: omaddons doctor fix --only fm-concurrency_primitives-journal-too-new -t retail --force --yes)",
            ),
            (
                "auto_fixable",
                &fixable,
                "  P2 The GitHub token file is readable by other users → omaddons doctor fix --only fm-secrets-credential-file-mode",
            ),
            (
                "manual",
                &manual,
                "  P1 Details's folder is gone → omaddons install Details -t retail --reinstall",
            ),
            (
                "force_as_an_argument",
                &argument,
                "  P1 --force's folder is gone → omaddons install -t retail -- --force",
            ),
        ] {
            assert_eq!(issue_line(issue), line, "{case}: {issue:?}");
        }
        let outcome = outcome_for(Vec::new(), vec![forced.clone()]);
        assert_eq!(
            outcome.machine_issue_lines(),
            [issue_line(&forced)],
            "machine"
        );
    }

    // ---- `--check` on the registry's issues (F8 part 3, M4-D53).

    /// The `--check` outcome of `targets` and the machine's issues.
    fn checked(targets: Vec<StatusTarget>, machine: Vec<Issue>) -> StatusOutcome {
        StatusOutcome {
            check: true,
            ..outcome_for(targets, machine)
        }
    }

    /// An issue of no client: its explain command has no `-t`.
    fn machine_issue(
        id: &'static str,
        severity: issues::Severity,
        title: &str,
        argv: Option<&[&str]>,
    ) -> Issue {
        Issue {
            explain: explain_of(id, None),
            ..issue_with(id, severity, title, argv)
        }
    }

    /// `id` on `attention`, carrying `issues`.
    fn client_with(id: &str, attention: &[Attention], issues: Vec<Issue>) -> StatusTarget {
        let mut t = target(id, 0, attention, 0);
        t.issues = issues;
        t
    }

    /// A crashed check: P1, about the machine, with advice.
    fn crashed() -> Issue {
        machine_issue(
            ids::DETECTOR_CRASHED,
            issues::Severity::P1,
            "The check fm-x stopped with an internal error",
            None,
        )
    }

    /// What a `--check` failure says: its message, its fix's display and its argv.
    type Failure = (String, String, Option<Vec<String>>);

    fn failure_of(outcome: &StatusOutcome) -> Option<Failure> {
        outcome.assertion_failure().map(|error| {
            assert_eq!(error.code, ErrorCode::CheckFailed);
            (
                error.message.clone(),
                error.fix.display().to_owned(),
                error.fix.argv().map(<[String]>::to_vec),
            )
        })
    }

    /// (case, targets, the machine's issues, the failure, or `None` for a pass)
    type CheckCase = (&'static str, Vec<StatusTarget>, Vec<Issue>, Option<Failure>);

    fn check_cases() -> Vec<CheckCase> {
        use issues::Severity::{P0, P2, P3};
        let words = |argv: &[&str]| Some(argv.iter().map(|w| (*w).to_owned()).collect());
        let fails = |message: &str, display: &str, argv: Option<Vec<String>>| {
            Some((message.to_owned(), display.to_owned(), argv))
        };
        let newer = issue_with(
            ids::STATE_VERSION_NEWER,
            P0,
            "State was written by a newer omaddons",
            None,
        );
        let quiet = vec![
            machine_issue(
                ids::CREDENTIAL_FILE_MODE,
                P2,
                "The GitHub token file is readable by other users",
                Some(&[
                    "omaddons",
                    "doctor",
                    "fix",
                    "--only",
                    ids::CREDENTIAL_FILE_MODE,
                ]),
            ),
            machine_issue(
                ids::SKILL_VERSION_DRIFT,
                P3,
                "The agent skill is from another omaddons build",
                Some(&["omaddons", "robot-docs", "install-skill"]),
            ),
        ];
        let unattributed_argv = [
            "omaddons",
            "doctor",
            "fix",
            "--only",
            ids::JOURNAL_UNATTRIBUTED,
            "--force",
            "--yes",
        ];
        let unattributed = machine_issue(
            ids::JOURNAL_UNATTRIBUTED,
            P0,
            "An interrupted change cannot be attributed to any client, so every change is refused",
            Some(&unattributed_argv),
        );
        let retail_rm = ["omaddons", "targets", "rm", "retail"];
        vec![
            (
                "nothing",
                vec![client_with("retail", &[], vec![])],
                vec![],
                None,
            ),
            (
                "p2_attention_only",
                vec![client_with("retail", &[Attention::TargetMissing], vec![])],
                vec![],
                fails(
                    "retail's client is gone or no longer installed",
                    "omaddons targets rm retail",
                    words(&retail_rm),
                ),
            ),
            (
                "p0_issue_without_attention",
                vec![client_with("retail", &[], vec![newer.clone()])],
                vec![],
                fails(
                    "retail: State was written by a newer omaddons",
                    "advice",
                    None,
                ),
            ),
            (
                "p1_machine_issue",
                vec![client_with("retail", &[], vec![])],
                vec![crashed()],
                fails(
                    "The check fm-x stopped with an internal error",
                    "advice",
                    None,
                ),
            ),
            (
                "p2_and_p3_issues_only",
                vec![client_with(
                    "retail",
                    &[],
                    vec![issue_with(
                        ids::OWNERSHIP_TRANSFERRED,
                        P2,
                        "1 addon lost a folder to another addon",
                        Some(&["omaddons", "list", "-t", "retail", "-v"]),
                    )],
                )],
                quiet.clone(),
                None,
            ),
            (
                "attention_and_p0_issue",
                vec![client_with("retail", &[Attention::TargetMissing], vec![])],
                vec![Issue {
                    explain: explain_of(ids::STATE_VERSION_NEWER, None),
                    ..newer
                }],
                fails(
                    "retail's client is gone or no longer installed",
                    "omaddons targets rm retail",
                    words(&retail_rm),
                ),
            ),
            (
                "unattributed_and_p1_issue",
                vec![client_with("retail", &[], vec![])],
                vec![unattributed, crashed()],
                fails(
                    "An interrupted change cannot be attributed to any client, so every change is refused",
                    &format!("omaddons doctor explain {}", ids::JOURNAL_UNATTRIBUTED),
                    words(&["omaddons", "doctor", "explain", ids::JOURNAL_UNATTRIBUTED]),
                ),
            ),
            (
                "detector_crashed",
                vec![client_with("retail", &[], vec![])],
                [quiet, vec![crashed()]].concat(),
                fails(
                    "The check fm-x stopped with an internal error",
                    "advice",
                    None,
                ),
            ),
        ]
    }

    /// `--check` fails on every attention reason as before, and also on any P0 or P1
    /// issue, a client's or the machine's; P2 and P3 issues alone pass. An attention item's
    /// fix comes first, then the unattributed journal's, then the urgent issue's.
    #[test]
    fn check_fails_on_attention_or_an_urgent_issue() {
        for (case, targets, machine, expected) in check_cases() {
            let outcome = checked(targets, machine);
            let got = failure_of(&outcome);
            assert_eq!(
                got, expected,
                "{case}: needs_attention {}, targets {:?}, machine {:?}",
                outcome.data.needs_attention, outcome.data.targets, outcome.data.issues
            );
        }
    }

    /// `data.needs_attention` is the `--check` rule itself.
    #[test]
    fn needs_attention_equals_the_check_rule() {
        for (case, targets, machine, expected) in check_cases() {
            let outcome = checked(targets, machine);
            assert_eq!(
                outcome.data.needs_attention,
                outcome.assertion_failure().is_some(),
                "{case}: needs_attention {}, expected {}; error {:?}",
                outcome.data.needs_attention,
                expected.is_some(),
                outcome.assertion_failure()
            );
        }
    }

    /// The fix of the most urgent issue: by severity, then client order with the
    /// machine's last, then id; its argv as a command, its advice as text, a forced
    /// repair's explain command.
    #[test]
    fn the_first_fix_of_an_issue() {
        use issues::Severity::{P0, P1};
        let reinstall = [
            "omaddons",
            "install",
            "Details",
            "-t",
            "retail",
            "--reinstall",
        ];
        let missing = |client: &str| {
            issue_with(
                ids::RECORD_FOLDER_MISSING,
                P1,
                "Details's folder is gone",
                Some(&[
                    "omaddons",
                    "install",
                    "Details",
                    "-t",
                    client,
                    "--reinstall",
                ]),
            )
        };
        let newer_machine = machine_issue(
            ids::STATE_VERSION_NEWER,
            P0,
            "The target registry was written by a newer omaddons",
            None,
        );
        let forced_argv = [
            "omaddons",
            "doctor",
            "fix",
            "--only",
            ids::JOURNAL_TOO_NEW,
            "-t",
            "retail",
            "--force",
            "--yes",
        ];
        let forced = issue_with(
            ids::JOURNAL_TOO_NEW,
            P0,
            "An interrupted change was started by a newer omaddons",
            Some(&forced_argv),
        );
        let words = |argv: &[&str]| argv.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        // (case, targets, the machine's issues, the message, the fix's argv)
        type FixCase = (
            &'static str,
            Vec<StatusTarget>,
            Vec<Issue>,
            &'static str,
            Option<Vec<String>>,
        );
        let cases: Vec<FixCase> = vec![
            (
                "client_argv",
                vec![client_with("retail", &[], vec![missing("retail")])],
                vec![],
                "retail: Details's folder is gone",
                Some(words(&reinstall)),
            ),
            (
                "client_prose",
                vec![client_with(
                    "retail",
                    &[],
                    vec![issue_with(
                        ids::STATE_VERSION_NEWER,
                        P0,
                        "State was written by a newer omaddons",
                        None,
                    )],
                )],
                vec![],
                "retail: State was written by a newer omaddons",
                None,
            ),
            (
                "machine_title_only",
                vec![],
                vec![newer_machine.clone()],
                "The target registry was written by a newer omaddons",
                None,
            ),
            (
                "severity_then_client_then_id",
                vec![
                    client_with("classic", &[], vec![missing("classic")]),
                    client_with("retail", &[], vec![missing("retail")]),
                ],
                vec![newer_machine],
                "The target registry was written by a newer omaddons",
                None,
            ),
            (
                "client_before_client_before_machine",
                vec![
                    client_with("classic", &[], vec![missing("classic")]),
                    client_with("retail", &[], vec![missing("retail")]),
                ],
                vec![crashed()],
                "classic: Details's folder is gone",
                Some(words(&[
                    "omaddons",
                    "install",
                    "Details",
                    "-t",
                    "classic",
                    "--reinstall",
                ])),
            ),
            (
                "forced_is_explained_first",
                vec![client_with("retail", &[], vec![forced])],
                vec![],
                "retail: An interrupted change was started by a newer omaddons",
                Some(words(&[
                    "omaddons",
                    "doctor",
                    "explain",
                    ids::JOURNAL_TOO_NEW,
                    "-t",
                    "retail",
                ])),
            ),
        ];
        for (case, targets, machine, message, argv) in cases {
            let got = urgent_fix(&targets, &machine);
            let (got_message, fix) = got.clone().unwrap_or_else(|| panic!("{case}: no fix"));
            assert_eq!(
                (got_message.as_str(), fix.argv().map(<[String]>::to_vec)),
                (message, argv.clone()),
                "{case}: {targets:?} {machine:?} gave {got:?}"
            );
            if argv.is_none() {
                assert_eq!(fix.display(), "advice", "{case}: prose as text");
            }
        }
        assert!(
            urgent_fix(&[client_with("retail", &[], vec![])], &[]).is_none(),
            "no urgent issue, no fix"
        );
    }

    /// `summary.issues` counts every issue `status` lists, once: a client's journals of
    /// one row are one issue, and a finding on a client `status` does not list is counted
    /// in the machine's list only.
    #[test]
    fn summary_counts_every_listed_issue_once() {
        use crate::doctor::finding::{Confidence, Evidence, Observation, Subject};
        use crate::model::{TargetKey, TxId};
        let listed: TargetKey = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT".parse().unwrap();
        let unlisted: TargetKey = "01M7A1B2C3D4E5F6G7H8J9K0MN".parse().unwrap();
        let finding = |id: &str, subject: Subject| {
            crate::doctor::registry::row(id)
                .unwrap()
                .finding(Observation {
                    subject,
                    title: "t".to_owned(),
                    confidence: Confidence::High,
                    addon: None,
                    folder: None,
                    evidence: Evidence::default(),
                })
        };
        let journal = |n: u8| {
            let tx: TxId = format!("01M7A1B2C3D4E5F6G7H8J9K0M{}", char::from(b'0' + n))
                .parse()
                .unwrap();
            finding(
                ids::JOURNAL_PENDING,
                Subject::target("retail", listed).tx(tx),
            )
        };
        let findings = vec![
            journal(1),
            journal(2),
            journal(3),
            finding(ids::LOCK_HELD_LONG, Subject::machine()),
            finding(ids::TARGET_VANISHED, Subject::target("gone", unlisted)),
        ];
        let (mut clients, machine) = issues::assign(&findings, &BTreeSet::from([listed]), false);
        let mut retail = target("retail", 0, &[], 0);
        retail.key = Some(listed);
        retail.issues = clients.remove(&listed).unwrap_or_default();
        assert_eq!(
            retail.issues.len(),
            1,
            "one journal issue: {:?}",
            retail.issues
        );
        assert_eq!(
            machine.len(),
            2,
            "the lock and the unlisted client: {machine:?}"
        );
        let counts = summary(&[retail], &machine, &[], false).issues;
        assert_eq!(
            (counts.p0, counts.p1, counts.p2, counts.p3, counts.total()),
            (1, 0, 2, 0, 3),
            "{counts:?}"
        );
    }

    /// A check `status` could not make becomes one `-v` note, naming its client when it
    /// was skipped for one.
    #[test]
    fn skipped_rows_become_verbose_notes() {
        use crate::doctor::selection::{SkipReason, SkippedTarget};
        let skipped = [
            Skipped {
                id: ids::CREDENTIAL_FILE_MODE,
                reason: SkipReason::CredentialsUnavailable,
                target: None,
            },
            Skipped {
                id: ids::RECORD_FOLDER_MISSING,
                reason: SkipReason::RecordsUnavailable,
                target: Some(SkippedTarget {
                    id: "retail".to_owned(),
                    key: None,
                }),
            },
        ];
        assert_eq!(
            skipped_notes(&skipped),
            [
                "status: fm-secrets-credential-file-mode not checked (credentials_unavailable)",
                "status: fm-state_files-record-folder-missing not checked (records_unavailable) on retail",
            ]
        );
    }

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
