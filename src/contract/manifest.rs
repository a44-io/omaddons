//! The semantic manifest of omaddons' contract (M4-D56): one canonical, deterministic
//! record of every fact a consumer can rely on, the outputs with their types and bounds,
//! the inputs with every flag and default, the registries and the persisted formats. From
//! the first tag the gate compares it with the released baseline, so an addition needs a
//! higher minor `schema_version` and anything breaking a higher major. It is generated
//! from the code, never kept by hand, and it reads no environment, file, clock or
//! `config.toml`.

pub mod ids;

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::Registry;

/// The node model's own version. Two manifests of different formats are never compared
/// fact by fact (the classifier reports the difference as one unclassified change).
pub const FORMAT: u32 = 1;

/// Every node of the contract, by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub format: u32,
    pub nodes: BTreeMap<String, Node>,
}

/// One fact-bearing point of the contract: a command, a flag, an output position, a code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// A [`NodeKind`]'s wire name. A `String`, so a manifest a newer build wrote keeps a
    /// kind this build does not know.
    pub kind: String,
    #[serde(flatten)]
    pub facts: BTreeMap<String, Fact>,
}

impl Node {
    /// The kind, when this build knows it.
    pub fn kind(&self) -> Option<NodeKind> {
        NodeKind::from_wire(&self.kind)
    }
}

/// A fact's value. Untagged, and in this order, so each written value reads back as
/// itself: an array of strings is `List`, of integers `Ints`. An empty set is never
/// recorded (absence means the empty set), and an absent boolean means `false`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fact {
    Bool(bool),
    Int(i64),
    Text(String),
    List(Vec<String>),
    Ints(Vec<i64>),
}

impl Fact {
    /// The set of `items`, sorted and deduplicated; `None` for an empty one.
    pub fn list<I, S>(items: I) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut items: Vec<String> = items.into_iter().map(Into::into).collect();
        items.sort_unstable();
        items.dedup();
        (!items.is_empty()).then_some(Self::List(items))
    }

    /// The set of `items`, sorted and deduplicated; `None` for an empty one.
    pub fn ints(items: impl IntoIterator<Item = i64>) -> Option<Self> {
        let mut items: Vec<i64> = items.into_iter().collect();
        items.sort_unstable();
        items.dedup();
        (!items.is_empty()).then_some(Self::Ints(items))
    }
}

/// What a node describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    Command,
    Flag,
    Positional,
    OutputPosition,
    OutputVariant,
    FileKind,
    FilePosition,
    FileVariant,
    ConfigKey,
    DesiredPosition,
    DesiredVariant,
    EnvVar,
    ErrorCode,
    WarningCode,
    Action,
    FailureMode,
    Event,
    Phase,
    LogLevel,
    ExitCode,
    DoctorOp,
}

impl NodeKind {
    pub const ALL: [Self; 21] = [
        Self::Command,
        Self::Flag,
        Self::Positional,
        Self::OutputPosition,
        Self::OutputVariant,
        Self::FileKind,
        Self::FilePosition,
        Self::FileVariant,
        Self::ConfigKey,
        Self::DesiredPosition,
        Self::DesiredVariant,
        Self::EnvVar,
        Self::ErrorCode,
        Self::WarningCode,
        Self::Action,
        Self::FailureMode,
        Self::Event,
        Self::Phase,
        Self::LogLevel,
        Self::ExitCode,
        Self::DoctorOp,
    ];

    pub const fn wire(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Flag => "flag",
            Self::Positional => "positional",
            Self::OutputPosition => "output_position",
            Self::OutputVariant => "output_variant",
            Self::FileKind => "file_kind",
            Self::FilePosition => "file_position",
            Self::FileVariant => "file_variant",
            Self::ConfigKey => "config_key",
            Self::DesiredPosition => "desired_position",
            Self::DesiredVariant => "desired_variant",
            Self::EnvVar => "env_var",
            Self::ErrorCode => "error_code",
            Self::WarningCode => "warning_code",
            Self::Action => "action",
            Self::FailureMode => "failure_mode",
            Self::Event => "event",
            Self::Phase => "phase",
            Self::LogLevel => "log_level",
            Self::ExitCode => "exit_code",
            Self::DoctorOp => "doctor_op",
        }
    }

    pub fn from_wire(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.wire() == wire)
    }
}

/// The fact names, one constant each, so every collector spells them the same way. None is
/// `kind`, which would collide with [`Node::kind`] under `flatten`. Adding one means adding
/// its rule to the classifier in the same commit.
pub mod fact {
    // Positions.
    pub const TYPES: &str = "types";
    pub const REQUIRED: &str = "required";
    pub const NULLABLE: &str = "nullable";
    pub const ENUM: &str = "enum";
    pub const MINIMUM: &str = "minimum";
    pub const MAXIMUM: &str = "maximum";
    pub const EXCLUSIVE_MINIMUM: &str = "exclusive_minimum";
    pub const EXCLUSIVE_MAXIMUM: &str = "exclusive_maximum";
    pub const MULTIPLE_OF: &str = "multiple_of";
    pub const MIN_ITEMS: &str = "min_items";
    pub const MAX_ITEMS: &str = "max_items";
    pub const MIN_LENGTH: &str = "min_length";
    pub const MAX_LENGTH: &str = "max_length";
    pub const MIN_PROPERTIES: &str = "min_properties";
    pub const MAX_PROPERTIES: &str = "max_properties";
    pub const PATTERN: &str = "pattern";
    pub const UNIQUE_ITEMS: &str = "unique_items";
    pub const KEY_PATTERNS: &str = "key_patterns";
    pub const OPAQUE: &str = "opaque";
    pub const TAG: &str = "tag";
    pub const SAME_AS: &str = "same_as";
    pub const UNION_OF: &str = "union_of";
    pub const KEYWORDS: &str = "keywords";
    // Commands (`ALIASES` serves arguments too).
    pub const ALIASES: &str = "aliases";
    pub const SUBCOMMAND_REQUIRED: &str = "subcommand_required";
    pub const ARGS_CONFLICT_WITH_SUBCOMMANDS: &str = "args_conflict_with_subcommands";
    pub const EXITS: &str = "exits";
    pub const DICTIONARY: &str = "dictionary";
    pub const MUTATES: &str = "mutates";
    pub const STORES_PLAN: &str = "stores_plan";
    pub const NETWORK: &str = "network";
    pub const BINDS: &str = "binds";
    pub const ENVELOPE_ONLY: &str = "envelope_only";
    // Arguments (`REQUIRED` and `ALIASES` above).
    pub const SHORT: &str = "short";
    pub const SHORT_ALIASES: &str = "short_aliases";
    pub const GRAMMAR: &str = "grammar";
    pub const VALUES: &str = "values";
    pub const IGNORE_CASE: &str = "ignore_case";
    pub const DEFAULT: &str = "default";
    pub const ARITY_MIN: &str = "arity_min";
    pub const ARITY_MAX: &str = "arity_max";
    pub const REPEAT: &str = "repeat";
    pub const GLOBAL: &str = "global";
    pub const CONFLICTS: &str = "conflicts";
    pub const REQUIRES: &str = "requires";
    pub const ACTION: &str = "action";
    pub const VALUE_NAME: &str = "value_name";
    pub const ENV: &str = "env";
    pub const ALLOW_HYPHEN_VALUES: &str = "allow_hyphen_values";
    pub const ALLOW_NEGATIVE_NUMBERS: &str = "allow_negative_numbers";
    pub const TRAILING_VAR_ARG: &str = "trailing_var_arg";
    pub const REQUIRE_EQUALS: &str = "require_equals";
    pub const EXCLUSIVE: &str = "exclusive";
    pub const LAST: &str = "last";
    pub const VALUE_DELIMITER: &str = "value_delimiter";
    // Config keys, env vars, registries, persisted kinds.
    pub const CONFIG_TYPE: &str = "config_type";
    pub const MIRRORS: &str = "mirrors";
    pub const EXIT: &str = "exit";
    pub const DOCTOR_EXIT: &str = "doctor_exit";
    pub const RETRYABLE: &str = "retryable";
    pub const DESTRUCTIVE: &str = "destructive";
    pub const NAME: &str = "name";
    pub const VERSION: &str = "version";

    /// Every fact name above.
    pub const ALL: &[&str] = &[
        TYPES,
        REQUIRED,
        NULLABLE,
        ENUM,
        MINIMUM,
        MAXIMUM,
        EXCLUSIVE_MINIMUM,
        EXCLUSIVE_MAXIMUM,
        MULTIPLE_OF,
        MIN_ITEMS,
        MAX_ITEMS,
        MIN_LENGTH,
        MAX_LENGTH,
        MIN_PROPERTIES,
        MAX_PROPERTIES,
        PATTERN,
        UNIQUE_ITEMS,
        KEY_PATTERNS,
        OPAQUE,
        TAG,
        SAME_AS,
        UNION_OF,
        KEYWORDS,
        ALIASES,
        SUBCOMMAND_REQUIRED,
        ARGS_CONFLICT_WITH_SUBCOMMANDS,
        EXITS,
        DICTIONARY,
        MUTATES,
        STORES_PLAN,
        NETWORK,
        BINDS,
        ENVELOPE_ONLY,
        SHORT,
        SHORT_ALIASES,
        GRAMMAR,
        VALUES,
        IGNORE_CASE,
        DEFAULT,
        ARITY_MIN,
        ARITY_MAX,
        REPEAT,
        GLOBAL,
        CONFLICTS,
        REQUIRES,
        ACTION,
        VALUE_NAME,
        ENV,
        ALLOW_HYPHEN_VALUES,
        ALLOW_NEGATIVE_NUMBERS,
        TRAILING_VAR_ARG,
        REQUIRE_EQUALS,
        EXCLUSIVE,
        LAST,
        VALUE_DELIMITER,
        CONFIG_TYPE,
        MIRRORS,
        EXIT,
        DOCTOR_EXIT,
        RETRYABLE,
        DESTRUCTIVE,
        NAME,
        VERSION,
    ];
}

/// Why a manifest's text could not be read. No command returns it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("manifest: {0}")]
pub struct ParseError(pub String);

impl Manifest {
    /// [`FORMAT`], and no nodes.
    pub fn empty() -> Self {
        Self {
            format: FORMAT,
            nodes: BTreeMap::new(),
        }
    }

    /// The canonical text: the top-level keys sorted, one per line (`format`, `nodes` and
    /// each `extra` key, such as the golden's `header`), an extra value compact on its
    /// line; then one node per line in id order, compact with sorted keys; one final
    /// newline. An extra named `format` or `nodes` is ignored.
    pub fn render(&self, extra: &[(&str, &Value)]) -> String {
        let mut top: BTreeMap<&str, String> = BTreeMap::new();
        top.insert("format", self.format.to_string());
        top.insert("nodes", self.render_nodes());
        for (key, value) in extra {
            top.entry(key).or_insert_with(|| value.to_string());
        }
        let lines: Vec<String> = top
            .iter()
            .map(|(key, value)| format!("  {}: {value}", json_string(key)))
            .collect();
        format!("{{\n{}\n}}\n", lines.join(",\n"))
    }

    fn render_nodes(&self) -> String {
        if self.nodes.is_empty() {
            return "{}".to_owned();
        }
        let lines: Vec<String> = self
            .nodes
            .iter()
            .map(|(id, node)| format!("    {}: {}", json_string(id), compact(node)))
            .collect();
        format!("{{\n{}\n  }}", lines.join(",\n"))
    }

    /// Reads `format` and `nodes` and ignores every other top-level key, so nothing in a
    /// header can change a comparison (M4-D56). Unknown kinds and fact names are kept.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let fail = |message: String| Err(ParseError(message));
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return fail(format!("not JSON: {e}")),
        };
        let Value::Object(top) = value else {
            return fail("not a JSON object".to_owned());
        };
        let format = match top.get("format") {
            None => return fail("`format` is missing".to_owned()),
            Some(format) => match format.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => n,
                None => return fail(format!("`format` is not a non-negative integer: {format}")),
            },
        };
        let nodes = match top.get("nodes") {
            None => return fail("`nodes` is missing".to_owned()),
            Some(Value::Object(nodes)) => nodes,
            Some(other) => return fail(format!("`nodes` is not an object: {other}")),
        };
        let mut parsed = BTreeMap::new();
        for (id, node) in nodes {
            parsed.insert(id.clone(), parse_node(id, node)?);
        }
        Ok(Self {
            format,
            nodes: parsed,
        })
    }
}

fn parse_node(id: &str, value: &Value) -> Result<Node, ParseError> {
    let fail = |what: String| Err(ParseError(format!("node {id:?}: {what}")));
    let Value::Object(map) = value else {
        return fail(format!("not an object: {value}"));
    };
    let kind = match map.get("kind") {
        None => return fail("`kind` is missing".to_owned()),
        Some(Value::String(kind)) => kind.clone(),
        Some(other) => return fail(format!("`kind` is not a string: {other}")),
    };
    let mut facts = BTreeMap::new();
    for (name, value) in map.iter().filter(|(name, _)| *name != "kind") {
        match parse_fact(value) {
            Ok(fact) => {
                facts.insert(name.clone(), fact);
            }
            Err(what) => return fail(format!("fact {name:?} {what}: {value}")),
        }
    }
    Ok(Node { kind, facts })
}

/// One fact's value, or what is wrong with it.
fn parse_fact(value: &Value) -> Result<Fact, &'static str> {
    match value {
        Value::Null => Err("is null"),
        Value::Bool(b) => Ok(Fact::Bool(*b)),
        Value::Number(n) => n.as_i64().map(Fact::Int).ok_or("is not a 64-bit integer"),
        Value::String(s) => Ok(Fact::Text(s.clone())),
        Value::Object(_) => Err("is an object"),
        Value::Array(items) if items.is_empty() => Err("is an empty array"),
        Value::Array(items) => {
            if let Some(texts) = items.iter().map(Value::as_str).collect::<Option<Vec<_>>>() {
                Ok(Fact::List(texts.into_iter().map(str::to_owned).collect()))
            } else if let Some(ints) = items.iter().map(Value::as_i64).collect::<Option<Vec<_>>>() {
                Ok(Fact::Ints(ints))
            } else {
                Err("is neither all strings nor all 64-bit integers")
            }
        }
    }
}

/// `s` as a JSON string.
fn json_string(s: &str) -> String {
    Value::from(s).to_string()
}

/// A node as compact JSON with sorted keys (`serde_json` sorts its maps without
/// `preserve_order`). A node holds only strings, integers, booleans and arrays, so this
/// cannot fail; were it to, the round-trip tests would see the empty object.
fn compact(node: &Node) -> String {
    serde_json::to_value(node).map_or_else(|_| "{}".to_owned(), |value| value.to_string())
}

/// The nodes the collectors insert, and the ids two of them produced differently.
#[derive(Debug, Default)]
pub(crate) struct Builder {
    nodes: BTreeMap<String, Node>,
    conflicts: Vec<String>,
}

impl Builder {
    /// Records `id`. `None` facts are skipped; an identical duplicate is a no-op; one with
    /// another kind or other facts keeps the first and records `"<id>: <first> vs
    /// <second>"`, as does a fact given twice with different values.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "subtasks 2 and 3's collectors are its first callers"
        )
    )]
    pub(crate) fn insert(
        &mut self,
        id: String,
        kind: NodeKind,
        facts: impl IntoIterator<Item = (&'static str, Option<Fact>)>,
    ) {
        let mut node = Node {
            kind: kind.wire().to_owned(),
            facts: BTreeMap::new(),
        };
        for (name, value) in facts {
            let Some(value) = value else { continue };
            match node.facts.entry(name.to_owned()) {
                Entry::Vacant(slot) => {
                    slot.insert(value);
                }
                Entry::Occupied(slot) if *slot.get() == value => {}
                Entry::Occupied(slot) => self
                    .conflicts
                    .push(format!("{id}: fact {name} {:?} vs {value:?}", slot.get())),
            }
        }
        match self.nodes.entry(id) {
            Entry::Vacant(slot) => {
                slot.insert(node);
            }
            Entry::Occupied(slot) if *slot.get() == node => {}
            Entry::Occupied(slot) => {
                let conflict = format!(
                    "{}: {} vs {}",
                    slot.key(),
                    compact(slot.get()),
                    compact(&node)
                );
                self.conflicts.push(conflict);
            }
        }
    }

    fn finish(self) -> (Manifest, Vec<String>) {
        let manifest = Manifest {
            format: FORMAT,
            nodes: self.nodes,
        };
        (manifest, self.conflicts)
    }
}

/// Every section of the manifest, and the ids two collectors produced differently.
pub fn build_checked(_registry: &Registry) -> (Manifest, Vec<String>) {
    // The collectors (outputs, inputs, registries) each add one call here.
    Builder::default().finish()
}

/// The manifest (M4-D56). Infallible: what a collector cannot model becomes a `keywords`
/// fact.
pub fn build(registry: &Registry) -> Manifest {
    build_checked(registry).0
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;
    use crate::source_scan::{Literals, production_code, sources};

    /// The first line where two texts differ.
    fn first_difference(got: &str, want: &str) -> String {
        let (got_lines, want_lines): (Vec<&str>, Vec<&str>) =
            (got.lines().collect(), want.lines().collect());
        for n in 0..got_lines.len().max(want_lines.len()) {
            let (a, b) = (got_lines.get(n), want_lines.get(n));
            if a != b {
                return format!("line {}: got {a:?}, expected {b:?}", n + 1);
            }
        }
        format!(
            "the same lines; got ends {:?}, expected {:?}",
            got.ends_with('\n'),
            want.ends_with('\n')
        )
    }

    fn assert_text(case: &str, got: &str, want: &str) {
        assert!(got == want, "{case}: {}", first_difference(got, want));
    }

    fn some(fact: Fact) -> Option<Fact> {
        Some(fact)
    }

    /// A node's facts as a collector passes them to [`Builder::insert`].
    type Facts = Vec<(&'static str, Option<Fact>)>;

    /// The three nodes of the layout example, inserted in `order`.
    fn three(order: &[usize]) -> Manifest {
        let inserts: [(&str, NodeKind, Facts); 3] = [
            (
                "action apply",
                NodeKind::Action,
                vec![(fact::DESTRUCTIVE, some(Fact::Bool(false)))],
            ),
            (
                "command install",
                NodeKind::Command,
                vec![
                    (fact::ALIASES, Fact::list(["add"])),
                    (fact::BINDS, some(Fact::Bool(false))),
                    (fact::DICTIONARY, some(Fact::Text("general".to_owned()))),
                    (fact::EXITS, Fact::ints([6, 5, 4, 3, 2, 1, 0])),
                    (fact::MUTATES, some(Fact::Bool(true))),
                    (fact::NETWORK, some(Fact::Text("optional".to_owned()))),
                    (fact::STORES_PLAN, some(Fact::Bool(false))),
                ],
            ),
            (
                "envelope meta.elapsed_ms",
                NodeKind::OutputPosition,
                vec![
                    (fact::TYPES, Fact::list(["integer"])),
                    (fact::REQUIRED, some(Fact::Bool(true))),
                    (fact::NULLABLE, some(Fact::Bool(false))),
                    (fact::MINIMUM, some(Fact::Int(0))),
                ],
            ),
        ];
        let mut builder = Builder::default();
        for &i in order {
            let (id, kind, facts) = inserts[i].clone();
            builder.insert(id.to_owned(), kind, facts);
        }
        let (manifest, conflicts) = builder.finish();
        assert_eq!(
            conflicts,
            Vec::<String>::new(),
            "the example has no conflicts"
        );
        manifest
    }

    const LAYOUT: &str = r#"{
  "format": 1,
  "nodes": {
    "action apply": {"destructive":false,"kind":"action"},
    "command install": {"aliases":["add"],"binds":false,"dictionary":"general","exits":[0,1,2,3,4,5,6],"kind":"command","mutates":true,"network":"optional","stores_plan":false},
    "envelope meta.elapsed_ms": {"kind":"output_position","minimum":0,"nullable":false,"required":true,"types":["integer"]}
  }
}
"#;

    #[test]
    fn canonical_json_has_the_documented_layout() {
        assert_text("layout", &three(&[0, 1, 2]).render(&[]), LAYOUT);
        assert_text(
            "empty",
            &Manifest::empty().render(&[]),
            "{\n  \"format\": 1,\n  \"nodes\": {}\n}\n",
        );
    }

    #[test]
    fn render_sorts_ids_keys_and_lists() {
        assert_text("reverse insertion", &three(&[2, 1, 0]).render(&[]), LAYOUT);
        let mut builder = Builder::default();
        builder.insert(
            "command x".to_owned(),
            NodeKind::Command,
            [(fact::ALIASES, Fact::list(["b", "a", "b"]))],
        );
        let (manifest, _) = builder.finish();
        let got = manifest.render(&[]);
        assert!(
            got.contains(r#"{"aliases":["a","b"],"kind":"command"}"#),
            "a sorted, deduplicated list: {got}"
        );
    }

    #[test]
    fn render_places_extra_keys_and_parse_ignores_them() {
        let manifest = three(&[0, 1, 2]);
        let (header, provenance) = (
            json!({"contract": "1.0", "frozen": true}),
            json!({"tag": "v0.1.0"}),
        );
        let text = manifest.render(&[
            ("header", &header),
            ("provenance", &provenance),
            ("format", &json!(9)),
        ]);
        let tops: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("  \"") && !line.starts_with("    "))
            .map(|line| line.split('"').nth(1).unwrap_or(""))
            .collect();
        assert_eq!(
            tops,
            ["format", "header", "nodes", "provenance"],
            "the top-level keys, sorted: {text}"
        );
        assert!(
            text.contains(r#"  "header": {"contract":"1.0","frozen":true},"#),
            "an extra is compact: {text}"
        );
        assert!(
            text.contains("  \"format\": 1,"),
            "an extra named format is ignored: {text}"
        );
        assert_eq!(
            Manifest::parse(&text),
            Ok(manifest.clone()),
            "parse ignores the extras"
        );
        let flipped = text.replace(r#""frozen":true"#, r#""frozen":false"#);
        assert_ne!(flipped, text, "the header was flipped");
        assert_eq!(
            Manifest::parse(&flipped),
            Ok(manifest),
            "the header changes nothing"
        );
    }

    #[test]
    fn render_and_parse_round_trip() {
        let manifest = three(&[0, 1, 2]);
        let text = manifest.render(&[]);
        let parsed = Manifest::parse(&text);
        assert_eq!(parsed, Ok(manifest), "parse reads render's text back");
        assert_eq!(
            Manifest::parse(LAYOUT).map(|m| m.render(&[])),
            Ok(LAYOUT.to_owned()),
            "render is parse's inverse"
        );
    }

    #[test]
    fn parse_rejects_malformed_input() {
        let cases: [(&str, &str, Option<&str>); 14] = [
            ("not_json", "{", None),
            ("format_missing", r#"{"nodes":{}}"#, None),
            ("format_negative", r#"{"format":-1,"nodes":{}}"#, None),
            ("format_not_integer", r#"{"format":1.5,"nodes":{}}"#, None),
            ("nodes_missing", r#"{"format":1}"#, None),
            ("nodes_not_object", r#"{"format":1,"nodes":[]}"#, None),
            (
                "node_not_object",
                r#"{"format":1,"nodes":{"command a":1}}"#,
                Some("command a"),
            ),
            (
                "kind_missing",
                r#"{"format":1,"nodes":{"command b":{}}}"#,
                Some("command b"),
            ),
            (
                "kind_not_string",
                r#"{"format":1,"nodes":{"command c":{"kind":1}}}"#,
                Some("command c"),
            ),
            (
                "fact_null",
                r#"{"format":1,"nodes":{"command d":{"kind":"command","x":null}}}"#,
                Some("command d"),
            ),
            (
                "fact_object",
                r#"{"format":1,"nodes":{"command e":{"kind":"command","x":{}}}}"#,
                Some("command e"),
            ),
            (
                "fact_float",
                r#"{"format":1,"nodes":{"command f":{"kind":"command","x":1.5}}}"#,
                Some("command f"),
            ),
            (
                "fact_mixed_array",
                r#"{"format":1,"nodes":{"command g":{"kind":"command","x":[1,"a"]}}}"#,
                Some("command g"),
            ),
            (
                "fact_empty_array",
                r#"{"format":1,"nodes":{"command h":{"kind":"command","x":[]}}}"#,
                Some("command h"),
            ),
        ];
        for (case, input, id) in cases {
            let got = Manifest::parse(input);
            let message = match &got {
                Err(ParseError(message)) => message.clone(),
                Ok(_) => String::new(),
            };
            assert!(
                got.is_err(),
                "{case}: input {input:?} gave {got:?}, expected an error"
            );
            if let Some(id) = id {
                assert!(
                    message.contains(id),
                    "{case}: input {input:?} gave {message:?}, expected it to name {id:?}"
                );
            }
        }
    }

    #[test]
    fn parse_keeps_unknown_kinds_and_facts() {
        let input = r#"{"format":1,"nodes":{"x y":{"kind":"frobnicate","zeal":true}}}"#;
        let manifest = Manifest::parse(input).unwrap();
        let node = &manifest.nodes["x y"];
        assert_eq!(
            (node.kind(), node.kind.as_str()),
            (None, "frobnicate"),
            "the unknown kind is kept"
        );
        assert_eq!(
            node.facts.get("zeal"),
            Some(&Fact::Bool(true)),
            "the unknown fact is kept"
        );
        let canonical = "{\n  \"format\": 1,\n  \"nodes\": {\n    \"x y\": {\"kind\":\"frobnicate\",\"zeal\":true}\n  }\n}\n";
        assert_text("re-rendered", &manifest.render(&[]), canonical);
        assert_eq!(
            Manifest::parse(canonical),
            Ok(manifest),
            "the canonical form reads back equal"
        );
    }

    #[test]
    fn fact_constructors_refuse_empty_sets() {
        assert_eq!(Fact::list(Vec::<String>::new()), None, "an empty list");
        assert_eq!(Fact::ints(Vec::<i64>::new()), None, "an empty integer set");
        assert_eq!(
            Fact::ints([3, 1, 3]),
            Some(Fact::Ints(vec![1, 3])),
            "sorted and deduplicated"
        );
        assert_eq!(
            Fact::list(["b", "a", "b"]),
            Some(Fact::List(vec!["a".to_owned(), "b".to_owned()]))
        );
    }

    #[test]
    fn builder_reports_conflicts() {
        let flag = |default: &str| [(fact::DEFAULT, Some(Fact::Text(default.to_owned())))];
        let mut builder = Builder::default();
        builder.insert("input x --y".to_owned(), NodeKind::Flag, flag("a"));
        builder.insert("input x --y".to_owned(), NodeKind::Flag, flag("a"));
        assert_eq!(
            builder.conflicts,
            Vec::<String>::new(),
            "an identical duplicate is a no-op"
        );
        builder.insert("input x --y".to_owned(), NodeKind::Flag, flag("b"));
        assert_eq!(
            builder.conflicts.len(),
            1,
            "other facts: {:?}",
            builder.conflicts
        );
        builder.insert("input x --y".to_owned(), NodeKind::Positional, flag("a"));
        assert_eq!(
            builder.conflicts.len(),
            2,
            "another kind: {:?}",
            builder.conflicts
        );
        assert!(
            builder
                .conflicts
                .iter()
                .all(|c| c.starts_with("input x --y: ")),
            "{:?}",
            builder.conflicts
        );
        builder.insert(
            "input x --z".to_owned(),
            NodeKind::Flag,
            [
                (fact::DEFAULT, Some(Fact::Int(1))),
                (fact::DEFAULT, Some(Fact::Int(2))),
                (fact::SHORT, None),
            ],
        );
        let (manifest, conflicts) = builder.finish();
        assert_eq!(conflicts.len(), 3, "a fact given twice: {conflicts:?}");
        assert_eq!(
            manifest.nodes["input x --y"].facts[fact::DEFAULT],
            Fact::Text("a".to_owned()),
            "the first is kept"
        );
        assert_eq!(
            manifest.nodes["input x --z"].facts.len(),
            1,
            "a None fact is skipped"
        );
    }

    #[test]
    fn no_fact_constant_is_kind() {
        let mut seen = std::collections::BTreeSet::new();
        for name in fact::ALL {
            assert_ne!(*name, "kind", "no fact is named kind");
            assert!(
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{name}: snake case"
            );
            assert!(seen.insert(*name), "{name}: distinct");
        }
    }

    #[test]
    fn node_kinds_round_trip() {
        let mut wires = std::collections::BTreeSet::new();
        for kind in NodeKind::ALL {
            assert_eq!(NodeKind::from_wire(kind.wire()), Some(kind), "{kind:?}");
            assert!(wires.insert(kind.wire()), "{kind:?}: a distinct wire name");
        }
        assert_eq!(NodeKind::from_wire("frobnicate"), None);
    }

    #[test]
    fn build_returns_the_format_and_no_conflicts() {
        let (manifest, conflicts) = build_checked(&Registry::builtin());
        assert_eq!((manifest.format, conflicts), (FORMAT, Vec::new()));
        assert_eq!(build(&Registry::builtin()), manifest);
    }

    /// The manifest's own files: this one, every file under `manifest/`, and `wire.rs`
    /// once it exists.
    fn manifest_sources() -> Vec<(PathBuf, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contract");
        let mut files: Vec<(PathBuf, String)> = ["manifest.rs", "wire.rs"]
            .iter()
            .map(|name| root.join(name))
            .filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
            .collect();
        files.extend(sources(&root.join("manifest")));
        assert!(
            files.len() >= 2,
            "manifest.rs and ids.rs are scanned: {files:?}"
        );
        files
    }

    /// Each `pattern` in `text`'s production code that starts (and, ending in a word
    /// character, ends) a word: `line <n>: <pattern>`.
    fn found(text: &str, patterns: &[&str]) -> Vec<String> {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let Some(code) = production_code(text, Literals::Blank) else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for (n, line) in code.lines().enumerate() {
            for pattern in patterns {
                for (at, _) in line.match_indices(pattern) {
                    let before = line[..at].chars().next_back();
                    let after = line[at + pattern.len()..].chars().next();
                    let starts = before.is_none_or(|c| !ident(c));
                    let ends = !pattern.ends_with(ident) || after.is_none_or(|c| !ident(c));
                    if starts && ends {
                        hits.push(format!("line {}: {pattern}", n + 1));
                    }
                }
            }
        }
        hits
    }

    const READS: &[&str] = &[
        "std::env",
        "env::var",
        "std::fs",
        "fs::",
        "File::",
        "Paths",
        "config::load",
        "SystemTime",
        "Timestamp::now",
        "Instant::now",
        "Zoned::now",
    ];

    const HASH_COLLECTIONS: &[&str] = &["HashMap", "HashSet"];

    #[test]
    fn manifest_modules_read_no_environment() {
        for (path, text) in manifest_sources() {
            let hits = found(&text, READS);
            assert_eq!(
                hits,
                Vec::<String>::new(),
                "{}: reads the environment, a file or the clock",
                path.display()
            );
        }
        let planted = "fn f() -> bool {\n    std::env::var(\"X\").is_ok()\n}\n";
        assert_eq!(
            found(planted, READS),
            ["line 2: std::env", "line 2: env::var"],
            "a planted read is flagged"
        );
        let quiet =
            "// std::env::var\nfn g() -> &'static str { \"std::fs\" }\nstruct MyPathsLike;\n";
        assert_eq!(
            found(quiet, READS),
            Vec::<String>::new(),
            "comments, strings and longer words are not reads"
        );
    }

    #[test]
    fn manifest_modules_use_no_hash_collections() {
        for (path, text) in manifest_sources() {
            let hits = found(&text, HASH_COLLECTIONS);
            assert_eq!(
                hits,
                Vec::<String>::new(),
                "{}: a hash collection",
                path.display()
            );
        }
        let planted = "use std::collections::HashMap;\n";
        assert_eq!(
            found(planted, HASH_COLLECTIONS),
            ["line 1: HashMap"],
            "a planted HashMap is flagged"
        );
    }
}
