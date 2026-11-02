//! `omaddons undo [<tx> | --run <run> | --last-by <actor>] [--saved-variables
//! auto|restore|keep]`: reverts transactions through the engine (vision §5.5), each as a
//! transaction of its own that can be undone in turn. The output always names what was
//! reverted, and reports SavedVariables by count only: their paths hold account, realm
//! and character names.
//!
//! What it reverts (M2-D38): bare `undo` reverts the most recent run, every transaction
//! of the command invocation that made it (one per target), newest first; `--last-by`
//! reverts that actor's most recent run; `--run` names a run. `undo <tx>` reverts one
//! transaction, and `-t` the newest on that target, as in M1. A transaction from before
//! M2 is a run by itself, so after a single-target command bare `undo` is M1's. The undo
//! transactions of one invocation share a new run, so a second `undo` reverts them all.
//! When one transaction of a run cannot be undone, the others still are (`partial`).
//! `<tx>` and `<run>` are full ids or unique prefixes of at least 8 characters.

use std::io::Write;
use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use super::approval::{Approval, ApprovalFlags};
use super::engine::Engine;
use super::history::{MIN_PREFIX, TargetRef, kind_name, local_time, resolve_run, short_tx};
use super::intent::{self, Preview, Storable, StorableArgs, StoredIntent, UndoSelection};
use super::outcome::ItemError;
use super::precondition;
use super::world::World;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::doctor::runtime::DoctorRunId;
use crate::engine::repair::undo::{Dest, is_repair_family};
use crate::engine::{SvPolicy, UndoOutcome as EngineUndo, UndoRequest, UndoSelector, WtfUndo};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, FolderName, PlanId, ReloadRequired, RunId, TargetKey, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::state::history::{
    self, HistoryEntry, HistoryFilter, RepairEntry, RepairOp, SettingEntry,
};
use crate::state::plans::NotStored;

/// `undo`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UndoArgs {
    /// The transaction to revert: its id or a unique prefix of 8+ characters
    /// (default: the most recent run)
    #[arg(value_name = "TX")]
    pub tx: Option<String>,
    /// Revert this actor's most recent run: human, agent or widget
    #[arg(
        long,
        value_name = "ACTOR",
        conflicts_with = "tx",
        value_parser = clap::builder::PossibleValuesParser::new(Actor::WORDS)
    )]
    pub last_by: Option<String>,
    /// Revert every transaction of this run: its id or a unique prefix of 8+ characters
    #[arg(long, value_name = "RUN", conflicts_with_all = ["tx", "last_by"])]
    pub run: Option<String>,
    /// SavedVariables: auto (restore what changed, later if the game runs), restore
    /// (refuse while the game runs) or keep (never touch them)
    #[arg(long, value_name = "POLICY", value_parser = ["auto", "restore", "keep"])]
    pub saved_variables: Option<String>,
}

/// `undo`'s data. The fields up to `atomic_swap` describe the first transaction undone
/// (M1's shape); `transactions` lists every one.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UndoData {
    pub dry_run: bool,
    /// What the first transaction undone was.
    pub kind: UndoKind,
    pub reverted: Reverted,
    /// The new undo transaction; `null` under `--dry-run`.
    pub tx: Option<TxId>,
    pub folders: UndoFolders,
    pub saved_variables: SavedVariables,
    /// The undo of a whole `WTF/` restore only: the files the play since it changed,
    /// which this undo reverts too and keeps in its own snapshot; `null` when that cannot
    /// be told (the backup it restored is gone). Omitted unless the first transaction
    /// undone is a whole `WTF/` restore or the undo of one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<Option<u64>>,
    pub reload_required: ReloadRequired,
    /// `null` when no swap happened.
    pub atomic_swap: Option<bool>,
    /// `-v` only: the target's client dir.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dir: Option<PathBuf>,
    /// The undo's own run, shared by its undo transactions; `null` under `--dry-run`.
    pub run: Option<RunId>,
    /// The run reverted; `null` for `undo <tx>`, for `-t`, and for a transaction from
    /// before M2.
    pub reverted_run: Option<RunId>,
    /// The undo of a repair: its inverse operations in execution order, the same in a
    /// preview and in the run (M4-D44's canonical form, which the token binds); `null`
    /// for `folders`. History's `undo` line records them as committed.
    pub repairs: Option<Vec<UndoRepairOp>>,
    /// The undo of a repair: its own doctor run directory (kind `undo`); `null` for
    /// `folders` and under `--dry-run`.
    pub doctor_run_id: Option<DoctorRunId>,
    /// Every transaction reverted (or tried), in the order they were undone.
    pub transactions: Vec<UndoTx>,
    /// `--dry-run`: the token `--if-match` takes to bind the real run to this preview
    /// (M3-D46); `null` for a real run.
    pub precondition: Option<String>,
    /// The stored preview's id; `null` on a real run or when no plan was stored.
    pub plan_id: Option<PlanId>,
    /// When the stored preview expires; `null` when `plan_id` is.
    #[schemars(with = "Option<String>")]
    pub plan_expires_at: Option<Timestamp>,
    /// What approving this preview covers; `null` on a real run.
    pub approval: Option<Approval>,
}

/// One inverse operation of the undo of a repair, as a preview and the run both show it:
/// nothing in it names the undo's own run or transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UndoRepairOp {
    /// 1-based, in execution order.
    pub seq: u32,
    /// The reverted fix it belongs to.
    pub fix: u32,
    /// The failure mode that fix repaired.
    pub fm: String,
    pub op: RepairOp,
    /// What it acts on, absolute: a rename's source.
    pub path: PathBuf,
    /// A rename's destination: the absolute path, or `<side>/<path below it>` for one in
    /// the undo's own quarantine (`state`, `config`, `cache`, `skill` or `client`), as
    /// `doctor fix` names one; `null` for a write or a chmod.
    pub to_rel: Option<String>,
    /// The mode a write or a chmod sets, as four octal digits (`"0644"`); `null` for a
    /// rename.
    #[schemars(pattern(r"^[0-7]{4}$"))]
    pub octal_mode: Option<String>,
    /// The rename that puts a set-aside journal back under its own name, last.
    pub activation: bool,
}

impl UndoRepairOp {
    fn of(inverse: &crate::engine::repair::undo::InverseStep) -> Self {
        let op = match inverse.kind.op() {
            "write_file" => RepairOp::WriteFile,
            "chmod" => RepairOp::Chmod,
            _ => RepairOp::RenameNoReplace,
        };
        let to_rel = match &inverse.to {
            Dest::None => None,
            Dest::Path(path) => Some(path.display().to_string()),
            Dest::Quarantine { side, rel } => Some(format!("{}/{}", side.name(), rel.display())),
        };
        Self {
            seq: inverse.seq,
            fix: inverse.fix,
            fm: inverse.fm.clone(),
            op,
            path: inverse.path.clone(),
            to_rel,
            octal_mode: inverse.mode.map(|mode| mode.to_string()),
            activation: inverse.activation,
        }
    }
}

/// What an undo reverts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UndoKind {
    /// Every undo M1–M3 knew: folders, records, SavedVariables, a whole `WTF/` restore.
    Folders,
    /// The undo of a repair, or of the undo of one.
    Repair,
}

/// The transaction reverted.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Reverted {
    pub tx: TxId,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: jiff::Timestamp,
    /// The client it changed; `null` for a repair, which names its clients in `targets`.
    pub target: Option<TargetRef>,
    /// Every client it changed, sorted by id then key: a folder line's one target, a
    /// repair's `targets` (none for a repair of omaddons' own files alone).
    pub targets: Vec<TargetRef>,
    pub actor: Actor,
    pub argv: Vec<String>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Folders by what the undo did to them, sorted by key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UndoFolders {
    /// Brought back as they were before the reverted transaction.
    pub restored: Vec<FolderName>,
    /// Folders the reverted transaction had added.
    pub removed: Vec<FolderName>,
}

/// SavedVariables files of the reverted transaction, by count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SavedVariables {
    pub policy: SvPolicy,
    pub restored: usize,
    pub unchanged: usize,
    pub deferred: usize,
    pub kept: usize,
}

/// One transaction of the undo.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UndoTx {
    /// `null` for a repair, which names no single client.
    pub target: Option<TargetRef>,
    /// The transaction reverted.
    pub reverted: TxId,
    /// The undo transaction; `null` when it failed or under `--dry-run`.
    pub tx: Option<TxId>,
    pub status: UndoTxStatus,
    pub error: Option<ItemError>,
}

/// Whether a transaction was undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UndoTxStatus {
    Undone,
    Failed,
}

/// One transaction undone, as M1's output describes it.
#[derive(Debug, Clone)]
struct Undone {
    reverted: Reverted,
    tx: Option<TxId>,
    folders: UndoFolders,
    sv: SavedVariables,
    reload: ReloadRequired,
    atomic_swap: Option<bool>,
    /// The undo of a whole restore.
    wtf: Option<WtfUndo>,
    /// The undo of a repair.
    repair: Option<Box<RepairUndo>>,
}

/// What the undo of a repair did (or would do), for its data and its human output.
#[derive(Debug, Clone)]
struct RepairUndo {
    /// The line reverted is itself an undo (this is a redo): the repair it reverted.
    redo_of: Option<TxId>,
    /// Each inverse primitive as `(op, path, to)`: as committed, or as planned under
    /// `--dry-run`, where a destination in the undo's own quarantine is named by its side.
    ops: Vec<(&'static str, String, Option<String>)>,
    /// The inverse operations, for `data.repairs`.
    data: Vec<UndoRepairOp>,
    /// The undo's own doctor run.
    run: Option<DoctorRunId>,
}

/// One transaction tried: undone, or why not.
#[derive(Debug, Clone)]
enum Attempt {
    Undone(Box<Undone>),
    Failed(Box<Reverted>, Error),
}

/// `undo`'s outcome.
#[derive(Debug)]
pub struct UndoOutcome {
    data: UndoData,
    preview: Option<Box<Preview>>,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    partial: bool,
    attempts: Vec<Attempt>,
    /// Each committed undo transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
}

impl Storable for UndoOutcome {
    fn take_preview(&mut self) -> Option<Preview> {
        self.preview.take().map(|preview| *preview)
    }

    fn record_plan(&mut self, plan_id: PlanId, expires_at: Timestamp, hint: CommandHint) {
        self.data.plan_id = Some(plan_id);
        self.data.plan_expires_at = Some(expires_at);
        self.commands.insert(0, hint);
    }
}

impl Outcome for UndoOutcome {
    type Data = UndoData;

    fn command(&self) -> &'static str {
        "undo"
    }

    fn data(&self) -> &UndoData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        // One transaction: M1's block, with its own undo hint.
        let single = self.attempts.len() == 1;
        for attempt in &self.attempts {
            match attempt {
                Attempt::Undone(u) => match &u.repair {
                    Some(repair) => render_repair(w, u, repair, self.data.dry_run, single)?,
                    None => render_undone(w, u, self.data.dry_run, single)?,
                },
                Attempt::Failed(r, error) => writeln!(
                    w,
                    "could not revert {} ({} by {}){}: {}",
                    short_tx(&r.tx),
                    kind_name(r.kind),
                    r.actor.as_str(),
                    on_clients(r),
                    error.message
                )?,
            }
        }
        if !single && let Some(hint) = self.commands.first() {
            writeln!(w, "undo this: {}", hint.command())?;
        }
        let repaired = self
            .attempts
            .iter()
            .any(|a| matches!(a, Attempt::Undone(u) if u.repair.is_some()));
        if repaired && let Some(token) = &self.data.precondition {
            writeln!(
                w,
                "precondition {token} (pass --if-match to apply exactly this)"
            )?;
        }
        intent::render_plan_line(
            w,
            self.data.plan_id.as_ref(),
            self.data.plan_expires_at.as_ref(),
        )
    }
}

/// ` on <ids>`: the clients `r` changed, or nothing for a repair of omaddons' own files.
fn on_clients(r: &Reverted) -> String {
    if r.targets.is_empty() {
        return String::new();
    }
    let ids: Vec<&str> = r.targets.iter().map(|t| t.id.as_str()).collect();
    format!(" on {}", ids.join(", "))
}

/// The command a history line's `argv` ran, as `doctor fix`: its words after `omaddons`
/// up to the first flag or value.
fn command_of(argv: &[String]) -> String {
    let words: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .take_while(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()))
        .take(2)
        .collect();
    if words.is_empty() {
        "omaddons".to_owned()
    } else {
        words.join(" ")
    }
}

/// The most inverse primitives a repair undo's block lists before `and N more`.
const REPAIR_LINES_SHOWN: usize = 10;

/// The block of the undo of a repair: what it reverted and on which clients, its
/// primitives by kind, each one (at most [`REPAIR_LINES_SHOWN`]), and the run and the
/// transaction it made.
fn render_repair(
    w: &mut dyn Write,
    u: &Undone,
    repair: &RepairUndo,
    dry_run: bool,
    hint: bool,
) -> std::io::Result<()> {
    let r = &u.reverted;
    let verb = if dry_run { "would revert" } else { "reverted" };
    let what = match repair.redo_of {
        Some(repair_tx) => format!(
            "undo {} of repair {}",
            short_tx(&r.tx),
            short_tx(&repair_tx)
        ),
        None => format!("repair {}", short_tx(&r.tx)),
    };
    let mut counts = Vec::new();
    for (op, word) in [("write", "write"), ("chmod", "chmod"), ("rename", "rename")] {
        // ubs:ignore — comparing operation names, not secrets
        let n = repair.ops.iter().filter(|(o, ..)| *o == op).count();
        if n > 0 {
            counts.push(crate::output::count(n, word));
        }
    }
    writeln!(
        w,
        "{verb} {what} ({} by {}, {}){}: {}",
        command_of(&r.argv),
        r.actor.as_str(),
        local_time(r.at),
        on_clients(r),
        counts.join(", ")
    )?;
    for (op, path, to) in repair.ops.iter().take(REPAIR_LINES_SHOWN) {
        match to {
            Some(to) => writeln!(w, "  {op} {path} → {to}")?,
            None => writeln!(w, "  {op} {path}")?,
        }
    }
    if repair.ops.len() > REPAIR_LINES_SHOWN {
        writeln!(w, "  and {} more", repair.ops.len() - REPAIR_LINES_SHOWN)?;
    }
    let mut tail = Vec::new();
    if let Some(run) = &repair.run {
        tail.push(format!("run {}", run.as_str()));
    }
    if let Some(tx) = u.tx {
        tail.push(format!("tx {tx}"));
        if hint {
            tail.push(format!("undo this: omaddons undo {tx}"));
        }
    }
    if !tail.is_empty() {
        writeln!(w, "{}", tail.join(" · "))?;
    }
    Ok(())
}

/// M1's block for one transaction undone; `hint` adds `undo this: …` to its last line.
fn render_undone(w: &mut dyn Write, u: &Undone, dry_run: bool, hint: bool) -> std::io::Result<()> {
    let r = &u.reverted;
    let verb = if dry_run { "would revert" } else { "reverted" };
    writeln!(
        w,
        "{verb} {} ({} by {}, {}){}",
        short_tx(&r.tx),
        kind_name(r.kind),
        r.actor.as_str(),
        local_time(r.at),
        on_clients(r)
    )?;
    if let Some(wtf) = u.wtf {
        render_whole(w, r.kind, wtf, dry_run)?;
        return render_tail(w, u, hint);
    }
    let sv = &u.sv;
    let (restore, remove) = if dry_run {
        ("would restore", "remove")
    } else {
        ("restored", "removed")
    };
    let mut parts = vec![format!(
        "{restore} {}, {remove} {}",
        crate::output::count(u.folders.restored.len(), "folder"),
        u.folders.removed.len()
    )];
    let mut counts = vec![
        if dry_run {
            format!("would restore {}", sv.restored)
        } else {
            format!("{} restored", sv.restored)
        },
        format!("{} unchanged", sv.unchanged),
    ];
    if sv.deferred > 0 {
        counts.push(format!("{} deferred", sv.deferred));
    }
    if sv.kept > 0 {
        counts.push(format!("{} kept", sv.kept));
    }
    parts.push(format!("SavedVariables: {}", counts.join(", ")));
    writeln!(w, "{}", parts.join(" · "))?;
    render_tail(w, u, hint)
}

/// The middle line of the undo of a whole restore (`kind` `restore`), or of the undo of
/// one: `WTF/` put back, and the play since that goes with it.
fn render_whole(
    w: &mut dyn Write,
    kind: TxKind,
    wtf: WtfUndo,
    dry_run: bool,
) -> std::io::Result<()> {
    let (back, since) = match kind {
        TxKind::Restore => ("as it was before the restore", "since the restore"),
        _ => ("as the restore left it", "since that undo"),
    };
    // (one file is reverted, several are, it stays, they stay)
    let (put, is, are, it, they) = if dry_run {
        (
            format!("would put WTF/ back {back}"),
            "would be reverted",
            "would be reverted",
            "it would stay",
            "they would stay",
        )
    } else {
        (
            format!("WTF/ put back {back}"),
            "is reverted",
            "are reverted",
            "it stays",
            "they stay",
        )
    };
    let changed = match wtf.changed_since {
        Some(1) => format!("1 file changed {since} {is} too; {it} in this undo's snapshot"),
        Some(n) => format!("{n} files changed {since} {are} too; {they} in this undo's snapshot"),
        None => format!(
            "files changed {since} cannot be counted (its backup is gone); {they} in this undo's snapshot"
        ),
    };
    writeln!(w, "{put} · {changed}")
}

/// The last line of an undone block: the undo's tx, the reload it needs, and (`hint`) how
/// to undo it.
fn render_tail(w: &mut dyn Write, u: &Undone, hint: bool) -> std::io::Result<()> {
    let mut tail = Vec::new();
    if let Some(tx) = u.tx {
        tail.push(format!("tx {tx}"));
    }
    match u.reload {
        ReloadRequired::Reload => tail.push("/reload in game".to_owned()),
        ReloadRequired::Restart => tail.push("restart the game client".to_owned()),
        ReloadRequired::None => {}
    }
    if hint && let Some(tx) = u.tx {
        tail.push(format!("undo this: omaddons undo {tx}"));
    }
    if !tail.is_empty() {
        writeln!(w, "{}", tail.join(" · "))?;
    }
    Ok(())
}

/// A doctor run id given where `undo` takes a transaction or a run of the history
/// (§6.14.4: the two kinds of run): `usage`, whose fix is the same undo through the
/// doctor's own form. From the arguments alone, before anything is read.
fn refuse_doctor_run(args: &UndoArgs) -> Result<()> {
    for given in [args.run.as_deref(), args.tx.as_deref()]
        .into_iter()
        .flatten()
    {
        // A ULID in any case, as `undo`'s own prefixes are.
        if let Some(run) = DoctorRunId::parse(&given.to_ascii_uppercase()) {
            return Err(Error::new(
                ErrorCode::Usage,
                format!(
                    "{given} is a doctor run directory; `undo` takes a transaction or a run of the history"
                ),
                Suggestion::command(["omaddons", "doctor", "undo", run.as_str()]),
            )
            .user_verb());
        }
    }
    Ok(())
}

/// Runs `undo`.
pub fn run(ctx: &Context, args: &UndoArgs) -> Result<UndoOutcome> {
    refuse_doctor_run(args)?;
    // clap admits only the three actors (its value fix corrects a typo in place); this
    // guard never guesses one.
    let last_by = args
        .last_by
        .as_deref()
        .map(|a| {
            a.parse::<Actor>().map_err(|_| {
                Error::new(
                    ErrorCode::Usage,
                    format!("--last-by is human, agent or widget, not {a:?}"),
                    Suggestion::command(["omaddons", "help", "undo"]),
                )
            })
        })
        .transpose()?;
    let sv_policy = match args.saved_variables.as_deref() {
        Some("restore") => SvPolicy::Restore,
        Some("keep") => SvPolicy::Keep,
        _ => SvPolicy::Auto,
    };
    let world = World::load(ctx)?;
    let read = history::read(&world.paths, &HistoryFilter::default())?;
    // What it chooses from may lack the transactions on lines that did not read: said
    // even when choosing then fails (`undo <tx>` of such a transaction).
    ctx.notes.warn_all(read.skipped_warning().as_slice());
    // A settings change is in the history, but undo never reverts it.
    if let Some(prefix) = args.tx.as_deref() {
        settings_prefix(&read.settings, &read.entries, prefix)?;
    }
    let entries = read.entries;
    let selection = select(
        &entries,
        &Selector {
            tx: args.tx.as_deref(),
            run: args.run.as_deref(),
            last_by,
            target: world.history_key(ctx)?,
        },
    )?;

    let engine = Engine::new(ctx, &world, "undo")?;
    let run = RunId::new();
    let request = |entry: &HistoryEntry, dry_run: bool| UndoRequest {
        selector: UndoSelector::Tx(entry.tx),
        sv_policy,
        dry_run,
        while_running: ctx.while_running,
        reason: ctx.reason.clone(),
        targets: world.resolved.targets.clone(),
        run,
    };
    // `--if-match`: every undo is planned first, as its preview was, and nothing is
    // staged unless that is what the preview showed (M3-D46).
    let binding = match ctx.if_match.as_deref() {
        Some(token) if !ctx.dry_run => {
            let previewed: Vec<(Selected<'_>, Result<EngineUndo>)> = selection
                .entries
                .iter()
                .map(|chosen| (*chosen, engine.undo(&request(chosen.entry, true))))
                .collect();
            let preview = precondition::preview_argv(&ctx.argv);
            precondition::check(Some(token), &undo_precondition(&previewed), &preview)?;
            let tokens = previewed
                .iter()
                .filter_map(|(chosen, result)| {
                    let undo = result.as_ref().ok()?;
                    let one = precondition::of_undo(&[(chosen.entry.tx, chosen.keys(), Ok(undo))]);
                    Some((chosen.entry.tx, one.token()))
                })
                .collect();
            Some(precondition::UndoBinding { tokens, preview })
        }
        _ => None,
    };
    let mut done = Vec::new();
    // Each in its own lock window; a failure does not stop the rest.
    for chosen in &selection.entries {
        let result = engine.undo_bound(
            &request(chosen.entry, ctx.dry_run),
            binding.as_ref().map(|b| b as &dyn crate::engine::Binding),
        );
        done.push((*chosen, result));
    }
    let precondition = ctx.dry_run.then(|| undo_precondition(&done));
    let token = precondition.as_ref().map(|value| value.token());
    let mut outcome = outcome(ctx, &world, &selection, run, sv_policy, done, token)?;
    if let Some(precondition) = precondition {
        // One item per client each reverted transaction changed; one naming no client
        // for a repair of omaddons' own files alone.
        let reverted: Vec<_> = outcome
            .attempts
            .iter()
            .map(|attempt| match attempt {
                Attempt::Undone(undone) => &undone.reverted,
                Attempt::Failed(reverted, _) => reverted,
            })
            .flat_map(|r| {
                if r.targets.is_empty() {
                    vec![(None, String::new(), r.tx)]
                } else {
                    r.targets
                        .iter()
                        .map(|t| (Some(t.key), t.id.clone(), r.tx))
                        .collect()
                }
            })
            .collect();
        let folders_removed: Vec<_> = outcome
            .attempts
            .iter()
            .filter_map(|attempt| match attempt {
                Attempt::Undone(undone) => Some(&undone.folders.removed),
                Attempt::Failed(..) => None,
            })
            .flatten()
            .cloned()
            .collect();
        let flags = ApprovalFlags {
            while_running: ctx.while_running,
            destructive: true,
            ..ApprovalFlags::default()
        };
        let approval = Approval::of_undo(&reverted, &folders_removed, &flags);
        outcome.data.approval = Some(approval.clone());
        let narrowed = ctx.target.is_some();
        let intent = resolved_selection(&outcome.data, narrowed).and_then(|selection| {
            StoredIntent::from_args(ctx, &StorableArgs::Undo { args, selection })
        });
        match intent {
            Ok(intent) => {
                outcome.preview = Some(Box::new(Preview {
                    intent,
                    precondition,
                    releases: Vec::new(),
                    approval,
                    changes: outcome
                        .attempts
                        .iter()
                        .any(|attempt| matches!(attempt, Attempt::Undone(_))),
                }));
            }
            Err(error) => {
                ctx.notes
                    .warn(intent::plan_not_stored(&NotStored::Io(format!(
                        "this preview cannot be stored: {}",
                        error.message
                    ))));
            }
        }
    }
    Ok(outcome)
}

/// Relative selectors have already resolved in the preview. A stored undo names only
/// that full run or transaction, so newer history cannot retarget it. A run `-t`
/// `narrowed` to one client is that client's transaction: a run commits at most one
/// per client, and the run alone would name every client's.
fn resolved_selection(data: &UndoData, narrowed: bool) -> Result<UndoSelection> {
    match data.reverted_run {
        Some(run) if !narrowed => Ok(UndoSelection::Run(run)),
        Some(run) => match data.transactions.as_slice() {
            [one] => Ok(UndoSelection::Tx(one.reverted)),
            other => Err(Error::internal(format!(
                "run {run} narrowed to one client selected {} transactions",
                other.len()
            ))),
        },
        None => data
            .transactions
            .first()
            .map(|tx| UndoSelection::Tx(tx.reverted))
            .ok_or_else(|| Error::internal("an undo preview selected no transaction")),
    }
}

/// The precondition of the undos `planned`, each of a selected transaction.
fn undo_precondition(planned: &[(Selected<'_>, Result<EngineUndo>)]) -> precondition::Precondition {
    let planned: Vec<precondition::UndoPlanned<'_>> = planned
        .iter()
        .map(|(chosen, result)| (chosen.entry.tx, chosen.keys(), result.as_ref()))
        .collect();
    precondition::of_undo(&planned)
}

/// What `undo`'s arguments ask for.
#[derive(Debug, Clone, Copy)]
struct Selector<'a> {
    tx: Option<&'a str>,
    run: Option<&'a str>,
    last_by: Option<Actor>,
    /// `-t`: `Some(None)` names a target that was never registered.
    target: Option<Option<TargetKey>>,
}

/// The transactions to revert, newest first, and the run they make up.
#[derive(Debug)]
struct Selection<'e> {
    entries: Vec<Selected<'e>>,
    reverted_run: Option<RunId>,
}

/// One transaction to revert, and the client it changed: `None` for a repair-family
/// line, whose clients are its `targets`.
#[derive(Debug, Clone, Copy)]
struct Selected<'e> {
    entry: &'e HistoryEntry,
    key: Option<TargetKey>,
}

impl Selected<'_> {
    /// Every client it changed: the one, or a repair's `targets`.
    fn keys(&self) -> &[TargetKey] {
        match &self.key {
            Some(key) => std::slice::from_ref(key),
            None => &self.entry.targets,
        }
    }
}

/// `entries` with the client each changed. A repair-family line names none (the engine
/// routes it to the repair undo); any other target-less line is refused before anything
/// is planned.
fn selected(entries: Vec<&HistoryEntry>) -> Result<Vec<Selected<'_>>> {
    entries
        .into_iter()
        .map(|entry| {
            let key = if is_repair_family(entry) {
                None
            } else {
                Some(crate::engine::undo::target_of(entry)?)
            };
            Ok(Selected { entry, key })
        })
        .collect()
}

/// `<tx>`: that transaction. `--run`: that run's transactions (on the `-t` target when
/// one is given). `-t`: the newest on that target (by `--last-by`). Otherwise the newest
/// transaction (by `--last-by`) and every other of its run.
fn select<'e>(entries: &'e [HistoryEntry], s: &Selector<'_>) -> Result<Selection<'e>> {
    let none = |message: String| {
        Error::new(
            ErrorCode::NotFound,
            message,
            Suggestion::command(["omaddons", "history"]),
        )
    };
    // A target never registered (`Some(None)`) matches nothing; a target-less line (a
    // repair) is on each target it touches.
    let on_target = |e: &HistoryEntry| match s.target {
        Some(Some(key)) => e.touches_target(key),
        Some(None) => false,
        None => true,
    };
    if let Some(prefix) = s.tx {
        let entry = by_prefix(entries, prefix)?;
        if entry.kind == TxKind::Backup {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "transaction {} made a backup, which undo does not revert",
                    entry.tx
                ),
                Suggestion::command(["omaddons", "backup", "list"]),
            ));
        }
        return Ok(Selection {
            entries: selected(vec![entry])?,
            reverted_run: None,
        });
    }
    if let Some(prefix) = s.run {
        let run = resolve_run(entries, prefix)?;
        let of_run: Vec<&HistoryEntry> = entries
            .iter()
            .rev()
            .filter(|e| e.run == Some(run) && e.kind.is_undoable() && on_target(e))
            .collect();
        if of_run.is_empty() {
            return Err(none(format!("run {run} has nothing to undo here")));
        }
        return Ok(Selection {
            entries: selected(of_run)?,
            reverted_run: Some(run),
        });
    }
    let Some(found) = history::newest_run(entries, s.last_by, s.target) else {
        return Err(match s.last_by {
            Some(actor) => none(format!(
                "there is no transaction by {} to undo",
                actor.as_str()
            )),
            None => none("nothing to undo".to_owned()),
        });
    };
    Ok(Selection {
        entries: selected(found.entries)?,
        reverted_run: found.run,
    })
}

/// `not_found` when `prefix` (8+ characters) names exactly one history line and it is a
/// settings change, which undo never reverts; the ambiguity error when it names several,
/// settings changes and transactions together. A prefix unique among transactions is
/// [`by_prefix`]'s to resolve.
fn settings_prefix(
    settings: &[SettingEntry],
    entries: &[HistoryEntry],
    prefix: &str,
) -> Result<()> {
    let wanted = prefix.to_ascii_uppercase();
    if wanted.len() < MIN_PREFIX {
        return Ok(());
    }
    let named = |tx: &TxId| tx.to_string().starts_with(&wanted);
    let matching: Vec<&SettingEntry> = settings.iter().filter(|e| named(&e.tx)).collect();
    let txs: Vec<String> = entries
        .iter()
        .filter(|e| named(&e.tx))
        .map(|e| e.tx.to_string())
        .collect();
    match (matching.as_slice(), txs.len()) {
        ([], _) => Ok(()),
        ([setting], 0) => {
            let kind = crate::app::history::kind_name(setting.kind);
            Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "transaction {} is a {kind} change, which undo does not revert",
                    setting.tx
                ),
                Suggestion::command(["omaddons", "history"]),
            ))
        }
        (many, _) => {
            let mut candidates: Vec<String> = many.iter().map(|e| e.tx.to_string()).collect();
            candidates.extend(txs);
            candidates.sort();
            Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "{prefix} matches {} transactions; give more of the id",
                    candidates.len()
                ),
                Suggestion::command(["omaddons", "history"]),
            )
            .details(serde_json::json!({ "candidates": candidates })))
        }
    }
}

/// The entry whose id is `prefix` or starts with it (8+ characters, any case).
fn by_prefix<'e>(entries: &'e [HistoryEntry], prefix: &str) -> Result<&'e HistoryEntry> {
    let wanted = prefix.to_ascii_uppercase();
    if wanted.len() < MIN_PREFIX {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "a transaction id prefix needs at least {MIN_PREFIX} characters, not {prefix:?}"
            ),
            Suggestion::command(["omaddons", "history"]),
        ));
    }
    let matching: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|e| e.tx.to_string().starts_with(&wanted))
        .collect();
    match matching.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::new(
            ErrorCode::NotFound,
            format!("there is no transaction {prefix} in the history"),
            Suggestion::command(["omaddons", "history"]),
        )),
        many => {
            let candidates: Vec<String> = many.iter().map(|e| e.tx.to_string()).collect();
            Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "{prefix} matches {} transactions; give more of the id",
                    many.len()
                ),
                Suggestion::command(["omaddons", "history"]),
            )
            .details(serde_json::json!({ "candidates": candidates })))
        }
    }
}

/// The outcome of every transaction tried: the first undone fills M1's fields; any
/// failure beside one undone makes the run partial; all failed is the first error.
/// `precondition` is a dry run's token.
fn outcome(
    ctx: &Context,
    world: &World,
    selection: &Selection<'_>,
    run: RunId,
    policy: SvPolicy,
    done: Vec<(Selected<'_>, Result<EngineUndo>)>,
    precondition: Option<String>,
) -> Result<UndoOutcome> {
    let verbose = ctx.output.style.verbose > 0;
    let mut attempts = Vec::new();
    let mut transactions = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();
    let mut locks = Vec::new();
    let mut first_error = None;
    for (chosen, result) in done {
        let entry = chosen.entry;
        let reverted = reverted(world, chosen, verbose);
        match result {
            Ok(undo) => {
                warnings.extend(undo.outcome.warnings.iter().cloned());
                if let (Some(tx), Some(ms)) = (undo.outcome.tx, undo.outcome.lock_held_ms) {
                    locks.push((tx, ms));
                }
                transactions.push(UndoTx {
                    target: reverted.target.clone(),
                    reverted: entry.tx,
                    tx: undo.outcome.tx,
                    status: UndoTxStatus::Undone,
                    error: None,
                });
                attempts.push(Attempt::Undone(Box::new(undone(
                    reverted,
                    entry.reverts,
                    policy,
                    &undo,
                ))));
            }
            Err(error) => {
                let mut warning = Warning::item(&error);
                let ids: Vec<&str> = reverted.targets.iter().map(|t| t.id.as_str()).collect();
                warning.message = if ids.is_empty() {
                    error.message.clone()
                } else {
                    format!("{}: {}", ids.join(", "), error.message)
                };
                warnings.push(warning);
                transactions.push(UndoTx {
                    target: reverted.target.clone(),
                    reverted: entry.tx,
                    tx: None,
                    status: UndoTxStatus::Failed,
                    error: Some(ItemError::from(&error)),
                });
                first_error.get_or_insert_with(|| error.clone());
                attempts.push(Attempt::Failed(Box::new(reverted), error));
            }
        }
    }
    let Some(first) = attempts.iter().find_map(|a| match a {
        Attempt::Undone(u) => Some(u.as_ref().clone()),
        Attempt::Failed(..) => None,
    }) else {
        return Err(first_error.unwrap_or_else(|| Error::internal("undo tried nothing")));
    };
    let partial = first_error.is_some();
    let committed: Vec<TxId> = transactions.iter().filter_map(|t| t.tx).collect();
    let mut commands = Vec::new();
    if committed.len() > 1 {
        commands.push(CommandHint::new(
            Action::UndoRun,
            [
                "omaddons".to_owned(),
                "undo".to_owned(),
                "--run".to_owned(),
                run.to_string(),
            ],
        ));
    }
    for tx in &committed {
        commands.push(CommandHint::new(
            Action::Undo,
            ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
        ));
    }
    let client_dir = verbose
        .then(|| {
            let key = first.reverted.target.as_ref()?.key;
            world
                .resolved
                .targets
                .iter()
                // ubs:ignore — client identity keys are public identifiers, not credentials
                .find(|d| d.target.key == Some(key))
                .map(|d| d.target.client_dir.clone())
        })
        .flatten();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    let (kind, repairs, doctor_run_id) = match &first.repair {
        Some(repair) => (
            UndoKind::Repair,
            Some(repair.data.clone()),
            repair.run.clone(),
        ),
        None => (UndoKind::Folders, None, None),
    };
    Ok(UndoOutcome {
        data: UndoData {
            dry_run: ctx.dry_run,
            kind,
            reverted: first.reverted,
            tx: first.tx,
            folders: first.folders,
            saved_variables: first.sv,
            changed_since: first.wtf.map(|wtf| wtf.changed_since),
            reload_required: first.reload,
            atomic_swap: first.atomic_swap,
            client_dir,
            run: (!ctx.dry_run).then_some(run),
            reverted_run: selection.reverted_run,
            repairs,
            doctor_run_id,
            transactions,
            precondition,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        },
        preview: None,
        warnings: unique,
        commands,
        partial,
        attempts,
        locks,
    })
}

/// How the history names the transaction reverted.
fn reverted(world: &World, chosen: Selected<'_>, verbose: bool) -> Reverted {
    let entry = chosen.entry;
    let named = |key: TargetKey| TargetRef {
        id: world.target_id(key),
        key,
    };
    let mut targets: Vec<TargetRef> = chosen.keys().iter().copied().map(named).collect();
    targets.sort_by(|a, b| (a.id.as_str(), a.key).cmp(&(b.id.as_str(), b.key)));
    // ubs:ignore — client identity keys are public identifiers, not credentials
    targets.dedup_by(|a, b| a.key == b.key);
    Reverted {
        tx: entry.tx,
        kind: entry.kind,
        at: entry.at,
        target: chosen.key.map(named),
        targets,
        actor: entry.actor,
        argv: entry.argv.clone(),
        reason: entry.reason.clone().filter(|_| verbose),
    }
}

/// One transaction undone; `reverted_reverts` is what the reverted line itself reverts
/// (an undo's), so a redo of a repair names the repair.
fn undone(
    reverted: Reverted,
    reverted_reverts: Option<TxId>,
    policy: SvPolicy,
    done: &EngineUndo,
) -> Undone {
    let mut folders = UndoFolders::default();
    for change in &done.outcome.changes {
        folders
            .restored
            .extend(change.folders.added.iter().cloned());
        folders
            .restored
            .extend(change.folders.replaced.iter().cloned());
        folders
            .removed
            .extend(change.folders.removed.iter().cloned());
    }
    folders.restored.sort_by(|a, b| a.key().cmp(b.key()));
    folders.removed.sort_by(|a, b| a.key().cmp(b.key()));
    let tx = done.outcome.tx;
    let repair = done.repair.as_ref().map(|r| {
        Box::new(RepairUndo {
            redo_of: (r.plan.plan.reverted_kind == TxKind::Undo)
                .then_some(reverted_reverts)
                .flatten(),
            ops: if r.committed.is_empty() {
                r.plan.plan.inverses.iter().map(planned_op).collect()
            } else {
                r.committed.iter().map(committed_op).collect()
            },
            data: r.plan.plan.inverses.iter().map(UndoRepairOp::of).collect(),
            run: r.undo_run.as_deref().and_then(DoctorRunId::parse),
        })
    });
    Undone {
        reverted,
        tx,
        folders,
        sv: SavedVariables {
            policy,
            restored: done.sv.restored,
            unchanged: done.sv.unchanged,
            deferred: done.sv.deferred,
            kept: done.sv.kept,
        },
        reload: done.outcome.reload_required,
        // A repair's undo swaps nothing (M1's atomic swap is a folder transaction's).
        atomic_swap: tx
            .filter(|_| repair.is_none())
            .map(|_| done.outcome.atomic_swap),
        wtf: done.wtf,
        repair,
    }
}

/// The word a human line names an operation by.
const fn op_word(op: RepairOp) -> &'static str {
    match op {
        RepairOp::WriteFile => "write",
        RepairOp::Chmod => "chmod",
        RepairOp::RenameNoReplace => "rename",
    }
}

/// A committed inverse primitive, as its human line shows it.
fn committed_op(entry: &RepairEntry) -> (&'static str, String, Option<String>) {
    (
        op_word(entry.op),
        entry.path.display().to_string(),
        entry.to.as_ref().map(|to| to.display().to_string()),
    )
}

/// A planned inverse primitive, as a dry run's human line shows it: a destination in
/// the undo's own quarantine, which the run names, by its side.
fn planned_op(
    inverse: &crate::engine::repair::undo::InverseStep,
) -> (&'static str, String, Option<String>) {
    let word = match inverse.kind.op() {
        "write_file" => "write",
        "chmod" => "chmod",
        _ => "rename",
    };
    let to = match &inverse.to {
        Dest::None => None,
        Dest::Path(path) => Some(path.display().to_string()),
        Dest::Quarantine { side, rel } => Some(format!(
            "this undo's quarantine ({}/{})",
            side.name(),
            rel.display()
        )),
    };
    (word, inverse.path.display().to_string(), to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::history::TxResult;

    fn entry(tx: &str, actor: Actor, target: TargetKey, kind: TxKind) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: tx.parse().unwrap(),
            kind,
            at: "2026-10-14T12:00:00Z".parse().unwrap(),
            target: Some(target),
            targets: Vec::new(),
            actor,
            reason: None,
            argv: vec!["omaddons".into()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: None,
            repairs: Vec::new(),
        }
    }

    fn pick(
        entries: &[HistoryEntry],
        tx: Option<&str>,
        last_by: Option<Actor>,
        target: Option<Option<TargetKey>>,
    ) -> std::result::Result<Vec<String>, ErrorCode> {
        let selector = Selector {
            tx,
            run: None,
            last_by,
            target,
        };
        select(entries, &selector)
            .map(|s| s.entries.iter().map(|e| e.entry.tx.to_string()).collect())
            .map_err(|e| e.code)
    }

    #[test]
    fn an_undo_preview_resolves_its_selection() {
        let target = TargetRef {
            id: "retail".into(),
            key: TargetKey::new(),
        };
        let tx = "01JB8Q2VA00000000000000001".parse::<TxId>().unwrap();
        let run = "01JB8Q2VC00000000000000000".parse::<RunId>().unwrap();
        let mut data = UndoData {
            dry_run: true,
            kind: UndoKind::Folders,
            reverted: Reverted {
                tx,
                kind: TxKind::Install,
                at: "2026-10-14T12:00:00Z".parse().unwrap(),
                target: Some(target.clone()),
                targets: vec![target.clone()],
                actor: Actor::Agent,
                argv: vec!["omaddons".into(), "install".into(), "Foo".into()],
                reason: None,
            },
            tx: None,
            folders: UndoFolders::default(),
            saved_variables: SavedVariables {
                policy: SvPolicy::Auto,
                restored: 0,
                unchanged: 0,
                deferred: 0,
                kept: 0,
            },
            changed_since: None,
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            client_dir: None,
            run: None,
            reverted_run: None,
            repairs: None,
            doctor_run_id: None,
            transactions: vec![UndoTx {
                target: Some(target),
                reverted: tx,
                tx: None,
                status: UndoTxStatus::Undone,
                error: None,
            }],
            precondition: None,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        };
        for (case, reverted_run, narrowed, expected) in [
            (
                "bare, last-by or --run resolved a run",
                Some(run),
                false,
                serde_json::json!({"run": run}),
            ),
            (
                "--run narrowed by -t is its client's transaction",
                Some(run),
                true,
                serde_json::json!({"tx": tx}),
            ),
            (
                "a prefix resolved a transaction",
                None,
                false,
                serde_json::json!({"tx": tx}),
            ),
            (
                "-t resolved a transaction",
                None,
                true,
                serde_json::json!({"tx": tx}),
            ),
        ] {
            data.reverted_run = reverted_run;
            let selected = resolved_selection(&data, narrowed).unwrap();
            assert_eq!(
                serde_json::to_value(selected).unwrap(),
                expected,
                "{case}: narrowed={narrowed}, {data:?} resolved to {selected:?}"
            );
        }
        // A narrowed run names exactly one transaction, never the run again.
        data.reverted_run = Some(run);
        data.transactions.push(data.transactions[0].clone());
        assert_eq!(
            resolved_selection(&data, true).unwrap_err().code,
            ErrorCode::Internal,
            "two transactions of a narrowed run: {data:?}"
        );
        data.transactions.clear();
        for (reverted_run, narrowed) in [(None, false), (None, true), (Some(run), true)] {
            data.reverted_run = reverted_run;
            assert_eq!(
                resolved_selection(&data, narrowed).unwrap_err().code,
                ErrorCode::Internal,
                "an incomplete preview cannot retarget a stored undo: narrowed={narrowed}, {data:?}"
            );
        }
    }

    /// A prefix is matched against settings changes and transactions together: unique
    /// to a settings change it is refused as one, shared it is ambiguous.
    #[test]
    fn a_settings_prefix_is_matched_with_the_transactions() {
        let key = TargetKey::new();
        let setting = |tx: &str| SettingEntry {
            tx: tx.parse().unwrap(),
            ..SettingEntry::now(TxKind::Config, None, Actor::Human, None, Vec::new())
        };
        let txs = vec![entry(
            "01JB8Q2VA00000000000000001",
            Actor::Human,
            key,
            TxKind::Install,
        )];
        let settings = vec![
            setting("01JB8Q2VA00000000000000002"),
            setting("01JB8Q2VB00000000000000003"),
        ];
        let code = |prefix: &str| {
            settings_prefix(&settings, &txs, prefix)
                .map_err(|e| (e.code, e.details["candidates"].as_array().map(Vec::len)))
        };
        assert_eq!(
            code("01JB8Q2VB"),
            Err((ErrorCode::NotFound, None)),
            "one setting"
        );
        assert_eq!(
            code("01JB8Q2VA"),
            Err((ErrorCode::NotFound, Some(2))),
            "a setting and a transaction"
        );
        assert_eq!(code("01JB8Q2V"), Err((ErrorCode::NotFound, Some(3))));
        assert_eq!(
            code("01JB8Q2VA00000000000000001"),
            Ok(()),
            "the transaction"
        );
    }

    #[test]
    fn selection() {
        let (retail, classic) = (TargetKey::new(), TargetKey::new());
        let entries = vec![
            entry(
                "01JB8Q2VA00000000000000001",
                Actor::Agent,
                retail,
                TxKind::Install,
            ),
            entry(
                "01JB8Q2VA00000000000000002",
                Actor::Human,
                classic,
                TxKind::Install,
            ),
            entry(
                "01JB8Q2VB00000000000000003",
                Actor::Human,
                retail,
                TxKind::Backup,
            ),
        ];
        let id = |n: u8| format!("01JB8Q2VA0000000000000000{n}");
        assert_eq!(
            pick(&entries, None, None, None),
            Ok(vec![id(2)]),
            "backups are not undoable; a line from before M2 is a run by itself"
        );
        assert_eq!(
            pick(&entries, None, None, Some(Some(retail))),
            Ok(vec![id(1)]),
            "-t narrows"
        );
        assert_eq!(
            pick(&entries, None, Some(Actor::Agent), None),
            Ok(vec![id(1)])
        );
        assert_eq!(
            pick(&entries, None, Some(Actor::Widget), None),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            pick(&entries, None, None, Some(None)),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(pick(&[], None, None, None), Err(ErrorCode::NotFound));

        // Prefixes: unique, any case; ambiguous and unknown are not found; short is usage.
        assert_eq!(
            pick(&entries, Some("01jb8q2va00000000000000002"), None, None),
            Ok(vec![id(2)])
        );
        assert_eq!(
            pick(&entries, Some("01jb8q2vb"), None, None),
            Err(ErrorCode::NotFound),
            "a backup's line is no transaction to undo"
        );
        assert_eq!(pick(&entries, Some(&id(2)), None, None), Ok(vec![id(2)]));
        let ambiguous = by_prefix(&entries, "01JB8Q2VA").unwrap_err();
        assert_eq!(ambiguous.code, ErrorCode::NotFound);
        assert_eq!(ambiguous.details["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(
            pick(&entries, Some("01JB8Q2VZ"), None, None),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            pick(&entries, Some("01JB8Q2"), None, None),
            Err(ErrorCode::Usage)
        );
    }

    #[test]
    fn runs_are_selected_whole() {
        let (retail, classic, era) = (TargetKey::new(), TargetKey::new(), TargetKey::new());
        let (agent_run, human_run) = (
            "01JB8Q2VC00000000000000000".parse::<RunId>().unwrap(),
            "01JB8Q2VD00000000000000000".parse::<RunId>().unwrap(),
        );
        let with_run = |tx: &str, actor, target, run| HistoryEntry {
            run: Some(run),
            ..entry(tx, actor, target, TxKind::Update)
        };
        let entries = vec![
            entry(
                "01JB8Q2VA00000000000000001",
                Actor::Human,
                retail,
                TxKind::Install,
            ),
            with_run(
                "01JB8Q2VA00000000000000002",
                Actor::Agent,
                retail,
                agent_run,
            ),
            with_run(
                "01JB8Q2VA00000000000000003",
                Actor::Agent,
                classic,
                agent_run,
            ),
            with_run("01JB8Q2VA00000000000000004", Actor::Agent, era, agent_run),
            with_run(
                "01JB8Q2VA00000000000000005",
                Actor::Human,
                classic,
                human_run,
            ),
        ];
        let id = |n: u8| format!("01JB8Q2VA0000000000000000{n}");
        let select_with = |s: Selector<'_>| {
            select(&entries, &s).map(|sel| {
                (
                    sel.entries
                        .iter()
                        .map(|e| e.entry.tx.to_string())
                        .collect::<Vec<_>>(),
                    sel.reverted_run,
                )
            })
        };
        let bare = Selector {
            tx: None,
            run: None,
            last_by: None,
            target: None,
        };
        assert_eq!(
            select_with(bare).unwrap(),
            (vec![id(5)], Some(human_run)),
            "the newest run, a run of one"
        );
        assert_eq!(
            select_with(Selector {
                last_by: Some(Actor::Agent),
                ..bare
            })
            .unwrap(),
            (vec![id(4), id(3), id(2)], Some(agent_run)),
            "the agent's run, newest first"
        );
        assert_eq!(
            select_with(Selector {
                run: Some("01jb8q2vc"),
                ..bare
            })
            .unwrap(),
            (vec![id(4), id(3), id(2)], Some(agent_run))
        );
        assert_eq!(
            select_with(Selector {
                run: Some("01JB8Q2VC"),
                target: Some(Some(classic)),
                ..bare
            })
            .unwrap(),
            (vec![id(3)], Some(agent_run)),
            "--run with -t: that run's transaction on the target"
        );
        assert_eq!(
            select_with(Selector {
                target: Some(Some(retail)),
                ..bare
            })
            .unwrap(),
            (vec![id(2)], None),
            "-t: the newest transaction there, as in M1"
        );
        let unknown = select_with(Selector {
            run: Some("01JB8Q2VZ"),
            ..bare
        })
        .unwrap_err();
        assert_eq!(unknown.code, ErrorCode::NotFound);
        assert_eq!(unknown.fix.argv().map(<[String]>::len), Some(2));
        let short = select_with(Selector {
            run: Some("01JB"),
            ..bare
        })
        .unwrap_err();
        assert_eq!(short.code, ErrorCode::Usage);
    }

    /// A repair (target-less, M4-D31) is the newest change (M4-D46): every selector that
    /// reaches it selects it, with no key (the engine routes it), never stepping over it
    /// to an older transaction; a target it did not touch still selects its own.
    #[test]
    fn select_selects_a_repair_line() {
        let a = TargetKey::new();
        let c = TargetKey::new();
        let run = RunId::new();
        let on_a = entry(
            "01JB8Q2VA00000000000000001",
            Actor::Agent,
            a,
            TxKind::Install,
        );
        let on_c = entry(
            "01JB8Q2VA00000000000000002",
            Actor::Agent,
            c,
            TxKind::Install,
        );
        let repair = HistoryEntry {
            target: None,
            targets: vec![a],
            run: Some(run),
            ..entry(
                "01JB8Q2VA00000000000000003",
                Actor::Human,
                a,
                TxKind::Repair,
            )
        };
        let entries = vec![on_a, on_c.clone(), repair.clone()];
        let tx_prefix = repair.tx.to_string();
        let run_prefix = run.to_string();
        let bare = Selector {
            tx: None,
            run: None,
            last_by: None,
            target: None,
        };
        let cases = [
            (
                "tx_prefix",
                Selector {
                    tx: Some(&tx_prefix),
                    ..bare
                },
            ),
            (
                "run",
                Selector {
                    run: Some(&run_prefix),
                    ..bare
                },
            ),
            (
                "on_a",
                Selector {
                    target: Some(Some(a)),
                    ..bare
                },
            ),
            (
                "last_by",
                Selector {
                    last_by: Some(Actor::Human),
                    ..bare
                },
            ),
            ("bare", bare),
        ];
        for (name, selector) in cases {
            let selection =
                select(&entries, &selector).unwrap_or_else(|err| panic!("{name}: {err:?}")); // ubs:ignore — fails the test
            let chosen: Vec<(TxId, Option<TargetKey>, Vec<TargetKey>)> = selection
                .entries
                .iter()
                .map(|s| (s.entry.tx, s.key, s.keys().to_vec()))
                .collect();
            assert_eq!(chosen, [(repair.tx, None, vec![a])], "{name}");
        }
        let on_c_only = select(
            &entries,
            &Selector {
                target: Some(Some(c)),
                ..bare
            },
        )
        .unwrap();
        let txs: Vec<TxId> = on_c_only.entries.iter().map(|s| s.entry.tx).collect();
        assert_eq!(txs, [on_c.tx], "-t C: its own install");
    }

    /// A doctor run id is refused from the arguments alone (no history or home is read):
    /// `usage`, whose fix is the same undo through `doctor undo`, the user's verb kept.
    #[test]
    fn a_doctor_run_id_is_refused_before_anything_is_read() {
        let run = "2026-10-30T18-03-40Z__01JB8Q2VC00000000000000001";
        let lower = run.to_ascii_lowercase();
        for (case, args) in [
            (
                "--run",
                UndoArgs {
                    run: Some(run.to_owned()),
                    ..UndoArgs::default()
                },
            ),
            (
                "positional",
                UndoArgs {
                    tx: Some(run.to_owned()),
                    ..UndoArgs::default()
                },
            ),
            (
                "lower-case --run",
                UndoArgs {
                    run: Some(lower.clone()),
                    ..UndoArgs::default()
                },
            ),
            (
                "lower-case positional",
                UndoArgs {
                    tx: Some(lower.clone()),
                    ..UndoArgs::default()
                },
            ),
        ] {
            let err = refuse_doctor_run(&args).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{case}");
            assert_eq!(
                err.fix.argv(),
                Some(
                    ["omaddons", "doctor", "undo", run]
                        .map(str::to_owned)
                        .as_slice()
                ),
                "{case}"
            );
            assert_eq!(err.usage_fix, crate::error::UsageFix::UserVerb, "{case}");
        }
        for (case, args) in [
            ("bare", UndoArgs::default()),
            (
                "a transaction",
                UndoArgs {
                    tx: Some("01JB8Q2VA00000000000000001".to_owned()),
                    ..UndoArgs::default()
                },
            ),
            (
                "a run of the history",
                UndoArgs {
                    run: Some("01JB8Q2VC00000000000000000".to_owned()),
                    ..UndoArgs::default()
                },
            ),
        ] {
            assert!(refuse_doctor_run(&args).is_ok(), "{case}");
        }
    }

    /// The undo of a repair, as `render_repair` prints it, and its `undone` facts.
    mod repair_output {
        use super::*;

        const REPAIR: &str = "01JB8Q2VA00000000000000005";
        const UNDO: &str = "01JB8Q2VA00000000000000007";
        const RUN: &str = "2026-10-30T18-05-12Z__01JB8Q2VC00000000000000000";

        fn reverted(targets: &[&str], kind: TxKind) -> Reverted {
            Reverted {
                tx: REPAIR.parse().unwrap(),
                kind,
                at: "2026-10-30T18:03:00Z".parse().unwrap(),
                target: None,
                targets: targets
                    .iter()
                    .map(|id| TargetRef {
                        id: (*id).to_owned(),
                        key: TargetKey::new(),
                    })
                    .collect(),
                actor: Actor::Agent,
                argv: vec![
                    "omaddons".into(),
                    "doctor".into(),
                    "fix".into(),
                    "--only".into(),
                    "fm-test-write".into(),
                ],
                reason: None,
            }
        }

        fn renames(n: usize) -> Vec<(&'static str, String, Option<String>)> {
            (0..n)
                .map(|i| {
                    (
                        "rename",
                        format!("/c/AddOns/Y{i}"),
                        Some(format!("/c/AddOns/X/Y{i}")),
                    )
                })
                .collect()
        }

        fn rendered(reverted: Reverted, repair: RepairUndo, dry_run: bool) -> Vec<String> {
            let tx = (!dry_run).then(|| UNDO.parse::<TxId>().unwrap());
            let u = Undone {
                reverted,
                tx,
                folders: UndoFolders::default(),
                sv: SavedVariables {
                    policy: SvPolicy::Auto,
                    restored: 0,
                    unchanged: 0,
                    deferred: 0,
                    kept: 0,
                },
                reload: ReloadRequired::None,
                atomic_swap: None,
                wtf: None,
                repair: None,
            };
            let mut out = Vec::new();
            render_repair(&mut out, &u, &repair, dry_run, true).unwrap();
            String::from_utf8(out)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn repair(
            ops: Vec<(&'static str, String, Option<String>)>,
            redo_of: Option<&str>,
        ) -> RepairUndo {
            RepairUndo {
                redo_of: redo_of.map(|tx| tx.parse().unwrap()),
                ops,
                data: Vec::new(),
                run: DoctorRunId::parse(RUN),
            }
        }

        #[test]
        fn the_human_output_of_a_repair_undo() {
            let short = short_tx(&REPAIR.parse().unwrap());
            let when = local_time("2026-10-30T18:03:00Z".parse().unwrap());
            let real = rendered(
                reverted(&["retail"], TxKind::Repair),
                repair(renames(2), None),
                false,
            );
            assert_eq!(
                real,
                [
                    format!(
                        "reverted repair {short} (doctor fix by agent, {when}) on retail: 2 renames"
                    ),
                    "  rename /c/AddOns/Y0 → /c/AddOns/X/Y0".to_owned(),
                    "  rename /c/AddOns/Y1 → /c/AddOns/X/Y1".to_owned(),
                    format!("run {RUN} · tx {UNDO} · undo this: omaddons undo {UNDO}"),
                ],
                "a real run"
            );

            let dry = rendered(
                reverted(&["retail", "classic"], TxKind::Repair),
                RepairUndo {
                    run: None,
                    ..repair(renames(1), None)
                },
                true,
            );
            assert_eq!(
                dry,
                [
                    format!(
                        "would revert repair {short} (doctor fix by agent, {when}) on retail, classic: 1 rename"
                    ),
                    "  rename /c/AddOns/Y0 → /c/AddOns/X/Y0".to_owned(),
                ],
                "a dry run makes no run and no transaction"
            );

            let redo = rendered(
                reverted(&["retail"], TxKind::Undo),
                repair(renames(1), Some(UNDO)),
                false,
            );
            assert_eq!(
                redo[0],
                format!(
                    "reverted undo {short} of repair {} (doctor fix by agent, {when}) on retail: 1 rename",
                    short_tx(&UNDO.parse().unwrap())
                ),
                "a redo names the repair"
            );

            let home_side = rendered(
                reverted(&[], TxKind::Repair),
                repair(
                    vec![
                        ("write", "/h/state/a.write".to_owned(), None),
                        ("chmod", "/h/config/credentials/github".to_owned(), None),
                    ],
                    None,
                ),
                false,
            );
            assert_eq!(
                home_side[..3],
                [
                    format!(
                        "reverted repair {short} (doctor fix by agent, {when}): 1 write, 1 chmod"
                    ),
                    "  write /h/state/a.write".to_owned(),
                    "  chmod /h/config/credentials/github".to_owned(),
                ],
                "a repair of omaddons' own files names no client"
            );

            let many = rendered(
                reverted(&["retail"], TxKind::Repair),
                repair(renames(12), None),
                false,
            );
            assert_eq!(many.len(), 1 + REPAIR_LINES_SHOWN + 1 + 1, "{many:#?}");
            assert_eq!(
                many[REPAIR_LINES_SHOWN],
                "  rename /c/AddOns/Y9 → /c/AddOns/X/Y9"
            );
            assert_eq!(many[REPAIR_LINES_SHOWN + 1], "  and 2 more");
            assert!(many[0].ends_with(": 12 renames"), "{}", many[0]);
        }

        #[test]
        fn command_of_names_the_command_run() {
            for (argv, want) in [
                (
                    vec!["omaddons", "doctor", "fix", "--only", "x"],
                    "doctor fix",
                ),
                (vec!["omaddons", "undo", "01JB8Q2V"], "undo"),
                (
                    vec!["omaddons", "doctor", "undo", "2026-10-30T18-05-12Z__01JB"],
                    "doctor undo",
                ),
                (vec!["omaddons"], "omaddons"),
            ] {
                let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
                assert_eq!(command_of(&argv), want, "{argv:?}");
            }
        }
    }
}
