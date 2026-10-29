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
use crate::engine::{SvPolicy, UndoOutcome as EngineUndo, UndoRequest, UndoSelector, WtfUndo};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, FolderName, PlanId, ReloadRequired, RunId, TargetKey, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::state::history::{self, HistoryEntry, HistoryFilter, SettingEntry};
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
        value_parser = ["human", "agent", "widget"]
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

/// The transaction reverted.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Reverted {
    pub tx: TxId,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: jiff::Timestamp,
    pub target: TargetRef,
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
    pub target: TargetRef,
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

    fn record_plan(&mut self, plan_id: PlanId, expires_at: Timestamp) {
        self.data.plan_id = Some(plan_id);
        self.data.plan_expires_at = Some(expires_at);
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
                Attempt::Undone(u) => render_undone(w, u, self.data.dry_run, single)?,
                Attempt::Failed(r, error) => writeln!(
                    w,
                    "could not revert {} ({} by {}) on {}: {}",
                    short_tx(&r.tx),
                    kind_name(r.kind),
                    r.actor.as_str(),
                    r.target.id,
                    error.message
                )?,
            }
        }
        if !single && let Some(hint) = self.commands.first() {
            writeln!(w, "undo this: {}", hint.command())?;
        }
        intent::render_plan_line(
            w,
            self.data.plan_id.as_ref(),
            self.data.plan_expires_at.as_ref(),
        )
    }
}

/// M1's block for one transaction undone; `hint` adds `undo this: …` to its last line.
fn render_undone(w: &mut dyn Write, u: &Undone, dry_run: bool, hint: bool) -> std::io::Result<()> {
    let r = &u.reverted;
    let verb = if dry_run { "would revert" } else { "reverted" };
    writeln!(
        w,
        "{verb} {} ({} by {}, {}) on {}",
        short_tx(&r.tx),
        kind_name(r.kind),
        r.actor.as_str(),
        local_time(r.at),
        r.target.id
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

/// Runs `undo`.
pub fn run(ctx: &Context, args: &UndoArgs) -> Result<UndoOutcome> {
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
                    let one = precondition::of_undo(&[(chosen.entry.tx, chosen.key, Ok(undo))]);
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
        let reverted: Vec<_> = outcome
            .data
            .transactions
            .iter()
            .map(|tx| (Some(tx.target.key), tx.target.id.clone(), tx.reverted))
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
        let intent = resolved_selection(&outcome.data).and_then(|selection| {
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
/// that full run or transaction, so newer history cannot retarget it.
fn resolved_selection(data: &UndoData) -> Result<UndoSelection> {
    match data.reverted_run {
        Some(run) => Ok(UndoSelection::Run(run)),
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
        .map(|(chosen, result)| (chosen.entry.tx, chosen.key, result.as_ref()))
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

/// One transaction to revert, and the client it changed.
#[derive(Debug, Clone, Copy)]
struct Selected<'e> {
    entry: &'e HistoryEntry,
    key: TargetKey,
}

/// `entries` with the client each changed; `not_found` for a target-less line (a
/// repair), which this build cannot undo yet, before anything is planned.
fn selected(entries: Vec<&HistoryEntry>) -> Result<Vec<Selected<'_>>> {
    entries
        .into_iter()
        .map(|entry| {
            Ok(Selected {
                key: crate::engine::undo::target_of(entry)?,
                entry,
            })
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
                attempts.push(Attempt::Undone(Box::new(undone(reverted, policy, &undo))));
            }
            Err(error) => {
                let mut warning = Warning::item(&error);
                warning.message = format!("{}: {}", reverted.target.id, error.message);
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
            world
                .resolved
                .targets
                .iter()
                // ubs:ignore — client identity keys are public identifiers, not credentials
                .find(|d| d.target.key == Some(first.reverted.target.key))
                .map(|d| d.target.client_dir.clone())
        })
        .flatten();
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(UndoOutcome {
        data: UndoData {
            dry_run: ctx.dry_run,
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
    Reverted {
        tx: entry.tx,
        kind: entry.kind,
        at: entry.at,
        target: TargetRef {
            id: world.target_id(chosen.key),
            key: chosen.key,
        },
        actor: entry.actor,
        argv: entry.argv.clone(),
        reason: entry.reason.clone().filter(|_| verbose),
    }
}

fn undone(reverted: Reverted, policy: SvPolicy, done: &EngineUndo) -> Undone {
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
        atomic_swap: tx.map(|_| done.outcome.atomic_swap),
        wtf: done.wtf,
    }
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
            reverted: Reverted {
                tx,
                kind: TxKind::Install,
                at: "2026-10-14T12:00:00Z".parse().unwrap(),
                target: target.clone(),
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
            transactions: vec![UndoTx {
                target,
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
        for (case, reverted_run, expected) in [
            (
                "bare or last-by resolved a run",
                Some(run),
                serde_json::json!({"run": run}),
            ),
            (
                "target or prefix resolved a transaction",
                None,
                serde_json::json!({"tx": tx}),
            ),
        ] {
            data.reverted_run = reverted_run;
            let selected = resolved_selection(&data).unwrap();
            assert_eq!(
                serde_json::to_value(selected).unwrap(),
                expected,
                "{case}: {data:?} resolved to {selected:?}"
            );
        }
        data.transactions.clear();
        assert_eq!(
            resolved_selection(&data).unwrap_err().code,
            ErrorCode::Internal,
            "an incomplete preview cannot retarget a stored undo: {data:?}"
        );
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

    /// A repair (target-less, M4-D31) is the newest change: every selector that reaches
    /// it refuses it, never stepping over it to an older transaction; a target it did not
    /// touch still selects its own.
    #[test]
    fn select_refuses_a_target_less_line() {
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
            let output = select(&entries, &selector).map(|s| s.entries.len());
            let err = output.as_ref().unwrap_err();
            assert_eq!(err.code, ErrorCode::NotFound, "{name}: gave {output:?}");
            assert!(
                err.message.contains("cannot undo yet"),
                "{name}: {}",
                err.message
            );
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
}
