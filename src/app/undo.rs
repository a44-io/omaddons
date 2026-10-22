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

use schemars::JsonSchema;
use serde::Serialize;

use super::engine::Engine;
use super::history::{MIN_PREFIX, TargetRef, kind_name, local_time, resolve_run, short_tx};
use super::outcome::ItemError;
use super::precondition;
use super::world::World;
use crate::cli::Context;
use crate::engine::{SvPolicy, UndoOutcome as EngineUndo, UndoRequest, UndoSelector};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, FolderName, ReloadRequired, RunId, TargetKey, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::state::history::{self, HistoryEntry, HistoryFilter, SettingEntry};

/// `undo`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UndoArgs {
    /// The transaction to revert: its id or a unique prefix of 8+ characters
    /// (default: the most recent run)
    #[arg(value_name = "TX")]
    pub tx: Option<String>,
    /// Revert this actor's most recent run: human, agent or widget
    #[arg(long, value_name = "ACTOR", conflicts_with = "tx")]
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
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    partial: bool,
    attempts: Vec<Attempt>,
    /// Each committed undo transaction's global lock hold time, for `-v` log lines.
    pub locks: Vec<(TxId, u64)>,
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
        Ok(())
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
    let last_by = args
        .last_by
        .as_deref()
        .map(|a| {
            a.parse::<Actor>().map_err(|_| {
                Error::new(
                    ErrorCode::Usage,
                    format!("--last-by is human, agent or widget, not {a:?}"),
                    Suggestion::command(["omaddons", "undo", "--last-by", "agent"]),
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
    if let Some(prefix) = args.tx.as_deref()
        && let Some(setting) = setting_by_prefix(&read.settings, prefix)
    {
        let kind = crate::app::history::kind_name(setting.kind);
        return Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "transaction {} is a {kind} change, which undo does not revert",
                setting.tx
            ),
            Suggestion::command(["omaddons", "history"]),
        ));
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
            let previewed: Vec<(&HistoryEntry, Result<EngineUndo>)> = selection
                .entries
                .iter()
                .map(|entry| (*entry, engine.undo(&request(entry, true))))
                .collect();
            let preview = precondition::preview_argv(&ctx.argv);
            precondition::check(Some(token), &undo_precondition(&previewed), &preview)?;
            let tokens = previewed
                .iter()
                .filter_map(|(entry, result)| {
                    let undo = result.as_ref().ok()?;
                    let one = precondition::of_undo(&[(entry.tx, entry.target, Ok(undo))]);
                    Some((entry.tx, one.token()))
                })
                .collect();
            Some(precondition::UndoBinding { tokens, preview })
        }
        _ => None,
    };
    let mut done = Vec::new();
    // Each in its own lock window; a failure does not stop the rest.
    for entry in &selection.entries {
        let result = engine.undo_bound(
            &request(entry, ctx.dry_run),
            binding.as_ref().map(|b| b as &dyn crate::engine::Binding),
        );
        done.push((*entry, result));
    }
    let token = ctx.dry_run.then(|| undo_precondition(&done).token());
    outcome(ctx, &world, &selection, run, sv_policy, done, token)
}

/// The precondition of the undos `planned`, each of a selected transaction.
fn undo_precondition(
    planned: &[(&HistoryEntry, Result<EngineUndo>)],
) -> precondition::Precondition {
    let planned: Vec<precondition::UndoPlanned<'_>> = planned
        .iter()
        .map(|(entry, result)| (entry.tx, entry.target, result.as_ref()))
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
    entries: Vec<&'e HistoryEntry>,
    reverted_run: Option<RunId>,
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
    let on_target = |e: &HistoryEntry| match s.target {
        Some(key) => Some(e.target) == key,
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
            entries: vec![entry],
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
            entries: of_run,
            reverted_run: Some(run),
        });
    }
    let newest = entries
        .iter()
        .rev()
        .find(|e| e.kind.is_undoable() && on_target(e) && s.last_by.is_none_or(|a| e.actor == a));
    let Some(newest) = newest else {
        return Err(match s.last_by {
            Some(actor) => none(format!(
                "there is no transaction by {} to undo",
                actor.as_str()
            )),
            None => none("nothing to undo".to_owned()),
        });
    };
    if s.target.is_some() {
        return Ok(Selection {
            entries: vec![newest],
            reverted_run: None,
        });
    }
    let key = newest.run_key();
    Ok(Selection {
        entries: entries
            .iter()
            .rev()
            .filter(|e| e.kind.is_undoable() && e.run_key() == key)
            .collect(),
        reverted_run: newest.run,
    })
}

/// The one settings change whose id is `prefix` or starts with it (8+ characters).
fn setting_by_prefix<'e>(settings: &'e [SettingEntry], prefix: &str) -> Option<&'e SettingEntry> {
    let wanted = prefix.to_ascii_uppercase();
    if wanted.len() < MIN_PREFIX {
        return None;
    }
    let mut matching = settings
        .iter()
        .filter(|e| e.tx.to_string().starts_with(&wanted));
    match (matching.next(), matching.next()) {
        (Some(one), None) => Some(one),
        _ => None,
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
    done: Vec<(&HistoryEntry, Result<EngineUndo>)>,
    precondition: Option<String>,
) -> Result<UndoOutcome> {
    let verbose = ctx.output.style.verbose > 0;
    let mut attempts = Vec::new();
    let mut transactions = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();
    let mut locks = Vec::new();
    let mut first_error = None;
    for (entry, result) in done {
        let reverted = reverted(world, entry, verbose);
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
            "undo_run",
            [
                "omaddons".to_owned(),
                "undo".to_owned(),
                "--run".to_owned(),
                run.to_string(),
            ],
            false,
        ));
    }
    for tx in &committed {
        commands.push(CommandHint::new(
            "undo",
            ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
            false,
        ));
    }
    let client_dir = verbose
        .then(|| {
            world
                .resolved
                .targets
                .iter()
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
            reload_required: first.reload,
            atomic_swap: first.atomic_swap,
            client_dir,
            run: (!ctx.dry_run).then_some(run),
            reverted_run: selection.reverted_run,
            transactions,
            precondition,
        },
        warnings: unique,
        commands,
        partial,
        attempts,
        locks,
    })
}

/// How the history names the transaction reverted.
fn reverted(world: &World, entry: &HistoryEntry, verbose: bool) -> Reverted {
    Reverted {
        tx: entry.tx,
        kind: entry.kind,
        at: entry.at,
        target: TargetRef {
            id: world.target_id(entry.target),
            key: entry.target,
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
            target,
            actor,
            reason: None,
            argv: vec!["omaddons".into()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            created_addons_dir: false,
            run: None,
            backup: None,
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
            .map(|s| s.entries.iter().map(|e| e.tx.to_string()).collect())
            .map_err(|e| e.code)
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
                        .map(|e| e.tx.to_string())
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
}
