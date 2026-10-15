//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod help;

use crate::cli::{Cli, Command, Context};
use crate::error::Exit;
use crate::output::{self, Reporter};

/// The canonical names of the subcommands this build has. Each command bead adds its
/// own (DEC-1).
const COMMANDS: &[&str] = &["help"];

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
    let result = match &cli.command {
        None => Ok(help::bare()),
        Some(Command::Help { command }) => help::for_command(command.as_deref(), &ctx.argv),
    };
    output::emit(cfg, &mut reporter, name, ctx.started, ctx.offline, result)
}
