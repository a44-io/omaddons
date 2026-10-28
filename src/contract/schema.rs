//! The JSON Schemas of command data, the envelope, stderr events and saved files.
//! M4-D9 uses the serialize contract, so omitted fields are optional. Each root gets
//! a fresh generator, so its definitions depend only on its own type. Published
//! schemas stay open; strictness belongs to the contract suite's test transform.

use std::collections::BTreeMap;

use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator};
use serde_json::json;

use crate::app::{self, CommandEntry};
use crate::engine::journal::JournalLine;
use crate::output::Envelope;
use crate::state::{desired::DesiredStateSchema, store};

/// The settings every published schema is generated with (M4-D9).
pub fn settings() -> SchemaSettings {
    SchemaSettings::draft2020_12().for_serialize()
}

/// One fresh generator per root; never reuse it for another schema.
pub fn generator() -> SchemaGenerator {
    settings().into_generator()
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

/// Every saved format, plus one journal line and `addons.toml` represented as JSON.
pub fn file_schemas() -> BTreeMap<&'static str, Schema> {
    let mut schemas: BTreeMap<_, _> = store::KINDS
        .iter()
        .map(|row| (row.kind, (row.schema)(&mut generator())))
        .collect();
    schemas.insert("journal", generator().root_schema_for::<JournalLine>());
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use schemars::JsonSchema;
    use serde::Serialize;
    use serde_json::Value;

    use super::*;

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
            .chain(["journal", "desired_state"])
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
