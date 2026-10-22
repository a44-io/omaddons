//! Preview binding (M3-D46): every `--dry-run` of a destructive command returns a
//! precondition token (`data.precondition`), and the real run with `--if-match <token>`
//! computes the token again from its own plan before it stages anything, refusing with
//! `plan_stale` (not retryable) when a part differs. Without `--if-match` nothing changes.
//!
//! A token is `pc1:` and four fields of 16 hex digits, the first 64 bits of a SHA-256 of
//! each part's canonical JSON, in order: the command path, the targets (keys, with the
//! record generations planned against), the selection (what the run would change) and
//! what it observed. Separate fields let a refusal name the parts that changed with no
//! stored state: a token holds exactly as long as the world it describes, and it is no
//! credential. M4's stored plans can embed the same [`Precondition`].

use std::collections::BTreeMap;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::engine::plan::Plan;
use crate::engine::{Binding, UndoOutcome as EngineUndo};
use crate::error::{Error, PlanStaleSource, Result, Suggestion};
use crate::model::{TargetKey, TxId};

/// The command paths whose runs bind to a preview; `--if-match` is a usage error on any
/// other.
pub const BINDING_COMMANDS: &[&str] = &[
    "remove",
    "undo",
    "backup restore",
    "backup prune",
    "errors dismiss",
];

/// The token format's version and separator.
const PREFIX: &str = "pc1:";
/// Hex digits per part.
const FIELD: usize = 16;

/// The parts of a precondition, in token order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Command,
    Targets,
    Selection,
    Observed,
}

impl Part {
    pub const ALL: [Self; 4] = [
        Self::Command,
        Self::Targets,
        Self::Selection,
        Self::Observed,
    ];

    /// The name `details.changed` uses.
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Targets => "targets",
            Self::Selection => "selection",
            Self::Observed => "observed",
        }
    }
}

/// What a destructive run would do, from the plan it executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Precondition {
    command: &'static str,
    /// Each target's key and the generation planned against; `None` for a part that
    /// binds to keys only (`errors dismiss`).
    targets: Vec<(TargetKey, Option<u64>)>,
    selection: Value,
    observed: Value,
}

impl Precondition {
    pub fn new(
        command: &'static str,
        targets: Vec<(TargetKey, Option<u64>)>,
        selection: Value,
        observed: Value,
    ) -> Self {
        Self {
            command,
            targets,
            selection,
            observed,
        }
    }

    /// `pc1:` and the four parts' fields.
    pub fn token(&self) -> String {
        let mut token = PREFIX.to_owned();
        for field in self.fields() {
            token.push_str(&field);
        }
        token
    }

    fn fields(&self) -> [String; 4] {
        self.canonical().map(|text| field(&text))
    }

    /// Each part's canonical JSON, in token order.
    fn canonical(&self) -> [String; 4] {
        let mut targets = self.targets.clone();
        targets.sort();
        targets.dedup();
        let targets: Vec<Value> = targets
            .iter()
            .map(|(key, generation)| match generation {
                Some(generation) => json!({"key": key.to_string(), "generation": generation}),
                None => json!({"key": key.to_string()}),
            })
            .collect();
        [
            json!(self.command),
            Value::Array(targets),
            self.selection.clone(),
            self.observed.clone(),
        ]
        .map(|value| {
            let mut text = String::new();
            canonical(&value, &mut text);
            text
        })
    }
}

/// The first 64 bits of the SHA-256 of a part's canonical JSON, in hex.
fn field(canonical: &str) -> String {
    Sha256::digest(canonical.as_bytes())
        .iter()
        .take(FIELD / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `value` as JSON with every object's keys sorted, whatever map type built it. Callers
/// sort arrays whose order carries no meaning and use integers, never floats.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Whether `token` is a well-formed precondition token: `pc1:` and 64 lowercase hex
/// digits.
pub fn is_token(token: &str) -> bool {
    fields_of(token).is_some()
}

/// A well-formed token's four fields.
fn fields_of(token: &str) -> Option<[&str; 4]> {
    let hex = token.strip_prefix(PREFIX)?;
    if hex.len() != 4 * FIELD || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    Some([0, 1, 2, 3].map(|i| &hex[i * FIELD..(i + 1) * FIELD]))
}

/// `Ok` when `expected` (`--if-match`) is `actual`'s token or absent; otherwise
/// `plan_stale` naming the parts that differ, whose fix is `preview`.
pub fn check(expected: Option<&str>, actual: &Precondition, preview: &[String]) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let fields = actual.fields();
    let changed: Vec<Part> = match fields_of(expected) {
        Some(given) => Part::ALL
            .into_iter()
            .zip(given)
            .zip(&fields)
            .filter(|((_, given), now)| *given != now.as_str())
            .map(|((part, _), _)| part)
            .collect(),
        None => Part::ALL.to_vec(),
    };
    if changed.is_empty() {
        Ok(())
    } else {
        Err(stale(actual.command, &changed, preview))
    }
}

/// `plan_stale` for a run whose `changed` parts are not what its preview showed: not
/// retryable (the same token can only fail again), with `details.changed` and the
/// preview (`preview`) as the fix.
pub fn stale(command: &str, changed: &[Part], preview: &[String]) -> Error {
    let names: Vec<&str> = changed.iter().map(|p| p.wire()).collect();
    Error::plan_stale(
        PlanStaleSource::Apply,
        format!(
            "this {command} is no longer what its preview showed ({} changed); nothing was changed: preview it again",
            names.join(", ")
        ),
        Suggestion::command(preview.to_vec()),
    )
    .details(json!({ "changed": names }))
}

/// The preview of the run `argv`: the same command without `--if-match` and its token,
/// with `--dry-run`, and without presentation flags.
pub fn preview_argv(argv: &[String]) -> Vec<String> {
    let mut argv = crate::cli::without_flag(argv, "--if-match");
    if !argv.iter().any(|a| a == "--dry-run") {
        // Before any `--`, after which everything is a positional.
        let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
        argv.insert(at, "--dry-run".to_owned());
    }
    crate::cli::fix_argv(&argv)
}

// ---------------------------------------------------------------------------------------
// `remove`.

/// `remove`'s precondition: each target's key and generation, and the exact folders it
/// would move out with the records it would change; nothing observed beyond the targets
/// (their generations cover what the selection was made from).
pub fn of_remove(plan: &Plan) -> Precondition {
    let mut targets = Vec::new();
    let mut selection = BTreeMap::new();
    for tp in &plan.targets {
        let Some(key) = tp.key else {
            continue;
        };
        targets.push((key, Some(tp.generation)));
        let mut ops: Vec<Value> = tp
            .ops
            .iter()
            .map(|o| json!({"op": o.op, "folder": o.folder.as_str(), "addon": o.addon}))
            .collect();
        ops.sort_by_key(Value::to_string);
        let mut changes: Vec<Value> = tp
            .changes
            .iter()
            .map(|c| {
                json!({
                    "addon": c.addon,
                    "kind": c.kind,
                    "fields": c.fields,
                    "removed": c.folders.removed,
                    "recorded": c.folders.recorded,
                })
            })
            .collect();
        changes.sort_by_key(Value::to_string);
        selection.insert(key.to_string(), json!({"ops": ops, "changes": changes}));
    }
    Precondition::new("remove", targets, json!(selection), Value::Null)
}

/// A `remove` run bound to its preview's token.
pub struct RemoveBinding {
    pub token: String,
    pub preview: Vec<String>,
}

impl Binding for RemoveBinding {
    fn plan(&self, plan: &Plan) -> Result<()> {
        check(Some(&self.token), &of_remove(plan), &self.preview)
    }

    fn undo(&self, _planned: &EngineUndo) -> Result<()> {
        Ok(())
    }

    fn moved(&self, _target: &str) -> Error {
        stale("remove", &[Part::Targets], &self.preview)
    }
}

// ---------------------------------------------------------------------------------------
// `undo`.

/// One transaction an undo selected: the target it changed, and the undo's plan or why
/// it cannot be undone.
pub type UndoPlanned<'a> = (
    TxId,
    TargetKey,
    std::result::Result<&'a EngineUndo, &'a Error>,
);

/// `undo`'s precondition over every transaction it would revert: each target's key and
/// generation; the transactions, their folder operations and the SavedVariables files
/// each would restore; and, observed, each one's verdict (`ok`, or why it cannot be
/// undone) and those files' stamps now.
pub fn of_undo(planned: &[UndoPlanned<'_>]) -> Precondition {
    let mut targets = Vec::new();
    let mut selection = Vec::new();
    let mut observed = Vec::new();
    for (tx, key, result) in planned {
        match result {
            Ok(undo) => {
                let facts = &undo.plan;
                targets.push((*key, Some(facts.generation)));
                let mut ops: Vec<Value> = facts
                    .ops
                    .iter()
                    .map(|op| serde_json::to_value(op).unwrap_or(Value::Null))
                    .collect();
                ops.sort_by_key(Value::to_string);
                let mut paths: Vec<&str> = facts
                    .saved_variables
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .collect();
                paths.sort_unstable();
                selection.push(json!({
                    "tx": tx.to_string(),
                    "ops": ops,
                    "saved_variables": paths,
                }));
                let mut stamps: Vec<Value> = facts
                    .saved_variables
                    .iter()
                    .map(|(path, stamp)| json!({"path": path, "stamp": stamp}))
                    .collect();
                stamps.sort_by_key(Value::to_string);
                observed.push(json!({
                    "tx": tx.to_string(),
                    "verdict": "ok",
                    "saved_variables": stamps,
                }));
            }
            Err(error) => {
                targets.push((*key, None));
                selection.push(json!({ "tx": tx.to_string() }));
                observed.push(json!({ "tx": tx.to_string(), "verdict": error.code.wire() }));
            }
        }
    }
    selection.sort_by_key(Value::to_string);
    observed.sort_by_key(Value::to_string);
    Precondition::new(
        "undo",
        targets,
        Value::Array(selection),
        Value::Array(observed),
    )
}

/// An `undo` run bound to its preview: each transaction's plan, made before staging and
/// again under the lock, must be what the preview pass planned (its token per
/// transaction).
pub struct UndoBinding {
    pub tokens: BTreeMap<TxId, String>,
    pub preview: Vec<String>,
}

impl Binding for UndoBinding {
    fn plan(&self, _plan: &Plan) -> Result<()> {
        Ok(())
    }

    fn undo(&self, planned: &EngineUndo) -> Result<()> {
        let Some(token) = self.tokens.get(&planned.undoes) else {
            return Err(stale("undo", &[Part::Selection], &self.preview));
        };
        let Some(key) = planned.outcome.key else {
            return Err(stale("undo", &[Part::Targets], &self.preview));
        };
        let actual = of_undo(&[(planned.undoes, key, Ok(planned))]);
        check(Some(token), &actual, &self.preview)
    }

    fn moved(&self, _target: &str) -> Error {
        stale("undo", &[Part::Targets], &self.preview)
    }
}

// ---------------------------------------------------------------------------------------
// `errors dismiss`.

/// One client's part of an `errors dismiss`: its key, the key its dismissals are filed
/// under, and what was observed there.
pub struct DismissPart<'a> {
    pub key: Option<TargetKey>,
    pub filed_as: &'a str,
    pub observation: &'a crate::errors::Observation,
}

/// `errors dismiss`'s precondition (M3-D46): the target keys without generations (an
/// update between the read and the dismissal does not change what was shown), the
/// databases it would acknowledge (those read whole), and each database's file stamp and
/// session as observed. A session ended since the read changes `observed`.
pub fn of_dismiss(parts: &[DismissPart<'_>]) -> Precondition {
    let mut selection = Vec::new();
    let mut observed = Vec::new();
    for part in parts {
        for db in &part.observation.databases {
            let key = crate::errors::ack::db_key(&db.path);
            if db.state.complete() {
                selection.push(json!({"target": part.filed_as, "database": key}));
            }
            observed.push(json!({
                "target": part.filed_as,
                "database": key,
                "stamp": db.stamp.fingerprint(),
                "session": db.session,
            }));
        }
    }
    selection.sort_by_key(Value::to_string);
    observed.sort_by_key(Value::to_string);
    Precondition::new(
        "errors dismiss",
        parts
            .iter()
            .filter_map(|p| p.key)
            .map(|k| (k, None))
            .collect(),
        Value::Array(selection),
        Value::Array(observed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorBody, ErrorCode};

    const KEY: &str = "01JB8Q2V9W0000000000000000";
    const OTHER: &str = "01JB8Q2V9W0000000000000001";

    fn key(s: &str) -> TargetKey {
        s.parse().unwrap()
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn fixture() -> Precondition {
        Precondition::new(
            "remove",
            vec![(key(KEY), Some(7))],
            json!({ KEY: {"ops": [{"op": "remove", "folder": "Foo", "addon": "Foo"}]} }),
            Value::Null,
        )
    }

    #[test]
    fn a_fixture_token_never_drifts() {
        // The fields are the first 8 bytes of the SHA-256 of `"remove"`,
        // `[{"generation":7,"key":"<KEY>"}]`, `{"<KEY>":{"ops":[{"addon":"Foo","folder":
        // "Foo","op":"remove"}]}}` and `null`, as `sha256sum` gives them.
        let token = fixture().token();
        assert_eq!(
            token,
            "pc1:66f3d68b7627c9f62b9796bcd091931ef7f2567b601b33f774234e98afe7498f"
        );
        assert!(is_token(&token));
        assert_eq!(fixture().token(), token, "the same inputs, the same token");
    }

    #[test]
    fn parts_are_canonical_json() {
        let p = Precondition::new(
            "undo",
            vec![
                (key(OTHER), Some(2)),
                (key(KEY), None),
                (key(OTHER), Some(2)),
            ],
            json!({"b": [3, 1], "a": {"z": null, "y": "é\"x"}}),
            json!([true, 0]),
        );
        let [command, targets, selection, observed] = p.canonical();
        assert_eq!(command, r#""undo""#);
        assert_eq!(
            targets,
            format!(r#"[{{"key":"{KEY}"}},{{"generation":2,"key":"{OTHER}"}}]"#),
            "sorted by key, duplicates once; a key-only entry has no generation"
        );
        assert_eq!(selection, r#"{"a":{"y":"é\"x","z":null},"b":[3,1]}"#);
        assert_eq!(observed, "[true,0]");
        // Target order and repeats never move the token.
        let reordered = Precondition::new(
            "undo",
            vec![(key(KEY), None), (key(OTHER), Some(2))],
            p.selection.clone(),
            p.observed.clone(),
        );
        assert_eq!(reordered.token(), p.token());
    }

    #[test]
    fn every_covered_input_moves_its_own_field() {
        let base = fixture();
        let cases: [(Precondition, Part); 5] = [
            (
                Precondition {
                    command: "undo",
                    ..base.clone()
                },
                Part::Command,
            ),
            (
                Precondition {
                    targets: vec![(key(KEY), Some(8))],
                    ..base.clone()
                },
                Part::Targets,
            ),
            (
                Precondition {
                    targets: vec![(key(KEY), None)],
                    ..base.clone()
                },
                Part::Targets,
            ),
            (
                Precondition {
                    selection: json!({ KEY: {"ops": []} }),
                    ..base.clone()
                },
                Part::Selection,
            ),
            (
                Precondition {
                    observed: json!([{"stamp": "1:2"}]),
                    ..base.clone()
                },
                Part::Observed,
            ),
        ];
        let preview = argv(&["omaddons", "remove", "Foo", "--dry-run"]);
        for (changed, part) in cases {
            assert_ne!(changed.token(), base.token(), "{part:?}");
            let error = check(Some(&base.token()), &changed, &preview).unwrap_err();
            let body = ErrorBody::from(error);
            assert_eq!(body.code, ErrorCode::PlanStale);
            assert!(!body.retryable, "the same token can only fail again");
            assert_eq!(body.details["changed"], json!([part.wire()]), "{part:?}");
            assert_eq!(body.fix_argv, Some(preview.clone()));
        }
        // Several parts at once, in token order.
        let both = Precondition {
            command: "undo",
            observed: json!(1),
            ..base.clone()
        };
        let body = ErrorBody::from(check(Some(&base.token()), &both, &preview).unwrap_err());
        assert_eq!(body.details["changed"], json!(["command", "observed"]));
    }

    #[test]
    fn a_matching_or_absent_token_passes_and_a_malformed_one_changes_everything() {
        let p = fixture();
        let preview = argv(&["omaddons", "remove", "Foo", "--dry-run"]);
        check(None, &p, &preview).unwrap();
        check(Some(&p.token()), &p, &preview).unwrap();
        let body = ErrorBody::from(check(Some("pc1:nope"), &p, &preview).unwrap_err());
        assert_eq!(
            body.details["changed"],
            json!(["command", "targets", "selection", "observed"])
        );
    }

    #[test]
    fn tokens_are_pc1_and_64_lowercase_hex_digits() {
        let good = format!("pc1:{}", "0123456789abcdef".repeat(4));
        assert!(is_token(&good));
        for bad in [
            "",
            "pc1:",
            "pc2:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "pc1:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "pc1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
            "pc1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
            "pc1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg",
        ] {
            assert!(!is_token(bad), "{bad}");
        }
    }

    #[test]
    fn the_preview_swaps_if_match_for_dry_run() {
        let token = format!("pc1:{}", "0".repeat(64));
        assert_eq!(
            preview_argv(&argv(&[
                "omaddons",
                "remove",
                "Foo",
                "-t",
                "retail",
                "--if-match",
                &token,
                "--json",
            ])),
            argv(&["omaddons", "remove", "Foo", "-t", "retail", "--dry-run"])
        );
        let attached = format!("--if-match={token}");
        assert_eq!(
            preview_argv(&argv(&["omaddons", "undo", &attached, "--", "01JB8Q2V"])),
            argv(&["omaddons", "undo", "--dry-run", "--", "01JB8Q2V"]),
            "before any --"
        );
        assert_eq!(
            preview_argv(&argv(&[
                "omaddons",
                "undo",
                "--dry-run",
                "--if-match",
                &token
            ])),
            argv(&["omaddons", "undo", "--dry-run"]),
            "never twice"
        );
    }

    #[test]
    fn the_binding_commands_are_the_five_of_m3_d46() {
        assert_eq!(
            BINDING_COMMANDS,
            [
                "remove",
                "undo",
                "backup restore",
                "backup prune",
                "errors dismiss"
            ]
        );
        assert_eq!(
            Part::ALL.map(Part::wire),
            ["command", "targets", "selection", "observed"]
        );
    }
}
