//! The shared multi-target outcome: one data shape and one renderer for the commands
//! that commit one transaction per target (`update`, `sync`, `adopt --apply`, `pin` and
//! `unpin`, `copy`), so agents read `data.targets[].items[].status` the same way from all
//! of them. Also the item vocabulary `install` shares (`ItemStatus`, `ItemError`,
//! `TargetRef`).
//!
//! Targets commit in the engine's key order but are listed by id. Items keep request
//! order within a target. `unchanged` items are counted in `summary` and listed only
//! with `-v`. Any failed item or target error makes the run partial; when every item
//! failed, the command fails with the first error.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;

use crate::app::approval::Approval;
use crate::app::intent::{Preview, Storable, render_bind_line, render_plan_line};
use crate::contract::actions::Action;
use crate::engine::TxOutcome;
use crate::engine::TxOutcomes;
use crate::engine::edit::EditField;
use crate::engine::plan::{
    Change, ChangeKind, DepReport, FolderSummary, PlanFailure, TargetPlan, Unresolved, WorkMode,
};
use crate::error::{CommandHint, Error, ErrorBody, ErrorCode, Suggestion, Warning};
use crate::model::{
    AddonReason, AddonRecord, FolderName, PlanId, Ref, ReloadRequired, RunId, TargetKey, TxId,
    TxKind,
};
use crate::output::{Outcome, Style};
use crate::resolve::deps::BridgeDependency;

/// The target acted on.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetRef {
    pub id: String,
    /// `null` only under `--dry-run` on an unregistered target.
    pub key: Option<TargetKey>,
    /// Present when `-t` named a game mode of the client (`-t sod`) rather than the
    /// target: the addons go to the client, which every mode in it shares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeNote>,
}

/// A game mode `-t` named, and the client's other modes, which share its addons.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModeNote {
    pub id: String,
    pub name: String,
    /// The other modes' names, e.g. `["Classic Era", "Hardcore"]`.
    pub shared_with: Vec<String>,
}

/// What happened to one requested addon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Installed,
    Updated,
    Removed,
    /// Unmanaged folders brought under management, unchanged.
    Adopted,
    /// The installed release is now pinned (only the record changed).
    Pinned,
    /// The installed release is no longer pinned.
    Unpinned,
    /// Other record fields changed (`fields` names them); the release stays.
    Edited,
    /// Planned, with nothing to do.
    Unchanged,
    /// Left alone on purpose (`skip_reason` says why).
    Skipped,
    Failed,
}

impl ItemStatus {
    /// The verb a human line says it with: what happened (`installed`), or under
    /// `--dry-run` what would (`would install`). The wire name stays the past form.
    pub fn verb(self, dry_run: bool) -> String {
        match (dry_run, self.base()) {
            (true, Some(base)) => format!("would {base}"),
            _ => self.wire().to_owned(),
        }
    }

    /// The bare verb of a change (`install`), for a dry run's lines.
    pub const fn base(self) -> Option<&'static str> {
        match self {
            Self::Installed => Some("install"),
            Self::Updated => Some("update"),
            Self::Removed => Some("remove"),
            Self::Adopted => Some("adopt"),
            Self::Pinned => Some("pin"),
            Self::Unpinned => Some("unpin"),
            Self::Edited => Some("edit"),
            Self::Unchanged | Self::Skipped | Self::Failed => None,
        }
    }

    /// Counted as a change in `summary`.
    pub const fn changed(self) -> bool {
        matches!(
            self,
            Self::Installed
                | Self::Updated
                | Self::Removed
                | Self::Adopted
                | Self::Pinned
                | Self::Unpinned
                | Self::Edited
        )
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Removed => "removed",
            Self::Adopted => "adopted",
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
            Self::Edited => "edited",
            Self::Unchanged => "unchanged",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Why an item was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Pinned: left at its release.
    Pinned,
    /// The installed release is newer than the newest on its channel.
    Ahead,
    /// The newest release is known not to load here.
    Blocked,
    /// A local file, URL or given archive: nothing upstream.
    Static,
    /// Its status is not known (offline, never checked).
    Unchecked,
    AlreadyInstalled,
    NoBuildForTarget,
    LocalSource,
    /// Another request named the same addon.
    Duplicate,
}

impl SkipReason {
    /// `Questie is pinned`, as a phrase after the name.
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::Pinned => "is pinned",
            Self::Ahead => "is ahead of its newest release",
            Self::Blocked => "has no release that loads here",
            Self::Static => "has nothing upstream to check",
            Self::Unchecked => "was not checked",
            Self::AlreadyInstalled => "is already installed",
            Self::NoBuildForTarget => "has no build for this client",
            Self::LocalSource => "comes from a local source",
            Self::Duplicate => "was requested twice",
        }
    }
}

/// A failed item's error.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemError {
    pub code: ErrorCode,
    pub message: String,
    pub fix: String,
    pub fix_argv: Option<Vec<String>>,
    /// Only for a conflict between two works of one transaction
    /// (`within_transaction: true`, with each folder and its claimant), an `adopt` item
    /// refused before the engine (`folder`, and `owner`, `class` or `refs`), a rate
    /// limit (`reset_at`), a dependency that failed (`required_by`), a client that runs
    /// (`game_running`: its `reason`, and `running: "unknown"` when `/proc` could not be
    /// read), a stuck journal's fence (`journal_pending`, `reason`, `target`, `tx`,
    /// `resource`), and an archive other than the one a `sync --locked` entry records
    /// (`hash_mismatch`: `reason: "lock_mismatch"`, the lock as `expected`, the archive's
    /// own `actual`). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// The error the item failed with, whole: its own `details`, alternatives and
    /// retryability, which the fields above keep none or only some of. A command whose
    /// every item failed fails with the first one's ([`TargetsData::all_failed`]). Not
    /// serialized.
    #[serde(skip)]
    pub original: Option<Error>,
}

/// The details an item error with `code` carries: a within-transaction conflict's, a
/// `desired_state_invalid`'s (`{path, line, key, reason}`: a `sync` entry listed twice),
/// a stuck journal's fence's (`{journal_pending, reason, target, tx, resource}`, E14), a
/// rate limit's `reset_at`, a dependency's `required_by`, a running client's `reason`
/// and `running`, a locked archive's mismatch (`{reason, expected, actual}`); else none.
fn item_details(code: ErrorCode, details: &serde_json::Value) -> Option<serde_json::Value> {
    if details["within_transaction"] == true
        || (code == ErrorCode::DesiredStateInvalid && details.is_object())
    {
        return Some(details.clone());
    }
    // `sync --locked`: which archive the lock records and which one the source served,
    // so an item says it failed its lock, not a download's published hash.
    if code == ErrorCode::HashMismatch && details["reason"] == "lock_mismatch" {
        let kept: serde_json::Map<String, serde_json::Value> = ["reason", "expected", "actual"]
            .iter()
            .filter_map(|key| details.get(*key).map(|v| ((*key).to_owned(), v.clone())))
            .collect();
        return Some(serde_json::Value::Object(kept));
    }
    // A fence's refusal is the one error whose details name a `resource`: kept as the
    // fencing journal's stop, so a run whose every item it refused reports that journal.
    if details["resource"].is_string() {
        let kept: serde_json::Map<String, serde_json::Value> =
            ["journal_pending", "reason", "target", "tx", "resource"]
                .iter()
                .filter_map(|key| details.get(*key).map(|v| ((*key).to_owned(), v.clone())))
                .collect();
        return Some(serde_json::Value::Object(kept));
    }
    let running: &[&str] = if matches!(code, ErrorCode::GameRunning) {
        &["reason", "running"]
    } else {
        &[]
    };
    let mut kept = serde_json::Map::new();
    for key in ["reset_at", "required_by"].iter().chain(running) {
        if let Some(value) = details.get(*key) {
            kept.insert((*key).to_owned(), value.clone());
        }
    }
    (!kept.is_empty()).then_some(serde_json::Value::Object(kept))
}

impl From<&Error> for ItemError {
    fn from(e: &Error) -> Self {
        Self {
            code: e.code,
            message: e.message.clone(),
            fix: e.fix.display().to_owned(),
            fix_argv: e.fix.argv().map(<[String]>::to_vec),
            details: item_details(e.code, &e.details),
            original: Some(e.clone()),
        }
    }
}

impl From<&ErrorBody> for ItemError {
    fn from(e: &ErrorBody) -> Self {
        Self {
            code: e.code,
            message: e.message.clone(),
            fix: e.fix.clone(),
            fix_argv: e.fix_argv.clone(),
            details: item_details(e.code, &e.details),
            original: Some(error_of(e)),
        }
    }
}

impl ItemError {
    /// The error the item failed with, whole when it was kept, else [`Self::error`].
    pub fn original_error(&self) -> Error {
        self.original.clone().unwrap_or_else(|| self.error())
    }

    /// Replaces the fix, on the wire fields and the kept original alike: a command whose
    /// engine fix adds a flag it does not have (`--force`) says how with `install`, and
    /// the command-level error built from this item must say the same.
    pub fn set_fix(&mut self, fix: Suggestion) {
        self.fix = fix.display().to_owned();
        self.fix_argv = fix.argv().map(<[String]>::to_vec);
        if let Some(original) = &mut self.original {
            original.fix = fix;
        }
    }

    /// Back to an error, with its code's exit code and the details it kept.
    pub fn error(&self) -> Error {
        let fix = match &self.fix_argv {
            Some(argv) => Suggestion::command(argv.clone()),
            None => Suggestion::text(self.fix.clone()),
        };
        Error::new(self.code, self.message.clone(), fix).details(
            self.details
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
        )
    }
}

/// A target's error back from its wire body, kept whole: code, message, fix,
/// alternatives, retryability and details.
fn error_of(body: &ErrorBody) -> Error {
    let fix = match &body.fix_argv {
        Some(argv) => Suggestion::command(argv.clone()),
        None => Suggestion::text(body.fix.clone()),
    };
    body.alternatives.iter().fold(
        Error::new(body.code, body.message.clone(), fix)
            .retryable(body.retryable)
            .details(body.details.clone()),
        |error, alternative| error.alternative(alternative.clone()),
    )
}

/// A multi-target command's data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetsData {
    pub dry_run: bool,
    /// The command's run when at least one transaction committed, else `null`.
    pub run: Option<RunId>,
    /// Sorted by target id.
    pub targets: Vec<TargetResult>,
    /// Every item, listed or not.
    pub summary: Summary,
    /// `--dry-run`: the token `--if-match` takes to bind the real run to this preview (one
    /// token for the whole run); `null` for a real run, and until the command binds.
    pub precondition: Option<String>,
    /// The stored preview's id; `null` on real runs, no-ops and declined stores.
    pub plan_id: Option<PlanId>,
    /// The stored preview's expiry; `null` when no plan was stored.
    #[schemars(schema_with = "crate::contract::schema::timestamp_opt")]
    pub plan_expires_at: Option<Timestamp>,
    /// What approving this dry run covers; `null` on a real run.
    pub approval: Option<Approval>,
}

/// One target's result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetResult {
    pub target: TargetRef,
    /// `null` when nothing committed (and always under `--dry-run`).
    pub tx: Option<TxId>,
    pub kind: Option<TxKind>,
    /// `sync`: the target has no section in the desired set.
    pub no_section: bool,
    /// In request order.
    pub items: Vec<ItemResult>,
    pub reload_required: ReloadRequired,
    /// `null` when no swap happened.
    pub atomic_swap: Option<bool>,
    pub creates_addons_dir: bool,
    /// Nothing of this target committed (its items say `failed` with this error).
    pub error: Option<ErrorBody>,
}

/// One addon's result on one target.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ItemResult {
    /// The user's argument (for a dependency, the folder required).
    pub request: String,
    pub name: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    pub status: ItemStatus,
    pub skip_reason: Option<SkipReason>,
    /// A duplicate's first request, which it collapsed into. Omitted on every item but a
    /// duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    /// Why the addon is installed, from its planned record; `dependency` on a dependency
    /// that failed to plan. Omitted for a removal and for an unchanged, skipped or other
    /// failed item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<AddonReason>,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    /// Omitted for items that never got a planned record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folders: Option<FolderSummary>,
    /// The addons that require it, from its planned record (empty for a removal), or the
    /// requesters of a dependency that failed to plan. Omitted for an unchanged, skipped
    /// or other failed item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_by: Option<Vec<String>>,
    #[serde(flatten)]
    pub deps: ItemDeps,
    pub error: Option<ItemError>,
}

/// What an item says about its dependencies beyond `reason` and `required_by` (C5),
/// each omitted for an item without a planned record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ItemDeps {
    /// What its other folders (bridges, plugins) work with that is not installed.
    /// Omitted for an item without a planned record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_dependencies: Option<Vec<BridgeDependency>>,
    /// The required folders nothing was installed for, and why. Omitted for an item
    /// without a planned record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unresolved: Option<Vec<Unresolved>>,
    /// `-v` only: its needed folders' `OptionalDeps`, never installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_dependencies: Option<Vec<FolderName>>,
    /// Listed right after its requester, as its dependency: a command that orders its
    /// items keeps it there.
    #[serde(skip)]
    pub listed_as_dependency: bool,
}

impl ItemDeps {
    /// `addon`'s, from its target's dependency reports and planned works.
    pub fn of(tp: &TargetPlan, addon: &str) -> Self {
        let report = report_of(&tp.dependencies, addon);
        Self {
            bridge_dependencies: Some(
                report
                    .map(|r| r.bridge_dependencies.clone())
                    .unwrap_or_default(),
            ),
            unresolved: Some(report.map(|r| r.unresolved.clone()).unwrap_or_default()),
            optional_dependencies: Some(optional_of(tp, addon)),
            listed_as_dependency: false,
        }
    }
}

/// `items` sorted by `key`, each requester with its dependency items kept right after it.
pub fn sort_keeping_dependencies<K: Ord>(
    items: &mut Vec<ItemResult>,
    key: impl Fn(&ItemResult) -> K,
) {
    let mut groups: Vec<Vec<ItemResult>> = Vec::new();
    for item in std::mem::take(items) {
        match groups.last_mut() {
            Some(group) if item.deps.listed_as_dependency => group.push(item),
            _ => groups.push(vec![item]),
        }
    }
    groups.sort_by_cached_key(|g| g.first().map(&key));
    *items = groups.into_iter().flatten().collect();
}

/// One dependency of a requester, as items list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepItem {
    /// Installed (or updated) with it: the folder it required and the dependency.
    Planned { folder: FolderName, addon: String },
    /// Resolved, but failed to plan: the folder and the dependency's ref.
    Failed { folder: FolderName, r#ref: Ref },
}

/// `requester`'s dependency items in resolution order, each followed by its own, every
/// dependency once: `listed` holds what is already listed (addon identities and ref
/// identities) and gains what this returns.
pub fn dependency_items(
    deps: &BTreeMap<String, DepReport>,
    requester: &str,
    listed: &mut BTreeSet<String>,
) -> Vec<DepItem> {
    let mut out = Vec::new();
    let Some(report) = report_of(deps, requester) else {
        return out;
    };
    for (folder, addon) in &report.installs {
        if listed.insert(addon_identity(addon)) {
            out.push(DepItem::Planned {
                folder: folder.clone(),
                addon: addon.clone(),
            });
            out.extend(dependency_items(deps, addon, listed));
        }
    }
    for (folder, r) in &report.failed {
        if listed.insert(r.identity()) {
            out.push(DepItem::Failed {
                folder: folder.clone(),
                r#ref: r.clone(),
            });
        }
    }
    out
}

/// `addon`'s dependency report (by name, ignoring case).
pub fn report_of<'a>(deps: &'a BTreeMap<String, DepReport>, addon: &str) -> Option<&'a DepReport> {
    deps.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(addon))
        .map(|(_, report)| report)
}

/// The `OptionalDeps` of `addon`'s needed folders, from its planned work.
pub fn optional_of(tp: &TargetPlan, addon: &str) -> Vec<FolderName> {
    tp.work
        .iter()
        .filter(|w| w.addon.eq_ignore_ascii_case(addon))
        .find_map(|w| match &w.mode {
            WorkMode::Folders(f) => Some(f.deps.optional.clone()),
            WorkMode::Records(_) => None,
        })
        .unwrap_or_default()
}

/// Items by outcome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Summary {
    pub targets: usize,
    /// Installed, updated, removed, adopted, pinned, unpinned or edited.
    pub changed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// One addon a command asked for on one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRequest {
    /// The user's argument.
    pub request: String,
    /// The target's id.
    pub target: String,
    /// The installed addon it names, when it names one.
    pub addon: Option<String>,
    /// The ref it names, when it names one.
    pub r#ref: Option<Ref>,
    /// The installed version, shown for an `unchanged` item.
    pub version: Option<String>,
}

impl ItemRequest {
    fn names(&self, addon: Option<&str>, r: Option<&Ref>) -> bool {
        let by_name =
            matches!((&self.addon, addon), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b));
        let by_ref = matches!((&self.r#ref, r), (Some(a), Some(b)) if a == b);
        by_name || by_ref
    }
}

/// A target as the engine planned it: its id, key and planned changes, and each
/// planned record's pin (by addon name), which says which way a pin edit went. With its
/// plan, items carry their dependency fields and each requester's dependencies follow
/// it as items of their own.
#[derive(Debug, Clone)]
pub struct Planned<'a> {
    pub target: &'a str,
    pub key: Option<TargetKey>,
    pub changes: &'a [Change],
    pub pinned: Vec<(String, bool)>,
    pub plan: Option<&'a TargetPlan>,
}

impl Planned<'_> {
    fn pinned_after(&self, addon: &str) -> Option<bool> {
        self.pinned
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(addon))
            .map(|(_, pinned)| *pinned)
    }

    /// The record `addon`'s work leaves.
    fn record_after(&self, addon: &str) -> Option<&AddonRecord> {
        self.plan?
            .work
            .iter()
            .filter(|w| w.addon.eq_ignore_ascii_case(addon))
            .find_map(|w| w.record())
    }
}

impl TargetsData {
    /// The results of one engine run with run id `run`, for the items `requested`.
    pub fn build(outcomes: &TxOutcomes, run: RunId, requested: &[ItemRequest]) -> Self {
        let planned: Vec<Planned<'_>> = outcomes
            .plan
            .targets
            .iter()
            .map(|tp| Planned {
                target: &tp.target,
                key: tp.key,
                changes: &tp.changes,
                pinned: tp
                    .work
                    .iter()
                    .filter_map(|w| w.record().map(|r| (r.name.clone(), r.pinned)))
                    .collect(),
                plan: Some(tp),
            })
            .collect();
        Self::assemble(
            &planned,
            &outcomes.outcomes,
            &outcomes.failures,
            outcomes.plan.dry_run,
            run,
            requested,
        )
    }

    /// [`TargetsData::build`] over the run's parts.
    pub fn assemble(
        planned: &[Planned<'_>],
        outcomes: &[TxOutcome],
        failures: &[PlanFailure],
        dry_run: bool,
        run: RunId,
        requested: &[ItemRequest],
    ) -> Self {
        let mut targets: Vec<TargetResult> = planned
            .iter()
            .map(|p| target_result(p, outcomes, failures, requested))
            .collect();
        targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
        let committed = targets.iter().any(|t| t.tx.is_some());
        let mut data = Self {
            dry_run,
            run: (committed && !dry_run).then_some(run),
            targets,
            summary: Summary::default(),
            precondition: None,
            plan_id: None,
            plan_expires_at: None,
            approval: None,
        };
        data.recount();
        data
    }

    /// Adds an item the command decided without the engine (a pinned addon `update`
    /// leaves alone), creating the target's result when it has none.
    pub fn push_item(&mut self, target: TargetRef, item: ItemResult) {
        match self.targets.iter_mut().find(|t| t.target.id == target.id) {
            Some(t) => t.items.push(item),
            None => self.push_target(TargetResult {
                items: vec![item],
                ..TargetResult::empty(target)
            }),
        }
        self.recount();
    }

    /// Adds a target that never reached the engine (a `sync` target without a section).
    pub fn push_target(&mut self, target: TargetResult) {
        self.targets.push(target);
        self.targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
        self.recount();
    }

    /// Recounts `summary` after a command changed items in place.
    pub fn recount(&mut self) {
        let mut summary = Summary {
            targets: self.targets.len(),
            ..Summary::default()
        };
        for item in self.targets.iter().flat_map(|t| &t.items) {
            match item.status {
                ItemStatus::Unchanged => summary.unchanged += 1,
                ItemStatus::Skipped => summary.skipped += 1,
                ItemStatus::Failed => summary.failed += 1,
                _ => summary.changed += 1,
            }
        }
        self.summary = summary;
    }

    /// `token` as `precondition`: a bound command's dry-run token, else `None`.
    #[must_use]
    pub fn with_precondition(mut self, token: Option<String>) -> Self {
        self.precondition = token;
        self
    }

    /// Adds the dry run's typed approval, without storing its preview.
    #[must_use]
    pub fn with_approval(mut self, approval: Option<Approval>) -> Self {
        self.approval = approval;
        self
    }

    /// Records the plan only after its file was stored successfully.
    pub fn with_plan(&mut self, plan_id: PlanId, expires_at: Timestamp) {
        self.plan_id = Some(plan_id);
        self.plan_expires_at = Some(expires_at);
    }

    /// Drops `unchanged` items unless `verbose` (`summary` keeps counting them).
    #[must_use]
    pub fn listing(mut self, verbose: bool) -> Self {
        if !verbose {
            for t in &mut self.targets {
                t.items.retain(|i| i.status != ItemStatus::Unchanged);
                for i in &mut t.items {
                    i.deps.optional_dependencies = None;
                }
            }
        }
        self
    }

    /// Some item or target failed.
    pub fn partial(&self) -> bool {
        self.summary.failed > 0 || self.targets.iter().any(|t| t.error.is_some())
    }

    /// When every item failed (nothing changed, stayed unchanged or was skipped): the
    /// first failure's error, whole (its own `details`, alternatives and retryability,
    /// as `install`'s), with every item's error beside them in `details.items`.
    ///
    /// A run bound to a preview (`--if-match`) that changed nothing and that a target's
    /// lock refused is that refusal too, whatever else was unchanged or skipped: what
    /// the preview approved did not run (M4-D19). With a commit elsewhere it stays
    /// partial (M4-D61).
    pub fn all_failed(&self) -> Option<Error> {
        let s = &self.summary;
        // A binding's refusal: the only `plan_stale` that is not retryable.
        let refusal = self
            .targets
            .iter()
            .filter_map(|t| t.error.as_ref())
            // ubs:ignore — comparing error codes, not secrets
            .find(|body| body.code == ErrorCode::PlanStale && !body.retryable)
            .filter(|_| s.changed == 0);
        if refusal.is_none() && (s.failed == 0 || s.changed + s.unchanged + s.skipped > 0) {
            return None;
        }
        let failed: Vec<(&TargetResult, &ItemResult)> = self
            .targets
            .iter()
            .flat_map(|t| t.items.iter().map(move |i| (t, i)))
            .filter(|(_, i)| i.status == ItemStatus::Failed)
            .collect();
        let details: Vec<serde_json::Value> = failed
            .iter()
            .map(|(t, i)| {
                serde_json::json!({
                    "target": t.target.id,
                    "request": i.request,
                    "error": i.error,
                })
            })
            .collect();
        // A target's own error keeps its form, which the item's wire fields do not: a
        // binding's refusal its retryability and `details.changed`, a stopped journal its
        // `journal_pending` and `reason`, a lock its `holder`. Without one, the first
        // item's original error keeps its own (a damaged archive's `blob` and `actual`).
        let mut error = match refusal {
            Some(body) => error_of(body),
            None => {
                let (first_target, first_item) = failed.first()?;
                match &first_target.error {
                    Some(body) => error_of(body),
                    None => first_item.error.as_ref()?.original_error(),
                }
            }
        };
        if let Some(kept) = error.details.as_object_mut() {
            kept.insert("items".to_owned(), serde_json::Value::Array(details));
        }
        Some(error)
    }

    /// The undo hints: `undo --run <run>` when more than one transaction committed, then
    /// `undo <tx>` per transaction, by target id.
    pub fn undo_commands(&self) -> Vec<CommandHint> {
        let committed: Vec<TxId> = self.targets.iter().filter_map(|t| t.tx).collect();
        let mut commands = Vec::new();
        if let (true, Some(run)) = (committed.len() > 1, self.run) {
            commands.push(CommandHint::new(
                Action::UndoRun,
                [
                    "omaddons".to_owned(),
                    "undo".to_owned(),
                    "--run".to_owned(),
                    run.to_string(),
                ],
            ));
        }
        for tx in committed {
            commands.push(CommandHint::new(
                Action::Undo,
                ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()],
            ));
        }
        commands
    }
}

impl TargetResult {
    /// A target with nothing in it yet.
    pub fn empty(target: TargetRef) -> Self {
        Self {
            target,
            tx: None,
            kind: None,
            no_section: false,
            items: Vec::new(),
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            creates_addons_dir: false,
            error: None,
        }
    }
}

/// How an item without a ref is identified: its addon's record key.
pub fn addon_identity(name: &str) -> String {
    format!(
        "addon:{}",
        crate::state::records::TargetRecord::addon_key(name)
    )
}

/// What one target's items are built from.
struct ItemsOf<'p, 'a> {
    p: &'p Planned<'a>,
    /// The committed (or, under `--dry-run`, planned) changes.
    changes: &'p [Change],
    /// The target's failures.
    mine: &'p [&'p PlanFailure],
    /// Nothing committed, for this reason.
    target_error: Option<&'p ErrorBody>,
}

/// The items built so far, and what they hold.
#[derive(Default)]
struct Listed {
    items: Vec<ItemResult>,
    /// Indices into the changes.
    used: BTreeSet<usize>,
    /// Addon and ref identities listed as dependencies (or asked for).
    deps: BTreeSet<String>,
}

impl ItemsOf<'_, '_> {
    /// `requester`'s dependencies as items, after it: each installed one with its
    /// change, each failed one with its failure, in resolution order.
    fn dependencies(&self, listed: &mut Listed, requester: &str) {
        let Some(tp) = self.p.plan else {
            return;
        };
        for dep in dependency_items(&tp.dependencies, requester, &mut listed.deps) {
            let item = match dep {
                DepItem::Planned { folder, addon } => {
                    let change = self
                        .changes
                        .iter()
                        .enumerate()
                        .find(|(_, c)| c.addon.eq_ignore_ascii_case(&addon));
                    match (change, self.target_error) {
                        (Some((i, change)), _) => {
                            listed.used.insert(i);
                            change_item(folder.as_str().to_owned(), change, self.p)
                        }
                        (None, Some(error)) => {
                            let req = ItemRequest {
                                request: folder.as_str().to_owned(),
                                target: self.p.target.to_owned(),
                                addon: Some(addon),
                                r#ref: None,
                                version: None,
                            };
                            failed_item(&req, None, None, error)
                        }
                        // Dropped under the lock: a concurrent run made it.
                        (None, None) => continue,
                    }
                }
                DepItem::Failed { folder, r#ref } => {
                    let failure = self.mine.iter().find(|f| f.r#ref.as_ref() == Some(&r#ref));
                    let Some(f) = failure else {
                        continue;
                    };
                    dependency_failed(&folder, f)
                }
            };
            let mut item = item;
            item.deps.listed_as_dependency = true;
            listed.items.push(item);
        }
    }
}

/// One planned target's result: requested items first, in request order, each followed
/// by its dependencies; then changes nobody asked for by name, except the bookkeeping
/// of `required_by` alone; then the dependencies of requesters no item lists.
fn target_result(
    p: &Planned<'_>,
    outcomes: &[TxOutcome],
    failures: &[PlanFailure],
    requested: &[ItemRequest],
) -> TargetResult {
    let outcome = outcomes.iter().find(|o| o.target == p.target);
    let mine: Vec<&PlanFailure> = failures.iter().filter(|f| f.target == p.target).collect();
    let target_error = mine
        .iter()
        .find(|f| f.r#ref.is_none() && f.addon.is_none())
        .map(|f| f.error.clone());
    let changes: &[Change] = match outcome {
        Some(o) if target_error.is_none() => &o.changes,
        _ => &[],
    };
    let of = ItemsOf {
        p,
        changes,
        mine: &mine,
        target_error: target_error.as_ref(),
    };
    let mut listed = Listed::default();
    // Each ref's (or named addon's) first request: a later one collapsed into it.
    let mut seen: Vec<(String, String)> = Vec::new();
    for req in requested.iter().filter(|r| r.target == p.target) {
        let identity = req.r#ref.as_ref().map_or_else(
            || addon_identity(req.addon.as_deref().unwrap_or(&req.request)),
            Ref::identity,
        );
        if let Some((_, first)) = seen.iter().find(|(i, _)| *i == identity) {
            listed.items.push(ItemResult {
                request: req.request.clone(),
                name: req.addon.clone(),
                r#ref: req.r#ref.clone(),
                status: ItemStatus::Skipped,
                skip_reason: Some(SkipReason::Duplicate),
                duplicate_of: Some(first.clone()),
                reason: None,
                from_version: None,
                to_version: None,
                folders: None,
                required_by: None,
                deps: ItemDeps::default(),
                error: None,
            });
            continue;
        }
        seen.push((identity, req.request.clone()));
        if let Some(f) = mine.iter().find(|f| {
            (f.r#ref.is_some() || f.addon.is_some())
                && req.names(f.addon.as_deref(), f.r#ref.as_ref())
        }) {
            let item = failed_item(req, f.addon.clone(), f.r#ref.clone(), &f.error);
            listed.items.push(item);
            continue;
        }
        if let Some((i, change)) = changes
            .iter()
            .enumerate()
            .find(|(_, c)| req.names(Some(&c.addon), Some(&c.r#ref)))
        {
            listed.used.insert(i);
            listed.deps.insert(addon_identity(&change.addon));
            listed
                .items
                .push(change_item(req.request.clone(), change, p));
            of.dependencies(&mut listed, &change.addon);
            continue;
        }
        let planned = p
            .changes
            .iter()
            .find(|c| req.names(Some(&c.addon), Some(&c.r#ref)));
        match (planned, &target_error) {
            (Some(c), Some(error)) => {
                listed.items.push(failed_item(
                    req,
                    Some(c.addon.clone()),
                    Some(c.r#ref.clone()),
                    error,
                ));
            }
            _ => listed.items.push(ItemResult {
                request: req.request.clone(),
                name: req.addon.clone(),
                r#ref: req.r#ref.clone(),
                status: ItemStatus::Unchanged,
                skip_reason: None,
                duplicate_of: None,
                reason: None,
                from_version: req.version.clone(),
                to_version: req.version.clone(),
                folders: None,
                required_by: None,
                deps: ItemDeps::default(),
                error: None,
            }),
        }
    }
    for (i, change) in changes.iter().enumerate() {
        // `required_by` bookkeeping on another addon is the plan's, not an item.
        let bookkeeping =
            change.kind == ChangeKind::Edit && change.fields == [EditField::RequiredBy];
        if listed.used.contains(&i) || bookkeeping {
            continue;
        }
        listed.used.insert(i);
        listed.deps.insert(addon_identity(&change.addon));
        listed
            .items
            .push(change_item(change.addon.clone(), change, p));
        of.dependencies(&mut listed, &change.addon);
    }
    if let Some(tp) = p.plan {
        for requester in tp.dependencies.keys() {
            of.dependencies(&mut listed, requester);
        }
    }
    let items = listed.items;
    TargetResult {
        target: TargetRef {
            id: p.target.to_owned(),
            key: outcome.and_then(|o| o.key).or(p.key),
            mode: None,
        },
        tx: outcome.and_then(|o| o.tx),
        kind: outcome.and_then(|o| o.kind),
        no_section: false,
        items,
        reload_required: outcome.map_or(ReloadRequired::None, |o| o.reload_required),
        atomic_swap: outcome.and_then(|o| o.tx.map(|_| o.atomic_swap)),
        creates_addons_dir: outcome.is_some_and(|o| o.creates_addons_dir),
        error: target_error,
    }
}

fn failed_item(
    req: &ItemRequest,
    name: Option<String>,
    r: Option<Ref>,
    error: &ErrorBody,
) -> ItemResult {
    ItemResult {
        request: req.request.clone(),
        name: name.or_else(|| req.addon.clone()),
        r#ref: r.or_else(|| req.r#ref.clone()),
        status: ItemStatus::Failed,
        skip_reason: None,
        duplicate_of: None,
        reason: None,
        from_version: req.version.clone(),
        to_version: None,
        folders: None,
        required_by: None,
        deps: ItemDeps::default(),
        error: Some(ItemError::from(error)),
    }
}

/// A dependency that resolved but failed to plan, as the item of the `folder` its
/// requesters required (`details.required_by` names them).
fn dependency_failed(folder: &FolderName, f: &PlanFailure) -> ItemResult {
    let required_by: Vec<String> = f.error.details["required_by"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    ItemResult {
        request: folder.as_str().to_owned(),
        name: f.addon.clone(),
        r#ref: f.r#ref.clone(),
        status: ItemStatus::Failed,
        skip_reason: None,
        duplicate_of: None,
        reason: Some(AddonReason::Dependency),
        from_version: None,
        to_version: None,
        folders: None,
        required_by: Some(required_by),
        deps: ItemDeps::default(),
        error: Some(ItemError::from(&f.error)),
    }
}

/// A change as an item. An edit of the pin alone is `pinned` or `unpinned` by the
/// planned record's new value. The planned record gives its `reason`, `required_by`
/// and dependency fields (a removal has none).
fn change_item(request: String, change: &Change, p: &Planned<'_>) -> ItemResult {
    let status = match change.kind {
        ChangeKind::Install => ItemStatus::Installed,
        ChangeKind::Update => ItemStatus::Updated,
        ChangeKind::Remove => ItemStatus::Removed,
        ChangeKind::Adopt => ItemStatus::Adopted,
        ChangeKind::Edit if change.fields == [EditField::Pinned] => {
            if p.pinned_after(&change.addon) == Some(false) {
                ItemStatus::Unpinned
            } else {
                ItemStatus::Pinned
            }
        }
        ChangeKind::Edit => ItemStatus::Edited,
    };
    let record = p.record_after(&change.addon);
    let deps = match (record, p.plan) {
        (Some(_), Some(tp)) => ItemDeps::of(tp, &change.addon),
        _ => ItemDeps::default(),
    };
    ItemResult {
        request,
        name: Some(change.addon.clone()),
        r#ref: Some(change.r#ref.clone()),
        status,
        skip_reason: None,
        duplicate_of: None,
        reason: record.map(|r| r.reason),
        from_version: change.from_version.clone(),
        to_version: change.to_version.clone(),
        folders: Some(sorted(&change.folders)),
        required_by: Some(record.map(|r| r.required_by.clone()).unwrap_or_default()),
        deps,
        error: None,
    }
}

/// Folder lists sorted by key.
pub fn sorted(folders: &FolderSummary) -> FolderSummary {
    let sort = |list: &Vec<crate::model::FolderName>| {
        let mut list = list.clone();
        list.sort_by(|a, b| a.key().cmp(b.key()));
        list
    };
    let mut skipped = folders.skipped.clone();
    skipped.sort_by(|a, b| a.name.key().cmp(b.name.key()));
    FolderSummary {
        added: sort(&folders.added),
        replaced: sort(&folders.replaced),
        removed: sort(&folders.removed),
        skipped,
        recorded: sort(&folders.recorded),
    }
}

/// A multi-target command's outcome.
#[derive(Debug)]
pub struct TargetsOutcome {
    pub command: &'static str,
    pub data: TargetsData,
    pub warnings: Vec<Warning>,
    pub commands: Vec<CommandHint>,
    /// Lines the command adds after the targets (`sync does not update; …`).
    pub notes: Vec<String>,
    /// Kept until dispatch offers this dry run to the plan store.
    pub(crate) preview: Option<Box<Preview>>,
}

impl Storable for TargetsOutcome {
    fn take_preview(&mut self) -> Option<Preview> {
        self.preview.take().map(|preview| *preview)
    }

    fn record_plan(&mut self, plan_id: PlanId, expires_at: Timestamp, hint: CommandHint) {
        self.data.with_plan(plan_id, expires_at);
        self.commands.insert(0, hint);
    }
}

impl Outcome for TargetsOutcome {
    type Data = TargetsData;

    fn command(&self) -> &'static str {
        self.command
    }

    fn data(&self) -> &TargetsData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn partial(&self) -> bool {
        self.data.partial()
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        render(w, self.command, &self.data, &self.notes)?;
        render_plan_line(
            w,
            self.data.plan_id.as_ref(),
            self.data.plan_expires_at.as_ref(),
        )?;
        // Behind `-v`: the stored plan's `apply` is the default way to run the preview.
        match self.data.precondition.as_deref() {
            Some(token) if style.detail() => {
                render_bind_line(w, token, self.data.plan_id.is_some())
            }
            _ => Ok(()),
        }
    }
}

/// The human rendering of `data` for `command`.
pub fn render(
    w: &mut dyn Write,
    command: &str,
    data: &TargetsData,
    notes: &[String],
) -> std::io::Result<()> {
    if data.targets.is_empty() {
        writeln!(w, "Nothing to {command}.")?;
    }
    for t in &data.targets {
        writeln!(w, "{}", header(command, data.dry_run, t))?;
        let width = t
            .items
            .iter()
            .map(|i| item_name(i).chars().count())
            .max()
            .unwrap_or(0);
        for i in &t.items {
            writeln!(
                w,
                "  {:<width$}  {}",
                item_name(i),
                item_detail(i, data.dry_run)
            )?;
            if let Some(error) = &i.error {
                writeln!(w, "  {:<width$}  fix: {}", "", error.fix)?;
            }
        }
    }
    let committed: Vec<TxId> = data.targets.iter().filter_map(|t| t.tx).collect();
    match committed.as_slice() {
        [] => {}
        [tx] => writeln!(w, "undo: omaddons undo {tx}")?,
        _ => writeln!(w, "Undo all of it with: omaddons undo")?,
    }
    for note in notes {
        writeln!(w, "{note}")?;
    }
    Ok(())
}

pub(crate) fn item_name(i: &ItemResult) -> &str {
    i.name.as_deref().unwrap_or(&i.request)
}

/// `retail: updated 2 addons (tx 01M5…) — /reload in game`.
pub(crate) fn header(command: &str, dry_run: bool, t: &TargetResult) -> String {
    let id = &t.target.id;
    if let Some(error) = &t.error {
        return format!("{id}: failed: {}: {}", error.code.wire(), error.message);
    }
    let changed: Vec<&ItemResult> = t.items.iter().filter(|i| i.status.changed()).collect();
    if changed.is_empty() {
        let skipped: Vec<String> = t
            .items
            .iter()
            .filter_map(|i| {
                i.skip_reason
                    .map(|r| format!("{} {}", item_name(i), r.phrase()))
            })
            .collect();
        let why = if skipped.is_empty() {
            String::new()
        } else {
            format!(" ({})", skipped.join(", "))
        };
        let nothing = if t.no_section {
            "no section in the desired set".to_owned()
        } else {
            format!("nothing to {command}")
        };
        return format!("{id}: {nothing}{why}");
    }
    let mut kinds: Vec<ItemStatus> = changed.iter().map(|i| i.status).collect();
    kinds.sort();
    kinds.dedup();
    let n = changed.len();
    let addons = if n == 1 { "addon" } else { "addons" };
    let what = match kinds.as_slice() {
        [ItemStatus::Adopted] => {
            let folders: usize = changed
                .iter()
                .filter_map(|i| i.folders.as_ref())
                .map(|f| f.recorded.len())
                .sum();
            let plural = if folders == 1 { "folder" } else { "folders" };
            let verb = ItemStatus::Adopted.verb(dry_run);
            format!("{verb} {n} {addons} ({folders} {plural})")
        }
        [only] => format!("{} {n} {addons}", only.verb(dry_run)),
        _ => {
            // A dry run's breakdown counts bare verbs: `1 install, 1 update`.
            let parts: Vec<String> = kinds
                .iter()
                .map(|k| {
                    let count = changed.iter().filter(|i| i.status == *k).count();
                    let verb = if dry_run {
                        k.base().unwrap_or(k.wire())
                    } else {
                        k.wire()
                    };
                    format!("{count} {verb}")
                })
                .collect();
            let changed = if dry_run { "would change" } else { "changed" };
            format!("{changed} {n} {addons} ({})", parts.join(", "))
        }
    };
    let mut line = format!("{id}: {what}");
    // An adoption records folders as they are: `retail: adopted 2 addons (3 folders), tx
    // 01M5… — no folder was changed`.
    let adoption = t.kind == Some(TxKind::Adopt);
    if let Some(tx) = t.tx {
        let short = super::history::short_tx(&tx);
        if adoption {
            line.push_str(&format!(", tx {short}"));
        } else {
            line.push_str(&format!(" (tx {short})"));
        }
    }
    match t.reload_required {
        ReloadRequired::Reload => line.push_str(" — /reload in game"),
        ReloadRequired::Restart => line.push_str(" — restart the game client"),
        ReloadRequired::None if adoption && t.tx.is_some() => {
            line.push_str(" — no folder was changed");
        }
        ReloadRequired::None => {}
    }
    line
}

/// What an item's line says after its name.
/// ` (dependency of LittleWigs)`, ` (dependency of A and B)`: what a dependency's line
/// ends with; empty for anything else.
pub fn dependency_suffix(reason: Option<AddonReason>, required_by: &[String]) -> String {
    let names = match required_by {
        _ if reason != Some(AddonReason::Dependency) => return String::new(),
        [] => return String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!(" (dependency of {names})")
}

pub(crate) fn item_detail(i: &ItemResult, dry_run: bool) -> String {
    let suffix = dependency_suffix(i.reason, i.required_by.as_deref().unwrap_or_default());
    format!("{}{suffix}", status_detail(i, dry_run))
}

fn status_detail(i: &ItemResult, dry_run: bool) -> String {
    let from = i.from_version.as_deref().unwrap_or("?");
    let to = i.to_version.as_deref().unwrap_or("?");
    let verb = i.status.verb(dry_run);
    match i.status {
        ItemStatus::Updated => format!("{from} → {to}"),
        ItemStatus::Installed | ItemStatus::Adopted | ItemStatus::Edited => {
            format!("{to} ({verb})")
        }
        ItemStatus::Removed => format!("{from} ({verb})"),
        ItemStatus::Pinned | ItemStatus::Unpinned => format!("{verb} at {to}"),
        ItemStatus::Unchanged => format!("{to} (unchanged)"),
        ItemStatus::Skipped => match i.skip_reason {
            Some(reason) => format!("skipped: {}", reason.phrase()),
            None => "skipped".to_owned(),
        },
        ItemStatus::Failed => match &i.error {
            Some(e) => format!("failed: {}: {}", e.code.wire(), e.message),
            None => "failed".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FolderName;

    const RUN: &str = "01JB8Q2VR00000000000000000";

    fn run() -> RunId {
        RUN.parse().unwrap()
    }

    #[test]
    fn item_details_keep_a_fences_stop_and_nothing_more() {
        let tx = "01JB8Q2VA00000000000000009";
        let fence = serde_json::json!({
            "journal_pending": tx,
            "reason": "journal_unreadable",
            "target": null,
            "tx": tx,
            "resource": format!("journal:{tx}"),
            "items": [],
        });
        let kept = item_details(ErrorCode::StateCorrupt, &fence).unwrap();
        assert_eq!(
            kept,
            serde_json::json!({
                "journal_pending": tx,
                "reason": "journal_unreadable",
                "target": null,
                "tx": tx,
                "resource": format!("journal:{tx}"),
            })
        );
        // The same error back from the item keeps them, so an all-failed run reports the
        // fencing journal as a stopped one.
        let item = ItemError::from(
            &Error::new(ErrorCode::StateCorrupt, "fenced", Suggestion::text("x")).details(fence),
        );
        assert_eq!(item.error().details, kept);
        // Without a resource, an error of that code keeps nothing, as before.
        let corrupt = serde_json::json!({"reason": "journal_unreadable", "tx": tx});
        assert_eq!(item_details(ErrorCode::StateCorrupt, &corrupt), None);
    }

    fn change(addon: &str, kind: ChangeKind, from: Option<&str>, to: Option<&str>) -> Change {
        Change {
            addon: addon.to_owned(),
            r#ref: format!("gh:o/{}", addon.to_lowercase()).parse().unwrap(),
            kind,
            fields: Vec::new(),
            from_version: from.map(str::to_owned),
            to_version: to.map(str::to_owned),
            folders: FolderSummary {
                replaced: vec![FolderName::new(addon).unwrap()],
                ..FolderSummary::default()
            },
        }
    }

    fn outcome(target: &str, tx: Option<&str>, changes: Vec<Change>) -> TxOutcome {
        TxOutcome {
            target: target.to_owned(),
            key: Some(TargetKey::from_ulid(ulid(target))),
            tx: tx.map(|t| t.parse().unwrap()),
            kind: tx.map(|_| TxKind::Update),
            dry_run: false,
            changes,
            atomic_swap: true,
            reload_required: ReloadRequired::None,
            warnings: Vec::new(),
            creates_addons_dir: false,
            lock_held_ms: None,
            notes: Vec::new(),
        }
    }

    /// A stable ULID per target name, for keys.
    fn ulid(target: &str) -> ulid::Ulid {
        let n = target
            .bytes()
            .fold(0u128, |acc, b| acc * 31 + u128::from(b));
        ulid::Ulid::from(n)
    }

    fn request(target: &str, addon: &str) -> ItemRequest {
        ItemRequest {
            request: addon.to_owned(),
            target: target.to_owned(),
            addon: Some(addon.to_owned()),
            r#ref: None,
            version: Some("1".to_owned()),
        }
    }

    fn failure(target: &str, addon: Option<&str>, code: ErrorCode) -> PlanFailure {
        let error = Error::new(code, format!("{code:?} here"), Suggestion::text("wait"));
        PlanFailure {
            r#ref: None,
            addon: addon.map(str::to_owned),
            target: target.to_owned(),
            error: ErrorBody::from(error.clone()),
            cause: Some(error),
        }
    }

    fn planned<'a>(target: &'a str, changes: &'a [Change]) -> Planned<'a> {
        Planned {
            target,
            key: None,
            changes,
            pinned: Vec::new(),
            plan: None,
        }
    }

    /// Three targets: retail updated two addons and left one current, era failed one
    /// addon and updated another, classic's commit failed.
    fn three() -> TargetsData {
        let retail = vec![
            change("DBM", ChangeKind::Update, Some("1"), Some("2")),
            change("Plater", ChangeKind::Update, Some("661"), Some("662")),
        ];
        let era = vec![change("Questie", ChangeKind::Update, Some("9"), Some("10"))];
        let classic = vec![change("DBM", ChangeKind::Update, Some("1"), Some("2"))];
        let outcomes = vec![
            outcome("retail", Some("01JB8Q2VA00000000000000001"), retail.clone()),
            outcome("era", Some("01JB8Q2VA00000000000000002"), era.clone()),
        ];
        let failures = vec![
            failure("era", Some("Bartender4"), ErrorCode::OwnershipConflict),
            failure("classic", None, ErrorCode::Locked),
        ];
        let requested = vec![
            request("retail", "DBM"),
            request("retail", "Details"),
            request("retail", "Plater"),
            request("era", "Questie"),
            request("era", "Bartender4"),
            request("classic", "DBM"),
        ];
        TargetsData::assemble(
            &[
                planned("retail", &retail),
                planned("era", &era),
                planned("classic", &classic),
            ],
            &outcomes,
            &failures,
            false,
            run(),
            &requested,
        )
    }

    fn statuses(data: &TargetsData) -> Vec<(String, String, ItemStatus)> {
        data.targets
            .iter()
            .flat_map(|t| {
                t.items
                    .iter()
                    .map(|i| (t.target.id.clone(), i.request.clone(), i.status))
            })
            .collect()
    }

    #[test]
    fn builds_items_per_target_in_id_order() {
        let data = three();
        assert_eq!(
            statuses(&data),
            [
                ("classic".into(), "DBM".into(), ItemStatus::Failed),
                ("era".into(), "Questie".into(), ItemStatus::Updated),
                ("era".into(), "Bartender4".into(), ItemStatus::Failed),
                ("retail".into(), "DBM".into(), ItemStatus::Updated),
                ("retail".into(), "Details".into(), ItemStatus::Unchanged),
                ("retail".into(), "Plater".into(), ItemStatus::Updated),
            ]
        );
        let classic = &data.targets[0];
        assert_eq!(
            classic.error.as_ref().map(|e| e.code),
            Some(ErrorCode::Locked)
        );
        assert_eq!(
            classic.items[0].error.as_ref().map(|e| e.code),
            Some(ErrorCode::Locked)
        );
        assert_eq!(classic.tx, None);
        assert_eq!(
            data.summary,
            Summary {
                targets: 3,
                changed: 3,
                unchanged: 1,
                skipped: 0,
                failed: 2,
            }
        );
        assert!(data.partial());
        assert_eq!(data.run, Some(run()));
        assert!(data.all_failed().is_none());
        let hints: Vec<(String, String)> = data
            .undo_commands()
            .iter()
            .map(|c| (c.action().wire().to_owned(), c.command().to_owned()))
            .collect();
        assert_eq!(
            hints,
            [
                ("undo_run".into(), format!("omaddons undo --run {RUN}")),
                (
                    "undo".into(),
                    "omaddons undo 01JB8Q2VA00000000000000002".into()
                ),
                (
                    "undo".into(),
                    "omaddons undo 01JB8Q2VA00000000000000001".into()
                ),
            ]
        );
        // Unchanged items are counted but listed only with -v.
        let listed = data.clone().listing(false);
        assert_eq!(listed.summary, data.summary);
        assert_eq!(statuses(&listed).len(), 5);
        assert_eq!(statuses(&data.listing(true)).len(), 6);
    }

    #[test]
    fn dry_runs_name_no_run_and_pushed_items_count() {
        let retail = vec![change("DBM", ChangeKind::Install, None, Some("2"))];
        let mut dry = outcome("retail", None, retail.clone());
        dry.dry_run = true;
        let mut data = TargetsData::assemble(
            &[planned("retail", &retail)],
            &[dry],
            &[],
            true,
            run(),
            &[request("retail", "DBM")],
        );
        assert_eq!((data.dry_run, data.run), (true, None));
        assert_eq!(statuses(&data)[0].2, ItemStatus::Installed);
        assert!(data.undo_commands().is_empty());
        let forever = TargetRef {
            id: "forever".into(),
            key: None,
            mode: None,
        };
        data.push_item(
            forever,
            ItemResult {
                request: "Questie".into(),
                name: Some("Questie".into()),
                r#ref: None,
                status: ItemStatus::Skipped,
                skip_reason: Some(SkipReason::Pinned),
                duplicate_of: None,
                reason: None,
                from_version: Some("1".into()),
                to_version: None,
                folders: None,
                required_by: None,
                deps: ItemDeps::default(),
                error: None,
            },
        );
        assert_eq!(data.targets[0].target.id, "forever", "kept in id order");
        assert_eq!((data.summary.targets, data.summary.skipped), (2, 1));
        assert!(!data.partial());
    }

    #[test]
    fn every_item_failed_is_the_first_error() {
        let failures = vec![
            failure("retail", Some("DBM"), ErrorCode::GameRunning),
            failure("retail", Some("Plater"), ErrorCode::OwnershipConflict),
        ];
        let data = TargetsData::assemble(
            &[planned("retail", &[])],
            &[outcome("retail", None, Vec::new())],
            &failures,
            false,
            run(),
            &[request("retail", "DBM"), request("retail", "Plater")],
        );
        let error = data.all_failed().unwrap();
        assert_eq!(error.code, ErrorCode::GameRunning);
        assert_eq!(error.details["items"].as_array().unwrap().len(), 2);
        assert_eq!(data.run, None, "nothing committed");
    }

    /// The command's error is the first failed item's own (omad-r86s): its `details` keys
    /// beside `items`, its retryability and alternatives, while each item's wire `error`
    /// keeps only what it always kept.
    #[test]
    fn all_failed_keeps_the_first_items_own_error() {
        let failed_with = |error: Error| {
            let failure = PlanFailure {
                r#ref: None,
                addon: Some("DBM".to_owned()),
                target: "retail".to_owned(),
                error: ErrorBody::from(error.clone()),
                cause: Some(error),
            };
            TargetsData::assemble(
                &[planned("retail", &[])],
                &[outcome("retail", None, Vec::new())],
                &[failure],
                false,
                run(),
                &[request("retail", "DBM")],
            )
        };
        let cases = [
            (
                "a damaged cached archive",
                Error::new(
                    ErrorCode::HashMismatch,
                    "cached archive 0a1b… is damaged",
                    Suggestion::command(["omaddons", "doctor", "--include-deep"]),
                )
                .retryable(true)
                .details(serde_json::json!({ "blob": "0a1b", "actual": "ffee" })),
                vec!["actual", "blob", "items"],
            ),
            (
                "no build loads here",
                Error::new(
                    ErrorCode::NoCompatibleRelease,
                    "no build of DBM loads on retail",
                    Suggestion::text("use an archive built for this client"),
                )
                .alternative(Suggestion::command(["omaddons", "info", "DBM"]))
                .details(serde_json::json!({
                    "tried": [{ "asset": "DBM.zip", "code": "wrong_flavor", "message": "mainline" }],
                })),
                vec!["items", "tried"],
            ),
        ];
        for (case, original, keys) in cases {
            let data = failed_with(original.clone());
            let error = data.all_failed().unwrap();
            let mut found: Vec<&str> = error
                .details
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            found.sort_unstable();
            assert_eq!(found, keys, "{case}: {error:?}");
            for key in keys.iter().filter(|k| **k != "items") {
                assert_eq!(error.details[*key], original.details[*key], "{case}: {key}");
            }
            assert_eq!(
                (error.code, error.retryable, &error.alternatives),
                (original.code, original.retryable, &original.alternatives),
                "{case}: the original error whole"
            );
            let items = error.details["items"].as_array().unwrap();
            assert_eq!(items.len(), 1, "{case}: {error:?}");
            assert!(
                items[0]["error"].get("details").is_none(),
                "{case}: no item error gains a key: {}",
                items[0]
            );
            let wire = serde_json::to_value(&data.targets[0].items[0]).unwrap();
            assert!(
                wire["error"].get("details").is_none() && wire["error"].get("original").is_none(),
                "{case}: the item's wire error is unchanged: {wire}"
            );

            // A fix the command rewrites on the item (`install … --force` for a command
            // without `--force`) is the command-level error's fix too.
            let mut rewritten = data.clone();
            let install = Suggestion::command(["omaddons", "install", "DBM", "--force"]);
            rewritten.targets[0].items[0]
                .error
                .as_mut()
                .unwrap()
                .set_fix(install.clone());
            let error = rewritten.all_failed().unwrap();
            assert_eq!(error.fix, install, "{case}: {error:?}");
            assert_eq!(
                rewritten.targets[0].items[0]
                    .error
                    .as_ref()
                    .unwrap()
                    .fix_argv,
                install.argv().map(<[String]>::to_vec),
                "{case}"
            );
        }
    }

    #[test]
    fn targets_data_serializes_a_null_precondition_until_a_command_sets_it() {
        let mut data = three();
        let json = serde_json::to_value(&data).unwrap();
        assert!(
            json.as_object().unwrap().contains_key("precondition")
                && json["precondition"].is_null(),
            "{json}"
        );
        let token = format!("pc1:{}", "0".repeat(64));
        data.precondition = Some(token.clone());
        assert_eq!(serde_json::to_value(&data).unwrap()["precondition"], token);
        let data = three().with_precondition(None);
        assert!(data.precondition.is_none(), "with_precondition(None)");
    }

    #[test]
    fn targets_data_keys_are_null_until_set() {
        let outcomes = TxOutcomes {
            plan: crate::engine::plan::Plan::idle(&[], &[], crate::model::Actor::Agent),
            outcomes: Vec::new(),
            failures: Vec::new(),
            warnings: Vec::new(),
        };
        let approval = Approval::nothing(&crate::app::approval::ApprovalFlags::default());
        let plan_id: PlanId = "01JB8Q2VR00000000000000001".parse().unwrap();
        let expires: Timestamp = "2026-10-28T00:00:00Z".parse().unwrap();
        for (case, data) in [
            ("build", TargetsData::build(&outcomes, run(), &[])),
            (
                "assemble",
                TargetsData::assemble(&[], &[], &[], true, run(), &[]),
            ),
        ] {
            let json = serde_json::to_value(&data).unwrap();
            for key in ["plan_id", "plan_expires_at", "approval"] {
                assert!(
                    json.as_object().unwrap().contains_key(key),
                    "{case}: {json}"
                );
                assert!(json[key].is_null(), "{case}: {key}: {json}");
            }
            let text = serde_json::to_string(&data).unwrap();
            let keys = ["precondition", "plan_id", "plan_expires_at", "approval"];
            for pair in keys.windows(2) {
                let before = text.find(&format!("\"{}\":", pair[0])).unwrap();
                let after = text.find(&format!("\"{}\":", pair[1])).unwrap();
                assert!(before < after, "{case}: {pair:?}: {text}");
            }
            let mut set = data.with_approval(Some(approval.clone()));
            set.with_plan(plan_id, expires);
            let json = serde_json::to_value(&set).unwrap();
            assert_eq!(json["plan_id"], plan_id.to_string(), "{case}: {json}");
            assert_eq!(
                json["plan_expires_at"],
                expires.to_string(),
                "{case}: {json}"
            );
            assert_eq!(
                json["approval"],
                serde_json::to_value(&approval).unwrap(),
                "{case}: {json}"
            );
            assert!(set.with_approval(None).approval.is_none(), "{case}");
        }
    }

    #[test]
    fn the_token_line_is_shown_only_under_verbose() {
        let token = format!("pc1:{}", "0123456789abcdef".repeat(4));
        let line = format!("Run it bound to this preview: --if-match {token}\n");
        let outcome = |data: TargetsData| TargetsOutcome {
            command: "update",
            data,
            warnings: Vec::new(),
            commands: Vec::new(),
            notes: vec!["a note".to_owned()],
            preview: None,
        };
        let human = |o: &TargetsOutcome, verbose: u8| {
            let mut out = Vec::new();
            let style = Style {
                color: false,
                verbose,
                quiet: false,
            };
            o.render_human(&mut out, &style).unwrap();
            String::from_utf8(out).unwrap()
        };
        let dry = TargetsData::assemble(&[], &[], &[], true, run(), &[]);
        let bound = outcome(dry.clone().with_precondition(Some(token.clone())));
        let unbound = outcome(dry);
        let real = outcome(three());
        for (case, o, verbose, shown) in [
            ("a bound dry run, without -v", &bound, 0, false),
            ("a bound dry run, with -v", &bound, 1, true),
            ("a dry run without a token, with -v", &unbound, 1, false),
            ("a real run, with -v", &real, 1, false),
        ] {
            let text = human(o, verbose);
            assert_eq!(text.ends_with(&line), shown, "{case}: {text}");
            assert_eq!(
                text.matches("--if-match").count(),
                usize::from(shown),
                "{case}: {text}"
            );
            assert!(text.contains("a note\n"), "{case}: the notes stay: {text}");
        }
        assert_eq!(
            human(&bound, 0),
            human(&unbound, 0),
            "without -v the output is unchanged"
        );
        let mut stored = bound;
        let plan_id: PlanId = "01JB8Q2VR00000000000000001".parse().unwrap();
        let expires = crate::clock::now()
            .checked_add(jiff::SignedDuration::from_hours(24))
            .unwrap();
        stored.data.with_plan(plan_id, expires);
        // A kept plan: its apply ends the output, and under -v the token is the other way.
        let plan_line = format!(
            "Plan {plan_id} stored for 24 hours. Run exactly this: omaddons apply {plan_id}\n"
        );
        let or_bind = format!("Or bind a typed run to it: --if-match {token}\n");
        for (verbose, ending) in [(0, plan_line.clone()), (1, format!("{plan_line}{or_bind}"))] {
            let text = human(&stored, verbose);
            assert!(text.ends_with(&ending), "verbose {verbose}: {text}");
            assert!(!text.contains(&line), "verbose {verbose}: {text}");
        }
    }

    /// The command's error when one target's commit failed with `error` and so did its
    /// one item.
    fn all_failed_with(error: Error) -> Error {
        let changes = [change("DBM", ChangeKind::Edit, Some("1"), Some("1"))];
        let failure = PlanFailure {
            r#ref: None,
            addon: None,
            target: "retail".to_owned(),
            error: ErrorBody::from(error.clone()),
            cause: Some(error),
        };
        TargetsData::assemble(
            &[planned("retail", &changes)],
            &[],
            &[failure],
            false,
            run(),
            &[request("retail", "DBM")],
        )
        .all_failed()
        .unwrap()
    }

    #[test]
    fn an_all_failed_target_error_keeps_its_retryability_and_details() {
        let preview: Vec<String> = ["omaddons", "pin", "DBM", "--dry-run"]
            .map(str::to_owned)
            .to_vec();
        let items = |error: &Error| error.details["items"].as_array().map_or(0, Vec::len);

        // A binding's refusal under the lock: kept whole.
        let bound = all_failed_with(crate::app::precondition::stale(
            "pin",
            &[crate::app::precondition::Part::Targets],
            &preview,
        ));
        let body = ErrorBody::from(bound.clone());
        assert_eq!(body.code, ErrorCode::PlanStale, "{body:?}");
        assert!(!body.retryable, "the same token can only fail again");
        assert_eq!(
            body.details["changed"],
            serde_json::json!(["targets"]),
            "{body:?}"
        );
        assert_eq!(body.fix_argv, Some(preview.clone()));
        assert_eq!(items(&bound), 1, "{body:?}");

        // The engine's re-plan form: retryable, with its cause.
        let cause = Error::new(
            ErrorCode::HashMismatch,
            "a hash moved",
            Suggestion::text("x"),
        );
        let replan = all_failed_with(
            Error::plan_stale(
                crate::error::PlanStaleSource::Replan,
                &[crate::error::StalePart::Targets],
                format!(
                    "the target changed while this command planned: {}",
                    cause.message
                ),
                Suggestion::text("run the command again; it re-plans against the current state"),
            )
            .extend_details(
                serde_json::json!({ "cause": cause.code.wire(), "cause_message": cause.message }),
            ),
        );
        assert!(replan.retryable, "{replan:?}");
        assert_eq!(replan.details["cause"], "hash_mismatch", "{replan:?}");
        assert_eq!(items(&replan), 1, "{replan:?}");

        // Any other target-level error keeps its details too: a journal stopped for
        // repair names its transaction and why (omad-alz5), a lock its holder.
        let stopped = all_failed_with(
            Error::new(
                ErrorCode::Internal,
                "journal step 2 matches neither its before nor its after state",
                Suggestion::command(["omaddons", "status"]),
            )
            .details(serde_json::json!({
                "reason": "step_unrecognized",
                "journal_pending": "01M6498HYPZBRS6MK9NAN7T5Y9",
            })),
        );
        assert_eq!(stopped.code, ErrorCode::Internal);
        assert_eq!(
            stopped.details["reason"], "step_unrecognized",
            "{stopped:?}"
        );
        assert_eq!(
            stopped.details["journal_pending"], "01M6498HYPZBRS6MK9NAN7T5Y9",
            "{stopped:?}"
        );
        assert_eq!(items(&stopped), 1, "{stopped:?}");
        let locked = all_failed_with(
            Error::new(ErrorCode::Locked, "locked here", Suggestion::text("wait"))
                .details(serde_json::json!({ "holder": { "pid": 7 } })),
        );
        assert_eq!(locked.code, ErrorCode::Locked);
        assert_eq!(
            locked.retryable,
            ErrorCode::Locked.default_retryable(),
            "the registry's default"
        );
        let keys: Vec<&String> = locked.details.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["holder", "items"], "{locked:?}");
    }

    /// A bound run that changed nothing and that a target's lock refused is that refusal,
    /// though another item was unchanged; a commit elsewhere keeps it partial; an
    /// unbound target error beside an unchanged item is still a partial run.
    #[test]
    fn a_binding_refusal_that_changed_nothing_is_the_refusal() {
        let preview: Vec<String> = ["omaddons", "sync", "--dry-run"]
            .map(str::to_owned)
            .to_vec();
        let refused = crate::app::precondition::stale(
            "sync",
            &[crate::app::precondition::Part::Targets],
            &preview,
        );
        let replan = Error::plan_stale(
            crate::error::PlanStaleSource::Replan,
            &[crate::error::StalePart::Targets],
            "the target changed while this command planned",
            Suggestion::text("run the command again"),
        );
        // Foo's commit on retail failed with `error`, Baz was unchanged there, and with
        // `committed` era updated Questie.
        let data = |error: &Error, committed: bool| {
            let foo = [change("Foo", ChangeKind::Install, None, Some("1"))];
            let questie = [change("Questie", ChangeKind::Update, Some("9"), Some("10"))];
            let mut planned_targets = vec![planned("retail", &foo)];
            let mut outcomes = Vec::new();
            let mut requested = vec![request("retail", "Foo")];
            if committed {
                planned_targets.push(planned("era", &questie));
                outcomes.push(outcome(
                    "era",
                    Some("01JB8Q2VA00000000000000002"),
                    questie.to_vec(),
                ));
                requested.push(request("era", "Questie"));
            }
            let failure = PlanFailure {
                r#ref: None,
                addon: None,
                target: "retail".to_owned(),
                error: ErrorBody::from(error.clone()),
                cause: Some(error.clone()),
            };
            let mut data = TargetsData::assemble(
                &planned_targets,
                &outcomes,
                &[failure],
                false,
                run(),
                &requested,
            );
            data.push_item(
                TargetRef {
                    id: "retail".into(),
                    key: None,
                    mode: None,
                },
                item_with("Baz", ItemStatus::Unchanged),
            );
            data
        };
        let bound = data(&refused, false);
        assert_eq!(
            (
                bound.summary.changed,
                bound.summary.unchanged,
                bound.summary.failed
            ),
            (0, 1, 1),
            "{:?}",
            bound.summary
        );
        let error = bound.all_failed().expect("the refusal");
        let body = ErrorBody::from(error.clone());
        assert_eq!(
            (
                body.code,
                body.retryable,
                &body.details["changed"],
                &body.fix_argv
            ),
            (
                ErrorCode::PlanStale,
                false,
                &serde_json::json!(["targets"]),
                &Some(preview.clone())
            ),
            "{body:?}"
        );
        assert_eq!(
            error.details["items"].as_array().map(Vec::len),
            Some(1),
            "the failed item, beside changed: {error:?}"
        );
        assert!(
            data(&refused, true).all_failed().is_none(),
            "a commit on era: partial (M4-D61)"
        );
        assert!(
            data(&replan, false).all_failed().is_none(),
            "an unbound target error beside an unchanged item: partial, as before"
        );
    }

    /// A failed item keeps why a running client refused it, and whether `/proc` could
    /// tell; other codes keep only their own short list. The run's error, when every
    /// item failed, keeps them beside `items`.
    #[test]
    fn a_running_refusal_keeps_its_reason_on_the_item_and_the_run() {
        use serde_json::json;

        let details = json!({
            "target": "retail",
            "reason": "loaded_toc",
            "running": "unknown",
            "ops": ["remove DBM"],
        });
        let refused = Error::new(ErrorCode::GameRunning, "running", Suggestion::text("wait"))
            .details(details.clone());
        assert_eq!(
            ItemError::from(&refused).details,
            Some(json!({ "reason": "loaded_toc", "running": "unknown" }))
        );
        let other =
            Error::new(ErrorCode::NotFound, "gone", Suggestion::text("wait")).details(details);
        assert_eq!(ItemError::from(&other).details, None);

        let mut failed = failure("retail", Some("DBM"), ErrorCode::GameRunning);
        failed.error = ErrorBody::from(refused.clone());
        failed.cause = Some(refused);
        let data = TargetsData::assemble(
            &[planned("retail", &[])],
            &[outcome("retail", None, Vec::new())],
            &[failed],
            false,
            run(),
            &[request("retail", "DBM")],
        );
        let error = data.all_failed().unwrap();
        assert_eq!(
            (&error.details["reason"], &error.details["running"]),
            (&json!("loaded_toc"), &json!("unknown"))
        );
        assert_eq!(error.details["items"][0]["request"], "DBM");
    }

    #[test]
    fn pin_edits_read_the_planned_record() {
        let mut pin = change("DBM", ChangeKind::Edit, Some("1"), Some("1"));
        pin.fields = vec![EditField::Pinned];
        let changes = vec![pin];
        let mut p = planned("retail", &changes);
        for (pinned, status) in [(true, ItemStatus::Pinned), (false, ItemStatus::Unpinned)] {
            p.pinned = vec![("DBM".into(), pinned)];
            let data = TargetsData::assemble(
                std::slice::from_ref(&p),
                &[outcome(
                    "retail",
                    Some("01JB8Q2VA00000000000000001"),
                    changes.clone(),
                )],
                &[],
                false,
                run(),
                &[request("retail", "DBM")],
            );
            assert_eq!(statuses(&data)[0].2, status);
        }
    }

    #[test]
    fn wire_keys_keep_their_order() {
        let data = three();
        // Each object on its own, so a nested key never stands in for its parent's.
        let order = |json: String, keys: &[&str]| {
            let at: Vec<usize> = keys
                .iter()
                .map(|k| json.find(&format!("\"{k}\":")).unwrap())
                .collect();
            assert!(at.is_sorted(), "{keys:?}\n{json}");
        };
        let mut empty = data.clone();
        empty.targets.clear();
        order(
            serde_json::to_string(&empty).unwrap(),
            &["dry_run", "run", "targets", "summary"],
        );
        let mut target = data.targets[2].clone();
        let item = target.items.remove(0);
        target.items.clear();
        order(
            serde_json::to_string(&target).unwrap(),
            &[
                "target",
                "tx",
                "kind",
                "no_section",
                "items",
                "reload_required",
                "atomic_swap",
                "creates_addons_dir",
                "error",
            ],
        );
        order(
            serde_json::to_string(&item).unwrap(),
            &[
                "request",
                "name",
                "ref",
                "status",
                "skip_reason",
                "from_version",
                "to_version",
                "folders",
                "required_by",
                "error",
            ],
        );
        order(
            serde_json::to_string(&data.summary).unwrap(),
            &["targets", "changed", "unchanged", "skipped", "failed"],
        );
    }

    fn rendered(data: &TargetsData) -> String {
        let mut out = Vec::new();
        render(&mut out, "update", data, &[]).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn renders_zero_one_and_three_targets() {
        let none = TargetsData::assemble(&[], &[], &[], false, run(), &[]);
        let retail = vec![change("DBM", ChangeKind::Update, Some("1"), Some("2"))];
        let mut one_outcome = outcome("retail", Some("01JB8Q2VA00000000000000001"), retail.clone());
        one_outcome.reload_required = ReloadRequired::Reload;
        let one = TargetsData::assemble(
            &[planned("retail", &retail)],
            &[one_outcome],
            &[],
            false,
            run(),
            &[request("retail", "DBM"), request("retail", "Details")],
        )
        .listing(false);
        let three = three().listing(false);
        insta::assert_snapshot!("targets_human_0", rendered(&none));
        insta::assert_snapshot!("targets_human_1", rendered(&one));
        insta::assert_snapshot!("targets_human_3", rendered(&three));
        insta::assert_json_snapshot!("targets_json_0", none);
        insta::assert_json_snapshot!("targets_json_1", one);
        insta::assert_json_snapshot!("targets_json_3", three);
    }

    fn item_with(name: &str, status: ItemStatus) -> ItemResult {
        ItemResult {
            request: name.to_owned(),
            name: Some(name.to_owned()),
            r#ref: None,
            status,
            skip_reason: None,
            duplicate_of: None,
            reason: None,
            from_version: Some("1".to_owned()),
            to_version: Some("2".to_owned()),
            folders: None,
            required_by: None,
            deps: ItemDeps::default(),
            error: None,
        }
    }

    /// A dry run's lines say what would happen, never "would be installed".
    #[test]
    fn a_dry_run_says_what_would_happen() {
        use ItemStatus::{
            Adopted, Edited, Installed, Pinned, Removed, Unchanged, Unpinned, Updated,
        };
        for (status, real, dry) in [
            (Installed, "2 (installed)", "2 (would install)"),
            (Updated, "1 → 2", "1 → 2"),
            (Removed, "1 (removed)", "1 (would remove)"),
            (Adopted, "2 (adopted)", "2 (would adopt)"),
            (Pinned, "pinned at 2", "would pin at 2"),
            (Unpinned, "unpinned at 2", "would unpin at 2"),
            (Edited, "2 (edited)", "2 (would edit)"),
            (Unchanged, "2 (unchanged)", "2 (unchanged)"),
        ] {
            let i = item_with("Foo", status);
            assert_eq!(
                (status_detail(&i, false), status_detail(&i, true)),
                (real.to_owned(), dry.to_owned()),
                "{status:?}"
            );
        }
        let target = |items| TargetResult {
            target: TargetRef {
                id: "retail".to_owned(),
                key: None,
                mode: None,
            },
            tx: None,
            kind: None,
            no_section: false,
            items,
            reload_required: ReloadRequired::None,
            atomic_swap: None,
            creates_addons_dir: false,
            error: None,
        };
        let one = target(vec![item_with("Foo", Installed)]);
        assert_eq!(header("sync", true, &one), "retail: would install 1 addon");
        assert_eq!(header("sync", false, &one), "retail: installed 1 addon");
        let two = target(vec![item_with("Foo", Installed), item_with("Bar", Updated)]);
        assert_eq!(
            header("sync", true, &two),
            "retail: would change 2 addons (1 install, 1 update)"
        );
        assert_eq!(
            header("sync", false, &two),
            "retail: changed 2 addons (1 installed, 1 updated)"
        );
    }
}
