//! The compatibility classifier (M4-D56): two manifests compared node by node, each change
//! labelled additive, breaking or unclassified in its direction. For outputs, emitting
//! more is additive and emitting less breaking; for inputs, accepting more is additive and
//! accepting less, or a changed default, breaking. A change it cannot place is
//! unclassified, which counts as breaking until reviewed. Persisted formats keep their own
//! versions: their changes demand that kind's bump, never a wire one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::ids::{self, section};
use super::{Fact, Manifest, Node, NodeKind, fact};
use crate::state::store::SchemaVersion;

/// Every change between two manifests, and what each persisted kind's version must do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    /// Sorted by node, then fact.
    pub changes: Vec<Change>,
    /// Sorted by kind.
    pub kinds: Vec<KindBump>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub node: String,
    pub kind: ChangeKind,
    /// The fact that changed; `None` for a node added or removed.
    pub fact: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub label: Label,
    /// The rule's phrase, `output type widened`.
    pub rule: &'static str,
    /// The persisted kind whose own version this change binds, `None` for the wire.
    pub persisted: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Label {
    Additive,
    Breaking,
    /// Not placed by any rule: breaking until reviewed.
    Unclassified,
}

impl Label {
    fn name(self) -> &'static str {
        match self {
            Self::Additive => "additive",
            Self::Breaking => "breaking",
            Self::Unclassified => "unclassified",
        }
    }
}

/// A version bump a change demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    None,
    Minor,
    Major,
}

impl Bump {
    /// Whether going from `baseline` to `current` makes this bump.
    pub fn is_satisfied(self, baseline: SchemaVersion, current: SchemaVersion) -> bool {
        match self {
            Self::None => true,
            Self::Minor => {
                current.major > baseline.major
                    || (current.major == baseline.major && current.minor > baseline.minor)
            }
            Self::Major => current.major > baseline.major,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "no",
            Self::Minor => "a minor",
            Self::Major => "a major",
        }
    }
}

/// What one persisted kind's version must do for its schema changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindBump {
    pub kind: String,
    pub baseline: Option<String>,
    pub current: Option<String>,
    pub required: Bump,
    pub satisfied: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.kinds.is_empty()
    }

    /// The `schema_version` bump the wire changes demand: major for anything breaking or
    /// unclassified, else minor for anything additive.
    pub fn wire_bump(&self) -> Bump {
        let wire = self.changes.iter().filter(|c| c.persisted.is_none());
        wire.fold(Bump::None, |bump, change| {
            bump.max(match change.label {
                Label::Additive => Bump::Minor,
                Label::Breaking | Label::Unclassified => Bump::Major,
            })
        })
    }

    pub fn unsatisfied_kinds(&self) -> Vec<&KindBump> {
        self.kinds.iter().filter(|k| !k.satisfied).collect()
    }

    /// `(additive, breaking, unclassified)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        let count = |label| self.changes.iter().filter(|c| c.label == label).count();
        (
            count(Label::Additive),
            count(Label::Breaking),
            count(Label::Unclassified),
        )
    }

    /// The change list a contributor reads: wire changes grouped by label (breaking, then
    /// unclassified, then additive), then one block per persisted kind. A change below a
    /// node that was itself added or removed is folded into that node's line.
    pub fn render(&self) -> String {
        let changed: BTreeSet<&str> = self
            .changes
            .iter()
            .filter(|c| c.kind != ChangeKind::Changed)
            .map(|c| c.node.as_str())
            .collect();
        let folded_under = |node: &str| -> Option<String> {
            let mut at = ids::parent(node);
            while let Some(ancestor) = at {
                if changed.contains(ancestor.as_str()) {
                    return Some(ancestor);
                }
                at = ids::parent(&ancestor);
            }
            None
        };
        let mut below: BTreeMap<String, usize> = BTreeMap::new();
        let mut shown: Vec<&Change> = Vec::new();
        for change in &self.changes {
            match folded_under(&change.node) {
                Some(ancestor) => *below.entry(ancestor).or_default() += 1,
                None => shown.push(change),
            }
        }
        let line = |change: &Change| {
            let mut out = match change.kind {
                ChangeKind::Added => {
                    let summary = change.after.as_deref().unwrap_or("");
                    if summary.is_empty() {
                        format!("  + {}", change.node)
                    } else {
                        format!("  + {}  {summary}", change.node)
                    }
                }
                ChangeKind::Removed => {
                    let summary = change.before.as_deref().unwrap_or("");
                    let summary = if summary.is_empty() {
                        String::new()
                    } else {
                        format!("  {summary}")
                    };
                    format!("  - {}{summary}  ({})", change.node, change.rule)
                }
                ChangeKind::Changed => format!(
                    "  ~ {}  {}: {} -> {}  ({})",
                    change.node,
                    change.fact.as_deref().unwrap_or(""),
                    change.before.as_deref().unwrap_or("none"),
                    change.after.as_deref().unwrap_or("none"),
                    change.rule
                ),
            };
            if change.kind != ChangeKind::Changed
                && let Some(n) = below.get(&change.node)
            {
                let _ = write!(out, " (and {n} nodes below)");
            }
            out
        };
        let mut out = String::new();
        for label in [Label::Breaking, Label::Unclassified, Label::Additive] {
            let group: Vec<&&Change> = shown
                .iter()
                .filter(|c| {
                    c.label == label
                        && !self
                            .kinds
                            .iter()
                            .any(|k| c.persisted.as_deref() == Some(&k.kind))
                })
                .collect();
            if group.is_empty() {
                continue;
            }
            let _ = writeln!(out, "{} ({}):", label.name(), group.len());
            for change in group {
                let _ = writeln!(out, "{}", line(change));
            }
        }
        for kind in &self.kinds {
            let changes: Vec<&&Change> = shown
                .iter()
                .filter(|c| c.persisted.as_deref() == Some(kind.kind.as_str()))
                .collect();
            let verb = if kind.satisfied { "has" } else { "needs" };
            let plural = if changes.len() == 1 { "" } else { "s" };
            let _ = writeln!(
                out,
                "persisted file {} ({} -> {}) {verb} {} bump ({} change{plural}):",
                kind.kind,
                kind.baseline.as_deref().unwrap_or("none"),
                kind.current.as_deref().unwrap_or("none"),
                kind.required.name(),
                changes.len()
            );
            for change in changes {
                let _ = writeln!(out, "{}", line(change));
            }
        }
        out
    }
}

/// The shapes a fact's rule takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// A set: elements added are additive, removed breaking.
    MoreAdditive,
    /// A set: elements added are breaking, removed additive.
    MoreBreaking,
    TrueAdditive,
    TrueBreaking,
    LowerOutput,
    UpperOutput,
    LowerInput,
    /// Absent is unbounded (`arity_max`), as for any input upper bound.
    UpperInput,
    PatternOutput,
    PatternInput,
    /// Text: appearing is additive; any other change breaking.
    AddedAdditive,
    /// Any change breaking.
    Equal,
    Grammar,
    TypesOutput,
    TypesInput,
    /// An output enum: more values additive; the enum dropped (opened) breaking, introduced
    /// (closed) additive.
    EnumOutput,
    /// An input enum: more values additive; dropped additive, introduced breaking.
    EnumInput,
    /// Equal, but appearing is additive (`doctor_exit` when the doctor lands).
    EqualOrAppearing,
    /// A persisted kind's version: lowered is breaking; raised is that kind's own rule.
    Version,
    /// Help text and the like: never a change.
    Presentation,
    /// Any change unclassified.
    Unclassifiable,
}

/// One rule: the kinds and fact it governs, its shape, and its phrases for an additive
/// and for a breaking outcome.
struct Row {
    kinds: &'static [NodeKind],
    fact: &'static str,
    rule: Rule,
    additive: &'static str,
    breaking: &'static str,
}

const fn row(
    kinds: &'static [NodeKind],
    fact: &'static str,
    rule: Rule,
    additive: &'static str,
    breaking: &'static str,
) -> Row {
    Row {
        kinds,
        fact,
        rule,
        additive,
        breaking,
    }
}

use NodeKind as K;

const OUT_POS: &[NodeKind] = &[K::OutputPosition, K::FilePosition];
const OUT_ANY: &[NodeKind] = &[
    K::OutputPosition,
    K::OutputVariant,
    K::FilePosition,
    K::FileVariant,
];
const IN_POS: &[NodeKind] = &[K::DesiredPosition];
const IN_ANY: &[NodeKind] = &[K::DesiredPosition, K::DesiredVariant];
const ARGS: &[NodeKind] = &[K::Flag, K::Positional];
const COMMAND: &[NodeKind] = &[K::Command];

/// The rules, one row per (kinds, fact): the review surface of the classifier.
#[rustfmt::skip]
const RULES: &[Row] = &[
    // Outputs (and persisted files, read later, maybe by an older build).
    row(OUT_POS, fact::TYPES, Rule::TypesOutput, "output type narrowed", "output type widened"),
    row(OUT_POS, fact::REQUIRED, Rule::TrueAdditive, "output field made always present", "output field made optional"),
    row(OUT_POS, fact::NULLABLE, Rule::TrueBreaking, "output field made non-null", "output field made nullable"),
    row(OUT_ANY, fact::OPAQUE, Rule::TrueBreaking, "output typed again", "output became opaque"),
    row(OUT_POS, fact::ENUM, Rule::EnumOutput, "enum value added", "enum value removed"),
    row(OUT_POS, fact::MINIMUM, Rule::LowerOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::EXCLUSIVE_MINIMUM, Rule::LowerOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MIN_ITEMS, Rule::LowerOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MIN_LENGTH, Rule::LowerOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MIN_PROPERTIES, Rule::LowerOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MAXIMUM, Rule::UpperOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::EXCLUSIVE_MAXIMUM, Rule::UpperOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MAX_ITEMS, Rule::UpperOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MAX_LENGTH, Rule::UpperOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::MAX_PROPERTIES, Rule::UpperOutput, "bound narrowed", "bound widened"),
    row(OUT_POS, fact::PATTERN, Rule::PatternOutput, "pattern added", "pattern removed"),
    row(OUT_POS, fact::UNIQUE_ITEMS, Rule::TrueAdditive, "items made unique", "items may repeat"),
    row(OUT_ANY, fact::KEY_PATTERNS, Rule::MoreBreaking, "key pattern removed", "key pattern added"),
    row(OUT_ANY, fact::TAG, Rule::Equal, "", "union tag changed"),
    row(OUT_ANY, fact::SAME_AS, Rule::Equal, "", "recursion changed"),
    row(OUT_ANY, fact::UNION_OF, Rule::Equal, "", "error union reference changed"),
    row(OUT_POS, fact::MULTIPLE_OF, Rule::Unclassifiable, "", "multiple_of changed"),
    // addons.toml: an input.
    row(IN_POS, fact::TYPES, Rule::TypesInput, "addons.toml type widened", "addons.toml type narrowed"),
    row(IN_POS, fact::REQUIRED, Rule::TrueBreaking, "addons.toml key made optional", "addons.toml key made required"),
    row(IN_POS, fact::NULLABLE, Rule::TrueAdditive, "addons.toml key made nullable", "addons.toml key made non-null"),
    row(IN_ANY, fact::OPAQUE, Rule::TrueAdditive, "addons.toml key opened", "addons.toml key closed"),
    row(IN_POS, fact::ENUM, Rule::EnumInput, "addons.toml value added", "addons.toml value removed"),
    row(IN_ANY, fact::KEY_PATTERNS, Rule::MoreAdditive, "key pattern added", "key pattern removed"),
    row(IN_POS, fact::MINIMUM, Rule::LowerInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::EXCLUSIVE_MINIMUM, Rule::LowerInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MIN_ITEMS, Rule::LowerInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MIN_LENGTH, Rule::LowerInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MIN_PROPERTIES, Rule::LowerInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MAXIMUM, Rule::UpperInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::EXCLUSIVE_MAXIMUM, Rule::UpperInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MAX_ITEMS, Rule::UpperInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MAX_LENGTH, Rule::UpperInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::MAX_PROPERTIES, Rule::UpperInput, "bound widened", "bound narrowed"),
    row(IN_POS, fact::PATTERN, Rule::PatternInput, "pattern removed", "pattern added"),
    row(IN_POS, fact::UNIQUE_ITEMS, Rule::TrueBreaking, "items may repeat", "items made unique"),
    row(IN_ANY, fact::TAG, Rule::Equal, "", "union tag changed"),
    row(IN_ANY, fact::SAME_AS, Rule::Equal, "", "recursion changed"),
    // Commands.
    row(COMMAND, fact::ALIASES, Rule::MoreAdditive, "alias added", "alias removed"),
    row(COMMAND, fact::EXITS, Rule::MoreAdditive, "exit added", "exit removed"),
    row(COMMAND, fact::BINDS, Rule::TrueAdditive, "binding added", "binding removed"),
    row(COMMAND, fact::STORES_PLAN, Rule::TrueAdditive, "plan storage added", "plan storage removed"),
    row(COMMAND, fact::SUBCOMMAND_REQUIRED, Rule::TrueBreaking, "subcommand made optional", "subcommand made required"),
    row(COMMAND, fact::ARGS_CONFLICT_WITH_SUBCOMMANDS, Rule::TrueBreaking, "arguments allowed with subcommands", "arguments refused with subcommands"),
    row(COMMAND, fact::DICTIONARY, Rule::Equal, "", "exit dictionary changed"),
    row(COMMAND, fact::NETWORK, Rule::Equal, "", "network use changed"),
    row(COMMAND, fact::MUTATES, Rule::Equal, "", "mutates changed"),
    row(COMMAND, fact::ENVELOPE_ONLY, Rule::Equal, "", "envelope_only changed"),
    // Flags and positionals.
    row(ARGS, fact::VALUES, Rule::MoreAdditive, "value added", "value removed"),
    row(ARGS, fact::ALIASES, Rule::MoreAdditive, "alias added", "alias removed"),
    row(ARGS, fact::SHORT_ALIASES, Rule::MoreAdditive, "short alias added", "short alias removed"),
    row(ARGS, fact::CONFLICTS, Rule::MoreBreaking, "conflict removed", "conflict added"),
    row(ARGS, fact::REQUIRES, Rule::MoreBreaking, "requirement removed", "requirement added"),
    row(ARGS, fact::REPEAT, Rule::TrueAdditive, "argument made repeatable", "argument no longer repeatable"),
    row(ARGS, fact::IGNORE_CASE, Rule::TrueAdditive, "case ignored", "case no longer ignored"),
    row(ARGS, fact::GLOBAL, Rule::TrueAdditive, "argument made global", "argument no longer global"),
    row(ARGS, fact::ALLOW_HYPHEN_VALUES, Rule::TrueAdditive, "hyphen values allowed", "hyphen values refused"),
    row(ARGS, fact::ALLOW_NEGATIVE_NUMBERS, Rule::TrueAdditive, "negative numbers allowed", "negative numbers refused"),
    row(ARGS, fact::TRAILING_VAR_ARG, Rule::TrueAdditive, "trailing arguments allowed", "trailing arguments refused"),
    row(ARGS, fact::REQUIRED, Rule::TrueBreaking, "argument became optional", "argument became required"),
    row(ARGS, fact::REQUIRE_EQUALS, Rule::TrueBreaking, "= no longer required", "= required"),
    row(ARGS, fact::EXCLUSIVE, Rule::TrueBreaking, "argument no longer exclusive", "argument made exclusive"),
    row(ARGS, fact::LAST, Rule::TrueBreaking, "argument no longer last", "argument made last"),
    row(ARGS, fact::ARITY_MIN, Rule::LowerInput, "arity lowered", "arity raised"),
    row(ARGS, fact::ARITY_MAX, Rule::UpperInput, "arity raised", "arity lowered"),
    row(ARGS, fact::SHORT, Rule::AddedAdditive, "short added", "short changed"),
    row(ARGS, fact::ENV, Rule::AddedAdditive, "env added", "env changed"),
    row(ARGS, fact::DEFAULT, Rule::Equal, "", "default changed"),
    row(ARGS, fact::VALUE_DELIMITER, Rule::Equal, "", "value delimiter changed"),
    row(ARGS, fact::GRAMMAR, Rule::Grammar, "grammar widened to string", "grammar narrowed from string"),
    row(ARGS, fact::ACTION, Rule::Unclassifiable, "", "action changed"),
    row(ARGS, fact::VALUE_NAME, Rule::Presentation, "", ""),
    // Registries.
    row(&[K::ConfigKey], fact::CONFIG_TYPE, Rule::Equal, "", "config type changed"),
    row(&[K::EnvVar], fact::MIRRORS, Rule::Equal, "", "mirrored flag changed"),
    row(&[K::ErrorCode], fact::EXIT, Rule::Equal, "", "exit changed"),
    row(&[K::ErrorCode], fact::RETRYABLE, Rule::Equal, "", "retryability changed"),
    row(&[K::ErrorCode], fact::DOCTOR_EXIT, Rule::EqualOrAppearing, "doctor exit added", "doctor exit changed"),
    row(&[K::Action], fact::DESTRUCTIVE, Rule::Equal, "", "destructive changed"),
    row(&[K::ExitCode], fact::NAME, Rule::Equal, "", "exit name changed"),
    row(&[K::ExitCode], fact::RETRYABLE, Rule::Equal, "", "exit retryability changed"),
    row(&[K::FileKind], fact::VERSION, Rule::Version, "", "persisted kind version lowered"),
    // Any kind.
    row(&NodeKind::ALL, fact::KEYWORDS, Rule::Unclassifiable, "", "unmodelled schema keyword changed"),
];

fn rule_for(kind: NodeKind, name: &str) -> Option<&'static Row> {
    RULES
        .iter()
        .find(|row| row.fact == name && row.kinds.contains(&kind))
}

/// An untagged variant's keys, from its id's last segment (`<untagged=r1,r2|o1>`):
/// (required, optional). `None` for any other id, and for one whose keys are quoted, which
/// [`extensions`] leaves to the plain comparison.
fn untagged_keys<'a>(id: &'a str) -> Option<(&'a str, BTreeSet<&'a str>, BTreeSet<&'a str>)> {
    let at = id.rfind("<untagged=")?;
    let inner = id[at..].strip_prefix("<untagged=")?.strip_suffix('>')?;
    if inner.contains(['"', '<', '>', '.', '[', '{']) {
        return None;
    }
    let (required, optional) = inner.split_once('|').unwrap_or((inner, ""));
    let keys = |list: &'a str| -> BTreeSet<&'a str> {
        list.split(',').filter(|k| !k.is_empty()).collect()
    };
    Some((&id[..at], keys(required), keys(optional)))
}

/// The untagged variants of `baseline` that `current` extends: each removed one whose
/// position holds exactly one added untagged variant keeping every key it had (a required
/// key still required, an optional one still present) and adding some, and that added one
/// extends no other. An untagged variant is named by its keys, so a key added to it
/// renames it: a reader of the old variant reads the new one as it did, so the change is
/// the keys added, never a variant removed. Outermost first, as `(old, new)` ids.
fn extensions(baseline: &Manifest, current: &Manifest) -> Vec<(String, String)> {
    let variant = |node: &Node| {
        matches!(
            node.kind(),
            Some(K::OutputVariant | K::FileVariant | K::DesiredVariant)
        )
    };
    let mut nodes: BTreeMap<String, Node> = baseline.nodes.clone();
    let mut out: Vec<(String, String)> = Vec::new();
    loop {
        let removed: Vec<&String> = nodes
            .iter()
            .filter(|(id, node)| variant(node) && !current.nodes.contains_key(*id))
            .map(|(id, _)| id)
            .collect();
        let added: Vec<&String> = current
            .nodes
            .iter()
            .filter(|(id, node)| variant(node) && !nodes.contains_key(*id))
            .map(|(id, _)| id)
            .collect();
        let extends = |old: &str, new: &str| {
            let (Some((at, r, o)), Some((at2, r2, o2))) = (untagged_keys(old), untagged_keys(new))
            else {
                return false;
            };
            at == at2
                && r.is_subset(&r2)
                && o.iter().all(|k| r2.contains(k) || o2.contains(k))
                && (r != r2 || o != o2)
        };
        let mut pair = None;
        let mut ordered = removed.clone();
        ordered.sort_by_key(|id| id.len());
        for old in ordered {
            let candidates: Vec<&&String> = added.iter().filter(|new| extends(old, new)).collect();
            if let [new] = candidates.as_slice()
                && removed.iter().filter(|o| extends(o, new)).count() == 1
            {
                pair = Some((old.clone(), (**new).clone()));
                break;
            }
        }
        let Some((old, new)) = pair else {
            return out;
        };
        move_subtree(&mut nodes, &old, &new);
        out.push((old, new));
    }
}

/// Every node of `nodes` at or below the variant `old`, moved under `new`. A variant id
/// ends with its `>`, so only it and its descendants start with it.
fn move_subtree(nodes: &mut BTreeMap<String, Node>, old: &str, new: &str) {
    let moved: Vec<String> = nodes
        .keys()
        .filter(|id| id.starts_with(old))
        .cloned()
        .collect();
    for id in moved {
        if let Some(node) = nodes.remove(&id) {
            nodes.insert(format!("{new}{}", &id[old.len()..]), node);
        }
    }
}

/// `baseline` with each extended variant's subtree under its new id ([`extensions`]).
fn extended(baseline: &Manifest, pairs: &[(String, String)]) -> Manifest {
    let mut nodes = baseline.nodes.clone();
    for (old, new) in pairs {
        move_subtree(&mut nodes, old, new);
    }
    Manifest {
        format: baseline.format,
        nodes,
    }
}

/// Compares `current` with `baseline`. Pure and total: it reads nothing else.
pub fn classify(baseline: &Manifest, current: &Manifest) -> Changes {
    if baseline.format != current.format {
        return Changes {
            changes: vec![Change {
                node: "format".to_owned(),
                kind: ChangeKind::Changed,
                fact: None,
                before: Some(baseline.format.to_string()),
                after: Some(current.format.to_string()),
                label: Label::Unclassified,
                rule: "manifest format changed; a migration of the older format is needed to compare",
                persisted: None,
            }],
            kinds: Vec::new(),
        };
    }
    // An untagged variant that gained keys is compared under its new name.
    let pairs = extensions(baseline, current);
    let renamed = extended(baseline, &pairs);
    let baseline = &renamed;
    let ids: BTreeSet<&String> = baseline.nodes.keys().chain(current.nodes.keys()).collect();
    let mut changes: Vec<Change> = pairs
        .iter()
        .map(|(old, new)| Change {
            node: new.clone(),
            kind: ChangeKind::Changed,
            fact: None,
            before: Some(old.clone()),
            after: Some(new.clone()),
            label: Label::Additive,
            rule: "variant extended (keys added)",
            persisted: persisted(new),
        })
        .collect();
    for id in ids {
        match (baseline.nodes.get(id), current.nodes.get(id)) {
            (None, Some(node)) => changes.push(node_change(id, node, ChangeKind::Added, baseline)),
            (Some(node), None) => {
                changes.push(node_change(id, node, ChangeKind::Removed, baseline))
            }
            (Some(before), Some(after)) => fact_changes(id, before, after, &mut changes),
            (None, None) => {}
        }
    }
    changes.sort_by(|a, b| (&a.node, &a.fact).cmp(&(&b.node, &b.fact)));
    let kinds = kind_bumps(baseline, current, &changes);
    Changes { changes, kinds }
}

/// The persisted kind a node's id belongs to: `file <kind> <position>`.
fn persisted(id: &str) -> Option<String> {
    let rest = id.strip_prefix(section::FILE)?.strip_prefix(' ')?;
    let (kind, position) = rest.split_once(' ')?;
    (!position.is_empty()).then(|| kind.to_owned())
}

fn node_change(id: &str, node: &Node, kind: ChangeKind, baseline: &Manifest) -> Change {
    let added = kind == ChangeKind::Added;
    let required = node.facts.get(fact::REQUIRED) == Some(&Fact::Bool(true));
    let parent_existed = ids::parent(id).is_some_and(|parent| baseline.nodes.contains_key(&parent));
    let (label, rule) = match node.kind() {
        None => (Label::Unclassified, "unknown node kind"),
        Some(K::Flag | K::Positional) if added && required && parent_existed => {
            (Label::Breaking, "required argument added")
        }
        Some(K::DesiredPosition) if added && required && parent_existed => {
            (Label::Breaking, "required addons.toml key added")
        }
        Some(K::OutputVariant | K::FileVariant | K::DesiredVariant) => {
            if added {
                (Label::Additive, "variant added")
            } else {
                (Label::Breaking, "variant removed")
            }
        }
        Some(K::OutputPosition | K::FilePosition | K::DesiredPosition) => {
            if added {
                (Label::Additive, "field added")
            } else {
                (Label::Breaking, "field removed")
            }
        }
        Some(K::Flag) => {
            if added {
                (Label::Additive, "flag added")
            } else {
                (Label::Breaking, "flag removed")
            }
        }
        Some(K::Positional) => {
            if added {
                (Label::Additive, "positional added")
            } else {
                (Label::Breaking, "positional removed")
            }
        }
        Some(K::Command) => {
            if added {
                (Label::Additive, "command added")
            } else {
                (Label::Breaking, "command removed")
            }
        }
        Some(_) => {
            if added {
                (Label::Additive, "registry entry added")
            } else {
                (Label::Breaking, "registry entry removed")
            }
        }
    };
    let summary = Some(summary(node));
    Change {
        node: id.to_owned(),
        kind,
        fact: None,
        before: if added { None } else { summary.clone() },
        after: if added { summary } else { None },
        label,
        rule,
        persisted: persisted(id),
    }
}

fn fact_changes(id: &str, before: &Node, after: &Node, out: &mut Vec<Change>) {
    let change = |name: &str, label: Label, rule: &'static str| Change {
        node: id.to_owned(),
        kind: ChangeKind::Changed,
        fact: Some(name.to_owned()),
        before: before.facts.get(name).map(show),
        after: after.facts.get(name).map(show),
        label,
        rule,
        persisted: persisted(id),
    };
    if before.kind != after.kind {
        out.push(Change {
            before: Some(before.kind.clone()),
            after: Some(after.kind.clone()),
            ..change("kind", Label::Unclassified, "node kind changed")
        });
        return;
    }
    let names: BTreeSet<&String> = before.facts.keys().chain(after.facts.keys()).collect();
    for name in names {
        let (b, a) = (
            before.facts.get(name.as_str()),
            after.facts.get(name.as_str()),
        );
        if b == a {
            continue;
        }
        let Some(row) = before.kind().and_then(|kind| rule_for(kind, name)) else {
            out.push(change(name, Label::Unclassified, "unknown fact"));
            continue;
        };
        if let Some((label, rule)) = judge(row, b, a) {
            out.push(change(name, label, rule));
        }
    }
}

/// The label a rule gives one fact's change, `None` when it is no change.
fn judge(row: &Row, before: Option<&Fact>, after: Option<&Fact>) -> Option<(Label, &'static str)> {
    let additive = Some((Label::Additive, row.additive));
    let breaking = Some((Label::Breaking, row.breaking));
    let unclassified = |rule: &'static str| Some((Label::Unclassified, rule));
    let by = |good: bool| if good { additive } else { breaking };
    match row.rule {
        Rule::Presentation => None,
        Rule::Unclassifiable => unclassified(row.breaking),
        Rule::Equal => breaking,
        Rule::EqualOrAppearing => by(before.is_none()),
        Rule::MoreAdditive | Rule::MoreBreaking => {
            let (Some(b), Some(a)) = (set(before), set(after)) else {
                return unclassified("unknown fact value");
            };
            let grew = !a.is_subset(&b);
            let shrank = !b.is_subset(&a);
            let more_is_good = row.rule == Rule::MoreAdditive;
            match (grew, shrank) {
                (false, false) => None,
                (true, true) => breaking,
                (true, false) => by(more_is_good),
                (false, true) => by(!more_is_good),
            }
        }
        Rule::TrueAdditive | Rule::TrueBreaking => {
            let (Some(b), Some(a)) = (boolean(before), boolean(after)) else {
                return unclassified("unknown fact value");
            };
            if a == b {
                return None;
            }
            by(a == (row.rule == Rule::TrueAdditive))
        }
        Rule::LowerOutput | Rule::UpperOutput | Rule::LowerInput | Rule::UpperInput => {
            let (b, a) = match (bound(before), bound(after)) {
                (Some(b), Some(a)) => (b, a),
                _ => return unclassified("non-integer bound changed"),
            };
            // An absent lower bound is the lowest; an absent upper bound the highest.
            let lower = matches!(row.rule, Rule::LowerOutput | Rule::LowerInput);
            let rank = |v: Option<i64>| match v {
                Some(v) => i128::from(v),
                None if lower => i128::MIN,
                None => i128::MAX,
            };
            let (b, a) = (rank(b), rank(a));
            if a == b {
                return None;
            }
            let narrowed = if lower { a > b } else { a < b };
            let output = matches!(row.rule, Rule::LowerOutput | Rule::UpperOutput);
            by(narrowed == output)
        }
        Rule::PatternOutput | Rule::PatternInput => match (before, after) {
            (None, Some(_)) => by(row.rule == Rule::PatternOutput),
            (Some(_), None) => by(row.rule == Rule::PatternInput),
            _ => unclassified("pattern changed"),
        },
        Rule::AddedAdditive => by(before.is_none()),
        Rule::Grammar => {
            let (b, a) = (text(before), text(after));
            match (b.as_deref(), a.as_deref()) {
                (Some("enum" | "duration_or_time" | "path" | "integer"), Some("string")) => {
                    additive
                }
                (Some("string"), Some(_)) => breaking,
                _ => unclassified("grammar changed"),
            }
        }
        Rule::TypesOutput | Rule::TypesInput => {
            let output = row.rule == Rule::TypesOutput;
            // Absent types constrain nothing: appearing narrows, disappearing widens.
            let (b, a) = match (set(before), set(after), before.is_none(), after.is_none()) {
                (_, _, true, false) => return by(output),
                (_, _, false, true) => return by(!output),
                (Some(b), Some(a), _, _) => (b, a),
                _ => return unclassified("unknown fact value"),
            };
            let number = |set: &BTreeSet<String>| set.contains("number");
            let added: Vec<&String> = a
                .difference(&b)
                .filter(|t| !(*t == "integer" && number(&b)))
                .collect();
            let removed: Vec<&String> = b
                .difference(&a)
                .filter(|t| !(*t == "integer" && number(&a)))
                .collect();
            // Widening (a type the other side lacks) breaks an output reader and suits an
            // input writer; narrowing the reverse.
            let (widened, narrowed) = (!added.is_empty(), !removed.is_empty());
            match (widened, narrowed) {
                (false, false) => {
                    // integer <-> number only.
                    let towards_number = a.contains("number") && !b.contains("number");
                    if output {
                        by(!towards_number)
                    } else {
                        by(towards_number)
                    }
                }
                (true, true) => breaking,
                (true, false) => by(!output),
                (false, true) => by(output),
            }
        }
        Rule::EnumOutput | Rule::EnumInput => {
            let output = row.rule == Rule::EnumOutput;
            match (before, after) {
                // Dropped: the field is open now.
                (Some(_), None) => by(!output),
                // Introduced: the field is closed now.
                (None, Some(_)) => by(output),
                _ => {
                    let (Some(b), Some(a)) = (set(before), set(after)) else {
                        return unclassified("unknown fact value");
                    };
                    match (!a.is_subset(&b), !b.is_subset(&a)) {
                        (false, false) => None,
                        (true, true) => breaking,
                        (true, false) => additive,
                        (false, true) => breaking,
                    }
                }
            }
        }
        Rule::Version => {
            let parse =
                |fact: Option<&Fact>| text(fact).and_then(|t| t.parse::<SchemaVersion>().ok());
            match (parse(before), parse(after)) {
                (Some(b), Some(a)) if a < b => breaking,
                _ => None,
            }
        }
    }
}

/// A set fact's elements; an absent set is empty.
fn set(fact: Option<&Fact>) -> Option<BTreeSet<String>> {
    match fact {
        None => Some(BTreeSet::new()),
        Some(Fact::List(items)) => Some(items.iter().cloned().collect()),
        Some(Fact::Ints(items)) => Some(items.iter().map(i64::to_string).collect()),
        Some(_) => None,
    }
}

/// A boolean fact; an absent one is false.
fn boolean(fact: Option<&Fact>) -> Option<bool> {
    match fact {
        None => Some(false),
        Some(Fact::Bool(b)) => Some(*b),
        Some(_) => None,
    }
}

/// A bound: `Some(None)` when absent, `None` when it is not an integer.
fn bound(fact: Option<&Fact>) -> Option<Option<i64>> {
    match fact {
        None => Some(None),
        Some(Fact::Int(n)) => Some(Some(*n)),
        Some(_) => None,
    }
}

fn text(fact: Option<&Fact>) -> Option<String> {
    match fact {
        Some(Fact::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// A fact's value as a change line shows it.
fn show(fact: &Fact) -> String {
    match fact {
        Fact::Bool(b) => b.to_string(),
        Fact::Int(n) => n.to_string(),
        Fact::Text(t) => t.clone(),
        Fact::List(items) => items.join("|"),
        Fact::Ints(items) => items
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join("|"),
    }
}

/// The bounds a position's summary names, in order.
const SUMMARY_BOUNDS: &[&str] = &[
    fact::MINIMUM,
    fact::EXCLUSIVE_MINIMUM,
    fact::MAXIMUM,
    fact::EXCLUSIVE_MAXIMUM,
    fact::MIN_ITEMS,
    fact::MAX_ITEMS,
    fact::MIN_LENGTH,
    fact::MAX_LENGTH,
    fact::MIN_PROPERTIES,
    fact::MAX_PROPERTIES,
];

/// An added or removed node's facts, as one line shows them.
fn summary(node: &Node) -> String {
    let facts = &node.facts;
    let yes = |name: &str| facts.get(name) == Some(&Fact::Bool(true));
    let mut parts: Vec<String> = Vec::new();
    match node.kind() {
        Some(K::OutputPosition | K::FilePosition | K::DesiredPosition) => {
            if let Some(types) = facts.get(fact::TYPES) {
                parts.push(show(types));
            }
            for flag in [fact::REQUIRED, fact::NULLABLE] {
                if yes(flag) {
                    parts.push(flag.to_owned());
                }
            }
            for name in SUMMARY_BOUNDS {
                if let Some(value) = facts.get(*name) {
                    parts.push(format!("{name} {}", show(value)));
                }
            }
            if let Some(values) = facts.get(fact::ENUM) {
                parts.push(format!("enum ({})", show(values)));
            }
            if yes(fact::OPAQUE) {
                parts.push(fact::OPAQUE.to_owned());
            }
        }
        Some(K::Flag | K::Positional) => {
            if let Some(grammar) = facts.get(fact::GRAMMAR) {
                let values = facts
                    .get(fact::VALUES)
                    .map(|v| format!(" ({})", show(v)))
                    .unwrap_or_default();
                parts.push(format!("{}{values}", show(grammar)));
            }
            if let Some(min) = facts.get(fact::ARITY_MIN) {
                let max = facts.get(fact::ARITY_MAX).map(show).unwrap_or_default();
                parts.push(format!("arity {}..{max}", show(min)));
            }
            for flag in [fact::REQUIRED, fact::REPEAT, fact::GLOBAL] {
                if yes(flag) {
                    parts.push(flag.to_owned());
                }
            }
        }
        _ => parts.extend(
            facts
                .iter()
                .map(|(name, value)| format!("{name} {}", show(value))),
        ),
    }
    parts.join(", ")
}

/// For each persisted kind with schema changes and a `file <kind>` node on both sides: the
/// bump its own version must make.
fn kind_bumps(baseline: &Manifest, current: &Manifest, changes: &[Change]) -> Vec<KindBump> {
    let mut by_kind: BTreeMap<&str, Bump> = BTreeMap::new();
    for change in changes {
        if let Some(kind) = &change.persisted {
            let bump = if change.label == Label::Additive {
                Bump::Minor
            } else {
                Bump::Major
            };
            let entry = by_kind.entry(kind.as_str()).or_insert(Bump::None);
            *entry = (*entry).max(bump);
        }
    }
    by_kind
        .into_iter()
        .filter_map(|(kind, required)| {
            let id = ids::anchored(section::FILE, kind, None);
            // A kind on one side only is a kind added or removed: no bump of its own.
            if !(baseline.nodes.contains_key(&id) && current.nodes.contains_key(&id)) {
                return None;
            }
            let version = |m: &Manifest| text(m.nodes.get(&id)?.facts.get(fact::VERSION));
            let (before, after) = (version(baseline), version(current));
            let parsed = before
                .as_deref()
                .and_then(|v| v.parse::<SchemaVersion>().ok())
                .zip(
                    after
                        .as_deref()
                        .and_then(|v| v.parse::<SchemaVersion>().ok()),
                );
            Some(KindBump {
                kind: kind.to_owned(),
                baseline: before,
                current: after,
                required,
                satisfied: parsed.is_some_and(|(b, a)| required.is_satisfied(b, a)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::contract::manifest::FORMAT;

    type N = (String, String, Vec<(String, Fact)>);

    fn node(id: &str, kind: &str, facts: &[(&str, Fact)]) -> N {
        (
            id.to_owned(),
            kind.to_owned(),
            facts
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    fn man(nodes: &[N]) -> Manifest {
        Manifest {
            format: FORMAT,
            nodes: nodes
                .iter()
                .map(|(id, kind, facts)| {
                    (
                        id.clone(),
                        Node {
                            kind: kind.clone(),
                            facts: facts.iter().cloned().collect(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn v(s: &str) -> SchemaVersion {
        s.parse().unwrap()
    }

    fn l(items: &[&str]) -> Fact {
        Fact::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    fn t(s: &str) -> Fact {
        Fact::Text(s.to_owned())
    }

    fn yes() -> Fact {
        Fact::Bool(true)
    }

    fn no() -> Fact {
        Fact::Bool(false)
    }

    const OP: &str = "output_position";
    const OV: &str = "output_variant";
    const FLAG: &str = "flag";
    const POS: &str = "positional";
    const CMD: &str = "command";
    const DP: &str = "desired_position";

    struct Case {
        name: &'static str,
        before: Vec<N>,
        after: Vec<N>,
        want: Vec<(String, Option<&'static str>, Label)>,
        bump: Bump,
    }

    /// One fact of one node changing from `before` to `after`.
    fn fc(
        name: &'static str,
        id: &str,
        kind: &str,
        fact_name: &'static str,
        before: Option<Fact>,
        after: Option<Fact>,
        label: Option<Label>,
    ) -> Case {
        let with = |value: &Option<Fact>| -> Vec<N> {
            let facts: Vec<(&str, Fact)> = value.iter().map(|v| (fact_name, v.clone())).collect();
            vec![node(id, kind, &facts)]
        };
        let bump = match label {
            None => Bump::None,
            Some(Label::Additive) => Bump::Minor,
            Some(_) => Bump::Major,
        };
        Case {
            name,
            before: with(&before),
            after: with(&after),
            want: label
                .map(|label| (id.to_owned(), Some(fact_name), label))
                .into_iter()
                .collect(),
            bump,
        }
    }

    /// `extra` added on top of `common`.
    fn added(name: &'static str, common: Vec<N>, extra: Vec<N>, want: Vec<(&str, Label)>) -> Case {
        let after: Vec<N> = common.iter().cloned().chain(extra).collect();
        let bump = want
            .iter()
            .map(|(_, label)| {
                if *label == Label::Additive {
                    Bump::Minor
                } else {
                    Bump::Major
                }
            })
            .max()
            .unwrap_or(Bump::None);
        Case {
            name,
            before: common,
            after,
            want: want
                .into_iter()
                .map(|(id, label)| (id.to_owned(), None, label))
                .collect(),
            bump,
        }
    }

    /// The reverse of `added`: `extra` removed from `common`.
    fn removed(
        name: &'static str,
        common: Vec<N>,
        extra: Vec<N>,
        want: Vec<(&str, Label)>,
    ) -> Case {
        let mut case = added(name, common, extra, want);
        std::mem::swap(&mut case.before, &mut case.after);
        case
    }

    const SIZE: &str = "output t data.size";
    const NEW: &str = "input t --new";

    fn cmd_t() -> N {
        node("command t", CMD, &[])
    }

    fn cases() -> Vec<Case> {
        use Label::{Additive as A, Breaking as B, Unclassified as U};
        let data = || node("output t data", OP, &[(fact::TYPES, l(&["object"]))]);
        let size = || {
            node(
                SIZE,
                OP,
                &[(fact::TYPES, l(&["integer"])), (fact::REQUIRED, yes())],
            )
        };
        let variant = |v: &str| node(&format!("output t data<type={v}>"), OV, &[]);
        let x_in = |v: &str| {
            node(
                &format!("output t data<type={v}>.x"),
                OP,
                &[(fact::TYPES, l(&["string"]))],
            )
        };
        let flag = |required: bool| node(NEW, FLAG, &[(fact::REQUIRED, Fact::Bool(required))]);
        let positional =
            |required: bool| node("input t @2", POS, &[(fact::REQUIRED, Fact::Bool(required))]);
        let parent = || {
            node(
                "desired family{}.addon[]",
                DP,
                &[(fact::TYPES, l(&["object"]))],
            )
        };
        let key = |required: bool| {
            node(
                "desired family{}.addon[].x",
                DP,
                &[(fact::REQUIRED, Fact::Bool(required))],
            )
        };
        let ints = |items: &[i64]| Fact::Ints(items.to_vec());
        vec![
            Case {
                name: "identical",
                before: vec![data(), size()],
                after: vec![data(), size()],
                want: vec![],
                bump: Bump::None,
            },
            added(
                "output_field_added",
                vec![data()],
                vec![size()],
                vec![(SIZE, A)],
            ),
            removed(
                "output_field_removed",
                vec![data()],
                vec![size()],
                vec![(SIZE, B)],
            ),
            fc(
                "output_field_made_optional",
                SIZE,
                OP,
                fact::REQUIRED,
                Some(yes()),
                Some(no()),
                Some(B),
            ),
            fc(
                "output_field_made_always_present",
                SIZE,
                OP,
                fact::REQUIRED,
                Some(no()),
                Some(yes()),
                Some(A),
            ),
            fc(
                "output_field_made_nullable",
                SIZE,
                OP,
                fact::NULLABLE,
                Some(no()),
                Some(yes()),
                Some(B),
            ),
            fc(
                "output_field_made_non_null",
                SIZE,
                OP,
                fact::NULLABLE,
                Some(yes()),
                Some(no()),
                Some(A),
            ),
            fc(
                "output_integer_to_number",
                SIZE,
                OP,
                fact::TYPES,
                Some(l(&["integer"])),
                Some(l(&["number"])),
                Some(B),
            ),
            fc(
                "output_number_to_integer",
                SIZE,
                OP,
                fact::TYPES,
                Some(l(&["number"])),
                Some(l(&["integer"])),
                Some(A),
            ),
            fc(
                "output_string_to_integer",
                SIZE,
                OP,
                fact::TYPES,
                Some(l(&["string"])),
                Some(l(&["integer"])),
                Some(B),
            ),
            fc(
                "output_type_narrowed",
                SIZE,
                OP,
                fact::TYPES,
                Some(l(&["integer", "string"])),
                Some(l(&["string"])),
                Some(A),
            ),
            fc(
                "output_minimum_raised",
                SIZE,
                OP,
                fact::MINIMUM,
                Some(Fact::Int(0)),
                Some(Fact::Int(1)),
                Some(A),
            ),
            fc(
                "output_minimum_lowered",
                SIZE,
                OP,
                fact::MINIMUM,
                Some(Fact::Int(1)),
                Some(Fact::Int(0)),
                Some(B),
            ),
            fc(
                "output_minimum_added",
                SIZE,
                OP,
                fact::MINIMUM,
                None,
                Some(Fact::Int(0)),
                Some(A),
            ),
            fc(
                "output_minimum_removed",
                SIZE,
                OP,
                fact::MINIMUM,
                Some(Fact::Int(0)),
                None,
                Some(B),
            ),
            fc(
                "output_max_items_lowered",
                SIZE,
                OP,
                fact::MAX_ITEMS,
                Some(Fact::Int(5)),
                Some(Fact::Int(3)),
                Some(A),
            ),
            fc(
                "output_max_items_raised",
                SIZE,
                OP,
                fact::MAX_ITEMS,
                Some(Fact::Int(3)),
                Some(Fact::Int(5)),
                Some(B),
            ),
            fc(
                "output_pattern_added",
                SIZE,
                OP,
                fact::PATTERN,
                None,
                Some(t("^a")),
                Some(A),
            ),
            fc(
                "output_pattern_removed",
                SIZE,
                OP,
                fact::PATTERN,
                Some(t("^a")),
                None,
                Some(B),
            ),
            fc(
                "output_pattern_changed",
                SIZE,
                OP,
                fact::PATTERN,
                Some(t("^a")),
                Some(t("^b")),
                Some(U),
            ),
            fc(
                "output_non_integer_bound_changed",
                SIZE,
                OP,
                fact::MAXIMUM,
                Some(t("1.5")),
                Some(t("2.5")),
                Some(U),
            ),
            fc(
                "output_enum_value_added",
                SIZE,
                OP,
                fact::ENUM,
                Some(l(&["a"])),
                Some(l(&["a", "b"])),
                Some(A),
            ),
            fc(
                "output_enum_value_removed",
                SIZE,
                OP,
                fact::ENUM,
                Some(l(&["a", "b"])),
                Some(l(&["a"])),
                Some(B),
            ),
            fc(
                "output_enum_value_swapped",
                SIZE,
                OP,
                fact::ENUM,
                Some(l(&["a", "b"])),
                Some(l(&["a", "c"])),
                Some(B),
            ),
            fc(
                "output_enum_dropped",
                SIZE,
                OP,
                fact::ENUM,
                Some(l(&["a"])),
                None,
                Some(B),
            ),
            fc(
                "output_enum_introduced",
                SIZE,
                OP,
                fact::ENUM,
                None,
                Some(l(&["a"])),
                Some(A),
            ),
            added(
                "output_variant_added",
                vec![data(), variant("a")],
                vec![variant("c")],
                vec![("output t data<type=c>", A)],
            ),
            removed(
                "output_variant_removed",
                vec![data(), variant("a")],
                vec![variant("c")],
                vec![("output t data<type=c>", B)],
            ),
            Case {
                name: "output_field_moved_between_variants",
                before: vec![variant("a"), variant("b"), x_in("a")],
                after: vec![variant("a"), variant("b"), x_in("b")],
                want: vec![
                    ("output t data<type=a>.x".into(), None, B),
                    ("output t data<type=b>.x".into(), None, A),
                ],
                bump: Bump::Major,
            },
            fc(
                "output_tag_changed",
                "output t data",
                OP,
                fact::TAG,
                Some(t("type")),
                Some(t("kind")),
                Some(B),
            ),
            fc(
                "output_became_opaque",
                SIZE,
                OP,
                fact::OPAQUE,
                None,
                Some(yes()),
                Some(B),
            ),
            fc(
                "output_typed_again",
                SIZE,
                OP,
                fact::OPAQUE,
                Some(yes()),
                None,
                Some(A),
            ),
            fc(
                "output_union_of_changed",
                SIZE,
                OP,
                fact::UNION_OF,
                Some(t("error")),
                None,
                Some(B),
            ),
            fc(
                "output_unique_items_added",
                SIZE,
                OP,
                fact::UNIQUE_ITEMS,
                None,
                Some(yes()),
                Some(A),
            ),
            fc(
                "output_key_patterns_grew",
                SIZE,
                OP,
                fact::KEY_PATTERNS,
                Some(l(&["^a"])),
                Some(l(&["^a", "^b"])),
                Some(B),
            ),
            added(
                "flag_added_optional",
                vec![cmd_t()],
                vec![flag(false)],
                vec![(NEW, A)],
            ),
            added(
                "flag_added_required",
                vec![cmd_t()],
                vec![flag(true)],
                vec![(NEW, B)],
            ),
            added(
                "flag_added_required_with_new_command",
                vec![cmd_t()],
                vec![
                    node("command u", CMD, &[]),
                    node("input u --new", FLAG, &[(fact::REQUIRED, yes())]),
                ],
                vec![("command u", A), ("input u --new", A)],
            ),
            removed(
                "flag_removed",
                vec![cmd_t()],
                vec![flag(false)],
                vec![(NEW, B)],
            ),
            fc(
                "value_added",
                NEW,
                FLAG,
                fact::VALUES,
                Some(l(&["a"])),
                Some(l(&["a", "b"])),
                Some(A),
            ),
            fc(
                "value_removed",
                NEW,
                FLAG,
                fact::VALUES,
                Some(l(&["a", "b"])),
                Some(l(&["a"])),
                Some(B),
            ),
            fc(
                "alias_added",
                NEW,
                FLAG,
                fact::ALIASES,
                None,
                Some(l(&["n"])),
                Some(A),
            ),
            fc(
                "alias_removed",
                NEW,
                FLAG,
                fact::ALIASES,
                Some(l(&["n"])),
                None,
                Some(B),
            ),
            fc(
                "short_added",
                NEW,
                FLAG,
                fact::SHORT,
                None,
                Some(t("-n")),
                Some(A),
            ),
            fc(
                "short_removed",
                NEW,
                FLAG,
                fact::SHORT,
                Some(t("-n")),
                None,
                Some(B),
            ),
            fc(
                "short_changed",
                NEW,
                FLAG,
                fact::SHORT,
                Some(t("-n")),
                Some(t("-m")),
                Some(B),
            ),
            fc(
                "default_changed",
                NEW,
                FLAG,
                fact::DEFAULT,
                Some(t("50")),
                Some(t("20")),
                Some(B),
            ),
            fc(
                "default_added",
                NEW,
                FLAG,
                fact::DEFAULT,
                None,
                Some(t("50")),
                Some(B),
            ),
            fc(
                "default_removed",
                NEW,
                FLAG,
                fact::DEFAULT,
                Some(t("50")),
                None,
                Some(B),
            ),
            fc(
                "conflict_added",
                NEW,
                FLAG,
                fact::CONFLICTS,
                None,
                Some(l(&["--old"])),
                Some(B),
            ),
            fc(
                "conflict_removed",
                NEW,
                FLAG,
                fact::CONFLICTS,
                Some(l(&["--old"])),
                None,
                Some(A),
            ),
            fc(
                "requirement_added",
                NEW,
                FLAG,
                fact::REQUIRES,
                None,
                Some(l(&["--yes"])),
                Some(B),
            ),
            fc(
                "requirement_removed",
                NEW,
                FLAG,
                fact::REQUIRES,
                Some(l(&["--yes"])),
                None,
                Some(A),
            ),
            added(
                "positional_added_required",
                vec![cmd_t()],
                vec![positional(true)],
                vec![("input t @2", B)],
            ),
            added(
                "positional_added_optional",
                vec![cmd_t()],
                vec![positional(false)],
                vec![("input t @2", A)],
            ),
            fc(
                "positional_made_required",
                "input t @2",
                POS,
                fact::REQUIRED,
                Some(no()),
                Some(yes()),
                Some(B),
            ),
            fc(
                "positional_made_optional",
                "input t @2",
                POS,
                fact::REQUIRED,
                Some(yes()),
                Some(no()),
                Some(A),
            ),
            fc(
                "arity_max_raised",
                NEW,
                FLAG,
                fact::ARITY_MAX,
                Some(Fact::Int(1)),
                Some(Fact::Int(2)),
                Some(A),
            ),
            fc(
                "arity_max_lowered",
                NEW,
                FLAG,
                fact::ARITY_MAX,
                Some(Fact::Int(2)),
                Some(Fact::Int(1)),
                Some(B),
            ),
            fc(
                "arity_max_became_unbounded",
                NEW,
                FLAG,
                fact::ARITY_MAX,
                Some(Fact::Int(1)),
                None,
                Some(A),
            ),
            fc(
                "arity_min_raised",
                NEW,
                FLAG,
                fact::ARITY_MIN,
                Some(Fact::Int(0)),
                Some(Fact::Int(1)),
                Some(B),
            ),
            fc(
                "arity_min_lowered",
                NEW,
                FLAG,
                fact::ARITY_MIN,
                Some(Fact::Int(1)),
                Some(Fact::Int(0)),
                Some(A),
            ),
            fc(
                "repeat_added",
                NEW,
                FLAG,
                fact::REPEAT,
                None,
                Some(yes()),
                Some(A),
            ),
            fc(
                "repeat_removed",
                NEW,
                FLAG,
                fact::REPEAT,
                Some(yes()),
                None,
                Some(B),
            ),
            fc(
                "global_removed",
                NEW,
                FLAG,
                fact::GLOBAL,
                Some(yes()),
                None,
                Some(B),
            ),
            fc(
                "ignore_case_removed",
                NEW,
                FLAG,
                fact::IGNORE_CASE,
                Some(yes()),
                None,
                Some(B),
            ),
            fc(
                "grammar_widened_to_string",
                NEW,
                FLAG,
                fact::GRAMMAR,
                Some(t("enum")),
                Some(t("string")),
                Some(A),
            ),
            fc(
                "grammar_duration_to_string",
                NEW,
                FLAG,
                fact::GRAMMAR,
                Some(t("duration_or_time")),
                Some(t("string")),
                Some(A),
            ),
            fc(
                "duration_unit_added",
                NEW,
                FLAG,
                fact::VALUES,
                Some(l(&["d", "h"])),
                Some(l(&["d", "h", "w"])),
                Some(A),
            ),
            fc(
                "duration_unit_removed",
                NEW,
                FLAG,
                fact::VALUES,
                Some(l(&["d", "h", "w"])),
                Some(l(&["d", "h"])),
                Some(B),
            ),
            fc(
                "grammar_narrowed_from_string",
                NEW,
                FLAG,
                fact::GRAMMAR,
                Some(t("string")),
                Some(t("enum")),
                Some(B),
            ),
            fc(
                "grammar_path_to_integer",
                NEW,
                FLAG,
                fact::GRAMMAR,
                Some(t("path")),
                Some(t("integer")),
                Some(U),
            ),
            fc(
                "grammar_to_other",
                NEW,
                FLAG,
                fact::GRAMMAR,
                Some(t("string")),
                Some(t("other")),
                Some(B),
            ),
            fc(
                "action_changed",
                NEW,
                FLAG,
                fact::ACTION,
                Some(t("set_true")),
                Some(t("count")),
                Some(U),
            ),
            fc(
                "value_name_changed",
                "input t @1",
                POS,
                fact::VALUE_NAME,
                Some(t("REF")),
                Some(t("ADDON")),
                None,
            ),
            added(
                "command_added",
                vec![],
                vec![cmd_t()],
                vec![("command t", A)],
            ),
            removed(
                "command_removed",
                vec![],
                vec![cmd_t()],
                vec![("command t", B)],
            ),
            fc(
                "command_alias_removed",
                "command t",
                CMD,
                fact::ALIASES,
                Some(l(&["u"])),
                None,
                Some(B),
            ),
            fc(
                "command_exit_added",
                "command t",
                CMD,
                fact::EXITS,
                Some(ints(&[0, 1, 2])),
                Some(ints(&[0, 1, 2, 6])),
                Some(A),
            ),
            fc(
                "command_exit_removed",
                "command t",
                CMD,
                fact::EXITS,
                Some(ints(&[0, 1, 2, 6])),
                Some(ints(&[0, 1, 2])),
                Some(B),
            ),
            fc(
                "binds_added",
                "command t",
                CMD,
                fact::BINDS,
                Some(no()),
                Some(yes()),
                Some(A),
            ),
            fc(
                "stores_plan_removed",
                "command t",
                CMD,
                fact::STORES_PLAN,
                Some(yes()),
                Some(no()),
                Some(B),
            ),
            fc(
                "mutates_changed",
                "command t",
                CMD,
                fact::MUTATES,
                Some(no()),
                Some(yes()),
                Some(B),
            ),
            fc(
                "network_changed",
                "command t",
                CMD,
                fact::NETWORK,
                Some(t("none")),
                Some(t("optional")),
                Some(B),
            ),
            fc(
                "dictionary_changed",
                "command t",
                CMD,
                fact::DICTIONARY,
                Some(t("general")),
                Some(t("doctor")),
                Some(B),
            ),
            fc(
                "subcommand_required_added",
                "command t",
                CMD,
                fact::SUBCOMMAND_REQUIRED,
                None,
                Some(yes()),
                Some(B),
            ),
            added(
                "config_key_added",
                vec![],
                vec![node(
                    "config x",
                    "config_key",
                    &[(fact::CONFIG_TYPE, t("text"))],
                )],
                vec![("config x", A)],
            ),
            removed(
                "config_key_removed",
                vec![],
                vec![node(
                    "config x",
                    "config_key",
                    &[(fact::CONFIG_TYPE, t("text"))],
                )],
                vec![("config x", B)],
            ),
            fc(
                "config_key_type_changed",
                "config x",
                "config_key",
                fact::CONFIG_TYPE,
                Some(t("text")),
                Some(t("count")),
                Some(B),
            ),
            added(
                "desired_key_added_optional",
                vec![parent()],
                vec![key(false)],
                vec![("desired family{}.addon[].x", A)],
            ),
            added(
                "desired_key_added_required",
                vec![parent()],
                vec![key(true)],
                vec![("desired family{}.addon[].x", B)],
            ),
            fc(
                "desired_type_widened",
                "desired x",
                DP,
                fact::TYPES,
                Some(l(&["string"])),
                Some(l(&["integer", "string"])),
                Some(A),
            ),
            fc(
                "desired_integer_to_number",
                "desired x",
                DP,
                fact::TYPES,
                Some(l(&["integer"])),
                Some(l(&["number"])),
                Some(A),
            ),
            fc(
                "desired_number_to_integer",
                "desired x",
                DP,
                fact::TYPES,
                Some(l(&["number"])),
                Some(l(&["integer"])),
                Some(B),
            ),
            fc(
                "desired_enum_value_removed",
                "desired x",
                DP,
                fact::ENUM,
                Some(l(&["a", "b"])),
                Some(l(&["a"])),
                Some(B),
            ),
            fc(
                "desired_enum_value_added",
                "desired x",
                DP,
                fact::ENUM,
                Some(l(&["a"])),
                Some(l(&["a", "b"])),
                Some(A),
            ),
            fc(
                "desired_made_nullable",
                "desired x",
                DP,
                fact::NULLABLE,
                Some(no()),
                Some(yes()),
                Some(A),
            ),
            fc(
                "desired_made_required",
                "desired x",
                DP,
                fact::REQUIRED,
                Some(no()),
                Some(yes()),
                Some(B),
            ),
            added(
                "env_added",
                vec![],
                vec![node("env X", "env_var", &[])],
                vec![("env X", A)],
            ),
            removed(
                "env_removed",
                vec![],
                vec![node("env X", "env_var", &[])],
                vec![("env X", B)],
            ),
            fc(
                "env_mirrors_changed",
                "env X",
                "env_var",
                fact::MIRRORS,
                Some(t("--a")),
                Some(t("--b")),
                Some(B),
            ),
            added(
                "error_code_added",
                vec![],
                vec![node("error x", "error_code", &[])],
                vec![("error x", A)],
            ),
            removed(
                "error_code_removed",
                vec![],
                vec![node("error x", "error_code", &[])],
                vec![("error x", B)],
            ),
            fc(
                "error_exit_changed",
                "error x",
                "error_code",
                fact::EXIT,
                Some(Fact::Int(1)),
                Some(Fact::Int(3)),
                Some(B),
            ),
            fc(
                "error_retryable_changed",
                "error x",
                "error_code",
                fact::RETRYABLE,
                Some(t("true")),
                Some(t("conditional")),
                Some(B),
            ),
            fc(
                "doctor_exit_appeared",
                "error x",
                "error_code",
                fact::DOCTOR_EXIT,
                None,
                Some(Fact::Int(4)),
                Some(A),
            ),
            fc(
                "doctor_exit_changed",
                "error x",
                "error_code",
                fact::DOCTOR_EXIT,
                Some(Fact::Int(4)),
                Some(Fact::Int(5)),
                Some(B),
            ),
            removed(
                "warning_removed",
                vec![],
                vec![node("warning x", "warning_code", &[])],
                vec![("warning x", B)],
            ),
            removed(
                "phase_removed",
                vec![],
                vec![node("phase x", "phase", &[])],
                vec![("phase x", B)],
            ),
            removed(
                "fm_removed",
                vec![],
                vec![node("fm x", "failure_mode", &[])],
                vec![("fm x", B)],
            ),
            added(
                "fm_added",
                vec![],
                vec![node("fm x", "failure_mode", &[])],
                vec![("fm x", A)],
            ),
            added(
                "action_added",
                vec![],
                vec![node("action x", "action", &[(fact::DESTRUCTIVE, no())])],
                vec![("action x", A)],
            ),
            fc(
                "action_destructive_changed",
                "action x",
                "action",
                fact::DESTRUCTIVE,
                Some(no()),
                Some(yes()),
                Some(B),
            ),
            fc(
                "exit_code_name_changed",
                "exit general 5",
                "exit_code",
                fact::NAME,
                Some(t("locked")),
                Some(t("busy")),
                Some(B),
            ),
            fc(
                "file_version_lowered",
                "file k",
                "file_kind",
                fact::VERSION,
                Some(t("1.4")),
                Some(t("1.3")),
                Some(B),
            ),
            fc(
                "file_version_raised_alone",
                "file k",
                "file_kind",
                fact::VERSION,
                Some(t("1.3")),
                Some(t("1.4")),
                None,
            ),
            added(
                "unknown_kind_added",
                vec![],
                vec![node("x y", "frobnicate", &[])],
                vec![("x y", U)],
            ),
            fc(
                "unknown_kind_changed",
                "x y",
                "frobnicate",
                "zeal",
                Some(yes()),
                Some(no()),
                Some(U),
            ),
            fc(
                "unknown_fact_changed",
                SIZE,
                OP,
                "zeal",
                Some(yes()),
                Some(no()),
                Some(U),
            ),
            Case {
                name: "node_kind_changed",
                before: vec![node(SIZE, OP, &[])],
                after: vec![node(SIZE, OV, &[])],
                want: vec![(SIZE.into(), Some("kind"), U)],
                bump: Bump::Major,
            },
            fc(
                "keywords_changed",
                SIZE,
                OP,
                fact::KEYWORDS,
                Some(l(&["not"])),
                Some(l(&["if"])),
                Some(U),
            ),
        ]
    }

    #[test]
    fn classifies_each_case() {
        for case in cases() {
            let changes = classify(&man(&case.before), &man(&case.after));
            let got: Vec<(String, Option<&str>, Label)> = changes
                .changes
                .iter()
                .map(|c| (c.node.clone(), c.fact.as_deref(), c.label))
                .collect();
            let want: Vec<(String, Option<&str>, Label)> = case
                .want
                .iter()
                .map(|(n, f, l)| (n.clone(), *f, *l))
                .collect();
            let render = changes.render();
            assert_eq!(
                got, want,
                "{}: expected {want:?}, got {got:?}\n{render}",
                case.name
            );
            assert_eq!(
                changes.wire_bump(),
                case.bump,
                "{}: wire bump\n{render}",
                case.name
            );
        }
    }

    /// A persisted kind `k` at `version` with the field `x` or not.
    fn file(version: &str, with_x: bool) -> Vec<N> {
        let mut nodes = vec![
            node("file k", "file_kind", &[(fact::VERSION, t(version))]),
            node(
                "file k .",
                "file_position",
                &[(fact::TYPES, l(&["object"]))],
            ),
        ];
        if with_x {
            nodes.push(node(
                "file k x",
                "file_position",
                &[(fact::TYPES, l(&["string"])), (fact::REQUIRED, yes())],
            ));
        }
        nodes
    }

    /// A persisted kind `k` at `version` whose root is an untagged variant with `required`
    /// keys (and one more variant beside it, `<untagged=z>`), each key a string field.
    fn untagged_file(version: &str, required: &[&str]) -> Vec<N> {
        let variant = format!("file k <untagged={}>", required.join(","));
        let mut nodes = vec![
            node("file k", "file_kind", &[(fact::VERSION, t(version))]),
            node(&variant, "file_variant", &[]),
            node("file k <untagged=z>", "file_variant", &[]),
        ];
        for key in required {
            nodes.push(node(
                &format!("{variant}.{key}"),
                "file_position",
                &[(fact::TYPES, l(&["string"])), (fact::REQUIRED, yes())],
            ));
        }
        nodes
    }

    #[test]
    fn an_untagged_variant_that_gains_keys_is_extended_not_removed() {
        let changes = classify(
            &man(&untagged_file("1.0", &["a", "b"])),
            &man(&untagged_file("1.1", &["a", "b", "c"])),
        );
        let labels: Vec<(&str, Label, &str)> = changes
            .changes
            .iter()
            .map(|c| (c.node.as_str(), c.label, c.rule))
            .collect();
        assert_eq!(
            labels,
            [
                (
                    "file k <untagged=a,b,c>",
                    Label::Additive,
                    "variant extended (keys added)"
                ),
                ("file k <untagged=a,b,c>.c", Label::Additive, "field added"),
            ],
            "{changes:#?}"
        );
        assert_eq!(changes.kinds.len(), 1);
        assert!(changes.kinds[0].satisfied, "{:?}", changes.kinds);
    }

    #[test]
    fn an_untagged_variant_that_loses_or_trades_a_key_is_still_breaking() {
        for (case, after) in [
            ("a_key_lost", &["a"][..]),
            ("a_key_traded", &["a", "c"][..]),
        ] {
            let changes = classify(
                &man(&untagged_file("1.0", &["a", "b"])),
                &man(&untagged_file("1.1", after)),
            );
            assert!(
                changes
                    .changes
                    .iter()
                    .any(|c| c.label == Label::Breaking && c.rule == "variant removed"),
                "{case}: {changes:#?}"
            );
        }
    }

    #[test]
    fn two_variants_that_could_extend_one_are_left_to_the_plain_comparison() {
        let mut after = untagged_file("1.1", &["a", "b", "c"]);
        after.push(node("file k <untagged=a,b,d>", "file_variant", &[]));
        let changes = classify(&man(&untagged_file("1.0", &["a", "b"])), &man(&after));
        assert!(
            changes
                .changes
                .iter()
                .any(|c| c.label == Label::Breaking && c.rule == "variant removed"),
            "{changes:#?}"
        );
    }

    #[test]
    fn persisted_kinds_follow_their_own_versions() {
        let kind_bump = |required, satisfied, b: &str, c: &str| KindBump {
            kind: "k".into(),
            baseline: Some(b.into()),
            current: Some(c.into()),
            required,
            satisfied,
        };
        let cases = [
            (
                "file_field_added_with_minor_bump",
                file("1.3", false),
                file("1.4", true),
                Label::Additive,
                kind_bump(Bump::Minor, true, "1.3", "1.4"),
            ),
            (
                "file_field_added_without_bump",
                file("1.3", false),
                file("1.3", true),
                Label::Additive,
                kind_bump(Bump::Minor, false, "1.3", "1.3"),
            ),
            (
                "file_field_removed_with_minor_bump",
                file("1.3", true),
                file("1.4", false),
                Label::Breaking,
                kind_bump(Bump::Major, false, "1.3", "1.4"),
            ),
            (
                "file_field_removed_with_major_bump",
                file("1.3", true),
                file("2.0", false),
                Label::Breaking,
                kind_bump(Bump::Major, true, "1.3", "2.0"),
            ),
            (
                "file_version_unparseable",
                file("1.3", false),
                file("x", true),
                Label::Additive,
                kind_bump(Bump::Minor, false, "1.3", "x"),
            ),
        ];
        for (name, before, after, label, want) in cases {
            let changes = classify(&man(&before), &man(&after));
            let x: Vec<&Change> = changes
                .changes
                .iter()
                .filter(|c| c.node == "file k x")
                .collect();
            let render = changes.render();
            assert_eq!(x.len(), 1, "{name}\n{render}");
            assert_eq!(
                (x[0].label, x[0].persisted.as_deref()),
                (label, Some("k")),
                "{name}\n{render}"
            );
            assert_eq!(changes.kinds, vec![want], "{name}\n{render}");
            assert_eq!(
                changes.wire_bump(),
                Bump::None,
                "{name}: a persisted change is never a wire bump\n{render}"
            );
        }
        // A kind added or removed whole: the kind itself is wire; its positions are
        // persisted with no bump of their own.
        for (name, before, after, bump) in [
            ("file_kind_added", vec![], file("1.0", true), Bump::Minor),
            ("file_kind_removed", file("1.0", true), vec![], Bump::Major),
        ] {
            let changes = classify(&man(&before), &man(&after));
            assert!(changes.kinds.is_empty(), "{name}: {:?}", changes.kinds);
            assert_eq!(changes.wire_bump(), bump, "{name}");
            let wire: Vec<&str> = changes
                .changes
                .iter()
                .filter(|c| c.persisted.is_none())
                .map(|c| c.node.as_str())
                .collect();
            assert_eq!(wire, ["file k"], "{name}");
        }
    }

    #[test]
    fn format_mismatch_is_one_unclassified_change() {
        let before = man(&[node(SIZE, OP, &[])]);
        let mut after = man(&[node("output t other", OP, &[])]);
        after.format = FORMAT + 1;
        let changes = classify(&before, &after);
        assert_eq!(changes.changes.len(), 1);
        let change = &changes.changes[0];
        assert_eq!(
            (change.node.as_str(), change.label),
            ("format", Label::Unclassified)
        );
        assert_eq!(changes.wire_bump(), Bump::Major);
    }

    #[test]
    fn reordered_members_and_headers_change_nothing() {
        let nodes = vec![
            node("command t", CMD, &[(fact::ALIASES, l(&["a", "b"]))]),
            node(SIZE, OP, &[(fact::TYPES, l(&["integer"]))]),
        ];
        let mut reversed = nodes.clone();
        reversed.reverse();
        let shuffled = vec![
            node(
                "command t",
                CMD,
                &[(fact::ALIASES, Fact::list(["b", "a"]).unwrap())],
            ),
            nodes[1].clone(),
        ];
        assert!(
            classify(&man(&nodes), &man(&reversed)).is_empty(),
            "reordered_members"
        );
        assert!(
            classify(&man(&nodes), &man(&shuffled)).is_empty(),
            "shuffled list input"
        );
        let text = man(&nodes).render(&[]);
        let header_a = man(&nodes).render(&[
            ("header", &serde_json::json!({"frozen": true})),
            ("provenance", &serde_json::json!({"tag": "v0.1.0"})),
        ]);
        let header_b = man(&nodes).render(&[("header", &serde_json::json!({"frozen": false}))]);
        let parse = |t: &str| Manifest::parse(t).unwrap();
        assert!(
            classify(&parse(&text), &parse(&header_a)).is_empty(),
            "header_differs"
        );
        assert!(
            classify(&parse(&header_a), &parse(&header_b)).is_empty(),
            "header_differs"
        );
    }

    #[test]
    fn mixed_labels_bump_major_and_additive_with_persisted_bumps_minor() {
        let mixed_before: Vec<N> = file("1.3", false)
            .into_iter()
            .chain([cmd_t(), node(SIZE, OP, &[])])
            .collect();
        let mixed_after: Vec<N> = file("1.4", true)
            .into_iter()
            .chain([node("command u", CMD, &[])])
            .collect();
        let mixed = classify(&man(&mixed_before), &man(&mixed_after));
        assert_eq!(
            mixed.wire_bump(),
            Bump::Major,
            "mixed_labels\n{}",
            mixed.render()
        );
        let additive_before: Vec<N> = file("1.3", false).into_iter().chain([cmd_t()]).collect();
        let additive_after: Vec<N> = file("1.4", true)
            .into_iter()
            .chain([cmd_t(), node("command u", CMD, &[])])
            .collect();
        let additive = classify(&man(&additive_before), &man(&additive_after));
        assert_eq!(
            additive.wire_bump(),
            Bump::Minor,
            "only_additive_and_persisted\n{}",
            additive.render()
        );
        assert_eq!(additive.counts(), (2, 0, 0));
    }

    #[test]
    fn bump_is_satisfied() {
        let cases = [
            ("none_any", Bump::None, "1.0", "1.0", true),
            ("minor_needs_higher_minor", Bump::Minor, "1.0", "1.1", true),
            ("minor_same_version", Bump::Minor, "1.0", "1.0", false),
            ("major_satisfies_minor", Bump::Minor, "1.3", "2.0", true),
            ("major_needs_higher_major", Bump::Major, "1.0", "1.9", false),
            ("major_higher_major", Bump::Major, "1.0", "2.0", true),
            (
                "lower_never_satisfies_minor",
                Bump::Minor,
                "2.0",
                "1.9",
                false,
            ),
            (
                "lower_never_satisfies_major",
                Bump::Major,
                "2.0",
                "1.9",
                false,
            ),
        ];
        for (name, bump, b, c, want) in cases {
            assert_eq!(bump.is_satisfied(v(b), v(c)), want, "{name}");
        }
    }

    #[test]
    fn render_groups_labels_and_folds_children() {
        let before = man(&[
            cmd_t(),
            node("input t --a", FLAG, &[]),
            node("input t --b", FLAG, &[]),
            node("input t @1", POS, &[]),
            node("command s", CMD, &[]),
            node("output s data", OP, &[(fact::TYPES, l(&["object"]))]),
            node("output s data.mode", OP, &[(fact::TYPES, l(&["string"]))]),
        ]);
        let mut after_nodes = vec![
            node("command s", CMD, &[]),
            node("output s data", OP, &[(fact::TYPES, l(&["object"]))]),
            node(
                "output s data.mode",
                OP,
                &[(fact::TYPES, l(&["string"])), (fact::PATTERN, t("^a"))],
            ),
            node(
                "output s data.size_bytes",
                OP,
                &[
                    (fact::TYPES, l(&["integer"])),
                    (fact::REQUIRED, yes()),
                    (fact::MINIMUM, Fact::Int(0)),
                ],
            ),
        ];
        after_nodes.extend(file("1.0", false));
        let mut before_with_file = before.clone();
        before_with_file.nodes.extend(man(&file("1.0", true)).nodes);
        let after = man(&after_nodes);
        let rendered = classify(&before_with_file, &after).render();
        let removed: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("  - "))
            .collect();
        // The command's three arguments fold into its line; the persisted removal is in its
        // kind's block, not in a label group.
        assert_eq!(
            removed,
            [
                "  - command t  (command removed) (and 3 nodes below)",
                "  - file k x  string, required  (field removed)"
            ],
            "{rendered}"
        );
        assert!(
            rendered
                .lines()
                .any(|line| line == "  + output s data.size_bytes  integer, required, minimum 0"),
            "{rendered}"
        );
        let headers: Vec<&str> = rendered
            .lines()
            .filter(|line| !line.starts_with(' '))
            .collect();
        assert_eq!(
            headers,
            [
                "breaking (1):",
                "additive (2):",
                "persisted file k (1.0 -> 1.0) needs a major bump (1 change):"
            ],
            "{rendered}"
        );
    }

    #[test]
    fn rules_table_covers_every_fact_constant() {
        for name in fact::ALL {
            assert!(RULES.iter().any(|row| row.fact == *name), "{name}: no rule");
        }
        for row in RULES {
            assert!(
                fact::ALL.contains(&row.fact),
                "{}: a rule for a fact that is no constant",
                row.fact
            );
        }
    }

    /// The monotone rows and a pair of values each orders: `(row, smaller, larger)`.
    fn monotone() -> Vec<(&'static Row, Option<Fact>, Option<Fact>)> {
        RULES
            .iter()
            .filter_map(|row| {
                let pair = match row.rule {
                    Rule::MoreAdditive | Rule::MoreBreaking => {
                        (Some(l(&["a"])), Some(l(&["a", "b"])))
                    }
                    Rule::TrueAdditive | Rule::TrueBreaking => (None, Some(yes())),
                    Rule::LowerOutput | Rule::UpperOutput | Rule::LowerInput | Rule::UpperInput => {
                        (Some(Fact::Int(1)), Some(Fact::Int(2)))
                    }
                    _ => return None,
                };
                Some((row, pair.0, pair.1))
            })
            .collect()
    }

    fn reversed(label: Label) -> Label {
        match label {
            Label::Additive => Label::Breaking,
            Label::Breaking => Label::Additive,
            Label::Unclassified => Label::Unclassified,
        }
    }

    /// A small manifest: up to 20 nodes of the known kinds, with facts from their rows.
    fn arb_manifest() -> impl Strategy<Value = Manifest> {
        let facts_for = |kind: NodeKind| -> Vec<&'static str> {
            RULES
                .iter()
                .filter(|row| row.kinds.contains(&kind))
                .map(|row| row.fact)
                .collect()
        };
        prop::collection::vec(
            (
                0..NodeKind::ALL.len(),
                0u32..40,
                prop::collection::vec((0usize..64, 0i64..4), 0..4),
            ),
            0..20,
        )
        .prop_map(move |entries| {
            let mut nodes = BTreeMap::new();
            for (kind_index, n, facts) in entries {
                let kind = NodeKind::ALL[kind_index];
                let names = facts_for(kind);
                let mut node = Node {
                    kind: kind.wire().to_owned(),
                    facts: BTreeMap::new(),
                };
                for (fact_index, value) in facts {
                    if names.is_empty() {
                        break;
                    }
                    let name = names[fact_index % names.len()];
                    node.facts.insert(name.to_owned(), Fact::Int(value));
                }
                nodes.insert(format!("{} t{n}", kind.wire()), node);
            }
            Manifest {
                format: FORMAT,
                nodes,
            }
        })
    }

    proptest! {
        #[test]
        fn classify_identity_is_empty(m in arb_manifest()) {
            prop_assert!(classify(&m, &m).is_empty());
        }

        #[test]
        fn render_parse_round_trip(m in arb_manifest()) {
            prop_assert_eq!(Manifest::parse(&m.render(&[])).ok(), Some(m));
        }

        #[test]
        fn order_never_matters(m in arb_manifest(), seed in any::<u64>()) {
            let mut nodes: Vec<(String, Node)> = m.nodes.clone().into_iter().collect();
            let n = nodes.len().max(1);
            nodes.rotate_left(usize::try_from(seed % n as u64).unwrap_or(0));
            let rebuilt = Manifest { format: m.format, nodes: nodes.into_iter().collect() };
            prop_assert_eq!(rebuilt.render(&[]), m.render(&[]));
            prop_assert!(classify(&m, &rebuilt).is_empty());
        }

        #[test]
        fn monotone_rules_reverse(index in 0usize..64) {
            let rows = monotone();
            let (row, small, large) = &rows[index % rows.len()];
            let kind = row.kinds[0];
            let with = |value: &Option<Fact>| {
                let mut node = Node { kind: kind.wire().to_owned(), facts: BTreeMap::new() };
                if let Some(value) = value {
                    node.facts.insert(row.fact.to_owned(), value.clone());
                }
                Manifest { format: FORMAT, nodes: [("x y".to_owned(), node)].into() }
            };
            let up = classify(&with(small), &with(large));
            let down = classify(&with(large), &with(small));
            prop_assert_eq!(up.changes.len(), 1);
            prop_assert_eq!(down.changes.len(), 1);
            prop_assert_eq!(down.changes[0].label, reversed(up.changes[0].label), "{}", row.fact);
        }
    }
}
