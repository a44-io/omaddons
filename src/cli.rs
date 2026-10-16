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
use std::sync::atomic::{AtomicBool, Ordering};
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
    about = "World of Warcraft addon manager (CLI + Omarchy plugin)",
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
    #[arg(long, global = true, value_name = "FORMAT", value_parser = ["json", "human"])]
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
    /// Show the plan without changing anything (mutating commands)
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Who is acting: human, agent or widget [env: OMADDONS_ACTOR]
    #[arg(long, global = true, value_name = "ACTOR")]
    pub actor: Option<String>,
    /// Why, recorded verbatim in history (at most 200 characters)
    #[arg(long, global = true, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Fail any network access [env: OMADDONS_OFFLINE=1]
    #[arg(long, global = true)]
    pub offline: bool,
    /// Allow folder changes while the client runs (never SavedVariables)
    #[arg(long, global = true)]
    pub while_running: bool,
    /// No ANSI color
    #[arg(long, global = true)]
    pub no_color: bool,
    /// No progress output
    #[arg(long, global = true)]
    pub no_progress: bool,
    /// Progress events on stderr
    #[arg(long, global = true, value_name = "MODE", value_parser = ["json", "human", "none"])]
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
        /// The command to explain
        command: Option<String>,
    },
    /// Triage: every client, its addons, running state and what needs attention
    Status(crate::app::status::StatusArgs),
    /// Install or update addons in one target, verified by its own TOC rules
    Install(crate::app::install::InstallArgs),
    /// Remove managed addons from one target (undoable)
    Remove(crate::app::remove::RemoveArgs),
    /// Revert the last change (or one by id), SavedVariables included
    Undo(crate::app::undo::UndoArgs),
    /// Show what changed, when, by whom and why
    History(crate::app::history::HistoryArgs),
    /// Which managed addons have updates, on every client (changes nothing)
    #[command(visible_alias = "check")]
    Outdated(crate::app::outdated::OutdatedArgs),
    /// What an addon is: installed records, catalog entry and newest release
    Info(crate::app::info::InfoArgs),
    /// Every network request omaddons made
    NetLog(crate::app::net_log::NetLogArgs),
    /// List the addons in each target's AddOns folder
    List(crate::app::list::ListArgs),
    /// List the WoW clients, or add, remove, rename and choose them
    Targets(crate::app::targets::TargetsArgs),
    /// Show or change config.toml
    Config(crate::app::config::ConfigArgs),
}

/// What every command receives (DEC-2).
#[derive(Debug, Clone)]
pub struct Context {
    pub output: OutputConfig,
    pub actor: Actor,
    pub reason: Option<String>,
    /// Verbatim (lossy UTF-8), with `argv[0]` normalized to `omaddons`, for history and
    /// for building `fix_argv`s.
    pub argv: Vec<String>,
    pub offline: bool,
    pub dry_run: bool,
    pub while_running: bool,
    /// The raw `-t` value; the registry resolves it.
    pub target: Option<String>,
    pub started: Instant,
    /// Where long commands report progress (nothing when progress is off).
    pub progress: crate::output::ProgressSink,
}

impl Context {
    /// Validates the attribution flags against an injected environment.
    pub fn new(
        global: &GlobalArgs,
        output: OutputConfig,
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
            let fix = if err.origin == "--actor" {
                Suggestion::command(fix_argv(&without_flag(argv, "--actor")))
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
        Ok(Self {
            output,
            actor,
            reason: global.reason.clone(),
            argv: argv.to_vec(),
            offline: global.offline || env("OMADDONS_OFFLINE").as_deref() == Some("1"),
            dry_run: global.dry_run,
            while_running: global.while_running,
            target: global.target.clone(),
            started,
            progress: crate::output::ProgressSink::none(),
        })
    }
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
    .unwrap_or(Exit::Failed);
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
    let early = output::resolve_output_mode_from_argv(raw, term);
    let fallback = OutputConfig {
        output: early,
        ..output::resolve(&OutputFlags::default(), term)
    };
    set_panic_state(fallback, "omaddons", started);
    let cli = match Cli::try_parse_from(raw) {
        Ok(cli) => cli,
        // `-h` on a command with required arguments: help, not a usage error.
        Err(_) if wants_help(&argv) => {
            let path = command_path(&argv).join(" ");
            let name = (!path.is_empty()).then_some(path.as_str());
            let result = crate::app::help::for_command(name, &argv);
            let mut reporter = Reporter::stderr(&fallback);
            return output::emit(&fallback, &mut reporter, "help", started, false, result);
        }
        Err(err) => {
            let error = usage_error(&err, &argv);
            return render_error(&fallback, command_of(&argv), started, error);
        }
    };
    let output = output::resolve(&cli.global.output_flags(), term);
    let command = crate::app::command_name(cli.command.as_ref());
    set_panic_state(output, command, started);
    if let Err(err) = crate::fsx::failpoint::hit("cli.panic") {
        return render_error(&output, command, started, Error::internal(err.to_string()));
    }
    let mut ctx = match Context::new(&cli.global, output, &argv, env, stdin_is_tty, started) {
        Ok(ctx) => ctx,
        Err(error) => return render_error(&output, command, started, error),
    };
    // One reporter for the whole run: the command's progress and the outcome's output.
    let reporter = output::shared_stderr(&output);
    ctx.progress = output::ProgressSink::new(&reporter, &output);
    crate::app::dispatch(&cli, &ctx, &reporter)
}

/// Renders an error that has no outcome (usage errors, a failed failpoint).
fn render_error(cfg: &OutputConfig, command: &'static str, started: Instant, error: Error) -> Exit {
    let mut reporter = Reporter::stderr(cfg);
    output::emit::<crate::app::help::HelpOutcome, _>(
        cfg,
        &mut reporter,
        command,
        started,
        false,
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
    let cmd = Cli::command();
    let path = command_path(argv);
    let subcommand = path.first().cloned();
    let help_fix = || {
        let mut argv = vec!["omaddons".to_owned(), "help".to_owned()];
        argv.extend(path.iter().cloned());
        (Suggestion::command(argv), Vec::new())
    };
    let (fix, alternatives) = match err.kind() {
        ErrorKind::InvalidSubcommand => invalid(ContextKind::InvalidSubcommand)
            .and_then(|bad| {
                let names: Vec<String> = cmd
                    .get_subcommands()
                    .map(|c| c.get_name().to_owned())
                    .collect();
                suggest(argv, &bad, &names)
            })
            .unwrap_or_else(help_fix),
        ErrorKind::UnknownArgument => invalid(ContextKind::InvalidArg)
            .and_then(|bad| {
                let bad_flag = bad.split('=').next().unwrap_or(&bad).to_owned();
                let mut longs: Vec<String> = cmd
                    .get_arguments()
                    .filter_map(|a| a.get_long())
                    .map(|l| format!("--{l}"))
                    .collect();
                if let Some(sub) = subcommand.as_deref().and_then(|s| cmd.find_subcommand(s)) {
                    longs.extend(
                        sub.get_arguments()
                            .filter_map(|a| a.get_long())
                            .map(|l| format!("--{l}")),
                    );
                }
                suggest(argv, &bad_flag, &longs)
            })
            .unwrap_or_else(help_fix),
        _ => help_fix(),
    };
    let mut error = Error::new(ErrorCode::Usage, message, fix);
    for alternative in alternatives {
        error = error.alternative(alternative);
    }
    error
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
        if arg == "--" {
            break;
        }
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

/// `argv` without presentation flags.
pub fn fix_argv(argv: &[String]) -> Vec<String> {
    fix_argv_keeping(argv, None)
}

/// `argv` without presentation flags, except the one at `keep` (a corrected token).
fn fix_argv_keeping(argv: &[String], keep: Option<usize>) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_value = false;
    for (i, arg) in argv.iter().enumerate() {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        if Some(i) == keep {
            out.push(arg.clone());
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        match PRESENTATION_FLAGS.iter().find(|(flag, _)| *flag == name) {
            Some((_, takes_value)) => skip_value = *takes_value && !arg.contains('='),
            None => out.push(arg.clone()),
        }
    }
    out
}

/// `argv` without `flag` (and its value).
fn without_flag(argv: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in argv {
        if std::mem::take(&mut skip) {
            continue;
        }
        if arg == flag {
            skip = true;
        } else if !arg.starts_with(&format!("{flag}=")) {
            out.push(arg.clone());
        }
    }
    out
}

/// `argv` with `flag`'s value replaced by `value`.
fn with_flag_value(argv: &[String], flag: &str, value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut replace_next = false;
    for arg in argv {
        if std::mem::take(&mut replace_next) {
            out.push(value.to_owned());
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
    started: Instant,
}

static PANIC_STATE: Mutex<Option<PanicState>> = Mutex::new(None);
static PANIC_RENDERED: AtomicBool = AtomicBool::new(false);

fn set_panic_state(output: OutputConfig, command: &'static str, started: Instant) {
    if let Ok(mut state) = PANIC_STATE.lock() {
        *state = Some(PanicState {
            output,
            command,
            started,
        });
    }
}

/// On a panic: one `internal` envelope on stdout under JSON (unless a document already
/// went out), the error mirrored to stderr, and nothing else, once per process.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if PANIC_RENDERED.swap(true, Ordering::SeqCst) {
            return;
        }
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
        let error = Error::internal(format!("internal error: {payload}{at}"));
        let (cfg, command, started) =
            match PANIC_STATE.lock().ok().as_deref().and_then(Option::as_ref) {
                Some(state) => (state.output, state.command, state.started),
                None => (OutputConfig::machine(), "omaddons", Instant::now()),
            };
        if output::stdout_written() {
            Reporter::stderr(&cfg).error(&error);
        } else {
            render_error(&cfg, command, started, error);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let raw = argv(&["--jsno"]);
        let err = usage_error(&Cli::try_parse_from(&raw).unwrap_err(), &raw);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons", "--json"]);

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
        assert_eq!(
            with_flag_value(&argv(&["--reason=long"]), "--reason", "cut"),
            argv(&["--reason=cut"])
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

        let err = make(&["--actor", "robot"], &env_none).unwrap_err();
        assert_eq!(err.code, ErrorCode::Usage);
        assert_eq!(err.fix.argv().unwrap(), ["omaddons"]);

        let env_widget = |name: &str| (name == "OMADDONS_ACTOR").then(|| "widget".to_owned());
        assert_eq!(make(&[], &env_widget).unwrap().actor, Actor::Widget);
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
