//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod adopt;
pub mod apply;
pub mod approval;
pub mod auth;
pub mod backup;
pub mod backup_verify;
pub mod backups_status;
pub mod capabilities;
pub mod complete;
pub mod completions;
pub mod config;
pub mod copy;
pub mod doctor;
pub mod doctor_gc;
pub mod doctor_undo;
pub mod drift;
pub mod engine;
pub mod errors;
pub mod errors_status;
pub mod export;
pub mod help;
pub mod help_extras;
pub mod history;
pub mod history_items;
pub mod info;
pub mod install;
pub mod install_completions;
pub mod install_skill;
pub mod intent;
pub mod issues;
pub mod list;
pub mod naming;
pub mod net_log;
pub mod omarchy;
pub mod outcome;
pub mod outdated;
pub mod pin;
pub mod plans;
pub mod precondition;
pub mod remembered;
pub mod remove;
pub mod resolve_args;
pub mod restore_addons;
pub mod robot_docs;
pub mod search;
pub mod skip;
pub mod status;
pub mod sync;
pub mod targets;
pub mod undo;
pub mod update;
pub mod world;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{Cli, Command, Context};
use crate::error::{Exit, ProcessExit};
use crate::output::{self, SharedReporter};
use targets::TargetsAction;

/// Records a settings change of `kind` (about `target`, for a `targets` change) in
/// history, in the session `s` of the command that made it, under the global lock it
/// holds (vision §5.9 Attribution): who, why and the argv. A line that cannot be written
/// is `history_not_recorded`, never the command's failure. Then it rings the doorbell:
/// every settings change counts as one that can change what `status` reports (M3-D4).
pub(crate) fn record_setting(
    ctx: &Context,
    s: &mut crate::fsx::WriteSession<'_>,
    paths: &crate::state::paths::Paths,
    kind: crate::model::TxKind,
    target: Option<crate::model::TargetKey>,
) {
    let entry = crate::state::history::SettingEntry::now(
        kind,
        target,
        ctx.actor,
        ctx.reason.clone(),
        ctx.argv.clone(),
    );
    if let Some(warning) = crate::state::history::record_setting(s, paths, &entry) {
        ctx.notes.warn(warning);
    }
    crate::state::doorbell::ring(paths, ctx.command, ctx.actor);
}

/// What a command is for: bare help lists the commands one line per group, so it stays
/// one screen as the command count grows (M2-D25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Read-only views.
    Look,
    /// Commands that change addons.
    Change,
    /// Clients, configuration and help.
    Setup,
}

impl Group {
    /// In bare help's order.
    pub const ALL: [Self; 3] = [Self::Look, Self::Change, Self::Setup];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Look => "Look",
            Self::Change => "Change",
            Self::Setup => "Setup",
        }
    }
}

/// Which exit dictionary a command's process exit comes from (M4-D5, M4-D40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Dictionary {
    /// The general dictionary (`Exit`): 0 to 6.
    General,
    /// The doctor tree's own dictionary (E3).
    Doctor,
}

impl Dictionary {
    pub const ALL: [Self; 2] = [Self::General, Self::Doctor];
}

/// Whether a command uses the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    /// No path of the command sends a request (it may read the release memo).
    None,
    /// Some path may send a request (a release, an archive, a listing, the rate limit);
    /// under --offline the command still does its local work, from caches, or refuses
    /// only what needs the network.
    Optional,
    /// Its purpose is an upstream answer (search, outdated): it asks by default, and
    /// offline it answers only from what is remembered, marking the rest unchecked.
    Required,
}

impl Network {
    pub const ALL: [Self; 3] = [Self::None, Self::Optional, Self::Required];
}

/// A command path this build has, with every fact clap cannot provide (M4-D2, M4-D5).
/// Not comparable: `data` is a function pointer.
#[derive(Debug, Clone, Copy)]
pub struct CommandEntry {
    /// Its canonical name; a nested one is its path (`targets add`): the envelope's
    /// `command`.
    pub name: &'static str,
    pub group: Group,
    /// Whether some form of it changes state (conservative: `export` writes with
    /// `--write`, `auth github` with `set`). A usage error never guesses one for you.
    pub mutates: bool,
    pub dictionary: Dictionary,
    /// Every process exit it can return, ascending: declared here, and verified against
    /// what the contract suite observes.
    pub exits: &'static [u8],
    /// The schema of its envelope's `data` (the `Outcome` its dispatch arm returns).
    pub data: DataSchema,
    /// Whether its dry run stores a plan (M4-D22): set by the plan store (D4).
    pub stores_plan: bool,
    pub network: Network,
}

/// What a row's `data` points at: the generator of its envelope's `data` schema, with the
/// `schemas` feature (every test build). Without it (the release binary, which prints the
/// schemas it embedded, omad-ho1p) there is no generator, and the row points at a
/// function that does nothing, so every row reads the same in both builds.
#[cfg(feature = "schemas")]
pub type DataSchema = fn(&mut schemars::SchemaGenerator) -> schemars::Schema;
#[cfg(not(feature = "schemas"))]
pub type DataSchema = fn();

/// The schema of `O`'s data: what a row's `data` points at, naming the outcome its
/// dispatch arm returns.
#[cfg(feature = "schemas")]
pub fn data_schema<O: output::Outcome>(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    g.root_schema_for::<O::Data>()
}

/// Without schemas: names the outcome, generates nothing ([`DataSchema`]).
#[cfg(not(feature = "schemas"))]
pub fn data_schema<O: output::Outcome>() {}

/// A doctor path's `data` schema: its report's, whose keys its envelope carries at the
/// top level (M4-D34).
#[cfg(feature = "schemas")]
pub fn report_schema<O: output::DoctorOutcome>(
    g: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    g.root_schema_for::<O::Report>()
}

/// Without schemas: names the report, generates nothing ([`DataSchema`]).
#[cfg(not(feature = "schemas"))]
pub fn report_schema<O: output::DoctorOutcome>() {}

/// A general-dictionary row that stores no plan; [`CommandEntry::doctor`] and
/// [`CommandEntry::storing_plan`] change either.
const fn entry(
    name: &'static str,
    group: Group,
    mutates: bool,
    exits: &'static [u8],
    data: DataSchema,
    network: Network,
) -> CommandEntry {
    CommandEntry {
        name,
        group,
        mutates,
        dictionary: Dictionary::General,
        exits,
        data,
        stores_plan: false,
        network,
    }
}

impl CommandEntry {
    /// A doctor-tree command (E3): its exits come from the doctor dictionary.
    pub const fn doctor(self) -> Self {
        Self {
            dictionary: Dictionary::Doctor,
            ..self
        }
    }

    /// A command whose dry run stores a plan (D4, M4-D22).
    pub const fn storing_plan(self) -> Self {
        Self {
            stores_plan: true,
            ..self
        }
    }
}

use Group::{Change, Look, Setup};

/// Every runnable command path this build has, one per line, in display order within its
/// group (a nested one takes its parent's group): every clap leaf, and every parent whose
/// bare form runs (`backup`, `config`, `errors`, `omarchy`, `targets`). A parent that
/// requires a subcommand (`auth`) has no line. Each command bead adds its own line, every
/// field filled, in the commit that adds its clap variant (DEC-1).
///
/// Exits beyond 0, 1 and 2 (and 4 and 5 on a mutating row): 3 where the command resolves
/// a selector, id, ref, name or file; 4 where it loads the world (a registry or record a
/// newer omaddons wrote is `state_too_new`) or refuses a change; 6 exactly with
/// `--check` or `--strict`.
#[rustfmt::skip]
pub const COMMANDS: &[CommandEntry] = &[
    entry("status", Look, false, &[0, 1, 2, 3, 4, 6], data_schema::<status::StatusOutcome>, Network::None),
    entry("list", Look, false, &[0, 1, 2, 3, 4], data_schema::<list::ListOutcome>, Network::None),
    entry("outdated", Look, false, &[0, 1, 2, 3, 4, 6], data_schema::<outdated::OutdatedOutcome>, Network::Required),
    entry("info", Look, false, &[0, 1, 2, 3, 4], data_schema::<info::InfoOutcome>, Network::Optional),
    entry("search", Look, false, &[0, 1, 2, 3, 4], data_schema::<search::SearchOutcome>, Network::Required),
    entry("errors", Look, false, &[0, 1, 2, 3, 4, 6], data_schema::<errors::ErrorsOutcome>, Network::None),
    entry("errors dismiss", Look, true, &[0, 1, 2, 3, 4, 5], data_schema::<errors::DismissOutcome>, Network::None),
    entry("export", Look, true, &[0, 1, 2, 3, 4, 5], data_schema::<export::ExportOutcome>, Network::None),
    entry("history", Look, false, &[0, 1, 2, 3, 4], data_schema::<history::HistoryOutcome>, Network::None),
    entry("plans show", Look, false, &[0, 1, 2, 3], data_schema::<plans::PlansShowOutcome>, Network::None),
    entry("net-log", Look, false, &[0, 1, 2], data_schema::<net_log::NetLogOutcome>, Network::None),
    // Bare `doctor` runs as `doctor diagnose`: this name appears only on usage errors.
    entry("doctor", Look, false, &[0, 1, 4, 6, 64, 66, 73, 74], report_schema::<doctor::DiagnoseOutcome>, Network::Optional).doctor(),
    entry("doctor diagnose", Look, false, &[0, 1, 4, 6, 64, 66, 73, 74], report_schema::<doctor::DiagnoseOutcome>, Network::Optional).doctor(),
    entry("doctor explain", Look, false, &[0, 64, 66, 74], report_schema::<doctor::ExplainOutcome>, Network::None).doctor(),
    entry("doctor fix", Look, true, &[0, 1, 2, 3, 4, 5, 6, 64, 66, 73, 74], report_schema::<doctor::FixRun>, Network::Optional).doctor(),
    entry("doctor diff", Look, false, &[0, 1, 4, 64, 66, 74], report_schema::<doctor::DiffRun>, Network::None).doctor(),
    entry("doctor undo", Look, true, &[0, 3, 4, 5, 64, 66, 73, 74], report_schema::<doctor_undo::DoctorUndoRun>, Network::None).doctor(),
    entry("doctor gc", Look, true, &[0, 4, 5, 64, 74], report_schema::<doctor_gc::GcRun>, Network::None).doctor(),
    entry("install", Change, true, &[0, 1, 2, 3, 4, 5, 6], data_schema::<install::InstallOutcome>, Network::Optional).storing_plan(),
    entry("update", Change, true, &[0, 1, 2, 3, 4, 5, 6], data_schema::<update::UpdateOutcome>, Network::Optional).storing_plan(),
    entry("pin", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<pin::PinOutcome>, Network::Optional).storing_plan(),
    entry("unpin", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<pin::PinOutcome>, Network::Optional).storing_plan(),
    entry("skip", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<skip::SkipOutcome>, Network::Optional).storing_plan(),
    entry("unskip", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<skip::SkipOutcome>, Network::Optional).storing_plan(),
    entry("adopt", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<adopt::AdoptOutcome>, Network::Optional).storing_plan(),
    entry("sync", Change, true, &[0, 1, 2, 3, 4, 5, 6], data_schema::<sync::SyncOutcome>, Network::Optional).storing_plan(),
    entry("copy", Change, true, &[0, 1, 2, 3, 4, 5, 6], data_schema::<copy::CopyOutcome>, Network::Optional).storing_plan(),
    entry("remove", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<remove::RemoveOutcome>, Network::None).storing_plan(),
    entry("undo", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<undo::UndoOutcome>, Network::None).storing_plan(),
    entry("apply", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<apply::ApplyOutcome>, Network::Optional),
    // Bare `backup` runs as `backup list`, bare `omarchy` as `omarchy status`: these two
    // names appear only on usage errors.
    entry("backup", Change, false, &[0, 1, 2], data_schema::<backup::BackupListOutcome>, Network::None),
    entry("backup list", Change, false, &[0, 1, 2, 3, 4], data_schema::<backup::BackupListOutcome>, Network::None),
    entry("backup create", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup::BackupCreateOutcome>, Network::None),
    entry("backup restore", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup::BackupRestoreOutcome>, Network::None).storing_plan(),
    entry("backup prune", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup::BackupPruneOutcome>, Network::None).storing_plan(),
    entry("backup export", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup::BackupExportOutcome>, Network::None),
    entry("backup import", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup::BackupImportOutcome>, Network::None),
    entry("backup verify", Change, true, &[0, 1, 2, 3, 4, 5], data_schema::<backup_verify::BackupVerifyOutcome>, Network::None),
    entry("targets", Setup, false, &[0, 1, 2, 4], data_schema::<targets::Done<targets::TargetsData>>, Network::None),
    entry("targets add", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<targets::Done<targets::AddData>>, Network::None),
    entry("targets rm", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<targets::Done<targets::RmData>>, Network::None),
    entry("targets rename", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<targets::Done<targets::RenameData>>, Network::None),
    entry("targets default", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<targets::Done<targets::DefaultData>>, Network::None),
    entry("config", Setup, false, &[0, 1, 2], data_schema::<config::Done<config::ConfigData>>, Network::None),
    entry("config get", Setup, false, &[0, 1, 2], data_schema::<config::Done<config::GetData>>, Network::None),
    entry("config set", Setup, true, &[0, 1, 2, 4, 5], data_schema::<config::Done<config::SetData>>, Network::None),
    entry("config path", Setup, false, &[0, 1, 2], data_schema::<config::Done<config::PathData>>, Network::None),
    entry("auth github", Setup, true, &[0, 1, 2, 4, 5], data_schema::<auth::AuthOutcome>, Network::Optional),
    entry("omarchy", Setup, false, &[0, 1, 2], data_schema::<omarchy::StatusOutcome>, Network::None),
    entry("omarchy status", Setup, false, &[0, 1, 2], data_schema::<omarchy::StatusOutcome>, Network::None),
    entry("omarchy install", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<omarchy::InstallOutcome>, Network::None),
    entry("omarchy remove", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<omarchy::RemoveOutcome>, Network::None),
    entry("completions", Setup, false, &[0, 1, 2], data_schema::<completions::CompletionsOutcome>, Network::None),
    entry("install-completions", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<install_completions::InstallCompletionsOutcome>, Network::None),
    entry("robot-docs", Setup, false, &[0, 1, 2], data_schema::<robot_docs::GuideOutcome>, Network::None),
    entry("robot-docs schema", Setup, false, &[0, 1, 2], data_schema::<robot_docs::SchemaOutcome>, Network::None),
    entry("robot-docs errors", Setup, false, &[0, 1, 2], data_schema::<robot_docs::ErrorsOutcome>, Network::None),
    entry("robot-docs guide", Setup, false, &[0, 1, 2], data_schema::<robot_docs::GuideOutcome>, Network::None),
    entry("robot-docs install-skill", Setup, true, &[0, 1, 2, 3, 4, 5], data_schema::<install_skill::InstallSkillOutcome>, Network::None),
    entry("capabilities", Setup, false, &[0, 1, 2], data_schema::<capabilities::CapabilitiesOutcome>, Network::None),
    entry("help", Setup, false, &[0, 1, 2], data_schema::<help::HelpOutcome>, Network::None),
];

/// An envelope `command` that is no clap path: it never appears in help, in
/// capabilities' commands or in the widget's `CLI_PATHS`.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeOnly {
    pub name: &'static str,
    pub exits: &'static [u8],
    pub data: DataSchema,
    pub why: &'static str,
}

pub const ENVELOPE_ONLY: &[EnvelopeOnly] = &[
    EnvelopeOnly {
        name: "omaddons",
        exits: &[1, 2],
        data: data_schema::<help::HelpOutcome>,
        why: "no command could be read from the arguments: a usage error before any subcommand, or a panic before parsing",
    },
    EnvelopeOnly {
        name: "version",
        exits: &[0],
        data: data_schema::<help::HelpOutcome>,
        why: "-V or --version before any subcommand",
    },
];

/// The row of the command at `path`.
pub fn command_entry(path: &str) -> Option<&'static CommandEntry> {
    COMMANDS.iter().find(|c| c.name == path)
}

/// The exit dictionary of the envelope `command`: its row's; the general one for a name
/// with no row (`omaddons`, `version`).
pub fn dictionary_of(command: &str) -> Dictionary {
    command_entry(command).map_or(Dictionary::General, |c| c.dictionary)
}

/// The exits a row declares, else an [`ENVELOPE_ONLY`] name's; `None` for any other.
pub fn declared_exits(name: &str) -> Option<&'static [u8]> {
    command_entry(name).map(|c| c.exits).or_else(|| {
        ENVELOPE_ONLY
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.exits)
    })
}

/// Whether the command at `path` (`targets add`) may change state; one this build does
/// not list counts as changing it.
pub fn mutates(path: &str) -> bool {
    COMMANDS
        .iter()
        .find(|c| c.name == path)
        .is_none_or(|c| c.mutates)
}

/// The canonical (static) name for a command path read from argv that did not parse:
/// its row; for a parent with no row, the only row under it (`auth` is `auth github`,
/// the only command its user can have meant); else `omaddons`.
pub fn canonical(name: &str) -> &'static str {
    if let Some(row) = COMMANDS.iter().find(|c| c.name == name) {
        return row.name;
    }
    let under = format!("{name} ");
    let mut rows = COMMANDS.iter().filter(|c| c.name.starts_with(&under));
    match (rows.next(), rows.next()) {
        (Some(only), None) => only.name,
        _ => "omaddons",
    }
}

/// The group of the command `name`, and its place in `COMMANDS`; a top-level name with no
/// row (`auth`) takes those of the first row under it.
pub fn group_of(name: &str) -> Option<(Group, usize)> {
    let under = format!("{name} ");
    let rows = || COMMANDS.iter().enumerate();
    rows()
        .find(|(_, c)| c.name == name)
        .or_else(|| rows().find(|(_, c)| c.name.starts_with(&under)))
        .map(|(at, c)| (c.group, at))
}

/// The envelope `command` of a parsed invocation; bare `omaddons` is help.
pub fn command_name(command: Option<&Command>) -> &'static str {
    command.map_or("help", canonical_path)
}

/// The canonical path of a parsed command: the one mapping from what argv parsed to its
/// name (the envelope's `command`, `Context::command`), a `COMMANDS` row (M4-D55 rule 5).
/// A bare command that runs its default action names that action (`backup` is `backup
/// list`, `omarchy` is `omarchy status`).
pub fn canonical_path(command: &Command) -> &'static str {
    match command {
        Command::Help { .. } => "help",
        Command::Status(_) => "status",
        Command::Install(_) => "install",
        Command::Update(_) => "update",
        Command::Pin(_) => "pin",
        Command::Unpin(_) => "unpin",
        Command::Skip(_) => "skip",
        Command::Unskip(_) => "unskip",
        Command::Adopt(_) => "adopt",
        Command::Sync(_) => "sync",
        Command::Copy(_) => "copy",
        Command::Remove(_) => "remove",
        Command::Undo(_) => "undo",
        Command::Apply(_) => "apply",
        Command::Backup(args) => args
            .action
            .as_ref()
            .map_or("backup list", backup::BackupAction::command),
        Command::History(_) => "history",
        Command::Plans(args) => args.action.command(),
        Command::Info(_) => "info",
        Command::Export(_) => "export",
        Command::Outdated(_) => "outdated",
        Command::Search(_) => "search",
        Command::Errors(args) => match args.action {
            Some(errors::ErrorsAction::Dismiss(_)) => "errors dismiss",
            None => "errors",
        },
        Command::NetLog(_) => "net-log",
        Command::Doctor(args) => args
            .action
            .as_ref()
            .map_or("doctor diagnose", doctor::DoctorAction::command),
        Command::List(_) => "list",
        Command::Targets(args) => args
            .action
            .as_ref()
            .map_or("targets", TargetsAction::command),
        Command::Config(args) => args
            .action
            .as_ref()
            .map_or("config", config::ConfigAction::command),
        Command::Auth(args) => args.service.command(),
        Command::Omarchy(args) => args
            .action
            .as_ref()
            .map_or("omarchy status", omarchy::OmarchyAction::command),
        Command::Completions(_) => "completions",
        Command::InstallCompletions(_) => install_completions::INSTALL_COMPLETIONS_COMMAND,
        Command::RobotDocs(args) => args.action.as_ref().map_or(
            robot_docs::GUIDE_COMMAND,
            robot_docs::RobotDocsAction::command,
        ),
        Command::Capabilities(_) => "capabilities",
    }
}

/// The run's output: the shared reporter, locked only for each write, so a command
/// reporting progress while it runs never waits on `dispatch`.
struct Out<'a> {
    cfg: &'a output::OutputConfig,
    reporter: &'a SharedReporter,
    name: &'static str,
    started: std::time::Instant,
    offline: bool,
    notes: &'a output::RunNotes,
    /// For an internal error's fix, which must not rerun what failed, and a retryable
    /// one's, which reruns it.
    argv: &'a [String],
    /// `OMADDONS_OFFLINE=1`, which no rerun without `--offline` escapes.
    offline_by_env: bool,
}

impl Out<'_> {
    fn emit<O: output::Outcome>(&self, result: crate::error::Result<O>) -> Exit {
        self.emit_as(self.name, result)
    }

    fn emit_as<O: output::Outcome>(
        &self,
        name: &'static str,
        result: crate::error::Result<O>,
    ) -> Exit {
        let result = result.map_err(|mut e| {
            self.boundary_fix(name, &mut e);
            e
        });
        let mut reporter = output::lock(self.reporter);
        output::emit(
            self.cfg,
            &mut reporter,
            name,
            self.started,
            self.offline,
            self.notes,
            result,
        )
    }

    /// A doctor path's result, in the doctor's dictionary: a failure with the same
    /// boundary fixes as [`Out::emit_as`]'s.
    fn emit_doctor<O: output::DoctorOutcome>(
        &self,
        result: std::result::Result<O, output::DoctorFailure>,
    ) -> ProcessExit {
        let result = result.map_err(|mut failure| {
            self.boundary_fix(self.name, &mut failure.error);
            failure
        });
        let mut reporter = output::lock(self.reporter);
        output::emit_doctor(
            self.cfg,
            &mut reporter,
            self.name,
            self.started,
            self.offline,
            self.notes,
            result,
        )
        .into()
    }

    /// The fixes only the command's boundary knows: an internal error's never reruns
    /// what failed; a retryable `plan_stale` and a `locked` rerun the same command; an
    /// `offline` one, when `--offline` alone made the run offline, reruns it without; a
    /// `target_not_found` whose selector was a near miss names the closest one in the
    /// user's own command (M4-D14).
    fn boundary_fix(&self, name: &str, e: &mut crate::error::Error) {
        use crate::error::ErrorCode;
        let rerun = |argv: &[String]| crate::error::Suggestion::command(crate::cli::fix_argv(argv));
        match e.code {
            ErrorCode::Internal if e.fix == crate::error::internal_fix() => {
                e.fix = crate::error::internal_fix_for(name, self.argv);
            }
            ErrorCode::PlanStale if e.retryable => e.fix = rerun(self.argv),
            ErrorCode::Locked => e.fix = rerun(self.argv),
            ErrorCode::Offline => {
                if let Some(online) = crate::cli::online_rerun(self.argv, self.offline_by_env) {
                    e.fix = crate::error::Suggestion::command(online);
                }
            }
            ErrorCode::TargetNotFound => {
                crate::discover::registry::fix_with_closest_selector(e, self.argv);
            }
            _ => {}
        }
    }

    fn log(&self, level: output::LogLevel, message: &str) {
        output::lock(self.reporter).log(level, message);
    }

    fn lock_released(&self, held: Option<u64>, tx: Option<crate::model::TxId>) {
        if let (Some(ms), Some(tx)) = (held, tx) {
            self.log(
                output::LogLevel::Debug,
                &format!("lock released: tx {tx}, lock_held_ms {ms}"),
            );
        }
    }
}

/// What a dispatch arm logs about an outcome before rendering it: its `-v` debug notes,
/// then one `lock released` line per transaction the run committed. A direct invocation
/// and `apply` log them through [`emit_side_events`] alike (M4-D23, one render).
pub(crate) trait SideEvents {
    fn notes(&self) -> &[String] {
        &[]
    }

    /// Each committed transaction and how long it held the global lock, in order.
    fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
        Vec::new()
    }
}

/// A storable command's arm: its stored plan's `-v` note, its side events, then the
/// outcome.
fn out_storable<O: intent::Storable + SideEvents>(
    ctx: &Context,
    out: &Out<'_>,
    mut result: crate::error::Result<O>,
) -> Exit {
    if let Ok(outcome) = &mut result {
        if let Some(note) = intent::offer(ctx, outcome) {
            out.log(output::LogLevel::Debug, &note);
        }
        emit_side_events(out, outcome);
    }
    out.emit(result)
}

/// A command's arm that logs only its side events, then the outcome.
fn out_logged<O: output::Outcome + SideEvents>(
    out: &Out<'_>,
    result: crate::error::Result<O>,
) -> Exit {
    if let Ok(outcome) = &result {
        emit_side_events(out, outcome);
    }
    out.emit(result)
}

/// `events`' notes, then its lock lines, as `-v` debug events.
fn emit_side_events(out: &Out<'_>, events: &dyn SideEvents) {
    for note in events.notes() {
        out.log(output::LogLevel::Debug, note);
    }
    for (tx, ms) in events.locks() {
        out.lock_released(Some(ms), Some(tx));
    }
}

/// The one lock of a single-target command, when it committed and held one.
fn one_lock(tx: Option<crate::model::TxId>, held: Option<u64>) -> Vec<(crate::model::TxId, u64)> {
    tx.zip(held).into_iter().collect()
}

impl SideEvents for install::InstallOutcome {
    fn notes(&self) -> &[String] {
        &self.notes
    }

    fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
        one_lock(output::Outcome::data(self).tx, self.lock_held_ms)
    }
}

/// The commands over several targets: their notes, then each target's lock.
macro_rules! side_events_of_runs {
    ($($outcome:ty),+) => {
        $(impl SideEvents for $outcome {
            fn notes(&self) -> &[String] {
                &self.notes
            }

            fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
                self.locks.clone()
            }
        })+
    };
}

side_events_of_runs!(
    update::UpdateOutcome,
    pin::PinOutcome,
    skip::SkipOutcome,
    sync::SyncOutcome,
    copy::CopyOutcome,
    adopt::AdoptOutcome
);

impl SideEvents for undo::UndoOutcome {
    fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
        self.locks.clone()
    }
}

impl SideEvents for remove::RemoveOutcome {
    fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
        one_lock(output::Outcome::data(self).tx, self.lock_held_ms)
    }
}

impl SideEvents for backup::BackupRestoreOutcome {
    fn locks(&self) -> Vec<(crate::model::TxId, u64)> {
        one_lock(output::Outcome::data(self).tx, self.lock_held_ms)
    }
}

impl SideEvents for backup::BackupPruneOutcome {}

/// The read commands: their notes only.
macro_rules! side_events_of_reads {
    ($($outcome:ty),+) => {
        $(impl SideEvents for $outcome {
            fn notes(&self) -> &[String] {
                &self.notes
            }
        })+
    };
}

side_events_of_reads!(
    status::StatusOutcome,
    info::InfoOutcome,
    export::ExportOutcome,
    outdated::OutdatedOutcome,
    search::SearchOutcome,
    errors::ErrorsOutcome
);

/// Runs the parsed command and renders its outcome through `reporter`, the run's one
/// stderr reporter (also behind `ctx.progress`), and returns its exit in the command's
/// dictionary.
pub fn dispatch(cli: &Cli, ctx: &Context, reporter: &SharedReporter) -> ProcessExit {
    let name = command_name(cli.command.as_ref());
    let out = Out {
        cfg: &ctx.output,
        reporter,
        name,
        started: ctx.started,
        offline: ctx.offline,
        notes: &ctx.notes,
        argv: &ctx.argv,
        offline_by_env: ctx.offline_by_env,
    };
    if cli.global.version {
        return out.emit_as("version", Ok(help::version())).into();
    }
    if cli.global.help {
        // The command as typed, as `help <path>` reads it: `backup --help` is backup's
        // help, though its envelope name is the default action's (`backup list`).
        let result = match &cli.command {
            None => help::for_flag(None, &ctx.argv),
            Some(_) => {
                let path = crate::cli::command_path(&ctx.argv).join(" ");
                help::for_flag(Some(&path), &ctx.argv)
            }
        };
        return out.emit_as("help", result).into();
    }
    // The doctor exits in its own dictionary, and changes nothing a history projection
    // follows.
    if let Some(Command::Doctor(args)) = &cli.command {
        return match args.verb() {
            doctor::Verb::Diagnose(diagnose) => {
                let result = doctor::run_diagnose(ctx, diagnose);
                if let Ok(outcome) = &result {
                    for note in &outcome.notes {
                        out.log(output::LogLevel::Debug, note);
                    }
                }
                out.emit_doctor(result)
            }
            doctor::Verb::Explain(explain) => out.emit_doctor(doctor::run_explain(ctx, explain)),
            // A repair's own cleanup rebuilds the history projection once it releases the
            // lock (E5), so the arm needs no rebuild of its own.
            doctor::Verb::Fix(fix) => {
                let result = doctor::run_fix(ctx, fix);
                if let Ok(run) = &result {
                    for note in &run.outcome.notes {
                        out.log(output::LogLevel::Debug, note);
                    }
                }
                out.emit_doctor(result)
            }
            doctor::Verb::Diff(diff) => out.emit_doctor(doctor::run_diff(ctx, diff)),
            // As `doctor fix`: the repair undo's own cleanup rebuilds the projection.
            doctor::Verb::Undo(undo) => out.emit_doctor(doctor_undo::run(ctx, undo)),
            doctor::Verb::Gc(gc) => out.emit_doctor(doctor_gc::run(ctx, gc)),
        };
    }
    let exit = run_command(cli, ctx, &out);
    // A command that may have appended history (by its canonical path, a `COMMANDS` row:
    // `config set`, not `config`) rebuilds the history projection it could not keep
    // current under its lock, now that the lock is released (M3-D37). The parsed command
    // names it, so argv is not walked again: that walk builds every subcommand's clap
    // definition, a fixed cost of about 0.35 ms that `status --cached` paid on every
    // widget refresh.
    if mutates(name)
        && !ctx.dry_run
        && let Ok(paths) = crate::state::paths::Paths::from_env()
    {
        crate::state::history_tail::rebuild_if_stale(&paths, name, ctx.actor);
    }
    exit.into()
}

/// Runs the parsed command and renders its outcome through `out`.
fn run_command(cli: &Cli, ctx: &Context, out: &Out<'_>) -> Exit {
    match &cli.command {
        None => out.emit(Ok(help::bare())),
        // `dispatch` runs the doctor first, in its own dictionary.
        Some(Command::Doctor(_)) => out.emit::<help::HelpOutcome>(Err(
            crate::error::Error::internal("the doctor is dispatched before run_command"),
        )),
        Some(Command::Help { command }) => {
            let path = command.join(" ");
            let name = (!path.is_empty()).then_some(path.as_str());
            out.emit(help::for_command(name, &ctx.argv))
        }
        Some(Command::Status(args)) => {
            let result = status::run(ctx, *args);
            if let Ok(outcome) = &result {
                let d = outcome.discovery;
                out.log(
                    output::LogLevel::Debug,
                    &format!(
                        "discovery: scanned {}, roots reused {}, parsed {}, cache written {}",
                        if d.scanned { "yes" } else { "no" },
                        d.reused,
                        d.parsed,
                        if d.written { "yes" } else { "no" }
                    ),
                );
                emit_side_events(out, outcome);
            }
            out.emit(result)
        }
        Some(Command::Install(args)) => out_storable(ctx, out, install::run(ctx, args)),
        Some(Command::Update(args)) => out_storable(ctx, out, update::run(ctx, args)),
        Some(Command::Pin(args)) => out_storable(ctx, out, pin::pin(ctx, args)),
        Some(Command::Unpin(args)) => out_storable(ctx, out, pin::unpin(ctx, args)),
        Some(Command::Skip(args)) => out_storable(ctx, out, skip::skip(ctx, args)),
        Some(Command::Unskip(args)) => out_storable(ctx, out, skip::unskip(ctx, args)),
        Some(Command::Sync(args)) => out_storable(ctx, out, sync::run(ctx, args)),
        Some(Command::Copy(args)) => out_storable(ctx, out, copy::run(ctx, args)),
        Some(Command::Remove(args)) => out_storable(ctx, out, remove::run(ctx, args)),
        Some(Command::Undo(args)) => out_storable(ctx, out, undo::run(ctx, args)),
        Some(Command::Apply(args)) => out_logged(out, apply::run(ctx, args)),
        Some(Command::Backup(args)) => {
            use backup::BackupAction;
            match &args.action {
                None => out.emit(backup::list(ctx, &backup::BackupListArgs::default())),
                Some(BackupAction::List(list)) => out.emit(backup::list(ctx, list)),
                Some(BackupAction::Create(create)) => out.emit(backup::create(ctx, create)),
                Some(BackupAction::Restore(restore)) => {
                    out_storable(ctx, out, backup::restore_backup(ctx, restore))
                }
                Some(BackupAction::Prune(prune)) => {
                    out_storable(ctx, out, backup::prune_backups(ctx, prune))
                }
                Some(BackupAction::Export(export)) => out.emit(backup::export_backup(ctx, export)),
                Some(BackupAction::Import(import)) => out.emit(backup::import_backup(ctx, import)),
                Some(BackupAction::Verify(verify)) => out.emit(backup_verify::run(ctx, verify)),
            }
        }
        Some(Command::Info(args)) => out_logged(out, info::run(ctx, args)),
        Some(Command::Export(args)) => out_logged(out, export::run(ctx, args)),
        Some(Command::Adopt(args)) => out_storable(ctx, out, adopt::run(ctx, args)),
        Some(Command::Outdated(args)) => out_logged(out, outdated::run(ctx, args)),
        Some(Command::Search(args)) => out_logged(out, search::run(ctx, args)),
        Some(Command::NetLog(args)) => out.emit(net_log::run(ctx, args)),
        Some(Command::Errors(args)) => match &args.action {
            Some(errors::ErrorsAction::Dismiss(_)) => out.emit(errors::dismiss(ctx)),
            None => out_logged(out, errors::run(ctx, args)),
        },
        Some(Command::History(args)) => {
            let result = history::run(ctx, args, cli.global.actor.as_deref());
            if result.as_ref().is_ok_and(|o| o.torn_tail) {
                out.log(
                    output::LogLevel::Info,
                    "history.jsonl ends with an incomplete line (an interrupted write); it was skipped",
                );
            }
            out.emit(result)
        }
        Some(Command::Plans(args)) => match &args.action {
            plans::PlansAction::Show(show) => out_logged(out, plans::show(ctx, show)),
        },
        Some(Command::List(args)) => out.emit(list::run(ctx, *args)),
        Some(Command::Targets(args)) => match &args.action {
            None => out.emit(targets::list(ctx, args.rescan)),
            Some(TargetsAction::Add { path, name: id }) => {
                out.emit(targets::add(ctx, path, id.as_deref()))
            }
            Some(TargetsAction::Rm { id, purge, yes }) => {
                out.emit(targets::rm(ctx, id, *purge, *yes))
            }
            Some(TargetsAction::Rename { id, new }) => out.emit(targets::rename(ctx, id, new)),
            Some(TargetsAction::Default { id }) => out.emit(targets::default(ctx, id)),
        },
        Some(Command::Auth(args)) => out.emit(auth::run(ctx, args)),
        Some(Command::Omarchy(args)) => {
            use omarchy::OmarchyAction;
            match &args.action {
                None | Some(OmarchyAction::Status) => out.emit(omarchy::status()),
                Some(OmarchyAction::Install { no_enable, section }) => {
                    out.emit(omarchy::install(ctx, !no_enable, *section))
                }
                Some(OmarchyAction::Remove) => out.emit(omarchy::remove(ctx)),
            }
        }
        Some(Command::Config(args)) => {
            use config::ConfigAction;
            match &args.action {
                None => out.emit(config::list()),
                Some(ConfigAction::Get { key }) => out.emit(config::get(key)),
                Some(ConfigAction::Set { key, value }) => out.emit(config::set(ctx, key, value)),
                Some(ConfigAction::Path) => out.emit(config::path()),
            }
        }
        Some(Command::Completions(args)) => out.emit(completions::run(args)),
        Some(Command::InstallCompletions(args)) => out.emit(install_completions::run(ctx, args)),
        Some(Command::RobotDocs(args)) => match &args.action {
            None | Some(robot_docs::RobotDocsAction::Guide) => out.emit(robot_docs::guide()),
            Some(robot_docs::RobotDocsAction::Schema(schema)) => {
                out.emit(robot_docs::schema(&schema.command))
            }
            Some(robot_docs::RobotDocsAction::Errors) => out.emit(robot_docs::errors()),
            Some(robot_docs::RobotDocsAction::InstallSkill(skill)) => {
                out.emit(install_skill::run(ctx, skill))
            }
        },
        Some(Command::Capabilities(_)) => out.emit(capabilities::run()),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::Cli;

    /// The arms log what [`SideEvents`] says, through the same two impl macros the
    /// outcomes use (their fields are private to their modules); the commands' own `-v`
    /// tests hold the lines each command logs.
    #[test]
    fn side_events_of_each_outcome() {
        use crate::model::TxId;
        let tx = |n: u8| -> TxId { format!("01JB8Q2VC000000000000000{n:02}").parse().unwrap() };

        struct Run {
            notes: Vec<String>,
            locks: Vec<(TxId, u64)>,
        }
        side_events_of_runs!(Run);
        let run = Run {
            notes: vec!["first".into(), "second".into()],
            locks: vec![(tx(2), 7), (tx(1), 3)],
        };
        assert_eq!(run.notes(), ["first", "second"], "notes in order");
        assert_eq!(run.locks(), [(tx(2), 7), (tx(1), 3)], "locks in order");

        struct Read {
            notes: Vec<String>,
        }
        side_events_of_reads!(Read);
        let read = Read {
            notes: vec!["looked".into()],
        };
        assert_eq!(
            (read.notes(), read.locks()),
            (&["looked".to_owned()][..], Vec::new())
        );

        // A single-target command logs its lock only when it committed and held one.
        for (name, committed, held, want) in [
            ("both", Some(tx(1)), Some(5), vec![(tx(1), 5)]),
            ("no_tx", None, Some(5), Vec::new()),
            ("no_hold", Some(tx(1)), None, Vec::new()),
            ("neither", None, None, Vec::new()),
        ] {
            assert_eq!(one_lock(committed, held), want, "{name}");
        }
    }

    #[test]
    fn canonical_path_names_each_sample_by_its_row() {
        let cases: &[(&[&str], &str)] = &[
            (&[], "help"),
            (&["help", "install"], "help"),
            (&["status"], "status"),
            (&["ls"], "list"),
            (&["add", "x"], "install"),
            (&["upgrade"], "update"),
            (&["rm", "x"], "remove"),
            (&["revert"], "undo"),
            (&["log"], "history"),
            (&["show", "x"], "info"),
            (&["find", "x"], "search"),
            (&["check"], "outdated"),
            (&["backup"], "backup list"),
            (&["backup", "create"], "backup create"),
            (&["omarchy"], "omarchy status"),
            (&["omarchy", "install"], "omarchy install"),
            (&["targets"], "targets"),
            (&["targets", "add", "/x"], "targets add"),
            (&["config"], "config"),
            (&["config", "get", "k"], "config get"),
            (&["errors"], "errors"),
            (&["errors", "dismiss"], "errors dismiss"),
            (&["auth", "github"], "auth github"),
            (&["auth", "github", "set"], "auth github"),
            (&["auth", "github", "clear"], "auth github"),
            (&["auth", "github", "status", "--offline"], "auth github"),
            (&["auth", "github", "--from-gh"], "auth github"),
            (&["auth", "--from-gh", "github"], "auth github"),
            (&["plans", "show", "01M8Q2VA"], "plans show"),
            (&["robot-docs"], "robot-docs guide"),
            (&["robot-docs", "guide"], "robot-docs guide"),
            (&["robot-docs", "errors"], "robot-docs errors"),
            (&["robot-docs", "install-skill"], "robot-docs install-skill"),
        ];
        for (argv, expected) in cases {
            let parsed =
                Cli::try_parse_from(std::iter::once("omaddons").chain(argv.iter().copied()))
                    .unwrap_or_else(|e| panic!("sample {argv:?} does not parse: {e}")); // ubs:ignore
            let got = command_name(parsed.command.as_ref());
            assert_eq!(
                got, *expected,
                "sample {argv:?}: expected {expected}, got {got}"
            );
            assert!(
                COMMANDS.iter().any(|c| c.name == got),
                "sample {argv:?}: {got} is no COMMANDS row"
            );
        }
    }

    /// Every clap path under `cmd`, with whether it is a leaf and whether it requires a
    /// subcommand.
    fn clap_paths(cmd: &clap::Command, prefix: &str, found: &mut Vec<(String, bool, bool)>) {
        for sub in cmd.get_subcommands() {
            let path = if prefix.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            let leaf = sub.get_subcommands().next().is_none();
            found.push((path.clone(), leaf, sub.is_subcommand_required_set()));
            clap_paths(sub, &path, found);
        }
    }

    #[test]
    fn every_clap_path_has_its_row_and_every_row_is_a_clap_path() {
        let mut root = Cli::command();
        root.build();
        let mut paths = Vec::new();
        clap_paths(&root, "", &mut paths);
        let rows = |path: &str| COMMANDS.iter().filter(|c| c.name == path).count();
        for (path, leaf, required) in &paths {
            match (leaf, required) {
                (true, _) => assert_eq!(
                    rows(path),
                    1,
                    "clap leaf `{path}` has no COMMANDS row (DEC-1), or more than one"
                ),
                (false, true) => assert_eq!(
                    rows(path),
                    0,
                    "`{path}` cannot run bare, so it has no row: its subcommands carry rows"
                ),
                (false, false) => assert_eq!(
                    rows(path),
                    1,
                    "`{path}` runs bare, so it needs a COMMANDS row"
                ),
            }
        }
        for row in COMMANDS {
            let runnable = paths
                .iter()
                .any(|(path, leaf, required)| path == row.name && (*leaf || !*required));
            assert!(runnable, "COMMANDS row `{}` names no clap path", row.name);
            let top = row.name.split(' ').next().unwrap_or_default();
            if top != row.name {
                assert_eq!(
                    group_of(top).map(|(g, _)| g),
                    Some(row.group),
                    "`{}` takes its top-level command's group",
                    row.name
                );
            }
        }
        assert_eq!(rows("auth"), 0, "no auth row");
        assert_eq!(rows("auth github"), 1, "one auth github row");
        for (name, canonical) in [
            ("targets add", "targets add"),
            ("nosuch", "omaddons"),
            ("auth", "auth github"),
            ("backup", "backup"),
        ] {
            assert_eq!(super::canonical(name), canonical, "canonical({name:?})");
        }
        let wire: Vec<String> = Group::ALL
            .iter()
            .map(|g| {
                serde_json::to_value(g)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(wire, ["look", "change", "setup"]);
    }

    #[test]
    fn group_of_places_a_rowless_parent_at_its_first_subcommand() {
        assert_eq!(group_of("auth"), group_of("auth github"));
        assert!(group_of("auth").is_some());
        assert_eq!(group_of("nosuch"), None);
    }

    // ---- the per-command facts (M4-D2, M4-D5) ----

    /// Whether the clap command at `path` has a `--check` or `--strict` flag.
    fn has_check_or_strict(path: &str) -> bool {
        let mut root = Cli::command();
        root.build();
        let cmd = path
            .split(' ')
            .try_fold(&root, |cmd, name| cmd.find_subcommand(name));
        cmd.is_some_and(|cmd| {
            cmd.get_arguments()
                .any(|a| matches!(a.get_long(), Some("check" | "strict")))
        })
    }

    /// Every static exit rule `entry` breaks, one message each.
    fn exit_rule_violations(entry: &CommandEntry, has_check_or_strict: bool) -> Vec<String> {
        let mut broken = Vec::new();
        let exits = entry.exits;
        if exits.is_empty() {
            broken.push("exits is empty".to_owned());
        }
        if !exits.windows(2).all(|w| w[0] < w[1]) {
            broken.push(format!("exits {exits:?} are not strictly ascending"));
        }
        match entry.dictionary {
            Dictionary::General => {
                let general: Vec<u8> = Exit::ALL.iter().map(|e| e.code()).collect();
                let outside: Vec<&u8> = exits.iter().filter(|c| !general.contains(c)).collect();
                if !outside.is_empty() {
                    broken.push(format!("{outside:?} are outside the general dictionary"));
                }
                for code in [0, 1, 2] {
                    if !exits.contains(&code) {
                        broken.push(format!("a general row lists 0, 1 and 2: {code} is missing"));
                    }
                }
            }
            Dictionary::Doctor => {
                let doctor: Vec<u8> = crate::doctor::exit::Exit::ALL
                    .iter()
                    .map(|e| e.code())
                    .collect();
                let outside: Vec<&u8> = exits.iter().filter(|c| !doctor.contains(c)).collect();
                if !outside.is_empty() {
                    broken.push(format!("{outside:?} are outside the doctor dictionary"));
                }
                // healthy, usage_error and io_error: every doctor command can end so.
                for code in [0, 64, 74] {
                    if !exits.contains(&code) {
                        broken.push(format!(
                            "a doctor row lists 0, 64 and 74: {code} is missing"
                        ));
                    }
                }
            }
        }
        if entry.mutates {
            for code in [4, 5] {
                if !exits.contains(&code) {
                    broken.push(format!("a mutating row lists 4 and 5: {code} is missing"));
                }
            }
        }
        // In the doctor's dictionary 6 is online_required, never an assertion.
        if entry.dictionary == Dictionary::General && exits.contains(&6) != has_check_or_strict {
            broken.push(format!(
                "6 is listed exactly with --check or --strict: listed {}, flag {has_check_or_strict}",
                exits.contains(&6)
            ));
        }
        if entry.stores_plan && !entry.mutates {
            broken.push("a row that stores a plan must mutate".to_owned());
        }
        broken
    }

    #[test]
    fn every_row_follows_the_static_exit_rules() {
        for row in COMMANDS {
            let broken = exit_rule_violations(row, has_check_or_strict(row.name));
            assert!(
                broken.is_empty(),
                "{} (exits {:?}): {broken:?}",
                row.name,
                row.exits
            );
        }
    }

    #[test]
    fn the_static_rules_reject_each_planted_violation() {
        let base = entry(
            "x",
            Group::Look,
            false,
            &[0, 1, 2],
            data_schema::<help::HelpOutcome>,
            Network::None,
        );
        let with = |exits: &'static [u8], mutates: bool| CommandEntry {
            exits,
            mutates,
            ..base
        };
        for (case, planted, flag, expected) in [
            (
                "general_without_2",
                with(&[0, 1], false),
                false,
                "2 is missing",
            ),
            (
                "mutating_without_5",
                with(&[0, 1, 2, 4], true),
                false,
                "5 is missing",
            ),
            (
                "mutating_without_4",
                with(&[0, 1, 2, 5], true),
                false,
                "4 is missing",
            ),
            (
                "check_flag_without_6",
                with(&[0, 1, 2], false),
                true,
                "exactly with --check",
            ),
            (
                "six_without_check_flag",
                with(&[0, 1, 2, 6], false),
                false,
                "exactly with --check",
            ),
            (
                "unsorted",
                with(&[0, 2, 1], false),
                false,
                "strictly ascending",
            ),
            (
                "duplicate",
                with(&[0, 0, 1, 2], false),
                false,
                "strictly ascending",
            ),
            (
                "outside_the_general_dictionary",
                with(&[0, 1, 2, 7], false),
                false,
                "outside the general",
            ),
            ("empty", with(&[], false), false, "exits is empty"),
            ("plan_on_a_read", base.storing_plan(), false, "must mutate"),
            (
                "doctor_row_without_64",
                CommandEntry {
                    exits: &[0, 1, 74],
                    ..base.doctor()
                },
                false,
                "64 is missing",
            ),
            (
                "doctor_row_outside_its_dictionary",
                CommandEntry {
                    exits: &[0, 7, 64, 74],
                    ..base.doctor()
                },
                false,
                "outside the doctor dictionary",
            ),
        ] {
            let broken = exit_rule_violations(&planted, flag);
            assert!(
                broken.iter().any(|m| m.contains(expected)),
                "{case} (exits {:?}): expected {expected:?} in {broken:?}",
                planted.exits
            );
        }
        let valid = with(&[0, 1, 2, 4, 5], true);
        let broken = exit_rule_violations(&valid, false);
        assert!(broken.is_empty(), "valid: {broken:?}");
        // 6 is online_required in the doctor's dictionary, with no --check flag.
        let doctor = CommandEntry {
            exits: &[0, 1, 6, 64, 74],
            ..base.doctor()
        };
        let broken = exit_rule_violations(&doctor, false);
        assert!(broken.is_empty(), "a valid doctor row: {broken:?}");
    }

    fn schema_of(row: &str) -> serde_json::Value {
        let entry = command_entry(row);
        assert!(entry.is_some(), "no row {row}");
        let mut generator = schemars::generate::SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator();
        entry.map_or(serde_json::Value::Null, |e| {
            (e.data)(&mut generator).to_value()
        })
    }

    #[test]
    fn every_row_declares_a_data_schema() {
        for row in COMMANDS {
            let schema = schema_of(row.name);
            assert!(
                schema
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "{}: no titled root schema",
                row.name
            );
        }
        for (a, b) in [
            ("backup", "backup list"),
            ("omarchy", "omarchy status"),
            ("pin", "unpin"),
        ] {
            assert_eq!(schema_of(a), schema_of(b), "{a} and {b} share an outcome");
        }
        assert_ne!(schema_of("install"), schema_of("update"));
    }

    #[test]
    fn network_is_declared_as_decided() {
        let network = |name: &str| command_entry(name).map(|c| c.network);
        for name in ["search", "outdated"] {
            assert_eq!(network(name), Some(Network::Required), "{name}");
        }
        for name in [
            "install",
            "update",
            "pin",
            "unpin",
            "adopt",
            "sync",
            "copy",
            "info",
            "auth github",
        ] {
            assert_eq!(network(name), Some(Network::Optional), "{name}");
        }
        for name in [
            "status",
            "list",
            "errors",
            "errors dismiss",
            "export",
            "history",
            "net-log",
            "remove",
            "undo",
            "backup",
            "backup list",
            "backup create",
            "backup restore",
            "backup prune",
            "backup export",
            "backup import",
            "backup verify",
            "targets",
            "targets add",
            "targets rm",
            "targets rename",
            "targets default",
            "config",
            "config get",
            "config set",
            "config path",
            "omarchy",
            "omarchy status",
            "omarchy install",
            "omarchy remove",
            "completions",
            "help",
        ] {
            assert_eq!(network(name), Some(Network::None), "{name}");
        }
    }

    #[test]
    fn envelope_only_names_are_no_clap_paths() {
        let mut root = Cli::command();
        root.build();
        for only in ENVELOPE_ONLY {
            assert!(command_entry(only.name).is_none(), "{} is a row", only.name);
            assert!(
                root.find_subcommand(only.name).is_none(),
                "{} is a clap path",
                only.name
            );
            assert!(!only.why.is_empty());
        }
        assert_eq!(declared_exits("version"), Some(&[0][..]));
        assert_eq!(declared_exits("omaddons"), Some(&[1, 2][..]));
        assert_eq!(
            declared_exits("install"),
            command_entry("install").map(|c| c.exits)
        );
        assert_eq!(declared_exits("auth"), None);
    }

    #[test]
    fn command_entry_finds_rows_by_path() {
        let add = command_entry("targets add");
        assert_eq!(
            add.map(|c| (c.group, c.mutates)),
            Some((Group::Setup, true))
        );
        assert!(command_entry("auth").is_none());
        // The doctor tree, and only it, exits in the doctor's dictionary.
        for row in COMMANDS {
            assert_eq!(
                row.dictionary == Dictionary::Doctor,
                row.name.split(' ').next() == Some("doctor"),
                "{}",
                row.name
            );
        }
    }

    #[test]
    fn the_storable_commands_are_the_intent_variants() {
        use std::collections::BTreeSet;
        let storing: BTreeSet<_> = COMMANDS
            .iter()
            .filter(|c| c.stores_plan)
            .map(|c| c.name)
            .collect();
        let intents: BTreeSet<_> = intent::StoredIntent::PATHS
            .iter()
            .map(|(path, _)| *path)
            .collect();
        assert_eq!(
            storing,
            intents,
            "only in COMMANDS: {:?}; only in intents: {:?}",
            storing.difference(&intents).collect::<Vec<_>>(),
            intents.difference(&storing).collect::<Vec<_>>()
        );
        for path in intents {
            assert!(
                command_entry(path).is_some_and(|row| row.mutates),
                "{path}: a stored intent must name a mutating command"
            );
        }
    }
}
