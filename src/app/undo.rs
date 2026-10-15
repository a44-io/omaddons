//! `omaddons undo [<tx> | --last-by <actor>] [--saved-variables auto|restore|keep]`:
//! reverts a transaction through the engine (vision §5.5), as a transaction of its own
//! that can be undone in turn. The output always names the reverted transaction, and
//! reports SavedVariables by count only: their paths hold account, realm and character
//! names.
//!
//! Without arguments: the most recent undoable transaction, on the `-t` target when one
//! is given. `<tx>` is a full id or a unique prefix of at least 8 characters.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::engine::Engine;
use super::history::{TargetRef, kind_name, local_time, short_tx};
use super::world::World;
use crate::cli::Context;
use crate::engine::{SvPolicy, UndoOutcome as EngineUndo, UndoRequest, UndoSelector};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, FolderName, ReloadRequired, TxId, TxKind};
use crate::output::{Outcome, Style};
use crate::state::history::{self, HistoryEntry, HistoryFilter};

/// The shortest transaction id prefix `undo` accepts.
const MIN_PREFIX: usize = 8;

/// `undo`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct UndoArgs {
    /// The transaction to revert: its id or a unique prefix of 8+ characters
    /// (default: the most recent)
    #[arg(value_name = "TX")]
    pub tx: Option<String>,
    /// Revert this actor's most recent transaction: human, agent or widget
    #[arg(long, value_name = "ACTOR", conflicts_with = "tx")]
    pub last_by: Option<String>,
    /// SavedVariables: auto (restore what changed, later if the game runs), restore
    /// (refuse while the game runs) or keep (never touch them)
    #[arg(long, value_name = "POLICY", value_parser = ["auto", "restore", "keep"])]
    pub saved_variables: Option<String>,
}

/// `undo`'s data.
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

/// `undo`'s outcome.
#[derive(Debug)]
pub struct UndoOutcome {
    data: UndoData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    /// The global lock's hold time, for the `-v` log line.
    pub lock_held_ms: Option<u64>,
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

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let r = &d.reverted;
        let verb = if d.dry_run {
            "would revert"
        } else {
            "reverted"
        };
        writeln!(
            w,
            "{verb} {} ({} by {}, {}) on {}",
            short_tx(&r.tx),
            kind_name(r.kind),
            r.actor.as_str(),
            local_time(r.at),
            r.target.id
        )?;
        let sv = &d.saved_variables;
        let mut parts = vec![format!(
            "restored {} folders, removed {}",
            d.folders.restored.len(),
            d.folders.removed.len()
        )];
        let mut counts = vec![
            format!("{} restored", sv.restored),
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
        if let Some(tx) = d.tx {
            tail.push(format!("tx {tx}"));
        }
        match d.reload_required {
            ReloadRequired::Reload => tail.push("/reload in game".to_owned()),
            ReloadRequired::Restart => tail.push("restart the game client".to_owned()),
            ReloadRequired::None => {}
        }
        if let Some(tx) = d.tx {
            tail.push(format!("undo this: omaddons undo {tx}"));
        }
        if !tail.is_empty() {
            writeln!(w, "{}", tail.join(" · "))?;
        }
        Ok(())
    }
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
    let world = World::load()?;
    let entries = history::read(&world.paths, &HistoryFilter::default())?.entries;
    let reverted = select(
        &entries,
        args.tx.as_deref(),
        last_by,
        world.history_key(ctx)?,
    )?
    .clone();

    let engine = Engine::new(ctx, &world, "undo")?;
    let done = engine.undo(&UndoRequest {
        selector: UndoSelector::Tx(reverted.tx),
        sv_policy,
        dry_run: ctx.dry_run,
        while_running: ctx.while_running,
        reason: ctx.reason.clone(),
        targets: world.resolved.targets.clone(),
    })?;
    Ok(outcome(ctx, &world, &reverted, sv_policy, done))
}

/// The transaction to revert: `tx` (an id or a unique 8+ character prefix), else the
/// newest undoable one (by `last_by`), on the `-t` target when `target` names one.
fn select<'e>(
    entries: &'e [HistoryEntry],
    tx: Option<&str>,
    last_by: Option<Actor>,
    target: Option<Option<crate::model::TargetKey>>,
) -> Result<&'e HistoryEntry> {
    let none = |message: String| {
        Error::new(
            ErrorCode::NotFound,
            message,
            Suggestion::command(["omaddons", "history"]),
        )
    };
    if let Some(prefix) = tx {
        return by_prefix(entries, prefix);
    }
    let on_target = |e: &HistoryEntry| match target {
        Some(key) => Some(e.target) == key,
        None => true,
    };
    let found = entries
        .iter()
        .rev()
        .find(|e| e.kind.is_undoable() && on_target(e) && last_by.is_none_or(|a| e.actor == a));
    found.ok_or_else(|| match last_by {
        Some(actor) => none(format!(
            "there is no transaction by {} to undo",
            actor.as_str()
        )),
        None => none("nothing to undo".to_owned()),
    })
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

fn outcome(
    ctx: &Context,
    world: &World,
    reverted: &HistoryEntry,
    policy: SvPolicy,
    done: EngineUndo,
) -> UndoOutcome {
    let verbose = ctx.output.style.verbose > 0;
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
    let commands = tx
        .map(|tx| {
            vec![CommandHint::new(
                "undo",
                ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
                false,
            )]
        })
        .unwrap_or_default();
    let client_dir = verbose
        .then(|| {
            world
                .resolved
                .targets
                .iter()
                .find(|d| d.target.key == Some(reverted.target))
                .map(|d| d.target.client_dir.clone())
        })
        .flatten();
    UndoOutcome {
        data: UndoData {
            dry_run: done.outcome.dry_run,
            reverted: Reverted {
                tx: reverted.tx,
                kind: reverted.kind,
                at: reverted.at,
                target: TargetRef {
                    id: world.target_id(reverted.target),
                    key: reverted.target,
                },
                actor: reverted.actor,
                argv: reverted.argv.clone(),
                reason: reverted.reason.clone().filter(|_| verbose),
            },
            tx,
            folders,
            saved_variables: SavedVariables {
                policy,
                restored: done.sv.restored,
                unchanged: done.sv.unchanged,
                deferred: done.sv.deferred,
                kept: done.sv.kept,
            },
            reload_required: done.outcome.reload_required,
            atomic_swap: tx.map(|_| done.outcome.atomic_swap),
            client_dir,
        },
        warnings: done.outcome.warnings,
        commands,
        lock_held_ms: done.outcome.lock_held_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TargetKey;
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
        }
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
        let tx = |r: Result<&HistoryEntry>| r.map(|e| e.tx.to_string()).map_err(|e| e.code);
        let id = |n: u8| format!("01JB8Q2VA0000000000000000{n}");
        assert_eq!(
            tx(select(&entries, None, None, None)),
            Ok(id(2)),
            "backups are not undoable"
        );
        assert_eq!(
            tx(select(&entries, None, None, Some(Some(retail)))),
            Ok(id(1)),
            "-t narrows"
        );
        assert_eq!(
            tx(select(&entries, None, Some(Actor::Agent), None)),
            Ok(id(1))
        );
        assert_eq!(
            tx(select(&entries, None, Some(Actor::Widget), None)),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            tx(select(&entries, None, None, Some(None))),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(tx(select(&[], None, None, None)), Err(ErrorCode::NotFound));

        // Prefixes: unique, any case; ambiguous and unknown are not found; short is usage.
        assert_eq!(
            tx(select(&entries, Some("01jb8q2vb"), None, None)),
            Ok("01JB8Q2VB00000000000000003".into())
        );
        assert_eq!(tx(select(&entries, Some(&id(2)), None, None)), Ok(id(2)));
        let ambiguous = by_prefix(&entries, "01JB8Q2VA").unwrap_err();
        assert_eq!(ambiguous.code, ErrorCode::NotFound);
        assert_eq!(ambiguous.details["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(
            tx(select(&entries, Some("01JB8Q2VZ"), None, None)),
            Err(ErrorCode::NotFound)
        );
        assert_eq!(
            tx(select(&entries, Some("01JB8Q2"), None, None)),
            Err(ErrorCode::Usage)
        );
    }
}
