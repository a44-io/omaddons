//! `omaddons doctor` and `doctor diagnose` (E3, §6.11): what is broken, with evidence and
//! the exact fix. Every refusal comes before the run starts and creates nothing; a run
//! records itself in `<state>/doctor/runs/<run>/` and changes nothing else.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::cli::{Context, fix_argv, fix_argv_with_value};
use crate::contract::actions::Action;
use crate::doctor::budget::{self, Budget, parse_budget};
use crate::doctor::delegate::{DelegatedOutcome, JournalDelegate, RecoveryOutcome};
use crate::doctor::exit::Exit;
use crate::doctor::explain::{self, ExplainReport};
use crate::doctor::finding::{Finding, Severity};
use crate::doctor::fix_runtime::{DiffOutcome, DiffRequest, FixOutcome, FixRequest};
use crate::doctor::plan::{Asked, DiagnosePlan, plan_diagnose};
use crate::doctor::registry::{Layer, failure_modes, ids};
use crate::doctor::report::{self, DiagnoseReport, DiffReport, FixReport, FullReport, RunState};
use crate::doctor::runtime::{self, DoctorRunId, Latest, REPORT_FILE, Runtime};
use crate::doctor::selection::{Selection, SkipReason};
use crate::doctor::view::{DoctorView, ViewOptions};
use crate::engine::repair_action::{Attribution, Primitive, Step};
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
    /// One failure mode in full: what it is, why it matters, and its live instances
    /// (changes nothing)
    Explain(ExplainArgs),
    /// Repair the findings a fixer can repair, as one undoable transaction
    #[command(after_long_help = FIX_HELP)]
    Fix(FixArgs),
    /// Show what doctor fix would change, or what changed since a run (changes nothing)
    #[command(after_long_help = DIFF_HELP)]
    Diff(DiffArgs),
    /// Undo a doctor repair: revert a fix run (or the newest one), strictly
    #[command(after_long_help = super::doctor_undo::UNDO_HELP)]
    Undo(super::doctor_undo::DoctorUndoArgs),
    /// Delete old doctor runs and what they kept, never what an unfinished change needs
    #[command(after_long_help = super::doctor_gc::GC_HELP)]
    Gc(super::doctor_gc::GcArgs),
}

const FIX_HELP: &str = "Examples:\n  omaddons doctor fix --dry-run                 what it would repair, and the token that binds it\n  omaddons doctor fix --if-match <token>        exactly that preview, or nothing\n  omaddons doctor fix --only <id> --force --yes a forced repair, by its failure mode";

const DIFF_HELP: &str = "Examples:\n  omaddons doctor diff           what doctor fix would change\n  omaddons doctor diff latest    the findings that changed since the last run";

impl DoctorAction {
    /// Its canonical path.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Diagnose(_) => "doctor diagnose",
            Self::Explain(_) => "doctor explain",
            Self::Fix(_) => "doctor fix",
            Self::Diff(_) => "doctor diff",
            Self::Undo(_) => "doctor undo",
            Self::Gc(_) => "doctor gc",
        }
    }
}

/// `doctor explain`'s argument (`-t` is global).
#[derive(Debug, Clone, clap::Args)]
pub struct ExplainArgs {
    /// A failure-mode id, or one instance's key (<fm-id>@<target key or machine>…)
    #[arg(value_name = "ID")]
    pub id: String,
}

/// What a `doctor` command line asks for.
#[derive(Debug, Clone, Copy)]
pub enum Verb<'a> {
    /// `doctor diagnose`, or bare `doctor`.
    Diagnose(&'a DiagnoseArgs),
    Explain(&'a ExplainArgs),
    Fix(&'a FixArgs),
    Diff(&'a DiffArgs),
    Undo(&'a super::doctor_undo::DoctorUndoArgs),
    Gc(&'a super::doctor_gc::GcArgs),
}

impl DoctorArgs {
    pub fn verb(&self) -> Verb<'_> {
        match &self.action {
            None => Verb::Diagnose(&self.diagnose),
            Some(DoctorAction::Diagnose(diagnose)) => Verb::Diagnose(diagnose),
            Some(DoctorAction::Explain(explain)) => Verb::Explain(explain),
            Some(DoctorAction::Fix(fix)) => Verb::Fix(fix),
            Some(DoctorAction::Diff(diff)) => Verb::Diff(diff),
            Some(DoctorAction::Undo(undo)) => Verb::Undo(undo),
            Some(DoctorAction::Gc(gc)) => Verb::Gc(gc),
        }
    }
}

/// `doctor diagnose`'s flags (and bare `doctor`'s).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DiagnoseArgs {
    #[command(flatten)]
    pub select: SelectionArgs,
    /// Compare with an earlier run: its id, or latest
    #[arg(long, value_name = "RUN")]
    pub since: Option<String>,
}

/// The checks a doctor run makes: `doctor diagnose`'s and `doctor fix`'s.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SelectionArgs {
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
}

/// `doctor fix`'s flags: the selection, and the consent a forced repair needs.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct FixArgs {
    #[command(flatten)]
    pub select: SelectionArgs,
    /// Run the forced repairs (set-asides) the --only list names; needs --yes
    #[arg(long, requires = "yes")]
    pub force: bool,
    /// Consent to a forced repair
    #[arg(long)]
    pub yes: bool,
}

/// `doctor diff`'s argument (`-t` is global).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct DiffArgs {
    /// A run id, or latest: the findings that changed since that run. Without it: what
    /// doctor fix would change
    #[arg(value_name = "RUN")]
    pub run: Option<String>,
}

/// The doctor's long help: its exit dictionary, which every doctor command exits by.
pub fn doctor_help() -> String {
    let mut out = String::from(
        "Examples:\n  omaddons doctor                       every check but the deep ones\n  omaddons doctor --quick               the fast checks only\n  omaddons doctor --only fm-userland_state-target-vanished\n  omaddons doctor --since latest        what changed since the last run\n  omaddons doctor explain fm-userland_state-target-vanished\n  omaddons doctor fix --dry-run         what a repair would change\n  omaddons doctor fix --if-match <token>  that repair, exactly as previewed\n\nThe doctor exits with its own codes:\n",
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
        // The checks only `--online` runs are named apart: every plain run skips them.
        let online = full
            .skipped
            .iter()
            .filter(|s| s.reason == SkipReason::OnlineRequired)
            .count();
        let skipped: Vec<String> = [
            (full.skipped.len() - online, "check"),
            (online, "online check"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{} skipped", crate::output::count(n, what)))
        .collect();
        if !skipped.is_empty() {
            summary.push_str(&format!(" ({})", skipped.join(", ")));
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

/// An explain's result: always exit 0 when it answers.
#[derive(Debug)]
pub struct ExplainOutcome {
    report: ExplainReport,
    warnings: Vec<Warning>,
}

/// The evidence keys a stuck journal's set block says (`set_block`).
const SET_KEYS: [&str; 5] = [
    "attribution",
    "unattributed_dirs",
    "may_hold_only_copy",
    "stage_folders",
    "snapshot_folders",
];

/// What a set-aside of a stuck journal would move (M4-D38), from its finding's evidence:
/// the set and its folder counts, whether a folder may be the only copy, the attribution,
/// and each directory not accounted for. Sizes need a walk the cached tier forbids: the
/// set-aside's dry run shows them.
fn set_block(evidence: &crate::doctor::finding::Evidence) -> Vec<String> {
    let folders = |n: Option<u64>| {
        crate::output::count(
            usize::try_from(n.unwrap_or(0)).unwrap_or(usize::MAX),
            "folder",
        )
    };
    let mut lines = vec![
        format!(
            "the set: the journal file and what its transaction id names, as far as it exists (the in-flight lock, the state snapshot, the stage with {}, the client snapshot with {})",
            folders(evidence.stage_folders),
            folders(evidence.snapshot_folders)
        ),
        format!(
            "may hold the only copy of a folder: {}",
            if evidence.may_hold_only_copy == Some(true) {
                "yes"
            } else {
                "no"
            }
        ),
    ];
    if let Some(attribution) = evidence.attribution {
        lines.push(format!(
            "attribution: {}",
            match attribution {
                Attribution::Complete => "complete",
                Attribution::Partial => "partial",
            }
        ));
    }
    lines.extend(
        evidence
            .unattributed_dirs
            .iter()
            .flatten()
            .map(|dir| format!("not accounted for: {}", dir.display())),
    );
    lines
}

impl DoctorOutcome for ExplainOutcome {
    type Report = ExplainReport;

    fn command(&self) -> &'static str {
        "doctor explain"
    }

    fn report(&self) -> &ExplainReport {
        &self.report
    }

    fn exit(&self) -> Exit {
        Exit::Healthy
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let report = &self.report;
        let fm = &report.failure_mode;
        let tier = serde_json::to_value(fm.tier)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        writeln!(
            w,
            "{} · {:?} · {} · {tier}",
            style.bold(fm.id),
            fm.severity,
            fm.subsystem.as_str()
        )?;
        writeln!(w, "{}", fm.title)?;
        writeln!(w, "  what: {}", fm.description)?;
        writeln!(w, "  why:  {}", fm.why)?;
        let manual = fm
            .manual
            .as_ref()
            .map(|m| format!("{} ({})", m.instruction, m.why));
        let repair = match (&fm.fixer, manual) {
            (Some(fixer), manual) => {
                let fix = if fixer.needs_force {
                    "doctor fix repairs it with --force"
                } else if fixer.enabled {
                    "doctor fix repairs it"
                } else {
                    "doctor fix can repair it, when enabled"
                };
                manual.map_or_else(|| fix.to_owned(), |manual| format!("{fix}; or {manual}"))
            }
            (None, Some(manual)) => format!("no automatic repair; {manual}"),
            (None, None) => "no repair".to_owned(),
        };
        writeln!(w, "  repair: {repair}")?;
        if report.instances.is_empty() {
            writeln!(w, "no instance now")?;
        } else {
            writeln!(
                w,
                "{}:",
                crate::output::count(report.instances.len(), "instance")
            )?;
        }
        for instance in &report.instances {
            writeln!(w, "  {}", instance.label)?;
            // A stuck journal's set (F2) reads as one block, its keys not listed twice.
            let set = instance.evidence.attribution.is_some();
            if let Ok(serde_json::Value::Object(evidence)) =
                serde_json::to_value(&instance.evidence)
            {
                for (key, value) in evidence {
                    if set && SET_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    let value = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned);
                    writeln!(w, "    {key}: {value}")?;
                }
            }
            if set {
                for line in set_block(&instance.evidence) {
                    writeln!(w, "    {line}")?;
                }
            }
            writeln!(w, "    → {}", instance.remediation.command)?;
        }
        for skipped in &report.skipped {
            let reason = serde_json::to_value(skipped.reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let target = skipped
                .target
                .as_ref()
                .map(|t| format!(" on {}", t.id))
                .unwrap_or_default();
            writeln!(w, "not checked{target}: {reason}")?;
        }
        if let Some(cut) = &report.incomplete {
            writeln!(
                w,
                "this list may be partial: the check stopped at its {} limit",
                cut.limit.as_str()
            )?;
        }
        Ok(())
    }
}

/// `doctor explain <ID>`: the failure mode, and its instances found now. Writes nothing.
pub fn run_explain(
    ctx: &Context,
    args: &ExplainArgs,
) -> std::result::Result<ExplainOutcome, DoctorFailure> {
    let asked = explain::resolve(&args.id, &ctx.argv)?;
    let paths = Paths::from_env()?;
    let view = DoctorView::load_at(paths, ViewOptions::default());
    let report = explain::explain(&view, &asked, ctx.target.as_deref())?;
    Ok(ExplainOutcome {
        report,
        warnings: view.warnings(),
    })
}

/// The checks `select` and `-t` choose, refused before anything starts when they cannot
/// run: `--online` while offline, an unknown id, a selection left empty, an over-budget
/// one (E3's refusals, common to `doctor diagnose` and `doctor fix`).
fn checks_of(
    ctx: &Context,
    select: &SelectionArgs,
) -> std::result::Result<(Selection, DiagnosePlan), DoctorFailure> {
    if select.online && ctx.offline {
        return Err(DoctorFailure::from(
            Error::new(
                ErrorCode::Usage,
                "--online asks for the network, which this run is offline from",
                Suggestion::command(without_switch(&ctx.argv, "--online")),
            )
            .user_verb(),
        ));
    }
    let selection = Selection::parse(ctx.target.as_deref(), &select.only, &select.skip, &ctx.argv)?;
    let asked = Asked {
        severity: select.severity.as_deref().and_then(severity_of),
        quick: select.quick,
        include_deep: select.include_deep,
        online: select.online,
        budget: select.budget,
    };
    let plan = plan_diagnose(failure_modes(), &selection, &asked, &ctx.argv)?;
    Ok((selection, plan))
}

/// `doctor diagnose` (and bare `doctor`): refuse what cannot run before anything starts,
/// then run the selected checks in a run of their own and record it.
pub fn run_diagnose(
    ctx: &Context,
    args: &DiagnoseArgs,
) -> std::result::Result<DiagnoseOutcome, DoctorFailure> {
    let (selection, plan) = checks_of(ctx, &args.select)?;
    let paths = Paths::from_env()?;
    let view = DoctorView::load_at(
        paths.clone(),
        ViewOptions {
            online: plan.online,
            ..ViewOptions::default()
        },
    );
    // -t resolves before the run starts: a client that is not there creates nothing.
    selection.scope(&view)?;
    let since_run = args
        .since
        .as_deref()
        .map(|given| read_since(&paths, given, &ctx.argv, SinceArg::Flag))
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
    // One `doctor explain` per failure mode found, as `next_steps` shows them.
    let commands = report::explain_argvs(&diagnosis.findings)
        .into_iter()
        .map(|argv| CommandHint::new(Action::DoctorExplain, argv))
        .collect();
    let full = FullReport::new(
        runtime.id().clone(),
        runtime.dir().display().to_string(),
        diagnosis,
        since,
    );
    let exit = full.state.exit();
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
        commands,
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

// ---------------------------------------------------------------------------------------
// `doctor fix` and `doctor diff`.

/// `--force` with nothing to apply it to: a usage error whose fix is the preview, never
/// the forced run (a fix never supplies consent, M4-D38).
pub(crate) fn force_refused(message: &str, argv: &[String]) -> Error {
    Error::new(
        ErrorCode::Usage,
        message,
        Suggestion::command(crate::cli::force_preview(argv)),
    )
    .details(serde_json::json!({ "flag": "--force" }))
    .user_verb()
}

/// `doctor fix`: refuse a `--force` that names no failure mode before anything starts,
/// then plan, preview or repair as the runtime orders it (s3).
pub fn run_fix(ctx: &Context, args: &FixArgs) -> std::result::Result<FixRun, DoctorFailure> {
    if args.force && args.select.only.is_empty() {
        return Err(DoctorFailure::from(force_refused(
            "--force applies to the failure modes --only names; a forced repair is never part of a default doctor fix",
            &ctx.argv,
        )));
    }
    let (selection, checks) = checks_of(ctx, &args.select)?;
    let paths = Paths::from_env()?;
    let req = FixRequest {
        selection,
        checks,
        force: args.force,
        dry_run: ctx.dry_run,
        if_match: ctx.if_match.clone(),
        argv: ctx.argv.clone(),
        actor: ctx.actor,
        reason: ctx.reason.clone(),
        offline: ctx.offline,
        notes: ctx.notes.clone(),
    };
    let delegate = JournalDelegate {
        paths: &paths,
        actor: ctx.actor,
    };
    let outcome = runtime::run_fix_at(&paths, &req, &delegate)?;
    Ok(FixRun {
        outcome,
        roots: Roots::of(&paths, None),
        verbose: ctx.output.style.verbose > 0,
    })
}

/// `doctor diff`: what a default `doctor fix --dry-run` would change, or, given a run,
/// what changed since it. No run directory, no lock, no write.
pub fn run_diff(ctx: &Context, args: &DiffArgs) -> std::result::Result<DiffRun, DoctorFailure> {
    let (selection, checks) = checks_of(ctx, &SelectionArgs::default())?;
    let paths = Paths::from_env()?;
    let since = args
        .run
        .as_deref()
        .map(|given| read_since(&paths, given, &ctx.argv, SinceArg::Positional))
        .transpose()?;
    let mut outcome = runtime::run_diff_at(
        &paths,
        &DiffRequest {
            selection,
            checks,
            since,
        },
    )?;
    // A comparison offers one `doctor explain` per failure mode with a new instance.
    if let Some(since) = &outcome.report.since {
        let mut seen = std::collections::BTreeSet::new();
        for key in &since.new_instances {
            let fm = key.split('@').next().unwrap_or(key);
            if seen.insert(fm.to_owned()) {
                let mut argv: Vec<String> = ["omaddons", "doctor", "explain", fm]
                    .map(str::to_owned)
                    .into();
                if let Some(target) = &ctx.target {
                    argv.extend(["-t".to_owned(), target.clone()]);
                }
                outcome
                    .commands
                    .push(CommandHint::new(Action::DoctorExplain, argv));
            }
        }
    }
    Ok(DiffRun {
        outcome,
        roots: Roots::of(&paths, Some("<run>")),
        verbose: ctx.output.style.verbose > 0,
    })
}

/// The roots human text shows paths below, each by its name (`<state>/…`).
#[derive(Debug, Clone)]
pub struct Roots(Vec<(String, &'static str)>);

impl Roots {
    /// A run directory first (it is below the state directory), then the state, config
    /// and cache directories. `run` names the placeholder a diff's run directory has.
    fn of(paths: &Paths, run: Option<&str>) -> Self {
        let mut roots = Vec::new();
        if let Some(run) = run {
            roots.push((
                paths.doctor_runs_dir().join(run).display().to_string(),
                "<run>",
            ));
        }
        roots.extend([
            (paths.state_dir().display().to_string(), "<state>"),
            (paths.config_dir().display().to_string(), "<config>"),
            (paths.cache_dir().display().to_string(), "<cache>"),
        ]);
        Self(roots)
    }

    /// `path` below the first root that holds it, named; else as given.
    fn short(&self, path: &str, run_dir: Option<&str>) -> String {
        if let Some(run) = run_dir
            && let Some(rest) = path.strip_prefix(run)
        {
            return format!("<run>{rest}");
        }
        for (root, name) in &self.0 {
            if let Some(rest) = path.strip_prefix(root.as_str())
                && (rest.is_empty() || rest.starts_with('/'))
            {
                return format!("{name}{rest}");
            }
        }
        path.to_owned()
    }
}

/// A `doctor fix` run, as the command renders it.
#[derive(Debug)]
pub struct FixRun {
    pub outcome: FixOutcome,
    roots: Roots,
    verbose: bool,
}

/// A `doctor diff`, as the command renders it.
#[derive(Debug)]
pub struct DiffRun {
    pub outcome: DiffOutcome,
    roots: Roots,
    verbose: bool,
}

/// A set-aside's lines (M4-D38): its header (the transaction, how many paths, their size
/// when planned, the attribution), one line per artifact it moves (`kind  from  → to`),
/// then each directory attribution could not account for. `steps` are its steps, with
/// their sizes in a dry run.
fn suspend_lines(
    s: &crate::doctor::fix::Suspended,
    steps: &[(&Step, Option<u64>)],
    show: &dyn Fn(&str) -> String,
    dry_run: bool,
) -> Vec<String> {
    let attribution = match s.attribution {
        Attribution::Complete => "complete",
        Attribution::Partial => "partial",
    };
    let paths = crate::output::count(steps.len(), "path");
    let mut lines = vec![if dry_run {
        let bytes: u64 = steps.iter().filter_map(|(_, b)| *b).sum();
        format!(
            "would suspend tx {} ({paths}, {}, nothing copied; attribution {attribution}):",
            s.tx,
            super::backup::size(bytes)
        )
    } else {
        format!(
            "suspended tx {} ({paths}, nothing copied; attribution {attribution}):",
            s.tx
        )
    }];
    for (step, _) in steps {
        if let Primitive::RenameNoReplace { from, to, .. } = &step.op {
            lines.push(format!(
                "  {:<15} {}  → {}",
                artifact_word(from),
                show(&from.display().to_string()),
                show(&to.display().to_string())
            ));
        }
    }
    lines.extend(
        s.unattributed
            .iter()
            .map(|dir| format!("  not accounted for: {}", show(&dir.display().to_string()))),
    );
    lines
}

/// What a set-aside artifact is, by where it lies.
fn artifact_word(path: &std::path::Path) -> &'static str {
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let client_side = path.components().any(|c| c.as_os_str() == ".omaddons");
    match (client_side, parent) {
        (true, "stage") => "stage",
        (true, _) => "snapshot",
        (false, "journal") => "journal",
        (false, "inflight") => "in-flight lock",
        (false, _) => "state snapshot",
    }
}

/// One recovery a preview discloses, as a line: its transaction and which way it settles.
fn recovery_line(r: &crate::doctor::report::RecoveryPlanned) -> String {
    let way = match r.direction {
        crate::engine::repair_action::Direction::RollBack => "rolls back",
        crate::engine::repair_action::Direction::RollForward => "rolls forward",
    };
    format!("recovers tx {} ({way})", r.tx)
}

/// One delegated recovery as a line (M4-D37): a roll-forward with the undo that reverts
/// it, a wait, or a failure with its reason. Transactions only, never a path.
fn delegated_line(d: &DelegatedOutcome) -> String {
    let reason = d.reason.as_deref().unwrap_or("unknown");
    match d.outcome {
        RecoveryOutcome::RolledBack => format!("recovered tx {} (rolled back)", d.tx),
        RecoveryOutcome::RolledForward => format!(
            "recovered tx {} (rolled forward) · undo: omaddons undo {}",
            d.tx, d.tx
        ),
        RecoveryOutcome::Waiting => format!("tx {} waits for the game to close", d.tx),
        RecoveryOutcome::Failed => format!("could not recover tx {}: {reason}", d.tx),
    }
}

/// One planned or run primitive as a line: its op, its path and a rename's destination.
fn action_line(step: &serde_json::Value, show: &dyn Fn(&str) -> String) -> String {
    let field = |key: &str| step.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let mut line = format!("{:<17} {}", field("op"), show(field("path")));
    if let Some(to) = step.get("to").and_then(|v| v.as_str()) {
        line.push_str(&format!(" → {}", show(to)));
    }
    if step.get("backup").is_some_and(|v| v.is_string()) {
        line.push_str("  (kept as a backup)");
    }
    line
}

/// The findings a run leaves for a person, by why: a forced repair, a disabled one, or
/// one no fixer makes.
fn left_lines(w: &mut dyn Write, findings: &[&Finding]) -> std::io::Result<()> {
    let mut manual: Vec<&Finding> = Vec::new();
    for f in findings {
        let r = &f.remediation;
        let fixer = r
            .argv
            .as_ref()
            .is_some_and(|argv| argv.get(1..3) == Some(&["doctor".to_owned(), "fix".to_owned()]));
        if r.needs_force {
            writeln!(
                w,
                "needs --force --yes: {} {} → {}",
                f.id, f.label, r.explain_command
            )?;
        } else if fixer && !r.auto_fixable {
            writeln!(w, "disabled: {} {} → {}", f.id, f.label, r.command)?;
        } else if !fixer {
            manual.push(f);
        }
    }
    if let Some(first) = manual.first() {
        writeln!(
            w,
            "manual: {} · {}",
            crate::output::count(manual.len(), "finding"),
            first.remediation.explain_command
        )?;
    }
    Ok(())
}

impl DoctorOutcome for FixRun {
    type Report = FixReport;

    fn command(&self) -> &'static str {
        "doctor fix"
    }

    fn report(&self) -> &FixReport {
        &self.outcome.report
    }

    fn exit(&self) -> Exit {
        self.outcome.exit
    }

    fn error(&self) -> Option<&Error> {
        self.outcome.error.as_ref()
    }

    fn warnings(&self) -> &[Warning] {
        &self.outcome.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.outcome.commands
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let r = &self.outcome.report;
        let run_dir = r.run_dir.clone();
        let show = |path: &str| {
            if self.verbose {
                path.to_owned()
            } else {
                self.roots.short(path, Some(&run_dir))
            }
        };
        let mut last = format!("run {}", r.run_id.as_str());
        if let Some(tx) = r.tx {
            last.push_str(&format!(" · tx {tx}"));
        }
        if let Some(undo) = &r.undo_command {
            last.push_str(&format!(" · undo: {undo}"));
        }
        let stopped = matches!(r.state, RunState::Refusing | RunState::LockLost)
            || (r.state == RunState::DoneFailed
                && r.summary.backups_restored == 0
                && r.summary.actions_failed == 0);
        if stopped {
            writeln!(
                w,
                "{} · nothing was changed",
                style.bold("omaddons doctor fix")
            )?;
            return writeln!(w, "{last}");
        }
        if r.dry_run {
            let planned = r.actions_planned.as_deref().unwrap_or_default();
            let recoveries = r.delegated_planned.as_deref().unwrap_or_default();
            if r.findings.is_empty() && planned.is_empty() && recoveries.is_empty() {
                writeln!(
                    w,
                    "{} · no findings",
                    style.bold("omaddons doctor fix --dry-run")
                )?;
                return writeln!(w, "{last}");
            }
            writeln!(
                w,
                "{} · {} · {} planned ({} B to back up)",
                style.bold("omaddons doctor fix --dry-run"),
                crate::output::count(r.findings.len(), "finding"),
                crate::output::count(planned.len(), "action"),
                r.summary.bytes_to_back_up
            )?;
            let mut fms: Vec<String> = Vec::new();
            let mut shown = std::collections::BTreeSet::new();
            for action in planned {
                let step = serde_json::to_value(action).unwrap_or_default();
                let fm = step["fm"].as_str().unwrap_or_default().to_owned();
                if !fms.contains(&fm) {
                    writeln!(w, "  {fm}")?;
                    fms.push(fm);
                }
                if let Some(s) = self
                    .outcome
                    .suspended
                    .iter()
                    .find(|s| s.fix == action.step.fix)
                {
                    if shown.insert(s.fix) {
                        let steps: Vec<(&Step, Option<u64>)> = planned
                            .iter()
                            .filter(|a| a.step.fix == s.fix)
                            .map(|a| (&a.step, Some(a.bytes)))
                            .collect();
                        for line in suspend_lines(s, &steps, &show, true) {
                            writeln!(w, "    {line}")?;
                        }
                    }
                    continue;
                }
                writeln!(w, "    {}", action_line(&step, &show))?;
            }
            for recovery in recoveries {
                writeln!(w, "  {}", recovery_line(recovery))?;
            }
            let left: Vec<&Finding> = r
                .findings
                .iter()
                .filter(|f| !fms.iter().any(|fm| fm == f.id))
                .collect();
            left_lines(w, &left)?;
            if let Some(token) = &r.precondition {
                writeln!(
                    w,
                    "precondition {token} (pass --if-match to apply exactly this)"
                )?;
            }
            return writeln!(w, "{last}");
        }
        let actions = r.actions.as_deref().unwrap_or_default();
        let delegated = r.delegated.as_deref().unwrap_or_default();
        if r.state == RunState::DoneFailed {
            // What the delegation finished before the repair stands (its lines follow).
            let changed = if delegated.is_empty() {
                "; nothing was changed"
            } else {
                ""
            };
            writeln!(
                w,
                "{} · rolled back: {} restored{changed}",
                style.bold("omaddons doctor fix"),
                crate::output::count(r.summary.backups_restored as usize, "action")
            )?;
        } else if actions.is_empty() && delegated.is_empty() {
            let what = if r.findings.is_empty() {
                "no findings"
            } else {
                "nothing to repair"
            };
            writeln!(w, "{} · {what}", style.bold("omaddons doctor fix"))?;
        } else if !actions.is_empty() {
            let fms: std::collections::BTreeSet<&str> =
                actions.iter().map(|a| a.step.fm.as_ref()).collect();
            writeln!(
                w,
                "repaired {} ({}, {} B backed up)",
                crate::output::count(fms.len(), "failure mode"),
                crate::output::count(r.summary.actions_taken as usize, "action"),
                r.summary.bytes_backed_up
            )?;
            let mut shown = std::collections::BTreeSet::new();
            for action in actions {
                if let Some(s) = self
                    .outcome
                    .suspended
                    .iter()
                    .find(|s| s.fix == action.step.fix)
                {
                    if shown.insert(s.fix) {
                        let steps: Vec<(&Step, Option<u64>)> = actions
                            .iter()
                            .filter(|a| a.step.fix == s.fix)
                            .map(|a| (&a.step, None))
                            .collect();
                        writeln!(w, "  {}", action.step.fm)?;
                        for line in suspend_lines(s, &steps, &show, false) {
                            writeln!(w, "  {line}")?;
                        }
                    }
                    continue;
                }
                let step = serde_json::to_value(&action.step).unwrap_or_default();
                writeln!(w, "  {}  {}", action.step.fm, action_line(&step, &show))?;
            }
        }
        for d in delegated {
            writeln!(w, "{}", delegated_line(d))?;
        }
        let repaired: std::collections::BTreeSet<&str> =
            actions.iter().map(|a| a.step.fm.as_ref()).collect();
        let mut left: Vec<&Finding> = Vec::new();
        for f in &r.findings {
            if repaired.contains(f.id) {
                writeln!(w, "still found: {} {}", f.id, f.label)?;
            } else {
                left.push(f);
            }
        }
        left_lines(w, &left)?;
        writeln!(w, "{last}")
    }
}

impl DoctorOutcome for DiffRun {
    type Report = DiffReport;

    fn command(&self) -> &'static str {
        "doctor diff"
    }

    fn report(&self) -> &DiffReport {
        &self.outcome.report
    }

    fn exit(&self) -> Exit {
        self.outcome.exit
    }

    fn warnings(&self) -> &[Warning] {
        &self.outcome.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.outcome.commands
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let r = &self.outcome.report;
        if let Some(since) = &r.since {
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
            return Ok(());
        }
        let show = |path: &str| {
            if self.verbose {
                path.to_owned()
            } else {
                self.roots.short(path, None)
            }
        };
        let planned = r.actions_planned.as_deref().unwrap_or_default();
        let recoveries = r.delegated_planned.as_deref().unwrap_or_default();
        if planned.is_empty() && recoveries.is_empty() {
            return writeln!(
                w,
                "{} · nothing to repair",
                style.bold("omaddons doctor diff")
            );
        }
        writeln!(
            w,
            "{} · {} planned ({} B to back up)",
            style.bold("omaddons doctor diff"),
            crate::output::count(planned.len(), "action"),
            r.bytes_to_back_up.unwrap_or(0)
        )?;
        let mut fms: Vec<String> = Vec::new();
        for action in planned {
            let step = serde_json::to_value(action).unwrap_or_default();
            let fm = step["fm"].as_str().unwrap_or_default().to_owned();
            if !fms.contains(&fm) {
                writeln!(w, "  {fm}")?;
                fms.push(fm);
            }
            writeln!(w, "    {}", action_line(&step, &show))?;
        }
        for recovery in recoveries {
            writeln!(w, "  {}", recovery_line(recovery))?;
        }
        if let Some(token) = &r.precondition {
            writeln!(
                w,
                "precondition {token} (pass doctor fix --if-match to apply exactly this)"
            )?;
        }
        Ok(())
    }
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
    how: SinceArg,
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
        return Err(since_not_found(paths, given, argv, how));
    };
    let path_text = report.display().to_string();
    let corrupt = |why: String| {
        Error::new(
            ErrorCode::StateCorrupt,
            format!("the report of doctor run {} {why}", run.as_str()),
            Suggestion::command(how.retarget(argv, given, None)),
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

/// How a command line names the run it compares with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SinceArg {
    /// `doctor --since <run>`.
    Flag,
    /// `doctor diff <run>`.
    Positional,
}

impl SinceArg {
    /// `argv` naming `run` where it named `given`, or, for `None`, naming no run.
    fn retarget(self, argv: &[String], given: &str, run: Option<&str>) -> Vec<String> {
        match (self, run) {
            (Self::Flag, Some(run)) => {
                fix_argv_with_value(argv, "--since", run).unwrap_or_else(|| without_since(argv))
            }
            (Self::Flag, None) => without_since(argv),
            (Self::Positional, run) => {
                let verb = argv
                    .iter()
                    .position(|a| a == "diff")
                    .map_or(argv.len(), |at| at + 1);
                let mut kept = argv.to_vec();
                if let Some(at) = argv.iter().skip(verb).position(|a| a == given) {
                    match run {
                        Some(run) => {
                            if let Some(slot) = kept.get_mut(verb + at) {
                                run.clone_into(slot);
                            }
                        }
                        None => {
                            kept.remove(verb + at);
                        }
                    }
                }
                fix_argv(&kept)
            }
        }
    }
}

/// `not_found` for the run `given`: the newest finished run is the fix, the next two the
/// alternatives; with none, the fix names no run.
fn since_not_found(paths: &Paths, given: &str, argv: &[String], how: SinceArg) -> Error {
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
    let fix = how.retarget(argv, given, named.first().map(String::as_str));
    let mut error = Error::new(ErrorCode::NotFound, why, Suggestion::command(fix));
    for other in named.iter().skip(1) {
        error = error.alternative(Suggestion::command(how.retarget(argv, given, Some(other))));
    }
    error.details(serde_json::json!({
        "path": paths.doctor_runs_dir().display().to_string(),
        "newest_ids": named,
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

    /// A set-aside's header, artifact lines and what it could not account for, as a
    /// dry run and a real run show them, for a complete and a partial set.
    #[test]
    fn the_human_suspend_header() {
        use std::borrow::Cow;
        use std::path::PathBuf;

        use crate::doctor::fix::Suspended;
        use crate::engine::repair_action::{Endpoint, Endpoints};
        let tx: crate::model::TxId = "01JB8Q2VA00000000000000009".parse().unwrap();
        let rename = |seq: u32, from: &str, to: &str| Step {
            seq,
            fix: 1,
            fm: Cow::Borrowed(ids::JOURNAL_TOO_NEW),
            op: Primitive::RenameNoReplace {
                from: PathBuf::from(from),
                to: PathBuf::from(to),
                activation: false,
            },
            pre: Endpoints::Pair {
                path: Endpoint::Absent,
                to: Endpoint::Absent,
            },
            post: Endpoints::Pair {
                path: Endpoint::Absent,
                to: Endpoint::Absent,
            },
            guards: Vec::new(),
        };
        let steps = [
            rename(
                1,
                "/c/_retail_/Interface/.omaddons/stage/T",
                "/c/_retail_/Interface/.omaddons/snapshots/R.repair/quarantine/Interface/.omaddons/stage/T",
            ),
            rename(
                2,
                "/s/omaddons/inflight/T.lock",
                "/s/omaddons/doctor/runs/R/quarantine/state/inflight/T.lock",
            ),
            rename(
                3,
                "/s/omaddons/journal/T.jsonl",
                "/s/omaddons/doctor/runs/R/quarantine/state/journal/T.jsonl",
            ),
        ];
        let sized: Vec<(&Step, Option<u64>)> = steps
            .iter()
            .zip([18_400_000, 0, 120])
            .map(|(s, b)| (s, Some(b)))
            .collect();
        let show = |p: &str| p.to_owned();
        let complete = Suspended {
            fix: 1,
            tx,
            attribution: Attribution::Complete,
            unattributed: Vec::new(),
        };
        let lines = suspend_lines(&complete, &sized, &show, true);
        assert_eq!(
            lines[0],
            format!(
                "would suspend tx {tx} (3 paths, 18.4 MB, nothing copied; attribution complete):"
            ),
            "{lines:#?}"
        );
        assert!(
            lines[1].starts_with("  stage ") && lines[1].contains("→"),
            "{lines:#?}"
        );
        assert!(lines[2].starts_with("  in-flight lock "), "{lines:#?}");
        assert!(lines[3].starts_with("  journal "), "{lines:#?}");
        assert_eq!(lines.len(), 4, "{lines:#?}");
        let partial = Suspended {
            attribution: Attribution::Partial,
            unattributed: vec![PathBuf::from("/c/_classic_era_")],
            ..complete
        };
        let committed: Vec<(&Step, Option<u64>)> = steps.iter().map(|s| (s, None)).collect();
        let lines = suspend_lines(&partial, &committed, &show, false);
        assert_eq!(
            lines[0],
            format!("suspended tx {tx} (3 paths, nothing copied; attribution partial):")
        );
        assert_eq!(
            lines.last().unwrap(),
            "  not accounted for: /c/_classic_era_"
        );
    }

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

    /// The doctor verb `words` parses to, or clap's error kind.
    fn parsed(words: &[&str]) -> std::result::Result<Option<DoctorAction>, clap::error::ErrorKind> {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(argv(words)).map_err(|e| e.kind())?;
        match cli.command {
            Some(crate::cli::Command::Doctor(doctor)) => Ok(doctor.action),
            other => panic!("{words:?} parsed to {other:?}"), // ubs:ignore — the test's own failure
        }
    }

    #[test]
    fn fix_args_parse() {
        use clap::error::ErrorKind;
        let fix = |words: &[&str]| match parsed(words) {
            Ok(Some(DoctorAction::Fix(fix))) => Ok(fix),
            Ok(other) => panic!("{words:?} parsed to {other:?}"), // ubs:ignore — the test's own failure
            Err(kind) => Err(kind),
        };
        let all = fix(&[
            "doctor",
            "fix",
            "--only",
            "fm-a",
            "--skip",
            "fm-b",
            "--severity",
            "P1",
            "--quick",
            "--online",
            "--budget",
            "5s",
            "--force",
            "--yes",
        ])
        .unwrap();
        assert_eq!(all.select.only, ["fm-a"]);
        assert_eq!(all.select.skip, ["fm-b"]);
        assert_eq!(all.select.severity.as_deref(), Some("P1"));
        assert!(all.select.quick && all.select.online && all.select.budget.is_some());
        assert!(all.force && all.yes);
        let deep = fix(&["doctor", "fix", "--include-deep"]).unwrap();
        assert!(deep.select.include_deep && !deep.force && !deep.yes);
        assert_eq!(
            fix(&["doctor", "fix", "--quick", "--include-deep"]).unwrap_err(),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            fix(&["doctor", "fix", "--only", "fm-a", "--force"]).unwrap_err(),
            ErrorKind::MissingRequiredArgument,
            "--force requires --yes"
        );
        assert!(fix(&["doctor", "fix", "--yes"]).is_ok_and(|f| f.yes && !f.force));
        for flag in ["--since", "--robot-triage"] {
            assert!(
                fix(&["doctor", "fix", flag, "latest"]).is_err(),
                "doctor fix takes no {flag}"
            );
        }
    }

    #[test]
    fn diff_args_parse() {
        let diff = |words: &[&str]| match parsed(words) {
            Ok(Some(DoctorAction::Diff(diff))) => Some(diff.run),
            Ok(_) | Err(_) => None,
        };
        assert_eq!(diff(&["doctor", "diff"]), Some(None));
        assert_eq!(
            diff(&["doctor", "diff", "latest"]),
            Some(Some("latest".to_owned()))
        );
        assert_eq!(diff(&["doctor", "diff", OLD]), Some(Some(OLD.to_owned())));
        assert_eq!(
            diff(&["doctor", "diff", "-t", "retail"]),
            Some(None),
            "-t is global"
        );
        assert_eq!(
            diff(&["doctor", "diff", "--only", "fm-a"]),
            None,
            "no selection flag"
        );
    }

    fn fix_ctx(words: &[&str]) -> Context {
        Context {
            output: crate::output::OutputConfig::machine(),
            command: "doctor fix",
            actor: crate::model::Actor::Human,
            reason: None,
            argv: argv(words),
            offline: false,
            offline_by_env: false,
            dry_run: false,
            while_running: false,
            if_match: None,
            target: None,
            started: std::time::Instant::now(),
            progress: crate::output::ProgressSink::none(),
            notes: crate::output::RunNotes::default(),
            stdin_is_tty: false,
            force_revalidate: false,
            applying: None,
        }
    }

    /// `--force --yes` without `--only` is refused before anything runs, and its fix is
    /// the preview, never the forced run.
    #[test]
    fn doctor_force_without_only_is_usage() {
        let words = [
            "doctor", "fix", "--force", "--yes", "-t", "retail", "--json",
        ];
        let args = FixArgs {
            force: true,
            yes: true,
            ..FixArgs::default()
        };
        let refused = run_fix(&fix_ctx(&words), &args).unwrap_err();
        assert_eq!(refused.error.code, ErrorCode::Usage);
        assert_eq!(refused.exit, Exit::UsageError);
        assert_eq!(refused.error.details["flag"], "--force");
        assert_eq!(
            refused.error.fix.argv().unwrap(),
            argv(&["doctor", "fix", "-t", "retail", "--dry-run"])
        );
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
        let (run, old) = read_since(&paths, OLD, &line(OLD), SinceArg::Flag).unwrap();
        assert_eq!(
            (run.as_str(), old),
            (OLD, vec!["fm-a-b@machine".to_owned()])
        );
        let (run, old) = read_since(&paths, "latest", &line("latest"), SinceArg::Flag).unwrap();
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
            let refused = read_since(&paths, given, &line(given), SinceArg::Flag).unwrap_err();
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
        let refused = read_since(&none, "latest", &line("latest"), SinceArg::Flag).unwrap_err();
        assert_eq!(
            (refused.code, refused.fix.argv()),
            (ErrorCode::NotFound, Some(argv(&["doctor"]).as_slice())),
            "no run at all: the fix drops --since"
        );
    }

    /// `doctor diff <run>` names its run as a positional: the fix replaces it there, and
    /// with no run at all drops it (diff's plan mode).
    #[test]
    fn diff_run_resolution_cases() {
        let (_dir, paths) = runs();
        let line = |given: &str| argv(&["doctor", "diff", given, "-t", "retail", "--json"]);
        let fixed = |run: &str| argv(&["doctor", "diff", run, "-t", "retail"]);
        let (run, _) = read_since(&paths, OLD, &line(OLD), SinceArg::Positional).unwrap();
        assert_eq!(run.as_str(), OLD);
        let unknown = "2026-01-01T00-00-00Z__01M8R000000000000000000000";
        let refused =
            read_since(&paths, unknown, &line(unknown), SinceArg::Positional).unwrap_err();
        assert_eq!(refused.code, ErrorCode::NotFound);
        assert_eq!(refused.fix.argv(), Some(fixed(NEW).as_slice()));
        let alternatives: Vec<_> = refused
            .alternatives
            .iter()
            .filter_map(|a| a.argv())
            .collect();
        assert_eq!(alternatives, [fixed(OLD).as_slice()]);

        let empty = tempfile::tempdir().unwrap();
        let none = Paths::with_roots(empty.path(), empty.path(), empty.path(), empty.path());
        let refused =
            read_since(&none, "latest", &line("latest"), SinceArg::Positional).unwrap_err();
        assert_eq!(
            refused.fix.argv(),
            Some(argv(&["doctor", "diff", "-t", "retail"]).as_slice()),
            "no run at all: diff's plan mode"
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
            let refused = read_since(&paths, OLD, &line, SinceArg::Flag).unwrap_err();
            assert_eq!(refused.code, code, "{case}: {}", refused.message);
            assert_eq!(
                refused.details["path"],
                report.display().to_string(),
                "{case}: names the file"
            );
        }
    }
}
