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
            values: values(arg),
            help: help(arg),
            conflicts_with: conflicts(arg),
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
                values: values(arg),
                conflicts_with: conflicts(arg),
                requires: Vec::new(),
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

fn values(arg: &clap::Arg) -> Values {
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

/// The symmetric closure of clap's conflicts for `arg` among the published arguments:
/// those `arg` names and those that name `arg`, in argument order. clap enforces a
/// conflict both ways but records it on one side.
fn conflicts_of(cmd: &clap::Command, published: &[&clap::Arg], arg: &clap::Arg) -> Vec<String> {
    let names_of = |a: &clap::Arg| -> Vec<String> {
        cmd.get_arg_conflicts_with(a)
            .iter()
            .map(|other| other.get_id().to_string())
            .collect()
    };
    let own = names_of(arg);
    let id = arg.get_id().as_str();
    published
        .iter()
        .filter(|other| other.get_id() != arg.get_id())
        .filter(|other| {
            own.iter().any(|named| named == other.get_id().as_str())
                || names_of(other).iter().any(|named| named == id)
        })
        .map(|other| name(other))
        .collect()
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
}
