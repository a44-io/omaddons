//! `omaddons history [-t T] [--limit N] [--by A] [--run R] [--since D]`: the transaction
//! log, newest first, with who did what and why, and the run (command invocation) each
//! transaction belongs to. Read-only: no lock, no writes.
//!
//! `--by` filters by actor. The global `--actor` flag, read as the same filter, keeps
//! working (M1); `OMADDONS_ACTOR` never filters. A torn last line is skipped and
//! reported as one stderr log event.

use std::collections::BTreeSet;
use std::io::Write;

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::Serialize;

use super::history_items::{self, HistoryItem};
use super::world::World;
use crate::cli::Context;
use crate::contract::values::{DurationUnit, ValueSet};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{Actor, PlanId, RunId, TargetKey, TxId, TxKind};
use crate::output::{Outcome, Style, table};
use crate::state::history::{
    self, BackupNote, Dismissed, FolderOp, HistoryEntry, HistoryFilter, HistoryOp, RepairEndpoints,
    RepairEntry, RepairOp, RepairPredicate, SettingEntry, TxResult,
};

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
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub at: Timestamp,
    /// `null` for a settings change about no client (`config`, `auth`, `export`), and on
    /// a repair or the undo of one, which name their clients in `targets`.
    pub target: Option<TargetRef>,
    /// Target-less lines only (a repair, or the undo of one): the clients it touched, by
    /// their ids now, sorted by id then key; empty when it touched none. Omitted on a
    /// line with a target and on a settings change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub targets: Option<Vec<TargetRef>>,
    pub actor: Actor,
    #[schemars(schema_with = "crate::contract::schema::reason_text")]
    pub reason: Option<String>,
    pub argv: Vec<String>,
    /// Folder counts; `-v` shows `ops` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<ChangeCounts>,
    /// `-v`: every folder the transaction touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<Vec<HistoryOp>>,
    /// `-v`: what it did to each addon, `ops` grouped by addon in their order, with the
    /// addon's version before and after; empty on a line without ops (a settings change,
    /// a backup, a repair).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<HistoryItem>>,
    /// The transaction an undo reverted. Omitted on every line but an undo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverts: Option<TxId>,
    pub result: TxResult,
    /// The backup a `backup` line published. Omitted on every other kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupNote>,
    /// What an `errors dismiss` line acknowledged, as counts. Omitted on other kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<Dismissed>,
    /// The stored plan `apply` committed it from; `null` for every other line.
    pub plan: Option<PlanId>,
    /// Target-less lines only: the doctor run directory it belongs to. Omitted on a line
    /// with a target, on a settings change, and on a target-less line that recorded none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doctor_run_id: Option<String>,
    /// Target-less lines only: its filesystem operations, as the repair recorded them
    /// (paths cut after a `WTF` component); empty when it recorded none. Omitted on a
    /// line with a target and on a settings change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repairs: Option<Vec<RepairEntry>>,
    /// The skill folder a `skill` line installed into. Omitted on every other kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<std::path::PathBuf>,
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
    /// Settings files a restore put back (`wtf` and `saved_variables` ops).
    pub restored: usize,
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
                FolderOp::Wtf | FolderOp::SavedVariables => {
                    let files = op.files.unwrap_or(0);
                    counts.restored += usize::try_from(files).unwrap_or(usize::MAX);
                }
            }
        }
        counts
    }

    /// `+added ~replaced -removed`, then the recorded folders and the files restored when
    /// there are any.
    pub fn human(&self) -> String {
        let mut text = format!("+{} ~{} -{}", self.added, self.replaced, self.removed);
        if self.recorded > 0 {
            text.push_str(&format!(" ={}", self.recorded));
        }
        if self.restored > 0 {
            let files = crate::output::count(self.restored, "file");
            text.push_str(&format!(" · {files} restored"));
        }
        text
    }
}

/// `history`'s outcome.
#[derive(Debug)]
pub struct HistoryOutcome {
    data: HistoryData,
    warnings: Vec<Warning>,
    /// The last line of `history.jsonl` was cut short and skipped (a stderr log event).
    pub torn_tail: bool,
    /// Runs with two or more lines, whose rows name the run (human output only).
    shared_runs: BTreeSet<RunId>,
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

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        if self.data.entries.is_empty() {
            return writeln!(w, "No transactions yet.");
        }
        // A run column only when some row names its run.
        let with_runs = self
            .data
            .entries
            .iter()
            .any(|e| e.run.is_some_and(|r| self.shared_runs.contains(&r)));
        // With `-v`, a plan column when some row was applied from a stored plan.
        let with_plans = style.detail() && self.data.entries.iter().any(|e| e.plan.is_some());
        let rows: Vec<Vec<String>> = self
            .data
            .entries
            .iter()
            .map(|e| {
                let c = e
                    .changes
                    .unwrap_or_else(|| ChangeCounts::of(e.ops.as_deref().unwrap_or_default()));
                // A settings change touches no folder: what was run says what it did (a
                // dismissal, what it acknowledged). A backup touches none either: its
                // note does.
                let what = if let Some(repairs) = &e.repairs {
                    repair_summary(repairs, e.doctor_run_id.as_deref())
                } else if let Some(d) = &e.dismissed {
                    let count = |n: u64, what| {
                        crate::output::count(usize::try_from(n).unwrap_or(usize::MAX), what)
                    };
                    format!(
                        "{} in {}",
                        count(d.errors, "error"),
                        count(d.databases, "database")
                    )
                } else if e.kind.is_setting() {
                    e.argv.get(1..).unwrap_or_default().join(" ")
                } else if let Some(b) = &e.backup {
                    format!(
                        "WTF/ as {} ({})",
                        short_id(&b.id.to_string()),
                        crate::output::count(
                            usize::try_from(b.files).unwrap_or(usize::MAX),
                            "file"
                        )
                    )
                } else {
                    c.human()
                };
                // The run, only when it holds more than this transaction: a lone
                // transaction's run id would read as a second copy of its own.
                let run = e
                    .run
                    .filter(|r| self.shared_runs.contains(r))
                    .map_or_else(String::new, |r| format!("run {}", short_id(&r.to_string())));
                let mut row = vec![short_tx(&e.tx)];
                if with_runs {
                    row.push(run);
                }
                // The line's target; else a target-less line's clients; else `-`.
                let target = match (&e.target, &e.targets) {
                    (Some(t), _) => t.id.clone(),
                    (None, Some(targets)) if !targets.is_empty() => {
                        let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
                        ids.join(", ")
                    }
                    _ => "-".to_owned(),
                };
                row.extend([
                    local_time(e.at),
                    e.actor.as_str().to_owned(),
                    kind_name(e.kind),
                    target,
                    what,
                ]);
                if with_plans {
                    row.push(e.plan.map_or_else(String::new, |p| {
                        format!("plan {}", short_id(&p.to_string()))
                    }));
                }
                row.push(
                    e.reason
                        .as_ref()
                        .map(|r| format!("{r:?}"))
                        .unwrap_or_default(),
                );
                row
            })
            .collect();
        // Each row, then (`-v`) what it did to each addon, one line each under it.
        for (row, entry) in table(&rows).lines().zip(&self.data.entries) {
            writeln!(w, "{row}")?;
            for item in entry.items.iter().flatten() {
                writeln!(w, "    {}", item.human())?;
            }
        }
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
                Suggestion::command(crate::cli::fix_argv(&without_flag(&ctx.argv, "--actor"))),
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
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.detail();
    let text = history::text(&world.paths)?;
    // Every readable line, for `--run`'s prefix and for the neighbours `-v`'s items read
    // their versions from, whatever the filters keep.
    let all = if args.run.is_some() || verbose {
        history::parse(&text, &HistoryFilter::default())
    } else {
        history::HistoryRead::default()
    };
    let run = match &args.run {
        Some(prefix) => {
            let runs = all
                .entries
                .iter()
                .filter_map(|e| e.run)
                .chain(all.settings.iter().map(|s| s.run));
            Some(resolve_run_in(runs, prefix)?)
        }
        None => None,
    };
    let target = world.history_key(ctx)?;
    let read = match target {
        // A target that was never registered has no history.
        Some(None) => history::HistoryRead::default(),
        Some(Some(key)) => history::parse(
            &text,
            &HistoryFilter {
                target: Some(key),
                actor,
                since,
                run,
                ..HistoryFilter::default()
            },
        ),
        None => history::parse(
            &text,
            &HistoryFilter {
                actor,
                since,
                run,
                ..HistoryFilter::default()
            },
        ),
    };
    // Transactions and settings changes, in the order they were recorded.
    let mut lines: Vec<HistoryLine> = read
        .entries
        .iter()
        .map(|e| line(&world, e, verbose))
        .chain(
            read.settings
                .iter()
                .map(|e| setting_line(&world, e, verbose)),
        )
        .collect();
    lines.sort_by_key(|l| (l.at, l.tx));
    let truncated = lines.len() > limit;
    let shared_runs = shared_runs(&lines);
    let mut entries: Vec<HistoryLine> = lines.into_iter().rev().take(limit).collect();
    if verbose {
        // Only the lines shown read their records: a settings change has no ops.
        let by_tx: std::collections::BTreeMap<TxId, &HistoryEntry> =
            read.entries.iter().map(|e| (e.tx, e)).collect();
        let mut reader = history_items::Reader::new(&world.paths, &all.entries);
        for line in &mut entries {
            line.items = Some(
                by_tx
                    .get(&line.tx)
                    .map(|e| reader.items(e))
                    .unwrap_or_default(),
            );
        }
    }
    let mut warnings = world.warnings.clone();
    warnings.extend(read.skipped_warning());
    Ok(HistoryOutcome {
        data: HistoryData { entries, truncated },
        warnings,
        torn_tail: read.torn_tail,
        shared_runs,
    })
}

/// The runs that recorded two or more of `lines` (all of them, before `--limit`): only
/// their rows name the run.
fn shared_runs(lines: &[HistoryLine]) -> BTreeSet<RunId> {
    let mut seen = BTreeSet::new();
    let mut shared = BTreeSet::new();
    for run in lines.iter().filter_map(|l| l.run) {
        if !seen.insert(run) {
            shared.insert(run);
        }
    }
    shared
}

fn setting_line(world: &World, e: &SettingEntry, verbose: bool) -> HistoryLine {
    setting_line_with(e, verbose, &|key| world.target_id(key))
}

/// The settings change `e` as `history` lists it, naming its client by `id_of`.
fn setting_line_with(
    e: &SettingEntry,
    verbose: bool,
    id_of: &dyn Fn(TargetKey) -> String,
) -> HistoryLine {
    HistoryLine {
        tx: e.tx,
        run: Some(e.run),
        kind: e.kind,
        at: e.at,
        target: e.target.map(|key| TargetRef {
            id: id_of(key),
            key,
        }),
        targets: None,
        actor: e.actor,
        reason: e.reason.clone(),
        argv: e.argv.clone(),
        changes: (!verbose).then(ChangeCounts::default),
        ops: verbose.then(Vec::new),
        items: verbose.then(Vec::new),
        reverts: None,
        result: e.result,
        backup: None,
        dismissed: e.dismissed,
        plan: None,
        doctor_run_id: None,
        repairs: None,
        dir: e.dir.clone(),
    }
}

fn line(world: &World, e: &HistoryEntry, verbose: bool) -> HistoryLine {
    line_with(e, verbose, &|key| world.target_id(key))
}

/// `e` as `history` lists it, naming each client by `id_of`. A target-less line (a
/// repair, or the undo of one) also carries its `targets`, doctor run and operations, as
/// it recorded them; every other line is listed as before M4.
fn line_with(e: &HistoryEntry, verbose: bool, id_of: &dyn Fn(TargetKey) -> String) -> HistoryLine {
    let target_ref = |key: TargetKey| TargetRef {
        id: id_of(key),
        key,
    };
    let target_less = e.target.is_none();
    let targets = target_less.then(|| {
        let mut targets: Vec<TargetRef> = e.targets.iter().copied().map(target_ref).collect();
        targets.sort_by(|a, b| (&a.id, a.key).cmp(&(&b.id, b.key)));
        targets
    });
    HistoryLine {
        tx: e.tx,
        run: e.run,
        kind: e.kind,
        at: e.at,
        target: e.target.map(target_ref),
        targets,
        actor: e.actor,
        reason: e.reason.clone(),
        argv: e.argv.clone(),
        changes: (!verbose).then(|| ChangeCounts::of(&e.ops)),
        ops: verbose.then(|| e.ops.clone()),
        // `run` reads them for the lines it shows (`history_items::Reader`).
        items: verbose.then(Vec::new),
        reverts: e.reverts,
        result: e.result,
        backup: e.backup.clone(),
        dismissed: None,
        plan: e.plan,
        doctor_run_id: target_less.then(|| e.doctor_run_id.clone()).flatten(),
        repairs: target_less.then(|| e.repairs.iter().map(shown).collect()),
        dir: None,
    }
}

/// `entry` as `history` shows it: every path with a component `WTF` (ASCII
/// case-insensitive) cut after it (`…/WTF/…`), so a hand-made line never prints an
/// account, realm or character name. A real repair never writes under `WTF/`; the line
/// on disk is never changed (M4-D49).
fn shown(entry: &RepairEntry) -> RepairEntry {
    let mut entry = entry.clone();
    let cut = |path: &mut std::path::PathBuf| *path = cut_at_wtf(path);
    cut(&mut entry.path);
    for path in [entry.to.as_mut(), entry.backup.as_mut()]
        .into_iter()
        .flatten()
    {
        cut(path);
    }
    for endpoints in [&mut entry.pre, &mut entry.post] {
        let predicates: Vec<&mut RepairPredicate> = match endpoints {
            RepairEndpoints::Rename { path, to } => vec![path, to],
            RepairEndpoints::Path(predicate) => vec![predicate],
        };
        for link in predicates
            .into_iter()
            .filter_map(|p| p.link_target.as_mut())
        {
            cut(link);
        }
    }
    entry
}

/// `path` up to and including its first `WTF` component, then `…`; unchanged without one.
fn cut_at_wtf(path: &std::path::Path) -> std::path::PathBuf {
    let mut kept = std::path::PathBuf::new();
    for part in path.iter() {
        kept.push(part);
        if part
            .to_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("WTF"))
        {
            kept.push("…");
            return kept;
        }
    }
    path.to_path_buf()
}

/// A repair line's "what" column: its first operation and path, how many others, and
/// its doctor run.
fn repair_summary(repairs: &[RepairEntry], doctor_run_id: Option<&str>) -> String {
    let Some(first) = repairs.iter().min_by_key(|r| r.seq) else {
        return "no file operations".to_owned();
    };
    let op = match first.op {
        RepairOp::WriteFile => "write",
        RepairOp::Chmod => "chmod",
        RepairOp::RenameNoReplace => "rename",
    };
    let mut text = format!("{op} {}", first.path.display());
    if repairs.len() > 1 {
        text.push_str(&format!(" and {} more", repairs.len() - 1));
    }
    if let Some(run) = doctor_run_id {
        text.push_str(&format!(" · doctor run {run}"));
    }
    text
}

/// The `--since` grammar of `history`, `net-log` and `errors` (and `doctor gc --before`):
/// an RFC 3339 time, or a whole number of these units before now (M4-D2: declared once,
/// read by [`parse_since`], `capabilities` and the manifest).
pub const SINCE: ValueSet = ValueSet::DurationOrTime(&[
    DurationUnit {
        suffix: "m",
        seconds: 60,
    },
    DurationUnit {
        suffix: "h",
        seconds: 3_600,
    },
    DurationUnit {
        suffix: "d",
        seconds: 86_400,
    },
    DurationUnit {
        suffix: "w",
        seconds: 604_800,
    },
]);

/// `--since`: an RFC 3339 time, or a whole number of minutes, hours, days or weeks
/// before `now` (`30m`, `12h`, `1d`, `2w`): [`SINCE`]'s units.
pub fn parse_since(s: &str, now: Timestamp) -> Option<Timestamp> {
    if let Ok(at) = s.parse::<Timestamp>() {
        return Some(at);
    }
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (count, unit) = s.split_at(split);
    let count: i64 = count.parse().ok()?;
    let ValueSet::DurationOrTime(units) = SINCE else {
        return None;
    };
    let unit = units.iter().find(|u| u.suffix == unit)?.seconds;
    now.checked_sub(SignedDuration::from_secs(count.checked_mul(unit)?))
        .ok()
}

/// A transaction id as human output shows it ([`short_id`]): enough to undo by prefix.
pub fn short_tx(tx: &TxId) -> String {
    short_id(&tx.to_string())
}

/// The first sixteen characters of an id, then `…`: a ULID's ten of time and six of
/// randomness, so ids minted in one millisecond (one run's transactions) still differ.
pub fn short_id(full: &str) -> String {
    format!("{}…", full.get(..16).unwrap_or(full))
}

/// The run whose id is `prefix` or starts with it (8+ characters, any case), among the
/// runs of `entries`.
pub fn resolve_run(entries: &[HistoryEntry], prefix: &str) -> Result<RunId> {
    resolve_run_in(entries.iter().filter_map(|e| e.run), prefix)
}

/// [`resolve_run`] among `runs`: `history` resolves over settings changes too, which
/// carry a run of their own.
fn resolve_run_in(runs: impl Iterator<Item = RunId>, prefix: &str) -> Result<RunId> {
    let wanted = prefix.to_ascii_uppercase();
    if wanted.len() < MIN_PREFIX {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("a run id prefix needs at least {MIN_PREFIX} characters, not {prefix:?}"),
            Suggestion::command(["omaddons", "history"]),
        ));
    }
    let mut matching: Vec<RunId> = runs
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
    fn since_units_drive_the_parser() {
        let now: Timestamp = "2026-10-14T12:00:00Z".parse().unwrap();
        let ValueSet::DurationOrTime(units) = SINCE else {
            panic!("SINCE is a duration grammar"); // ubs:ignore — the test's own failure
        };
        for unit in units {
            let given = format!("2{}", unit.suffix);
            assert_eq!(
                parse_since(&given, now),
                now.checked_sub(SignedDuration::from_secs(2 * unit.seconds))
                    .ok(),
                "{given}"
            );
        }
        for refused in ["2y", "m", "-1d", ""] {
            assert_eq!(parse_since(refused, now), None, "{refused:?}");
        }
        let at: Timestamp = "2026-01-02T03:04:05Z".parse().unwrap();
        assert_eq!(parse_since("2026-01-02T03:04:05Z", now), Some(at));
    }

    #[test]
    fn every_since_help_names_its_units() {
        use clap::CommandFactory;
        let ValueSet::DurationOrTime(units) = SINCE else {
            panic!("SINCE is a duration grammar"); // ubs:ignore — the test's own failure
        };
        let mut root = crate::cli::Cli::command();
        root.build();
        for row in crate::contract::commands::APP_VALIDATED
            .iter()
            .filter(|row| row.long == "since")
        {
            let path = row.path.unwrap_or_default();
            let cmd = path
                .split(' ')
                .try_fold(&root, |cmd, word| cmd.find_subcommand(word))
                .unwrap();
            let help = cmd
                .get_arguments()
                .find(|arg| arg.get_long() == Some("since"))
                .and_then(clap::Arg::get_help)
                .map(ToString::to_string)
                .unwrap_or_default();
            for unit in units {
                let named = help
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| {
                        word.strip_suffix(unit.suffix).is_some_and(|digits| {
                            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
                        })
                    });
                assert!(
                    named,
                    "{path} --since's help names no {}: {help}",
                    unit.suffix
                );
            }
        }
    }

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
            backup: None,
            from: None,
            files: None,
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
                recorded: 1,
                restored: 0,
            }
        );
        let pin = ChangeCounts::of(&[op(FolderOp::Record), op(FolderOp::Record)]);
        assert_eq!(pin.human(), "+0 ~0 -0 =2", "recorded folders are counted");
        assert_eq!(ChangeCounts::of(&[op(FolderOp::Add)]).human(), "+1 ~0 -0");
        // A whole restore names `WTF`, no addon, and counts its files.
        let wtf = HistoryOp {
            folder: crate::model::FolderName::new("WTF").unwrap(),
            addon: String::new(),
            files: Some(410),
            from: Some(crate::state::history::WtfSource::Backup),
            ..op(FolderOp::Wtf)
        };
        let restore = ChangeCounts::of(&[wtf]);
        assert_eq!(restore.restored, 410);
        assert_eq!(restore.human(), "+0 ~0 -0 · 410 files restored");
        let tx: TxId = "01JB8Q2VA00000000000000000".parse().unwrap();
        assert_eq!(short_tx(&tx), "01JB8Q2VA0000000…");
        assert_eq!(kind_name(TxKind::Undo), "undo");
    }

    // -----------------------------------------------------------------------------------
    // Repair lines (M4-D31).

    use crate::state::history::{PredicateKind, RepairIdentity};

    const KEY_A: &str = "01JB8Q2V9W0000000000000000";
    const KEY_B: &str = "01JB8Q2V9W0000000000000001";
    const RUN_DIR: &str = "2026-10-30T18-03-40Z__01M8S00000000000000000000";

    fn key(text: &str) -> TargetKey {
        text.parse().unwrap()
    }

    /// `A` is `retail`, `B` is `classic`.
    fn id_of(k: TargetKey) -> String {
        if k == key(KEY_A) { "retail" } else { "classic" }.to_owned()
    }

    fn predicate(kind: PredicateKind) -> RepairPredicate {
        RepairPredicate {
            kind,
            identity: Some(RepairIdentity {
                ino: "4215".into(),
                born_ns: Some("1792900000000000000".into()),
            }),
            sha256: None,
            mode: None,
            stamp: None,
            link_target: None,
        }
    }

    fn primitive(seq: u32, op: RepairOp, path: &str, to: Option<&str>) -> RepairEntry {
        RepairEntry {
            seq,
            fix: 1,
            fm: "fm-external_artifacts-nested-addon-folder".into(),
            op,
            path: path.into(),
            to: to.map(Into::into),
            mode: None,
            pre: RepairEndpoints::Path(predicate(PredicateKind::Dir)),
            post: RepairEndpoints::Path(predicate(PredicateKind::Absent)),
            backup: None,
            activation: false,
            guards: Vec::new(),
            target: None,
            folder: None,
        }
    }

    fn repair(targets: &[&str], repairs: Vec<RepairEntry>, run_dir: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            schema_version: history::CURRENT,
            tx: "01M9Q00000000000000000000A".parse().unwrap(),
            kind: TxKind::Repair,
            at: "2026-10-30T18:03:40Z".parse().unwrap(),
            target: None,
            targets: targets.iter().map(|t| key(t)).collect(),
            actor: Actor::Human,
            reason: None,
            argv: vec!["omaddons".into(), "doctor".into(), "fix".into()],
            ops: Vec::new(),
            result: TxResult::Committed,
            reverts: None,
            sv_deferred: false,
            sv_restored: None,
            created_addons_dir: false,
            run: None,
            backup: None,
            plan: None,
            doctor_run_id: run_dir.map(str::to_owned),
            repairs,
        }
    }

    /// The table `history` prints for `lines`.
    fn rendered(lines: Vec<HistoryLine>) -> String {
        rendered_with(lines, &Style::default())
    }

    /// The table `history` prints for `lines` in `style`.
    fn rendered_with(lines: Vec<HistoryLine>, style: &Style) -> String {
        let outcome = HistoryOutcome {
            data: HistoryData {
                entries: lines,
                truncated: false,
            },
            warnings: Vec::new(),
            torn_tail: false,
            shared_runs: BTreeSet::new(),
        };
        let mut out = Vec::new();
        outcome.render_human(&mut out, style).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_repair_line_renders_its_targets_paths_and_doctor_run() {
        let entry = repair(
            &[KEY_B, KEY_A],
            vec![
                primitive(
                    1,
                    RepairOp::RenameNoReplace,
                    "/c/_retail_/Interface/AddOns/X/Y",
                    Some("/c/_retail_/Interface/AddOns/Y"),
                ),
                primitive(
                    2,
                    RepairOp::RenameNoReplace,
                    "/c/_retail_/Interface/AddOns/X",
                    Some("/c/_retail_/Interface/.omaddons/quarantine/01M9Q/X"),
                ),
            ],
            Some(RUN_DIR),
        );
        let line = line_with(&entry, false, &id_of);
        let ids: Vec<&str> = line
            .targets
            .as_ref()
            .unwrap()
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["classic", "retail"], "sorted by id: {line:?}");
        assert_eq!(line.repairs.as_ref(), Some(&entry.repairs), "as recorded");
        assert_eq!(line.doctor_run_id.as_deref(), Some(RUN_DIR));
        let json = serde_json::to_value(&line).unwrap();
        assert_eq!(json["target"], serde_json::Value::Null, "{json}");
        assert_eq!(json["targets"][0]["key"], KEY_B, "{json}");
        let table = rendered(vec![line]);
        for expected in [
            "repair",
            "classic, retail",
            "rename /c/_retail_/Interface/AddOns/X/Y and 1 more",
            &format!("doctor run {RUN_DIR}"),
        ] {
            assert!(
                table.contains(expected),
                "{expected:?} missing from: {table}"
            );
        }
    }

    #[test]
    fn a_chmod_repair_names_its_one_path() {
        let path = "/home/u/.config/omaddons/credentials/github";
        let entry = repair(
            &[],
            vec![primitive(1, RepairOp::Chmod, path, None)],
            Some(RUN_DIR),
        );
        let table = rendered(vec![line_with(&entry, false, &id_of)]);
        assert!(
            table.contains(&format!("chmod {path} · doctor run {RUN_DIR}")),
            "{table}"
        );
        assert!(!table.contains("more"), "{table}");
        assert!(table.contains("  -  "), "no client: {table}");
    }

    #[test]
    fn an_empty_repair_line_says_so() {
        let table = rendered(vec![line_with(
            &repair(&[KEY_A], Vec::new(), None),
            false,
            &id_of,
        )]);
        assert!(table.contains("no file operations"), "{table}");
        assert!(!table.contains("doctor run"), "{table}");
    }

    #[test]
    fn a_path_under_wtf_is_cut_at_wtf() {
        let account = "/c/_retail_/WTF/Account/FAKEACCT/x.lua";
        let with_link = |link: &str| RepairEntry {
            post: RepairEndpoints::Path(RepairPredicate {
                link_target: Some(link.into()),
                ..predicate(PredicateKind::Symlink)
            }),
            ..primitive(1, RepairOp::WriteFile, "/c/x", None)
        };
        let cases = [
            (
                "path_under_wtf",
                primitive(1, RepairOp::WriteFile, account, None),
            ),
            (
                "lowercase_wtf",
                primitive(
                    1,
                    RepairOp::WriteFile,
                    "/c/_retail_/wtf/Account/FAKEACCT/x",
                    None,
                ),
            ),
            (
                "to_under_wtf",
                primitive(1, RepairOp::RenameNoReplace, "/c/a", Some(account)),
            ),
            (
                "backup_under_wtf",
                RepairEntry {
                    backup: Some(account.into()),
                    ..primitive(1, RepairOp::WriteFile, "/c/a", None)
                },
            ),
            ("link_target_under_wtf", with_link(account)),
            (
                "no_wtf_component",
                primitive(1, RepairOp::WriteFile, "/c/Account/x", None),
            ),
            (
                "wtf_inside_a_name",
                primitive(1, RepairOp::WriteFile, "/c/WTFish/x", None),
            ),
        ];
        for (name, input) in cases {
            let output = shown(&input);
            let json = serde_json::to_string(&output).unwrap();
            assert!(
                !json.contains("FAKEACCT"),
                "{name}: input {input:?} gave {json}"
            );
            let untouched = ["no_wtf_component", "wtf_inside_a_name"].contains(&name);
            assert_eq!(
                output == input,
                untouched,
                "{name}: input {input:?} gave {output:?}"
            );
            if !untouched {
                assert!(
                    json.contains("/WTF/…") || json.contains("/wtf/…"),
                    "{name}: gave {json}"
                );
            }
        }
    }

    #[test]
    fn lines_with_a_target_render_as_before() {
        let entry = HistoryEntry {
            target: Some(key(KEY_A)),
            kind: TxKind::Install,
            ops: vec![HistoryOp {
                op: FolderOp::Add,
                folder: crate::model::FolderName::new("Foo").unwrap(),
                addon: "Foo".into(),
                edit: None,
                backup: None,
                from: None,
                files: None,
            }],
            doctor_run_id: None,
            ..repair(&[], Vec::new(), None)
        };
        let line = line_with(&entry, false, &id_of);
        let json = serde_json::to_value(&line).unwrap();
        for absent in ["targets", "doctor_run_id", "repairs"] {
            assert!(json.get(absent).is_none(), "{absent} in {json}");
        }
        let table = rendered(vec![line]);
        assert!(
            table.contains("retail") && table.contains("+1 ~0 -0"),
            "{table}"
        );
    }

    // -----------------------------------------------------------------------------------
    // The plan a transaction was applied from (M4-D25).

    const PLAN: &str = "01M9P0000000000000000000PA";

    /// An install on `A`, applied from `plan`.
    fn install(tx: &str, plan: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            tx: tx.parse().unwrap(),
            target: Some(key(KEY_A)),
            kind: TxKind::Install,
            plan: plan.map(|p| p.parse().unwrap()),
            ..repair(&[], Vec::new(), None)
        }
    }

    #[test]
    fn plan_is_null_on_lines_without_one() {
        let setting = SettingEntry::now(
            TxKind::Config,
            None,
            Actor::Human,
            None,
            vec!["omaddons".into(), "config".into(), "set".into()],
        );
        let lines = [
            (
                "transaction",
                line_with(&install("01M9Q00000000000000000000B", None), false, &id_of),
            ),
            (
                "repair",
                line_with(&repair(&[KEY_A], Vec::new(), None), true, &id_of),
            ),
            ("setting", setting_line_with(&setting, false, &id_of)),
        ];
        for (name, line) in lines {
            let json = serde_json::to_value(&line).unwrap();
            assert_eq!(
                json.get("plan"),
                Some(&serde_json::Value::Null),
                "{name}: the key is always there, null: {json}"
            );
        }
    }

    #[test]
    fn plan_is_shown_on_lines_with_one() {
        let applied = line_with(
            &install("01M9Q00000000000000000000B", Some(PLAN)),
            false,
            &id_of,
        );
        let json = serde_json::to_value(&applied).unwrap();
        assert_eq!(json["plan"], PLAN, "{json}");
        let other = line_with(&install("01M9Q00000000000000000000C", None), false, &id_of);
        let lines = vec![applied, other];
        let verbose = Style {
            verbose: 1,
            ..Style::default()
        };
        let table = rendered_with(lines.clone(), &verbose);
        let rows: Vec<&str> = table.lines().collect();
        assert!(
            rows[0].contains("plan 01M9P00000000000…"),
            "-v names the plan on its row: {table}"
        );
        assert!(!rows[1].contains("plan"), "and on no other row: {table}");
        let plain = rendered(lines);
        assert!(!plain.contains("plan"), "only under -v: {plain}");
        let without = rendered_with(
            vec![line_with(
                &install("01M9Q00000000000000000000C", None),
                true,
                &id_of,
            )],
            &verbose,
        );
        assert!(
            !without.contains("plan"),
            "no column without a plan: {without}"
        );
    }

    #[test]
    fn history_line_shows_dir_only_when_set() {
        let argv = ["omaddons", "robot-docs", "install-skill"].map(str::to_owned);
        let skill = SettingEntry {
            dir: Some("/x/skills/omaddons".into()),
            ..SettingEntry::now(TxKind::Skill, None, Actor::Agent, None, argv.to_vec())
        };
        let config = SettingEntry::now(TxKind::Config, None, Actor::Human, None, argv.to_vec());
        let lines = [
            ("skill", setting_line_with(&skill, false, &id_of)),
            ("skill -v", setting_line_with(&skill, true, &id_of)),
            ("config", setting_line_with(&config, false, &id_of)),
            (
                "transaction",
                line_with(&install("01M9Q00000000000000000000B", None), false, &id_of),
            ),
            (
                "repair",
                line_with(&repair(&[KEY_A], Vec::new(), None), true, &id_of),
            ),
        ];
        for (name, line) in lines {
            let json = serde_json::to_value(&line).unwrap();
            if name.starts_with("skill") {
                assert_eq!(json["dir"], "/x/skills/omaddons", "{name}: {json}");
                assert_eq!(json["kind"], "skill", "{name}: {json}");
                assert_eq!(json["target"], serde_json::Value::Null, "{name}: {json}");
            } else {
                assert_eq!(json.get("dir"), None, "{name}: omitted: {json}");
            }
        }
        assert_eq!(kind_name(TxKind::Skill), "skill");
    }
}
