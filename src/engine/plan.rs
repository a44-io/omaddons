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

use super::blobs::{BlobCache, BlobRef, DownloadTarget, Fetched, Storage};
use super::edit::{
    EditField, RecordEdit, RecordGuard, apply_edit, apply_edits, changed_fields, effective_edits,
    sorted_names,
};
use super::indexes::{DepIndexes, IndexLookups};
use super::inspect::{
    ArchiveLimits, FolderVerdict, Grade, Inspected, SkippedFolder, grade, inspect, inspect_dir,
    partition, verify,
};
use super::ownership::{self, AllowedOp, FolderChange, Transfer, WorkingView};
use super::retention::RetentionLimits;
use super::running::{self, PolicyContext, PolicyOp};
use super::swap::SwapOp;
use super::witness::{self, ApprovalWitness, InputWitness, WitnessCtx, WitnessScope};
use crate::error::{Error, ErrorBody, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::fsx::{ScopeTable, TreeDigest, WriteSession, Writer, ensure_dir_all, failpoint};
use crate::inventory::{Depth, FolderClass, Inventory, inventory_shared, sibling_records};
use crate::limits::{DEPENDENCY_MAX_ADDONS, DEPENDENCY_MAX_DEPTH};
use crate::model::{
    Actor, AddonReason, AddonRecord, AddonState, AssetLocation, Candidate, Channel, ExpectedHash,
    FlavorHint, FolderName, MutableFolderName, RankReason, Ref, ReleaseSet, ReloadRequired, Target,
    TargetKey, TxId, TxKind, VersionConfidence,
};
use crate::net::Http;
use crate::output::{Phase, ProgressSink};
use crate::resolve::cache::ReleaseCache;
use crate::resolve::catalog::Catalog;
use crate::resolve::deps::{
    AmbiguityReason, BridgeDependency, DepLookups, DepResolution, DepView, MissingReason,
    Satisfier, TukuiDirs, ViewFolder, WowiDirs, bridge_dependencies, is_satisfied, needed_folders,
    resolve,
};
use crate::resolve::select::{EmptyReason, NameEvidence, name_evidence, select};
use crate::resolve::updates::is_static;
use crate::rules::{Family, RulesTable};
use crate::sources::github_rest::evidence_hints;
use crate::sources::{SourceId, SourceRegistry, Wanted, wowace};
use crate::state::config::RunningPolicy;
use crate::state::files_index::{FilesIndex, FolderIndex};
use crate::state::memo::{CandidateFact, VerdictFact, merge_fact};
use crate::state::paths::{ClientPaths, Paths};
use crate::state::records::{TargetRecord, load_record};
use crate::state::store;
use crate::toc::{DiskFolder, TocLevel, Verdict, WrongFlavorReason, loaded_toc};

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
    /// `--no-deps`: install only what was asked for; what the TOCs require is still
    /// checked and warned about.
    pub no_deps: bool,
    /// Collect an approval witness per target while planning (`Plan.witnesses`), with
    /// what it covers beyond the plan; `None` collects none.
    pub witness: Option<WitnessScope>,
}

/// The changes for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRequest {
    pub target: Target,
    pub changes: Vec<RequestedChange>,
    /// What the command means when only records change (see [`target_kind`]).
    pub default_kind: TxKind,
    /// Versions to fetch for dependencies that resolve to these refs (by identity);
    /// a lock never pins.
    pub dependency_locks: BTreeMap<Ref, String>,
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
        /// `sync --locked`: the archive the desired state recorded (its `sha256`), used
        /// from the blob cache while it is there and still hashes to it. Internal.
        blob_sha256: Option<String>,
        /// A pasted release-asset link's file name: that asset or nothing.
        asset: Option<String>,
        /// With `version`: the release's source revision the desired state recorded
        /// (WowAce), so exactly that tree is installed, whatever the tag says now.
        revision: Option<u32>,
        /// `--any-flavor` / `--no-any-flavor`, or a desired-state entry's `any_flavor`.
        any_flavor: AnyFlavorIntent,
    },
    Remove {
        addon: String,
    },
    /// The newest compatible release of an installed addon, keeping its pin, channel
    /// and reason: a pinned addon stays where it is (`addon_pinned`).
    Update {
        addon: String,
        /// For a record versioned by its TOC (an identified adoption): the installed
        /// primary folder's in-family interface. A candidate below it, or out of date on
        /// this client, is refused (M2-D42), so an adopted newer build is never replaced
        /// by an older one.
        installed_primary_interface: Option<u32>,
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

/// What a change does to an installed addon's `--any-flavor` setting
/// ([`AddonRecord::any_flavor`]): kept as recorded unless a request sets or clears it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnyFlavorIntent {
    /// The recorded setting stays; a new addon has none.
    #[default]
    Keep,
    Set,
    Clear,
}

impl AnyFlavorIntent {
    /// Whether an install under this intent judges builds with the setting, given the
    /// installed record.
    pub fn applies(self, existing: Option<&AddonRecord>) -> bool {
        match self {
            Self::Keep => existing.is_some_and(|e| e.any_flavor),
            Self::Set => true,
            Self::Clear => false,
        }
    }
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
    /// The floor a candidate must reach (`Update.installed_primary_interface`).
    pub floor_interface: Option<u32>,
    /// A reinstall's installed release as planned against (`Some(release_id)`): under the
    /// lock, a record that moved since (a concurrent update) refuses the reinstall
    /// instead of rolling it back. `None` for every other install.
    pub reinstall_of: Option<Option<String>>,
    /// The one asset to install, by file name (a pasted release-asset link): never
    /// another asset of the release.
    pub asset: Option<String>,
    /// `--any-flavor` / `--no-any-flavor`; the resolver's installs always `Keep`.
    pub any_flavor: AnyFlavorIntent,
    /// Set on the second pass of a GitHub look-back. A look-back never looks back again.
    pub looked_back: Option<LookedBack>,
}

/// The second pass of a GitHub look-back: the newest release's tag, which had no build
/// for this client, and when the release found was published (from the feed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookedBack {
    pub newest: String,
    pub published: Option<Timestamp>,
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
            floor_interface: None,
            reinstall_of: None,
            asset: None,
            any_flavor: AnyFlavorIntent::Keep,
            looked_back: None,
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
        existing.pinned
            && self.pin == PinIntent::Keep
            && self.version.is_none()
            && !self.reinstall
            && self.any_flavor == AnyFlavorIntent::Keep
    }

    /// Sets or clears `--any-flavor` on the pinned `existing` without moving it: its
    /// release stays, and only the record changes.
    pub fn pinned_setting_only(&self, existing: &AddonRecord) -> bool {
        existing.pinned
            && self.pin == PinIntent::Keep
            && self.version.is_none()
            && !self.reinstall
            && self.any_flavor != AnyFlavorIntent::Keep
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
/// | `any_flavor` | `Set` | `Set` true, `Clear` false, `Keep` as recorded | as recorded |
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
    record.any_flavor = intent.any_flavor.applies(existing);
    match existing {
        None => {
            record.pinned = matches!(intent.pin, PinIntent::Pin);
            record.channel = intent.channel;
            record.reason = intent.reason;
        }
        Some(existing) => {
            record.name.clone_from(&existing.name);
            if intent.from_resolver() {
                record.pinned = existing.pinned;
                record.channel = existing.channel;
                record.reason = existing.reason;
                record.any_flavor = existing.any_flavor;
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
                EditField::Pinned | EditField::Channel | EditField::Reason | EditField::AnyFlavor
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
            EditField::AnyFlavor => Some(RecordEdit::SetAnyFlavor(carried.any_flavor)),
            EditField::Ref | EditField::RequiredBy => None,
        })
        .collect()
}

/// `addon_pinned`: a pinned addon was left where it is. Its fix unpins it there.
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
        "unpin",
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
    /// Client dirs whose game client runs now: every one when the process table could
    /// not be read.
    pub running: &'a BTreeSet<PathBuf>,
    /// Why the process table could not be read, when it could not.
    pub running_unknown: Option<&'a str>,
    /// Every target the command discovered: the clients sharing a target's AddOns
    /// directory are found among them.
    pub known_targets: &'a [Target],
    pub policy: RunningPolicy,
    pub limits: ArchiveLimits,
    /// How long snapshots are kept, which warnings promising an undo state.
    pub retention: RetentionLimits,
    pub actor: Actor,
    pub argv: &'a [String],
    /// Where dependencies resolve (with the Tukui and WoWInterface indexes once they
    /// exist).
    pub catalog: &'a Catalog,
    /// The channel dependencies install from (config `default_channel`, else stable;
    /// never the requester's, M2-D33).
    pub default_channel: Channel,
    /// What the command downloaded so far, shared by its plans (M2-D28).
    pub downloads: &'a DownloadMemo,
    /// Where dependencies resolve after the catalog; `None`: the catalog only.
    pub indexes: Option<&'a super::indexes::DepIndexes>,
}

/// What a command downloaded, by `(release_id, asset file name)`: the blob, or the
/// error, so its later plans and targets never download one archive twice (M2-D28).
/// It lives in memory for one command (`install`'s probe and real run share it), so a
/// new command tries a failed download again. Local and git assets never enter it.
#[derive(Debug, Default)]
pub struct DownloadMemo {
    entries: std::sync::Mutex<BTreeMap<(String, String), Result<Downloaded>>>,
}

impl DownloadMemo {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), Result<Downloaded>>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get(&self, key: &(String, String)) -> Option<Result<Downloaded>> {
        self.lock().get(key).cloned()
    }

    fn put(&self, key: (String, String), result: Result<Downloaded>) {
        self.lock().insert(key, result);
    }

    /// How many assets the command downloaded or failed to.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A downloaded asset: its blob, and a deferred asset's canonical URL (`None` when it
/// came from the cache without resolving).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub blob: BlobRef,
    pub canonical_url: Option<String>,
    /// Why the download was not checked against the hash its source normally
    /// publishes (`download_unverified`); `None` when it was, or there is none.
    pub unverified: Option<String>,
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
    /// One approval witness per target, in `targets` order, when the request asked for
    /// them; empty otherwise. Only its digest leaves the engine.
    #[serde(skip)]
    pub witnesses: Vec<ApprovalWitness>,
}

impl Plan {
    /// Whether any target has something to do.
    pub fn has_ops(&self) -> bool {
        self.targets
            .iter()
            .any(|t| !t.ops.is_empty() || !t.work.is_empty())
    }

    /// The plan of a run that changes nothing on `targets`, each with the record
    /// generation it was judged at: one empty `TargetPlan` per target (no ops, changes,
    /// releases, warnings or work; `creates_addons_dir: false`), no failures and no
    /// witnesses. A no-op path's token is computed over it.
    pub fn idle(targets: &[(&Target, u64)], argv: &[String], actor: Actor) -> Self {
        let mut generations = BTreeMap::new();
        let targets = targets
            .iter()
            .map(|(target, generation)| {
                let key = target
                    .key
                    .map_or_else(|| target.id.clone(), |k| k.to_string());
                generations.insert(key, *generation);
                TargetPlan {
                    target: target.id.clone(),
                    key: target.key,
                    kind: None,
                    changes: Vec::new(),
                    ops: Vec::new(),
                    releases: Vec::new(),
                    reload_required: ReloadRequired::None,
                    warnings: Vec::new(),
                    creates_addons_dir: false,
                    generation: *generation,
                    work: Vec::new(),
                    unchanged_warnings: Vec::new(),
                    notes: Vec::new(),
                    default_kind: TxKind::Install,
                    dependencies: BTreeMap::new(),
                }
            })
            .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            plan_id: None,
            created_at: Timestamp::now(),
            expires_at: None,
            argv: argv.to_vec(),
            actor,
            dry_run: true,
            generations,
            targets,
            failures: Vec::new(),
            facts: Vec::new(),
            witnesses: Vec::new(),
        }
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
    /// What each requester's dependencies came to, by requester name.
    #[serde(skip)]
    pub dependencies: BTreeMap<String, DepReport>,
}

/// What one addon's required dependencies came to on a target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepReport {
    /// Installed with it: (the folder it required, the dependency's addon).
    pub installs: Vec<(FolderName, String)>,
    /// Present but unmanaged: nothing installed, a hint to look.
    pub unmanaged: Vec<FolderName>,
    /// Not installed, and why (with up to three candidates).
    pub unresolved: Vec<Unresolved>,
    /// Resolved, but the dependency failed to plan: (folder, its ref).
    pub failed: Vec<(FolderName, Ref)>,
    /// What its other folders (bridges, plugins) require and nothing provides once the
    /// transaction is done: reported, never installed (M2-D7). Sorted.
    pub bridge_dependencies: Vec<BridgeDependency>,
    /// The target had no addon with this ref before the transaction.
    pub first_install: bool,
}

/// A required folder nothing was installed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Unresolved {
    pub folder: FolderName,
    pub reason: UnresolvedReason,
    pub candidates: Vec<Ref>,
}

/// Why a required folder was not installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedReason {
    NoSource,
    ManagedFolderMissing,
    SeveralCatalogEntries,
    WowiAmbiguous,
    WowiAbandoned,
    UnknownCondition,
    /// The candidate's build is out of date for this client.
    OutOfDate,
    /// Past `DEPENDENCY_MAX_DEPTH` or `DEPENDENCY_MAX_ADDONS`.
    LimitReached,
    /// `--no-deps`.
    NotRequested,
    /// The resolved project's archive does not ship the folder for this client.
    NotShipped,
}

/// What an install's folders require and how they verified, for the dependency rounds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepFacts {
    /// The required dependencies of its needed folders (the primary and the folders of
    /// its own archive it requires, transitively), each once, in folder order. Optional
    /// dependencies are never here.
    pub requires: Vec<FolderName>,
    /// What those folders require under a condition of a kind this build does not
    /// read: reported `unknown_condition`, never installed.
    pub unknown_condition: Vec<FolderName>,
    /// Installed folders that verified out of date on this client.
    pub out_of_date: BTreeSet<FolderName>,
    /// What the other installed folders require outside the archive, before anything is
    /// judged satisfied.
    pub bridges: Vec<BridgeDependency>,
    /// The needed folders' `OptionalDeps`, each once, in folder order: shown under `-v`,
    /// never installed or warned about.
    pub optional: Vec<FolderName>,
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
    /// `{"sha256": …}` or `{"md5": …}`; `null` when the source declares none.
    pub expected_hash: Option<ExpectedHash>,
    pub blob_sha256: Option<String>,
    /// A deferred asset's source and key: its URL is resolved only for a fresh
    /// download, so a token keys the release by these instead (`precondition::of_plan`).
    #[serde(skip)]
    pub deferred: Option<DeferredAsset>,
}

/// A release whose download URL a source resolves right before each attempt (Tukui).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredAsset {
    pub source: crate::sources::SourceId,
    pub key: String,
}

impl DeferredAsset {
    /// The deferred asset `location` names, if it is one.
    fn of(location: &AssetLocation) -> Option<Self> {
        match location {
            AssetLocation::Deferred { source, key } => Some(Self {
                source: *source,
                key: key.clone(),
            }),
            _ => None,
        }
    }
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
    /// Folders this addon stopped shipping that another work of the transaction took
    /// over (a handover): their removal is that work's replacement.
    pub handed_over: Vec<FolderName>,
    /// What the installed folders require (empty for removes).
    pub deps: DepFacts,
    /// Files of the folders it replaces that changed on disk since they were indexed,
    /// found at plan time (M2-D17): the commit warns `local_changes_replaced`.
    pub local_changes: Option<LocalChanges>,
    /// What checking its archive warned at plan time: the verdicts of the folders that
    /// install (`out_of_date`, `toc_comma_on_legacy`) and `download_unverified`. The
    /// commit inspects and downloads nothing, so it warns again from the work.
    pub archive_warnings: Vec<Warning>,
    /// The local input it installs from (a `--file` zip or directory, a git checkout),
    /// for the approval witness; `None` for downloads, cached blobs and removes.
    pub input: Option<InputWitness>,
}

/// Changed files of the folders a work replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalChanges {
    /// `Folder/relative/path`, sorted, at most `LOCAL_CHANGES_SHOWN`.
    pub paths: Vec<String>,
    /// Every changed file: modified, deleted or added.
    pub count: u64,
    /// The replaced folders that changed, by key.
    pub folders: Vec<FolderName>,
}

/// Changed paths a `local_changes_replaced` warning names.
const LOCAL_CHANGES_SHOWN: usize = 10;

/// Folders a `snapshot_only_copy` warning names.
const ONLY_COPY_SHOWN: usize = 10;

/// `local_changes_replaced` for `addon`: its replaced folders held files changed by
/// hand. Committed as `tx`, the fix is `omaddons undo <tx>`; planned only (a dry run),
/// where the files are kept. The message states how long `retention` keeps them.
pub fn local_changes_warning(
    addon: &str,
    changes: &LocalChanges,
    tx: Option<TxId>,
    retention: &RetentionLimits,
) -> Warning {
    let n = changes.count;
    let files = if n == 1 { "file" } else { "files" };
    let more = if (changes.paths.len() as u64) < n {
        ", …"
    } else {
        ""
    };
    let message = format!(
        "{addon}: replaced {n} locally changed {files} ({}{more}); undo brings them back until retention prunes the snapshot ({})",
        changes.paths.join(", "),
        retention_bound(retention)
    );
    let fix = match tx {
        Some(tx) => Suggestion::command(["omaddons".to_owned(), "undo".to_owned(), tx.to_string()]),
        None => Suggestion::text(
            "the replaced files are kept in the transaction snapshot; `omaddons undo` restores them",
        ),
    };
    Warning::new(WarningCode::LocalChangesReplaced, message).with_fix(fix)
}

/// Whether a folder a transaction moves into its snapshot (replaced or removed) exists
/// nowhere else, judged by `owner`, the addon that recorded it before: none (an
/// unmanaged folder replaced under `--force`), or one with no cached archive that no
/// source gives again (adopted as it was, or from a local archive or directory).
pub fn only_copy(owner: Option<&AddonRecord>) -> bool {
    owner.is_none_or(|o| o.blob_sha256.is_none() && (o.release_id.is_none() || is_static(o)))
}

/// The live folder `op` moves into the snapshot when that is its only copy
/// ([`only_copy`], the owner taken from `record`, the target's record before).
pub fn moves_only_copy<'s>(record: &TargetRecord, op: &'s SwapOp) -> Option<&'s MutableFolderName> {
    let live = match op {
        SwapOp::Replace { live, .. } | SwapOp::Remove { live, .. } => live,
        SwapOp::Add { .. } => return None,
    };
    let owner = record
        .addons
        .values()
        .find(|a| a.folders.iter().any(|f| f.key() == live.key()));
    only_copy(owner).then_some(live)
}

/// The folders `works` move into the snapshot that exist nowhere else, by key, once
/// each, judged against `record` (the target's record before). A folder whose files
/// changed by hand is `local_changes_replaced`'s; one replaced by the same bytes (its
/// indexed digest equals the incoming folder's) loses nothing.
pub fn only_copies(record: &TargetRecord, works: &[AddonWork]) -> Vec<FolderName> {
    let mut found: Vec<FolderName> = Vec::new();
    for work in works {
        let WorkMode::Folders(f) = &work.mode else {
            continue;
        };
        let changed = f
            .local_changes
            .as_ref()
            .map_or(&[][..], |local| &local.folders[..]);
        for op in &f.swap {
            let Some(live) = moves_only_copy(record, op) else {
                continue;
            };
            // ubs:ignore — comparing folder name keys, not secrets
            let listed = |among: &[FolderName]| among.iter().any(|x| x.key() == live.key());
            if listed(changed) || listed(&found) || same_bytes(op, f.inspected.as_ref()) {
                continue;
            }
            found.push(live.as_folder().clone());
        }
    }
    found.sort_by(|a, b| a.key().cmp(b.key()));
    found
}

/// Whether `op` replaces its live folder with the same bytes: the live folder's indexed
/// digest equals the incoming folder's in `inspected`.
fn same_bytes(op: &SwapOp, inspected: Option<&Inspected>) -> bool {
    let SwapOp::Replace {
        name: staged,
        old_tree: Some(live_tree),
        ..
    } = op
    else {
        return false;
    };
    let Some(folder) =
        inspected.and_then(|i| i.folders.iter().find(|x| x.name.key() == staged.key()))
    else {
        return false;
    };
    let staged_tree = TreeDigest::from_hex_entries(
        folder
            .files
            .iter()
            .map(|e| (e.rel_path.as_str(), e.size, &e.sha256)),
    );
    // ubs:ignore — tree digests of addon files, public content, not secrets
    staged_tree == *live_tree
}

/// `snapshot_only_copy` naming `folders` (the first `ONLY_COPY_SHOWN`, then a count)
/// on `target`: committed as `tx`, its snapshot holds their only copy and the fix is
/// `omaddons undo <tx>`; planned only (a dry run), a reminder. `None` for no folders.
pub fn only_copy_warning(
    target: &str,
    folders: &[FolderName],
    tx: Option<TxId>,
    retention: &RetentionLimits,
) -> Option<Warning> {
    if folders.is_empty() {
        return None;
    }
    let shown: Vec<&str> = folders
        .iter()
        .take(ONLY_COPY_SHOWN)
        .map(FolderName::as_str)
        .collect();
    let list = match folders.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    };
    let bound = retention_bound(retention);
    let (message, fix) = match tx {
        Some(tx) => (
            format!(
                "On {target}, {tx}'s snapshot holds the only copy of {list}: `omaddons undo {tx}` brings them back until retention prunes it ({bound})"
            ),
            Suggestion::command(["omaddons".to_owned(), "undo".to_owned(), tx.to_string()]),
        ),
        None => (
            format!(
                "On {target}, the transaction's snapshot will hold the only copy of {list}: undo brings them back until retention prunes it ({bound})"
            ),
            Suggestion::text(
                "nothing is moved under --dry-run; after the run, its snapshot holds the only copy until retention prunes it",
            ),
        ),
    };
    Some(Warning::new(WarningCode::SnapshotOnlyCopy, message).with_fix(fix))
}

/// How long `retention` keeps a transaction's snapshot, in words.
fn retention_bound(retention: &RetentionLimits) -> String {
    format!(
        "kept at least {}; then pruned once {} newer transactions on this client exist, after {}, or past the {} snapshot cap",
        span(retention.tx.min_age),
        retention.tx.keep_last,
        span(retention.tx.window),
        size(retention.snapshot_max_bytes)
    )
}

/// `d` in the largest whole unit up to days (hours below two days: "24 hours").
fn span(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (n, unit) = if s >= 2 * 86_400 && s.is_multiple_of(86_400) {
        (s / 86_400, "day")
    } else if s >= 3_600 && s.is_multiple_of(3_600) {
        (s / 3_600, "hour")
    } else if s >= 60 && s.is_multiple_of(60) {
        (s / 60, "minute")
    } else {
        (s, "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// `bytes` in the largest binary unit that divides it.
fn size(bytes: u64) -> String {
    for (unit, n) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if bytes >= n && bytes.is_multiple_of(n) {
            return format!("{} {unit}", bytes / n);
        }
    }
    format!("{bytes} bytes")
}

/// The warnings dependency bookkeeping raises for `works` on `target`, whose record
/// before the transaction is `record`:
/// - `still_required` for each removed addon that addons staying still require (the
///   remove goes ahead); committed as `tx`, its fix is `omaddons undo <tx>`;
/// - one `orphaned_dependency` naming every dependency the transaction left required by
///   nothing, with one `omaddons remove` for all of them as its fix.
pub fn bookkeeping_warnings(
    record: &TargetRecord,
    works: &[AddonWork],
    target: &str,
    tx: Option<TxId>,
) -> Vec<Warning> {
    let key = |name: &str| TargetRecord::addon_key(name);
    let removed: BTreeSet<String> = works
        .iter()
        .filter(|w| w.record().is_none())
        .map(|w| key(&w.addon))
        .collect();
    let stays = |name: &str| {
        let k = key(name);
        !removed.contains(&k)
            && (record.addons.contains_key(&k)
                || works
                    .iter()
                    .any(|w| w.record().is_some_and(|r| key(&r.name) == k)))
    };
    let mut warnings = Vec::new();
    for w in works.iter().filter(|w| w.record().is_none()) {
        let Some(gone) = record.addons.get(&key(&w.addon)) else {
            continue;
        };
        let dependents: Vec<&str> = gone
            .required_by
            .iter()
            .filter(|n| stays(n))
            .map(String::as_str)
            .collect();
        warnings.extend(still_required(target, &gone.name, &dependents, tx));
    }
    let orphans = sorted_names(works.iter().filter_map(|w| {
        let after = w.record()?;
        let before = record.addons.get(&key(&w.addon))?;
        (!before.is_orphan() && after.is_orphan()).then(|| after.name.clone())
    }));
    warnings.extend(orphaned(target, orphans));
    warnings
}

/// [`bookkeeping_warnings`] for a change of `target`'s record from `before` to `after`
/// (an undo, which restores records rather than planning works). `still_required` takes
/// each removed addon's requesters from `before`, so a requester recorded only by a
/// later in-place edit counts.
pub fn record_change_warnings(
    before: &TargetRecord,
    after: &TargetRecord,
    target: &str,
    tx: Option<TxId>,
) -> Vec<Warning> {
    let stays = |name: &str| after.addons.contains_key(&TargetRecord::addon_key(name));
    let mut warnings = Vec::new();
    for (key, gone) in &before.addons {
        if after.addons.contains_key(key) {
            continue;
        }
        let dependents: Vec<&str> = gone
            .required_by
            .iter()
            .filter(|n| stays(n))
            .map(String::as_str)
            .collect();
        warnings.extend(still_required(target, &gone.name, &dependents, tx));
    }
    let orphans = sorted_names(after.addons.iter().filter_map(|(key, now)| {
        let then = before.addons.get(key)?;
        (!then.is_orphan() && now.is_orphan()).then(|| now.name.clone())
    }));
    warnings.extend(orphaned(target, orphans));
    warnings
}

/// `still_required` for `gone` on `target` while `dependents` stay (none: no warning).
/// Committed as `tx`, its fix is `omaddons undo <tx>`.
fn still_required(
    target: &str,
    gone: &str,
    dependents: &[&str],
    tx: Option<TxId>,
) -> Option<Warning> {
    if dependents.is_empty() {
        return None;
    }
    let message = format!(
        "On {target}, {gone} is required by {}, which will not load without it",
        dependents.join(", ")
    );
    let fix = match tx {
        Some(tx) => Suggestion::command(["omaddons".to_owned(), "undo".to_owned(), tx.to_string()]),
        None => Suggestion::text(
            "nothing is removed under --dry-run; after the remove, `omaddons undo <tx>` restores it",
        ),
    };
    Some(Warning::new(WarningCode::StillRequired, message).with_fix(fix))
}

/// One `orphaned_dependency` naming every one of `orphans` (none: no warning), with one
/// `omaddons remove` for all of them as its fix.
fn orphaned(target: &str, orphans: Vec<String>) -> Option<Warning> {
    let message = match orphans.as_slice() {
        [] => return None,
        [one] => format!(
            "On {target}, {one} was installed as a dependency, and nothing requires it any more"
        ),
        many => format!(
            "On {target}, {} were installed as dependencies, and nothing requires them any more",
            many.join(", ")
        ),
    };
    let argv = ["omaddons", "remove"]
        .into_iter()
        .map(str::to_owned)
        .chain(orphans)
        .chain(["-t".to_owned(), target.to_owned()]);
    Some(
        Warning::new(WarningCode::OrphanedDependency, message)
            .with_fix(Suggestion::command(argv.collect::<Vec<_>>())),
    )
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

    /// Files changed by hand in the folders it replaces.
    pub fn local_changes(&self) -> Option<&LocalChanges> {
        match &self.mode {
            WorkMode::Folders(f) => f.local_changes.as_ref(),
            WorkMode::Records(_) => None,
        }
    }

    /// The local input it installs from, for the approval witness.
    pub fn input(&self) -> Option<&InputWitness> {
        match &self.mode {
            WorkMode::Folders(f) => f.input.as_ref(),
            WorkMode::Records(_) => None,
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
        intent: Box<InstallIntent>,
        /// A reinstall's recorded release from the blob cache ([`local_blob_candidate`]):
        /// the source is not asked.
        local: Option<Box<Candidate>>,
        /// A locked install's recorded archive hash (`RequestedChange::Install.blob_sha256`).
        cached: Option<String>,
        /// The recorded source revision of the release `wanted.version` names: a lock's
        /// (`RequestedChange::Install.revision`), or the installed one's on an exact
        /// reinstall.
        revision: Option<u32>,
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
        // A target whose record or files index does not read fails its own changes, each
        // under its own ref and addon; the other targets plan on.
        let (record, index, unreadable) = match target_state(ctx.paths, &tr.target) {
            Ok((record, index)) => (record, index, None),
            Err(error) => (
                TargetRecord::empty(TargetKey::from_ulid(ulid::Ulid::nil())),
                None,
                Some(error),
            ),
        };
        let generation_key = tr
            .target
            .key
            .map_or_else(|| tr.target.id.clone(), |k| k.to_string());
        generations.insert(generation_key, record.generation);
        let mut wants: Vec<Want> = tr
            .changes
            .iter()
            .map(|c| match &unreadable {
                Some(error) => unreadable_want(c, error),
                None => want(c, &record),
            })
            .collect();
        // A reinstall of the recorded release whose archive is still cached needs no
        // source.
        for w in &mut wants {
            if let Want::Install {
                wanted,
                file: None,
                intent,
                local,
                ..
            } = w
                && intent.reinstall
                && intent.version.is_none()
                && let Some(installed) = record.addons.values().find(|a| a.r#ref == wanted.r#ref)
                && let (Some(release_id), Some(sha)) =
                    (&installed.release_id, &installed.blob_sha256)
            {
                *local = local_blob_candidate(
                    ctx.blobs,
                    ctx.scopes,
                    release_id,
                    &installed.version,
                    installed.channel,
                    sha,
                    installed.revision,
                )
                .map(Box::new);
            }
        }
        // A locked install whose archive the cache holds, and whose release a fact names
        // by that archive's hash, needs no source either.
        for w in &mut wants {
            if let Want::Install {
                wanted,
                file: None,
                local: local @ None,
                cached: Some(sha),
                revision,
                ..
            } = w
                && let Some(version) = &wanted.version
                && let Some(release_id) = ctx.cache.facts().release_of(sha)
            {
                *local = local_blob_candidate(
                    ctx.blobs,
                    ctx.scopes,
                    release_id,
                    version,
                    wanted.channel,
                    sha,
                    *revision,
                )
                .map(Box::new);
            }
        }
        // A WowAce release recorded at its revision (a lock, an exact reinstall) whose
        // archive is not cached: that tree at that revision, never the tag as it is now,
        // and no tag list asked for.
        for w in &mut wants {
            if let Want::Install {
                wanted,
                file: None,
                local: local @ None,
                revision: Some(revision),
                ..
            } = w
                && let Ref::WowAce { slug, rev } = &wanted.r#ref
                && let Some(version) = &wanted.version
            {
                *local = wowace::recorded_candidate(
                    slug,
                    rev.as_ref(),
                    version,
                    *revision,
                    wanted.channel,
                )
                .map(Box::new);
            }
        }
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
                local: None,
                ..
            } if !keeps_installed(record, &wanted.r#ref, intent) => Some(wanted.clone()),
            _ => None,
        })
        .collect();
    // The cache reports each ref it asks of a source as it answers (`fetch` progress).
    let fetched = ctx.cache.get(&to_fetch);
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
    let mut witnesses = Vec::new();
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
            dependencies: BTreeMap::new(),
        };
        let live = match live_inventory(ctx, target, &record) {
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
                if req.witness.is_some() {
                    witnesses.push(bare_witness(target));
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
            view: RefCell::new(WorkingView::default()),
            notes: RefCell::new(Vec::new()),
        };
        // First pass, in argument order: inspect every install; a second request for a
        // ref collapses into the first (the command reports it as a duplicate).
        let mut seen: Vec<String> = Vec::new();
        let mut steps = Vec::with_capacity(wants.len());
        for w in wants {
            steps.push(match w {
                Want::Failed {
                    r#ref,
                    addon,
                    error,
                } => Step::Ready(r#ref, addon, Err(error)),
                Want::Install {
                    name,
                    wanted,
                    file,
                    intent,
                    local,
                    cached,
                    // Already a recorded tree's candidate in `local`.
                    revision: _,
                } => {
                    let identity = wanted.r#ref.identity();
                    if seen.contains(&identity) {
                        continue;
                    }
                    seen.push(identity);
                    // A pinned addon, one whose release is kept, or an install from the
                    // blob cache was not fetched.
                    let set = match (&file, &local) {
                        (None, None) => fetched.get(&wanted).map(|f| f.set.clone()),
                        _ => None,
                    };
                    let r = Some(wanted.r#ref.clone());
                    match planner.prepare_install(
                        name.as_deref(),
                        &wanted,
                        CandidateSource {
                            file: file.as_deref(),
                            local: local.as_deref(),
                            set,
                            cached: cached.as_deref(),
                        },
                        &intent,
                    ) {
                        Ok(Prepared::Build(build)) => Step::Build(r, name, build),
                        Ok(Prepared::Done(planned)) => Step::Ready(r, name, Ok(planned)),
                        Err(error) => Step::Ready(r, name, Err(error)),
                    }
                }
                Want::Remove { addon } => Step::Remove(addon),
                Want::Edit { addon, edit } => Step::Edit(addon, edit),
            });
        }
        // Before any claim is judged: the folders an owner's own work stops shipping.
        for step in &steps {
            let mut view = planner.view.borrow_mut();
            match step {
                Step::Build(_, _, build) => {
                    if let Some(existing) = &build.existing {
                        for folder in build.released() {
                            view.release(&folder, &existing.name);
                        }
                    }
                }
                Step::Remove(addon) => {
                    if let Some(installed) = record.addons.get(&TargetRecord::addon_key(addon)) {
                        for folder in &installed.folders {
                            view.release(folder, &installed.name);
                        }
                    }
                }
                Step::Ready(..) | Step::Edit(..) => {}
            }
        }
        // Second pass, in argument order: judge each against the working view.
        for step in steps {
            // What the step released in the first pass, withdrawn should it fail: a
            // failed update or remove lets go of nothing, so a later claim of one of
            // those folders is judged as a takeover, never a silent handover.
            let releases: Vec<(FolderName, String)> = match &step {
                Step::Build(_, _, build) => build
                    .existing
                    .iter()
                    .flat_map(|e| build.released().into_iter().map(|f| (f, e.name.clone())))
                    .collect(),
                Step::Remove(addon) => record
                    .addons
                    .get(&TargetRecord::addon_key(addon))
                    .map(|i| {
                        i.folders
                            .iter()
                            .map(|f| (f.clone(), i.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                Step::Ready(..) | Step::Edit(..) => Vec::new(),
            };
            let (r, addon, result) = match step {
                Step::Ready(r, addon, result) => (r, addon, result),
                Step::Build(r, addon, build) => {
                    (r, addon, planner.build(*build).map(Planned::change))
                }
                Step::Remove(addon) => {
                    let result = planner.remove(&addon).map(Planned::change);
                    (None, Some(addon), result)
                }
                Step::Edit(addon, edit) => {
                    let result = planner.edit(&addon, &edit, &tp.work);
                    (None, Some(addon), result)
                }
            };
            match &result {
                Ok(Planned::Change(p)) => claim_work(&mut planner.view.borrow_mut(), &p.work),
                Err(_) => {
                    let mut view = planner.view.borrow_mut();
                    for (folder, owner) in &releases {
                        view.withdraw(folder, owner);
                    }
                }
                Ok(_) => {}
            }
            match result.and_then(|planned| planned.merge_into(&mut tp, &target.id)) {
                Ok(()) => {}
                Err(error) => failures.push(failure(r, addon, target, error)),
            }
        }
        settle_handovers(&mut tp.work, &mut tp.changes, Some(&mut tp.ops));
        planner.dependency_rounds(&mut tp, &mut failures, &tr.dependency_locks);
        settle_handovers(&mut tp.work, &mut tp.changes, Some(&mut tp.ops));
        planner.unrequire(&mut tp, &mut failures);
        planner.bridges(&mut tp);
        // A dry run shows them; a commit raises them again with its transaction.
        let bookkept = bookkeeping_warnings(&record, &tp.work, &target.id, None);
        push_new(&mut tp.warnings, bookkept);
        let only = only_copies(&record, &tp.work);
        let only = only_copy_warning(&target.id, &only, None, &ctx.retention);
        push_new(&mut tp.warnings, only.into_iter().collect());
        tp.kind = target_kind(&tp.changes, req.kind, tp.default_kind);
        tp.creates_addons_dir =
            live.addons_dir_missing && tp.work.iter().any(|w| w.inspected().is_some());
        tp.notes.extend(planner.notes.take());
        if let Some(scope) = &req.witness {
            let wctx = WitnessCtx {
                paths: ctx.paths,
                target,
                index: index.as_ref(),
                scope,
                limits: ctx.limits,
            };
            match witness::collect(&tp, &wctx) {
                Ok(witnessed) => witnesses.push(witnessed),
                Err(error) => {
                    unwitnessed(&mut tp, &mut failures, target, &error);
                    witnesses.push(bare_witness(target));
                }
            }
        }
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
        witnesses,
    })
}

/// A target's bare witness (its marker and judge), else one without a marker when even
/// that cannot be read (its changes have failed already): the token pairs a witness
/// with every target.
fn bare_witness(target: &Target) -> ApprovalWitness {
    witness::bare(target).unwrap_or_else(|_| {
        ApprovalWitness::new(
            crate::model::TargetRef::of(target.key, &target.id),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            target.judge.clone(),
        )
    })
}

/// A target whose witness could not be collected: nothing of it can commit. Each change
/// it planned fails with `error` under its own ref and addon, as when its AddOns cannot
/// be listed; only the warnings of addons it left unchanged remain.
fn unwitnessed(
    tp: &mut TargetPlan,
    failures: &mut Vec<PlanFailure>,
    target: &Target,
    error: &Error,
) {
    for change in std::mem::take(&mut tp.changes) {
        failures.push(failure(
            Some(change.r#ref),
            Some(change.addon),
            target,
            error.clone(),
        ));
    }
    tp.work.clear();
    tp.ops.clear();
    tp.releases.clear();
    tp.dependencies.clear();
    tp.warnings.clone_from(&tp.unchanged_warnings);
    tp.reload_required = ReloadRequired::None;
    tp.kind = None;
    tp.creates_addons_dir = false;
}

/// The target as dependency checks see it while planning: what this transaction's
/// works claim, then AddOns as it is.
struct PlanView<'a> {
    view: &'a WorkingView,
    live: &'a Inventory,
}

impl DepView for PlanView<'_> {
    fn folder(&self, dep: &FolderName) -> Option<ViewFolder> {
        if let Some(addon) = self.view.owner_of(dep) {
            return Some(ViewFolder::Planned {
                addon: addon.to_owned(),
            });
        }
        // Its owner's work in this transaction stops shipping it.
        if self.view.released_by(dep).is_some() {
            return None;
        }
        let entry = self.live.folders.iter().find(|e| e.key == dep.key())?;
        Some(ViewFolder::Present {
            class: entry.class,
            owner: entry.owners.first().cloned(),
        })
    }
}

/// The limits' names, as `limit_reached` warnings say them.
const MAX_DEPTH: &str = "DEPENDENCY_MAX_DEPTH";
const MAX_ADDONS: &str = "DEPENDENCY_MAX_ADDONS";

/// `missing_dependency`: `requester` needs `folder`, and nothing was installed for it.
/// `detail` is the owner for `managed_folder_missing` and the limit's name for
/// `limit_reached`.
fn missing_dependency(
    target: &Target,
    requester: &str,
    unresolved: &Unresolved,
    detail: Option<&str>,
    argv: &[String],
) -> Warning {
    let folder = unresolved.folder.as_str();
    let id = target.id.as_str();
    let top = unresolved.candidates.first();
    // Without a candidate to install, what ships the folder is the next question.
    let search = || Suggestion::command(["omaddons", "search", folder]);
    let install_top = |r: &Ref| {
        Suggestion::command([
            "omaddons".to_owned(),
            "install".to_owned(),
            r.to_string(),
            "-t".to_owned(),
            id.to_owned(),
        ])
    };
    let listed: Vec<String> = unresolved
        .candidates
        .iter()
        .map(ToString::to_string)
        .collect();
    let (why, fix) = match unresolved.reason {
        UnresolvedReason::NoSource => (
            match detail {
                // The sources that could not be consulted, and why.
                Some(skipped) => format!(
                    "no keyless source consulted ships it (not consulted: {skipped}); it may only be on CurseForge or Wago, which arrive in M5"
                ),
                None => "no keyless source ships it; it may only be on CurseForge or Wago, which arrive in M5"
                    .to_owned(),
            },
            Suggestion::command(["omaddons", "search", folder]),
        ),
        UnresolvedReason::ManagedFolderMissing => (
            format!(
                "it is missing from {}'s folders",
                detail.unwrap_or("an installed addon")
            ),
            // Reinstalling its owner brings it back, at the recorded release.
            detail.map_or_else(
                || Suggestion::command(["omaddons", "list", "-t", id, "-v"]),
                |owner| {
                    Suggestion::command(["omaddons", "install", owner, "--reinstall", "-t", id])
                },
            ),
        ),
        UnresolvedReason::SeveralCatalogEntries | UnresolvedReason::WowiAmbiguous => (
            format!("several projects ship it ({})", listed.join(", ")),
            top.map_or_else(search, install_top),
        ),
        UnresolvedReason::WowiAbandoned => (
            format!(
                "the project that ships it ({}) has not changed in two years",
                listed.join(", ")
            ),
            top.map_or_else(search, install_top),
        ),
        UnresolvedReason::OutOfDate => (
            format!(
                "its build ({}) is out of date for this client",
                listed.join(", ")
            ),
            top.map_or_else(search, install_top),
        ),
        UnresolvedReason::LimitReached => (
            if detail == Some(MAX_ADDONS) {
                format!(
                    "it is past {MAX_ADDONS} ({DEPENDENCY_MAX_ADDONS} dependency installs per target)"
                )
            } else {
                format!("it is past {MAX_DEPTH} ({DEPENDENCY_MAX_DEPTH} dependency rounds)")
            },
            top.map_or_else(search, install_top),
        ),
        UnresolvedReason::NotRequested => (
            "--no-deps installs only what was named".to_owned(),
            // The same command with its dependencies.
            Suggestion::command(crate::cli::fix_argv(&crate::cli::without_switch(
                argv,
                "--no-deps",
            ))),
        ),
        UnresolvedReason::NotShipped => (
            format!("{} does not ship it for this client", listed.join(", ")),
            search(),
        ),
        UnresolvedReason::UnknownCondition => (
            "its TOC requires it only under a condition omaddons cannot decide offline (a text locale, say)".to_owned(),
            search(),
        ),
    };
    Warning::new(
        WarningCode::MissingDependency,
        format!("{requester} on {id} requires {folder}, which was not installed: {why}"),
    )
    .with_fix(fix)
}

/// The resolver's reasons, as the report says them.
impl From<AmbiguityReason> for UnresolvedReason {
    fn from(reason: AmbiguityReason) -> Self {
        match reason {
            AmbiguityReason::SeveralCatalogEntries => Self::SeveralCatalogEntries,
            AmbiguityReason::WowiAmbiguous => Self::WowiAmbiguous,
            AmbiguityReason::WowiAbandoned => Self::WowiAbandoned,
        }
    }
}

impl From<MissingReason> for UnresolvedReason {
    fn from(reason: MissingReason) -> Self {
        match reason {
            MissingReason::NoSource => Self::NoSource,
            MissingReason::ManagedFolderMissing => Self::ManagedFolderMissing,
        }
    }
}

/// One dependency a round plans: its ref, the name it resolved under, the folder it
/// provides and who requires it.
struct RoundDep {
    r#ref: Ref,
    name: Option<String>,
    folder: FolderName,
    requesters: BTreeSet<String>,
    /// Other folders that resolved to the same ref in this round, each with who requires
    /// it: checked against what the release ships, each reported on its own.
    others: Vec<(FolderName, BTreeSet<String>)>,
    /// From a catalog entry not declaring this client's family: a release that does not
    /// load here is `not_shipped`, no failure.
    undeclared: bool,
}

impl RoundDep {
    /// Every requester, of every folder.
    fn all_requesters(&self) -> BTreeSet<String> {
        let mut all = self.requesters.clone();
        for (_, requesters) in &self.others {
            all.extend(requesters.iter().cloned());
        }
        all
    }

    /// Each folder with its requesters, the first one's first.
    fn needs(&self) -> impl Iterator<Item = (&FolderName, &BTreeSet<String>)> {
        std::iter::once((&self.folder, &self.requesters)).chain(
            self.others
                .iter()
                .map(|(folder, requesters)| (folder, requesters)),
        )
    }
}

/// What a ref the rounds met came to, for a later requester resolving to it.
#[derive(Debug, Clone, Copy)]
enum Settled {
    /// Planned, by a request or a round: a folder it does not provide is not shipped.
    Planned,
    /// Failed to plan: the failure names the requesters that asked first.
    Failed,
    /// Not installed, for this reason (and the limit's name, for `limit_reached`).
    Skipped(UnresolvedReason, Option<&'static str>),
}

/// The dependency rounds' running state on one target.
struct Rounds<'l> {
    lookups: DepLookups<'l>,
    locks: &'l BTreeMap<Ref, String>,
    /// Requesters whose dependencies are collected, by addon key.
    collected: BTreeSet<String>,
    /// Every ref met, by identity.
    settled: BTreeMap<String, Settled>,
    /// The next round's refs, by identity.
    wanted: BTreeMap<String, RoundDep>,
    /// Dependency installs planned so far.
    installed: usize,
}

impl TargetPlanner<'_> {
    /// Dependency rounds (M2-D7..D9): each planned install's needed folders' required
    /// dependencies are satisfied (the client's, its own archive's, a present or planned
    /// folder; a managed or planned owner gains the requester), else resolved and
    /// planned as `dependency` installs from the default channel, one fetch per round,
    /// until nothing new is wanted or a limit is reached. What resolves to nothing is a
    /// `missing_dependency` warning; a dependency that fails to plan is a failure with
    /// `details.required_by`, and the requester still installs.
    fn dependency_rounds(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        locks: &BTreeMap<Ref, String>,
    ) {
        let planned = tp
            .work
            .iter()
            .filter_map(AddonWork::record)
            .map(|r| (r.r#ref.identity(), Settled::Planned));
        let failed = failures
            .iter()
            .filter(|f| f.target == self.target.id)
            .filter_map(|f| f.r#ref.as_ref())
            .map(|r| (r.identity(), Settled::Failed));
        // After the catalog: Tukui's list, then WoWInterface's index, each loaded only
        // when a lookup reaches it.
        let indexes = self.ctx.indexes.map(|indexes| IndexLookups {
            indexes,
            ctx: self.ctx,
        });
        let released = |f: &FolderName| self.view.borrow().released_by(f).is_some();
        let mut rounds = Rounds {
            lookups: DepLookups {
                managed: self.record,
                released: &released,
                catalog: self.ctx.catalog,
                tukui: indexes.as_ref().map(|l| l as &dyn TukuiDirs),
                wowi: indexes.as_ref().map(|l| l as &dyn WowiDirs),
                now: Timestamp::now(),
            },
            locks,
            collected: BTreeSet::new(),
            settled: failed.chain(planned).collect(),
            wanted: BTreeMap::new(),
            installed: 0,
        };
        for round in 1.. {
            self.collect_round(tp, failures, &mut rounds);
            let mut now: Vec<RoundDep> = std::mem::take(&mut rounds.wanted).into_values().collect();
            let (room, limit) = if round > DEPENDENCY_MAX_DEPTH {
                (0, MAX_DEPTH)
            } else {
                let room = DEPENDENCY_MAX_ADDONS.saturating_sub(rounds.installed);
                (room, MAX_ADDONS)
            };
            for over in now.split_off(room.min(now.len())) {
                let reason = UnresolvedReason::LimitReached;
                let skipped = Settled::Skipped(reason, Some(limit));
                rounds.settled.insert(over.r#ref.identity(), skipped);
                self.skip(tp, &over, reason, Some(limit));
            }
            if now.is_empty() {
                break;
            }
            self.plan_round(tp, failures, &mut rounds, now);
        }
    }

    /// Collects the required dependencies of every planned install not collected yet:
    /// the satisfied ones record their requester or a note, the rest resolve into
    /// `rounds.wanted` (or a warning).
    fn collect_round(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        rounds: &mut Rounds<'_>,
    ) {
        type Pending = (String, Vec<FolderName>, Vec<FolderName>, Vec<FolderName>);
        let pending: Vec<Pending> = tp
            .work
            .iter()
            .filter_map(|w| {
                let WorkMode::Folders(f) = &w.mode else {
                    return None;
                };
                let inspected = f.inspected.as_ref()?;
                let archive = inspected.folders.iter().map(|x| x.name.clone()).collect();
                let key = TargetRecord::addon_key(&w.addon);
                (!rounds.collected.contains(&key)).then(|| {
                    (
                        w.addon.clone(),
                        archive,
                        f.deps.requires.clone(),
                        f.deps.unknown_condition.clone(),
                    )
                })
            })
            .collect();
        for (requester, archive, requires, unknown) in pending {
            rounds.collected.insert(TargetRecord::addon_key(&requester));
            for dep in requires {
                self.collect_one(tp, failures, rounds, &requester, &archive, dep);
            }
            // Required under a condition this build cannot read: reported unless
            // something already provides it, never resolved or installed.
            for dep in unknown {
                let satisfied = {
                    let view = self.view.borrow();
                    let plan_view = PlanView {
                        view: &view,
                        live: self.live,
                    };
                    is_satisfied(&dep, &archive, &plan_view)
                };
                if satisfied.is_none() {
                    let u = Unresolved {
                        folder: dep,
                        reason: UnresolvedReason::UnknownCondition,
                        candidates: Vec::new(),
                    };
                    self.unresolved(tp, &requester, u, None);
                }
            }
        }
    }

    /// One required folder `dep` of `requester`, whose own archive is `archive`.
    fn collect_one(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        rounds: &mut Rounds<'_>,
        requester: &str,
        archive: &[FolderName],
        dep: FolderName,
    ) {
        let satisfied = {
            let view = self.view.borrow();
            let plan_view = PlanView {
                view: &view,
                live: self.live,
            };
            is_satisfied(&dep, archive, &plan_view)
        };
        let unresolved = |reason, candidates| Unresolved {
            folder: dep.clone(),
            reason,
            candidates,
        };
        match satisfied {
            Some(Satisfier::Managed(owner) | Satisfier::Planned(owner)) => {
                // ubs:ignore — comparing addon keys, not secrets
                if TargetRecord::addon_key(&owner) != TargetRecord::addon_key(requester) {
                    self.require(tp, failures, &owner, requester);
                }
            }
            Some(Satisfier::Present(FolderClass::Unmanaged)) => {
                let report = tp.dependencies.entry(requester.to_owned()).or_default();
                if !report.unmanaged.contains(&dep) {
                    report.unmanaged.push(dep);
                }
            }
            Some(_) => {}
            None if self.req.no_deps => {
                let u = unresolved(UnresolvedReason::NotRequested, Vec::new());
                self.unresolved(tp, requester, u, None);
            }
            None => self.resolved(tp, rounds, requester, dep),
        }
    }

    /// Plans the resolution of `dep`, a folder `requester` requires that nothing here has.
    fn resolved(
        &self,
        tp: &mut TargetPlan,
        rounds: &mut Rounds<'_>,
        requester: &str,
        dep: FolderName,
    ) {
        let unresolved = |reason, candidates| Unresolved {
            folder: dep.clone(),
            reason,
            candidates,
        };
        let resolution = resolve(&dep, self.target, &rounds.lookups);
        // An index read from what is stored says so (`stale_check`), once per command.
        if let Some(indexes) = self.ctx.indexes {
            let warnings = indexes.take_warnings();
            push_new(&mut tp.warnings, warnings.clone());
            push_new(&mut tp.unchanged_warnings, warnings);
        }
        match resolution {
            DepResolution::Resolved {
                r#ref,
                name,
                undeclared,
                ..
            } => {
                let id = r#ref.identity();
                if let Some(wanted) = rounds.wanted.get_mut(&id) {
                    // The same ref for another folder: that folder is checked and reported
                    // on its own once the ref is planned.
                    if wanted.folder == dep {
                        wanted.requesters.insert(requester.to_owned());
                    } else if let Some((_, requesters)) =
                        wanted.others.iter_mut().find(|(folder, _)| *folder == dep)
                    {
                        requesters.insert(requester.to_owned());
                    } else {
                        wanted
                            .others
                            .push((dep.clone(), BTreeSet::from([requester.to_owned()])));
                    }
                    return;
                }
                match rounds.settled.get(&id) {
                    // Planned already, yet the folder is not there.
                    Some(Settled::Planned) => {
                        let u = unresolved(UnresolvedReason::NotShipped, vec![r#ref]);
                        self.unresolved(tp, requester, u, None);
                    }
                    Some(Settled::Skipped(reason, detail)) => {
                        let u = unresolved(*reason, vec![r#ref]);
                        self.unresolved(tp, requester, u, *detail);
                    }
                    Some(Settled::Failed) => {
                        let report = tp.dependencies.entry(requester.to_owned()).or_default();
                        report.failed.push((dep, r#ref));
                    }
                    None => {
                        let wanted = RoundDep {
                            r#ref,
                            name,
                            folder: dep.clone(),
                            requesters: BTreeSet::from([requester.to_owned()]),
                            others: Vec::new(),
                            undeclared,
                        };
                        rounds.wanted.insert(id, wanted);
                    }
                }
            }
            DepResolution::Ambiguous { candidates, reason } => {
                let top = candidates.iter().take(3).map(|c| c.r#ref.clone()).collect();
                let u = unresolved(reason.into(), top);
                self.unresolved(tp, requester, u, None);
            }
            DepResolution::Missing { reason, owner } => {
                // No source has it: say which could not be consulted.
                let skipped = self.ctx.indexes.and_then(DepIndexes::skipped_note);
                let u = unresolved(reason.into(), Vec::new());
                self.unresolved(tp, requester, u, owner.as_deref().or(skipped.as_deref()));
            }
        }
    }

    /// Plans one round's dependencies after one fetch for all of them.
    fn plan_round(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        rounds: &mut Rounds<'_>,
        now: Vec<RoundDep>,
    ) {
        let ctx = self.ctx;
        let lock = |r: &Ref| {
            rounds
                .locks
                .iter()
                .find(|(k, _)| k.identity() == r.identity())
                .map(|(_, v)| v.clone())
        };
        let wants: Vec<Wanted> = now
            .iter()
            .map(|d| Wanted {
                r#ref: d.r#ref.clone(),
                channel: ctx.default_channel,
                version: lock(&d.r#ref),
            })
            .collect();
        // The round's fetches join the run's progress as the cache asks them (it grows
        // the phase); its downloads and verifications here.
        let answers = ctx.cache.get(&wants);
        ctx.progress.grow(Phase::Download, now.len() as u64);
        ctx.progress.grow(Phase::Verify, now.len() as u64);
        for (dep, w) in now.into_iter().zip(wants) {
            let id = dep.r#ref.identity();
            let set = answers.get(&w).map(|f| f.set.clone());
            match self.plan_dependency(&dep, &w, set) {
                Ok(Ok(planned)) => {
                    claim_work(&mut self.view.borrow_mut(), &planned.work);
                    rounds.installed += 1;
                    let ships = |folder: &FolderName| {
                        planned
                            .work
                            .inspected()
                            .is_some_and(|i| i.folders.iter().any(|f| f.name == *folder))
                    };
                    for (folder, requesters) in dep.needs() {
                        for requester in requesters {
                            if ships(folder) {
                                let report = tp.dependencies.entry(requester.clone()).or_default();
                                report
                                    .installs
                                    .push((folder.clone(), planned.work.addon.clone()));
                            } else {
                                // The release the first folder chose does not ship this
                                // one here.
                                let u = Unresolved {
                                    folder: folder.clone(),
                                    reason: UnresolvedReason::NotShipped,
                                    candidates: vec![dep.r#ref.clone()],
                                };
                                self.unresolved(tp, requester, u, None);
                            }
                        }
                    }
                    rounds.settled.insert(id, Settled::Planned);
                    if let Err(error) = Planned::Change(planned).merge_into(tp, &self.target.id) {
                        self.dependency_failed(tp, failures, &dep, error);
                    }
                }
                Ok(Err(reason)) => {
                    rounds.settled.insert(id, Settled::Skipped(reason, None));
                    self.skip(tp, &dep, reason, None);
                }
                Err(error) => {
                    rounds.settled.insert(id, Settled::Failed);
                    self.dependency_failed(tp, failures, &dep, error);
                }
            }
        }
    }

    /// Plans one dependency install from the default channel (judged without
    /// `--force`), or says why it is not installed: its folder verified out of date
    /// for this client (the guard), or its archive does not ship the folder here.
    fn plan_dependency(
        &self,
        dep: &RoundDep,
        wanted: &Wanted,
        set: Option<Result<ReleaseSet>>,
    ) -> Result<std::result::Result<Box<PlannedAddon>, UnresolvedReason>> {
        let intent = InstallIntent {
            required_by_add: dep.all_requesters(),
            ..InstallIntent::requested(
                self.ctx.default_channel,
                false,
                wanted.version.clone(),
                PinIntent::Keep,
                AddonReason::Dependency,
                false,
            )
        };
        let source = CandidateSource {
            file: None,
            local: None,
            set,
            cached: None,
        };
        let build = match self.prepare_install(dep.name.as_deref(), wanted, source, &intent) {
            Ok(Prepared::Build(build)) => build,
            // Installed and kept as it is, yet the folder is not there.
            Ok(Prepared::Done(_)) => return Ok(Err(UnresolvedReason::NotShipped)),
            // An entry not declaring this client was a guess its TOCs settled.
            Err(e)
                if dep.undeclared
                    && matches!(
                        e.code,
                        ErrorCode::NoCompatibleRelease | ErrorCode::WrongFlavor
                    ) =>
            {
                return Ok(Err(UnresolvedReason::NotShipped));
            }
            Err(e) => return Err(e),
        };
        if build.deps.out_of_date.contains(&dep.folder) {
            return Ok(Err(UnresolvedReason::OutOfDate));
        }
        if !build
            .loadable
            .inspected
            .folders
            .iter()
            .any(|f| f.name == dep.folder)
        {
            return Ok(Err(UnresolvedReason::NotShipped));
        }
        let releases: Vec<(FolderName, String)> = build
            .existing
            .iter()
            .flat_map(|e| build.released().into_iter().map(|f| (f, e.name.clone())))
            .collect();
        {
            let mut view = self.view.borrow_mut();
            for (folder, owner) in &releases {
                view.release(folder, owner);
            }
        }
        let built = self.build(*build);
        // Failed: it lets go of nothing after all.
        if built.is_err() {
            let mut view = self.view.borrow_mut();
            for (folder, owner) in &releases {
                view.withdraw(folder, owner);
            }
        }
        built.map(Box::new).map(Ok)
    }

    /// `dep` is not installed, for `reason`: each of its requesters is told.
    fn skip(
        &self,
        tp: &mut TargetPlan,
        dep: &RoundDep,
        reason: UnresolvedReason,
        detail: Option<&str>,
    ) {
        for (folder, requesters) in dep.needs() {
            for requester in requesters {
                let unresolved = Unresolved {
                    folder: folder.clone(),
                    reason,
                    candidates: vec![dep.r#ref.clone()],
                };
                self.unresolved(tp, requester, unresolved, detail);
            }
        }
    }

    /// The bridge pass, after the last round: each planned install's bridge requirements
    /// that the final working view leaves unsatisfied (not the client's, not its own
    /// archive's, not in AddOns in any class, not installed by this transaction) go on
    /// its report with `first_install`. Nothing is resolved, fetched or installed.
    fn bridges(&self, tp: &mut TargetPlan) {
        let view = self.view.borrow();
        let plan_view = PlanView {
            view: &view,
            live: self.live,
        };
        let mut warnings: Vec<Warning> = Vec::new();
        for w in &tp.work {
            let WorkMode::Folders(f) = &w.mode else {
                continue;
            };
            let (Some(inspected), Some(record)) = (&f.inspected, &f.record) else {
                continue;
            };
            let archive: Vec<FolderName> =
                inspected.folders.iter().map(|x| x.name.clone()).collect();
            let open: Vec<BridgeDependency> = f
                .deps
                .bridges
                .iter()
                .filter(|b| is_satisfied(&b.requires, &archive, &plan_view).is_none())
                .cloned()
                .collect();
            let reported = tp.dependencies.contains_key(&w.addon);
            if open.is_empty() && !reported {
                continue;
            }
            let first_install = !self
                .record
                .addons
                .values()
                .any(|a| a.r#ref.identity() == record.r#ref.identity());
            // Said once, when the addon arrives; an update keeps the item field only.
            if first_install {
                warnings.extend(open.iter().map(|b| self.bridge_warning(b)));
            }
            let report = tp.dependencies.entry(w.addon.clone()).or_default();
            report.bridge_dependencies = open;
            report.first_install = first_install;
        }
        // Tied to no work, so the commit keeps them as it keeps an unchanged addon's.
        push_new(&mut tp.warnings, warnings.clone());
        push_new(&mut tp.unchanged_warnings, warnings);
    }

    /// `bridge_dependency`: a folder of a newly installed addon works with `requires`,
    /// which is not installed. The fix installs it by its catalog name, else by the ref
    /// the resolver finds; with neither, it searches for it.
    fn bridge_warning(&self, bridge: &BridgeDependency) -> Warning {
        let id = self.target.id.as_str();
        let requires = bridge.requires.as_str();
        let install = |arg: String| {
            Suggestion::command([
                "omaddons".to_owned(),
                "install".to_owned(),
                arg,
                "-t".to_owned(),
                id.to_owned(),
            ])
        };
        // A catalog name with variants only where one covers this client's family.
        let catalogued = self
            .ctx
            .catalog
            .resolve(requires, Some(self.target.family))
            .is_some();
        let fix = if catalogued {
            install(requires.to_owned())
        } else {
            let indexes = self.ctx.indexes.map(|indexes| IndexLookups {
                indexes,
                ctx: self.ctx,
            });
            let released = |f: &FolderName| self.view.borrow().released_by(f).is_some();
            let lookups = DepLookups {
                managed: self.record,
                released: &released,
                catalog: self.ctx.catalog,
                tukui: indexes.as_ref().map(|l| l as &dyn TukuiDirs),
                wowi: indexes.as_ref().map(|l| l as &dyn WowiDirs),
                now: Timestamp::now(),
            };
            match resolve(&bridge.requires, self.target, &lookups) {
                DepResolution::Resolved { r#ref, .. } => install(r#ref.to_string()),
                _ => Suggestion::command(["omaddons", "search", requires]),
            }
        };
        Warning::new(
            WarningCode::BridgeDependency,
            format!(
                "{} works with {requires}, which is not installed on {id}",
                bridge.folder.as_str()
            ),
        )
        .with_fix(fix)
    }

    /// `owner`'s record gains `requester` in `required_by` (folded into its work when
    /// it has one; nothing when it already lists it).
    fn require(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        owner: &str,
        requester: &str,
    ) {
        let edit = RecordEdit::AddRequiredBy(requester.to_owned());
        self.bookkeep(tp, failures, owner, &edit);
    }

    /// Keeps `required_by` true once the requests and dependency rounds are planned
    /// (C6): a record listing an addon this transaction removes drops it, and one listing
    /// an updated requester whose new release's needed folders no longer require any of
    /// its folders drops that requester. A record the transaction removes gets nothing;
    /// a requester with no work (current, pinned) changes nothing. Planned under
    /// `--no-deps` too: bookkeeping installs nothing.
    fn unrequire(&self, tp: &mut TargetPlan, failures: &mut Vec<PlanFailure>) {
        let key = |name: &str| TargetRecord::addon_key(name);
        let removed: BTreeSet<String> = tp
            .work
            .iter()
            .filter(|w| w.record().is_none())
            .map(|w| key(&w.addon))
            .collect();
        // An owner's folders as its own work leaves them, else as recorded.
        let folders_of = |owner: &AddonRecord| -> Vec<FolderName> {
            tp.work
                .iter()
                .find(|w| key(&w.addon) == key(&owner.name))
                .and_then(AddonWork::record)
                .map_or_else(|| owner.folders.clone(), |r| r.folders.clone())
        };
        let mut drops: Vec<(String, String)> = Vec::new();
        for w in &tp.work {
            let requester = key(&w.addon);
            // `None` for a removed requester: it requires nothing any more.
            let requires: Option<&[FolderName]> = match &w.mode {
                _ if removed.contains(&requester) => None,
                WorkMode::Folders(f)
                    if f.inspected.is_some() && self.record.addons.contains_key(&requester) =>
                {
                    Some(&f.deps.requires)
                }
                _ => continue,
            };
            for owner in self.record.addons.values() {
                let owner_key = key(&owner.name);
                if owner_key == requester
                    || removed.contains(&owner_key)
                    || !owner.required_by.iter().any(|n| key(n) == requester)
                {
                    continue;
                }
                let still = requires
                    .is_some_and(|requires| folders_of(owner).iter().any(|f| requires.contains(f)));
                if !still {
                    drops.push((owner.name.clone(), w.addon.clone()));
                }
            }
        }
        for (owner, requester) in drops {
            self.bookkeep(
                tp,
                failures,
                &owner,
                &RecordEdit::RemoveRequiredBy(requester),
            );
        }
    }

    /// Plans a `required_by` `edit` of `owner`, folded into its work when it has one. A
    /// failure is reported under the owner's ref.
    fn bookkeep(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        owner: &str,
        edit: &RecordEdit,
    ) {
        let planned = self.edit(owner, edit, &tp.work);
        if let Err(error) = planned.and_then(|p| p.merge_into(tp, &self.target.id)) {
            // Under the owner's ref: a failure without one is the target's.
            let key = TargetRecord::addon_key(owner);
            let r#ref = tp
                .work
                .iter()
                .filter_map(AddonWork::record)
                .chain(self.record.addons.values())
                .find(|r| TargetRecord::addon_key(&r.name) == key)
                .map(|r| r.r#ref.clone());
            failures.push(failure(r#ref, Some(owner.to_owned()), self.target, error));
        }
    }

    /// Records `unresolved` for `requester` and warns.
    fn unresolved(
        &self,
        tp: &mut TargetPlan,
        requester: &str,
        unresolved: Unresolved,
        detail: Option<&str>,
    ) {
        // Tied to no work, so the commit keeps it as it keeps an unchanged addon's.
        let warning =
            missing_dependency(self.target, requester, &unresolved, detail, self.ctx.argv);
        push_new(&mut tp.warnings, vec![warning.clone()]);
        push_new(&mut tp.unchanged_warnings, vec![warning]);
        let report = tp.dependencies.entry(requester.to_owned()).or_default();
        if !report.unresolved.contains(&unresolved) {
            report.unresolved.push(unresolved);
        }
    }

    /// A resolved dependency that could not be planned: a failure under its ref, naming
    /// who requires it.
    fn dependency_failed(
        &self,
        tp: &mut TargetPlan,
        failures: &mut Vec<PlanFailure>,
        dep: &RoundDep,
        error: Error,
    ) {
        let requesters: Vec<String> = dep.all_requesters().into_iter().collect();
        let mut details = error.details.clone();
        match details.as_object_mut() {
            Some(map) => {
                map.insert("required_by".into(), json!(requesters));
            }
            None => details = json!({ "required_by": requesters }),
        }
        let error = error.details(details);
        for (folder, requesters) in dep.needs() {
            for requester in requesters {
                tp.dependencies
                    .entry(requester.clone())
                    .or_default()
                    .failed
                    .push((folder.clone(), dep.r#ref.clone()));
            }
        }
        failures.push(failure(Some(dep.r#ref.clone()), None, self.target, error));
    }
}

/// One request of a target after the first planning pass.
enum Step {
    /// Settled (pinned, kept, failed): (ref, addon, result).
    Ready(Option<Ref>, Option<String>, Result<Planned>),
    /// A verified install to judge.
    Build(Option<Ref>, Option<String>, Box<PendingBuild>),
    Remove(String),
    Edit(String, RecordEdit),
}

/// Claims in `view` the folders `work` installs, under its addon.
fn claim_work(view: &mut WorkingView, work: &AddonWork) {
    for op in work.swap() {
        match op {
            SwapOp::Add { name } | SwapOp::Replace { name, .. } => {
                view.claim(name.as_folder(), &work.addon);
            }
            SwapOp::Remove { .. } => {}
        }
    }
}

/// Releases in `view` the folders `work` stops shipping, under its addon.
fn release_work(view: &mut WorkingView, work: &AddonWork) {
    if let WorkMode::Folders(f) = &work.mode {
        for op in &f.swap {
            if let SwapOp::Remove { live, .. } = op {
                view.release(live.as_folder(), &work.addon);
            }
        }
        for folder in &f.handed_over {
            view.release(folder, &work.addon);
        }
    }
}

/// Merges each handover (M2-D6): the claimant's replacement takes the folder, so the
/// releasing addon's removal of it goes from its swap ops, its planned ops and its
/// change's folder summary, and is remembered as handed over.
pub fn settle_handovers(
    works: &mut [AddonWork],
    changes: &mut [Change],
    mut ops: Option<&mut Vec<PlannedOp>>,
) {
    let handovers: Vec<(FolderName, String)> = works
        .iter()
        .flat_map(|w| w.transfers().iter())
        .filter(|t| t.handover)
        .map(|t| (t.folder.clone(), TargetRecord::addon_key(&t.from)))
        .collect();
    for (folder, from) in handovers {
        let gone = |key: &str| key == folder.key();
        for w in works
            .iter_mut()
            .filter(|w| TargetRecord::addon_key(&w.addon) == from)
        {
            if let WorkMode::Folders(f) = &mut w.mode {
                let before = f.swap.len();
                f.swap
                    .retain(|op| !matches!(op, SwapOp::Remove { live, .. } if gone(live.key())));
                if f.swap.len() != before && !f.handed_over.contains(&folder) {
                    f.handed_over.push(folder.clone());
                }
            }
        }
        if let Some(ops) = ops.as_deref_mut() {
            ops.retain(|o| {
                !(o.op == OpKind::Remove
                    && gone(o.folder.key())
                    && TargetRecord::addon_key(&o.addon) == from)
            });
        }
        for c in changes
            .iter_mut()
            .filter(|c| TargetRecord::addon_key(&c.addon) == from)
        {
            c.folders.removed.retain(|f| !gone(f.key()));
        }
    }
}

/// A target as it is now: its record, files index and AddOns inventory.
#[derive(Debug, Clone)]
pub struct TargetState {
    pub record: TargetRecord,
    pub index: Option<FilesIndex>,
    pub live: Inventory,
}

impl TargetState {
    /// Reads `target` now.
    pub fn now(ctx: &PlanCtx<'_>, target: &Target) -> Result<Self> {
        let (record, index) = target_state(ctx.paths, target)?;
        let live = live_inventory(ctx, target, &record)?;
        Ok(Self {
            record,
            index,
            live,
        })
    }
}

/// `target`'s inventory under `record`, with the records of the clients sharing its
/// AddOns directory: a folder only they record is `shared`, never ours to take. Under
/// the lock this reads their records as they are now.
fn live_inventory(ctx: &PlanCtx<'_>, target: &Target, record: &TargetRecord) -> Result<Inventory> {
    let shared = sibling_records(ctx.paths, target, ctx.known_targets)?;
    inventory_shared(target, Some(record), &shared, ctx.table, Depth::Classes)
}

/// The view a replay of `works` starts from: every folder their addons stop shipping.
pub fn released_view(works: &[AddonWork]) -> WorkingView {
    let mut view = WorkingView::default();
    for w in works {
        release_work(&mut view, w);
    }
    view
}

/// Claims `work`'s folders in `view` (the replay's step after each rejudged work).
pub fn claim(view: &mut WorkingView, work: &AddonWork) {
    claim_work(view, work);
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

/// The newest release of a ref judged on a target as installing it would, without
/// ownership, the running policy, staging or the lock: adoption's exactness check.
#[derive(Debug, Clone)]
pub struct ReleaseCheck {
    pub release_id: String,
    pub version: String,
    /// The folders the client loads, as inspected (each with its files).
    pub install: Vec<crate::engine::inspect::InspectedFolder>,
    /// The archive's folders the client does not load.
    pub skipped: Vec<SkippedFolder>,
    /// The archive's SHA-256; `None` for a directory.
    pub blob_sha256: Option<String>,
}

/// What checking one release found.
#[derive(Debug, Clone)]
pub enum Checked {
    /// A candidate loads here.
    Loads(ReleaseCheck),
    /// None loads here, or there is none: why.
    Refused(String),
}

/// What [`check_release`] learned beside its answer.
#[derive(Debug, Default)]
pub struct CheckNotes {
    /// Candidate facts, for the release memo.
    pub facts: Vec<CandidateFact>,
    /// `-v` notes from downloading.
    pub notes: Vec<String>,
    /// `download_unverified` for each release checked from an unchecked download.
    pub warnings: Vec<Warning>,
}

/// Selects `set`'s release for `wanted` on `target` and tries its candidates in order,
/// downloading, inspecting and verifying each as an install would ([`TargetPlanner`]'s
/// candidate step), until one loads. Judges no ownership, policy or dependencies and
/// writes nothing but downloads. `Err` when no candidate could even be read and none was
/// judged.
pub fn check_release(
    ctx: &PlanCtx<'_>,
    target: &Target,
    wanted: &Wanted,
    set: &ReleaseSet,
    name_hint: Option<&str>,
    out: &mut CheckNotes,
) -> Result<Checked> {
    let req = PlanRequest {
        targets: Vec::new(),
        force: false,
        while_running: true,
        dry_run: true,
        kind: None,
        no_deps: true,
        witness: None,
    };
    let record = TargetRecord::empty(TargetKey::from_ulid(ulid::Ulid::nil()));
    let live = Inventory {
        addons_dir_missing: false,
        folders: Vec::new(),
        missing: Vec::new(),
        counts: crate::inventory::Counts::default(),
        stray_files: 0,
    };
    let planner = TargetPlanner {
        req: &req,
        ctx,
        target,
        record: &record,
        index: None,
        live: &live,
        created_at: Timestamp::now(),
        facts: RefCell::new(Vec::new()),
        candidate_items: 1,
        view: RefCell::new(WorkingView::default()),
        notes: RefCell::new(Vec::new()),
    };
    let selection = select(set, target, wanted.channel, wanted.version.as_deref());
    let result = match selection.empty_reason {
        Some(reason) => Ok(Checked::Refused(reason_text(reason).to_owned())),
        None => {
            let mut refused: Vec<String> = Vec::new();
            let mut unreadable: Option<Error> = None;
            // As install chooses: the first clean candidate, else the first that leaves
            // out folders built for other clients.
            let mut clean = None;
            let mut left_out = None;
            for candidate in &selection.candidates {
                match planner.try_candidate(&wanted.r#ref, candidate, name_hint, None, false) {
                    Ok(Ok(winner)) if winner.grade == Grade::Clean => {
                        clean = Some(winner);
                        break;
                    }
                    Ok(Ok(winner)) => {
                        left_out.get_or_insert(winner);
                    }
                    Ok(Err(refusal)) => {
                        refused.push(format!("{}: {}", candidate.asset.filename, refusal.reason))
                    }
                    Err(error) => {
                        unreadable.get_or_insert(error);
                    }
                }
            }
            let found = clean.or(left_out).map(|winner| {
                let unverified = winner
                    .warnings
                    .iter()
                    .filter(|w| matches!(w.code, WarningCode::DownloadUnverified));
                push_new(&mut out.warnings, unverified.cloned().collect());
                ReleaseCheck {
                    release_id: winner.candidate.release_id,
                    version: winner.candidate.version,
                    blob_sha256: winner.loadable.inspected.blob_sha256,
                    install: winner.loadable.inspected.folders,
                    skipped: winner.loadable.skipped,
                }
            });
            match (found, unreadable) {
                (Some(check), _) => Ok(Checked::Loads(check)),
                (None, Some(error)) if refused.is_empty() => Err(error),
                (None, _) => Ok(Checked::Refused(if refused.is_empty() {
                    "no candidate loads on this client".to_owned()
                } else {
                    refused.join("; ")
                })),
            }
        }
    };
    out.facts.extend(planner.facts.into_inner());
    out.notes.extend(planner.notes.into_inner());
    result
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
///
/// The works of a target are replayed in planning order over one [`TargetState`], read
/// once under the lock, and one `view` of what the earlier works claim and release, so a
/// handover is judged again and a concurrent transaction is seen the same way.
pub fn rejudge(
    req: &PlanRequest,
    ctx: &PlanCtx<'_>,
    target: &Target,
    state: &TargetState,
    work: &AddonWork,
    generation: u64,
    view: &WorkingView,
) -> Result<Planned> {
    let record = &state.record;
    let planner = TargetPlanner {
        req,
        ctx,
        target,
        record,
        index: state.index.as_ref(),
        live: &state.live,
        created_at: Timestamp::now(),
        // Re-judging downloads nothing, so it learns and reports nothing.
        facts: RefCell::new(Vec::new()),
        candidate_items: 0,
        view: RefCell::new(view.clone()),
        notes: RefCell::new(Vec::new()),
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
                // A reinstall planned against a release the record has left (a
                // concurrent update): refused, never rolled back (`plan_stale` with the
                // generation moved).
                if let Some(planned) = &intent.reinstall_of
                    && existing.release_id != *planned
                {
                    return Err(Error::new(
                        ErrorCode::NotFound,
                        format!(
                            "{} is now at {}, not the release this reinstall was planned for",
                            existing.name, existing.version
                        ),
                        Suggestion::command(["omaddons", "list", "-t", target.id.as_str()]),
                    ));
                }
                // Installed meanwhile: its pin, channel or reason difference still
                // records, as a records-only change. A reinstall installs again.
                if intent.keeps_release()
                    || (!intent.reinstall
                        && existing.release_id == template.release_id
                        && existing.revision == template.revision
                        && planner.all_present(existing))
                {
                    return planner.current(existing, intent);
                }
            }
            // The archive was checked once, at plan time: its warnings come first, as
            // planning gave them.
            let mut rebuilt = planner.build_install(
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
                folders.archive_warnings.clone(),
            )?;
            if let WorkMode::Folders(f) = &mut rebuilt.work.mode {
                f.deps = folders.deps.clone();
                // What planning found changed, while the work still replaces it.
                let replaced = |folder: &FolderName| {
                    f.swap.iter().any(
                        |op| matches!(op, SwapOp::Replace { live, .. } if live.key() == folder.key()),
                    )
                };
                f.local_changes = folders
                    .local_changes
                    .clone()
                    .filter(|c| c.folders.iter().any(replaced));
                f.archive_warnings = folders.archive_warnings.clone();
            }
            Ok(Planned::change(rebuilt))
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
/// `change` failed with `error`, its target's state unreadable: under its own ref and
/// addon, as an item failure.
fn unreadable_want(change: &RequestedChange, error: &Error) -> Want {
    let (r#ref, addon) = match change {
        RequestedChange::Install { r#ref, name, .. } => (Some(r#ref.clone()), name.clone()),
        RequestedChange::Remove { addon }
        | RequestedChange::Update { addon, .. }
        | RequestedChange::Edit { addon, .. } => (None, Some(addon.clone())),
    };
    Want::Failed {
        r#ref,
        addon,
        error: error.clone(),
    }
}

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
            blob_sha256,
            asset,
            revision,
            any_flavor,
        } => {
            // A reinstall without a version asks for the recorded release exactly; the
            // intent keeps no version, so the pin stays as recorded.
            let installed = record.addons.values().find(|a| a.r#ref == *r#ref);
            let exact_of = installed.filter(|_| *reinstall && version.is_none());
            let exact = exact_of.and_then(exact_version);
            let revision = match &exact {
                Some(_) => exact_of.and_then(|a| a.revision),
                None => revision.filter(|_| version.is_some()),
            };
            Want::Install {
                name: name.clone(),
                wanted: Wanted {
                    r#ref: r#ref.clone(),
                    channel: *channel,
                    version: version.clone().or(exact),
                },
                file: file.clone(),
                intent: Box::new(InstallIntent {
                    asset: asset.clone(),
                    any_flavor: *any_flavor,
                    ..InstallIntent::requested(
                        *channel,
                        *channel_explicit,
                        version.clone(),
                        *pin,
                        *reason,
                        *reinstall,
                    )
                }),
                local: None,
                cached: blob_sha256.clone(),
                revision,
            }
        }
        RequestedChange::Remove { addon } => Want::Remove {
            addon: addon.clone(),
        },
        RequestedChange::Edit { addon, edit } => Want::Edit {
            addon: addon.clone(),
            edit: edit.clone(),
        },
        RequestedChange::Update {
            addon,
            installed_primary_interface,
        } => {
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
                    intent: Box::new(InstallIntent {
                        floor_interface: *installed_primary_interface,
                        ..InstallIntent::requested(
                            installed.channel,
                            false,
                            None,
                            PinIntent::Keep,
                            installed.reason,
                            false,
                        )
                    }),
                    local: None,
                    cached: None,
                    revision: None,
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
        .is_some_and(|e| {
            intent.pinned_noop(e) || intent.keeps_release() || intent.pinned_setting_only(e)
        })
}

/// A local archive or directory (`file:`): a newer one of the same addon replaces it.
fn is_local(r: &Ref) -> bool {
    matches!(r, Ref::File { .. })
}

/// The release has no asset `named` (a pasted asset link): `not_found`, naming the assets
/// it has, fixed by installing that release and letting the client's TOCs choose.
fn asset_not_found(wanted: &Wanted, release: Option<&crate::model::Release>, named: &str) -> Error {
    let assets: Vec<&str> = release
        .into_iter()
        .flat_map(|r| &r.assets)
        .map(|a| a.filename.as_str())
        .collect();
    let version = release.map_or("its newest release", |r| r.version.as_str());
    let mut fix = vec![
        "omaddons".to_owned(),
        "install".to_owned(),
        wanted.r#ref.to_string(),
    ];
    if let Some(r) = release {
        fix.extend(["--version".to_owned(), r.version.clone()]);
    }
    Error::new(
        ErrorCode::NotFound,
        format!(
            "{} {version} has no asset {named}; it has {}",
            wanted.r#ref,
            if assets.is_empty() {
                "none".to_owned()
            } else {
                assets.join(", ")
            }
        ),
        Suggestion::command(fix),
    )
    .details(json!({ "asset": named, "assets": assets }))
}

/// Installing `new` would replace `other`, an addon of the same name from another ref:
/// `ownership_conflict`, fixed by the same command with `--force`, or by removing it.
fn takeover_refused(other: &AddonRecord, new: &Ref, argv: &[String], target: &str) -> Error {
    // Runnable as is: without the failing run's presentation flags.
    let mut forced = crate::cli::fix_argv(argv);
    forced.push("--force".to_owned());
    let folders: Vec<Value> = other
        .folders
        .iter()
        .map(|f| {
            json!({
                "folder": f.as_str(),
                "code": ErrorCode::OwnershipConflict.wire(),
                "reason": "same_name_other_ref",
                "owner": other.name,
                "ref": other.r#ref.to_string(),
            })
        })
        .collect();
    Error::new(
        ErrorCode::OwnershipConflict,
        format!(
            "{} on {target} comes from {}; installing {new} would replace it",
            other.name, other.r#ref
        ),
        Suggestion::command(forced),
    )
    .alternative(Suggestion::command([
        "omaddons",
        "remove",
        other.name.as_str(),
        "-t",
        target,
    ]))
    .details(json!({ "folders": folders }))
}

/// `ownership_transferred` for a forced install of `new` over `other`, an addon of the
/// same name from another ref: it now comes from `new` (undo brings `other` back).
fn takeover_warning(other: &AddonRecord, new: &Ref, target: &str) -> Warning {
    Warning::new(
        WarningCode::OwnershipTransferred,
        format!(
            "{} on {target} now comes from {new} instead of {}",
            other.name, other.r#ref
        ),
    )
    .with_fix(Suggestion::command([
        "omaddons",
        "list",
        "-t",
        target,
        "--verbose",
    ]))
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
    /// What the works judged so far in this transaction claim and release (M2-D6).
    view: RefCell<WorkingView>,
    /// `-v` notes from downloading (a header hash that could not be checked).
    notes: RefCell<Vec<String>>,
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
    /// [`Grade::Clean`], or [`Grade::LeftOut`] with a `folder_left_out` warning per
    /// folder left out.
    grade: Grade,
    loadable: Loadable,
    /// The name a new addon takes from the archive.
    name: String,
    blob: Option<PathBuf>,
    /// A deferred asset's stable metadata URL, which plans show instead of its token.
    canonical_url: Option<String>,
    warnings: Vec<Warning>,
    deps: DepFacts,
}

/// A candidate that does not install here.
struct Refusal {
    /// Its `tried[]` reason.
    reason: String,
    /// Why, in words, for the refusal's message.
    why: String,
    /// `--any-flavor` would lift it: evidence against, or folders built for other clients
    /// only (never a game-type gate, TOCs the client never reads, or the floor).
    overridable: bool,
    /// It does not load on this client (`wrong_flavor`), so an older release may: what a
    /// GitHub look-back needs. Never for a packaging refusal or an adopted build's floor.
    wrong_flavor: bool,
}

impl Refusal {
    /// A refusal whose `tried[]` reason (`<code>: <text>`) also says it in words.
    fn new(reason: String) -> Self {
        let why = reason
            .split_once(": ")
            .map_or(reason.as_str(), |(_, text)| text)
            .to_owned();
        Self {
            reason,
            why,
            overridable: false,
            wrong_flavor: false,
        }
    }
}

impl WrongFlavorReason {
    /// A folder `--any-flavor` installs anyway, as out of date: one whose TOC the client
    /// reads but whose interfaces are off its range or missing.
    fn admitted_by_any_flavor(&self) -> bool {
        matches!(self, Self::InterfaceNotInFamily { .. } | Self::NoInterface)
    }
}

/// An inspected archive cut down to the folders the target's client loads, and the
/// folders left out.
struct Loadable {
    inspected: Inspected,
    skipped: Vec<SkippedFolder>,
}

/// Where an install's candidates come from, first match wins: the `--file` archive, a
/// reinstall's cached archive, else the release set the source answered.
struct CandidateSource<'a> {
    file: Option<&'a std::path::Path>,
    local: Option<&'a Candidate>,
    set: Option<Result<ReleaseSet>>,
    /// A locked install's recorded archive hash: the set's release is tried from the
    /// blob cache first, when the cache still holds that archive.
    cached: Option<&'a str>,
}

/// An install after its first pass: settled, or a verified winner still to judge
/// against the working view.
enum Prepared {
    Done(Planned),
    Build(Box<PendingBuild>),
}

/// A verified release of an addon, not yet judged for ownership and policy.
struct PendingBuild {
    existing: Option<AddonRecord>,
    template: AddonRecord,
    intent: InstallIntent,
    loadable: Loadable,
    blob: Option<PathBuf>,
    release: Option<PlannedRelease>,
    warnings: Vec<Warning>,
    deps: DepFacts,
    input: Option<InputWitness>,
}

impl PendingBuild {
    /// The installed folders this release no longer ships (an update's drops).
    fn released(&self) -> Vec<FolderName> {
        let new: Vec<&FolderName> = self
            .loadable
            .inspected
            .folders
            .iter()
            .map(|f| &f.name)
            .collect();
        self.existing
            .iter()
            .flat_map(|e| &e.folders)
            .filter(|f| !new.contains(f))
            .cloned()
            .collect()
    }
}

impl TargetPlanner<'_> {
    /// The first pass of an install: resolve, select, download, inspect and verify,
    /// without judging ownership.
    fn prepare_install(
        &self,
        name: Option<&str>,
        wanted: &Wanted,
        source: CandidateSource<'_>,
        intent: &InstallIntent,
    ) -> Result<Prepared> {
        one_addons_dir(&ClientPaths::new(&self.target.client_dir))?;
        let CandidateSource {
            file,
            local,
            set,
            cached,
        } = source;
        let existing = self
            .record
            .addons
            .values()
            .find(|a| a.r#ref == wanted.r#ref);
        // Before any candidate is looked at: these never move an installed release.
        match existing {
            Some(existing) => {
                if intent.pinned_noop(existing) {
                    return Ok(Prepared::Done(Planned::pinned(existing, &self.target.id)));
                }
                if intent.keeps_release() || intent.pinned_setting_only(existing) {
                    return self.current(existing, intent).map(Prepared::Done);
                }
            }
            // Only what is installed can be installed again.
            None if intent.reinstall => {
                return Err(super::edit::not_managed(
                    &wanted.r#ref.to_string(),
                    &self.target.id,
                ));
            }
            None => {}
        }
        // The release a reinstall repairs, for the race check under the lock.
        let intent = &InstallIntent {
            reinstall_of: existing
                .filter(|_| intent.reinstall)
                .map(|e| e.release_id.clone()),
            ..intent.clone()
        };
        // The source's name for the addon (WoWInterface's `UIName`), when no catalog
        // name applies.
        let mut source_name: Option<String> = None;
        // The tag of the release the candidates come from, for a look-back, and whether
        // its own `release.json` declares a build for this client (then a refusal is of
        // that build, and an older release would only fail the same way).
        let mut newest_tag: Option<String> = None;
        let mut newest_declares = false;
        let candidates = match (file, local, set) {
            (Some(file), _, _) => vec![file_candidate(file, wanted)],
            (None, Some(local), _) => vec![local.clone()],
            (None, None, Some(set)) => {
                let set = set.map_err(|e| self.not_cached(e, wanted, intent))?;
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
                    None => {
                        let release = selection.candidates.first().and_then(|c| {
                            // ubs:ignore — matching release ids, not secrets
                            set.releases.iter().find(|r| r.release_id == c.release_id)
                        });
                        source_name = release.and_then(|r| r.name_hint.clone());
                        newest_tag = release.and_then(|r| tag_of(wanted, &r.release_id));
                        let family = self.target.family;
                        let range = self.ctx.table.family_range(family);
                        newest_declares = release.is_some_and(|r| {
                            r.assets
                                .iter()
                                .flat_map(|a| &a.flavors)
                                .any(|f| declares_client(f, family, range))
                        });
                        let mut candidates = selection.candidates;
                        // A named asset (a pasted asset link): that one or nothing; the
                        // TOCs still judge whether it loads here.
                        if let Some(named) = &intent.asset {
                            candidates.retain(|c| c.asset.filename == *named);
                            if candidates.is_empty() {
                                return Err(asset_not_found(wanted, release, named));
                            }
                        }
                        candidates
                    }
                }
            }
            (None, None, None) => {
                return Err(Error::internal(format!("{} was not fetched", wanted.r#ref)));
            }
        };
        if let (Some(existing), Some(first)) = (existing, candidates.first())
            && !intent.reinstall
            && existing
                .release_id
                .as_deref()
                .is_some_and(|id| crate::resolve::updates::same_release(id, &first.release_id))
            // A re-tagged WowAce tag keeps its release id at another revision.
            && existing.revision == first.revision
            && self.all_present(existing)
        {
            return self.current(existing, intent).map(Prepared::Done);
        }
        // A locked install whose recorded archive the cache still holds: the selected
        // release from there first, with no download (its candidates follow).
        let mut candidates = candidates;
        let mut cached_first = false;
        if let (None, None, Some(sha), Some(first)) = (file, local, cached, candidates.first())
            && let Some(from_cache) = local_blob_candidate(
                self.ctx.blobs,
                self.ctx.scopes,
                &first.release_id,
                &first.version,
                first.channel,
                sha,
                first.revision,
            )
        {
            candidates.insert(0, from_cache);
            cached_first = true;
        }
        // A WowAce release recorded at a revision is that tree whatever its rank: the
        // source could only answer the tag as it is now.
        let recorded_tree = matches!(wanted.r#ref, Ref::WowAce { .. })
            && local.is_some_and(|l| l.revision.is_some());
        // A cached archive carries no rank (a reinstall's, a locked sync's): it wins only
        // clean, so the source's metadata weighs a build that leaves folders out.
        let rankless = |at: usize| (local.is_some() && !recorded_tree) || (cached_first && at == 0);

        let user_supplied =
            file.is_some() || matches!(wanted.r#ref, Ref::File { .. } | Ref::Url { .. });
        let mut tried: Vec<Value> = Vec::new();
        let mut winner = None;
        // The first candidate that leaves out folders built for other clients: it wins
        // when no clean candidate follows.
        let mut left_out = None;
        let mut unreadable: Option<Error> = None;
        // Some candidate was judged and does not load (as opposed to failing to arrive).
        let mut refused = false;
        // Some candidate does not load on this client at all (not merely older than an
        // adopted build): an older release may, so a GitHub look-back is worth it.
        let mut wrong_flavor = false;
        // Why the best-ranked candidate judged did not install: (its file, why).
        let mut first_why: Option<(String, String)> = None;
        let hint = name.or(source_name.as_deref());
        // `--any-flavor` on this addon here (a dependency is judged without it).
        let any_flavor = intent.any_flavor.applies(existing) && !intent.from_resolver();
        // A refusal the setting would lift, so the error can offer it.
        let mut overridable = false;
        for (at, candidate) in candidates.into_iter().enumerate() {
            let tried_here = self.try_candidate(
                &wanted.r#ref,
                &candidate,
                hint,
                intent.floor_interface,
                any_flavor,
            );
            match tried_here {
                Ok(Ok(won)) if won.grade == Grade::Clean => {
                    winner = Some(won);
                    break;
                }
                Ok(Ok(won)) => {
                    if !rankless(at) && left_out.is_none() {
                        left_out = Some(won);
                    }
                }
                Ok(Err(refusal)) => {
                    refused = true;
                    overridable |= refusal.overridable;
                    wrong_flavor |= refusal.wrong_flavor;
                    first_why.get_or_insert((candidate.asset.filename.clone(), refusal.why));
                    tried.push(
                        json!({ "asset": candidate.asset.filename, "reason": refusal.reason }),
                    );
                }
                Err(error) => {
                    let reason = format!("{}: {}", error.code.wire(), error.message);
                    tried.push(json!({ "asset": candidate.asset.filename, "reason": reason }));
                    unreadable.get_or_insert(error);
                }
            }
        }
        let Some(winner) = winner.or(left_out) else {
            // A reinstall's cached archive carries no rank, so a release that loads only
            // as its publisher declares it, or only leaving folders out, is refused: ask
            // the source for the exact release (its blob is still found through the
            // facts). Offline, the memo answers or the refusal stands as `offline`.
            if local.is_some() && !recorded_tree {
                let fetched = self.ctx.cache.get(std::slice::from_ref(wanted));
                let source = CandidateSource {
                    file: None,
                    local: None,
                    set: fetched.get(wanted).map(|f| f.set.clone()),
                    cached: None,
                };
                return self.prepare_install(name, wanted, source, intent);
            }
            if user_supplied {
                // The user's own archive: why it cannot be read, else it does not load.
                if let Some(error) = unreadable {
                    return Err(error);
                }
                let why = first_why
                    .as_ref()
                    .map(|(_, why)| format!(": {why}"))
                    .unwrap_or_default();
                let error = Error::new(
                    ErrorCode::WrongFlavor,
                    format!("{} does not load on {} ({:?}){why}", wanted.r#ref, self.target.id, self.target.family),
                    Suggestion::text("use an archive built for this client"),
                )
                .details(json!({ "ref": wanted.r#ref.to_string(), "target": self.target.id, "tried": tried }));
                return Err(self.offer_any_flavor(error, wanted, overridable));
            }
            // A repository that publishes one release per client: its newest may be
            // another client's, so look back for the newest that declares this one. Not
            // when the newest declares this client already: its build does not load.
            if wrong_flavor
                && !newest_declares
                && intent.looked_back.is_none()
                && intent.asset.is_none()
                && wanted.version.is_none()
                && let Some(newest) = &newest_tag
                && let Some(looked) = self.look_back(wanted, newest)
            {
                match looked {
                    Ok(look_back) => match look_back.found {
                        Some(found) => {
                            return self.install_looked_back(
                                name,
                                wanted,
                                existing,
                                found,
                                &look_back.order,
                                newest,
                                intent,
                            );
                        }
                        None => tried.extend(look_back.ruled_out.iter().map(|(tag, why)| {
                            json!({ "asset": format!("{tag}/release.json"), "reason": format!("look_back: {why}") })
                        })),
                    },
                    // A look-back that could not be made, as a download that failed is.
                    Err(error) => tried.push(json!({
                        "asset": "look_back",
                        "reason": format!("{}: {}", error.code.wire(), error.message),
                    })),
                }
            }
            // Every candidate failed its published hash and none was judged: the
            // download is the problem, not the client (retryable as the download left it).
            if !refused
                && let Some(error) = unreadable
                    .as_ref()
                    .filter(|e| e.code == ErrorCode::HashMismatch)
            {
                let mut details = error.details.clone();
                if let Some(map) = details.as_object_mut() {
                    map.insert("tried".into(), json!(tried));
                }
                return Err(error.clone().details(details));
            }
            // A release that does not exist at all (a git rev naming nothing), or one
            // whose archive cannot be downloaded offline, is not a question of flavor;
            // nor is a WowAce tree that cannot be assembled (a collision, a bound).
            let tree = matches!(wanted.r#ref, Ref::WowAce { .. });
            if let Some(error) = unreadable.filter(|e| {
                matches!(e.code, ErrorCode::NotFound | ErrorCode::Offline)
                    || (tree
                        && matches!(e.code, ErrorCode::BadArchive | ErrorCode::ArchiveTooLarge))
            }) {
                return Err(error);
            }
            let why = match first_why {
                Some((file, why)) => format!("no candidate loads on this client: {file}: {why}"),
                None => "no candidate loads on this client".to_owned(),
            };
            let error = self.no_release(wanted, &tried, &why);
            return Err(self.offer_any_flavor(error, wanted, overridable));
        };

        let mut winner = winner;
        if let Some(looked) = &intent.looked_back {
            let tag = tag_of(wanted, &winner.candidate.release_id)
                .unwrap_or_else(|| winner.candidate.version.clone());
            let date = looked
                .published
                .map(|at| format!(" ({})", at.strftime("%Y-%m-%d")))
                .unwrap_or_default();
            winner.warnings.push(Warning::new(
                WarningCode::OlderReleaseForClient,
                format!(
                    "{}: the newest release {} has no build for {} ({}); installed {tag}{date}, the newest that has",
                    name.unwrap_or(&winner.name),
                    looked.newest,
                    self.target.id,
                    family_name(self.target)
                ),
            ));
        }
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
            revision: winner.candidate.revision,
            left_out: winner
                .loadable
                .skipped
                .iter()
                .map(SkippedFolder::left_out)
                .collect(),
            any_flavor: false,
        };
        // A deferred asset's token URL is never stored: the plan shows its canonical URL.
        let (url, expected_hash) = match &winner.candidate.asset.location {
            AssetLocation::Url(url) => (
                Some(url.clone()),
                winner.candidate.asset.expected_hash.clone(),
            ),
            AssetLocation::Deferred { .. } => (
                winner.canonical_url.clone(),
                winner.candidate.asset.expected_hash.clone(),
            ),
            // The pinned tree the archive was assembled from.
            AssetLocation::WowAceTree { base, .. } => (Some(base.clone()), None),
            _ => (None, None),
        };
        let release = PlannedRelease {
            release_id: winner.candidate.release_id.clone(),
            url,
            expected_hash,
            blob_sha256: winner.loadable.inspected.blob_sha256.clone(),
            deferred: DeferredAsset::of(&winner.candidate.asset.location),
        };
        let input = super::witness::input_of(
            &winner.candidate.asset.location,
            winner.loadable.inspected.blob_sha256.as_deref(),
            &self.ctx.paths.blobs_dir(),
        );
        Ok(Prepared::Build(Box::new(PendingBuild {
            existing: existing.cloned(),
            template,
            intent: intent.clone(),
            loadable: winner.loadable,
            blob: winner.blob,
            release: Some(release),
            warnings: winner.warnings,
            deps: winner.deps,
            input,
        })))
    }

    /// The second pass of an install: ownership and policy against the working view.
    fn build(&self, b: PendingBuild) -> Result<PlannedAddon> {
        // The commit inspects and downloads nothing: it warns from the work.
        let archive_warnings = b.warnings.clone();
        let mut planned = self.build_install(
            b.existing.as_ref(),
            b.template,
            &b.intent,
            &[],
            b.loadable,
            b.blob,
            b.release,
            b.warnings,
        )?;
        if let WorkMode::Folders(f) = &mut planned.work.mode {
            f.deps = b.deps;
            f.input = b.input;
            f.archive_warnings = archive_warnings;
            // Here, never in `build_install`: the walk stays out of the commit window
            // that `rejudge` holds the lock for.
            f.local_changes = self.local_changes(&f.swap);
            if let Some(changes) = &f.local_changes {
                planned.warnings.push(local_changes_warning(
                    &planned.work.addon,
                    changes,
                    None,
                    &self.ctx.retention,
                ));
            }
        }
        Ok(planned)
    }

    /// The files of the folders `swap` replaces that changed on disk since they were
    /// indexed (a stat-first drift check of each); `None` when none did. An unindexed
    /// folder has nothing to compare.
    fn local_changes(&self, swap: &[SwapOp]) -> Option<LocalChanges> {
        let index = self.index?;
        let addons = self.target.addons_dir();
        let mut changes = LocalChanges {
            paths: Vec::new(),
            count: 0,
            folders: Vec::new(),
        };
        for op in swap {
            let SwapOp::Replace { live, .. } = op else {
                continue;
            };
            let Some(entry) = index.folders.get(live.key()) else {
                continue;
            };
            let walked = crate::drift::check_folder(
                &addons,
                live.as_folder(),
                Some(entry),
                index.recorded_at_ns(entry),
                crate::drift::DriftMode::Stat,
            );
            let d = walked.drift;
            if d.changed == 0 {
                continue;
            }
            changes.count += d.changed;
            changes.folders.push(live.as_folder().clone());
            changes.paths.extend(
                d.modified
                    .iter()
                    .chain(&d.deleted)
                    .chain(&d.added)
                    .map(|p| format!("{}/{p}", live.as_str())),
            );
        }
        if changes.count == 0 {
            return None;
        }
        changes.paths.sort();
        changes.paths.truncate(LOCAL_CHANGES_SHOWN);
        changes.folders.sort_by(|a, b| a.key().cmp(b.key()));
        Some(changes)
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
                // Only what still changes the record as it is now: an edit another
                // commit made meanwhile is not this transaction's to log or undo.
                let edits = effective_edits(current.cloned(), &work.edits, addon, target)?;
                let after = apply_edits(current.cloned(), &edits, addon, target)?;
                let before = current.cloned();
                let guard = work.guard.clone();
                Ok(records_change(addon, before, after, edits, guard)
                    .map_or_else(Planned::nothing, Planned::change))
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
        // An addon of the same name from another ref is its record's to keep: a newer
        // local archive of a local install replaces it as an update; any other ref takes
        // it over only with `--force`, as an update that warns.
        let namesake = existing
            .is_none()
            .then(|| {
                self.record
                    .addons
                    .get(&TargetRecord::addon_key(&template.name))
            })
            .flatten()
            .filter(|a| a.r#ref != template.r#ref);
        // `--force` is for what the user named: a dependency the resolver chose is judged
        // without it, so its archive never replaces a folder it would otherwise refuse.
        let force = self.req.force && !intent.from_resolver();
        let existing = match namesake {
            Some(other) if is_local(&other.r#ref) && is_local(&template.r#ref) => Some(other),
            Some(other) if !force => {
                return Err(takeover_refused(
                    other,
                    &template.r#ref,
                    self.ctx.argv,
                    &self.target.id,
                ));
            }
            Some(other) => {
                warnings.push(takeover_warning(other, &template.r#ref, &self.target.id));
                Some(other)
            }
            None => existing,
        };
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
        let new_ref = existing.is_none().then_some(&template.r#ref);
        let judged = self.judge(&template.name, &changes, toc_after, new_ref, force)?;
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
                    handed_over: Vec::new(),
                    deps: DepFacts::default(),
                    local_changes: None,
                    archive_warnings: Vec::new(),
                    input: None,
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
        let judged = self.judge(
            &installed.name,
            &changes,
            |_| BTreeSet::new(),
            None,
            self.req.force,
        )?;
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
                    handed_over: Vec::new(),
                    deps: DepFacts::default(),
                    local_changes: None,
                    archive_warnings: Vec::new(),
                    input: None,
                })),
            },
        })
    }

    /// Ownership, then the running policy.
    /// `new_ref` is the ref of an addon with no record on the target: an unmanaged folder
    /// it ships is adopted under it rather than overwritten. `force` is `--force` where it
    /// applies (never to a dependency the resolver chose).
    fn judge(
        &self,
        addon: &str,
        changes: &[FolderChange],
        toc_after: impl Fn(&FolderName) -> BTreeSet<String>,
        new_ref: Option<&Ref>,
        force: bool,
    ) -> Result<Judged> {
        let ctx = self.ctx;
        let adopt = new_ref.map(|r| ownership::AdoptFix {
            target: &self.target.id,
            r#ref: r,
        });
        let owned = ownership::check_in(
            changes,
            self.live,
            addon,
            force,
            ctx.argv,
            &self.view.borrow(),
            adopt,
        )?;
        let addons_dir = self.target.addons_dir();
        // Every client that sees these folders judges them: this one, and each running
        // client sharing its AddOns directory (a symlink between Classic clients), by
        // the TOCs it loads itself.
        let mut clients: Vec<&Target> = vec![self.target];
        clients.extend(
            crate::discover::shared::siblings(self.target, ctx.known_targets.iter())
                .into_iter()
                .filter(|t| ctx.running.contains(&t.client_dir)),
        );
        let mut reload_required = ReloadRequired::None;
        let mut warnings = owned.warnings;
        for client in clients {
            let loaded = |live: &MutableFolderName| {
                DiskFolder::new(addons_dir.join(live.as_str()))
                    .ok()
                    .and_then(|view| loaded_toc(&view, &client.rules).ok().flatten())
                    .map(|l| l.file)
            };
            let policy_ops: Vec<PolicyOp> = owned
                .allowed
                .iter()
                .map(|op| match op {
                    AllowedOp::Add { name } => PolicyOp::AddFolder {
                        folder: name.as_folder().clone(),
                    },
                    AllowedOp::Replace { name, live, .. } => PolicyOp::ReplaceFolder {
                        folder: live.as_folder().clone(),
                        loaded_toc_before: loaded(live),
                        toc_names_after: toc_after(name.as_folder()),
                    },
                    AllowedOp::Remove { live } => PolicyOp::RemoveFolder {
                        folder: live.as_folder().clone(),
                        loaded_toc_before: loaded(live),
                    },
                })
                .collect();
            let running = ctx.running.contains(&client.client_dir);
            let policy_ctx = PolicyContext {
                target_id: &client.id,
                argv: ctx.argv,
                running_unknown: ctx.running_unknown,
            };
            let policy = running::evaluate(
                &policy_ctx,
                ctx.policy,
                running,
                &client.rules,
                &policy_ops,
                self.req.while_running,
            )?;
            reload_required = reload_required.max(policy.reload_required);
            for warning in policy.warnings {
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
        Ok(Judged {
            allowed: owned.allowed,
            transfers: owned.transfers,
            degraded: owned.degraded,
            reload_required,
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
                        summary.added.push(name.as_folder().clone());
                        (OpKind::Add, name.as_folder().clone())
                    }
                    AllowedOp::Replace { live, .. } => {
                        summary.replaced.push(live.as_folder().clone());
                        (OpKind::Replace, live.as_folder().clone())
                    }
                    AllowedOp::Remove { live } => {
                        summary.removed.push(live.as_folder().clone());
                        (OpKind::Remove, live.as_folder().clone())
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
        let old = |live: &MutableFolderName| -> Option<TreeDigest> {
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
    /// it does not load on this client, or (with a `floor`) is older than the installed
    /// build; `Err` when it cannot even be read (download, archive). A winner keeps only
    /// the folders this client loads (see [`grade`]). `name_hint` names the primary
    /// folder (see [`primary_folder`]). Under `any_flavor` the publisher's word is
    /// ignored and folders built for other clients install as out of date; what the
    /// candidate teaches the memo is still judged by the client's own rules.
    fn try_candidate(
        &self,
        r#ref: &Ref,
        candidate: &Candidate,
        name_hint: Option<&str>,
        floor: Option<u32>,
        any_flavor: bool,
    ) -> Result<std::result::Result<Winner, Refusal>> {
        let ctx = self.ctx;
        let mut downloaded = None;
        let mut canonical_url = None;
        let mut unverified = None;
        let (inspected, blob) = match &candidate.asset.location {
            AssetLocation::Url(_)
            | AssetLocation::Deferred { .. }
            | AssetLocation::WowAceTree { .. } => {
                let done = match self.download(&candidate.release_id, &candidate.asset) {
                    Ok(done) => done,
                    // A WowAce project that needs the packager is refused by name, as
                    // one that does not load here is.
                    Err(e) if let Some(reason) = wowace::packaging_refusal(&e) => {
                        return Ok(Err(Refusal::new(reason)));
                    }
                    Err(e) => {
                        return Err(match &candidate.asset.location {
                            AssetLocation::Url(url) => missing_git_rev(e, candidate, url),
                            _ => e,
                        });
                    }
                };
                let inspected = inspect(&done.blob.path, &ctx.limits, ctx.table)?;
                let path = done.blob.path.clone();
                canonical_url = done.canonical_url;
                unverified = done.unverified;
                downloaded = Some(done.blob);
                (inspected, Some(path))
            }
            AssetLocation::LocalZip(path) => {
                // A recorded release's cached archive, reused as it is.
                if let Some(ExpectedHash::Sha256(sha)) = &candidate.asset.expected_hash {
                    unverified = self.reused_unverified(&candidate.release_id, sha);
                }
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
        let primary = primary_folder(r#ref, &all, name_hint);
        // The publisher's release.json names this client for this very file.
        // A source's list (Tukui's patches) only ranks: the folders are judged strictly.
        let declared = matches!(
            candidate.rank,
            RankReason::MetadataInterface | RankReason::MetadataFlavor
        ) && candidate
            .asset
            .flavors
            .iter()
            .any(|f| f.source.is_release_json());
        let grade = primary.map_or(Grade::Refused, |p| grade(&split, declared, p));
        // The addon's own folder carries a TOC named for this client (a game or family
        // suffix of its row) at an in-range interface: the author built it for this
        // client, whatever the metadata says (Raider.IO's `_Classic` and `_Vanilla`
        // TOCs under a release.json naming mainline only).
        let range = ctx.table.family_range(self.target.family);
        let own_toc = verdicts
            .iter()
            .find(|v| Some(&v.folder) == primary)
            .is_some_and(|v| {
                matches!(v.report.verdict, Verdict::Compatible { .. })
                    && v.report
                        .loaded
                        .as_ref()
                        .is_some_and(|l| l.level != TocLevel::Plain)
                    && v.interface
                        .is_some_and(|i| range.is_some_and(|r| r.contains(i)))
            });
        // The publisher labelled this very file for other clients only.
        let against = (grade != Grade::Refused && !own_toc)
            .then(|| self.evidence_against(r#ref, candidate, &all))
            .flatten();
        // What install would do with it here, for the next checks to know.
        let wins = grade != Grade::Refused && against.is_none();
        if let Some(blob) = &downloaded {
            let verdict = verdicts.iter().find(|v| Some(&v.folder) == primary);
            self.learn(candidate, blob, unverified.is_some(), wins, verdict);
        }
        // `--any-flavor`: the publisher's word is ignored, and a folder whose TOC the
        // client reads, off its range, installs as out of date, as the client lists it.
        let mut allowed: Vec<String> = Vec::new();
        let (verdicts, split, grade, against) = if any_flavor {
            allowed.extend(against);
            let admitted: Vec<FolderVerdict> = verdicts
                .into_iter()
                .map(|mut v| {
                    if let Verdict::WrongFlavor { reason } = &v.report.verdict
                        && reason.admitted_by_any_flavor()
                    {
                        let said = self.describe(&v.folder, reason);
                        v.report.warnings.push(self.admitted_out_of_date(&said));
                        allowed.push(said);
                        v.report.verdict = Verdict::Compatible { out_of_date: true };
                    }
                    v
                })
                .collect();
            let split = partition(&admitted);
            let grade = primary.map_or(Grade::Refused, |p| {
                super::inspect::grade(&split, declared, p)
            });
            (admitted, split, grade, None)
        } else {
            (verdicts, split, grade, against)
        };
        if grade == Grade::Refused {
            let reasons: Vec<String> = verdicts
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
            // The addon's own folder when it is skipped, else the folder that refuses.
            let deciding = split
                .skip
                .iter()
                // ubs:ignore — comparing folder names, not secrets
                .find(|s| Some(&s.name) == primary)
                .or_else(|| {
                    split
                        .skip
                        .iter()
                        .find(|s| !s.reason.rules_itself_out() && !s.reason.built_for_others())
                })
                .or_else(|| split.skip.first());
            let why = deciding.map_or_else(
                || "no folder loads on this client".to_owned(),
                |s| self.describe(&s.name, &s.reason),
            );
            return Ok(Err(Refusal {
                reason: format!("wrong_flavor: {}", reasons.join("; ")),
                why,
                overridable: !any_flavor
                    && deciding.is_some_and(|s| s.reason.admitted_by_any_flavor()),
                wrong_flavor: true,
            }));
        }
        if let Some(evidence) = against {
            return Ok(Err(Refusal {
                reason: format!("wrong_flavor: {evidence}"),
                why: evidence,
                overridable: true,
                wrong_flavor: true,
            }));
        }
        // M2-D42: an adopted build is never replaced by an older one. The fact above is
        // kept, so the next check shows the candidate blocked.
        if let Some(floor) = floor {
            let verdict = verdicts.iter().find(|v| Some(&v.folder) == primary);
            let out_of_date = verdict.is_some_and(|v| {
                matches!(v.report.verdict, Verdict::Compatible { out_of_date: true })
            });
            let below = verdict.and_then(|v| v.interface).filter(|i| *i < floor);
            if out_of_date || below.is_some() {
                let why = below.map_or_else(
                    || "it is out of date on this client".to_owned(),
                    |i| format!("its interface {i} is below the installed {floor}"),
                );
                return Ok(Err(Refusal::new(format!("older_than_installed: {why}"))));
            }
        }
        let mut inspected = inspected;
        inspected
            .folders
            .retain(|f| split.install.contains(&f.name));
        // Named as on every client, unless that folder is one this client skips.
        let name = primary
            .filter(|p| split.install.contains(p))
            .map_or_else(|| addon_name(r#ref, &inspected), |p| p.as_str().to_owned());
        // What the needed folders require, for the dependency rounds (M2-D7).
        let installed: Vec<FolderVerdict> = verdicts
            .iter()
            .filter(|v| split.install.contains(&v.folder))
            .cloned()
            .collect();
        let needed = match primary.filter(|p| split.install.contains(p)) {
            Some(primary) => needed_folders(primary, &installed),
            // No primary: every folder installed is the addon.
            None => installed.iter().map(|v| v.folder.clone()).collect(),
        };
        let mut requires: Vec<FolderName> = Vec::new();
        let mut unknown_condition: Vec<FolderName> = Vec::new();
        let mut optional: Vec<FolderName> = Vec::new();
        for v in installed.iter().filter(|v| needed.contains(&v.folder)) {
            for dep in &v.dependencies.required {
                if !requires.contains(dep) {
                    requires.push(dep.clone());
                }
            }
            for dep in &v.dependencies.unknown_condition {
                if !unknown_condition.contains(dep) {
                    unknown_condition.push(dep.clone());
                }
            }
            for dep in &v.dependencies.optional {
                if !optional.contains(dep) {
                    optional.push(dep.clone());
                }
            }
        }
        unknown_condition.retain(|dep| !requires.contains(dep));
        optional.retain(|dep| !requires.contains(dep) && !needed.contains(dep));
        let deps = DepFacts {
            optional,
            requires,
            unknown_condition,
            bridges: bridge_dependencies(&needed, &installed),
            out_of_date: installed
                .iter()
                .filter(|v| matches!(v.report.verdict, Verdict::Compatible { out_of_date: true }))
                .map(|v| v.folder.clone())
                .collect(),
        };
        let mut warnings: Vec<Warning> = verdicts
            .into_iter()
            .filter(|v| split.install.contains(&v.folder))
            .flat_map(|v| v.report.warnings)
            .collect();
        if grade == Grade::LeftOut {
            for left in split.skip.iter().filter(|s| s.reason.built_for_others()) {
                warnings.push(self.left_out(&name, left));
            }
        }
        if !allowed.is_empty() {
            warnings.push(Warning::new(
                WarningCode::AnyFlavor,
                format!(
                    "{name}: installed under --any-flavor on {}, although {}",
                    self.target.id,
                    allowed.join("; ")
                ),
            ));
        }
        // TLS protected the transfer and the TOCs verified, but not the source's hash.
        if let Some(reason) = unverified {
            warnings.push(Warning::new(
                WarningCode::DownloadUnverified,
                format!(
                    "{name} {}: its download was not checked: {reason}",
                    candidate.version
                ),
            ));
        }
        Ok(Ok(Winner {
            candidate: candidate.clone(),
            grade,
            loadable: Loadable {
                inspected,
                skipped: split.skip,
            },
            name,
            blob,
            canonical_url,
            warnings,
            deps,
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
        unverified: bool,
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
                judge: Some(self.target.judge.clone()),
            }],
            unverified,
        };
        merge_fact(&mut self.facts.borrow_mut(), fact);
    }

    /// The blob of `asset` of `release_id`: what this command already downloaded (or
    /// failed to: the same error again), else [`TargetPlanner::fetch_blob`], remembered
    /// for the command's later plans and targets. A remembered blob evicted since is
    /// fetched again.
    fn download(&self, release_id: &str, asset: &crate::model::Asset) -> Result<Downloaded> {
        let ctx = self.ctx;
        let key = (release_id.to_owned(), asset.filename.clone());
        match ctx.downloads.get(&key) {
            Some(Ok(done)) => {
                if let Some(blob) = ctx.blobs.get(ctx.scopes, &done.blob.sha256)? {
                    return Ok(Downloaded { blob, ..done });
                }
            }
            Some(Err(error)) => return Err(error),
            None => {}
        }
        let result = self.fetch_blob(release_id, asset);
        ctx.downloads.put(key, result.clone());
        result
    }

    /// The blob of `asset` of `release_id`, in this order: the cached blob of its
    /// expected hash; the cached blob of the hash an earlier download remembered, unless
    /// the source now declares another hash or size (a fact never stands in for the
    /// blob: eviction makes facts stale); else a download, retried once on a hash
    /// mismatch. A `Deferred` location is resolved by its source right before each
    /// attempt, and a 403 or 404 is retried once after resolving again.
    fn fetch_blob(&self, release_id: &str, asset: &crate::model::Asset) -> Result<Downloaded> {
        let ctx = self.ctx;
        let expected = asset.expected_hash.as_ref();
        // A WoWInterface file published without its MD5 is not checked.
        let unpublished = crate::sources::wowi::unpublished_md5(release_id, expected);
        // The blob cache is keyed by SHA-256: an MD5 source's fast path is its facts.
        let expected_sha = match expected {
            Some(ExpectedHash::Sha256(sha)) => Some(sha.to_ascii_lowercase()),
            Some(ExpectedHash::Md5(_)) | None => None,
        };
        let cached = |blob, unverified| Downloaded {
            blob,
            canonical_url: None,
            unverified,
        };
        if let Some(sha) = &expected_sha
            && let Some(blob) = ctx.blobs.get(ctx.scopes, sha)?
        {
            return Ok(cached(blob, None));
        }
        if let Some((sha, size)) = ctx.cache.facts().hash(release_id, &asset.filename)
            // ubs:ignore — comparing archive hashes, not secrets
            && expected_sha.as_ref().is_none_or(|e| e == sha)
            && asset.size.is_none_or(|declared| declared == size)
            && let Some(blob) = ctx.blobs.get(ctx.scopes, sha)?
        {
            let unverified = self.reused_unverified(release_id, &blob.sha256);
            return Ok(cached(blob, unverified));
        }
        let mut canonical_url = None;
        let fetched = match &asset.location {
            AssetLocation::Deferred { source, key } => {
                let Some(source) = ctx.registry.get(*source) else {
                    return Err(ctx.registry.unavailable(*source, release_id));
                };
                let mut resolve = || {
                    let resolved = source.resolve_asset(key, ctx.http)?;
                    canonical_url = resolved.canonical_url;
                    Ok(DownloadTarget {
                        url: resolved.url,
                        declared: resolved.expected_hash.or_else(|| expected.cloned()),
                        from_header: resolved.hash_from_header,
                    })
                };
                ctx.blobs
                    .download_resolving(ctx.scopes, &ctx.tx, ctx.http, &mut resolve, true)?
            }
            AssetLocation::Url(url) => ctx
                .blobs
                .download(ctx.scopes, &ctx.tx, ctx.http, url, expected, None)?,
            // Assembled at its pinned revision and packed; no hash comes from the
            // server, so the TOCs and TLS are the check, as on GitHub's no-REST path.
            AssetLocation::WowAceTree { base, revision } => {
                if ctx.registry.get(SourceId::Wowace).is_none() {
                    return Err(ctx.registry.unavailable(SourceId::Wowace, release_id));
                }
                let zip = wowace::assemble(ctx.http, base, *revision, ctx.limits.max_download)?;
                let blob = ctx.blobs.store_stream(
                    ctx.scopes,
                    &ctx.tx,
                    std::io::Cursor::new(zip),
                    None,
                    &Storage::Cache,
                )?;
                Fetched {
                    blob,
                    unverified: None,
                }
            }
            AssetLocation::LocalZip(_)
            | AssetLocation::LocalDir(_)
            | AssetLocation::GitCheckout { .. } => {
                return Err(Error::internal(format!(
                    "{release_id}: {} is not downloaded",
                    asset.filename
                )));
            }
        };
        failpoint::hit("engine.plan.downloaded").map_err(|e| fail(ctx, &e))?;
        let item = format!("{release_id} on {}", self.target.id);
        ctx.progress
            .event(Phase::Download, self.candidate_items, &item);
        Ok(Downloaded {
            blob: fetched.blob,
            canonical_url,
            unverified: fetched.unverified.or(unpublished),
        })
    }

    /// Why reusing the cached archive `sha256` of `release_id` goes unchecked: its
    /// download did (the release memo says so), and the cache cannot check it either.
    fn reused_unverified(&self, release_id: &str, sha256: &str) -> Option<String> {
        self.ctx
            .cache
            .facts()
            .unverified_blob(release_id, sha256)
            .then(|| {
                crate::sources::wowi::unpublished_md5(release_id, None)
                    .unwrap_or_else(|| "the cached download it reuses was never checked".to_owned())
            })
    }

    /// A reinstall that needs its source while offline: its archive is not in the blob
    /// cache. Any other error stays as it is.
    fn not_cached(&self, error: Error, wanted: &Wanted, intent: &InstallIntent) -> Error {
        // ubs:ignore — comparing error codes, not secrets
        if !intent.reinstall || error.code != ErrorCode::Offline {
            return error;
        }
        let name = self
            .record
            .addons
            .values()
            .find(|a| a.r#ref == wanted.r#ref)
            .map_or_else(|| wanted.r#ref.to_string(), |a| a.name.clone());
        // The command boundary makes it the same command online when `--offline` alone
        // kept the run offline.
        Error::new(
            ErrorCode::Offline,
            format!(
                "{name}: its archive is not in the blob cache, and getting it again needs the network"
            ),
            Suggestion::text("run without --offline and with OMADDONS_OFFLINE unset"),
        )
        .details(json!({ "ref": wanted.r#ref.to_string(), "reason": "not_cached" }))
    }

    /// A GitHub look-back for this client, from `newest` (a tag), on any channel: the
    /// feed on stable, the REST listing on beta and alpha. Only on a client a
    /// `release.json` can name.
    fn look_back(&self, wanted: &Wanted, newest: &str) -> Option<Result<crate::sources::LookBack>> {
        if !matches!(wanted.r#ref, Ref::GitHub { .. }) {
            return None;
        }
        let family = self.target.family;
        family.release_flavor()?;
        let range = self.ctx.table.family_range(family);
        let names = move |f: &crate::model::AssetFlavor| declares_client(f, family, range);
        self.ctx.cache.look_back(wanted, newest, family, &names)
    }

    /// The second pass of a look-back: `found`, older than `newest`, installs unless the
    /// installed release is it or newer, by `order` (the feed's tags, newest first) or
    /// else by dotted version, or the two cannot be ordered at all: a look-back never
    /// downgrades ([`crate::resolve::updates::older_than_found`]).
    #[expect(
        clippy::too_many_arguments,
        reason = "what the look-back found, with the request of the first pass"
    )]
    fn install_looked_back(
        &self,
        name: Option<&str>,
        wanted: &Wanted,
        existing: Option<&AddonRecord>,
        found: ReleaseSet,
        order: &[String],
        newest: &str,
        intent: &InstallIntent,
    ) -> Result<Prepared> {
        let release = found.releases.first();
        let found_tag = release.and_then(|r| tag_of(wanted, &r.release_id));
        let published = release.and_then(|r| r.published_at);
        if let Some(existing) = existing {
            let installed = existing
                .release_id
                .as_deref()
                .and_then(|id| tag_of(wanted, id));
            let older =
                installed
                    .as_deref()
                    .zip(found_tag.as_deref())
                    .is_some_and(|(installed, found)| {
                        crate::resolve::updates::older_than_found(order, installed, found)
                    });
            if !older {
                return self.current(existing, intent).map(Prepared::Done);
            }
        }
        let intent = InstallIntent {
            looked_back: Some(LookedBack {
                newest: newest.to_owned(),
                published,
            }),
            ..intent.clone()
        };
        let source = CandidateSource {
            file: None,
            local: None,
            set: Some(Ok(found)),
            cached: None,
        };
        self.prepare_install(name, wanted, source, &intent)
    }

    /// `error` with the `--any-flavor` retry among its alternatives when the setting would
    /// lift the refusal (`overridable`): never its fix, which an agent may run unread.
    fn offer_any_flavor(&self, error: Error, wanted: &Wanted, overridable: bool) -> Error {
        if !overridable {
            return error;
        }
        error.alternative(Suggestion::command([
            "omaddons".to_owned(),
            "install".to_owned(),
            wanted.r#ref.to_string(),
            "-t".to_owned(),
            self.target.id.clone(),
            "--any-flavor".to_owned(),
        ]))
    }

    /// Why a candidate below the client's interface `floor` never loads, for refusals.
    fn floor_note(&self, floor: u32) -> String {
        format!(
            "{} never loads an addon whose interface is below {floor}, whatever the player enables",
            self.target.id
        )
    }

    /// The publisher's word that `candidate` was built for other clients only, in words:
    /// its `release.json` entries name no client of this family (said only on a client a
    /// `release.json` can name), else its file name's evidence ([`evidence_hints`],
    /// without the words of the archive's `folders` or the repository's name) names only
    /// other clients. A source's own list (Tukui's patches) is never evidence, and a file
    /// name only where the source gives it meaning (a GitHub asset's).
    fn evidence_against(
        &self,
        r#ref: &Ref,
        candidate: &Candidate,
        folders: &[FolderName],
    ) -> Option<String> {
        let asset = &candidate.asset;
        let family = self.target.family;
        let listed: Vec<_> = asset
            .flavors
            .iter()
            .filter(|f| f.source.is_release_json())
            .collect();
        if !listed.is_empty() {
            family.release_flavor()?;
            let range = self.ctx.table.family_range(family);
            if listed.iter().any(|f| declares_client(f, family, range)) {
                return None;
            }
            let mut flavors: Vec<&str> = Vec::new();
            for f in &listed {
                if !flavors.contains(&f.flavor.as_str()) {
                    flavors.push(&f.flavor);
                }
            }
            return Some(format!(
                "release.json lists {} for {} only",
                asset.filename,
                flavors.join(", ")
            ));
        }
        if asset.name_flavors.is_empty() {
            return None;
        }
        let mut own: Vec<&str> = folders.iter().map(FolderName::as_str).collect();
        if let Ref::GitHub { repo, .. } = r#ref {
            own.push(repo);
        }
        let hints = evidence_hints(&asset.filename, &own);
        if name_evidence(&hints, family) != NameEvidence::Against {
            return None;
        }
        let named: Vec<&str> = hints.iter().map(|h| hint_label(*h)).collect();
        Some(format!(
            "its file name {} names {} only",
            asset.filename,
            named.join(", ")
        ))
    }

    /// One folder's verdict on this client, in words.
    fn describe(&self, folder: &FolderName, reason: &WrongFlavorReason) -> String {
        let f = folder.as_str();
        match reason {
            WrongFlavorReason::GameTypeExcluded => {
                format!("{f} is gated to other game types (AllowLoadGameType, ExcludeLoadGameType)")
            }
            WrongFlavorReason::NoInterface => {
                format!("{f}'s TOC has no Interface this client reads")
            }
            WrongFlavorReason::InterfaceNotInFamily { interfaces, .. } => format!(
                "{f} lists only {} ({})",
                interface_list(interfaces),
                self.families_of(interfaces)
            ),
            WrongFlavorReason::BelowClientFloor { interfaces, floor } => format!(
                "{f} targets {}, and {}",
                interface_list(interfaces),
                self.floor_note(*floor)
            ),
            WrongFlavorReason::TargetInterfaceUnknown => {
                format!("{f} cannot be judged: this client's interface is unknown")
            }
            WrongFlavorReason::NoLoadableToc => format!("{f} has no TOC this client loads"),
            WrongFlavorReason::DependencySkipped { folder: needed } => format!(
                "{f} requires {}, which this client does not load",
                needed.as_str()
            ),
        }
    }

    /// The families whose range holds `interfaces`, by name, each once.
    fn families_of(&self, interfaces: &[u32]) -> String {
        let mut names: Vec<&str> = Vec::new();
        for i in interfaces {
            let family = Family::ALL.into_iter().find(|f| {
                self.ctx
                    .table
                    .family_range(*f)
                    .is_some_and(|r| r.contains(*i))
            });
            let name = family.map_or("no known client", Family::as_str);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names.join(", ")
    }

    /// The `out_of_date` warning of a folder `--any-flavor` installs anyway (`said`: its
    /// verdict in words), as the client lists it.
    fn admitted_out_of_date(&self, said: &str) -> Warning {
        Warning::new(
            WarningCode::OutOfDate,
            format!(
                "{said}: installed under --any-flavor; {} lists it as out of date and loads it only with out-of-date addons enabled",
                self.target.id
            ),
        )
        .with_fix(Suggestion::text(
            "update the addon, or enable \"Load out of date AddOns\" in the client",
        ))
    }

    /// The `folder_left_out` warning for `left`, a folder of `addon` built for other
    /// clients only, which the install leaves out.
    fn left_out(&self, addon: &str, left: &SkippedFolder) -> Warning {
        let tail = match left.reason {
            WrongFlavorReason::InterfaceNotInFamily { .. } => {
                format!(" ({} would list it as out of date)", self.target.id)
            }
            _ => String::new(),
        };
        Warning::new(
            WarningCode::FolderLeftOut,
            format!(
                "{addon}: {}, built for other clients, so it is left out{tail}",
                self.describe(&left.name, &left.reason)
            ),
        )
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
        // What the addon says it supports, read-only.
        .alternative(Suggestion::command([
            "omaddons".to_owned(),
            "info".to_owned(),
            wanted.r#ref.to_string(),
            "-t".to_owned(),
            self.target.id.clone(),
        ]))
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

/// The tag of a GitHub `release_id` (`gh:o/r@<tag>`).
fn tag_of(wanted: &Wanted, release_id: &str) -> Option<String> {
    release_id
        .strip_prefix(&format!("{}@", wanted.r#ref))
        .map(str::to_owned)
}

/// Whether a release's flavor entry names a client of `family` (whose interfaces lie in
/// `range`): a `release.json` entry whose interface is in the range, or, failing that,
/// whose flavor names the family (Questie's `wrath:38002` names Titan).
fn declares_client(
    f: &crate::model::AssetFlavor,
    family: Family,
    range: Option<crate::rules::NumRange>,
) -> bool {
    f.source.is_release_json()
        && (Family::from_release_flavor(&f.flavor) == Some(family)
            || f.interface
                .is_some_and(|i| range.is_some_and(|r| r.contains(i))))
}

/// `interface 120105`, or `interfaces 11509, 16001`.
fn interface_list(interfaces: &[u32]) -> String {
    let listed: Vec<String> = interfaces.iter().map(u32::to_string).collect();
    match listed.len() {
        1 => format!("interface {}", listed.join("")),
        _ => format!("interfaces {}", listed.join(", ")),
    }
}

/// A file-name flavor hint as messages name it: the family, or the legacy version.
const fn hint_label(hint: FlavorHint) -> &'static str {
    match hint {
        FlavorHint::Mainline => "mainline",
        FlavorHint::Classic => "vanilla",
        FlavorHint::Bcc => "tbc",
        FlavorHint::Wrath => "wrath",
        FlavorHint::Cata => "cata",
        FlavorHint::Mists => "mists",
        FlavorHint::Titan => "titan",
        FlavorHint::Forever => "forever",
        FlavorHint::ClassicAny => "classic",
        FlavorHint::VanillaLegacy => "1.12",
        FlavorHint::TbcLegacy => "2.4.3",
        FlavorHint::WrathLegacy => "3.3.5",
        FlavorHint::CataLegacy => "4.3.4",
        FlavorHint::MistsLegacy => "5.4.8",
    }
}

fn family_name(target: &Target) -> String {
    serde_json::to_value(target.family)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The version that asks a source for `installed`'s release exactly: its tag, a git
/// ref's revision (from its `release_id`), and nothing for a URL or a local path, whose
/// one release is the archive itself.
fn exact_version(installed: &AddonRecord) -> Option<String> {
    match &installed.r#ref {
        Ref::Url { .. } | Ref::File { .. } => None,
        Ref::Git { .. } => installed
            .release_id
            .as_deref()
            .and_then(|id| id.rsplit_once('@'))
            .map(|(_, rev)| rev.to_owned()),
        _ => Some(installed.version.clone()),
    }
}

/// The cached archive of a recorded release as an install candidate, carrying that
/// release's identity (`release_id`, `version`) rather than a local file's: `None` when
/// the blob `sha256` is not in the cache, or no longer hashes to it. Its rank is not
/// recorded, so it is `Unannotated`. Reinstalls use it, and `sync --locked` will.
/// `revision` is the release's source revision, which the archive was built at.
pub fn local_blob_candidate(
    blobs: &BlobCache,
    scopes: &ScopeTable,
    release_id: &str,
    version: &str,
    channel: Channel,
    sha256: &str,
    revision: Option<u32>,
) -> Option<Candidate> {
    let blob = blobs.get(scopes, sha256).ok().flatten()?;
    let (size, hex) = crate::state::files_index::hash_file_streaming(&blob.path).ok()?;
    if !hex.eq_ignore_ascii_case(sha256) {
        return None;
    }
    Some(Candidate {
        release_id: release_id.to_owned(),
        version: version.to_owned(),
        channel,
        asset: crate::model::Asset {
            filename: format!("{}.zip", blob.sha256),
            location: AssetLocation::LocalZip(blob.path),
            size: Some(size),
            expected_hash: Some(ExpectedHash::Sha256(blob.sha256)),
            flavors: Vec::new(),
            name_flavors: Vec::new(),
            nolib: false,
        },
        rank: crate::model::RankReason::Unannotated,
        revision,
    })
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
        // A zip is identified by its content and a directory by its listing, so an
        // unchanged one is current and a changed one an update.
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
        revision: None,
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

/// `sha256:<hex>` of a local zip; `listing:<hash>` of a directory, as a `file:`
/// directory ref is identified (M4-D19), so two previews of an unchanged one agree; a
/// fresh `dir:<ulid>` for anything else, which fails inspection before its id can reach
/// a record or a token.
fn local_identity(file: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    if file.is_dir() {
        return format!("listing:{}", crate::sources::local::listing_hash(file));
    }
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
    primary_folder(r#ref, &folders, None)
        .map_or_else(|| r#ref.to_string(), |f| f.as_str().to_owned())
}

/// The folder an archive is the addon of: the folder named like `name_hint` (a catalog
/// name, a WoWInterface `UIName` or a Tukui slug; normalised), else the folder named
/// like the repository, else the folder its names point at ([`primary_by_shape`]).
pub fn primary_folder<'a>(
    r#ref: &Ref,
    folders: &'a [FolderName],
    name_hint: Option<&str>,
) -> Option<&'a FolderName> {
    if let Some(hint) = name_hint.map(crate::resolve::deps::normalized)
        && let Some(named) = folders
            .iter()
            .find(|f| crate::resolve::deps::normalized(f.as_str()) == hint)
    {
        return Some(named);
    }
    if let Ref::GitHub { repo, .. } = r#ref
        && let Some(same) = folders
            .iter()
            .find(|f| f.as_str().eq_ignore_ascii_case(repo))
    {
        return Some(same);
    }
    primary_by_shape(folders)
}

/// The primary folder by the folders' names alone (no ref, no hint): the folder the
/// others extend (`WeakAuras` for `WeakAurasOptions`), unless it is a module beside the
/// one core folder (`DBM-Test`, extended by `DBM-Test-WarWithin`, beside `DBM-Core`),
/// else the one core folder (`DBM-Core` beside `DBM-GUI`), else the shortest.
pub fn primary_by_shape(folders: &[FolderName]) -> Option<&FolderName> {
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
    // `DBM-Test` shares `DBM-Core`'s stem; `BigWigs` is the stem `BigWigs_Core` extends.
    let beside = |top: &FolderName, core: &FolderName| {
        let (top, core) = (
            top.as_str().to_ascii_lowercase(),
            core.as_str().to_ascii_lowercase(),
        );
        let stem = core
            .strip_suffix("-core")
            .or_else(|| core.strip_suffix("_core"))
            .unwrap_or(&core);
        top.strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with(['-', '_']))
    };
    let top = ranked.first().copied();
    if let Some(top) = top
        && let [only] = folders.iter().filter(core).collect::<Vec<_>>().as_slice()
        && (extended(top) == 0 || beside(top, only))
    {
        return Some(only);
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_blob_is_the_recorded_release_while_it_hashes_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        let scopes = ScopeTable::new(&paths, &[], None).unwrap();
        let blobs = BlobCache::new(&paths);
        let bytes = b"PK fake archive";
        let sha: String = {
            use sha2::{Digest, Sha256};
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        let build = || {
            local_blob_candidate(
                &blobs,
                &scopes,
                "gh:o/foo@v1",
                "v1",
                Channel::Beta,
                &sha,
                None,
            )
        };
        assert!(build().is_none(), "not cached");

        std::fs::create_dir_all(paths.blobs_dir()).unwrap();
        std::fs::write(paths.blob(&sha), bytes).unwrap();
        let candidate = build().unwrap();
        assert_eq!(
            (
                candidate.release_id.as_str(),
                candidate.version.as_str(),
                candidate.channel
            ),
            ("gh:o/foo@v1", "v1", Channel::Beta),
            "the recorded identity, not a local file's"
        );
        let local = match &candidate.asset.location {
            AssetLocation::LocalZip(path) => Some(path.clone()),
            _ => None,
        };
        assert_eq!(local, Some(paths.blob(&sha)), "the cached blob itself");
        assert_eq!(candidate.rank, crate::model::RankReason::Unannotated);

        std::fs::write(paths.blob(&sha), b"changed since").unwrap();
        assert!(build().is_none(), "a blob that no longer hashes to it");
    }

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
        // WoWInterface's DBM 11.2.15 (no release.json, so no repository name to match).
        let dbm_wowi = inspected(&[
            "DBM-Affixes",
            "DBM-Brawlers",
            "DBM-Core",
            "DBM-GUI",
            "DBM-KhazAlgar",
            "DBM-Raids-WarWithin",
            "DBM-StatusBarTimers",
            "DBM-Test",
            "DBM-Test-WarWithin",
            "DBM-VPVEM",
        ]);
        assert_eq!(
            addon_name(&"wowi:8814".parse().unwrap(), &dbm_wowi),
            "DBM-Core",
            "a module family beside the core folder does not take its place"
        );
        let plain = inspected(&["Zeta", "Alpha"]);
        assert_eq!(
            addon_name(&"gh:o/pack".parse().unwrap(), &plain),
            "Zeta",
            "no core folder: the shortest, then by name"
        );
    }

    #[test]
    fn a_name_hint_names_the_primary_folder() {
        let folders = |names: &[&str]| -> Vec<FolderName> {
            names.iter().map(|n| FolderName::new(n).unwrap()).collect()
        };
        let pick = |names: &[&str], hint: Option<&str>| {
            let list = folders(names);
            primary_folder(&"wowi:1".parse().unwrap(), &list, hint).map(|f| f.as_str().to_owned())
        };
        assert_eq!(
            pick(&["BugSack", "BugSack_Extras"], Some("BugSack")),
            Some("BugSack".into())
        );
        assert_eq!(
            pick(&["Lib", "MyAddon"], None),
            Some("Lib".into()),
            "the shortest"
        );
        assert_eq!(
            pick(&["Lib", "MyAddon"], Some("My Addon")),
            Some("MyAddon".into()),
            "a hint, normalised, overrides the order"
        );
        assert_eq!(pick(&["Lib", "MyAddon"], Some("Other")), Some("Lib".into()));
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
            revision: None,
            left_out: Vec::new(),
            any_flavor: false,
        }
    }

    /// `name`'s record with `reason`, required by `by`.
    fn requiring(name: &str, reason: AddonReason, by: &[&str]) -> AddonRecord {
        AddonRecord {
            r#ref: format!("gh:o/{}", name.to_ascii_lowercase())
                .parse()
                .unwrap(),
            name: name.into(),
            required_by: by.iter().map(|b| (*b).to_owned()).collect(),
            folders: vec![FolderName::new(name).unwrap()],
            ..installed("1", false, Channel::Stable, reason)
        }
    }

    #[test]
    fn bookkeeping_warns_once_per_still_required_removal_and_once_for_every_orphan() {
        use AddonReason::{Dependency, Explicit};
        let mut record = TargetRecord::empty(TargetKey::new());
        for r in [
            requiring("Little", Explicit, &[]),
            requiring("Big", Dependency, &["Little", "Plater"]),
            requiring("Lib", Dependency, &["Little"]),
            requiring("Ace", Dependency, &["little"]),
            requiring("Promoted", Explicit, &["Little"]),
            requiring("Plater", Explicit, &[]),
        ] {
            record.addons.insert(TargetRecord::addon_key(&r.name), r);
        }
        let forget = |name: &str| {
            let before = record.addons[&TargetRecord::addon_key(name)].clone();
            records_change(
                name,
                Some(before),
                None,
                vec![RecordEdit::Forget],
                RecordGuard::default(),
            )
            .unwrap()
            .work
        };
        let unrequire = |name: &str, by: &str| {
            let before = record.addons[&TargetRecord::addon_key(name)].clone();
            let edit = RecordEdit::RemoveRequiredBy(by.into());
            let after = apply_edit(Some(before.clone()), &edit, name, "retail").unwrap();
            records_change(
                name,
                Some(before),
                after,
                vec![edit],
                RecordGuard::default(),
            )
            .unwrap()
            .work
        };
        let codes = |warnings: &[Warning]| -> Vec<WarningCode> {
            warnings.iter().map(|w| w.code).collect()
        };

        // Little goes: nothing required it; Lib and Ace are left to nothing, Big keeps
        // Plater, and Promoted was asked for.
        let works = [
            forget("Little"),
            unrequire("Big", "Little"),
            unrequire("Lib", "Little"),
            unrequire("Ace", "Little"),
            unrequire("Promoted", "Little"),
        ];
        let warnings = bookkeeping_warnings(&record, &works, "retail", None);
        assert_eq!(codes(&warnings), [WarningCode::OrphanedDependency]);
        let fix = warnings[0].fix.as_ref().unwrap();
        assert_eq!(
            fix.argv().unwrap(),
            ["omaddons", "remove", "Ace", "Lib", "-t", "retail"]
        );
        assert!(
            warnings[0]
                .message
                .contains("Ace, Lib were installed as dependencies")
        );

        // Big goes while Little stays: still required by Little alone (Plater goes too).
        let tx = TxId::new();
        let works = [forget("Big"), forget("Plater")];
        let warnings = bookkeeping_warnings(&record, &works, "retail", Some(tx));
        assert_eq!(codes(&warnings), [WarningCode::StillRequired]);
        assert_eq!(
            warnings[0].message,
            "On retail, Big is required by Little, which will not load without it"
        );
        let undo = ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()];
        assert_eq!(warnings[0].fix.as_ref().unwrap().argv().unwrap(), undo);
        // Planned only, the fix is text.
        let planned = bookkeeping_warnings(&record, &works, "retail", None);
        assert!(planned[0].fix.as_ref().unwrap().argv().is_none());

        // Removed with everything that requires it: nothing to say.
        let works = [forget("Big"), forget("Plater"), forget("Little")];
        assert!(bookkeeping_warnings(&record, &works, "retail", None).is_empty());
    }

    /// The same warnings from two records, as an undo raises them.
    #[test]
    fn record_changes_warn_as_bookkeeping_does() {
        use AddonReason::{Dependency, Explicit};
        let of = |records: &[AddonRecord]| {
            let mut record = TargetRecord::empty(TargetKey::new());
            for r in records {
                record
                    .addons
                    .insert(TargetRecord::addon_key(&r.name), r.clone());
            }
            record
        };
        let little = requiring("Little", Explicit, &[]);
        let other = requiring("Other", Explicit, &[]);
        let big = requiring("Big", Dependency, &["Little", "Other"]);
        let before = of(&[little.clone(), other.clone(), big.clone()]);
        let codes = |warnings: &[Warning]| -> Vec<WarningCode> {
            warnings.iter().map(|w| w.code).collect()
        };

        // Big goes with Little, while Other stays.
        let tx = TxId::new();
        let stays = of(std::slice::from_ref(&other));
        let warnings = record_change_warnings(&before, &stays, "retail", Some(tx));
        assert_eq!(codes(&warnings), [WarningCode::StillRequired]);
        assert_eq!(
            warnings[0].message,
            "On retail, Big is required by Other, which will not load without it"
        );
        let undo = ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()];
        assert_eq!(warnings[0].fix.as_ref().unwrap().argv().unwrap(), undo);
        let planned = record_change_warnings(&before, &stays, "retail", None);
        assert!(planned[0].fix.as_ref().unwrap().argv().is_none());

        // Big goes with everything that requires it: nothing to say.
        assert!(record_change_warnings(&before, &of(&[]), "retail", None).is_empty());

        // Big stays, required by nothing now.
        let alone = of(&[requiring("Big", Dependency, &[])]);
        let warnings = record_change_warnings(&before, &alone, "retail", None);
        assert_eq!(codes(&warnings), [WarningCode::OrphanedDependency]);
        assert_eq!(
            warnings[0].fix.as_ref().unwrap().argv().unwrap(),
            ["omaddons", "remove", "Big", "-t", "retail"]
        );
        // Already an orphan before: not news.
        assert!(record_change_warnings(&alone, &alone, "retail", None).is_empty());
    }

    /// An owner of `Foo` installed from `r` as `release_id`, with `blob`.
    fn owned(r: &str, release_id: Option<&str>, blob: Option<&str>) -> AddonRecord {
        AddonRecord {
            r#ref: r.parse().unwrap(),
            release_id: release_id.map(str::to_owned),
            blob_sha256: blob.map(str::to_owned),
            ..requiring("Foo", AddonReason::Explicit, &[])
        }
    }

    #[test]
    fn an_idle_plan_has_no_ops_and_keeps_each_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut retail = crate::engine::testing::target(&tmp.path().join("retail"));
        let key = TargetKey::new();
        retail.key = Some(key);
        let mut classic = crate::engine::testing::target(&tmp.path().join("classic"));
        classic.id = "classic".into();
        let plan = Plan::idle(&[(&retail, 7), (&classic, 0)], &[], Actor::Agent);
        assert!(!plan.has_ops());
        assert!(plan.failures.is_empty() && plan.witnesses.is_empty());
        let shown: Vec<(&str, Option<TargetKey>, u64)> = plan
            .targets
            .iter()
            .map(|t| (t.target.as_str(), t.key, t.generation))
            .collect();
        assert_eq!(shown, [("retail", Some(key), 7), ("classic", None, 0)]);
        assert!(plan.targets.iter().all(|t| t.changes.is_empty()
            && t.releases.is_empty()
            && t.work.is_empty()
            && !t.creates_addons_dir));
        assert_eq!(
            plan.generations,
            BTreeMap::from([(key.to_string(), 7), ("classic".to_owned(), 0)])
        );
    }

    #[test]
    fn a_deferred_location_marks_its_release() {
        let deferred = AssetLocation::Deferred {
            source: crate::sources::SourceId::Tukui,
            key: "elvui".into(),
        };
        assert_eq!(
            DeferredAsset::of(&deferred),
            Some(DeferredAsset {
                source: crate::sources::SourceId::Tukui,
                key: "elvui".into(),
            })
        );
        let direct = AssetLocation::Url("https://example.com/Foo.zip".into());
        assert_eq!(DeferredAsset::of(&direct), None);
    }

    #[test]
    fn a_directory_input_is_identified_by_its_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Foo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("Foo.toc"), "## Interface: 120105\n").unwrap();
        let first = local_identity(&dir);
        assert!(first.starts_with("listing:"), "unchanged: {first}");
        let again = local_identity(&dir);
        assert_eq!(first, again, "unchanged: {first} vs {again}");
        type Change = fn(&std::path::Path);
        let rows: [(&str, Change, bool); 3] = [
            (
                ".git/HEAD written",
                |dir| std::fs::write(dir.join(".git/HEAD"), "ref: main\n").unwrap(),
                false,
            ),
            (
                "file edited",
                |dir| std::fs::write(dir.join("Foo.toc"), "## Interface: 120106\n\n").unwrap(),
                true,
            ),
            (
                "file added",
                |dir| std::fs::write(dir.join("Foo.lua"), "-- foo\n").unwrap(),
                true,
            ),
        ];
        for (row, change, moves) in rows {
            let before = local_identity(&dir);
            change(&dir);
            let after = local_identity(&dir);
            assert_eq!(before != after, moves, "{row}: {before} vs {after}");
        }
        let zip = tmp.path().join("Foo.zip");
        std::fs::write(&zip, b"PK\x05\x06").unwrap();
        let zipped = local_identity(&zip);
        assert!(zipped.starts_with("sha256:"), "a zip: {zipped}");
        let missing = local_identity(&tmp.path().join("nothing"));
        assert!(missing.starts_with("dir:"), "a missing path: {missing}");
    }

    #[test]
    fn only_copies_are_what_no_source_gives_again() {
        for (case, owner, expected) in [
            ("unmanaged", None, true),
            (
                "adopted as it is",
                Some(owned("gh:o/foo", None, None)),
                true,
            ),
            (
                "a local archive",
                Some(owned("gh:o/foo", Some("gh:o/foo@sha256:ab"), None)),
                true,
            ),
            (
                "a local directory",
                Some(owned("gh:o/foo", Some("gh:o/foo@dir:ab"), None)),
                true,
            ),
            (
                "a local directory by listing",
                Some(owned("gh:o/foo", Some("gh:o/foo@listing:ab"), None)),
                true,
            ),
            (
                "a file: ref",
                Some(owned("file:/dl/Foo.zip", Some("x"), None)),
                true,
            ),
            (
                "a release with its archive",
                Some(owned("gh:o/foo", Some("gh:o/foo@1"), Some("aa"))),
                false,
            ),
            (
                "a release whose archive was evicted",
                Some(owned("gh:o/foo", Some("gh:o/foo@1"), None)),
                false,
            ),
        ] {
            assert_eq!(only_copy(owner.as_ref()), expected, "{case}");
        }
    }

    #[test]
    fn only_copies_skip_what_the_transaction_keeps_or_another_warning_names() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("Foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("Foo.toc"), "## Interface: 120105\n").unwrap();
        let inspected = inspect_dir(
            dir.path(),
            &ArchiveLimits::default(),
            &RulesTable::builtin(),
        )
        .unwrap();
        let identical = TreeDigest::compute(&foo).unwrap();
        let other = TreeDigest::from_hex_entries([("Foo.toc", 1, "00")]);
        let foo_folder = || FolderName::new("Foo").unwrap().into_mutable().unwrap();
        let work = |op: SwapOp, local: bool| AddonWork {
            addon: "Foo".into(),
            mode: WorkMode::Folders(Box::new(FolderWork {
                record: None,
                blob: None,
                inspected: Some(inspected.clone()),
                skipped: Vec::new(),
                swap: vec![op],
                transfers: Vec::new(),
                degraded: Vec::new(),
                intent: None,
                edits: Vec::new(),
                handed_over: Vec::new(),
                deps: DepFacts::default(),
                local_changes: local.then(|| LocalChanges {
                    paths: vec!["Foo/Foo.lua".into()],
                    count: 1,
                    folders: vec![FolderName::new("Foo").unwrap()],
                }),
                archive_warnings: Vec::new(),
                input: None,
            })),
        };
        let replace = |indexed: Option<TreeDigest>| SwapOp::Replace {
            name: foo_folder(),
            live: foo_folder(),
            old_tree: indexed,
        };
        let mut record = TargetRecord::empty(TargetKey::new());
        let local = owned("gh:o/foo", Some("gh:o/foo@dir:ab"), None);
        record.addons.insert("foo".into(), local);
        let foo_only = |record: &TargetRecord, works: &[AddonWork]| -> Vec<String> {
            only_copies(record, works)
                .iter()
                .map(|f| f.as_str().to_owned())
                .collect()
        };
        let changed = [work(replace(Some(other)), false)];
        assert_eq!(foo_only(&record, &changed), ["Foo"]);
        let unindexed = [work(replace(None), false)];
        assert_eq!(foo_only(&record, &unindexed), ["Foo"]);
        let removed = SwapOp::Remove {
            live: foo_folder(),
            old_tree: None,
        };
        assert_eq!(foo_only(&record, &[work(removed.clone(), false)]), ["Foo"]);
        // Once, however many works move it.
        let twice = [work(removed.clone(), false), work(removed, false)];
        assert_eq!(foo_only(&record, &twice), ["Foo"]);
        // The same bytes come back: nothing is lost.
        assert!(foo_only(&record, &[work(replace(Some(identical)), false)]).is_empty());
        // Changed by hand: `local_changes_replaced` says it.
        assert!(foo_only(&record, &[work(replace(Some(other)), true)]).is_empty());
        // Added only: nothing moves.
        let added = SwapOp::Add { name: foo_folder() };
        assert!(foo_only(&record, &[work(added, false)]).is_empty());
        // A release's archive is cached: not the only copy.
        record.addons.insert(
            "foo".into(),
            owned("gh:o/foo", Some("gh:o/foo@1"), Some("aa")),
        );
        assert!(foo_only(&record, &changed).is_empty());
    }

    #[test]
    fn the_only_copy_warning_states_the_configured_retention() {
        let retention = RetentionLimits {
            tx: crate::limits::TxRetention {
                min_age: std::time::Duration::from_secs(86_400),
                keep_last: 3,
                window: std::time::Duration::from_secs(14 * 86_400),
            },
            snapshot_max_bytes: 512 << 20,
            ..RetentionLimits::default()
        };
        let twelve: Vec<FolderName> = (0..12)
            .map(|i| FolderName::new(&format!("F{i:02}")).unwrap())
            .collect();
        let tx = TxId::new();
        let w = only_copy_warning("retail", &twelve, Some(tx), &retention).unwrap();
        assert!(matches!(w.code, WarningCode::SnapshotOnlyCopy));
        assert!(
            w.message.starts_with(&format!(
                "On retail, {tx}'s snapshot holds the only copy of F00, F01,"
            )),
            "{}",
            w.message
        );
        assert!(w.message.contains("F09 and 2 more:"), "{}", w.message);
        assert!(
            w.message.ends_with(
                "(kept at least 24 hours; then pruned once 3 newer transactions on this client exist, after 14 days, or past the 512 MiB snapshot cap)"
            ),
            "{}",
            w.message
        );
        let undo = ["omaddons".to_owned(), "undo".to_owned(), tx.to_string()];
        assert_eq!(w.fix.as_ref().unwrap().argv().unwrap(), undo);
        let planned = only_copy_warning("retail", &twelve[..1], None, &retention).unwrap();
        assert!(planned.fix.as_ref().unwrap().argv().is_none());
        assert!(only_copy_warning("retail", &[], Some(tx), &retention).is_none());
        // The default limits, as README states them.
        let edited = LocalChanges {
            paths: vec!["Foo/a.lua".into()],
            count: 1,
            folders: Vec::new(),
        };
        let local = local_changes_warning("Foo", &edited, None, &RetentionLimits::default());
        assert!(
            local.message.contains(
                "until retention prunes the snapshot (kept at least 24 hours; then pruned once 10 newer transactions on this client exist, after 7 days, or past the 2 GiB snapshot cap)"
            ),
            "{}",
            local.message
        );
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
            Some(&["omaddons", "unpin", "Foo", "-t", "retail"].map(String::from)[..])
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
        let catalog = Catalog::default();
        let downloads = DownloadMemo::default();
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
            running_unknown: None,
            known_targets: &[],
            policy: RunningPolicy::Safe,
            limits: ArchiveLimits::default(),
            retention: RetentionLimits::default(),
            actor: Actor::Agent,
            argv: &argv,
            catalog: &catalog,
            default_channel: Channel::Stable,
            downloads: &downloads,
            indexes: None,
        };
        let req = PlanRequest {
            targets: vec![TargetRequest {
                target,
                changes,
                default_kind: TxKind::Install,
                dependency_locks: BTreeMap::new(),
            }],
            force: false,
            while_running: false,
            dry_run: false,
            kind: None,
            no_deps: false,
            witness: None,
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
                let state = TargetState::now(ctx, &target).unwrap();
                let view = released_view(std::slice::from_ref(work));
                let rejudged = rejudge(req, ctx, &target, &state, work, generation, &view).unwrap();
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
