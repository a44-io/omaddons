//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod config;
pub mod engine;
pub mod help;
pub mod history;
pub mod install;
pub mod list;
pub mod remove;
pub mod targets;
pub mod undo;
pub mod world;

use crate::cli::{Cli, Command, Context};
use crate::error::Exit;
use crate::output::{self, Reporter};
use targets::TargetsAction;

/// The canonical names of the subcommands this build has. Each command bead adds its
/// own (DEC-1).
const COMMANDS: &[&str] = &[
    "help",
    "install",
    "remove",
    "undo",
    "history",
    "list",
    "targets",
    "targets add",
    "targets rm",
    "targets rename",
    "targets default",
    "config",
    "config get",
    "config set",
    "config path",
];

/// The canonical (static) name for a subcommand name clap reported.
pub fn canonical(name: &str) -> &'static str {
    COMMANDS
        .iter()
        .copied()
        .find(|c| *c == name)
        .unwrap_or("omaddons")
}

/// The envelope `command` of a parsed invocation; bare `omaddons` is help.
pub fn command_name(command: Option<&Command>) -> &'static str {
    match command {
        None | Some(Command::Help { .. }) => "help",
        Some(Command::Install(_)) => "install",
        Some(Command::Remove(_)) => "remove",
        Some(Command::Undo(_)) => "undo",
        Some(Command::History(_)) => "history",
        Some(Command::List(_)) => "list",
        Some(Command::Targets(args)) => args
            .action
            .as_ref()
            .map_or("targets", TargetsAction::command),
        Some(Command::Config(args)) => args
            .action
            .as_ref()
            .map_or("config", config::ConfigAction::command),
    }
}

/// Runs the parsed command and renders its outcome.
pub fn dispatch(cli: &Cli, ctx: &Context) -> Exit {
    let cfg = &ctx.output;
    let mut reporter = Reporter::stderr(cfg);
    let name = command_name(cli.command.as_ref());
    if cli.global.version {
        return output::emit(
            cfg,
            &mut reporter,
            "version",
            ctx.started,
            ctx.offline,
            Ok(help::version()),
        );
    }
    if cli.global.help {
        let result = match &cli.command {
            None => Ok(help::bare()),
            Some(_) => help::for_command(Some(name), &ctx.argv),
        };
        return output::emit(cfg, &mut reporter, "help", ctx.started, ctx.offline, result);
    }
    let (started, offline) = (ctx.started, ctx.offline);
    match &cli.command {
        None => output::emit(cfg, &mut reporter, name, started, offline, Ok(help::bare())),
        Some(Command::Help { command }) => {
            let result = help::for_command(command.as_deref(), &ctx.argv);
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::Install(args)) => {
            let result = install::run(ctx, args);
            if let Ok(outcome) = &result
                && let (Some(ms), Some(tx)) =
                    (outcome.lock_held_ms, output::Outcome::data(outcome).tx)
            {
                reporter.log(
                    output::LogLevel::Debug,
                    &format!("lock released: tx {tx}, lock_held_ms {ms}"),
                );
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::Remove(args)) => {
            let result = remove::run(ctx, args);
            if let Ok(outcome) = &result
                && let (Some(ms), Some(tx)) =
                    (outcome.lock_held_ms, output::Outcome::data(outcome).tx)
            {
                reporter.log(
                    output::LogLevel::Debug,
                    &format!("lock released: tx {tx}, lock_held_ms {ms}"),
                );
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::Undo(args)) => {
            let result = undo::run(ctx, args);
            if let Ok(outcome) = &result
                && let (Some(ms), Some(tx)) =
                    (outcome.lock_held_ms, output::Outcome::data(outcome).tx)
            {
                reporter.log(
                    output::LogLevel::Debug,
                    &format!("lock released: tx {tx}, lock_held_ms {ms}"),
                );
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::History(args)) => {
            let result = history::run(ctx, args, cli.global.actor.as_deref());
            if result.as_ref().is_ok_and(|o| o.torn_tail) {
                reporter.log(
                    output::LogLevel::Info,
                    "history.jsonl ends with an incomplete line (an interrupted write); it was skipped",
                );
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::List(args)) => output::emit(
            cfg,
            &mut reporter,
            name,
            started,
            offline,
            list::run(ctx, *args),
        ),
        Some(Command::Targets(args)) => {
            let r = &mut reporter;
            match &args.action {
                None => output::emit(
                    cfg,
                    r,
                    name,
                    started,
                    offline,
                    targets::list(ctx, args.rescan),
                ),
                Some(TargetsAction::Add { path, name: id }) => output::emit(
                    cfg,
                    r,
                    name,
                    started,
                    offline,
                    targets::add(ctx, path, id.as_deref()),
                ),
                Some(TargetsAction::Rm { id }) => {
                    output::emit(cfg, r, name, started, offline, targets::rm(ctx, id))
                }
                Some(TargetsAction::Rename { id, new }) => output::emit(
                    cfg,
                    r,
                    name,
                    started,
                    offline,
                    targets::rename(ctx, id, new),
                ),
                Some(TargetsAction::Default { id }) => {
                    output::emit(cfg, r, name, started, offline, targets::default(ctx, id))
                }
            }
        }
        Some(Command::Config(args)) => {
            use config::ConfigAction;
            let r = &mut reporter;
            match &args.action {
                None => output::emit(cfg, r, name, started, offline, config::list()),
                Some(ConfigAction::Get { key }) => {
                    output::emit(cfg, r, name, started, offline, config::get(key))
                }
                Some(ConfigAction::Set { key, value }) => {
                    output::emit(cfg, r, name, started, offline, config::set(ctx, key, value))
                }
                Some(ConfigAction::Path) => {
                    output::emit(cfg, r, name, started, offline, config::path())
                }
            }
        }
    }
}
