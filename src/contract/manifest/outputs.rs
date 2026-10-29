//! The manifest's outputs (M4-D56): every published JSON Schema normalized into nodes, by
//! meaning and never by schemars' `$defs` names. Each position records its types (integer
//! and number distinct), requiredness and nullability as separate facts, its bounds, its
//! enum values and its variants (which fields exist under which tag value), open subtrees
//! marked `opaque`. `addons.toml` is recorded here too, as an input (`desired_*` nodes).
//!
//! A position is the conjunction of every schema node that applies there: the node, its
//! `$ref` target and the branches of its `allOf`. A union (`anyOf`, `oneOf`) splits into
//! null branches (`nullable`), scalar branches (merged into the position) and object
//! branches (variants). Anything not modelled is kept in `keywords`, never dropped.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::ids::{self, DATA, ROOT, section};
use super::{Builder, Fact, Node, NodeKind, fact};
use crate::app::{ENVELOPE_ONLY, command_entry};
use crate::contract::Registry;
use crate::contract::schema::{self, is_open, resolve};
use crate::error::ErrorCode;

/// A surface's position and variant kinds.
type Kinds = (NodeKind, NodeKind);

const OUTPUT: Kinds = (NodeKind::OutputPosition, NodeKind::OutputVariant);
const FILE: Kinds = (NodeKind::FilePosition, NodeKind::FileVariant);
const DESIRED: Kinds = (NodeKind::DesiredPosition, NodeKind::DesiredVariant);

/// The file kind recorded as an input, `addons.toml`.
const DESIRED_STATE: &str = "desired_state";

/// Keywords that annotate, or close what the published schemas leave open, and so are
/// never recorded. `format` is one: an integer width change already moves a bound.
const IGNORED: &[&str] = &[
    "$anchor",
    "$comment",
    "$defs",
    "$id",
    "$schema",
    "default",
    "deprecated",
    "description",
    "examples",
    "format",
    "readOnly",
    "title",
    "writeOnly",
];

/// Bounds: the JSON keyword, its fact, and whether the larger of two is the tighter.
const BOUNDS: &[(&str, &str, bool)] = &[
    ("minimum", fact::MINIMUM, true),
    ("maximum", fact::MAXIMUM, false),
    ("exclusiveMinimum", fact::EXCLUSIVE_MINIMUM, true),
    ("exclusiveMaximum", fact::EXCLUSIVE_MAXIMUM, false),
    ("multipleOf", fact::MULTIPLE_OF, true),
    ("minItems", fact::MIN_ITEMS, true),
    ("maxItems", fact::MAX_ITEMS, false),
    ("minLength", fact::MIN_LENGTH, true),
    ("maxLength", fact::MAX_LENGTH, false),
    ("minProperties", fact::MIN_PROPERTIES, true),
    ("maxProperties", fact::MAX_PROPERTIES, false),
];

/// Records every output surface, and `addons.toml`.
pub(crate) fn collect(b: &mut Builder, registry: &Registry) {
    let errors = ErrorSurfaces::new();
    let mut add = |prefix: String, root: &'static str, kinds: Kinds, schema: &Value| {
        Walker::new(b, schema, &prefix, kinds, Some(&errors)).surface(root);
    };
    for info in &registry.commands {
        if let Some(entry) = command_entry(info.path) {
            let schema = schema::command_schema(entry);
            add(
                prefix(section::OUTPUT, info.path),
                DATA,
                OUTPUT,
                schema.as_value(),
            );
        }
    }
    for only in ENVELOPE_ONLY {
        if let Some(schema) = schema::data_schema_for(only.name) {
            add(
                prefix(section::OUTPUT, only.name),
                DATA,
                OUTPUT,
                schema.as_value(),
            );
        }
    }
    add(
        section::ENVELOPE.to_owned(),
        ROOT,
        OUTPUT,
        schema::envelope_schema().as_value(),
    );
    for (name, schema) in schema::event_schemas() {
        add(
            prefix(section::EVENT, name),
            ROOT,
            OUTPUT,
            schema.as_value(),
        );
    }
    for code in ErrorCode::ALL {
        let schema = schema::error_variant(*code);
        add(
            prefix(section::ERROR, code.wire()),
            ROOT,
            OUTPUT,
            schema.as_value(),
        );
    }
    for (kind, schema) in schema::file_schemas() {
        if kind == DESIRED_STATE {
            add(
                section::DESIRED.to_owned(),
                ROOT,
                DESIRED,
                schema.as_value(),
            );
        } else {
            add(prefix(section::FILE, kind), ROOT, FILE, schema.as_value());
        }
    }
}

fn prefix(section: &str, name: &str) -> String {
    format!("{section} {name}")
}

/// Each error code's own surface, normalized, so the union of them all is recognized by
/// what it holds and referenced (`union_of: "error"`) instead of repeated at every route.
struct ErrorSurfaces {
    by_code: BTreeMap<&'static str, BTreeMap<String, Node>>,
}

impl ErrorSurfaces {
    fn new() -> Self {
        let by_code = ErrorCode::ALL
            .iter()
            .map(|code| {
                let schema = schema::error_variant(*code);
                let prefix = prefix(section::ERROR, code.wire());
                let mut b = Builder::default();
                Walker::new(&mut b, schema.as_value(), &prefix, OUTPUT, None).surface(ROOT);
                (code.wire(), b.nodes)
            })
            .collect();
        Self { by_code }
    }
}

/// A `$ref` being resolved on the current route, and the id of the position that entered
/// it.
type Route<'a> = Vec<(&'a str, String)>;

/// What a position is to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A surface's root: no requiredness or nullability.
    Root,
    /// A property, required or not.
    Property(bool),
    /// Array items, a `prefixItems` member or a map's values.
    Element,
    /// A variant of its parent's union: no facts of its own beyond what it holds.
    Variant,
}

/// Everything the schema nodes at one position say, merged as `allOf` merges.
#[derive(Debug, Default)]
struct Shape<'a> {
    /// `None` when no node constrains the type; `null` included while merging.
    types: Option<BTreeSet<String>>,
    /// `None` when any value of the types is allowed.
    enums: Option<Vec<&'a Value>>,
    bounds: BTreeMap<&'static str, (f64, Fact)>,
    pattern: Option<&'a str>,
    unique_items: bool,
    properties: BTreeMap<String, Vec<&'a Value>>,
    required: BTreeSet<String>,
    items: Vec<&'a Value>,
    prefix_items: Vec<Vec<&'a Value>>,
    map_values: Vec<&'a Value>,
    pattern_properties: BTreeMap<String, Vec<&'a Value>>,
    open: bool,
    unions: Vec<Vec<&'a Value>>,
    keywords: BTreeSet<String>,
    same_as: Option<String>,
    /// The `$ref`s this position entered, for its children's route.
    entered: Route<'a>,
}

impl<'a> Shape<'a> {
    /// The types it allows, `null` included: its `type`, else what its enum values are.
    fn effective_types(&self) -> Option<BTreeSet<String>> {
        self.types.clone().or_else(|| {
            self.enums
                .as_ref()
                .map(|values| values.iter().map(|v| json_type(v).to_owned()).collect())
        })
    }

    fn has_children(&self) -> bool {
        !self.properties.is_empty()
            || !self.required.is_empty()
            || !self.map_values.is_empty()
            || !self.pattern_properties.is_empty()
    }

    /// A node that only holds one union: its branches are the union's own.
    fn only_a_union(&self) -> bool {
        self.unions.len() == 1
            && self.types.is_none()
            && self.enums.is_none()
            && self.bounds.is_empty()
            && self.pattern.is_none()
            && !self.has_children()
            && self.items.is_empty()
            && self.prefix_items.is_empty()
            && !self.open
            && self.keywords.is_empty()
            && self.same_as.is_none()
    }
}

/// What a union branch is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `true`, `{}` or annotations only: any value.
    Open,
    Null,
    /// No object type: merged into the position.
    Scalar,
    /// A variant.
    Object,
}

fn class(shape: &Shape<'_>) -> Class {
    let types = shape.effective_types();
    if shape.open && types.is_none() && !shape.has_children() && shape.unions.is_empty() {
        return Class::Open;
    }
    if types
        .as_ref()
        .is_some_and(|t| t.len() == 1 && t.contains("null"))
        && !shape.has_children()
    {
        return Class::Null;
    }
    let object = types.as_ref().is_some_and(|t| t.contains("object"))
        || shape.has_children()
        || (types.is_none() && !shape.unions.is_empty());
    if object { Class::Object } else { Class::Scalar }
}

/// One branch of a union, with the schema nodes it was gathered from.
struct Branch<'a> {
    sources: Vec<&'a Value>,
    shape: Shape<'a>,
}

/// One union's object branches as variants of the position.
struct VariantSet<'a> {
    /// The tag's property path (`["type"]`, `["applied", "command"]`), or none.
    tag: Option<Vec<String>>,
    /// Each variant's position, its sources, and the property path it must not repeat
    /// (its tag; empty for an untagged variant).
    variants: Vec<(String, Vec<&'a Value>, Vec<String>)>,
}

/// What a position's unions add to it.
#[derive(Default)]
struct Unions<'a> {
    types: Option<BTreeSet<String>>,
    enums: Option<Vec<&'a Value>>,
    /// The scalar branches' bounds, when every one has the same.
    bounds: Option<BTreeMap<&'static str, (f64, Fact)>>,
    nullable: bool,
    open: bool,
    sets: Vec<VariantSet<'a>>,
    error: bool,
    keywords: BTreeSet<String>,
}

struct Walker<'a, 'b> {
    builder: &'b mut Builder,
    root: &'a Value,
    prefix: &'b str,
    kinds: Kinds,
    errors: Option<&'b ErrorSurfaces>,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(
        builder: &'b mut Builder,
        root: &'a Value,
        prefix: &'b str,
        kinds: Kinds,
        errors: Option<&'b ErrorSurfaces>,
    ) -> Self {
        Self {
            builder,
            root,
            prefix,
            kinds,
            errors,
        }
    }

    fn id(&self, position: &str) -> String {
        format!("{} {position}", self.prefix)
    }

    /// The whole surface, from its root position.
    fn surface(&mut self, start: &str) {
        let route: Route<'a> = vec![("#", self.id(start))];
        self.walk(start, &[self.root], Role::Root, &route, &[]);
    }

    /// The nodes `schemas` give at `position` on their own, in a builder of their own.
    fn subtree(
        &mut self,
        position: &str,
        schemas: &[&'a Value],
        role: Role,
        route: &Route<'a>,
    ) -> BTreeMap<String, Node> {
        let mut b = Builder::default();
        Walker::new(&mut b, self.root, self.prefix, self.kinds, self.errors).walk(
            position,
            schemas,
            role,
            route,
            &[],
        );
        b.nodes
    }

    /// Adds `node` to `shape`: its keywords, its `$ref` target and its `allOf` branches.
    fn gather(&self, shape: &mut Shape<'a>, node: &'a Value, route: &Route<'a>, id: &str) {
        let object = match node {
            Value::Bool(true) => {
                shape.open = true;
                return;
            }
            Value::Bool(false) => {
                shape.keywords.insert("false".to_owned());
                return;
            }
            Value::Object(object) => object,
            _ => {
                shape.keywords.insert("not a schema".to_owned());
                return;
            }
        };
        if is_open(node) {
            shape.open = true;
        }
        for (keyword, value) in object {
            match keyword.as_str() {
                "$ref" => self.reference(shape, value, route, id),
                "allOf" => {
                    for branch in value.as_array().into_iter().flatten() {
                        self.gather(shape, branch, route, id);
                    }
                }
                "anyOf" | "oneOf" => shape
                    .unions
                    .push(value.as_array().into_iter().flatten().collect()),
                "type" => {
                    let types: BTreeSet<String> = match value {
                        Value::String(t) => [t.clone()].into(),
                        Value::Array(ts) => ts
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        _ => BTreeSet::new(),
                    };
                    shape.types = Some(match shape.types.take() {
                        None => types,
                        Some(had) => intersect_types(&had, &types),
                    });
                }
                "enum" => intersect_enums(shape, value.as_array().into_iter().flatten().collect()),
                "const" => intersect_enums(shape, vec![value]),
                "properties" => {
                    for (key, schema) in value.as_object().into_iter().flatten() {
                        shape
                            .properties
                            .entry(key.clone())
                            .or_default()
                            .push(schema);
                    }
                }
                "required" => shape.required.extend(
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                ),
                "items" => match value {
                    // A closed tuple: `maxItems` already bounds it.
                    Value::Bool(false) => {}
                    Value::Array(_) => {
                        shape.keywords.insert("items".to_owned());
                    }
                    items => shape.items.push(items),
                },
                "prefixItems" => {
                    for (i, schema) in value.as_array().into_iter().flatten().enumerate() {
                        if shape.prefix_items.len() <= i {
                            shape.prefix_items.resize_with(i + 1, Vec::new);
                        }
                        shape.prefix_items[i].push(schema);
                    }
                }
                "additionalProperties" => match value {
                    Value::Bool(_) => {}
                    values => shape.map_values.push(values),
                },
                "unevaluatedProperties" => {
                    if !value.is_boolean() {
                        shape.keywords.insert(keyword.clone());
                    }
                }
                "patternProperties" => {
                    for (pattern, schema) in value.as_object().into_iter().flatten() {
                        shape
                            .pattern_properties
                            .entry(pattern.clone())
                            .or_default()
                            .push(schema);
                    }
                }
                "pattern" => match (shape.pattern, value.as_str()) {
                    (None, Some(pattern)) => shape.pattern = Some(pattern),
                    (Some(had), Some(pattern)) if had == pattern => {}
                    _ => {
                        shape.keywords.insert(keyword.clone());
                    }
                },
                "uniqueItems" => shape.unique_items |= value == &Value::Bool(true),
                ignored if IGNORED.contains(&ignored) => {}
                other => match BOUNDS.iter().find(|(name, _, _)| *name == other) {
                    Some(&(_, name, larger)) => bound(shape, name, larger, value),
                    None => {
                        shape.keywords.insert(other.to_owned());
                    }
                },
            }
        }
    }

    /// A `$ref`: a local one resolved in place, or the position that entered it on this
    /// route named with `same_as`; any other kept in `keywords`.
    fn reference(&self, shape: &mut Shape<'a>, value: &'a Value, route: &Route<'a>, id: &str) {
        let Some(reference) = value.as_str() else {
            shape.keywords.insert("$ref".to_owned());
            return;
        };
        let local = reference == "#" || reference.starts_with("#/$defs/");
        let target = if local {
            resolve(self.root, reference)
        } else {
            None
        };
        let Some(target) = target else {
            shape.keywords.insert(format!("$ref:{reference}"));
            return;
        };
        if let Some((_, at)) = route
            .iter()
            .chain(&shape.entered)
            .find(|(entered, _)| *entered == reference)
        {
            shape.same_as = Some(at.clone());
            return;
        }
        shape.entered.push((reference, id.to_owned()));
        self.gather(shape, target, route, id);
    }

    /// The shape of `schemas` at `id`.
    fn shape(&self, schemas: &[&'a Value], route: &Route<'a>, id: &str) -> Shape<'a> {
        let mut shape = Shape::default();
        for schema in schemas {
            self.gather(&mut shape, schema, route, id);
        }
        shape
    }

    /// Records the position at `position` and everything below it, leaving out the
    /// property at the `exclude` path (a variant's tag, which its name already holds).
    fn walk(
        &mut self,
        position: &str,
        schemas: &[&'a Value],
        role: Role,
        route: &Route<'a>,
        exclude: &[String],
    ) {
        let id = self.id(position);
        let mut shape = self.shape(schemas, route, &id);
        let (kind, variant_kind) = self.kinds;
        let kind = if role == Role::Variant {
            variant_kind
        } else {
            kind
        };
        if let Some(at) = shape.same_as.take() {
            let mut facts = role_facts(role, false);
            facts.push((fact::SAME_AS, Some(Fact::Text(at))));
            self.builder.insert(id, kind, facts);
            return;
        }
        let mut inner = route.clone();
        let unions = self.unions(&mut shape, route, &id);
        inner.extend(shape.entered.iter().cloned());

        let own = shape.effective_types();
        let types = match (own, unions.types.clone()) {
            (Some(own), Some(union)) => Some(intersect_types(&own, &union)),
            (own, union) => own.or(union),
        };
        let nullable = unions.nullable
            || types.as_ref().is_some_and(|t| t.contains("null"))
            || shape
                .enums
                .as_ref()
                .is_some_and(|values| values.iter().any(|v| v.is_null()));
        let enums = match (shape.enums.take(), unions.enums.clone()) {
            (Some(own), Some(union)) => {
                Some(own.into_iter().filter(|v| union.contains(v)).collect())
            }
            (own, union) => own.or(union),
        };
        let mut bounds = shape.bounds.clone();
        if let Some(union) = &unions.bounds {
            for (name, value) in union {
                bounds.entry(name).or_insert_with(|| value.clone());
            }
        }

        let mut facts = role_facts(role, nullable);
        if role != Role::Variant {
            let named: Vec<String> = types
                .iter()
                .flatten()
                .filter(|t| *t != "null")
                .cloned()
                .collect();
            facts.push((fact::TYPES, Fact::list(named)));
            if let Some(values) = &enums {
                facts.push((
                    fact::ENUM,
                    Fact::list(values.iter().filter(|v| !v.is_null()).map(|v| encode(v))),
                ));
            }
            for (name, (_, value)) in &bounds {
                facts.push((name, Some(value.clone())));
            }
            facts.push((
                fact::PATTERN,
                shape.pattern.map(|p| Fact::Text(p.to_owned())),
            ));
            facts.push((
                fact::UNIQUE_ITEMS,
                shape.unique_items.then_some(Fact::Bool(true)),
            ));
        }
        facts.push((
            fact::OPAQUE,
            (shape.open || unions.open).then_some(Fact::Bool(true)),
        ));
        let tags: Vec<String> = unions
            .sets
            .iter()
            .filter_map(|set| set.tag.as_ref().map(|t| t.join(".")))
            .collect();
        facts.push((
            fact::TAG,
            (!tags.is_empty()).then(|| Fact::Text(tags.join(","))),
        ));
        facts.push((
            fact::UNION_OF,
            unions.error.then(|| Fact::Text("error".to_owned())),
        ));

        let mut keywords = shape.keywords.clone();
        keywords.extend(unions.keywords.iter().cloned());
        let map = self.map_values(&shape, position, &inner, &mut keywords);
        facts.push((
            fact::KEY_PATTERNS,
            map.as_ref()
                .and_then(|(patterns, _)| Fact::list(patterns.clone())),
        ));
        facts.push((fact::KEYWORDS, Fact::list(keywords)));
        self.builder.insert(id, kind, facts);

        for (key, schemas) in &shape.properties {
            let below: &[String] = match exclude.split_first() {
                Some((first, [])) if first == key => continue,
                Some((first, rest)) if first == key => rest,
                _ => &[],
            };
            let required = shape.required.contains(key);
            self.walk(
                &ids::push_key(position, key),
                schemas,
                Role::Property(required),
                &inner,
                below,
            );
        }
        if !shape.items.is_empty() {
            let items = shape.items.clone();
            self.walk(
                &ids::push_items(position),
                &items,
                Role::Element,
                &inner,
                &[],
            );
        }
        for (i, schemas) in shape.prefix_items.iter().enumerate() {
            self.walk(
                &ids::push_index(position, i),
                schemas,
                Role::Element,
                &inner,
                &[],
            );
        }
        if let Some((_, values)) = map {
            self.walk(
                &ids::push_map(position),
                &values,
                Role::Element,
                &inner,
                &[],
            );
        }
        for set in unions.sets {
            for (variant, sources, exclude) in set.variants {
                self.walk(&variant, &sources, Role::Variant, &inner, &exclude);
            }
        }
    }

    /// The map values to walk at `{}`, and the key patterns. Differing pattern values are
    /// not modelled: `keywords` names them and nothing is walked.
    fn map_values(
        &mut self,
        shape: &Shape<'a>,
        position: &str,
        route: &Route<'a>,
        keywords: &mut BTreeSet<String>,
    ) -> Option<(Vec<String>, Vec<&'a Value>)> {
        if shape.pattern_properties.is_empty() {
            return (!shape.map_values.is_empty()).then(|| (Vec::new(), shape.map_values.clone()));
        }
        let mut candidates: Vec<Vec<&'a Value>> =
            shape.pattern_properties.values().cloned().collect();
        if !shape.map_values.is_empty() {
            candidates.push(shape.map_values.clone());
        }
        let at = ids::push_map(position);
        let first = self.subtree(&at, &candidates[0], Role::Element, route);
        let same = candidates[1..]
            .iter()
            .all(|schemas| self.subtree(&at, schemas, Role::Element, route) == first);
        if same {
            Some((
                shape.pattern_properties.keys().cloned().collect(),
                candidates[0].clone(),
            ))
        } else {
            keywords.insert("patternProperties".to_owned());
            None
        }
    }

    /// Splits each union at the position: null branches make it nullable, scalar branches
    /// merge into it, object branches become variants, and a lone non-null branch is
    /// merged in whole (`Option<T>`).
    fn unions(&mut self, shape: &mut Shape<'a>, route: &Route<'a>, id: &str) -> Unions<'a> {
        let mut out = Unions::default();
        let position = id
            .strip_prefix(self.prefix)
            .map_or(id, |rest| rest.trim_start())
            .to_owned();
        let mut next = 0;
        while next < shape.unions.len() {
            let union = shape.unions[next].clone();
            next += 1;
            let here: Route<'a> = route.iter().chain(&shape.entered).cloned().collect();
            let branches = self.branches(&union, &here, id);
            let mut nullable = false;
            let mut scalars = Vec::new();
            let mut objects = Vec::new();
            for branch in branches {
                match class(&branch.shape) {
                    Class::Open => out.open = true,
                    Class::Null => nullable = true,
                    Class::Scalar => scalars.push(branch),
                    Class::Object => objects.push(branch),
                }
            }
            out.nullable |= nullable;
            if scalars.len() + objects.len() == 1 && !out.open {
                let lone = scalars.pop().or_else(|| objects.pop());
                for source in lone.into_iter().flat_map(|branch| branch.sources) {
                    self.gather(shape, source, route, id);
                }
                continue;
            }
            self.merge_scalars(&mut out, scalars);
            if objects.is_empty() {
                continue;
            }
            add_type(&mut out.types, "object");
            let set = self.variants(&position, objects, &here);
            if self.is_error_union(&set, &here) {
                out.error = true;
            } else {
                out.sets.push(set);
            }
        }
        out
    }

    /// A union's branches, each a nested union of its own flattened into the list.
    fn branches(&self, union: &[&'a Value], route: &Route<'a>, id: &str) -> Vec<Branch<'a>> {
        let mut out = Vec::new();
        for source in union {
            let shape = self.shape(&[source], route, id);
            if shape.only_a_union() {
                let inner = shape.unions[0].clone();
                out.extend(self.branches(&inner, route, id));
            } else {
                out.push(Branch {
                    sources: vec![source],
                    shape,
                });
            }
        }
        out
    }

    /// Scalar branches: their types united, their enum values united when every one is
    /// enumerated, a bound kept when every one has the same.
    fn merge_scalars(&self, out: &mut Unions<'a>, scalars: Vec<Branch<'a>>) {
        if scalars.is_empty() {
            return;
        }
        let mut enums: Option<Vec<&'a Value>> = Some(Vec::new());
        let mut bounds: Option<BTreeMap<&'static str, (f64, Fact)>> = None;
        let mut with_items = 0;
        for branch in &scalars {
            let types = branch.shape.effective_types().unwrap_or_default();
            out.nullable |= types.contains("null");
            for t in types.iter().filter(|t| *t != "null") {
                add_type(&mut out.types, t);
            }
            enums = match (enums, &branch.shape.enums) {
                (Some(all), Some(values)) => Some(united(all, values.iter().copied())),
                _ => None,
            };
            bounds = Some(match bounds {
                None => branch.shape.bounds.clone(),
                Some(had) => had
                    .into_iter()
                    .filter(|(name, v)| branch.shape.bounds.get(name).is_some_and(|w| w.1 == v.1))
                    .collect(),
            });
            if !branch.shape.items.is_empty() || !branch.shape.prefix_items.is_empty() {
                with_items += 1;
            }
        }
        if with_items > 1 {
            out.keywords.insert("anyOf".to_owned());
        }
        out.enums = match (out.enums.take(), enums) {
            (Some(had), Some(values)) => Some(united(had, values)),
            (None, values) => values,
            (Some(_), None) => None,
        };
        out.bounds = bounds;
    }

    /// The object branches as variants: tagged when a property tells them apart (at the
    /// top level, else one level down), else named by their keys.
    fn variants(
        &mut self,
        position: &str,
        objects: Vec<Branch<'a>>,
        route: &Route<'a>,
    ) -> VariantSet<'a> {
        if objects.len() > 1
            && let Some((tag, values)) = self.tag(&objects, route, position)
        {
            let parts: Vec<&str> = tag.iter().map(String::as_str).collect();
            let variants = objects
                .into_iter()
                .zip(values)
                .map(|(branch, value)| {
                    (
                        ids::push_variant_at(position, &parts, &value),
                        branch.sources,
                        tag.clone(),
                    )
                })
                .collect();
            return VariantSet {
                tag: Some(tag),
                variants,
            };
        }
        // Untagged: named by required keys; a name two different branches share also names
        // the optional keys; identical branches are one variant.
        let mut unique: Vec<(Branch<'a>, BTreeMap<String, Node>)> = Vec::new();
        for branch in objects {
            let nodes = self.subtree(position, &branch.sources, Role::Variant, route);
            if !unique.iter().any(|(_, seen)| *seen == nodes) {
                unique.push((branch, nodes));
            }
        }
        let names: Vec<(Vec<&str>, Vec<&str>)> = unique
            .iter()
            .map(|(branch, _)| {
                let required: Vec<&str> =
                    branch.shape.required.iter().map(String::as_str).collect();
                let optional: Vec<&str> = branch
                    .shape
                    .properties
                    .keys()
                    .map(String::as_str)
                    .filter(|key| !branch.shape.required.contains(*key))
                    .collect();
                (required, optional)
            })
            .collect();
        let variants = unique
            .iter()
            .zip(&names)
            .map(|((branch, _), (required, optional))| {
                let shared = names.iter().filter(|(r, _)| r == required).count() > 1;
                let name = if shared {
                    ids::push_untagged(position, required, optional)
                } else {
                    ids::push_untagged(position, required, &[])
                };
                (name, branch.sources.clone(), Vec::new())
            })
            .collect();
        VariantSet {
            tag: None,
            variants,
        }
    }

    /// The tag that tells `objects` apart, and each branch's value: a property every
    /// branch requires with one string value, distinct across branches; else such a
    /// property of an object property every branch requires.
    fn tag(
        &self,
        objects: &[Branch<'a>],
        route: &Route<'a>,
        position: &str,
    ) -> Option<(Vec<String>, Vec<String>)> {
        let id = self.id(position);
        let common: BTreeSet<&String> = objects
            .iter()
            .map(|branch| {
                branch
                    .shape
                    .required
                    .iter()
                    .filter(|key| branch.shape.properties.contains_key(*key))
                    .collect::<BTreeSet<_>>()
            })
            .reduce(|a, b| a.intersection(&b).copied().collect())
            .unwrap_or_default();
        let distinct =
            |values: &[String]| values.iter().collect::<BTreeSet<_>>().len() == values.len();
        for key in &common {
            let values: Option<Vec<String>> = objects
                .iter()
                .map(|branch| self.single_string(&branch.shape.properties[*key], route, &id))
                .collect();
            if let Some(values) = values.filter(|v| distinct(v)) {
                return Some((vec![(*key).clone()], values));
            }
        }
        for key in &common {
            let inner: Vec<Shape<'a>> = objects
                .iter()
                .map(|branch| self.shape(&branch.shape.properties[*key], route, &id))
                .collect();
            let nested: BTreeSet<&String> = inner
                .iter()
                .map(|shape| {
                    shape
                        .required
                        .iter()
                        .filter(|k| shape.properties.contains_key(*k))
                        .collect::<BTreeSet<_>>()
                })
                .reduce(|a, b| a.intersection(&b).copied().collect())
                .unwrap_or_default();
            for sub in nested {
                let values: Option<Vec<String>> = inner
                    .iter()
                    .map(|shape| self.single_string(&shape.properties[sub], route, &id))
                    .collect();
                if let Some(values) = values.filter(|v| distinct(v)) {
                    return Some((vec![(*key).clone(), sub.clone()], values));
                }
            }
        }
        None
    }

    /// The one string `schemas` allow, if they allow exactly one.
    fn single_string(&self, schemas: &[&'a Value], route: &Route<'a>, id: &str) -> Option<String> {
        let shape = self.shape(schemas, route, id);
        let string_only = shape
            .types
            .as_ref()
            .is_none_or(|t| t.len() == 1 && t.contains("string"));
        match shape.enums.as_deref() {
            Some([Value::String(value)]) if string_only => Some(value.clone()),
            _ => None,
        }
    }

    /// Whether `set` is the error union: tagged by `code`, one variant per registered code,
    /// each normalizing to that code's own `error <code>` surface.
    fn is_error_union(&mut self, set: &VariantSet<'a>, route: &Route<'a>) -> bool {
        let Some(errors) = self.errors else {
            return false;
        };
        if set.tag.as_deref() != Some(&["code".to_owned()][..])
            || set.variants.len() != errors.by_code.len()
        {
            return false;
        }
        set.variants.iter().all(|(variant, sources, _)| {
            let code = ids::segments(variant).last().and_then(|segment| {
                let value = segment.strip_prefix("<code=")?.strip_suffix('>')?;
                errors.by_code.get_key_value(value).map(|(code, _)| *code)
            });
            let Some(code) = code else { return false };
            let prefix = prefix(section::ERROR, code);
            let mut b = Builder::default();
            Walker::new(&mut b, self.root, &prefix, OUTPUT, None).walk(
                ROOT,
                sources,
                Role::Root,
                route,
                &[],
            );
            errors.by_code.get(code) == Some(&b.nodes)
        })
    }
}

/// `required` and `nullable` as a role carries them.
fn role_facts(role: Role, nullable: bool) -> Vec<(&'static str, Option<Fact>)> {
    match role {
        Role::Root | Role::Variant => Vec::new(),
        Role::Element => vec![(fact::NULLABLE, Some(Fact::Bool(nullable)))],
        Role::Property(required) => vec![
            (fact::NULLABLE, Some(Fact::Bool(nullable))),
            (fact::REQUIRED, Some(Fact::Bool(required))),
        ],
    }
}

/// `values` added to `all`, each once.
fn united<'a>(
    mut all: Vec<&'a Value>,
    values: impl IntoIterator<Item = &'a Value>,
) -> Vec<&'a Value> {
    for value in values {
        if !all.contains(&value) {
            all.push(value);
        }
    }
    all
}

fn add_type(types: &mut Option<BTreeSet<String>>, t: &str) {
    types.get_or_insert_with(BTreeSet::new).insert(t.to_owned());
}

/// What both type sets allow; an integer is a number.
fn intersect_types(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = a.intersection(b).cloned().collect();
    let crossed = (a.contains("number") && b.contains("integer"))
        || (a.contains("integer") && b.contains("number"));
    if crossed {
        out.insert("integer".to_owned());
    }
    out
}

fn intersect_enums<'a>(shape: &mut Shape<'a>, values: Vec<&'a Value>) {
    shape.enums = Some(match shape.enums.take() {
        None => values,
        Some(had) => had.into_iter().filter(|v| values.contains(v)).collect(),
    });
}

/// Keeps the tighter of two bounds of one kind.
fn bound(shape: &mut Shape<'_>, name: &'static str, larger_is_tighter: bool, value: &Value) {
    let Some(number) = value.as_number() else {
        shape.keywords.insert(name.to_owned());
        return;
    };
    let fact = number
        .as_i64()
        .map_or_else(|| Fact::Text(number.to_string()), Fact::Int);
    let float = number.as_f64().unwrap_or(0.0);
    let tighter = |had: f64| {
        if larger_is_tighter {
            float > had
        } else {
            float < had
        }
    };
    match shape.bounds.get(name) {
        Some((had, _)) if !tighter(*had) => {}
        _ => {
            shape.bounds.insert(name, (float, fact));
        }
    }
}

/// A JSON value's type, as `type` names it.
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

/// An enum value as the manifest writes it: a string as itself, anything else as
/// `json:<JSON>`, and a string that begins `json:` as `json:"json:…"`.
fn encode(value: &Value) -> String {
    match value {
        Value::String(s) if !s.starts_with("json:") => s.clone(),
        other => format!("json:{other}"),
    }
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Serialize;
    use serde_json::json;

    use super::*;
    use crate::contract::OPAQUE;
    use crate::contract::events::EventName;
    use crate::contract::manifest::build_checked;
    use crate::state::store::KINDS;

    type Listed = Vec<(String, String, Vec<(String, Fact)>)>;

    const P: &str = "output_position";
    const V: &str = "output_variant";

    fn listed(nodes: BTreeMap<String, Node>) -> Listed {
        nodes
            .into_iter()
            .map(|(id, node)| (id, node.kind, node.facts.into_iter().collect()))
            .collect()
    }

    /// `schema` normalized as the data of a command `t`.
    fn nodes_of(schema: Value) -> Listed {
        let mut b = Builder::default();
        Walker::new(&mut b, &schema, "output t", OUTPUT, None).surface(DATA);
        listed(b.nodes)
    }

    fn nodes_with_errors(schema: &Value, errors: &ErrorSurfaces) -> BTreeMap<String, Node> {
        let mut b = Builder::default();
        Walker::new(&mut b, schema, "output t", OUTPUT, Some(errors)).surface(DATA);
        b.nodes
    }

    fn list(items: &[&str]) -> Fact {
        Fact::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// One expected node, its facts sorted by name.
    fn n(
        position: &str,
        kind: &str,
        facts: &[(&str, Fact)],
    ) -> (String, String, Vec<(String, Fact)>) {
        let mut facts: Vec<(String, Fact)> = facts
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        facts.sort();
        (format!("output t {position}"), kind.to_owned(), facts)
    }

    /// A property position: its types, requiredness and nullability.
    fn prop(
        position: &str,
        types: &[&str],
        required: bool,
        nullable: bool,
    ) -> (String, String, Vec<(String, Fact)>) {
        n(
            position,
            P,
            &[
                (fact::TYPES, list(types)),
                (fact::REQUIRED, Fact::Bool(required)),
                (fact::NULLABLE, Fact::Bool(nullable)),
            ],
        )
    }

    fn object(position: &str) -> (String, String, Vec<(String, Fact)>) {
        n(position, P, &[(fact::TYPES, list(&["object"]))])
    }

    fn variant(position: &str) -> (String, String, Vec<(String, Fact)>) {
        n(position, V, &[])
    }

    fn sorted(mut nodes: Listed) -> Listed {
        nodes.sort();
        nodes
    }

    fn x_def() -> Value {
        json!({"type": "object", "properties": {"k": {"type": "integer"}}, "required": ["k"]})
    }

    fn tagged(tag: &str, value: &str, extra: Value) -> Value {
        let mut properties = json!({tag: {"type": "string", "const": value}});
        let mut required = vec![tag.to_owned()];
        for (key, schema) in extra.as_object().into_iter().flatten() {
            properties[key] = schema.clone();
            required.push(key.clone());
        }
        json!({"type": "object", "properties": properties, "required": required})
    }

    #[test]
    fn normalizes_each_schema_shape() {
        let s = json!({"type": "string"});
        let i = json!({"type": "integer"});
        let cases: Vec<(&str, Value, Listed)> = vec![
            (
                "plain_object",
                json!({"type": "object", "properties": {"a": s}, "required": ["a"]}),
                vec![object("data"), prop("data.a", &["string"], true, false)],
            ),
            (
                "optional_and_nullable_are_distinct",
                json!({"type": "object", "properties": {"a": s, "b": {"type": ["string", "null"]}}, "required": ["b"]}),
                vec![
                    object("data"),
                    prop("data.a", &["string"], false, false),
                    prop("data.b", &["string"], true, true),
                ],
            ),
            (
                "option_of_ref",
                json!({"type": "object", "properties": {"b": {"anyOf": [{"$ref": "#/$defs/X"}, {"type": "null"}]}}, "$defs": {"X": x_def()}}),
                vec![
                    object("data"),
                    prop("data.b", &["object"], false, true),
                    prop("data.b.k", &["integer"], true, false),
                ],
            ),
            (
                "ref_used_twice_is_recorded_per_route",
                json!({"type": "object", "properties": {"a": {"$ref": "#/$defs/X"}, "b": {"$ref": "#/$defs/X"}}, "$defs": {"X": x_def()}}),
                vec![
                    object("data"),
                    prop("data.a", &["object"], false, false),
                    prop("data.a.k", &["integer"], true, false),
                    prop("data.b", &["object"], false, false),
                    prop("data.b.k", &["integer"], true, false),
                ],
            ),
            (
                "self_recursive_ref_is_same_as",
                json!({"type": "object", "properties": {"x": {"$ref": "#/$defs/X"}}, "$defs": {"X": {"type": "object", "properties": {"child": {"$ref": "#/$defs/X"}}}}}),
                vec![
                    object("data"),
                    prop("data.x", &["object"], false, false),
                    n(
                        "data.x.child",
                        P,
                        &[
                            (fact::SAME_AS, Fact::Text("output t data.x".into())),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "integer_and_number_are_distinct",
                json!({"type": "object", "properties": {"i": i, "n": {"type": "number"}}}),
                vec![
                    object("data"),
                    prop("data.i", &["integer"], false, false),
                    prop("data.n", &["number"], false, false),
                ],
            ),
            (
                "bounds_are_recorded",
                json!({"type": "object", "properties": {
                    "v": {"type": "integer", "minimum": 0, "maximum": 255, "exclusiveMinimum": -1},
                    "l": {"type": "array", "minItems": 1, "maxItems": 3, "uniqueItems": true,
                          "items": {"type": "string", "minLength": 1, "maxLength": 8, "pattern": "^a"}}
                }}),
                vec![
                    object("data"),
                    n(
                        "data.l",
                        P,
                        &[
                            (fact::TYPES, list(&["array"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                            (fact::MIN_ITEMS, Fact::Int(1)),
                            (fact::MAX_ITEMS, Fact::Int(3)),
                            (fact::UNIQUE_ITEMS, Fact::Bool(true)),
                        ],
                    ),
                    n(
                        "data.l[]",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::NULLABLE, Fact::Bool(false)),
                            (fact::MIN_LENGTH, Fact::Int(1)),
                            (fact::MAX_LENGTH, Fact::Int(8)),
                            (fact::PATTERN, Fact::Text("^a".into())),
                        ],
                    ),
                    n(
                        "data.v",
                        P,
                        &[
                            (fact::TYPES, list(&["integer"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                            (fact::MINIMUM, Fact::Int(0)),
                            (fact::MAXIMUM, Fact::Int(255)),
                            (fact::EXCLUSIVE_MINIMUM, Fact::Int(-1)),
                        ],
                    ),
                ],
            ),
            (
                "non_integer_bound_is_text",
                json!({"type": "number", "maximum": 1.5}),
                vec![n(
                    "data",
                    P,
                    &[
                        (fact::TYPES, list(&["number"])),
                        (fact::MAXIMUM, Fact::Text("1.5".into())),
                    ],
                )],
            ),
            (
                "const_is_an_enum_of_one",
                json!({"type": "string", "const": "x"}),
                vec![n(
                    "data",
                    P,
                    &[(fact::TYPES, list(&["string"])), (fact::ENUM, list(&["x"]))],
                )],
            ),
            (
                "null_in_enum_is_nullable",
                json!({"type": "object", "properties": {"v": {"type": ["string", "null"], "enum": ["a", "b", null]}}}),
                vec![
                    object("data"),
                    n(
                        "data.v",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::ENUM, list(&["a", "b"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(true)),
                        ],
                    ),
                ],
            ),
            (
                "non_string_enum_values",
                json!({"enum": [1, true, "json:x"]}),
                vec![n(
                    "data",
                    P,
                    &[
                        (fact::TYPES, list(&["boolean", "integer", "string"])),
                        (
                            fact::ENUM,
                            list(&["json:\"json:x\"", "json:1", "json:true"]),
                        ),
                    ],
                )],
            ),
            (
                "oneof_of_consts_is_an_enum",
                json!({"oneOf": [{"type": "string", "const": "a", "description": "A"}, {"type": "string", "const": "b"}]}),
                vec![n(
                    "data",
                    P,
                    &[
                        (fact::TYPES, list(&["string"])),
                        (fact::ENUM, list(&["a", "b"])),
                    ],
                )],
            ),
            (
                "internally_tagged_union",
                json!({"oneOf": [tagged("type", "a", json!({"x": i})), tagged("type", "b", json!({"x": i, "y": s}))]}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::TAG, Fact::Text("type".into())),
                        ],
                    ),
                    variant("data<type=a>"),
                    prop("data<type=a>.x", &["integer"], true, false),
                    variant("data<type=b>"),
                    prop("data<type=b>.x", &["integer"], true, false),
                    prop("data<type=b>.y", &["string"], true, false),
                ],
            ),
            (
                "nested_tag_is_found",
                json!({"oneOf": [
                    {"type": "object", "properties": {"applied": tagged("command", "install", json!({})), "result": s}, "required": ["applied", "result"]},
                    {"type": "object", "properties": {"applied": tagged("command", "update", json!({})), "result": s}, "required": ["applied", "result"]}
                ]}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::TAG, Fact::Text("applied.command".into())),
                        ],
                    ),
                    variant("data<applied.command=install>"),
                    prop(
                        "data<applied.command=install>.applied",
                        &["object"],
                        true,
                        false,
                    ),
                    prop(
                        "data<applied.command=install>.result",
                        &["string"],
                        true,
                        false,
                    ),
                    variant("data<applied.command=update>"),
                    prop(
                        "data<applied.command=update>.applied",
                        &["object"],
                        true,
                        false,
                    ),
                    prop(
                        "data<applied.command=update>.result",
                        &["string"],
                        true,
                        false,
                    ),
                ],
            ),
            (
                "two_candidate_tags_pick_the_first",
                json!({"oneOf": [
                    tagged("code", "a", json!({"event": {"type": "string", "const": "x"}})),
                    tagged("code", "b", json!({"event": {"type": "string", "const": "y"}}))
                ]}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::TAG, Fact::Text("code".into())),
                        ],
                    ),
                    variant("data<code=a>"),
                    n(
                        "data<code=a>.event",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::ENUM, list(&["x"])),
                            (fact::REQUIRED, Fact::Bool(true)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                    variant("data<code=b>"),
                    n(
                        "data<code=b>.event",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::ENUM, list(&["y"])),
                            (fact::REQUIRED, Fact::Bool(true)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "partly_tagged_union_is_untagged",
                json!({"oneOf": [tagged("type", "a", json!({})), {"type": "object", "properties": {"z": s}, "required": ["z"]}]}),
                vec![
                    object("data"),
                    variant("data<untagged=type>"),
                    n(
                        "data<untagged=type>.type",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::ENUM, list(&["a"])),
                            (fact::REQUIRED, Fact::Bool(true)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                    variant("data<untagged=z>"),
                    prop("data<untagged=z>.z", &["string"], true, false),
                ],
            ),
            (
                "untagged_union_named_by_required_keys",
                json!({"anyOf": [
                    {"type": "object", "properties": {"name": s, "usage": s}, "required": ["name", "usage"]},
                    {"type": "object", "properties": {"command": s}, "required": ["command"]},
                    {"type": "object", "properties": {"version": s}, "required": ["version"]}
                ]}),
                vec![
                    object("data"),
                    variant("data<untagged=command>"),
                    prop("data<untagged=command>.command", &["string"], true, false),
                    variant("data<untagged=name,usage>"),
                    prop("data<untagged=name,usage>.name", &["string"], true, false),
                    prop("data<untagged=name,usage>.usage", &["string"], true, false),
                    variant("data<untagged=version>"),
                    prop("data<untagged=version>.version", &["string"], true, false),
                ],
            ),
            (
                "untagged_name_collision_uses_optional_keys",
                json!({"anyOf": [
                    {"type": "object", "properties": {"a": s, "b": s}, "required": ["a"]},
                    {"type": "object", "properties": {"a": s, "c": s}, "required": ["a"]}
                ]}),
                vec![
                    object("data"),
                    variant("data<untagged=a|b>"),
                    prop("data<untagged=a|b>.a", &["string"], true, false),
                    prop("data<untagged=a|b>.b", &["string"], false, false),
                    variant("data<untagged=a|c>"),
                    prop("data<untagged=a|c>.a", &["string"], true, false),
                    prop("data<untagged=a|c>.c", &["string"], false, false),
                ],
            ),
            (
                "identical_untagged_branches_merge",
                json!({"anyOf": [
                    {"type": "object", "properties": {"a": s}, "required": ["a"]},
                    {"type": "object", "properties": {"a": s}, "required": ["a"]}
                ]}),
                vec![
                    object("data"),
                    variant("data<untagged=a>"),
                    prop("data<untagged=a>.a", &["string"], true, false),
                ],
            ),
            (
                "scalar_branches_merge",
                json!({"anyOf": [{"enum": ["a"]}, {"enum": ["b"]}]}),
                vec![n(
                    "data",
                    P,
                    &[
                        (fact::TYPES, list(&["string"])),
                        (fact::ENUM, list(&["a", "b"])),
                    ],
                )],
            ),
            (
                "open_scalar_branch_drops_enum",
                json!({"anyOf": [{"enum": ["a"]}, {"type": "string"}]}),
                vec![n("data", P, &[(fact::TYPES, list(&["string"]))])],
            ),
            (
                "mixed_scalar_and_object_branches",
                json!({"anyOf": [{"type": "string"}, {"type": "object", "properties": {"k": s}, "required": ["k"]}]}),
                vec![
                    n("data", P, &[(fact::TYPES, list(&["object", "string"]))]),
                    variant("data<untagged=k>"),
                    prop("data<untagged=k>.k", &["string"], true, false),
                ],
            ),
            (
                "allof_merges_properties_and_required",
                json!({"allOf": [{"type": "object", "properties": {"a": s}, "required": ["a"]}, {"properties": {"b": s}}]}),
                vec![
                    object("data"),
                    prop("data.a", &["string"], true, false),
                    prop("data.b", &["string"], false, false),
                ],
            ),
            (
                "allof_of_two_unions_keeps_both_variant_sets",
                json!({"allOf": [
                    {"oneOf": [tagged("x", "1", json!({})), tagged("x", "2", json!({}))]},
                    {"oneOf": [tagged("y", "a", json!({})), tagged("y", "b", json!({}))]}
                ]}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::TAG, Fact::Text("x,y".into())),
                        ],
                    ),
                    variant("data<x=1>"),
                    variant("data<x=2>"),
                    variant("data<y=a>"),
                    variant("data<y=b>"),
                ],
            ),
            (
                "ref_with_sibling_keywords_merges",
                json!({"$ref": "#/$defs/X", "description": "d", "properties": {"extra": s}, "$defs": {"X": x_def()}}),
                vec![
                    object("data"),
                    prop("data.extra", &["string"], false, false),
                    prop("data.k", &["integer"], true, false),
                ],
            ),
            (
                "map_additional_properties_is_braces",
                json!({"type": "object", "additionalProperties": i}),
                vec![
                    object("data"),
                    n(
                        "data{}",
                        P,
                        &[
                            (fact::TYPES, list(&["integer"])),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "map_pattern_properties_records_key_patterns",
                json!({"type": "object", "patternProperties": {"^[0-9A-Z]{26}$": i}}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::KEY_PATTERNS, list(&["^[0-9A-Z]{26}$"])),
                        ],
                    ),
                    n(
                        "data{}",
                        P,
                        &[
                            (fact::TYPES, list(&["integer"])),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "map_differing_pattern_values_is_unmodelled",
                json!({"type": "object", "patternProperties": {"^a": i, "^b": s}}),
                vec![n(
                    "data",
                    P,
                    &[
                        (fact::TYPES, list(&["object"])),
                        (fact::KEYWORDS, list(&["patternProperties"])),
                    ],
                )],
            ),
            (
                "enum_keyed_map_is_optional_properties",
                json!({"type": "object", "properties": {"mainline": i, "classic": i}, "additionalProperties": false}),
                vec![
                    object("data"),
                    prop("data.classic", &["integer"], false, false),
                    prop("data.mainline", &["integer"], false, false),
                ],
            ),
            (
                "prefix_items_are_numbered",
                json!({"type": "array", "prefixItems": [s, i], "items": false}),
                vec![
                    n("data", P, &[(fact::TYPES, list(&["array"]))]),
                    n(
                        "data[0]",
                        P,
                        &[
                            (fact::TYPES, list(&["string"])),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                    n(
                        "data[1]",
                        P,
                        &[
                            (fact::TYPES, list(&["integer"])),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "open_object_is_opaque_and_walked",
                json!({"type": "object", "properties": {"reason": s}, "additionalProperties": true}),
                vec![
                    n(
                        "data",
                        P,
                        &[
                            (fact::TYPES, list(&["object"])),
                            (fact::OPAQUE, Fact::Bool(true)),
                        ],
                    ),
                    prop("data.reason", &["string"], false, false),
                ],
            ),
            (
                "true_schema_is_opaque",
                json!({"type": "object", "properties": {"v": true}}),
                vec![
                    object("data"),
                    n(
                        "data.v",
                        P,
                        &[
                            (fact::OPAQUE, Fact::Bool(true)),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "empty_schema_is_opaque",
                json!({"type": "object", "properties": {"v": {}}}),
                vec![
                    object("data"),
                    n(
                        "data.v",
                        P,
                        &[
                            (fact::OPAQUE, Fact::Bool(true)),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "unmodelled_keywords_are_kept",
                json!({"type": "object", "properties": {"a": {"not": s}, "b": {"if": s, "then": i}}}),
                vec![
                    object("data"),
                    n(
                        "data.a",
                        P,
                        &[
                            (fact::KEYWORDS, list(&["not"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                    n(
                        "data.b",
                        P,
                        &[
                            (fact::KEYWORDS, list(&["if", "then"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "remote_ref_is_unmodelled",
                json!({"$ref": "https://example.invalid/x"}),
                vec![n(
                    "data",
                    P,
                    &[(fact::KEYWORDS, list(&["$ref:https://example.invalid/x"]))],
                )],
            ),
            (
                "false_schema_is_unmodelled",
                json!({"type": "object", "properties": {"v": false}}),
                vec![
                    object("data"),
                    n(
                        "data.v",
                        P,
                        &[
                            (fact::KEYWORDS, list(&["false"])),
                            (fact::REQUIRED, Fact::Bool(false)),
                            (fact::NULLABLE, Fact::Bool(false)),
                        ],
                    ),
                ],
            ),
            (
                "keys_with_special_characters_are_quoted",
                json!({"type": "object", "properties": {"a.b": s}}),
                vec![
                    object("data"),
                    prop("data.\"a.b\"", &["string"], false, false),
                ],
            ),
        ];
        for (case, schema, want) in cases {
            let got = nodes_of(schema.clone());
            let want = sorted(want);
            assert!(
                got == want,
                "{case}: schema {schema} gave {got:#?}, expected {want:#?}"
            );
        }
    }

    #[test]
    fn annotations_are_ignored() {
        let bare = json!({"type": "object", "properties": {"n": {"type": "integer", "minimum": 0}}, "required": ["n"]});
        let annotated = json!({
            "title": "T", "description": "d", "$comment": "c", "examples": [{}], "default": {},
            "type": "object", "properties": {"n": {"type": "integer", "minimum": 0, "format": "uint64",
            "deprecated": true, "readOnly": true, "writeOnly": false, "description": "n"}}, "required": ["n"]
        });
        assert_eq!(nodes_of(annotated), nodes_of(bare));
    }

    #[test]
    fn defs_names_do_not_matter() {
        let named = |name: &str| json!({"type": "object", "properties": {"s": {"$ref": format!("#/$defs/{name}")}}, "$defs": {name: x_def()}});
        assert_eq!(nodes_of(named("Summary")), nodes_of(named("Summary2")));
    }

    mod alpha {
        use super::*;

        #[derive(Serialize, JsonSchema)]
        pub struct Outer {
            /// The inner one
            pub inner: Inner,
        }

        #[derive(Serialize, JsonSchema)]
        #[schemars(rename = "Alpha")]
        pub struct Inner {
            pub count: u8,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub note: Option<String>,
        }
    }

    mod zeta {
        use super::*;

        #[derive(Serialize, JsonSchema)]
        pub struct Outer {
            pub inner: Inner,
        }

        /// Documented differently.
        #[derive(Serialize, JsonSchema)]
        #[schemars(rename = "Zeta")]
        pub struct Inner {
            /// How many
            pub count: u8,
            #[serde(skip_serializing_if = "Option::is_none")]
            pub note: Option<String>,
        }
    }

    fn nodes_for<T: JsonSchema>() -> Listed {
        nodes_of(schema::generator().into_root_schema_for::<T>().to_value())
    }

    #[test]
    fn renaming_a_type_and_doc_comments_change_nothing() {
        assert_eq!(nodes_for::<alpha::Outer>(), nodes_for::<zeta::Outer>());
    }

    #[test]
    fn skip_serializing_if_field_is_optional() {
        let nodes = nodes_for::<alpha::Outer>();
        let required = |position: &str| {
            nodes
                .iter()
                .find(|(id, _, _)| id == &format!("output t {position}"))
                .and_then(|(_, _, facts)| {
                    facts
                        .iter()
                        .find(|(k, _)| k == fact::REQUIRED)
                        .map(|(_, v)| v.clone())
                })
        };
        assert_eq!(
            required("data.inner.note"),
            Some(Fact::Bool(false)),
            "{nodes:#?}"
        );
        assert_eq!(
            required("data.inner.count"),
            Some(Fact::Bool(true)),
            "{nodes:#?}"
        );
    }

    /// The error union with its `$defs`, under the property `e` of an object root.
    fn holding_union(edit: impl FnOnce(&mut Vec<Value>)) -> Value {
        let mut union = schema::error_schema().to_value();
        let defs = union
            .as_object_mut()
            .and_then(|o| o.remove("$defs"))
            .unwrap_or(json!({}));
        let branches = union
            .get_mut("oneOf")
            .and_then(Value::as_array_mut)
            .unwrap();
        edit(branches);
        json!({"type": "object", "properties": {"e": union}, "required": ["e"], "$defs": defs})
    }

    #[test]
    fn error_union_is_referenced_and_a_differing_one_is_expanded() {
        let errors = ErrorSurfaces::new();
        let whole = nodes_with_errors(&holding_union(|_| {}), &errors);
        let e = &whole["output t data.e"];
        assert_eq!(
            e.facts.get(fact::UNION_OF),
            Some(&Fact::Text("error".into())),
            "{whole:#?}"
        );
        assert_eq!(whole.len(), 2, "nothing below the reference: {whole:#?}");

        let missing = nodes_with_errors(&holding_union(|branches| drop(branches.pop())), &errors);
        assert!(
            !missing["output t data.e"]
                .facts
                .contains_key(fact::UNION_OF)
        );
        assert!(
            missing
                .keys()
                .any(|id| id.starts_with("output t data.e<code=")),
            "a code missing is walked in full"
        );

        let extra = nodes_with_errors(
            &holding_union(|branches| {
                branches[0]["properties"]["extra"] = json!({"type": "string"});
            }),
            &errors,
        );
        assert!(!extra["output t data.e"].facts.contains_key(fact::UNION_OF));
        assert!(
            extra.keys().any(|id| id.ends_with(".extra")),
            "an extra field is walked in full"
        );
    }

    fn real() -> BTreeMap<String, Node> {
        let (manifest, conflicts) = build_checked(&Registry::builtin());
        assert_eq!(
            conflicts,
            Vec::<String>::new(),
            "build_checked's conflicts on the real contract"
        );
        manifest.nodes
    }

    /// Up to 50 ids, then the count of the rest.
    fn shown(ids: &[String]) -> String {
        let mut out: Vec<String> = ids.iter().take(50).cloned().collect();
        if ids.len() > 50 {
            out.push(format!("… and {} more", ids.len() - 50));
        }
        out.join("\n")
    }

    #[test]
    fn every_surface_has_its_root() {
        let nodes = real();
        let mut missing = Vec::new();
        let mut expect = |id: String| {
            if !nodes.contains_key(&id) {
                missing.push(id);
            }
        };
        for row in crate::app::COMMANDS {
            expect(ids::output(row.name, DATA));
        }
        for only in ENVELOPE_ONLY {
            expect(ids::output(only.name, DATA));
        }
        expect(ids::envelope(ROOT));
        for name in EventName::ALL {
            expect(ids::anchored(section::EVENT, name.wire(), Some(ROOT)));
        }
        for code in ErrorCode::ALL {
            expect(ids::anchored(section::ERROR, code.wire(), Some(ROOT)));
            expect(ids::anchored(section::ERROR, code.wire(), Some("details")));
        }
        for kind in KINDS.iter().map(|row| row.kind).chain(["journal"]) {
            expect(ids::anchored(section::FILE, kind, Some(ROOT)));
        }
        expect(ids::desired(ROOT));
        assert!(
            missing.is_empty(),
            "surfaces without their root:\n{}",
            shown(&missing)
        );
    }

    #[test]
    fn desired_state_is_recorded_as_input() {
        let nodes = real();
        let wrong: Vec<String> = nodes
            .iter()
            .filter(|(id, node)| {
                let desired = ids::section(id) == section::DESIRED;
                let input_kind = node.kind == NodeKind::DesiredPosition.wire()
                    || node.kind == NodeKind::DesiredVariant.wire();
                desired != input_kind || id.starts_with("file desired_state")
            })
            .map(|(id, _)| id.clone())
            .collect();
        assert!(
            wrong.is_empty(),
            "desired_* kinds outside `desired`, or the reverse:\n{}",
            shown(&wrong)
        );
        assert!(
            nodes
                .keys()
                .filter(|id| ids::section(id) == section::DESIRED)
                .count()
                > 1
        );
    }

    #[test]
    fn envelope_error_and_target_errors_reference_the_error_sections() {
        let nodes = real();
        let union_of = |id: &str| {
            nodes
                .get(id)
                .and_then(|node| node.facts.get(fact::UNION_OF))
                .cloned()
        };
        assert_eq!(union_of("envelope error"), Some(Fact::Text("error".into())));
        // A target's error is the union, or an item error whose details it keeps opaque.
        let wrong: Vec<String> = nodes
            .keys()
            .filter(|id| id.ends_with("targets[].error"))
            .filter(|id| {
                let referenced = union_of(id).is_some()
                    && !nodes
                        .keys()
                        .any(|other| other.starts_with(&format!("{id}.")));
                let item_error = nodes.get(&format!("{id}.details")).is_some_and(|details| {
                    details.facts.get(fact::OPAQUE) == Some(&Fact::Bool(true))
                });
                !(referenced || item_error)
            })
            .cloned()
            .collect();
        assert!(
            wrong.is_empty(),
            "targets[].error neither the union nor an item error:\n{}",
            shown(&wrong)
        );
        let referencing = nodes
            .keys()
            .filter(|id| id.ends_with("targets[].error") && union_of(id).is_some())
            .count();
        assert!(
            referencing >= 5,
            "multi-target commands reference the union: {referencing}"
        );
    }

    /// An id's `contract::OPAQUE` scope and A3 walk path: the surface, then the position
    /// without its root token and variant segments.
    fn opaque_address(id: &str) -> Option<(String, String)> {
        let (scope, position) = match ids::section(id) {
            section::OUTPUT => {
                let rest = id.strip_prefix("output ")?;
                let at = rest.find(" data")?;
                (format!("command {}", &rest[..at]), &rest[at + 1..])
            }
            section::ENVELOPE => ("envelope".to_owned(), id.strip_prefix("envelope ")?),
            section::EVENT => {
                let (name, position) = id.strip_prefix("event ")?.split_once(' ')?;
                (format!("event {name}"), position)
            }
            section::ERROR => (
                "error".to_owned(),
                id.strip_prefix("error ")?.split_once(' ')?.1,
            ),
            section::FILE => {
                let (kind, position) = id.strip_prefix("file ")?.split_once(' ')?;
                (format!("file {kind}"), position)
            }
            section::DESIRED => (
                "file desired_state".to_owned(),
                id.strip_prefix("desired ")?,
            ),
            _ => return None,
        };
        // Only a command's data has a root token of its own (`data`); elsewhere the root
        // is `.` and `data` is a key (`envelope data`).
        let data_root = ids::section(id) == section::OUTPUT;
        let mut path = String::new();
        for (i, segment) in ids::segments(position).into_iter().enumerate() {
            if segment.starts_with('<') || segment == ROOT || (data_root && i == 0) {
                continue;
            }
            let key = segment.strip_prefix('.').unwrap_or(segment);
            let key = if key.starts_with('"') {
                serde_json::from_str::<String>(key).ok()?
            } else {
                key.to_owned()
            };
            let joined = if key.starts_with('[') || key.starts_with('{') || path.is_empty() {
                key
            } else {
                format!(".{key}")
            };
            path.push_str(&joined);
        }
        Some((scope, path))
    }

    #[test]
    fn opaque_nodes_agree_with_contract_opaque() {
        let nodes = real();
        let opaque: Vec<(String, String, String)> = nodes
            .iter()
            .filter(|(_, node)| node.facts.get(fact::OPAQUE) == Some(&Fact::Bool(true)))
            .filter_map(|(id, _)| opaque_address(id).map(|(scope, path)| (id.clone(), scope, path)))
            .collect();
        let unlisted: Vec<String> = opaque
            .iter()
            .filter(|(_, scope, path)| !OPAQUE.iter().any(|row| row.matches(scope, path)))
            .map(|(id, scope, path)| format!("{id} ({scope} | {path})"))
            .collect();
        assert!(
            unlisted.is_empty(),
            "opaque nodes no OPAQUE row names:\n{}",
            shown(&unlisted)
        );
        // A row inside the error union is met through the reference: the union's own
        // `error` surface holds the open position.
        let references: Vec<(String, String)> = nodes
            .iter()
            .filter(|(_, node)| node.facts.contains_key(fact::UNION_OF))
            .filter_map(|(id, _)| opaque_address(id))
            .collect();
        let unmet: Vec<String> = OPAQUE
            .iter()
            .filter(|row| {
                let direct = opaque
                    .iter()
                    .any(|(_, scope, path)| row.matches(scope, path));
                let through = references.iter().any(|(scope, at)| {
                    let below = row
                        .path
                        .strip_prefix(at.as_str())
                        .and_then(|rest| rest.strip_prefix('.'));
                    row.matches(scope, &format!("{at}.{}", below.unwrap_or("")))
                        && below.is_some_and(|below| {
                            opaque.iter().any(|(_, s, p)| s == "error" && p == below)
                        })
                });
                !(direct || through)
            })
            .map(|row| format!("{} | {}", row.scope, row.path))
            .collect();
        assert!(
            unmet.is_empty(),
            "OPAQUE rows no opaque node meets:\n{}",
            shown(&unmet)
        );
    }

    #[test]
    fn real_contract_has_no_unmodelled_keyword() {
        let found: Vec<String> = real()
            .into_iter()
            .filter_map(|(id, node)| {
                node.facts
                    .get(fact::KEYWORDS)
                    .map(|k| format!("{id}: {k:?}"))
            })
            .collect();
        assert!(found.is_empty(), "unmodelled keywords:\n{}", shown(&found));
    }
}
