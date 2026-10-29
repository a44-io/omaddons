//! The manifest's inputs (M4-D56): the CLI itself, not the shape of a description of it.
//! Every built clap path (the root and pure parents included) with its aliases and row
//! facts; every flag and positional with its default, grammar, arity, repeatability,
//! conflicts and requirements; and the `config.toml` keys with their types. Accepting
//! less, or a changed default, is what the classifier calls breaking.

use clap::CommandFactory;

use super::ids::{self, section};
use super::{Builder, Fact, NodeKind, fact};
use crate::app::precondition::BINDING_COMMANDS;
use crate::app::{Dictionary, ENVELOPE_ONLY, Network};
use crate::cli::Cli;
use crate::contract::Registry;
use crate::contract::commands::{self, ArgFacts, CommandInfo};
use crate::state::config::KEYS;

/// The root's path, as its envelope-only name.
const ROOT: &str = "omaddons";

pub(crate) fn collect(b: &mut Builder, registry: &Registry) {
    let mut root = Cli::command();
    root.build();
    walk(b, registry, ROOT, &root, None);
    // An envelope-only name with no clap path of its own (`version`).
    for only in ENVELOPE_ONLY.iter().filter(|only| only.name != ROOT) {
        command_node(b, registry, only.name, None);
    }
    for key in KEYS {
        let facts = [(
            fact::CONFIG_TYPE,
            Some(Fact::Text(key.kind.name().to_owned())),
        )];
        b.insert(
            ids::registry(section::CONFIG, key.key),
            NodeKind::ConfigKey,
            facts,
        );
    }
}

/// The command at `path` and everything under it. An argument it inherits from `parent`
/// (a propagated global) is recorded once, where it is declared.
fn walk(
    b: &mut Builder,
    registry: &Registry,
    path: &str,
    cmd: &clap::Command,
    parent: Option<&clap::Command>,
) {
    command_node(b, registry, path, Some(cmd));
    for arg in cmd.get_arguments() {
        let inherited = arg.is_global_set()
            && parent.is_some_and(|p| {
                p.get_arguments()
                    .any(|a| a.get_id() == arg.get_id() && a.is_global_set())
            });
        if !inherited {
            arg_node(b, path, &commands::arg_facts(path, cmd, arg));
        }
    }
    for sub in cmd.get_subcommands() {
        let child = if path == ROOT {
            sub.get_name().to_owned()
        } else {
            format!("{path} {}", sub.get_name())
        };
        walk(b, registry, &child, sub, Some(cmd));
    }
}

fn row<'r>(registry: &'r Registry, path: &str) -> Option<&'r CommandInfo> {
    registry.commands.iter().find(|info| info.path == path)
}

fn command_node(b: &mut Builder, registry: &Registry, path: &str, cmd: Option<&clap::Command>) {
    let facts = cmd.map(commands::command_facts);
    let info = row(registry, path);
    let envelope_only = ENVELOPE_ONLY.iter().find(|only| only.name == path);
    let exits = info
        .map(|info| info.exits)
        .or(envelope_only.map(|only| only.exits));
    let flag = |set: bool| set.then_some(Fact::Bool(true));
    let entries = [
        (
            fact::ALIASES,
            facts.as_ref().and_then(|f| Fact::list(f.aliases.clone())),
        ),
        (
            fact::SUBCOMMAND_REQUIRED,
            facts.as_ref().and_then(|f| flag(f.subcommand_required)),
        ),
        (
            fact::ARGS_CONFLICT_WITH_SUBCOMMANDS,
            facts
                .as_ref()
                .and_then(|f| flag(f.args_conflict_with_subcommands)),
        ),
        (
            fact::EXITS,
            exits.and_then(|exits| Fact::ints(exits.iter().map(|&e| i64::from(e)))),
        ),
        (
            fact::DICTIONARY,
            info.map(|info| Fact::Text(dictionary(info.dictionary).to_owned())),
        ),
        (fact::MUTATES, info.map(|info| Fact::Bool(info.mutates))),
        (
            fact::STORES_PLAN,
            info.map(|info| Fact::Bool(info.stores_plan)),
        ),
        (
            fact::NETWORK,
            info.map(|info| Fact::Text(network(info.network).to_owned())),
        ),
        (
            fact::BINDS,
            Some(Fact::Bool(BINDING_COMMANDS.contains(&path))),
        ),
        (fact::ENVELOPE_ONLY, flag(envelope_only.is_some())),
    ];
    b.insert(ids::command(path), NodeKind::Command, entries);
}

fn dictionary(dictionary: Dictionary) -> &'static str {
    match dictionary {
        Dictionary::General => "general",
        Dictionary::Doctor => "doctor",
    }
}

fn network(network: Network) -> &'static str {
    match network {
        Network::None => "none",
        Network::Optional => "optional",
        Network::Required => "required",
    }
}

fn arg_node(b: &mut Builder, path: &str, facts: &ArgFacts) {
    let (id, kind) = match (facts.index, facts.identity.strip_prefix("--"), facts.short) {
        (Some(index), _, _) => (ids::positional(path, index), NodeKind::Positional),
        (None, Some(long), _) => (ids::flag(path, long), NodeKind::Flag),
        (None, None, Some(short)) => (ids::short_flag(path, short), NodeKind::Flag),
        (None, None, None) => (
            format!("{} {path} {}", section::INPUT, facts.identity),
            NodeKind::Flag,
        ),
    };
    let text = |s: &str| Some(Fact::Text(s.to_owned()));
    let flag = |set: bool| set.then_some(Fact::Bool(true));
    let mut entries = vec![
        (
            fact::SHORT,
            facts.short.and_then(|c| text(&format!("-{c}"))),
        ),
        (
            fact::SHORT_ALIASES,
            Fact::list(facts.short_aliases.iter().map(|c| format!("-{c}"))),
        ),
        (fact::ALIASES, Fact::list(facts.aliases.clone())),
        (fact::GRAMMAR, text(facts.grammar.wire())),
        (fact::VALUES, Fact::list(facts.values.clone())),
        (fact::IGNORE_CASE, flag(facts.ignore_case)),
        (
            fact::DEFAULT,
            (!facts.default.is_empty()).then(|| Fact::Text(facts.default.join(","))),
        ),
        (
            fact::ARITY_MIN,
            i64::try_from(facts.arity.0).ok().map(Fact::Int),
        ),
        (
            fact::ARITY_MAX,
            facts
                .arity
                .1
                .and_then(|max| i64::try_from(max).ok())
                .map(Fact::Int),
        ),
        (fact::REPEAT, flag(facts.repeat)),
        (fact::REQUIRED, Some(Fact::Bool(facts.required))),
        (fact::GLOBAL, flag(facts.global)),
        (fact::CONFLICTS, Fact::list(facts.conflicts.clone())),
        (fact::REQUIRES, Fact::list(facts.requires.clone())),
        (fact::ACTION, text(facts.action)),
        (fact::VALUE_NAME, facts.value_name.as_deref().and_then(text)),
        (fact::ENV, facts.env.as_deref().and_then(text)),
        (
            fact::VALUE_DELIMITER,
            facts.value_delimiter.and_then(|c| text(&c.to_string())),
        ),
    ];
    // Each set switch is named by its own fact (`allow_hyphen_values`, …).
    for switch in &facts.switches {
        if let Some(name) = fact::ALL.iter().find(|name| **name == *switch) {
            entries.push((name, Some(Fact::Bool(true))));
        }
    }
    b.insert(id, kind, entries);
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::app::COMMANDS;
    use crate::contract::commands::{Grammar, Values};
    use crate::contract::manifest::{Node, build_checked};

    fn nodes() -> BTreeMap<String, Node> {
        build_checked(&Registry::builtin()).0.nodes
    }

    fn built() -> clap::Command {
        let mut root = Cli::command();
        root.build();
        root
    }

    /// Every built path with its command and its parent, depth first.
    fn paths(root: &clap::Command) -> Vec<(String, clap::Command, Option<clap::Command>)> {
        fn visit(
            path: String,
            cmd: &clap::Command,
            parent: Option<&clap::Command>,
            out: &mut Vec<(String, clap::Command, Option<clap::Command>)>,
        ) {
            out.push((path.clone(), cmd.clone(), parent.cloned()));
            for sub in cmd.get_subcommands() {
                let child = if path == ROOT {
                    sub.get_name().to_owned()
                } else {
                    format!("{path} {}", sub.get_name())
                };
                visit(child, sub, Some(cmd), out);
            }
        }
        let mut out = Vec::new();
        visit(ROOT.to_owned(), root, None, &mut out);
        out
    }

    fn get<'n>(nodes: &'n BTreeMap<String, Node>, id: &str, name: &str) -> Option<&'n Fact> {
        nodes.get(id).and_then(|node| node.facts.get(name))
    }

    fn list(items: &[&str]) -> Fact {
        Fact::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// Up to 50 mismatches, then the count of the rest.
    fn shown(lines: &[String]) -> String {
        let mut out: Vec<String> = lines.iter().take(50).cloned().collect();
        if lines.len() > 50 {
            out.push(format!("… and {} more", lines.len() - 50));
        }
        out.join("\n")
    }

    #[test]
    fn every_commands_path_flag_and_positional_appears_with_its_facts() {
        let nodes = nodes();
        let mut mismatches = Vec::new();
        let mut check = |id: &str, name: &str, got: Option<&Fact>, want: Option<Fact>| {
            if got != want.as_ref() {
                mismatches.push(format!(
                    "{id} {name}: clap says {want:?}, the manifest {got:?}"
                ));
            }
        };
        for (path, cmd, parent) in paths(&built()) {
            let id = ids::command(&path);
            let mut aliases: Vec<String> = cmd.get_all_aliases().map(str::to_owned).collect();
            aliases.sort();
            check(
                &id,
                fact::ALIASES,
                get(&nodes, &id, fact::ALIASES),
                Fact::list(aliases),
            );
            check(
                &id,
                fact::BINDS,
                get(&nodes, &id, fact::BINDS),
                Some(Fact::Bool(BINDING_COMMANDS.contains(&path.as_str()))),
            );
            if let Some(row) = COMMANDS.iter().find(|row| row.name == path) {
                check(
                    &id,
                    fact::EXITS,
                    get(&nodes, &id, fact::EXITS),
                    Fact::ints(row.exits.iter().map(|&e| i64::from(e))),
                );
                check(
                    &id,
                    fact::MUTATES,
                    get(&nodes, &id, fact::MUTATES),
                    Some(Fact::Bool(row.mutates)),
                );
                check(
                    &id,
                    fact::STORES_PLAN,
                    get(&nodes, &id, fact::STORES_PLAN),
                    Some(Fact::Bool(row.stores_plan)),
                );
                check(
                    &id,
                    fact::DICTIONARY,
                    get(&nodes, &id, fact::DICTIONARY),
                    Some(Fact::Text(dictionary(row.dictionary).into())),
                );
                check(
                    &id,
                    fact::NETWORK,
                    get(&nodes, &id, fact::NETWORK),
                    Some(Fact::Text(network(row.network).into())),
                );
            }
            for arg in cmd.get_arguments() {
                let inherited = arg.is_global_set()
                    && parent
                        .as_ref()
                        .is_some_and(|p| p.get_arguments().any(|a| a.get_id() == arg.get_id()));
                if inherited {
                    continue;
                }
                let id = match (
                    arg.get_index().filter(|_| arg.is_positional()),
                    arg.get_long(),
                ) {
                    (Some(index), _) => ids::positional(&path, index),
                    (None, Some(long)) => ids::flag(&path, long),
                    (None, None) => continue,
                };
                if !nodes.contains_key(&id) {
                    check(&id, "a node", None, Some(Fact::Bool(true)));
                    continue;
                }
                let defaults: Vec<String> = arg
                    .get_default_values()
                    .iter()
                    .map(|v| v.to_string_lossy().into_owned())
                    .collect();
                check(
                    &id,
                    fact::DEFAULT,
                    get(&nodes, &id, fact::DEFAULT),
                    (!defaults.is_empty()).then(|| Fact::Text(defaults.join(","))),
                );
                let range = arg.get_num_args();
                let min = range.map_or(0, |r| r.min_values());
                check(
                    &id,
                    fact::ARITY_MIN,
                    get(&nodes, &id, fact::ARITY_MIN),
                    Some(Fact::Int(i64::try_from(min).unwrap())),
                );
                let max = range.map_or(Some(0), |r| {
                    (r.max_values() != usize::MAX).then_some(r.max_values())
                });
                check(
                    &id,
                    fact::ARITY_MAX,
                    get(&nodes, &id, fact::ARITY_MAX),
                    max.map(|m| Fact::Int(i64::try_from(m).unwrap())),
                );
                check(
                    &id,
                    fact::REQUIRED,
                    get(&nodes, &id, fact::REQUIRED),
                    Some(Fact::Bool(arg.is_required_set())),
                );
                let repeat = matches!(
                    arg.get_action(),
                    clap::ArgAction::Append | clap::ArgAction::Count
                );
                check(
                    &id,
                    fact::REPEAT,
                    get(&nodes, &id, fact::REPEAT),
                    repeat.then_some(Fact::Bool(true)),
                );
                check(
                    &id,
                    fact::SHORT,
                    get(&nodes, &id, fact::SHORT),
                    arg.get_short().map(|c| Fact::Text(format!("-{c}"))),
                );
                let mut long_aliases: Vec<String> = arg
                    .get_all_aliases()
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                long_aliases.sort();
                check(
                    &id,
                    fact::ALIASES,
                    get(&nodes, &id, fact::ALIASES),
                    Fact::list(long_aliases),
                );
                let values: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .flat_map(|v| {
                        v.get_name_and_aliases()
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .collect();
                check(
                    &id,
                    fact::VALUES,
                    get(&nodes, &id, fact::VALUES),
                    Fact::list(values),
                );
                check(
                    &id,
                    fact::IGNORE_CASE,
                    get(&nodes, &id, fact::IGNORE_CASE),
                    arg.is_ignore_case_set().then_some(Fact::Bool(true)),
                );
            }
        }
        assert!(
            mismatches.is_empty(),
            "the manifest disagrees with clap:\n{}",
            shown(&mismatches)
        );
    }

    #[test]
    fn globals_are_recorded_once_on_the_root() {
        let nodes = nodes();
        let globals: Vec<&String> = nodes
            .keys()
            .filter(|id| id.starts_with("input omaddons -"))
            .filter(|id| get(&nodes, id, fact::GLOBAL) == Some(&Fact::Bool(true)))
            .collect();
        assert_eq!(globals.len(), 16, "{globals:#?}");
        assert!(nodes.contains_key("input omaddons --version"));
        assert_eq!(
            get(&nodes, "input omaddons --version", fact::GLOBAL),
            None,
            "--version is root-only"
        );
        let repeated: Vec<&String> = nodes
            .keys()
            .filter(|id| {
                id.starts_with("input ")
                    && !id.starts_with("input omaddons ")
                    && id.ends_with(" --json")
            })
            .collect();
        assert!(
            repeated.is_empty(),
            "a global repeated below the root: {repeated:?}"
        );
        for (path, cmd, _) in paths(&built()) {
            assert!(
                cmd.get_arguments()
                    .any(|arg| arg.get_long() == Some("json")),
                "{path}: clap propagates --json"
            );
        }
    }

    /// A case's name, the node's id and the facts it must hold.
    type Case = (&'static str, &'static str, Vec<(&'static str, Fact)>);

    #[test]
    fn grammar_kinds() {
        let nodes = nodes();
        let cases: Vec<Case> = vec![
            (
                "json_is_flag",
                "input omaddons --json",
                vec![
                    (fact::GRAMMAR, Fact::Text("flag".into())),
                    (fact::DEFAULT, Fact::Text("false".into())),
                    (fact::ARITY_MIN, Fact::Int(0)),
                    (fact::ARITY_MAX, Fact::Int(0)),
                    (fact::ACTION, Fact::Text("set_true".into())),
                ],
            ),
            (
                "verbose_is_count",
                "input omaddons --verbose",
                vec![
                    (fact::GRAMMAR, Fact::Text("count".into())),
                    (fact::REPEAT, Fact::Bool(true)),
                    (fact::SHORT, Fact::Text("-v".into())),
                ],
            ),
            (
                "format_is_enum_ignoring_case",
                "input omaddons --format",
                vec![
                    (fact::GRAMMAR, Fact::Text("enum".into())),
                    (fact::VALUES, list(&["human", "json"])),
                    (fact::IGNORE_CASE, Fact::Bool(true)),
                ],
            ),
            (
                "progress_values",
                "input omaddons --progress",
                vec![(fact::VALUES, list(&["human", "json", "none"]))],
            ),
            (
                "channel_is_enum",
                "input install --channel",
                vec![
                    (fact::GRAMMAR, Fact::Text("enum".into())),
                    (fact::VALUES, list(&["alpha", "beta", "stable"])),
                ],
            ),
            (
                "target_has_alias",
                "input omaddons --target",
                vec![
                    (fact::GRAMMAR, Fact::Text("string".into())),
                    (fact::SHORT, Fact::Text("-t".into())),
                    (fact::ALIASES, list(&["flavor"])),
                ],
            ),
            (
                "install_file_is_path",
                "input install --file",
                vec![(fact::GRAMMAR, Fact::Text("path".into()))],
            ),
            (
                "sync_file_positional_is_path_and_optional",
                "input sync @1",
                vec![
                    (fact::GRAMMAR, Fact::Text("path".into())),
                    (fact::REQUIRED, Fact::Bool(false)),
                    (fact::VALUE_NAME, Fact::Text("FILE".into())),
                ],
            ),
            (
                "net_log_limit_is_integer",
                "input net-log --limit",
                vec![(fact::GRAMMAR, Fact::Text("integer".into()))],
            ),
            (
                "install_refs_are_required_repeatable",
                "input install @1",
                vec![
                    (fact::GRAMMAR, Fact::Text("string".into())),
                    (fact::REQUIRED, Fact::Bool(true)),
                    (fact::REPEAT, Fact::Bool(true)),
                    (fact::ARITY_MIN, Fact::Int(1)),
                    (fact::VALUE_NAME, Fact::Text("REF".into())),
                ],
            ),
            (
                "help_command_positional_is_optional",
                "input help @1",
                vec![
                    (fact::REQUIRED, Fact::Bool(false)),
                    (fact::REPEAT, Fact::Bool(true)),
                ],
            ),
            (
                "last_by_is_enum",
                "input undo --last-by",
                vec![
                    (fact::GRAMMAR, Fact::Text("enum".into())),
                    (fact::VALUES, list(&["agent", "human", "widget"])),
                ],
            ),
        ];
        let mut failures = Vec::new();
        for (case, id, facts) in cases {
            for (name, want) in facts {
                let got = get(&nodes, id, name);
                if got != Some(&want) {
                    failures.push(format!(
                        "{case}: {id} has {name} {got:?}, expected {want:?}"
                    ));
                }
            }
        }
        assert_eq!(
            get(&nodes, "input install @1", fact::ARITY_MAX),
            None,
            "install's refs are unbounded"
        );
        let source = get(&nodes, "input search --source", fact::VALUES);
        let want: Vec<String> = built()
            .find_subcommand("search")
            .and_then(|s| {
                s.get_arguments()
                    .find(|a| a.get_long() == Some("source"))
                    .cloned()
            })
            .map(|a| {
                a.get_possible_values()
                    .iter()
                    .map(|v| v.get_name().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(source, Fact::list(want).as_ref(), "search_source_values");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn conflicts_are_symmetric() {
        let nodes = nodes();
        let cases = [
            ("undo_tx", "input undo @1", &["--last-by", "--run"][..]),
            ("undo_run", "input undo --run", &["--last-by", "@1"]),
            ("undo_last_by", "input undo --last-by", &["--run", "@1"]),
            (
                "install_any_flavor",
                "input install --any-flavor",
                &["--no-any-flavor"],
            ),
            (
                "install_no_any_flavor",
                "input install --no-any-flavor",
                &["--any-flavor"],
            ),
            ("status_deep", "input status --deep", &["--cached"]),
            ("status_cached", "input status --cached", &["--deep"]),
            (
                "backup_create_auto",
                "input backup create --auto",
                &["--verify"],
            ),
            (
                "backup_create_verify",
                "input backup create --verify",
                &["--auto"],
            ),
        ];
        for (case, id, want) in cases {
            let got = get(&nodes, id, fact::CONFLICTS);
            assert_eq!(
                got,
                Some(&list(want)),
                "{case}: {id} has {got:?}, expected {want:?}"
            );
        }
        let keep = get(&nodes, "input backup prune --keep", fact::CONFLICTS);
        assert!(
            matches!(keep, Some(Fact::List(l)) if l.contains(&"@1".to_owned())),
            "backup_prune_keep: {keep:?}"
        );
        let ids_conflicts = get(&nodes, "input backup prune @1", fact::CONFLICTS);
        assert!(
            matches!(ids_conflicts, Some(Fact::List(l)) if l.contains(&"--keep".to_owned()) && l.contains(&"--older-than".to_owned())),
            "backup_prune_ids: {ids_conflicts:?}"
        );
        assert_eq!(
            get(
                &nodes,
                "command errors",
                fact::ARGS_CONFLICT_WITH_SUBCOMMANDS
            ),
            Some(&Fact::Bool(true))
        );
    }

    #[test]
    fn positionals_are_identified_by_index() {
        let nodes = nodes();
        for (id, node) in nodes.iter().filter(|(id, _)| id.starts_with("input ")) {
            assert!(!id.contains('<'), "{id}: a name in a positional's id");
            if node.kind == NodeKind::Positional.wire() {
                assert!(id.contains(" @"), "{id}: a positional by index");
                assert!(
                    node.facts.contains_key(fact::VALUE_NAME),
                    "{id}: its value_name"
                );
            }
        }
    }

    #[test]
    fn arg_facts_are_complete() {
        for (path, cmd, _) in paths(&built()) {
            let mut seen = BTreeSet::new();
            for arg in cmd.get_arguments() {
                let facts = commands::arg_facts(&path, &cmd, arg);
                assert_ne!(
                    facts.grammar,
                    Grammar::Other,
                    "{path} {}: an unclassified grammar",
                    facts.identity
                );
                assert!(
                    seen.insert(facts.identity.clone()),
                    "{path} {}: a repeated identity",
                    facts.identity
                );
            }
        }
    }

    #[test]
    fn shared_helpers_agree_with_capabilities() {
        let root = built();
        for info in commands::commands() {
            let cmd = info
                .path
                .split(' ')
                .try_fold(&root, |cmd, word| cmd.find_subcommand(word))
                .unwrap();
            for flag in &info.flags {
                let arg = cmd
                    .get_arguments()
                    .find(|a| a.get_long() == Some(flag.long.as_str()))
                    .unwrap();
                let facts = commands::arg_facts(info.path, cmd, arg);
                let words = match &flag.values {
                    Values::Words(words) => words.clone(),
                    Values::Any => Vec::new(),
                };
                let mut sorted_words = words;
                sorted_words.sort();
                assert_eq!(
                    facts.values, sorted_words,
                    "{} --{}: values",
                    info.path, flag.long
                );
                let renamed: BTreeSet<String> = facts
                    .conflicts
                    .iter()
                    .filter_map(|identity| {
                        identity.strip_prefix("--").map(str::to_owned).or_else(|| {
                            let index: usize = identity.strip_prefix('@')?.parse().ok()?;
                            cmd.get_arguments()
                                .find(|a| a.is_positional() && a.get_index() == Some(index))
                                .map(|a| a.get_id().to_string())
                        })
                    })
                    .collect();
                let published: BTreeSet<String> = flag.conflicts_with.iter().cloned().collect();
                // capabilities leaves out `--if-match` where it does not bind.
                let renamed: BTreeSet<String> = renamed
                    .into_iter()
                    .filter(|name| info.binds || name != "if-match")
                    .collect();
                assert_eq!(
                    renamed, published,
                    "{} --{}: conflicts",
                    info.path, flag.long
                );
                let requires: Vec<String> = facts
                    .requires
                    .iter()
                    .map(|r| r.trim_start_matches('-').to_owned())
                    .collect();
                assert_eq!(
                    requires, flag.requires,
                    "{} --{}: requires",
                    info.path, flag.long
                );
            }
        }
    }

    #[test]
    fn every_config_key_has_its_node() {
        let nodes = nodes();
        let configs: Vec<&String> = nodes
            .keys()
            .filter(|id| ids::section(id) == section::CONFIG)
            .collect();
        assert_eq!(configs.len(), KEYS.len(), "{configs:#?}");
        for key in KEYS {
            let id = ids::registry(section::CONFIG, key.key);
            assert_eq!(
                get(&nodes, &id, fact::CONFIG_TYPE),
                Some(&Fact::Text(key.kind.name().into())),
                "{id}"
            );
        }
    }

    #[test]
    fn version_and_the_root_are_envelope_only_commands() {
        let nodes = nodes();
        for path in [ROOT, "version"] {
            let id = ids::command(path);
            assert_eq!(
                get(&nodes, &id, fact::ENVELOPE_ONLY),
                Some(&Fact::Bool(true)),
                "{id}"
            );
            assert!(get(&nodes, &id, fact::EXITS).is_some(), "{id}: its exits");
        }
        assert!(
            nodes.contains_key("command auth"),
            "a pure parent has a node"
        );
    }
}
