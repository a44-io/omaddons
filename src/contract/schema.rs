//! The JSON Schemas of command data, the envelope, stderr events and saved files.
//! M4-D9 uses the serialize contract, so omitted fields are optional. Each root gets
//! a fresh generator, so its definitions depend only on its own type. Published
//! schemas stay open; strictness belongs to the contract suite's test transform.

use std::collections::BTreeMap;

use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

use crate::app::{self, CommandEntry};
use crate::engine::journal::JournalLine;
use crate::error::{Error, ErrorBody, ErrorCode, Result, Suggestion, error_variant_value};
use crate::output::Envelope;
use crate::state::{desired::DesiredStateSchema, store};

/// One published JSON Schema (draft 2020-12) or schema fragment: JSON Schema's own
/// vocabulary, described by the draft 2020-12 meta-schema, not by omaddons' wire names.
/// Its own schema is structurally open (`contract::OPAQUE`).
///
/// It prints as schemars 1.x prints a `Schema` (omad-ho1p), so the binary can print
/// schemas it embedded without schemars: in a schema object, the keywords `$id`,
/// `$schema`, `title`, `description`, `type`, `format` and `properties` first, in that
/// order, `$defs` and `definitions` last, and the others between in the map's own order
/// (sorted: serde_json is built without `preserve_order`). Every subschema prints the same
/// way: the values of `properties`, `patternProperties`, `dependentSchemas`, `$defs` and
/// `definitions` (whose own keys keep their order), array items, and any other keyword's
/// object value. `examples`, `default` and `x-…` values print as plain JSON. A test holds
/// this to schemars's own output for every schema omaddons publishes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct SchemaDoc(Value);

impl SchemaDoc {
    /// `value` as a schema, or `None` unless it is an object or a boolean, the two forms a
    /// JSON Schema takes.
    pub fn new(value: Value) -> Option<Self> {
        (value.is_object() || value.is_boolean()).then_some(Self(value))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    /// `code`'s typed `details`, as the fragment `robot-docs errors` and `capabilities`
    /// publish. Its one failure is `internal`: a specification that renders to no schema.
    pub fn details(code: ErrorCode) -> Result<Self> {
        Self::new(code.details_schema())
            .ok_or_else(|| Error::internal(format!("{code}: its details schema is no schema")))
    }
}

impl From<Schema> for SchemaDoc {
    fn from(schema: Schema) -> Self {
        Self(schema.to_value())
    }
}

impl Serialize for SchemaDoc {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        Keywords::schema(&self.0).serialize(serializer)
    }
}

/// The keywords a schema object prints first, in this order, and last (schemars 1.x).
const KEYWORDS_FIRST: [&str; 7] = [
    "$id",
    "$schema",
    "title",
    "description",
    "type",
    "format",
    "properties",
];
const KEYWORDS_LAST: [&str; 2] = ["$defs", "definitions"];
/// The keywords whose value names subschemas: its keys keep their order.
const SCHEMA_MAPS: [&str; 5] = [
    "properties",
    "patternProperties",
    "dependentSchemas",
    "$defs",
    "definitions",
];

/// A value at a schema position, printed in [`SchemaDoc`]'s keyword order.
struct Keywords<'a> {
    value: &'a Value,
    /// An object of named subschemas (`properties`, `$defs`, …), not a schema.
    named: bool,
}

impl<'a> Keywords<'a> {
    fn schema(value: &'a Value) -> Self {
        Self {
            value,
            named: false,
        }
    }
}

impl Serialize for Keywords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.value {
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&Keywords::schema(item))?;
                }
                seq.end()
            }
            Value::Object(object) if self.named => {
                let mut map = serializer.serialize_map(Some(object.len()))?;
                for (name, schema) in object {
                    map.serialize_entry(name, &Keywords::schema(schema))?;
                }
                map.end()
            }
            Value::Object(object) => {
                let entries = object.iter().map(|(key, value)| (key.as_str(), value));
                let first = KEYWORDS_FIRST
                    .iter()
                    .filter_map(|key| object.get(*key).map(|value| (*key, value)));
                let middle = entries.filter(|(key, _)| {
                    !KEYWORDS_FIRST.contains(key) && !KEYWORDS_LAST.contains(key)
                });
                let last = KEYWORDS_LAST
                    .iter()
                    .filter_map(|key| object.get(*key).map(|value| (*key, value)));
                let mut map = serializer.serialize_map(Some(object.len()))?;
                for (key, value) in first.chain(middle).chain(last) {
                    if matches!(key, "examples" | "default") || key.starts_with("x-") {
                        map.serialize_entry(key, value)?;
                    } else {
                        let named = SCHEMA_MAPS.contains(&key);
                        map.serialize_entry(key, &Keywords { value, named })?;
                    }
                }
                map.end()
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                self.value.serialize(serializer)
            }
        }
    }
}

impl schemars::JsonSchema for SchemaDoc {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SchemaDoc".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": true,
            "description": "A JSON Schema (draft 2020-12): JSON Schema's own vocabulary, not omaddons' wire names",
        })
    }
}

/// The settings every published schema is generated with (M4-D9).
pub fn settings() -> SchemaSettings {
    SchemaSettings::draft2020_12().for_serialize()
}

/// One fresh generator per root; never reuse it for another schema.
pub fn generator() -> SchemaGenerator {
    settings().into_generator()
}

/// The schema of a serialized `jiff::Timestamp`, an RFC 3339 UTC string (`…Z`). Its
/// `format: date-time` lets the contract's rule 1 find every instant by its type, under
/// whatever key it sits (`tests/contract/rules.rs`, `r1-instant-type`). Used as
/// `#[schemars(schema_with = "crate::contract::schema::timestamp")]` on every
/// `Timestamp` field; `format` is an annotation, so the schema stays as open as before.
pub fn timestamp(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type": "string", "format": "date-time"})
}

/// [`timestamp`] for an `Option<Timestamp>`: the instant, or `null`.
pub fn timestamp_opt(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({"type": ["string", "null"], "format": "date-time"})
}

/// The schema of the `--reason` a person gave, recorded verbatim and echoed back (a
/// history line's, a backup's, the last change's): bounded free text, at most
/// [`REASON_MAX_CHARS`](crate::limits::REASON_MAX_CHARS) characters, or `null` when none
/// was given. Its `maxLength` says it is no vocabulary (`tests/contract/rules.rs`,
/// `r4-open-enum`). Used as
/// `#[schemars(schema_with = "crate::contract::schema::reason_text")]`.
pub fn reason_text(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "maxLength": crate::limits::REASON_MAX_CHARS,
    })
}

/// One command's data schema, titled with its canonical command path.
pub fn command_schema(entry: &CommandEntry) -> Schema {
    let mut schema = (entry.data)(&mut generator());
    schema.insert("title".into(), json!(entry.name));
    schema
}

/// Every command row's data schema, keyed by its canonical path.
pub fn command_schemas() -> BTreeMap<&'static str, Schema> {
    app::COMMANDS
        .iter()
        .map(|entry| (entry.name, command_schema(entry)))
        .collect()
}

/// The data schema for any envelope command, including the two envelope-only names.
pub fn data_schema_for(command: &str) -> Option<Schema> {
    if let Some(entry) = app::command_entry(command) {
        return Some(command_schema(entry));
    }
    let entry = app::ENVELOPE_ONLY
        .iter()
        .find(|entry| entry.name == command)?;
    let mut schema = (entry.data)(&mut generator());
    schema.insert("title".into(), json!(entry.name));
    Some(schema)
}

/// The common frame; data stays open because each command supplies its own schema.
pub fn envelope_schema() -> Schema {
    generator().root_schema_for::<Envelope<'static, serde_json::Value>>()
}

/// The error union (M4-D10): one variant per code, discriminated by `code`.
pub fn error_schema() -> Schema {
    let mut schema = generator().root_schema_for::<ErrorBody>();
    schema.insert("title".into(), json!("ErrorBody"));
    schema
}

/// One code's variant of the union as a root of its own, so a failure can be checked
/// against its code alone.
pub fn error_variant(code: ErrorCode) -> Schema {
    let mut generator = generator();
    let alternatives = generator.subschema_for::<Vec<Suggestion>>();
    let mut root = serde_json::Map::new();
    if let Value::Object(variant) = error_variant_value(code, &alternatives) {
        root.extend(variant);
    }
    root.insert("title".into(), json!(format!("{} error", code.wire())));
    if let Some(meta_schema) = generator.settings().meta_schema.as_deref() {
        root.insert("$schema".into(), json!(meta_schema));
    }
    let definitions = generator.take_definitions(true);
    if !definitions.is_empty() {
        root.insert("$defs".into(), Value::Object(definitions));
    }
    Schema::from(root)
}

/// Each stderr event, with its discriminator pinned to the event's wire name.
pub fn event_schemas() -> BTreeMap<&'static str, Schema> {
    super::events::EventName::ALL
        .iter()
        .map(|name| {
            let mut schema = name.schema(&mut generator());
            if let Some(properties) = schema
                .as_object_mut()
                .and_then(|root| root.get_mut("properties"))
                .and_then(serde_json::Value::as_object_mut)
            {
                properties.insert(
                    "event".into(),
                    json!({"type": "string", "const": name.wire()}),
                );
            }
            (name.wire(), schema)
        })
        .collect()
}

/// Every saved format, plus one line of each journal family (whose version is in its
/// `prepared` line, not a leading `schema_version`) and `addons.toml` represented as JSON.
pub fn file_schemas() -> BTreeMap<&'static str, Schema> {
    let mut schemas: BTreeMap<_, _> = store::KINDS
        .iter()
        .map(|row| (row.kind, (row.schema)(&mut generator())))
        .collect();
    schemas.insert("journal", generator().root_schema_for::<JournalLine>());
    schemas.insert(
        "repair_journal",
        generator().root_schema_for::<crate::engine::repair::RepairLine>(),
    );
    let mut desired = generator().root_schema_for::<DesiredStateSchema>();
    desired.insert(
        "description".into(),
        json!("The desired addon sets in addons.toml, represented as JSON."),
    );
    schemas.insert("desired_state", desired);
    for (kind, schema) in &mut schemas {
        schema.insert("title".into(), json!(kind));
    }
    schemas
}

/// The published schemas as the binary carries them (omad-ho1p): the bytes of
/// [`Published::generated`], zstd-compressed. `robot-docs schema` prints them, so the
/// binary needs no schema generator. The contract suite regenerates them from the types
/// and fails when this file differs ([`EMBEDDED_PATH`], [`REGENERATE`]).
const EMBEDDED: &[u8] = include_bytes!("schemas.json.zst");

/// [`EMBEDDED`]'s path from the crate root.
pub const EMBEDDED_PATH: &str = "src/contract/schemas.json.zst";

/// The command that rewrites [`EMBEDDED_PATH`] from the types.
pub const REGENERATE: &str =
    "OMADDONS_UPDATE_SCHEMAS=1 cargo test --test integration contract::embedded_schemas";

/// The zstd level [`EMBEDDED_PATH`] is written at: about 70 KB for 1.5 MB of schemas.
pub const EMBEDDED_LEVEL: i32 = 19;

/// Every published schema, as `robot-docs schema` lists them: its `data`, keys in the
/// same order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Published {
    pub envelope: SchemaDoc,
    pub events: BTreeMap<String, SchemaDoc>,
    pub commands: BTreeMap<String, SchemaDoc>,
    pub files: BTreeMap<String, SchemaDoc>,
}

impl Published {
    /// The schemas this binary embedded. Its one failure is `internal`: the file is
    /// compiled in, and the contract suite reads it back on every gate.
    pub fn embedded() -> Result<Self> {
        let bytes = Self::embedded_bytes()?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::internal(format!("the embedded schemas do not read: {e}")))
    }

    /// [`EMBEDDED`] decompressed: the JSON [`Published::generated`] printed.
    pub fn embedded_bytes() -> Result<Vec<u8>> {
        zstd::stream::decode_all(EMBEDDED)
            .map_err(|e| Error::internal(format!("the embedded schemas do not decompress: {e}")))
    }

    /// Every schema generated from the types, as [`EMBEDDED`] must hold it.
    pub fn generated() -> Self {
        let keyed = |map: BTreeMap<&'static str, Schema>| {
            map.into_iter()
                .map(|(key, schema)| (key.to_owned(), SchemaDoc::from(schema)))
                .collect()
        };
        Self {
            envelope: SchemaDoc::from(envelope_schema()),
            events: keyed(event_schemas()),
            commands: keyed(command_schemas()),
            files: keyed(file_schemas()),
        }
    }
}

/// What [`walk`] reports.
#[derive(Debug, Clone, Copy)]
pub enum Visit<'a> {
    /// A schema position: the root, a property value, array items, a map's values, and
    /// each `$ref` target and in-place branch found there, at the same path.
    Position { path: &'a str, node: &'a Value },
    /// A property of an object node, at the property's own path.
    Property {
        path: &'a str,
        name: &'a str,
        node: &'a Value,
        required: bool,
        parent: &'a Value,
    },
}

/// Walks one published schema with every local `$ref` resolved in place.
///
/// Paths use the details syntax: `a`, `a.b`, `a[]` for an array's items (`prefixItems`
/// entries too), `a{}` for a map's values (`additionalProperties` or `patternProperties`
/// schemas); the root is the empty string. The branches of `allOf`, `anyOf` and `oneOf`
/// are walked at their node's path. A definition already being walked (the root from the
/// start) is not entered again, so every walk terminates. It does not validate, and
/// ignores the keywords it does not need.
pub fn walk(root: &Value, visit: &mut dyn FnMut(&Visit<'_>)) {
    let mut entered = vec!["#"];
    walk_at(root, root, "", &mut entered, visit);
}

fn walk_at<'a>(
    root: &'a Value,
    node: &'a Value,
    path: &str,
    entered: &mut Vec<&'a str>,
    visit: &mut dyn FnMut(&Visit<'_>),
) {
    visit(&Visit::Position { path, node });
    let Some(object) = node.as_object() else {
        return;
    };
    if let Some(reference) = object.get("$ref").and_then(Value::as_str)
        && !entered.contains(&reference)
        && let Some(target) = resolve(root, reference)
    {
        entered.push(reference);
        walk_at(root, target, path, entered, visit);
        entered.pop();
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        for branch in object
            .get(keyword)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk_at(root, branch, path, entered, visit);
        }
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        for (name, property) in properties {
            let child = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            visit(&Visit::Property {
                path: &child,
                name,
                node: property,
                required: required.contains(&name.as_str()),
                parent: node,
            });
            walk_at(root, property, &child, entered, visit);
        }
    }
    let values = format!("{path}{{}}");
    for value in object
        .get("patternProperties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(serde_json::Map::values)
    {
        walk_at(root, value, &values, entered, visit);
    }
    // A boolean opens or closes this node's own keys; only a schema is a position.
    if let Some(value) = object.get("additionalProperties")
        && value.is_object()
    {
        walk_at(root, value, &values, entered, visit);
    }
    let items = format!("{path}[]");
    for entry in object
        .get("prefixItems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk_at(root, entry, &items, entered, visit);
    }
    match object.get("items") {
        Some(Value::Array(entries)) => {
            for entry in entries {
                walk_at(root, entry, &items, entered, visit);
            }
        }
        Some(schema) => walk_at(root, schema, &items, entered, visit),
        None => {}
    }
}

/// A local reference's target: `#` or a JSON pointer into the root (`#/$defs/Name`).
pub(crate) fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        Some(root)
    } else {
        root.pointer(pointer)
    }
}

/// Keywords that annotate a schema without constraining what it admits.
const ANNOTATIONS: &[&str] = &[
    "$comment",
    "default",
    "deprecated",
    "description",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];

/// Whether a schema node admits any value, or any keys, on purpose: the boolean `true`,
/// a schema of annotations only (`{}`, or the `description` schemars gives a documented
/// `Value` field), or an object schema whose `additionalProperties` or
/// `unevaluatedProperties` is `true`. `contract::OPAQUE` names every such position.
pub fn is_open(node: &Value) -> bool {
    match node {
        Value::Bool(open) => *open,
        Value::Object(object) => {
            object
                .keys()
                .all(|keyword| ANNOTATIONS.contains(&keyword.as_str()))
                || ["additionalProperties", "unevaluatedProperties"]
                    .iter()
                    .any(|keyword| object.get(*keyword) == Some(&Value::Bool(true)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use schemars::JsonSchema;
    use serde::Serialize;
    use serde_json::Value;

    use super::*;

    /// Both helpers publish `format: date-time`, the optional one admitting `null`, and
    /// leave a field's presence to serde as `with = "String"` did (M4-D9): an
    /// `Option` that is always serialized stays required, one skipped when `None` does not.
    #[test]
    fn timestamps_publish_format_date_time() {
        #[derive(Serialize, JsonSchema)]
        struct Instants {
            #[schemars(schema_with = "crate::contract::schema::timestamp")]
            at: jiff::Timestamp,
            #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
            checked_at: Option<jiff::Timestamp>,
            #[serde(skip_serializing_if = "Option::is_none")]
            #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
            dismissed_at: Option<jiff::Timestamp>,
        }
        let schema = serde_json::to_value(generator().into_root_schema_for::<Instants>()).unwrap();
        let properties = &schema["properties"];
        assert_eq!(
            properties["at"],
            serde_json::json!({"type": "string", "format": "date-time"}),
            "{schema}"
        );
        assert_eq!(
            properties["checked_at"],
            serde_json::json!({"type": ["string", "null"], "format": "date-time"}),
            "{schema}"
        );
        assert_eq!(
            strings(&schema["required"]),
            BTreeSet::from(["at", "checked_at"]),
            "an always-serialized Option is required, a skipped one is not: {schema}"
        );
    }

    fn strings(value: &Value) -> BTreeSet<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    fn keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// `value` printed by schemars and by [`SchemaDoc`], compact and pretty.
    fn both_ways(value: &Value) -> [(String, String); 2] {
        let schema = Schema::try_from(value.clone()).unwrap();
        let doc = SchemaDoc::new(value.clone()).unwrap();
        [
            (
                serde_json::to_string(&schema).unwrap(),
                serde_json::to_string(&doc).unwrap(),
            ),
            (
                serde_json::to_string_pretty(&schema).unwrap(),
                serde_json::to_string_pretty(&doc).unwrap(),
            ),
        ]
    }

    /// [`SchemaDoc`] prints every keyword position as schemars 1.x does (omad-ho1p): the
    /// binary prints embedded schemas without schemars, byte for byte as generated.
    #[test]
    fn schema_doc_prints_as_schemars_does() {
        let cases: Vec<(&str, Value)> = vec![
            ("boolean", json!(true)),
            (
                "first_middle_last",
                json!({"$defs": {"b": {"type": "string", "title": "B"}}, "required": ["x"],
                       "additionalProperties": false, "properties": {"x": {"type": "integer"}},
                       "type": "object", "description": "d", "title": "t", "$schema": "s",
                       "$id": "i", "format": "f", "definitions": {"z": {"description": "z"}}}),
            ),
            (
                "named_maps_keep_their_keys",
                json!({"patternProperties": {"^b": {"type": "string", "description": "p"}},
                       "dependentSchemas": {"type": {"required": ["title"]}},
                       "properties": {"type": {"title": "named type", "type": "string"}}}),
            ),
            (
                "plain_values",
                json!({"examples": [{"type": 1, "title": 2}], "default": {"z": 1, "a": 2},
                       "x-extra": {"type": "t", "description": "d"}, "type": "object"}),
            ),
            (
                "subschemas_in_arrays_and_keywords",
                json!({"oneOf": [{"type": "null", "description": "n"}, {"properties": {}, "type": "object"}],
                       "items": {"format": "date-time", "type": "string"},
                       "additionalProperties": {"type": "integer", "description": "v"},
                       "const": {"type": "c", "description": "quirk: reordered like a schema"}}),
            ),
        ];
        for (case, value) in cases {
            for (schemars, ours) in both_ways(&value) {
                assert_eq!(ours, schemars, "case {case}");
            }
        }
    }

    /// Every schema omaddons publishes prints the same through [`SchemaDoc`]: the
    /// listing, the error union and each variant, and every code's `details` fragment.
    #[test]
    fn every_published_schema_prints_as_schemars_does() {
        let mut values: Vec<(String, Value)> = vec![
            ("envelope".into(), envelope_schema().to_value()),
            ("error union".into(), error_schema().to_value()),
        ];
        values.extend(
            command_schemas()
                .into_iter()
                .map(|(name, s)| (format!("command {name}"), s.to_value())),
        );
        values.extend(
            event_schemas()
                .into_iter()
                .map(|(name, s)| (format!("event {name}"), s.to_value())),
        );
        values.extend(
            file_schemas()
                .into_iter()
                .map(|(kind, s)| (format!("file {kind}"), s.to_value())),
        );
        for code in ErrorCode::ALL {
            values.push((format!("{code} error"), error_variant(*code).to_value()));
            values.push((format!("{code} details"), code.details_schema()));
        }
        for (label, value) in &values {
            for (schemars, ours) in both_ways(value) {
                assert!(ours == schemars, "{label}: SchemaDoc prints it differently");
            }
        }
    }

    #[test]
    fn schema_doc_is_an_object_or_a_boolean() {
        for (case, value, accepted) in [
            ("object", json!({"type": "string"}), true),
            ("boolean", json!(false), true),
            ("string", json!("schema"), false),
            ("array", json!([]), false),
            ("null", Value::Null, false),
        ] {
            assert_eq!(SchemaDoc::new(value).is_some(), accepted, "case {case}");
        }
        for code in ErrorCode::ALL {
            assert!(SchemaDoc::details(*code).is_ok(), "{code}");
        }
    }

    #[test]
    fn published_settings_use_the_serialize_contract() {
        #[derive(Serialize, JsonSchema)]
        struct Fields {
            #[serde(skip_serializing_if = "Option::is_none")]
            a: Option<u8>,
            b: Option<u8>,
            #[serde(default)]
            c: u8,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            d: Vec<u8>,
        }
        let schema = generator().root_schema_for::<Fields>().to_value();
        let required = strings(&schema["required"]);
        for (case, field, expected) in [
            ("skipped_option_is_optional", "a", false),
            ("plain_option_is_required_and_nullable", "b", true),
            ("default_without_skip_is_required", "c", true),
            ("skipped_vec_is_optional", "d", false),
        ] {
            assert_eq!(required.contains(field), expected, "{case}: {schema}");
        }
        assert!(
            strings(&schema["properties"]["b"]["type"]).contains("null"),
            "plain_option_is_required_and_nullable: {schema}"
        );
    }

    #[test]
    fn every_command_row_has_a_schema() {
        let schemas = command_schemas();
        assert_eq!(schemas.len(), app::COMMANDS.len());
        for name in app::COMMANDS
            .iter()
            .map(|row| row.name)
            .chain(app::ENVELOPE_ONLY.iter().map(|row| row.name))
        {
            let schema = data_schema_for(name).unwrap().to_value();
            assert!(schema.is_object(), "{name}: {schema}");
            assert_eq!(
                schema["$schema"], "https://json-schema.org/draft/2020-12/schema",
                "{name}: {schema}"
            );
            assert_eq!(schema["title"], name, "{name}: {schema}");
        }
        assert!(data_schema_for("no such command").is_none());
    }

    #[test]
    fn schemas_are_deterministic() {
        for (case, first, second) in [
            (
                "commands",
                serde_json::to_vec(&command_schemas()).unwrap(),
                serde_json::to_vec(&command_schemas()).unwrap(),
            ),
            (
                "envelope",
                serde_json::to_vec(&envelope_schema()).unwrap(),
                serde_json::to_vec(&envelope_schema()).unwrap(),
            ),
            (
                "events",
                serde_json::to_vec(&event_schemas()).unwrap(),
                serde_json::to_vec(&event_schemas()).unwrap(),
            ),
            (
                "files",
                serde_json::to_vec(&file_schemas()).unwrap(),
                serde_json::to_vec(&file_schemas()).unwrap(),
            ),
        ] {
            assert_eq!(first, second, "{case}: repeated schema generation");
        }
    }

    #[test]
    fn defs_names_depend_only_on_the_command() {
        let install = app::command_entry("install").unwrap();
        let first = command_schema(install);
        let _ = (command_schemas(), event_schemas(), file_schemas());
        assert_eq!(
            first,
            command_schema(install),
            "install schema after all other roots"
        );
    }

    #[test]
    fn envelope_schema_matches_the_envelope() {
        let schema = envelope_schema().to_value();
        assert_eq!(schema["title"], "Envelope", "{schema}");
        assert_eq!(
            keys(&schema["properties"]),
            BTreeSet::from([
                "ok",
                "schema_version",
                "tool_version",
                "command",
                "exit_code",
                "partial",
                "data",
                "error",
                "warnings",
                "commands",
                "meta"
            ]),
            "{schema}"
        );
        assert_eq!(
            strings(&schema["required"]),
            BTreeSet::from([
                "ok",
                "schema_version",
                "tool_version",
                "command",
                "exit_code",
                "warnings",
                "commands",
                "meta"
            ]),
            "{schema}"
        );
        // A description annotates the otherwise unconstrained Value schema.
        assert_eq!(
            keys(&schema["properties"]["data"]),
            BTreeSet::from(["description"]),
            "data is open: {schema}"
        );
        let meta = schema["properties"]["meta"]["$ref"].as_str().unwrap();
        assert_eq!(
            keys(&schema.pointer(&meta[1..]).unwrap()["properties"]),
            BTreeSet::from(["elapsed_ms", "offline"]),
            "{schema}"
        );
    }

    #[test]
    fn the_envelope_error_is_the_union() {
        let schema = envelope_schema().to_value();
        let union = &schema["$defs"]["ErrorBody"]["oneOf"];
        assert_eq!(
            union.as_array().map(Vec::len),
            Some(ErrorCode::ALL.len()),
            "$defs.ErrorBody: {schema:#}"
        );
        let mut reached = false;
        walk(&schema, &mut |visit| {
            if let Visit::Position {
                path: "error",
                node,
            } = visit
            {
                reached |= node.get("oneOf") == Some(union);
            }
        });
        assert!(reached, "error reaches the union: {schema:#}");
    }

    #[test]
    fn error_schemas_are_rooted_and_titled() {
        let union = error_schema().to_value();
        assert_eq!(union["title"], "ErrorBody", "{union:#}");
        assert_eq!(
            union["$schema"], "https://json-schema.org/draft/2020-12/schema",
            "{union:#}"
        );
        for code in ErrorCode::ALL {
            let variant = error_variant(*code).to_value();
            assert_eq!(
                variant["title"],
                format!("{} error", code.wire()),
                "{variant:#}"
            );
            assert_eq!(
                variant["$schema"], "https://json-schema.org/draft/2020-12/schema",
                "{code}: {variant:#}"
            );
            assert_eq!(
                variant["properties"]["code"]["const"],
                code.wire(),
                "{code}: {variant:#}"
            );
            assert_eq!(
                variant["properties"]["alternatives"],
                json!({"type": "array", "items": {"$ref": "#/$defs/Suggestion"}}),
                "{code}: {variant:#}"
            );
            assert!(
                variant
                    .pointer("/$defs/Suggestion/properties/argv")
                    .is_some(),
                "{code}: $defs holds Suggestion: {variant:#}"
            );
            let in_union = &union["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["properties"]["code"]["const"] == code.wire())
                .unwrap()["properties"];
            assert_eq!(
                &variant["properties"], in_union,
                "{code}: the root variant is the union's"
            );
        }
    }

    /// The visits of one walk, as sorted `position <path>` and `property <path>` lines
    /// (`(required)` on a required property's), the root shown as `.`.
    fn visits(schema: &Value) -> Vec<String> {
        let mut out = Vec::new();
        walk(schema, &mut |visit| {
            let shown = |path: &str| {
                if path.is_empty() {
                    ".".to_owned()
                } else {
                    path.to_owned()
                }
            };
            out.push(match visit {
                Visit::Position { path, .. } => format!("position {}", shown(path)),
                Visit::Property {
                    path,
                    name,
                    required,
                    parent,
                    ..
                } => {
                    assert!(
                        parent["properties"].get(*name).is_some(),
                        "{name}: its parent holds it: {parent}"
                    );
                    format!(
                        "property {}{}",
                        shown(path),
                        if *required { " (required)" } else { "" }
                    )
                }
            });
        });
        out.sort();
        out
    }

    #[test]
    fn walk_visits_every_position() {
        let inner =
            json!({"type": "object", "properties": {"x": {"type": "integer"}}, "required": ["x"]});
        let cases: Vec<(&str, Value, Vec<&str>)> = vec![
            (
                "ref_used_twice",
                json!({
                    "properties": {"a": {"$ref": "#/$defs/I"}, "b": {"$ref": "#/$defs/I"}},
                    "$defs": {"I": inner},
                }),
                vec![
                    "position .",
                    "position a",
                    "position a",
                    "position a.x",
                    "position b",
                    "position b",
                    "position b.x",
                    "property a",
                    "property a.x (required)",
                    "property b",
                    "property b.x (required)",
                ],
            ),
            (
                "self_recursive_definition",
                json!({
                    "type": "object",
                    "properties": {
                        "v": {"type": "integer"},
                        "children": {"type": "array", "items": {"$ref": "#"}},
                    },
                    "required": ["v"],
                }),
                vec![
                    "position .",
                    "position children",
                    "position children[]",
                    "position v",
                    "property children",
                    "property v (required)",
                ],
            ),
            (
                "recursive_definition_in_defs",
                json!({
                    "properties": {"n": {"$ref": "#/$defs/Node"}},
                    "$defs": {"Node": {"properties": {"next": {"anyOf": [{"$ref": "#/$defs/Node"}, {"type": "null"}]}}}},
                }),
                vec![
                    "position .",
                    "position n",
                    "position n",
                    "position n.next",
                    "position n.next",
                    "position n.next",
                    "property n",
                    "property n.next",
                ],
            ),
            (
                "any_of_with_null",
                json!({
                    "properties": {"n": {"anyOf": [{"$ref": "#/$defs/I"}, {"type": "null"}]}},
                    "$defs": {"I": inner},
                }),
                vec![
                    "position .",
                    "position n",
                    "position n",
                    "position n",
                    "position n",
                    "position n.x",
                    "property n",
                    "property n.x (required)",
                ],
            ),
            (
                "one_of_sharing_a_field",
                json!({"oneOf": [
                    {"properties": {"a": {"type": "string"}, "b": {}}, "required": ["a"]},
                    {"properties": {"a": {"type": "integer"}}},
                ]}),
                vec![
                    "position .",
                    "position .",
                    "position .",
                    "position a",
                    "position a",
                    "position b",
                    "property a",
                    "property a (required)",
                    "property b",
                ],
            ),
            (
                "map_additional_properties",
                json!({
                    "properties": {"m": {"type": "object", "additionalProperties": {"$ref": "#/$defs/I"}}},
                    "$defs": {"I": inner},
                }),
                vec![
                    "position .",
                    "position m",
                    "position m{}",
                    "position m{}",
                    "position m{}.x",
                    "property m",
                    "property m{}.x (required)",
                ],
            ),
            (
                "map_pattern_properties",
                json!({"properties": {"m": {
                    "type": "object",
                    "patternProperties": {"^[0-9]+$": {"type": "integer"}},
                    "additionalProperties": false,
                }}}),
                vec!["position .", "position m", "position m{}", "property m"],
            ),
            (
                "open_map_is_its_own_position",
                json!({"properties": {"m": {"type": "object", "additionalProperties": true}}}),
                vec!["position .", "position m", "property m"],
            ),
            (
                "prefix_items",
                json!({"properties": {"t": {
                    "type": "array",
                    "prefixItems": [{"type": "integer"}, {"$ref": "#/$defs/I"}],
                }}, "$defs": {"I": inner}}),
                vec![
                    "position .",
                    "position t",
                    "position t[]",
                    "position t[]",
                    "position t[]",
                    "position t[].x",
                    "property t",
                    "property t[].x (required)",
                ],
            ),
            (
                "nested_paths",
                json!({"properties": {"a": {"type": "array", "items": {"properties": {"b": {
                    "type": "object", "additionalProperties": {"type": "array", "items": true},
                }}}}}}),
                vec![
                    "position .",
                    "position a",
                    "position a[]",
                    "position a[].b",
                    "position a[].b{}",
                    "position a[].b{}[]",
                    "property a",
                    "property a[].b",
                ],
            ),
            (
                "non_local_reference_is_not_followed",
                json!({"properties": {"r": {"$ref": "https://example.com/schema"}}}),
                vec!["position .", "position r", "property r"],
            ),
        ];
        for (name, schema, expected) in cases {
            assert_eq!(visits(&schema), expected, "case {name}: walking {schema:#}");
        }
    }

    #[test]
    fn open_nodes() {
        for (name, node, expected) in [
            ("true", json!(true), true),
            ("false", json!(false), false),
            ("empty", json!({}), true),
            (
                "annotations_only",
                json!({"description": "d", "title": "t"}),
                true,
            ),
            ("typed", json!({"type": "object"}), false),
            (
                "open_keys",
                json!({"type": "object", "additionalProperties": true}),
                true,
            ),
            (
                "open_unevaluated",
                json!({"unevaluatedProperties": true}),
                true,
            ),
            (
                "closed_keys",
                json!({"type": "object", "additionalProperties": false}),
                false,
            ),
            (
                "map_of_values",
                json!({"type": "object", "additionalProperties": {"type": "string"}}),
                false,
            ),
            (
                "reference",
                json!({"$ref": "#/$defs/X", "description": "d"}),
                false,
            ),
        ] {
            assert_eq!(is_open(&node), expected, "case {name}: {node}");
        }
    }

    #[test]
    fn event_schemas_cover_every_event_and_pin_their_name() {
        let schemas = event_schemas();
        assert_eq!(
            schemas.keys().copied().collect::<BTreeSet<_>>(),
            super::super::events::EventName::ALL
                .iter()
                .map(|name| name.wire())
                .collect()
        );
        for (name, schema) in schemas {
            assert_eq!(
                schema.to_value()["properties"]["event"],
                json!({"type": "string", "const": name}),
                "{name}: event discriminator"
            );
        }
    }

    #[test]
    fn file_schemas_cover_every_kind() {
        let schemas = file_schemas();
        let expected: BTreeSet<_> = store::KINDS
            .iter()
            .map(|row| row.kind)
            .chain(["journal", "repair_journal", "desired_state"])
            .collect();
        assert_eq!(schemas.keys().copied().collect::<BTreeSet<_>>(), expected);
        for (name, schema) in &schemas {
            assert!(
                name.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'),
                "{name}: kind spelling"
            );
            let schema = schema.clone().to_value();
            assert_eq!(schema["title"], *name, "{name}: {schema}");
            if store::KINDS.iter().any(|row| row.kind == *name) {
                assert!(
                    schema["properties"].get("schema_version").is_some(),
                    "{name}: {schema}"
                );
                assert!(
                    strings(&schema["required"]).contains("schema_version"),
                    "{name}: {schema}"
                );
            }
        }
        assert!(
            schemas["desired_state"]
                .get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("addons.toml")
        );
    }
}
