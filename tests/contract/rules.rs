//! The contract's rules over every published shape (M4-D55, rules 1–4 and 6): timestamps,
//! null versus omitted, names, closed enumerations and `details` objects, checked on every
//! command's data, the envelope frame, every error variant, every event and every file
//! kind name. The violations that predate this test are listed in
//! `tests/contract/rules_allowlist.txt`, which may only shrink. Pure: no command runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use omaddons::contract::schema::{self, Visit, walk};
use omaddons::error::ErrorCode;
use serde_json::{Value, json};

/// Every checker, with the rule it belongs to.
const RULES: &[(&str, u8)] = &[
    ("r1-at-type", 1),
    ("r1-instant-name", 1),
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
    reason: &'static str,
}

/// The doctor contract's report (M4-D34, A.4) names its severities and run states as
/// generic doctor consumers read them.
const DOCTOR_CONTRACT: &str =
    "the doctor contract's own names (M4-D34, A.4), which generic doctor consumers read";

/// Outside vocabularies omaddons publishes as they are.
const EXCEPTIONS: &[Exception] = &[
    Exception {
        rule: "r3-array-plural",
        scope: "command doctor *",
        path: "skipped",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P0",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P1",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P2",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r3-name-case",
        scope: "command doctor *",
        path: "summary.by_severity.P3",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r4-open-enum",
        scope: "command doctor *",
        path: "**.evidence.mode",
        reason: "a POSIX permission mode, four octal digits checked by a pattern: no enumeration",
    },
    Exception {
        rule: "r4-value-case",
        scope: "command doctor *",
        path: "**.severity",
        reason: DOCTOR_CONTRACT,
    },
    Exception {
        rule: "r4-value-case",
        scope: "command doctor *",
        path: "state",
        reason: DOCTOR_CONTRACT,
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

const INSTANT_NAMES: &[&str] = &["at", "time", "date", "timestamp", "when"];
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
    if name.ends_with("_at") && !(exactly(&["string"]) || exactly(&["string", "null"])) {
        broken.push("r1-at-type");
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
    if has("array") && !(name.ends_with('s') || name.ends_with("argv")) {
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
    if ENUM_NAMES.contains(&name)
        && (exactly(&["string"]) || exactly(&["string", "null"]))
        && facts.enum_values.is_empty()
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

/// The allowlist's lines, or every malformed, unsorted or duplicate line with its number.
fn parse_allowlist(text: &str) -> Result<Vec<String>, Vec<String>> {
    let mut lines: Vec<String> = Vec::new();
    let mut problems = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = raw.split(" | ").collect();
        let well_formed = fields.len() == 3
            && RULES.iter().any(|(id, _)| *id == fields[0])
            && fields.iter().all(|f| !f.is_empty() && f.trim() == *f);
        if !well_formed {
            problems.push(format!(
                "line {number}: `{raw}` is not `<rule> | <scope> | <path>` with a known rule"
            ));
            continue;
        }
        if let Some(previous) = lines.last() {
            if raw == previous {
                problems.push(format!("line {number}: `{raw}` is listed twice"));
                continue;
            }
            if raw < previous.as_str() {
                problems.push(format!("line {number}: `{raw}` is out of order"));
            }
        }
        lines.push(raw.to_owned());
    }
    if problems.is_empty() {
        Ok(lines)
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

/// Whether the allowlist `line` covers `violation`: the same line, or a `<prefix> *` scope
/// with the same rule and path, for a shape defined once and carried by every scope below
/// it (the error wrapper's keys).
fn line_matches(line: &str, violation: &str) -> bool {
    match (fields(line), fields(violation)) {
        (Some((rule, scope, path)), Some((found_rule, found_scope, found_path))) => {
            rule == found_rule && path == found_path && scope_matches(scope, found_scope)
        }
        _ => false,
    }
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

/// Compares the violations with the allowlist and the exceptions: `Err` names every new
/// violation, stale line and stale exception, then the allowlist's count per rule.
fn compare(
    violations: &BTreeSet<String>,
    allowlist: &str,
    exceptions: &[Exception],
) -> Result<(), String> {
    let listed = parse_allowlist(allowlist).map_err(|problems| problems.join("\n"))?;
    let mut failures = Vec::new();
    for violation in violations {
        if listed.iter().any(|line| line_matches(line, violation))
            || exceptions.iter().any(|e| exception_matches(e, violation))
        {
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
    for line in &listed {
        if !violations
            .iter()
            .any(|violation| line_matches(line, violation))
        {
            failures.push(format!(
                "`{line}` no longer matches a violation: remove it (the allowlist may only shrink)"
            ));
        }
    }
    for exception in exceptions {
        if !violations.iter().any(|v| exception_matches(exception, v)) {
            failures.push(format!(
                "the exception `{} | {} | {}` ({}) matches no violation: remove it",
                exception.rule, exception.scope, exception.path, exception.reason
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("{}\n{}", failures.join("\n"), counts(&listed)))
    }
}

fn counts(listed: &[String]) -> String {
    let mut per_rule: BTreeMap<&str, usize> = RULES.iter().map(|(id, _)| (*id, 0)).collect();
    for line in listed {
        if let Some(count) = line.split(" | ").next().and_then(|id| per_rule.get_mut(id)) {
            *count += 1;
        }
    }
    let shown: Vec<String> = per_rule
        .iter()
        .map(|(id, count)| format!("{id} {count}"))
        .collect();
    format!("allowlisted lines per rule: {}", shown.join(", "))
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
    let listed = parse_allowlist(&allowlist()).unwrap();
    eprintln!("{}", counts(&listed));
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
                "string",
                C,
                property("started_at", json!({"type": "string"}), true),
                &[],
            ),
            (
                "nullable",
                C,
                property("started_at", json!({"type": ["string", "null"]}), true),
                &[],
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
                "bare_at",
                C,
                property("at", json!({"type": "string"}), true),
                &["at"],
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
    table(
        "r3-array-plural",
        vec![
            ("plural", C, property("entries", strings.clone(), true), &[]),
            ("argv", C, property("fix_argv", strings.clone(), true), &[]),
            (
                "singular",
                C,
                property("left_out", strings, true),
                &["left_out"],
            ),
        ],
    );
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

#[test]
fn allowlist_parses_sorted_and_unique() {
    let header = "# a comment\n\n";
    let cases: &[(&str, String, Result<usize, &str>)] = &[
        (
            "well_formed",
            format!("{header}r1-at-type | command a | x\nr3-name-case | file | Bad\n"),
            Ok(2),
        ),
        (
            "unsorted",
            "r3-name-case | file | Bad\nr1-at-type | command a | x\n".to_owned(),
            Err("line 2:"),
        ),
        (
            "duplicate",
            "r1-at-type | command a | x\nr1-at-type | command a | x\n".to_owned(),
            Err("listed twice"),
        ),
        (
            "unknown_rule_id",
            "r9-made-up | command a | x\n".to_owned(),
            Err("known rule"),
        ),
        (
            "missing_field",
            "r1-at-type | command a\n".to_owned(),
            Err("line 1:"),
        ),
        (
            "comment_and_blank_lines_ignored",
            format!("{header}# another\n\n"),
            Ok(0),
        ),
    ];
    for (name, text, expected) in cases {
        match (parse_allowlist(text), expected) {
            (Ok(lines), Ok(count)) => assert_eq!(lines.len(), *count, "case {name}"),
            (Err(problems), Err(phrase)) => assert!(
                problems.iter().any(|p| p.contains(phrase)),
                "case {name}: {problems:?}"
            ),
            (got, _) => panic!("case {name}: {got:?} for\n{text}"), // ubs:ignore — a table case's own failure
        }
    }
}

/// One synthetic root breaking exactly `rule`.
fn planted(rule: &str) -> Root {
    let command = |schema| synthetic(Kind::Command, "command planted", schema);
    match rule {
        "r1-at-type" => command(property("started_at", json!({"type": "integer"}), true)),
        "r1-instant-name" => command(property("when", json!({"type": "string"}), true)),
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
            "left_out",
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
fn a_stale_line_fails() {
    let mut found = violations(&roots(), &file_kinds());
    let listed = parse_allowlist(&allowlist()).unwrap();
    let Some(removed) = listed.first() else {
        return;
    };
    found.remove(removed);
    let failure = compare(&found, &allowlist(), EXCEPTIONS).unwrap_err();
    assert!(
        failure.contains(&format!("`{removed}` no longer matches"))
            && failure.contains("may only shrink"),
        "{failure}"
    );
}

/// A `<prefix> *` line covers its rule and path in every scope below the prefix, and is
/// stale only when none of them has it.
#[test]
fn a_wildcard_line_covers_every_scope_below_it() {
    let line = "r3-array-plural | error * | details.required_by";
    for (violation, covered) in [
        ("r3-array-plural | error usage | details.required_by", true),
        (
            "r3-array-plural | error doctor_refused | details.required_by",
            true,
        ),
        ("r3-array-plural | error usage | details.tried", false),
        ("r3-name-case | error usage | details.required_by", false),
        (
            "r3-array-plural | errors usage | details.required_by",
            false,
        ),
        (
            "r3-array-plural | command usage | details.required_by",
            false,
        ),
    ] {
        assert_eq!(line_matches(line, violation), covered, "{violation}");
    }
    let found: BTreeSet<String> =
        ["r3-array-plural | error usage | details.tried".to_owned()].into();
    let failure = compare(&found, &format!("{line}\n"), &[]).unwrap_err();
    assert!(
        failure.contains(&format!("`{line}` no longer matches")),
        "{failure}"
    );
}

#[test]
fn a_stale_exception_fails() {
    let stale = [Exception {
        rule: "r4-value-case",
        scope: "command *",
        path: "**.no_such_field",
        reason: "an outside vocabulary",
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
        reason: "an outside vocabulary",
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
        reason: "an outside vocabulary",
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
