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

use crate::app::outcome::SkipReason;
use crate::cli::Context;
use crate::engine::plan::{Change, Plan, PlannedOp, PlannedRelease, TargetPlan};
use crate::engine::witness::{self, ApprovalWitness, WitnessScope};
use crate::engine::{Binding, UndoOutcome as EngineUndo};
use crate::error::{Error, PlanStaleSource, Result, Suggestion};
pub use crate::model::TargetRef;
use crate::model::{Actor, BackupId, FolderName, Target, TargetKey, TxId};

/// The command paths whose runs bind to a preview; `--if-match` is a usage error on any
/// other.
pub const BINDING_COMMANDS: &[&str] = &[
    "remove",
    "undo",
    "backup restore",
    "backup prune",
    "errors dismiss",
    "install",
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
    /// Each target (its key, or its provisional id while unregistered) and the
    /// generation planned against; `None` for a part that binds to keys only (`errors
    /// dismiss`, `backup prune`).
    targets: Vec<(TargetRef, Option<u64>)>,
    selection: Value,
    observed: Value,
}

impl Precondition {
    pub fn new(
        command: &'static str,
        targets: Vec<(TargetRef, Option<u64>)>,
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

    /// The four parts' canonical JSON texts, in token order: what each field hashes (for
    /// test logs, and a stored plan's `precondition.parts`).
    pub fn parts(&self) -> [String; 4] {
        self.canonical()
    }

    /// Each part's canonical JSON, in token order. Keys sort before provisional ids.
    fn canonical(&self) -> [String; 4] {
        let mut targets = self.targets.clone();
        targets.sort();
        targets.dedup();
        let targets: Vec<Value> = targets
            .iter()
            .map(|(target, generation)| match generation {
                Some(generation) => {
                    json!({"key": target.to_string(), "generation": generation})
                }
                None => json!({"key": target.to_string()}),
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

/// `remove`'s precondition: each target (its key, or its provisional id while
/// unregistered) with its generation, and the exact folders it would move out with the
/// records it would change; nothing observed beyond the targets (their generations cover
/// what the selection was made from).
pub fn of_remove(plan: &Plan) -> Precondition {
    let mut targets = Vec::new();
    let mut selection = BTreeMap::new();
    for tp in &plan.targets {
        let target = TargetRef::of(tp.key, &tp.target);
        targets.push((target.clone(), Some(tp.generation)));
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
        selection.insert(target.to_string(), json!({"ops": ops, "changes": changes}));
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
// The engine commands: `install`, `update`, `pin`, `unpin`, `adopt`, `sync`, `copy`.

/// What a command decided above the engine, which its token covers too.
#[derive(Debug, Clone, Default)]
pub struct PlanExtras<'a> {
    /// `copy`: `--from`'s key and record generation.
    pub copy_source: Option<(TargetKey, u64)>,
    /// `sync`: the lowercase hex SHA-256 of the desired-state file's bytes.
    pub desired_sha256: Option<String>,
    /// `update`: what it chose not to touch, as (target, addon name, why).
    pub update_skips: &'a [(TargetRef, String, SkipReason)],
}

/// An engine command's precondition (M4-D19, M4-D60), from the plan it will execute:
/// each target with its generation; per target, the folder ops, the changes, the
/// releases (a deferred one by its source and key, never the URL it resolves to) and
/// whether it creates `Interface/AddOns`; and, observed, this build's id, the command's
/// extras and each target's approval witness digest (`null` when none was collected).
/// Text a human reads (warnings, notes) never enters it.
pub fn of_plan(
    command: &'static str,
    plan: &Plan,
    witnesses: &[ApprovalWitness],
    extras: &PlanExtras<'_>,
) -> Precondition {
    of_plan_at(
        crate::build_info::BUILD_ID,
        command,
        plan,
        witnesses,
        extras,
    )
}

/// [`of_plan`] for the build `build_id`.
fn of_plan_at(
    build_id: &str,
    command: &'static str,
    plan: &Plan,
    witnesses: &[ApprovalWitness],
    extras: &PlanExtras<'_>,
) -> Precondition {
    let mut targets = Vec::new();
    let mut selection = BTreeMap::new();
    let mut witnessed = BTreeMap::new();
    for tp in &plan.targets {
        let target = TargetRef::of(tp.key, &tp.target);
        targets.push((target.clone(), Some(tp.generation)));
        selection.insert(target.to_string(), target_selection(tp));
        // Paired by target, never by position.
        let digest = witnesses
            .iter()
            .find(|w| w.target == target)
            .map(witness_digest);
        witnessed.insert(target.to_string(), json!(digest));
    }
    let mut skips: Vec<(String, &str, Value)> = extras
        .update_skips
        .iter()
        .map(|(target, addon, reason)| (target.to_string(), addon.as_str(), json!(reason)))
        .collect();
    skips.sort_by(|a, b| (&a.0, a.1, a.2.to_string()).cmp(&(&b.0, b.1, b.2.to_string())));
    let skips: Vec<Value> = skips
        .into_iter()
        .map(|(target, addon, reason)| json!({"target": target, "addon": addon, "reason": reason}))
        .collect();
    let observed = json!({
        "build_id": build_id,
        "copy_source": extras
            .copy_source
            .map(|(key, generation)| json!({"key": key.to_string(), "generation": generation})),
        "desired_sha256": extras.desired_sha256,
        "update_skips": skips,
        "witnesses": witnessed,
    });
    Precondition::new(command, targets, json!(selection), observed)
}

/// One target's selection: its folder ops, changes and releases, canonically ordered,
/// and whether it creates `Interface/AddOns`.
fn target_selection(tp: &TargetPlan) -> Value {
    let mut ops: Vec<&PlannedOp> = tp.ops.iter().collect();
    ops.sort_by_key(|o| (o.folder.key(), op_wire(o), o.addon.as_str()));
    let ops: Vec<Value> = ops
        .into_iter()
        .map(|o| json!({"op": op_wire(o), "folder": o.folder.as_str(), "addon": o.addon}))
        .collect();
    let mut changes: Vec<&Change> = tp.changes.iter().collect();
    changes.sort_by_key(|c| {
        (
            c.addon.to_ascii_lowercase(),
            c.addon.as_str(),
            c.r#ref.to_string(),
        )
    });
    let changes: Vec<Value> = changes.into_iter().map(change_value).collect();
    let mut releases: Vec<Value> = tp.releases.iter().map(release_value).collect();
    releases.sort_by_key(|r| {
        (
            r["release_id"].as_str().unwrap_or_default().to_owned(),
            r.to_string(),
        )
    });
    json!({
        "ops": ops,
        "changes": changes,
        "releases": releases,
        "creates_addons_dir": tp.creates_addons_dir,
    })
}

fn op_wire(op: &PlannedOp) -> String {
    json!(op.op).as_str().unwrap_or_default().to_owned()
}

/// Folder names as spelled, sorted by key.
fn folder_names(folders: &[FolderName]) -> Vec<&str> {
    let mut sorted: Vec<&FolderName> = folders.iter().collect();
    sorted.sort_by_key(|f| f.key());
    sorted.into_iter().map(FolderName::as_str).collect()
}

fn change_value(c: &Change) -> Value {
    let mut fields: Vec<&str> = c.fields.iter().map(|f| f.wire()).collect();
    fields.sort_unstable();
    let mut skipped: Vec<_> = c.folders.skipped.iter().collect();
    skipped.sort_by_key(|s| s.name.key());
    json!({
        "addon": c.addon,
        "ref": c.r#ref.to_string(),
        "kind": c.kind,
        "fields": fields,
        "from_version": c.from_version,
        "to_version": c.to_version,
        "folders": {
            "added": folder_names(&c.folders.added),
            "replaced": folder_names(&c.folders.replaced),
            "removed": folder_names(&c.folders.removed),
            "recorded": folder_names(&c.folders.recorded),
            "skipped": skipped,
        },
    })
}

/// A release by what identifies it; a deferred one by its source and key, never the URL
/// a fresh download resolves to (a blob-cache hit has none).
fn release_value(r: &PlannedRelease) -> Value {
    match &r.deferred {
        Some(deferred) => json!({
            "release_id": r.release_id,
            "source": deferred.source,
            "key": deferred.key,
            "expected_hash": r.expected_hash,
            "blob_sha256": r.blob_sha256,
        }),
        None => json!({
            "release_id": r.release_id,
            "url": r.url,
            "expected_hash": r.expected_hash,
            "blob_sha256": r.blob_sha256,
        }),
    }
}

/// The lowercase hex SHA-256 (untruncated) of the canonical JSON of the witness's public
/// parts, in M4-D60's order: marker, folders, absent names, inputs, dependency choices,
/// judge. Its private stamps and cost never enter it.
pub fn witness_digest(w: &ApprovalWitness) -> String {
    let folders: Vec<Value> = w
        .folders
        .iter()
        .map(|f| json!({"role": f.role.wire(), "key": f.key, "digest": f.digest.to_string()}))
        .collect();
    let inputs: Vec<Value> = w
        .inputs
        .iter()
        .map(|i| json!({"kind": i.kind.wire(), "source": i.source, "digest": i.digest}))
        .collect();
    let dependencies: Vec<Value> = w
        .dependencies
        .iter()
        .map(|d| {
            json!({
                "requester": d.requester,
                "folder": d.folder,
                "by": d.by.wire(),
                "detail": d.by.detail(),
            })
        })
        .collect();
    let value = json!([w.marker, folders, w.absent, inputs, dependencies, w.judge]);
    let mut text = String::new();
    canonical(&value, &mut text);
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// An engine command's run bound to its preview's token (M4-D19): its own plan's token,
/// witnesses and extras included, must be the preview's before anything is staged (on a
/// dry run too), and its targets' generations must not have moved under the lock. D8
/// adds the under-lock witness and operation checks to this type; its fields stay
/// private so it can, without touching a call site.
pub struct PlanBinding<'a> {
    command: &'static str,
    expected: Option<String>,
    extras: PlanExtras<'a>,
    preview: Vec<String>,
}

impl<'a> PlanBinding<'a> {
    /// `expected: None` binds nothing: it computes no token and checks nothing.
    pub fn new(
        command: &'static str,
        expected: Option<String>,
        extras: PlanExtras<'a>,
        preview: Vec<String>,
    ) -> Self {
        Self {
            command,
            expected,
            extras,
            preview,
        }
    }

    /// This run's binding: `Some` exactly when `--if-match` was given, dry run or not,
    /// with the run's preview ([`preview_argv`]) as the fix.
    pub fn of_run(ctx: &Context, command: &'static str, extras: PlanExtras<'a>) -> Option<Self> {
        ctx.if_match
            .clone()
            .map(|token| Self::new(command, Some(token), extras, preview_argv(&ctx.argv)))
    }
}

impl Binding for PlanBinding<'_> {
    fn plan(&self, plan: &Plan) -> Result<()> {
        let Some(expected) = &self.expected else {
            return Ok(());
        };
        let actual = of_plan(self.command, plan, &plan.witnesses, &self.extras);
        check(Some(expected), &actual, &self.preview)
    }

    fn undo(&self, _planned: &EngineUndo) -> Result<()> {
        Ok(())
    }

    fn moved(&self, _target: &str) -> Error {
        stale(self.command, &[Part::Targets], &self.preview)
    }
}

/// A dry run's token: [`of_plan`] over the plan it ran, its witnesses and `extras`.
pub fn plan_token(command: &'static str, plan: &Plan, extras: &PlanExtras<'_>) -> String {
    of_plan(command, plan, &plan.witnesses, extras).token()
}

/// The witness scope of this run: the default scope for a dry run or a bound run, `None`
/// for an unbound real run, which compares nothing and so collects nothing.
pub fn witnessing(ctx: &Context) -> Option<WitnessScope> {
    collects_witnesses(ctx).then(WitnessScope::default)
}

/// Whether this run compares or returns a token: a dry run, or a run given `--if-match`.
fn collects_witnesses(ctx: &Context) -> bool {
    ctx.dry_run || ctx.if_match.is_some()
}

/// One in-scope target of a run that found nothing to change, with the record
/// generation it was judged at.
pub struct Idle<'a> {
    pub target: &'a Target,
    pub generation: u64,
}

/// The token of a run that changes nothing on `targets`: [`of_plan`] over
/// [`Plan::idle`], each target witnessed with [`witness::bare`] (its marker and judge),
/// plus `extras`. So a preview with work and a run with none differ in `selection`, and
/// two runs with nothing to do agree. An unreadable marker fails it (fail closed).
pub fn of_noop(
    command: &'static str,
    targets: &[Idle<'_>],
    extras: &PlanExtras<'_>,
) -> Result<Precondition> {
    let entries: Vec<(&Target, u64)> = targets.iter().map(|i| (i.target, i.generation)).collect();
    let mut plan = Plan::idle(&entries, &[], Actor::Agent);
    plan.witnesses = targets
        .iter()
        .map(|i| witness::bare(i.target))
        .collect::<Result<_>>()?;
    Ok(of_plan(command, &plan, &plan.witnesses, extras))
}

/// The token check of a run that found nothing to change and never reaches the engine
/// (M4-D19, "on each command's no-op path"). An unbound real run reads nothing and gives
/// `Ok(None)`. Otherwise the run's [`of_noop`] token is checked against `--if-match`
/// (`plan_stale` when it differs), and a dry run gives it back.
pub fn check_noop(
    ctx: &Context,
    command: &'static str,
    targets: &[Idle<'_>],
    extras: &PlanExtras<'_>,
) -> Result<Option<String>> {
    if !collects_witnesses(ctx) {
        return Ok(None);
    }
    let actual = of_noop(command, targets, extras)?;
    check(ctx.if_match.as_deref(), &actual, &preview_argv(&ctx.argv))?;
    Ok(ctx.dry_run.then(|| actual.token()))
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
                targets.push((TargetRef::Key(*key), Some(facts.generation)));
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
                let mut seen = json!({
                    "tx": tx.to_string(),
                    "verdict": "ok",
                    "saved_variables": stamps,
                });
                // The undo of a whole restore: the live `WTF/` as previewed, so a play
                // session before the run refuses it (M3-D46). Absent otherwise, so no
                // other token moves.
                if let (Some(wtf), Some(map)) = (&facts.wtf, seen.as_object_mut()) {
                    map.insert("wtf".to_owned(), json!(wtf));
                }
                observed.push(seen);
            }
            Err(error) => {
                targets.push((TargetRef::Key(*key), None));
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
            .map(|k| (TargetRef::Key(k), None))
            .collect(),
        Value::Array(selection),
        Value::Array(observed),
    )
}

// ---------------------------------------------------------------------------------------
// `backup restore`.

/// What a `backup restore` would do, for its precondition.
pub struct RestorePart<'a> {
    pub key: Option<TargetKey>,
    /// The target's record generation it was planned against.
    pub generation: u64,
    pub backup: BackupId,
    pub files: &'a crate::engine::restore::RestoreFiles,
    /// The SHA-256 of the source's `backup.json`, `None` when it does not read.
    pub backup_json: Option<String>,
    /// The live `WTF/`'s stamp digest, `None` without one.
    pub wtf: Option<String>,
}

/// `backup restore`'s precondition (M3-D46): the target's key with the generation planned
/// against; the backup, the mode (`whole`; `folders` is a partial restore's) and the
/// counts; and, observed, the source's `backup.json` and the live `WTF/`'s stamp digest,
/// because the game changes `WTF/` without touching any generation.
pub fn of_restore(part: &RestorePart<'_>) -> Precondition {
    let files = part.files;
    Precondition::new(
        "backup restore",
        part.key
            .map(|key| (TargetRef::Key(key), Some(part.generation)))
            .into_iter()
            .collect(),
        json!({
            "backup": part.backup.to_string(),
            "mode": "whole",
            "folders": null,
            "files": {
                "restored": files.restored,
                "differed": files.differed,
                "created_since": files.created_since,
                "unchanged": files.unchanged,
            },
        }),
        json!({ "backup_json": part.backup_json, "wtf": part.wtf }),
    )
}

/// One client's part of a `backup restore --addon`.
pub struct AddonsPart<'a> {
    pub key: Option<TargetKey>,
    /// The target's record generation it was planned against.
    pub generation: u64,
    pub backup: BackupId,
    /// The folders whose SavedVariables come back, as resolved.
    pub folders: Vec<String>,
    /// Each file (its live path) with what the restore does to it.
    pub files: Vec<(&'a str, &'static str)>,
    /// The SHA-256 of the source's `backup.json`, `None` when it does not read.
    pub backup_json: Option<String>,
    /// The stamp digest of every live SavedVariables file
    /// ([`crate::engine::swap::sv_stamp`]).
    pub saved_variables: String,
}

/// `backup restore --addon`'s precondition (M3-D46): the target's key with the
/// generation; the backup, the mode (`addons`), the resolved folders and each file's
/// action; and, observed, the source's `backup.json` and every live SavedVariables file's
/// stamp. Those cover more than the files restored, so that naming other addons changes
/// only the selection, and any play session since changes what was observed.
pub fn of_restore_addons(part: &AddonsPart<'_>) -> Precondition {
    let mut folders = part.folders.clone();
    folders.sort_by_key(|f| f.to_ascii_lowercase());
    let files: Vec<Value> = part
        .files
        .iter()
        .map(|(path, action)| json!([path, action]))
        .collect();
    Precondition::new(
        "backup restore",
        part.key
            .map(|key| (TargetRef::Key(key), Some(part.generation)))
            .into_iter()
            .collect(),
        json!({
            "backup": part.backup.to_string(),
            "mode": "addons",
            "folders": folders,
            "files": files,
        }),
        json!({
            "backup_json": part.backup_json,
            "saved_variables": part.saved_variables,
        }),
    )
}

/// A `backup restore` run bound to its preview: compared before anything is copied; the
/// engine asks it only for the refusal when the target's records, or a partial restore's
/// files, moved under the lock.
pub struct RestoreBinding {
    pub preview: Vec<String>,
}

impl Binding for RestoreBinding {
    fn plan(&self, _plan: &Plan) -> Result<()> {
        Ok(())
    }

    fn undo(&self, _planned: &EngineUndo) -> Result<()> {
        Ok(())
    }

    fn moved(&self, _target: &str) -> Error {
        stale("backup restore", &[Part::Targets], &self.preview)
    }

    fn observed(&self, _target: &str) -> Error {
        stale("backup restore", &[Part::Observed], &self.preview)
    }
}

// ---------------------------------------------------------------------------------------
// `backup prune`.

/// One client's part of a `backup prune`: its key, its id, and each backup it would
/// delete with the SHA-256 of that backup's `backup.json` (`None` when it does not read).
pub struct PrunePart<'a> {
    pub key: Option<TargetKey>,
    pub target_id: &'a str,
    pub victims: Vec<(BackupId, Option<String>)>,
}

/// `backup prune`'s precondition (M3-D46): the target keys without generations (a
/// prune neither reads nor changes the addon records they count), the exact backups it
/// would delete per target (by key, or id when unregistered), and each one's
/// `backup.json` digest as observed.
pub fn of_prune(parts: &[PrunePart<'_>]) -> Precondition {
    let mut selection = BTreeMap::new();
    let mut observed = Vec::new();
    for part in parts {
        let name = part
            .key
            .map_or_else(|| part.target_id.to_owned(), |k| k.to_string());
        let mut ids: Vec<String> = part.victims.iter().map(|(id, _)| id.to_string()).collect();
        ids.sort();
        selection.insert(name, ids);
        for (id, digest) in &part.victims {
            observed.push(json!({"id": id.to_string(), "backup_json": digest}));
        }
    }
    observed.sort_by_key(Value::to_string);
    Precondition::new(
        "backup prune",
        parts
            .iter()
            .filter_map(|p| p.key)
            .map(|k| (TargetRef::Key(k), None))
            .collect(),
        json!(selection),
        Value::Array(observed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::edit::EditField;
    use crate::engine::inspect::SkippedFolder;
    use crate::engine::plan::{ChangeKind, DeferredAsset, FolderSummary, OpKind, SCHEMA_VERSION};
    use crate::engine::witness::{
        DependencyBy, DependencyChoice, EntryDigest, FolderWitness, InputKind, InputWitness,
        WitnessCost, WitnessRole,
    };
    use crate::error::{ErrorBody, ErrorCode, Warning, WarningCode};
    use crate::model::{Actor, ExpectedHash, ReloadRequired, TxKind};
    use crate::sources::SourceId;
    use crate::toc::WrongFlavorReason;

    const KEY: &str = "01JB8Q2V9W0000000000000000";
    const OTHER: &str = "01JB8Q2V9W0000000000000001";

    fn key(s: &str) -> TargetKey {
        s.parse().unwrap()
    }

    /// The registered target `s`.
    fn by_key(s: &str) -> TargetRef {
        TargetRef::Key(key(s))
    }

    fn folder(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    /// A target's plan with nothing in it: registered as `key`, else provisional as `id`.
    fn tp(key: Option<TargetKey>, id: &str, generation: u64) -> TargetPlan {
        TargetPlan {
            target: id.to_owned(),
            key,
            kind: None,
            changes: Vec::new(),
            ops: Vec::new(),
            releases: Vec::new(),
            reload_required: ReloadRequired::None,
            warnings: Vec::new(),
            creates_addons_dir: false,
            generation,
            work: Vec::new(),
            unchanged_warnings: Vec::new(),
            notes: Vec::new(),
            default_kind: TxKind::Install,
            dependencies: BTreeMap::new(),
        }
    }

    fn op(op: OpKind, name: &str, addon: &str) -> PlannedOp {
        PlannedOp {
            op,
            folder: folder(name),
            addon: addon.to_owned(),
        }
    }

    fn change(addon: &str, r: &str, kind: ChangeKind, folders: FolderSummary) -> Change {
        Change {
            addon: addon.to_owned(),
            r#ref: r.parse().unwrap(),
            kind,
            fields: Vec::new(),
            from_version: None,
            to_version: None,
            folders,
        }
    }

    fn plan_with(targets: Vec<TargetPlan>) -> Plan {
        Plan {
            schema_version: SCHEMA_VERSION,
            plan_id: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            expires_at: None,
            argv: Vec::new(),
            actor: Actor::Agent,
            dry_run: true,
            generations: BTreeMap::new(),
            targets,
            failures: Vec::new(),
            facts: Vec::new(),
            witnesses: Vec::new(),
        }
    }

    /// The remove of Foo on a registered target at generation 7.
    fn remove_plan(key: Option<TargetKey>) -> Plan {
        let mut t = tp(key, "retail", 7);
        t.ops.push(op(OpKind::Remove, "Foo", "Foo"));
        t.changes.push(change(
            "Foo",
            "gh:o/foo",
            ChangeKind::Remove,
            FolderSummary {
                removed: vec![folder("Foo")],
                ..FolderSummary::default()
            },
        ));
        plan_with(vec![t])
    }

    #[test]
    fn of_remove_keeps_its_m3_token() {
        // Pinned against M3's `of_remove` before unregistered targets were covered.
        let token = of_remove(&remove_plan(Some(key(KEY)))).token();
        assert_eq!(
            token,
            "pc1:66f3d68b7627c9f62b9796bcd091931eee83806e6f17042474234e98afe7498f"
        );
    }

    #[test]
    fn remove_covers_an_unregistered_target() {
        let p = of_remove(&remove_plan(None));
        let [_, targets, selection, _] = p.parts();
        assert_eq!(targets, r#"[{"generation":7,"key":"retail"}]"#);
        assert!(selection.starts_with(r#"{"retail":"#), "{selection}");
        assert_ne!(p.token(), of_remove(&remove_plan(Some(key(KEY)))).token());
    }

    // ---- `of_plan` ---------------------------------------------------------------

    const BUILD: &str = "0123456789abcdef";
    const THIRD: &str = "01JB8Q2V9W0000000000000002";

    fn digest(n: u8) -> EntryDigest {
        EntryDigest::from_bytes([n; 32])
    }

    fn release(id: &str, url: &str) -> PlannedRelease {
        PlannedRelease {
            release_id: id.to_owned(),
            url: Some(url.to_owned()),
            expected_hash: Some(ExpectedHash::Sha256("a".repeat(64))),
            blob_sha256: Some("b".repeat(64)),
            deferred: None,
        }
    }

    fn deferred(id: &str, key: &str, url: Option<&str>) -> PlannedRelease {
        PlannedRelease {
            release_id: id.to_owned(),
            url: url.map(str::to_owned),
            expected_hash: Some(ExpectedHash::Md5("c".repeat(32))),
            blob_sha256: Some("d".repeat(64)),
            deferred: Some(DeferredAsset {
                source: SourceId::Tukui,
                key: key.to_owned(),
            }),
        }
    }

    fn folder_witness(role: WitnessRole, key: &str, n: u8) -> FolderWitness {
        FolderWitness {
            role,
            key: key.to_owned(),
            digest: digest(n),
        }
    }

    fn input(kind: InputKind, source: &str, digest: &str) -> InputWitness {
        InputWitness {
            kind,
            source: source.to_owned(),
            digest: digest.to_owned(),
        }
    }

    /// Everything `of_plan` reads, as the tests vary it.
    #[derive(Clone)]
    struct Inputs {
        command: &'static str,
        plan: Plan,
        witnesses: Vec<ApprovalWitness>,
        copy_source: Option<(TargetKey, u64)>,
        desired: Option<String>,
        skips: Vec<(TargetRef, String, SkipReason)>,
        build_id: String,
    }

    impl Inputs {
        fn precondition(&self) -> Precondition {
            let extras = PlanExtras {
                copy_source: self.copy_source,
                desired_sha256: self.desired.clone(),
                update_skips: &self.skips,
            };
            of_plan_at(
                &self.build_id,
                self.command,
                &self.plan,
                &self.witnesses,
                &extras,
            )
        }

        /// The witness of `target`'s folder in `role`.
        fn folder_digest(&mut self, role: WitnessRole) -> &mut EntryDigest {
            let folders = &mut self.witnesses[0].folders;
            let at = folders.iter().position(|f| f.role == role).unwrap();
            &mut folders[at].digest
        }

        fn input_digest(&mut self, kind: InputKind) -> &mut String {
            let inputs = &mut self.witnesses[0].inputs;
            let at = inputs.iter().position(|i| i.kind == kind).unwrap();
            &mut inputs[at].digest
        }
    }

    /// A plan over a registered target (an update with every folder list, a removal, a
    /// direct and a deferred release) and a provisional one (a first install creating
    /// `Interface/AddOns`), their witnesses, and every extra.
    fn inputs() -> Inputs {
        let mut registered = tp(Some(key(KEY)), "retail", 7);
        registered.ops = vec![
            op(OpKind::Replace, "Foo", "Foo"),
            op(OpKind::Add, "FooExtra", "Foo"),
            op(OpKind::Remove, "Old", "Old"),
        ];
        registered.changes = vec![
            Change {
                from_version: Some("1".into()),
                to_version: Some("2".into()),
                ..change(
                    "Foo",
                    "gh:o/foo",
                    ChangeKind::Update,
                    FolderSummary {
                        added: vec![folder("FooExtra")],
                        replaced: vec![folder("Foo")],
                        removed: Vec::new(),
                        skipped: vec![SkippedFolder {
                            name: folder("Foo_Vanilla"),
                            reason: WrongFlavorReason::NoInterface,
                        }],
                        recorded: vec![folder("FooRecorded")],
                    },
                )
            },
            change(
                "Old",
                "gh:o/old",
                ChangeKind::Remove,
                FolderSummary {
                    removed: vec![folder("Old")],
                    ..FolderSummary::default()
                },
            ),
        ];
        registered.releases = vec![
            release("gh:o/foo@2", "https://example.com/foo-2.zip"),
            deferred(
                "tukui:elvui@13.80",
                "elvui",
                Some("https://tukui.example/download?token=x"),
            ),
        ];
        let mut provisional = tp(None, "classic", 0);
        provisional.ops = vec![op(OpKind::Add, "Baz", "Baz")];
        provisional.changes = vec![change(
            "Baz",
            "gh:o/baz",
            ChangeKind::Install,
            FolderSummary {
                added: vec![folder("Baz")],
                ..FolderSummary::default()
            },
        )];
        provisional.releases = vec![release("gh:o/baz@1", "https://example.com/baz-1.zip")];
        provisional.creates_addons_dir = true;
        let witnesses = vec![
            ApprovalWitness::new(
                by_key(KEY),
                Some(format!("sha256:{}", "e".repeat(64))),
                vec![
                    folder_witness(WitnessRole::Replaced, "foo", 1),
                    folder_witness(WitnessRole::Removed, "old", 2),
                    folder_witness(WitnessRole::CopySource, "src", 3),
                    folder_witness(WitnessRole::Adopted, "adopted", 4),
                ],
                vec!["fooextra".into()],
                vec![
                    input(InputKind::Zip, "/dl/Foo.zip", &"f".repeat(64)),
                    input(InputKind::Dir, "/src/Foo", &"1".repeat(64)),
                    input(InputKind::Git, "https://git.example/o/foo.git", "0123abcd"),
                ],
                vec![DependencyChoice {
                    requester: "Foo".into(),
                    folder: "libstub".into(),
                    by: DependencyBy::Installs("LibStub".into()),
                }],
                "ab12".repeat(16),
            ),
            ApprovalWitness::new(
                TargetRef::Provisional("classic".into()),
                None,
                Vec::new(),
                vec!["Interface/AddOns".into(), "baz".into()],
                Vec::new(),
                Vec::new(),
                "cd34".repeat(16),
            ),
        ];
        Inputs {
            command: "install",
            plan: plan_with(vec![registered, provisional]),
            witnesses,
            copy_source: Some((key(OTHER), 4)),
            desired: Some("9".repeat(64)),
            skips: vec![
                (by_key(KEY), "DBM-Core".into(), SkipReason::Pinned),
                (by_key(KEY), "Plater".into(), SkipReason::Ahead),
            ],
            build_id: BUILD.into(),
        }
    }

    /// Both tokens and the four parts of each, for a failure message.
    fn shown(a: &Precondition, b: &Precondition) -> String {
        format!(
            "{}\n{:#?}\n{}\n{:#?}",
            a.token(),
            a.parts(),
            b.token(),
            b.parts()
        )
    }

    #[test]
    fn of_plan_fixture_token_never_drifts() {
        // Each field is the first 16 hex digits of `sha256sum` over one of `parts()`.
        let p = inputs().precondition();
        assert_eq!(
            p.token(),
            "pc1:39975c7e71548c5101f290ceb610e821935a76b37d3a58dce2ee9968a74a44df",
            "{:#?}",
            p.parts()
        );
        assert_eq!(inputs().precondition().token(), p.token());
    }

    #[test]
    fn every_covered_input_moves_its_own_part() {
        type Edit = fn(&mut Inputs);
        const SELECTION: &[Part] = &[Part::Selection];
        const OBSERVED: &[Part] = &[Part::Observed];
        let rows: Vec<(&str, Edit, &[Part])> = vec![
            (
                "generation 7 -> 8",
                |i| i.plan.targets[0].generation = 8,
                &[Part::Targets],
            ),
            (
                "an op added",
                |i| i.plan.targets[0].ops.push(op(OpKind::Add, "New", "Foo")),
                SELECTION,
            ),
            (
                "an op's kind",
                |i| i.plan.targets[0].ops[1].op = OpKind::Replace,
                SELECTION,
            ),
            (
                "a release id",
                |i| i.plan.targets[0].releases[0].release_id = "gh:o/foo@3".into(),
                SELECTION,
            ),
            (
                "an expected hash",
                |i| i.plan.targets[0].releases[0].expected_hash = None,
                SELECTION,
            ),
            (
                "a blob digest",
                |i| i.plan.targets[0].releases[0].blob_sha256 = Some("0".repeat(64)),
                SELECTION,
            ),
            (
                "a direct release's url",
                |i| i.plan.targets[0].releases[0].url = Some("https://example.com/x.zip".into()),
                SELECTION,
            ),
            (
                "a deferred release's source key",
                |i| {
                    if let Some(d) = &mut i.plan.targets[0].releases[1].deferred {
                        d.key = "tukui-other".into();
                    }
                },
                SELECTION,
            ),
            (
                "a change's to_version",
                |i| i.plan.targets[0].changes[0].to_version = Some("3".into()),
                SELECTION,
            ),
            (
                "a change's replaced folders",
                |i| {
                    i.plan.targets[0].changes[0]
                        .folders
                        .replaced
                        .push(folder("FooToo"));
                },
                SELECTION,
            ),
            (
                "a change's fields",
                |i| i.plan.targets[0].changes[0].fields.push(EditField::Pinned),
                SELECTION,
            ),
            (
                "creates_addons_dir",
                |i| i.plan.targets[0].creates_addons_dir = true,
                SELECTION,
            ),
            (
                "the judge digest",
                |i| i.witnesses[0].judge = "ef56".repeat(16),
                OBSERVED,
            ),
            ("the marker", |i| i.witnesses[0].marker = None, OBSERVED),
            (
                "a replaced folder's digest (any hand edit)",
                |i| *i.folder_digest(WitnessRole::Replaced) = digest(9),
                OBSERVED,
            ),
            (
                "a removed folder's digest",
                |i| *i.folder_digest(WitnessRole::Removed) = digest(9),
                OBSERVED,
            ),
            (
                "an absent name added",
                |i| i.witnesses[0].absent.push("new".into()),
                OBSERVED,
            ),
            (
                "a zip input's digest",
                |i| *i.input_digest(InputKind::Zip) = "0".repeat(64),
                OBSERVED,
            ),
            (
                "a dir input's listing",
                |i| *i.input_digest(InputKind::Dir) = "0".repeat(64),
                OBSERVED,
            ),
            (
                "a git input's commit",
                |i| *i.input_digest(InputKind::Git) = "4567cdef".into(),
                OBSERVED,
            ),
            (
                "a dependency choice",
                |i| i.witnesses[0].dependencies[0].by = DependencyBy::Unmanaged,
                OBSERVED,
            ),
            (
                "the build id",
                |i| i.build_id = "fedcba9876543210".into(),
                OBSERVED,
            ),
            (
                "copy's source generation",
                |i| i.copy_source = Some((key(OTHER), 5)),
                OBSERVED,
            ),
            (
                "a copy-source folder's digest",
                |i| *i.folder_digest(WitnessRole::CopySource) = digest(9),
                OBSERVED,
            ),
            (
                "sync's file bytes",
                |i| i.desired = Some("8".repeat(64)),
                OBSERVED,
            ),
            (
                "an adopted folder's digest",
                |i| *i.folder_digest(WitnessRole::Adopted) = digest(9),
                OBSERVED,
            ),
            (
                "update's skip reason",
                |i| i.skips[0].2 = SkipReason::Ahead,
                OBSERVED,
            ),
            (
                "update's skip added",
                |i| {
                    i.skips
                        .push((by_key(KEY), "WeakAuras".into(), SkipReason::Pinned));
                },
                OBSERVED,
            ),
            ("the command", |i| i.command = "update", &[Part::Command]),
            (
                "a target registering",
                |i| {
                    i.plan.targets[1].key = Some(key(THIRD));
                    i.witnesses[1].target = by_key(THIRD);
                    i.witnesses[1].marker = Some(format!("sha256:{}", "7".repeat(64)));
                },
                &[Part::Targets, Part::Selection, Part::Observed],
            ),
        ];
        let base = inputs().precondition();
        let preview = argv(&["omaddons", "install", "Foo", "--dry-run"]);
        for (row, edit, expected) in rows {
            let mut changed = inputs();
            edit(&mut changed);
            let actual = changed.precondition();
            let result = check(Some(&base.token()), &actual, &preview);
            assert!(
                result.is_err(),
                "{row}: the token did not move\n{}",
                shown(&base, &actual)
            );
            let body = ErrorBody::from(result.unwrap_err());
            let names: Vec<&str> = expected.iter().map(|p| p.wire()).collect();
            assert_eq!(body.code, ErrorCode::PlanStale, "{row}");
            assert!(!body.retryable, "{row}");
            assert_eq!(
                body.details["changed"],
                json!(names),
                "{row}\n{}",
                shown(&base, &actual)
            );
            assert_eq!(body.fix_argv, Some(preview.clone()), "{row}");
        }
    }

    #[test]
    fn benign_differences_leave_the_token_equal() {
        type Edit = fn(&mut Inputs);
        let rows: Vec<(&str, Edit)> = vec![
            ("warnings text", |i| {
                i.plan.targets[0]
                    .warnings
                    .push(Warning::new(WarningCode::AddonPinned, "Foo is pinned"));
            }),
            ("notes", |i| i.plan.targets[0].notes.push("a note".into())),
            ("created_at", |i| {
                i.plan.created_at = "2027-06-01T12:00:00Z".parse().unwrap();
            }),
            ("plan_id", |i| i.plan.plan_id = Some("01JB8Q2V9W".into())),
            ("argv", |i| {
                i.plan.argv = argv(&["omaddons", "install", "Foo", "--reason", "x"]);
            }),
            ("actor", |i| i.plan.actor = Actor::Human),
            ("generations map", |i| {
                i.plan.generations.insert(KEY.into(), 99);
            }),
            ("targets reordered", |i| {
                i.plan.targets.reverse();
                i.witnesses.reverse();
            }),
            ("ops, changes, releases and skips shuffled", |i| {
                for t in &mut i.plan.targets {
                    t.ops.reverse();
                    t.changes.reverse();
                    t.releases.reverse();
                }
                i.skips.reverse();
            }),
            ("witness stamps and cost", |i| {
                let tmp = tempfile::tempdir().unwrap();
                let stamp = crate::fsx::TreeStamp::read(tmp.path()).unwrap();
                let cost = WitnessCost {
                    files_hashed: 3,
                    files_reused: 2,
                    bytes_hashed: 99,
                };
                i.witnesses[0] = i.witnesses[0]
                    .clone()
                    .with_private(vec![(tmp.path().to_path_buf(), stamp)], cost);
            }),
            ("a deferred release with and without its url", |i| {
                i.plan.targets[0].releases[1].url = None;
            }),
        ];
        let base = inputs().precondition();
        for (row, edit) in rows {
            let mut changed = inputs();
            edit(&mut changed);
            let changed = changed.precondition();
            assert_eq!(
                changed.token(),
                base.token(),
                "{row}\n{}",
                shown(&base, &changed)
            );
        }
    }

    #[test]
    fn a_deferred_release_is_keyed_without_its_url() {
        let [_, _, selection, _] = inputs().precondition().parts();
        let parsed: Value = serde_json::from_str(&selection).unwrap();
        let releases = &parsed[KEY]["releases"];
        let deferred = releases
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r.get("source").is_some())
            .unwrap();
        assert_eq!(deferred["source"], "tukui", "{selection}");
        assert_eq!(deferred["key"], "elvui", "{selection}");
        assert!(deferred.get("url").is_none(), "{selection}");
        assert!(!selection.contains("tukui.example"), "{selection}");
    }

    #[test]
    fn a_missing_witness_is_null() {
        let mut i = inputs();
        i.witnesses.truncate(1);
        let [_, _, _, observed] = i.precondition().parts();
        assert!(observed.contains(r#""classic":null"#), "{observed}");
        assert!(observed.contains(&format!(r#""{KEY}":""#)), "{observed}");
    }

    #[test]
    fn observed_always_has_one_shape() {
        let plan = plan_with(vec![tp(Some(key(KEY)), "retail", 1)]);
        let p = of_plan_at(BUILD, "pin", &plan, &[], &PlanExtras::default());
        let [_, _, _, observed] = p.parts();
        assert_eq!(
            observed,
            format!(
                r#"{{"build_id":"{BUILD}","copy_source":null,"desired_sha256":null,"update_skips":[],"witnesses":{{"{KEY}":null}}}}"#
            )
        );
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    fn fixture() -> Precondition {
        Precondition::new(
            "remove",
            vec![(by_key(KEY), Some(7))],
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
                (by_key(OTHER), Some(2)),
                (by_key(KEY), None),
                (by_key(OTHER), Some(2)),
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
            vec![(by_key(KEY), None), (by_key(OTHER), Some(2))],
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
                    targets: vec![(by_key(KEY), Some(8))],
                    ..base.clone()
                },
                Part::Targets,
            ),
            (
                Precondition {
                    targets: vec![(by_key(KEY), None)],
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
    fn a_prune_binds_to_its_victims_and_their_metadata() {
        let (a, b): (BackupId, BackupId) = (
            "01M6Q2VA000000000000000000".parse().unwrap(),
            "01M6Q2VB000000000000000000".parse().unwrap(),
        );
        let part = |victims: Vec<(BackupId, Option<String>)>| PrunePart {
            key: Some(key(KEY)),
            target_id: "retail",
            victims,
        };
        let digest = |s: &str| Some(s.to_owned());
        let token = of_prune(&[part(vec![(a, digest("1")), (b, digest("2"))])]).token();
        assert_eq!(
            of_prune(&[part(vec![(b, digest("2")), (a, digest("1"))])]).token(),
            token,
            "order carries no meaning"
        );
        let changed = |actual: Precondition| {
            let err = check(Some(&token), &actual, &argv(&["omaddons"])).unwrap_err();
            err.details["changed"].clone()
        };
        assert_eq!(
            changed(of_prune(&[part(vec![(a, digest("1"))])])),
            json!(["selection", "observed"]),
            "a different set"
        );
        assert_eq!(
            changed(of_prune(&[part(vec![(a, digest("1")), (b, digest("3"))])])),
            json!(["observed"]),
            "a victim's backup.json changed"
        );
        let other = PrunePart {
            key: None,
            target_id: "retail",
            victims: vec![(a, digest("1")), (b, digest("2"))],
        };
        assert_eq!(
            changed(of_prune(&[other])),
            json!(["targets", "selection"]),
            "unregistered: named by id"
        );
    }

    #[test]
    fn the_binding_commands_are_m3s_five_then_the_engine_commands() {
        assert_eq!(
            BINDING_COMMANDS,
            [
                "remove",
                "undo",
                "backup restore",
                "backup prune",
                "errors dismiss",
                "install",
            ]
        );
        assert_eq!(
            Part::ALL.map(Part::wire),
            ["command", "targets", "selection", "observed"]
        );
    }

    // ---- Binding the engine commands ------------------------------------------------

    const PREVIEW: &[&str] = &["omaddons", "install", "Foo", "--dry-run"];

    /// The fixture plan with its witnesses on it, as the engine hands a binding.
    fn witnessed_plan(i: &Inputs) -> Plan {
        let mut plan = i.plan.clone();
        plan.witnesses = i.witnesses.clone();
        plan
    }

    fn extras_of(i: &Inputs) -> PlanExtras<'_> {
        PlanExtras {
            copy_source: i.copy_source,
            desired_sha256: i.desired.clone(),
            update_skips: &i.skips,
        }
    }

    /// `details.changed` of `result`'s refusal, with its code, retryability and fix
    /// checked; `None` when it passed.
    fn refused_parts(case: &str, result: Result<()>) -> Option<Value> {
        let error = result.err()?;
        let body = ErrorBody::from(error);
        assert_eq!(body.code, ErrorCode::PlanStale, "{case}");
        assert!(
            !body.retryable,
            "{case}: the same token can only fail again"
        );
        assert_eq!(body.fix_argv, Some(argv(PREVIEW)), "{case}");
        Some(body.details["changed"].clone())
    }

    #[test]
    fn a_plan_binding_passes_its_own_token() {
        let i = inputs();
        let plan = witnessed_plan(&i);
        let token = plan_token("install", &plan, &extras_of(&i));
        let binding = PlanBinding::new("install", Some(token), extras_of(&i), argv(PREVIEW));
        assert!(binding.plan(&plan).is_ok());
    }

    #[test]
    fn a_plan_binding_names_each_moved_part() {
        type Edit = fn(&mut Inputs);
        let rows: [(&str, Edit, &str, &str); 8] = [
            (
                "generation_moved",
                |i| i.plan.targets[0].generation = 8,
                "install",
                "targets",
            ),
            (
                "op_added",
                |i| i.plan.targets[0].ops.push(op(OpKind::Add, "New", "Foo")),
                "install",
                "selection",
            ),
            (
                "release_changed",
                |i| i.plan.targets[0].releases[0].release_id = "gh:o/foo@3".into(),
                "install",
                "selection",
            ),
            (
                "witness_digest_changed",
                |i| i.witnesses[0].judge = "ef56".repeat(16),
                "install",
                "observed",
            ),
            (
                "copy_source_generation_moved",
                |i| i.copy_source = Some((key(OTHER), 5)),
                "install",
                "observed",
            ),
            (
                "desired_sha256_changed",
                |i| i.desired = Some("8".repeat(64)),
                "install",
                "observed",
            ),
            (
                "update_skip_added",
                |i| {
                    i.skips
                        .push((by_key(KEY), "WeakAuras".into(), SkipReason::Pinned));
                },
                "install",
                "observed",
            ),
            ("other_command", |_| {}, "update", "command"),
        ];
        let base = inputs();
        let token = plan_token("install", &witnessed_plan(&base), &extras_of(&base));
        for (case, edit, command, part) in rows {
            let mut changed = inputs();
            edit(&mut changed);
            let binding = PlanBinding::new(
                command,
                Some(token.clone()),
                extras_of(&changed),
                argv(PREVIEW),
            );
            let parts = refused_parts(case, binding.plan(&witnessed_plan(&changed)));
            assert_eq!(parts, Some(json!([part])), "{case}");
        }
    }

    #[test]
    fn a_plan_binding_without_a_token_checks_nothing() {
        let i = inputs();
        let binding = PlanBinding::new("install", None, PlanExtras::default(), argv(PREVIEW));
        assert!(binding.plan(&witnessed_plan(&i)).is_ok());
        assert!(binding.plan(&plan_with(Vec::new())).is_ok());
    }

    #[test]
    fn a_plan_bindings_moved_is_a_non_retryable_targets_refusal() {
        let binding = PlanBinding::new(
            "install",
            Some("pc1:x".into()),
            PlanExtras::default(),
            argv(PREVIEW),
        );
        let body = ErrorBody::from(binding.moved("retail"));
        assert_eq!(body.code, ErrorCode::PlanStale);
        assert!(!body.retryable);
        assert_eq!(body.details["changed"], json!(["targets"]));
        assert_eq!(body.fix_argv, Some(argv(PREVIEW)));
        assert!(
            body.message.starts_with("this install is no longer"),
            "{}",
            body.message
        );
    }

    /// A run of `install Foo`, dry or not, with or without `--if-match`.
    fn ctx(dry_run: bool, if_match: Option<&str>) -> Context {
        let mut words = vec!["omaddons", "install", "Foo"];
        if dry_run {
            words.push("--dry-run");
        }
        if let Some(token) = if_match {
            words.extend(["--if-match", token]);
        }
        Context {
            output: crate::output::OutputConfig::machine(),
            command: "install",
            actor: Actor::Agent,
            reason: None,
            argv: argv(&words),
            offline: false,
            offline_by_env: false,
            dry_run,
            while_running: false,
            if_match: if_match.map(str::to_owned),
            target: None,
            started: std::time::Instant::now(),
            progress: crate::output::ProgressSink::none(),
            notes: crate::output::RunNotes::default(),
            stdin_is_tty: false,
        }
    }

    /// A well-formed preview token (no credential).
    fn sample_token() -> String {
        format!("pc1:{}", "0123456789abcdef".repeat(4))
    }

    #[test]
    fn of_run_binds_only_with_if_match() {
        let sample = sample_token();
        for (case, dry_run, if_match, binds) in [
            ("dry_without", true, None, false),
            ("dry_with", true, Some(sample.as_str()), true),
            ("real_without", false, None, false),
            ("real_with", false, Some(sample.as_str()), true),
        ] {
            let ctx = ctx(dry_run, if_match);
            let binding = PlanBinding::of_run(&ctx, "install", PlanExtras::default());
            assert_eq!(binding.is_some(), binds, "{case}");
            if let Some(binding) = binding {
                assert_eq!(binding.preview, preview_argv(&ctx.argv), "{case}");
                assert_eq!(binding.expected.as_deref(), Some(sample.as_str()), "{case}");
            }
        }
    }

    #[test]
    fn witnessing_is_on_for_dry_and_bound_runs_only() {
        let sample = sample_token();
        for (case, dry_run, if_match, on) in [
            ("dry_without", true, None, true),
            ("dry_with", true, Some(sample.as_str()), true),
            ("real_without", false, None, false),
            ("real_with", false, Some(sample.as_str()), true),
        ] {
            let scope = witnessing(&ctx(dry_run, if_match));
            assert_eq!(scope.is_some(), on, "{case}");
        }
    }

    /// A target at `dir` (a client dir in a tempdir), registered as `key` when given.
    fn client(dir: &std::path::Path, id: &str, key: Option<TargetKey>) -> Target {
        std::fs::create_dir_all(dir.join("Interface/AddOns")).unwrap();
        let mut target = crate::engine::testing::target(dir);
        target.id = id.to_owned();
        target.key = key;
        if let Some(key) = key {
            crate::engine::testing::mark(dir, key);
        }
        target
    }

    fn noop(
        command: &'static str,
        targets: &[(&Target, u64)],
        extras: &PlanExtras<'_>,
    ) -> Precondition {
        let idle: Vec<Idle<'_>> = targets
            .iter()
            .map(|(target, generation)| Idle {
                target,
                generation: *generation,
            })
            .collect();
        of_noop(command, &idle, extras).unwrap()
    }

    #[test]
    fn the_noop_token_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let classic = client(&tmp.path().join("classic"), "classic", None);
        let extras = PlanExtras::default();
        let a = noop("update", &[(&retail, 7), (&classic, 0)], &extras);
        let b = noop("update", &[(&classic, 0), (&retail, 7)], &extras);
        assert_eq!(a.token(), b.token(), "{}", shown(&a, &b));
        assert_eq!(
            a.token(),
            noop("update", &[(&retail, 7), (&classic, 0)], &extras).token()
        );
    }

    #[test]
    fn the_noop_token_covers_its_targets_and_extras() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let classic = client(&tmp.path().join("classic"), "classic", None);
        let skips = [(by_key(KEY), "DBM-Core".to_owned(), SkipReason::Pinned)];
        let none = PlanExtras::default();
        let base = noop("update", &[(&retail, 7)], &none);
        let provisional = noop("update", &[(&classic, 0)], &none);
        let mut registered = classic.clone();
        registered.key = Some(key(THIRD));
        crate::engine::testing::mark(&registered.client_dir, key(THIRD));
        let remarked = client(&tmp.path().join("remarked"), "retail", Some(key(KEY)));
        std::fs::write(
            crate::discover::registry::marker_path(&remarked.client_dir),
            format!("{{\n  \"key\": \"{KEY}\"\n}}\n"),
        )
        .unwrap();
        // Adding or registering a target also adds or renames its selection entry and
        // its witness: those rows move all three parts.
        let cases: Vec<(&str, &Precondition, Precondition, &[&str])> = vec![
            (
                "generation_moved",
                &base,
                noop("update", &[(&retail, 8)], &none),
                &["targets"],
            ),
            (
                "target_added",
                &base,
                noop("update", &[(&retail, 7), (&classic, 0)], &none),
                &["targets", "selection", "observed"],
            ),
            (
                "target_registered",
                &provisional,
                noop("update", &[(&registered, 0)], &none),
                &["targets", "selection", "observed"],
            ),
            (
                "marker_changed",
                &base,
                noop("update", &[(&remarked, 7)], &none),
                &["observed"],
            ),
            (
                "desired_sha256",
                &base,
                noop(
                    "update",
                    &[(&retail, 7)],
                    &PlanExtras {
                        desired_sha256: Some("9".repeat(64)),
                        ..PlanExtras::default()
                    },
                ),
                &["observed"],
            ),
            (
                "update_skip",
                &base,
                noop(
                    "update",
                    &[(&retail, 7)],
                    &PlanExtras {
                        update_skips: &skips,
                        ..PlanExtras::default()
                    },
                ),
                &["observed"],
            ),
            (
                "copy_source",
                &base,
                noop(
                    "update",
                    &[(&retail, 7)],
                    &PlanExtras {
                        copy_source: Some((key(OTHER), 4)),
                        ..PlanExtras::default()
                    },
                ),
                &["observed"],
            ),
        ];
        let preview = argv(PREVIEW);
        for (case, before, actual, parts) in cases {
            let changed = check(Some(&before.token()), &actual, &preview)
                .err()
                .map(|e| e.details["changed"].clone());
            assert_eq!(
                changed,
                Some(json!(parts)),
                "{case}\n{}",
                shown(before, &actual)
            );
        }
    }

    #[test]
    fn a_noop_token_differs_from_a_token_with_work_in_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let extras = PlanExtras::default();
        let idle = noop("update", &[(&retail, 7)], &extras);
        let mut busy = Plan::idle(&[(&retail, 7)], &[], Actor::Agent);
        busy.targets[0].ops.push(op(OpKind::Replace, "Foo", "Foo"));
        busy.witnesses = vec![witness::bare(&retail).unwrap()];
        let busy = of_plan("update", &busy, &busy.witnesses, &extras);
        let changed = check(Some(&idle.token()), &busy, &argv(PREVIEW))
            .err()
            .map(|e| e.details["changed"].clone());
        assert_eq!(
            changed,
            Some(json!(["selection"])),
            "{}",
            shown(&idle, &busy)
        );
    }

    #[test]
    fn check_noop_reads_nothing_on_an_unbound_real_run() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 file
        }
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let marker = crate::discover::registry::marker_path(&retail.client_dir);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
        let idle = [Idle {
            target: &retail,
            generation: 7,
        }];
        let extras = PlanExtras::default();
        let unbound = check_noop(&ctx(false, None), "update", &idle, &extras);
        let dry = check_noop(&ctx(true, None), "update", &idle, &extras);
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(unbound, Ok(None)), "{unbound:?}");
        let error = dry.unwrap_err();
        assert_eq!(error.code, ErrorCode::Io, "{error:?}");
    }

    #[test]
    fn check_noop_returns_the_dry_runs_token() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let extras = PlanExtras::default();
        let idle = [Idle {
            target: &retail,
            generation: 7,
        }];
        let token = check_noop(&ctx(true, None), "update", &idle, &extras).unwrap();
        assert_eq!(
            token,
            Some(noop("update", &[(&retail, 7)], &extras).token())
        );
    }

    #[test]
    fn check_noop_refuses_a_stale_bound_run() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let extras = PlanExtras::default();
        let stale = noop("update", &[(&retail, 6)], &extras).token();
        let idle = [Idle {
            target: &retail,
            generation: 7,
        }];
        let error = check_noop(&ctx(false, Some(&stale)), "update", &idle, &extras).unwrap_err();
        let body = ErrorBody::from(error);
        assert_eq!(body.code, ErrorCode::PlanStale);
        assert!(!body.retryable);
        assert_eq!(body.details["changed"], json!(["targets"]));
    }

    #[test]
    fn check_noop_passes_a_matching_bound_run() {
        let tmp = tempfile::tempdir().unwrap();
        let retail = client(&tmp.path().join("retail"), "retail", Some(key(KEY)));
        let extras = PlanExtras::default();
        let token = noop("update", &[(&retail, 7)], &extras).token();
        let idle = [Idle {
            target: &retail,
            generation: 7,
        }];
        let result = check_noop(&ctx(false, Some(&token)), "update", &idle, &extras);
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    // ---- The shared binding through the engine (D3) ---------------------------------

    use crate::engine::edit::RecordEdit;
    use crate::engine::testing::TxScene;

    const PIN_PREVIEW: &[&str] = &["omaddons", "pin", "Foo", "--dry-run"];

    /// A retail client with Foo installed and registered.
    fn scene_with_foo() -> TxScene {
        let scene = TxScene::new();
        let zip = scene.zip("foo.zip", &[("Foo/Foo.toc", "## Interface: 120105\n")]);
        let done = scene.run(vec![scene.install("gh:o/foo", &zip)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);
        scene
    }

    fn pin_foo(scene: &TxScene) -> Vec<crate::engine::plan::RequestedChange> {
        vec![scene.edit("Foo", RecordEdit::SetPinned(true))]
    }

    /// `pin Foo` as a dry run with witnesses, and its token.
    fn pin_preview(scene: &TxScene) -> String {
        let done = scene
            .run_bound(
                pin_foo(scene),
                Some(TxKind::Pin),
                TxKind::Pin,
                true,
                Some(WitnessScope::default()),
                None,
            )
            .unwrap();
        plan_token("pin", &done.plan, &PlanExtras::default())
    }

    /// `pin Foo`, dry or real, bound to `token`.
    fn bound_pin(scene: &TxScene, dry_run: bool, token: &str) -> Result<crate::engine::TxOutcomes> {
        let binding = PlanBinding::new(
            "pin",
            Some(token.to_owned()),
            PlanExtras::default(),
            argv(PIN_PREVIEW),
        );
        scene.run_bound(
            pin_foo(scene),
            Some(TxKind::Pin),
            TxKind::Pin,
            dry_run,
            Some(WitnessScope::default()),
            Some(&binding),
        )
    }

    #[test]
    fn a_plan_binding_accepts_the_plan_it_previewed() {
        let scene = scene_with_foo();
        let token = pin_preview(&scene);
        let done = bound_pin(&scene, false, &token).unwrap();
        assert_eq!(done.outcomes.len(), 1);
        assert!(done.outcomes[0].tx.is_some(), "it committed");
        assert!(scene.record().addons["foo"].pinned);
    }

    #[test]
    fn a_plan_binding_refuses_a_commit_made_after_its_preview() {
        let scene = scene_with_foo();
        let token = pin_preview(&scene);
        scene
            .run_as(
                vec![scene.edit("Foo", RecordEdit::SetChannel(crate::model::Channel::Beta))],
                Some(TxKind::Pin),
                TxKind::Pin,
            )
            .unwrap();
        let generation = scene.record().generation;
        let error = bound_pin(&scene, false, &token).unwrap_err();
        let body = ErrorBody::from(error);
        assert_eq!(body.code, ErrorCode::PlanStale, "{body:?}");
        assert!(!body.retryable);
        assert_eq!(body.details["changed"], json!(["targets"]), "{body:?}");
        assert_eq!(body.fix_argv, Some(argv(PIN_PREVIEW)));
        let record = scene.record();
        assert!(!record.addons["foo"].pinned, "nothing committed");
        assert_eq!(record.generation, generation);
    }

    #[test]
    fn a_bound_dry_run_compares_its_token() {
        let scene = scene_with_foo();
        let token = pin_preview(&scene);
        // Unbound and matching dry runs give the preview's token back.
        assert_eq!(pin_preview(&scene), token, "an unbound dry run");
        let matching = bound_pin(&scene, true, &token).unwrap();
        assert_eq!(
            plan_token("pin", &matching.plan, &PlanExtras::default()),
            token,
            "a bound dry run, matching"
        );
        // A malformed token changes every part.
        let malformed = ErrorBody::from(bound_pin(&scene, true, "pc1:nope").unwrap_err());
        assert_eq!(
            malformed.details["changed"],
            json!(["command", "targets", "selection", "observed"])
        );
        // The world moved: a stale bound dry run is refused.
        scene
            .run_as(
                vec![scene.edit("Foo", RecordEdit::SetChannel(crate::model::Channel::Beta))],
                Some(TxKind::Pin),
                TxKind::Pin,
            )
            .unwrap();
        let stale = ErrorBody::from(bound_pin(&scene, true, &token).unwrap_err());
        assert_eq!(stale.code, ErrorCode::PlanStale);
        assert_eq!(stale.details["changed"], json!(["targets"]), "{stale:?}");
    }
}
