//! `omaddons status [-t T] [--cached] [--check]`: offline triage, the first call for
//! orientation (vision §5.8). Every target with its client, live running state, folder
//! classes and addon counts; unfinished transactions; what needs attention and the
//! commands that address it. Read-only: never writes, never takes the lock, no network.
//!
//! `--cached` (the widget's authority from M3) never scans: it re-stats the discovery
//! cache's roots, or without a cache parses only the registered roots and recommends a
//! full `omaddons status`. Running state is live either way. `--check` turns any
//! attention into exit 6 (`check_failed`) with the data still there.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::cache::{CacheStats, DiscoveryMode};
use crate::discover::registry::resolve_one;
use crate::discover::running::detect_running;
use crate::engine::journal;
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{LockState, probe_global};
use crate::inventory::{Counts, Depth, inventory};
use crate::model::{AddonState, Target, TargetKey};
use crate::output::{Outcome, Style, table};
use crate::rules::{ClientVersion, Family, InstallKind, ModeName, TargetKind};
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
}

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
    /// What discovery did, for the `-v` log line.
    pub discovery: CacheStats,
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
                if !t.attention.is_empty() {
                    let names: Vec<String> = t.attention.iter().map(|a| wire(*a)).collect();
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
        write!(w, "{}", table(&rows))
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

    let selected: Vec<&DiscoveredTarget> = match &ctx.target {
        Some(selector) => vec![resolve_one(
            selector,
            &world.resolved.targets,
            &world.table,
            &ctx.argv,
        )?],
        None => world.resolved.targets.iter().collect(),
    };
    let missing: Vec<&RegistryEntry> = match &ctx.target {
        Some(_) => Vec::new(),
        None => world.resolved.missing.iter().collect(),
    };
    let found: Vec<Target> = selected.iter().map(|d| d.target.clone()).collect();
    let running = detect_running(&found);
    let pending = journal::pending(&world.paths)?;
    let held = matches!(probe_global(&world.paths)?, LockState::Held { .. });

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
            attention,
            client_dir: verbose.then(|| e.client_dir.clone()),
            install_root: verbose.then(|| e.install_root.clone()),
            exe_name: None,
        });
    }
    targets.sort_by(|a, b| a.id.cmp(&b.id));

    let (mut commands, first_fix) = recommendations(&targets);
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
        discovery: world.discovery,
    })
}

/// The commands that address each target's attention items (target order, then item
/// order), then a look at unmanaged folders; and the first item's message and fix.
fn recommendations(targets: &[StatusTarget]) -> (Vec<CommandHint>, Option<(String, Suggestion)>) {
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
            };
            if first.is_none() {
                first = Some((message, fix));
            }
            commands.extend(hint);
        }
        if t.folders.unmanaged > 0 {
            commands.push(CommandHint::new(
                "list_unmanaged",
                ["omaddons", "list", "-t", t.id.as_str(), "--unmanaged"],
                false,
            ));
        }
    }
    (commands, first)
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
