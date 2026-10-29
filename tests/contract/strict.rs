//! The strict transform and the validator, proved before the contract suite relies on
//! them (M4-D9): every composition schemars emits for omaddons' types, built here from
//! derived test types or written by hand, with instances that must pass and instances
//! that must fail at a named instance path. Pure: no command runs.

use std::collections::{BTreeMap, BTreeSet};

use omaddons::contract::schema::generator;
use omaddons::model::TargetKey;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::validate::{Failure, MAX_FAILURES, StrictError, Validator, strict};

#[derive(Serialize, JsonSchema)]
struct Inner {
    a: u8,
}

/// The published form of `T`'s schema, as `contract::schema` generates every root.
fn published<T: JsonSchema>() -> Value {
    generator().root_schema_for::<T>().to_value()
}

/// One composition case: a published schema, instances that pass its strict form, and
/// instances that fail it, each at the instance path named beside it ("" is the root).
/// `closed_by_strict` marks an invalid instance the published schema itself accepts: an
/// unknown field only the strict form rejects.
struct Case {
    name: &'static str,
    schema: Value,
    valid: Vec<Value>,
    invalid: Vec<Invalid>,
}

struct Invalid {
    instance: Value,
    at: &'static str,
    closed_by_strict: bool,
}

fn unknown(instance: Value, at: &'static str) -> Invalid {
    Invalid {
        instance,
        at,
        closed_by_strict: true,
    }
}

fn wrong(instance: Value, at: &'static str) -> Invalid {
    Invalid {
        instance,
        at,
        closed_by_strict: false,
    }
}

fn shown(failures: &[Failure]) -> String {
    if failures.is_empty() {
        return "passed".to_owned();
    }
    failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn cases() -> Vec<Case> {
    #[derive(Serialize, JsonSchema)]
    struct M {
        m: BTreeMap<String, Inner>,
    }
    #[derive(Serialize, JsonSchema)]
    struct P {
        m: BTreeMap<TargetKey, Inner>,
    }
    #[derive(Serialize, JsonSchema)]
    #[serde(tag = "type", rename_all = "snake_case")]
    #[expect(
        dead_code,
        reason = "schema only: its variants shape the schema, never a value"
    )]
    enum E {
        A { x: u8 },
        B { y: String },
        C,
    }
    #[derive(Serialize, JsonSchema)]
    struct Tagged {
        e: E,
    }
    #[derive(Serialize, JsonSchema)]
    struct F {
        f: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        g: Option<u8>,
    }
    #[derive(Serialize, JsonSchema)]
    struct O {
        o: u8,
        #[serde(flatten)]
        f: F,
    }
    #[derive(Serialize, JsonSchema)]
    struct N {
        n: Option<Inner>,
    }
    #[derive(Serialize, JsonSchema)]
    struct Node {
        v: u8,
        children: Vec<Node>,
    }
    #[derive(Serialize, JsonSchema)]
    struct V {
        v: Vec<Inner>,
        t: (Inner, u8),
    }
    let key = "00000000000000000000000000";
    let inner =
        json!({"type": "object", "properties": {"a": {"type": "integer"}}, "required": ["a"]});
    let result = |field: &str| json!({"type": "object", "properties": {field: {"type": "integer"}}, "required": [field]});
    let applied = |command: &str| json!({"type": "object", "properties": {"command": {"type": "string", "const": command}}, "required": ["command"]});
    vec![
        Case {
            name: "ref_as_property_and_in_all_of",
            schema: json!({
                "type": "object",
                "properties": {
                    "p": {"$ref": "#/$defs/Inner"},
                    "q": {"allOf": [{"$ref": "#/$defs/Inner"}]},
                },
                "$defs": {"Inner": inner},
            }),
            valid: vec![json!({"p": {"a": 1}, "q": {"a": 2}})],
            invalid: vec![
                unknown(json!({"p": {"a": 1, "x": 1}, "q": {"a": 2}}), "/p"),
                unknown(json!({"p": {"a": 1}, "q": {"a": 2, "x": 1}}), "/q"),
                unknown(json!({"p": {"a": 1}, "q": {"a": 2}, "x": 1}), ""),
            ],
        },
        Case {
            name: "map_through_additional_properties",
            schema: published::<M>(),
            valid: vec![
                json!({"m": {}}),
                json!({"m": {"k1": {"a": 1}, "k2": {"a": 2}}}),
            ],
            invalid: vec![
                unknown(json!({"m": {"k1": {"a": 1, "x": 1}}}), "/m/k1"),
                unknown(json!({"m": {}, "x": 1}), ""),
            ],
        },
        Case {
            name: "map_through_pattern_properties",
            schema: published::<P>(),
            valid: vec![json!({"m": {key: {"a": 1}}})],
            invalid: vec![
                unknown(
                    json!({"m": {key: {"a": 1, "x": 1}}}),
                    "/m/00000000000000000000000000",
                ),
                wrong(json!({"m": {"nope": {"a": 1}}}), "/m"),
            ],
        },
        Case {
            name: "internally_tagged_enum",
            schema: published::<Tagged>(),
            valid: vec![
                json!({"e": {"type": "a", "x": 1}}),
                json!({"e": {"type": "b", "y": "s"}}),
                json!({"e": {"type": "c"}}),
            ],
            invalid: vec![
                wrong(json!({"e": {"type": "a", "y": "s"}}), "/e"),
                unknown(json!({"e": {"type": "a", "x": 1, "z": 1}}), "/e"),
            ],
        },
        Case {
            name: "flattened_struct",
            schema: published::<O>(),
            valid: vec![json!({"o": 1, "f": 2}), json!({"o": 1, "f": 2, "g": 3})],
            invalid: vec![unknown(json!({"o": 1, "f": 2, "h": 4}), "")],
        },
        Case {
            name: "one_of_with_shared_fields",
            schema: json!({
                "type": "object",
                "properties": {"plan_id": {"type": "string"}},
                "required": ["plan_id"],
                "oneOf": [
                    {
                        "properties": {"applied": applied("install"), "result": {"$ref": "#/$defs/InstallResult"}},
                        "required": ["applied", "result"],
                    },
                    {
                        "properties": {"applied": applied("update"), "result": {"$ref": "#/$defs/UpdateResult"}},
                        "required": ["applied", "result"],
                    },
                ],
                "$defs": {"InstallResult": result("installed"), "UpdateResult": result("updated")},
            }),
            valid: vec![
                json!({"plan_id": "p", "applied": {"command": "install"}, "result": {"installed": 1}}),
                json!({"plan_id": "p", "applied": {"command": "update"}, "result": {"updated": 1}}),
            ],
            invalid: vec![
                wrong(
                    json!({"plan_id": "p", "applied": {"command": "install"}, "result": {"updated": 1}}),
                    "",
                ),
                // Closing `result` makes the install branch fail, so no branch matches:
                // the validator reports the `oneOf`, at the root, not the field inside it.
                unknown(
                    json!({"plan_id": "p", "applied": {"command": "install"}, "result": {"installed": 1, "x": 1}}),
                    "",
                ),
                unknown(
                    json!({"plan_id": "p", "applied": {"command": "install"}, "result": {"installed": 1}, "x": 1}),
                    "",
                ),
            ],
        },
        Case {
            name: "option_of_a_ref",
            schema: published::<N>(),
            valid: vec![json!({"n": null}), json!({"n": {"a": 1}})],
            invalid: vec![unknown(json!({"n": {"a": 1, "x": 1}}), "/n")],
        },
        Case {
            name: "recursive_ref",
            schema: published::<Node>(),
            valid: vec![
                json!({"v": 1, "children": [{"v": 2, "children": [{"v": 3, "children": []}]}]}),
            ],
            invalid: vec![unknown(
                json!({"v": 1, "children": [{"v": 2, "children": [{"v": 3, "children": [], "x": 1}]}]}),
                "/children/0/children/0",
            )],
        },
        Case {
            name: "items_and_prefix_items",
            schema: published::<V>(),
            valid: vec![json!({"v": [{"a": 1}], "t": [{"a": 1}, 2]})],
            invalid: vec![
                unknown(json!({"v": [{"a": 1, "x": 1}], "t": [{"a": 1}, 2]}), "/v/0"),
                unknown(json!({"v": [{"a": 1}], "t": [{"a": 1, "x": 1}, 2]}), "/t/0"),
            ],
        },
    ]
}

/// The distinct instance paths of `failures`, sorted.
fn paths(failures: &[Failure]) -> BTreeSet<&str> {
    failures.iter().map(|f| f.instance_path.as_str()).collect()
}

#[test]
fn every_composition_case_closes_exactly_what_it_should() {
    for case in cases() {
        let strict_schema = strict(&case.schema);
        assert!(
            strict_schema.is_ok(),
            "case {}: {:?}\n{:#}",
            case.name,
            strict_schema,
            case.schema
        );
        let strict_schema = strict_schema.unwrap();
        let context = || {
            format!(
                "case {}\npublished: {:#}\nstrict: {strict_schema:#}",
                case.name, case.schema
            )
        };
        let (closed, open) = (
            Validator::new(&case.schema, true),
            Validator::new(&case.schema, false),
        );
        assert!(
            closed.is_ok() && open.is_ok(),
            "{:?} {:?}\n{}",
            closed.as_ref().err(),
            open.as_ref().err(),
            context()
        );
        let (closed, open) = (closed.unwrap(), open.unwrap());
        for instance in &case.valid {
            let failures = closed.validate(instance);
            assert!(
                failures.is_empty(),
                "a valid instance failed\ninstance: {instance:#}\nfailures:\n{}\n{}",
                shown(&failures),
                context()
            );
        }
        for invalid in &case.invalid {
            let failures = closed.validate(&invalid.instance);
            assert_eq!(
                paths(&failures),
                BTreeSet::from([invalid.at]),
                "an invalid instance did not fail at exactly {:?}\ninstance: {:#}\nfailures:\n{}\n{}",
                invalid.at,
                invalid.instance,
                shown(&failures),
                context()
            );
            let published_failures = open.validate(&invalid.instance);
            assert_eq!(
                published_failures.is_empty(),
                invalid.closed_by_strict,
                "the published schema {} the instance\ninstance: {:#}\nfailures:\n{}\n{}",
                if invalid.closed_by_strict {
                    "must accept"
                } else {
                    "must refuse"
                },
                invalid.instance,
                shown(&published_failures),
                context()
            );
        }
    }
}

#[test]
fn open_positions_stay_open() {
    #[derive(Serialize, JsonSchema)]
    struct Open {
        any: Value,
        /// A documented value: schemars gives it a `description`, never `{}`.
        documented: Value,
        map: BTreeMap<String, Value>,
        closed: Inner,
    }
    let mut schema = published::<Open>();
    schema["properties"]["empty"] = json!({});
    schema["properties"]["opaque"] = json!({
        "type": "object",
        "description": "Kept as written.",
        "additionalProperties": true,
    });
    let validator = Validator::new(&schema, true).unwrap();
    let anything = json!({"nested": {"deep": [1, null, {"k": true}]}, "x": "y"});
    let instance = json!({
        "any": anything, "documented": anything, "map": anything, "empty": anything,
        "opaque": anything, "closed": {"a": 1},
    });
    let failures = validator.validate(&instance);
    assert!(
        failures.is_empty(),
        "open fields admit anything\nfailures:\n{}\nstrict: {:#}",
        shown(&failures),
        strict(&schema).unwrap()
    );
    let mut sibling = instance.clone();
    sibling["closed"]["x"] = json!(1);
    let failures = validator.validate(&sibling);
    assert_eq!(
        paths(&failures),
        BTreeSet::from(["/closed"]),
        "the closed sibling stays closed\nfailures:\n{}",
        shown(&failures)
    );
}

/// Every node of `schema` with the pointer it sits at, and whether it is the root of an
/// in-place branch or a `$defs` entry.
fn nodes(schema: &Value) -> Vec<(String, &Value, bool)> {
    fn visit<'a>(
        node: &'a Value,
        path: String,
        in_place: bool,
        out: &mut Vec<(String, &'a Value, bool)>,
    ) {
        out.push((path.clone(), node, in_place));
        let Some(object) = node.as_object() else {
            return;
        };
        for (keyword, value) in object {
            let here = format!("{path}/{keyword}");
            match (keyword.as_str(), value) {
                ("properties" | "patternProperties", Value::Object(entries)) => {
                    for (name, entry) in entries {
                        visit(entry, format!("{here}/{name}"), false, out);
                    }
                }
                ("$defs", Value::Object(entries)) => {
                    for (name, entry) in entries {
                        visit(entry, format!("{here}/{name}"), true, out);
                    }
                }
                ("allOf" | "anyOf" | "oneOf" | "prefixItems", Value::Array(entries)) => {
                    for (index, entry) in entries.iter().enumerate() {
                        visit(
                            entry,
                            format!("{here}/{index}"),
                            keyword != "prefixItems",
                            out,
                        );
                    }
                }
                ("items" | "additionalProperties", entry) => visit(entry, here, false, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    visit(schema, String::new(), false, &mut out);
    out
}

#[test]
fn defs_and_branch_roots_are_untouched() {
    for case in cases() {
        let strict_schema = strict(&case.schema).unwrap();
        for (path, node, in_place) in nodes(&strict_schema) {
            let Some(object) = node.as_object() else {
                continue;
            };
            let closed = object.get("unevaluatedProperties") == Some(&json!(false));
            if in_place {
                assert!(
                    object.get("unevaluatedProperties").is_none(),
                    "case {}: an in-place root at {path} was closed\n{strict_schema:#}",
                    case.name
                );
            } else {
                assert!(
                    closed || omaddons::contract::schema::is_open(node),
                    "case {}: the instance position {path} was left open\n{strict_schema:#}",
                    case.name
                );
            }
        }
    }
}

#[test]
fn unmodelled_keywords_are_refused() {
    let cases = [
        (
            "not",
            json!({"properties": {"p": {"not": {}}}}),
            "/properties/p/not",
        ),
        ("if", json!({"if": {}}), "/if"),
        ("then", json!({"then": {}}), "/then"),
        ("else", json!({"else": {}}), "/else"),
        (
            "contains",
            json!({"items": {"contains": {}}}),
            "/items/contains",
        ),
        (
            "dependentSchemas",
            json!({"dependentSchemas": {}}),
            "/dependentSchemas",
        ),
        ("dependencies", json!({"dependencies": {}}), "/dependencies"),
        (
            "propertyNames",
            json!({"propertyNames": {}}),
            "/propertyNames",
        ),
        (
            "unevaluatedItems",
            json!({"unevaluatedItems": false}),
            "/unevaluatedItems",
        ),
        (
            "additionalItems",
            json!({"additionalItems": false}),
            "/additionalItems",
        ),
        (
            "$defs",
            json!({"properties": {"p": {"$defs": {}}}}),
            "/properties/p/$defs",
        ),
        (
            "x-unknown",
            json!({"oneOf": [{"x-unknown": 1}, {}]}),
            "/oneOf/0/x-unknown",
        ),
        ("not", json!({"$defs": {"D": {"not": {}}}}), "/$defs/D/not"),
    ];
    for (keyword, schema, path) in cases {
        let expected = StrictError {
            keyword: keyword.to_owned(),
            path: path.to_owned(),
        };
        assert_eq!(
            strict(&schema),
            Err(expected.clone()),
            "case {keyword} at {path}: {schema}"
        );
        assert_eq!(
            expected.to_string(),
            format!("strict: unsupported `{keyword}` at {path}"),
            "case {keyword}: display"
        );
    }
}

#[test]
fn strict_is_idempotent() {
    for case in cases() {
        let once = strict(&case.schema).unwrap();
        assert_eq!(strict(&once).unwrap(), once, "case {}: {once:#}", case.name);
    }
}

#[test]
fn strict_does_not_mutate_its_input() {
    for case in cases() {
        let before = case.schema.clone();
        let after = strict(&case.schema).unwrap();
        assert_eq!(case.schema, before, "case {}: the input changed", case.name);
        assert_ne!(after, before, "case {}: strict added nothing", case.name);
    }
}

#[test]
fn failures_name_instance_and_schema_paths() {
    let schema =
        json!({"type": "object", "properties": {"p": {"type": "object", "properties": {"a": {}}}}});
    let validator = Validator::new(&schema, true).unwrap();
    let failures = validator.validate(&json!({"p": {"a": 1, "x": 1}}));
    assert_eq!(failures.len(), 1, "{}", shown(&failures));
    let failure = &failures[0];
    assert_eq!(failure.instance_path, "/p", "{failure}");
    assert!(
        failure.schema_path.ends_with("unevaluatedProperties"),
        "{failure}"
    );
    let text = failure.to_string();
    assert!(
        text.starts_with("/p: ") && text.contains(&format!("(schema {})", failure.schema_path)),
        "{text}"
    );
    let root = validator.validate(&json!({"p": {}, "x": 1}));
    assert!(root[0].to_string().starts_with("/: "), "{}", shown(&root));
}

#[test]
fn failures_are_capped() {
    // The validator reports a set of unevaluated keys on one object as one failure, so
    // the 30 unknown keys sit on 30 objects, one each.
    let schema = json!({"type": "array", "items": {"type": "object", "properties": {"a": {}}}});
    let validator = Validator::new(&schema, true).unwrap();
    let instance = Value::Array((0..30).map(|i| json!({"a": i, "x": i})).collect());
    let failures = validator.validate(&instance);
    assert_eq!(failures.len(), MAX_FAILURES + 1, "{}", shown(&failures));
    assert_eq!(
        failures.last().map(|f| f.message.as_str()),
        Some("… and 10 more"),
        "{}",
        shown(&failures)
    );
}

#[test]
fn a_schema_that_does_not_compile_is_an_error() {
    let refused = Validator::new(&json!({"properties": {"p": {"not": {}}}}), true);
    assert_eq!(
        refused.err().as_deref(),
        Some("strict: unsupported `not` at /properties/p/not")
    );
    let broken = Validator::new(&json!({"type": 7}), false);
    assert!(
        broken
            .as_ref()
            .err()
            .is_some_and(|e| e.starts_with("the schema does not compile")),
        "{:?}",
        broken.err()
    );
}

#[test]
fn the_validator_reads_draft_2020_12_without_formats_or_remote_references() {
    let cases = [
        // `prefixItems` exists only from draft 2020-12; an older draft would ignore it.
        (
            "draft_2020_12",
            json!({"prefixItems": [{"type": "integer"}]}),
            json!(["x"]),
            false,
        ),
        // schemars' integer formats are annotations: an unknown one asserts nothing.
        (
            "unknown_format_is_an_annotation",
            json!({"type": "integer", "format": "uint8"}),
            json!(300),
            true,
        ),
        (
            "known_format_is_an_annotation",
            json!({"type": "string", "format": "email"}),
            json!("not an address"),
            true,
        ),
    ];
    for (name, schema, instance, passes) in cases {
        let failures = Validator::new(&schema, false).unwrap().validate(&instance);
        assert_eq!(
            failures.is_empty(),
            passes,
            "case {name}: {schema} against {instance}: {}",
            shown(&failures)
        );
    }
    let remote = Validator::new(&json!({"$ref": "https://example.com/schema.json"}), false);
    assert!(
        remote.is_err(),
        "a remote reference is never fetched: the schema does not compile"
    );
}
