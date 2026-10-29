//! Locating an observed JSON instance in the manifest: every path of the instance that has
//! no node. It checks paths, never values (the schema validation checks those), so it
//! proves the manifest covers what the binary actually prints.

use serde_json::Value;

use super::ids::{self, ROOT, section};
use super::{Fact, Manifest, fact};

/// Every path of `instance`, observed at the position node `anchor` (`output install
/// data`, `envelope .`, `event progress .`), that no node covers: sorted, deduplicated, in
/// the manifest's id syntax. An anchor that is no node is itself the answer.
pub fn unmatched(manifest: &Manifest, anchor: &str, instance: &Value) -> Vec<String> {
    let Some((prefix, position)) = split(anchor).filter(|_| manifest.nodes.contains_key(anchor))
    else {
        return vec![anchor.to_owned()];
    };
    let mut out = Vec::new();
    Locator { manifest }.walk(&prefix, &position, instance, &[], &mut out);
    out.sort();
    out.dedup();
    out
}

/// A position id's surface prefix and position: `output backup restore` and `data.x`.
fn split(id: &str) -> Option<(String, String)> {
    let (head, rest) = id.split_once(' ')?;
    match head {
        section::OUTPUT => {
            let at = rest.match_indices(" data").map(|(at, _)| at).find(|at| {
                rest[at + 5..]
                    .chars()
                    .next()
                    .is_none_or(|c| matches!(c, '.' | '[' | '{' | '<'))
            })?;
            Some((format!("{head} {}", &rest[..at]), rest[at + 1..].to_owned()))
        }
        section::ENVELOPE | section::DESIRED => Some((head.to_owned(), rest.to_owned())),
        section::EVENT | section::ERROR | section::FILE => {
            let (name, position) = rest.split_once(' ')?;
            Some((format!("{head} {name}"), position.to_owned()))
        }
        _ => None,
    }
}

/// `position<tag=?>`: a union position whose instance lacks its tag.
fn missing(position: &str, tag: &str) -> String {
    let segment = format!("<{tag}=?>");
    if position == ROOT {
        segment
    } else {
        format!("{position}{segment}")
    }
}

/// A position an instance's keys are looked up under, and the property path its tag
/// consumes (empty when none).
type Scope = (String, Vec<String>);

struct Locator<'m> {
    manifest: &'m Manifest,
}

impl Locator<'_> {
    fn exists(&self, prefix: &str, position: &str) -> bool {
        self.manifest
            .nodes
            .contains_key(&format!("{prefix} {position}"))
    }

    /// Matches `value` at `position`, leaving out the property at `skip` (a variant's tag).
    fn walk(
        &self,
        prefix: &str,
        position: &str,
        value: &Value,
        skip: &[String],
        out: &mut Vec<String>,
    ) {
        let id = format!("{prefix} {position}");
        let Some(node) = self.manifest.nodes.get(&id) else {
            out.push(id);
            return;
        };
        if node.facts.get(fact::OPAQUE) == Some(&Fact::Bool(true)) {
            return;
        }
        if let Some(Fact::Text(target)) = node.facts.get(fact::SAME_AS) {
            match split(target) {
                Some((p, at)) => self.walk(&p, &at, value, skip, out),
                None => out.push(target.clone()),
            }
            return;
        }
        if node.facts.contains_key(fact::UNION_OF) {
            match value.get("code").and_then(Value::as_str) {
                Some(code) if self.exists(&format!("{} {code}", section::ERROR), ROOT) => {
                    self.walk(&format!("{} {code}", section::ERROR), ROOT, value, &[], out);
                }
                Some(code) => out.push(format!(
                    "{prefix} {}",
                    ids::push_variant(position, "code", code)
                )),
                None => out.push(format!("{prefix} {}", missing(position, "code"))),
            }
            return;
        }
        let mut scopes: Vec<Scope> = vec![(position.to_owned(), skip.to_vec())];
        if let Some(Fact::Text(tags)) = node.facts.get(fact::TAG) {
            for tag in tags.split(',') {
                let parts: Vec<&str> = tag.split('.').collect();
                let selected = parts
                    .iter()
                    .try_fold(value, |v, part| v.get(part))
                    .and_then(Value::as_str);
                let Some(selected) = selected else {
                    out.push(format!("{prefix} {}", missing(position, tag)));
                    return;
                };
                let variant = ids::push_variant_at(position, &parts, selected);
                if !self.exists(prefix, &variant) {
                    out.push(format!("{prefix} {variant}"));
                    return;
                }
                scopes.push((variant, parts.iter().map(|p| (*p).to_owned()).collect()));
            }
        }
        let untagged = self.untagged(prefix, position);
        if !untagged.is_empty() {
            let mut best: Option<Vec<String>> = None;
            let mut chosen = None;
            for variant in untagged {
                let mut trial = Vec::new();
                let mut with: Vec<Scope> = scopes.clone();
                with.push((variant.clone(), Vec::new()));
                self.match_value(prefix, &with, value, &mut trial);
                if trial.is_empty() {
                    chosen = Some(variant);
                    break;
                }
                if best.as_ref().is_none_or(|b| trial.len() < b.len()) {
                    best = Some(trial);
                }
            }
            match chosen {
                Some(variant) => scopes.push((variant, Vec::new())),
                None => {
                    out.extend(best.unwrap_or_default());
                    return;
                }
            }
        }
        self.match_value(prefix, &scopes, value, out);
    }

    /// The untagged variants directly under `position`, in id order.
    fn untagged(&self, prefix: &str, position: &str) -> Vec<String> {
        let start = format!("{prefix} {}", ids::push_untagged(position, &[], &[]));
        let start = start.strip_suffix(">").unwrap_or(&start).to_owned();
        self.manifest
            .nodes
            .range(start.clone()..)
            .take_while(|(id, _)| id.starts_with(&start))
            .filter_map(|(id, _)| id.strip_prefix(prefix)?.strip_prefix(' '))
            .filter(|at| {
                ids::segments(at).len()
                    == ids::segments(position).len() + usize::from(position != ROOT)
            })
            .map(str::to_owned)
            .collect()
    }

    /// Matches `value`'s keys or elements under any of `scopes`.
    fn match_value(&self, prefix: &str, scopes: &[Scope], value: &Value, out: &mut Vec<String>) {
        let first = scopes.first().map_or(ROOT, |(scope, _)| scope.as_str());
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    // A tag is consumed by the variant it selected.
                    if scopes
                        .iter()
                        .any(|(_, skip)| skip.len() == 1 && skip[0] == *key)
                    {
                        continue;
                    }
                    let child = scopes.iter().find_map(|(scope, skip)| {
                        let at = ids::push_key(scope, key);
                        let below = match skip.split_first() {
                            Some((head, rest)) if head == key => rest.to_vec(),
                            _ => Vec::new(),
                        };
                        self.exists(prefix, &at).then_some((at, below))
                    });
                    let child = child.or_else(|| {
                        scopes.iter().find_map(|(scope, _)| {
                            let at = ids::push_map(scope);
                            self.exists(prefix, &at).then_some((at, Vec::new()))
                        })
                    });
                    match child {
                        Some((at, below)) => self.walk(prefix, &at, item, &below, out),
                        None => out.push(format!("{prefix} {}", ids::push_key(first, key))),
                    }
                }
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    let child = scopes
                        .iter()
                        .map(|(scope, _)| ids::push_index(scope, i))
                        .chain(scopes.iter().map(|(scope, _)| ids::push_items(scope)))
                        .find(|at| self.exists(prefix, at));
                    match child {
                        Some(at) => self.walk(prefix, &at, item, &[], out),
                        None => out.push(format!("{prefix} {}", ids::push_items(first))),
                    }
                }
            }
            // Values are not checked: a scalar or null matches any position.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::contract::manifest::{FORMAT, Node};

    /// A manifest of `ids`, each an output position with `facts` where given.
    fn man(nodes: &[(&str, &[(&str, Fact)])]) -> Manifest {
        Manifest {
            format: FORMAT,
            nodes: nodes
                .iter()
                .map(|(id, facts)| {
                    let kind = if id.contains("<") && !id.contains('.') && id.ends_with('>') {
                        "output_variant"
                    } else {
                        "output_position"
                    };
                    let node = Node {
                        kind: kind.into(),
                        facts: facts
                            .iter()
                            .map(|(k, v)| ((*k).to_owned(), v.clone()))
                            .collect(),
                    };
                    ((*id).to_owned(), node)
                })
                .collect(),
        }
    }

    fn t(s: &str) -> Fact {
        Fact::Text(s.to_owned())
    }

    /// A case's name, its nodes with their facts, the anchor, the instance and the
    /// unmatched paths it expects.
    type Case<'a> = (
        &'static str,
        Vec<(&'static str, &'a [(&'static str, Fact)])>,
        &'static str,
        Value,
        Vec<&'static str>,
    );

    #[test]
    fn unmatched_cases() {
        let tagged = [
            ("output t data", &[(fact::TAG, t("type"))][..]),
            ("output t data<type=a>", &[]),
            ("output t data<type=a>.x", &[]),
        ];
        let help = [
            ("output t data", &[][..]),
            ("output t data<untagged=command,usage>", &[]),
            ("output t data<untagged=command,usage>.command", &[]),
            ("output t data<untagged=command,usage>.usage", &[]),
            ("output t data<untagged=version>", &[]),
            ("output t data<untagged=version>.version", &[]),
        ];
        let errors = [
            ("envelope .", &[][..]),
            ("envelope error", &[(fact::UNION_OF, t("error"))]),
            ("error plan_stale .", &[]),
            ("error plan_stale code", &[]),
            ("error plan_stale details", &[]),
            ("error plan_stale details.changed", &[]),
            ("error plan_stale details.changed[]", &[]),
        ];
        let nested_tag = [(fact::TAG, t("applied.command"))];
        let recursive = [(fact::SAME_AS, t("output t data.x"))];
        let integer = [(fact::TYPES, Fact::List(vec!["integer".into()]))];
        let cases: Vec<Case<'_>> = vec![
            (
                "object_keys",
                vec![("output t data", &[]), ("output t data.a", &[])],
                "output t data",
                json!({"a": 1, "b": 2}),
                vec!["output t data.b"],
            ),
            (
                "nested_object",
                vec![("output t data", &[]), ("output t data.a", &[])],
                "output t data",
                json!({"a": {"x": 1}}),
                vec!["output t data.a.x"],
            ),
            (
                "array_items",
                vec![
                    ("output t data", &[]),
                    ("output t data.a", &[]),
                    ("output t data.a[]", &[]),
                    ("output t data.a[].x", &[]),
                ],
                "output t data",
                json!({"a": [{"x": 1}]}),
                vec![],
            ),
            (
                "tuple_positions",
                vec![
                    ("output t data", &[]),
                    ("output t data[0]", &[]),
                    ("output t data[1]", &[]),
                ],
                "output t data",
                json!([1, "s"]),
                vec![],
            ),
            (
                "map_values",
                vec![
                    ("output t data", &[]),
                    ("output t data.m", &[]),
                    ("output t data.m{}", &[]),
                    ("output t data.m{}.y", &[]),
                ],
                "output t data",
                json!({"m": {"k1": {"y": 1}}}),
                vec![],
            ),
            (
                "quoted_key",
                vec![("output t data", &[]), ("output t data.\"a.b\"", &[])],
                "output t data",
                json!({"a.b": 1}),
                vec![],
            ),
            (
                "tagged_variant",
                tagged.to_vec(),
                "output t data",
                json!({"type": "a", "x": 1}),
                vec![],
            ),
            (
                "nested_tag_variant",
                vec![
                    ("output t data", &nested_tag),
                    ("output t data<applied.command=install>", &[]),
                    ("output t data<applied.command=install>.applied", &[]),
                    ("output t data<applied.command=install>.result", &[]),
                ],
                "output t data",
                json!({"applied": {"command": "install"}, "result": {}}),
                vec![],
            ),
            (
                "unknown_tag_value",
                tagged.to_vec(),
                "output t data",
                json!({"type": "z"}),
                vec!["output t data<type=z>"],
            ),
            (
                "missing_tag",
                tagged.to_vec(),
                "output t data",
                json!({"x": 1}),
                vec!["output t data<type=?>"],
            ),
            (
                "untagged_variant",
                help.to_vec(),
                "output t data",
                json!({"version": "1.0"}),
                vec![],
            ),
            (
                "untagged_no_match",
                help.to_vec(),
                "output t data",
                json!({"command": "x", "extra": 1}),
                vec!["output t data.extra"],
            ),
            (
                "opaque_subtree",
                vec![
                    ("output t data", &[]),
                    ("output t data.settings", &[]),
                    ("output t data.settings[]", &[]),
                    (
                        "output t data.settings[].value",
                        &[(fact::OPAQUE, Fact::Bool(true))],
                    ),
                ],
                "output t data",
                json!({"settings": [{"value": {"any": [1]}}]}),
                vec![],
            ),
            (
                "error_union_jumps_to_code",
                errors.to_vec(),
                "envelope .",
                json!({"error": {"code": "plan_stale", "details": {"changed": ["selection"]}}}),
                vec![],
            ),
            (
                "error_union_unknown_code",
                errors.to_vec(),
                "envelope .",
                json!({"error": {"code": "nope"}}),
                vec!["envelope error<code=nope>"],
            ),
            (
                "same_as_recursion",
                vec![
                    ("output t data", &[]),
                    ("output t data.x", &[]),
                    ("output t data.x.child", &recursive),
                ],
                "output t data",
                json!({"x": {"child": {"child": {}}}}),
                vec![],
            ),
            (
                "missing_anchor",
                vec![("output t data", &[])],
                "output nope data",
                json!({}),
                vec!["output nope data"],
            ),
            (
                "values_are_not_checked",
                vec![("output t data", &[]), ("output t data.n", &integer)],
                "output t data",
                json!({"n": "text"}),
                vec![],
            ),
        ];
        for (case, nodes, anchor, instance, want) in cases {
            let got = unmatched(&man(&nodes), anchor, &instance);
            assert_eq!(
                got, want,
                "{case}: input {instance} gave {got:?}, expected {want:?}"
            );
        }
    }

    #[test]
    fn real_output_is_located() {
        let manifest = crate::contract::manifest::build(&crate::contract::Registry::builtin());
        let help = json!({"version": "0.1.0"});
        assert_eq!(
            unmatched(&manifest, "output help data", &help),
            Vec::<String>::new()
        );
        let envelope = json!({"ok": false, "command": "x", "error": {"code": "locked", "message": "m", "fix": "f", "fix_argv": null, "alternatives": [], "retryable": true, "details": {}}});
        assert_eq!(
            unmatched(&manifest, "envelope .", &envelope),
            Vec::<String>::new()
        );
    }
}
