//! `omaddons backup [list] [ID] [--limit N]`: each client's `WTF/` backups, newest
//! capture first, with what each proves, the apparent space they take and the space
//! free beside them. Bare `backup` is `backup list`. It reads `backup.json` files and
//! checks each manifest exists, never a tree (M3-D47); its only write is the backup
//! inventory projection, a derived cache it rebuilds when stale.
//!
//! `omaddons backup create [-t T] [--verify]` copies each selected client's `WTF/`
//! without the global lock (M3-D23), then takes the lock only to register the client
//! and append the backup's history line (M3-D33). Without `-t` it backs up every live
//! client that has a `WTF/` (M3-D43), one backup each, all under one run.

use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::outcome::{ItemError, TargetRef};
use super::world::World;
use crate::backup::create::{self as capture, CreateRequest, Created};
use crate::backup::fs::CloneHint;
use crate::backup::inventory::{self, AutoLast, Capability, Inventory, Ring};
use crate::backup::{
    self, BackupKind, BackupMeta, BackupRef, Baseline, Consistency, Integrity, Reflinked, Trigger,
};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::LockedRegistry;
use crate::discover::running::{RunningScan, detect_running};
use crate::engine::journal::{no_saved_variables, recover_under};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{ClonePolicy, GlobalLock, ScopeTable, WriteSession, Writer, ensure_dir_all};
use crate::limits::{BACKUP_STALE_AFTER, BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES};
use crate::model::{Actor, BackupId, FolderName, RunId, Target, TxId, TxKind};
use crate::output::{Outcome, Style, table};
use crate::privacy::WtfPathRedactor;
use crate::rules::TargetKind;
use crate::state::config::AutoBackup;
use crate::state::doorbell;
use crate::state::history::{self, BackupNote, HistoryEntry, TxResult};
use crate::state::paths::{ClientPaths, Paths};

/// `backup`'s actions.
#[derive(Debug, Clone, clap::Args)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub action: Option<BackupAction>,
}

/// What `backup` does.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum BackupAction {
    /// List each client's WTF/ backups, newest first (changes nothing)
    #[command(after_long_help = LIST_HELP)]
    List(BackupListArgs),
    /// Back up WTF/: every live client's, or the clients -t names
    #[command(after_long_help = CREATE_HELP)]
    Create(BackupCreateArgs),
}

impl BackupAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::List(_) => "backup list",
            Self::Create(_) => "backup create",
        }
    }
}

const CREATE_HELP: &str = "\
Examples:
  omaddons backup create                 every live client that has a WTF/
  omaddons backup create -t retail --reason \"before the raid\"
  omaddons backup create -t era --verify also hash every file it copied
  omaddons backup create --dry-run       how much it would copy, and the space free

A backup is a copy of WTF/ in Interface/.omaddons/backups/, made without stopping
other changes. On btrfs and xfs it shares its data with WTF/ until either changes.
Stable means WTF/ was observed not to change while it was copied (not a filesystem
snapshot); best effort means it changed even on a second try.";

/// `backup create`'s flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct BackupCreateArgs {
    /// Hash every file copied, so the backup records its content (integrity: verified)
    #[arg(long)]
    pub verify: bool,
}

const LIST_HELP: &str = "\
Examples:
  omaddons backup list
  omaddons backup list -t retail --limit 5
  omaddons backup list 01M6Q2VA -v      one backup, with the addon versions it holds

Sizes are apparent: on btrfs and xfs a backup initially shares its data with the
live WTF/, so the free space beside them says what is really left.";

/// `backup list`'s flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct BackupListArgs {
    /// Only this backup: its id, or a prefix of 8 or more characters
    pub id: Option<String>,
    /// Show at most N backups per client, newest first
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// `backup list`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupListData {
    /// Each client with backups (or each selected one), by id.
    pub targets: Vec<BackupTarget>,
}

/// One client's backups.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupTarget {
    pub target: TargetRef,
    pub auto: AutoView,
    /// Newest capture first.
    pub backups: Vec<BackupItem>,
    /// The backups' apparent size: on a copy-on-write filesystem they initially share
    /// their data with the live tree.
    pub bytes: u64,
    /// The apparent size of undo's `WTF.previous` trees.
    pub snapshot_bytes: u64,
    /// Free space for unprivileged users on the filesystem holding the backups.
    pub free_bytes: u64,
    pub shown: u64,
    pub total: u64,
}

/// Automatic backups on one client: the configured mode and what attempts observed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AutoView {
    pub mode: AutoBackup,
    pub capability: Capability,
    pub last: Option<AutoLast>,
}

/// One backup. The fields its metadata gives are `null` when `backup.json` cannot be
/// read (it is then not restorable).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupItem {
    pub id: BackupId,
    #[schemars(with = "Option<String>")]
    pub created_at: Option<Timestamp>,
    #[schemars(with = "Option<String>")]
    pub captured_at: Option<Timestamp>,
    pub kind: Option<BackupKind>,
    pub trigger: Option<Trigger>,
    pub actor: Option<Actor>,
    pub reason: Option<String>,
    pub files: Option<u64>,
    pub bytes: Option<u64>,
    pub reflinked: Option<Reflinked>,
    pub client_running: Option<bool>,
    pub unsettled: Option<u64>,
    pub consistency: Option<Consistency>,
    pub integrity: Option<Integrity>,
    #[schemars(with = "Option<String>")]
    pub verified_at: Option<Timestamp>,
    pub baseline: Option<Baseline>,
    pub restorable: bool,
    /// It counts as a checkpoint (M3-D31).
    pub qualifying: bool,
    /// A restore, export or verify held it when listed.
    pub busy: bool,
    /// With `-v`: the managed addons when it was captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addons: Option<Vec<AddonView>>,
}

/// An addon a backup's capture recorded.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AddonView {
    pub name: String,
    pub folders: Vec<FolderName>,
    pub version: Option<String>,
}

/// `backup list`'s outcome.
#[derive(Debug)]
pub struct BackupListOutcome {
    data: BackupListData,
    warnings: Vec<Warning>,
    /// `backup create -t <id>` for each listed client with SavedVariables and no
    /// qualifying backup captured in the last [`BACKUP_STALE_AFTER`].
    commands: Vec<CommandHint>,
    /// Each target's label for human output: `retail (12.1.5, mainline)`.
    labels: Vec<String>,
}

impl Outcome for BackupListOutcome {
    type Data = BackupListData;

    fn command(&self) -> &'static str {
        "backup list"
    }

    fn data(&self) -> &BackupListData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(w, "No backups.");
        }
        let now = crate::clock::now();
        for (target, label) in self.data.targets.iter().zip(&self.labels) {
            let mut header = vec![style.bold(label)];
            header.push(match target.total {
                0 => "no backups".to_owned(),
                1 => "1 backup".to_owned(),
                n => format!("{n} backups"),
            });
            if target.total > 0 {
                let shared = target
                    .backups
                    .iter()
                    // ubs:ignore — comparing how a copy was made, not secrets
                    .any(|b| b.reflinked.is_some_and(|r| r != Reflinked::None));
                header.push(if shared {
                    format!(
                        "{} apparent, initially shared copy-on-write",
                        size(target.bytes)
                    )
                } else {
                    format!("{} apparent", size(target.bytes))
                });
            }
            header.push(format!("{} free", size(target.free_bytes)));
            writeln!(w, "{}", header.join(" · "))?;
            let rows: Vec<Vec<String>> = target.backups.iter().map(|b| row(b, now)).collect();
            for line in table(&rows).lines() {
                writeln!(w, "  {line}")?;
            }
            if target.shown < target.total {
                writeln!(
                    w,
                    "  … {} older (--limit shows more)",
                    target.total - target.shown
                )?;
            }
            for item in &target.backups {
                let Some(addons) = &item.addons else {
                    continue;
                };
                let names: Vec<String> = addons
                    .iter()
                    .map(|a| match &a.version {
                        Some(v) => format!("{} {v}", a.name),
                        None => a.name.clone(),
                    })
                    .collect();
                if !names.is_empty() {
                    let id = super::history::short_id(&item.id.to_string());
                    writeln!(w, "  {id} addons: {}", names.join(", "))?;
                }
            }
        }
        Ok(())
    }
}

/// One backup's human line.
fn row(b: &BackupItem, now: Timestamp) -> Vec<String> {
    let id = super::history::short_id(&b.id.to_string());
    let Some(kind) = b.kind else {
        return vec![id, "unreadable: backup.json cannot be read".to_owned()];
    };
    let when = b.captured_at.map_or_else(String::new, |at| when(at, now));
    let kind = match (kind, b.trigger) {
        (BackupKind::Auto, Some(Trigger::PreChange)) => "auto (before change)",
        (BackupKind::Auto, Some(Trigger::PostSession)) => "auto (after play)",
        (BackupKind::Auto, None) => "auto",
        (BackupKind::Manual, _) => "manual",
        (BackupKind::BeforeRestore, _) => "before restore",
        (BackupKind::Imported, _) => "imported",
        (BackupKind::Recovered, _) => "recovered",
    };
    let files = b.files.map_or_else(String::new, |n| match n {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    });
    let mut state = vec![match b.consistency {
        Some(Consistency::Stable) => "stable",
        _ => "best effort",
    }];
    match b.integrity {
        Some(Integrity::Verified) => state.push("verified"),
        Some(Integrity::Damaged) => state.push("damaged"),
        _ => {}
    }
    // ubs:ignore — comparing integrity states, not secrets
    if !b.restorable && b.integrity != Some(Integrity::Damaged) {
        state.push("not restorable");
    }
    if b.busy {
        state.push("in use");
    }
    let mut row = vec![
        id,
        when,
        kind.to_owned(),
        files,
        b.bytes.map_or_else(String::new, size),
        state.join(" · "),
    ];
    if let Some(reason) = &b.reason {
        row.push(format!("{reason:?}"));
    }
    row
}

/// `today 18:02`, else `2026-10-18 21:15`, in the local time zone.
fn when(at: Timestamp, now: Timestamp) -> String {
    when_in(at, now, &jiff::tz::TimeZone::system())
}

fn when_in(at: Timestamp, now: Timestamp, tz: &jiff::tz::TimeZone) -> String {
    let (local, today) = (at.to_zoned(tz.clone()), now.to_zoned(tz.clone()));
    if local.date() == today.date() {
        format!("today {}", local.strftime("%H:%M"))
    } else {
        local.strftime("%Y-%m-%d %H:%M").to_string()
    }
}

/// `15.2 MB`, `61.0 MB`, `212 GB`: decimal units, as disks are sold, one decimal below
/// 100.
fn size(bytes: u64) -> String {
    let units = [(1e12, "TB"), (1e9, "GB"), (1e6, "MB"), (1e3, "kB")];
    let value = bytes as f64;
    for (scale, unit) in units {
        if value >= scale {
            let n = value / scale;
            return if n >= 100.0 {
                format!("{n:.0} {unit}")
            } else {
                format!("{n:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

/// Runs `backup list` (and bare `backup`).
pub fn list(ctx: &Context, args: &BackupListArgs) -> Result<BackupListOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let mode = world.config.auto_backup.unwrap_or(AutoBackup::Off);
    let candidates: Vec<&DiscoveredTarget> = match ctx.target {
        Some(_) => world.select_many(ctx)?,
        None => world
            .resolved
            .targets
            .iter()
            .filter(|d| ClientPaths::new(&d.target.client_dir).backups().is_dir())
            .collect(),
    };
    let verbose = ctx.output.style.verbose > 0;
    let mut listed: Vec<(&DiscoveredTarget, Vec<BackupRef>)> = Vec::new();
    for d in candidates {
        let (backups, found) = backup::list(&ClientPaths::new(&d.target.client_dir));
        warnings.extend(found);
        listed.push((d, backups));
    }
    listed.sort_by(|a, b| a.0.target.id.cmp(&b.0.target.id));
    let mut show = None;
    if let Some(prefix) = &args.id {
        let (client, id) = only(&listed, prefix, ctx.target.as_deref())?;
        // ubs:ignore — comparing target ids, not secrets
        listed.retain(|(d, _)| d.target.id == client);
        show = Some(id);
    }
    let mut targets = Vec::new();
    let mut labels = Vec::new();
    let mut commands = Vec::new();
    let ring = Ring {
        command: ctx.command,
        actor: ctx.actor,
    };
    for (d, backups) in listed {
        labels.push(format!(
            "{} ({}, {})",
            d.target.id,
            d.target.version,
            family_name(d.target.family)
        ));
        let view = View {
            mode,
            limit: args.limit,
            show,
            verbose,
        };
        targets.push(target(&world.paths, d, &backups, &view, ring));
        if stale(d, &backups, crate::clock::now()) {
            commands.push(CommandHint::new(
                "backup_create",
                ["omaddons", "backup", "create", "-t", d.target.id.as_str()],
                false,
            ));
        }
    }
    Ok(BackupListOutcome {
        data: BackupListData { targets },
        warnings,
        commands,
        labels,
    })
}

/// Whether `d`'s `WTF/` holds SavedVariables and none of `backups` is a qualifying one
/// captured in the last [`BACKUP_STALE_AFTER`] before `now`.
fn stale(d: &DiscoveredTarget, backups: &[BackupRef], now: Timestamp) -> bool {
    let window = i64::try_from(BACKUP_STALE_AFTER.as_secs()).unwrap_or(i64::MAX);
    let fresh = |b: &BackupRef| {
        b.qualifying
            && b.captured_at()
                .is_some_and(|at| now.as_second().saturating_sub(at.as_second()) < window)
    };
    !backups.iter().any(fresh)
        && backup::has_saved_variables(&ClientPaths::new(&d.target.client_dir).wtf())
}

/// The client and id of the one backup in `listed` whose id starts with `prefix`.
fn only(
    listed: &[(&DiscoveredTarget, Vec<BackupRef>)],
    prefix: &str,
    selector: Option<&str>,
) -> Result<(String, BackupId)> {
    let list_fix = || match selector {
        Some(t) => Suggestion::command(["omaddons", "backup", "list", "-t", t]),
        None => Suggestion::command(["omaddons", "backup", "list"]),
    };
    backup::check_prefix(prefix, list_fix())?;
    let mut found: Vec<(&DiscoveredTarget, BackupRef)> = Vec::new();
    for (d, backups) in listed {
        for b in backup::matching(backups, prefix) {
            found.push((*d, b.clone()));
        }
    }
    match found.as_slice() {
        [(d, b)] => Ok((d.target.id.clone(), b.id)),
        [] => {
            let mut newest: Vec<BackupRef> = listed.iter().flat_map(|(_, b)| b.clone()).collect();
            newest.sort_by_key(|b| std::cmp::Reverse(b.captured_at()));
            Err(backup::not_found(prefix, &newest, selector))
        }
        _ => Err(backup::ambiguous(
            prefix,
            found
                .iter()
                .map(|(d, b)| (b.id, d.target.id.as_str()))
                .collect(),
        )),
    }
}

/// What a listing shows.
struct View {
    mode: AutoBackup,
    limit: Option<usize>,
    /// Only this backup (`backup list <id>`); the totals still count every one.
    show: Option<BackupId>,
    verbose: bool,
}

/// One target's listing; `snapshot_bytes` and `auto` from the validated inventory
/// projection (rebuilt when stale, which rings the doorbell for `ring`).
fn target(
    paths: &Paths,
    d: &DiscoveredTarget,
    backups: &[BackupRef],
    view: &View,
    ring: Ring<'_>,
) -> BackupTarget {
    let client = ClientPaths::new(&d.target.client_dir);
    let projection =
        d.target.key.map(
            |key| match inventory::load_validated(paths, &client, &key) {
                Inventory::Fresh(inventory) => inventory,
                Inventory::Stale(..) | Inventory::Missing(_) => {
                    inventory::rebuild(paths, &client, &key, ring).inventory
                }
            },
        );
    let total = backups.len() as u64;
    let bytes = backups
        .iter()
        .filter_map(|b| b.meta.as_ref().map(|m| m.bytes))
        .sum();
    let shown: Vec<BackupItem> = backups
        .iter()
        // ubs:ignore — comparing backup ids, not secrets
        .filter(|b| view.show.is_none_or(|id| id == b.id))
        .take(view.limit.unwrap_or(usize::MAX))
        .map(|b| item(b, view.verbose))
        .collect();
    let auto = projection
        .as_ref()
        .map(|p| p.auto.clone())
        .unwrap_or_default();
    BackupTarget {
        target: TargetRef {
            id: d.target.id.clone(),
            key: d.target.key,
            mode: None,
        },
        auto: AutoView {
            mode: view.mode,
            capability: auto.capability,
            last: auto.last,
        },
        shown: shown.len() as u64,
        backups: shown,
        bytes,
        snapshot_bytes: projection.map_or(0, |p| p.snapshot_bytes),
        free_bytes: backup::fs::free_bytes(&client.backups()),
        total,
    }
}

fn item(b: &BackupRef, verbose: bool) -> BackupItem {
    let m = b.meta.as_ref();
    let addons = verbose.then(|| {
        b.addons()
            .map(|file| {
                file.addons
                    .into_iter()
                    .map(|a| AddonView {
                        name: a.name,
                        folders: a.folders,
                        version: a.version,
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    BackupItem {
        id: b.id,
        created_at: m.map(|m| m.created_at),
        captured_at: m.map(|m| m.captured_at),
        kind: m.map(|m| m.kind),
        trigger: m.and_then(|m| m.trigger),
        actor: m.map(|m| m.actor),
        reason: m.and_then(|m| m.reason.clone()),
        files: m.map(|m| m.files),
        bytes: m.map(|m| m.bytes),
        reflinked: m.map(|m| m.reflinked),
        client_running: m.map(|m| m.client_running),
        unsettled: m.map(|m| m.unsettled),
        consistency: m.map(|m| m.consistency),
        integrity: m.map(|m| m.integrity),
        verified_at: m.and_then(|m| m.verified_at),
        baseline: m.and_then(|m| m.baseline),
        restorable: b.restorable,
        qualifying: b.qualifying,
        busy: b.busy,
        addons,
    }
}

fn family_name(family: crate::rules::Family) -> String {
    serde_json::to_value(family)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// `backup create`.

/// `backup create`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupCreateData {
    pub dry_run: bool,
    /// The run every backup's history line shares; `null` under `--dry-run`.
    pub run: Option<RunId>,
    /// One per selected client, by id.
    pub targets: Vec<BackupCreateTarget>,
}

/// One client's backup.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupCreateTarget {
    pub target: TargetRef,
    /// The backup published, as `backup list` shows it.
    pub backup: Option<BackupItem>,
    /// Its history line; `null` when that could not be written (`history_not_recorded`).
    pub tx: Option<TxId>,
    /// Why no backup was made although nothing failed; `null` for a manual backup.
    pub skipped: Option<BackupSkip>,
    pub error: Option<ItemError>,
    /// `--dry-run`: what it would copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub would: Option<WouldCopy>,
}

/// Why an automatic backup made none.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupSkip {
    pub reason: String,
}

/// What a backup would copy, from a stat walk of `WTF/`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WouldCopy {
    pub files: u64,
    /// The files' apparent size.
    pub bytes: u64,
    /// Whether the filesystem holding the backups clones files (a hint: whether a clone
    /// works is seen when it is tried).
    pub copy: CopyHint,
    /// Free space for unprivileged users beside the backups.
    pub free_bytes: u64,
}

/// How a backup's files would be copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CopyHint {
    /// Cloned: the copy shares its data with `WTF/` until either changes.
    CopyOnWrite,
    ByteCopy,
}

/// `backup create`'s outcome.
#[derive(Debug)]
pub struct BackupCreateOutcome {
    data: BackupCreateData,
    warnings: Vec<Warning>,
    partial: bool,
}

impl Outcome for BackupCreateOutcome {
    type Data = BackupCreateData;

    fn command(&self) -> &'static str {
        "backup create"
    }

    fn data(&self) -> &BackupCreateData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        for t in &self.data.targets {
            let id = &t.target.id;
            if let Some(would) = &t.would {
                let copy = match would.copy {
                    CopyHint::CopyOnWrite => "copy-on-write",
                    CopyHint::ByteCopy => "byte copy",
                };
                writeln!(
                    w,
                    "{id}: would back up {} ({}), {copy} · {} free",
                    files(would.files),
                    size(would.bytes),
                    size(would.free_bytes)
                )?;
            } else if let Some(b) = &t.backup {
                let mut how = vec![files(b.files.unwrap_or(0)), size(b.bytes.unwrap_or(0))];
                how.push(
                    match b.reflinked {
                        Some(Reflinked::All) => "copy-on-write",
                        Some(Reflinked::Some) => "partly copy-on-write",
                        _ => "byte copy",
                    }
                    .to_owned(),
                );
                how.push(
                    match b.consistency {
                        Some(Consistency::Stable) => "stable",
                        _ => "best effort",
                    }
                    .to_owned(),
                );
                // ubs:ignore — comparing integrity states, not secrets
                if b.integrity == Some(Integrity::Verified) {
                    how.push("verified".to_owned());
                }
                writeln!(
                    w,
                    "{id}: backed up WTF/ as {} ({})",
                    super::history::short_id(&b.id.to_string()),
                    how.join(", ")
                )?;
            } else if let Some(error) = &t.error {
                writeln!(w, "{id}: not backed up: {}", error.message)?;
            }
        }
        Ok(())
    }
}

/// `412 files`, `1 file`.
fn files(n: u64) -> String {
    crate::output::count(usize::try_from(n).unwrap_or(usize::MAX), "file")
}

const CREATE_COMMAND: &str = "backup create";

/// Runs `backup create`.
pub fn create(ctx: &Context, args: &BackupCreateArgs) -> Result<BackupCreateOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let selected = create_selection(ctx, &world, args)?;
    let limits = world.config.effective_limits().unwrap_or_default();
    let running = if ctx.dry_run {
        RunningScan::default()
    } else {
        let targets: Vec<Target> = selected.iter().map(|d| d.target.clone()).collect();
        detect_running(&targets)
    };
    warnings.extend(running.warning());
    let run = (!ctx.dry_run).then(RunId::new);
    let mut targets = Vec::new();
    let mut first_error = None;
    let mut succeeded = false;
    for d in selected {
        let target = || TargetRef {
            id: d.target.id.clone(),
            key: d.target.key,
            mode: None,
        };
        let one = One {
            ctx,
            world: &world,
            d,
            verify: args.verify,
            min_free: limits.backup_min_free,
        };
        let done = match run {
            None => one.dry_run().map(|(would, found)| {
                warnings.extend(found);
                BackupCreateTarget {
                    target: target(),
                    backup: None,
                    tx: None,
                    skipped: None,
                    error: None,
                    would: Some(would),
                }
            }),
            Some(run) => one.create(run, &running).map(|done| {
                warnings.extend(done.warnings);
                BackupCreateTarget {
                    target: TargetRef {
                        key: Some(done.created.meta.target),
                        id: done.created.meta.target_id.clone(),
                        mode: None,
                    },
                    backup: Some(published(&done.created)),
                    tx: done.tx,
                    skipped: None,
                    error: None,
                    would: None,
                }
            }),
        };
        match done {
            Ok(item) => {
                succeeded = true;
                targets.push(item);
            }
            Err(error) => {
                let mut warning = Warning::item(&error);
                warning.message = format!("{}: {}", d.target.id, error.message);
                warnings.push(warning);
                targets.push(BackupCreateTarget {
                    target: target(),
                    backup: None,
                    tx: None,
                    skipped: None,
                    error: Some(ItemError::from(&error)),
                    would: None,
                });
                first_error.get_or_insert(error);
            }
        }
    }
    if !succeeded && let Some(error) = first_error {
        return Err(error);
    }
    Ok(BackupCreateOutcome {
        partial: first_error.is_some(),
        data: BackupCreateData {
            dry_run: ctx.dry_run,
            run,
            targets,
        },
        warnings,
    })
}

/// The clients `backup create` backs up (M3-D43): those `-t` names, else every live
/// client that has a `WTF/`, by id. Without `-t`, none is `target_not_found` (only test
/// clients have one: one fix each, with `-t`) or `not_found` (no client has one).
fn create_selection<'w>(
    ctx: &Context,
    world: &'w World,
    args: &BackupCreateArgs,
) -> Result<Vec<&'w DiscoveredTarget>> {
    if ctx.target.is_some() {
        return world.select_many(ctx);
    }
    let has_wtf = |d: &&DiscoveredTarget| ClientPaths::new(&d.target.client_dir).wtf().is_dir();
    let mut with_wtf: Vec<&DiscoveredTarget> =
        world.resolved.targets.iter().filter(has_wtf).collect();
    with_wtf.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let live: Vec<&DiscoveredTarget> = with_wtf
        .iter()
        .copied()
        // ubs:ignore — comparing target kinds, not secrets
        .filter(|d| d.target.kind == TargetKind::Live)
        .collect();
    if !live.is_empty() {
        return Ok(live);
    }
    if world.resolved.targets.is_empty() {
        return Err(Error::new(
            ErrorCode::TargetNotFound,
            "no WoW client was found on this machine",
            Suggestion::text("omaddons targets add <path>"),
        ));
    }
    let argv = |id: &str| {
        let mut argv = vec!["omaddons", "backup", "create", "-t", id];
        if args.verify {
            argv.push("--verify");
        }
        Suggestion::command(argv)
    };
    let Some((first, rest)) = with_wtf.split_first() else {
        return Err(Error::new(
            ErrorCode::NotFound,
            "no client has a WTF/ to back up yet: none has been launched",
            Suggestion::text("launch a client once; it creates WTF/"),
        ));
    };
    let ids: Vec<&str> = with_wtf.iter().map(|d| d.target.id.as_str()).collect();
    let mut error = Error::new(
        ErrorCode::TargetNotFound,
        format!(
            "only test clients have a WTF/ ({}); name one with -t",
            ids.join(", ")
        ),
        argv(&first.target.id),
    )
    .details(serde_json::json!({ "candidates": ids }));
    for d in rest {
        error = error.alternative(argv(&d.target.id));
    }
    Err(error)
}

/// One client's backup, made or planned.
struct One<'a> {
    ctx: &'a Context,
    world: &'a World,
    d: &'a DiscoveredTarget,
    verify: bool,
    /// `limits.backup_min_free_mb` in bytes; 0 turns the warning off.
    min_free: u64,
}

/// A backup made, its history line (when written) and what it warned.
struct Done {
    created: Created,
    tx: Option<TxId>,
    warnings: Vec<Warning>,
}

impl One<'_> {
    fn client(&self) -> ClientPaths {
        ClientPaths::new(&self.d.target.client_dir)
    }

    /// The free space beside the client's backups, and `backup_space_low` when it is
    /// below the configured minimum. A manual backup goes ahead anyway: it was asked for
    /// (M3-D48).
    fn space(&self) -> (u64, Option<Warning>) {
        let free = backup::fs::free_bytes(&self.client().backups());
        let low = (self.min_free > 0 && free < self.min_free).then(|| {
            Warning::new(
                WarningCode::BackupSpaceLow,
                format!(
                    "{}: only {} is free beside its backups, below limits.backup_min_free_mb ({} MiB)",
                    self.d.target.id,
                    size(free),
                    self.min_free >> 20
                ),
            )
            .with_fix(Suggestion::text(
                "free some space, or lower limits.backup_min_free_mb in config.toml",
            ))
        });
        (free, low)
    }

    /// `--dry-run`: what a backup would copy, from a read-only stat walk. Writes nothing.
    fn dry_run(&self) -> Result<(WouldCopy, Vec<Warning>)> {
        let client = self.client();
        let source = capture::locate(&client, &self.d.target.id)?;
        let walked =
            backup::tree::walk(&source.root, BACKUP_TREE_MAX_ENTRIES, BACKUP_TREE_MAX_DEPTH)
                .map_err(|e| e.error(&WtfPathRedactor::for_client(&client)))?;
        let files: Vec<&backup::ManifestEntry> = walked
            .entries
            .iter()
            // ubs:ignore — comparing entry kinds, not secrets
            .filter(|e| e.kind == backup::EntryKind::File)
            .collect();
        let (free_bytes, low) = self.space();
        let copy = match backup::fs::clone_hint(&client.meta_dir()) {
            CloneHint::Capable => CopyHint::CopyOnWrite,
            CloneHint::Incapable => CopyHint::ByteCopy,
        };
        let would = WouldCopy {
            files: files.len() as u64,
            bytes: files.iter().map(|e| e.size).sum(),
            copy,
            free_bytes,
        };
        Ok((would, low.into_iter().collect()))
    }

    /// Registers the client if needed, copies its `WTF/` (no lock held), then appends
    /// the backup's history line under the lock and rings the doorbell.
    fn create(&self, run: RunId, running: &RunningScan) -> Result<Done> {
        let client = self.client();
        capture::locate(&client, &self.d.target.id)?;
        let (_, low) = self.space();
        let scopes = ScopeTable::new(
            &self.world.paths,
            std::slice::from_ref(&self.d.target.client_dir),
            None,
        )?;
        let target = self.registered(&scopes)?;
        let client_running = running.unavailable.is_some()
            || running.clients.contains_key(&self.d.target.client_dir);
        let req = CreateRequest {
            target: &target,
            kind: BackupKind::Manual,
            trigger: None,
            actor: self.ctx.actor,
            reason: self.ctx.reason.clone(),
            // A manual backup byte-copies where it cannot clone.
            policy: ClonePolicy::Preferred,
            verify: self.verify,
            client_running,
            command: CREATE_COMMAND,
        };
        let created = capture::create(&self.world.paths, &scopes, &req, &self.ctx.progress)
            .map_err(capture::CreateError::into_error)?;
        let mut warnings: Vec<Warning> = low.into_iter().collect();
        warnings.extend(created.warnings.iter().cloned());
        let tx = match self.record(&scopes, &created, run) {
            Ok(()) => Some(created.tx),
            Err(error) => {
                warnings.push(not_recorded(&self.world.paths, &target.id, &error));
                None
            }
        };
        doorbell::ring(&self.world.paths, CREATE_COMMAND, self.ctx.actor);
        Ok(Done {
            created,
            tx,
            warnings,
        })
    }

    /// The client as registered: under the global lock (recovering first, as every
    /// change does) when it was not yet, so its backup can name its key.
    fn registered(&self, scopes: &ScopeTable) -> Result<Target> {
        let paths = &self.world.paths;
        let mut target = self.d.target.clone();
        if target.key.is_some() {
            return Ok(target);
        }
        let guard = GlobalLock::acquire(paths, CREATE_COMMAND)?;
        self.ctx.notes.warn_all(&recover_under(
            &guard,
            paths,
            &no_saved_variables,
            CREATE_COMMAND,
            self.ctx.actor,
        ));
        let mut s = WriteSession::untracked(Writer::Backup, scopes, "registry");
        let mut registry = LockedRegistry::load(&guard, paths)?;
        let key = registry.ensure_registered(&mut s, self.d)?;
        if let Some(entry) = registry.registry().entry(&key) {
            target.id.clone_from(&entry.id);
        }
        target.key = Some(key);
        Ok(target)
    }

    /// Appends `created`'s history line (M3-D33) under the global lock, recovering first.
    fn record(&self, scopes: &ScopeTable, created: &Created, run: RunId) -> Result<()> {
        let paths = &self.world.paths;
        let meta = &created.meta;
        let entry = HistoryEntry {
            schema_version: history::CURRENT,
            tx: created.tx,
            kind: TxKind::Backup,
            at: crate::clock::now(),
            target: meta.target,
            actor: self.ctx.actor,
            reason: self.ctx.reason.clone(),
            argv: self.ctx.argv.clone(),
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: Some(run),
            backup: Some(BackupNote {
                id: meta.id,
                kind: meta.kind,
                files: meta.files,
                bytes: meta.bytes,
            }),
        };
        let guard = GlobalLock::acquire(paths, CREATE_COMMAND)?;
        self.ctx.notes.warn_all(&recover_under(
            &guard,
            paths,
            &no_saved_variables,
            CREATE_COMMAND,
            self.ctx.actor,
        ));
        let mut s = WriteSession::untracked(Writer::Backup, scopes, "history");
        ensure_dir_all(&mut s, paths.state_dir())?;
        history::append(&mut s, paths, &entry)?;
        drop(guard);
        Ok(())
    }
}

/// `history_not_recorded`: the backup stands, without its history line.
fn not_recorded(paths: &Paths, target_id: &str, error: &Error) -> Warning {
    Warning::new(
        WarningCode::HistoryNotRecorded,
        format!(
            "{target_id}: the backup was made, but its history line could not be written: {}",
            error.message
        ),
    )
    .with_fix(Suggestion::text(format!(
        "check that {} is writable and no other omaddons command is stuck",
        paths.state_dir().display()
    )))
}

/// A published backup as `backup list` shows it.
fn published(created: &Created) -> BackupItem {
    let meta: &BackupMeta = &created.meta;
    let b = BackupRef {
        id: meta.id,
        meta: Some(meta.clone()),
        dir: created.dir.clone(),
        // ubs:ignore — comparing integrity states, not secrets
        restorable: meta.integrity != Integrity::Damaged && !meta.wtf_outside_client,
        qualifying: backup::qualifying(meta),
        busy: false,
    };
    item(&b, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_disks_are_sold() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(15_937_536), "15.9 MB");
        assert_eq!(size(63_963_136), "64.0 MB");
        assert_eq!(size(227_633_266_688), "228 GB");
        assert_eq!(size(4_500), "4.5 kB");
    }

    #[test]
    fn captures_from_today_say_so() {
        let now: Timestamp = "2026-10-21T18:30:00Z".parse().unwrap();
        let today: Timestamp = "2026-10-21T18:02:00Z".parse().unwrap();
        let earlier: Timestamp = "2026-10-18T21:15:00Z".parse().unwrap();
        let utc = jiff::tz::TimeZone::UTC;
        assert_eq!(when_in(today, now, &utc), "today 18:02");
        assert_eq!(when_in(earlier, now, &utc), "2026-10-18 21:15");
        // 23:59 on the 21st in Tokyo, while it is already the 22nd there.
        let late: Timestamp = "2026-10-21T14:59:00Z".parse().unwrap();
        assert_eq!(when_in(late, now, &utc), "today 14:59");
        let tokyo = jiff::tz::TimeZone::get("Asia/Tokyo").unwrap();
        assert_eq!(
            when_in(late, now, &tokyo),
            "2026-10-21 23:59",
            "local dates, not UTC's"
        );
        assert_eq!(when_in(today, now, &tokyo), "today 03:02");
    }
}
