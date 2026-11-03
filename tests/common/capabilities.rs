//! `omaddons capabilities` held to the binary that printed it: every command, flag, value,
//! alias, conflict, requirement and pointer it lists parses as it says ([`round_trip`]),
//! and every registry it publishes is listed whole, once and in order ([`completeness`]).
//!
//! `tests/capabilities.rs` runs these, and T2's slice-1 demo reuses them: keep the
//! signatures of [`fetch`], [`round_trip`] and [`completeness`] stable. Each check
//! collects every failure it finds, never stopping at the first, so one run shows every
//! broken item; an empty list is a pass.

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use serde_json::{Value, json};

use omaddons::cli::Cli;

use super::tlog::{Runner, TestLog};

/// What a value is tried with when neither a listed value nor a default says better, in
/// order: a number, a path and free text take `1`; a duration or a time takes the others.
pub const SAMPLES: &[&str] = &["1", "1d", "500ms"];

/// `omaddons capabilities --json` on `on`, through the log, which records its digest (the
/// data itself is about 300 KB); its `data`.
#[track_caller]
pub fn fetch(log: &TestLog, on: &impl Runner) -> Value {
    let ran = log.run(on, &["capabilities", "--json"]);
    ran.ok();
    log.json("digest", &digest(ran.stdout()));
    ran.data().clone()
}

/// The values to try for `entry` (an `args[]` or `flags[]` entry), best first, each once:
/// its listed values, its default, then [`SAMPLES`] (only without a list: a listed value is
/// the only kind clap takes).
pub fn candidates(entry: &Value) -> Vec<String> {
    let mut out: Vec<String> = list(entry, "possible_values")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let listed = !out.is_empty();
    let mut add = |value: &str| {
        if !out.iter().any(|have| have == value) {
            out.push(value.to_owned());
        }
    };
    if let Some(default) = entry["default"].as_str() {
        add(default);
    }
    if !listed {
        for sample in SAMPLES {
            add(sample);
        }
    }
    out
}

/// The sample of `entry`: its best candidate.
fn sample(entry: &Value) -> String {
    candidates(entry).swap_remove(0)
}

fn path_words(command: &Value) -> Vec<String> {
    command["path"]
        .as_str()
        .unwrap_or_default()
        .split(' ')
        .map(str::to_owned)
        .collect()
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

fn flag_named<'a>(command: &'a Value, long: &str) -> Option<&'a Value> {
    list(command, "flags")
        .iter()
        .find(|flag| flag["long"] == long)
}

/// `--<long>`, with its sample when it takes a value.
fn flag_words(flag: &Value, value: Option<&str>) -> Vec<String> {
    let mut words = vec![format!("--{}", flag["long"].as_str().unwrap_or_default())];
    if flag["takes_value"] == true {
        words.push(value.map_or_else(|| sample(flag), str::to_owned));
    }
    words
}

/// The flags `flag` requires, each with its sample, except `without`.
fn requires_words(command: &Value, flag: &Value, without: Option<&str>) -> Vec<String> {
    list(flag, "requires")
        .iter()
        .filter_map(Value::as_str)
        .filter(|long| Some(*long) != without)
        .filter_map(|long| flag_named(command, long))
        .flat_map(|required| flag_words(required, None))
        .collect()
}

/// The positionals up to and including the one named `name` (every one, without a name),
/// each its sample, `name`'s `value` when given; only required ones beyond those needed.
fn positional_words(command: &Value, name: Option<&str>, value: Option<&str>) -> Vec<String> {
    let args = list(command, "args");
    let upto = name.and_then(|name| args.iter().position(|arg| arg["name"] == name));
    let mut words = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        let wanted = upto.is_some_and(|upto| index <= upto) || arg["required"] == true;
        if !wanted {
            break;
        }
        if upto == Some(index) {
            words.push(value.map_or_else(|| sample(arg), str::to_owned));
        } else {
            words.push(sample(arg));
        }
    }
    words
}

/// The required flags, with their samples and what they require.
fn required_flag_words(command: &Value) -> Vec<String> {
    list(command, "flags")
        .iter()
        .filter(|flag| flag["required"] == true)
        .flat_map(|flag| {
            let mut words = flag_words(flag, None);
            words.extend(requires_words(command, flag, None));
            words
        })
        .collect()
}

/// The argv that parses for `command` (a `commands[]` entry): `omaddons`, its path, a
/// sample for each required positional and each required flag.
pub fn sample_argv(command: &Value) -> Vec<String> {
    let mut argv = vec!["omaddons".to_owned()];
    argv.extend(path_words(command));
    argv.extend(positional_words(command, None, None));
    argv.extend(required_flag_words(command));
    argv
}

/// [`sample_argv`] with the positional `name` set to `value`.
fn argv_with_positional(command: &Value, name: &str, value: &str) -> Vec<String> {
    let mut argv = vec!["omaddons".to_owned()];
    argv.extend(path_words(command));
    argv.extend(positional_words(command, Some(name), Some(value)));
    argv.extend(required_flag_words(command));
    argv
}

fn parse(argv: &[String]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(argv)
}

/// What clap said, on one line.
fn said(error: &clap::Error) -> String {
    let text = error.to_string();
    let first = text.lines().next().unwrap_or_default().to_owned();
    format!("{:?}: {first}", error.kind())
}

fn joined(argv: &[String]) -> String {
    shell_words::join(argv)
}

/// Every round-trip failure of `data` (empty when it round-trips); each names the
/// command, the flag, value or alias, the argv and what clap said.
pub fn round_trip(data: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    for command in list(data, "commands") {
        round_trip_command(command, &mut failures);
    }
    let mut pointers: Vec<(String, &Value)> = vec![
        ("schemas_uri".to_owned(), &data["schemas_uri"]),
        ("robot_docs_uri".to_owned(), &data["robot_docs_uri"]),
        (
            "failure_modes.command".to_owned(),
            &data["failure_modes"]["command"],
        ),
    ];
    for command in list(data, "commands") {
        let path = command["path"].as_str().unwrap_or_default();
        pointers.push((format!("{path}: data_schema"), &command["data_schema"]));
    }
    for (name, pointer) in pointers {
        let Some(pointer) = pointer.as_str() else {
            continue;
        };
        match shell_words::split(pointer) {
            Ok(argv) => {
                if let Err(error) = parse(&argv) {
                    failures.push(format!(
                        "{name}: `{pointer}` does not parse: {}",
                        said(&error)
                    ));
                }
            }
            Err(error) => failures.push(format!("{name}: `{pointer}` does not split: {error}")),
        }
    }
    failures
}

fn round_trip_command(command: &Value, failures: &mut Vec<String>) {
    let path = command["path"].as_str().unwrap_or_default();
    let base = sample_argv(command);

    // 1. Its help parses, with the help flag set.
    let mut help = base.clone();
    help.push("--help".to_owned());
    match parse(&help) {
        Ok(cli) if cli.global.help => {}
        Ok(_) => failures.push(format!(
            "{path}: `{}` parsed without help set",
            joined(&help)
        )),
        Err(error) => failures.push(format!("{path}: `{}`: {}", joined(&help), said(&error))),
    }
    let plain = match parse(&base) {
        Ok(cli) => format!("{cli:?}"),
        Err(error) => {
            failures.push(format!("{path}: `{}`: {}", joined(&base), said(&error)));
            return;
        }
    };

    for flag in list(command, "flags") {
        round_trip_flag(command, flag, &base, &plain, failures);
    }

    // 4. Every listed value of every positional parses.
    for arg in list(command, "args") {
        let name = arg["name"].as_str().unwrap_or_default();
        for value in list(arg, "possible_values")
            .iter()
            .filter_map(Value::as_str)
        {
            let argv = argv_with_positional(command, name, value);
            if let Err(error) = parse(&argv) {
                failures.push(format!(
                    "{path}: <{name}> {value}: `{}`: {}",
                    joined(&argv),
                    said(&error)
                ));
            }
        }
    }

    // 7. Every alias of the command resolves to its path.
    for alias in list(command, "aliases").iter().filter_map(Value::as_str) {
        let mut argv = base.clone();
        let at = path_words(command).len();
        argv[at] = alias.to_owned();
        match parse(&argv) {
            Ok(cli) => {
                let name = omaddons::app::command_name(cli.command.as_ref());
                if name != path {
                    failures.push(format!("{path}: alias {alias} resolves to {name}"));
                }
            }
            Err(error) => failures.push(format!(
                "{path}: alias {alias}: `{}`: {}",
                joined(&argv),
                said(&error)
            )),
        }
    }
}

fn round_trip_flag(
    command: &Value,
    flag: &Value,
    base: &[String],
    plain: &str,
    failures: &mut Vec<String>,
) {
    let path = command["path"].as_str().unwrap_or_default();
    let long = flag["long"].as_str().unwrap_or_default();
    let required = flag["required"] == true;
    let with = |words: &[String], extra: &[String]| -> Vec<String> {
        let mut argv = base.to_vec();
        argv.extend(words.iter().cloned());
        argv.extend(extra.iter().cloned());
        argv
    };
    let requires = requires_words(command, flag, None);

    // 2. The flag parses and reaches the parsed command line. A value equal to the default
    // (or to the required flag's own sample) changes nothing, so another is used.
    let takes_value = flag["takes_value"] == true;
    let unchanged: Option<String> = if required {
        Some(sample(flag))
    } else {
        flag["default"].as_str().map(str::to_owned)
    };
    let mut values: Vec<Option<String>> = if takes_value {
        candidates(flag).into_iter().map(Some).collect()
    } else {
        vec![None]
    };
    // A value that changes nothing is tried last (stable: the rest keep their order).
    values.sort_by_key(|value| value.is_some() && *value == unchanged);
    let mut parsed = None;
    let mut last_error = None;
    for value in &values {
        let argv = with(&flag_words(flag, value.as_deref()), &requires);
        match parse(&argv) {
            Ok(cli) => {
                parsed = Some((argv, cli, value.clone()));
                break;
            }
            Err(error) => last_error = Some((argv, error)),
        }
    }
    let Some((argv, cli, value)) = parsed else {
        if let Some((argv, error)) = last_error {
            failures.push(format!(
                "{path}: --{long}: `{}`: {}",
                joined(&argv),
                said(&error)
            ));
        }
        return;
    };
    let same_as_default = value.is_some() && value == unchanged;
    if !same_as_default && format!("{cli:?}") == plain {
        failures.push(format!(
            "{path}: --{long}: `{}` parses to the same command line as without it",
            joined(&argv)
        ));
    }
    let reached = format!("{cli:?}");

    // 3. The short form and every alias parse to the same command line.
    let mut spellings: Vec<String> = list(flag, "aliases")
        .iter()
        .filter_map(Value::as_str)
        .map(|alias| format!("--{alias}"))
        .collect();
    spellings.extend(flag["short"].as_str().map(|short| format!("-{short}")));
    for spelling in spellings {
        let mut words = vec![spelling.clone()];
        words.extend(value.clone());
        let argv = with(&words, &requires);
        match parse(&argv) {
            Ok(cli) if format!("{cli:?}") == reached => {}
            Ok(_) => failures.push(format!(
                "{path}: {spelling} parses unlike --{long}: `{}`",
                joined(&argv)
            )),
            Err(error) => failures.push(format!(
                "{path}: {spelling}: `{}`: {}",
                joined(&argv),
                said(&error)
            )),
        }
    }

    // 4. Every listed value parses.
    for listed in list(flag, "possible_values")
        .iter()
        .filter_map(Value::as_str)
    {
        let argv = with(&flag_words(flag, Some(listed)), &requires);
        if let Err(error) = parse(&argv) {
            failures.push(format!(
                "{path}: --{long} {listed}: `{}`: {}",
                joined(&argv),
                said(&error)
            ));
        }
    }

    // 5. Every declared conflict is refused as one.
    let own = with(&flag_words(flag, value.as_deref()), &requires);
    for other in list(flag, "conflicts_with")
        .iter()
        .filter_map(Value::as_str)
    {
        let argv = match flag_named(command, other) {
            Some(other_flag) => {
                let mut argv = own.clone();
                argv.extend(flag_words(other_flag, None));
                argv.extend(requires_words(command, other_flag, None));
                argv
            }
            None => {
                let Some(arg) = list(command, "args")
                    .iter()
                    .find(|arg| arg["name"] == other)
                else {
                    failures.push(format!("{path}: --{long} conflicts with unknown {other}"));
                    continue;
                };
                let mut argv = argv_with_positional(command, other, &sample(arg));
                argv.extend(flag_words(flag, value.as_deref()));
                argv.extend(requires.iter().cloned());
                argv
            }
        };
        match parse(&argv) {
            Err(error) if error.kind() == ErrorKind::ArgumentConflict => {}
            Err(error) => failures.push(format!(
                "{path}: --{long} with {other}: `{}`: {} (expected a conflict)",
                joined(&argv),
                said(&error)
            )),
            Ok(_) => failures.push(format!(
                "{path}: --{long} with {other}: `{}` parses (expected a conflict)",
                joined(&argv)
            )),
        }
    }

    // 6. Every declared requirement is enforced.
    for required_long in list(flag, "requires").iter().filter_map(Value::as_str) {
        let without = requires_words(command, flag, Some(required_long));
        let argv = with(&flag_words(flag, value.as_deref()), &without);
        match parse(&argv) {
            Err(error) if error.kind() == ErrorKind::MissingRequiredArgument => {}
            Err(error) => failures.push(format!(
                "{path}: --{long} without --{required_long}: `{}`: {} (expected a missing \
                 requirement)",
                joined(&argv),
                said(&error)
            )),
            Ok(_) => failures.push(format!(
                "{path}: --{long} without --{required_long}: `{}` parses (expected a missing \
                 requirement)",
                joined(&argv)
            )),
        }
    }
}

/// The `key` of every entry of `data[list]`, as text.
fn names(data: &Value, list_key: &str, key: &str) -> Vec<String> {
    list(data, list_key)
        .iter()
        .map(|entry| match &entry[key] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// A failure when `got` is not `want`: what is missing, what is extra, or that the order
/// differs; and one for each name listed twice.
fn same_names(failures: &mut Vec<String>, label: &str, got: &[String], want: &[String]) {
    // Each name's first place; a later one is a duplicate, reported apart.
    let mut seen = std::collections::BTreeSet::new();
    let mut once: Vec<String> = Vec::new();
    for name in got {
        if seen.insert(name) {
            once.push(name.clone());
        } else {
            failures.push(format!("{label}: {name} is listed twice"));
        }
    }
    if once == want {
        return;
    }
    let missing: Vec<&String> = want.iter().filter(|name| !once.contains(name)).collect();
    let extra: Vec<&String> = once.iter().filter(|name| !want.contains(name)).collect();
    if missing.is_empty() && extra.is_empty() {
        failures.push(format!(
            "{label}: the order differs: {once:?}, expected {want:?}"
        ));
    } else {
        failures.push(format!("{label}: missing {missing:?}, extra {extra:?}"));
    }
}

/// A failure when `got` is not `want`.
fn same_value(failures: &mut Vec<String>, label: &str, got: &Value, want: &Value) {
    if got != want {
        failures.push(format!("{label}: {got}, expected {want}"));
    }
}

fn wire<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn text<T: serde::Serialize>(value: T) -> String {
    match wire(value) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

/// The clap command at `path`, built (globals propagated).
fn clap_command(root: &clap::Command, path: &str) -> Option<clap::Command> {
    path.split(' ')
        .try_fold(root, |cmd, word| {
            cmd.get_subcommands().find(|sub| sub.get_name() == word)
        })
        .cloned()
}

/// Every completeness failure of `data` against the library's registries: each list
/// whole, in order, each name once, and each entry's facts equal to its registry's. The
/// client-rules rows are the built-in table's, so `data` must come from a home whose
/// config adds no row.
pub fn completeness(data: &Value) -> Vec<String> {
    use omaddons::app::COMMANDS;
    use omaddons::app::precondition::BINDING_COMMANDS;
    use omaddons::contract::actions::Action;
    use omaddons::doctor::finding::Subsystem;
    use omaddons::doctor::registry::FAILURE_MODES;
    use omaddons::error::{ErrorCode, Exit, WarningCode};
    use omaddons::rules::{Family, InstallKind, RulesTable};
    use omaddons::sources::SourceId;

    let mut failures = Vec::new();

    // Commands, with every fact of their rows and of clap.
    let want: Vec<String> = COMMANDS.iter().map(|row| row.name.to_owned()).collect();
    same_names(
        &mut failures,
        "commands",
        &names(data, "commands", "path"),
        &want,
    );
    let mut root = Cli::command();
    root.build();
    for entry in list(data, "commands") {
        let path = entry["path"].as_str().unwrap_or_default();
        let Some(row) = COMMANDS.iter().find(|row| row.name == path) else {
            continue;
        };
        let label = |key: &str| format!("commands {path}: {key}");
        same_value(
            &mut failures,
            &label("group"),
            &entry["group"],
            &wire(row.group),
        );
        same_value(
            &mut failures,
            &label("mutates"),
            &entry["mutates"],
            &json!(row.mutates),
        );
        same_value(
            &mut failures,
            &label("dictionary"),
            &entry["dictionary"],
            &wire(row.dictionary),
        );
        same_value(
            &mut failures,
            &label("exit_codes"),
            &entry["exit_codes"],
            &json!(row.exits),
        );
        same_value(
            &mut failures,
            &label("stores_plan"),
            &entry["stores_plan"],
            &json!(row.stores_plan),
        );
        same_value(
            &mut failures,
            &label("network"),
            &entry["network"],
            &wire(row.network),
        );
        same_value(
            &mut failures,
            &label("binds"),
            &entry["binds"],
            &json!(BINDING_COMMANDS.contains(&path)),
        );
        same_value(&mut failures, &label("json"), &entry["json"], &json!(true));
        match clap_command(&root, path) {
            Some(cmd) => {
                let about = cmd
                    .get_about()
                    .map(|about| about.to_string().trim().to_owned())
                    .unwrap_or_default();
                same_value(
                    &mut failures,
                    &label("description"),
                    &entry["description"],
                    &json!(about),
                );
                let aliases: Vec<&str> = cmd.get_visible_aliases().collect();
                same_value(
                    &mut failures,
                    &label("aliases"),
                    &entry["aliases"],
                    &json!(aliases),
                );
            }
            None => failures.push(format!("commands {path}: no such clap command")),
        }
    }

    // The global flags: every program-wide one on every command, `--if-match` only where a
    // run binds, each `global: true`, after the command's own flags.
    let program_wide: Vec<String> =
        <omaddons::cli::GlobalArgs as clap::Args>::augment_args(clap::Command::default())
            .get_arguments()
            .filter(|arg| arg.is_global_set() && !arg.is_hide_set())
            .filter_map(|arg| arg.get_long().map(str::to_owned))
            .collect();
    for entry in list(data, "commands") {
        let path = entry["path"].as_str().unwrap_or_default();
        let flags = list(entry, "flags");
        let first = flags
            .iter()
            .position(|flag| flag["global"] == true)
            .unwrap_or(flags.len());
        if flags[first..].iter().any(|flag| flag["global"] != true) {
            failures.push(format!(
                "commands {path}: an own flag after the global ones"
            ));
        }
        let binds = entry["binds"] == true;
        let want: Vec<String> = program_wide
            .iter()
            .filter(|long| binds || long.as_str() != "if-match")
            .cloned()
            .collect();
        let got: Vec<String> = flags[first..]
            .iter()
            .map(|flag| text(&flag["long"]))
            .collect();
        same_names(
            &mut failures,
            &format!("commands {path}: globals"),
            &got,
            &want,
        );
    }

    // Error codes, warning codes and actions.
    let want: Vec<String> = ErrorCode::ALL
        .iter()
        .map(|code| code.wire().to_owned())
        .collect();
    same_names(
        &mut failures,
        "error_codes",
        &names(data, "error_codes", "code"),
        &want,
    );
    for entry in list(data, "error_codes") {
        let Some(code) = ErrorCode::ALL
            .iter()
            .find(|code| entry["code"] == code.wire())
        else {
            continue;
        };
        let label = |key: &str| format!("error_codes {}: {key}", code.wire());
        same_value(
            &mut failures,
            &label("exit"),
            &entry["exit"],
            &json!(code.exit().code()),
        );
        same_value(
            &mut failures,
            &label("doctor_exit"),
            &entry["doctor_exit"],
            &json!(omaddons::doctor::exit::exit_for(*code).code()),
        );
        same_value(
            &mut failures,
            &label("retryable"),
            &entry["retryable"],
            &wire(code.retryability()),
        );
        same_value(
            &mut failures,
            &label("meaning"),
            &entry["meaning"],
            &json!(code.meaning()),
        );
        same_value(
            &mut failures,
            &label("details_schema"),
            &entry["details_schema"],
            &code.details_schema(),
        );
    }
    let want: Vec<String> = WarningCode::ALL
        .iter()
        .map(|code| code.wire().to_owned())
        .collect();
    same_names(
        &mut failures,
        "warning_codes",
        &names(data, "warning_codes", "code"),
        &want,
    );
    for (entry, code) in list(data, "warning_codes").iter().zip(WarningCode::ALL) {
        same_value(
            &mut failures,
            &format!("warning_codes {}: meaning", code.wire()),
            &entry["meaning"],
            &json!(code.meaning()),
        );
    }
    let want: Vec<String> = Action::ALL
        .iter()
        .map(|action| action.wire().to_owned())
        .collect();
    same_names(
        &mut failures,
        "actions",
        &names(data, "actions", "action"),
        &want,
    );
    for (entry, action) in list(data, "actions").iter().zip(Action::ALL) {
        let want = json!({
            "action": action.wire(),
            "description": action.description(),
            "destructive": action.destructive(),
        });
        same_value(
            &mut failures,
            &format!("actions {}", action.wire()),
            entry,
            &want,
        );
    }

    // Environment variables and exit dictionaries.
    let release: Vec<Value> = omaddons::contract::env::release()
        .map(
            |var| json!({"name": var.name, "description": var.description, "mirrors": var.mirrors}),
        )
        .collect();
    let want: Vec<String> = release.iter().map(|var| text(&var["name"])).collect();
    same_names(
        &mut failures,
        "env_vars",
        &names(data, "env_vars", "name"),
        &want,
    );
    same_value(
        &mut failures,
        "env_vars",
        &data["env_vars"],
        &json!(release),
    );
    let general: Vec<Value> = Exit::ALL
        .iter()
        .map(|exit| {
            json!({"code": exit.code(), "name": exit.name(), "meaning": exit.meaning(),
                   "retryable": exit.retryable()})
        })
        .collect();
    same_value(
        &mut failures,
        "exit_codes.general",
        &data["exit_codes"]["general"],
        &json!(general),
    );
    let doctor: Vec<Value> = omaddons::doctor::exit::Exit::ALL
        .iter()
        .map(|exit| json!({"code": exit.code(), "name": exit.name(), "meaning": exit.meaning()}))
        .collect();
    same_value(
        &mut failures,
        "exit_codes.doctor",
        &data["exit_codes"]["doctor"],
        &json!(doctor),
    );

    // The values config can change, by name.
    let want: Vec<String> = omaddons::politeness::REGISTRY
        .iter()
        .chain(omaddons::limits::REGISTRY)
        .map(|info| info.name.to_owned())
        .collect();
    same_names(
        &mut failures,
        "limits",
        &names(data, "limits", "name"),
        &want,
    );
    let want: Vec<String> = SourceId::ALL
        .iter()
        .map(|id| id.wire().to_owned())
        .collect();
    same_names(
        &mut failures,
        "sources",
        &names(data, "sources", "id"),
        &want,
    );
    let want: Vec<String> = Family::ALL.iter().map(text).collect();
    same_names(
        &mut failures,
        "families",
        &names(data, "families", "name"),
        &want,
    );
    let table = RulesTable::builtin();
    let want: Vec<String> = table
        .client_rules()
        .iter()
        .chain([
            table.fallback(InstallKind::Battlenet),
            table.fallback(InstallKind::Custom),
        ])
        .map(|rules| rules.name.clone())
        .collect();
    same_names(
        &mut failures,
        "client_rules",
        &names(data, "client_rules", "row"),
        &want,
    );

    // The plain lists.
    let forms: Vec<String> = omaddons::contract::target_id_forms()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let got: Vec<String> = list(data, "target_id_forms").iter().map(text).collect();
    same_names(&mut failures, "target_id_forms", &got, &forms);
    let features: Vec<String> = omaddons::contract::features::FEATURES
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
    let got: Vec<String> = list(data, "features").iter().map(text).collect();
    same_names(&mut failures, "features", &got, &features);

    // The failure-mode summary.
    let summary = &data["failure_modes"];
    same_value(
        &mut failures,
        "failure_modes.count",
        &summary["count"],
        &json!(FAILURE_MODES.len()),
    );
    let subsystems: Vec<String> = Subsystem::ALL
        .iter()
        .filter(|subsystem| FAILURE_MODES.iter().any(|fm| fm.subsystem == **subsystem))
        .map(text)
        .collect();
    let got: Vec<String> = list(summary, "subsystems").iter().map(text).collect();
    same_names(&mut failures, "failure_modes.subsystems", &got, &subsystems);
    let row = COMMANDS.iter().any(|row| row.name == "doctor capabilities");
    if summary["command"].is_null() == row {
        failures.push(format!(
            "failure_modes.command: {} while doctor capabilities is {}a command",
            summary["command"],
            if row { "" } else { "not " }
        ));
    }
    failures
}

/// What a snapshot pins of a `capabilities --json` output, `printed` as the binary wrote
/// it (an envelope, or a bare `data` object, whose `envelope_keys` is then `null`): the
/// keys in written order, the size of each list, every command path, its own entry, the
/// pointers and `config_error`. The build id and the guide digest change with the code, so
/// they are checked for form and replaced. It takes the text because a parsed `Value`
/// sorts its keys, and the digest pins their order.
pub fn digest(printed: &str) -> Value {
    let parsed: Value = serde_json::from_str(printed).unwrap_or(Value::Null);
    let (envelope_keys, data, data_keys) = if parsed.get("ok").is_some() {
        let (envelope_keys, data_keys) = written_keys(printed, Some("data"));
        (json!(envelope_keys), parsed["data"].clone(), data_keys)
    } else {
        let (data_keys, _) = written_keys(printed, None);
        (Value::Null, parsed, data_keys)
    };
    let len = |key: &str| data[key].as_array().map_or(0, Vec::len);
    let commands = data["commands"].as_array().cloned().unwrap_or_default();
    let hex = |text: &str, digits: usize| {
        text.len() == digits && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let build_id = data["build_id"].as_str().unwrap_or_default();
    assert!(hex(build_id, 16), "build_id {build_id:?}");
    let guide_digest = data["guide_digest"].as_str().unwrap_or_default();
    assert!(
        guide_digest
            .strip_prefix("sha256:")
            .is_some_and(|digest| hex(digest, 64)),
        "guide_digest {guide_digest:?}"
    );
    json!({
        "envelope_keys": envelope_keys,
        "data_keys": data_keys,
        "build_id": "<build_id>",
        "guide_digest": "<guide_digest>",
        "counts": {
            "commands": len("commands"),
            "error_codes": len("error_codes"),
            "warning_codes": len("warning_codes"),
            "actions": len("actions"),
            "env_vars": len("env_vars"),
            "limits": len("limits"),
            "sources": len("sources"),
            "families": len("families"),
            "client_rules": len("client_rules"),
            "target_id_forms": len("target_id_forms"),
            "features": len("features"),
        },
        "paths": commands.iter().map(|entry| &entry["path"]).collect::<Vec<_>>(),
        "capabilities_entry": commands
            .iter()
            .find(|entry| entry["path"] == "capabilities")
            .cloned()
            .unwrap_or(Value::Null),
        "pointers": {
            "schemas_uri": data["schemas_uri"],
            "robot_docs_uri": data["robot_docs_uri"],
            "failure_modes_command": data["failure_modes"]["command"],
        },
        "config_error": data["config_error"],
    })
}

/// The keys of the JSON object `text` in written order, and those of its member `inner`
/// when given.
fn written_keys(text: &str, inner: Option<&str>) -> (Vec<String>, Vec<String>) {
    use serde::Deserialize;
    use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};

    struct Keys(Vec<String>);
    impl<'de> Deserialize<'de> for Keys {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Keys;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        map.next_value::<IgnoredAny>()?;
                        keys.push(key);
                    }
                    Ok(Keys(keys))
                }
            }
            d.deserialize_map(V)
        }
    }

    struct Outer<'a>(Option<&'a str>);
    impl<'de> DeserializeSeed<'de> for Outer<'_> {
        type Value = (Vec<String>, Vec<String>);
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            struct V<'a>(Option<&'a str>);
            impl<'de> Visitor<'de> for V<'_> {
                type Value = (Vec<String>, Vec<String>);
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                    let (mut keys, mut inner) = (Vec::new(), Vec::new());
                    while let Some(key) = map.next_key::<String>()? {
                        // ubs:ignore — comparing a JSON key, not a secret
                        if Some(key.as_str()) == self.0 {
                            inner = map.next_value::<Keys>()?.0;
                        } else {
                            map.next_value::<IgnoredAny>()?;
                        }
                        keys.push(key);
                    }
                    Ok((keys, inner))
                }
            }
            d.deserialize_map(V(self.0))
        }
    }

    let mut deserializer = serde_json::Deserializer::from_str(text);
    Outer(inner)
        .deserialize(&mut deserializer)
        .unwrap_or_else(|e| panic!("not a JSON object ({e}):\n{text}")) // ubs:ignore — the test's own failure
}
