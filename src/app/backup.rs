//! `omaddons backup [list] [ID] [--limit N]`: each client's `WTF/` backups, newest
//! capture first, with what each proves, the apparent space they take and the space
//! free beside them. Bare `backup` is `backup list`. It reads `backup.json` files and
//! checks each manifest exists, never a tree (M3-D47); its only write is the backup
//! inventory projection, a derived cache it rebuilds when stale. The commands that
//! make, check and restore backups are subcommands of their own.

use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::outcome::TargetRef;
use super::world::World;
use crate::backup::inventory::{self, AutoLast, Capability, Inventory};
use crate::backup::{
    self, BackupKind, BackupRef, Baseline, Consistency, Integrity, Reflinked, Trigger,
};
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::error::{Result, Suggestion, Warning};
use crate::model::{Actor, BackupId, FolderName};
use crate::output::{Outcome, Style, table};
use crate::state::config::AutoBackup;
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
}

impl BackupAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::List(_) => "backup list",
        }
    }
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

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(w, "No backups.");
        }
        let now = Timestamp::now();
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
        listed.retain(|(d, _)| d.target.id == client);
        show = Some(id);
    }
    let mut targets = Vec::new();
    let mut labels = Vec::new();
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
        targets.push(target(&world.paths, d, &backups, &view));
    }
    Ok(BackupListOutcome {
        data: BackupListData { targets },
        warnings,
        labels,
    })
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
/// projection (rebuilt when stale).
fn target(paths: &Paths, d: &DiscoveredTarget, backups: &[BackupRef], view: &View) -> BackupTarget {
    let client = ClientPaths::new(&d.target.client_dir);
    let projection =
        d.target.key.map(
            |key| match inventory::load_validated(paths, &client, &key) {
                Inventory::Fresh(inventory) => inventory,
                Inventory::Stale(..) | Inventory::Missing(_) => {
                    inventory::rebuild(paths, &client, &key).inventory
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
