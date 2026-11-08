//! `omaddons capabilities`: this build, described for agents (vision §5.10, M4-D4).
//! Offline, scan-free and write-free: the live clap tree, the registries and config.toml.
//!
//! Everything comes from [`Registry::build`] and the static registries, never from a list
//! kept by hand (M4-D2). It reads no discovery, no target registry and no state, sends no
//! request, runs no credential command and writes nothing. A `config.toml` it cannot use
//! still answers, with the built-in values and `config_error`.

use std::io::Write;

use schemars::JsonSchema;
use serde::Serialize;

use super::{COMMANDS, Dictionary, Group, Network};
use crate::contract::Registry;
use crate::contract::actions::Action;
use crate::contract::commands::{ArgInfo, CommandInfo, FlagInfo, Values};
use crate::contract::effective::{
    EffectiveFamily, EffectiveLimit, EffectiveRules, EffectiveSource, ValueSource,
};
use crate::contract::schema::SchemaDoc;
use crate::doctor::finding::Subsystem;
use crate::doctor::registry::FAILURE_MODES;
use crate::error::{Error, ErrorCode, Exit, Result, Retryability, Warning, WarningCode};
use crate::limits::LimitValue;
use crate::net::allowlist::hosts_for;
use crate::output::{Outcome, Style};
use crate::rules::{ClientVersion, Family, InstallKind};
use crate::sources::SourceId;
use crate::state::config::ConfigProblem;

/// The envelope `command`.
const COMMAND: &str = "capabilities";

/// The human form's limits: at most this many lines, each narrower than this many columns.
const MAX_LINES: usize = 40;
const MAX_COLUMNS: usize = 80;
/// A `config_error` message longer than this is cut in the human form (the JSON keeps it
/// whole), so a long parser message cannot push the summary past [`MAX_LINES`].
const MESSAGE_MAX_CHARS: usize = 160;

/// `capabilities` takes no arguments of its own.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CapabilitiesArgs {}

/// What `capabilities` returns.
#[derive(Debug)]
pub struct CapabilitiesOutcome {
    data: CapabilitiesData,
    warnings: Vec<Warning>,
    text: String,
}

/// This build, described: its commands and their flags, its exit dictionaries, codes,
/// actions, environment variables, limits, sources, families and load rules, as in effect.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CapabilitiesData {
    /// This binary's version (`tool_version`).
    pub version: &'static str,
    /// The JSON contract's version: every envelope's `schema_version`.
    pub contract_version: &'static str,
    /// This build's id: 16 hex digits, the same in every run of one binary. A stored plan
    /// records it, and `apply` refuses a plan another build made.
    pub build_id: &'static str,
    /// The agent guide's digest, `sha256:` and 64 hex digits, as its stamp line writes it
    /// (`omaddons robot-docs guide`).
    pub guide_digest: String,
    /// Feature words, one per surface this build has.
    pub features: Vec<&'static str>,
    /// Every command path, in help's order.
    pub commands: Vec<CommandDescription>,
    pub exit_codes: ExitDictionaries,
    /// The environment variables a release build reads.
    pub env_vars: Vec<EnvVarDescription>,
    /// Every limit and politeness constant, as in effect.
    pub limits: Vec<LimitDescription>,
    pub sources: Vec<SourceDescription>,
    /// Every game family and its interface range, as in effect.
    pub families: Vec<FamilyDescription>,
    /// Every client-rules row, then the Battle.net and custom fallbacks, as in effect.
    pub client_rules: Vec<ClientRulesDescription>,
    /// The forms `-t` accepts, in selector precedence, then the `all` keyword.
    pub target_id_forms: Vec<&'static str>,
    pub error_codes: Vec<ErrorCodeDescription>,
    pub warning_codes: Vec<WarningCodeDescription>,
    /// Every `commands[].action` an envelope may carry.
    pub actions: Vec<ActionDescription>,
    pub failure_modes: FailureModeSummary,
    /// The command that prints every published JSON Schema; `null` in a build without it.
    pub schemas_uri: Option<String>,
    /// The command that prints the agent guide; `null` in a build without it.
    pub robot_docs_uri: Option<String>,
    /// Why `config.toml` could not be used; every value above is then the built-in one.
    /// `null` when it was used, or there is none.
    pub config_error: Option<ConfigErrorDescription>,
}

/// One command path.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CommandDescription {
    /// The path, as the envelope's `command` names it (`backup restore`).
    pub path: &'static str,
    pub description: String,
    pub group: Group,
    /// Whether some form of it changes state.
    pub mutates: bool,
    /// Always `true`: every command prints one envelope with `--json`.
    pub json: bool,
    /// Which exit dictionary its exit codes come from.
    pub dictionary: Dictionary,
    /// Every process exit it can return, ascending.
    pub exit_codes: &'static [u8],
    /// Whether its dry run stores a plan that `apply` runs.
    pub stores_plan: bool,
    /// Whether a run binds to its preview with `--if-match`.
    pub binds: bool,
    pub network: Network,
    pub aliases: Vec<String>,
    /// Its positional arguments, in order.
    pub args: Vec<ArgDescription>,
    /// Its own flags, then the global flags.
    pub flags: Vec<FlagDescription>,
    /// The command that prints its data's JSON Schema; `null` in a build without it.
    pub data_schema: Option<String>,
}

/// A positional argument.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ArgDescription {
    pub name: String,
    pub value_name: String,
    pub required: bool,
    /// It takes several values.
    pub multiple: bool,
    pub default: Option<String>,
    /// Exactly the values it accepts; `null` when it takes any text.
    pub possible_values: Option<Vec<String>>,
}

/// A flag.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FlagDescription {
    /// Without dashes.
    pub long: String,
    /// One character; `null` when it has none.
    pub short: Option<String>,
    /// Other long names it answers to, without dashes.
    pub aliases: Vec<String>,
    /// `null` for a flag that takes no value.
    pub value_name: Option<String>,
    pub takes_value: bool,
    /// The command is refused without it.
    pub required: bool,
    /// It may be given more than once, or takes several values.
    pub multiple: bool,
    pub default: Option<String>,
    /// Exactly the values it accepts; `null` when it takes no value or any text.
    pub possible_values: Option<Vec<String>>,
    /// The flags and arguments it cannot be given with.
    pub conflicts_with: Vec<String>,
    /// The flags it needs.
    pub requires: Vec<String>,
    /// A global flag: it works before or after the command.
    pub global: bool,
    pub description: String,
}

/// Both exit dictionaries.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ExitDictionaries {
    /// Every command's but the doctor's.
    pub general: Vec<GeneralExit>,
    /// The `doctor` commands'.
    pub doctor: Vec<DoctorExit>,
}

/// One general exit code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GeneralExit {
    pub code: u8,
    pub name: &'static str,
    pub meaning: &'static str,
    /// `null` for success, and for `failed`, where `error.retryable` decides.
    pub retryable: Option<bool>,
}

/// One doctor exit code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DoctorExit {
    pub code: u8,
    pub name: &'static str,
    pub meaning: &'static str,
}

/// One environment variable.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EnvVarDescription {
    pub name: &'static str,
    pub description: &'static str,
    /// The global flag it stands for; `null` when it stands for none.
    pub mirrors: Option<&'static str>,
}

/// One limit or politeness constant.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LimitDescription {
    /// The constant's name (`DOWNLOAD_MAX_BYTES`).
    pub name: &'static str,
    /// The value in effect, its JSON type following `unit`.
    pub value: PublishedValue,
    /// `bytes`, `ms`, `count`, `ratio` or `chars` (an integer), `text` (a string), `list`
    /// (an array of strings) or `retention` (an object).
    pub unit: crate::limits::LimitUnit,
    /// The `config.toml` key that changes it (the one `omaddons config set` takes);
    /// `null` for a constant config cannot change.
    pub config_key: Option<&'static str>,
    pub source: ValueSource,
}

/// A limit's value, bare: its JSON type is what `unit` says.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum PublishedValue {
    Integer(u64),
    Text(&'static str),
    List(&'static [&'static str]),
    Retention {
        min_age_ms: u64,
        keep_last: u64,
        window_ms: u64,
    },
}

/// One source of addons.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SourceDescription {
    pub id: SourceId,
    /// In this build and not turned off in `config.toml`.
    pub enabled: bool,
    pub requires_credential: bool,
    /// A credential is configured: present, not proven valid (`omaddons auth github`
    /// checks GitHub's).
    pub credential_configured: bool,
    /// The hosts it may contact.
    pub hosts: Vec<String>,
    pub source: ValueSource,
}

/// One game family.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FamilyDescription {
    pub name: Family,
    /// `null` for a family with no range (`unknown`).
    pub interface_min: Option<u32>,
    /// `null` when the range has no upper bound, or there is none.
    pub interface_max: Option<u32>,
    pub legacy: bool,
    pub source: ValueSource,
}

/// One client-rules row: what a client generation loads.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ClientRulesDescription {
    /// The row's name (`mainline-12.0.7`), as `[client_rules.<row>]` patches it.
    pub row: String,
    pub family: Family,
    /// `null` when the row applies to every install kind.
    pub install_kind: Option<InstallKind>,
    pub version_min: ClientVersion,
    /// Inclusive; `null` when unbounded.
    pub version_max: Option<ClientVersion>,
    pub game_types: Vec<String>,
    pub multi_toc: bool,
    pub comma_interface: bool,
    pub game_suffixes: Vec<String>,
    pub family_suffixes: Vec<String>,
    pub legacy_suffixes: Vec<String>,
    pub directive_conditionals: bool,
    pub interface_includes_patch: bool,
    pub legacy: bool,
    pub reload_rereads_toc: bool,
    /// The lowest interface such a client loads at all; `null` when there is no floor.
    pub interface_floor: Option<u32>,
    pub source: ValueSource,
}

/// One error code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorCodeDescription {
    pub code: ErrorCode,
    /// The general exit code of every command.
    pub exit: u8,
    /// The exit code of every `doctor` command, from the doctor's own dictionary.
    pub doctor_exit: u8,
    /// `true`, `false`, or `"conditional"` when each error's own `retryable` decides.
    pub retryable: Retryability,
    /// What the code reports, in one sentence.
    pub meaning: &'static str,
    /// The code's typed `details`, as a JSON Schema fragment.
    pub details_schema: SchemaDoc,
}

/// One warning code.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WarningCodeDescription {
    pub code: WarningCode,
    /// What the warning reports, in one sentence.
    pub meaning: &'static str,
}

/// One recommended next command's action.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ActionDescription {
    pub action: Action,
    pub description: &'static str,
    /// Whether its hint is destructive by default.
    pub destructive: bool,
}

/// The doctor's failure-mode registry, summarized.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FailureModeSummary {
    /// How many failure modes are registered.
    pub count: usize,
    /// The subsystems they belong to, each once. `omaddons doctor explain <id>` describes
    /// one in full; no command lists them all.
    pub subsystems: Vec<Subsystem>,
}

/// Why `config.toml` could not be used.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfigErrorDescription {
    /// The file; `null` when HOME gives no config path at all.
    pub path: Option<String>,
    /// 1-based; `null` when the file could not be read at all.
    pub line: Option<u64>,
    /// 1-based; `null` when the file could not be read at all.
    pub column: Option<u64>,
    pub message: String,
}

impl Outcome for CapabilitiesOutcome {
    type Data = CapabilitiesData;

    fn command(&self) -> &'static str {
        COMMAND
    }

    fn data(&self) -> &CapabilitiesData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        w.write_all(self.text.as_bytes())
    }
}

/// Runs `capabilities`: this machine's registry, described. It reads nothing the context
/// holds, so `-t` and the other globals change nothing.
pub fn run() -> Result<CapabilitiesOutcome> {
    let registry = Registry::build();
    let data = data_of(&registry)?;
    let text = render(&data);
    Ok(CapabilitiesOutcome {
        data,
        warnings: registry.warnings,
        text,
    })
}

/// The data of `registry`. Its one failure is `internal`: a code's `details`
/// specification that renders to no JSON Schema.
pub fn data_of(registry: &Registry) -> Result<CapabilitiesData> {
    let mut error_codes = Vec::with_capacity(ErrorCode::ALL.len());
    for code in ErrorCode::ALL {
        let details = schemars::Schema::try_from(code.details_schema()).map_err(|e| {
            Error::internal(format!("{code}: its details schema is no schema: {e}"))
        })?;
        error_codes.push(ErrorCodeDescription {
            code: *code,
            exit: code.exit().code(),
            doctor_exit: crate::doctor::exit::exit_for(*code).code(),
            retryable: code.retryability(),
            meaning: code.meaning(),
            details_schema: SchemaDoc(details),
        });
    }
    let effective = &registry.effective;
    Ok(CapabilitiesData {
        version: env!("CARGO_PKG_VERSION"),
        contract_version: crate::output::SCHEMA_VERSION,
        build_id: crate::build_info::BUILD_ID,
        guide_digest: format!("sha256:{}", crate::contract::guide::EXPECTED_DIGEST),
        features: crate::contract::features::FEATURES.to_vec(),
        commands: registry.commands.iter().map(command_description).collect(),
        exit_codes: ExitDictionaries {
            general: Exit::ALL
                .iter()
                .map(|exit| GeneralExit {
                    code: exit.code(),
                    name: exit.name(),
                    meaning: exit.meaning(),
                    retryable: exit.retryable(),
                })
                .collect(),
            doctor: crate::doctor::exit::Exit::ALL
                .iter()
                .map(|exit| DoctorExit {
                    code: exit.code(),
                    name: exit.name(),
                    meaning: exit.meaning(),
                })
                .collect(),
        },
        env_vars: crate::contract::env::release()
            .map(|var| EnvVarDescription {
                name: var.name,
                description: var.description,
                mirrors: var.mirrors,
            })
            .collect(),
        limits: effective.limits.iter().map(limit_description).collect(),
        sources: effective.sources.iter().map(source_description).collect(),
        families: effective.families.iter().map(family_description).collect(),
        client_rules: effective
            .client_rules
            .iter()
            .map(rules_description)
            .collect(),
        target_id_forms: crate::contract::target_id_forms(),
        error_codes,
        warning_codes: WarningCode::ALL
            .iter()
            .map(|code| WarningCodeDescription {
                code: *code,
                meaning: code.meaning(),
            })
            .collect(),
        actions: Action::ALL
            .iter()
            .map(|action| ActionDescription {
                action: *action,
                description: action.description(),
                destructive: action.destructive(),
            })
            .collect(),
        failure_modes: FailureModeSummary {
            count: FAILURE_MODES.len(),
            subsystems: Subsystem::ALL
                .iter()
                .copied()
                .filter(|subsystem| FAILURE_MODES.iter().any(|fm| fm.subsystem == *subsystem))
                .collect(),
        },
        schemas_uri: pointer(&["robot-docs", "schema"], "robot-docs schema"),
        robot_docs_uri: pointer(&["robot-docs", "guide"], "robot-docs guide"),
        config_error: registry.config_error.as_ref().map(config_error_description),
    })
}

/// `omaddons <words…>` as a runnable display string, when `path` is a command this build
/// has; `None` otherwise, so capabilities never names a command the build lacks.
fn pointer(words: &[&str], path: &str) -> Option<String> {
    COMMANDS.iter().any(|row| row.name == path).then(|| {
        let argv: Vec<&str> = std::iter::once("omaddons")
            .chain(words.iter().copied())
            .collect();
        shell_words::join(argv)
    })
}

fn command_description(info: &CommandInfo) -> CommandDescription {
    // `robot-docs schema` takes the path as words, as `help` does.
    let schema_words: Vec<&str> = ["robot-docs", "schema"]
        .into_iter()
        .chain(info.path.split(' '))
        .collect();
    CommandDescription {
        path: info.path,
        description: info.about.clone(),
        group: info.group,
        mutates: info.mutates,
        json: true,
        dictionary: info.dictionary,
        exit_codes: info.exits,
        stores_plan: info.stores_plan,
        binds: info.binds,
        network: info.network,
        aliases: info.aliases.clone(),
        args: info.args.iter().map(arg_description).collect(),
        flags: info.flags.iter().map(flag_description).collect(),
        data_schema: pointer(&schema_words, "robot-docs schema"),
    }
}

/// Exactly the accepted words, when there is such a list.
fn possible_values(values: &Values) -> Option<Vec<String>> {
    match values {
        Values::Words(words) => Some(words.clone()),
        Values::Any | Values::DurationOrTime(_) => None,
    }
}

fn arg_description(arg: &ArgInfo) -> ArgDescription {
    ArgDescription {
        name: arg.id.clone(),
        value_name: arg.value_name.clone(),
        required: arg.required,
        multiple: arg.multiple,
        default: arg.default.clone(),
        possible_values: possible_values(&arg.values),
    }
}

fn flag_description(flag: &FlagInfo) -> FlagDescription {
    FlagDescription {
        long: flag.long.clone(),
        short: flag.short.map(String::from),
        aliases: flag.aliases.clone(),
        value_name: flag.value_name.clone(),
        takes_value: flag.takes_value,
        required: flag.required,
        multiple: flag.multiple,
        default: flag.default.clone(),
        possible_values: possible_values(&flag.values),
        conflicts_with: flag.conflicts_with.clone(),
        requires: flag.requires.clone(),
        global: flag.global,
        description: flag.help.clone(),
    }
}

fn limit_description(limit: &EffectiveLimit) -> LimitDescription {
    LimitDescription {
        name: limit.info.name,
        value: published(limit.value),
        unit: limit.info.unit,
        config_key: limit.config_key,
        source: limit.source,
    }
}

/// A limit's value as capabilities publishes it: bare, by unit.
const fn published(value: LimitValue) -> PublishedValue {
    match value {
        LimitValue::Count(n)
        | LimitValue::Bytes(n)
        | LimitValue::Millis(n)
        | LimitValue::Ratio(n) => PublishedValue::Integer(n),
        LimitValue::Text(text) => PublishedValue::Text(text),
        LimitValue::List(list) => PublishedValue::List(list),
        LimitValue::Retention {
            min_age_ms,
            keep_last,
            window_ms,
        } => PublishedValue::Retention {
            min_age_ms,
            keep_last,
            window_ms,
        },
    }
}

fn source_description(source: &EffectiveSource) -> SourceDescription {
    SourceDescription {
        id: source.id,
        enabled: source.enabled,
        requires_credential: source.requires_credential,
        credential_configured: source.credential_configured,
        hosts: source.hosts.clone(),
        source: source.source,
    }
}

fn family_description(family: &EffectiveFamily) -> FamilyDescription {
    FamilyDescription {
        name: family.family,
        interface_min: family.range.map(|range| range.min),
        interface_max: family.range.and_then(|range| range.max),
        legacy: family.legacy,
        source: family.source,
    }
}

fn rules_description(effective: &EffectiveRules) -> ClientRulesDescription {
    let rules = &effective.rules;
    ClientRulesDescription {
        row: rules.name.clone(),
        family: rules.family,
        install_kind: rules.install_kind,
        version_min: rules.version_min,
        version_max: rules.version_max,
        game_types: rules.game_types.clone(),
        multi_toc: rules.multi_toc,
        comma_interface: rules.comma_interface,
        game_suffixes: rules.game_suffixes.clone(),
        family_suffixes: rules.family_suffixes.clone(),
        legacy_suffixes: rules.legacy_suffixes.clone(),
        directive_conditionals: rules.directive_conditionals,
        interface_includes_patch: rules.interface_includes_patch,
        legacy: rules.legacy,
        reload_rereads_toc: rules.reload_rereads_toc,
        // 0 is no floor too, as the row's own rule reads it.
        interface_floor: rules.interface_floor.filter(|floor| *floor != 0),
        source: effective.source,
    }
}

fn config_error_description(problem: &ConfigProblem) -> ConfigErrorDescription {
    ConfigErrorDescription {
        path: problem.path.as_ref().map(|path| path.display().to_string()),
        line: problem.line,
        column: problem.column,
        message: problem.message.clone(),
    }
}

/// The human summary: at most [`MAX_LINES`] lines, each narrower than [`MAX_COLUMNS`], that
/// never print a path or a credential.
fn render(data: &CapabilitiesData) -> String {
    let mut lines = vec![format!(
        "omaddons {} · contract {} · build {}",
        data.version, data.contract_version, data.build_id
    )];
    // ubs:ignore — comparing command groups, not secrets
    let in_group = |group: Group| data.commands.iter().filter(|c| c.group == group).count();
    lines.push(format!(
        "{} commands: look {} · change {} · setup {} (omaddons help <command> for one)",
        data.commands.len(),
        in_group(Group::Look),
        in_group(Group::Change),
        in_group(Group::Setup),
    ));
    lines.push("Exit codes:".to_owned());
    lines.extend(super::help::exit_table().lines().map(str::to_owned));
    if !data.exit_codes.doctor.is_empty() {
        let items: Vec<String> = data
            .exit_codes
            .doctor
            .iter()
            .map(|exit| format!("{} {}", exit.code, exit.name))
            .collect();
        lines.extend(wrap("Doctor exit codes: ", &items));
    }
    lines.push("Sources:".to_owned());
    lines.extend(data.sources.iter().map(source_line));
    let from_config = data
        .limits
        .iter()
        .filter(|limit| limit.source == ValueSource::Config)
        .count();
    let counts = [
        format!("{} error codes", data.error_codes.len()),
        format!("{} warning codes", data.warning_codes.len()),
        format!("{} actions", data.actions.len()),
        format!("{} failure modes", data.failure_modes.count),
        format!("{} limits ({from_config} from config)", data.limits.len()),
    ];
    lines.extend(wrap("", &counts));
    if let Some(error) = &data.config_error {
        lines.extend(config_error_lines(error));
    }
    lines.push("--json for the full description".to_owned());
    debug_assert!(lines.len() <= MAX_LINES, "{} lines", lines.len());
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// A source's line: its id, `on` or `off`, then what sets it apart.
fn source_line(source: &SourceDescription) -> String {
    let mut notes: Vec<String> = Vec::new();
    if let Some(milestone) = source.id.later() {
        notes.push(format!("needs a key ({milestone})"));
    } else if source.credential_configured {
        notes.push("token configured".to_owned());
    }
    let builtin = hosts_for(source.id);
    let added: Vec<&str> = source
        .hosts
        .iter()
        .map(String::as_str)
        .filter(|host| !builtin.contains(host))
        .collect();
    let state = if source.enabled { "on" } else { "off" };
    let line = |hosts: &str| {
        let mut notes = notes.clone();
        if !hosts.is_empty() {
            notes.push(hosts.to_owned());
        }
        if source.source == ValueSource::Config {
            notes.push("(config)".to_owned());
        }
        format!(
            "  {:<10}  {state:<3}  {}",
            source.id.wire(),
            notes.join(" ")
        )
        .trim_end()
        .to_owned()
    };
    if added.is_empty() {
        return line("");
    }
    let listed = line(&format!("+ {}", added.join(", ")));
    if listed.chars().count() < MAX_COLUMNS {
        return listed;
    }
    let counted = match added.len() {
        1 => "+ 1 host".to_owned(),
        n => format!("+ {n} hosts"),
    };
    line(&counted)
}

/// The `config.toml` problem in words, wrapped, never with the file's path.
fn config_error_lines(error: &ConfigErrorDescription) -> Vec<String> {
    if error.path.is_none() {
        return vec!["HOME is not an absolute path; built-in values shown".to_owned()];
    }
    let mut message: String = error
        .message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if message.chars().count() > MESSAGE_MAX_CHARS {
        message = message.chars().take(MESSAGE_MAX_CHARS - 1).collect();
        message.push('…');
    }
    let at = match (error.line, error.column) {
        (Some(line), Some(column)) => format!("config.toml line {line}, column {column}"),
        (Some(line), None) => format!("config.toml line {line}"),
        _ => "config.toml".to_owned(),
    };
    let sentence = format!("{at}: {message}; built-in values shown");
    // A word no line could hold (a long quoted value) is cut into pieces that fit.
    let width = MAX_COLUMNS - 3;
    let words: Vec<String> = sentence
        .split(' ')
        .flat_map(|word| {
            let chars: Vec<char> = word.chars().collect();
            chars
                .chunks(width)
                .map(|piece| piece.iter().collect::<String>())
                .collect::<Vec<_>>()
        })
        .collect();
    wrap_with("", &words, " ", 2)
}

/// `items` after `prefix`, joined with ` · `, wrapped under the first item (or two
/// columns in, without a prefix) so no line reaches [`MAX_COLUMNS`].
fn wrap(prefix: &str, items: &[String]) -> Vec<String> {
    let indent = match prefix.chars().count() {
        0 => 2,
        width => width,
    };
    wrap_with(prefix, items, " · ", indent)
}

/// `items` joined with `separator`, a new line (indented by `indent`) begun before an item
/// that would reach [`MAX_COLUMNS`]; `first` is the first line's start. An item wider than
/// a line is put on its own.
fn wrap_with(first: &str, items: &[String], separator: &str, indent: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = first.to_owned();
    let mut empty = true;
    for item in items {
        let candidate = if empty {
            format!("{line}{item}")
        } else {
            format!("{line}{separator}{item}")
        };
        if empty || candidate.chars().count() < MAX_COLUMNS {
            line = candidate;
        } else {
            lines.push(line);
            line = format!("{:indent$}{item}", "");
        }
        empty = false;
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser as _;
    use serde::Deserialize as _;
    use serde_json::Value;

    use super::*;
    use crate::app::precondition::BINDING_COMMANDS;
    use crate::cli::Cli;
    use crate::state::config::{Config, LoadedConfig, TolerantConfig};

    /// The data keys, in the order the data is written.
    const EXPECTED_KEYS: &[&str] = &[
        "version",
        "contract_version",
        "build_id",
        "guide_digest",
        "features",
        "commands",
        "exit_codes",
        "env_vars",
        "limits",
        "sources",
        "families",
        "client_rules",
        "target_id_forms",
        "error_codes",
        "warning_codes",
        "actions",
        "failure_modes",
        "schemas_uri",
        "robot_docs_uri",
        "config_error",
    ];

    fn builtin() -> CapabilitiesData {
        data_of(&Registry::builtin()).unwrap()
    }

    /// The registry of config.toml `text`, `credential` saying which sources have one.
    fn configured(text: &str, credential: &dyn Fn(SourceId) -> bool) -> Registry {
        let (config, warnings) = Config::parse(text, Path::new("/x/config.toml")).unwrap();
        let loaded = LoadedConfig {
            config,
            warnings,
            path: PathBuf::from("/x/config.toml"),
            exists: true,
            text: Some(text.to_owned()),
        };
        Registry::from_parts(
            TolerantConfig {
                loaded,
                problem: None,
            },
            credential,
        )
    }

    /// The registry of a config.toml that could not be used, for `problem`.
    fn broken(problem: ConfigProblem) -> Registry {
        let mut config = TolerantConfig::default_for_tests();
        config.problem = Some(problem);
        Registry::from_parts(config, &|_| false)
    }

    /// The top-level keys of the JSON object `text`, in written order.
    fn written_keys(text: &str) -> Vec<String> {
        struct Keys(Vec<String>);
        impl<'de> serde::Deserialize<'de> for Keys {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Self, D::Error> {
                struct V;
                impl<'de> serde::de::Visitor<'de> for V {
                    type Value = Keys;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a JSON object")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(
                        self,
                        mut map: A,
                    ) -> std::result::Result<Keys, A::Error> {
                        let mut keys = Vec::new();
                        while let Some(key) = map.next_key::<String>()? {
                            map.next_value::<serde::de::IgnoredAny>()?;
                            keys.push(key);
                        }
                        Ok(Keys(keys))
                    }
                }
                d.deserialize_map(V)
            }
        }
        Keys::deserialize(&mut serde_json::Deserializer::from_str(text))
            .unwrap()
            .0
    }

    #[test]
    fn data_keys_are_in_the_published_order() {
        let text = serde_json::to_string(&builtin()).unwrap();
        assert_eq!(written_keys(&text), EXPECTED_KEYS);
    }

    #[test]
    fn every_command_entry_carries_its_registry_facts() {
        let data = builtin();
        assert_eq!(data.commands.len(), COMMANDS.len());
        for (entry, row) in data.commands.iter().zip(COMMANDS) {
            assert_eq!(entry.path, row.name);
            let got = (
                entry.group,
                entry.mutates,
                entry.dictionary,
                entry.exit_codes,
                entry.stores_plan,
                entry.network,
                entry.binds,
                entry.json,
            );
            let want = (
                row.group,
                row.mutates,
                row.dictionary,
                row.exits,
                row.stores_plan,
                row.network,
                BINDING_COMMANDS.contains(&row.name),
                true,
            );
            assert_eq!(got, want, "{}", row.name);
            assert!(
                !entry.description.is_empty(),
                "{}: no description",
                row.name
            );
        }
    }

    #[test]
    fn possible_values_are_published_for_word_lists_only() {
        let data = builtin();
        let flag = |path: &str, long: &str| {
            data.commands
                .iter()
                .find(|c| c.path == path)
                .and_then(|c| c.flags.iter().find(|f| f.long == long))
                .unwrap_or_else(|| panic!("{path} --{long}")) // ubs:ignore — test lookup
        };
        assert_eq!(
            flag("install", "channel").possible_values,
            Some(vec![
                "stable".to_owned(),
                "beta".to_owned(),
                "alpha".to_owned()
            ])
        );
        // An open grammar and free text have no list.
        assert_eq!(flag("history", "since").possible_values, None);
        assert_eq!(flag("install", "target").possible_values, None);
        let target = flag("install", "target");
        assert_eq!(
            (target.short.as_deref(), target.global, &target.aliases),
            (Some("t"), true, &vec!["flavor".to_owned()])
        );
    }

    #[test]
    fn pointers_are_null_exactly_when_their_command_is_missing() {
        let data = builtin();
        let has = |path: &str| COMMANDS.iter().any(|row| row.name == path);
        let cases: [(&str, &Option<String>, &str); 2] = [
            ("schemas_uri", &data.schemas_uri, "robot-docs schema"),
            ("robot_docs_uri", &data.robot_docs_uri, "robot-docs guide"),
        ];
        let mut pointers: Vec<(String, &Option<String>)> = cases
            .iter()
            .map(|(name, pointer, path)| {
                assert_eq!(pointer.is_some(), has(path), "{name}: {pointer:?}");
                ((*name).to_owned(), *pointer)
            })
            .collect();
        for entry in &data.commands {
            assert_eq!(
                entry.data_schema.is_some(),
                has("robot-docs schema"),
                "{}",
                entry.path
            );
            pointers.push((format!("{} data_schema", entry.path), &entry.data_schema));
        }
        for (name, pointer) in pointers {
            let Some(pointer) = pointer else { continue };
            let argv = shell_words::split(pointer).unwrap();
            assert!(
                Cli::try_parse_from(&argv).is_ok(),
                "{name}: {pointer} does not parse"
            );
        }
        // ubs:ignore — comparing command paths, not secrets
        let restore = data.commands.iter().find(|c| c.path == "backup restore");
        assert_eq!(
            restore.and_then(|c| c.data_schema.as_deref()),
            Some("omaddons robot-docs schema backup restore")
        );
    }

    #[test]
    fn limit_values_publish_bare_values() {
        let data = builtin();
        let cases: [(&str, &str, Value); 6] = [
            (
                "a count",
                "ARCHIVE_MAX_ENTRIES",
                Value::from(crate::limits::ARCHIVE_MAX_ENTRIES),
            ),
            (
                "bytes",
                "DOWNLOAD_MAX_BYTES",
                Value::from(crate::limits::DOWNLOAD_MAX_BYTES),
            ),
            (
                "milliseconds",
                "DEFAULT_CHECK_INTERVAL",
                Value::from(crate::limits::ms(crate::politeness::DEFAULT_CHECK_INTERVAL)),
            ),
            (
                "text",
                "USER_AGENT",
                Value::from(crate::politeness::USER_AGENT),
            ),
            (
                "a list",
                "DISCOVERY_SKIP_DIRS",
                serde_json::json!(crate::politeness::DISCOVERY_SKIP_DIRS),
            ),
            (
                "retention",
                "TX_RETENTION",
                serde_json::json!({
                    "min_age_ms": crate::limits::ms(crate::limits::TX_RETENTION.min_age),
                    "keep_last": crate::limits::TX_RETENTION.keep_last,
                    "window_ms": crate::limits::ms(crate::limits::TX_RETENTION.window),
                }),
            ),
        ];
        for (case, name, want) in cases {
            // ubs:ignore — comparing limit names, not secrets
            let limit = data.limits.iter().find(|l| l.name == name).unwrap();
            assert_eq!(
                serde_json::to_value(&limit.value).unwrap(),
                want,
                "{case}: {name} ({:?})",
                limit.unit
            );
        }
    }

    #[test]
    fn limits_carry_config_keys_and_sources() {
        let key = |data: &CapabilitiesData, name: &str| {
            // ubs:ignore — comparing limit names, not secrets
            let limit = data.limits.iter().find(|l| l.name == name).unwrap();
            (limit.config_key, limit.source)
        };
        let data = builtin();
        let builtin = ValueSource::Builtin;
        assert_eq!(
            key(&data, "DOWNLOAD_MAX_BYTES"),
            (Some("limits.download_max_bytes"), builtin)
        );
        assert_eq!(
            key(&data, "TX_RETENTION"),
            (Some("limits.tx_retention_count"), builtin)
        );
        assert_eq!(
            key(&data, "DEFAULT_CHECK_INTERVAL"),
            (Some("check_interval_hours"), builtin)
        );
        assert_eq!(key(&data, "USER_AGENT"), (None, builtin));

        let data = data_of(&configured(
            "[limits]\ndownload_max_bytes = \"1GiB\"\n",
            &|_| false,
        ))
        .unwrap();
        let download = data
            .limits
            .iter()
            .find(|l| l.name == "DOWNLOAD_MAX_BYTES")
            .unwrap();
        assert_eq!(download.source, ValueSource::Config);
        assert_eq!(
            serde_json::to_value(&download.value).unwrap(),
            Value::from(1u64 << 30)
        );
        assert_eq!(
            data.limits
                .iter()
                .filter(|l| l.source == ValueSource::Config)
                .count(),
            1
        );
    }

    #[test]
    fn error_codes_publish_meaning_spec_and_tri_state() {
        let data = builtin();
        assert_eq!(data.error_codes.len(), ErrorCode::ALL.len());
        for (row, code) in data.error_codes.iter().zip(ErrorCode::ALL) {
            assert_eq!(row.code, *code);
            assert_eq!(row.exit, code.exit().code(), "{code}");
            assert_eq!(
                row.doctor_exit,
                crate::doctor::exit::exit_for(*code).code(),
                "{code}"
            );
            assert_eq!(row.retryable, code.retryability(), "{code}");
            assert_eq!(row.meaning, code.meaning(), "{code}");
            assert_eq!(
                serde_json::to_value(&row.details_schema).unwrap(),
                code.details_schema(),
                "{code}"
            );
        }
        let retryable = |wire: &str| {
            let row = data
                .error_codes
                .iter()
                .find(|row| row.code.wire() == wire)
                .unwrap();
            serde_json::to_value(row.retryable).unwrap()
        };
        for (wire, want) in [
            ("plan_stale", Value::from("conditional")),
            ("network", Value::from("conditional")),
            ("io", Value::from("conditional")),
            ("locked", Value::from(true)),
            ("usage", Value::from(false)),
        ] {
            assert_eq!(retryable(wire), want, "{wire}");
        }
        let doctor: Vec<u8> = data.exit_codes.doctor.iter().map(|e| e.code).collect();
        let want: Vec<u8> = crate::doctor::exit::Exit::ALL
            .iter()
            .map(|e| e.code())
            .collect();
        assert_eq!(doctor, want);
        let general: Vec<u8> = data.exit_codes.general.iter().map(|e| e.code).collect();
        assert_eq!(
            general,
            Exit::ALL.iter().map(|e| e.code()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_other_lists_mirror_their_registries() {
        let data = builtin();
        assert_eq!(data.features, crate::contract::features::FEATURES);
        assert_eq!(data.target_id_forms, crate::contract::target_id_forms());
        assert_eq!(
            data.env_vars.iter().map(|v| v.name).collect::<Vec<_>>(),
            crate::contract::env::release()
                .map(|v| v.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            data.warning_codes
                .iter()
                .map(|w| w.code)
                .collect::<Vec<_>>(),
            WarningCode::ALL.to_vec()
        );
        assert_eq!(
            data.actions.iter().map(|a| a.action).collect::<Vec<_>>(),
            Action::ALL.to_vec()
        );
        assert_eq!(
            data.sources.iter().map(|s| s.id).collect::<Vec<_>>(),
            SourceId::ALL.to_vec()
        );
        assert_eq!(
            data.families.iter().map(|f| f.name).collect::<Vec<_>>(),
            Family::ALL.to_vec()
        );
        let unknown = data
            .families
            .iter()
            .find(|f| f.name == Family::Unknown)
            .unwrap();
        assert_eq!((unknown.interface_min, unknown.interface_max), (None, None));
        let rows: Vec<&str> = data.client_rules.iter().map(|r| r.row.as_str()).collect();
        assert_eq!(rows.last(), Some(&"fallback-custom"), "{rows:?}");
        assert!(rows.contains(&"fallback-battlenet"), "{rows:?}");
        assert!(data.version.starts_with(|c: char| c.is_ascii_digit()));
        assert_eq!(data.contract_version, crate::output::SCHEMA_VERSION);
        assert_eq!(data.build_id, crate::build_info::BUILD_ID);
        assert_eq!(
            data.guide_digest,
            format!("sha256:{}", crate::contract::guide::EXPECTED_DIGEST)
        );
    }

    #[test]
    fn failure_modes_summarize_the_registry() {
        let summary = builtin().failure_modes;
        assert_eq!(summary.count, FAILURE_MODES.len());
        let order = |s: &Subsystem| Subsystem::ALL.iter().position(|a| a == s);
        assert!(
            summary
                .subsystems
                .windows(2)
                .all(|pair| order(&pair[0]) < order(&pair[1])),
            "distinct, in Subsystem::ALL order: {:?}",
            summary.subsystems
        );
        for subsystem in &summary.subsystems {
            assert!(subsystem.applicable(), "{subsystem:?}");
            assert!(
                FAILURE_MODES.iter().any(|fm| fm.subsystem == *subsystem),
                "{subsystem:?}"
            );
        }
        for fm in FAILURE_MODES {
            assert!(summary.subsystems.contains(&fm.subsystem), "{}", fm.id);
        }
    }

    #[test]
    fn config_error_is_reported_with_builtin_values() {
        let path = PathBuf::from("/home/someone/.config/omaddons/config.toml");
        let parse = Config::parse_checked("default_channel = \n", &path).unwrap_err();
        let cases = [
            (
                "a parse problem",
                parse,
                serde_json::json!({"path": path.display().to_string(), "line": 1, "column": 19}),
            ),
            (
                "an unreadable file",
                ConfigProblem::unlocated(Some(path.clone()), "Permission denied (os error 13)"),
                serde_json::json!({"path": path.display().to_string(), "line": null, "column": null}),
            ),
            (
                "no HOME",
                ConfigProblem::unlocated(None, "HOME is not set to an absolute path"),
                serde_json::json!({"path": null, "line": null, "column": null}),
            ),
        ];
        for (case, problem, want) in cases {
            let data = data_of(&broken(problem.clone())).unwrap();
            let got = serde_json::to_value(&data.config_error).unwrap();
            for key in ["path", "line", "column"] {
                assert_eq!(got[key], want[key], "{case}: {key} in {got}");
            }
            assert_eq!(got["message"], Value::from(problem.message), "{case}");
            for limit in &data.limits {
                assert_eq!(limit.source, ValueSource::Builtin, "{case}: {}", limit.name);
            }
            for source in &data.sources {
                assert_eq!(
                    source.source,
                    ValueSource::Builtin,
                    "{case}: {:?}",
                    source.id
                );
            }
        }
    }

    /// Whether `text` keeps the human form's limits; the failure names `case`.
    fn assert_fits(case: &str, text: &str) {
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.len() <= MAX_LINES,
            "{case}: {} lines\n{text}",
            lines.len()
        );
        for line in &lines {
            assert!(
                line.chars().count() < MAX_COLUMNS,
                "{case}: {} columns: {line:?}\n{text}",
                line.chars().count()
            );
        }
        assert!(
            lines.first().is_some_and(|line| line.starts_with(&format!(
                "omaddons {} · contract {}",
                env!("CARGO_PKG_VERSION"),
                crate::output::SCHEMA_VERSION
            ))),
            "{case}\n{text}"
        );
        assert_eq!(
            lines.last(),
            Some(&"--json for the full description"),
            "{case}\n{text}"
        );
        assert!(text.ends_with('\n'), "{case}");
    }

    #[test]
    fn human_form_fits() {
        let hosts: Vec<String> = (0..12)
            .map(|n| format!("\"mirror-number-{n}.some-very-long-git-host.example.org\""))
            .collect();
        let many_hosts = format!("git_hosts = [{}]\n", hosts.join(", "));
        let mut seventy = Registry::builtin();
        let groups = [Group::Look, Group::Change, Group::Setup];
        let mut n = 0;
        while seventy.commands.len() < 70 {
            let mut info = seventy.commands[n % 10].clone();
            info.group = groups[n % 3];
            seventy.commands.push(info);
            n += 1;
        }
        let long_message = ConfigProblem::unlocated(
            Some(PathBuf::from("/x/config.toml")),
            "a message\nthat runs over several lines ".repeat(20),
        );
        let cases: Vec<(&str, Registry)> = vec![
            ("the built-in registry", Registry::builtin()),
            (
                "a config error",
                broken(ConfigProblem::unlocated(
                    Some(PathBuf::from("/x/config.toml")),
                    "Is a directory (os error 21)",
                )),
            ),
            ("a long config error", broken(long_message)),
            (
                "a config error with one long word",
                broken(ConfigProblem::unlocated(
                    Some(PathBuf::from("/x/config.toml")),
                    format!("invalid value: string \"{}\"", "x".repeat(150)),
                )),
            ),
            (
                "no HOME",
                broken(ConfigProblem::unlocated(None, "HOME is not set")),
            ),
            (
                "configured overrides",
                configured(
                    "git_hosts = [\"codeberg.org\"]\n[sources.wowi]\nenabled = false\n[limits]\ndownload_max_bytes = \"1GiB\"\n",
                    &|id| id == SourceId::Github,
                ),
            ),
            ("many git hosts", configured(&many_hosts, &|_| false)),
            ("seventy commands", seventy),
        ];
        for (case, registry) in cases {
            let mut data = data_of(&registry).unwrap();
            assert_fits(case, &render(&data));
            // A doctor dictionary of eleven three-digit codes still wraps within the
            // limits.
            data.exit_codes.doctor = (0..11)
                .map(|n| DoctorExit {
                    code: 100 + n,
                    name: "a_long_doctor_exit_name",
                    meaning: "",
                })
                .collect();
            assert_fits(&format!("{case}, a long doctor dictionary"), &render(&data));
        }
    }

    #[test]
    fn human_form_names_every_general_exit_and_source() {
        let data = data_of(&configured(
            "git_hosts = [\"codeberg.org\"]\n[sources.wowi]\nenabled = false\n",
            &|id| id == SourceId::Github,
        ))
        .unwrap();
        let text = render(&data);
        for exit in Exit::ALL {
            let row = format!("  {}  {}", exit.code(), exit.name());
            assert!(text.contains(&row), "{row}\n{text}");
        }
        for source in SourceId::ALL {
            assert!(
                text.lines()
                    .any(|line| line.starts_with(&format!("  {} ", source.wire()))),
                "{}\n{text}",
                source.wire()
            );
        }
        for line in [
            "  github      on   token configured",
            "  git         on   + codeberg.org (config)",
            "  wowi        off  (config)",
            "  curseforge  off  needs a key (M5)",
        ] {
            // ubs:ignore — comparing rendered lines, not secrets
            assert!(text.lines().any(|l| l == line), "{line:?}\n{text}");
        }
        assert!(text.contains("Doctor exit codes: 0 healthy"), "{text}");
        assert!(!text.contains("config.toml"), "no config error\n{text}");
    }

    #[test]
    fn human_form_never_prints_a_path_or_credential() {
        let path = PathBuf::from("/home/someone/.config/omaddons/config.toml");
        let text = render(
            &data_of(&broken(ConfigProblem::unlocated(
                Some(path.clone()),
                "Permission denied (os error 13)",
            )))
            .unwrap(),
        );
        assert!(text.contains("config.toml: Permission denied"), "{text}");
        assert!(!text.contains("/home/someone"), "{text}");

        let registry = configured(
            "[sources.github]\ncredential = { env = \"SOME_SECRET_TOKEN_VARIABLE\" }\n",
            &|id| id == SourceId::Github,
        );
        let data = data_of(&registry).unwrap();
        let text = render(&data);
        let json = serde_json::to_string(&data).unwrap();
        for out in [&text, &json] {
            assert!(!out.contains("SOME_SECRET_TOKEN_VARIABLE"), "{out}");
        }
        assert!(text.contains("token configured"), "{text}");
    }

    #[test]
    fn config_warnings_reach_the_envelope() {
        let registry = configured("nope = 1\n", &|_| false);
        assert_eq!(registry.warnings.len(), 1);
        assert_eq!(registry.warnings[0].code, WarningCode::ConfigUnknownKey);
    }
}
