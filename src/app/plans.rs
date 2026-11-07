//! `omaddons plans show` (D9): what a stored plan approved, and what history records of
//! it, for an agent that lost an `apply`'s output (M4-D63). It reads the plan and a
//! bounded tail of history, the unfinished journals and the registry, and nothing else:
//! no `World` (which writes the discovery cache), no lock, no request, no write. Only
//! history lines carrying the plan's id count as applied, never addon versions that happen
//! to match.

use std::collections::BTreeSet;
use std::io::Write;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use super::approval::Approval;
use super::history::{MIN_PREFIX, short_id};
use super::intent::StoredIntent;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::engine::journal;
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::limits::PLAN_EVIDENCE_LINES_MAX;
use crate::model::{PlanId, TargetKey, TxId};
use crate::output::{Outcome, Style};
use crate::state::history::{self, PlanEvidence, PlanLine, PlanMatch, ScanStop, TxResult};
use crate::state::paths::Paths;
use crate::state::plans::{self, Found, StoredPlan};
use crate::state::registry::{Registry, load_registry};

/// `plans show`'s help after its flags.
const PLANS_SHOW_HELP: &str = "Examples:
  omaddons plans show 01M8Q2VA          what plan 01M8Q2VA… approved, and whether it was applied
  omaddons plans show 01M8Q2VA --json   the same as one JSON envelope, for an agent

A plan is what a --dry-run stored (data.plan_id). plans show reads it and history.jsonl
and changes nothing: no lock, no network. Its state is pending, completed, partial,
expired or unknown; only history lines that carry the plan id count as applied, never
addon versions that happen to match. -t does not apply: a plan names its own clients.";

/// `plans`' arguments: its one subcommand.
#[derive(Debug, Clone, clap::Args)]
pub struct PlansArgs {
    #[command(subcommand)]
    pub action: PlansAction,
}

/// What `plans` does.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum PlansAction {
    /// What a stored plan approved, and whether history records it as applied (offline;
    /// changes nothing)
    #[command(after_long_help = PLANS_SHOW_HELP)]
    Show(ShowArgs),
}

impl PlansAction {
    /// The envelope `command`.
    pub const fn command(&self) -> &'static str {
        match self {
            Self::Show(_) => "plans show",
        }
    }
}

/// `plans show`'s arguments.
#[derive(Debug, Clone, clap::Args)]
pub struct ShowArgs {
    /// The plan id a --dry-run returned (data.plan_id), or its first 8 or more characters
    #[arg(value_name = "PLAN_ID")]
    pub plan_id: String,
}

// ---------------------------------------------------------------------------------------
// The data.

/// `plans show`'s data: the stored approval, history's evidence, and the state they make.
/// Every key is always present, `null` when it is not known.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlansShowData {
    pub plan_id: PlanId,
    pub state: PlanState,
    /// A plan file of this id is in the cache.
    pub stored: bool,
    /// It read; `false` whenever nothing was read.
    pub readable: bool,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub created_at: Option<Timestamp>,
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub expires_at: Option<Timestamp>,
    pub build_id: Option<String>,
    /// The plan was made by this build of omaddons.
    pub build_matches: Option<bool>,
    pub intent: Option<IntentSummary>,
    pub approval: Option<Approval>,
    /// The absolute inputs it installs from, in the intent's order.
    pub inputs: Vec<PlanInput>,
    pub evidence: Evidence,
}

/// The command a plan runs.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IntentSummary {
    /// Its path: `install`, `backup restore`.
    pub command: String,
    pub display_argv: Vec<String>,
}

/// An input a plan installs from, and whether it is still there (so `apply` would run).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanInput {
    pub path: String,
    pub present: bool,
}

/// What history records of the plan.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Evidence {
    /// One per history line carrying the plan, oldest first (commit order).
    pub targets: Vec<EvidenceTarget>,
    /// The scan covered its whole window: its bound did not cut it.
    pub complete: bool,
}

/// One transaction a plan committed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EvidenceTarget {
    /// The client's id: the registry's, else the approval's, else its key; `this machine`
    /// for the undo of a repair of omaddons' own files.
    pub target: String,
    /// `null` for the undo of a repair of omaddons' own files, which names no client.
    pub key: Option<TargetKey>,
    pub tx: TxId,
    pub result: TxResult,
    /// When its history line was appended, under the commit lock.
    #[schemars(schema_with = "crate::contract::schema::timestamp")]
    pub committed_at: Timestamp,
}

/// Whether a plan was applied, as history shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// Stored, unexpired, no commit recorded.
    Pending,
    /// A committed transaction for every client of the approved selection.
    Completed,
    /// Some, not all.
    Partial,
    /// Past its expiry, nothing recorded.
    Expired,
    /// The file is gone or unreadable, or history cannot tell.
    Unknown,
}

// ---------------------------------------------------------------------------------------
// The judgment.

/// Why a state is `unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Why {
    FileGone,
    Unreadable(String),
    NoHistoryLine,
    NoApprovedClients,
    Disagrees,
    ScanCut,
    UnreadableLines(usize),
    Unfinished { tx: TxId, client: String },
}

/// An unfinished journal: its transaction, the client it names (as [`Judgement`]'s
/// identities spell it; `None` when it names none), and that client's name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Open {
    tx: TxId,
    client: Option<String>,
    name: String,
}

/// What [`judge`] decides from. Clients are identities: a key's string, or `unregistered
/// <id>` for an approved client that has no key yet.
#[derive(Debug, Clone, Default)]
struct Judgement {
    stored: bool,
    readable: bool,
    records_history: bool,
    approved: BTreeSet<String>,
    committed: BTreeSet<String>,
    complete: bool,
    /// Lines in the window that did not read, or matched and could not be attributed.
    unreadable: usize,
    open: Vec<Open>,
    expired: bool,
}

/// The state history shows, and why when it cannot tell (M4-D63). First match wins: a
/// commit on every approved client is certain whatever the scan missed, so it comes
/// before a cut scan, unreadable lines and open journals.
fn judge(j: &Judgement) -> (PlanState, Option<Why>) {
    use PlanState::{Completed, Expired, Partial, Pending, Unknown};
    let unknown = |why| (Unknown, Some(why));
    if !j.stored {
        return unknown(Why::FileGone);
    }
    if !j.readable {
        return unknown(Why::Unreadable(String::new()));
    }
    if !j.records_history {
        return unknown(Why::NoHistoryLine);
    }
    if j.approved.is_empty() {
        return unknown(Why::NoApprovedClients);
    }
    if !j.committed.is_subset(&j.approved) {
        return unknown(Why::Disagrees);
    }
    if j.approved.is_subset(&j.committed) {
        return (Completed, None);
    }
    if !j.complete {
        return unknown(Why::ScanCut);
    }
    if j.unreadable > 0 {
        return unknown(Why::UnreadableLines(j.unreadable));
    }
    if let Some(open) = j
        .open
        .iter()
        .find(|o| o.client.as_ref().is_none_or(|c| j.approved.contains(c)))
    {
        return unknown(Why::Unfinished {
            tx: open.tx,
            client: open.name.clone(),
        });
    }
    if !j.committed.is_empty() {
        return (Partial, None);
    }
    if j.expired {
        return (Expired, None);
    }
    (Pending, None)
}

/// Whether a run of `intent` records a history line: `backup prune` appends none, so
/// history cannot show whether a prune plan ran.
fn records_history(intent: &StoredIntent) -> bool {
    match intent {
        StoredIntent::BackupPrune(_) => false,
        StoredIntent::Install(_)
        | StoredIntent::Update(_)
        | StoredIntent::Pin(_)
        | StoredIntent::Unpin(_)
        | StoredIntent::Adopt(_)
        | StoredIntent::Sync(_)
        | StoredIntent::Copy(_)
        | StoredIntent::Remove(_)
        | StoredIntent::Undo(_)
        | StoredIntent::BackupRestore(_) => true,
    }
}

// ---------------------------------------------------------------------------------------
// The command.

/// `plans show`'s outcome.
#[derive(Debug)]
pub struct PlansShowOutcome {
    data: PlansShowData,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    notes: Vec<String>,
    view: View,
}

/// What the human view says beyond the data.
#[derive(Debug, Clone)]
struct View {
    why: Option<Why>,
    summary: Vec<String>,
    /// The approved clients' names, and which of them no line names.
    approved: Vec<String>,
    unrecorded: Vec<String>,
    horizon: Timestamp,
    lines_read: usize,
    stop: ScanStop,
}

/// Runs `plans show`: offline, write-free, lock-free.
pub fn show(_ctx: &Context, args: &ShowArgs) -> Result<PlansShowOutcome> {
    show_in(&Paths::from_env()?, &args.plan_id, crate::clock::now())
}

fn show_in(paths: &Paths, given: &str, now: Timestamp) -> Result<PlansShowOutcome> {
    let given = given.to_ascii_uppercase();
    if given.chars().count() < MIN_PREFIX {
        return Err(Error::new(
            ErrorCode::Usage,
            format!("a plan id or prefix has at least {MIN_PREFIX} characters, not {given:?}"),
            Suggestion::command(["omaddons", "help", "plans", "show"]),
        ));
    }
    let mut notes = Vec::new();
    // Stored plans first, then history.
    let (stored_id, file) = match plans::find(paths, &given)? {
        Found::One { id, path } => (Some(id), Some(path)),
        Found::Many(ids) => return Err(ambiguous(&given, &ids, "stored plans have")),
        Found::None { .. } => (None, None),
    };
    let (plan, unreadable) = match file.as_deref().map(plans::load) {
        Some(Ok(plan)) => (Some(plan), None),
        Some(Err(err)) => {
            notes.push(format!(
                "plans show: the plan file does not read: {}",
                err.message
            ));
            (None, Some(err.message.clone()))
        }
        None => (None, None),
    };
    let named = stored_id.map_or_else(|| given.clone(), |id| id.to_string());
    let horizon = plans::evidence_horizon(plan.as_ref().map(|p| p.created_at), &named, now);
    let wanted = match stored_id {
        Some(id) => PlanMatch::Exact(id),
        None => PlanMatch::Prefix(&given),
    };
    let mut evidence = history::plan_evidence(paths, &wanted, horizon, PLAN_EVIDENCE_LINES_MAX)?;
    notes.push(format!(
        "plans show: history read {} lines back to {horizon}, {}",
        evidence.lines_read,
        stopped(evidence.stop)
    ));
    let id = match stored_id {
        Some(id) => id,
        None => {
            let ids: BTreeSet<PlanId> = evidence.lines.iter().map(|l| l.plan).collect();
            let ids: Vec<PlanId> = ids.into_iter().collect();
            match ids.as_slice() {
                [] => {
                    // The horizon is the id's own time, or a day before now, which moves.
                    let since = plans::encoded_time(&given).filter(|at| *at == horizon);
                    return Err(not_found(&given, since, evidence.complete()));
                }
                [one] => *one,
                _ => return Err(ambiguous(&given, &ids, "plans in history have")),
            }
        }
    };
    evidence.lines.retain(|l| l.plan == id);
    let open = journal::pending(paths)?;
    let mut warnings = journal::pending_warnings(paths)?;
    let registry = load_registry(paths)
        .map(|loaded| loaded.value)
        .unwrap_or_else(|err| {
            notes.push(format!(
                "plans show: the registry does not read: {}",
                err.message
            ));
            Registry::default()
        });
    let names = Names {
        registry: &registry,
        approval: plan.as_ref().map(|p| &p.approval),
    };
    let skipped = evidence.unreadable + evidence.unattributed;
    if skipped > 0 {
        warnings.push(skipped_warning(skipped));
    }
    let approved = plan
        .as_ref()
        .map(|p| names.approved(&p.approval))
        .unwrap_or_default();
    let committed: BTreeSet<String> = evidence
        .lines
        .iter()
        .flat_map(PlanLine::clients)
        .map(|client| client.map_or_else(String::new, |k| k.to_string()))
        .collect();
    let judgement = Judgement {
        stored: stored_id.is_some(),
        readable: plan.is_some(),
        records_history: plan.as_ref().is_some_and(|p| records_history(&p.intent)),
        approved: approved
            .iter()
            .map(|(identity, _)| identity.clone())
            .collect(),
        committed: committed.clone(),
        complete: evidence.complete(),
        unreadable: skipped,
        open: open
            .iter()
            .map(|o| Open {
                tx: o.tx,
                client: o.target.map(|k| k.to_string()),
                name: o
                    .target
                    .map_or_else(|| "a client it does not name".to_owned(), |k| names.of(k)),
            })
            .collect(),
        expired: plan.as_ref().is_some_and(|p| p.is_expired(now)),
    };
    let (state, mut why) = judge(&judgement);
    if let (Some(Why::Unreadable(message)), Some(read)) = (why.as_mut(), unreadable) {
        *message = read;
    }
    let view = View {
        why,
        summary: plan.as_ref().map(|p| p.summary.clone()).unwrap_or_default(),
        approved: approved.iter().map(|(_, name)| name.clone()).collect(),
        unrecorded: approved
            .iter()
            .filter(|(identity, _)| !committed.contains(identity))
            .map(|(_, name)| name.clone())
            .collect(),
        horizon,
        lines_read: evidence.lines_read,
        stop: evidence.stop,
    };
    let commands = undo_hints(&evidence.lines, &names);
    let data = data_of(
        id,
        state,
        stored_id.is_some(),
        plan.as_ref(),
        &evidence,
        &names,
    );
    Ok(PlansShowOutcome {
        data,
        warnings,
        commands,
        notes,
        view,
    })
}

/// How a scan stopped, for the notes and `-v`.
fn stopped(stop: ScanStop) -> &'static str {
    match stop {
        ScanStop::Start => "stopped at the start",
        ScanStop::Horizon => "stopped at the horizon",
        ScanStop::Bound => "stopped at its bound",
    }
}

/// How clients are named: by the registry, else by the approval, else by key.
struct Names<'a> {
    registry: &'a Registry,
    approval: Option<&'a Approval>,
}

impl Names<'_> {
    /// [`Self::of`] the client, or `this machine` for none.
    fn of_client(&self, key: Option<TargetKey>) -> String {
        key.map_or_else(|| "this machine".to_owned(), |key| self.of(key))
    }

    fn of(&self, key: TargetKey) -> String {
        self.registry
            .targets
            .iter()
            .find(|e| e.key == key)
            .map(|e| e.id.clone())
            .or_else(|| {
                self.approval?
                    .selection
                    .iter()
                    .find(|item| item.target_key == Some(key))
                    .map(|item| item.target_id.clone())
            })
            .unwrap_or_else(|| key.to_string())
    }

    /// The approved clients, each `(identity, name)`, sorted by name, once each. A client
    /// approved before it was registered is known by the key the registry gave its id
    /// since, else as `unregistered <id>`.
    fn approved(&self, approval: &Approval) -> Vec<(String, String)> {
        let mut clients: Vec<(String, String)> = Vec::new();
        for item in &approval.selection {
            // An undo of a repair of omaddons' own files alone names no client: the
            // machine, whose identity matches its committed line's.
            if item.target_key.is_none() && item.target_id.is_empty() {
                if !clients.iter().any(|(i, _)| i.is_empty()) {
                    clients.push((String::new(), self.of_client(None)));
                }
                continue;
            }
            let key = item.target_key.or_else(|| {
                self.registry
                    .targets
                    .iter()
                    .find(|e| e.id == item.target_id)
                    .map(|e| e.key)
            });
            let identity = key.map_or_else(
                || format!("unregistered {}", item.target_id),
                |k| k.to_string(),
            );
            let name = key.map_or_else(|| item.target_id.clone(), |k| self.of(k));
            if !clients.iter().any(|(i, _)| *i == identity) {
                clients.push((identity, name));
            }
        }
        clients.sort_by(|a, b| a.1.cmp(&b.1));
        clients
    }
}

fn data_of(
    plan_id: PlanId,
    state: PlanState,
    stored: bool,
    plan: Option<&StoredPlan>,
    evidence: &PlanEvidence,
    names: &Names<'_>,
) -> PlansShowData {
    PlansShowData {
        plan_id,
        state,
        stored,
        readable: plan.is_some(),
        created_at: plan.map(|p| p.created_at),
        expires_at: plan.map(|p| p.expires_at),
        build_id: plan.map(|p| p.build_id.clone()),
        build_matches: plan.map(|p| p.build_id == crate::build_info::BUILD_ID),
        intent: plan.map(|p| IntentSummary {
            command: p.intent.command_path().to_owned(),
            display_argv: p.display_argv.clone(),
        }),
        approval: plan.map(|p| p.approval.clone()),
        inputs: plan
            .map(|p| {
                p.intent
                    .inputs()
                    .into_iter()
                    .map(|input| PlanInput {
                        path: input.as_str().to_owned(),
                        present: !plans::input_is_gone(input.as_path()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        evidence: Evidence {
            targets: evidence
                .lines
                .iter()
                .flat_map(|line| {
                    line.clients().into_iter().map(|key| EvidenceTarget {
                        target: names.of_client(key),
                        key,
                        tx: line.tx,
                        result: line.result,
                        committed_at: line.at,
                    })
                })
                .collect(),
            complete: evidence.complete(),
        },
    }
}

/// The undo of what the plan committed, as `TargetsData::undo_commands` offers it: the
/// whole run first when several transactions share it, then each transaction by client.
fn undo_hints(lines: &[PlanLine], names: &Names<'_>) -> Vec<CommandHint> {
    let mut commands = Vec::new();
    let runs: BTreeSet<_> = lines.iter().map(|l| l.run).collect();
    if lines.len() > 1
        && runs.len() == 1
        && let Some(Some(run)) = runs.first()
    {
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
    let mut by_client: Vec<(String, TxId)> = lines
        .iter()
        .map(|l| {
            (
                names.of_client(l.clients().first().copied().flatten()),
                l.tx,
            )
        })
        .collect();
    by_client.sort();
    for (_, tx) in by_client {
        commands.push(CommandHint::new(
            Action::Undo,
            ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
        ));
    }
    commands
}

/// `history_lines_skipped`, worded as `history` words it, for lines of the scan's window.
fn skipped_warning(n: usize) -> Warning {
    let message = match n {
        1 => "1 line of history.jsonl since the plan was made could not be read or attributed; the transaction on it may be this plan's".to_owned(),
        n => format!(
            "{n} lines of history.jsonl since the plan was made could not be read or attributed; the transactions on them may be this plan's"
        ),
    };
    Warning::new(WarningCode::HistoryLinesSkipped, message).with_fix(Suggestion::text(
        "a line was damaged by a crash or a hand edit, or written by a newer omaddons; omaddons never rewrites history.jsonl",
    ))
}

/// Several plans start with `given`: the first is the fix, each other an alternative.
fn ambiguous(given: &str, ids: &[PlanId], whose: &str) -> Error {
    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let show = |id: &str| Suggestion::command(["omaddons", "plans", "show", id]);
    let mut error = Error::new(
        ErrorCode::RefAmbiguous,
        format!(
            "{} {whose} ids starting with {given}: {}",
            names.len(),
            names.join(", ")
        ),
        show(names.first().map_or("", String::as_str)),
    )
    .details(json!({ "prefix": given, "candidates": names }));
    for id in names.iter().skip(1) {
        error = error.alternative(show(id));
    }
    error
}

/// No stored plan, and no history line since `since` (the time the id encodes; `None`:
/// the last day), names `given`.
fn not_found(given: &str, since: Option<Timestamp>, complete: bool) -> Error {
    let since = since.map_or_else(
        || "in the last 24 hours".to_owned(),
        |at| format!("since {at}"),
    );
    let cut = if complete {
        String::new()
    } else {
        format!(
            "; history holds more than {PLAN_EVIDENCE_LINES_MAX} lines since then, and the search stopped there"
        )
    };
    Error::new(
        ErrorCode::PlanNotFound,
        format!(
            "no stored plan has an id starting with {given}, and history records no apply of one {since}{cut}"
        ),
        Suggestion::command(["omaddons", "help", "plans", "show"]),
    )
    .details(json!({ "plan_id": given }))
}

// ---------------------------------------------------------------------------------------
// Output.

impl Outcome for PlansShowOutcome {
    type Data = PlansShowData;

    fn command(&self) -> &'static str {
        "plans show"
    }

    fn data(&self) -> &PlansShowData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        self.render_in(w, style, &TimeZone::system())
    }
}

impl super::SideEvents for PlansShowOutcome {
    fn notes(&self) -> &[String] {
        &self.notes
    }
}

/// `at` as `history` shows times, in `tz`.
fn when(at: Timestamp, tz: &TimeZone) -> String {
    at.to_zoned(tz.clone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// `argv` as one shell-quoted line.
fn shell(argv: &[String]) -> String {
    Suggestion::command(argv.to_vec()).display().to_owned()
}

/// `display_argv` previewed again: `--dry-run` before its first `--`, else at the end.
fn preview_again(display_argv: &[String]) -> String {
    let mut argv = display_argv.to_vec();
    let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
    argv.insert(at, "--dry-run".to_owned());
    shell(&argv)
}

impl PlansShowOutcome {
    fn render_in(&self, w: &mut dyn Write, style: &Style, tz: &TimeZone) -> std::io::Result<()> {
        let d = &self.data;
        let v = &self.view;
        let committed = d.evidence.targets.len();
        let clients = v.approved.len();
        let state = match d.state {
            PlanState::Pending => format!(
                "pending, stored until {}, nothing applied yet",
                d.expires_at.map(|at| when(at, tz)).unwrap_or_default()
            ),
            PlanState::Completed => format!("completed, {clients} of {clients} clients committed"),
            PlanState::Partial => {
                let k = clients - v.unrecorded.len();
                format!("partial, {k} of {clients} clients committed")
            }
            PlanState::Expired => format!(
                "expired at {}, nothing applied",
                d.expires_at.map(|at| when(at, tz)).unwrap_or_default()
            ),
            PlanState::Unknown => format!("unknown: {}", self.why_text()),
        };
        match &d.intent {
            Some(intent) => writeln!(w, "plan {} ({}): {state}", d.plan_id, intent.command)?,
            None => writeln!(w, "plan {}: {state}", d.plan_id)?,
        }
        if let (Some(intent), Some(approval)) = (&d.intent, &d.approval) {
            writeln!(w, "approved: {}", shell(&intent.display_argv))?;
            if v.summary.is_empty() {
                for item in &approval.selection {
                    let op = serde_json::to_value(item.op)
                        .ok()
                        .and_then(|op| op.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    writeln!(
                        w,
                        "  {}: {op} {} {} → {}",
                        item.target_id,
                        item.addon.as_deref().unwrap_or_default(),
                        item.from_version.as_deref().unwrap_or("-"),
                        item.to_version.as_deref().unwrap_or("-"),
                    )?;
                }
            } else {
                for line in &v.summary {
                    writeln!(w, "  {line}")?;
                }
            }
        }
        if committed > 0 {
            writeln!(w, "committed:")?;
            for t in &d.evidence.targets {
                let tx = t.tx.to_string();
                writeln!(
                    w,
                    "  {}: tx {} at {} · undo: omaddons undo {tx}",
                    t.target,
                    short_id(&tx),
                    when(t.committed_at, tz)
                )?;
            }
        }
        if d.state == PlanState::Partial {
            writeln!(w, "not recorded: {}", v.unrecorded.join(", "))?;
        }
        if let Some(advice) = self.advice() {
            writeln!(w, "{advice}")?;
        }
        if style.detail() {
            if let Some(approval) = &d.approval {
                let reasons: Vec<String> = approval
                    .reasons
                    .iter()
                    .filter_map(|r| serde_json::to_value(r).ok())
                    .filter_map(|r| r.as_str().map(str::to_owned))
                    .collect();
                writeln!(
                    w,
                    "reasons: {}",
                    if reasons.is_empty() {
                        "none (routine)".to_owned()
                    } else {
                        reasons.join(", ")
                    }
                )?;
            }
            if let (Some(build), Some(matches)) = (&d.build_id, d.build_matches) {
                let whose = if matches {
                    "this build"
                } else {
                    "another build"
                };
                writeln!(w, "build: {build} ({whose})")?;
            }
            for input in &d.inputs {
                let there = if input.present { "present" } else { "missing" };
                writeln!(w, "inputs: {} ({there})", input.path)?;
            }
            writeln!(
                w,
                "history: read {} lines back to {} ({})",
                v.lines_read,
                when(v.horizon, tz),
                stopped(v.stop)
            )?;
        }
        Ok(())
    }

    fn why_text(&self) -> String {
        match &self.view.why {
            Some(Why::FileGone) => "its file is no longer in the cache, so what it approved is not known; history shows:".to_owned(),
            Some(Why::Unreadable(message)) => {
                format!("its file cannot be read ({message}), so nothing in it is trusted")
            }
            Some(Why::NoHistoryLine) => {
                "backup prune records no history line, so history cannot show whether it ran"
                    .to_owned()
            }
            Some(Why::NoApprovedClients) => "its approval names no client".to_owned(),
            Some(Why::Disagrees) => "history names a client the plan did not approve".to_owned(),
            Some(Why::ScanCut) => format!(
                "history holds more than {PLAN_EVIDENCE_LINES_MAX} lines since the plan was made; the search stopped there"
            ),
            Some(Why::UnreadableLines(n)) => {
                format!("{n} lines of history.jsonl in that time could not be read")
            }
            Some(Why::Unfinished { tx, client }) => format!(
                "an interrupted transaction ({tx}) on {client} may hold its commit; the next change on that client finishes or rolls it back"
            ),
            None => String::new(),
        }
    }

    /// The one line of advice the state calls for.
    fn advice(&self) -> Option<String> {
        let d = &self.data;
        let display = &d.intent.as_ref()?.display_argv;
        match (d.state, &self.view.why) {
            (PlanState::Pending, _) => Some(if d.build_matches == Some(false) {
                format!(
                    "made by another build of omaddons, so apply will refuse it: preview again with {}",
                    preview_again(display)
                )
            } else if let Some(gone) = d.inputs.iter().find(|i| !i.present) {
                format!(
                    "{} is gone, so apply will refuse it: preview again with {}",
                    gone.path,
                    preview_again(display)
                )
            } else {
                format!("apply it with: omaddons apply {}", d.plan_id)
            }),
            (PlanState::Partial, _) => Some(format!(
                "apply never resumes a partial plan; preview again for the rest: {}",
                preview_again(display)
            )),
            (PlanState::Expired, _) => Some(format!("preview again: {}", preview_again(display))),
            (PlanState::Unknown, Some(Why::NoHistoryLine)) => {
                Some(format!("see what is left: {}", preview_again(display)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
