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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::blobs::{BlobCache, Storage};
use super::inspect::{ArchiveLimits, Inspected, compatible, inspect, inspect_dir, verify};
use super::ownership::{self, AllowedOp, FolderChange, Transfer};
use super::running::{self, PolicyContext, PolicyOp};
use super::swap::SwapOp;
use crate::error::{Error, ErrorBody, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{ScopeTable, TreeDigest, failpoint};
use crate::inventory::{Depth, Inventory, inventory};
use crate::model::{
    Actor, AddonReason, AddonRecord, AddonState, AssetLocation, Candidate, Channel, ExpectedHash,
    FolderName, Ref, ReleaseSet, ReloadRequired, Target, TargetKey, TxId, TxKind,
    VersionConfidence,
};
use crate::net::{Http, Request};
use crate::resolve::select::{EmptyReason, select};
use crate::rules::RulesTable;
use crate::sources::{SourceRegistry, Wanted, fetch_once};
use crate::state::config::RunningPolicy;
use crate::state::files_index::FilesIndex;
use crate::state::paths::Paths;
use crate::state::records::{TargetRecord, load_record};
use crate::state::store;
use crate::toc::{DiskFolder, loaded_toc};

/// The plan format's version.
pub const SCHEMA_VERSION: &str = "1.0";

/// What a command asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    pub targets: Vec<TargetRequest>,
    pub force: bool,
    pub while_running: bool,
    pub dry_run: bool,
}

/// The changes for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRequest {
    pub target: Target,
    pub changes: Vec<RequestedChange>,
}

/// One requested change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedChange {
    Install {
        r#ref: Ref,
        channel: Channel,
        /// `--version`, or a release URL's tag.
        version: Option<String>,
        /// `--file <zip>`: this archive instead of the release's assets.
        file: Option<PathBuf>,
        /// The name a new addon is recorded under (the catalog's); `None` derives it
        /// from the archive's folders. An installed addon keeps its own.
        name: Option<String>,
    },
    Remove {
        addon: String,
    },
    /// Update in M1: install the newest compatible release of an installed addon.
    Reinstall {
        addon: String,
    },
}

/// What planning reads.
pub struct PlanCtx<'a> {
    pub paths: &'a Paths,
    pub scopes: &'a ScopeTable,
    pub http: &'a dyn Http,
    pub registry: &'a SourceRegistry,
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
    /// The record generation planned against.
    #[serde(skip)]
    pub generation: u64,
    /// What the commit stages, swaps and records.
    #[serde(skip)]
    pub work: Vec<AddonWork>,
}

/// One addon changing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Change {
    pub addon: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub kind: ChangeKind,
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
}

/// Folders by what happens to them (live spellings for replaced and removed ones).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderSummary {
    pub added: Vec<FolderName>,
    pub replaced: Vec<FolderName>,
    pub removed: Vec<FolderName>,
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

/// Everything the commit needs for one addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonWork {
    pub addon: String,
    /// The record after the change; `None` when the addon is removed.
    pub record: Option<AddonRecord>,
    /// The zip staging reads; `None` for directory inputs (their files name sources).
    pub blob: Option<PathBuf>,
    pub inspected: Option<Inspected>,
    pub swap: Vec<SwapOp>,
    pub transfers: Vec<Transfer>,
    /// Addon keys that become degraded.
    pub degraded: Vec<String>,
}

/// A change after its ref is known, before fetching.
enum Want {
    Install {
        /// The installed addon's name, for a reinstall.
        name: Option<String>,
        wanted: Wanted,
        file: Option<PathBuf>,
    },
    Remove {
        addon: String,
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
        .flat_map(|(_, _, _, wants)| wants)
        .filter_map(|w| match w {
            Want::Install {
                wanted, file: None, ..
            } => Some(wanted.clone()),
            _ => None,
        })
        .collect();
    let fetched = fetch_once(to_fetch, ctx.registry, ctx.http);
    failpoint::hit("engine.plan.fetched").map_err(|e| fail(ctx, &e))?;

    let mut targets = Vec::with_capacity(prepared.len());
    let mut failures = Vec::new();
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
            generation: record.generation,
            work: Vec::new(),
        };
        let live = match inventory(target, Some(&record), ctx.table, Depth::Classes) {
            Ok(live) => live,
            Err(error) => {
                failures.push(failure(None, None, target, error));
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
        };
        for w in wants {
            let (r, addon, result) = match w {
                Want::Failed {
                    r#ref,
                    addon,
                    error,
                } => (r#ref, addon, Err(error)),
                Want::Install { name, wanted, file } => {
                    let set = match &file {
                        Some(_) => None,
                        None => Some(fetched.get(&wanted).cloned().unwrap_or_else(|| {
                            Err(Error::internal(format!("{} was not fetched", wanted.r#ref)))
                        })),
                    };
                    let result = planner.install(name.as_deref(), &wanted, file.as_deref(), set);
                    (Some(wanted.r#ref.clone()), name, result)
                }
                Want::Remove { addon } => {
                    let result = planner.remove(&addon);
                    (None, Some(addon), result)
                }
            };
            match result {
                Ok(Some(planned)) => planned.merge_into(&mut tp),
                Ok(None) => {}
                Err(error) => failures.push(failure(r, addon, target, error)),
            }
        }
        tp.kind = target_kind(&tp.changes);
        targets.push(tp);
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
/// archive is already inspected and staged. `Ok(None)` when the change is no longer
/// needed (a concurrent transaction installed the same release, or removed the
/// addon). A refusal is the caller's `plan_stale`.
pub fn rejudge(
    req: &PlanRequest,
    ctx: &PlanCtx<'_>,
    target: &Target,
    work: &AddonWork,
) -> Result<Option<PlannedAddon>> {
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
    };
    match (&work.record, &work.inspected) {
        (Some(template), Some(inspected)) => {
            let existing = record.addons.values().find(|a| a.r#ref == template.r#ref);
            if let Some(existing) = existing
                && existing.release_id == template.release_id
                && existing
                    .folders
                    .iter()
                    .all(|f| live.folders.iter().any(|e| e.key == f.key()))
            {
                return Ok(None);
            }
            planner
                .build_install(
                    existing,
                    template.clone(),
                    inspected.clone(),
                    work.blob.clone(),
                    None,
                    Vec::new(),
                )
                .map(Some)
        }
        _ if !record
            .addons
            .contains_key(&TargetRecord::addon_key(&work.addon)) =>
        {
            Ok(None)
        }
        _ => planner.remove(&work.addon),
    }
}

/// A requested change with its ref resolved against the target's record.
fn want(change: &RequestedChange, record: &TargetRecord) -> Want {
    match change {
        RequestedChange::Install {
            r#ref,
            channel,
            version,
            file,
            name,
        } => Want::Install {
            name: name.clone(),
            wanted: Wanted {
                r#ref: r#ref.clone(),
                channel: *channel,
                version: version.clone(),
            },
            file: file.clone(),
        },
        RequestedChange::Remove { addon } => Want::Remove {
            addon: addon.clone(),
        },
        RequestedChange::Reinstall { addon } => {
            match record.addons.get(&TargetRecord::addon_key(addon)) {
                Some(installed) => Want::Install {
                    name: Some(installed.name.clone()),
                    wanted: Wanted {
                        r#ref: installed.r#ref.clone(),
                        channel: installed.channel,
                        version: installed.pinned.then(|| installed.version.clone()),
                    },
                    file: None,
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

/// The transaction kind for a target's changes.
pub fn target_kind(changes: &[Change]) -> Option<TxKind> {
    if changes.is_empty() {
        None
    } else if changes.iter().all(|c| c.kind == ChangeKind::Remove) {
        Some(TxKind::Remove)
    } else if changes.iter().any(|c| c.kind == ChangeKind::Update) {
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

impl PlannedAddon {
    fn merge_into(self, tp: &mut TargetPlan) {
        tp.changes.push(self.change);
        tp.ops.extend(self.ops);
        tp.releases.extend(self.release);
        tp.reload_required = tp.reload_required.max(self.reload_required);
        for warning in self.warnings {
            if !tp.warnings.contains(&warning) {
                tp.warnings.push(warning);
            }
        }
        tp.work.push(self.work);
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
    inspected: Inspected,
    blob: Option<PathBuf>,
    warnings: Vec<Warning>,
}

impl TargetPlanner<'_> {
    fn install(
        &self,
        name: Option<&str>,
        wanted: &Wanted,
        file: Option<&std::path::Path>,
        set: Option<Result<ReleaseSet>>,
    ) -> Result<Option<PlannedAddon>> {
        let existing = self
            .record
            .addons
            .values()
            .find(|a| a.r#ref == wanted.r#ref);
        let candidates = match (file, set) {
            (Some(file), _) => vec![file_candidate(file, wanted)],
            (None, Some(set)) => {
                let set = set?;
                let selection =
                    select(&set, self.target, wanted.channel, wanted.version.as_deref());
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
            (None, None) => return Err(Error::internal("an install without a release set")),
        };
        if let (Some(existing), Some(first)) = (existing, candidates.first())
            && existing.release_id.as_deref() == Some(first.release_id.as_str())
            && existing
                .folders
                .iter()
                .all(|f| self.live.folders.iter().any(|e| e.key == f.key()))
        {
            return Ok(None);
        }

        let user_supplied =
            file.is_some() || matches!(wanted.r#ref, Ref::File { .. } | Ref::Url { .. });
        let mut tried: Vec<Value> = Vec::new();
        let mut winner = None;
        for candidate in candidates {
            match self.try_candidate(&candidate) {
                Ok(Ok(won)) => {
                    winner = Some(won);
                    break;
                }
                Ok(Err(reason)) | Err(reason) => {
                    tried.push(json!({ "asset": candidate.asset.filename, "reason": reason }));
                }
            }
        }
        let Some(winner) = winner else {
            if user_supplied {
                return Err(Error::new(
                    ErrorCode::WrongFlavor,
                    format!("{} does not load on {} ({:?})", wanted.r#ref, self.target.id, self.target.family),
                    Suggestion::text("use an archive built for this client"),
                )
                .details(json!({ "ref": wanted.r#ref.to_string(), "target": self.target.id, "tried": tried })));
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
            .unwrap_or_else(|| addon_name(&wanted.r#ref, &winner.inspected));
        let template = AddonRecord {
            r#ref: wanted.r#ref.clone(),
            name: addon,
            version: winner.candidate.version.clone(),
            version_confidence: VersionConfidence::Release,
            release_id: Some(winner.candidate.release_id.clone()),
            channel: wanted.channel,
            pinned: wanted.version.is_some(),
            reason: AddonReason::Explicit,
            required_by: Vec::new(),
            folders: Vec::new(),
            state: AddonState::Ok,
            installed_at: self.created_at,
            tx_id: self.ctx.tx,
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
            blob_sha256: winner.inspected.blob_sha256.clone(),
        };
        self.build_install(
            existing,
            template,
            winner.inspected,
            winner.blob,
            Some(release),
            winner.warnings,
        )
        .map(Some)
    }

    /// Folder changes, ownership, policy and swap ops for installing `inspected` as
    /// `template` (whose folders and dependents are filled in here).
    fn build_install(
        &self,
        existing: Option<&AddonRecord>,
        mut template: AddonRecord,
        inspected: Inspected,
        blob: Option<PathBuf>,
        release: Option<PlannedRelease>,
        mut warnings: Vec<Warning>,
    ) -> Result<PlannedAddon> {
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
        let mut summary = FolderSummary::default();
        let ops = self.ops(&template.name, &judged.allowed, &mut summary);
        let kind = if existing.is_some() {
            ChangeKind::Update
        } else {
            ChangeKind::Install
        };
        template.folders = new_folders;
        template.required_by = existing.map(|e| e.required_by.clone()).unwrap_or_default();
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
                record: Some(template),
                blob,
                swap: self.swap_ops(&judged.allowed),
                inspected: Some(inspected),
                transfers: judged.transfers,
                degraded: judged.degraded,
            },
        })
    }

    fn remove(&self, addon: &str) -> Result<Option<PlannedAddon>> {
        let Some(installed) = self.record.addons.get(&TargetRecord::addon_key(addon)) else {
            return Err(not_managed(addon));
        };
        let changes: Vec<FolderChange> = installed
            .folders
            .iter()
            .cloned()
            .map(FolderChange::Remove)
            .collect();
        let judged = self.judge(&installed.name, &changes, |_| BTreeSet::new())?;
        let mut summary = FolderSummary::default();
        let ops = self.ops(&installed.name, &judged.allowed, &mut summary);
        Ok(Some(PlannedAddon {
            change: Change {
                addon: installed.name.clone(),
                r#ref: installed.r#ref.clone(),
                kind: ChangeKind::Remove,
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
                record: None,
                blob: None,
                inspected: None,
                swap: self.swap_ops(&judged.allowed),
                transfers: judged.transfers,
                degraded: judged.degraded,
            },
        }))
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
        let loaded = |live: &FolderName| {
            DiskFolder::new(self.target.addons_dir().join(live.as_str()))
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

    /// Downloads, inspects and verifies one candidate: `Ok(Ok(winner))`, or the reason
    /// it was rejected.
    fn try_candidate(
        &self,
        candidate: &Candidate,
    ) -> std::result::Result<std::result::Result<Winner, String>, String> {
        let ctx = self.ctx;
        let reason = |e: Error| format!("{}: {}", e.code.wire(), e.message);
        let (inspected, blob) = match &candidate.asset.location {
            AssetLocation::Url(url) => {
                let blob = self
                    .download(url, candidate.asset.expected_hash.as_ref())
                    .map_err(reason)?;
                let inspected = inspect(&blob, &ctx.limits, ctx.table).map_err(reason)?;
                (inspected, Some(blob))
            }
            AssetLocation::LocalZip(path) => (
                inspect(path, &ctx.limits, ctx.table).map_err(reason)?,
                Some(path.clone()),
            ),
            AssetLocation::LocalDir(path) => (
                inspect_dir(path, &ctx.limits, ctx.table).map_err(reason)?,
                None,
            ),
            AssetLocation::GitCheckout { .. } => {
                return Err("source_disabled: git checkouts arrive with the git sources".to_owned());
            }
        };
        let verdicts = verify(&inspected, self.target, ctx.table).map_err(reason)?;
        if !compatible(&verdicts) {
            let why: Vec<String> = verdicts
                .iter()
                .map(|v| {
                    format!(
                        "{}: {}",
                        v.folder.as_str(),
                        serde_json::to_value(&v.report.verdict)
                            .map(|j| j.to_string())
                            .unwrap_or_default()
                    )
                })
                .collect();
            return Ok(Err(format!("wrong_flavor: {}", why.join("; "))));
        }
        let warnings = verdicts
            .into_iter()
            .flat_map(|v| v.report.warnings)
            .collect();
        Ok(Ok(Winner {
            candidate: candidate.clone(),
            inspected,
            blob,
            warnings,
        }))
    }

    /// The blob for `url`: the cached one when the expected hash is known and present,
    /// else a download (retried once on a hash mismatch).
    fn download(&self, url: &str, expected: Option<&ExpectedHash>) -> Result<PathBuf> {
        let ctx = self.ctx;
        if let Some(ExpectedHash::Sha256(sha)) = expected
            && let Some(blob) = ctx.blobs.get(ctx.scopes, sha)?
        {
            return Ok(blob.path);
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
                    return Ok(blob.path);
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

fn reason_text(reason: EmptyReason) -> &'static str {
    match reason {
        EmptyReason::NoEligibleRelease => "no release on this channel",
        EmptyReason::NoAssets => "the release has no installable archive",
        EmptyReason::PinnedVersionNotFound => "no such release",
    }
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

/// A new addon's name: the folder named like the repository, else the folder the
/// others extend (`WeakAuras` for `WeakAurasOptions`), else the one core folder
/// (`DBM-Core` beside `DBM-GUI`), else the shortest.
pub fn addon_name(r#ref: &Ref, inspected: &Inspected) -> String {
    let names: Vec<&str> = inspected.folders.iter().map(|f| f.name.as_str()).collect();
    if let Ref::GitHub { repo, .. } = r#ref
        && let Some(same) = names.iter().find(|n| n.eq_ignore_ascii_case(repo))
    {
        return (*same).to_owned();
    }
    let extended = |n: &str| {
        names
            .iter()
            .filter(|o| {
                o.len() > n.len() && o.to_ascii_lowercase().starts_with(&n.to_ascii_lowercase())
            })
            .count()
    };
    let mut ranked = names.clone();
    ranked.sort_by(|a, b| {
        extended(b)
            .cmp(&extended(a))
            .then(a.len().cmp(&b.len()))
            .then(a.cmp(b))
    });
    let core = |n: &&&str| {
        let n = n.to_ascii_lowercase();
        n.ends_with("-core") || n.ends_with("_core")
    };
    if ranked.first().is_some_and(|top| extended(top) == 0)
        && let [only] = names.iter().filter(core).collect::<Vec<_>>().as_slice()
    {
        return (**only).to_owned();
    }
    ranked
        .first()
        .map_or_else(|| r#ref.to_string(), |n| (*n).to_owned())
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

    #[test]
    fn target_kinds() {
        let change = |kind| Change {
            addon: "a".into(),
            r#ref: "gh:o/r".parse().unwrap(),
            kind,
            from_version: None,
            to_version: None,
            folders: FolderSummary::default(),
        };
        assert_eq!(target_kind(&[]), None);
        assert_eq!(
            target_kind(&[change(ChangeKind::Remove)]),
            Some(TxKind::Remove)
        );
        assert_eq!(
            target_kind(&[change(ChangeKind::Install), change(ChangeKind::Remove)]),
            Some(TxKind::Install)
        );
        assert_eq!(
            target_kind(&[change(ChangeKind::Install), change(ChangeKind::Update)]),
            Some(TxKind::Update)
        );
    }
}
