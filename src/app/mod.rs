//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod config;
pub mod engine;
pub mod help;
pub mod history;
pub mod info;
pub mod install;
pub mod list;
pub mod net_log;
pub mod remove;
pub mod status;
pub mod targets;
pub mod undo;
pub mod world;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{Cli, Command, Context};
use crate::error::Exit;
use crate::output::{self, Reporter};
use targets::TargetsAction;

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

/// A subcommand this build has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// Its canonical name; a nested one is its path (`targets add`).
    pub name: &'static str,
    pub group: Group,
}

const fn entry(name: &'static str, group: Group) -> CommandEntry {
    CommandEntry { name, group }
}

/// Every subcommand this build has, one per line, in display order within its group (a
/// nested one takes its parent's group). Each command bead adds its own line in the
/// commit that adds its clap variant (DEC-1).
pub const COMMANDS: &[CommandEntry] = &[
    entry("status", Group::Look),
    entry("list", Group::Look),
    entry("info", Group::Look),
    entry("history", Group::Look),
    entry("net-log", Group::Look),
    entry("install", Group::Change),
    entry("remove", Group::Change),
    entry("undo", Group::Change),
    entry("targets", Group::Setup),
    entry("targets add", Group::Setup),
    entry("targets rm", Group::Setup),
    entry("targets rename", Group::Setup),
    entry("targets default", Group::Setup),
    entry("config", Group::Setup),
    entry("config get", Group::Setup),
    entry("config set", Group::Setup),
    entry("config path", Group::Setup),
    entry("help", Group::Setup),
];

/// The canonical (static) name for a subcommand name clap reported.
pub fn canonical(name: &str) -> &'static str {
    COMMANDS
        .iter()
        .map(|c| c.name)
        .find(|c| *c == name)
        .unwrap_or("omaddons")
}

/// The group of the subcommand `name`, and its place in `COMMANDS`.
pub fn group_of(name: &str) -> Option<(Group, usize)> {
    COMMANDS
        .iter()
        .enumerate()
        .find(|(_, c)| c.name == name)
        .map(|(at, c)| (c.group, at))
}

/// The envelope `command` of a parsed invocation; bare `omaddons` is help.
pub fn command_name(command: Option<&Command>) -> &'static str {
    match command {
        None | Some(Command::Help { .. }) => "help",
        Some(Command::Status(_)) => "status",
        Some(Command::Install(_)) => "install",
        Some(Command::Remove(_)) => "remove",
        Some(Command::Undo(_)) => "undo",
        Some(Command::History(_)) => "history",
        Some(Command::Info(_)) => "info",
        Some(Command::NetLog(_)) => "net-log",
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
        Some(Command::Status(args)) => {
            let result = status::run(ctx, *args);
            if let Ok(outcome) = &result {
                let d = outcome.discovery;
                reporter.log(
                    output::LogLevel::Debug,
                    &format!(
                        "discovery: scanned {}, roots reused {}, parsed {}, cache written {}",
                        if d.scanned { "yes" } else { "no" },
                        d.reused,
                        d.parsed,
                        if d.written { "yes" } else { "no" }
                    ),
                );
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::Install(args)) => {
            let result = install::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    reporter.log(output::LogLevel::Debug, note);
                }
                if let (Some(ms), Some(tx)) =
                    (outcome.lock_held_ms, output::Outcome::data(outcome).tx)
                {
                    reporter.log(
                        output::LogLevel::Debug,
                        &format!("lock released: tx {tx}, lock_held_ms {ms}"),
                    );
                }
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
        Some(Command::Info(args)) => {
            let result = info::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    reporter.log(output::LogLevel::Debug, note);
                }
            }
            output::emit(cfg, &mut reporter, name, started, offline, result)
        }
        Some(Command::NetLog(args)) => output::emit(
            cfg,
            &mut reporter,
            name,
            started,
            offline,
            net_log::run(ctx, args),
        ),
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
