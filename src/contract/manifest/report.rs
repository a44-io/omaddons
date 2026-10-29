//! The manifest's report: its section counts and the classifier's verdict against a
//! baseline, so every leaf sees its own semantic diff in review. Pure: no clock, no file,
//! no environment.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::ids::{self, DATA, section};
use super::{Bump, Fact, Manifest, NodeKind, classify, fact};

/// Nodes per section word (`unknown` for a kind this build does not know), and the derived
/// counts `commands`, `commands with data`, `flags`, `positionals`, `error codes`, `events`,
/// `file kinds`, `config keys`, `env vars` and `opaque positions`.
pub fn section_counts(manifest: &Manifest) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |key: &str| *counts.entry(key.to_owned()).or_default() += 1;
    for (id, node) in &manifest.nodes {
        let Some(kind) = node.kind() else {
            add("unknown");
            continue;
        };
        add(ids::section(id));
        let derived = match kind {
            NodeKind::Command => Some("commands"),
            NodeKind::Flag => Some("flags"),
            NodeKind::Positional => Some("positionals"),
            NodeKind::ErrorCode => Some("error codes"),
            NodeKind::Event => Some("events"),
            NodeKind::FileKind => Some("file kinds"),
            NodeKind::ConfigKey => Some("config keys"),
            NodeKind::EnvVar => Some("env vars"),
            _ => None,
        };
        if let Some(key) = derived {
            add(key);
        }
        if ids::section(id) == section::OUTPUT && id.ends_with(&format!(" {DATA}")) {
            add("commands with data");
        }
        if node.facts.get(fact::OPAQUE) == Some(&Fact::Bool(true)) {
            add("opaque positions");
        }
    }
    counts
}

/// `n` with thousands separators: `5,231`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn bump_name(bump: Bump) -> &'static str {
    match bump {
        Bump::None => "none",
        Bump::Minor => "minor",
        Bump::Major => "major",
    }
}

/// The section counts, then the verdict against `baseline` (the empty manifest when
/// none): the change counts and the wire bump, each unsatisfied persisted-kind bump, and the
/// change list, or `no change`.
pub fn report(current: &Manifest, baseline: Option<&Manifest>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "semantic manifest, format {}: {} nodes",
        current.format,
        thousands(current.nodes.len())
    );
    for (name, count) in section_counts(current) {
        let _ = writeln!(out, "  {name:<20} {}", thousands(count));
    }
    let empty = Manifest::empty();
    let (against, base) = match baseline {
        Some(base) => ("the given baseline", base),
        None => ("an empty baseline", &empty),
    };
    let changes = classify(base, current);
    if changes.is_empty() {
        let _ = writeln!(out, "against {against}: no change");
        return out;
    }
    let (additive, breaking, unclassified) = changes.counts();
    let _ = writeln!(
        out,
        "against {against}: {} changes ({} additive, {} breaking, {} unclassified); wire bump: {}",
        thousands(changes.changes.len()),
        thousands(additive),
        thousands(breaking),
        thousands(unclassified),
        bump_name(changes.wire_bump())
    );
    for kind in changes.unsatisfied_kinds() {
        let _ = writeln!(
            out,
            "  persisted file {} needs a {} bump ({} -> {})",
            kind.kind,
            bump_name(kind.required),
            kind.baseline.as_deref().unwrap_or("none"),
            kind.current.as_deref().unwrap_or("none")
        );
    }
    if baseline.is_some() {
        out.push_str(&changes.render());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::manifest::{FORMAT, Node};

    fn node(kind: &str, facts: &[(&str, Fact)]) -> Node {
        Node {
            kind: kind.to_owned(),
            facts: facts
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        }
    }

    fn man(nodes: &[(&str, Node)]) -> Manifest {
        Manifest {
            format: FORMAT,
            nodes: nodes
                .iter()
                .map(|(id, n)| ((*id).to_owned(), n.clone()))
                .collect(),
        }
    }

    #[test]
    fn section_counts_of_a_synthetic_manifest() {
        let m = man(&[
            ("command a", node("command", &[])),
            ("command b", node("command", &[])),
            ("output a data", node("output_position", &[])),
            (
                "output a data.x",
                node("output_position", &[(fact::OPAQUE, Fact::Bool(true))]),
            ),
            ("input a --x", node("flag", &[])),
            ("input a --y", node("flag", &[])),
            ("input b --z", node("flag", &[])),
            ("input b @1", node("positional", &[])),
            ("error e", node("error_code", &[])),
            ("event p", node("event", &[])),
            ("file k", node("file_kind", &[])),
            ("x y", node("frobnicate", &[])),
        ]);
        let want: BTreeMap<String, usize> = [
            ("command", 2),
            ("commands", 2),
            ("commands with data", 1),
            ("error", 1),
            ("error codes", 1),
            ("event", 1),
            ("events", 1),
            ("file", 1),
            ("file kinds", 1),
            ("flags", 3),
            ("input", 4),
            ("opaque positions", 1),
            ("output", 2),
            ("positionals", 1),
            ("unknown", 1),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        assert_eq!(section_counts(&m), want);
    }

    #[test]
    fn report_against_empty_baseline() {
        let m = man(&[
            ("command a", node("command", &[])),
            (
                "input a --x",
                node("flag", &[(fact::REQUIRED, Fact::Bool(true))]),
            ),
            ("output a data", node("output_position", &[])),
        ]);
        let want = "\
semantic manifest, format 1: 3 nodes
  command              1
  commands             1
  commands with data   1
  flags                1
  input                1
  output               1
against an empty baseline: 3 changes (3 additive, 0 breaking, 0 unclassified); wire bump: minor
";
        assert_eq!(report(&m, None), want);
    }

    #[test]
    fn report_against_a_baseline_renders_changes() {
        let current = man(&[
            ("output a data", node("output_position", &[])),
            (
                "output a data.size",
                node(
                    "output_position",
                    &[(fact::TYPES, Fact::List(vec!["integer".into()]))],
                ),
            ),
        ]);
        let baseline = man(&[("output a data", node("output_position", &[]))]);
        let text = report(&current, Some(&baseline));
        assert!(text.contains("against the given baseline: 1 changes (1 additive, 0 breaking, 0 unclassified); wire bump: minor\n"), "{text}");
        assert!(
            text.ends_with("additive (1):\n  + output a data.size  integer\n"),
            "{text}"
        );
        let same = report(&current, Some(&current));
        assert!(
            same.ends_with("against the given baseline: no change\n"),
            "{same}"
        );
    }

    #[test]
    fn report_formats_thousands() {
        let cases = [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (5231, "5,231"),
            (1_234_567, "1,234,567"),
        ];
        for (n, want) in cases {
            assert_eq!(thousands(n), want, "{n}");
        }
    }
}
