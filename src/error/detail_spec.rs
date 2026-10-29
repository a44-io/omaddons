//! The declared shape of error details, independent of their emitters.
//!
//! Published schemas are open for forward compatibility. The reference matcher
//! rejects undeclared keys so tests can detect changes to the contract.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Value, json};

/// A typed description of error details or a value inside them (M4-D10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailSpec {
    Str,
    Int,
    Num,
    Bool,
    /// A closed set of snake_case strings, in declaration order.
    Enum(&'static [&'static str]),
    Array(&'static DetailSpec),
    /// Exactly these keys, in sorted order.
    Object(&'static [Field]),
    /// A value matching exactly one branch.
    OneOf(&'static [&'static DetailSpec]),
    /// An object with unconstrained keys; the text explains why. It renders structurally
    /// open (`additionalProperties: true`), which the contract suite's strict form keeps.
    Opaque(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nullability {
    NotNull,
    Nullable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub key: &'static str,
    pub spec: &'static DetailSpec,
    pub presence: Presence,
    pub null: Nullability,
}

impl Field {
    pub const fn req(key: &'static str, spec: &'static DetailSpec) -> Self {
        Self {
            key,
            spec,
            presence: Presence::Required,
            null: Nullability::NotNull,
        }
    }

    pub const fn opt(key: &'static str, spec: &'static DetailSpec) -> Self {
        Self {
            key,
            spec,
            presence: Presence::Optional,
            null: Nullability::NotNull,
        }
    }

    pub const fn nullable(mut self) -> Self {
        self.null = Nullability::Nullable;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub path: String,
    pub expected: String,
    pub found: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: expected {}, found {}",
            location(&self.path),
            self.expected,
            self.found
        )
    }
}

fn location(path: &str) -> String {
    if path.is_empty() {
        "details".to_owned()
    } else {
        format!("details.{path}")
    }
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn snake_case(value: &str) -> bool {
    value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl DetailSpec {
    pub const EMPTY: Self = Self::Object(&[]);

    /// Render an open JSON Schema fragment in deterministic declaration order.
    pub fn to_json_schema(&self) -> Value {
        self.render(false)
    }

    fn render(&self, nullable: bool) -> Value {
        let mut schema = match self {
            Self::Str => json!({"type": "string"}),
            Self::Int => json!({"type": "integer"}),
            Self::Num => json!({"type": "number"}),
            Self::Bool => json!({"type": "boolean"}),
            Self::Enum(values) => json!({"enum": values}),
            Self::Array(item) => json!({"type": "array", "items": item.to_json_schema()}),
            Self::Object(fields) => {
                let mut schema = json!({"type": "object"});
                if !fields.is_empty() {
                    let properties: serde_json::Map<String, Value> = fields
                        .iter()
                        .map(|field| {
                            (
                                field.key.to_owned(),
                                field.spec.render(field.null == Nullability::Nullable),
                            )
                        })
                        .collect();
                    schema["properties"] = Value::Object(properties);
                }
                let required: Vec<_> = fields
                    .iter()
                    .filter(|field| field.presence == Presence::Required)
                    .map(|field| field.key)
                    .collect();
                if !required.is_empty() {
                    schema["required"] = json!(required);
                }
                schema
            }
            Self::OneOf(branches) => {
                json!({"oneOf": branches.iter().map(|branch| branch.to_json_schema()).collect::<Vec<_>>()})
            }
            Self::Opaque(reason) => {
                json!({"type": "object", "description": reason, "additionalProperties": true})
            }
        };
        if nullable && let Some(object) = schema.as_object_mut() {
            if let Some(Value::Array(values)) = object.get_mut("enum") {
                values.push(Value::Null);
            } else if let Some(Value::Array(branches)) = object.get_mut("oneOf") {
                branches.push(json!({"type": "null"}));
            } else if let Some(kind) = object.get_mut("type") {
                *kind = json!([kind.clone(), "null"]);
            }
        }
        schema
    }

    /// Check the declared shape, including unknown keys and exclusive branches.
    pub fn admits(&self, value: &Value) -> Result<(), Mismatch> {
        self.admit_at(value, "")
    }

    fn expected(&self) -> String {
        match self {
            Self::Str => "string".to_owned(),
            Self::Int => "integer".to_owned(),
            Self::Num => "number".to_owned(),
            Self::Bool => "boolean".to_owned(),
            Self::Enum(values) => format!("one of {values:?}"),
            Self::Array(_) => "array".to_owned(),
            Self::Object(_) | Self::Opaque(_) => "object".to_owned(),
            Self::OneOf(_) => "exactly one branch".to_owned(),
        }
    }

    fn mismatch(&self, path: &str, found: impl Into<String>) -> Mismatch {
        Mismatch {
            path: path.to_owned(),
            expected: self.expected(),
            found: found.into(),
        }
    }

    fn admit_at(&self, value: &Value, path: &str) -> Result<(), Mismatch> {
        let matches = match self {
            Self::Str => value.is_string(),
            Self::Int => value.is_i64() || value.is_u64(),
            Self::Num => value.is_number(),
            Self::Bool => value.is_boolean(),
            Self::Enum(values) => value.as_str().is_some_and(|value| values.contains(&value)),
            Self::Array(item) => {
                if let Some(values) = value.as_array() {
                    for (index, value) in values.iter().enumerate() {
                        item.admit_at(value, &format!("{path}[{index}]"))?;
                    }
                    true
                } else {
                    false
                }
            }
            Self::Object(fields) => {
                if let Some(object) = value.as_object() {
                    for field in *fields {
                        let field_path = child_path(path, field.key);
                        match object.get(field.key) {
                            None if field.presence == Presence::Required => {
                                return Err(field.spec.mismatch(&field_path, "missing"));
                            }
                            Some(Value::Null) if field.null == Nullability::Nullable => {}
                            Some(value) => field.spec.admit_at(value, &field_path)?,
                            None => {}
                        }
                    }
                    for (key, value) in object {
                        if !fields.iter().any(|field| field.key == key) {
                            return Err(Mismatch {
                                path: child_path(path, key),
                                expected: "a declared key".to_owned(),
                                found: value.to_string(),
                            });
                        }
                    }
                    true
                } else {
                    false
                }
            }
            Self::OneOf(branches) => {
                let count = branches
                    .iter()
                    .filter(|branch| branch.admit_at(value, path).is_ok())
                    .count();
                if count != 1 {
                    let found = if count == 0 {
                        "matches no branch".to_owned()
                    } else {
                        format!("matches {count} branches")
                    };
                    return Err(self.mismatch(path, found));
                }
                true
            }
            Self::Opaque(_) => value.is_object(),
        };
        if matches {
            Ok(())
        } else {
            Err(self.mismatch(path, value.to_string()))
        }
    }

    /// Every declared key position, sorted and deduplicated.
    pub fn declared_paths(&self) -> Vec<String> {
        let mut paths = BTreeSet::new();
        self.walk("", false, &mut paths);
        paths.into_iter().collect()
    }

    /// Positions whose object contents deliberately remain unconstrained.
    pub fn opaque_paths(&self) -> Vec<String> {
        let mut paths = BTreeSet::new();
        self.walk("", true, &mut paths);
        paths.into_iter().collect()
    }

    fn walk(&self, path: &str, opaque_only: bool, paths: &mut BTreeSet<String>) {
        match self {
            Self::Object(fields) => {
                for field in *fields {
                    let next = child_path(path, field.key);
                    if !opaque_only {
                        paths.insert(next.clone());
                    }
                    field.spec.walk(&next, opaque_only, paths);
                }
            }
            Self::Array(item) => item.walk(&format!("{path}[]"), opaque_only, paths),
            Self::OneOf(branches) => {
                for branch in *branches {
                    branch.walk(path, opaque_only, paths);
                }
            }
            Self::Opaque(_) if opaque_only => {
                paths.insert(path.to_owned());
            }
            _ => {}
        }
    }

    pub fn field(&self, key: &str) -> Option<&'static Field> {
        match self {
            Self::Object(fields) => fields.iter().find(|field| field.key == key),
            _ => None,
        }
    }

    pub fn enum_values(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Enum(values) => Some(values),
            Self::Array(item) => item.enum_values(),
            _ => None,
        }
    }

    /// Check that declarations are canonical and cannot silently drift.
    pub fn well_formed(&self) -> Result<(), String> {
        self.well_formed_at("")
    }

    fn well_formed_at(&self, path: &str) -> Result<(), String> {
        let fault = |why: &str| format!("{}: {why}", location(path));
        match self {
            Self::Object(fields) => {
                let mut previous = None;
                for field in *fields {
                    let next = child_path(path, field.key);
                    if !snake_case(field.key) {
                        return Err(format!("{}: key is not snake_case", location(&next)));
                    }
                    if let Some(previous) = previous
                        && field.key <= previous
                    {
                        return Err(format!(
                            "{}: keys must be unique and sorted",
                            location(&next)
                        ));
                    }
                    previous = Some(field.key);
                    field.spec.well_formed_at(&next)?;
                }
            }
            Self::Enum(values) => {
                if values.is_empty() {
                    return Err(fault("enum must not be empty"));
                }
                let mut seen = BTreeSet::new();
                for value in *values {
                    if !snake_case(value) {
                        return Err(fault(&format!("enum value {value:?} is not snake_case")));
                    }
                    if !seen.insert(value) {
                        return Err(fault(&format!("duplicate enum value {value:?}")));
                    }
                }
            }
            Self::Array(item) => item.well_formed_at(&format!("{path}[]"))?,
            Self::OneOf(branches) => {
                if branches.len() < 2 {
                    return Err(fault("oneOf needs at least two branches"));
                }
                for (index, branch) in branches.iter().enumerate() {
                    if branches[..index].contains(branch) {
                        return Err(fault("oneOf has equal branches"));
                    }
                    branch.well_formed_at(path)?;
                }
            }
            Self::Opaque(reason) if reason.trim().is_empty() => {
                return Err(fault("opaque reason must not be empty"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Check the structure of the small schema vocabulary rendered above.
/// Full JSON Schema validation belongs to the contract schema suite (A3).
#[cfg(test)]
pub(crate) fn check_rendered(schema: &Value) -> Result<(), String> {
    let object = schema.as_object().ok_or("schema must be an object")?;
    for (key, value) in object {
        match key.as_str() {
            "type" => {
                let valid = |value: &Value| {
                    matches!(
                        value.as_str(),
                        Some(
                            "string"
                                | "integer"
                                | "number"
                                | "boolean"
                                | "array"
                                | "object"
                                | "null"
                        )
                    )
                };
                if let Some(values) = value.as_array() {
                    let mut seen = BTreeSet::new();
                    if values.is_empty()
                        || values
                            .iter()
                            .any(|value| !valid(value) || !seen.insert(value.as_str()))
                    {
                        return Err("type must list distinct JSON type names".to_owned());
                    }
                } else if !valid(value) {
                    return Err("type must be a JSON type name".to_owned());
                }
            }
            "enum" => {
                let values = value.as_array().ok_or("enum must be an array")?;
                let mut seen = BTreeSet::new();
                let mut null_seen = false;
                if values.is_empty() {
                    return Err("enum must not be empty".to_owned());
                }
                for value in values {
                    match value {
                        Value::String(value) if seen.insert(value) => {}
                        Value::Null if !null_seen => {
                            null_seen = true;
                        }
                        _ => {
                            return Err("enum must contain distinct strings and at most one null"
                                .to_owned());
                        }
                    }
                }
            }
            "items" => check_rendered(value)?,
            "properties" => {
                let fields = value.as_object().ok_or("properties must be an object")?;
                for schema in fields.values() {
                    check_rendered(schema)?;
                }
            }
            "required" => {
                let required = value.as_array().ok_or("required must be an array")?;
                let mut seen = BTreeSet::new();
                for key in required {
                    let key = key.as_str().ok_or("required keys must be strings")?;
                    if !seen.insert(key) {
                        return Err(format!("duplicate required key {key:?}"));
                    }
                    if !object
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|fields| fields.contains_key(key))
                    {
                        return Err(format!("required key {key:?} is not in properties"));
                    }
                }
            }
            "oneOf" => {
                let branches = value.as_array().ok_or("oneOf must be an array")?;
                if branches.is_empty() {
                    return Err("oneOf must not be empty".to_owned());
                }
                for branch in branches {
                    check_rendered(branch)?;
                }
            }
            "description" if value.as_str().is_some_and(|value| !value.trim().is_empty()) => {}
            "description" => return Err("description must be a non-empty string".to_owned()),
            // Only an opaque object opens its keys; nothing renders a closed one.
            "additionalProperties" if value == &Value::Bool(true) => {}
            "additionalProperties" => return Err("additionalProperties must be true".to_owned()),
            _ => return Err(format!("unknown schema keyword {key:?}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STR: DetailSpec = DetailSpec::Str;
    const INT: DetailSpec = DetailSpec::Int;
    const ENUM: DetailSpec = DetailSpec::Enum(&["x", "y"]);
    const ARRAY: DetailSpec = DetailSpec::Array(&STR);
    const OBJECT: DetailSpec = DetailSpec::Object(&[Field::req("a", &STR), Field::opt("b", &INT)]);
    const ONE_OF: DetailSpec = DetailSpec::OneOf(&[&STR, &INT]);
    const OPAQUE: DetailSpec =
        DetailSpec::Opaque("Nested error details are specified by their own code.");
    const NESTED: DetailSpec = DetailSpec::Object(&[Field::req(
        "folders",
        &DetailSpec::Array(&DetailSpec::Object(&[Field::req("owner", &STR)])),
    )]);

    fn rendering_cases() -> Vec<(&'static str, DetailSpec, Value)> {
        vec![
            ("str", STR, json!({"type":"string"})),
            ("int", INT, json!({"type":"integer"})),
            ("num", DetailSpec::Num, json!({"type":"number"})),
            ("bool", DetailSpec::Bool, json!({"type":"boolean"})),
            ("enum", ENUM, json!({"enum":["x","y"]})),
            (
                "array_of_str",
                ARRAY,
                json!({"type":"array","items":{"type":"string"}}),
            ),
            (
                "object_required_and_optional",
                OBJECT,
                json!({"type":"object","properties":{"a":{"type":"string"},"b":{"type":"integer"}},"required":["a"]}),
            ),
            (
                "object_without_required",
                const { DetailSpec::Object(&[Field::opt("a", &STR)]) },
                json!({"type":"object","properties":{"a":{"type":"string"}}}),
            ),
            ("empty_object", DetailSpec::EMPTY, json!({"type":"object"})),
            (
                "nullable_str_field",
                const { DetailSpec::Object(&[Field::opt("a", &STR).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["string","null"]}}}),
            ),
            (
                "nullable_int_field",
                const { DetailSpec::Object(&[Field::opt("a", &INT).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["integer","null"]}}}),
            ),
            (
                "nullable_num_field",
                const { DetailSpec::Object(&[Field::opt("a", &DetailSpec::Num).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["number","null"]}}}),
            ),
            (
                "nullable_bool_field",
                const { DetailSpec::Object(&[Field::opt("a", &DetailSpec::Bool).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["boolean","null"]}}}),
            ),
            (
                "nullable_enum_field",
                const { DetailSpec::Object(&[Field::opt("a", &ENUM).nullable()]) },
                json!({"type":"object","properties":{"a":{"enum":["x","y",null]}}}),
            ),
            (
                "nullable_object_field",
                const { DetailSpec::Object(&[Field::opt("a", &DetailSpec::EMPTY).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["object","null"]}}}),
            ),
            (
                "nullable_array_field",
                const { DetailSpec::Object(&[Field::opt("a", &ARRAY).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["array","null"],"items":{"type":"string"}}}}),
            ),
            (
                "one_of",
                ONE_OF,
                json!({"oneOf":[{"type":"string"},{"type":"integer"}]}),
            ),
            (
                "nullable_one_of",
                const { DetailSpec::Object(&[Field::opt("a", &ONE_OF).nullable()]) },
                json!({"type":"object","properties":{"a":{"oneOf":[{"type":"string"},{"type":"integer"},{"type":"null"}]}}}),
            ),
            (
                "opaque",
                OPAQUE,
                json!({"type":"object","description":"Nested error details are specified by their own code.","additionalProperties":true}),
            ),
            (
                "nullable_opaque",
                const { DetailSpec::Object(&[Field::opt("a", &OPAQUE).nullable()]) },
                json!({"type":"object","properties":{"a":{"type":["object","null"],"description":"Nested error details are specified by their own code.","additionalProperties":true}}}),
            ),
            (
                "nested_array_of_objects",
                NESTED,
                json!({"type":"object","properties":{"folders":{"type":"array","items":{"type":"object","properties":{"owner":{"type":"string"}},"required":["owner"]}}},"required":["folders"]}),
            ),
        ]
    }

    #[test]
    fn renders_each_variant() {
        for (name, spec, expected) in rendering_cases() {
            let got = spec.to_json_schema();
            assert_eq!(got, expected, "case {name}: {spec:?}");
            assert_eq!(
                serde_json::to_string(&got).unwrap(),
                serde_json::to_string(&spec.to_json_schema()).unwrap(),
                "case {name}: deterministic bytes"
            );
        }
    }

    #[test]
    fn specs_are_const_items() {
        assert_eq!(
            NESTED.to_json_schema()["properties"]["folders"]["items"]["properties"]["owner"],
            json!({"type":"string"})
        );
        let nullable = Field::req("holder", &OBJECT).nullable();
        assert_eq!(nullable.presence, Presence::Required);
        assert_eq!(nullable.null, Nullability::Nullable);
    }

    #[test]
    fn admits_and_rejects() {
        let cases = [
            ("str_ok", STR, json!("x"), None),
            ("str_given_int", STR, json!(1), Some("")),
            ("int_ok", INT, json!(1), None),
            ("unsigned_int_ok", INT, json!(u64::MAX), None),
            ("int_given_float", INT, json!(1.0), Some("")),
            ("num_given_int", DetailSpec::Num, json!(1), None),
            ("num_given_float", DetailSpec::Num, json!(1.5), None),
            ("bool_ok", DetailSpec::Bool, json!(true), None),
            (
                "bool_given_string",
                DetailSpec::Bool,
                json!("true"),
                Some(""),
            ),
            ("enum_member", ENUM, json!("x"), None),
            ("enum_outsider", ENUM, json!("z"), Some("")),
            ("enum_given_null", ENUM, Value::Null, Some("")),
            ("missing_required", OBJECT, json!({}), Some("a")),
            ("missing_optional", OBJECT, json!({"a":"x"}), None),
            ("present_optional", OBJECT, json!({"a":"x","b":2}), None),
            (
                "unknown_key",
                OBJECT,
                json!({"a":"x","extra":0}),
                Some("extra"),
            ),
            ("null_for_not_null", OBJECT, json!({"a":null}), Some("a")),
            (
                "null_for_nullable",
                const { DetailSpec::Object(&[Field::req("a", &STR).nullable()]) },
                json!({"a":null}),
                None,
            ),
            (
                "nullable_required_still_required",
                const { DetailSpec::Object(&[Field::req("a", &STR).nullable()]) },
                json!({}),
                Some("a"),
            ),
            (
                "nested_failure",
                NESTED,
                json!({"folders":[{"owner":"a"},{"owner":2}]}),
                Some("folders[1].owner"),
            ),
            (
                "nested_ok",
                NESTED,
                json!({"folders":[{"owner":"a"}]}),
                None,
            ),
            ("array_given_object", ARRAY, json!({}), Some("")),
            ("array_item_failure", ARRAY, json!(["x", 2]), Some("[1]")),
            ("one_of_no_branch", ONE_OF, json!(true), Some("")),
            (
                "one_of_two_branches",
                const {
                    DetailSpec::OneOf(&[
                        &DetailSpec::Object(&[Field::opt("a", &STR)]),
                        &DetailSpec::Object(&[Field::opt("b", &STR)]),
                    ])
                },
                json!({}),
                Some(""),
            ),
            ("one_of_one_branch", ONE_OF, json!(2), None),
            (
                "opaque_any_object",
                OPAQUE,
                json!({"anything":{"goes":[1,null,true]}}),
                None,
            ),
            ("opaque_given_string", OPAQUE, json!("x"), Some("")),
            ("object_given_array", OBJECT, json!([]), Some("")),
        ];
        for (name, spec, value, expected_path) in cases {
            let got = spec.admits(&value);
            match expected_path {
                None => assert!(got.is_ok(), "case {name}: {spec:?} value {value}: {got:?}"),
                Some(path) => {
                    let error = got.unwrap_err();
                    assert_eq!(
                        error.path, path,
                        "case {name}: {spec:?} value {value}: {error:?}"
                    );
                    assert!(
                        !error.expected.is_empty() && !error.found.is_empty(),
                        "case {name}: {error:?}"
                    );
                }
            }
        }
        assert_eq!(
            ENUM.admits(&json!("z")).unwrap_err().expected,
            "one of [\"x\", \"y\"]"
        );
        assert_eq!(
            ONE_OF.admits(&json!(true)).unwrap_err().found,
            "matches no branch"
        );
        assert_eq!(
            DetailSpec::OneOf(&[&STR, &STR])
                .admits(&json!("x"))
                .unwrap_err()
                .found,
            "matches 2 branches"
        );
    }

    #[test]
    fn mismatch_names_the_path_and_both_sides() {
        for (path, label) in [
            ("", "details"),
            ("holder.pid", "details.holder.pid"),
            ("folders[1].owner", "details.folders[1].owner"),
        ] {
            let error = Mismatch {
                path: path.to_owned(),
                expected: "string".to_owned(),
                found: "2".to_owned(),
            };
            assert_eq!(
                error.to_string(),
                format!("{label}: expected string, found 2"),
                "path {path:?}"
            );
        }
    }

    #[test]
    fn well_formed_rejects_each_fault() {
        const BAD_NESTED: DetailSpec = DetailSpec::Object(&[Field::opt(
            "folders",
            &DetailSpec::Array(&DetailSpec::Object(&[Field::opt(
                "owner",
                &DetailSpec::Enum(&[]),
            )])),
        )]);
        let cases = [
            (
                "unsorted_keys",
                const { DetailSpec::Object(&[Field::opt("b", &STR), Field::opt("a", &STR)]) },
                "details.a",
            ),
            (
                "duplicate_key",
                const { DetailSpec::Object(&[Field::opt("a", &STR), Field::opt("a", &STR)]) },
                "details.a",
            ),
            (
                "camel_case_key",
                const { DetailSpec::Object(&[Field::opt("someKey", &STR)]) },
                "details.someKey",
            ),
            (
                "empty_key",
                const { DetailSpec::Object(&[Field::opt("", &STR)]) },
                "details",
            ),
            ("empty_enum", DetailSpec::Enum(&[]), "details"),
            (
                "duplicate_enum_value",
                DetailSpec::Enum(&["a", "a"]),
                "details",
            ),
            (
                "enum_value_with_dash",
                DetailSpec::Enum(&["some-value"]),
                "details",
            ),
            (
                "enum_value_with_capital",
                DetailSpec::Enum(&["Value"]),
                "details",
            ),
            ("one_branch_one_of", DetailSpec::OneOf(&[&STR]), "details"),
            ("empty_one_of", DetailSpec::OneOf(&[]), "details"),
            (
                "equal_one_of_branches",
                DetailSpec::OneOf(&[&STR, &STR]),
                "details",
            ),
            ("empty_opaque_reason", DetailSpec::Opaque(""), "details"),
            ("blank_opaque_reason", DetailSpec::Opaque("  "), "details"),
            ("fault_deep_inside", BAD_NESTED, "details.folders[].owner"),
            (
                "fault_inside_branch",
                DetailSpec::OneOf(&[&STR, &DetailSpec::Enum(&[])]),
                "details",
            ),
        ];
        for (name, spec, path) in cases {
            let error = spec.well_formed().unwrap_err();
            assert!(
                error.contains(path),
                "case {name}: {spec:?}: expected path {path:?}, got {error}"
            );
        }
    }

    #[test]
    fn well_formed_accepts_a_correct_nested_spec() {
        for (name, spec, _) in rendering_cases() {
            assert_eq!(spec.well_formed(), Ok(()), "case {name}: {spec:?}");
        }
    }

    const WALK: DetailSpec = DetailSpec::Object(&[
        Field::opt(
            "a",
            &DetailSpec::Array(&DetailSpec::Object(&[Field::opt("b", &STR)])),
        ),
        Field::opt("c", &DetailSpec::OneOf(&[&STR, &ARRAY])),
        Field::opt("d", &DetailSpec::Object(&[Field::opt("e", &OPAQUE)])),
    ]);

    #[test]
    fn declared_paths_walk_every_level() {
        assert_eq!(WALK.declared_paths(), ["a", "a[].b", "c", "d", "d.e"]);
        const UNION: DetailSpec = DetailSpec::OneOf(&[
            &DetailSpec::Object(&[Field::opt("a", &STR), Field::opt("c", &STR)]),
            &DetailSpec::Object(&[Field::opt("a", &INT), Field::opt("b", &STR)]),
        ]);
        assert_eq!(UNION.declared_paths(), ["a", "b", "c"]);
        assert!(DetailSpec::EMPTY.declared_paths().is_empty());
        assert!(OPAQUE.declared_paths().is_empty());
    }

    #[test]
    fn opaque_paths_find_each_opaque_node() {
        assert_eq!(WALK.opaque_paths(), ["d.e"]);
        const ITEMS: DetailSpec = DetailSpec::Object(&[Field::opt(
            "items",
            &DetailSpec::Array(&DetailSpec::Object(&[Field::opt(
                "error",
                &DetailSpec::Object(&[Field::opt("details", &OPAQUE)]),
            )])),
        )]);
        assert_eq!(ITEMS.opaque_paths(), ["items[].error.details"]);
        assert_eq!(
            const {
                DetailSpec::OneOf(&[
                    &OPAQUE,
                    &DetailSpec::Object(&[Field::opt("nested", &OPAQUE)]),
                ])
            }
            .opaque_paths(),
            ["", "nested"]
        );
        assert!(NESTED.opaque_paths().is_empty());
    }

    #[test]
    fn field_and_enum_values_reach_inside() {
        const CHANGED: DetailSpec = DetailSpec::Array(&DetailSpec::Enum(&["state", "source"]));
        const SPEC: DetailSpec = DetailSpec::Object(&[Field::opt("changed", &CHANGED)]);
        assert_eq!(
            SPEC.field("changed").unwrap().spec.enum_values(),
            Some(["state", "source"].as_slice())
        );
        assert_eq!(ENUM.enum_values(), Some(["x", "y"].as_slice()));
        assert!(SPEC.field("missing").is_none());
        assert!(STR.field("changed").is_none());
        assert!(STR.enum_values().is_none());
        assert!(SPEC.enum_values().is_none());
    }

    #[test]
    fn rendered_schemas_pass_the_structural_check() {
        for (name, spec, _) in rendering_cases() {
            let schema = spec.to_json_schema();
            assert_eq!(check_rendered(&schema), Ok(()), "case {name}: {schema}");
        }
    }

    #[test]
    fn the_structural_check_rejects_each_fault() {
        let cases = [
            ("unknown_keyword", json!({"pattern":"^a$"})),
            (
                "closed_additional_properties",
                json!({"additionalProperties":false}),
            ),
            (
                "additional_properties_schema",
                json!({"additionalProperties":{"type":"string"}}),
            ),
            ("bad_type_name", json!({"type":"float"})),
            ("empty_type_array", json!({"type":[]})),
            ("duplicate_type", json!({"type":["string","string"]})),
            ("bad_type_in_array", json!({"type":["string",3]})),
            ("empty_enum", json!({"enum":[]})),
            ("duplicate_enum", json!({"enum":["a","a"]})),
            ("duplicate_null", json!({"enum":[null,null]})),
            ("non_string_enum", json!({"enum":[2]})),
            ("enum_not_array", json!({"enum":"a"})),
            ("required_not_in_properties", json!({"required":["a"]})),
            (
                "duplicate_required",
                json!({"properties":{"a":{}},"required":["a","a"]}),
            ),
            ("required_not_array", json!({"required":"a"})),
            ("required_not_string", json!({"required":[1]})),
            ("items_not_a_schema", json!({"items":true})),
            ("bad_nested_item", json!({"items":{"type":"bad"}})),
            ("properties_not_object", json!({"properties":[]})),
            ("bad_property", json!({"properties":{"a":false}})),
            ("one_of_not_array", json!({"oneOf":{}})),
            ("empty_one_of", json!({"oneOf":[]})),
            ("bad_one_of_branch", json!({"oneOf":[{} ,1]})),
            ("empty_description", json!({"description":""})),
            ("blank_description", json!({"description":" "})),
            ("description_not_string", json!({"description":1})),
        ];
        for (name, schema) in cases {
            assert!(check_rendered(&schema).is_err(), "case {name}: {schema}");
        }
    }
}
