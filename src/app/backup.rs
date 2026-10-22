//! `omaddons backup [list] [ID] [--limit N]`: each client's `WTF/` backups, newest
//! capture first, with what each proves, the apparent space they take and the space
//! free beside them. Bare `backup` is `backup list`. It reads `backup.json` files and
//! checks each manifest exists, never a tree (M3-D47); its only write is the backup
//! inventory projection, a derived cache it rebuilds when stale.
//!
//! `omaddons backup create [-t T] [--verify]` copies each selected client's `WTF/`
//! without the global lock (M3-D23), then takes the lock only to register the client
//! and append the backup's history line (M3-D33). Without `-t` it backs up every live
//! client that has a `WTF/` (M3-D43), one backup each, all under one run. Each backup is
//! followed by the retention pass on its client's backups (M3-D32).
//!
//! `omaddons backup prune [ID…] [-t T] [--keep N] [--older-than D]` runs that pass on
//! demand, reaching the user's own backups with `--keep` or `--older-than`, or deletes
//! exactly the backups named; its preview binds the real run (M3-D46).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::outcome::{ItemError, TargetRef};
use super::precondition::{
    Precondition, PrunePart, RestoreBinding, RestorePart, check, of_prune, of_restore, preview_argv,
};
use super::world::World;
use crate::backup::auto::{self, AutoResult, AutoRun, Decision};
use crate::backup::create::{self as capture, CreateRequest, Created};
use crate::backup::export::{self, VerifiedAgainst};
use crate::backup::fs::CloneHint;
use crate::backup::import;
use crate::backup::inventory::{self, AutoLast, Capability, Inventory, Ring};
use crate::backup::prune::{self, PruneOpts, PruneReport, Pruned};
use crate::backup::{
    self, BackupKind, BackupMeta, BackupRef, Baseline, Consistency, ID_PREFIX_MIN, ImportSource,
    Integrity, Reflinked, Trigger,
};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::LockedRegistry;
use crate::discover::running::{RunningScan, detect_running};
use crate::engine::Binding;
use crate::engine::journal::{no_saved_variables, recover_under};
use crate::engine::restore::{self, Exchange, RestoreFiles, RestoreRequest};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{ClonePolicy, GlobalLock, ScopeTable, WriteSession, Writer, ensure_dir_all};
use crate::limits::{
    BACKUP_STALE_AFTER, BACKUP_TREE_MAX_DEPTH, BACKUP_TREE_MAX_ENTRIES, EffectiveLimits,
};
use crate::model::{
    Actor, BackupId, FolderName, ReloadRequired, RunId, Target, TargetKey, TxId, TxKind,
};
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
    /// Restore the whole WTF/ (keybindings, macros, every addon's settings) from a
    /// backup, or with --addon one addon's settings (undoable)
    #[command(after_long_help = RESTORE_HELP)]
    Restore(BackupRestoreArgs),
    /// Delete old backups, or the ones named (it cannot be undone)
    #[command(after_long_help = PRUNE_HELP)]
    Prune(BackupPruneArgs),
    /// Write a backup to a file off this disk, as a .tar.zst (never replaces a file)
    #[command(after_long_help = EXPORT_HELP)]
    Export(BackupExportArgs),
    /// Bring back a backup that backup export wrote, as a backup of the client -t names
    #[command(after_long_help = IMPORT_HELP)]
    Import(BackupImportArgs),
}

impl BackupAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::List(_) => "backup list",
            Self::Create(_) => "backup create",
            Self::Restore(_) => RESTORE_COMMAND,
            Self::Prune(_) => PRUNE_COMMAND,
            Self::Export(_) => EXPORT_COMMAND,
            Self::Import(_) => IMPORT_COMMAND,
        }
    }
}

const EXPORT_COMMAND: &str = "backup export";
const IMPORT_COMMAND: &str = "backup import";

const IMPORT_HELP: &str = "\
Examples:
  omaddons backup import /run/media/me/usb/retail-wtf.tar.zst -t retail
  omaddons backup import retail-wtf.tar.zst -t retail --dry-run   check it, write nothing
  omaddons backup restore 01M7A3XC -t retail                      then put it back

Reads an archive backup export wrote and adds it as an imported backup of the client
-t names, verified against every hash it carries, with its original capture time. It
accepts only an exact, unmodified export: anything else (a link that leaves WTF/, a file
the manifest does not list, a damaged file) is refused before it is kept. A backup from
another kind of client is refused unless --force. An imported backup never counts as
today's automatic checkpoint, and pruning never deletes it on its own.";

/// `backup import`'s arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct BackupImportArgs {
    /// The archive backup export wrote
    #[arg(value_name = "FILE")]
    pub file: std::path::PathBuf,
    /// Import a backup from a client of another kind than this one
    #[arg(long)]
    pub force: bool,
}

const EXPORT_HELP: &str = "\
Examples:
  omaddons backup export 01M6Q2VA /run/media/me/usb/retail-wtf.tar.zst
  omaddons backup export 01M6Q2VA wtf.tar.zst --dry-run   what it would write

Writes one backup as a zstd-compressed tar: its description, its WTF/ with the modes
it recorded, and every file's SHA-256, checked against the backup's own hashes when it
has them. The same backup always exports to the same bytes. It never replaces a file,
writes nothing anywhere else, and refuses a destination inside a game client, a Wine
prefix or omaddons' own folders. The archive holds account and character names, as WTF/
does: keep it private.";

/// `backup export`'s arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct BackupExportArgs {
    /// The backup: its id, or a prefix of 8 or more characters
    #[arg(value_name = "ID")]
    pub id: String,
    /// The file to write (a new one, in a folder that exists)
    #[arg(value_name = "FILE")]
    pub file: std::path::PathBuf,
}

const RESTORE_HELP: &str = "\
Examples:
  omaddons backup restore 01M6Q2VA --dry-run   what it would put back, and its token
  omaddons backup restore 01M6Q2VA -t retail
  omaddons backup restore 01M6Q2VA -t retail --addon WeakAuras   one addon's settings
  omaddons undo                                put back what it replaced

Restores the whole WTF/ (keybindings, macros, chat and every addon's settings) as the
backup holds them; use --addon for one addon: only the SavedVariables of the folders it
had when the backup was taken, under every account and character. It refuses while the
game runs (or may be running), and where files cannot be replaced in one atomic step; it
checks the copy against the backup's hashes when it has them. Before a whole restore,
where the filesystem clones files, it keeps the current WTF/ as a backup (before
restore), and undo puts back what a restore replaced either way.
--if-match <token> restores exactly what that --dry-run showed.";

/// `backup restore`'s arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct BackupRestoreArgs {
    /// The backup: its id, or a prefix of 8 or more characters
    #[arg(value_name = "ID")]
    pub id: String,
    /// Only this addon's settings (repeatable): its name, ref or a folder; folder:<F> for
    /// one folder alone
    #[arg(long = "addon", value_name = "ADDON")]
    pub addon: Vec<String>,
}

const CREATE_HELP: &str = "\
Examples:
  omaddons backup create                 every live client that has a WTF/
  omaddons backup create -t retail --reason \"before the raid\"
  omaddons backup create -t era --verify also hash every file it copied
  omaddons backup create --dry-run       how much it would copy, and the space free
  omaddons backup create --auto -t retail   an automatic checkpoint, only when due

A backup is a copy of WTF/ in Interface/.omaddons/backups/, made without stopping
other changes. On btrfs and xfs it shares its data with WTF/ until either changes.
Stable means WTF/ was observed not to change while it was copied (not a filesystem
snapshot); best effort means it changed even on a second try. --auto follows
auto_backup: it makes none while the game runs, when WTF/ is unchanged since the last
checkpoint, or (with daily) where the filesystem cannot clone files.";

/// `backup create`'s flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct BackupCreateArgs {
    /// Hash every file copied, so the backup records its content (integrity: verified)
    #[arg(long)]
    pub verify: bool,
    /// Only when due: the automatic checkpoint after a play session, skipped (exit 0,
    /// with the reason) when WTF/ is unchanged since the last checkpoint
    #[arg(long, conflicts_with = "verify")]
    pub auto: bool,
}

const LIST_HELP: &str = "\
Examples:
  omaddons backup list
  omaddons backup list -t retail --limit 5
  omaddons backup list 01M6Q2VA -v      one backup, with the addon versions it holds

Sizes are apparent: on btrfs and xfs a backup initially shares its data with the
live WTF/, so the free space beside them says what is really left.";

const PRUNE_HELP: &str = "\
Examples:
  omaddons backup prune                      the retention pass, on every client
  omaddons backup prune -t retail --keep 3   also the newest 3 of each kind only
  omaddons backup prune --older-than 30d     also your own backups from before 30 days ago
  omaddons backup prune -t retail -- 01M6Q2VA   exactly that backup
  omaddons backup prune --dry-run            what it would delete, and its token

Retention keeps every backup younger than a day, each client's newest checkpoint, its
manual, imported and recovered backups, and for each of the newest
limits.backup_auto_keep_days days its first pre-change and last post-session automatic
backup and every before-restore one. --keep and --older-than reach your own backups
too, but never one younger than a day nor the newest checkpoint; naming a backup lifts
only the day. A backup a restore or an export is reading is skipped as busy. Pruning
cannot be undone: --if-match <token> deletes exactly what that --dry-run showed.";

/// `backup prune`'s flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct BackupPruneArgs {
    /// Only these backups: ids, or prefixes of 8 or more characters
    #[arg(value_name = "ID")]
    pub ids: Vec<String>,
    /// Also prune every backup beyond the newest N of its kind
    #[arg(long, value_name = "N", conflicts_with = "ids")]
    pub keep: Option<usize>,
    /// Also prune manual, imported and recovered backups older than D (12h, 30d, 2w, or
    /// an RFC 3339 time)
    #[arg(long, value_name = "D", conflicts_with = "ids")]
    pub older_than: Option<String>,
}

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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

/// A backup's kind as human output says it.
fn kind_label(kind: BackupKind, trigger: Option<Trigger>) -> &'static str {
    match (kind, trigger) {
        (BackupKind::Auto, Some(Trigger::PreChange)) => "auto (before change)",
        (BackupKind::Auto, Some(Trigger::PostSession)) => "auto (after play)",
        (BackupKind::Auto, None) => "auto",
        (BackupKind::Manual, _) => "manual",
        (BackupKind::BeforeRestore, _) => "before restore",
        (BackupKind::Imported, _) => "imported",
        (BackupKind::Recovered, _) => "recovered",
    }
}

/// One backup's human line.
fn row(b: &BackupItem, now: Timestamp) -> Vec<String> {
    let id = super::history::short_id(&b.id.to_string());
    let Some(kind) = b.kind else {
        return vec![id, "unreadable: backup.json cannot be read".to_owned()];
    };
    let when = b.captured_at.map_or_else(String::new, |at| when(at, now));
    let kind = kind_label(kind, b.trigger);
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
pub(super) fn when(at: Timestamp, now: Timestamp) -> String {
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
pub(super) fn size(bytes: u64) -> String {
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
    let mode = world.config.auto_backup.unwrap_or_default();
    let verbose = ctx.output.style.verbose > 0;
    let mut listed = listed(ctx, &world, &mut warnings)?;
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

/// Each client `-t` selects, else each that has a `backups/` dir, with its backups, by
/// id. The warnings are `backup.json` files of another version.
fn listed<'w>(
    ctx: &Context,
    world: &'w World,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<(&'w DiscoveredTarget, Vec<BackupRef>)>> {
    let candidates: Vec<&DiscoveredTarget> = match ctx.target {
        Some(_) => world.select_many(ctx)?,
        None => world
            .resolved
            .targets
            .iter()
            .filter(|d| ClientPaths::new(&d.target.client_dir).backups().is_dir())
            .collect(),
    };
    let mut listed: Vec<(&DiscoveredTarget, Vec<BackupRef>)> = Vec::new();
    for d in candidates {
        let (backups, found) = backup::list(&ClientPaths::new(&d.target.client_dir));
        warnings.extend(found);
        listed.push((d, backups));
    }
    listed.sort_by(|a, b| a.0.target.id.cmp(&b.0.target.id));
    Ok(listed)
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
            } else if let Some(skipped) = &t.skipped {
                writeln!(w, "{id}: no automatic backup: {}", why(&skipped.reason))?;
            }
        }
        Ok(())
    }
}

/// A skip's reason as human output says it.
fn why(reason: &str) -> &'static str {
    match reason {
        "off" => "auto_backup is off",
        "no_wtf" => "there is no WTF/ yet",
        "running" => "the game is running (it writes WTF/ when it exits)",
        "done_today" => "today's checkpoint was made",
        "unchanged" => "WTF/ is unchanged since the last checkpoint",
        "no_reflink" => "the filesystem cannot clone files (auto_backup = \"daily\")",
        "space_low" => "too little space, even after pruning old automatic backups",
        "in_progress" => "another automatic backup of this client is running",
        _ => "not due",
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
    let selected = if args.auto {
        auto_selection(ctx, &world)?
    } else {
        create_selection(ctx, &world, args)?
    };
    let limits = world.config.effective_limits().unwrap_or_default();
    // An automatic backup's decision reads it, a dry run's too.
    let running = if ctx.dry_run && !args.auto {
        RunningScan::default()
    } else {
        let targets: Vec<Target> = selected.iter().map(|d| d.target.clone()).collect();
        detect_running(&targets)
    };
    warnings.extend(running.warning());
    let mode = world.config.auto_backup.unwrap_or_default();
    let run = (!ctx.dry_run).then(RunId::new);
    let mut targets = Vec::new();
    let mut first_error = None;
    let mut succeeded = false;
    for d in selected {
        let one = One {
            ctx,
            world: &world,
            d,
            verify: args.verify,
            min_free: limits.backup_min_free,
            keep_days: limits.backup_auto_keep_days,
        };
        let done = match run {
            None if args.auto => one.auto_dry_run(&running, mode, &limits),
            None => one.dry_run().map(|(would, found)| {
                let item = BackupCreateTarget {
                    would: Some(would),
                    ..empty_item(d)
                };
                (item, found)
            }),
            Some(run) if args.auto => one.auto(run, &running, mode, &limits),
            Some(run) => one
                .create(run, &running)
                .map(|done| (made_item(&done.created, done.tx), done.warnings)),
        };
        match done {
            Ok((item, found)) => {
                warnings.extend(found);
                succeeded = true;
                targets.push(item);
            }
            Err(error) => {
                let mut warning = Warning::item(&error);
                warning.message = format!("{}: {}", d.target.id, error.message);
                warnings.push(warning);
                targets.push(BackupCreateTarget {
                    error: Some(ItemError::from(&error)),
                    ..empty_item(d)
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

/// `d`'s item before anything is said about it.
fn empty_item(d: &DiscoveredTarget) -> BackupCreateTarget {
    BackupCreateTarget {
        target: target_ref(d),
        backup: None,
        tx: None,
        skipped: None,
        error: None,
        would: None,
    }
}

/// The item of a backup `created`, its history line `tx`.
fn made_item(created: &Created, tx: Option<TxId>) -> BackupCreateTarget {
    BackupCreateTarget {
        target: TargetRef {
            key: Some(created.meta.target),
            id: created.meta.target_id.clone(),
            mode: None,
        },
        backup: Some(published(created)),
        tx,
        skipped: None,
        error: None,
        would: None,
    }
}

/// The clients `backup create --auto` considers: those `-t` names, else every live
/// client, by id (one without a `WTF/` is skipped `no_wtf`, never an error).
fn auto_selection<'w>(ctx: &Context, world: &'w World) -> Result<Vec<&'w DiscoveredTarget>> {
    if ctx.target.is_some() {
        return world.select_many(ctx);
    }
    let mut live: Vec<&DiscoveredTarget> = world
        .resolved
        .targets
        .iter()
        // ubs:ignore — comparing target kinds, not secrets
        .filter(|d| d.target.kind == TargetKind::Live)
        .collect();
    live.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    Ok(live)
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
    /// `limits.backup_auto_keep_days`, for the retention pass after the backup.
    keep_days: usize,
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
    /// the backup's history line under the lock, rings the doorbell and runs retention.
    fn create(&self, run: RunId, running: &RunningScan) -> Result<Done> {
        let client = self.client();
        capture::locate(&client, &self.d.target.id)?;
        let (_, low) = self.space();
        let scopes = ScopeTable::new(
            &self.world.paths,
            std::slice::from_ref(&self.d.target.client_dir),
            None,
        )?;
        let target = registered(self.ctx, self.world, self.d, &scopes, CREATE_COMMAND)?;
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
        warnings.extend(self.retain(&scopes, &target));
        Ok(Done {
            created,
            tx,
            warnings,
        })
    }

    /// `--auto`: the after-play checkpoint (M3-D31), made only when due; a skip is a
    /// success with its reason. A backup made gets its history line, the doorbell and
    /// retention, as one asked for does.
    fn auto(
        &self,
        run: RunId,
        running: &RunningScan,
        mode: AutoBackup,
        limits: &EffectiveLimits,
    ) -> Result<(BackupCreateTarget, Vec<Warning>)> {
        let scopes = ScopeTable::new(
            &self.world.paths,
            std::slice::from_ref(&self.d.target.client_dir),
            None,
        )?;
        let req = self.auto_run(&scopes, running, mode, limits);
        let register = || registered(self.ctx, self.world, self.d, &scopes, CREATE_COMMAND);
        let outcome = auto::run(&req, &register);
        let mut warnings = outcome.warnings;
        match outcome.result {
            AutoResult::Skipped(skip) => {
                let item = BackupCreateTarget {
                    skipped: Some(BackupSkip {
                        reason: skip.wire().to_owned(),
                    }),
                    ..empty_item(self.d)
                };
                Ok((item, warnings))
            }
            AutoResult::Failed(error) => Err(error),
            AutoResult::Created(created) => {
                let target = outcome.target.unwrap_or_else(|| self.d.target.clone());
                warnings.extend(created.warnings.iter().cloned());
                let tx = match self.record(&scopes, &created, run) {
                    Ok(()) => Some(created.tx),
                    Err(error) => {
                        warnings.push(not_recorded(&self.world.paths, &target.id, &error));
                        None
                    }
                };
                doorbell::ring(&self.world.paths, CREATE_COMMAND, self.ctx.actor);
                warnings.extend(self.retain(&scopes, &target));
                Ok((made_item(&created, tx), warnings))
            }
        }
    }

    /// `--auto --dry-run`: whether one is due, and what it would copy; writes nothing.
    fn auto_dry_run(
        &self,
        running: &RunningScan,
        mode: AutoBackup,
        limits: &EffectiveLimits,
    ) -> Result<(BackupCreateTarget, Vec<Warning>)> {
        let scopes = ScopeTable::new(
            &self.world.paths,
            std::slice::from_ref(&self.d.target.client_dir),
            None,
        )?;
        let req = self.auto_run(&scopes, running, mode, limits);
        match auto::due(&req, &self.client()) {
            Decision::Skip(skip) => {
                let item = BackupCreateTarget {
                    skipped: Some(BackupSkip {
                        reason: skip.wire().to_owned(),
                    }),
                    ..empty_item(self.d)
                };
                Ok((item, Vec::new()))
            }
            Decision::Go(_) => self.dry_run().map(|(would, found)| {
                let item = BackupCreateTarget {
                    would: Some(would),
                    ..empty_item(self.d)
                };
                (item, found)
            }),
        }
    }

    /// The after-play attempt on this client.
    fn auto_run<'s>(
        &'s self,
        scopes: &'s ScopeTable,
        running: &RunningScan,
        mode: AutoBackup,
        limits: &'s EffectiveLimits,
    ) -> AutoRun<'s> {
        AutoRun {
            paths: &self.world.paths,
            scopes,
            target: self.d,
            trigger: Trigger::PostSession,
            mode,
            limits,
            running: running.unavailable.is_some()
                || running.clients.contains_key(&self.d.target.client_dir),
            actor: self.ctx.actor,
            reason: self.ctx.reason.clone(),
            command: CREATE_COMMAND,
            progress: &self.ctx.progress,
            record_skips: true,
        }
    }

    /// The retention pass on the client's backups after a backup (M3-D32), outside the
    /// lock. A failure is only `retention_failed`: the backup stands.
    fn retain(&self, scopes: &ScopeTable, target: &Target) -> Option<Warning> {
        let ring = Ring {
            command: CREATE_COMMAND,
            actor: self.ctx.actor,
        };
        let opts = PruneOpts::default();
        let client = self.client();
        let pass = prune::retain(
            &self.world.paths,
            scopes,
            &client,
            target.key.as_ref(),
            self.keep_days,
            &opts,
            ring,
        );
        let error = pass.err()?;
        Some(
            Warning::new(
                WarningCode::RetentionFailed,
                format!(
                    "{}: pruning old backups after the backup failed: {}",
                    target.id, error.message
                ),
            )
            .with_fix(Suggestion::command([
                "omaddons",
                "backup",
                "prune",
                "-t",
                target.id.as_str(),
            ])),
        )
    }

    /// Appends `created`'s history line (M3-D33) under the global lock, recovering first.
    fn record(&self, scopes: &ScopeTable, created: &Created, run: RunId) -> Result<()> {
        record_line(
            self.ctx,
            &self.world.paths,
            scopes,
            created,
            (run, CREATE_COMMAND),
        )
    }
}

/// Appends `created`'s history line (M3-D33) under the global lock, recovering first,
/// for `command`.
fn record_line(
    ctx: &Context,
    paths: &Paths,
    scopes: &ScopeTable,
    created: &Created,
    (run, command): (RunId, &'static str),
) -> Result<()> {
    let entry = backup_line(ctx, created, run);
    let guard = GlobalLock::acquire(paths, command)?;
    ctx.notes.warn_all(&recover_under(
        &guard,
        paths,
        &no_saved_variables,
        command,
        ctx.actor,
    ));
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "history");
    ensure_dir_all(&mut s, paths.state_dir())?;
    history::append(&mut s, paths, &entry)?;
    drop(guard);
    Ok(())
}

/// `created`'s history line (M3-D33), of `run`.
fn backup_line(ctx: &Context, created: &Created, run: RunId) -> HistoryEntry {
    let meta = &created.meta;
    HistoryEntry {
        schema_version: history::CURRENT,
        tx: created.tx,
        kind: TxKind::Backup,
        at: crate::clock::now(),
        target: meta.target,
        actor: ctx.actor,
        reason: ctx.reason.clone(),
        argv: ctx.argv.clone(),
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
    }
}

/// `d`'s client as registered: under the global lock (recovering first, as every change
/// does) when it was not yet, so a backup of it can name its key.
fn registered(
    ctx: &Context,
    world: &World,
    d: &DiscoveredTarget,
    scopes: &ScopeTable,
    command: &'static str,
) -> Result<Target> {
    let paths = &world.paths;
    let mut target = d.target.clone();
    if target.key.is_some() {
        return Ok(target);
    }
    let guard = GlobalLock::acquire(paths, command)?;
    ctx.notes.warn_all(&recover_under(
        &guard,
        paths,
        &no_saved_variables,
        command,
        ctx.actor,
    ));
    let mut s = WriteSession::untracked(Writer::Backup, scopes, "registry");
    let mut registry = LockedRegistry::load(&guard, paths)?;
    let key = registry.ensure_registered(&mut s, d)?;
    if let Some(entry) = registry.registry().entry(&key) {
        target.id.clone_from(&entry.id);
    }
    target.key = Some(key);
    Ok(target)
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

// ---------------------------------------------------------------------------------------
// `backup restore`.

const RESTORE_COMMAND: &str = "backup restore";

/// What `backup restore` puts back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// The whole `WTF/`.
    Whole,
    /// Single addons' SavedVariables (`--addon`).
    Addons,
}

/// `backup restore`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupRestoreData {
    pub dry_run: bool,
    pub target: TargetRef,
    /// The restore's transaction, which `undo` reverts; `null` under `--dry-run`.
    pub tx: Option<TxId>,
    /// `restore`.
    pub kind: TxKind,
    pub mode: RestoreMode,
    pub backup: RestoreSource,
    /// A whole restore's files; `null` for `--addon`, whose files `addons` counts.
    pub files: Option<RestoreFiles>,
    /// The current `WTF/`, kept as a backup first; `null` under `--dry-run`, when none
    /// was made (`before_restore_skipped`), and for `--addon`.
    pub before_restore: Option<KeptBackup>,
    /// `restart` for a whole restore (the client reads `WTF/` when it starts); `none`
    /// for `--addon` (the next login reads the files).
    pub reload_required: ReloadRequired,
    /// `--dry-run`: the token `--if-match` takes to restore exactly this (M3-D46); `null`
    /// for a real run.
    pub precondition: Option<String>,
    /// `verified` when the exchange was probed where it runs; `unverified` under
    /// `--dry-run`, which writes nothing.
    pub exchange: Exchange,
    /// `--addon`: each addon whose settings come back; `null` for a whole restore.
    pub addons: Option<Vec<AddonRestore>>,
}

/// One addon a `--addon` restore puts settings back for.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AddonRestore {
    /// Its name, as the backup's addon record, the target's record, or the folder named.
    pub addon: String,
    /// The folders whose SavedVariables come back.
    pub folders: Vec<FolderName>,
    /// Where those folders came from: `backup` (what it owned when the backup was taken),
    /// `installed` (not in the backup's addon record: what it owns now), or `folder` (a
    /// bare folder of the backup).
    pub folders_from: FoldersFrom,
    /// The version that wrote those settings, when the backup recorded it.
    pub captured_version: Option<String>,
    /// The version installed now, when it is managed.
    pub installed_version: Option<String>,
    /// Folders it owns now that it did not when the backup was taken: left as they are.
    pub not_in_backup: Vec<FolderName>,
    pub files: crate::engine::restore_addons::AddonFiles,
}

/// Where a `--addon` restore's folders came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FoldersFrom {
    Backup,
    Installed,
    Folder,
}

/// The backup a restore puts back.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RestoreSource {
    pub id: BackupId,
    #[schemars(with = "String")]
    pub captured_at: Timestamp,
    pub kind: BackupKind,
    /// What its content is proved to be (M3-D47): `verified` only when the copy put back
    /// matched its recorded hashes (under `--dry-run`, when it has hashes a restore will
    /// check), else `metadata_only`.
    pub integrity: Integrity,
}

/// The `before_restore` backup a restore kept.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct KeptBackup {
    pub id: BackupId,
}

/// `backup restore`'s outcome.
#[derive(Debug)]
pub struct BackupRestoreOutcome {
    pub(super) data: BackupRestoreData,
    pub(super) warnings: Vec<Warning>,
    /// `undo <tx>` after a real run.
    pub(super) commands: Vec<CommandHint>,
    /// For human output: the source's trigger and hashes' baseline, and whether a dry run
    /// would keep the current `WTF/` as a backup.
    pub(super) trigger: Option<Trigger>,
    pub(super) baseline: Option<Baseline>,
    pub(super) would_keep: bool,
}

impl Outcome for BackupRestoreOutcome {
    type Data = BackupRestoreData;

    fn command(&self) -> &'static str {
        RESTORE_COMMAND
    }

    fn data(&self) -> &BackupRestoreData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let (id, b) = (&d.target.id, &d.backup);
        let f = &d.files.unwrap_or_default();
        let short = |id: &str| super::history::short_id(id);
        let from = format!(
            "backup {} ({}, {})",
            short(&b.id.to_string()),
            when(b.captured_at, crate::clock::now()),
            kind_label(b.kind, self.trigger)
        );
        if let Some(addons) = &d.addons {
            render_addons(w, d, addons, &from)?;
        } else if d.dry_run {
            writeln!(
                w,
                "{id}: would restore WTF/ from {from}: {}, {} differed, {} created since",
                files(f.restored),
                f.differed,
                f.created_since
            )?;
            if self.would_keep {
                writeln!(
                    w,
                    "  would keep the current WTF/ as a backup (before restore)"
                )?;
            }
        } else {
            writeln!(
                w,
                "{id}: restored WTF/ from {from}: {}, {} differed",
                files(f.restored),
                f.differed
            )?;
            if let Some(kept) = &d.before_restore {
                writeln!(
                    w,
                    "  kept the current WTF/ as backup {} (before restore)",
                    short(&kept.id.to_string())
                )?;
            }
        }
        writeln!(w, "  {}", proved(b.integrity, self.baseline, d.dry_run))?;
        if let Some(tx) = &d.tx {
            let tx = tx.to_string();
            writeln!(w, "tx {} · undo: omaddons undo {tx}", short(&tx))?;
        }
        if let Some(token) = &d.precondition {
            writeln!(w, "Run it bound to this preview: --if-match {token}")?;
        }
        Ok(())
    }
}

/// A `--addon` restore's lines: what comes back and from where, each addon's version note
/// and its files, by what happens to them. Addon names and counts only, never a path.
fn render_addons(
    w: &mut dyn Write,
    d: &BackupRestoreData,
    addons: &[AddonRestore],
    from: &str,
) -> std::io::Result<()> {
    let id = &d.target.id;
    let whose: Vec<String> = addons.iter().map(|a| format!("{}'s", a.addon)).collect();
    let whose = whose.join(" and ");
    let total: u64 = addons
        .iter()
        .map(|a| a.files.replaced + a.files.restored + a.files.created_since)
        .sum();
    if d.dry_run {
        writeln!(w, "{id}: would restore {whose} SavedVariables from {from}")?;
    } else {
        writeln!(
            w,
            "{id}: restored {whose} SavedVariables from {from}: {}",
            files(total)
        )?;
    }
    for a in addons {
        let name = &a.addon;
        match (&a.captured_version, &a.installed_version) {
            (Some(then), Some(now)) => {
                writeln!(
                    w,
                    "  settings written by {name} {then}; installed now: {now}"
                )?;
            }
            (Some(then), None) => {
                writeln!(w, "  settings written by {name} {then}; not installed now")?;
            }
            (None, Some(now)) => writeln!(
                w,
                "  the version of {name} that wrote them is not recorded; installed now: {now}"
            )?,
            (None, None) => {}
        }
        if matches!(a.folders_from, FoldersFrom::Installed) {
            writeln!(
                w,
                "  {name} is not in this backup's addon record; using today's folders"
            )?;
        }
        if !a.not_in_backup.is_empty() {
            let left: Vec<&str> = a.not_in_backup.iter().map(FolderName::as_str).collect();
            writeln!(
                w,
                "  not in this backup; left as it is: {}",
                left.join(", ")
            )?;
        }
        if d.dry_run {
            let n = &a.files;
            let prefix = if addons.len() > 1 {
                format!("{name}: ")
            } else {
                String::new()
            };
            writeln!(
                w,
                "  {prefix}{}: {} replaced, {} restored (absent now), {} created since (moved aside)",
                files(n.replaced + n.restored + n.created_since),
                n.replaced,
                n.restored,
                n.created_since
            )?;
        }
    }
    Ok(())
}

/// What a restore says its content is proved to be (M3-D47): never "verified" for a
/// backup whose content no hash checked.
fn proved(integrity: Integrity, baseline: Option<Baseline>, dry_run: bool) -> String {
    let hashes = match baseline {
        Some(Baseline::Import) => "import-time hashes",
        Some(Baseline::Later) => "hashes recorded later (unchanged since then)",
        Some(Baseline::Capture) | None => "capture-time hashes",
    };
    match (integrity, dry_run) {
        (Integrity::Verified, false) => format!("verified against {hashes}"),
        (Integrity::Verified, true) => format!("its files will be checked against {hashes}"),
        _ => "metadata checked; content not verified".to_owned(),
    }
}

/// Runs `backup restore` in M3-D24's order, so a refusal never costs a copy: resolve and
/// reserve the backup (M3-D48); refuse a partial one, a newer state and a running client;
/// plan; under `--dry-run`, stop there with the token; compare `--if-match`; begin (the
/// exchange probed where it will run); keep the current `WTF/` as a backup (M3-D26); and
/// restore. Then backup retention runs once, the source pinned.
pub fn restore_backup(ctx: &Context, args: &BackupRestoreArgs) -> Result<BackupRestoreOutcome> {
    if !args.addon.is_empty() {
        return super::restore_addons::run(ctx, args);
    }
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let (d, backup) = named_backup(ctx, &world, &args.id, &mut warnings)?;
    let reservation = restore::reserve(&backup)?;
    if backup.meta.as_ref().is_some_and(|m| m.partial) {
        return Err(partial(&backup, &d.target.id));
    }
    let engine = super::engine::Engine::new(ctx, &world, RESTORE_COMMAND)?;
    engine.refuse_restore(&d.target, backup.meta.as_ref().map_or(0, |m| m.files))?;
    let plan = restore::plan(&world.paths, &d.target, &backup)?;
    let meta = backup
        .meta
        .clone()
        .ok_or_else(|| Error::internal("a restorable backup has its metadata"))?;
    let client = ClientPaths::new(&d.target.client_dir);
    let bound = of_restore(&RestorePart {
        key: d.target.key,
        generation: plan.generation,
        backup: backup.id,
        files: &plan.files,
        backup_json: meta_digest(&backup),
        wtf: plan.observed.map(|stamp| stamp.to_string()),
    });
    let mut data = BackupRestoreData {
        dry_run: ctx.dry_run,
        target: target_ref(d),
        tx: None,
        kind: TxKind::Restore,
        mode: RestoreMode::Whole,
        backup: RestoreSource {
            id: backup.id,
            captured_at: meta.captured_at,
            kind: meta.kind,
            integrity: meta.integrity,
        },
        files: Some(plan.files),
        before_restore: None,
        reload_required: ReloadRequired::Restart,
        precondition: None,
        exchange: Exchange::Unverified,
        addons: None,
    };
    if ctx.dry_run {
        // The mount checks only: no probe, nothing written. A token given with it
        // previews afresh.
        let topology = restore::preflight(&client, &backup.wtf(), None)?;
        let clones = matches!(
            backup::fs::clone_hint(&client.meta_dir()),
            CloneHint::Capable
        );
        data.precondition = Some(bound.token());
        return Ok(BackupRestoreOutcome {
            data,
            warnings,
            commands: Vec::new(),
            trigger: meta.trigger,
            baseline: meta.baseline,
            would_keep: topology.wtf.is_some() && clones,
        });
    }
    let preview = preview_argv(&ctx.argv);
    check(ctx.if_match.as_deref(), &bound, &preview)?;
    let begun = restore::begin(&world.paths, &client, &backup)?;
    let run = RunId::new();
    let kept = match keep_current(ctx, &world, d, &begun.topology) {
        Ok(kept) => kept,
        Err(error) => {
            begun.abandon(&world.paths, &client);
            return Err(error);
        }
    };
    let before_restore = match &kept {
        Some(created) => {
            warnings.extend(created.warnings.iter().cloned());
            Some(backup_line(ctx, created, run))
        }
        None => {
            warnings.push(before_restore_skipped(&d.target.id));
            None
        }
    };
    let binding = ctx.if_match.as_ref().map(|_| RestoreBinding { preview });
    let restore::Begun {
        tx,
        inflight,
        topology,
    } = begun;
    let req = RestoreRequest {
        target: d,
        backup: &backup,
        reservation,
        topology,
        tx,
        inflight,
        plan: &plan,
        before_restore,
        reason: ctx.reason.clone(),
        run,
    };
    let done = engine.restore(req, binding.as_ref().map(|b| b as &dyn Binding))?;
    warnings.extend(done.warnings.iter().cloned());
    warnings.extend(retain_source(ctx, &world, d, done.key, backup.id));
    data.tx = Some(done.tx);
    data.target.key = Some(done.key);
    data.backup.integrity = done.integrity;
    data.files = Some(done.files);
    data.before_restore = kept.map(|created| KeptBackup {
        id: created.meta.id,
    });
    data.exchange = done.exchange;
    let tx = done.tx.to_string();
    Ok(BackupRestoreOutcome {
        data,
        warnings,
        commands: vec![CommandHint::new(
            "undo",
            ["omaddons", "undo", tx.as_str()],
            false,
        )],
        trigger: meta.trigger,
        baseline: meta.baseline,
        would_keep: false,
    })
}

/// The client and the backup `prefix` names (`restore`, `export`): on the one client `-t`
/// selects, else on whichever has it. A backup on another client than `-t`'s is
/// `not_found`, with a fix naming its own.
pub(super) fn named_backup<'w>(
    ctx: &Context,
    world: &'w World,
    prefix: &str,
    warnings: &mut Vec<Warning>,
) -> Result<(&'w DiscoveredTarget, BackupRef)> {
    let mut all: Vec<(&DiscoveredTarget, Vec<BackupRef>)> = Vec::new();
    for d in &world.resolved.targets {
        let (backups, found) = backup::list(&ClientPaths::new(&d.target.client_dir));
        warnings.extend(found);
        all.push((d, backups));
    }
    all.sort_by(|a, b| a.0.target.id.cmp(&b.0.target.id));
    let found = |listed: &[(&'w DiscoveredTarget, Vec<BackupRef>)], selector| {
        only(listed, prefix, selector).and_then(|(client, id)| {
            listed
                .iter()
                // ubs:ignore — comparing target ids, not secrets
                .filter(|(d, _)| d.target.id == client)
                .find_map(|(d, backups)| {
                    let b = backups.iter().find(|b| b.id == id)?;
                    Some((*d, b.clone()))
                })
                .ok_or_else(|| Error::internal("a matched backup is listed"))
        })
    };
    let Some(selector) = ctx.target.as_deref() else {
        return found(&all, None);
    };
    let selected = world.select_many(ctx)?;
    let d = match selected.as_slice() {
        [one] => *one,
        many => return Err(several(ctx, many, "a restore")),
    };
    let on_it: Vec<(&DiscoveredTarget, Vec<BackupRef>)> = all
        .iter()
        // ubs:ignore — comparing target ids, not secrets
        .filter(|(t, _)| t.target.id == d.target.id)
        .cloned()
        .collect();
    match found(&on_it, Some(selector)) {
        Err(error) if matches!(error.code, ErrorCode::NotFound) => match found(&all, None) {
            Ok((owner, b)) => Err(elsewhere(&b, &d.target.id, &owner.target.id)),
            Err(_) => Err(error),
        },
        other => other,
    }
}

/// `target_ambiguous`: a restore changes one client, and `-t` selects `many`.
fn several(ctx: &Context, many: &[&DiscoveredTarget], what: &str) -> Error {
    let ids: Vec<&str> = many.iter().map(|d| d.target.id.as_str()).collect();
    let with =
        |id: &str| Suggestion::command(crate::discover::registry::with_target(&ctx.argv, id));
    let mut error = Error::new(
        ErrorCode::TargetAmbiguous,
        format!(
            "-t selects {} clients ({}); {what} changes one",
            ids.len(),
            ids.join(", ")
        ),
        with(ids.first().copied().unwrap_or_default()),
    )
    .details(serde_json::json!({ "candidates": ids }));
    for id in ids.iter().skip(1) {
        error = error.alternative(with(id));
    }
    error
}

/// `not_found`: the backup is `owner`'s, not `selected`'s.
fn elsewhere(b: &BackupRef, selected: &str, owner: &str) -> Error {
    let id = b.id.to_string();
    Error::new(
        ErrorCode::NotFound,
        format!("backup {id} is {owner}'s, not {selected}'s"),
        Suggestion::command(["omaddons", "backup", "restore", id.as_str(), "-t", owner]),
    )
    .details(serde_json::json!({ "id": id, "target": selected, "owner": owner }))
}

/// `usage`: a partial backup (the files a crashed partial restore displaced) holds only
/// some addons' settings, so it never replaces the whole `WTF/`.
fn partial(b: &BackupRef, target: &str) -> Error {
    Error::new(
        ErrorCode::Usage,
        format!(
            "backup {} holds only some addons' settings, so it cannot replace the whole WTF/",
            b.id
        ),
        Suggestion::command(["omaddons", "backup", "list", "-t", target]),
    )
    .details(serde_json::json!({ "id": b.id.to_string(), "partial": true }))
}

/// Keeps the current `WTF/` as a `before_restore` backup (M3-D26): a clone only, so none
/// is made where the filesystem cannot clone or there is no `WTF/`. It registers the
/// client first when needed, and runs no retention: the restore's source stays.
fn keep_current(
    ctx: &Context,
    world: &World,
    d: &DiscoveredTarget,
    topology: &restore::Topology,
) -> Result<Option<Created>> {
    let client = ClientPaths::new(&d.target.client_dir);
    let clones = matches!(
        backup::fs::clone_hint(&client.meta_dir()),
        CloneHint::Capable
    );
    if topology.wtf.is_none() || !clones {
        return Ok(None);
    }
    let scopes = ScopeTable::new(
        &world.paths,
        std::slice::from_ref(&d.target.client_dir),
        None,
    )?;
    let target = registered(ctx, world, d, &scopes, RESTORE_COMMAND)?;
    let req = CreateRequest {
        target: &target,
        kind: BackupKind::BeforeRestore,
        trigger: None,
        actor: ctx.actor,
        reason: ctx.reason.clone(),
        policy: ClonePolicy::Required,
        verify: false,
        // The restore refused a client that runs or may.
        client_running: false,
        command: RESTORE_COMMAND,
    };
    match capture::create(&world.paths, &scopes, &req, &ctx.progress) {
        Ok(created) => Ok(Some(created)),
        Err(capture::CreateError::NotReflinkable) => Ok(None),
        Err(capture::CreateError::Failed(error)) => Err(error),
    }
}

/// `before_restore_skipped`: no copy of the current `WTF/` but the transaction's own.
fn before_restore_skipped(target: &str) -> Warning {
    Warning::new(
        WarningCode::BeforeRestoreSkipped,
        format!(
            "{target}: the current WTF/ was not kept as a backup (the filesystem cannot clone files, or there was no WTF/); the restore's undo snapshot keeps it until retention"
        ),
    )
}

/// Backup retention once after a restore (M3-D26), its source `source` pinned for the
/// pass. A failure is only `retention_failed`: the restore stands.
pub(super) fn retain_source(
    ctx: &Context,
    world: &World,
    d: &DiscoveredTarget,
    key: TargetKey,
    source: BackupId,
) -> Option<Warning> {
    let keep_days = world
        .config
        .effective_limits()
        .unwrap_or_default()
        .backup_auto_keep_days;
    let opts = PruneOpts {
        pins: BTreeSet::from([source]),
        ..PruneOpts::default()
    };
    let ring = Ring {
        command: RESTORE_COMMAND,
        actor: ctx.actor,
    };
    let pass = ScopeTable::new(
        &world.paths,
        std::slice::from_ref(&d.target.client_dir),
        None,
    )
    .and_then(|scopes| {
        prune::retain(
            &world.paths,
            &scopes,
            &ClientPaths::new(&d.target.client_dir),
            Some(&key),
            keep_days,
            &opts,
            ring,
        )
    });
    let error = pass.err()?;
    Some(
        Warning::new(
            WarningCode::RetentionFailed,
            format!(
                "{}: pruning old backups after the restore failed: {}",
                d.target.id, error.message
            ),
        )
        .with_fix(Suggestion::command([
            "omaddons",
            "backup",
            "prune",
            "-t",
            d.target.id.as_str(),
        ])),
    )
}

// ---------------------------------------------------------------------------------------
// `backup prune`.

/// `backup prune`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupPruneData {
    pub dry_run: bool,
    /// One per selected client, by id.
    pub targets: Vec<BackupPruneTarget>,
    /// `--dry-run`: the token `--if-match` takes to delete exactly these backups
    /// (M3-D46); `null` for a real run.
    pub precondition: Option<String>,
}

/// One client's prune.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupPruneTarget {
    pub target: TargetRef,
    /// The backups deleted (with `--dry-run`, that would be), newest capture first.
    pub pruned: Vec<Pruned>,
    /// Backups a reader (a restore, an export, a verify) held: skipped, and pruned by a
    /// later run once free.
    pub busy: Vec<BusyBackup>,
    /// The backups left.
    pub kept: u64,
    /// The deletions came from a pass under space pressure, which `backup prune` never
    /// is: always `false` here.
    pub reclaimed_for_space: bool,
    pub error: Option<ItemError>,
}

/// A backup skipped because a reader held it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BusyBackup {
    pub id: BackupId,
}

/// `backup prune`'s outcome.
#[derive(Debug)]
pub struct BackupPruneOutcome {
    data: BackupPruneData,
    warnings: Vec<Warning>,
    partial: bool,
}

impl Outcome for BackupPruneOutcome {
    type Data = BackupPruneData;

    fn command(&self) -> &'static str {
        PRUNE_COMMAND
    }

    fn data(&self) -> &BackupPruneData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(w, "No backups.");
        }
        for t in &self.data.targets {
            let id = &t.target.id;
            if let Some(error) = &t.error {
                writeln!(w, "{id}: not pruned: {}", error.message)?;
                continue;
            }
            if t.pruned.is_empty() {
                writeln!(w, "{id}: nothing to prune, kept {}", t.kept)?;
            } else {
                let bytes: u64 = t.pruned.iter().map(|p| p.bytes).sum();
                let verb = if self.data.dry_run {
                    "would prune"
                } else {
                    "pruned"
                };
                writeln!(
                    w,
                    "{id}: {verb} {} ({} apparent), kept {}",
                    crate::output::count(t.pruned.len(), "backup"),
                    size(bytes),
                    t.kept
                )?;
            }
            for b in &t.busy {
                let short = super::history::short_id(&b.id.to_string());
                writeln!(w, "{id}: {short} is in use, skipped")?;
            }
        }
        if let Some(token) = &self.data.precondition {
            writeln!(w, "Run it bound to this preview: --if-match {token}")?;
        }
        Ok(())
    }
}

const PRUNE_COMMAND: &str = "backup prune";

/// One client's planned prune.
struct PrunePlan<'w> {
    d: &'w DiscoveredTarget,
    backups: Vec<BackupRef>,
    victims: Vec<BackupId>,
}

/// Runs `backup prune`: the retention pass (M3-D32) on each selected client's backups,
/// `--keep` and `--older-than` reaching the user's own too, or exactly the backups named.
/// With `--if-match`, only when its victims are what the token's preview showed (M3-D46),
/// checked before anything is deleted. Appends no history line (M3-D33): the backups are
/// the record.
pub fn prune_backups(ctx: &Context, args: &BackupPruneArgs) -> Result<BackupPruneOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let now = crate::clock::now();
    let captured_before = args
        .older_than
        .as_deref()
        .map(|d| older_than(d, now))
        .transpose()?;
    let keep_days = world
        .config
        .effective_limits()
        .unwrap_or_default()
        .backup_auto_keep_days;
    let mut listed = listed(ctx, &world, &mut warnings)?;
    let mut named: BTreeMap<String, BTreeSet<BackupId>> = BTreeMap::new();
    for prefix in &args.ids {
        let (client, id) = only(&listed, prefix, ctx.target.as_deref())?;
        named.entry(client).or_default().insert(id);
    }
    if !args.ids.is_empty() {
        listed.retain(|(d, _)| named.contains_key(&d.target.id));
    }
    let tz = jiff::tz::TimeZone::system();
    let plans: Vec<PrunePlan<'_>> = listed
        .into_iter()
        .map(|(d, backups)| {
            let opts = PruneOpts {
                ids: named.get(&d.target.id).cloned(),
                keep: args.keep,
                captured_before,
                pins: BTreeSet::new(),
            };
            let victims = prune::select(&backups, now, &tz, keep_days, &opts);
            PrunePlan {
                d,
                backups,
                victims,
            }
        })
        .collect();
    for p in &plans {
        let named = named.get(&p.d.target.id).into_iter().flatten();
        if let Some(id) = named.into_iter().find(|id| !p.victims.contains(id)) {
            return Err(always_kept(&p.d.target.id, id));
        }
    }
    // A preview given a token previews afresh: it returns the current one.
    if ctx.dry_run {
        return Ok(BackupPruneOutcome {
            data: BackupPruneData {
                dry_run: true,
                targets: plans.iter().map(would_prune).collect(),
                precondition: Some(prune_precondition(&plans).token()),
            },
            warnings,
            partial: false,
        });
    }
    check(
        ctx.if_match.as_deref(),
        &prune_precondition(&plans),
        &preview_argv(&ctx.argv),
    )?;
    let ring = Ring {
        command: PRUNE_COMMAND,
        actor: ctx.actor,
    };
    let mut targets = Vec::new();
    let mut first_error = None;
    for p in &plans {
        let client = ClientPaths::new(&p.d.target.client_dir);
        let pass = ScopeTable::new(
            &world.paths,
            std::slice::from_ref(&p.d.target.client_dir),
            None,
        )
        .and_then(|scopes| {
            prune::delete(
                &world.paths,
                &scopes,
                &client,
                p.d.target.key.as_ref(),
                &p.backups,
                &p.victims,
                ring,
            )
        });
        let target = target_ref(p.d);
        match pass {
            Ok(report) => targets.push(pruned_target(target, report)),
            Err(error) => {
                let mut warning = Warning::item(&error);
                warning.message = format!("{}: {}", p.d.target.id, error.message);
                warnings.push(warning);
                targets.push(BackupPruneTarget {
                    target,
                    pruned: Vec::new(),
                    busy: Vec::new(),
                    kept: p.backups.len() as u64,
                    reclaimed_for_space: false,
                    error: Some(ItemError::from(&error)),
                });
                first_error.get_or_insert(error);
            }
        }
    }
    let failed = targets.iter().filter(|t| t.error.is_some()).count();
    if failed == targets.len()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    Ok(BackupPruneOutcome {
        partial: failed > 0,
        data: BackupPruneData {
            dry_run: false,
            targets,
            precondition: None,
        },
        warnings,
    })
}

/// `--older-than D`: the capture time before which the user's backups go.
fn older_than(d: &str, now: Timestamp) -> Result<Timestamp> {
    super::history::parse_since(d, now).ok_or_else(|| {
        Error::new(
            ErrorCode::Usage,
            format!(
                "--older-than takes a duration (30m, 12h, 1d, 2w) or an RFC 3339 time, not {d:?}"
            ),
            Suggestion::command([
                "omaddons",
                "backup",
                "prune",
                "--older-than",
                "30d",
                "--dry-run",
            ]),
        )
    })
}

/// `usage`: the backup named is the client's newest checkpoint, which no prune deletes
/// (M3-D32); naming a backup lifts only the 24 h protection.
fn always_kept(target: &str, id: &BackupId) -> Error {
    Error::new(
        ErrorCode::Usage,
        format!(
            "{id} is {target}'s newest checkpoint, which pruning always keeps; nothing was deleted: back up first, then prune it"
        ),
        Suggestion::command(["omaddons", "backup", "create", "-t", target]),
    )
    .details(serde_json::json!({ "id": id.to_string(), "target": target }))
}

/// The precondition of `plans` (M3-D46): its victims and their metadata digests.
fn prune_precondition(plans: &[PrunePlan<'_>]) -> Precondition {
    let parts: Vec<PrunePart<'_>> = plans
        .iter()
        .map(|p| PrunePart {
            key: p.d.target.key,
            target_id: &p.d.target.id,
            victims: p
                .backups
                .iter()
                .filter(|b| p.victims.contains(&b.id))
                .map(|b| (b.id, meta_digest(b)))
                .collect(),
        })
        .collect();
    of_prune(&parts)
}

/// The lowercase hex SHA-256 of `b`'s `backup.json`, `None` when it does not read.
pub(super) fn meta_digest(b: &BackupRef) -> Option<String> {
    let bytes = crate::discover::read_bytes(&b.dir.join("backup.json"))?;
    Some(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// `backup export`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupExportData {
    pub dry_run: bool,
    pub target: TargetRef,
    /// The backup, as `backup list` shows it.
    pub backup: BackupItem,
    /// The archive written (or, in a dry run, that would be).
    pub file: std::path::PathBuf,
    /// The backup's files, and their apparent bytes.
    pub files: u64,
    pub bytes: u64,
    /// The archive's size and SHA-256, as written; `null` in a dry run.
    pub compressed_bytes: Option<u64>,
    pub sha256: Option<String>,
    /// Where the archive's hashes come from: the backup's own (`capture`, `import`,
    /// `later`), or `computed_at_export`.
    pub verified_against: VerifiedAgainst,
}

/// `backup export`'s outcome.
#[derive(Debug)]
pub struct BackupExportOutcome {
    data: BackupExportData,
    warnings: Vec<Warning>,
}

impl Outcome for BackupExportOutcome {
    type Data = BackupExportData;

    fn command(&self) -> &'static str {
        EXPORT_COMMAND
    }

    fn data(&self) -> &BackupExportData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let short = super::history::short_id(&d.backup.id.to_string());
        let captured = d
            .backup
            .captured_at
            .map(|at| format!(", {}", when(at, crate::clock::now())))
            .unwrap_or_default();
        let what = format!(
            "backup {short} ({}{captured}) to {}: {} {}, {}",
            d.target.id,
            d.file.display(),
            d.files,
            if d.files == 1 { "file" } else { "files" },
            size(d.bytes)
        );
        match d.compressed_bytes {
            Some(compressed) if !d.dry_run => {
                writeln!(w, "exported {what} → {}", size(compressed))
            }
            _ => writeln!(w, "would export {what}"),
        }
    }
}

/// Runs `backup export` (M3-D34): reserves the backup, admits exactly `<FILE>` to the
/// write scopes, and streams the archive there. A dry run checks the same and writes
/// nothing. Not a transaction: no history line, no doorbell.
pub fn export_backup(ctx: &Context, args: &BackupExportArgs) -> Result<BackupExportOutcome> {
    if args.file.as_os_str() == "-" {
        return Err(Error::new(
            ErrorCode::Usage,
            "backup export writes to a file, not standard output (binary data would break --json's one envelope)",
            Suggestion::command(["omaddons", "help", "backup", "export"]),
        ));
    }
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    let (d, backup) = named_backup(ctx, &world, &args.id, &mut warnings)?;
    let reservation = export::reserve(&backup)?;
    let plan = export::plan(&backup)?;
    let client_dirs: Vec<std::path::PathBuf> = world
        .resolved
        .targets
        .iter()
        .map(|t| t.target.client_dir.clone())
        .collect();
    let mut scopes = ScopeTable::new(&world.paths, &client_dirs, None)?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let file = scopes.with_export_file(&cwd.join(&args.file))?;
    let names = WtfPathRedactor::for_client(&ClientPaths::new(&d.target.client_dir));
    let (compressed_bytes, sha256) = if ctx.dry_run {
        if std::fs::symlink_metadata(&file).is_ok() {
            return Err(Error::new(
                ErrorCode::FileExists,
                format!("{} exists; an export never replaces a file", file.display()),
                Suggestion::text("choose another file name, or delete that file yourself"),
            )
            .details(serde_json::json!({ "file": file.display().to_string() })));
        }
        (None, None)
    } else {
        let exported = export::export(&scopes, &backup, &plan, &file, (&names, &ctx.progress))?;
        (Some(exported.compressed_bytes), Some(exported.sha256))
    };
    drop(reservation);
    Ok(BackupExportOutcome {
        data: BackupExportData {
            dry_run: ctx.dry_run,
            target: target_ref(d),
            backup: item(&backup, false),
            file,
            files: plan.files,
            bytes: plan.bytes,
            compressed_bytes,
            sha256,
            verified_against: plan.verified_against,
        },
        warnings,
    })
}

/// `backup import`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BackupImportData {
    pub dry_run: bool,
    pub target: TargetRef,
    /// The imported backup, as `backup list` shows it; `null` in a dry run.
    pub backup: Option<BackupItem>,
    /// The exported backup the archive holds.
    pub source: ImportSource,
    /// What the archive holds: its files, and their bytes.
    pub files: u64,
    pub bytes: u64,
}

/// `backup import`'s outcome.
#[derive(Debug)]
pub struct BackupImportOutcome {
    data: BackupImportData,
    warnings: Vec<Warning>,
}

impl Outcome for BackupImportOutcome {
    type Data = BackupImportData;

    fn command(&self) -> &'static str {
        IMPORT_COMMAND
    }

    fn data(&self) -> &BackupImportData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let source = format!(
            "backup {} ({}, {})",
            super::history::short_id(&d.source.id.to_string()),
            d.source.target_id,
            when(d.source.captured_at, crate::clock::now())
        );
        match &d.backup {
            Some(b) => {
                let id = b.id.to_string();
                writeln!(
                    w,
                    "imported {source} as backup {} on {}: {} {}, {}",
                    super::history::short_id(&id),
                    d.target.id,
                    d.files,
                    if d.files == 1 { "file" } else { "files" },
                    size(d.bytes)
                )?;
                writeln!(
                    w,
                    "restore with: omaddons backup restore {} -t {}",
                    &id[..id.len().min(ID_PREFIX_MIN)],
                    d.target.id
                )
            }
            None => writeln!(
                w,
                "would import {source} on {}: {} {}, {}",
                d.target.id,
                d.files,
                if d.files == 1 { "file" } else { "files" },
                size(d.bytes)
            ),
        }
    }
}

/// Runs `backup import` (M3-D35): pass 1 over the archive (everything checked, nothing
/// written), the family check, then pass 2 into a stage, published as an imported backup
/// with its history line. A dry run stops after the checks.
pub fn import_backup(ctx: &Context, args: &BackupImportArgs) -> Result<BackupImportOutcome> {
    let world = World::load(ctx)?;
    let mut warnings = world.warnings.clone();
    if ctx.target.is_none() {
        return Err(Error::new(
            ErrorCode::Usage,
            "backup import needs -t: the client the backup is for",
            Suggestion::command(["omaddons", "targets"]),
        ));
    }
    let selected = world.select_many(ctx)?;
    let d = match selected.as_slice() {
        [one] => *one,
        many => return Err(several(ctx, many, "an import")),
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let file = cwd.join(&args.file);
    let scanned = import::scan(&file)?;
    let source = &scanned.source.meta;
    if source.family != d.target.family && !args.force {
        return Err(other_family(ctx, &world, d, source.family));
    }
    let origin = ImportSource {
        id: source.id,
        target_id: source.target_id.clone(),
        family: source.family,
        created_at: source.created_at,
        captured_at: source.captured_at,
        tool_version: source.tool_version.clone(),
    };
    let mut data = BackupImportData {
        dry_run: ctx.dry_run,
        target: target_ref(d),
        backup: None,
        source: origin,
        files: scanned.files,
        bytes: scanned.bytes,
    };
    if ctx.dry_run {
        return Ok(BackupImportOutcome { data, warnings });
    }
    let scopes = ScopeTable::new(
        &world.paths,
        std::slice::from_ref(&d.target.client_dir),
        None,
    )?;
    let target = registered(ctx, &world, d, &scopes, IMPORT_COMMAND)?;
    let req = import::ImportRequest {
        target: &target,
        actor: ctx.actor,
        reason: ctx.reason.clone(),
        command: IMPORT_COMMAND,
    };
    let created = import::import(&world.paths, &scopes, &file, &scanned, &req, &ctx.progress)?;
    if let Err(error) = record_line(
        ctx,
        &world.paths,
        &scopes,
        &created,
        (RunId::new(), IMPORT_COMMAND),
    ) {
        warnings.push(not_recorded(&world.paths, &target.id, &error));
    }
    let client = ClientPaths::new(&d.target.client_dir);
    let (listed, found) = backup::list(&client);
    warnings.extend(found);
    data.target = TargetRef {
        id: target.id.clone(),
        key: target.key,
        mode: None,
    };
    data.backup = listed
        .iter()
        .find(|b| b.id == created.meta.id)
        .map(|b| item(b, false));
    Ok(BackupImportOutcome { data, warnings })
}

/// `wrong_flavor`: the archive is another family's. The fix imports into a client of
/// that family when one exists; the alternative, `--force`.
fn other_family(
    ctx: &Context,
    world: &World,
    d: &DiscoveredTarget,
    family: crate::rules::Family,
) -> Error {
    let force = {
        let mut argv = crate::cli::fix_argv(&ctx.argv);
        argv.push("--force".to_owned());
        Suggestion::command(argv)
    };
    let same = world
        .resolved
        .targets
        .iter()
        .find(|t| t.target.family == family)
        .map(|t| {
            Suggestion::command(crate::discover::registry::with_target(
                &ctx.argv,
                &t.target.id,
            ))
        });
    let message = format!(
        "this backup is from a {} client; {} is {}",
        family.as_str(),
        d.target.id,
        d.target.family.as_str()
    );
    let details = serde_json::json!({ "family": family, "target_family": d.target.family });
    match same {
        Some(fix) => Error::new(ErrorCode::WrongFlavor, message, fix)
            .alternative(force)
            .details(details),
        None => Error::new(ErrorCode::WrongFlavor, message, force).details(details),
    }
}

pub(super) fn target_ref(d: &DiscoveredTarget) -> TargetRef {
    TargetRef {
        id: d.target.id.clone(),
        key: d.target.key,
        mode: None,
    }
}

/// `--dry-run`: what `p` would delete; a victim a reader holds now would be skipped.
fn would_prune(p: &PrunePlan<'_>) -> BackupPruneTarget {
    let mut pruned = Vec::new();
    let mut busy = Vec::new();
    for b in p.backups.iter().filter(|b| p.victims.contains(&b.id)) {
        if b.busy {
            busy.push(BusyBackup { id: b.id });
        } else {
            pruned.push(Pruned {
                id: b.id,
                kind: b.meta.as_ref().map(|m| m.kind),
                bytes: b.meta.as_ref().map_or(0, |m| m.bytes),
            });
        }
    }
    BackupPruneTarget {
        target: target_ref(p.d),
        kept: p.backups.len().saturating_sub(pruned.len()) as u64,
        pruned,
        busy,
        reclaimed_for_space: false,
        error: None,
    }
}

fn pruned_target(target: TargetRef, report: PruneReport) -> BackupPruneTarget {
    BackupPruneTarget {
        target,
        pruned: report.pruned,
        busy: report
            .busy
            .into_iter()
            .map(|id| BusyBackup { id })
            .collect(),
        kept: report.kept as u64,
        reclaimed_for_space: report.reclaimed_for_space,
        error: None,
    }
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
