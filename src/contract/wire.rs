//! The contract's readable projection (M4-D56): one name per line, sorted, generated from
//! the semantic manifest alone, for a person to review as a diff. It enforces nothing: the
//! comparisons read the manifest.

use std::collections::BTreeMap;

use crate::contract::manifest::ids::{self, section};
use crate::contract::manifest::{Fact, Manifest, Node, NodeKind, fact};

/// Every name of `manifest`, one per line, sorted bytewise and deduplicated.
pub fn names(manifest: &Manifest) -> Vec<String> {
    let mut lines = Vec::new();
    for (id, node) in &manifest.nodes {
        let Some(kind) = node.kind() else {
            let facts: Vec<String> = node
                .facts
                .iter()
                .map(|(k, v)| format!("{k}={}", value(v)))
                .collect();
            lines.push(join(&[&format!("unknown {id}"), &facts.join(" ")]));
            continue;
        };
        let position = surface(id);
        if let Some((surface, path)) = &position
            && matches!(
                kind,
                NodeKind::OutputPosition | NodeKind::FilePosition | NodeKind::DesiredPosition
            )
        {
            if let Some(values) = node.facts.get(fact::ENUM)
                && printed_enum(id)
            {
                lines.push(format!("enum {surface} {path} {}", value(values)));
            }
            if node.facts.get(fact::OPAQUE) == Some(&Fact::Bool(true)) && printed_enum(id) {
                lines.push(format!("opaque {surface} {path}"));
            }
        }
        match kind {
            NodeKind::Command => lines.push(command(id, node)),
            NodeKind::Flag | NodeKind::Positional => lines.push(argument(id, node)),
            NodeKind::OutputPosition | NodeKind::FilePosition | NodeKind::DesiredPosition => {
                // Event and error positions print inside their one-line summaries.
                if !matches!(ids::section(id), section::EVENT | section::ERROR) {
                    lines.push(join(&[id, &shape(node)]));
                }
            }
            NodeKind::OutputVariant | NodeKind::FileVariant | NodeKind::DesiredVariant => {
                if !matches!(ids::section(id), section::EVENT | section::ERROR) {
                    lines.push(format!("{id} variant"));
                }
            }
            NodeKind::Event => lines.push(join(&[id, &summary(manifest, &format!("{id} "), "")])),
            NodeKind::ErrorCode => lines.push(error_code(manifest, id, node)),
            NodeKind::FileKind => lines.push(join(&[id, &text(node, fact::VERSION)])),
            NodeKind::ConfigKey => lines.push(join(&[id, &text(node, fact::CONFIG_TYPE)])),
            NodeKind::EnvVar => lines.push(join(&[id, &labelled(node, fact::MIRRORS, "mirrors")])),
            NodeKind::Action => lines.push(join(&[
                id,
                &labelled(node, fact::DESTRUCTIVE, "destructive"),
            ])),
            NodeKind::ExitCode => {
                let name = text(node, fact::NAME);
                lines.push(join(&[
                    id,
                    &name,
                    &labelled(node, fact::RETRYABLE, "retryable"),
                ]));
            }
            NodeKind::WarningCode
            | NodeKind::FailureMode
            | NodeKind::Phase
            | NodeKind::LogLevel
            | NodeKind::DoctorOp => lines.push(id.clone()),
        }
    }
    lines.sort();
    lines.dedup();
    lines
}

/// The surface of a position id and its path: the command path for command data,
/// `envelope`, `event:<name>`, `error:<code>`, `file:<kind>` or `desired`.
fn surface(id: &str) -> Option<(String, &str)> {
    let (head, rest) = id.split_once(' ')?;
    match head {
        section::OUTPUT => {
            let at = rest.match_indices(" data").map(|(at, _)| at).find(|at| {
                rest[at + 5..]
                    .chars()
                    .next()
                    .is_none_or(|c| matches!(c, '.' | '[' | '{' | '<'))
            })?;
            Some((rest[..at].to_owned(), &rest[at + 1..]))
        }
        section::ENVELOPE | section::DESIRED => Some((head.to_owned(), rest)),
        section::EVENT | section::ERROR | section::FILE => {
            let (name, path) = rest.split_once(' ')?;
            Some((format!("{head}:{name}"), path))
        }
        _ => None,
    }
}

/// Whether a position's `enum` and `opaque` lines print: every position but an error's
/// frame, which `error_variant` generates the same for every code.
fn printed_enum(id: &str) -> bool {
    ids::section(id) != section::ERROR || surface(id).is_some_and(|(_, path)| in_details(path))
}

fn in_details(path: &str) -> bool {
    path == "details"
        || ["details.", "details[", "details{", "details<"]
            .iter()
            .any(|p| path.starts_with(p))
}

/// Space-joined, empty parts left out.
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A fact's value as a line writes it: lists joined by `,`, line breaks escaped.
fn value(fact: &Fact) -> String {
    let raw = match fact {
        Fact::Bool(b) => b.to_string(),
        Fact::Int(n) => n.to_string(),
        Fact::Text(t) => t.clone(),
        Fact::List(items) => items.join(","),
        Fact::Ints(items) => items
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(","),
    };
    raw.replace('\n', "\\n").replace('\r', "\\r")
}

fn text(node: &Node, name: &str) -> String {
    node.facts.get(name).map(value).unwrap_or_default()
}

/// `<label>=<value>`, or nothing when the fact is absent.
fn labelled(node: &Node, name: &str, label: &str) -> String {
    node.facts
        .get(name)
        .map(|v| format!("{label}={}", value(v)))
        .unwrap_or_default()
}

fn yes(node: &Node, name: &str) -> bool {
    node.facts.get(name) == Some(&Fact::Bool(true))
}

fn command(id: &str, node: &Node) -> String {
    let mut parts = vec![id.to_owned()];
    for name in [
        fact::ALIASES,
        fact::EXITS,
        fact::DICTIONARY,
        fact::MUTATES,
        fact::BINDS,
        fact::STORES_PLAN,
        fact::NETWORK,
    ] {
        parts.push(labelled(node, name, name));
    }
    for flag in [
        fact::SUBCOMMAND_REQUIRED,
        fact::ARGS_CONFLICT_WITH_SUBCOMMANDS,
        fact::ENVELOPE_ONLY,
    ] {
        if yes(node, flag) {
            parts.push(flag.to_owned());
        }
    }
    join(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The switches an argument line names, in order.
const SWITCHES: &[&str] = &[
    fact::ALLOW_HYPHEN_VALUES,
    fact::ALLOW_NEGATIVE_NUMBERS,
    fact::TRAILING_VAR_ARG,
    fact::REQUIRE_EQUALS,
    fact::EXCLUSIVE,
    fact::LAST,
];

fn argument(id: &str, node: &Node) -> String {
    let mut parts = vec![id.to_owned()];
    if let Some(name) = node.facts.get(fact::VALUE_NAME) {
        parts.push(format!("<{}>", value(name)));
    }
    let values = node
        .facts
        .get(fact::VALUES)
        .map(|v| format!("({})", value(v).replace(',', "|")))
        .unwrap_or_default();
    parts.push(format!("{}{values}", text(node, fact::GRAMMAR)));
    parts.push(format!("default={}", text(node, fact::DEFAULT)));
    parts.push(format!(
        "arity={}..{}",
        text(node, fact::ARITY_MIN),
        text(node, fact::ARITY_MAX)
    ));
    for flag in [
        fact::REPEAT,
        fact::REQUIRED,
        fact::GLOBAL,
        fact::IGNORE_CASE,
    ] {
        if yes(node, flag) {
            parts.push(flag.to_owned());
        }
    }
    parts.push(labelled(node, fact::SHORT, "short"));
    for name in [
        fact::ALIASES,
        fact::SHORT_ALIASES,
        fact::CONFLICTS,
        fact::REQUIRES,
    ] {
        parts.push(labelled(node, name, name));
    }
    for switch in SWITCHES {
        if yes(node, switch) {
            parts.push((*switch).to_owned());
        }
    }
    parts.push(labelled(node, fact::ENV, "env"));
    parts.push(labelled(node, fact::VALUE_DELIMITER, "delimiter"));
    join(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A position's bounds, in order.
const BOUNDS: &[&str] = &[
    fact::MINIMUM,
    fact::EXCLUSIVE_MINIMUM,
    fact::MAXIMUM,
    fact::EXCLUSIVE_MAXIMUM,
    fact::MULTIPLE_OF,
    fact::MIN_ITEMS,
    fact::MAX_ITEMS,
    fact::MIN_LENGTH,
    fact::MAX_LENGTH,
    fact::MIN_PROPERTIES,
    fact::MAX_PROPERTIES,
];

/// `<types>[?][!]`: `|`-joined, `any` when unconstrained, `?` nullable, `!` required.
fn types(node: &Node) -> String {
    let named = node
        .facts
        .get(fact::TYPES)
        .map_or_else(|| "any".to_owned(), |v| value(v).replace(',', "|"));
    let nullable = if yes(node, fact::NULLABLE) { "?" } else { "" };
    let required = if yes(node, fact::REQUIRED) { "!" } else { "" };
    format!("{named}{nullable}{required}")
}

/// A position's shape: its types, its bounds, its tag and what it refers to.
fn shape(node: &Node) -> String {
    let mut parts = vec![types(node)];
    for name in BOUNDS {
        parts.push(labelled(node, name, name));
    }
    parts.push(labelled(node, fact::TAG, "tag"));
    let reference = node
        .facts
        .get(fact::SAME_AS)
        .or_else(|| node.facts.get(fact::UNION_OF));
    parts.push(
        reference
            .map(|r| format!("ref={}", value(r)))
            .unwrap_or_default(),
    );
    join(&parts.iter().map(String::as_str).collect::<Vec<_>>())
}

/// `path:types,…` of every position under `prefix` whose path `starts` with `under`.
fn summary(manifest: &Manifest, prefix: &str, under: &str) -> String {
    let below: BTreeMap<&str, &Node> = manifest
        .nodes
        .range(prefix.to_owned()..)
        .take_while(|(id, _)| id.starts_with(prefix))
        .filter_map(|(id, node)| Some((id.strip_prefix(prefix)?, node)))
        .filter(|(path, _)| !path.contains(' ') && (under.is_empty() || in_details(path)))
        .filter(|(_, node)| node.kind() != Some(NodeKind::OutputVariant))
        .collect();
    below
        .iter()
        .map(|(path, node)| format!("{path}:{}", types(node)))
        .collect::<Vec<_>>()
        .join(",")
}

fn error_code(manifest: &Manifest, id: &str, node: &Node) -> String {
    let details = summary(manifest, &format!("{id} "), "details");
    join(&[
        id,
        &labelled(node, fact::EXIT, "exit"),
        &labelled(node, fact::DOCTOR_EXIT, "doctor_exit"),
        &labelled(node, fact::RETRYABLE, "retryable"),
        &format!("details={details}"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::manifest::FORMAT;

    fn node(kind: NodeKind, facts: &[(&str, Fact)]) -> Node {
        Node {
            kind: kind.wire().to_owned(),
            facts: facts
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        }
    }

    fn man(nodes: Vec<(&str, Node)>) -> Manifest {
        Manifest {
            format: FORMAT,
            nodes: nodes
                .into_iter()
                .map(|(id, n)| (id.to_owned(), n))
                .collect(),
        }
    }

    fn l(items: &[&str]) -> Fact {
        Fact::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    fn t(s: &str) -> Fact {
        Fact::Text(s.to_owned())
    }

    fn yes_fact() -> Fact {
        Fact::Bool(true)
    }

    fn sample() -> Manifest {
        use NodeKind as K;
        man(vec![
            (
                "command install",
                node(
                    K::Command,
                    &[
                        (fact::ALIASES, l(&["add"])),
                        (fact::EXITS, Fact::Ints(vec![0, 1, 2])),
                        (fact::DICTIONARY, t("general")),
                        (fact::MUTATES, yes_fact()),
                        (fact::BINDS, Fact::Bool(false)),
                        (fact::STORES_PLAN, yes_fact()),
                        (fact::NETWORK, t("optional")),
                        (fact::SUBCOMMAND_REQUIRED, yes_fact()),
                    ],
                ),
            ),
            ("command auth", node(K::Command, &[])),
            (
                "input install --channel",
                node(
                    K::Flag,
                    &[
                        (fact::GRAMMAR, t("enum")),
                        (fact::VALUES, l(&["alpha", "beta"])),
                        (fact::DEFAULT, t("stable")),
                        (fact::ARITY_MIN, Fact::Int(1)),
                        (fact::ARITY_MAX, Fact::Int(1)),
                        (fact::REPEAT, yes_fact()),
                        (fact::REQUIRED, Fact::Bool(false)),
                        (fact::SHORT, t("-c")),
                        (fact::ALIASES, l(&["chan"])),
                        (fact::CONFLICTS, l(&["--x"])),
                        (fact::ALLOW_HYPHEN_VALUES, yes_fact()),
                        (fact::ENV, t("CHAN")),
                        (fact::VALUE_DELIMITER, t(",")),
                    ],
                ),
            ),
            (
                "input install @1",
                node(
                    K::Positional,
                    &[
                        (fact::VALUE_NAME, t("REF")),
                        (fact::GRAMMAR, t("string")),
                        (fact::ARITY_MIN, Fact::Int(1)),
                        (fact::REQUIRED, yes_fact()),
                    ],
                ),
            ),
            (
                "output install data.size",
                node(
                    K::OutputPosition,
                    &[
                        (fact::TYPES, l(&["integer"])),
                        (fact::NULLABLE, yes_fact()),
                        (fact::REQUIRED, yes_fact()),
                        (fact::MINIMUM, Fact::Int(0)),
                        (fact::MAXIMUM, Fact::Int(9)),
                        (fact::TAG, t("type")),
                    ],
                ),
            ),
            ("output install data<type=a>", node(K::OutputVariant, &[])),
            (
                "output install data.mode",
                node(
                    K::OutputPosition,
                    &[(fact::TYPES, l(&["string"])), (fact::ENUM, l(&["a", "b"]))],
                ),
            ),
            (
                "output install data.any",
                node(K::OutputPosition, &[(fact::OPAQUE, yes_fact())]),
            ),
            (
                "envelope error",
                node(
                    K::OutputPosition,
                    &[
                        (fact::TYPES, l(&["object"])),
                        (fact::REQUIRED, yes_fact()),
                        (fact::UNION_OF, t("error")),
                    ],
                ),
            ),
            ("event progress", node(K::Event, &[])),
            (
                "event progress .",
                node(K::OutputPosition, &[(fact::TYPES, l(&["object"]))]),
            ),
            (
                "event progress phase",
                node(
                    K::OutputPosition,
                    &[
                        (fact::TYPES, l(&["string"])),
                        (fact::REQUIRED, yes_fact()),
                        (fact::ENUM, l(&["scan"])),
                    ],
                ),
            ),
            (
                "event progress total",
                node(
                    K::OutputPosition,
                    &[(fact::TYPES, l(&["integer"])), (fact::NULLABLE, yes_fact())],
                ),
            ),
            (
                "error locked",
                node(
                    K::ErrorCode,
                    &[
                        (fact::EXIT, Fact::Int(5)),
                        (fact::DOCTOR_EXIT, Fact::Int(4)),
                        (fact::RETRYABLE, t("true")),
                    ],
                ),
            ),
            (
                "error locked .",
                node(K::OutputPosition, &[(fact::TYPES, l(&["object"]))]),
            ),
            (
                "error locked code",
                node(
                    K::OutputPosition,
                    &[(fact::TYPES, l(&["string"])), (fact::ENUM, l(&["locked"]))],
                ),
            ),
            (
                "error locked details",
                node(
                    K::OutputPosition,
                    &[(fact::TYPES, l(&["object"])), (fact::REQUIRED, yes_fact())],
                ),
            ),
            (
                "error locked details.holder",
                node(
                    K::OutputPosition,
                    &[(fact::TYPES, l(&["string"])), (fact::ENUM, l(&["x"]))],
                ),
            ),
            (
                "file journal",
                node(K::FileKind, &[(fact::VERSION, t("1.1"))]),
            ),
            (
                "file journal .",
                node(K::FilePosition, &[(fact::TYPES, l(&["object"]))]),
            ),
            ("file journal <type=step>", node(K::FileVariant, &[])),
            (
                "config default_channel",
                node(K::ConfigKey, &[(fact::CONFIG_TYPE, t("channel"))]),
            ),
            (
                "desired family",
                node(
                    K::DesiredPosition,
                    &[
                        (fact::TYPES, l(&["object"])),
                        (fact::REQUIRED, Fact::Bool(false)),
                    ],
                ),
            ),
            (
                "env OMADDONS_OFFLINE",
                node(K::EnvVar, &[(fact::MIRRORS, t("--offline"))]),
            ),
            ("warning stale_check", node(K::WarningCode, &[])),
            (
                "action apply",
                node(K::Action, &[(fact::DESTRUCTIVE, Fact::Bool(false))]),
            ),
            ("fm FM-1", node(K::FailureMode, &[])),
            ("phase scan", node(K::Phase, &[])),
            ("log-level debug", node(K::LogLevel, &[])),
            (
                "exit general 5",
                node(
                    K::ExitCode,
                    &[(fact::NAME, t("locked")), (fact::RETRYABLE, t("true"))],
                ),
            ),
            (
                "exit doctor 4",
                node(K::ExitCode, &[(fact::NAME, t("findings"))]),
            ),
            ("doctor-ops gc", node(K::DoctorOp, &[])),
            (
                "x y",
                Node {
                    kind: "frobnicate".into(),
                    facts: [("zeal".to_owned(), yes_fact())].into(),
                },
            ),
        ])
    }

    #[test]
    fn names_formats_each_node_kind() {
        let want = [
            "action apply destructive=false",
            "command auth",
            "command install aliases=add exits=0,1,2 dictionary=general mutates=true binds=false stores_plan=true network=optional subcommand_required",
            "config default_channel channel",
            "desired family object",
            "doctor-ops gc",
            "enum error:locked details.holder x",
            "enum event:progress phase scan",
            "enum install data.mode a,b",
            "env OMADDONS_OFFLINE mirrors=--offline",
            "envelope error object! ref=error",
            "error locked exit=5 doctor_exit=4 retryable=true details=details:object!,details.holder:string",
            "event progress .:object,phase:string!,total:integer?",
            "exit doctor 4 findings",
            "exit general 5 locked retryable=true",
            "file journal . object",
            "file journal 1.1",
            "file journal <type=step> variant",
            "fm FM-1",
            "input install --channel enum(alpha|beta) default=stable arity=1..1 repeat short=-c aliases=chan conflicts=--x allow_hyphen_values env=CHAN delimiter=,",
            "input install @1 <REF> string default= arity=1.. required",
            "log-level debug",
            "opaque install data.any",
            "output install data.any any",
            "output install data.mode string",
            "output install data.size integer?! minimum=0 maximum=9 tag=type",
            "output install data<type=a> variant",
            "phase scan",
            "unknown x y zeal=true",
            "warning stale_check",
        ];
        let got = names(&sample());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "line {i}: got {g:?}, expected {w:?}");
        }
        assert_eq!(got.len(), want.len(), "{got:#?}");
    }

    #[test]
    fn names_are_sorted_unique_and_deterministic() {
        let a = sample();
        let mut nodes: Vec<(String, Node)> = a.nodes.clone().into_iter().collect();
        nodes.reverse();
        let b = Manifest {
            format: FORMAT,
            nodes: nodes.into_iter().collect(),
        };
        let (x, y) = (names(&a), names(&b));
        assert_eq!(x, y);
        let mut sorted = x.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(x, sorted, "sorted and without duplicates");
    }

    #[test]
    fn names_escape_line_breaks() {
        let m = man(vec![(
            "config k",
            node(NodeKind::ConfigKey, &[(fact::CONFIG_TYPE, t("a\nb"))]),
        )]);
        assert_eq!(names(&m), ["config k a\\nb"]);
    }

    #[test]
    fn error_frame_positions_are_not_printed_per_code() {
        let m = man(vec![
            (
                "error x",
                node(
                    NodeKind::ErrorCode,
                    &[(fact::EXIT, Fact::Int(1)), (fact::RETRYABLE, t("false"))],
                ),
            ),
            (
                "error x .",
                node(NodeKind::OutputPosition, &[(fact::TYPES, l(&["object"]))]),
            ),
            (
                "error x code",
                node(
                    NodeKind::OutputPosition,
                    &[(fact::TYPES, l(&["string"])), (fact::ENUM, l(&["x"]))],
                ),
            ),
            (
                "error x message",
                node(NodeKind::OutputPosition, &[(fact::TYPES, l(&["string"]))]),
            ),
            (
                "error x details",
                node(NodeKind::OutputPosition, &[(fact::TYPES, l(&["object"]))]),
            ),
        ]);
        assert_eq!(
            names(&m),
            ["error x exit=1 retryable=false details=details:object"]
        );
    }

    #[test]
    fn the_real_manifest_projects_one_line_per_name() {
        let manifest = crate::contract::manifest::build(&crate::contract::Registry::builtin());
        let lines = names(&manifest);
        assert!(lines.len() > 1000, "{} lines", lines.len());
        assert!(
            lines.iter().all(|line| !line.contains('\n')),
            "one name per line"
        );
        // The envelope's error is optional and null on success.
        assert!(
            lines
                .iter()
                .any(|line| line == "envelope error object? ref=error"),
            "the envelope's error reference"
        );
    }
}
