//! `omaddons history [-t T] [--limit N] [--by A] [--run R] [--since D]`: the transaction
//! log, newest first, with who did what and why, and the run (command invocation) each
//! transaction belongs to. Read-only: no lock, no writes.
//!
//! `--by` filters by actor. The global `--actor` flag, read as the same filter, keeps
//! working (M1); `OMADDONS_ACTOR` never filters. A torn last line is skipped and
//! reported as one stderr log event.

use std::io::Write;

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::Serialize;

use super::world::World;
use crate::cli::Context;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, RunId, TargetKey, TxId, TxKind};
use crate::output::{Outcome, Style, table};
use crate::state::history::{self, FolderOp, HistoryEntry, HistoryFilter, HistoryOp, TxResult};

/// The default `--limit`: token-frugal; agents raise it when they need more.
const DEFAULT_LIMIT: usize = 20;

/// `history`'s own flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct HistoryArgs {
    /// At most this many entries, newest first (default 20)
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Only entries since a duration ago (30m, 12h, 1d, 2w) or an RFC 3339 time
    #[arg(long, value_name = "WHEN")]
    pub since: Option<String>,
    /// Only this run's transactions: its id or a unique prefix of 8+ characters
    #[arg(long, value_name = "RUN")]
    pub run: Option<String>,
    /// Only transactions by this actor: human, agent or widget
    #[arg(long, value_name = "ACTOR")]
    pub by: Option<String>,
}

/// `history`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryData {
    /// Newest first.
    pub entries: Vec<HistoryLine>,
    /// More entries matched than `--limit` shows.
    pub truncated: bool,
}

/// One transaction.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryLine {
    pub tx: TxId,
    /// The command invocation it belongs to; `null` for transactions from before M2.
    pub run: Option<RunId>,
    pub kind: TxKind,
    #[schemars(with = "String")]
    pub at: Timestamp,
    pub target: TargetRef,
    pub actor: Actor,
    pub reason: Option<String>,
    pub argv: Vec<String>,
    /// Folder counts; `-v` shows `ops` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<ChangeCounts>,
    /// `-v`: every folder the transaction touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<Vec<HistoryOp>>,
    /// The transaction an undo reverted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverts: Option<TxId>,
    pub result: TxResult,
}

/// A target as history names it: its id now (the key when no longer registered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TargetRef {
    pub id: String,
    pub key: TargetKey,
}

/// Folders by what happened to them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ChangeCounts {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
    /// Folders whose record changed without a swap (a pin, an adoption).
    pub recorded: usize,
}

impl ChangeCounts {
    pub fn of(ops: &[HistoryOp]) -> Self {
        let mut counts = Self::default();
        for op in ops {
            match op.op {
                FolderOp::Add => counts.added += 1,
                FolderOp::Replace => counts.replaced += 1,
                FolderOp::Remove => counts.removed += 1,
                FolderOp::Record => counts.recorded += 1,
            }
        }
        counts
    }

    /// `+added ~replaced -removed`, then the recorded folders when there are any.
    pub fn human(&self) -> String {
        let swapped = format!("+{} ~{} -{}", self.added, self.replaced, self.removed);
        match self.recorded {
            0 => swapped,
            n => format!("{swapped} ={n}"),
        }
    }
}

/// `history`'s outcome.
#[derive(Debug)]
pub struct HistoryOutcome {
    data: HistoryData,
    warnings: Vec<Warning>,
    /// The last line of `history.jsonl` was cut short and skipped (a stderr log event).
    pub torn_tail: bool,
}

impl Outcome for HistoryOutcome {
    type Data = HistoryData;

    fn command(&self) -> &'static str {
        "history"
    }

    fn data(&self) -> &HistoryData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        if self.data.entries.is_empty() {
            return writeln!(w, "No transactions yet.");
        }
        let rows: Vec<Vec<String>> = self
            .data
            .entries
            .iter()
            .map(|e| {
                let c = e
                    .changes
                    .unwrap_or_else(|| ChangeCounts::of(e.ops.as_deref().unwrap_or_default()));
                vec![
                    short_tx(&e.tx),
                    e.run
                        .map_or_else(|| "-".to_owned(), |r| short_id(&r.to_string())),
                    local_time(e.at),
                    e.actor.as_str().to_owned(),
                    kind_name(e.kind),
                    e.target.id.clone(),
                    c.human(),
                    e.reason
                        .as_ref()
                        .map(|r| format!("{r:?}"))
                        .unwrap_or_default(),
                ]
            })
            .collect();
        write!(w, "{}", table(&rows))?;
        if self.data.truncated {
            writeln!(w, "(older entries: --limit N)")?;
        }
        Ok(())
    }
}

/// Runs `history`. `actor` is the explicit global `--actor` value.
pub fn run(ctx: &Context, args: &HistoryArgs, actor: Option<&str>) -> Result<HistoryOutcome> {
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::new(
            ErrorCode::Usage,
            "--limit must be at least 1",
            Suggestion::command(["omaddons", "history", "--limit", "20"]),
        ));
    }
    let actor = actor
        .map(|a| {
            a.parse::<Actor>().map_err(|_| {
                Error::new(
                    ErrorCode::Usage,
                    format!("--actor is human, agent or widget, not {a:?}"),
                    Suggestion::command(["omaddons", "history", "--actor", "agent"]),
                )
            })
        })
        .transpose()?;
    let by = args
        .by
        .as_deref()
        .map(|a| {
            a.parse::<Actor>().map_err(|_| {
                Error::new(
                    ErrorCode::Usage,
                    format!("--by is human, agent or widget, not {a:?}"),
                    Suggestion::command(["omaddons", "history", "--by", "agent"]),
                )
            })
        })
        .transpose()?;
    let actor = match (by, actor) {
        (Some(by), Some(actor)) if by != actor => {
            return Err(Error::new(
                ErrorCode::Usage,
                format!(
                    "--by {} and --actor {} ask for different actors",
                    by.as_str(),
                    actor.as_str()
                ),
                Suggestion::command(without_flag(&ctx.argv, "--actor")),
            ));
        }
        (by, actor) => by.or(actor),
    };
    let since = args
        .since
        .as_deref()
        .map(|s| {
            parse_since(s, Timestamp::now()).ok_or_else(|| {
                Error::new(
                    ErrorCode::Usage,
                    format!(
                        "--since takes a duration (30m, 12h, 1d, 2w) or an RFC 3339 time, not {s:?}"
                    ),
                    Suggestion::command(["omaddons", "history", "--since", "1d"]),
                )
            })
        })
        .transpose()?;
    let world = World::load()?;
    let run = match &args.run {
        Some(prefix) => {
            let all = history::read(&world.paths, &HistoryFilter::default())?.entries;
            Some(resolve_run(&all, prefix)?)
        }
        None => None,
    };
    let target = world.history_key(ctx)?;
    let read = match target {
        // A target that was never registered has no history.
        Some(None) => history::HistoryRead::default(),
        Some(Some(key)) => history::read(
            &world.paths,
            &HistoryFilter {
                target: Some(key),
                actor,
                since,
                run,
                ..HistoryFilter::default()
            },
        )?,
        None => history::read(
            &world.paths,
            &HistoryFilter {
                actor,
                since,
                run,
                ..HistoryFilter::default()
            },
        )?,
    };
    let verbose = ctx.output.style.verbose > 0;
    let truncated = read.entries.len() > limit;
    let entries = read
        .entries
        .iter()
        .rev()
        .take(limit)
        .map(|e| line(&world, e, verbose))
        .collect();
    let mut warnings = world.warnings.clone();
    warnings.extend(read.skipped_warning());
    Ok(HistoryOutcome {
        data: HistoryData { entries, truncated },
        warnings,
        torn_tail: read.torn_tail,
    })
}

fn line(world: &World, e: &HistoryEntry, verbose: bool) -> HistoryLine {
    HistoryLine {
        tx: e.tx,
        run: e.run,
        kind: e.kind,
        at: e.at,
        target: TargetRef {
            id: world.target_id(e.target),
            key: e.target,
        },
        actor: e.actor,
        reason: e.reason.clone(),
        argv: e.argv.clone(),
        changes: (!verbose).then(|| ChangeCounts::of(&e.ops)),
        ops: verbose.then(|| e.ops.clone()),
        reverts: e.reverts,
        result: e.result,
    }
}

/// `--since`: an RFC 3339 time, or a whole number of minutes, hours, days or weeks
/// before `now` (`30m`, `12h`, `1d`, `2w`).
pub fn parse_since(s: &str, now: Timestamp) -> Option<Timestamp> {
    if let Ok(at) = s.parse::<Timestamp>() {
        return Some(at);
    }
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (count, unit) = s.split_at(split);
    let count: i64 = count.parse().ok()?;
    let unit: i64 = match unit {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    now.checked_sub(SignedDuration::from_secs(count.checked_mul(unit)?))
        .ok()
}

/// The first ten characters of a transaction id: enough to undo by prefix.
pub fn short_tx(tx: &TxId) -> String {
    short_id(&tx.to_string())
}

/// The first ten characters of an id, then `…`.
pub fn short_id(full: &str) -> String {
    format!("{}…", full.get(..10).unwrap_or(full))
}

/// The run whose id is `prefix` or starts with it (8+ characters, any case), among the
/// runs of `entries`.
pub fn resolve_run(entries: &[HistoryEntry], prefix: &str) -> Result<RunId> {
    let wanted = prefix.to_ascii_uppercase();
    if wanted.len() < MIN_PREFIX {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("a run id prefix needs at least {MIN_PREFIX} characters, not {prefix:?}"),
            Suggestion::command(["omaddons", "history"]),
        ));
    }
    let mut matching: Vec<RunId> = entries
        .iter()
        .filter_map(|e| e.run)
        .filter(|r| r.to_string().starts_with(&wanted))
        .collect();
    matching.sort_by_key(|r| r.ulid());
    matching.dedup();
    match matching.as_slice() {
        [one] => Ok(*one),
        [] => Err(Error::new(
            ErrorCode::NotFound,
            format!("there is no run {prefix} in the history"),
            Suggestion::command(["omaddons", "history"]),
        )),
        many => {
            let candidates: Vec<String> = many.iter().map(ToString::to_string).collect();
            Err(Error::new(
                ErrorCode::NotFound,
                format!("{prefix} matches {} runs; give more of the id", many.len()),
                Suggestion::command(["omaddons", "history"]),
            )
            .details(serde_json::json!({ "candidates": candidates })))
        }
    }
}

/// The shortest id prefix `undo` and `history` accept.
pub const MIN_PREFIX: usize = 8;

/// `argv` without `flag` and its value (`--actor agent`, `--actor=agent`).
fn without_flag(argv: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        if arg == flag {
            args.next();
        } else if !arg.starts_with(&format!("{flag}=")) {
            out.push(arg.clone());
        }
    }
    out
}

/// `2026-10-20 14:02` in the local time zone.
pub fn local_time(at: Timestamp) -> String {
    at.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

pub fn kind_name(kind: TxKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_takes_durations_and_rfc3339() {
        let now: Timestamp = "2026-10-14T12:00:00Z".parse().unwrap();
        let at = |s| parse_since(s, now).map(|t| t.to_string());
        assert_eq!(at("30m").as_deref(), Some("2026-10-14T11:30:00Z"));
        assert_eq!(at("12h").as_deref(), Some("2026-10-14T00:00:00Z"));
        assert_eq!(at("1d").as_deref(), Some("2026-10-13T12:00:00Z"));
        assert_eq!(at("2w").as_deref(), Some("2026-09-30T12:00:00Z"));
        assert_eq!(
            at("2026-10-01T08:00:00Z").as_deref(),
            Some("2026-10-01T08:00:00Z")
        );
        for bad in [
            "",
            "d",
            "1y",
            "1.5d",
            "yesterday",
            "-1d",
            "99999999999999999999w",
        ] {
            assert_eq!(at(bad), None, "{bad}");
        }
    }

    #[test]
    fn counts_and_short_ids() {
        let op = |op| HistoryOp {
            op,
            folder: crate::model::FolderName::new("Foo").unwrap(),
            addon: "Foo".into(),
            edit: None,
        };
        assert_eq!(
            ChangeCounts::of(&[
                op(FolderOp::Add),
                op(FolderOp::Add),
                op(FolderOp::Remove),
                op(FolderOp::Record)
            ]),
            ChangeCounts {
                added: 2,
                replaced: 0,
                removed: 1,
                recorded: 1
            }
        );
        let pin = ChangeCounts::of(&[op(FolderOp::Record), op(FolderOp::Record)]);
        assert_eq!(pin.human(), "+0 ~0 -0 =2", "recorded folders are counted");
        assert_eq!(ChangeCounts::of(&[op(FolderOp::Add)]).human(), "+1 ~0 -0");
        let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
        assert_eq!(short_tx(&tx), "01JB8Q2VA0…");
        assert_eq!(kind_name(TxKind::Undo), "undo");
    }
}
