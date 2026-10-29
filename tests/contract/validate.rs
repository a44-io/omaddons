//! The contract suite's JSON Schema tools (M4-D9): the strict form of a published schema,
//! and a validator. Published schemas stay open, so a newer minor's additions never break
//! an older consumer's validator; the suite closes them here, so an emitted key a schema
//! lacks fails. The validator crate is used in this file only. It depends only on the
//! library, `serde_json` and the validator, so another test binary can include it with
//! `#[path = "contract/validate.rs"] mod validate;`.

use std::fmt;

use omaddons::contract::schema::is_open;
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
        let mut failures = Vec::new();
        let mut more = 0usize;
        for error in self.compiled.iter_errors(instance) {
            if failures.len() < MAX_FAILURES {
                failures.push(Failure {
                    instance_path: error.instance_path().as_str().to_owned(),
                    schema_path: error.schema_path().as_str().to_owned(),
                    message: error.to_string(),
                });
            } else {
                more += 1;
            }
        }
        if more > 0 {
            failures.push(Failure {
                instance_path: String::new(),
                schema_path: String::new(),
                message: format!("… and {more} more"),
            });
        }
        failures
    }
}
