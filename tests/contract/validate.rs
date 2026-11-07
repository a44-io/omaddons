//! The contract suite's JSON Schema tools (M4-D9): the strict form of a published schema,
//! and a validator. Published schemas stay open, so a newer minor's additions never break
//! an older consumer's validator; the suite closes them here, so an emitted key a schema
//! lacks fails. The validator crate is used in this file only. It depends only on the
//! library, `serde_json` and the validator, so any suite can use it: the integration test
//! crate declares it once, as `crate::validate` (`tests/main.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::OnceLock;

use omaddons::app;
use omaddons::contract::schema::{self, is_open};
use omaddons::error::ErrorCode;
use serde_json::Value;

/// A schema construct the strict transform does not model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictError {
    pub keyword: String,
    /// The JSON pointer of the keyword in the published schema.
    pub path: String,
}

impl fmt::Display for StrictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "strict: unsupported `{}` at {}", self.keyword, self.path)
    }
}

/// Keywords the transform copies unchanged: assertions and annotations on one value.
const PASS_THROUGH: &[&str] = &[
    "type",
    "enum",
    "const",
    "required",
    "$ref",
    "$schema",
    "$id",
    "$comment",
    "$anchor",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "format",
    "pattern",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "unevaluatedProperties",
];

/// Whether the transform closes a node: an instance position's, or an in-place branch's
/// (`allOf`/`anyOf`/`oneOf` entries and `$defs` roots), which it never closes, because a
/// branch's own `unevaluatedProperties` sees only that branch's annotations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Instance,
    InPlace,
}

/// The test-side strict form of a published schema (M4-D9): `unevaluatedProperties: false`
/// at every instance position. Returns a new value; the input is not changed.
///
/// Instance positions are the root, every value of `properties` and `patternProperties`,
/// a schema `additionalProperties`, `items` and every `prefixItems` entry, inside `$defs`
/// too. An open node ([`is_open`], or one that already has `unevaluatedProperties`) is left
/// exactly as it is, so every `contract::OPAQUE` position stays open, and the transform is
/// idempotent. A keyword it does not model is refused rather than guessed at: inside a
/// `not`, an inserted `unevaluatedProperties: false` would invert its meaning.
pub fn strict(schema: &Value) -> Result<Value, StrictError> {
    let mut out = schema.clone();
    if let Value::Object(root) = &mut out {
        for keyword in ["$defs", "definitions"] {
            if let Some(Value::Object(entries)) = root.get_mut(keyword) {
                for (name, entry) in entries.iter_mut() {
                    close(entry, &pointer(&pointer("", keyword), name), Place::InPlace)?;
                }
            }
        }
    }
    close_node(&mut out, "", Place::Instance, true)?;
    Ok(out)
}

fn close(node: &mut Value, path: &str, place: Place) -> Result<(), StrictError> {
    close_node(node, path, place, false)
}

/// Closes `node` (at `path`) and every instance position under it. Only the root
/// (`at_root`) may carry `$defs`, which [`strict`] has already closed.
fn close_node(
    node: &mut Value,
    path: &str,
    place: Place,
    at_root: bool,
) -> Result<(), StrictError> {
    // Open, or already decided: the node's own keywords say so, whatever its children.
    let leave_open = is_open(node) || node.get("unevaluatedProperties").is_some();
    let Value::Object(object) = node else {
        return Ok(());
    };
    for (keyword, value) in object.iter_mut() {
        let here = pointer(path, keyword);
        match keyword.as_str() {
            "$defs" | "definitions" if at_root => {}
            "properties" | "patternProperties" => {
                if let Value::Object(entries) = value {
                    for (name, entry) in entries.iter_mut() {
                        close(entry, &pointer(&here, name), Place::Instance)?;
                    }
                }
            }
            "additionalProperties" => close(value, &here, Place::Instance)?,
            "items" => match value {
                Value::Array(entries) => {
                    for (index, entry) in entries.iter_mut().enumerate() {
                        close(entry, &pointer(&here, &index.to_string()), Place::Instance)?;
                    }
                }
                schema => close(schema, &here, Place::Instance)?,
            },
            "prefixItems" => {
                for (index, entry) in value.as_array_mut().into_iter().flatten().enumerate() {
                    close(entry, &pointer(&here, &index.to_string()), Place::Instance)?;
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                for (index, entry) in value.as_array_mut().into_iter().flatten().enumerate() {
                    close(entry, &pointer(&here, &index.to_string()), Place::InPlace)?;
                }
            }
            keyword if PASS_THROUGH.contains(&keyword) => {}
            _ => {
                return Err(StrictError {
                    keyword: keyword.clone(),
                    path: here,
                });
            }
        }
    }
    if place == Place::Instance && !leave_open {
        object.insert("unevaluatedProperties".into(), Value::Bool(false));
    }
    Ok(())
}

/// `path` extended by one JSON pointer segment.
fn pointer(path: &str, segment: &str) -> String {
    format!("{path}/{}", segment.replace('~', "~0").replace('/', "~1"))
}

/// One validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The JSON pointer of the failing value; empty for the root.
    pub instance_path: String,
    /// The JSON pointer of the failing keyword in the schema validated against.
    pub schema_path: String,
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The "… and N more" line names no schema position.
        if self.schema_path.is_empty() {
            return f.write_str(&self.message);
        }
        let instance = if self.instance_path.is_empty() {
            "/"
        } else {
            &self.instance_path
        };
        write!(
            f,
            "{instance}: {} (schema {})",
            self.message, self.schema_path
        )
    }
}

/// Failures reported per validation, before one "… and N more".
pub const MAX_FAILURES: usize = 20;

/// A compiled schema: draft 2020-12 selected explicitly, `format` an annotation (schemars'
/// integer formats), and no remote reference resolution (the validator is built without
/// its retrieval features; every published `$ref` is local).
pub struct Validator {
    compiled: jsonschema::Validator,
}

impl Validator {
    /// Compiles `schema`, after [`strict`] when `strict` is true.
    pub fn new(schema: &Value, strict: bool) -> Result<Validator, String> {
        let schema = if strict {
            self::strict(schema).map_err(|error| error.to_string())?
        } else {
            schema.clone()
        };
        jsonschema::draft202012::options()
            .should_validate_formats(false)
            .build(&schema)
            .map(|compiled| Validator { compiled })
            .map_err(|error| format!("the schema does not compile: {error}"))
    }

    /// Every failure, in the validator's order, at most [`MAX_FAILURES`], then one
    /// "… and N more" failure.
    pub fn validate(&self, instance: &Value) -> Vec<Failure> {
        cap(self.validate_all(instance))
    }

    /// Every failure, in the validator's order.
    fn validate_all(&self, instance: &Value) -> Vec<Failure> {
        self.compiled
            .iter_errors(instance)
            .map(|error| Failure {
                instance_path: error.instance_path().as_str().to_owned(),
                schema_path: error.schema_path().as_str().to_owned(),
                message: error.to_string(),
            })
            .collect()
    }
}

/// The first [`MAX_FAILURES`] failures, then one "… and N more" for the rest.
fn cap(mut failures: Vec<Failure>) -> Vec<Failure> {
    if failures.len() > MAX_FAILURES {
        let more = failures.len() - MAX_FAILURES;
        failures.truncate(MAX_FAILURES);
        failures.push(Failure {
            instance_path: String::new(),
            schema_path: String::new(),
            message: format!("… and {more} more"),
        });
    }
    failures
}

/// Every published schema in strict form, compiled once per process.
pub struct Schemas {
    /// Every `data_schema_for` name: the `COMMANDS` paths and the `ENVELOPE_ONLY` names.
    data: BTreeMap<String, Validator>,
    /// `error_variant(code)` for every code.
    errors: BTreeMap<ErrorCode, Validator>,
    /// `event_schemas()`, by wire name.
    events: BTreeMap<String, Validator>,
    /// `envelope_schema()`: the frame, validated without `data` and `error`.
    envelope: Validator,
}

/// The compiled schemas. A schema the transform refuses, or the validator cannot
/// compile, panics once, with its name and the reason.
pub fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let compiled = |name: &str, schema: Value| match Validator::new(&schema, true) {
            Ok(validator) => validator,
            Err(reason) => panic!("the published schema {name} does not compile: {reason}"), // ubs:ignore — a broken published schema stops the suite once, naming it
        };
        let names = app::COMMANDS
            .iter()
            .map(|entry| entry.name)
            .chain(app::ENVELOPE_ONLY.iter().map(|entry| entry.name));
        Schemas {
            data: names
                .filter_map(|name| {
                    let schema = schema::data_schema_for(name)?.to_value();
                    Some((name.to_owned(), compiled(&format!("`{name}` data"), schema)))
                })
                .collect(),
            errors: ErrorCode::ALL
                .iter()
                .map(|code| {
                    let schema = schema::error_variant(*code).to_value();
                    (*code, compiled(&format!("`{code}` error"), schema))
                })
                .collect(),
            events: schema::event_schemas()
                .into_iter()
                .map(|(name, schema)| {
                    let validator = compiled(&format!("`{name}` event"), schema.to_value());
                    (name.to_owned(), validator)
                })
                .collect(),
            envelope: compiled("envelope", schema::envelope_schema().to_value()),
        }
    })
}

/// Validates one envelope and its stderr, strictly, in four parts: `data` against its
/// command's schema, `error` against its code's variant (so `details` against that code's
/// typed specification), the frame (the rest: `partial`, `warnings`, `commands`, `meta`)
/// against the envelope's schema, and each stderr line against its event's. `Err` holds
/// every failure, formatted.
pub fn validate_envelope(envelope: &Value, stderr: &str) -> Result<(), String> {
    let schemas = schemas();
    let command = envelope["command"].as_str().unwrap_or_default();
    let mut lines = Vec::new();
    let mut part = |name: &str, failures: Vec<Failure>| {
        lines.extend(
            cap(failures)
                .iter()
                .map(|failure| format!("  {name} {failure}")),
        );
    };
    if let Some(data) = envelope.get("data") {
        match schemas.data.get(command) {
            Some(validator) => part("data", explain_unions(validator.validate_all(data), data)),
            None => part(
                "data",
                vec![note(format!(
                    "no schema for command `{command}`: add its COMMANDS row"
                ))],
            ),
        }
    }
    if let Some(error) = envelope.get("error") {
        let code = serde_json::from_value::<ErrorCode>(error["code"].clone()).ok();
        match code.and_then(|code| schemas.errors.get(&code)) {
            Some(validator) => part("error", validator.validate_all(error)),
            None => part(
                "error",
                vec![note(format!("no variant for code {}", error["code"]))],
            ),
        }
    }
    let mut frame = envelope.clone();
    if let Some(object) = frame.as_object_mut() {
        object.remove("data");
        object.remove("error");
    }
    part("frame", schemas.envelope.validate_all(&frame));
    for (index, line) in stderr.lines().enumerate() {
        let name = format!("stderr line {}", index + 1);
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            part(&name, vec![note(format!("not JSON: {line}"))]);
            continue;
        };
        let kind = event["event"].as_str().unwrap_or_default();
        match schemas.events.get(kind) {
            Some(validator) => part(&name, validator.validate_all(&event)),
            None => part(&name, vec![note(format!("no schema for event {kind:?}"))]),
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    Err(format!(
        "schema validation failed for `{command}` (exit {}):\n{}",
        envelope["exit_code"],
        lines.join("\n")
    ))
}

/// A failure that names no schema position.
fn note(message: String) -> Failure {
    Failure {
        instance_path: String::new(),
        schema_path: String::new(),
        message,
    }
}

/// Re-explains each failed union inside `data`: where a value that fails a `oneOf` or an
/// `anyOf` is an embedded error (an object whose `code` is a registered code, such as
/// `TargetResult.error`), its failures there are replaced by its failures against that
/// code's own variant, prefixed with its instance path. A union failure alone says only
/// that no branch matched.
fn explain_unions(failures: Vec<Failure>, data: &Value) -> Vec<Failure> {
    let schemas = schemas();
    let mut explained: BTreeMap<String, Vec<Failure>> = BTreeMap::new();
    for failure in &failures {
        if explained.contains_key(&failure.instance_path)
            || !(failure.schema_path.ends_with("/oneOf") || failure.schema_path.ends_with("/anyOf"))
        {
            continue;
        }
        let Some(value) = data.pointer(&failure.instance_path) else {
            continue;
        };
        // An error union by its `code`; `apply`'s wrapper union by the command it names,
        // whose own data its `result` must be.
        let (name, validator, inner_value, inner_path) =
            match serde_json::from_value::<ErrorCode>(value["code"].clone()) {
                Ok(code) => match schemas.errors.get(&code) {
                    Some(validator) => (format!("`{code}` error"), validator, value, ""),
                    None => continue,
                },
                Err(_) => match value["applied"]["command"]
                    .as_str()
                    .and_then(|command| Some((command, schemas.data.get(command)?)))
                {
                    Some((command, validator)) => (
                        format!("`{command}` result"),
                        validator,
                        &value["result"],
                        "/result",
                    ),
                    None => continue,
                },
            };
        let inner: Vec<Failure> = validator
            .validate_all(inner_value)
            .into_iter()
            .map(|inner| Failure {
                instance_path: format!(
                    "{}{inner_path}{}",
                    failure.instance_path, inner.instance_path
                ),
                schema_path: format!("{name} {}", inner.schema_path),
                message: inner.message,
            })
            .collect();
        if !inner.is_empty() {
            explained.insert(failure.instance_path.clone(), inner);
        }
    }
    // Every failure at an explained value (the union's, and the unevaluated keys its
    // failure leaves) gives way to the explanation, emitted once, where the first stood.
    let explained_paths: BTreeSet<String> = explained.keys().cloned().collect();
    let mut out = Vec::new();
    for failure in failures {
        match explained.remove(&failure.instance_path) {
            Some(inner) => out.extend(inner),
            None if explained_paths.contains(&failure.instance_path) => {}
            None => out.push(failure),
        }
    }
    out
}

/// The `(code, path)` pairs of every `details` one envelope shows: those of every object
/// in its `error` or `data` with a registered `code` and an object `details` (the
/// `error` itself, `TargetResult.error`, an item error a `details.items[]` keeps). Paths
/// are `DetailSpec::declared_paths`' syntax: `k`, `k.j`, `k[].j`.
pub fn observed_details(envelope: &Value) -> BTreeSet<(String, String)> {
    fn errors<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
        match value {
            Value::Object(object) => {
                let registered = object
                    .get("code")
                    .is_some_and(|code| serde_json::from_value::<ErrorCode>(code.clone()).is_ok());
                if registered && object.get("details").is_some_and(Value::is_object) {
                    out.push(value);
                }
                object.values().for_each(|v| errors(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| errors(v, out)),
            _ => {}
        }
    }
    fn paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, item) in object {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            out.insert(path.clone());
            match item {
                Value::Object(_) => paths(item, &path, out),
                Value::Array(items) => {
                    for element in items.iter().filter(|e| e.is_object()) {
                        paths(element, &format!("{path}[]"), out);
                    }
                }
                _ => {}
            }
        }
    }
    // An item error kept inside another error's `details.items[]` shows its own code's.
    let mut found = Vec::new();
    for part in ["error", "data"] {
        if let Some(value) = envelope.get(part) {
            errors(value, &mut found);
        }
    }
    let mut out = BTreeSet::new();
    for error in found {
        let code = error["code"].as_str().unwrap_or_default().to_owned();
        let mut keys = BTreeSet::new();
        paths(&error["details"], "", &mut keys);
        out.extend(keys.into_iter().map(|path| (code.clone(), path)));
    }
    out
}
