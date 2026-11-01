//! `omaddons doctor undo [<run>] [--dry-run]` (E7, M4-D46, §6.11): the undo of a `doctor
//! fix` repair, addressed by the doctor run that made it. It resolves the run to its
//! transaction and undoes that through the same engine code as `omaddons undo <tx>`, with
//! its own run directory of kind `undo`. With no run, it takes the newest repair still in
//! effect, whatever diagnoses ran since. Undoing a run already undone does nothing and
//! exits 0. It neither stores a plan nor binds (M4-D22): its bound form is `omaddons undo
//! <tx> --dry-run`, then `apply`, which its dry run offers.

use std::fs;
use std::io::{Read, Write};

use serde::Deserialize;

use super::engine::Engine;
use super::history::short_tx;
use super::world::World;
use crate::cli::Context;
use crate::contract::actions::Action;
use crate::doctor::exit::{Exit, exit_for};
use crate::doctor::report::{ActionPlanned, DOCTOR_CONTRACT_VERSION, RunState, UndoReport};
use crate::doctor::runtime::{DoctorRunId, REPORT_FILE};
use crate::engine::repair::undo::{
    Recorded, UndoState, already_undone_by, is_repair_family, undo_state,
};
use crate::engine::repair_action::Primitive;
use crate::engine::{SvPolicy, UndoRequest, UndoSelector};
use crate::error::{CommandHint, Error, ErrorCode, Suggestion, Warning};
use crate::limits::API_RESPONSE_MAX_BYTES;
use crate::model::{RunId, TxId, TxKind};
use crate::output::{DoctorFailure, DoctorOutcome, Style};
use crate::state::history::{self, HistoryEntry, HistoryFilter, RepairOp, TxResult};
use crate::state::paths::Paths;

/// `doctor undo`'s argument (`--dry-run` is global).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DoctorUndoArgs {
    /// The doctor run to undo: its run id, as doctor fix printed it (default: the newest
    /// fix run whose repair is still in effect)
    #[arg(value_name = "RUN")]
    pub run: Option<String>,
}

pub const UNDO_HELP: &str = "Examples:\n  omaddons doctor undo                                the newest repair still in effect\n  omaddons doctor undo 2026-10-30T18-03-40Z__01M8S…   the repair that run made\n  omaddons doctor undo --dry-run                      what it would revert, changing nothing\n  omaddons undo <tx> --dry-run                        the bindable preview: then omaddons apply <plan>";

/// The most inverse primitives a dry run's human output lists before `and N more`.
const LINES_SHOWN: usize = 10;

/// What a `doctor undo` did, as the command renders it.
#[derive(Debug)]
pub struct DoctorUndoRun {
    report: UndoReport,
    exit: Exit,
    error: Option<Error>,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    said: Said,
    /// Each primitive as `(op, path, to)`: committed, or planned under `--dry-run`.
    ops: Vec<(&'static str, String, Option<String>)>,
    /// Interrupted changes the reverted run completed, which it does not revert (M4-D37).
    delegated: Vec<TxId>,
}

/// Which outcome the human output describes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Said {
    Undone,
    Planned,
    /// Bare `doctor undo`, with no repair in effect.
    NothingInEffect,
    /// A run that committed no repair: a diagnose, a dry run, a rolled-back fix.
    ChangedNothing(DoctorRunId),
    AlreadyUndone,
    /// Refused or rolled back after its run directory existed.
    Stopped,
}

/// Runs `doctor undo`: the arguments, then history, then the engine.
pub fn run(ctx: &Context, args: &DoctorUndoArgs) -> Result<DoctorUndoRun, DoctorFailure> {
    let started_at = crate::clock::now();
    let given = args.run.as_deref();
    if given.is_some_and(|v| v.eq_ignore_ascii_case("latest")) {
        return Err(DoctorFailure::from(latest_refused()));
    }
    let paths = Paths::from_env()?;
    let read = history::read(&paths, &HistoryFilter::default())?;
    ctx.notes.warn_all(read.skipped_warning().as_slice());
    let entries = &read.entries;
    let at = match select(&paths, entries, given)? {
        Selected::At(at) => at,
        Selected::Nothing(said) => return Ok(nothing(&paths, started_at, said)),
    };
    let entry = &entries[at];
    let delegated = delegated_forward(&paths, entry);
    if let UndoState::UndoneBy(by) = undo_state(entries, at) {
        return Ok(already_undone(started_at, entry, by, None, delegated));
    }

    let world = World::load(ctx)?;
    let engine = Engine::new(ctx, &world, "doctor undo")?;
    let req = UndoRequest {
        selector: UndoSelector::Tx(entry.tx),
        sv_policy: SvPolicy::Auto,
        dry_run: ctx.dry_run,
        while_running: ctx.while_running,
        reason: ctx.reason.clone(),
        targets: world.resolved.targets.clone(),
        run: RunId::new(),
    };
    let (result, recorded) = engine.undo_repair_reported(&req, &read, at);
    let undo = match result {
        Ok(undo) => undo,
        Err(error) => {
            // Another process undid it between this selection and the lock: as undone.
            if let Some(by) = already_undone_by(&error) {
                let run = match recorded {
                    Recorded::Report(report) => Some(*report),
                    Recorded::Nothing | Recorded::CannotCreate => None,
                };
                return Ok(already_undone(started_at, entry, by, run, delegated));
            }
            return stopped(error, recorded, delegated);
        }
    };
    let warnings = undo.outcome.warnings.clone();
    let Some(repair) = undo.repair else {
        return Err(DoctorFailure::from(Error::internal(format!(
            "the undo of repair {} came back without its repair facts",
            entry.tx
        ))));
    };
    let mut commands = Vec::new();
    let (report, said, ops) = if ctx.dry_run {
        // The bindable preview: it stores a plan `apply` runs (M4-D22).
        commands.push(CommandHint::new(
            Action::Undo,
            ["omaddons", "undo", &entry.tx.to_string(), "--dry-run"].map(str::to_owned),
        ));
        let ops = repair.planned.iter().map(planned_op).collect();
        let report = UndoReport {
            doctor_contract_version: DOCTOR_CONTRACT_VERSION,
            run_id: None,
            run_dir: None,
            started_at,
            finished_at: crate::clock::now(),
            reverted_run: entry.doctor_run_id.clone(),
            tx: Some(entry.tx),
            undo_tx: None,
            state: RunState::DoneOk,
            actions_taken: 0,
            already_undone: false,
            actions_planned: Some(repair.planned),
        };
        (report, Said::Planned, ops)
    } else {
        let Recorded::Report(report) = recorded else {
            return Err(DoctorFailure::from(Error::internal(format!(
                "the undo of repair {} committed without its run's report",
                entry.tx
            ))));
        };
        let report = *report;
        if let Some(tx) = report.undo_tx {
            commands.push(CommandHint::new(
                Action::Undo,
                ["omaddons", "undo", &tx.to_string()].map(str::to_owned),
            ));
        }
        let ops = repair
            .committed
            .iter()
            .map(|e| {
                (
                    op_word(e.op),
                    e.path.display().to_string(),
                    e.to.as_ref().map(|to| to.display().to_string()),
                )
            })
            .collect();
        (report, Said::Undone, ops)
    };
    commands.extend(undo_hints(&delegated));
    Ok(DoctorUndoRun {
        report,
        exit: Exit::Healthy,
        error: None,
        warnings,
        commands,
        said,
        ops,
        delegated,
    })
}

/// What `doctor undo`'s argument selects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selected {
    /// The history line to undo.
    At(usize),
    /// Nothing to undo, and why.
    Nothing(Said),
}

/// What `given` selects in `entries`, reading nothing beyond them but whether a run's
/// directory exists: the line to undo, why there is none, or the refusal it is.
fn select(
    paths: &Paths,
    entries: &[HistoryEntry],
    given: Option<&str>,
) -> crate::error::Result<Selected> {
    let Some(v) = given else {
        return Ok(newest_in_effect(entries)
            .map_or(Selected::Nothing(Said::NothingInEffect), Selected::At));
    };
    if v.eq_ignore_ascii_case("latest") {
        return Err(latest_refused());
    }
    let Some(run) = DoctorRunId::parse(v) else {
        return Err(not_a_run(entries, v));
    };
    Ok(match resolve(paths, entries, &run)? {
        Some(at) if !entries[at].repairs.is_empty() => Selected::At(at),
        _ => Selected::Nothing(Said::ChangedNothing(run)),
    })
}

/// `latest` names the newest report, which is usually a diagnose (M4-D46).
fn latest_refused() -> Error {
    Error::new(
        ErrorCode::Usage,
        "`latest` names the newest report, which may be a diagnose; `doctor undo` takes a run",
        Suggestion::command(["omaddons", "doctor", "undo"]),
    )
    .user_verb()
}

/// `v`, which is no doctor run id: a repair's transaction names its run in the fix;
/// anything else is told what the argument is.
fn not_a_run(entries: &[HistoryEntry], v: &str) -> Error {
    let run_of = v.parse::<TxId>().ok().and_then(|tx| {
        entries
            .iter()
            // ubs:ignore — comparing transaction ids, not secrets
            .find(|e| e.tx == tx && is_repair_family(e))
            .and_then(|e| e.doctor_run_id.clone())
    });
    match run_of {
        Some(run) => Error::new(
            ErrorCode::Usage,
            format!("{v} is a transaction; `doctor undo` takes the doctor run that made it"),
            Suggestion::command(["omaddons", "doctor", "undo", &run]),
        )
        .user_verb(),
        None => Error::new(
            ErrorCode::Usage,
            format!("{v} is not a doctor run id"),
            Suggestion::command(["omaddons", "help", "doctor", "undo"]),
        ),
    }
}

/// The newest repair (`kind: repair`, committed, with operations) still in effect. Undo
/// lines are skipped, so a second bare `doctor undo` finds nothing; diagnoses write no
/// history, so they never change the answer.
fn newest_in_effect(entries: &[HistoryEntry]) -> Option<usize> {
    (0..entries.len()).rev().find(|&at| {
        let e = &entries[at];
        e.kind == TxKind::Repair
            && is_repair_family(e)
            && e.result == TxResult::Committed
            && !e.repairs.is_empty()
            && undo_state(entries, at) == UndoState::InEffect
    })
}

/// The history line `run` committed: a fix run's repair line, or an undo run's undo line
/// (whose undo redoes the repair). History is the authority on what committed, so a run
/// whose report is missing (a fix killed after `done`, rolled forward by recovery) still
/// resolves. `None` for a run that committed nothing (its directory is there, its line is
/// not); `not_found` when there is neither.
fn resolve(
    paths: &Paths,
    entries: &[HistoryEntry],
    run: &DoctorRunId,
) -> crate::error::Result<Option<usize>> {
    let committed = entries
        .iter()
        .rposition(|e| e.doctor_run_id.as_deref() == Some(run.as_str()) && is_repair_family(e));
    if committed.is_some() {
        return Ok(committed);
    }
    if paths.doctor_run(run).is_dir() {
        return Ok(None);
    }
    Err(Error::new(
        ErrorCode::NotFound,
        format!("there is no doctor run {}", run.as_str()),
        Suggestion::command(["omaddons", "history"]),
    ))
}

/// The interrupted changes the run that made `entry` completed (its report's `delegated[]`
/// rolled forward), which `doctor undo` does not revert. Read for advice only: a report
/// that is missing, too large or unreadable lists none.
fn delegated_forward(paths: &Paths, entry: &HistoryEntry) -> Vec<TxId> {
    entry
        .doctor_run_id
        .as_deref()
        .and_then(DoctorRunId::parse)
        .map_or_else(Vec::new, |run| delegated_forward_of(paths, &run))
}

/// [`delegated_forward`] of the run `run`, which may have committed no repair (a run that
/// only delegated, M4-D37).
fn delegated_forward_of(paths: &Paths, run: &DoctorRunId) -> Vec<TxId> {
    #[derive(Deserialize)]
    struct Delegated {
        tx: TxId,
        outcome: String,
    }
    #[derive(Deserialize)]
    struct Report {
        #[serde(default)]
        delegated: Option<Vec<Delegated>>,
    }
    let mut bytes = Vec::new();
    let read = fs::File::open(paths.doctor_run(run).join(REPORT_FILE)).and_then(|file| {
        file.take(API_RESPONSE_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
    });
    if read.is_err() || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > API_RESPONSE_MAX_BYTES {
        return Vec::new();
    }
    serde_json::from_slice::<Report>(&bytes)
        .ok()
        .and_then(|report| report.delegated)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.outcome == "rolled_forward")
        .map(|d| d.tx)
        .collect()
}

/// A report of nothing done: no run, nothing written.
fn empty_report(started_at: jiff::Timestamp) -> UndoReport {
    UndoReport {
        doctor_contract_version: DOCTOR_CONTRACT_VERSION,
        run_id: None,
        run_dir: None,
        started_at,
        finished_at: crate::clock::now(),
        reverted_run: None,
        tx: None,
        undo_tx: None,
        state: RunState::DoneOk,
        actions_taken: 0,
        already_undone: false,
        actions_planned: None,
    }
}

fn nothing(paths: &Paths, started_at: jiff::Timestamp, said: Said) -> DoctorUndoRun {
    let mut report = empty_report(started_at);
    // A run that committed no repair may still have completed interrupted changes (a
    // fix that only delegated): their own undo is named.
    let delegated = match &said {
        Said::ChangedNothing(run) => {
            report.reverted_run = Some(run.as_str().to_owned());
            delegated_forward_of(paths, run)
        }
        _ => Vec::new(),
    };
    DoctorUndoRun {
        report,
        exit: Exit::Healthy,
        error: None,
        warnings: Vec::new(),
        commands: undo_hints(&delegated),
        said,
        ops: Vec::new(),
        delegated,
    }
}

/// `omaddons undo <tx>` for each interrupted change a run completed (M4-D37).
fn undo_hints(delegated: &[TxId]) -> Vec<CommandHint> {
    delegated
        .iter()
        .map(|tx| {
            CommandHint::new(
                Action::Undo,
                ["omaddons", "undo", &tx.to_string()].map(str::to_owned),
            )
        })
        .collect()
}

/// `entry`'s repair was undone by `by` (the doctor contract's undo idempotence): exit 0,
/// nothing done. `recorded` is a run a lost race made before it found that.
fn already_undone(
    started_at: jiff::Timestamp,
    entry: &HistoryEntry,
    by: TxId,
    recorded: Option<UndoReport>,
    delegated: Vec<TxId>,
) -> DoctorUndoRun {
    let mut report = empty_report(started_at);
    report.reverted_run = entry.doctor_run_id.clone();
    report.tx = Some(entry.tx);
    report.undo_tx = Some(by);
    report.already_undone = true;
    if let Some(recorded) = recorded {
        report.run_id = recorded.run_id;
        report.run_dir = recorded.run_dir;
    }
    DoctorUndoRun {
        report,
        exit: Exit::Healthy,
        error: None,
        warnings: Vec::new(),
        commands: undo_hints(&delegated),
        said: Said::AlreadyUndone,
        ops: Vec::new(),
        delegated,
    }
}

/// An undo that ended in `error`: with the report its run recorded once its directory
/// existed (`DONE_FAILED` 3, `REFUSING` 4, `LOCK_LOST` 5), else the error alone, before
/// anything started (E3's rule).
fn stopped(
    error: Error,
    recorded: Recorded,
    delegated: Vec<TxId>,
) -> Result<DoctorUndoRun, DoctorFailure> {
    let report = match recorded {
        Recorded::Report(report) => *report,
        Recorded::CannotCreate => return Err(DoctorFailure::cannot_create(error)),
        Recorded::Nothing => return Err(DoctorFailure::from(error)),
    };
    let exit = match report.state {
        RunState::DoneFailed => Exit::FixFailedRolledBack,
        RunState::LockLost => Exit::ConcurrencyLost,
        _ => match exit_for(error.code) {
            Exit::Healthy => Exit::RefusedUnsafe,
            exit => exit,
        },
    };
    Ok(DoctorUndoRun {
        report,
        exit,
        error: Some(error),
        warnings: Vec::new(),
        commands: undo_hints(&delegated),
        said: Said::Stopped,
        ops: Vec::new(),
        delegated,
    })
}

/// The word a human line names an operation by.
const fn op_word(op: RepairOp) -> &'static str {
    match op {
        RepairOp::WriteFile => "write",
        RepairOp::Chmod => "chmod",
        RepairOp::RenameNoReplace => "rename",
    }
}

fn planned_op(action: &ActionPlanned) -> (&'static str, String, Option<String>) {
    match &action.step.op {
        Primitive::WriteFile { path, .. } => ("write", path.display().to_string(), None),
        Primitive::Chmod { path, .. } => ("chmod", path.display().to_string(), None),
        Primitive::RenameNoReplace { from, to, .. } => (
            "rename",
            from.display().to_string(),
            Some(to.display().to_string()),
        ),
    }
}

/// `1 write, 1 chmod, 2 renames`.
fn counts(ops: &[(&'static str, String, Option<String>)]) -> String {
    let mut parts = Vec::new();
    for word in ["write", "chmod", "rename"] {
        // ubs:ignore — comparing operation names, not secrets
        let n = ops.iter().filter(|(op, ..)| *op == word).count();
        if n > 0 {
            parts.push(crate::output::count(n, word));
        }
    }
    parts.join(", ")
}

impl DoctorOutcome for DoctorUndoRun {
    type Report = UndoReport;

    fn command(&self) -> &'static str {
        "doctor undo"
    }

    fn report(&self) -> &UndoReport {
        &self.report
    }

    fn exit(&self) -> Exit {
        self.exit
    }

    fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let r = &self.report;
        let tx = r.tx.as_ref().map(short_tx).unwrap_or_default();
        let from = r
            .reverted_run
            .as_deref()
            .map(|run| format!(" from run {run}"))
            .unwrap_or_default();
        match &self.said {
            Said::NothingInEffect => {
                writeln!(
                    w,
                    "nothing to undo: no fix run has a committed repair still in effect"
                )?;
            }
            Said::ChangedNothing(run) => {
                writeln!(w, "run {} changed nothing; nothing to undo", run.as_str())?;
            }
            Said::AlreadyUndone => {
                let by = r.undo_tx.map(|tx| tx.to_string()).unwrap_or_default();
                let run = r.reverted_run.as_deref().unwrap_or("its run");
                writeln!(w, "run {run} was already undone by {by}; nothing to do")?;
            }
            Said::Undone => {
                writeln!(w, "undid repair {tx}{from} ({})", counts(&self.ops))?;
                let mut last = Vec::new();
                if let Some(run) = &r.run_id {
                    last.push(format!("run {}", run.as_str()));
                }
                if let Some(undo) = r.undo_tx {
                    last.push(format!("tx {undo}"));
                    last.push(format!("redo: omaddons undo {undo}"));
                }
                writeln!(w, "{}", last.join(" · "))?;
            }
            Said::Planned => {
                writeln!(w, "would undo repair {tx}{from}: {}", counts(&self.ops))?;
                for (op, path, to) in self.ops.iter().take(LINES_SHOWN) {
                    match to {
                        Some(to) => writeln!(w, "  {op} {path} → {to}")?,
                        None => writeln!(w, "  {op} {path}")?,
                    }
                }
                if self.ops.len() > LINES_SHOWN {
                    writeln!(w, "  and {} more", self.ops.len() - LINES_SHOWN)?;
                }
                if let Some(full) = r.tx {
                    writeln!(
                        w,
                        "bind it: omaddons undo {full} --dry-run, then apply its plan"
                    )?;
                }
            }
            Said::Stopped => {
                let why = self.error.as_ref().map_or("", |e| e.message.as_str());
                writeln!(w, "could not undo repair {tx}{from}: {why}")?;
                if let Some(run) = &r.run_id {
                    let state = serde_json::to_value(r.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default();
                    writeln!(w, "run {} · {state}", run.as_str())?;
                }
            }
        }
        for tx in &self.delegated {
            writeln!(
                w,
                "this run also completed tx {tx} (an interrupted change); doctor undo does not revert it: omaddons undo {tx}"
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::UsageFix;
    use crate::model::TargetKey;

    const RUN_1: &str = "2026-10-30T18-03-40Z__01JB8Q2VC00000000000000001";
    const RUN_2: &str = "2026-10-30T18-04-40Z__01JB8Q2VC00000000000000002";

    /// A repair-family line: `kind` `repair` or `undo`, made by doctor run `run`, one chmod.
    fn line(tx: u8, kind: &str, reverts: Option<u8>, run: &str) -> HistoryEntry {
        let id = |n: u8| format!("01JB8Q2VA000000000000000{n:02}");
        serde_json::from_value(json!({
            "schema_version": "1.0",
            "tx": id(tx),
            "kind": kind,
            "at": "2026-10-30T18:03:40Z",
            "target": null,
            "targets": [],
            "actor": "agent",
            "argv": ["omaddons", "doctor", "fix"],
            "ops": [],
            "result": "committed",
            "reverts": reverts.map(id),
            "run": "01JB8Q2VB00000000000000000",
            "doctor_run_id": run,
            "repairs": [{
                "seq": 1, "fix": 1, "fm": "fm-secrets-credential-file-mode", "op": "chmod",
                "path": "/h/config/omaddons/credentials/github", "mode": "0600",
                "pre": {"kind": "file", "identity": {"ino": "7"}, "mode": "0644"},
                "post": {"kind": "file", "identity": {"ino": "7"}, "mode": "0600"},
            }],
        }))
        .unwrap()
    }

    fn folder_line(tx: u8) -> HistoryEntry {
        HistoryEntry {
            kind: TxKind::Install,
            target: Some(TargetKey::new()),
            doctor_run_id: None,
            repairs: Vec::new(),
            ..line(tx, "repair", None, RUN_1)
        }
    }

    #[test]
    fn doctor_undo_selects_the_newest_repair_in_effect() {
        let cases: Vec<(&str, Vec<HistoryEntry>, Option<usize>)> = vec![
            ("one repair", vec![line(1, "repair", None, RUN_1)], Some(0)),
            (
                "the newest of two",
                vec![
                    line(1, "repair", None, RUN_1),
                    line(2, "repair", None, RUN_2),
                ],
                Some(1),
            ),
            (
                "the newest undone: the older",
                vec![
                    line(1, "repair", None, RUN_1),
                    line(2, "repair", None, RUN_2),
                    line(3, "undo", Some(2), RUN_2),
                ],
                Some(0),
            ),
            (
                "the newest undone and redone: the newest again",
                vec![
                    line(1, "repair", None, RUN_1),
                    line(2, "repair", None, RUN_2),
                    line(3, "undo", Some(2), RUN_2),
                    line(4, "undo", Some(3), RUN_2),
                ],
                Some(1),
            ),
            (
                "only undo lines",
                vec![line(3, "undo", Some(2), RUN_2)],
                None,
            ),
            (
                "a folder line after a repair",
                vec![line(1, "repair", None, RUN_1), folder_line(2)],
                Some(0),
            ),
        ];
        for (case, entries, expected) in cases {
            assert_eq!(newest_in_effect(&entries), expected, "{case}");
        }
    }

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        (dir, paths)
    }

    #[test]
    fn doctor_undo_argument_cases() {
        let (_dir, paths) = paths();
        let entries = vec![line(1, "repair", None, RUN_1), folder_line(2)];
        let repair_tx = entries[0].tx.to_string();
        let folder_tx = entries[1].tx.to_string();
        let help = json!(["omaddons", "help", "doctor", "undo"]);
        // (case, argument, the selection or (code, fix argv, the user's verb kept))
        type Expected = std::result::Result<Selected, (ErrorCode, serde_json::Value, bool)>;
        let cases: Vec<(&str, Option<&str>, Expected)> = vec![
            ("none", None, Ok(Selected::At(0))),
            ("run_id", Some(RUN_1), Ok(Selected::At(0))),
            (
                "latest",
                Some("latest"),
                Err((
                    ErrorCode::Usage,
                    json!(["omaddons", "doctor", "undo"]),
                    true,
                )),
            ),
            (
                "LATEST",
                Some("LATEST"),
                Err((
                    ErrorCode::Usage,
                    json!(["omaddons", "doctor", "undo"]),
                    true,
                )),
            ),
            (
                "a_repair_tx",
                Some(&repair_tx),
                Err((
                    ErrorCode::Usage,
                    json!(["omaddons", "doctor", "undo", RUN_1]),
                    true,
                )),
            ),
            (
                "a_folder_tx",
                Some(&folder_tx),
                Err((ErrorCode::Usage, help.clone(), false)),
            ),
            (
                "garbage",
                Some("garbage"),
                Err((ErrorCode::Usage, help.clone(), false)),
            ),
            (
                "a_ulid_prefix",
                Some("01JB8Q2V"),
                Err((ErrorCode::Usage, help.clone(), false)),
            ),
            (
                "an_unknown_run",
                Some(RUN_2),
                Err((ErrorCode::NotFound, json!(["omaddons", "history"]), false)),
            ),
        ];
        for (case, given, expected) in cases {
            let got = select(&paths, &entries, given).map_err(|e| {
                (
                    e.code,
                    json!(e.fix.argv()),
                    e.usage_fix == UsageFix::UserVerb,
                )
            });
            assert_eq!(got, expected, "{case}: {given:?}");
        }
    }

    #[test]
    fn doctor_undo_resolves_a_run_without_its_report() {
        let (_dir, paths) = paths();
        let run = DoctorRunId::parse(RUN_1).unwrap();
        let entries = vec![line(1, "repair", None, RUN_1)];
        // History names the run; no directory, no report: it resolves.
        assert_eq!(resolve(&paths, &entries, &run).unwrap(), Some(0));
        // A run directory that committed nothing (a diagnose, a rolled-back fix).
        let other = DoctorRunId::parse(RUN_2).unwrap();
        fs::create_dir_all(paths.doctor_run(&other)).unwrap();
        assert_eq!(resolve(&paths, &entries, &other).unwrap(), None);
        assert_eq!(
            select(&paths, &entries, Some(RUN_2)).unwrap(),
            Selected::Nothing(Said::ChangedNothing(other))
        );
        // Neither.
        let err = resolve(&paths, &[], &run).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    }

    fn report(state: RunState) -> Recorded {
        let mut report = empty_report(crate::clock::now());
        report.state = state;
        report.run_id = DoctorRunId::parse(RUN_2);
        Recorded::Report(Box::new(report))
    }

    #[test]
    fn doctor_undo_maps_its_outcomes_to_the_doctor_dictionary() {
        let error =
            |code: ErrorCode| Error::new(code, "x", Suggestion::command(["omaddons", "history"]));
        let cases: Vec<(&str, ErrorCode, Recorded, u8)> = vec![
            (
                "undo_conflict",
                ErrorCode::UndoConflict,
                Recorded::Nothing,
                4,
            ),
            ("game_running", ErrorCode::GameRunning, Recorded::Nothing, 4),
            ("locked", ErrorCode::Locked, report(RunState::LockLost), 5),
            ("usage", ErrorCode::Usage, Recorded::Nothing, 64),
            ("not_found", ErrorCode::NotFound, Recorded::Nothing, 66),
            (
                "its run directory",
                ErrorCode::Io,
                Recorded::CannotCreate,
                73,
            ),
            (
                "state_corrupt",
                ErrorCode::StateCorrupt,
                Recorded::Nothing,
                74,
            ),
            (
                "rolled back",
                ErrorCode::Io,
                report(RunState::DoneFailed),
                3,
            ),
            (
                "refused under the lock",
                ErrorCode::UndoConflict,
                report(RunState::Refusing),
                4,
            ),
        ];
        for (case, code, recorded, want) in cases {
            let exit = match stopped(error(code), recorded, Vec::new()) {
                Ok(run) => {
                    assert!(run.error.is_some(), "{case}: the error stays");
                    run.exit
                }
                Err(failure) => failure.exit,
            };
            assert_eq!(exit.code(), want, "{case}");
        }
    }
}
