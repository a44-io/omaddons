//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod adopt;
pub mod auth;
pub mod config;
pub mod copy;
pub mod drift;
pub mod engine;
pub mod export;
pub mod help;
pub mod history;
pub mod info;
pub mod install;
pub mod list;
pub mod net_log;
pub mod outcome;
pub mod outdated;
pub mod pin;
pub mod remove;
pub mod resolve_args;
pub mod search;
pub mod status;
pub mod sync;
pub mod targets;
pub mod undo;
pub mod update;
pub mod world;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{Cli, Command, Context};
use crate::error::Exit;
use crate::output::{self, SharedReporter};
use targets::TargetsAction;

/// Records a settings change of `kind` (about `target`, for a `targets` change) in
/// history, in the session `s` of the command that made it, under the global lock it
/// holds (vision §5.9 Attribution): who, why and the argv. A line that cannot be written
/// is `history_not_recorded`, never the command's failure.
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

/// A subcommand this build has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// Its canonical name; a nested one is its path (`targets add`).
    pub name: &'static str,
    pub group: Group,
    /// Whether some form of it changes state (conservative: `export` writes with
    /// `--write`, `auth` with `set`). A usage error never guesses one for you.
    pub mutates: bool,
}

const fn entry(name: &'static str, group: Group, mutates: bool) -> CommandEntry {
    CommandEntry {
        name,
        group,
        mutates,
    }
}

/// Every subcommand this build has, one per line, in display order within its group (a
/// nested one takes its parent's group). Each command bead adds its own line in the
/// commit that adds its clap variant (DEC-1).
pub const COMMANDS: &[CommandEntry] = &[
    entry("status", Group::Look, false),
    entry("list", Group::Look, false),
    entry("outdated", Group::Look, false),
    entry("info", Group::Look, false),
    entry("search", Group::Look, false),
    entry("export", Group::Look, true),
    entry("history", Group::Look, false),
    entry("net-log", Group::Look, false),
    entry("install", Group::Change, true),
    entry("update", Group::Change, true),
    entry("pin", Group::Change, true),
    entry("unpin", Group::Change, true),
    entry("adopt", Group::Change, true),
    entry("sync", Group::Change, true),
    entry("copy", Group::Change, true),
    entry("remove", Group::Change, true),
    entry("undo", Group::Change, true),
    entry("targets", Group::Setup, false),
    entry("targets add", Group::Setup, true),
    entry("targets rm", Group::Setup, true),
    entry("targets rename", Group::Setup, true),
    entry("targets default", Group::Setup, true),
    entry("config", Group::Setup, false),
    entry("config get", Group::Setup, false),
    entry("config set", Group::Setup, true),
    entry("config path", Group::Setup, false),
    entry("auth", Group::Setup, true),
    entry("help", Group::Setup, false),
];

/// Whether the command at `path` (`targets add`) may change state; one this build does
/// not list counts as changing it.
pub fn mutates(path: &str) -> bool {
    COMMANDS
        .iter()
        .find(|c| c.name == path)
        .is_none_or(|c| c.mutates)
}

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
        Some(Command::Update(_)) => "update",
        Some(Command::Pin(_)) => "pin",
        Some(Command::Unpin(_)) => "unpin",
        Some(Command::Adopt(_)) => "adopt",
        Some(Command::Sync(_)) => "sync",
        Some(Command::Copy(_)) => "copy",
        Some(Command::Remove(_)) => "remove",
        Some(Command::Undo(_)) => "undo",
        Some(Command::History(_)) => "history",
        Some(Command::Info(_)) => "info",
        Some(Command::Export(_)) => "export",
        Some(Command::Outdated(_)) => "outdated",
        Some(Command::Search(_)) => "search",
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
        Some(Command::Auth(_)) => "auth github",
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
    /// For an internal error's fix, which must not rerun what failed.
    argv: &'a [String],
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
            if matches!(e.code, crate::error::ErrorCode::Internal)
                && e.fix == crate::error::internal_fix()
            {
                e.fix = crate::error::internal_fix_for(name, self.argv);
            }
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

/// `pin`'s and `unpin`'s `-v` lines, then the outcome.
fn out_pin(out: &Out<'_>, result: crate::error::Result<pin::PinOutcome>) -> Exit {
    if let Ok(outcome) = &result {
        for note in &outcome.notes {
            out.log(output::LogLevel::Debug, note);
        }
        for (tx, ms) in &outcome.locks {
            out.lock_released(Some(*ms), Some(*tx));
        }
    }
    out.emit(result)
}

/// Runs the parsed command and renders its outcome through `reporter`, the run's one
/// stderr reporter (also behind `ctx.progress`).
pub fn dispatch(cli: &Cli, ctx: &Context, reporter: &SharedReporter) -> Exit {
    let name = command_name(cli.command.as_ref());
    let out = Out {
        cfg: &ctx.output,
        reporter,
        name,
        started: ctx.started,
        offline: ctx.offline,
        notes: &ctx.notes,
        argv: &ctx.argv,
    };
    if cli.global.version {
        return out.emit_as("version", Ok(help::version()));
    }
    if cli.global.help {
        let result = match &cli.command {
            None => Ok(help::bare()),
            Some(_) => help::for_command(Some(name), &ctx.argv),
        };
        return out.emit_as("help", result);
    }
    match &cli.command {
        None => out.emit(Ok(help::bare())),
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
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
            }
            out.emit(result)
        }
        Some(Command::Install(args)) => {
            let result = install::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
                out.lock_released(outcome.lock_held_ms, output::Outcome::data(outcome).tx);
            }
            out.emit(result)
        }
        Some(Command::Update(args)) => {
            let result = update::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
                for (tx, ms) in &outcome.locks {
                    out.lock_released(Some(*ms), Some(*tx));
                }
            }
            out.emit(result)
        }
        Some(Command::Pin(args)) => out_pin(&out, pin::pin(ctx, args)),
        Some(Command::Unpin(args)) => out_pin(&out, pin::unpin(ctx, args)),
        Some(Command::Sync(args)) => {
            let result = sync::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
                for (tx, ms) in &outcome.locks {
                    out.lock_released(Some(*ms), Some(*tx));
                }
            }
            out.emit(result)
        }
        Some(Command::Copy(args)) => {
            let result = copy::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
                for (tx, ms) in &outcome.locks {
                    out.lock_released(Some(*ms), Some(*tx));
                }
            }
            out.emit(result)
        }
        Some(Command::Remove(args)) => {
            let result = remove::run(ctx, args);
            if let Ok(outcome) = &result {
                out.lock_released(outcome.lock_held_ms, output::Outcome::data(outcome).tx);
            }
            out.emit(result)
        }
        Some(Command::Undo(args)) => {
            let result = undo::run(ctx, args);
            if let Ok(outcome) = &result {
                for (tx, ms) in &outcome.locks {
                    out.lock_released(Some(*ms), Some(*tx));
                }
            }
            out.emit(result)
        }
        Some(Command::Info(args)) => {
            let result = info::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
            }
            out.emit(result)
        }
        Some(Command::Export(args)) => {
            let result = export::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
            }
            out.emit(result)
        }
        Some(Command::Adopt(args)) => {
            let result = adopt::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
                for (tx, ms) in &outcome.locks {
                    out.lock_released(Some(*ms), Some(*tx));
                }
            }
            out.emit(result)
        }
        Some(Command::Outdated(args)) => {
            let result = outdated::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
            }
            out.emit(result)
        }
        Some(Command::Search(args)) => {
            let result = search::run(ctx, args);
            if let Ok(outcome) = &result {
                for note in &outcome.notes {
                    out.log(output::LogLevel::Debug, note);
                }
            }
            out.emit(result)
        }
        Some(Command::NetLog(args)) => out.emit(net_log::run(ctx, args)),
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
        Some(Command::List(args)) => out.emit(list::run(ctx, *args)),
        Some(Command::Targets(args)) => match &args.action {
            None => out.emit(targets::list(ctx, args.rescan)),
            Some(TargetsAction::Add { path, name: id }) => {
                out.emit(targets::add(ctx, path, id.as_deref()))
            }
            Some(TargetsAction::Rm { id }) => out.emit(targets::rm(ctx, id)),
            Some(TargetsAction::Rename { id, new }) => out.emit(targets::rename(ctx, id, new)),
            Some(TargetsAction::Default { id }) => out.emit(targets::default(ctx, id)),
        },
        Some(Command::Auth(args)) => out.emit(auth::run(ctx, args)),
        Some(Command::Config(args)) => {
            use config::ConfigAction;
            match &args.action {
                None => out.emit(config::list()),
                Some(ConfigAction::Get { key }) => out.emit(config::get(key)),
                Some(ConfigAction::Set { key, value }) => out.emit(config::set(ctx, key, value)),
                Some(ConfigAction::Path) => out.emit(config::path()),
            }
        }
    }
}
