//! `targets rm <id> --purge` (omad-launch-week-fixes-bmcx.2): deletes what omaddons kept
//! for a registered client discovery no longer finds, previewed and bound.
//!
//! **What goes:** every entry of the client's `Interface/.omaddons/` that a prune kind
//! names (its `target.json` marker, undo snapshots, repair halves, stages and `WTF/`
//! backups) and whatever an interrupted purge left in its trash; the state halves of
//! each transaction that touched this client alone; its record and files index; then its
//! registry entry. **What stays:** anything else under `.omaddons/` (listed as kept, so
//! `.omaddons` stays too), `history.jsonl`, `Interface/AddOns/`, everything the launcher
//! or Proton left, and every other client's data.
//!
//! **The flow:** plan from what is on disk without the lock; a dry run stops there with
//! its token. A real run needs `--yes`, takes the global lock, recovers journals as every
//! `targets` change does, refuses while a stuck journal's fence covers what it would
//! delete, plans again and compares `--if-match`, then hands the lock to one `prune()`
//! that renames every victim into trash under it and deletes after releasing it. It
//! takes the lock again to sweep the client's trash and remove the emptied directories,
//! and unregisters the client last, only once nothing it planned is left: a crash
//! anywhere before leaves a registered missing client, which the next purge finishes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::{Done, RmData, render_rm};
use crate::app::precondition::{self, Precondition, PurgeTokenParts};
use crate::app::world::World;
use crate::backup::{self, BackupKind};
use crate::cli::Context;
use crate::discover::registry::{LockedRegistry, unknown_id};
use crate::engine::fence;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::fsx::{self, FileId, GlobalLock, LockUse, PruneReason, WriteSession, Writer};
use crate::limits::PURGE_MAX_ENTRIES;
use crate::model::{BackupId, TargetKey, TxId, TxKind};
use crate::output::Style;
use crate::state::history::{self, HistoryFilter, Reach};
use crate::state::paths::{ClientPaths, Paths, REPAIR_SUFFIX, tx_named};
use crate::state::records::load_record;
use crate::state::registry::RegistryEntry;

/// The lock holder's name.
const COMMAND: &str = "targets rm";

/// What a purge deletes (a preview: would delete).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PurgeData {
    /// Each path that goes, sorted by path, with what it is and its apparent bytes.
    pub deletions: Vec<PurgeDeletion>,
    /// The client's `WTF/` backups among them, newest capture first: copy one out of its
    /// `path` before the purge to keep it.
    pub backups: Vec<PurgeBackup>,
    /// The transactions whose undo snapshots go, sorted: none of them can be undone
    /// after.
    pub lost_undos: Vec<TxId>,
    /// Entries of `.omaddons/` no deletion names (omaddons did not write them): never
    /// deleted, so `.omaddons` itself stays.
    pub kept_entries: Vec<PathBuf>,
    /// The deletions' apparent bytes.
    pub bytes: u64,
}

/// One path a purge deletes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PurgeDeletion {
    pub kind: PurgeKind,
    pub path: PathBuf,
    /// Apparent bytes of the files in it.
    pub bytes: u64,
}

/// What a deleted path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PurgeKind {
    /// `.omaddons/target.json`, the client's marker.
    Marker,
    /// `.omaddons/backups/<id>`: a `WTF/` backup.
    Backup,
    /// `.omaddons/snapshots/<tx>` (or `<tx>.wtf`): a transaction's undo snapshot.
    Snapshot,
    /// `.omaddons/snapshots/<tx>.repair`: a doctor repair's client half.
    Repair,
    /// `.omaddons/stage/<tx>`: a stage an interrupted command left.
    Stage,
    /// `.omaddons/trash/<entry>`: what an interrupted deletion left.
    Trash,
    /// `<state>/snapshots/<tx>` (or `<tx>.wtf`): the state half of an undo snapshot.
    StateSnapshot,
    /// `<state>/targets/<key>.json` or `<key>.files.json`: its record and files index.
    Record,
}

impl PurgeKind {
    /// The wire name, for the token.
    fn wire(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::Backup => "backup",
            Self::Snapshot => "snapshot",
            Self::Repair => "repair",
            Self::Stage => "stage",
            Self::Trash => "trash",
            Self::StateSnapshot => "state_snapshot",
            Self::Record => "record",
        }
    }

    /// The human view's word for it.
    fn text(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::Backup => "backup",
            Self::Snapshot => "undo snapshot",
            Self::Repair => "repair half",
            Self::Stage => "stage",
            Self::Trash => "trash",
            Self::StateSnapshot => "undo snapshot",
            Self::Record => "record",
        }
    }
}

/// One `WTF/` backup a purge deletes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PurgeBackup {
    pub id: BackupId,
    /// `null` when its `backup.json` cannot be read.
    pub kind: Option<BackupKind>,
    /// When its content was captured; `null` when its `backup.json` cannot be read.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub captured_at: Option<Timestamp>,
    pub path: PathBuf,
}

/// What a purge deletes, read from disk.
struct Plan {
    data: PurgeData,
    /// What `prune()` deletes: every deletion but the trash entries, which the sweep of
    /// [`fsx::remove_purged_meta`] deletes.
    victims: Vec<PathBuf>,
    /// Each deletion's identity as observed, for the token.
    identities: Vec<(PathBuf, Option<FileId>)>,
    /// The client's `Interface/.omaddons/`.
    meta: PathBuf,
}

/// `targets rm <id> --purge`: the preview (`--dry-run`), or the purge itself (`--yes`).
pub(super) fn run(ctx: &Context, id: &str, yes: bool) -> Result<Done<RmData>> {
    let world = World::load(ctx)?;
    let paths = &world.paths;
    let entry = world
        .registry
        .entry_by_id(id)
        .ok_or_else(|| unknown_id(id))?
        .clone();
    if world
        .resolved
        .targets
        .iter()
        .any(|d| d.target.key == Some(entry.key))
    {
        return Err(installed(id));
    }
    let preview = precondition::preview_argv(&ctx.argv);
    if !ctx.dry_run && !yes {
        return Err(refused(
            "a purge deletes what it lists, and the undos that need it: a real run needs --yes (preview it first)",
            "--yes",
            preview,
        ));
    }
    let managed_addons = load_record(paths, &entry.key)?.value.addons.len();
    let was_default = world.registry.default == Some(entry.key);
    let done = |dry_run: bool, data: PurgeData, token: Option<String>| Done {
        command: COMMAND,
        data: RmData {
            id: entry.id.clone(),
            key: entry.key,
            managed_addons,
            was_default,
            dry_run,
            purge: Some(data),
            precondition: token,
        },
        warnings: Vec::new(),
        render: render_rm as fn(&RmData, &mut dyn Write, &Style) -> std::io::Result<()>,
    };

    // A real run plans only under the lock, so what a plan warns about is said once.
    if ctx.dry_run {
        let first = plan(ctx, paths, &entry)?;
        let token = token_of(entry.key, &first);
        precondition::check(ctx.if_match.as_deref(), &token, &preview)?;
        return Ok(done(true, first.data, Some(token.token())));
    }

    let guard = GlobalLock::acquire(paths, COMMAND)?;
    {
        use crate::engine::journal::{no_saved_variables, recover_under};
        ctx.notes.warn_all(&recover_under(
            &guard,
            paths,
            &no_saved_variables,
            ctx.command,
            ctx.actor,
        ));
    }
    // Under the lock everything is read again, and this plan is the one carried out.
    let entry = LockedRegistry::load(&guard, paths)?
        .registry()
        .entry_by_id(id)
        .filter(|e| e.key == entry.key)
        .cloned()
        .ok_or_else(|| unknown_id(id))?;
    let dirs = [entry.client_dir.clone()];
    let fresh = plan(ctx, paths, &entry)?;
    let reached: Vec<PathBuf> = std::iter::once(fresh.meta.clone())
        .chain(fresh.data.deletions.iter().map(|d| d.path.clone()))
        .collect();
    fence::refuse_covered(&fence::load(paths, &dirs), &reached)?;
    precondition::check(
        ctx.if_match.as_deref(),
        &token_of(entry.key, &fresh),
        &preview,
    )?;
    stop_at("targets.purge.planned", &preview)?;
    let scopes = fence::table(paths, &dirs, None)?;
    if fresh.victims.is_empty() {
        drop(guard);
    } else {
        fsx::prune(
            &scopes,
            PruneReason::TargetPurge,
            &fresh.victims,
            LockUse::HandOff(guard),
        )?;
    }

    let guard = GlobalLock::acquire(paths, COMMAND)?;
    let left = fsx::remove_purged_meta(&scopes, &fresh.meta, &guard)?;
    stop_at("targets.purge.emptied", &preview)?;
    let unfinished = unfinished(&fresh, &left);
    if !unfinished.is_empty() {
        return Err(not_finished(id, &unfinished, preview));
    }
    let mut session = WriteSession::untracked(Writer::Mutating, &scopes, "registry");
    let mut registry = LockedRegistry::load(&guard, paths)?;
    let removed = registry.remove(&mut session, id)?;
    crate::app::record_setting(ctx, &mut session, paths, TxKind::Target, Some(removed.key));
    Ok(done(false, fresh.data, None))
}

/// The plan's token.
fn token_of(key: TargetKey, plan: &Plan) -> Precondition {
    let deletions: Vec<(&'static str, &Path)> = plan
        .data
        .deletions
        .iter()
        .map(|d| (d.kind.wire(), d.path.as_path()))
        .collect();
    let identities: Vec<(&Path, Option<FileId>)> = plan
        .identities
        .iter()
        .map(|(path, id)| (path.as_path(), *id))
        .collect();
    precondition::of_purge(&PurgeTokenParts {
        key,
        deletions: &deletions,
        identities: &identities,
        kept: &plan.data.kept_entries,
    })
}

/// Entries visited so far, against [`PURGE_MAX_ENTRIES`].
struct Budget<'a> {
    left: usize,
    meta: &'a Path,
}

impl Budget<'_> {
    fn take(&mut self) -> Result<()> {
        if self.left == 0 {
            return Err(Error::new(
                ErrorCode::StateCorrupt,
                format!(
                    "{} and its state halves hold more than {PURGE_MAX_ENTRIES} entries, which omaddons never writes for one client; nothing was deleted",
                    self.meta.display()
                ),
                Suggestion::text(format!(
                    "look at {} by hand: it holds files omaddons did not write",
                    self.meta.display()
                )),
            )
            .details(serde_json::json!({ "path": self.meta.display().to_string() })));
        }
        self.left -= 1;
        Ok(())
    }
}

/// What purging `entry` deletes, and what it keeps, as the disk is now. Reads only.
fn plan(ctx: &Context, paths: &Paths, entry: &RegistryEntry) -> Result<Plan> {
    let client = ClientPaths::new(&entry.client_dir);
    let meta = client.meta_dir();
    let mut budget = Budget {
        left: PURGE_MAX_ENTRIES,
        meta: &meta,
    };
    let mut found: Vec<(PurgeKind, PathBuf)> = Vec::new();
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut undo_lost: Vec<TxId> = Vec::new();
    for (path, kind) in children(&meta, &mut budget)? {
        let name = file_name(&path);
        match (name.as_str(), kind) {
            ("target.json", Entry::File) => found.push((PurgeKind::Marker, path)),
            ("snapshots", Entry::Dir) => {
                for (child, kind) in children(&path, &mut budget)? {
                    let name = file_name(&child);
                    match (tx_named(&name), repair_half(&name), kind) {
                        (Some(tx), _, Entry::Dir) => {
                            undo_lost.push(tx);
                            found.push((PurgeKind::Snapshot, child));
                        }
                        (None, true, Entry::Dir) => found.push((PurgeKind::Repair, child)),
                        _ => kept.push(child),
                    }
                }
            }
            ("stage", Entry::Dir) => {
                for (child, kind) in children(&path, &mut budget)? {
                    if kind != Entry::Link && tx_named(&file_name(&child)).is_some() {
                        found.push((PurgeKind::Stage, child));
                    } else {
                        kept.push(child);
                    }
                }
            }
            ("backups", Entry::Dir) => {
                for (child, kind) in children(&path, &mut budget)? {
                    if kind == Entry::Dir && file_name(&child).parse::<BackupId>().is_ok() {
                        found.push((PurgeKind::Backup, child));
                    } else {
                        kept.push(child);
                    }
                }
            }
            // Only `prune()` renames into trash: everything there is garbage.
            ("trash", Entry::Dir) => found.extend(
                children(&path, &mut budget)?
                    .into_iter()
                    .map(|(child, _)| (PurgeKind::Trash, child)),
            ),
            _ => kept.push(path),
        }
    }
    // The state halves of every transaction that touched this client alone: a line that
    // could not be read keeps its halves, which is the safe side.
    let lines = history::read(
        paths,
        &HistoryFilter {
            target: Some(entry.key),
            ..HistoryFilter::default()
        },
    )?;
    if let Some(warning) = lines.skipped_warning() {
        ctx.notes.warn(warning);
    }
    let alone = Reach::Targets(std::iter::once(entry.key).collect());
    for line in lines.entries.iter().filter(|line| line.reach() == alone) {
        for half in [
            paths.state_snapshot(&line.tx),
            paths.state_snapshot_wtf(&line.tx),
        ] {
            if fs::symlink_metadata(&half).is_ok_and(|m| m.is_dir()) {
                undo_lost.push(line.tx);
                found.push((PurgeKind::StateSnapshot, half));
            }
        }
    }
    for record in [
        paths.target_record(&entry.key),
        paths.files_index(&entry.key),
    ] {
        if fs::symlink_metadata(&record).is_ok_and(|m| m.is_file()) {
            found.push((PurgeKind::Record, record));
        }
    }

    found.sort_by(|a, b| a.1.cmp(&b.1));
    kept.sort();
    undo_lost.sort();
    undo_lost.dedup();
    let mut deletions = Vec::with_capacity(found.len());
    let mut identities = Vec::with_capacity(found.len());
    let mut victims = Vec::new();
    let mut total = 0u64;
    for (kind, path) in found {
        let bytes = apparent_bytes(&path, &mut budget)?;
        total = total.saturating_add(bytes);
        identities.push((path.clone(), FileId::of(&path)));
        if kind != PurgeKind::Trash {
            victims.push(path.clone());
        }
        deletions.push(PurgeDeletion { kind, path, bytes });
    }
    let backups = backups(ctx, &client, &deletions)?;
    Ok(Plan {
        data: PurgeData {
            deletions,
            backups,
            lost_undos: undo_lost,
            kept_entries: kept,
            bytes: total,
        },
        victims,
        identities,
        meta,
    })
}

/// The backups among `deletions`, newest capture first, as `backup list` reads them. A
/// backup a reader holds refuses the purge: it is never deleted under a reader.
fn backups(
    ctx: &Context,
    client: &ClientPaths,
    deletions: &[PurgeDeletion],
) -> Result<Vec<PurgeBackup>> {
    let (listed, warnings) = backup::list(client);
    ctx.notes.warn_all(&warnings);
    let going = |dir: &Path| {
        deletions
            .iter()
            .any(|d| d.kind == PurgeKind::Backup && d.path == dir)
    };
    let mut out = Vec::new();
    for b in listed.into_iter().filter(|b| going(&b.dir)) {
        if b.busy {
            return Err(Error::new(
                ErrorCode::Locked,
                format!(
                    "backup {} is being read by another omaddons process; nothing was deleted",
                    b.id
                ),
                Suggestion::text("wait for that command to finish, then purge again"),
            ));
        }
        out.push(PurgeBackup {
            id: b.id,
            kind: b.meta.as_ref().map(|m| m.kind),
            captured_at: b.captured_at(),
            path: b.dir,
        });
    }
    Ok(out)
}

/// What a directory entry is, never following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    File,
    Dir,
    Link,
    Other,
}

/// The entries of `dir`, each counted against `budget`; none when it is absent.
fn children(dir: &Path, budget: &mut Budget<'_>) -> Result<Vec<(PathBuf, Entry)>> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(Error::io(
                dir,
                &err,
                Suggestion::command(["omaddons", "status"]),
            ));
        }
    };
    let mut out = Vec::new();
    for entry in listing {
        let entry = entry
            .map_err(|err| Error::io(dir, &err, Suggestion::command(["omaddons", "status"])))?;
        budget.take()?;
        let kind = match entry.file_type() {
            Ok(t) if t.is_symlink() => Entry::Link,
            Ok(t) if t.is_dir() => Entry::Dir,
            Ok(t) if t.is_file() => Entry::File,
            _ => Entry::Other,
        };
        out.push((entry.path(), kind));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Apparent bytes of the regular files at or below `path`, links never followed; a
/// directory that cannot be listed adds nothing.
fn apparent_bytes(path: &Path, budget: &mut Budget<'_>) -> Result<u64> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(0);
    };
    if !meta.is_dir() {
        return Ok(if meta.is_file() { meta.len() } else { 0 });
    }
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            budget.take()?;
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(entry.path()),
                Ok(t) if t.is_file() => {
                    total = total.saturating_add(entry.metadata().map_or(0, |m| m.len()));
                }
                _ => {}
            }
        }
    }
    Ok(total)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `<tx>.repair`: a repair's client half.
fn repair_half(name: &str) -> bool {
    name.strip_suffix(REPAIR_SUFFIX)
        .is_some_and(|tx| tx.parse::<TxId>().is_ok())
}

/// What the purge planned that is still there after it ran: a victim at its path (a
/// prune skipped it), or an entry of `.omaddons/` the plan did not keep (what a failed
/// delete left in its trash). Compared below `.omaddons`, which fsx names canonically.
fn unfinished(plan: &Plan, left: &[PathBuf]) -> Vec<PathBuf> {
    let canonical = fs::canonicalize(&plan.meta).ok();
    let below = |path: &Path, meta: &Path| path.strip_prefix(meta).ok().map(Path::to_path_buf);
    let kept: Vec<PathBuf> = plan
        .data
        .kept_entries
        .iter()
        .filter_map(|p| below(p, &plan.meta))
        .collect();
    let mut out: Vec<PathBuf> = left
        .iter()
        .filter(|p| {
            let rel = canonical.as_deref().and_then(|meta| below(p, meta));
            rel.is_none_or(|rel| !kept.contains(&rel))
        })
        .cloned()
        .collect();
    out.extend(
        plan.victims
            .iter()
            .filter(|v| fs::symlink_metadata(v).is_ok())
            .cloned(),
    );
    out.sort();
    out.dedup();
    out
}

/// Stops the run at failpoint `name`; nothing it planned to do after is done.
fn stop_at(name: &str, preview: &[String]) -> Result<()> {
    fsx::failpoint::hit(name).map_err(|err| {
        Error::new(
            ErrorCode::Io,
            format!("targets rm --purge stopped at {name}: {err}"),
            Suggestion::command(preview.to_vec()),
        )
    })
}

/// `--purge` named a client discovery finds.
fn installed(id: &str) -> Error {
    Error::new(
        ErrorCode::Usage,
        format!(
            "{id} is installed: --purge deletes only what omaddons kept for a client that is no longer installed"
        ),
        Suggestion::command(["omaddons", "targets", "rm", id]),
    )
    .details(serde_json::json!({ "flag": "--purge", "id": id }))
    .user_verb()
}

fn refused(message: &str, flag: &str, fix: Vec<String>) -> Error {
    Error::new(ErrorCode::Usage, message, Suggestion::command(fix))
        .details(serde_json::json!({ "flag": flag }))
        .user_verb()
}

/// The purge deleted what it could and left `paths`; the client stays registered, so the
/// next purge finishes.
fn not_finished(id: &str, paths: &[PathBuf], preview: Vec<String>) -> Error {
    let first = paths
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Error::new(
        ErrorCode::Io,
        format!(
            "the purge of {id} left {} path(s) in place, {first} first; {id} stays registered, and purging it again finishes the job",
            paths.len()
        ),
        Suggestion::command(preview),
    )
    .details(serde_json::json!({ "path": first }))
}

/// The human view of a purge, under `render_rm`'s first line.
pub(super) fn render(data: &RmData, purge: &PurgeData, w: &mut dyn Write) -> std::io::Result<()> {
    let size = crate::app::backup::size;
    let verb = if data.dry_run {
        "would delete"
    } else {
        "deleted"
    };
    writeln!(
        w,
        "{verb} what omaddons kept for {}: {} path(s), {}",
        data.id,
        purge.deletions.len(),
        size(purge.bytes)
    )?;
    for d in &purge.deletions {
        writeln!(
            w,
            "  {:<13} {}  {}",
            d.kind.text(),
            d.path.display(),
            size(d.bytes)
        )?;
    }
    if !purge.backups.is_empty() && data.dry_run {
        writeln!(
            w,
            "its backups (copy one out of its folder first to keep it):"
        )?;
        let now = crate::clock::now();
        for b in &purge.backups {
            let kind = b.kind.map_or_else(
                || "unreadable".to_owned(),
                |k| format!("{k:?}").to_lowercase(),
            );
            let when = b.captured_at.map_or_else(
                || "capture time unknown".to_owned(),
                |at| format!("captured {}", crate::app::backup::when(at, now)),
            );
            writeln!(w, "  {} ({kind}, {when})  {}", b.id, b.path.display())?;
        }
    }
    match purge.lost_undos.len() {
        0 => {}
        1 => writeln!(
            w,
            "the 1 transaction that touched it can no longer be undone"
        )?,
        n => writeln!(
            w,
            "the {n} transactions that touched it can no longer be undone"
        )?,
    }
    for k in &purge.kept_entries {
        writeln!(w, "kept (omaddons did not write it): {}", k.display())?;
    }
    if let Some(token) = &data.precondition {
        writeln!(
            w,
            "precondition {token} (delete exactly this: --yes --if-match {token})"
        )?;
    }
    Ok(())
}
