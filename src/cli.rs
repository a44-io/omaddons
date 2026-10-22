//! clap derive definitions: global flags, subcommands, aliases.
//!
//! `run` is the whole program: parse, build the [`Context`] (DEC-2), dispatch, and
//! render through `output`. Every outcome, including clap errors, help, version and
//! panics, reaches stdout and stderr only through the output layer, so the contract
//! holds even when parsing fails (vision §5.9, D17). Each command bead adds its own
//! subcommand (DEC-1).

use std::ffi::OsString;
use std::io::IsTerminal;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{ArgAction, CommandFactory, Parser, Subcommand};

use crate::error::{Error, ErrorCode, Exit, Suggestion};
use crate::limits::REASON_MAX_CHARS;
use crate::model::Actor;
use crate::output::{self, OutputConfig, OutputFlags, Reporter, TermEnv};

/// The command line.
#[derive(Debug, Parser)]
#[command(
    name = "omaddons",
    bin_name = "omaddons",
    about = "World of Warcraft addon manager for Linux and an Omarchy widget",
    color = clap::ColorChoice::Never,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    args_override_self = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags every command accepts, before or after the subcommand (vision §5.9).
#[derive(Debug, Clone, Default, clap::Args)]
#[command(next_help_heading = "Global options")]
pub struct GlobalArgs {
    /// One JSON envelope on stdout
    #[arg(long, global = true)]
    pub json: bool,
    /// The same as --json
    #[arg(long, global = true)]
    pub robot: bool,
    /// Output format
    #[arg(
        long,
        global = true,
        value_name = "FORMAT",
        value_parser = ["json", "human"],
        ignore_case = true
    )]
    pub format: Option<String>,
    /// The client to act on: id, key, game mode (sod and hardcore select the Classic Era
    /// client), family, product, flavor folder or path
    #[arg(
        short = 't',
        long = "target",
        visible_alias = "flavor",
        global = true,
        value_name = "TARGET"
    )]
    pub target: Option<String>,
    /// Show the plan without changing anything (ignored by commands that change nothing)
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Who is acting: human, agent or widget [env: OMADDONS_ACTOR]
    #[arg(long, global = true, value_name = "ACTOR")]
    pub actor: Option<String>,
    /// Why, recorded verbatim in history (at most 200 characters; ignored by commands
    /// that change nothing)
    #[arg(long, global = true, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Fail any network access [env: OMADDONS_OFFLINE=1]
    #[arg(long, global = true)]
    pub offline: bool,
    /// Allow folder changes while the client runs (never SavedVariables)
    #[arg(long, global = true)]
    pub while_running: bool,
    /// Change nothing unless the plan is still the one the --dry-run that returned this
    /// token (data.precondition) showed
    #[arg(long, global = true, value_name = "TOKEN")]
    pub if_match: Option<String>,
    /// No ANSI color
    #[arg(long, global = true)]
    pub no_color: bool,
    /// No progress output
    #[arg(long, global = true)]
    pub no_progress: bool,
    /// Progress events on stderr
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        value_parser = ["json", "human", "none"],
        ignore_case = true
    )]
    pub progress: Option<String>,
    /// Only errors on stderr
    #[arg(long, global = true)]
    pub quiet: bool,
    /// More detail (repeatable)
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Show help
    #[arg(short = 'h', long = "help", global = true)]
    pub help: bool,
    /// Show the version (before any command: `install --version V` pins a release)
    #[arg(short = 'V', long = "version")]
    pub version: bool,
}

impl GlobalArgs {
    fn output_flags(&self) -> OutputFlags {
        OutputFlags {
            json: self.json,
            robot: self.robot,
            format: self.format.clone(),
            progress: self.progress.clone(),
            no_color: self.no_color,
            no_progress: self.no_progress,
            quiet: self.quiet,
            verbose: self.verbose,
        }
    }
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show help for omaddons or one command
    Help {
        /// The command to explain; a nested one by its path (`targets add`)
        #[arg(num_args = 0..)]
        command: Vec<String>,
    },
    /// Triage: every client, its addons, running state and what needs attention
    Status(crate::app::status::StatusArgs),
    /// Install or update addons in one target, verified by its own TOC rules
    #[command(
        visible_alias = "add",
        after_long_help = "An addon is installed from one ref at a time. Another local archive of a local\ninstall (./Bar-2.2.0.zip over ./Bar-2.1.0.zip) updates it; an addon of the same\nname from any other ref is a takeover, refused (ownership_conflict) unless --force."
    )]
    Install(crate::app::install::InstallArgs),
    /// Update every managed addon on every client, or the ones named (undo with one undo)
    #[command(visible_alias = "upgrade", after_long_help = UPDATE_HELP)]
    Update(crate::app::update::UpdateArgs),
    /// Hold an addon at its release through updates (undoable)
    #[command(after_long_help = PIN_HELP)]
    Pin(crate::app::pin::PinArgs),
    /// Let a pinned addon update again
    #[command(after_long_help = UNPIN_HELP)]
    Unpin(crate::app::pin::UnpinArgs),
    /// Bring addons installed by hand or by other managers under management, as they are
    /// (records only, undoable); alone, report where they come from
    #[command(after_long_help = ADOPT_HELP)]
    Adopt(crate::app::adopt::AdoptArgs),
    /// Make every client match addons.toml: install what is missing, set pins and channels
    #[command(after_long_help = SYNC_HELP)]
    Sync(crate::app::sync::SyncArgs),
    /// Install one client's addons on another, where builds exist (undoable)
    #[command(after_long_help = COPY_HELP)]
    Copy(crate::app::copy::CopyArgs),
    /// Remove managed addons from one target (undoable)
    #[command(visible_aliases = ["uninstall", "rm", "delete"])]
    Remove(crate::app::remove::RemoveArgs),
    /// Revert the last change (or one by id), SavedVariables included
    #[command(visible_aliases = ["revert", "rollback"])]
    Undo(crate::app::undo::UndoArgs),
    /// Each client's WTF/ backups: copies of its settings (bare: list them)
    Backup(crate::app::backup::BackupArgs),
    /// Show what changed, when, by whom and why
    #[command(visible_alias = "log")]
    History(crate::app::history::HistoryArgs),
    /// Which managed addons have updates, on every client (changes nothing)
    #[command(visible_alias = "check", after_long_help = OUTDATED_HELP)]
    Outdated(crate::app::outdated::OutdatedArgs),
    /// What an addon is: installed records, catalog entry and newest release
    #[command(visible_alias = "show")]
    Info(crate::app::info::InfoArgs),
    /// Find addons by name in the catalog, WoWInterface, Tukui and GitHub
    #[command(visible_alias = "find", after_long_help = SEARCH_HELP)]
    Search(crate::app::search::SearchArgs),
    /// Which addon most likely throws Lua errors, how often, and whether an update may
    /// fix it (from BugGrabber)
    #[command(after_long_help = ERRORS_HELP, args_conflicts_with_subcommands = true)]
    Errors(crate::app::errors::ErrorsArgs),
    /// Every client's addon set as one addons.toml for sync (stdout, or --write)
    #[command(after_long_help = EXPORT_HELP)]
    Export(crate::app::export::ExportArgs),
    /// Every network request omaddons made
    NetLog(crate::app::net_log::NetLogArgs),
    /// List the addons in each target's AddOns folder
    #[command(visible_alias = "ls")]
    List(crate::app::list::ListArgs),
    /// List the WoW clients, or add, remove, rename and choose them
    Targets(crate::app::targets::TargetsArgs),
    /// Show or change config.toml
    Config(crate::app::config::ConfigArgs),
    /// Store, check or clear the opt-in GitHub token (5,000 REST requests an hour)
    #[command(after_long_help = AUTH_HELP)]
    Auth(crate::app::auth::AuthArgs),
    /// Put the omaddons widget on Omarchy's bar
    Omarchy(crate::app::omarchy::OmarchyArgs),
}

/// What every command receives (DEC-2).
#[derive(Debug, Clone)]
pub struct Context {
    pub output: OutputConfig,
    /// The command, as the envelope's `command` names it (`targets add`).
    pub command: &'static str,
    pub actor: Actor,
    pub reason: Option<String>,
    /// Verbatim (lossy UTF-8), with `argv[0]` normalized to `omaddons`, for history and
    /// for building `fix_argv`s.
    pub argv: Vec<String>,
    pub offline: bool,
    /// `OMADDONS_OFFLINE=1`: the run stays offline without `--offline` too.
    pub offline_by_env: bool,
    pub dry_run: bool,
    pub while_running: bool,
    /// `--if-match`: the preview token a destructive run is bound to (M3-D46), already
    /// checked to be well formed and given to a command that binds.
    pub if_match: Option<String>,
    /// The raw `-t` value; the registry resolves it.
    pub target: Option<String>,
    pub started: Instant,
    /// Where long commands report progress (nothing when progress is off).
    pub progress: crate::output::ProgressSink,
    /// What the run found on its way (warnings), reported even when it then fails.
    pub notes: crate::output::RunNotes,
    /// Standard input is a terminal: a command that reads a secret from it refuses.
    pub stdin_is_tty: bool,
}

impl Context {
    /// Validates the attribution flags against an injected environment, for `command`.
    pub fn new(
        global: &GlobalArgs,
        output: OutputConfig,
        command: &'static str,
        argv: &[String],
        env: &dyn Fn(&str) -> Option<String>,
        stdin_is_tty: bool,
        started: Instant,
    ) -> Result<Self, Error> {
        let actor = Actor::resolve(
            global.actor.as_deref(),
            env("OMADDONS_ACTOR").as_deref(),
            stdin_is_tty,
        )
        .map_err(|err| {
            let fix = if matches!(err.origin, "--actor") {
                actor_fix(argv, global.actor.as_deref().unwrap_or_default())
            } else {
                Suggestion::text("unset OMADDONS_ACTOR, or set it to human, agent or widget")
            };
            Error::new(ErrorCode::Usage, err.to_string(), fix)
        })?;
        if let Some(reason) = &global.reason
            && reason.chars().count() > REASON_MAX_CHARS
        {
            let cut: String = reason.chars().take(REASON_MAX_CHARS).collect();
            return Err(Error::new(
                ErrorCode::Usage,
                format!(
                    "--reason has {} characters; the limit is {REASON_MAX_CHARS}",
                    reason.chars().count()
                ),
                Suggestion::command(fix_argv(&with_flag_value(argv, "--reason", &cut))),
            ));
        }
        if let Some(token) = &global.if_match {
            check_if_match(token, command, argv)?;
        }
        Ok(Self {
            output,
            command,
            actor,
            reason: global.reason.clone(),
            argv: argv.to_vec(),
            offline: global.offline || env("OMADDONS_OFFLINE").as_deref() == Some("1"),
            offline_by_env: env("OMADDONS_OFFLINE").as_deref() == Some("1"),
            dry_run: global.dry_run,
            while_running: global.while_running,
            if_match: global.if_match.clone(),
            target: global.target.clone(),
            started,
            progress: crate::output::ProgressSink::none(),
            notes: crate::output::RunNotes::default(),
            stdin_is_tty,
        })
    }
}

/// `usage` unless `--if-match <token>` goes with a command whose runs bind to a preview
/// (M3-D46) and `token` is a well-formed precondition token. Off a binding command the
/// fix drops the flag for a command that only reads, and is the command's help for one
/// that changes something; a malformed token's fix is the preview that returns one.
fn check_if_match(token: &str, command: &str, argv: &[String]) -> Result<(), Error> {
    use crate::app::precondition::{BINDING_COMMANDS, is_token, preview_argv};
    if !BINDING_COMMANDS.contains(&command) {
        let (last, rest) = BINDING_COMMANDS.split_last().unwrap_or((&"", &[]));
        let fix = if crate::app::mutates(command) {
            let mut help = vec!["omaddons".to_owned(), "help".to_owned()];
            help.extend(command.split(' ').map(str::to_owned));
            Suggestion::command(help)
        } else {
            Suggestion::command(fix_argv(&without_flag(argv, "--if-match")))
        };
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--if-match binds a run to its preview, and only {} and {last} take it, not {command}",
                rest.join(", ")
            ),
            fix,
        ));
    }
    if !is_token(token) {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "--if-match takes the precondition token a --dry-run returns (data.precondition: pc1: and 64 hex digits), not {token:?}"
            ),
            Suggestion::command(preview_argv(argv)),
        ));
    }
    Ok(())
}

/// The program: parse `args`, run, render, and exit with a code from the general exit
/// dictionary (never Rust's 101, even on a panic).
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let raw: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let term = TermEnv::from_process();
    install_panic_hook();
    let started = Instant::now();
    let exit = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let env = |name: &str| std::env::var(name).ok();
        run_with(&raw, &term, &env, std::io::stdin().is_terminal(), started)
    }))
    .unwrap_or_else(|_| render_panic());
    ExitCode::from(exit)
}

fn run_with(
    raw: &[OsString],
    term: &TermEnv,
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    started: Instant,
) -> Exit {
    let argv = normalize_argv(raw);
    let fallback = fallback_output(raw, term);
    let early_offline = output::offline_from_argv(raw, env);
    set_panic_state(fallback, "omaddons", &argv, started, early_offline);
    let cli = match Cli::try_parse_from(raw) {
        Ok(cli) => cli,
        // `-h` on a command with required arguments: help, not a usage error.
        Err(_) if wants_help(&argv) => {
            let path = command_path(&argv).join(" ");
            let name = (!path.is_empty()).then_some(path.as_str());
            let result = crate::app::help::for_command(name, &argv);
            let mut reporter = Reporter::stderr(&fallback);
            return output::emit(
                &fallback,
                &mut reporter,
                "help",
                started,
                early_offline,
                &output::RunNotes::default(),
                result,
            );
        }
        Err(err) => {
            let error = usage_error(&err, &argv);
            return render_error(&fallback, command_of(&argv), started, early_offline, error);
        }
    };
    let output = output::resolve(&cli.global.output_flags(), term);
    let command = crate::app::command_name(cli.command.as_ref());
    let offline = cli.global.offline || env("OMADDONS_OFFLINE").as_deref() == Some("1");
    set_panic_state(output, command, &argv, started, offline);
    if let Err(err) = crate::fsx::failpoint::hit("cli.panic") {
        let mut error = Error::internal(err.to_string());
        error.fix = crate::error::internal_fix_for(command, &argv);
        return render_error(&output, command, started, offline, error);
    }
    let mut ctx = match Context::new(
        &cli.global,
        output,
        command,
        &argv,
        env,
        stdin_is_tty,
        started,
    ) {
        Ok(ctx) => ctx,
        Err(error) => return render_error(&output, command, started, offline, error),
    };
    // A panic from here on reports what the run noted (an undo hint among them).
    set_panic_notes(&ctx.notes);
    ctx.notes
        .online_rerun(online_rerun(&ctx.argv, ctx.offline_by_env));
    // One reporter for the whole run: the command's progress and the outcome's output.
    let reporter = output::shared_stderr(&output);
    ctx.progress = output::ProgressSink::new(&reporter, &output);
    crate::app::dispatch(&cli, &ctx, &reporter)
}

/// The output configuration until argv parses: its presentation flags read token by
/// token, so a usage error renders as asked (`--json`, `--no-color`, `--quiet`, …).
fn fallback_output(raw: &[OsString], term: &TermEnv) -> OutputConfig {
    output::resolve(&output::flags_from_argv(raw), term)
}

/// Renders an error that has no outcome (usage errors, a failed failpoint).
fn render_error(
    cfg: &OutputConfig,
    command: &'static str,
    started: Instant,
    offline: bool,
    error: Error,
) -> Exit {
    let mut reporter = Reporter::stderr(cfg);
    output::emit::<crate::app::help::HelpOutcome, _>(
        cfg,
        &mut reporter,
        command,
        started,
        offline,
        &output::RunNotes::default(),
        Err(error),
    )
}

/// Lossy argv with `argv[0]` normalized to `omaddons`, so the widget can substitute its
/// configured binary.
fn normalize_argv(raw: &[OsString]) -> Vec<String> {
    std::iter::once("omaddons".to_owned())
        .chain(raw.iter().skip(1).map(|a| a.to_string_lossy().into_owned()))
        .collect()
}

/// The canonical name of the subcommand the user reached for, for the envelope of a
/// usage error; `omaddons` when there is none.
fn command_of(argv: &[String]) -> &'static str {
    let path = command_path(argv);
    if path.is_empty() {
        return "omaddons";
    }
    crate::app::canonical(&path.join(" "))
}

/// `outdated`'s long help.
const OUTDATED_HELP: &str = "\
Examples:
  omaddons outdated                 what has updates, on every client
  omaddons outdated -t retail -v    one client, every addon whatever its status
  omaddons outdated --changelog     each update's release notes since the installed one
  omaddons outdated --check         exit 6 when any update is available";

/// `update`'s long help.
const UPDATE_HELP: &str = "\
Examples:
  omaddons update                   every addon on every client; one undo reverts it all
  omaddons update DBM -t retail     one addon on one client
  omaddons update --dry-run         what would change
  omaddons update WeakAuras --channel beta

A given version is an install, which pins it:
  omaddons install <addon> --version V
  omaddons pin <addon> --version V";

/// `pin`'s long help.
const PIN_HELP: &str = "\
Examples:
  omaddons pin WeakAuras                          hold it at its installed release
  omaddons pin DBM --version 12.1.14 -t retail    install that release and hold it there";

/// `unpin`'s long help.
const UNPIN_HELP: &str = "\
Examples:
  omaddons unpin WeakAuras          let it update again (nothing is installed now)";

/// `adopt`'s long help.
const ADOPT_HELP: &str = "\
Examples:
  omaddons adopt                                  where each unmanaged folder comes from
  omaddons adopt --apply -t retail                adopt what is identified, folders unchanged
  omaddons adopt MyFoo --ref gh:o/foo -t retail   say which ref a folder is";

/// `export`'s long help.
const EXPORT_HELP: &str = "\
Examples:
  omaddons export > addons.toml     every client's addon set, for sync elsewhere
  omaddons export -t vanilla        only these clients
  omaddons export --write           to omaddons' config directory, the old file kept as .bak";

/// `copy`'s long help.
const COPY_HELP: &str = "\
Examples:
  omaddons copy --from era --to anniversary             every explicitly installed addon
  omaddons copy --from era --to anniversary Questie     only the ones named
  omaddons copy --from era --to anniversary --dry-run   what would install, what is skipped

Copies every addon installed explicitly on --from (or the ones named) to --to, in one
transaction, unpinned. Dependencies resolve on --to from its own TOCs. An addon already
there, a local file and an addon with no build for --to are skipped, never failed.";

/// `search`'s long help.
const SEARCH_HELP: &str = "\
Examples:
  omaddons search details           the catalog, WoWInterface, Tukui and GitHub at once
  omaddons find elvui --source tukui
  omaddons search bags --offline    what this machine already knows

Only GitHub's search receives the query; the catalog, the WoWInterface index and
the Tukui list are searched on this machine. Compatibility per client is a hint:
install verifies each addon's TOC.";

/// `errors`' long help.
const ERRORS_HELP: &str = "\
Examples:
  omaddons errors                                  every client, newest first
  omaddons errors --since 1d --json                the last day, for an agent
  omaddons errors -t retail --addon WeakAuras -v   one addon, every frame
  omaddons errors --check                          exit 6 on new errors

Messages an addon wrote itself may hold other players' names: they show only
with --raw-details. Errors other players sent through BugSack are counted apart.";

/// `auth`'s long help.
const AUTH_HELP: &str = "\
Examples:
  omaddons auth github                        which token is used, and the hour's REST budget
  echo \"$TOKEN\" | omaddons auth github set    store a token (standard input only, never argv)
  omaddons auth github --from-gh              store the token gh auth token prints
  omaddons auth github clear                  forget the stored token

The main GitHub path needs no token. A token goes only to api.github.com; GITHUB_TOKEN
and gh's credentials are never read unless you ask with --from-gh. With --dry-run, set
and clear check the token and write nothing.";

/// `sync`'s long help: the file it reads.
const SYNC_HELP: &str = "\
Examples:
  omaddons sync --dry-run                         what the file would change
  omaddons sync                                   install what is missing, set pins and channels
  omaddons sync dotfiles/addons.toml --locked     the exact versions the file records

The file (default: addons.toml in omaddons' config directory):

  schema_version = \"1.0\"

  [family.mainline]               # every live mainline client
  addons = [\"WeakAuras\", \"DBM\", \"gh:BigWigsMods/BigWigs\"]

  [[family.mainline.addon]]       # options: ref, channel, version, pinned
  name = \"Plater\"
  version = \"Plater-v661\"
  pinned = true

  [target.\"ptr\"]                  # one client by id (the only way to reach a test client)
  addons = [\"DBM\"]

sync installs what is missing and sets pins and channels; it never updates an installed
addon (omaddons update does). A second run changes nothing. With --locked it installs
the version each entry records (from the download cache when it is still there), so
`omaddons export` on one machine and `sync --locked` on another reproduce the set.
With --prune it also removes managed addons the file does not list, never one a listed
addon requires or a lock names, and never on a client the file has no section for; a
section for no known client refuses it.";

// ---------------------------------------------------------------------------------------
// Usage errors.

/// Flags about presentation, never part of a fix: the consumer picks its own format.
const PRESENTATION_FLAGS: &[(&str, bool)] = &[
    ("--json", false),
    ("--robot", false),
    ("--format", true),
    ("--progress", true),
    ("--no-color", false),
    ("--no-progress", false),
];

/// Maps a clap error to `usage` (exit 2) with the closest valid command as the fix
/// (Levenshtein distance ≤ 2), else help.
pub fn usage_error(err: &clap::Error, argv: &[String]) -> Error {
    let rendered = err.to_string();
    let mut lines = rendered.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut message = lines
        .next()
        .unwrap_or("invalid arguments")
        .trim_start_matches("error: ")
        .to_owned();
    // "…were not provided:" names the arguments on the following line(s).
    if message.ends_with(':') {
        let detail: Vec<&str> = lines.take_while(|l| !l.starts_with("Usage:")).collect();
        if !detail.is_empty() {
            message = format!("{message} {}", detail.join(", "));
        }
    }
    let invalid = |kind: ContextKind| match err.get(kind) {
        Some(ContextValue::String(token)) => Some(token.clone()),
        _ => None,
    };
    // A presentation flag the parser refused (`--json=1`, `--format xml`, `--format`
    // without its value): the same command without it, which the caller reruns with
    // its own presentation flags. It is the failing argv minus that flag, so the
    // never-itself check below does not apply.
    if let Some(flag) = invalid(ContextKind::InvalidArg)
        && is_presentation(flag_name(&flag))
    {
        let fix = Suggestion::command(fix_argv(argv));
        if parses(&fix) {
            return Error::new(ErrorCode::Usage, message, fix);
        }
    }
    let cmd = Cli::command();
    let path = command_path(argv);
    let subcommand = path.first().cloned();
    let help_fix = || {
        let mut argv = vec!["omaddons".to_owned(), "help".to_owned()];
        argv.extend(path.iter().cloned());
        (Suggestion::command(argv), Vec::new())
    };
    // `update --version V`: a version is an install (which pins), never an update.
    // ubs:ignore — comparing a clap error kind, not a secret
    let version_on_update = err.kind() == ErrorKind::UnknownArgument
        && subcommand.as_deref() == Some("update")
        && invalid(ContextKind::InvalidArg)
            .is_some_and(|bad| bad.split('=').next() == Some("--version"));
    // A command that is only its subcommands (`omarchy`): clap says one is missing, or
    // renders its whole help, whose first line is its about.
    if matches!(
        err.kind(),
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        let mut root = Cli::command();
        root.build();
        let names: Vec<&str> = command_at(&root, &path)
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect();
        if !names.is_empty() {
            message = format!(
                "omaddons {} needs a subcommand: {}",
                path.join(" "),
                names.join(", ")
            );
        }
    }
    if version_on_update {
        let mut as_pin = as_install(argv);
        if let Some(slot) = as_pin.get_mut(1) {
            "pin".clone_into(slot);
        }
        return Error::new(
            ErrorCode::Usage,
            "update installs the newest release; a given version is an install, which pins it",
            Suggestion::command(as_install(argv)),
        )
        .alternative(Suggestion::command(as_pin));
    }
    let (fix, alternatives) = match err.kind() {
        ErrorKind::InvalidSubcommand => invalid(ContextKind::InvalidSubcommand)
            .and_then(|bad| {
                // M2-D39: `install` alone would leave a current addon as it is.
                if bad == "reinstall" {
                    return Some((reinstall_fix(argv), Vec::new()));
                }
                let (fix, alternatives, closest) = subcommand_fix(argv, &path, &bad)?;
                if let Some(closest) = closest {
                    message = format!("{message}; the closest command is {closest}");
                }
                Some((fix, alternatives))
            })
            .unwrap_or_else(help_fix),
        ErrorKind::UnknownArgument => invalid(ContextKind::InvalidArg)
            .and_then(|bad| {
                let bad_flag = bad.split('=').next().unwrap_or(&bad).to_owned();
                // `status --version`: the program's version is asked before any command.
                if subcommand.is_some() && matches!(bad_flag.as_str(), "--version" | "-V") {
                    let (help, _) = help_fix();
                    return Some((Suggestion::command(["omaddons", "--version"]), vec![help]));
                }
                // The root's global flags, and every flag of the command parsing reached
                // (`targets add`, not `targets`). The verb is the user's own, so the
                // corrected flag is applied.
                let mut root = Cli::command();
                root.build();
                let mut longs: Vec<String> = cmd
                    .get_arguments()
                    .filter(|a| subcommand.is_none() || a.is_global_set())
                    .filter_map(|a| a.get_long())
                    .map(|l| format!("--{l}"))
                    .collect();
                if !path.is_empty() {
                    longs.extend(
                        command_at(&root, &path)
                            .get_arguments()
                            .filter_map(|a| a.get_long_and_visible_aliases())
                            .flatten()
                            .map(|l| format!("--{l}")),
                    );
                }
                longs.sort();
                longs.dedup();
                // A misspelled presentation flag (`--jsno`): the command without it, as
                // for a refused one.
                let closest = longs
                    .iter()
                    .map(|l| (levenshtein(&bad_flag, l), l))
                    .filter(|(d, _)| *d <= 2)
                    .min();
                if closest.is_some_and(|(_, l)| is_presentation(l)) {
                    let without: Vec<String> =
                        argv.iter().filter(|a| **a != bad).cloned().collect();
                    return Some((Suggestion::command(fix_argv(&without)), Vec::new()));
                }
                suggest(argv, &bad_flag, &longs)
            })
            .unwrap_or_else(help_fix),
        ErrorKind::InvalidValue => invalid(ContextKind::InvalidValue)
            .and_then(|bad| {
                let valid = match err.get(ContextKind::ValidValue) {
                    Some(ContextValue::Strings(valid)) => valid.clone(),
                    _ => return None,
                };
                let arg = invalid(ContextKind::InvalidArg).unwrap_or_default();
                value_fix(argv, &path, &arg, &bad, &valid)
            })
            .unwrap_or_else(help_fix),
        _ => help_fix(),
    };
    // Every suggestion parses, and none is the failing command itself.
    let failing = fix_argv(argv);
    let fails = |s: &Suggestion| s.argv() == Some(failing.as_slice()) || !parses(s);
    let (fix, alternatives) = if fails(&fix) {
        help_fix()
    } else {
        (fix, alternatives)
    };
    let mut error = Error::new(ErrorCode::Usage, message, fix);
    let mut seen = vec![error.fix.clone()];
    for alternative in alternatives.into_iter().filter(|a| !fails(a)) {
        if !seen.contains(&alternative) {
            seen.push(alternative.clone());
            error = error.alternative(alternative);
        }
    }
    error
}

/// A flag's name in clap's rendering of it: `--format <FORMAT>` or `--json=1` is
/// `--format` or `--json`.
fn flag_name(arg: &str) -> &str {
    arg.split(['=', ' ']).next().unwrap_or(arg)
}

/// Whether `flag` (a long flag's name) is about presentation.
fn is_presentation(flag: &str) -> bool {
    // ubs:ignore — comparing flag names, not secrets
    PRESENTATION_FLAGS.iter().any(|(f, _)| *f == flag)
}

/// Whether a suggested command line parses (help and version requests count).
fn parses(suggestion: &Suggestion) -> bool {
    let Some(argv) = suggestion.argv() else {
        return true;
    };
    match Cli::try_parse_from(argv) {
        Ok(_) => true,
        Err(e) => matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion),
    }
}

/// The command `path` names under `root` (`root` itself for an empty path).
fn command_at<'a>(root: &'a clap::Command, path: &[String]) -> &'a clap::Command {
    path.iter()
        .fold(root, |cmd, name| cmd.find_subcommand(name).unwrap_or(cmd))
}

/// `omaddons help <path>`.
fn help_for(path: &[String]) -> Suggestion {
    let mut argv = vec!["omaddons".to_owned(), "help".to_owned()];
    argv.extend(path.iter().cloned());
    Suggestion::command(argv)
}

/// The fix for a mistyped `--actor` value (`bad`): the command with the closest actor in
/// its place, every other argument kept, or `omaddons help <command>` when none is close
/// (a guessed actor would record the change as someone else's).
fn actor_fix(argv: &[String], bad: &str) -> Suggestion {
    let closest = ["agent", "human", "widget"]
        .iter()
        .map(|actor| (levenshtein(bad, actor), *actor))
        .filter(|(d, _)| *d <= 2)
        .min();
    match closest {
        Some((_, actor)) => Suggestion::command(fix_argv(&with_flag_value(argv, "--actor", actor))),
        None => help_for(&command_path(argv)),
    }
}

/// Synonym words a nested level knows (vision §5.10), as an exact intent: `targets
/// remove` is `targets rm`, `targets ls` and `config show` are the bare listing.
fn synonym(argv: &[String], path: &[String], bad: &str) -> Option<Suggestion> {
    let level: Vec<&str> = path.iter().map(String::as_str).collect();
    match (level.as_slice(), bad) {
        (["targets"], "remove" | "delete" | "uninstall") => {
            fix_argv_replacing(argv, bad, "rm").map(Suggestion::command)
        }
        (["targets"] | ["config"], "ls" | "list" | "show") => {
            let at = argv.iter().skip(1).position(|a| a == bad)? + 1;
            let mut without = argv.to_vec();
            without.remove(at);
            Some(Suggestion::command(fix_argv(&without)))
        }
        _ => None,
    }
}

/// The fix for the unknown subcommand `bad` of the command `path` names: a synonym that
/// level knows, else its closest subcommands (names and visible aliases, distance ≤ 2).
/// A command that only reads is suggested as the corrected argv, one that changes
/// something as its help, so a guess never hands over a different mutating command;
/// the third value names that command for the message.
fn subcommand_fix(
    argv: &[String],
    path: &[String],
    bad: &str,
) -> Option<(Suggestion, Vec<Suggestion>, Option<String>)> {
    if let Some(fix) = synonym(argv, path, bad) {
        return Some((fix, Vec::new(), None));
    }
    let mut root = Cli::command();
    root.build();
    let parent = command_at(&root, path);
    let mut close: Vec<(usize, String)> = parent
        .get_subcommands()
        .flat_map(|c| {
            std::iter::once(c.get_name())
                .chain(c.get_visible_aliases())
                .map(move |spelling| (levenshtein(bad, spelling), c.get_name().to_owned()))
        })
        .filter(|(d, _)| *d <= 2)
        .collect();
    close.sort();
    let mut canonical: Vec<String> = Vec::new();
    for (_, name) in close {
        if !canonical.contains(&name) {
            canonical.push(name);
        }
    }
    let guess = |name: &str| {
        let mut full = path.to_vec();
        full.push(name.to_owned());
        if crate::app::mutates(&full.join(" ")) {
            (help_for(&full), Some(full.join(" ")))
        } else {
            let fixed = fix_argv_replacing(argv, bad, name).unwrap_or_else(|| fix_argv(argv));
            (Suggestion::command(fixed), None)
        }
    };
    let mut guesses = canonical.iter().map(|name| guess(name));
    let (fix, closest) = guesses.next()?;
    let alternatives = guesses.map(|(s, _)| s).collect();
    Some((fix, alternatives, closest))
}

/// The fix for `bad`, a value outside `valid` given to `arg` under `path`: the closest
/// valid value (distance ≤ 2) in its place. The verb is the user's own, so the value is
/// applied, except `auth`'s ACTION, itself a verb: `set` and `clear` are suggested as
/// `auth`'s help, and `show` means `status`.
fn value_fix(
    argv: &[String],
    path: &[String],
    arg: &str,
    bad: &str,
    valid: &[String],
) -> Option<(Suggestion, Vec<Suggestion>)> {
    // ubs:ignore — comparing command names, not secrets
    let action = path.first().is_some_and(|p| p == "auth") && arg.contains("ACTION");
    let mut close: Vec<(usize, &String)> = valid
        .iter()
        .map(|v| (levenshtein(bad, v), v))
        .filter(|(d, _)| *d <= 2)
        .collect();
    close.sort();
    let chosen: Vec<&str> = if action && bad == "show" {
        vec!["status"]
    } else {
        close.iter().map(|(_, v)| v.as_str()).collect()
    };
    let replaced = |with: &str| {
        if action && matches!(with, "set" | "clear") {
            return help_for(path);
        }
        let at = argv.iter().skip(1).position(|a| {
            a == bad
                || a.split_once('=')
                    .is_some_and(|(f, v)| f.starts_with('-') && v == bad)
        });
        let mut fixed = argv.to_vec();
        let position = at.map(|i| i + 1);
        if let Some(slot) = position.and_then(|i| fixed.get_mut(i)) {
            *slot = match slot.split_once('=') {
                Some((flag, _)) if slot.starts_with('-') => format!("{flag}={with}"),
                _ => with.to_owned(),
            };
        }
        Suggestion::command(fix_argv_keeping(&fixed, position))
    };
    let mut suggestions = chosen.iter().map(|v| replaced(v));
    let fix = suggestions.next()?;
    Some((fix, suggestions.collect()))
}

/// `omaddons reinstall X …` as `omaddons install X … --reinstall`, every other argument
/// kept.
fn reinstall_fix(argv: &[String]) -> Suggestion {
    let position = argv
        .iter()
        .skip(1)
        .position(|a| a == "reinstall")
        .map(|i| i + 1);
    let mut argv = argv.to_vec();
    if let Some(slot) = position.and_then(|i| argv.get_mut(i)) {
        "install".clone_into(slot);
    }
    let mut fixed = fix_argv_keeping(&argv, position);
    fixed.push("--reinstall".to_owned());
    Suggestion::command(fixed)
}

/// The fix replacing `bad` in argv with its closest candidate, plus the other close
/// candidates as alternatives; `None` when nothing is within distance 2. A
/// `--flag=value` keeps its value.
pub fn suggest(
    argv: &[String],
    bad: &str,
    candidates: &[String],
) -> Option<(Suggestion, Vec<Suggestion>)> {
    let mut close: Vec<(usize, &String)> = candidates
        .iter()
        .map(|c| (levenshtein(bad, c), c))
        .filter(|(d, _)| *d <= 2)
        .collect();
    close.sort();
    let (_, best) = close.first()?;
    let position = argv
        .iter()
        .skip(1)
        // ubs:ignore — comparing flag names, not secrets
        .position(|a| a == bad || a.split_once('=').is_some_and(|(flag, _)| flag == bad))
        .map(|i| i + 1);
    let replaced = |with: &str| {
        let mut argv = argv.to_vec();
        if let Some(slot) = position.and_then(|i| argv.get_mut(i)) {
            *slot = match slot.split_once('=') {
                Some((_, value)) if slot.starts_with('-') => format!("{with}={value}"),
                _ => with.to_owned(),
            };
        }
        Suggestion::command(fix_argv_keeping(&argv, position))
    };
    let alternatives = close.iter().skip(1).map(|(_, c)| replaced(c)).collect();
    Some((replaced(best), alternatives))
}

/// Whether `flag` (as written) takes the next argument as its value in `cmd`.
fn takes_value(cmd: &clap::Command, flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    cmd.get_arguments()
        .filter(|a| a.get_action().takes_values() && !a.is_positional())
        .any(|a| match flag.strip_prefix("--") {
            Some(long) => a
                .get_long_and_visible_aliases()
                .is_some_and(|names| names.contains(&long)),
            // `-t` or a cluster ending in one, such as `-vt`.
            None => flag.len() >= 2 && flag.chars().last() == a.get_short(),
        })
}

/// The subcommand path `argv` names (`["targets", "add"]`): each positional argument
/// that is a subcommand of the one before, skipping flag values; nothing after `--`.
pub fn command_path(argv: &[String]) -> Vec<String> {
    let mut root = Cli::command();
    root.build();
    let mut cmd = &root;
    let mut path = Vec::new();
    let mut args = argv.iter().skip(1);
    while let Some(arg) = args.next() {
        // ubs:ignore — comparing command-line words, not secrets
        if arg == "--" {
            break;
        }
        // ubs:ignore — comparing command-line words, not secrets
        if arg.starts_with('-') && arg != "-" {
            if takes_value(cmd, arg) {
                args.next();
            }
            continue;
        }
        match cmd.find_subcommand(arg) {
            Some(sub) => {
                path.push(sub.get_name().to_owned());
                cmd = sub;
            }
            None => break,
        }
    }
    path
}

/// `-h` or `--help` appears before any `--`.
fn wants_help(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|a| *a != "--")
        .any(|a| a == "-h" || a == "--help")
}

/// The same command online: `argv` without `--offline`, when that flag alone kept the
/// run offline (`OMADDONS_OFFLINE=1` would keep the rerun offline too).
pub fn online_rerun(argv: &[String], offline_by_env: bool) -> Option<Vec<String>> {
    (!offline_by_env && argv.iter().any(|a| a == "--offline"))
        .then(|| fix_argv(&without_switch(argv, "--offline")))
}

/// `argv` without presentation flags.
pub fn fix_argv(argv: &[String]) -> Vec<String> {
    fix_argv_keeping(argv, None)
}

/// `argv` (without presentation flags) with its first `old` argument after the program
/// name replaced by `new`, every other argument kept; `None` when no argument is `old`.
pub fn fix_argv_replacing(argv: &[String], old: &str, new: &str) -> Option<Vec<String>> {
    let position = argv.iter().skip(1).position(|a| a == old)? + 1;
    let mut argv = argv.to_vec();
    if let Some(slot) = argv.get_mut(position) {
        new.clone_into(slot);
    }
    Some(fix_argv_keeping(&argv, Some(position)))
}

/// `argv` without presentation flags, except the one at `keep` (a corrected token).
fn fix_argv_keeping(argv: &[String], keep: Option<usize>) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_value = false;
    for (i, arg) in argv.iter().enumerate() {
        // A presentation flag's value, unless it is a flag itself (clap refuses that as
        // the value too).
        if std::mem::take(&mut skip_value) && !arg.starts_with('-') {
            continue;
        }
        if Some(i) == keep {
            out.push(arg.clone());
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        // ubs:ignore — comparing flag names, not secrets
        match PRESENTATION_FLAGS.iter().find(|(flag, _)| *flag == name) {
            Some((_, takes_value)) => skip_value = *takes_value && !arg.contains('='),
            None => out.push(arg.clone()),
        }
    }
    out
}

/// `argv` (without presentation flags) with its `update` (or `upgrade`) subcommand
/// replaced by `install`.
fn as_install(argv: &[String]) -> Vec<String> {
    let mut argv = fix_argv(argv);
    if let Some(slot) = argv
        .iter_mut()
        .skip(1)
        .find(|a| *a == "update" || *a == "upgrade")
    {
        "install".clone_into(slot);
    }
    argv
}

/// `argv` without `flag` (and its value).
pub(crate) fn without_flag(argv: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in argv {
        if std::mem::take(&mut skip) {
            continue;
        }
        // ubs:ignore — comparing flag names, not secrets
        if arg == flag {
            skip = true;
        } else if !arg.starts_with(&format!("{flag}=")) {
            out.push(arg.clone());
        }
    }
    out
}

/// `argv` (without presentation flags) without its target: `-t`, `--target` and
/// `--flavor`, each with its value. `None` when a short cluster may hold one (`-vt`).
pub(crate) fn without_target(argv: &[String]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = argv.iter();
    while let Some(arg) = rest.next() {
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            // Positionals from here on.
            "--" => {
                out.push(arg.clone());
                out.extend(rest.by_ref().cloned());
            }
            "-t" | "--target" | "--flavor" => {
                if !arg.contains('=') {
                    rest.next();
                }
            }
            // The value attached (`-tretail`).
            _ if arg.starts_with("-t") => {}
            _ if !arg.starts_with("--") && arg.starts_with('-') && arg.contains('t') => {
                return None;
            }
            _ => out.push(arg.clone()),
        }
    }
    Some(fix_argv(&out))
}

/// `argv` without `switch`, a flag that takes no value (`--all`): the word after it is
/// kept.
pub(crate) fn without_switch(argv: &[String], switch: &str) -> Vec<String> {
    argv.iter().filter(|a| *a != switch).cloned().collect()
}

/// `argv` with `flag`'s value replaced by `value`.
fn with_flag_value(argv: &[String], flag: &str, value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut replace_next = false;
    for arg in argv {
        if std::mem::take(&mut replace_next) {
            out.push(value.to_owned());
        // ubs:ignore — comparing flag names, not secrets
        } else if arg == flag {
            out.push(arg.clone());
            replace_next = true;
        } else if arg.starts_with(&format!("{flag}=")) {
            out.push(format!("{flag}={value}"));
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// Edit distance over chars.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(usize::from(ca != *cb));
            let insert = current
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let delete = previous
                .get(j + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            current.push(substitute.min(insert).min(delete));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// The panic hook.

struct PanicState {
    output: OutputConfig,
    command: &'static str,
    /// For the fix, which must not rerun what failed.
    argv: Vec<String>,
    started: Instant,
    offline: bool,
    /// The run's warnings and hints, once its `Context` exists.
    notes: output::RunNotes,
}

static PANIC_STATE: Mutex<Option<PanicState>> = Mutex::new(None);
/// The first panic's message and place.
static PANIC_INFO: Mutex<Option<String>> = Mutex::new(None);

fn set_panic_state(
    output: OutputConfig,
    command: &'static str,
    argv: &[String],
    started: Instant,
    offline: bool,
) {
    if let Ok(mut state) = PANIC_STATE.lock() {
        *state = Some(PanicState {
            output,
            command,
            argv: argv.to_vec(),
            started,
            offline,
            notes: output::RunNotes::default(),
        });
    }
}

fn set_panic_notes(notes: &output::RunNotes) {
    if let Ok(mut state) = PANIC_STATE.lock()
        && let Some(state) = state.as_mut()
    {
        state.notes = notes.clone();
    }
}

/// On a panic: its message and place recorded, nothing written. A panic the program
/// recovers from (a tolerated reader thread) leaves no trace; one that reaches the top
/// is rendered there, once ([`render_panic`]).
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_owned());
        let at = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        first_panic(&PANIC_INFO, format!("internal error: {payload}{at}"));
    }));
}

/// Keeps `message` unless a panic is recorded already: a scoped thread's panic panics
/// again in its parent, more vaguely, and must not replace the cause.
fn first_panic(slot: &Mutex<Option<String>>, message: String) {
    let mut first = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    first.get_or_insert(message);
}

/// A panic that reached the top: one `internal` envelope with the run's warnings and
/// hints (the `undo` of a transaction that committed before it), mirrored on stderr.
/// When a document already went out, it is the contract: an `info` line on stderr, and
/// that document's exit status.
fn render_panic() -> Exit {
    let message = PANIC_INFO
        .lock()
        .ok()
        .and_then(|first| first.clone())
        .unwrap_or_else(|| "internal error: panic".to_owned());
    let state = PANIC_STATE.lock().ok().and_then(|state| {
        state.as_ref().map(|s| {
            (
                s.output,
                s.command,
                s.argv.clone(),
                s.started,
                s.offline,
                s.notes.clone(),
            )
        })
    });
    let (cfg, command, argv, started, offline, notes) = state.unwrap_or_else(|| {
        (
            OutputConfig::machine(),
            "omaddons",
            Vec::new(),
            Instant::now(),
            false,
            output::RunNotes::default(),
        )
    });
    let mut error = Error::internal(message);
    error.fix = crate::error::internal_fix_for(command, &argv);
    let mut reporter = Reporter::stderr(&cfg);
    if output::stdout_written() {
        let line = format!("{} after the result was written", error.message);
        reporter.log(output::LogLevel::Info, &line);
        return output::emitted_exit().unwrap_or(Exit::Failed);
    }
    output::emit_panic(
        &cfg,
        &mut reporter,
        command,
        started,
        offline,
        &notes,
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_panic_is_the_one_reported() {
        let slot = Mutex::new(None);
        first_panic(&slot, "internal error: the cause at src/x.rs:1".to_owned());
        first_panic(&slot, "internal error: a scoped thread panicked".to_owned());
        assert_eq!(
            slot.lock().unwrap().as_deref(),
            Some("internal error: the cause at src/x.rs:1")
        );
    }

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn global(args: &[&str]) -> GlobalArgs {
        let raw: Vec<String> = argv(args);
        Cli::try_parse_from(&raw).unwrap().global
    }

    #[test]
    fn levenshtein_distances() {
        for (a, b, d) in [
            ("hepl", "help", 2),
            ("help", "help", 0),
            ("--jsno", "--json", 2),
            ("stauts", "status", 2),
            ("", "abc", 3),
            ("instal", "install", 1),
            ("é", "e", 1),
        ] {
            assert_eq!(levenshtein(a, b), d, "{a} {b}");
        }
    }

    #[test]
    fn unknown_subcommands_and_flags_suggest_the_closest() {
        let raw = argv(&["hepl", "--json"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help"]);

        // The closest flag is a presentation one: the command without it.
        let raw = argv(&["--jsno"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons"]);

        let raw = argv(&["frobnicate"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "help"],
            "nothing close: help"
        );
        assert!(!err.message.contains('\u{1b}'));
        assert!(!err.message.starts_with("error:"), "{}", err.message);

        let raw = argv(&["help", "--frobnicate"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help", "help"]);
    }

    /// A command that is only its subcommands names them, not its about line.
    #[test]
    fn a_missing_subcommand_names_the_subcommands() {
        let raw = argv(&["omarchy", "--json"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.message, "omaddons omarchy needs a subcommand: install");
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help", "omarchy"]);
    }

    #[test]
    fn a_usage_error_never_suggests_itself() {
        // A root-only flag under a command: the program's version, else that help.
        for flag in ["--version", "-V"] {
            let raw = argv(&["status", flag]);
            let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
            assert_eq!(err.fix.argv().unwrap(), ["omaddons", "--version"], "{flag}");
            assert_eq!(
                err.alternatives[0].argv().unwrap(),
                ["omaddons", "help", "status"]
            );
        }
        // Every command with each of a spread of bad tokens: no fix is the argv itself.
        let commands: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_owned())
            .chain([String::new()])
            .collect();
        let tokens = [
            "--version",
            "-V",
            "--jsno",
            "--dry-rn",
            "--frobnicate",
            "--target",
            "--json=1",
            "stray",
        ];
        let mut checked = 0;
        for command in &commands {
            for token in tokens {
                let args: Vec<&str> = [command.as_str(), token, "--json"]
                    .into_iter()
                    .filter(|a| !a.is_empty())
                    .collect();
                let raw = argv(&args);
                let Err(e) = Cli::try_parse_from(&raw) else {
                    continue;
                };
                let err = usage_error(&e, &raw);
                let own = fix_argv(&raw);
                // A refused presentation flag's fix is the argv without it, by design,
                // when that parses (else help).
                if is_presentation(flag_name(token)) {
                    let fix = err.fix.argv().unwrap();
                    if parses(&Suggestion::command(own.clone())) {
                        assert_eq!(fix, own.as_slice(), "{args:?}");
                    } else {
                        assert_eq!(fix.get(1).map(String::as_str), Some("help"), "{args:?}");
                    }
                    checked += 1;
                    continue;
                }
                assert_ne!(err.fix.argv(), Some(own.as_slice()), "{args:?}");
                for alternative in &err.alternatives {
                    assert_ne!(alternative.argv(), Some(own.as_slice()), "{args:?}");
                }
                checked += 1;
            }
        }
        assert!(checked > 100, "{checked}");
    }

    #[test]
    fn a_usage_error_renders_as_its_argv_asks() {
        let tty = TermEnv {
            stdout_tty: true,
            stderr_tty: true,
            ..TermEnv::default()
        };
        let raw = |args: &[&str]| -> Vec<OsString> {
            argv(args).into_iter().map(OsString::from).collect()
        };
        let colored = |cfg: OutputConfig| cfg.style.color || cfg.stderr_color;
        assert!(colored(fallback_output(&raw(&["nosuch"]), &tty)));
        assert!(!colored(fallback_output(
            &raw(&["nosuch", "--no-color"]),
            &tty
        )));
        let no_color = TermEnv {
            no_color: Some("1".into()),
            ..tty.clone()
        };
        assert!(!colored(fallback_output(&raw(&["nosuch"]), &no_color)));
        let quiet = fallback_output(&raw(&["nosuch", "--quiet", "--json"]), &tty);
        assert_eq!(quiet.output, output::OutputMode::Json);
        assert!(quiet.style.quiet);
        let refused = fallback_output(&raw(&["status", "--json=1"]), &tty);
        assert_eq!(refused.output, output::OutputMode::Json);
    }

    /// A presentation flag clap refuses, or a misspelling of one, is dropped from the
    /// fix: the same command, which the caller reruns with its own flags. Values are
    /// read ignoring case.
    #[test]
    fn a_refused_presentation_flag_is_dropped_from_the_fix() {
        for args in [
            &["status", "--json=1"][..],
            &["status", "--json=true"],
            &["status", "--robot=1"],
            &["--robot=yes", "status"],
            &["status", "--format", "--json"],
            &["status", "--progress", "--json"],
            &["status", "--format", "xml"],
            &["status", "--jsno"],
        ] {
            let raw = argv(args);
            let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
            assert_eq!(
                err.fix.argv().unwrap(),
                ["omaddons", "status"],
                "{args:?}: {}",
                err.message
            );
            assert!(err.alternatives.is_empty(), "{args:?}");
        }
        let raw = argv(&["--jsno"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons"]);
        for args in [
            &["status", "--format", "JSON"][..],
            &["status", "--progress", "JSON"],
        ] {
            assert!(Cli::try_parse_from(argv(args)).is_ok(), "{args:?}");
        }
        // A missing presentation value never swallows the flag after it.
        assert_eq!(
            fix_argv(&argv(&["status", "--format", "--offline"])),
            argv(&["status", "--offline"])
        );
    }

    #[test]
    fn reinstall_is_install_with_reinstall_not_a_typo() {
        let raw = argv(&["reinstall", "Plater", "-t", "retail", "--json"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(
            err.fix.argv().unwrap(),
            argv(&["install", "Plater", "-t", "retail", "--reinstall"])
        );
        assert!(err.alternatives.is_empty(), "no Levenshtein alternatives");
        let raw = argv(&["-v", "reinstall", "Foo"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(
            err.fix.argv().unwrap(),
            argv(&["-v", "install", "Foo", "--reinstall"])
        );
    }

    /// The usage error's fix and alternatives for `args`, as argv lists.
    fn fixes(args: &[&str]) -> (Vec<String>, Vec<Vec<String>>, String) {
        let raw = argv(args);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        let alternatives = err
            .alternatives
            .iter()
            .map(|a| a.argv().unwrap().to_vec())
            .collect();
        (
            err.fix.argv().unwrap().to_vec(),
            alternatives,
            err.message.clone(),
        )
    }

    #[test]
    fn fixes_come_from_the_level_that_failed_and_never_guess_a_mutating_verb() {
        for (args, fix) in [
            // A typo of a command that changes something: its help, never the command.
            (&["plan"][..], &["help", "pin"][..]),
            (&["unpn", "x"], &["help", "unpin"]),
            (&["instal", "dbm"], &["help", "install"]),
            // Of one that only reads: the corrected command.
            (&["statsu"], &["status"]),
            (&["hepl"], &["help"]),
            // Nested levels suggest among their own subcommands.
            (&["targets", "rn", "a", "b"], &["help", "targets", "rm"]),
            (&["config", "st", "k", "v"], &["help", "config", "set"]),
            (&["targets", "ad", "/x"], &["help", "targets", "add"]),
            (&["config", "gte", "k"], &["config", "get", "k"]),
            // Synonyms a nested level knows: the intent is exact.
            (&["targets", "remove", "x"], &["targets", "rm", "x"]),
            (&["targets", "ls"], &["targets"]),
            (&["config", "show"], &["config"]),
            // Flags of the deepest command, applied: the verb is the user's own.
            (
                &["targets", "add", "/x", "--nme", "foo"],
                &["targets", "add", "/x", "--name", "foo"],
            ),
            (&["targets", "--rescn"], &["targets", "--rescan"]),
            // Values.
            (
                &["install", "x", "--channel", "betaa"],
                &["install", "x", "--channel", "beta"],
            ),
            (
                &["install", "x", "--channel=betaa"],
                &["install", "x", "--channel=beta"],
            ),
            (&["auth", "gihub"], &["auth", "github"]),
            (&["auth", "github", "st"], &["help", "auth"]),
            (&["auth", "github", "show"], &["auth", "github", "status"]),
        ] {
            let (got, alternatives, message) = fixes(args);
            assert_eq!(got, argv(fix), "{args:?}: {message}");
            for alternative in &alternatives {
                let parsed = Cli::try_parse_from(alternative);
                assert!(parsed.is_ok(), "{args:?}: {alternative:?} does not parse");
            }
        }
        // No alternative runs another mutating command; the message names the guess.
        let (_, alternatives, message) = fixes(&["unpn", "x"]);
        assert!(
            alternatives
                .iter()
                .all(|a| a.get(1).map(String::as_str) == Some("help")),
            "{alternatives:?}"
        );
        assert!(
            message.ends_with("the closest command is unpin"),
            "{message}"
        );
    }

    #[test]
    fn verb_aliases_run_their_command() {
        for (alias, canonical) in [
            ("add", "install"),
            ("uninstall", "remove"),
            ("rm", "remove"),
            ("delete", "remove"),
            ("ls", "list"),
            ("show", "info"),
            ("log", "history"),
            ("revert", "undo"),
            ("rollback", "undo"),
        ] {
            let raw = argv(&[alias, "x"]);
            let raw = if matches!(alias, "ls" | "log" | "revert" | "rollback") {
                argv(&[alias])
            } else {
                raw
            };
            assert_eq!(command_path(&raw), [canonical], "{alias}");
            let cli = Cli::try_parse_from(&raw).unwrap();
            assert_eq!(
                crate::app::command_name(cli.command.as_ref()),
                canonical,
                "{alias}"
            );
        }
    }

    /// Every subcommand at every level, misspelled three ways: the fix parses, and a
    /// guessed command (another path than the one meant) never changes anything.
    #[test]
    fn every_misspelled_subcommand_gets_a_fix_that_parses_and_never_mutates_by_guess() {
        let mut root = Cli::command();
        root.build();
        let mut paths: Vec<Vec<String>> = Vec::new();
        for sub in root.get_subcommands() {
            paths.push(vec![sub.get_name().to_owned()]);
            for nested in sub.get_subcommands() {
                paths.push(vec![
                    sub.get_name().to_owned(),
                    nested.get_name().to_owned(),
                ]);
            }
        }
        let mut checked = 0;
        for path in &paths {
            let Some((word, parent)) = path.split_last() else {
                continue;
            };
            let chars: Vec<char> = word.chars().collect();
            let mut typos = vec![chars[1..].iter().collect::<String>()];
            typos.push(
                std::iter::once('q')
                    .chain(chars[1..].iter().copied())
                    .collect(),
            );
            if chars.len() > 2 {
                let mut swapped = chars.clone();
                swapped.swap(0, 1);
                typos.push(swapped.into_iter().collect());
            }
            for typo in typos {
                let mut args: Vec<&str> = parent.iter().map(String::as_str).collect();
                args.push(&typo);
                let raw = argv(&args);
                let Err(e) = Cli::try_parse_from(&raw) else {
                    continue;
                };
                let err = usage_error(&e, &raw);
                for suggestion in std::iter::once(&err.fix).chain(&err.alternatives) {
                    let s = suggestion.argv().unwrap();
                    assert!(parses(suggestion), "{args:?}: {s:?}");
                    if s.get(1).map(String::as_str) != Some("help") {
                        let guessed = command_path(s).join(" ");
                        assert!(
                            !crate::app::mutates(&guessed) || guessed == path.join(" "),
                            "{args:?}: {s:?} runs {guessed}"
                        );
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 50, "{checked}");
    }

    #[test]
    fn fixes_drop_presentation_flags() {
        let raw = argv(&[
            "install",
            "x",
            "--json",
            "--format",
            "human",
            "--progress=json",
            "-t",
            "retail",
            "--no-color",
        ]);
        assert_eq!(fix_argv(&raw), argv(&["install", "x", "-t", "retail"]));
        assert_eq!(
            without_flag(&argv(&["--actor", "robot", "help"]), "--actor"),
            argv(&["help"])
        );
        // A switch takes no value: the name after it stays.
        assert_eq!(
            without_switch(&argv(&["update", "--all", "weakauras", "dbm"]), "--all"),
            argv(&["update", "weakauras", "dbm"])
        );
        assert_eq!(
            with_flag_value(&argv(&["--reason=long"]), "--reason", "cut"),
            argv(&["--reason=cut"])
        );
        // Every spelling of the target goes with its value; the rest stays.
        let copy = argv(&["copy", "--from", "tbc", "--to", "retail", "dbm", "--json"]);
        for given in [
            &["-t", "era"][..],
            &["-tera"],
            &["-t=era"],
            &["--target", "era"],
            &["--target=era"],
            &["--flavor", "era", "-t", "x"],
        ] {
            let mut raw = copy.clone();
            raw.splice(2..2, given.iter().map(|s| (*s).to_owned()));
            assert_eq!(
                without_target(&raw),
                Some(argv(&["copy", "--from", "tbc", "--to", "retail", "dbm"])),
                "{given:?}"
            );
        }
        assert_eq!(without_target(&argv(&["copy", "-vt", "era"])), None);
        assert_eq!(
            without_target(&argv(&["copy", "-v", "--", "-t"])),
            Some(argv(&["copy", "-v", "--", "-t"]))
        );
    }

    #[test]
    fn the_command_path_skips_flag_values() {
        for (args, expected) in [
            (&["-t", "retail", "help"][..], &["help"][..]),
            (&["--flavor", "era", "help"][..], &["help"][..]),
            (&["-vt", "retail", "help"][..], &["help"][..]),
            (&["--target=retail", "help"][..], &["help"][..]),
            (&["--json", "-v", "help"][..], &["help"][..]),
            (&["--reason", "help", "x"][..], &[][..]),
            (&["--", "help"][..], &[][..]),
            (&["--json"][..], &[][..]),
            (
                &["targets", "-t", "add", "add", "/x"][..],
                &["targets", "add"][..],
            ),
            (
                &["targets", "rename", "a", "b"][..],
                &["targets", "rename"][..],
            ),
        ] {
            assert_eq!(command_path(&argv(args)), expected, "{args:?}");
        }
        assert_eq!(command_of(&argv(&["-t", "help", "help", "--x"])), "help");
        assert_eq!(command_of(&argv(&["targets", "add"])), "targets add");
        assert_eq!(command_of(&argv(&["hepl"])), "omaddons");
        assert!(wants_help(&argv(&["targets", "add", "-h"])));
        assert!(!wants_help(&argv(&["install", "--", "-h"])));
    }

    #[test]
    fn a_corrected_flag_keeps_its_value() {
        let raw = argv(&["--targte=retail", "help"]);
        let (fix, _) = suggest(&raw, "--targte", &["--target".to_owned()]).unwrap();
        assert_eq!(fix.argv().unwrap(), argv(&["--target=retail", "help"]));
        assert!(suggest(&raw, "--zzzzzz", &["--target".to_owned()]).is_none());
        let raw = argv(&["--progres", "json"]);
        let (fix, _) = suggest(&raw, "--progres", &["--progress".to_owned()]).unwrap();
        assert_eq!(
            fix.argv().unwrap(),
            argv(&["--progress", "json"]),
            "the corrected token survives even when it is a presentation flag"
        );
    }

    #[test]
    fn context_validates_actor_and_reason() {
        let env_none = |_: &str| None;
        let make = |args: &[&str], env: &dyn Fn(&str) -> Option<String>| {
            Context::new(
                &global(args),
                OutputConfig::machine(),
                "status",
                &argv(args),
                env,
                false,
                Instant::now(),
            )
        };
        let ctx = make(
            &["--reason", &"é".repeat(200), "--target", "retail"],
            &env_none,
        )
        .unwrap();
        assert_eq!(
            (ctx.actor, ctx.target.as_deref()),
            (Actor::Agent, Some("retail"))
        );
        assert_eq!(ctx.argv[0], "omaddons");

        let long = "é".repeat(201);
        let err = make(&["--reason", &long], &env_none).unwrap_err();
        assert_eq!((err.code, err.exit().code()), (ErrorCode::Usage, 2));
        let fixed = err.fix.argv().unwrap();
        assert_eq!(fixed.last().unwrap().chars().count(), 200);

        // A mistyped actor is corrected in place; one nothing is close to gets help,
        // never a guess (README "Command reference").
        let err = make(&["--actor", "robot"], &env_none).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "help"]);
        let err = make(&["--actor", "humna", "--target", "retail"], &env_none).unwrap_err();
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "--actor", "human", "--target", "retail"]
        );
        let err = make(&["--actor=agnet"], &env_none).unwrap_err();
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "--actor=agent"]);
        // ubs:ignore — comparing environment variable names, not secrets
        let env_typo = |name: &str| (name == "OMADDONS_ACTOR").then(|| "humna".to_owned());
        let err = make(&[], &env_typo).unwrap_err();
        assert_eq!(err.fix.argv(), None, "the environment's fix stays prose");

        // ubs:ignore — comparing environment variable names, not secrets
        let env_widget = |name: &str| (name == "OMADDONS_ACTOR").then(|| "widget".to_owned());
        assert_eq!(make(&[], &env_widget).unwrap().actor, Actor::Widget);
        // ubs:ignore — comparing environment variable names, not secrets
        let env_offline = |name: &str| (name == "OMADDONS_OFFLINE").then(|| "1".to_owned());
        assert!(make(&[], &env_offline).unwrap().offline);
    }

    #[test]
    fn global_flags_parse_before_and_after_the_subcommand() {
        for args in [
            &["--json", "help"][..],
            &["help", "--json"][..],
            &["help", "help", "-t", "x", "--json"][..],
        ] {
            let raw = argv(args);
            let cli = Cli::try_parse_from(&raw).unwrap();
            assert!(cli.global.json, "{args:?}");
        }
        let raw = argv(&["--flavor", "era", "--target", "retail"]);
        assert_eq!(
            Cli::try_parse_from(&raw).unwrap().global.target.as_deref(),
            Some("retail"),
            "later flags override"
        );
    }
}
