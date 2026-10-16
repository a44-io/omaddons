//! Planning (vision §5.5 step 1, DEC-15): every requested change resolved, without the
//! lock, into concrete folder operations against a snapshot that records each target's
//! `generation` (D24: the commit revalidates under the lock).
//!
//! Resolution is the fetch/select/verify split (§5.4, D28): each distinct ref is
//! fetched once across all targets, candidates are selected per target, downloaded
//! into the blob cache, inspected and verified with the client's own load rules (D5);
//! the first that verifies wins. Then ownership and the running policy judge the
//! folder operations, so every refusal surfaces before anything is staged. Downloads
//! are the only writes, and only under `<cache>`, so `--dry-run` returns this plan
//! as-is and warms the cache.
//!
//! A failure for one change on one target is recorded in `failures` and the rest still
//! plans; the command layer turns that into `partial` (or `--strict` exit 6).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::blobs::{BlobCache, BlobRef, Storage};
use super::edit::{
    EditField, RecordEdit, RecordGuard, apply_edit, apply_edits, changed_fields, sorted_names,
};
use super::inspect::{
    ArchiveLimits, FolderVerdict, Inspected, SkippedFolder, accepts, inspect, inspect_dir,
    partition, verify,
};
use super::ownership::{self, AllowedOp, FolderChange, Transfer};
use super::running::{self, PolicyContext, PolicyOp};
use super::swap::SwapOp;
use crate::error::{Error, ErrorBody, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{ScopeTable, TreeDigest, WriteSession, Writer, ensure_dir_all, failpoint};
use crate::inventory::{Depth, Inventory, inventory};
use crate::model::{
    Actor, AddonReason, AddonRecord, AddonState, AssetLocation, Candidate, Channel, ExpectedHash,
    FolderName, RankReason, Ref, ReleaseSet, ReloadRequired, Target, TargetKey, TxId, TxKind,
    VersionConfidence,
};
use crate::net::{Http, Request};
use crate::output::{Phase, ProgressSink};
use crate::resolve::cache::{Origin, ReleaseCache};
use crate::resolve::select::{EmptyReason, select};
use crate::rules::RulesTable;
use crate::sources::{SourceRegistry, Wanted};
use crate::state::config::RunningPolicy;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::memo::{CandidateFact, VerdictFact, merge_fact};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::{TargetRecord, load_record};
use crate::state::store;
use crate::toc::{DiskFolder, Verdict, loaded_toc};

/// The plan format's version.
pub const SCHEMA_VERSION: &str = "1.0";

/// What a command asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    pub targets: Vec<TargetRequest>,
    pub force: bool,
    pub while_running: bool,
    pub dry_run: bool,
    /// The transaction kind, whatever the changes are (`adopt`, `pin`, `sync`).
    pub kind: Option<TxKind>,
}

/// The changes for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRequest {
    pub target: Target,
    pub changes: Vec<RequestedChange>,
    /// What the command means when only records change (see [`target_kind`]).
    pub default_kind: TxKind,
}

/// One requested change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedChange {
    Install {
        r#ref: Ref,
        /// Resolved by the app: `--channel`, else the recorded channel, else
        /// `default_channel`, else stable.
        channel: Channel,
        /// `--channel` (or a desired-state entry) chose `channel`, not the record or a
        /// default.
        channel_explicit: bool,
        /// `--version`, or a release URL's tag.
        version: Option<String>,
        /// `--file <zip>`: this archive instead of the release's assets.
        file: Option<PathBuf>,
        /// The name a new addon is recorded under (the catalog's); `None` derives it
        /// from the archive's folders. An installed addon keeps its own.
        name: Option<String>,
        /// What happens to an installed addon's pin.
        pin: PinIntent,
        /// `Explicit` for a user's request (it promotes a dependency); `update` and
        /// `pin --version` pass the recorded reason.
        reason: AddonReason,
        /// Install the chosen release even when it is the installed one. A pinned addon
        /// is reinstalled at its pinned release.
        reinstall: bool,
    },
    Remove {
        addon: String,
    },
    /// The newest compatible release of an installed addon, keeping its pin, channel
    /// and reason: a pinned addon stays where it is (`addon_pinned`).
    Update {
        addon: String,
    },
    /// A change to the record alone (an adoption names its addon in its spec).
    Edit {
        addon: String,
        edit: RecordEdit,
    },
}

/// What a change does to an installed addon's pin. A pin intent never moves the
/// release by itself: only a version, or a newer release under `Keep`, does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PinIntent {
    /// The recorded pin stays; a new addon is unpinned.
    #[default]
    Keep,
    Pin,
    Unpin,
}

/// The intent of an install, kept on its work so that [`carry_over`] runs again under
/// the lock against the record as it is then: a pin committed between planning and the
/// lock survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallIntent {
    pub channel: Channel,
    pub channel_explicit: bool,
    pub version: Option<String>,
    pub pin: PinIntent,
    pub reason: AddonReason,
    pub reinstall: bool,
    /// Requesters the planner itself adds: the dependency resolver's requester on its
    /// own installs. Empty for user requests.
    pub required_by_add: BTreeSet<String>,
    pub required_by_remove: BTreeSet<String>,
}

impl InstallIntent {
    /// The intent of a user's request.
    fn requested(
        channel: Channel,
        channel_explicit: bool,
        version: Option<String>,
        pin: PinIntent,
        reason: AddonReason,
        reinstall: bool,
    ) -> Self {
        Self {
            channel,
            channel_explicit,
            version,
            pin,
            reason,
            reinstall,
            required_by_add: BTreeSet::new(),
            required_by_remove: BTreeSet::new(),
        }
    }

    /// The dependency resolver's own install: reason `dependency` with its requester. A
    /// user request never names a requester, so an `update` of a dependency, which
    /// passes the recorded reason, is still a user request.
    pub fn from_resolver(&self) -> bool {
        self.reason == AddonReason::Dependency && !self.required_by_add.is_empty()
    }

    /// Leaves the pinned `existing` where it is: no version moves it, and nothing asks
    /// for a reinstall.
    pub fn pinned_noop(&self, existing: &AddonRecord) -> bool {
        existing.pinned && self.pin == PinIntent::Keep && self.version.is_none() && !self.reinstall
    }

    /// Never selects another release for an installed addon: a pin intent without a
    /// version, or the resolver (an installed dependency already satisfies it).
    pub fn keeps_release(&self) -> bool {
        (self.pin != PinIntent::Keep && self.version.is_none()) || self.from_resolver()
    }
}

/// The record an install leaves (M2-D29, M2-D33): the release from `chosen` (ref,
/// version, `release_id`, blob, folders), the rest carried over from `existing` as the
/// intent says.
///
/// | Field | New addon | Installed, user request | Installed, resolver |
/// |---|---|---|---|
/// | `pinned` | `Pin` | `Pin` true, `Unpin` false, `Keep` as recorded | as recorded |
/// | `channel` | as passed | as passed | as recorded |
/// | `reason` | as passed | as passed | as recorded |
/// | `required_by` | additions | recorded ∪ additions − removals | recorded ∪ additions − removals |
/// | `name` | `chosen`'s | as recorded | as recorded |
///
/// `required_by` comes out sorted and de-duplicated, and `state` is `ok`.
pub fn carry_over(
    existing: Option<&AddonRecord>,
    intent: &InstallIntent,
    chosen: &AddonRecord,
) -> AddonRecord {
    let mut record = chosen.clone();
    record.state = AddonState::Ok;
    let recorded = existing.map(|e| e.required_by.clone()).unwrap_or_default();
    let removed: BTreeSet<String> = intent
        .required_by_remove
        .iter()
        .map(|n| TargetRecord::addon_key(n))
        .collect();
    record.required_by = sorted_names(recorded.into_iter().chain(intent.required_by_add.clone()))
        .into_iter()
        .filter(|n| !removed.contains(&TargetRecord::addon_key(n)))
        .collect();
    match existing {
        None => {
            record.pinned = intent.pin == PinIntent::Pin;
            record.channel = intent.channel;
            record.reason = intent.reason;
        }
        Some(existing) => {
            record.name.clone_from(&existing.name);
            if intent.from_resolver() {
                record.pinned = existing.pinned;
                record.channel = existing.channel;
                record.reason = existing.reason;
            } else {
                record.pinned = match intent.pin {
                    PinIntent::Keep => existing.pinned,
                    PinIntent::Pin => true,
                    PinIntent::Unpin => false,
                };
                record.channel = intent.channel;
                record.reason = intent.reason;
            }
        }
    }
    record
}

/// The fields among `pinned`, `channel` and `reason` that `carried` changes in
/// `existing`, sorted: what a request that chose the installed release would still
/// record. `required_by` is never carried by a request.
pub fn record_differences(existing: &AddonRecord, carried: &AddonRecord) -> Vec<EditField> {
    changed_fields(Some(existing), Some(carried))
        .into_iter()
        .filter(|f| {
            matches!(
                f,
                EditField::Pinned | EditField::Channel | EditField::Reason
            )
        })
        .collect()
}

/// The edits that record [`record_differences`]: one per differing field, in field
/// order.
pub fn record_edits(existing: &AddonRecord, carried: &AddonRecord) -> Vec<RecordEdit> {
    record_differences(existing, carried)
        .into_iter()
        .filter_map(|field| match field {
            EditField::Pinned => Some(RecordEdit::SetPinned(carried.pinned)),
            EditField::Channel => Some(RecordEdit::SetChannel(carried.channel)),
            EditField::Reason => Some(RecordEdit::SetReason(carried.reason)),
            EditField::Ref | EditField::RequiredBy => None,
        })
        .collect()
}

/// `addon_pinned`: a pinned addon was left where it is. Its fix shows the pin and the
/// newest release (`unpin` arrives with the pin commands).
pub fn addon_pinned(existing: &AddonRecord, target: &str) -> Warning {
    Warning::new(
        WarningCode::AddonPinned,
        format!(
            "{} is pinned at {} on {target}; nothing was installed. Pass --version to move it",
            existing.name, existing.version
        ),
    )
    .with_fix(Suggestion::command([
        "omaddons",
        "info",
        existing.name.as_str(),
        "-t",
        target,
    ]))
}

/// What planning reads.
pub struct PlanCtx<'a> {
    pub paths: &'a Paths,
    pub scopes: &'a ScopeTable,
    pub http: &'a dyn Http,
    pub registry: &'a SourceRegistry,
    /// Release metadata: every fetch goes through it, once per wish per command.
    pub cache: &'a ReleaseCache<'a>,
    /// Where fetch, download and verify progress goes.
    pub progress: &'a ProgressSink,
    pub blobs: &'a BlobCache,
    pub table: &'a RulesTable,
    /// The transaction whose in-flight lock the caller holds (temp downloads).
    pub tx: TxId,
    /// Client dirs whose game client runs now.
    pub running: &'a BTreeSet<PathBuf>,
    pub policy: RunningPolicy,
    pub limits: ArchiveLimits,
    pub actor: Actor,
    pub argv: &'a [String],
}

/// The plan: what `--dry-run` returns and the commit executes.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Plan {
    pub schema_version: &'static str,
    /// Stored plans arrive in M4.
    pub plan_id: Option<String>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "Option<String>")]
    pub expires_at: Option<Timestamp>,
    pub argv: Vec<String>,
    pub actor: Actor,
    pub dry_run: bool,
    /// Target key (or provisional id) → generation at planning time.
    pub generations: BTreeMap<String, u64>,
    pub targets: Vec<TargetPlan>,
    pub failures: Vec<PlanFailure>,
    /// What downloading candidates taught, one fact per release and asset, for the
    /// release memo (dry runs included: their downloads are real).
    #[serde(skip)]
    pub facts: Vec<CandidateFact>,
}

impl Plan {
    /// Whether any target has something to do.
    pub fn has_ops(&self) -> bool {
        self.targets
            .iter()
            .any(|t| !t.ops.is_empty() || !t.work.is_empty())
    }
}

/// One target's part of the plan.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetPlan {
    pub target: String,
    pub key: Option<TargetKey>,
    /// `None` when nothing changes.
    pub kind: Option<TxKind>,
    pub changes: Vec<Change>,
    pub ops: Vec<PlannedOp>,
    pub releases: Vec<PlannedRelease>,
    pub reload_required: ReloadRequired,
    pub warnings: Vec<Warning>,
    /// The client has no `Interface/AddOns` yet (it creates one on its first launch),
    /// so the transaction creates it.
    pub creates_addons_dir: bool,
    /// The record generation planned against.
    #[serde(skip)]
    pub generation: u64,
    /// What the commit stages, swaps and records.
    #[serde(skip)]
    pub work: Vec<AddonWork>,
    /// The warnings of addons planning left unchanged (`addon_pinned`), also in
    /// `warnings`: the commit re-plans only `work`, so it carries these over.
    #[serde(skip)]
    pub unchanged_warnings: Vec<Warning>,
    /// `-v` notes on addons left unchanged, such as record fields a request would
    /// change although its release is current.
    #[serde(skip)]
    pub notes: Vec<String>,
    /// The command's kind when only records change (see [`target_kind`]).
    #[serde(skip)]
    pub default_kind: TxKind,
}

/// One addon changing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Change {
    pub addon: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub kind: ChangeKind,
    /// The record fields an `edit` changes, sorted; omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EditField>,
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub folders: FolderSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Install,
    Update,
    Remove,
    /// Unmanaged folders brought under management, unchanged.
    Adopt,
    /// The record alone changed (`fields` names what).
    Edit,
}

/// Folders by what happens to them (live spellings for replaced and removed ones).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderSummary {
    pub added: Vec<FolderName>,
    pub replaced: Vec<FolderName>,
    pub removed: Vec<FolderName>,
    /// Folders of the archive this client does not load: never written or recorded.
    pub skipped: Vec<SkippedFolder>,
    /// Folders whose record changed without a swap (a pin, an adoption, a forgotten
    /// addon whose folders were gone).
    pub recorded: Vec<FolderName>,
}

/// One folder operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PlannedOp {
    pub op: OpKind,
    pub folder: FolderName,
    pub addon: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Add,
    Replace,
    Remove,
}

/// A release the plan installs from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PlannedRelease {
    pub release_id: String,
    pub url: Option<String>,
    pub expected_hash: Option<String>,
    pub blob_sha256: Option<String>,
}

/// A change that could not be planned.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanFailure {
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    pub addon: Option<String>,
    pub target: String,
    pub error: ErrorBody,
    /// The error itself, for exit codes.
    #[serde(skip)]
    pub cause: Option<Error>,
}

/// Everything the commit needs for one addon. A transaction has one work per addon: a
/// second change to the same addon is folded into the first ([`Planned::merge_into`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonWork {
    pub addon: String,
    pub mode: WorkMode,
}

/// What a work changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkMode {
    /// Folders staged and swapped: an install, update or remove.
    Folders(Box<FolderWork>),
    /// The record alone: nothing is staged or swapped, the running policy does not
    /// apply, and the client may run.
    Records(Box<RecordsWork>),
}

/// An install, update or remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderWork {
    /// The record after the change; `None` when the addon is removed.
    pub record: Option<AddonRecord>,
    /// The zip staging reads; `None` for directory inputs (their files name sources).
    pub blob: Option<PathBuf>,
    /// The folders that install: the archive's folders this client loads.
    pub inspected: Option<Inspected>,
    /// The archive's folders this client does not load.
    pub skipped: Vec<SkippedFolder>,
    pub swap: Vec<SwapOp>,
    pub transfers: Vec<Transfer>,
    /// Addon keys that become degraded.
    pub degraded: Vec<String>,
    /// The install's intent, which the commit applies again to the current record;
    /// `None` for removes.
    pub intent: Option<InstallIntent>,
    /// Records-only edits folded into this work, applied after [`carry_over`] at plan
    /// time and again under the lock.
    pub edits: Vec<RecordEdit>,
}

/// A change to the record alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordsWork {
    /// The record as planned against; `None` for an adoption.
    pub before: Option<AddonRecord>,
    /// The record after `edits`; `None` when it is forgotten.
    pub after: Option<AddonRecord>,
    /// Applied again under the lock to the record as it is then, so a concurrent change
    /// to another field survives.
    pub edits: Vec<RecordEdit>,
    /// Files index entries the edits add (an adoption's folders).
    pub index_upserts: Vec<(FolderName, FolderIndex)>,
    /// Files index entries the edits drop (a forgotten addon's folders).
    pub index_removals: Vec<FolderName>,
    pub guard: RecordGuard,
}

impl AddonWork {
    /// The record after the change; `None` when the addon goes.
    pub fn record(&self) -> Option<&AddonRecord> {
        match &self.mode {
            WorkMode::Folders(f) => f.record.as_ref(),
            WorkMode::Records(r) => r.after.as_ref(),
        }
    }

    /// The archive's folders to stage, for an install or update.
    pub fn inspected(&self) -> Option<&Inspected> {
        match &self.mode {
            WorkMode::Folders(f) => f.inspected.as_ref(),
            WorkMode::Records(_) => None,
        }
    }

    /// The zip staging reads.
    pub fn blob(&self) -> Option<&std::path::Path> {
        match &self.mode {
            WorkMode::Folders(f) => f.blob.as_deref(),
            WorkMode::Records(_) => None,
        }
    }

    /// The folder renames.
    pub fn swap(&self) -> &[SwapOp] {
        match &self.mode {
            WorkMode::Folders(f) => &f.swap,
            WorkMode::Records(_) => &[],
        }
    }

    /// Folders taken from other addons (`--force`).
    pub fn transfers(&self) -> &[Transfer] {
        match &self.mode {
            WorkMode::Folders(f) => &f.transfers,
            WorkMode::Records(_) => &[],
        }
    }

    /// Addon keys that become degraded.
    pub fn degraded(&self) -> &[String] {
        match &self.mode {
            WorkMode::Folders(f) => &f.degraded,
            WorkMode::Records(_) => &[],
        }
    }
}

/// A change after its ref is known, before fetching.
enum Want {
    Install {
        /// The installed addon's name, for an update.
        name: Option<String>,
        wanted: Wanted,
        file: Option<PathBuf>,
        intent: InstallIntent,
    },
    Remove {
        addon: String,
    },
    Edit {
        addon: String,
        edit: RecordEdit,
    },
    Failed {
        r#ref: Option<Ref>,
        addon: Option<String>,
        error: Error,
    },
}

/// Plans `req`. Fails as a whole only for errors that apply to every change.
pub fn plan(req: &PlanRequest, ctx: &PlanCtx<'_>) -> Result<Plan> {
    let created_at = Timestamp::now();
    let mut generations = BTreeMap::new();
    let mut prepared = Vec::with_capacity(req.targets.len());
    for tr in &req.targets {
        let (record, index) = target_state(ctx.paths, &tr.target)?;
        let generation_key = tr
            .target
            .key
            .map_or_else(|| tr.target.id.clone(), |k| k.to_string());
        generations.insert(generation_key, record.generation);
        let wants: Vec<Want> = tr.changes.iter().map(|c| want(c, &record)).collect();
        prepared.push((tr, record, index, wants));
    }

    let to_fetch: Vec<Wanted> = prepared
        .iter()
        .flat_map(|(_, record, _, wants)| wants.iter().map(move |w| (record, w)))
        .filter_map(|(record, w)| match w {
            Want::Install {
                wanted,
                file: None,
                intent,
                ..
            } if !keeps_installed(record, &wanted.r#ref, intent) => Some(wanted.clone()),
            _ => None,
        })
        .collect();
    let fetched = ctx.cache.get(&to_fetch);
    let from_sources: Vec<&Wanted> = fetched
        .iter()
        .filter(|(_, f)| f.origin == Origin::Fetched)
        .map(|(w, _)| w)
        .collect();
    for w in &from_sources {
        let total = from_sources.len() as u64;
        ctx.progress
            .event(Phase::Fetch, total, &w.r#ref.to_string());
    }
    failpoint::hit("engine.plan.fetched").map_err(|e| fail(ctx, &e))?;
    // Each install wish is verified, and downloaded, at most once per target.
    let candidate_items = prepared
        .iter()
        .flat_map(|(_, record, _, wants)| wants.iter().map(move |w| (record, w)))
        .filter(|(record, w)| match w {
            Want::Install { wanted, intent, .. } => !keeps_installed(record, &wanted.r#ref, intent),
            _ => false,
        })
        .count() as u64;

    let mut targets = Vec::with_capacity(prepared.len());
    let mut failures = Vec::new();
    let mut facts = Vec::new();
    for (tr, record, index, wants) in prepared {
        let target = &tr.target;
        let mut tp = TargetPlan {
            target: target.id.clone(),
            key: target.key,
            kind: None,
            changes: Vec::new(),
            ops: Vec::new(),
            releases: Vec::new(),
            reload_required: ReloadRequired::None,
            warnings: Vec::new(),
            creates_addons_dir: false,
            generation: record.generation,
            work: Vec::new(),
            unchanged_warnings: Vec::new(),
            notes: Vec::new(),
            default_kind: tr.default_kind,
        };
        let live = match inventory(target, Some(&record), ctx.table, Depth::Classes) {
            Ok(live) => live,
            Err(error) => {
                // Every change on the target fails with it, each under its own ref and
                // addon, so the command reports each item failed rather than unchanged.
                for w in wants {
                    let (r, addon, error) = match w {
                        Want::Failed {
                            r#ref,
                            addon,
                            error,
                        } => (r#ref, addon, error),
                        Want::Install { name, wanted, .. } => {
                            (Some(wanted.r#ref), name, error.clone())
                        }
                        Want::Remove { addon } | Want::Edit { addon, .. } => {
                            (None, Some(addon), error.clone())
                        }
                    };
                    failures.push(failure(r, addon, target, error));
                }
                targets.push(tp);
                continue;
            }
        };
        let planner = TargetPlanner {
            req,
            ctx,
            target,
            record: &record,
            index: index.as_ref(),
            live: &live,
            created_at,
            facts: RefCell::new(Vec::new()),
            candidate_items,
        };
        for w in wants {
            let (r, addon, result) = match w {
                Want::Failed {
                    r#ref,
                    addon,
                    error,
                } => (r#ref, addon, Err(error)),
                Want::Install {
                    name,
                    wanted,
                    file,
                    intent,
                } => {
                    // A pinned addon, or one whose release is kept, was not fetched.
                    let set = match &file {
                        Some(_) => None,
                        None => fetched.get(&wanted).map(|f| f.set.clone()),
                    };
                    let result =
                        planner.install(name.as_deref(), &wanted, file.as_deref(), set, &intent);
                    (Some(wanted.r#ref.clone()), name, result)
                }
                Want::Remove { addon } => {
                    let result = planner.remove(&addon).map(Planned::change);
                    (None, Some(addon), result)
                }
                Want::Edit { addon, edit } => {
                    let result = planner.edit(&addon, &edit, &tp.work);
                    (None, Some(addon), result)
                }
            };
            match result.and_then(|planned| planned.merge_into(&mut tp, &target.id)) {
                Ok(()) => {}
                Err(error) => failures.push(failure(r, addon, target, error)),
            }
        }
        tp.kind = target_kind(&tp.changes, req.kind, tp.default_kind);
        tp.creates_addons_dir =
            live.addons_dir_missing && tp.work.iter().any(|w| w.inspected().is_some());
        targets.push(tp);
        for fact in planner.facts.into_inner() {
            merge_fact(&mut facts, fact);
        }
    }
    failpoint::hit("engine.plan.done").map_err(|e| fail(ctx, &e))?;
    Ok(Plan {
        schema_version: SCHEMA_VERSION,
        plan_id: None,
        created_at,
        expires_at: None,
        argv: ctx.argv.to_vec(),
        actor: ctx.actor,
        dry_run: req.dry_run,
        generations,
        targets,
        failures,
        facts,
    })
}

/// A target's record and files index; an unregistered target has an empty record.
fn target_state(paths: &Paths, target: &Target) -> Result<(TargetRecord, Option<FilesIndex>)> {
    Ok(match target.key {
        Some(key) => {
            let record = load_record(paths, &key)?.value;
            let index = store::load::<FilesIndex>(&paths.files_index(&key))?.map(|l| l.value);
            (record, index)
        }
        None => (
            TargetRecord::empty(TargetKey::from_ulid(ulid::Ulid::nil())),
            None,
        ),
    })
}

/// Re-checks one planned addon under the lock against the target's current record,
/// files index, inventory and running state (`ctx.running`), without network: the
/// archive is already inspected and staged. The install's intent and folded edits are
/// applied again to the current record ([`carry_over`]). [`Planned::Unchanged`] when the
/// change is no longer needed: a concurrent transaction installed the same release,
/// removed the addon, or pinned it (with `addon_pinned`, never `plan_stale`). A refusal
/// is the caller's `plan_stale`.
///
/// A records-only work re-checks its [`RecordGuard`]. When it fails, an adoption is
/// `plan_stale` whatever the generation; another edit is the caller's `plan_stale` when
/// the record's generation moved since `generation` (planning's), and is dropped as no
/// longer needed when it did not (a forgotten addon's folder put back by hand).
pub fn rejudge(
    req: &PlanRequest,
    ctx: &PlanCtx<'_>,
    target: &Target,
    work: &AddonWork,
    generation: u64,
) -> Result<Planned> {
    let (record, index) = target_state(ctx.paths, target)?;
    let live = inventory(target, Some(&record), ctx.table, Depth::Classes)?;
    let planner = TargetPlanner {
        req,
        ctx,
        target,
        record: &record,
        index: index.as_ref(),
        live: &live,
        created_at: Timestamp::now(),
        // Re-judging downloads nothing, so it learns and reports nothing.
        facts: RefCell::new(Vec::new()),
        candidate_items: 0,
    };
    let folders = match &work.mode {
        WorkMode::Folders(folders) => folders,
        WorkMode::Records(records) => {
            // ubs:ignore — comparing record generations, not secrets
            let changed = record.generation != generation;
            return planner.rejudge_records(&work.addon, records, changed);
        }
    };
    match (&folders.record, &folders.inspected) {
        (Some(template), Some(inspected)) => {
            let Some(intent) = &folders.intent else {
                return Err(Error::internal(format!(
                    "{} was planned without its intent",
                    work.addon
                )));
            };
            let existing = record.addons.values().find(|a| a.r#ref == template.r#ref);
            if let Some(existing) = existing {
                if intent.pinned_noop(existing) {
                    return Ok(Planned::pinned(existing, &target.id));
                }
                // Installed meanwhile: its pin, channel or reason difference still
                // records, as a records-only change.
                if intent.keeps_release()
                    || (existing.release_id == template.release_id && planner.all_present(existing))
                {
                    return planner.current(existing, intent);
                }
            }
            planner
                .build_install(
                    existing,
                    template.clone(),
                    intent,
                    &folders.edits,
                    Loadable {
                        inspected: inspected.clone(),
                        skipped: folders.skipped.clone(),
                    },
                    folders.blob.clone(),
                    None,
                    Vec::new(),
                )
                .map(Planned::change)
        }
        _ if !record
            .addons
            .contains_key(&TargetRecord::addon_key(&work.addon)) =>
        {
            Ok(Planned::nothing())
        }
        _ => planner.remove(&work.addon).map(Planned::change),
    }
}

/// A requested change with its ref resolved against the target's record.
fn want(change: &RequestedChange, record: &TargetRecord) -> Want {
    match change {
        RequestedChange::Install {
            r#ref,
            channel,
            channel_explicit,
            version,
            file,
            name,
            pin,
            reason,
            reinstall,
        } => Want::Install {
            name: name.clone(),
            wanted: Wanted {
                r#ref: r#ref.clone(),
                channel: *channel,
                version: version.clone(),
            },
            file: file.clone(),
            intent: InstallIntent::requested(
                *channel,
                *channel_explicit,
                version.clone(),
                *pin,
                *reason,
                *reinstall,
            ),
        },
        RequestedChange::Remove { addon } => Want::Remove {
            addon: addon.clone(),
        },
        RequestedChange::Edit { addon, edit } => Want::Edit {
            addon: addon.clone(),
            edit: edit.clone(),
        },
        RequestedChange::Update { addon } => {
            match record.addons.get(&TargetRecord::addon_key(addon)) {
                // No version: a pinned addon is the pinned no-op.
                Some(installed) => Want::Install {
                    name: Some(installed.name.clone()),
                    wanted: Wanted {
                        r#ref: installed.r#ref.clone(),
                        channel: installed.channel,
                        version: None,
                    },
                    file: None,
                    intent: InstallIntent::requested(
                        installed.channel,
                        false,
                        None,
                        PinIntent::Keep,
                        installed.reason,
                        false,
                    ),
                },
                None => Want::Failed {
                    r#ref: None,
                    addon: Some(addon.clone()),
                    error: not_managed(addon),
                },
            }
        }
    }
}

/// Whether `intent` leaves the installed addon of `r#ref` where it is (pinned, or its
/// release kept), so its releases need no fetching.
fn keeps_installed(record: &TargetRecord, r#ref: &Ref, intent: &InstallIntent) -> bool {
    record
        .addons
        .values()
        .find(|a| a.r#ref == *r#ref)
        .is_some_and(|e| intent.pinned_noop(e) || intent.keeps_release())
}

fn not_managed(addon: &str) -> Error {
    Error::new(
        ErrorCode::NotManaged,
        format!("{addon} is not installed by omaddons on this target"),
        Suggestion::command(["omaddons", "list"]),
    )
}

fn failure(
    r#ref: Option<Ref>,
    addon: Option<String>,
    target: &Target,
    error: Error,
) -> PlanFailure {
    PlanFailure {
        r#ref,
        addon,
        target: target.id.clone(),
        error: ErrorBody::from(error.clone()),
        cause: Some(error),
    }
}

fn fail(ctx: &PlanCtx<'_>, e: &std::io::Error) -> Error {
    Error::io(
        ctx.paths.state_dir(),
        e,
        Suggestion::command(["omaddons", "status"]),
    )
}

/// The transaction kind for a target's changes, in order:
/// 1. no changes, no transaction;
/// 2. the command's override (`adopt`, `pin`, `sync`) wins;
/// 3. edits that change only `pinned` are a `pin`;
/// 4. edits and adoptions alone are what the command means (`default_kind`);
/// 5. otherwise edits and adoptions are ignored, and the folder changes decide: all
///    removes are a `remove` (a remove plus a `RemoveRequiredBy` on another addon
///    too), any update an `update`, else an `install`.
pub fn target_kind(
    changes: &[Change],
    kind: Option<TxKind>,
    default_kind: TxKind,
) -> Option<TxKind> {
    let records_only = |c: &&Change| matches!(c.kind, ChangeKind::Edit | ChangeKind::Adopt);
    let folders: Vec<&Change> = changes.iter().filter(|c| !records_only(c)).collect();
    if changes.is_empty() {
        None
    } else if kind.is_some() {
        kind
    } else if changes
        .iter()
        .all(|c| c.kind == ChangeKind::Edit && c.fields == [EditField::Pinned])
    {
        Some(TxKind::Pin)
    } else if folders.is_empty() {
        Some(default_kind)
    } else if folders.iter().all(|c| c.kind == ChangeKind::Remove) {
        Some(TxKind::Remove)
    } else if folders.iter().any(|c| c.kind == ChangeKind::Update) {
        Some(TxKind::Update)
    } else {
        Some(TxKind::Install)
    }
}

/// One addon's planned part.
#[derive(Debug, Clone)]
pub struct PlannedAddon {
    pub change: Change,
    pub ops: Vec<PlannedOp>,
    pub release: Option<PlannedRelease>,
    pub reload_required: ReloadRequired,
    pub warnings: Vec<Warning>,
    pub work: AddonWork,
}

/// One addon's result, from planning or from [`rejudge`].
#[derive(Debug, Clone)]
pub enum Planned {
    Change(Box<PlannedAddon>),
    /// Nothing to commit: the release is current (`notes` names record fields the
    /// request would still change, for `-v`, until records-only work records them),
    /// the addon is gone, or it is pinned (`addon_pinned` in `warnings`). Not a refusal.
    Unchanged {
        warnings: Vec<Warning>,
        notes: Vec<String>,
    },
}

impl Planned {
    fn change(planned: PlannedAddon) -> Self {
        Self::Change(Box::new(planned))
    }

    /// Unchanged, with nothing to say.
    pub fn nothing() -> Self {
        Self::Unchanged {
            warnings: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// The pinned no-op on `target`.
    fn pinned(existing: &AddonRecord, target: &str) -> Self {
        Self::Unchanged {
            warnings: vec![addon_pinned(existing, target)],
            notes: Vec::new(),
        }
    }

    /// Adds this addon's result to its target's plan, folding it into the work the addon
    /// already has there, so a transaction keeps one work per addon:
    /// - edits join a folder work, applied after its carry-over (moot on a remove);
    /// - a folder work absorbs the edits planned before it and replaces their work;
    /// - edits join earlier edits, under both guards.
    ///
    /// Folding is order-independent. Two folder works for one addon stay two (M1).
    fn merge_into(self, tp: &mut TargetPlan, target: &str) -> Result<()> {
        let planned = match self {
            Self::Unchanged { warnings, notes } => {
                push_new(&mut tp.warnings, warnings.clone());
                push_new(&mut tp.unchanged_warnings, warnings);
                tp.notes.extend(notes);
                return Ok(());
            }
            Self::Change(planned) => *planned,
        };
        let key = TargetRecord::addon_key(&planned.work.addon);
        let at = tp
            .work
            .iter()
            .position(|w| TargetRecord::addon_key(&w.addon) == key);
        let Some(at) = at else {
            planned.push(tp);
            return Ok(());
        };
        // `changes` and `work` are pushed together, so they share indices.
        let addon = tp.work[at].addon.clone();
        match (&mut tp.work[at].mode, planned.work.mode) {
            (WorkMode::Folders(_), mode @ WorkMode::Folders(_)) => {
                PlannedAddon {
                    work: AddonWork {
                        addon: planned.work.addon,
                        mode,
                    },
                    ..planned
                }
                .push(tp);
            }
            (WorkMode::Folders(folders), WorkMode::Records(records)) => {
                folders.fold(records.edits, &addon, target)?;
            }
            (WorkMode::Records(records), WorkMode::Folders(mut folders)) => {
                folders.fold(records.edits.clone(), &addon, target)?;
                tp.work.remove(at);
                tp.changes.remove(at);
                PlannedAddon {
                    work: AddonWork {
                        addon: planned.work.addon,
                        mode: WorkMode::Folders(folders),
                    },
                    ..planned
                }
                .push(tp);
            }
            (WorkMode::Records(earlier), WorkMode::Records(later)) => {
                let after = apply_edits(earlier.after.clone(), &later.edits, &addon, target)?;
                let mut edits = earlier.edits.clone();
                edits.extend(later.edits);
                let guard = earlier.guard.clone().and(later.guard);
                let before = earlier.before.clone();
                tp.work.remove(at);
                tp.changes.remove(at);
                if let Some(merged) = records_change(&addon, before, after, edits, guard) {
                    merged.push(tp);
                }
            }
        }
        Ok(())
    }
}

impl PlannedAddon {
    fn push(self, tp: &mut TargetPlan) {
        tp.changes.push(self.change);
        tp.ops.extend(self.ops);
        tp.releases.extend(self.release);
        tp.reload_required = tp.reload_required.max(self.reload_required);
        push_new(&mut tp.warnings, self.warnings);
        tp.work.push(self.work);
    }
}

impl FolderWork {
    /// Folds records-only `edits` into this work: applied to its record after the
    /// carry-over, and again under the lock. A remove makes them moot.
    fn fold(&mut self, edits: Vec<RecordEdit>, addon: &str, target: &str) -> Result<()> {
        if self.record.is_none() {
            return Ok(());
        }
        let after = apply_edits(self.record.clone(), &edits, addon, target)?;
        if after.is_none() {
            return Err(Error::new(
                ErrorCode::Usage,
                format!("{addon} cannot be forgotten by the command that installs it"),
                Suggestion::command(["omaddons", "list", "-t", target]),
            ));
        }
        self.record = after;
        self.edits.extend(edits);
        Ok(())
    }
}

/// A records-only change of `addon` from `before` to `after` through `edits`, or `None`
/// when nothing changes. Its change is an `adopt` when a record appears, a `remove`
/// when one goes (its index entries go too), else an `edit` naming its `fields`; every
/// folder of either record is `recorded`.
pub fn records_change(
    addon: &str,
    before: Option<AddonRecord>,
    after: Option<AddonRecord>,
    edits: Vec<RecordEdit>,
    guard: RecordGuard,
) -> Option<PlannedAddon> {
    if before == after {
        return None;
    }
    let shown = after.as_ref().or(before.as_ref())?;
    let kind = match (&before, &after) {
        (None, _) => ChangeKind::Adopt,
        (_, None) => ChangeKind::Remove,
        _ => ChangeKind::Edit,
    };
    let mut recorded: Vec<FolderName> = Vec::new();
    for folder in before.iter().chain(after.iter()).flat_map(|r| &r.folders) {
        if !recorded.contains(folder) {
            recorded.push(folder.clone());
        }
    }
    recorded.sort_by(|a, b| a.key().cmp(b.key()));
    let index_upserts = edits
        .iter()
        .filter_map(|e| match e {
            RecordEdit::Adopt(spec) => Some(spec.folders.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let index_removals = match (&before, &after) {
        (Some(before), None) => before.folders.clone(),
        _ => Vec::new(),
    };
    let change = Change {
        addon: shown.name.clone(),
        r#ref: shown.r#ref.clone(),
        kind,
        fields: changed_fields(before.as_ref(), after.as_ref()),
        from_version: before.as_ref().map(|r| r.version.clone()),
        to_version: after.as_ref().map(|r| r.version.clone()),
        folders: FolderSummary {
            recorded,
            ..FolderSummary::default()
        },
    };
    Some(PlannedAddon {
        change,
        ops: Vec::new(),
        release: None,
        reload_required: ReloadRequired::None,
        warnings: Vec::new(),
        work: AddonWork {
            addon: addon.to_owned(),
            mode: WorkMode::Records(Box::new(RecordsWork {
                before,
                after,
                edits,
                index_upserts,
                index_removals,
                guard,
            })),
        },
    })
}

/// Appends the warnings `list` does not have yet.
pub fn push_new(list: &mut Vec<Warning>, warnings: Vec<Warning>) {
    for warning in warnings {
        if !list.contains(&warning) {
            list.push(warning);
        }
    }
}

/// Planning for one target.
struct TargetPlanner<'a> {
    req: &'a PlanRequest,
    ctx: &'a PlanCtx<'a>,
    target: &'a Target,
    record: &'a TargetRecord,
    index: Option<&'a FilesIndex>,
    live: &'a Inventory,
    created_at: Timestamp,
    /// What downloading candidates for this target taught.
    facts: RefCell<Vec<CandidateFact>>,
    /// Install wishes across the plan that try candidates: the download and verify
    /// progress total.
    candidate_items: u64,
}

/// What ownership and the running policy allow.
struct Judged {
    allowed: Vec<AllowedOp>,
    transfers: Vec<Transfer>,
    degraded: Vec<String>,
    reload_required: ReloadRequired,
    warnings: Vec<Warning>,
}

/// A candidate that verified.
struct Winner {
    candidate: Candidate,
    loadable: Loadable,
    /// The name a new addon takes from the archive.
    name: String,
    blob: Option<PathBuf>,
    warnings: Vec<Warning>,
}

/// An inspected archive cut down to the folders the target's client loads, and the
/// folders left out.
struct Loadable {
    inspected: Inspected,
    skipped: Vec<SkippedFolder>,
}

impl TargetPlanner<'_> {
    fn install(
        &self,
        name: Option<&str>,
        wanted: &Wanted,
        file: Option<&std::path::Path>,
        set: Option<Result<ReleaseSet>>,
        intent: &InstallIntent,
    ) -> Result<Planned> {
        one_addons_dir(&ClientPaths::new(&self.target.client_dir))?;
        let existing = self
            .record
            .addons
            .values()
            .find(|a| a.r#ref == wanted.r#ref);
        // Before any candidate is looked at: these never move an installed release.
        if let Some(existing) = existing {
            if intent.pinned_noop(existing) {
                return Ok(Planned::pinned(existing, &self.target.id));
            }
            if intent.keeps_release() {
                return self.current(existing, intent);
            }
        }
        let candidates = match (file, set) {
            (Some(file), _) => vec![file_candidate(file, wanted)],
            (None, Some(set)) => {
                let set = set?;
                // A git source already resolved `--version` as the rev it fetched.
                let pin = match wanted.r#ref {
                    Ref::Git { .. } => None,
                    _ => wanted.version.as_deref(),
                };
                let selection = select(&set, self.target, wanted.channel, pin);
                match selection.empty_reason {
                    Some(EmptyReason::PinnedVersionNotFound) => {
                        return Err(Error::new(
                            ErrorCode::NotFound,
                            format!(
                                "{} has no release {}",
                                wanted.r#ref,
                                wanted.version.as_deref().unwrap_or_default()
                            ),
                            Suggestion::command([
                                "omaddons".to_owned(),
                                "install".to_owned(),
                                wanted.r#ref.to_string(),
                            ]),
                        ));
                    }
                    Some(reason) => return Err(self.no_release(wanted, &[], reason_text(reason))),
                    None => selection.candidates,
                }
            }
            (None, None) => {
                return Err(Error::internal(format!("{} was not fetched", wanted.r#ref)));
            }
        };
        if let (Some(existing), Some(first)) = (existing, candidates.first())
            && existing.release_id.as_deref() == Some(first.release_id.as_str())
            && self.all_present(existing)
        {
            return self.current(existing, intent);
        }

        let user_supplied =
            file.is_some() || matches!(wanted.r#ref, Ref::File { .. } | Ref::Url { .. });
        let mut tried: Vec<Value> = Vec::new();
        let mut winner = None;
        let mut unreadable: Option<Error> = None;
        for candidate in candidates {
            match self.try_candidate(&wanted.r#ref, &candidate) {
                Ok(Ok(won)) => {
                    winner = Some(won);
                    break;
                }
                Ok(Err(reason)) => {
                    tried.push(json!({ "asset": candidate.asset.filename, "reason": reason }));
                }
                Err(error) => {
                    let reason = format!("{}: {}", error.code.wire(), error.message);
                    tried.push(json!({ "asset": candidate.asset.filename, "reason": reason }));
                    unreadable.get_or_insert(error);
                }
            }
        }
        let Some(winner) = winner else {
            if user_supplied {
                // The user's own archive: why it cannot be read, else it does not load.
                if let Some(error) = unreadable {
                    return Err(error);
                }
                return Err(Error::new(
                    ErrorCode::WrongFlavor,
                    format!("{} does not load on {} ({:?})", wanted.r#ref, self.target.id, self.target.family),
                    Suggestion::text("use an archive built for this client"),
                )
                .details(json!({ "ref": wanted.r#ref.to_string(), "target": self.target.id, "tried": tried })));
            }
            // A release that does not exist at all (a git rev naming nothing), or one
            // whose archive cannot be downloaded offline, is not a question of flavor.
            if let Some(error) =
                unreadable.filter(|e| matches!(e.code, ErrorCode::NotFound | ErrorCode::Offline))
            {
                return Err(error);
            }
            return Err(self.no_release(wanted, &tried, "no candidate loads on this client"));
        };

        // An installed addon keeps its name: it is the record's identity. A hint that
        // names another ref's addon is ignored (a folder-derived name that clashes
        // shares a folder too, which ownership refuses).
        let taken = |n: &str| {
            self.record
                .addons
                .get(&TargetRecord::addon_key(n))
                .is_some_and(|a| a.r#ref != wanted.r#ref)
        };
        let addon = existing
            .map(|e| e.name.clone())
            .or_else(|| name.filter(|n| !taken(n)).map(str::to_owned))
            .unwrap_or_else(|| winner.name.clone());
        // The release; `carry_over` decides the rest of the record.
        let template = AddonRecord {
            r#ref: wanted.r#ref.clone(),
            name: addon,
            version: winner.candidate.version.clone(),
            version_confidence: VersionConfidence::Release,
            release_id: Some(winner.candidate.release_id.clone()),
            channel: intent.channel,
            pinned: false,
            reason: intent.reason,
            required_by: Vec::new(),
            folders: Vec::new(),
            state: AddonState::Ok,
            installed_at: self.created_at,
            tx_id: self.ctx.tx,
            // Content-addressed: a cached blob's file name is its SHA-256.
            blob_sha256: winner
                .blob
                .as_ref()
                .filter(|b| b.starts_with(self.ctx.paths.blobs_dir()))
                .and_then(|b| b.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
        };
        let (url, expected_hash) = match &winner.candidate.asset.location {
            AssetLocation::Url(url) => (
                Some(url.clone()),
                winner
                    .candidate
                    .asset
                    .expected_hash
                    .as_ref()
                    .map(|ExpectedHash::Sha256(h)| h.clone()),
            ),
            _ => (None, None),
        };
        let release = PlannedRelease {
            release_id: winner.candidate.release_id.clone(),
            url,
            expected_hash,
            blob_sha256: winner.loadable.inspected.blob_sha256.clone(),
        };
        self.build_install(
            existing,
            template,
            intent,
            &[],
            winner.loadable,
            winner.blob,
            Some(release),
            winner.warnings,
        )
        .map(Planned::change)
    }

    /// `folder` has an entry in AddOns.
    fn present(&self, folder: &FolderName) -> bool {
        self.live.folders.iter().any(|e| e.key == folder.key())
    }

    /// Every folder `existing` records is in AddOns.
    fn all_present(&self, existing: &AddonRecord) -> bool {
        existing.folders.iter().all(|f| self.present(f))
    }

    /// A records-only edit of `addon` (an adoption's addon is its spec's), against the
    /// record as the works already planned on this target leave it, else as recorded.
    /// An edit that changes nothing plans nothing.
    fn edit(&self, addon: &str, edit: &RecordEdit, planned: &[AddonWork]) -> Result<Planned> {
        let target = &self.target.id;
        let name = match edit {
            RecordEdit::Adopt(spec) => spec.record.name.as_str(),
            _ => addon,
        };
        let key = TargetRecord::addon_key(name);
        let base = match planned
            .iter()
            .find(|w| TargetRecord::addon_key(&w.addon) == key)
        {
            Some(work) => work.record().cloned(),
            None => self.record.addons.get(&key).cloned(),
        };
        if let (RecordEdit::Forget, Some(recorded)) = (edit, &base)
            && let Some(here) = recorded.folders.iter().find(|f| self.present(f))
        {
            return Err(Error::new(
                ErrorCode::Usage,
                format!(
                    "{} is still in AddOns, so {} cannot be forgotten; remove it instead",
                    here.as_str(),
                    recorded.name
                ),
                Suggestion::command(["omaddons", "remove", recorded.name.as_str(), "-t", target]),
            ));
        }
        let guard = edit.guard(base.as_ref());
        if guard.adopt {
            guard
                .check(
                    name,
                    target,
                    base.as_ref(),
                    self.live,
                    &self.target.addons_dir(),
                    std::slice::from_ref(edit),
                )
                .map_err(|f| f.cause)?;
        }
        let after = apply_edit(base.clone(), edit, name, target)?;
        Ok(records_change(name, base, after, vec![edit.clone()], guard)
            .map_or_else(Planned::nothing, Planned::change))
    }

    /// Re-checks a records-only work under the lock (see [`rejudge`]), then applies its
    /// edits to the record as it is now.
    fn rejudge_records(&self, addon: &str, work: &RecordsWork, changed: bool) -> Result<Planned> {
        let target = &self.target.id;
        let current = self.record.addons.get(&TargetRecord::addon_key(addon));
        let checked = work.guard.check(
            addon,
            target,
            current,
            self.live,
            &self.target.addons_dir(),
            &work.edits,
        );
        match checked {
            Err(failure) if failure.adopt => Err(super::tx::stale(&failure.cause)),
            Err(failure) if changed => Err(failure.cause),
            Err(_) => Ok(Planned::nothing()),
            Ok(()) => {
                let after = apply_edits(current.cloned(), &work.edits, addon, target)?;
                let before = current.cloned();
                let guard = work.guard.clone();
                Ok(
                    records_change(addon, before, after, work.edits.clone(), guard)
                        .map_or_else(Planned::nothing, Planned::change),
                )
            }
        }
    }

    /// An installed addon that keeps its release. A request that would still change its
    /// `pinned`, `channel` or `reason` records that: a records-only work with one edit
    /// per field under the same-release guard (`install X --version <installed>` pins,
    /// an install promotes a dependency). With a folder missing it is unchanged, with a
    /// `-v` note: repairing it is a reinstall, not an edit.
    fn current(&self, existing: &AddonRecord, intent: &InstallIntent) -> Result<Planned> {
        let (target, name) = (&self.target.id, &existing.name);
        if !self.all_present(existing) {
            return Ok(Planned::Unchanged {
                warnings: Vec::new(),
                notes: vec![format!(
                    "{target}: {name} unchanged: this request keeps its release, and some of its folders are missing"
                )],
            });
        }
        let edits = record_edits(existing, &carry_over(Some(existing), intent, existing));
        let guard = edits.iter().fold(RecordGuard::default(), |guard, edit| {
            guard.and(edit.guard(Some(existing)))
        });
        let after = apply_edits(Some(existing.clone()), &edits, name, target)?;
        Ok(
            records_change(name, Some(existing.clone()), after, edits, guard)
                .map_or_else(Planned::nothing, Planned::change),
        )
    }

    /// Folder changes, ownership, policy and swap ops for installing the loadable
    /// folders as the release `template`, whose record fields [`carry_over`] decides
    /// from `existing` and `intent` and whose folders are filled in here. A folder an
    /// earlier release installed that this one no longer has, or that this client no
    /// longer loads, is removed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the plan-time path and rejudge share it with different sources for each part"
    )]
    fn build_install(
        &self,
        existing: Option<&AddonRecord>,
        template: AddonRecord,
        intent: &InstallIntent,
        edits: &[RecordEdit],
        loadable: Loadable,
        blob: Option<PathBuf>,
        release: Option<PlannedRelease>,
        mut warnings: Vec<Warning>,
    ) -> Result<PlannedAddon> {
        // Against whatever `existing` holds now: the snapshot at plan time, the current
        // record under the lock. Folded edits apply after the carry-over.
        let carried = carry_over(existing, intent, &template);
        let name = carried.name.clone();
        let Some(mut template) = apply_edits(Some(carried), edits, &name, &self.target.id)? else {
            return Err(Error::internal(format!(
                "a folded edit dropped the record of {name}"
            )));
        };
        let Loadable { inspected, skipped } = loadable;
        let new_folders: Vec<FolderName> =
            inspected.folders.iter().map(|f| f.name.clone()).collect();
        let dropped: Vec<FolderName> = existing
            .map(|e| {
                e.folders
                    .iter()
                    // FolderName equality is the case-folded key.
                    .filter(|old| !new_folders.contains(old))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let changes: Vec<FolderChange> = new_folders
            .iter()
            .cloned()
            .map(FolderChange::Install)
            .chain(dropped.iter().cloned().map(FolderChange::Remove))
            .collect();
        let toc_after = |name: &FolderName| -> BTreeSet<String> {
            inspected
                .folders
                .iter()
                .find(|f| f.name == *name)
                .map(|f| f.toc_files().into_iter().collect())
                .unwrap_or_default()
        };
        let judged = self.judge(&template.name, &changes, toc_after)?;
        warnings.extend(judged.warnings);
        let mut summary = FolderSummary {
            skipped: skipped.clone(),
            ..FolderSummary::default()
        };
        let ops = self.ops(&template.name, &judged.allowed, &mut summary);
        let kind = if existing.is_some() {
            ChangeKind::Update
        } else {
            ChangeKind::Install
        };
        template.folders = new_folders;
        if template.version.is_empty() {
            // file: and url: releases carry no version: the TOC's ## Version is it.
            template.version = toc_version(&inspected, &template.name, &self.target.rules)
                .unwrap_or_else(|| "unknown".to_owned());
            template.version_confidence = VersionConfidence::Toc;
        }
        Ok(PlannedAddon {
            change: Change {
                addon: template.name.clone(),
                r#ref: template.r#ref.clone(),
                kind,
                fields: Vec::new(),
                from_version: existing.map(|e| e.version.clone()),
                to_version: Some(template.version.clone()),
                folders: summary,
            },
            ops,
            release,
            reload_required: judged.reload_required,
            warnings,
            work: AddonWork {
                addon: template.name.clone(),
                mode: WorkMode::Folders(Box::new(FolderWork {
                    record: Some(template),
                    blob,
                    swap: self.swap_ops(&judged.allowed),
                    inspected: Some(inspected),
                    skipped,
                    transfers: judged.transfers,
                    degraded: judged.degraded,
                    intent: Some(intent.clone()),
                    edits: edits.to_vec(),
                })),
            },
        })
    }

    /// Removes an installed addon's folders. When every one of them is already gone
    /// from AddOns, only its record and index entries go: a records-only work, which
    /// the running policy does not judge.
    fn remove(&self, addon: &str) -> Result<PlannedAddon> {
        let Some(installed) = self.record.addons.get(&TargetRecord::addon_key(addon)) else {
            return Err(not_managed(addon));
        };
        if installed.folders.iter().all(|f| !self.present(f)) {
            let forget = RecordEdit::Forget;
            let guard = forget.guard(Some(installed));
            return records_change(
                &installed.name,
                Some(installed.clone()),
                None,
                vec![forget],
                guard,
            )
            .ok_or_else(|| Error::internal("forgetting a record changed nothing"));
        }
        let changes: Vec<FolderChange> = installed
            .folders
            .iter()
            .cloned()
            .map(FolderChange::Remove)
            .collect();
        let judged = self.judge(&installed.name, &changes, |_| BTreeSet::new())?;
        let mut summary = FolderSummary::default();
        let ops = self.ops(&installed.name, &judged.allowed, &mut summary);
        Ok(PlannedAddon {
            change: Change {
                addon: installed.name.clone(),
                r#ref: installed.r#ref.clone(),
                kind: ChangeKind::Remove,
                fields: Vec::new(),
                from_version: Some(installed.version.clone()),
                to_version: None,
                folders: summary,
            },
            ops,
            release: None,
            reload_required: judged.reload_required,
            warnings: judged.warnings,
            work: AddonWork {
                addon: installed.name.clone(),
                mode: WorkMode::Folders(Box::new(FolderWork {
                    record: None,
                    blob: None,
                    inspected: None,
                    skipped: Vec::new(),
                    swap: self.swap_ops(&judged.allowed),
                    transfers: judged.transfers,
                    degraded: judged.degraded,
                    intent: None,
                    edits: Vec::new(),
                })),
            },
        })
    }

    /// Ownership, then the running policy.
    fn judge(
        &self,
        addon: &str,
        changes: &[FolderChange],
        toc_after: impl Fn(&FolderName) -> BTreeSet<String>,
    ) -> Result<Judged> {
        let ctx = self.ctx;
        let owned = ownership::check(changes, self.live, addon, self.req.force, ctx.argv)?;
        let addons_dir = self.target.addons_dir();
        let loaded = |live: &FolderName| {
            DiskFolder::new(addons_dir.join(live.as_str()))
                .ok()
                .and_then(|view| loaded_toc(&view, &self.target.rules).ok().flatten())
                .map(|l| l.file)
        };
        let policy_ops: Vec<PolicyOp> = owned
            .allowed
            .iter()
            .map(|op| match op {
                AllowedOp::Add { name } => PolicyOp::AddFolder {
                    folder: name.clone(),
                },
                AllowedOp::Replace { name, live, .. } => PolicyOp::ReplaceFolder {
                    folder: live.clone(),
                    loaded_toc_before: loaded(live),
                    toc_names_after: toc_after(name),
                },
                AllowedOp::Remove { live } => PolicyOp::RemoveFolder {
                    folder: live.clone(),
                    loaded_toc_before: loaded(live),
                },
            })
            .collect();
        let running = ctx.running.contains(&self.target.client_dir);
        let policy_ctx = PolicyContext {
            target_id: &self.target.id,
            argv: ctx.argv,
        };
        let policy = running::evaluate(
            &policy_ctx,
            ctx.policy,
            running,
            &self.target.rules,
            &policy_ops,
            self.req.while_running,
        )?;
        let mut warnings = owned.warnings;
        warnings.extend(policy.warnings);
        Ok(Judged {
            allowed: owned.allowed,
            transfers: owned.transfers,
            degraded: owned.degraded,
            reload_required: policy.reload_required,
            warnings,
        })
    }

    fn ops(
        &self,
        addon: &str,
        allowed: &[AllowedOp],
        summary: &mut FolderSummary,
    ) -> Vec<PlannedOp> {
        allowed
            .iter()
            .map(|op| {
                let (kind, folder) = match op {
                    AllowedOp::Add { name } => {
                        summary.added.push(name.clone());
                        (OpKind::Add, name.clone())
                    }
                    AllowedOp::Replace { live, .. } => {
                        summary.replaced.push(live.clone());
                        (OpKind::Replace, live.clone())
                    }
                    AllowedOp::Remove { live } => {
                        summary.removed.push(live.clone());
                        (OpKind::Remove, live.clone())
                    }
                };
                PlannedOp {
                    op: kind,
                    folder,
                    addon: addon.to_owned(),
                }
            })
            .collect()
    }

    /// The swap's view of the allowed ops, with old digests from the files index.
    fn swap_ops(&self, allowed: &[AllowedOp]) -> Vec<SwapOp> {
        let old = |live: &FolderName| -> Option<TreeDigest> {
            self.index?
                .folders
                .get(live.key())
                .map(crate::state::files_index::FolderIndex::digest)
        };
        allowed
            .iter()
            .map(|op| match op {
                AllowedOp::Add { name } => SwapOp::Add { name: name.clone() },
                AllowedOp::Replace { name, live, .. } => SwapOp::Replace {
                    name: name.clone(),
                    live: live.clone(),
                    old_tree: old(live),
                },
                AllowedOp::Remove { live } => SwapOp::Remove {
                    live: live.clone(),
                    old_tree: old(live),
                },
            })
            .collect()
    }

    /// Downloads, inspects and verifies one candidate of `r#ref`: `Ok(Err(reason))` when
    /// it does not load on this client, `Err` when it cannot even be read (download,
    /// archive). A winner keeps only the folders this client loads (see [`accepts`]).
    fn try_candidate(
        &self,
        r#ref: &Ref,
        candidate: &Candidate,
    ) -> Result<std::result::Result<Winner, String>> {
        let ctx = self.ctx;
        let mut downloaded = None;
        let (inspected, blob) = match &candidate.asset.location {
            AssetLocation::Url(url) => {
                let blob = self
                    .download(&candidate.release_id, &candidate.asset, url)
                    .map_err(|e| missing_git_rev(e, candidate, url))?;
                let inspected = inspect(&blob.path, &ctx.limits, ctx.table)?;
                let path = blob.path.clone();
                downloaded = Some(blob);
                (inspected, Some(path))
            }
            AssetLocation::LocalZip(path) => {
                (inspect(path, &ctx.limits, ctx.table)?, Some(path.clone()))
            }
            AssetLocation::LocalDir(path) => (inspect_dir(path, &ctx.limits, ctx.table)?, None),
            AssetLocation::GitCheckout { url, rev, sha } => {
                let dir = self.checkout(&candidate.asset.filename, url, rev, sha)?;
                (inspect_dir(&dir, &ctx.limits, ctx.table)?, None)
            }
        };
        let verdicts = verify(&inspected, self.target, ctx.table)?;
        let item = format!("{} on {}", r#ref, self.target.id);
        ctx.progress
            .event(Phase::Verify, self.candidate_items, &item);
        let split = partition(&verdicts);
        let all: Vec<FolderName> = inspected.folders.iter().map(|f| f.name.clone()).collect();
        let primary = primary_folder(r#ref, &all);
        // The publisher's release.json names this client for this very file.
        let declared = matches!(
            candidate.rank,
            RankReason::MetadataInterface | RankReason::MetadataFlavor
        );
        let loads = primary.is_some_and(|p| accepts(&split, declared, p));
        if let Some(blob) = &downloaded {
            let verdict = verdicts.iter().find(|v| Some(&v.folder) == primary);
            self.learn(candidate, blob, loads, verdict);
        }
        if !loads {
            let why: Vec<String> = verdicts
                .iter()
                .map(|v| {
                    // ubs:ignore — comparing folder names, not secrets
                    let verdict = match split.skip.iter().find(|s| s.name == v.folder) {
                        Some(skipped) => serde_json::to_value(Verdict::WrongFlavor {
                            reason: skipped.reason.clone(),
                        }),
                        None => serde_json::to_value(&v.report.verdict),
                    };
                    format!(
                        "{}: {}",
                        v.folder.as_str(),
                        verdict.map(|j| j.to_string()).unwrap_or_default()
                    )
                })
                .collect();
            return Ok(Err(format!("wrong_flavor: {}", why.join("; "))));
        }
        let mut inspected = inspected;
        inspected
            .folders
            .retain(|f| split.install.contains(&f.name));
        // Named as on every client, unless that folder is one this client skips.
        let name = primary
            .filter(|p| split.install.contains(p))
            .map_or_else(|| addon_name(r#ref, &inspected), |p| p.as_str().to_owned());
        let warnings = verdicts
            .into_iter()
            .filter(|v| split.install.contains(&v.folder))
            .flat_map(|v| v.report.warnings)
            .collect();
        Ok(Ok(Winner {
            candidate: candidate.clone(),
            loadable: Loadable {
                inspected,
                skipped: split.skip,
            },
            name,
            blob,
            warnings,
        }))
    }

    /// A git checkout of `sha` in a fresh directory under the transaction's temp dir,
    /// created here through fsx and removed with it after the transaction. The directory
    /// is named like the asset (`<repo>-<sha>`) so a repository-root addon is named by
    /// its TOC.
    fn checkout(&self, name: &str, url: &str, rev: &str, sha: &str) -> Result<PathBuf> {
        let ctx = self.ctx;
        let Some(git) = ctx.registry.git() else {
            return Err(Error::new(
                ErrorCode::SourceDisabled,
                format!("{url} needs a git checkout, which this command does not run"),
                Suggestion::command(["omaddons", "help", "install"]),
            ));
        };
        let base = ctx.paths.cache_tmp(&ctx.tx).join("git");
        let dir = (0u32..)
            .map(|n| base.join(n.to_string()).join(name))
            .find(|d| !d.exists())
            .unwrap_or_else(|| base.join(name));
        let mut s = WriteSession::untracked(Writer::Derived, ctx.scopes, "git");
        ensure_dir_all(&mut s, &dir)?;
        git.materialize(url, rev, sha, &dir)?;
        Ok(dir)
    }

    /// Remembers what downloading `candidate` (into `blob`) taught: its hash, and whether
    /// it `loads` on this client, judged by its primary folder's `verdict`.
    fn learn(
        &self,
        candidate: &Candidate,
        blob: &BlobRef,
        loads: bool,
        verdict: Option<&FolderVerdict>,
    ) {
        let out_of_date = verdict
            .is_some_and(|v| matches!(v.report.verdict, Verdict::Compatible { out_of_date: true }));
        let fact = CandidateFact {
            release_id: candidate.release_id.clone(),
            asset: candidate.asset.filename.clone(),
            sha256: blob.sha256.clone(),
            size: blob.size,
            verdicts: vec![VerdictFact {
                rules: self.target.rules.name.clone(),
                interface: self.target.interface,
                loads,
                out_of_date: loads && out_of_date,
                primary_interface: verdict.and_then(|v| v.interface),
                at: Timestamp::now(),
            }],
        };
        merge_fact(&mut self.facts.borrow_mut(), fact);
    }

    /// The blob of `asset` of `release_id`, at `url`, in this order: the cached blob of
    /// its expected hash; the cached blob of the hash an earlier download remembered,
    /// unless the source now declares another hash or size (a fact never stands in for
    /// the blob: eviction makes facts stale); else a download, retried once on a hash
    /// mismatch.
    fn download(
        &self,
        release_id: &str,
        asset: &crate::model::Asset,
        url: &str,
    ) -> Result<BlobRef> {
        let ctx = self.ctx;
        let expected = asset.expected_hash.as_ref();
        let expected_sha = expected.map(|ExpectedHash::Sha256(sha)| sha.to_ascii_lowercase());
        if let Some(sha) = &expected_sha
            && let Some(blob) = ctx.blobs.get(ctx.scopes, sha)?
        {
            return Ok(blob);
        }
        if let Some((sha, size)) = ctx.cache.facts().hash(release_id, &asset.filename)
            && expected_sha.as_ref().is_none_or(|e| e == sha)
            && asset.size.is_none_or(|declared| declared == size)
            && let Some(blob) = ctx.blobs.get(ctx.scopes, sha)?
        {
            return Ok(blob);
        }
        let mut attempt = 1;
        loop {
            let resp = ctx
                .http
                .send(Request::get(url, "asset").download())?
                .error_for_status("asset")?;
            match ctx.blobs.store_stream(
                ctx.scopes,
                &ctx.tx,
                resp.into_reader(),
                expected,
                &Storage::Cache,
            ) {
                Ok(blob) => {
                    failpoint::hit("engine.plan.downloaded").map_err(|e| fail(ctx, &e))?;
                    let item = format!("{release_id} on {}", self.target.id);
                    ctx.progress
                        .event(Phase::Download, self.candidate_items, &item);
                    return Ok(blob);
                }
                Err(e) if e.code == ErrorCode::HashMismatch && attempt == 1 => attempt += 1,
                Err(e) if e.code == ErrorCode::HashMismatch => return Err(e.retryable(false)),
                Err(e) => return Err(e),
            }
        }
    }

    fn no_release(&self, wanted: &Wanted, tried: &[Value], why: &str) -> Error {
        Error::new(
            ErrorCode::NoCompatibleRelease,
            format!(
                "{}: {why} ({} on {})",
                wanted.r#ref,
                self.target.id,
                family_name(self.target)
            ),
            Suggestion::text(
                "check that the addon supports this client, or pin another release with --version",
            ),
        )
        .details(
            json!({ "ref": wanted.r#ref.to_string(), "target": self.target.id, "tried": tried }),
        )
    }
}

/// Refuses a client with two spellings of `Interface` or of `Interface/AddOns` that
/// differ only in case: Wine resolves names ignoring case, so the game reads one of
/// them and an addon installed into the other may never load.
pub(super) fn one_addons_dir(client: &ClientPaths) -> Result<()> {
    let variants = client.addons_case_variants();
    if variants.len() < 2 {
        return Ok(());
    }
    let listed: Vec<String> = variants.iter().map(|p| p.display().to_string()).collect();
    Err(Error::new(
        ErrorCode::CaseCollision,
        format!(
            "{} differ only in case; the game reads only one of them, so omaddons installs into neither",
            listed.join(" and ")
        ),
        Suggestion::text(
            "move everything into one Interface/AddOns folder and delete the other spelling, then retry",
        ),
    )
    .details(json!({ "paths": listed })))
}

fn reason_text(reason: EmptyReason) -> &'static str {
    match reason {
        EmptyReason::NoEligibleRelease => "no release on this channel",
        EmptyReason::NoAssets => "the release has no installable archive",
        EmptyReason::PinnedVersionNotFound => "no such release",
    }
}

/// A git ref fetched as a tag by name (its commits feed did not serve the rev) whose
/// codeload archive is a 404: the rev names nothing there, or the repository is private.
/// Any other download error is returned as it is.
fn missing_git_rev(err: Error, candidate: &Candidate, url: &str) -> Error {
    let by_name = url
        .strip_prefix("https://codeload.github.com/")
        .and_then(|rest| rest.split_once("/zip/refs/tags/"));
    let Some((_, tag)) = by_name else {
        return err;
    };
    if err.code != ErrorCode::HttpStatus
        || err.details["status"] != 404
        || !candidate.release_id.starts_with("git:")
    {
        return err;
    }
    let repo = candidate
        .release_id
        .trim_start_matches("git:")
        .rsplit_once('@')
        .map_or("", |(repo, _)| repo);
    let rev = crate::sources::github::percent_unescape(tag);
    Error::new(
        ErrorCode::NotFound,
        format!(
            "{repo} has no branch, tag or commit named {rev}, or it is private (private repositories are not supported)"
        ),
        Suggestion::text("check the name after #, or give a commit sha"),
    )
    .details(json!({ "repository": repo, "rev": rev }))
}

fn family_name(target: &Target) -> String {
    serde_json::to_value(target.family)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `--file <zip>`: the archive itself, as the only candidate of the ref.
fn file_candidate(file: &std::path::Path, wanted: &Wanted) -> Candidate {
    let location = if file.is_dir() {
        AssetLocation::LocalDir(file.to_path_buf())
    } else {
        AssetLocation::LocalZip(file.to_path_buf())
    };
    // Without --version the TOC's ## Version fills it in once the archive is inspected.
    let version = wanted.version.clone().unwrap_or_default();
    Candidate {
        // A zip is identified by its content, so the same file is a no-op and another
        // one an update; a directory has no cheap identity and always reinstalls.
        release_id: format!("{}@{}", wanted.r#ref, local_identity(file)),
        version,
        channel: wanted.channel,
        asset: crate::model::Asset {
            filename: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            location,
            size: None,
            expected_hash: None,
            flavors: Vec::new(),
            name_flavors: Vec::new(),
            nolib: false,
        },
        rank: crate::model::RankReason::Unannotated,
    }
}

/// The `## Version` of the addon's own folder (named like it, else the first), as the
/// client would load its TOC.
fn toc_version(
    inspected: &Inspected,
    addon: &str,
    rules: &crate::rules::ClientRules,
) -> Option<String> {
    let folder = inspected
        .folders
        .iter()
        .find(|f| f.name.as_str().eq_ignore_ascii_case(addon))
        .or_else(|| inspected.folders.first())?;
    loaded_toc(&folder.view(), rules)
        .ok()
        .flatten()
        .and_then(|loaded| loaded.toc.version)
        .filter(|v| !v.trim().is_empty())
}

/// `sha256:<hex>` of a local zip; a fresh `dir:<ulid>` for a directory or an
/// unreadable file.
fn local_identity(file: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let hashed = std::fs::read(file)
        .ok()
        .filter(|_| file.is_file())
        .map(|bytes| {
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
    match hashed {
        Some(hex) => format!("sha256:{hex}"),
        None => format!("dir:{}", ulid::Ulid::generate()),
    }
}

/// A new addon's name: its [`primary_folder`], else the ref.
pub fn addon_name(r#ref: &Ref, inspected: &Inspected) -> String {
    let folders: Vec<FolderName> = inspected.folders.iter().map(|f| f.name.clone()).collect();
    primary_folder(r#ref, &folders).map_or_else(|| r#ref.to_string(), |f| f.as_str().to_owned())
}

/// The folder an archive is the addon of: the folder named like the repository, else
/// the folder the others extend (`WeakAuras` for `WeakAurasOptions`), else the one core
/// folder (`DBM-Core` beside `DBM-GUI`), else the shortest.
pub fn primary_folder<'a>(r#ref: &Ref, folders: &'a [FolderName]) -> Option<&'a FolderName> {
    if let Ref::GitHub { repo, .. } = r#ref
        && let Some(same) = folders
            .iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(repo))
    {
        return Some(same);
    }
    let extended = |n: &FolderName| {
        let n = n.as_str();
        folders
            .iter()
            .map(FolderName::as_str)
            .filter(|o| {
                o.len() > n.len() && o.to_ascii_lowercase().starts_with(&n.to_ascii_lowercase())
            })
            .count()
    };
    let mut ranked: Vec<&FolderName> = folders.iter().collect();
    ranked.sort_by(|a, b| {
        extended(b)
            .cmp(&extended(a))
            .then(a.as_str().len().cmp(&b.as_str().len()))
            .then(a.as_str().cmp(b.as_str()))
    });
    let core = |n: &&FolderName| {
        let n = n.as_str().to_ascii_lowercase();
        n.ends_with("-core") || n.ends_with("_core")
    };
    if ranked.first().is_some_and(|top| extended(top) == 0)
        && let [only] = folders.iter().filter(core).collect::<Vec<_>>().as_slice()
    {
        return Some(only);
    }
    ranked.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspected(names: &[&str]) -> Inspected {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Repo-main");
        for name in names {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join(format!("{name}.toc")),
                "## Interface: 120105\n",
            )
            .unwrap();
        }
        inspect_dir(&root, &ArchiveLimits::default(), &RulesTable::builtin()).unwrap()
    }

    #[test]
    fn addon_names() {
        let wa = inspected(&[
            "WeakAurasOptions",
            "WeakAuras",
            "WeakAurasArchive",
            "WeakAurasTemplates",
        ]);
        assert_eq!(
            addon_name(&"gh:WeakAuras/WeakAuras2".parse().unwrap(), &wa),
            "WeakAuras"
        );
        let bt = inspected(&["Bartender4"]);
        assert_eq!(
            addon_name(&"gh:Nevcairiel/Bartender4".parse().unwrap(), &bt),
            "Bartender4"
        );
        let dbm = inspected(&["DBM-StatusBarTimers", "DBM-Core", "DBM-GUI"]);
        assert_eq!(
            addon_name(&"gh:DeadlyBossMods/DeadlyBossMods".parse().unwrap(), &dbm),
            "DBM-Core",
            "no folder extends another: the one core folder"
        );
        let bw = inspected(&["BigWigs_Core", "BigWigs", "BigWigs_Options"]);
        assert_eq!(
            addon_name(&"gh:o/bw".parse().unwrap(), &bw),
            "BigWigs",
            "the extended folder beats the core folder"
        );
        let plain = inspected(&["Zeta", "Alpha"]);
        assert_eq!(
            addon_name(&"gh:o/pack".parse().unwrap(), &plain),
            "Zeta",
            "no core folder: the shortest, then by name"
        );
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    /// An installed record of gh:o/foo at `release`.
    fn installed(
        release: &str,
        pinned: bool,
        channel: Channel,
        reason: AddonReason,
    ) -> AddonRecord {
        AddonRecord {
            r#ref: "gh:o/foo".parse().unwrap(),
            name: "Foo".into(),
            version: release.into(),
            version_confidence: VersionConfidence::Release,
            release_id: Some(format!("gh:o/foo@{release}")),
            channel,
            pinned,
            reason,
            required_by: vec!["Zed".into(), "Bar".into()],
            folders: vec![FolderName::new("Foo").unwrap()],
            state: AddonState::Degraded,
            installed_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            tx_id: TxId::new(),
            blob_sha256: Some("aa".into()),
        }
    }

    /// The release a request chose: what the planner's template holds.
    fn chosen() -> AddonRecord {
        let mut chosen = installed("2.0", false, Channel::Stable, AddonReason::Explicit);
        chosen.name = "FooFromArchive".into();
        chosen.required_by = Vec::new();
        chosen.state = AddonState::Ok;
        chosen.blob_sha256 = Some("bb".into());
        chosen
    }

    fn user(pin: PinIntent, channel: Channel, reason: AddonReason) -> InstallIntent {
        InstallIntent::requested(channel, false, None, pin, reason, false)
    }

    fn resolver(requester: &str) -> InstallIntent {
        let mut intent = user(PinIntent::Keep, Channel::Stable, AddonReason::Dependency);
        intent.required_by_add = names(&[requester]);
        intent
    }

    #[test]
    fn carry_over_new_addons_take_the_intent() {
        let chosen = chosen();
        for (pin, pinned) in [
            (PinIntent::Keep, false),
            (PinIntent::Pin, true),
            (PinIntent::Unpin, false),
        ] {
            let r = carry_over(
                None,
                &user(pin, Channel::Beta, AddonReason::Explicit),
                &chosen,
            );
            assert_eq!(r.pinned, pinned, "{pin:?}");
            assert_eq!(
                (r.channel, r.reason),
                (Channel::Beta, AddonReason::Explicit)
            );
            assert_eq!(
                r.name, "FooFromArchive",
                "a new addon takes the chosen name"
            );
            assert!(r.required_by.is_empty());
        }
        let r = carry_over(None, &resolver("Bar"), &chosen);
        assert_eq!(
            (r.pinned, r.channel, r.reason, r.required_by.clone()),
            (
                false,
                Channel::Stable,
                AddonReason::Dependency,
                vec!["Bar".to_owned()]
            ),
            "a new dependency is required by its requester"
        );
        // The release always comes from the chosen one.
        assert_eq!(
            (
                r.version.as_str(),
                r.release_id.as_deref(),
                r.blob_sha256.as_deref(),
                r.state
            ),
            ("2.0", Some("gh:o/foo@2.0"), Some("bb"), AddonState::Ok)
        );
    }

    #[test]
    fn carry_over_user_requests_on_installed_addons() {
        let chosen = chosen();
        for existing_pinned in [false, true] {
            let existing = installed(
                "1.0",
                existing_pinned,
                Channel::Alpha,
                AddonReason::Dependency,
            );
            for (pin, pinned) in [
                (PinIntent::Keep, existing_pinned),
                (PinIntent::Pin, true),
                (PinIntent::Unpin, false),
            ] {
                let intent = user(pin, Channel::Beta, AddonReason::Explicit);
                let r = carry_over(Some(&existing), &intent, &chosen);
                assert_eq!(r.pinned, pinned, "{existing_pinned} {pin:?}");
                assert_eq!(r.channel, Channel::Beta, "as passed");
                assert_eq!(r.reason, AddonReason::Explicit, "install promotes");
                assert_eq!(r.name, "Foo", "the record keeps its name");
                assert_eq!(r.required_by, ["Bar", "Zed"], "sorted");
                assert_eq!(
                    (r.version.as_str(), r.blob_sha256.as_deref(), r.state),
                    ("2.0", Some("bb"), AddonState::Ok)
                );
            }
        }
        // An update passes the recorded reason, which stays.
        let existing = installed("1.0", false, Channel::Alpha, AddonReason::Dependency);
        let update = user(PinIntent::Keep, Channel::Alpha, AddonReason::Dependency);
        assert!(!update.from_resolver(), "an update is a user request");
        let r = carry_over(Some(&existing), &update, &chosen);
        assert_eq!(
            (r.reason, r.channel),
            (AddonReason::Dependency, Channel::Alpha)
        );
    }

    #[test]
    fn carry_over_the_resolver_keeps_the_record() {
        let existing = installed("1.0", true, Channel::Beta, AddonReason::Explicit);
        let r = carry_over(Some(&existing), &resolver("Bar"), &chosen());
        assert_eq!(
            (r.pinned, r.channel, r.reason, r.name.as_str()),
            (true, Channel::Beta, AddonReason::Explicit, "Foo"),
            "every field as recorded"
        );
        assert_eq!(
            r.required_by,
            ["Bar", "Zed"],
            "already listed: no duplicate"
        );
        let r = carry_over(Some(&existing), &resolver("Alpha"), &chosen());
        assert_eq!(r.required_by, ["Alpha", "Bar", "Zed"]);
    }

    #[test]
    fn carry_over_required_by_adds_removes_and_deduplicates() {
        let mut existing = installed("1.0", false, Channel::Stable, AddonReason::Dependency);
        existing.required_by = vec!["Zed".into(), "Bar".into(), "Zed".into()];
        let mut intent = user(PinIntent::Keep, Channel::Stable, AddonReason::Dependency);
        let r = carry_over(Some(&existing), &intent, &chosen());
        assert_eq!(
            r.required_by,
            ["Bar", "Zed"],
            "survives an update, sorted and unique"
        );
        intent.required_by_add = names(&["Mid", "Bar"]);
        intent.required_by_remove = names(&["Zed", "Nobody"]);
        let r = carry_over(Some(&existing), &intent, &chosen());
        assert_eq!(
            r.required_by,
            ["Bar", "Mid"],
            "existing ∪ additions − removals"
        );
    }

    #[test]
    fn pin_intents_alone_keep_the_release_and_reinstall_skips_the_pinned_no_op() {
        let pinned = installed("1.0", true, Channel::Stable, AddonReason::Explicit);
        let unpinned = installed("1.0", false, Channel::Stable, AddonReason::Explicit);
        let keep = user(PinIntent::Keep, Channel::Stable, AddonReason::Explicit);
        assert!(keep.pinned_noop(&pinned) && !keep.pinned_noop(&unpinned));
        assert!(!keep.keeps_release(), "Keep looks for a newer release");

        let mut versioned = keep.clone();
        versioned.version = Some("2.0".into());
        assert!(
            !versioned.pinned_noop(&pinned),
            "a version moves a pinned addon"
        );
        let mut reinstall = keep.clone();
        reinstall.reinstall = true;
        assert!(
            !reinstall.pinned_noop(&pinned),
            "a reinstall repairs a pinned addon"
        );

        for pin in [PinIntent::Pin, PinIntent::Unpin] {
            let mut intent = user(pin, Channel::Stable, AddonReason::Explicit);
            assert!(
                intent.keeps_release(),
                "{pin:?} alone never moves the release"
            );
            assert!(!intent.pinned_noop(&pinned));
            intent.version = Some("2.0".into());
            assert!(!intent.keeps_release(), "{pin:?} with a version does");
        }
        assert!(
            resolver("Bar").keeps_release(),
            "an installed dependency satisfies"
        );
    }

    #[test]
    fn record_differences_name_each_changed_field() {
        let existing = installed("1.0", false, Channel::Stable, AddonReason::Dependency);
        let carried = |pin, channel, reason| {
            carry_over(Some(&existing), &user(pin, channel, reason), &existing)
        };
        let same = carried(PinIntent::Keep, Channel::Stable, AddonReason::Dependency);
        assert!(record_differences(&existing, &same).is_empty());
        for (record, field) in [
            (
                carried(PinIntent::Pin, Channel::Stable, AddonReason::Dependency),
                EditField::Pinned,
            ),
            (
                carried(PinIntent::Keep, Channel::Beta, AddonReason::Dependency),
                EditField::Channel,
            ),
            (
                carried(PinIntent::Keep, Channel::Stable, AddonReason::Explicit),
                EditField::Reason,
            ),
        ] {
            assert_eq!(record_differences(&existing, &record), [field]);
        }
        let all = carried(PinIntent::Pin, Channel::Alpha, AddonReason::Explicit);
        assert_eq!(
            record_differences(&existing, &all),
            [EditField::Channel, EditField::Pinned, EditField::Reason],
            "sorted by wire name"
        );
        assert_eq!(
            serde_json::to_value(EditField::RequiredBy).unwrap(),
            EditField::RequiredBy.wire()
        );
    }

    #[test]
    fn record_edits_cover_every_combination_of_differing_fields() {
        let existing = installed("1.0", false, Channel::Stable, AddonReason::Dependency);
        for pinned in [false, true] {
            for channel in [Channel::Stable, Channel::Beta] {
                for reason in [AddonReason::Dependency, AddonReason::Explicit] {
                    let mut carried = existing.clone();
                    (carried.pinned, carried.channel, carried.reason) = (pinned, channel, reason);
                    let mut edits = Vec::new();
                    let mut fields = Vec::new();
                    if channel != existing.channel {
                        edits.push(RecordEdit::SetChannel(channel));
                        fields.push(EditField::Channel);
                    }
                    if pinned != existing.pinned {
                        edits.push(RecordEdit::SetPinned(pinned));
                        fields.push(EditField::Pinned);
                    }
                    if reason != existing.reason {
                        edits.push(RecordEdit::SetReason(reason));
                        fields.push(EditField::Reason);
                    }
                    let case = format!("{pinned} {channel:?} {reason:?}");
                    assert_eq!(record_edits(&existing, &carried), edits, "{case}");
                    assert_eq!(record_differences(&existing, &carried), fields, "{case}");
                    let after = apply_edits(Some(existing.clone()), &edits, "Foo", "retail")
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        (after.pinned, after.channel, after.reason),
                        (pinned, channel, reason),
                        "{case}"
                    );
                }
            }
        }
        let mut other = existing.clone();
        other.required_by = vec!["Bar".into()];
        other.r#ref = "gh:o/other".parse().unwrap();
        assert!(
            record_edits(&existing, &other).is_empty(),
            "only pinned, channel and reason count"
        );
    }

    #[test]
    fn the_pinned_warning_names_the_release_and_a_runnable_fix() {
        let pinned = installed("1.0", true, Channel::Stable, AddonReason::Explicit);
        let warning = addon_pinned(&pinned, "retail");
        assert_eq!(warning.code, WarningCode::AddonPinned);
        assert_eq!(
            warning.message,
            "Foo is pinned at 1.0 on retail; nothing was installed. Pass --version to move it"
        );
        assert_eq!(
            warning.fix.as_ref().and_then(|f| f.argv()),
            Some(&["omaddons", "info", "Foo", "-t", "retail"].map(String::from)[..])
        );
    }

    /// Planning `changes` on `scene`'s target, as the commit's re-judging sees it.
    fn with_plan<R>(
        scene: &crate::engine::testing::TxScene,
        changes: Vec<RequestedChange>,
        f: impl FnOnce(&PlanRequest, &PlanCtx<'_>, &Plan) -> R,
    ) -> R {
        let target = scene.current_target().target;
        let scopes =
            ScopeTable::new(&scene.paths, std::slice::from_ref(&target.client_dir), None).unwrap();
        let blobs = BlobCache::new(&scene.paths);
        let registry = SourceRegistry::new();
        let table = RulesTable::builtin();
        let argv = ["omaddons".to_owned(), "install".to_owned()];
        let cache = ReleaseCache::transient(&crate::engine::testing::NoNetwork, &registry);
        let ctx = PlanCtx {
            paths: &scene.paths,
            scopes: &scopes,
            http: &crate::engine::testing::NoNetwork,
            registry: &registry,
            cache: &cache,
            progress: &ProgressSink::none(),
            blobs: &blobs,
            table: &table,
            tx: TxId::new(),
            running: &scene.running,
            policy: RunningPolicy::Safe,
            limits: ArchiveLimits::default(),
            actor: Actor::Agent,
            argv: &argv,
        };
        let req = PlanRequest {
            targets: vec![TargetRequest {
                target,
                changes,
                default_kind: TxKind::Install,
            }],
            force: false,
            while_running: false,
            dry_run: false,
            kind: None,
        };
        let plan = plan(&req, &ctx).unwrap();
        f(&req, &ctx, &plan)
    }

    #[test]
    fn a_pin_committed_after_planning_drops_the_update_under_the_lock() {
        let scene = crate::engine::testing::TxScene::new();
        let foo = |n: &str| {
            [(
                "Foo/Foo.toc",
                format!("## Interface: 120105\n## Version: {n}\n"),
            )]
        };
        let zip = |n: &str| {
            let entries = foo(n);
            let refs: Vec<(&str, &str)> = entries.iter().map(|(p, t)| (*p, t.as_str())).collect();
            scene.zip(&format!("v{n}.zip"), &refs)
        };
        let (v1, v2, v3) = (zip("1"), zip("2"), zip("3"));
        let done = scene.run(vec![scene.install("gh:o/foo", &v1)]).unwrap();
        assert!(done.failures.is_empty(), "{:?}", done.failures);

        with_plan(
            &scene,
            vec![scene.install("gh:o/foo", &v2)],
            |req, ctx, plan| {
                let work = &plan.targets[0].work[0];
                let intent = match &work.mode {
                    WorkMode::Folders(f) => f.intent.as_ref().map(|i| i.pin),
                    WorkMode::Records(_) => None,
                };
                assert_eq!(intent, Some(PinIntent::Keep));
                // Meanwhile: pinned at 3.
                let pinned = scene
                    .run(vec![scene.install_pinned("gh:o/foo", &v3, "3")])
                    .unwrap();
                assert!(pinned.failures.is_empty(), "{:?}", pinned.failures);
                let target = scene.current_target().target;
                let generation = plan.targets[0].generation;
                let rejudged = rejudge(req, ctx, &target, work, generation).unwrap();
                assert!(
                    matches!(rejudged, Planned::Unchanged { .. }),
                    "the update survived the pin: {rejudged:?}"
                );
                if let Planned::Unchanged { warnings, notes } = rejudged {
                    let codes: Vec<WarningCode> = warnings.iter().map(|w| w.code).collect();
                    assert_eq!(codes, [WarningCode::AddonPinned]);
                    assert!(warnings[0].message.contains("pinned at 3"), "{warnings:?}");
                    assert!(notes.is_empty());
                }
            },
        );
    }

    #[test]
    fn target_kinds() {
        let change = |kind, fields: &[EditField]| Change {
            addon: "a".into(),
            r#ref: "gh:o/r".parse().unwrap(),
            kind,
            fields: fields.to_vec(),
            from_version: None,
            to_version: None,
            folders: FolderSummary::default(),
        };
        use ChangeKind::{Adopt, Edit, Install, Remove, Update};
        let kind = |changes: &[Change], over: Option<TxKind>, default: TxKind| {
            target_kind(changes, over, default)
        };
        let pin = change(Edit, &[EditField::Pinned]);
        let required = change(Edit, &[EditField::RequiredBy]);
        let both = change(Edit, &[EditField::Channel, EditField::Pinned]);
        /// Changes, the override, the default kind, and the kind expected.
        type Case = (Vec<Change>, Option<TxKind>, TxKind, Option<TxKind>);
        let cases: [Case; 13] = [
            // 1. nothing changes
            (vec![], Some(TxKind::Pin), TxKind::Install, None),
            // 2. an override wins
            (
                vec![change(Install, &[])],
                Some(TxKind::Sync),
                TxKind::Install,
                Some(TxKind::Sync),
            ),
            (
                vec![change(Adopt, &[])],
                Some(TxKind::Adopt),
                TxKind::Install,
                Some(TxKind::Adopt),
            ),
            // 3. pinned-only edits are a pin
            (vec![pin.clone()], None, TxKind::Install, Some(TxKind::Pin)),
            (
                vec![pin.clone(), pin.clone()],
                None,
                TxKind::Update,
                Some(TxKind::Pin),
            ),
            // 4. edits and adoptions alone are the command's kind
            (
                vec![both.clone()],
                None,
                TxKind::Update,
                Some(TxKind::Update),
            ),
            (
                vec![pin.clone(), required.clone()],
                None,
                TxKind::Install,
                Some(TxKind::Install),
            ),
            (
                vec![change(Adopt, &[])],
                None,
                TxKind::Install,
                Some(TxKind::Install),
            ),
            // 5. otherwise the folder changes decide
            (
                vec![change(Remove, &[]), required.clone()],
                None,
                TxKind::Remove,
                Some(TxKind::Remove),
            ),
            (
                vec![change(Remove, &[]), pin.clone()],
                None,
                TxKind::Install,
                Some(TxKind::Remove),
            ),
            (
                vec![change(Install, &[]), change(Remove, &[])],
                None,
                TxKind::Install,
                Some(TxKind::Install),
            ),
            (
                vec![change(Install, &[]), change(Update, &[]), pin],
                None,
                TxKind::Install,
                Some(TxKind::Update),
            ),
            (
                vec![change(Remove, &[])],
                None,
                TxKind::Install,
                Some(TxKind::Remove),
            ),
        ];
        for (changes, over, default, expected) in cases {
            let kinds: Vec<ChangeKind> = changes.iter().map(|c| c.kind).collect();
            assert_eq!(
                kind(&changes, over, default),
                expected,
                "{kinds:?} {over:?} {default:?}"
            );
        }
    }
}
