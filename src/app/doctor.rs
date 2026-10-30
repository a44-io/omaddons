//! `omaddons doctor` and `doctor diagnose` (E3, §6.11): what is broken, with evidence and
//! the exact fix. Every refusal comes before the run starts and creates nothing; a run
//! records itself in `<state>/doctor/runs/<run>/` and changes nothing else.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::cli::{Context, fix_argv, fix_argv_with_value};
use crate::doctor::budget::{self, Budget, parse_budget};
use crate::doctor::exit::Exit;
use crate::doctor::finding::{Finding, Severity};
use crate::doctor::plan::{Asked, plan_diagnose};
use crate::doctor::registry::{FAILURE_MODES, Layer, ids};
use crate::doctor::report::{self, DiagnoseReport, FullReport, RunState};
use crate::doctor::runtime::{DoctorRunId, Latest, REPORT_FILE, Runtime};
use crate::doctor::selection::Selection;
use crate::doctor::view::{DoctorView, ViewOptions};
use crate::error::{CommandHint, Error, ErrorCode, Suggestion, Warning};
use crate::fsx::ScopeTable;
use crate::limits::{API_RESPONSE_MAX_BYTES, DOCTOR_SCAN_MAX_ENTRIES};
use crate::output::{DoctorEnvelope, DoctorFailure, DoctorOutcome, Meta, RunKind, Style};
use crate::state::paths::Paths;

/// `doctor`'s flags: bare `doctor` diagnoses, with `diagnose`'s flags.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DoctorArgs {
    #[command(subcommand)]
    pub action: Option<DoctorAction>,
    #[command(flatten)]
    pub diagnose: DiagnoseArgs,
}

/// The doctor's verbs.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum DoctorAction {
    /// Find what is broken, with evidence and the exact fix (changes nothing outside
    /// its run record)
    Diagnose(DiagnoseArgs),
}

impl DoctorAction {
    /// Its canonical path.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Diagnose(_) => "doctor diagnose",
        }
    }
}

/// `doctor diagnose`'s flags (and bare `doctor`'s).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DiagnoseArgs {
    /// Only these checks: failure-mode ids or subsystem names, comma-separated
    #[arg(long, value_name = "IDS")]
    pub only: Vec<String>,
    /// Not these checks (wins over --only)
    #[arg(long, value_name = "IDS")]
    pub skip: Vec<String>,
    /// Report findings of this severity and more severe ones only
    #[arg(long, value_parser = ["P0", "P1", "P2", "P3"])]
    pub severity: Option<String>,
    /// Only the fast checks (the cached and quick tiers)
    #[arg(long, conflicts_with = "include_deep")]
    pub quick: bool,
    /// Also the deep checks
    #[arg(long)]
    pub include_deep: bool,
    /// Also the checks that need the network
    #[arg(long)]
    pub online: bool,
    /// The whole run's time, shared by the checks by their cost: 500ms or 2s
    #[arg(long, value_name = "DURATION", value_parser = parse_budget)]
    pub budget: Option<Duration>,
    /// Compare with an earlier run: its id, or latest
    #[arg(long, value_name = "RUN")]
    pub since: Option<String>,
}

/// The doctor's long help: its exit dictionary, which every doctor command exits by.
pub fn doctor_help() -> String {
    let mut out = String::from(
        "Examples:\n  omaddons doctor                       every check but the deep ones\n  omaddons doctor --quick               the fast checks only\n  omaddons doctor --only fm-userland_state-target-vanished\n  omaddons doctor --since latest        what changed since the last run\n\nThe doctor exits with its own codes:\n",
    );
    for exit in Exit::ALL {
        out.push_str(&format!(
            "  {:<3} {:<23} {}\n",
            exit.code(),
            exit.name(),
            exit.meaning()
        ));
    }
    out.truncate(out.trim_end().len());
    out
}

/// A diagnose's result: its report, and the doctor exit it ends with.
#[derive(Debug)]
pub struct DiagnoseOutcome {
    report: DiagnoseReport,
    exit: Exit,
    /// The failure of a run that still has its report (its record could not be written).
    error: Option<Error>,
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
    /// `-v` debug notes for the dispatch arm to log.
    pub notes: Vec<String>,
    /// How many clients the run looked at, when it looked for them.
    clients: Option<usize>,
    checks_run: u32,
    verbose: bool,
}

impl DoctorOutcome for DiagnoseOutcome {
    type Report = DiagnoseReport;

    fn command(&self) -> &'static str {
        "doctor diagnose"
    }

    fn report(&self) -> &DiagnoseReport {
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

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let DiagnoseReport::Full(full) = &self.report;
        let mut head = vec![style.bold("omaddons doctor")];
        if let Some(clients) = self.clients {
            head.push(crate::output::count(clients, "client"));
        }
        head.push(crate::output::count(self.checks_run as usize, "check"));
        head.push(findings_phrase(&full.findings));
        let mut summary = head.join(" · ");
        if !full.skipped.is_empty() {
            summary.push_str(&format!(
                " ({} skipped)",
                crate::output::count(full.skipped.len(), "check")
            ));
        }
        writeln!(w, "{summary}")?;
        for finding in &full.findings {
            writeln!(
                w,
                "  {:?} {}  {}",
                finding.severity, finding.id, finding.label
            )?;
            writeln!(w, "     {}", finding.title)?;
            writeln!(w, "     → {}", finding.remediation.command)?;
        }
        if self.verbose {
            for skipped in &full.skipped {
                let reason = serde_json::to_value(skipped.reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let target = skipped
                    .target
                    .as_ref()
                    .map(|t| format!(" on {}", t.id))
                    .unwrap_or_default();
                writeln!(w, "  skipped {}{target}: {reason}", skipped.id)?;
            }
        }
        if let Some(since) = &full.since {
            writeln!(
                w,
                "since {}: {} new · {} resolved · {} unchanged",
                since.run.as_str(),
                since.new_instances.len(),
                since.resolved_instances.len(),
                since.unchanged
            )?;
            if self.verbose {
                for key in &since.new_instances {
                    writeln!(w, "  new {key}")?;
                }
                for key in &since.resolved_instances {
                    writeln!(w, "  resolved {key}")?;
                }
            }
        }
        writeln!(w, "run {}", full.run_id.as_str())
    }
}

/// `no findings`, or `2 findings (1 P0, 1 P3)`.
fn findings_phrase(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "no findings".to_owned();
    }
    let by: Vec<String> = [Severity::P0, Severity::P1, Severity::P2, Severity::P3]
        .into_iter()
        .filter_map(|severity| {
            let n = findings.iter().filter(|f| f.severity == severity).count();
            (n > 0).then(|| format!("{n} {severity:?}"))
        })
        .collect();
    format!(
        "{} ({})",
        crate::output::count(findings.len(), "finding"),
        by.join(", ")
    )
}

/// `doctor` and its verbs.
pub fn run(
    ctx: &Context,
    args: &DoctorArgs,
) -> std::result::Result<DiagnoseOutcome, DoctorFailure> {
    match &args.action {
        None => run_diagnose(ctx, &args.diagnose),
        Some(DoctorAction::Diagnose(diagnose)) => run_diagnose(ctx, diagnose),
    }
}

/// `doctor diagnose` (and bare `doctor`): refuse what cannot run before anything starts,
/// then run the selected checks in a run of their own and record it.
pub fn run_diagnose(
    ctx: &Context,
    args: &DiagnoseArgs,
) -> std::result::Result<DiagnoseOutcome, DoctorFailure> {
    if args.online && ctx.offline {
        return Err(DoctorFailure::from(
            Error::new(
                ErrorCode::Usage,
                "--online asks for the network, which this run is offline from",
                Suggestion::command(without_switch(&ctx.argv, "--online")),
            )
            .user_verb(),
        ));
    }
    let selection = Selection::parse(ctx.target.as_deref(), &args.only, &args.skip, &ctx.argv)?;
    let asked = Asked {
        severity: args.severity.as_deref().and_then(severity_of),
        quick: args.quick,
        include_deep: args.include_deep,
        online: args.online,
        budget: args.budget,
    };
    let plan = plan_diagnose(FAILURE_MODES, &selection, &asked, &ctx.argv)?;
    let paths = Paths::from_env()?;
    let view = DoctorView::load_at(paths.clone(), ViewOptions::default());
    // -t resolves before the run starts: a client that is not there creates nothing.
    selection.scope(&view)?;
    let since_run = args
        .since
        .as_deref()
        .map(|given| read_since(&paths, given, &ctx.argv))
        .transpose()?;

    let scopes = ScopeTable::new(&paths, &[], None)?;
    let runtime = Runtime::start(&paths, &scopes, crate::clock::now())?;
    let mut diagnosis =
        runtime.diagnose(&plan.rows, &view, &selection, &plan.tiers, plan.budgets)?;
    diagnosis
        .skipped
        .extend(plan.online_skipped.iter().cloned());
    diagnosis
        .skipped
        .sort_by(|a, b| (a.id, &a.target).cmp(&(b.id, &b.target)));
    if let Some(max) = plan.max_severity {
        // The runtime's own findings are never hidden: incomplete coverage must not
        // read as healthy.
        diagnosis.findings.retain(|f| {
            f.severity <= max || f.id == ids::CHECK_INCOMPLETE || f.id == ids::DETECTOR_CRASHED
        });
    }
    let since = since_run.map(|(run, old)| report::since(run, &old, &diagnosis.findings));
    let checks_run = diagnosis.checks_run;
    let mut full = FullReport::new(
        runtime.id().clone(),
        runtime.dir().display().to_string(),
        diagnosis,
        since,
    );
    // `doctor explain` (s6) gives the explain hints a command that parses; until then
    // neither commands[] nor next_steps names it.
    full.next_steps.clear();
    let exit = match full.state {
        RunState::DoneOk => Exit::Healthy,
        RunState::DoneFindings => Exit::FindingsPresent,
    };
    let report = DiagnoseReport::Full(full);
    let warnings = view.warnings();
    let clients = view
        .loaded_layers()
        .contains(&Layer::Discovery)
        .then(|| view.world().map(|world| world.resolved.targets.len()))
        .flatten();

    let mut outcome = DiagnoseOutcome {
        report,
        exit,
        error: None,
        warnings,
        commands: Vec::new(),
        notes: Vec::new(),
        clients,
        checks_run,
        verbose: ctx.output.style.verbose > 0,
    };
    let record = record_of(ctx, &outcome)?;
    match runtime.finish(&record) {
        Ok(finished) => {
            if finished.latest == Latest::NotALink {
                outcome.notes.push(format!(
                    "{} is not a symlink, so it was left alone",
                    paths.doctor_latest().display()
                ));
            }
        }
        Err(failure) => {
            outcome.exit = failure.exit;
            outcome.error = Some(failure.error);
        }
    }
    Ok(outcome)
}

/// The run's `report.json`: the envelope's record form, the run's warnings first, then
/// the outcome's not among them, as the envelope reports them.
fn record_of(ctx: &Context, outcome: &DiagnoseOutcome) -> crate::error::Result<Vec<u8>> {
    let (mut warnings, _) = ctx.notes.snapshot();
    let found = warnings.len();
    for warning in &outcome.warnings {
        if !warnings[..found].contains(warning) {
            warnings.push(warning.clone());
        }
    }
    let meta = Meta {
        elapsed_ms: 0,
        offline: ctx.offline,
    };
    let envelope = DoctorEnvelope::report(
        outcome.command(),
        &outcome.report,
        outcome.exit,
        None,
        &warnings,
        &outcome.commands,
        meta,
    )
    .record(RunKind::Diagnose);
    serde_json::to_vec(&envelope)
        .map_err(|e| Error::internal(format!("the doctor's record could not be serialized: {e}")))
}

fn severity_of(value: &str) -> Option<Severity> {
    match value {
        "P0" => Some(Severity::P0),
        "P1" => Some(Severity::P1),
        "P2" => Some(Severity::P2),
        "P3" => Some(Severity::P3),
        _ => None,
    }
}

/// `argv` without the bare flag `switch`.
fn without_switch(argv: &[String], switch: &str) -> Vec<String> {
    let kept: Vec<String> = argv.iter().filter(|arg| *arg != switch).cloned().collect();
    fix_argv(&kept)
}

/// `argv` without `--since` and its value.
fn without_since(argv: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = argv.iter().enumerate();
    while let Some((at, arg)) = args.next() {
        if at > 0 && arg == "--since" {
            args.next();
            continue;
        }
        if at > 0 && arg.starts_with("--since=") {
            continue;
        }
        kept.push(arg.clone());
    }
    fix_argv(&kept)
}

// ---------------------------------------------------------------------------------------
// `--since`.

/// The parts of an earlier `report.json` `--since` compares: its version and its findings'
/// instance keys; every other key is ignored.
#[derive(Deserialize)]
struct EarlierReport {
    schema_version: String,
    findings: Vec<EarlierFinding>,
}

#[derive(Deserialize)]
struct EarlierFinding {
    instance: String,
}

/// The run `--since` names (`latest`, or an exact run id) and its findings' instance keys.
/// A run that is not there, or that has no `report.json` (interrupted), is `not_found`
/// with the newest finished runs to choose from.
fn read_since(
    paths: &Paths,
    given: &str,
    argv: &[String],
) -> crate::error::Result<(DoctorRunId, Vec<String>)> {
    let run = if given == "latest" {
        fs::read_link(paths.doctor_latest()).ok().and_then(|link| {
            link.strip_prefix("runs")
                .ok()
                .and_then(Path::to_str)
                .and_then(DoctorRunId::parse)
        })
    } else {
        DoctorRunId::parse(given)
    };
    let report = run
        .as_ref()
        .map(|run| paths.doctor_run(run).join(REPORT_FILE));
    let (Some(run), Some(report)) = (run, report.filter(|path| path.is_file())) else {
        return Err(since_not_found(paths, given, argv));
    };
    let path_text = report.display().to_string();
    let corrupt = |why: String| {
        Error::new(
            ErrorCode::StateCorrupt,
            format!("the report of doctor run {} {why}", run.as_str()),
            Suggestion::command(without_since(argv)),
        )
        .details(serde_json::json!({ "path": path_text }))
    };
    let mut bytes = Vec::new();
    fs::File::open(&report)
        .and_then(|file| {
            file.take(API_RESPONSE_MAX_BYTES + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| corrupt(format!("cannot be read: {e}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > API_RESPONSE_MAX_BYTES {
        return Err(corrupt(format!(
            "is larger than {API_RESPONSE_MAX_BYTES} bytes"
        )));
    }
    let earlier: EarlierReport =
        serde_json::from_slice(&bytes).map_err(|e| corrupt(format!("does not decode: {e}")))?;
    let major = earlier.schema_version.split('.').next().unwrap_or_default();
    if major != "1" {
        return Err(Error::new(
            ErrorCode::StateTooNew,
            format!(
                "the report of doctor run {} is schema {}, newer than this omaddons reads",
                run.as_str(),
                earlier.schema_version
            ),
            Suggestion::text("upgrade omaddons, or compare with a run this version made"),
        )
        .details(serde_json::json!({
            "path": path_text,
            "schema_version": earlier.schema_version,
        })));
    }
    Ok((
        run,
        earlier.findings.into_iter().map(|f| f.instance).collect(),
    ))
}

/// `not_found` for `--since given`: the newest finished run is the fix, the next two the
/// alternatives; with none, the fix drops `--since`.
fn since_not_found(paths: &Paths, given: &str, argv: &[String]) -> Error {
    let newest = finished_runs(paths);
    let why = match DoctorRunId::parse(given) {
        Some(run) if paths.doctor_run(&run).is_dir() => {
            format!("doctor run {given} was interrupted: it has no report")
        }
        _ if given == "latest" => "no doctor run has finished yet".to_owned(),
        _ => format!("no doctor run {given}"),
    };
    let named: Vec<String> = newest
        .iter()
        .take(3)
        .map(|run| run.as_str().to_owned())
        .collect();
    let mut error = match named.first() {
        Some(first) => Error::new(
            ErrorCode::NotFound,
            why,
            Suggestion::command(
                fix_argv_with_value(argv, "--since", first).unwrap_or_else(|| without_since(argv)),
            ),
        ),
        None => Error::new(
            ErrorCode::NotFound,
            why,
            Suggestion::command(without_since(argv)),
        ),
    };
    for other in named.iter().skip(1) {
        if let Some(alternative) = fix_argv_with_value(argv, "--since", other) {
            error = error.alternative(Suggestion::command(alternative));
        }
    }
    error.details(serde_json::json!({
        "path": paths.doctor_runs_dir().display().to_string(),
        "newest": named,
    }))
}

/// The finished runs (those with a report), newest first, within the doctor's scan bound.
fn finished_runs(paths: &Paths) -> Vec<DoctorRunId> {
    let mut budget = Budget::new(Duration::from_secs(1), DOCTOR_SCAN_MAX_ENTRIES);
    let Ok(listing) = budget::list_dir(&paths.doctor_runs_dir(), &mut budget) else {
        return Vec::new();
    };
    let mut runs: Vec<DoctorRunId> = listing
        .entries
        .iter()
        .filter_map(|(name, _)| name.to_str().and_then(DoctorRunId::parse))
        .filter(|run| paths.doctor_run(run).join(REPORT_FILE).is_file())
        .collect();
    runs.sort();
    runs.reverse();
    runs
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const OLD: &str = "2026-10-30T18-00-00Z__01M8S7XQKZ0000000000000AAA";
    const NEW: &str = "2026-10-30T19-00-00Z__01M8S7XQKZ0000000000000BBB";
    const BROKEN: &str = "2026-10-30T20-00-00Z__01M8S7XQKZ0000000000000CCC";

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    /// A state dir with two finished runs (`OLD`, `NEW`), an interrupted one (`BROKEN`, no
    /// report), and `latest` naming `NEW`.
    fn runs() -> (TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        for (run, report) in [
            (
                OLD,
                Some(r#"{"schema_version":"1.0","findings":[{"instance":"fm-a-b@machine"}]}"#),
            ),
            (
                NEW,
                Some(r#"{"schema_version":"1.0","state":"DONE_OK","findings":[]}"#),
            ),
            (BROKEN, None),
        ] {
            let dir = paths.doctor_run(&DoctorRunId::parse(run).unwrap());
            fs::create_dir_all(&dir).unwrap();
            if let Some(report) = report {
                fs::write(dir.join(REPORT_FILE), report).unwrap();
            }
        }
        std::os::unix::fs::symlink(format!("runs/{NEW}"), paths.doctor_latest()).unwrap();
        (dir, paths)
    }

    #[test]
    fn doctor_help_lists_the_doctor_dictionary() {
        let help = doctor_help();
        for exit in Exit::ALL {
            let line = format!("{:<3} {}", exit.code(), exit.name());
            assert!(help.contains(&line), "{line} in\n{help}");
        }
    }

    #[test]
    fn online_with_offline_is_usage_dropping_online() {
        assert_eq!(
            without_switch(
                &argv(&["doctor", "--online", "--offline", "--json"]),
                "--online"
            ),
            argv(&["doctor", "--offline"]),
            "--online dropped, presentation flags too"
        );
    }

    #[test]
    fn since_resolution_cases() {
        let (_dir, paths) = runs();
        let line = |given: &str| argv(&["doctor", "--since", given]);
        let (run, old) = read_since(&paths, OLD, &line(OLD)).unwrap();
        assert_eq!(
            (run.as_str(), old),
            (OLD, vec!["fm-a-b@machine".to_owned()])
        );
        let (run, old) = read_since(&paths, "latest", &line("latest")).unwrap();
        assert_eq!(
            (run.as_str(), old),
            (NEW, Vec::<String>::new()),
            "extra keys ignored"
        );

        for (case, given, phrase) in [
            (
                "unknown",
                "2026-01-01T00-00-00Z__01M8R000000000000000000000",
                "no doctor run",
            ),
            ("interrupted", BROKEN, "was interrupted"),
            ("not a run id", "yesterday", "no doctor run"),
        ] {
            let refused = read_since(&paths, given, &line(given)).unwrap_err();
            assert_eq!(refused.code, ErrorCode::NotFound, "{case}");
            assert!(
                refused.message.contains(phrase),
                "{case}: {}",
                refused.message
            );
            assert_eq!(
                refused.fix.argv(),
                Some(line(NEW).as_slice()),
                "{case}: the newest finished run is the fix"
            );
            let alternatives: Vec<_> = refused
                .alternatives
                .iter()
                .filter_map(|a| a.argv())
                .collect();
            assert_eq!(
                alternatives,
                [line(OLD).as_slice()],
                "{case}: then the next"
            );
        }

        let empty = tempfile::tempdir().unwrap();
        let none = Paths::with_roots(empty.path(), empty.path(), empty.path(), empty.path());
        let refused = read_since(&none, "latest", &line("latest")).unwrap_err();
        assert_eq!(
            (refused.code, refused.fix.argv()),
            (ErrorCode::NotFound, Some(argv(&["doctor"]).as_slice())),
            "no run at all: the fix drops --since"
        );
    }

    #[test]
    fn since_report_read_cases() {
        let (_dir, paths) = runs();
        let report = paths
            .doctor_run(&DoctorRunId::parse(OLD).unwrap())
            .join(REPORT_FILE);
        let line = argv(&["doctor", "--since", OLD]);
        for (case, content, code) in [
            (
                "newer",
                r#"{"schema_version":"2.0","findings":[]}"#.to_owned(),
                ErrorCode::StateTooNew,
            ),
            (
                "undecodable",
                "{not json".to_owned(),
                ErrorCode::StateCorrupt,
            ),
            (
                "oversized",
                " ".repeat(usize::try_from(API_RESPONSE_MAX_BYTES).unwrap() + 1),
                ErrorCode::StateCorrupt,
            ),
        ] {
            fs::write(&report, &content).unwrap();
            let refused = read_since(&paths, OLD, &line).unwrap_err();
            assert_eq!(refused.code, code, "{case}: {}", refused.message);
            assert_eq!(
                refused.details["path"],
                report.display().to_string(),
                "{case}: names the file"
            );
        }
    }
}
