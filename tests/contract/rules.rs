//! The contract's rules over every published shape (M4-D55, rules 1–4 and 6): timestamps,
//! null versus omitted, names, closed enumerations and `details` objects, checked on every
//! command's data, the envelope frame, every error variant, every event and every file
//! kind name. The violations that predated this test were listed in
//! `tests/contract/rules_allowlist.txt` until H1 emptied it; the list is closed, and its
//! loader refuses any entry (M4-D55). Pure: no command runs.

use std::collections::BTreeSet;
use std::path::Path;

use omaddons::contract::schema::{self, Visit, walk};
use omaddons::error::ErrorCode;
use serde_json::{Value, json};

/// Every checker, with the rule it belongs to.
const RULES: &[(&str, u8)] = &[
    ("r1-at-type", 1),
    ("r1-instant-name", 1),
    ("r1-instant-type", 1),
    ("r1-duration-type", 1),
    ("r1-duration-name", 1),
    ("r2-omittable-undocumented", 2),
    ("r2-verbose-required", 2),
    ("r3-name-case", 3),
    ("r3-array-plural", 3),
    ("r3-bool-is", 3),
    ("r3-count-name", 3),
    ("r3-key-ulid", 3),
    ("r4-value-case", 4),
    ("r4-open-enum", 4),
    ("r6-details-object", 6),
];

/// A permanent, reasoned exception for a vocabulary omaddons does not own.
#[derive(Debug, Clone, Copy)]
struct Exception {
    rule: &'static str,
    /// Exact, or `<prefix> *` for that scope and every one below it (`command *` for
    /// every command, `command doctor *` for the doctor tree).
    scope: &'static str,
    /// A walk path; a leading `**.` matches at any depth.
    path: &'static str,
    /// Which vocabulary it is: the only three that may keep their own spelling.
    kind: ExceptionKind,
    reason: &'static str,
    /// Where that vocabulary is defined: a decision, a README section, an outside tool.
    source: &'static str,
}

/// The vocabularies an exception may keep (M4-D55; README, "Naming rules"). Anything else
/// that breaks a rule is renamed, or the checker is narrowed with its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExceptionKind {
    /// Defined by a contract or tool omaddons follows: the doctor contract, POSIX, clap.
    External,
    /// A spelling users type, in `-t`, `config.toml` or the catalog.
    Input,
    /// A persisted line a command echoes verbatim, whose fields change only with that
    /// file's own format (M4-D56).
    Persisted,
}

/// The doctor contract's report (M4-D34, A.4) names its severities and run states as
/// generic doctor consumers read them.
const DOCTOR_CONTRACT: &str =
    "the doctor contract's own names (M4-D34, A.4), which generic doctor consumers read";

/// Where the doctor contract's names are defined.
const DOCTOR_CONTRACT_SOURCE: &str = "M4-D34; vision D20";

/// Game-family names, which users type.
const FAMILIES: &str = "game-family names are an input vocabulary users type in `-t` and config.toml (`wrath-legacy`); allowlisted wherever `Family` appears";

/// Where the game-family names are published as input.
const FAMILIES_SOURCE: &str = "README: Families and interface ranges; Choosing a target with `-t`";

/// The net.log line `net-log` echoes in `entries[]`.
const NET_LOG_LINE: &str = "The net.log line, which `net-log` echoes verbatim; its fields change only with the line format README documents.";

/// Outside vocabularies omaddons publishes as they are.
const EXCEPTIONS: &[Exception] = &[
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P0",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P1",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P2",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P3",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r4-open-enum",
        scope: "command doctor *",
        path: "**.mode",
        kind: ExceptionKind::External,
        reason: "a POSIX permission mode, four octal digits checked by a pattern: no enumeration",
        source: "POSIX file permission modes, as chmod(1) writes them",
    },
    Exception {
        rule: "r4-value-case",
        scope: "command doctor *",
        path: "**.severity",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r4-value-case",
        scope: "command doctor *",
        path: "state",
        kind: ExceptionKind::External,
        reason: DOCTOR_CONTRACT,
        source: DOCTOR_CONTRACT_SOURCE,
    },
    // Re-judged by c6cm.8.1.4 and kept: `exit_codes` is the plural, a map whose keys are
    // the two exit dictionaries' own names, the values `commands[].dictionary` publishes.
    Exception {
        rule: "r3-array-plural",
        scope: "command capabilities",
        path: "exit_codes.doctor",
        kind: ExceptionKind::External,
        reason: "a key of the `exit_codes` map: the doctor contract's exit dictionary, by the name `commands[].dictionary` gives it",
        source: "the doctor contract's exit dictionary (M4-D40)",
    },
    Exception {
        rule: "r3-array-plural",
        scope: "command capabilities",
        path: "exit_codes.general",
        kind: ExceptionKind::External,
        reason: "a key of the `exit_codes` map: the general exit dictionary, by the name `commands[].dictionary` gives it beside `doctor`",
        source: "the doctor contract's two dictionaries (M4-D40); README: Exit codes",
    },
    Exception {
        rule: "r4-value-case",
        scope: "command *",
        path: "**.family",
        kind: ExceptionKind::Input,
        reason: FAMILIES,
        source: FAMILIES_SOURCE,
    },
    Exception {
        rule: "r4-value-case",
        scope: "command *",
        path: "**.families[]",
        kind: ExceptionKind::Input,
        reason: FAMILIES,
        source: FAMILIES_SOURCE,
    },
    Exception {
        rule: "r4-value-case",
        scope: "command *",
        path: "**.auto.mode",
        kind: ExceptionKind::Input,
        reason: "`auto_backup`'s values (`daily-copy`), which users type in config.toml and the widget compares",
        source: "README: Configuration",
    },
    Exception {
        rule: "r4-value-case",
        scope: "command status",
        path: "**.severity",
        kind: ExceptionKind::External,
        reason: "status's issues are the doctor's findings, with the doctor contract's severities `P0`–`P3`",
        source: DOCTOR_CONTRACT_SOURCE,
    },
    Exception {
        rule: "r4-open-enum",
        scope: "command history",
        path: "**.mode",
        kind: ExceptionKind::External,
        reason: "a repair's POSIX permission mode, four octal digits checked by a pattern: no enumeration",
        source: "POSIX file permission modes, as chmod(1) writes them",
    },
    Exception {
        rule: "r4-value-case",
        scope: "command capabilities",
        path: "families[].name",
        kind: ExceptionKind::Input,
        reason: FAMILIES,
        source: FAMILIES_SOURCE,
    },
    Exception {
        rule: "r1-instant-name",
        scope: "command net-log",
        path: "entries[].time",
        kind: ExceptionKind::Persisted,
        reason: NET_LOG_LINE,
        source: "README: Files and state",
    },
    Exception {
        rule: "r1-instant-type",
        scope: "command net-log",
        path: "entries[].time",
        kind: ExceptionKind::Persisted,
        reason: NET_LOG_LINE,
        source: "README: Files and state",
    },
];

const ULID_PATTERN: &str = "^[0-9A-HJKMNP-TV-Z]{26}$";

/// What a root holds, which decides the rules that apply to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Command,
    Envelope,
    Error,
    Event,
}

/// A published schema, with the scope its violations name.
struct Root {
    kind: Kind,
    scope: String,
    schema: Value,
}

/// Every published schema: each command's data (envelope-only names too), the envelope,
/// each error code's variant and each event.
fn roots() -> Vec<Root> {
    let mut out = Vec::new();
    for name in omaddons::app::COMMANDS
        .iter()
        .map(|entry| entry.name)
        .chain(omaddons::app::ENVELOPE_ONLY.iter().map(|entry| entry.name))
    {
        out.push(Root {
            kind: Kind::Command,
            scope: format!("command {name}"),
            schema: schema::data_schema_for(name).unwrap().to_value(),
        });
    }
    out.push(Root {
        kind: Kind::Envelope,
        scope: "envelope".to_owned(),
        schema: schema::envelope_schema().to_value(),
    });
    for code in ErrorCode::ALL {
        out.push(Root {
            kind: Kind::Error,
            scope: format!("error {code}"),
            schema: schema::error_variant(*code).to_value(),
        });
    }
    for (name, schema) in schema::event_schemas() {
        out.push(Root {
            kind: Kind::Event,
            scope: format!("event {name}"),
            schema: schema.to_value(),
        });
    }
    out
}

fn file_kinds() -> Vec<String> {
    schema::file_schemas()
        .into_keys()
        .map(str::to_owned)
        .collect()
}

/// What a node admits, merged through `$ref` and its in-place branches; `description` is
/// the node's own (a field's doc comment), never its type's.
#[derive(Debug, Default, PartialEq)]
struct Facts {
    types: BTreeSet<&'static str>,
    enum_values: Vec<Value>,
    pattern: Option<String>,
    description: Option<String>,
    /// Some merged node says `format: date-time`: the value is an instant, whatever its
    /// key (`contract::schema::timestamp`).
    date_time: bool,
    /// Some merged node bounds a string's length: free text, not a vocabulary.
    max_length: Option<u64>,
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_name(name: &str) -> Option<&'static str> {
    [
        "null", "boolean", "integer", "number", "string", "array", "object",
    ]
    .into_iter()
    .find(|known| *known == name)
}

fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        Some(root)
    } else {
        root.pointer(pointer)
    }
}

fn facts(root: &Value, node: &Value) -> Facts {
    let mut facts = Facts {
        description: node
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Facts::default()
    };
    merge(root, node, &mut facts, &mut Vec::new());
    facts
}

fn merge<'a>(root: &'a Value, node: &'a Value, facts: &mut Facts, seen: &mut Vec<&'a str>) {
    let Some(object) = node.as_object() else {
        return;
    };
    match object.get("type") {
        Some(Value::String(name)) => facts.types.extend(type_name(name)),
        Some(Value::Array(names)) => facts
            .types
            .extend(names.iter().filter_map(Value::as_str).filter_map(type_name)),
        _ => {}
    }
    let values = object.get("const").into_iter().chain(
        object
            .get("enum")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
    );
    for value in values {
        facts.types.insert(json_type(value));
        if !facts.enum_values.contains(value) {
            facts.enum_values.push(value.clone());
        }
    }
    if facts.pattern.is_none() {
        facts.pattern = object
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    if object.get("format").and_then(Value::as_str) == Some("date-time") {
        facts.date_time = true;
    }
    if facts.max_length.is_none() {
        facts.max_length = object.get("maxLength").and_then(Value::as_u64);
    }
    if let Some(reference) = object.get("$ref").and_then(Value::as_str)
        && !seen.contains(&reference)
        && let Some(target) = resolve(root, reference)
    {
        seen.push(reference);
        merge(root, target, facts, seen);
        seen.pop();
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        for branch in object
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            merge(root, branch, facts, seen);
        }
    }
}

fn snake(name: &str) -> bool {
    name.split('_').enumerate().all(|(index, word)| {
        !word.is_empty()
            && word
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && (index > 0 || word.starts_with(|c: char| c.is_ascii_lowercase()))
    })
}

fn line(rule: &str, scope: &str, path: &str) -> String {
    let path = if path.is_empty() { "." } else { path };
    format!("{rule} | {scope} | {path}")
}

/// Whether `path` lies strictly below `prefix`.
/// Whether `value` is a registered command's canonical path.
fn is_command_path(value: &str) -> bool {
    omaddons::app::COMMANDS
        .iter()
        .any(|entry| entry.name == value)
}

fn below(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with(['.', '[', '{']))
}

/// Whether `node` is a reference to the error union, which is checked once per code under
/// its own scope, not again inside every command that embeds it.
fn is_error_union(root: &Value, node: &Value) -> bool {
    node.get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| resolve(root, reference))
        .and_then(|target| target["oneOf"].as_array())
        .is_some_and(|variants| {
            variants.len() == ErrorCode::ALL.len()
                && variants[0]["properties"]["code"]["const"] == ErrorCode::ALL[0].wire()
        })
}

/// Markers that say when an omittable key is left out.
const OMISSION_MARKERS: &[&str] = &[
    "omitted",
    "absent",
    "present when",
    "present iff",
    "present only",
    "only with",
    "only when",
    "only under",
    "`-v`",
    "--raw-details",
];

// Narrowed (H1, c6cm.8.1.3): a bare `at` is the `_at` form with nothing before it, the
// moment of the object itself, unambiguous as an instant. The persisted history line has
// the same key, so renaming only the wire key would make `history` disagree with its own
// file. `time`, `date`, `timestamp` and `when` stay flagged.
const INSTANT_NAMES: &[&str] = &["time", "date", "timestamp", "when"];
const INSTANT_SUFFIXES: &[&str] = &["_time", "_date", "_timestamp", "_on"];
const DURATION_NAMES: &[&str] = &["duration", "elapsed", "timeout", "ttl", "age", "interval"];
const DURATION_SUFFIXES: &[&str] = &[
    "_duration",
    "_secs",
    "_seconds",
    "_millis",
    "_age",
    "_timeout",
    "_ttl",
    "_interval",
];
const ENUM_NAMES: &[&str] = &[
    "kind",
    "status",
    "state",
    "reason",
    "mode",
    "action",
    "op",
    "class",
    "level",
    "phase",
    "event",
    "severity",
    "confidence",
    "verdict",
    "channel",
    "type",
    "result",
];

// Narrowed (H1, c6cm.8.1.4): rule 3 asks a plural so an array's name says it holds many.
// Three other kinds of name say so too:
// - a past participle naming the set an operation acted on (`restored`, `pruned`,
//   `dependencies_added`, `sources_skipped`), or a participle phrase naming the members by
//   their relation (`required_by`, `shared_with`, `left_out`, `owned_on`);
// - a present participle naming the members in that state (`running`, `existing`);
// - a collective noun, each listed with its reason.
// A singular count noun (`item`, `top`, `busy`) is still a violation.
const PARTICIPLE_PHRASE_ENDINGS: &[&str] = &["_by", "_with", "_out", "_on"];
const PRESENT_PARTICIPLES: &[&str] = &["running", "pending", "missing", "existing", "conflicting"];
const COLLECTIVE_NOUNS: &[(&str, &str)] = &[
    (
        "evidence",
        "the claims an identification or a finding rests on",
    ),
    (
        "attention",
        "what needs attention, the word `status --check` asserts on",
    ),
    (
        "drift",
        "the managed addons whose files drifted from their commit",
    ),
    (
        "stack",
        "a Lua error's frames, as BugGrabber's own field names them",
    ),
    (
        "selection",
        "the changes a preview selected; `plan_stale.details.changed` names this set `selection`",
    ),
    (
        "discovery",
        "bare help's discovery trio (vision §5.10), the commands an agent starts from; README names the key",
    ),
];

/// Whether an array's singular-looking `name` names a set (see above).
fn names_a_set(name: &str) -> bool {
    name.ends_with("ed")
        || PARTICIPLE_PHRASE_ENDINGS
            .iter()
            .any(|end| name.ends_with(end))
        || PRESENT_PARTICIPLES.contains(&name)
        || COLLECTIVE_NOUNS.iter().any(|(noun, _)| *noun == name)
}

/// The rules a property breaks, by id.
fn property_rules(kind: Kind, name: &str, required: bool, facts: &Facts) -> Vec<&'static str> {
    let types = &facts.types;
    let has = |t: &str| types.contains(t);
    let exactly = |allowed: &[&str]| {
        let set: BTreeSet<&str> = allowed.iter().copied().collect();
        types.iter().copied().collect::<BTreeSet<_>>() == set
    };
    let description = facts
        .description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let mut broken = Vec::new();
    let instant_key = name == "at" || name.ends_with("_at");
    if name.ends_with("_at")
        && !((exactly(&["string"]) || exactly(&["string", "null"])) && facts.date_time)
    {
        broken.push("r1-at-type");
    }
    // By type, so an instant under any other key is found, not only the names below.
    if facts.date_time && !instant_key {
        broken.push("r1-instant-type");
    }
    // Narrowed (both flagged compliant fields): only a string or a number can hold an
    // instant (`owned_on` lists owners), and `no_<noun>` counts what lacks one (`no_time`).
    if (has("string") || has("integer") || has("number"))
        && !name.starts_with("no_")
        && (INSTANT_NAMES.contains(&name) || INSTANT_SUFFIXES.iter().any(|s| name.ends_with(s)))
    {
        broken.push("r1-instant-name");
    }
    if (name.ends_with("_ms") || name.ends_with("_s"))
        && !(exactly(&["integer"]) || exactly(&["integer", "null"]))
    {
        broken.push("r1-duration-type");
    }
    if (has("integer") || has("number"))
        && (DURATION_NAMES.contains(&name) || DURATION_SUFFIXES.iter().any(|s| name.ends_with(s)))
    {
        broken.push("r1-duration-name");
    }
    // Inside an error, whether a key appears depends on its construction site, which its
    // `DetailSpec` governs (rule 6).
    if kind != Kind::Error {
        if !required && !OMISSION_MARKERS.iter().any(|m| description.contains(m)) {
            broken.push("r2-omittable-undocumented");
        }
        if required && description.contains("`-v` only") {
            broken.push("r2-verbose-required");
        }
    }
    if !snake(name) {
        broken.push("r3-name-case");
    }
    // Only a property that is an array in every branch holds a list: one whose type
    // follows another key (`limits[].value`, a list only for a `list` unit) holds one value.
    if has("array")
        && types.iter().all(|t| matches!(*t, "array" | "null"))
        && !(name.ends_with('s') || name.ends_with("argv") || names_a_set(name))
    {
        broken.push("r3-array-plural");
    }
    if has("boolean") && name.starts_with("is_") {
        broken.push("r3-bool-is");
    }
    if has("integer")
        && (name.ends_with("_count")
            || ["num_", "n_", "count_"].iter().any(|p| name.starts_with(p)))
    {
        broken.push("r3-count-name");
    }
    if name == "key" && facts.pattern.as_deref() != Some(ULID_PATTERN) {
        broken.push("r3-key-ulid");
    }
    // Narrowed (H1, c6cm.8.1.4): a string with a `maxLength` is bounded free text, never
    // a vocabulary: the `--reason` a person gave (`REASON_MAX_CHARS`), which every shape
    // that echoes one publishes so (`contract::schema::reason_text`).
    if ENUM_NAMES.contains(&name)
        && (exactly(&["string"]) || exactly(&["string", "null"]))
        && facts.enum_values.is_empty()
        && facts.max_length.is_none()
    {
        broken.push("r4-open-enum");
    }
    if kind == Kind::Error && name == "details" && !exactly(&["object"]) {
        broken.push("r6-details-object");
    }
    broken
}

/// Every violation of every rule in `roots` and `file_kinds`, as allowlist lines.
fn violations(roots: &[Root], file_kinds: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for root in roots {
        // The frame's `data` is each command's schema, and its `error` each code's;
        // `apply`'s `result` is each storable command's data and its `approved` the
        // approval those carry, each checked in its own root, as `plans show`'s
        // `approval` is.
        let mut pruned: Vec<String> = match root.kind {
            Kind::Envelope => vec!["data".to_owned(), "error".to_owned()],
            Kind::Command if root.scope == "command apply" => {
                vec!["result".to_owned(), "approved".to_owned()]
            }
            Kind::Command if root.scope == "command plans show" => vec!["approval".to_owned()],
            _ => Vec::new(),
        };
        walk(&root.schema, &mut |visit| {
            let (Visit::Position { path, .. } | Visit::Property { path, .. }) = visit;
            if pruned.iter().any(|prefix| below(path, prefix)) {
                return;
            }
            match visit {
                Visit::Position { path, node } => {
                    if root.kind != Kind::Error && is_error_union(&root.schema, node) {
                        pruned.push((*path).to_owned());
                    }
                    let values = node.get("const").into_iter().chain(
                        node.get("enum")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten(),
                    );
                    // A command path (`backup restore`) is a command name (rule 5), the
                    // string the envelope's `command` holds, never an enumeration value.
                    for value in values.filter_map(Value::as_str) {
                        if !snake(value) && !is_command_path(value) {
                            out.insert(line("r4-value-case", &root.scope, path));
                        }
                    }
                }
                Visit::Property {
                    path,
                    name,
                    node,
                    required,
                    ..
                } => {
                    let facts = facts(&root.schema, node);
                    for rule in property_rules(root.kind, name, *required, &facts) {
                        out.insert(line(rule, &root.scope, path));
                    }
                }
            }
        });
    }
    for kind in file_kinds {
        if !snake(kind) {
            out.insert(line("r3-name-case", "file", kind));
        }
    }
    out
}

/// The closed allowlist (M4-D55): comments and blank lines only. `Err` names every entry
/// line with its number, since H1 emptied the list and the rules admit no exception from
/// then on: a shape that breaks a rule is fixed, never listed.
fn parse_allowlist(text: &str) -> Result<(), Vec<String>> {
    let problems: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, raw)| {
            let trimmed = raw.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|(index, raw)| {
            format!(
                "line {}: `{raw}` is an entry, and `tests/contract/rules_allowlist.txt` is \
                 closed (M4-D55): fix the shape it names instead",
                index + 1
            )
        })
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A line's or a violation's `<rule> | <scope> | <path>`.
fn fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut fields = line.split(" | ");
    Some((fields.next()?, fields.next()?, fields.next()?))
}

/// Whether `pattern` covers `scope`: the same scope, or `<prefix> *` over that prefix (but
/// a bare `command`) and every scope below it, never a longer word.
fn scope_matches(pattern: &str, scope: &str) -> bool {
    pattern == scope
        || pattern.strip_suffix(" *").is_some_and(|prefix| {
            scope.strip_prefix(prefix).is_some_and(|rest| {
                rest.starts_with(' ') || (rest.is_empty() && prefix != "command")
            })
        })
}

fn exception_matches(exception: &Exception, violation: &str) -> bool {
    let Some((rule, scope, path)) = fields(violation) else {
        return false;
    };
    let scope_matches = scope_matches(exception.scope, scope);
    let path_matches = match exception.path.strip_prefix("**.") {
        Some(suffix) => {
            path == suffix
                || path
                    .strip_suffix(suffix)
                    .is_some_and(|head| head.ends_with('.'))
        }
        None => path == exception.path,
    };
    exception.rule == rule && scope_matches && path_matches
}

/// Compares the violations with the exceptions, after the closed allowlist: `Err` names
/// every allowlist entry, every new violation and every stale exception.
fn compare(
    violations: &BTreeSet<String>,
    allowlist: &str,
    exceptions: &[Exception],
) -> Result<(), String> {
    parse_allowlist(allowlist).map_err(|problems| problems.join("\n"))?;
    let mut failures = Vec::new();
    for violation in violations {
        if exceptions.iter().any(|e| exception_matches(e, violation)) {
            continue;
        }
        let id = violation.split(" | ").next().unwrap_or_default();
        let rule = RULES
            .iter()
            .find(|(known, _)| *known == id)
            .map_or(0, |(_, rule)| *rule);
        failures.push(format!(
            "a new shape breaks rule {rule} (`{id}`): `{violation}`. Fix the shape; never add to \
             `tests/contract/rules_allowlist.txt` (M4-D55)"
        ));
    }
    for exception in exceptions {
        if !violations.iter().any(|v| exception_matches(exception, v)) {
            failures.push(format!(
                "the exception `{} | {} | {}` ({:?}: {}; source: {}) matches no violation: remove it",
                exception.rule,
                exception.scope,
                exception.path,
                exception.kind,
                exception.reason,
                exception.source
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn allowlist() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/contract/rules_allowlist.txt");
    std::fs::read_to_string(&path).unwrap_or_default()
}

#[test]
fn every_schema_follows_the_contract_rules() {
    let found = violations(&roots(), &file_kinds());
    let compared = compare(&found, &allowlist(), EXCEPTIONS);
    assert!(
        compared.is_ok(),
        "{}\nevery current violation, in file form:\n{}",
        compared.as_ref().err().map_or("", String::as_str),
        found.iter().cloned().collect::<Vec<_>>().join("\n")
    );
}

/// One synthetic root of `kind`, scoped `scope`.
fn synthetic(kind: Kind, scope: &str, schema: Value) -> Root {
    Root {
        kind,
        scope: scope.to_owned(),
        schema,
    }
}

/// The violation lines of `rule` in one synthetic command root.
fn of_rule(rule: &str, kind: Kind, schema: &Value) -> BTreeSet<String> {
    violations(&[synthetic(kind, "command t", schema.clone())], &[])
        .into_iter()
        .filter(|line| line.starts_with(&format!("{rule} | ")))
        .collect()
}

/// A root with one property `name`, required or not.
fn property(name: &str, schema: Value, required: bool) -> Value {
    let required: Vec<&str> = if required { vec![name] } else { vec![] };
    json!({"type": "object", "properties": {name: schema}, "required": required})
}

/// Runs one rule's table: each case a schema and the paths it must report.
fn table(rule: &str, cases: Vec<(&str, Kind, Value, &[&str])>) {
    for (name, kind, schema, paths) in cases {
        let expected: BTreeSet<String> = paths.iter().map(|p| line(rule, "command t", p)).collect();
        let got = of_rule(rule, kind, &schema);
        assert_eq!(got, expected, "case {name}: {rule} over {schema:#}");
    }
}

const C: Kind = Kind::Command;

#[test]
fn r1_at_type() {
    table(
        "r1-at-type",
        vec![
            (
                "date_time",
                C,
                property(
                    "started_at",
                    json!({"type": "string", "format": "date-time"}),
                    true,
                ),
                &[],
            ),
            (
                "nullable_date_time",
                C,
                property(
                    "started_at",
                    json!({"type": ["string", "null"], "format": "date-time"}),
                    true,
                ),
                &[],
            ),
            (
                "a_string_that_is_no_instant",
                C,
                property("started_at", json!({"type": "string"}), true),
                &["started_at"],
            ),
            (
                "integer",
                C,
                property("started_at", json!({"type": "integer"}), true),
                &["started_at"],
            ),
            (
                "object",
                C,
                property("seen_at", json!({"type": "object"}), true),
                &["seen_at"],
            ),
        ],
    );
}

#[test]
fn r1_instant_name() {
    table(
        "r1-instant-name",
        vec![
            (
                "suffix_at",
                C,
                property("created_at", json!({"type": "string"}), true),
                &[],
            ),
            (
                "bare_at_is_the_suffix_alone",
                C,
                property("at", json!({"type": "string"}), true),
                &[],
            ),
            (
                "bare_time",
                C,
                property("time", json!({"type": "string"}), true),
                &["time"],
            ),
            (
                "bare_date",
                C,
                property("date", json!({"type": "string"}), true),
                &["date"],
            ),
            (
                "bare_timestamp",
                C,
                property("timestamp", json!({"type": "string"}), true),
                &["timestamp"],
            ),
            (
                "bare_when",
                C,
                property("when", json!({"type": "string"}), true),
                &["when"],
            ),
            (
                "time_suffix",
                C,
                property("start_time", json!({"type": "string"}), true),
                &["start_time"],
            ),
            (
                "on_suffix",
                C,
                property("created_on", json!({"type": "string"}), true),
                &["created_on"],
            ),
            (
                "epoch_number",
                C,
                property("time", json!({"type": "integer"}), true),
                &["time"],
            ),
            (
                "a_list_named_on",
                C,
                property(
                    "owned_on",
                    json!({"type": "array", "items": {"type": "object"}}),
                    true,
                ),
                &[],
            ),
            (
                "a_count_of_what_has_none",
                C,
                property("no_time", json!({"type": "integer"}), true),
                &[],
            ),
        ],
    );
}

#[test]
fn r1_instant_type() {
    let instant = json!({"type": "string", "format": "date-time"});
    let nullable = json!({"type": ["string", "null"], "format": "date-time"});
    table(
        "r1-instant-type",
        vec![
            (
                "an_instant_without_its_suffix",
                C,
                property("since", instant.clone(), true),
                &["since"],
            ),
            (
                "a_nullable_instant_without_its_suffix",
                C,
                property("dismissed", nullable.clone(), true),
                &["dismissed"],
            ),
            (
                "an_instant_named_at",
                C,
                property("at", instant.clone(), true),
                &[],
            ),
            (
                "an_instant_named_for_it",
                C,
                property("checked_at", nullable, true),
                &[],
            ),
            (
                "a_string_that_is_no_instant",
                C,
                property("since", json!({"type": "string"}), true),
                &[],
            ),
            (
                "an_instant_behind_a_ref",
                C,
                json!({
                    "$defs": {"Instant": instant},
                    "type": "object",
                    "properties": {"when_seen": {"$ref": "#/$defs/Instant"}},
                    "required": ["when_seen"],
                }),
                &["when_seen"],
            ),
        ],
    );
}

/// The net.log line's `time` is excepted where `net-log` echoes it, and nowhere else.
#[test]
fn persisted_exception_covers_its_path_only() {
    let entries = |time: Value| {
        json!({
            "type": "object",
            "properties": {"entries": {"type": "array", "items": {
                "type": "object", "properties": {"time": time}, "required": ["time"],
            }}},
            "required": ["entries"],
        })
    };
    let instant = json!({"type": "string", "format": "date-time"});
    let found = |scope: &str| {
        violations(
            &[synthetic(Kind::Command, scope, entries(instant.clone()))],
            &[],
        )
    };
    let persisted: Vec<Exception> = EXCEPTIONS
        .iter()
        .copied()
        .filter(|e| e.kind == ExceptionKind::Persisted)
        .collect();
    assert_eq!(
        found("command net-log"),
        BTreeSet::from([
            line("r1-instant-name", "command net-log", "entries[].time"),
            line("r1-instant-type", "command net-log", "entries[].time"),
        ]),
        "the shape breaks both rule 1 name and type checks"
    );
    assert_eq!(
        compare(&found("command net-log"), "", &persisted),
        Ok(()),
        "net-log's entries[].time is excepted"
    );
    let elsewhere = compare(&found("command history"), "", &persisted).unwrap_err();
    assert!(
        elsewhere.contains("a new shape breaks rule 1 (`r1-instant-name`): `r1-instant-name | command history | entries[].time`")
            && elsewhere.contains("a new shape breaks rule 1 (`r1-instant-type`)"),
        "the same key in another command is not excepted: {elsewhere}"
    );
}

#[test]
fn r1_duration_type() {
    table(
        "r1-duration-type",
        vec![
            (
                "integer_ms",
                C,
                property("elapsed_ms", json!({"type": "integer"}), true),
                &[],
            ),
            (
                "nullable_s",
                C,
                property("wait_s", json!({"type": ["integer", "null"]}), true),
                &[],
            ),
            (
                "float_ms",
                C,
                property("elapsed_ms", json!({"type": "number"}), true),
                &["elapsed_ms"],
            ),
            (
                "string_s",
                C,
                property("wait_s", json!({"type": "string"}), true),
                &["wait_s"],
            ),
        ],
    );
}

#[test]
fn r1_duration_name() {
    table(
        "r1-duration-name",
        vec![
            (
                "with_unit",
                C,
                property("timeout_ms", json!({"type": "integer"}), true),
                &[],
            ),
            (
                "text",
                C,
                property("timeout", json!({"type": "string"}), true),
                &[],
            ),
            (
                "bare",
                C,
                property("timeout", json!({"type": "integer"}), true),
                &["timeout"],
            ),
            (
                "suffix",
                C,
                property("cache_ttl", json!({"type": "number"}), true),
                &["cache_ttl"],
            ),
        ],
    );
}

#[test]
fn r2_omittable_undocumented() {
    let documented = json!({"type": "string", "description": "Omitted when nothing matched."});
    let verbose = json!({"type": "string", "description": "`-v` only: the path."});
    table(
        "r2-omittable-undocumented",
        vec![
            (
                "required",
                C,
                property("a", json!({"type": "string"}), true),
                &[],
            ),
            ("documented", C, property("a", documented, false), &[]),
            ("verbose", C, property("a", verbose, false), &[]),
            (
                "undocumented",
                C,
                property("a", json!({"type": "string"}), false),
                &["a"],
            ),
            (
                "type_doc_does_not_count",
                C,
                json!({
                    "properties": {"a": {"$ref": "#/$defs/A"}},
                    "$defs": {"A": {"type": "object", "description": "Omitted when absent."}},
                }),
                &["a"],
            ),
            (
                "inside_an_error",
                Kind::Error,
                property("a", json!({"type": "string"}), false),
                &[],
            ),
        ],
    );
}

#[test]
fn r2_verbose_required() {
    let verbose = json!({"type": "string", "description": "`-v` only: the path."});
    table(
        "r2-verbose-required",
        vec![
            ("optional", C, property("a", verbose.clone(), false), &[]),
            ("required", C, property("a", verbose, true), &["a"]),
        ],
    );
}

#[test]
fn r3_name_case() {
    table(
        "r3-name-case",
        vec![
            (
                "snake",
                C,
                property("fix_argv", json!({"type": "string"}), true),
                &[],
            ),
            (
                "digits",
                C,
                property("v2_name", json!({"type": "string"}), true),
                &[],
            ),
            (
                "camel",
                C,
                property("fixArgv", json!({"type": "string"}), true),
                &["fixArgv"],
            ),
            (
                "kebab",
                C,
                property("fix-argv", json!({"type": "string"}), true),
                &["fix-argv"],
            ),
        ],
    );
    assert_eq!(
        violations(
            &[],
            &["target_record".to_owned(), "Target-Record".to_owned()]
        ),
        BTreeSet::from([line("r3-name-case", "file", "Target-Record")]),
        "file kind names"
    );
}

#[test]
fn r3_array_plural() {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let mut cases: Vec<(&str, Kind, Value, &[&str])> = vec![
        ("plural", C, property("entries", strings.clone(), true), &[]),
        ("argv", C, property("fix_argv", strings.clone(), true), &[]),
        (
            "nullable_singular",
            C,
            property(
                "item",
                json!({"type": ["array", "null"], "items": {}}),
                true,
            ),
            &["item"],
        ),
        (
            "an_array_in_one_branch_only",
            C,
            property(
                "value",
                json!({"anyOf": [{"type": "integer"}, {"type": "array", "items": {}}]}),
                true,
            ),
            &[],
        ),
    ];
    // Participles and the listed collective nouns name a set (the narrowing's reason is
    // beside `names_a_set`).
    for name in [
        "restored",
        "dependencies_added",
        "sources_skipped",
        "required_by",
        "shared_with",
        "left_out",
        "owned_on",
        "running",
        "existing",
        "evidence",
        "selection",
    ] {
        cases.push((name, C, property(name, strings.clone(), true), &[]));
    }
    // A singular count noun still breaks the rule.
    cases.push((
        "item",
        C,
        property("item", strings.clone(), true),
        &["item"],
    ));
    cases.push(("top", C, property("top", strings.clone(), true), &["top"]));
    cases.push(("busy", C, property("busy", strings, true), &["busy"]));
    table("r3-array-plural", cases);
}

#[test]
fn r3_bool_is() {
    table(
        "r3-bool-is",
        vec![
            (
                "state",
                C,
                property("busy", json!({"type": "boolean"}), true),
                &[],
            ),
            (
                "is_text",
                C,
                property("is_name", json!({"type": "string"}), true),
                &[],
            ),
            (
                "is_bool",
                C,
                property("is_busy", json!({"type": "boolean"}), true),
                &["is_busy"],
            ),
        ],
    );
}

#[test]
fn r3_count_name() {
    table(
        "r3-count-name",
        vec![
            (
                "bare_noun",
                C,
                property("files", json!({"type": "integer"}), true),
                &[],
            ),
            (
                "suffix",
                C,
                property("file_count", json!({"type": "integer"}), true),
                &["file_count"],
            ),
            (
                "prefix",
                C,
                property("num_files", json!({"type": "integer"}), true),
                &["num_files"],
            ),
        ],
    );
}

#[test]
fn r3_key_ulid() {
    table(
        "r3-key-ulid",
        vec![
            (
                "ulid",
                C,
                json!({
                    "properties": {"key": {"$ref": "#/$defs/TargetKey"}},
                    "required": ["key"],
                    "$defs": {"TargetKey": {"type": "string", "pattern": ULID_PATTERN}},
                }),
                &[],
            ),
            (
                "other_key",
                C,
                property("config_key", json!({"type": "string"}), true),
                &[],
            ),
            (
                "plain_string",
                C,
                property("key", json!({"type": "string"}), true),
                &["key"],
            ),
        ],
    );
}

#[test]
fn r4_value_case() {
    table(
        "r4-value-case",
        vec![
            (
                "snake",
                C,
                property("family", json!({"enum": ["vanilla", "mists"]}), true),
                &[],
            ),
            (
                "kebab",
                C,
                property("family", json!({"enum": ["vanilla-legacy"]}), true),
                &["family"],
            ),
            (
                "items",
                C,
                property(
                    "families",
                    json!({"type": "array", "items": {"const": "Mists"}}),
                    true,
                ),
                &["families[]"],
            ),
        ],
    );
}

#[test]
fn r4_open_enum() {
    table(
        "r4-open-enum",
        vec![
            (
                "closed",
                C,
                property("status", json!({"enum": ["ok", "failed"]}), true),
                &[],
            ),
            (
                "closed_through_a_ref",
                C,
                json!({
                    "properties": {"kind": {"anyOf": [{"$ref": "#/$defs/K"}, {"type": "null"}]}},
                    "required": ["kind"],
                    "$defs": {"K": {"type": "string", "enum": ["a"]}},
                }),
                &[],
            ),
            (
                "other_name",
                C,
                property("label", json!({"type": "string"}), true),
                &[],
            ),
            (
                "open",
                C,
                property("status", json!({"type": "string"}), true),
                &["status"],
            ),
            (
                "open_nullable",
                C,
                property("reason", json!({"type": ["string", "null"]}), true),
                &["reason"],
            ),
            (
                "bounded_free_text",
                C,
                property(
                    "reason",
                    json!({"type": ["string", "null"], "maxLength": 200}),
                    true,
                ),
                &[],
            ),
        ],
    );
}

#[test]
fn r6_details_object() {
    let error = |details: Value| property("details", details, true);
    table(
        "r6-details-object",
        vec![
            ("object", Kind::Error, error(json!({"type": "object"})), &[]),
            ("in_a_command", C, error(json!({"type": "string"})), &[]),
            (
                "string",
                Kind::Error,
                error(json!({"type": "string"})),
                &["details"],
            ),
            (
                "nullable",
                Kind::Error,
                error(json!({"type": ["object", "null"]})),
                &["details"],
            ),
        ],
    );
}

#[test]
fn facts_merge_refs_and_branches() {
    let root = json!({"$defs": {"E": {"type": "string", "enum": ["a", "b"]}}});
    let node = json!({"anyOf": [{"$ref": "#/$defs/E"}, {"type": "null"}]});
    let merged = facts(&root, &node);
    assert_eq!(
        merged.types,
        BTreeSet::from(["string", "null"]),
        "{merged:?}"
    );
    assert_eq!(merged.enum_values, [json!("a"), json!("b")], "{merged:?}");
    let described = json!({"$ref": "#/$defs/E", "description": "The field's own."});
    assert_eq!(
        facts(&root, &described).description.as_deref(),
        Some("The field's own."),
        "a sibling description is the field's"
    );
    let recursive = json!({"$defs": {"N": {"anyOf": [{"$ref": "#/$defs/N"}, {"type": "object"}]}}});
    assert_eq!(
        facts(&recursive, &json!({"$ref": "#/$defs/N"})).types,
        BTreeSet::from(["object"]),
        "a recursive reference terminates"
    );
}

#[test]
fn the_error_union_is_checked_once_under_each_code() {
    let update = schema::data_schema_for("update").unwrap().to_value();
    assert!(
        update["$defs"]["ErrorBody"]["oneOf"].is_array(),
        "{update:#}"
    );
    // A bad name planted in the union is not reported from the command embedding it…
    let mut in_union = update.clone();
    in_union["$defs"]["ErrorBody"]["oneOf"][0]["properties"]["badName"] = json!({"type": "string"});
    let found = violations(&[synthetic(Kind::Command, "command update", in_union)], &[]);
    assert!(
        !found.iter().any(|line| line.contains("badName")),
        "the union is walked inside a command: {found:#?}"
    );
    // …while one planted beside it, in the command's own result, is.
    let mut beside = update;
    beside["$defs"]["TargetResult"]["properties"]["badName"] = json!({"type": "string"});
    let found = violations(&[synthetic(Kind::Command, "command update", beside)], &[]);
    assert!(
        found.contains(&line("r3-name-case", "command update", "targets[].badName")),
        "{found:#?}"
    );
    // The frame enters neither `data` nor `error`.
    let mut frame = schema::envelope_schema().to_value();
    frame["$defs"]["ErrorBody"]["oneOf"][0]["properties"]["badName"] = json!({"type": "string"});
    frame["properties"]["data"] = json!({"properties": {"badName": {"type": "string"}}});
    let found = violations(&[synthetic(Kind::Envelope, "envelope", frame)], &[]);
    assert!(
        !found.iter().any(|line| line.contains("badName")),
        "the frame is walked into data or error: {found:#?}"
    );
}

/// The allowlist is closed (M4-D55): the committed file holds only comments, and any entry
/// line, well formed or not, is refused with its number and the decision's name.
#[test]
fn the_allowlist_is_closed() {
    let header = "# a comment\n\n# another\n";
    let cases: &[(&str, String, Option<&[&str]>)] = &[
        ("the_committed_file", allowlist(), None),
        ("comments_and_blank_lines", header.to_owned(), None),
        (
            "a_well_formed_entry",
            format!("{header}r1-at-type | command a | x\n"),
            Some(&["line 4: `r1-at-type | command a | x` is an entry", "M4-D55"]),
        ),
        (
            "any_other_text",
            "not an entry at all\n".to_owned(),
            Some(&["line 1: `not an entry at all`", "M4-D55"]),
        ),
    ];
    for (name, text, refused) in cases {
        match (parse_allowlist(text), refused) {
            (Ok(()), None) => {}
            (Err(problems), Some(phrases)) => {
                assert_eq!(problems.len(), 1, "case {name}: {problems:?}");
                for phrase in *phrases {
                    assert!(problems[0].contains(phrase), "case {name}: {problems:?}");
                }
            }
            (got, _) => panic!("case {name}: {got:?} for\n{text}"), // ubs:ignore — a table case's own failure
        }
    }
    let failure = compare(&BTreeSet::new(), "r1-at-type | command a | x\n", EXCEPTIONS);
    assert!(
        failure.as_ref().is_err_and(|f| f.contains("M4-D55")),
        "compare refuses an entry first: {failure:?}"
    );
}

/// One synthetic root breaking exactly `rule`.
fn planted(rule: &str) -> Root {
    let command = |schema| synthetic(Kind::Command, "command planted", schema);
    match rule {
        "r1-at-type" => command(property("started_at", json!({"type": "integer"}), true)),
        "r1-instant-name" => command(property("when", json!({"type": "string"}), true)),
        "r1-instant-type" => command(property(
            "since",
            json!({"type": "string", "format": "date-time"}),
            true,
        )),
        "r1-duration-type" => command(property("wait_ms", json!({"type": "string"}), true)),
        "r1-duration-name" => command(property("timeout", json!({"type": "integer"}), true)),
        "r2-omittable-undocumented" => command(property("extra", json!({"type": "string"}), false)),
        "r2-verbose-required" => command(property(
            "paths",
            json!({"type": "array", "items": {"type": "string"}, "description": "`-v` only: the paths."}),
            true,
        )),
        "r3-name-case" => command(property("badName", json!({"type": "string"}), true)),
        "r3-array-plural" => command(property(
            "top",
            json!({"type": "array", "items": {"type": "string"}}),
            true,
        )),
        "r3-bool-is" => command(property("is_busy", json!({"type": "boolean"}), true)),
        "r3-count-name" => command(property("file_count", json!({"type": "integer"}), true)),
        "r3-key-ulid" => command(property("key", json!({"type": "string"}), true)),
        "r4-value-case" => command(property("color", json!({"enum": ["Red"]}), true)),
        "r4-open-enum" => command(property("status", json!({"type": "string"}), true)),
        "r6-details-object" => synthetic(
            Kind::Error,
            "error planted",
            property("details", json!({"type": "string"}), true),
        ),
        other => panic!("no planted root for {other}"), // ubs:ignore — a newly registered rule needs its planted case
    }
}

#[test]
fn a_new_violation_fails_with_the_rule_message() {
    let real = roots();
    let allowlist = allowlist();
    for (rule, number) in RULES {
        let alone = violations(&[planted(rule)], &[]);
        assert_eq!(
            alone.len(),
            1,
            "{rule}: the planted root breaks exactly one rule: {alone:#?}"
        );
        let planted_line = alone.iter().next().cloned().unwrap_or_default();
        assert!(
            planted_line.starts_with(&format!("{rule} | ")),
            "{rule}: planted {planted_line}"
        );
        let mut all = violations(&real, &file_kinds());
        all.extend(alone);
        let failure = compare(&all, &allowlist, EXCEPTIONS).unwrap_err();
        for phrase in [
            format!("rule {number} (`{rule}`)"),
            planted_line.clone(),
            "never add to".to_owned(),
        ] {
            assert!(
                failure.contains(&phrase),
                "{rule}: {phrase:?} in\n{failure}"
            );
        }
    }
}

#[test]
fn a_stale_exception_fails() {
    let stale = [Exception {
        rule: "r4-value-case",
        scope: "command *",
        path: "**.no_such_field",
        kind: ExceptionKind::External,
        reason: "an outside vocabulary",
        source: "a test",
    }];
    let found = violations(&roots(), &file_kinds());
    let failure = compare(&found, &allowlist(), &stale).unwrap_err();
    assert!(
        failure.contains("matches no violation") && failure.contains("**.no_such_field"),
        "{failure}"
    );
    let mut matching: Vec<Exception> = EXCEPTIONS.to_vec();
    matching.push(Exception {
        rule: "r4-value-case",
        scope: "command planted",
        path: "color",
        kind: ExceptionKind::External,
        reason: "an outside vocabulary",
        source: "a test",
    });
    let mut with_planted = found.clone();
    with_planted.extend(violations(&[planted("r4-value-case")], &[]));
    assert_eq!(
        compare(&with_planted, &allowlist(), &matching),
        Ok(()),
        "a matching exception covers its violation"
    );

    // `<prefix> *` covers that scope and the ones below it, never a longer word.
    let tree = Exception {
        rule: "r4-value-case",
        scope: "command doctor *",
        path: "state",
        kind: ExceptionKind::External,
        reason: "an outside vocabulary",
        source: "a test",
    };
    for (scope, matches) in [
        ("command doctor", true),
        ("command doctor diagnose", true),
        ("command doctors", false),
        ("command status", false),
    ] {
        let violation = format!("r4-value-case | {scope} | state");
        assert_eq!(exception_matches(&tree, &violation), matches, "{scope}");
    }
    let every = Exception {
        scope: "command *",
        ..tree
    };
    assert!(exception_matches(
        &every,
        "r4-value-case | command status | state"
    ));
    assert!(!exception_matches(
        &every,
        "r4-value-case | error usage | state"
    ));
}
