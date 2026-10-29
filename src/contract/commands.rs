//! The one place the CLI's inputs are extracted (M4-D2): every `COMMANDS` row joined with
//! the clap command at its path in the built tree, so the manifest, `capabilities` and
//! the guide describe the same arguments. Pure: it builds the clap tree and reads no
//! environment.
//!
//! A10 (the manifest) created this module to A4's (`capabilities`) design, because A4 had
//! not landed; A4 adds the app-validated value sets and the declared requirements.

use clap::{ArgAction, CommandFactory};

use crate::app::precondition::BINDING_COMMANDS;
use crate::app::{COMMANDS, CommandEntry, Dictionary, Group, Network, command_entry};
use crate::cli::{Cli, GlobalArgs};

/// One command path this build has, with every fact the descriptions of the CLI publish:
/// clap's (arguments, aliases, about) joined with its `COMMANDS` row's (M4-D2, M4-D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    /// The `COMMANDS` row's name: the envelope's `command`.
    pub path: &'static str,
    /// clap's about (the variant's doc comment), trimmed.
    pub about: String,
    pub group: Group,
    pub mutates: bool,
    pub dictionary: Dictionary,
    pub exits: &'static [u8],
    /// The positionals, in index order.
    pub args: Vec<ArgInfo>,
    /// The command's own flags in declaration order, then the propagated globals in
    /// `GlobalArgs` order.
    pub flags: Vec<FlagInfo>,
    /// clap's visible aliases of the command.
    pub aliases: Vec<String>,
    pub stores_plan: bool,
    /// Whether its runs bind to a preview (`precondition::BINDING_COMMANDS`).
    pub binds: bool,
    pub network: Network,
}

/// A positional argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgInfo {
    /// clap's argument id (`refs`), as `help --json` names it.
    pub id: String,
    /// The first value name, else the id upper-cased (clap's display).
    pub value_name: String,
    pub required: bool,
    /// The arity's maximum exceeds 1.
    pub multiple: bool,
    /// clap's recorded default.
    pub default: Option<String>,
    pub values: Values,
    pub help: String,
    /// The arguments it conflicts with, named by long name or positional id, in both
    /// directions.
    pub conflicts_with: Vec<String>,
    pub requires: Vec<String>,
}

/// A flag: an argument with a long or a short name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagInfo {
    /// Without dashes.
    pub long: String,
    pub short: Option<char>,
    /// Visible long aliases, without dashes (`flavor`).
    pub aliases: Vec<String>,
    /// `None` for a flag that takes no value.
    pub value_name: Option<String>,
    pub takes_value: bool,
    /// `ArgAction::Append` or `Count`, or an arity above 1.
    pub multiple: bool,
    /// `None` for a flag that takes no value.
    pub default: Option<String>,
    pub values: Values,
    pub conflicts_with: Vec<String>,
    pub requires: Vec<String>,
    pub global: bool,
    /// clap's short help, trimmed.
    pub help: String,
}

/// What a value may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Values {
    /// Any text clap accepts (a path, a number, an id).
    Any,
    /// Exactly these words: clap's possible values, hidden ones left out. `String`, not
    /// `&'static str`: clap builds its possible values on demand.
    Words(Vec<String>),
}

/// Every `COMMANDS` row's facts, in `COMMANDS` order. A row whose path the clap tree does
/// not have is left out.
pub fn commands() -> Vec<CommandInfo> {
    let root = built();
    COMMANDS
        .iter()
        .filter_map(|entry| at_path(&root, entry.name).map(|cmd| command_info(entry, cmd)))
        .collect()
}

/// One row's facts; `None` for a path that is no row.
pub fn command(path: &str) -> Option<CommandInfo> {
    let entry = command_entry(path)?;
    let root = built();
    at_path(&root, entry.name).map(|cmd| command_info(entry, cmd))
}

/// The facts of `entry` from the built clap command `cmd` at its path (pure, so unit tests
/// can pass synthetic clap commands).
pub fn command_info(entry: &CommandEntry, cmd: &clap::Command) -> CommandInfo {
    let binds = BINDING_COMMANDS.contains(&entry.name);
    let published: Vec<&clap::Arg> = cmd
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        // clap propagates `--if-match` everywhere; `cli::check_if_match` refuses it on
        // every path that does not bind.
        .filter(|arg| binds || arg.get_long() != Some(IF_MATCH))
        .collect();
    let conflicts = |arg: &clap::Arg| conflicts_of(cmd, &published, arg);

    let mut positionals: Vec<&clap::Arg> = published
        .iter()
        .copied()
        .filter(|arg| arg.is_positional())
        .collect();
    positionals.sort_by_key(|arg| arg.get_index());
    let args = positionals
        .into_iter()
        .map(|arg| ArgInfo {
            id: arg.get_id().to_string(),
            value_name: value_name(arg).unwrap_or_else(|| upper_id(arg)),
            required: arg.is_required_set(),
            multiple: multiple(arg),
            default: default(arg),
            values: values_of(arg),
            help: help(arg),
            conflicts_with: conflicts(arg),
            // `REQUIRES` is keyed by long name: a positional has none.
            requires: Vec::new(),
        })
        .collect();

    // A global is program-wide: one of `GlobalArgs`, propagated. A flag a parent with no
    // row declares global (`auth --from-gh`) is an own flag of each row under it, and an
    // own flag may share a global's id (`install --version`, not the root's `-V`).
    let order = global_order();
    let program_wide =
        |arg: &clap::Arg| arg.is_global_set() && order.iter().any(|id| id == arg.get_id().as_str());
    let (mut globals, own): (Vec<&clap::Arg>, Vec<&clap::Arg>) = published
        .iter()
        .copied()
        .filter(|arg| !arg.is_positional())
        .partition(|arg| program_wide(arg));
    globals.sort_by_key(|arg| {
        order
            .iter()
            .position(|id| id == arg.get_id().as_str())
            .unwrap_or(order.len())
    });
    let flags = own
        .into_iter()
        .chain(globals)
        .map(|arg| {
            let takes_value = arg.get_action().takes_values();
            FlagInfo {
                long: name(arg),
                short: arg.get_short(),
                aliases: arg
                    .get_visible_aliases()
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                value_name: takes_value.then(|| value_name(arg).unwrap_or_else(|| upper_id(arg))),
                takes_value,
                multiple: multiple(arg),
                default: if takes_value { default(arg) } else { None },
                values: values_of(arg),
                conflicts_with: conflicts(arg),
                requires: arg
                    .get_long()
                    .map(|long| requires_of(entry.name, long))
                    .unwrap_or_default(),
                global: program_wide(arg),
                help: help(arg),
            }
        })
        .collect();

    CommandInfo {
        path: entry.name,
        about: cmd
            .get_about()
            .map(|about| about.to_string().trim().to_owned())
            .unwrap_or_default(),
        group: entry.group,
        mutates: entry.mutates,
        dictionary: entry.dictionary,
        exits: entry.exits,
        args,
        flags,
        aliases: cmd.get_visible_aliases().map(str::to_owned).collect(),
        stores_plan: entry.stores_plan,
        binds,
        network: entry.network,
    }
}

/// `--if-match`'s long name.
const IF_MATCH: &str = "if-match";

/// The clap tree as parsing sees it: built, so every subcommand carries the propagated
/// global flags.
fn built() -> clap::Command {
    let mut root = Cli::command();
    root.build();
    root
}

/// The command at `path` (`backup restore`), walked word by word by name.
fn at_path<'a>(root: &'a clap::Command, path: &str) -> Option<&'a clap::Command> {
    path.split(' ').try_fold(root, |cmd, word| {
        cmd.get_subcommands().find(|sub| sub.get_name() == word)
    })
}

/// The global flags' ids in `GlobalArgs` declaration order.
fn global_order() -> Vec<String> {
    <GlobalArgs as clap::Args>::augment_args(clap::Command::default())
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect()
}

/// An argument's published name: a flag's long name, else its id (a positional, or a
/// flag with a short name only).
fn name(arg: &clap::Arg) -> String {
    arg.get_long()
        .map_or_else(|| arg.get_id().to_string(), str::to_owned)
}

fn upper_id(arg: &clap::Arg) -> String {
    arg.get_id().as_str().to_uppercase()
}

fn value_name(arg: &clap::Arg) -> Option<String> {
    arg.get_value_names()
        .and_then(|names| names.first())
        .map(ToString::to_string)
}

fn multiple(arg: &clap::Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Append | ArgAction::Count)
        || arg
            .get_num_args()
            .is_some_and(|range| range.max_values() > 1)
}

/// clap's recorded default, for an argument that takes a value.
fn default(arg: &clap::Arg) -> Option<String> {
    if !arg.get_action().takes_values() {
        return None;
    }
    arg.get_default_values()
        .first()
        .map(|value| value.to_string_lossy().into_owned())
}

/// What a value may be, as capabilities and the manifest both publish it: clap's possible
/// values, hidden ones left out, else any text.
pub(crate) fn values_of(arg: &clap::Arg) -> Values {
    let words: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_owned())
        .collect();
    if words.is_empty() {
        Values::Any
    } else {
        Values::Words(words)
    }
}

fn help(arg: &clap::Arg) -> String {
    arg.get_help()
        .map(|help| help.to_string().trim().to_owned())
        .unwrap_or_default()
}

/// The symmetric closure of clap's conflicts for `arg` among `among`: those `arg` names and
/// those that name `arg`, in argument order. clap enforces a conflict both ways but records
/// it on one side.
fn conflicting<'c>(
    cmd: &clap::Command,
    among: &[&'c clap::Arg],
    arg: &clap::Arg,
) -> Vec<&'c clap::Arg> {
    let names_of = |a: &clap::Arg| -> Vec<String> {
        cmd.get_arg_conflicts_with(a)
            .iter()
            .map(|other| other.get_id().to_string())
            .collect()
    };
    let own = names_of(arg);
    let id = arg.get_id().as_str();
    among
        .iter()
        .copied()
        .filter(|other| other.get_id() != arg.get_id())
        .filter(|other| {
            own.iter().any(|named| named == other.get_id().as_str())
                || names_of(other).iter().any(|named| named == id)
        })
        .collect()
}

/// [`conflicting`], each named by long name or positional id, as capabilities publishes
/// them.
pub(crate) fn conflicts_of(
    cmd: &clap::Command,
    published: &[&clap::Arg],
    arg: &clap::Arg,
) -> Vec<String> {
    conflicting(cmd, published, arg)
        .into_iter()
        .map(name)
        .collect()
}

/// The flags `--<long>` of `path` requires, by long name: the declared [`REQUIRES`].
pub(crate) fn requires_of(path: &str, long: &str) -> Vec<String> {
    REQUIRES
        .iter()
        .filter(|row| row.path == path && row.long == long)
        .flat_map(|row| row.requires.iter().map(|required| (*required).to_owned()))
        .collect()
}

/// A flag clap requires another argument with, declared because clap gives no public
/// getter for requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub path: &'static str,
    pub long: &'static str,
    /// The long names it requires.
    pub requires: &'static [&'static str],
}

/// Every requirement between arguments. Empty today; `doctor fix --force` → `yes` will be
/// the first row. `requirements_are_declared_and_true` holds it equal to clap's behaviour.
pub const REQUIRES: &[Requirement] = &[];

/// Every way `table` disagrees with what `cmd` (the command at `path`) enforces: a flag
/// that needs another argument to parse without a row saying so, and a row whose flag
/// parses alone. Each flag is probed with the command's required positionals sampled (the
/// first possible value, else `1`).
pub fn requirement_violations(
    path: &str,
    cmd: &clap::Command,
    table: &[Requirement],
) -> Vec<String> {
    let sample = |arg: &clap::Arg| {
        arg.get_possible_values()
            .first()
            .map_or_else(|| "1".to_owned(), |value| value.get_name().to_owned())
    };
    let mut required: Vec<&clap::Arg> = cmd
        .get_arguments()
        .filter(|arg| arg.is_positional() && arg.is_required_set())
        .collect();
    required.sort_by_key(|arg| arg.get_index());
    // A required flag is not a requirement: each probe passes every other one too
    // (`copy --from` with `--to`).
    let required_flags: Vec<&clap::Arg> = cmd
        .get_arguments()
        .filter(|arg| !arg.is_positional() && arg.is_required_set() && arg.get_long().is_some())
        .collect();
    let mut violations = Vec::new();
    for flag in cmd.get_arguments().filter(|arg| !arg.is_positional()) {
        let Some(long) = flag.get_long() else {
            continue;
        };
        let mut argv = vec![cmd.get_name().to_owned()];
        argv.extend(required.iter().map(|arg| sample(arg)));
        for other in required_flags
            .iter()
            .filter(|other| other.get_id() != flag.get_id())
        {
            argv.push(format!("--{}", other.get_long().unwrap_or_default()));
            if other.get_action().takes_values() {
                argv.push(sample(other));
            }
        }
        argv.push(format!("--{long}"));
        if flag.get_action().takes_values() {
            argv.push(sample(flag));
        }
        let declared: Vec<&str> = table
            .iter()
            .filter(|row| row.path == path && row.long == long)
            .flat_map(|row| row.requires.iter().copied())
            .collect();
        match cmd.clone().try_get_matches_from(&argv) {
            Ok(_) if !declared.is_empty() => violations.push(format!(
                "`{path} --{long}` parses alone, but REQUIRES says it needs {}",
                declared.join(", ")
            )),
            Err(e) if e.kind() == clap::error::ErrorKind::MissingRequiredArgument => {
                let missing: Vec<String> = match e.get(clap::error::ContextKind::InvalidArg) {
                    Some(clap::error::ContextValue::Strings(names)) => names.clone(),
                    Some(clap::error::ContextValue::String(name)) => vec![name.clone()],
                    _ => Vec::new(),
                };
                for name in missing {
                    let long_name = name
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches('-')
                        .to_owned();
                    if !declared.contains(&long_name.as_str()) {
                        violations.push(format!(
                            "`{path} --{long}` needs `{name}` to parse: declare it in contract::commands::REQUIRES"
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    violations
}

/// Facts the manifest records about one built clap command: any path, the root and pure
/// parents included. Never serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFacts {
    /// Every alias, hidden ones included (they parse).
    pub aliases: Vec<String>,
    pub subcommand_required: bool,
    pub args_conflict_with_subcommands: bool,
}

pub fn command_facts(cmd: &clap::Command) -> CommandFacts {
    let mut aliases: Vec<String> = cmd.get_all_aliases().map(str::to_owned).collect();
    aliases.sort();
    CommandFacts {
        aliases,
        subcommand_required: cmd.is_subcommand_required_set(),
        args_conflict_with_subcommands: cmd.is_args_conflicts_with_subcommands_set(),
    }
}

/// A value grammar, from a closed vocabulary: clap names no parser stably in release
/// builds, so parsers are told apart by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grammar {
    Flag,
    Count,
    Enum,
    DurationOrTime,
    Path,
    Integer,
    String,
    Other,
}

impl Grammar {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Count => "count",
            Self::Enum => "enum",
            Self::DurationOrTime => "duration_or_time",
            Self::Path => "path",
            Self::Integer => "integer",
            Self::String => "string",
            Self::Other => "other",
        }
    }
}

/// Facts the manifest records about one argument of the command at a path: the whole
/// accepted input, hidden values and aliases included. Never serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgFacts {
    /// `--long` (`-c` without a long), or `@<index>` for a positional.
    pub identity: String,
    /// A positional's 1-based index.
    pub index: Option<usize>,
    /// A positional's first value name, else its id upper-cased.
    pub value_name: Option<String>,
    pub short: Option<char>,
    pub short_aliases: Vec<char>,
    pub aliases: Vec<String>,
    pub grammar: Grammar,
    /// Sorted accepted spellings, aliases included.
    pub values: Vec<String>,
    pub ignore_case: bool,
    /// clap's recorded defaults (`false` for a switch and `0` for a count, once built).
    pub default: Vec<String>,
    /// `(min, max)`; `None` when unbounded.
    pub arity: (usize, Option<usize>),
    pub action: &'static str,
    pub repeat: bool,
    pub required: bool,
    pub global: bool,
    /// Identities, both directions.
    pub conflicts: Vec<String>,
    /// Identities.
    pub requires: Vec<String>,
    pub env: Option<String>,
    /// The set ones of: `allow_hyphen_values`, `allow_negative_numbers`,
    /// `trailing_var_arg`, `require_equals`, `exclusive`, `last`.
    pub switches: Vec<&'static str>,
    pub value_delimiter: Option<char>,
}

/// `arg`'s identity: `--long`, `-c`, or `@<index>`.
fn identity(arg: &clap::Arg) -> String {
    match (arg.get_long(), arg.get_short(), arg.get_index()) {
        (Some(long), _, _) => format!("--{long}"),
        (None, Some(short), _) => format!("-{short}"),
        (None, None, Some(index)) => format!("@{index}"),
        (None, None, None) => arg.get_id().to_string(),
    }
}

fn action_name(action: &ArgAction) -> &'static str {
    match action {
        ArgAction::Set => "set",
        ArgAction::Append => "append",
        ArgAction::SetTrue => "set_true",
        ArgAction::SetFalse => "set_false",
        ArgAction::Count => "count",
        ArgAction::Help => "help",
        ArgAction::HelpShort => "help_short",
        ArgAction::HelpLong => "help_long",
        ArgAction::Version => "version",
        _ => "other",
    }
}

fn grammar(arg: &clap::Arg) -> Grammar {
    use std::any::TypeId;
    use std::ffi::OsString;
    use std::path::PathBuf;
    match arg.get_action() {
        ArgAction::SetTrue
        | ArgAction::SetFalse
        | ArgAction::Help
        | ArgAction::HelpShort
        | ArgAction::HelpLong
        | ArgAction::Version => return Grammar::Flag,
        ArgAction::Count => return Grammar::Count,
        _ => {}
    }
    if matches!(values_of(arg), Values::Words(_)) || !arg.get_possible_values().is_empty() {
        return Grammar::Enum;
    }
    let parser = arg.get_value_parser().type_id();
    let is = |id: TypeId| parser == id;
    if is(TypeId::of::<PathBuf>()) || is(TypeId::of::<OsString>()) {
        return Grammar::Path;
    }
    let integers = [
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
        TypeId::of::<i8>(),
        TypeId::of::<i16>(),
        TypeId::of::<i32>(),
        TypeId::of::<i64>(),
        TypeId::of::<isize>(),
    ];
    if integers.into_iter().any(is) {
        Grammar::Integer
    } else if is(TypeId::of::<String>()) {
        Grammar::String
    } else {
        Grammar::Other
    }
}

/// The manifest's facts about `arg` of `cmd`, the command at `path`. Conflicts are taken
/// among every argument of `cmd`.
pub fn arg_facts(path: &str, cmd: &clap::Command, arg: &clap::Arg) -> ArgFacts {
    let all: Vec<&clap::Arg> = cmd.get_arguments().collect();
    let mut conflicts: Vec<String> = conflicting(cmd, &all, arg)
        .into_iter()
        .map(identity)
        .collect();
    conflicts.sort();
    let mut requires: Vec<String> = arg
        .get_long()
        .map(|long| requires_of(path, long))
        .unwrap_or_default()
        .into_iter()
        .map(|long| format!("--{long}"))
        .collect();
    requires.sort();
    let mut values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .flat_map(|value| {
            value
                .get_name_and_aliases()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    values.sort();
    values.dedup();
    let mut aliases: Vec<String> = arg
        .get_all_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(str::to_owned)
        .collect();
    aliases.sort();
    let mut short_aliases = arg.get_all_short_aliases().unwrap_or_default();
    short_aliases.sort_unstable();
    let range = arg.get_num_args();
    let arity = range.map_or((0, Some(0)), |range| {
        let max = range.max_values();
        (range.min_values(), (max != usize::MAX).then_some(max))
    });
    let switches: Vec<&'static str> = [
        ("allow_hyphen_values", arg.is_allow_hyphen_values_set()),
        (
            "allow_negative_numbers",
            arg.is_allow_negative_numbers_set(),
        ),
        ("trailing_var_arg", arg.is_trailing_var_arg_set()),
        ("require_equals", arg.is_require_equals_set()),
        ("exclusive", arg.is_exclusive_set()),
        ("last", arg.is_last_set()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    ArgFacts {
        identity: identity(arg),
        index: if arg.is_positional() {
            arg.get_index()
        } else {
            None
        },
        value_name: arg
            .is_positional()
            .then(|| value_name(arg).unwrap_or_else(|| upper_id(arg))),
        short: arg.get_short(),
        short_aliases,
        aliases,
        grammar: grammar(arg),
        values,
        ignore_case: arg.is_ignore_case_set(),
        default: arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        arity,
        action: action_name(arg.get_action()),
        repeat: matches!(arg.get_action(), ArgAction::Append | ArgAction::Count),
        required: arg.is_required_set(),
        global: arg.is_global_set(),
        conflicts,
        requires,
        env: arg.get_env().map(|env| env.to_string_lossy().into_owned()),
        switches,
        value_delimiter: arg.get_value_delimiter(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Registry;

    fn info(path: &str) -> CommandInfo {
        command(path).unwrap_or_else(|| panic!("{path}: a COMMANDS row")) // ubs:ignore — test lookup
    }

    fn flag<'a>(info: &'a CommandInfo, long: &str) -> Option<&'a FlagInfo> {
        info.flags.iter().find(|flag| flag.long == long)
    }

    #[test]
    fn commands_has_one_entry_per_row_in_order() {
        let infos = commands();
        let got: Vec<&str> = infos.iter().map(|info| info.path).collect();
        let want: Vec<&str> = COMMANDS.iter().map(|row| row.name).collect();
        assert_eq!(got, want, "one CommandInfo per row, in table order");
        for (info, row) in infos.iter().zip(COMMANDS) {
            let got = (
                info.group,
                info.mutates,
                info.dictionary,
                info.exits,
                info.stores_plan,
                info.network,
            );
            let want = (
                row.group,
                row.mutates,
                row.dictionary,
                row.exits,
                row.stores_plan,
                row.network,
            );
            assert_eq!(got, want, "{}: the row's facts", row.name);
        }
    }

    #[test]
    fn binds_is_binding_commands_membership() {
        for info in commands() {
            assert_eq!(
                info.binds,
                BINDING_COMMANDS.contains(&info.path),
                "{}: binds",
                info.path
            );
        }
    }

    #[test]
    fn if_match_is_listed_only_where_it_binds() {
        for info in commands() {
            assert_eq!(
                flag(&info, IF_MATCH).is_some(),
                info.binds,
                "{}: --if-match listed exactly when it binds",
                info.path
            );
        }
        assert!(flag(&info("remove"), IF_MATCH).is_some(), "remove binds");
        assert!(flag(&info("status"), IF_MATCH).is_none(), "status does not");
    }

    #[test]
    fn conflicts_are_the_symmetric_closure() {
        let install = info("install");
        for (long, want) in [
            ("any-flavor", "no-any-flavor"),
            ("no-any-flavor", "any-flavor"),
        ] {
            let got = flag(&install, long).map(|f| f.conflicts_with.clone());
            assert_eq!(got, Some(vec![want.to_owned()]), "install --{long}");
        }
        let undo = info("undo");
        let tx = undo.args.iter().find(|arg| arg.id == "tx");
        assert_eq!(
            tx.map(|arg| arg.conflicts_with.clone()),
            Some(vec!["last-by".to_owned(), "run".to_owned()]),
            "undo's tx"
        );
        let last_by = flag(&undo, "last-by").map(|f| f.conflicts_with.clone());
        assert_eq!(
            last_by,
            Some(vec!["tx".to_owned(), "run".to_owned()]),
            "undo --last-by"
        );
    }

    #[test]
    fn root_version_and_hidden_arguments_are_left_out() {
        for info in commands() {
            let root_version = info.flags.iter().find(|f| f.short == Some('V'));
            assert_eq!(root_version, None, "{}: the root's -V/--version", info.path);
        }
        let own_version = flag(&info("install"), "version").map(|f| (f.global, f.takes_value));
        assert_eq!(
            own_version,
            Some((false, true)),
            "install --version is its own flag"
        );
        let from_gh = flag(&info("auth github"), "from-gh").map(|f| f.global);
        assert_eq!(
            from_gh,
            Some(false),
            "a parent's global is the row's own flag"
        );
        let entry = command_entry("status").unwrap();
        let cmd = clap::Command::new("status")
            .arg(clap::Arg::new("shown").long("shown"))
            .arg(clap::Arg::new("secret").long("secret").hide(true));
        let longs: Vec<String> = command_info(entry, &cmd)
            .flags
            .into_iter()
            .map(|f| f.long)
            .collect();
        assert_eq!(longs, ["shown"], "a hidden argument is left out");
    }

    #[test]
    fn globals_follow_own_flags() {
        let order = global_order();
        let globals: Vec<&str> = order
            .iter()
            .map(String::as_str)
            .filter(|id| *id != "version")
            .collect();
        for info in commands() {
            let split = info
                .flags
                .iter()
                .position(|f| f.global)
                .unwrap_or(info.flags.len());
            assert!(
                info.flags[split..].iter().all(|f| f.global),
                "{}: no own flag after a global",
                info.path
            );
            let got: Vec<String> = info.flags[split..]
                .iter()
                .map(|f| f.long.replace('-', "_"))
                .collect();
            let want: Vec<&str> = globals
                .iter()
                .copied()
                .filter(|id| info.binds || *id != "if_match")
                .collect();
            assert_eq!(got, want, "{}: the globals in GlobalArgs order", info.path);
        }
        let install = info("install");
        assert!(
            install.flags.first().is_some_and(|f| !f.global),
            "install's own flags come first"
        );
    }

    #[test]
    fn flag_facts_follow_the_walk_rules() {
        let install = info("install");
        let json = flag(&install, "json").unwrap();
        assert_eq!(
            (
                json.takes_value,
                json.value_name.as_deref(),
                json.default.as_deref(),
                json.multiple
            ),
            (false, None, None, false),
            "a switch takes no value and has no default"
        );
        let verbose = flag(&install, "verbose").unwrap();
        assert_eq!(
            (verbose.short, verbose.multiple),
            (Some('v'), true),
            "-v counts"
        );
        let format = flag(&install, "format").unwrap();
        assert_eq!(
            (format.value_name.as_deref(), format.values.clone()),
            (
                Some("FORMAT"),
                Values::Words(vec!["json".to_owned(), "human".to_owned()])
            ),
            "--format's value set"
        );
        let target = flag(&install, "target").unwrap();
        assert_eq!(target.aliases, ["flavor"], "--target's visible alias");
        assert_eq!(install.aliases, ["add"], "install's alias");
        let refs = install.args.first().unwrap();
        assert_eq!(
            (refs.id.as_str(), refs.multiple),
            ("refs", true),
            "install's positional"
        );
        assert!(
            !install.about.is_empty() && !refs.help.is_empty(),
            "about and help are read"
        );
    }

    #[test]
    fn no_argument_has_several_defaults() {
        let root = built();
        for row in COMMANDS {
            let cmd = at_path(&root, row.name).unwrap();
            for arg in cmd.get_arguments() {
                assert!(
                    arg.get_default_values().len() <= 1,
                    "{} {}: one default at most",
                    row.name,
                    arg.get_id()
                );
            }
        }
    }

    #[test]
    fn no_flag_has_a_visible_short_alias_or_lacks_a_long_name() {
        let root = built();
        for row in COMMANDS {
            let cmd = at_path(&root, row.name).unwrap();
            for arg in cmd.get_arguments().filter(|arg| !arg.is_positional()) {
                assert!(
                    arg.get_visible_short_aliases().is_none(),
                    "{} {}: a visible short alias",
                    row.name,
                    arg.get_id()
                );
                assert!(
                    arg.get_long().is_some(),
                    "{} {}: no long name",
                    row.name,
                    arg.get_id()
                );
            }
        }
    }

    #[test]
    fn the_clap_tree_passes_clap_debug_asserts() {
        Cli::command().debug_assert();
    }

    #[test]
    fn registry_builtin_holds_commands() {
        assert_eq!(Registry::builtin().commands, commands());
    }

    #[test]
    fn requirements_are_declared_and_true() {
        let root = built();
        let violations: Vec<String> = COMMANDS
            .iter()
            .filter_map(|row| {
                at_path(&root, row.name).map(|cmd| requirement_violations(row.name, cmd, REQUIRES))
            })
            .flatten()
            .collect();
        assert_eq!(violations, Vec::<String>::new());
    }

    #[test]
    fn requirement_violations_finds_each_fault() {
        let switch =
            |name: &'static str| clap::Arg::new(name).long(name).action(ArgAction::SetTrue);
        let requiring = clap::Command::new("x")
            .arg(switch("force").requires("yes"))
            .arg(switch("yes"));
        let free = clap::Command::new("x")
            .arg(switch("force"))
            .arg(switch("yes"));
        let declared = [Requirement {
            path: "x",
            long: "force",
            requires: &["yes"],
        }];
        let cases: [(&str, &clap::Command, &[Requirement], usize); 3] = [
            ("undeclared", &requiring, &[], 1),
            ("declared_not_enforced", &free, &declared, 1),
            ("correct", &requiring, &declared, 0),
        ];
        for (case, cmd, table, want) in cases {
            let got = requirement_violations("x", cmd, table);
            assert_eq!(got.len(), want, "{case}: {got:?}");
        }
        let message = requirement_violations("x", &requiring, &[]).join("");
        assert!(
            message.contains("`x --force` needs") && message.contains("REQUIRES"),
            "{message}"
        );
    }

    #[test]
    fn manifest_requires_equal_the_requires_table() {
        let nodes = crate::contract::manifest::build(&Registry::builtin()).nodes;
        let with_requires: Vec<&String> = nodes
            .iter()
            .filter(|(_, node)| {
                node.facts
                    .contains_key(crate::contract::manifest::fact::REQUIRES)
            })
            .map(|(id, _)| id)
            .collect();
        let declared: Vec<String> = REQUIRES
            .iter()
            .map(|row| crate::contract::manifest::ids::flag(row.path, row.long))
            .collect();
        assert_eq!(
            with_requires,
            declared.iter().collect::<Vec<_>>(),
            "every REQUIRES row, and no other flag"
        );
    }

    /// Lines of `text`'s production code that set a default-missing value.
    fn default_missing(text: &str) -> Vec<usize> {
        use crate::source_scan::{Literals, production_code};
        production_code(text, Literals::Blank)
            .unwrap_or_default()
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("default_missing_value"))
            .map(|(n, _)| n + 1)
            .collect()
    }

    #[test]
    fn no_argument_sets_a_default_missing_value() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let found: Vec<String> = crate::source_scan::sources(&root)
            .into_iter()
            .flat_map(|(path, text)| {
                default_missing(&text)
                    .into_iter()
                    .map(move |n| format!("{}:{n}", path.display()))
            })
            .collect();
        assert!(
            found.is_empty(),
            "clap keeps default-missing values private, so the manifest cannot record them: declare them beside the argument in a contract::commands table first: {found:?}"
        );
    }

    #[test]
    fn default_missing_scan_catches_planted_text() {
        let cases = [
            (
                "attribute",
                "#[arg(long, default_missing_value = \"x\")]\nstruct A;\n",
                1,
            ),
            (
                "plural",
                "#[arg(long, default_missing_values = [\"x\"])]\nstruct A;\n",
                1,
            ),
            (
                "builder",
                "fn f(a: Arg) -> Arg { a.default_missing_value(\"x\") }\n",
                1,
            ),
            (
                "commented",
                "// default_missing_value = \"x\"\nfn f() {}\n",
                0,
            ),
            (
                "string_literal",
                "fn f() -> &'static str { \"default_missing_value\" }\n",
                0,
            ),
        ];
        for (case, text, want) in cases {
            let got = default_missing(text).len();
            assert_eq!(
                got, want,
                "{case}: {text:?} gave {got} lines, expected {want}"
            );
        }
    }
}
