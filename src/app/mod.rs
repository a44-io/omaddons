//! One module per command. Each command is a function that returns a typed outcome;
//! rendering happens in `output`.

pub mod adopt;
pub mod auth;
pub mod backup;
pub mod backup_verify;
pub mod backups_status;
pub mod config;
pub mod copy;
pub mod drift;
pub mod engine;
pub mod errors;
pub mod errors_status;
pub mod export;
pub mod help;
pub mod history;
pub mod info;
pub mod install;
pub mod issues;
pub mod list;
pub mod net_log;
pub mod omarchy;
pub mod outcome;
pub mod outdated;
pub mod pin;
pub mod precondition;
pub mod remembered;
pub mod remove;
pub mod resolve_args;
pub mod restore_addons;
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

/// A subcommand this build has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// Its canonical name; a nested one is its path (`targets add`).
    pub name: &'static str,
    pub group: Group,
    /// Whether some form of it changes state (conservative: `export` writes with
    /// `--write`, `auth github` with `set`). A usage error never guesses one for you.
    pub mutates: bool,
}

const fn entry(name: &'static str, group: Group, mutates: bool) -> CommandEntry {
    CommandEntry {
        name,
        group,
        mutates,
    }
}

/// Every runnable command path this build has, one per line, in display order within its
/// group (a nested one takes its parent's group): every clap leaf, and every parent whose
/// bare form runs (`backup`, `config`, `errors`, `omarchy`, `targets`). A parent that
/// requires a subcommand (`auth`) has no line. Each command bead adds its own line in the
/// commit that adds its clap variant (DEC-1).
pub const COMMANDS: &[CommandEntry] = &[
    entry("status", Group::Look, false),
    entry("list", Group::Look, false),
    entry("outdated", Group::Look, false),
    entry("info", Group::Look, false),
    entry("search", Group::Look, false),
    entry("errors", Group::Look, false),
    entry("errors dismiss", Group::Look, true),
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
    // Bare `backup` is `backup list`, bare `omarchy` `omarchy status`: reads.
    entry("backup", Group::Change, false),
    entry("backup list", Group::Change, false),
    entry("backup create", Group::Change, true),
    entry("backup restore", Group::Change, true),
    entry("backup prune", Group::Change, true),
    entry("backup export", Group::Change, true),
    entry("backup import", Group::Change, true),
    entry("backup verify", Group::Change, true),
    entry("targets", Group::Setup, false),
    entry("targets add", Group::Setup, true),
    entry("targets rm", Group::Setup, true),
    entry("targets rename", Group::Setup, true),
    entry("targets default", Group::Setup, true),
    entry("config", Group::Setup, false),
    entry("config get", Group::Setup, false),
    entry("config set", Group::Setup, true),
    entry("config path", Group::Setup, false),
    entry("auth github", Group::Setup, true),
    entry("omarchy", Group::Setup, false),
    entry("omarchy status", Group::Setup, false),
    entry("omarchy install", Group::Setup, true),
    entry("omarchy remove", Group::Setup, true),
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
        Command::Adopt(_) => "adopt",
        Command::Sync(_) => "sync",
        Command::Copy(_) => "copy",
        Command::Remove(_) => "remove",
        Command::Undo(_) => "undo",
        Command::Backup(args) => args
            .action
            .as_ref()
            .map_or("backup list", backup::BackupAction::command),
        Command::History(_) => "history",
        Command::Info(_) => "info",
        Command::Export(_) => "export",
        Command::Outdated(_) => "outdated",
        Command::Search(_) => "search",
        Command::Errors(args) => match args.action {
            Some(errors::ErrorsAction::Dismiss(_)) => "errors dismiss",
            None => "errors",
        },
        Command::NetLog(_) => "net-log",
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

    /// The fixes only the command's boundary knows: an internal error's never reruns
    /// what failed; a retryable `plan_stale` and a `locked` rerun the same command; an
    /// `offline` one, when `--offline` alone made the run offline, reruns it without.
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
        offline_by_env: ctx.offline_by_env,
    };
    if cli.global.version {
        return out.emit_as("version", Ok(help::version()));
    }
    if cli.global.help {
        // The command as typed, as `help <path>` reads it: `backup --help` is backup's
        // help, though its envelope name is the default action's (`backup list`).
        let result = match &cli.command {
            None => Ok(help::bare()),
            Some(_) => {
                let path = crate::cli::command_path(&ctx.argv).join(" ");
                help::for_command(Some(&path), &ctx.argv)
            }
        };
        return out.emit_as("help", result);
    }
    let exit = run_command(cli, ctx, &out);
    // A command that may have appended history (by its full path: `config set`, not
    // `config`) rebuilds the history projection it could not keep current under its
    // lock, now that the lock is released (M3-D37).
    let path = crate::cli::command_path(&ctx.argv);
    if !path.is_empty()
        && mutates(&path.join(" "))
        && !ctx.dry_run
        && let Ok(paths) = crate::state::paths::Paths::from_env()
    {
        crate::state::history_tail::rebuild_if_stale(&paths, name, ctx.actor);
    }
    exit
}

/// Runs the parsed command and renders its outcome through `out`.
fn run_command(cli: &Cli, ctx: &Context, out: &Out<'_>) -> Exit {
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
        Some(Command::Pin(args)) => out_pin(out, pin::pin(ctx, args)),
        Some(Command::Unpin(args)) => out_pin(out, pin::unpin(ctx, args)),
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
        Some(Command::Backup(args)) => {
            use backup::BackupAction;
            match &args.action {
                None => out.emit(backup::list(ctx, &backup::BackupListArgs::default())),
                Some(BackupAction::List(list)) => out.emit(backup::list(ctx, list)),
                Some(BackupAction::Create(create)) => out.emit(backup::create(ctx, create)),
                Some(BackupAction::Restore(restore)) => {
                    let result = backup::restore_backup(ctx, restore);
                    if let Ok(outcome) = &result {
                        out.lock_released(outcome.lock_held_ms, output::Outcome::data(outcome).tx);
                    }
                    out.emit(result)
                }
                Some(BackupAction::Prune(prune)) => out.emit(backup::prune_backups(ctx, prune)),
                Some(BackupAction::Export(export)) => out.emit(backup::export_backup(ctx, export)),
                Some(BackupAction::Import(import)) => out.emit(backup::import_backup(ctx, import)),
                Some(BackupAction::Verify(verify)) => out.emit(backup_verify::run(ctx, verify)),
            }
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
        Some(Command::Errors(args)) => match &args.action {
            Some(errors::ErrorsAction::Dismiss(_)) => out.emit(errors::dismiss(ctx)),
            None => {
                let result = errors::run(ctx, args);
                if let Ok(outcome) = &result {
                    for note in &outcome.notes {
                        out.log(output::LogLevel::Debug, note);
                    }
                }
                out.emit(result)
            }
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
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::Cli;

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
}
