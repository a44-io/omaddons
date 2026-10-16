//! `omaddons adopt [-t T] [--no-verify] [--refresh]`, report mode: which unmanaged
//! folders come from where, and whether they are exactly a release. Changes nothing.
//!
//! Each target's unmanaged folders (inventory at full depth) are identified from their
//! TOCs, the catalog, the Tukui list and the WoWInterface index, and grouped into addons
//! with a tier (M2-D40, `resolve::adopt`). A group whose ref is already recorded on the
//! target is a leftover of a managed addon, reported as `already_managed`. Then, unless
//! `--no-verify`, each `identified` group's newest release is checked as installing it
//! would (downloads through the blob cache, once per command) and compared, file by
//! file, with the folders on disk: equal is `exact`. It writes only the release memo and
//! the blob cache, `net.log` and an in-flight lock while downloading.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use schemars::JsonSchema;
use serde::Serialize;

use super::engine::Engine;
use super::outcome::TargetRef;
use super::status::modes_or_family;
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::engine::plan::Checked;
use crate::engine::tx::CheckJob;
use crate::error::{CommandHint, Error, ErrorCode, Result, Warning};
use crate::inventory::{Depth, inventory};
use crate::model::{FolderName, Ref, VersionConfidence};
use crate::output::{Outcome, Phase, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::adopt::{
    AdoptLookups, AdoptTukui, AdoptWowi, CandidateFolder, Evidence, Group, Strength, Tier,
    TukuiProject, WowiProject, identify, normalise,
};
use crate::resolve::cache::{Fetched, Freshness, Origin};
use crate::resolve::catalog::Catalog;
use crate::resolve::select::select;
use crate::sources::wowi::index::{IndexPolicy, WowiIndex};
use crate::sources::{SourceId, SourceSkipReason, SourceSkipped, Wanted};
use crate::state::files_index::{FolderIndex, index_live_folder};
use crate::state::memo::TukuiList;

/// `adopt`'s own arguments (report mode).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct AdoptArgs {
    /// Identify only: download nothing, compare nothing
    #[arg(long)]
    pub no_verify: bool,
    /// Ask every source now, even when it was checked in the last 15 minutes
    #[arg(long)]
    pub refresh: bool,
}

/// `adopt`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AdoptData {
    /// What checking against releases will do; `null` under `--no-verify`.
    pub verify: Option<VerifyPlan>,
    /// Every selected target, sorted by id.
    pub targets: Vec<AdoptTarget>,
    /// Sources not consulted, and why.
    pub sources_skipped: Vec<SourceSkipped>,
}

/// The exactness checks ahead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VerifyPlan {
    /// Identified groups to check.
    pub groups: usize,
    /// Distinct archives to download (not in the blob cache).
    pub downloads: usize,
    /// Their known size, summed.
    pub bytes: u64,
}

/// One target's report.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AdoptTarget {
    pub target: TargetRef,
    /// Unmanaged entries in its AddOns folder.
    pub unmanaged: u32,
    /// By tier (`exact` first), name, then primary folder.
    pub groups: Vec<AdoptGroup>,
    /// Folders that cannot be adopted here, by name.
    pub failed: Vec<FailedFolder>,
}

/// One addon adoption would record.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AdoptGroup {
    /// Sorted by key.
    pub folders: Vec<FolderName>,
    pub primary: FolderName,
    pub tier: Tier,
    /// `null` for `unknown`.
    pub strength: Option<Strength>,
    #[serde(rename = "ref")]
    pub r#ref: Option<Ref>,
    /// The source's name for it, else the primary folder.
    pub name: String,
    /// The release's for `exact`; else the primary folder's `## Version` (`unknown` when
    /// it has none).
    pub version: String,
    pub version_confidence: VersionConfidence,
    /// `exact` only.
    pub release_id: Option<String>,
    pub evidence: Vec<Evidence>,
    pub alternatives: Vec<Ref>,
    /// Folders the release ships that this client does not load: reported, never
    /// adopted.
    pub unloaded_folders: Vec<FolderName>,
    /// Why an `identified` group is not `exact`; `null` otherwise.
    pub reason: Option<AdoptReason>,
}

/// Why an identified group is not `exact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdoptReason {
    /// The files differ from the newest release's.
    FilesDiffer,
    /// The folders differ from what the newest release installs here.
    FolderSetDiffers,
    /// Not checked: `--no-verify`, the source is off, or a folder cannot be indexed.
    NotVerified,
    /// Not checked: offline without a remembered release and cached archive.
    NotVerifiedOffline,
    /// Checking failed: fetching, downloading or reading the release, or nothing in it
    /// loads here (the cause at `-v`).
    SourceError,
}

/// A folder adoption cannot take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FailedFolder {
    pub folder: String,
    pub reason: FailReason,
}

/// Why a folder cannot be adopted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    /// Its source's ref is already recorded on the target: a leftover of a managed
    /// addon.
    AlreadyManaged,
    /// It holds a symlink, device, FIFO or socket, or cannot be read.
    Unindexable,
    /// It is larger than the archive limits.
    LimitExceeded,
}

/// `adopt`'s outcome.
#[derive(Debug)]
pub struct AdoptOutcome {
    data: AdoptData,
    warnings: Vec<Warning>,
    partial: bool,
    /// Per target, in `data.targets` order: `retail (12.1.5, mainline)`.
    headings: Vec<String>,
    /// The announcement before checking, for humans.
    announcement: Option<String>,
    /// For `-v` log lines.
    pub notes: Vec<String>,
}

impl Outcome for AdoptOutcome {
    type Data = AdoptData;

    fn command(&self) -> &'static str {
        "adopt"
    }

    fn data(&self) -> &AdoptData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn commands(&self) -> &[CommandHint] {
        &[]
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        if let Some(line) = &self.announcement {
            writeln!(w, "{line}")?;
        }
        for (t, heading) in self.data.targets.iter().zip(&self.headings) {
            writeln!(w, "{heading}: {} unmanaged", t.unmanaged)?;
            let width = t
                .groups
                .iter()
                .map(|g| g.name.chars().count())
                .max()
                .unwrap_or(0);
            for g in &t.groups {
                let source = g
                    .r#ref
                    .as_ref()
                    .map_or_else(|| "no source".to_owned(), ToString::to_string);
                let why = g
                    .reason
                    .map(|r| format!(" ({})", reason_text(r)))
                    .unwrap_or_default();
                let folders = if g.folders.len() > 1 {
                    format!(", {} folders", g.folders.len())
                } else {
                    String::new()
                };
                writeln!(
                    w,
                    "  {:<width$}  {:<10}  {source}  {}{folders}{why}",
                    g.name,
                    tier_text(g.tier),
                    g.version
                )?;
            }
            for f in &t.failed {
                writeln!(
                    w,
                    "  {}  cannot be adopted: {}",
                    f.folder,
                    fail_text(f.reason)
                )?;
            }
        }
        Ok(())
    }
}

const fn tier_text(tier: Tier) -> &'static str {
    match tier {
        Tier::Exact => "exact",
        Tier::Identified => "identified",
        Tier::Guess => "guess",
        Tier::CfOnly => "cf_only",
        Tier::Unknown => "unknown",
    }
}

const fn reason_text(reason: AdoptReason) -> &'static str {
    match reason {
        AdoptReason::FilesDiffer => "files differ from the newest release",
        AdoptReason::FolderSetDiffers => "folders differ from the newest release",
        AdoptReason::NotVerified => "not checked",
        AdoptReason::NotVerifiedOffline => "not checked offline",
        AdoptReason::SourceError => "checking failed",
    }
}

const fn fail_text(reason: FailReason) -> &'static str {
    match reason {
        FailReason::AlreadyManaged => "its addon is already managed",
        FailReason::Unindexable => "it holds a link or an unreadable file",
        FailReason::LimitExceeded => "it is larger than the archive limits",
    }
}

/// The Tukui list as adoption reads it.
struct TukuiLookup<'a>(&'a TukuiList);

impl TukuiLookup<'_> {
    fn project(entry: &crate::state::memo::TukuiEntry) -> TukuiProject {
        TukuiProject {
            id: entry.id,
            slug: entry.slug.clone(),
            name: entry.name.clone(),
            directories: entry
                .directories
                .iter()
                .filter_map(|d| FolderName::new(d).ok())
                .collect(),
        }
    }
}

impl AdoptTukui for TukuiLookup<'_> {
    fn by_id(&self, id: i64) -> Option<TukuiProject> {
        self.0
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(Self::project)
    }

    fn by_slug(&self, slug: &str) -> Option<TukuiProject> {
        self.0
            .entries
            .iter()
            .find(|e| e.slug.eq_ignore_ascii_case(slug))
            .map(Self::project)
    }
}

/// The WoWInterface index as adoption reads it, with names normalised once.
struct WowiLookup<'a> {
    index: &'a WowiIndex,
    by_name: BTreeMap<String, Vec<u64>>,
}

impl<'a> WowiLookup<'a> {
    fn new(index: &'a WowiIndex) -> Self {
        let mut by_name: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for e in index.entries() {
            by_name.entry(normalise(&e.name)).or_default().push(e.uid);
        }
        Self { index, by_name }
    }
}

impl AdoptWowi for WowiLookup<'_> {
    fn by_folder(&self, folder: &FolderName) -> Vec<u64> {
        self.index.by_folder(folder).to_vec()
    }

    fn by_uid(&self, uid: u64) -> Option<WowiProject> {
        self.index.by_uid(uid).map(|e| WowiProject {
            uid: e.uid,
            name: e.name.clone(),
            author: e.author.clone(),
            dirs: e.dirs.clone(),
            downloads: e.downloads,
            date_ms: i64::try_from(e.date_ms).unwrap_or(i64::MAX),
        })
    }

    fn by_name(&self, normalised: &str) -> Vec<u64> {
        self.by_name.get(normalised).cloned().unwrap_or_default()
    }
}

/// Where a list could not be had from: offline, rate-limited, else no index.
fn skip_reason(error: &Error) -> SourceSkipReason {
    match error.code {
        ErrorCode::Offline => SourceSkipReason::Offline,
        ErrorCode::RateLimited => SourceSkipReason::RateLimited,
        _ => SourceSkipReason::NoIndex,
    }
}

/// One target's identification, before checking.
struct TargetWork<'w> {
    discovered: &'w DiscoveredTarget,
    unmanaged: u32,
    candidates: Vec<CandidateFolder>,
    groups: Vec<GroupWork>,
    failed: Vec<FailedFolder>,
}

/// One group and what checking it found.
struct GroupWork {
    group: Group,
    reason: Option<AdoptReason>,
    /// `exact`: the release.
    release: Option<(String, String)>,
    /// Folders the release ships that this client does not load.
    unloaded: Vec<FolderName>,
    /// The live folders' indexes, by key, once indexed.
    indexes: BTreeMap<String, FolderIndex>,
}

/// Runs `adopt` in report mode.
pub fn run(ctx: &Context, args: &AdoptArgs) -> Result<AdoptOutcome> {
    let world = World::load()?;
    let mut warnings = world.warnings.clone();
    let mut notes: Vec<String> = Vec::new();
    let mut partial = false;
    let targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    let records = world.records()?;
    let engine = Engine::new(ctx, &world, "adopt")?;
    let enabled = engine.enabled_sources();
    // `--offline` wins over `--refresh`.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    };
    let cache = engine.cache(freshness);

    // Lookups. A source that is off, or a list that cannot be had, is said and skipped.
    let mut sources_skipped: Vec<SourceSkipped> =
        [SourceId::Github, SourceId::Wowi, SourceId::Tukui]
            .into_iter()
            .filter(|id| !enabled.contains(id))
            .map(|source| SourceSkipped {
                source,
                reason: SourceSkipReason::Disabled,
                reset_at: None,
            })
            .collect();
    let (catalog, catalog_warnings) = Catalog::load(&world.paths);
    warnings.extend(catalog_warnings);
    let tukui_list = if enabled.contains(&SourceId::Tukui) {
        match cache.tukui_list() {
            Ok(list) => Some(list),
            Err(error) => {
                notes.push(format!("the Tukui list: {}", error.message));
                sources_skipped.push(SourceSkipped {
                    source: SourceId::Tukui,
                    reason: skip_reason(&error),
                    reset_at: None,
                });
                None
            }
        }
    } else {
        None
    };
    let wowi_index = if enabled.contains(&SourceId::Wowi) {
        match WowiIndex::load_or_refresh(&world.paths, engine.http(), IndexPolicy::from(freshness))
        {
            Ok(load) => {
                warnings.extend(load.warnings.iter().cloned());
                notes.extend(load.diagnostics.iter().cloned());
                sources_skipped.extend(load.skipped());
                load.index
            }
            Err(error) => {
                notes.push(format!("the WoWInterface index: {}", error.message));
                sources_skipped.push(SourceSkipped {
                    source: SourceId::Wowi,
                    reason: skip_reason(&error),
                    reset_at: None,
                });
                None
            }
        }
    } else {
        None
    };
    let tukui = tukui_list.as_ref().map(TukuiLookup);
    let wowi = wowi_index.as_ref().map(WowiLookup::new);

    // Identification, per target.
    let mut works: Vec<TargetWork<'_>> = Vec::new();
    for d in &targets {
        let record = records.get(&d.target.id);
        let inv = inventory(&d.target, record, &world.table, Depth::Full)?;
        let candidates: Vec<CandidateFolder> = inv
            .folders
            .iter()
            .filter_map(CandidateFolder::from_entry)
            .collect();
        let lookups = AdoptLookups {
            family: d.target.family,
            catalog: &catalog,
            tukui: tukui.as_ref().map(|t| t as &dyn AdoptTukui),
            wowi: wowi.as_ref().map(|w| w as &dyn AdoptWowi),
        };
        let recorded: Vec<&Ref> = record
            .map(|r| r.addons.values().map(|a| &a.r#ref).collect())
            .unwrap_or_default();
        let mut failed = Vec::new();
        let mut groups = Vec::new();
        for group in identify(&candidates, &lookups) {
            let r = group.claim.as_ref().and_then(|c| c.r#ref.as_ref());
            if r.is_some_and(|r| recorded.contains(&r)) {
                failed.extend(group.folders.iter().map(|f| FailedFolder {
                    folder: f.as_str().to_owned(),
                    reason: FailReason::AlreadyManaged,
                }));
                continue;
            }
            groups.push(GroupWork {
                reason: (group.tier == Tier::Identified).then_some(AdoptReason::NotVerified),
                group,
                release: None,
                unloaded: Vec::new(),
                indexes: BTreeMap::new(),
            });
        }
        works.push(TargetWork {
            discovered: d,
            unmanaged: inv.counts.unmanaged,
            candidates,
            groups,
            failed,
        });
    }

    // Exactness: each identified group whose source is on, checked against its newest
    // release.
    let mut verify_plan = None;
    if !args.no_verify {
        let channel = engine.default_channel();
        let mut to_check: Vec<(usize, usize, Wanted)> = Vec::new();
        for (t, work) in works.iter_mut().enumerate() {
            let addons = work.discovered.target.addons_dir();
            for (g, gw) in work.groups.iter_mut().enumerate() {
                if gw.group.tier != Tier::Identified {
                    continue;
                }
                let Some(r) = gw.group.claim.as_ref().and_then(|c| c.r#ref.clone()) else {
                    continue;
                };
                if !enabled.contains(&SourceId::of(&r)) {
                    continue;
                }
                if ctx.offline && cache.entry(&r, channel).is_none() {
                    gw.reason = Some(AdoptReason::NotVerifiedOffline);
                    continue;
                }
                let mut indexed = true;
                for folder in &gw.group.folders {
                    match index_live_folder(&addons.join(folder.as_str()), &engine.archive_limits())
                    {
                        Ok(index) => {
                            gw.indexes.insert(folder.key().to_owned(), index);
                        }
                        Err(error) => {
                            let reason = match error.details["reason"].as_str() {
                                Some("limit_exceeded") => FailReason::LimitExceeded,
                                _ => FailReason::Unindexable,
                            };
                            notes.push(format!("adopt: {}", error.message));
                            work.failed.push(FailedFolder {
                                folder: folder.as_str().to_owned(),
                                reason,
                            });
                            indexed = false;
                        }
                    }
                }
                if !indexed {
                    continue;
                }
                to_check.push((
                    t,
                    g,
                    Wanted {
                        r#ref: r,
                        channel,
                        version: None,
                    },
                ));
            }
        }
        let mut wanted: Vec<Wanted> = Vec::new();
        for (_, _, w) in &to_check {
            if !wanted.contains(w) {
                wanted.push(w.clone());
            }
        }
        let answers = cache.get(&wanted);
        report_fetches(ctx, &answers);

        // What will download: each group's top candidate not in the blob cache.
        let mut plan = VerifyPlan::default();
        let mut uncached: BTreeSet<(String, String)> = BTreeSet::new();
        let mut cached = 0usize;
        let mut jobs: Vec<(usize, usize, CheckJob)> = Vec::new();
        for (t, g, w) in to_check {
            let Some(answer) = answers.get(&w) else {
                continue;
            };
            let Some(gw) = works.get_mut(t).and_then(|work| work.groups.get_mut(g)) else {
                continue;
            };
            let set = match &answer.set {
                Ok(set) => set.clone(),
                Err(error) => {
                    gw.reason = Some(if error.code == ErrorCode::Offline {
                        AdoptReason::NotVerifiedOffline
                    } else {
                        AdoptReason::SourceError
                    });
                    notes.push(format!("adopt: {}: {}", w.r#ref, error.message));
                    if error.retryable {
                        warnings.push(Warning::item(error));
                        partial = true;
                    }
                    continue;
                }
            };
            let Some(work) = works.get(t) else { continue };
            let target = &work.discovered.target;
            plan.groups += 1;
            if let Some(top) = select(&set, target, w.channel, None).candidates.first() {
                let known = cache
                    .facts()
                    .hash(&top.release_id, &top.asset.filename)
                    .map(|(sha, _)| sha.to_owned())
                    .or_else(|| match &top.asset.expected_hash {
                        Some(crate::model::ExpectedHash::Sha256(sha)) => Some(sha.clone()),
                        _ => None,
                    });
                let in_cache = known.is_some_and(|sha| world.paths.blob(&sha).is_file());
                if in_cache {
                    cached += 1;
                } else if uncached.insert((top.release_id.clone(), top.asset.filename.clone())) {
                    plan.downloads += 1;
                    plan.bytes += top.asset.size.unwrap_or(0);
                }
            }
            let name_hint = works
                .get(t)
                .and_then(|work| work.groups.get(g))
                .and_then(|gw| gw.group.claim.as_ref())
                .and_then(|c| c.name.clone());
            jobs.push((
                t,
                g,
                CheckJob {
                    target: target.clone(),
                    wanted: w,
                    set,
                    name_hint,
                },
            ));
        }
        let announcement = format!(
            "checking {} identified {} against their newest releases (≈ {}, {cached} already cached)",
            plan.groups,
            if plan.groups == 1 { "addon" } else { "addons" },
            megabytes(plan.bytes)
        );
        ctx.progress
            .announce(Phase::Download, plan.downloads as u64, &announcement);
        verify_plan = Some((plan, announcement));

        let job_list: Vec<CheckJob> = jobs.iter().map(|(_, _, j)| j.clone()).collect();
        let checked = engine.check_releases(&job_list, &cache)?;
        notes.extend(checked.notes.notes.iter().cloned());
        let total = jobs.len() as u64;
        for ((t, g, job), result) in jobs.into_iter().zip(checked.results) {
            let Some(work) = works.get_mut(t) else {
                continue;
            };
            let Some(gw) = work.groups.get_mut(g) else {
                continue;
            };
            ctx.progress.event(
                Phase::Verify,
                total,
                &format!("{} on {}", job.wanted.r#ref, job.target.id),
            );
            match result {
                Ok(Checked::Loads(check)) => compare(gw, &check),
                Ok(Checked::Refused(why)) => {
                    notes.push(format!("adopt: {}: {why}", job.wanted.r#ref));
                    gw.reason = Some(AdoptReason::SourceError);
                }
                Err(error) => {
                    notes.push(format!("adopt: {}: {}", job.wanted.r#ref, error.message));
                    gw.reason = Some(if error.code == ErrorCode::Offline {
                        AdoptReason::NotVerifiedOffline
                    } else {
                        AdoptReason::SourceError
                    });
                    if error.retryable {
                        warnings.push(Warning::item(&error));
                        partial = true;
                    }
                }
            }
        }
        let remembered = cache.remember(&checked.notes.facts);
        warnings.extend(remembered.warnings);
        notes.extend(remembered.notes);
    } else {
        let remembered = cache.remember(&[]);
        warnings.extend(remembered.warnings);
        notes.extend(remembered.notes);
    }

    let mut out_targets = Vec::new();
    let mut headings = Vec::new();
    for work in works {
        let t = &work.discovered.target;
        let modes = world.table.mode_names(t.product.as_deref());
        let v = t.version;
        headings.push(format!(
            "{} ({}.{}.{}, {})",
            t.id,
            v.major,
            v.minor,
            v.patch,
            modes_or_family(&modes, t.family)
        ));
        let mut groups: Vec<AdoptGroup> = work
            .groups
            .into_iter()
            .map(|gw| render(gw, &work.candidates))
            .collect();
        groups.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.primary.key().cmp(b.primary.key()))
        });
        let mut failed = work.failed;
        failed.sort_by_key(|f| f.folder.to_lowercase());
        failed.dedup();
        out_targets.push(AdoptTarget {
            target: TargetRef {
                id: t.id.clone(),
                key: t.key,
                mode: None,
            },
            unmanaged: work.unmanaged,
            groups,
            failed,
        });
    }
    let (verify, announcement) = match verify_plan {
        Some((plan, line)) => (Some(plan), Some(line)),
        None => (None, None),
    };
    let mut unique: Vec<Warning> = Vec::new();
    for w in warnings {
        if !unique.contains(&w) {
            unique.push(w);
        }
    }
    Ok(AdoptOutcome {
        data: AdoptData {
            verify,
            targets: out_targets,
            sources_skipped,
        },
        warnings: unique,
        partial,
        headings,
        announcement,
        notes,
    })
}

/// One released folder: its key and its `(path, sha256)` set.
type ReleasedFolder<'a> = (&'a str, BTreeSet<(&'a str, &'a str)>);

/// Checks a group against its release ([`exactness`]) and records the answer.
fn compare(gw: &mut GroupWork, check: &crate::engine::plan::ReleaseCheck) {
    let installs: Vec<ReleasedFolder<'_>> = check
        .install
        .iter()
        .map(|folder| {
            let files = folder
                .files
                .iter()
                .map(|f| (f.rel_path.as_str(), f.sha256.as_str()))
                .collect();
            (folder.name.key(), files)
        })
        .collect();
    let skipped: BTreeSet<&str> = check.skipped.iter().map(|s| s.name.key()).collect();
    let (reason, unloaded) = exactness(&gw.group.folders, &gw.indexes, &installs, &skipped);
    gw.unloaded = unloaded;
    gw.reason = reason;
    if reason.is_none() {
        gw.group.tier = Tier::Exact;
        gw.release = Some((check.release_id.clone(), check.version.clone()));
    }
}

/// Whether the folders on disk are exactly what the release installs here: the same
/// folders, but those this client does not load (`skipped`, reported as unloaded), and,
/// per folder, the same paths with the same hashes. `None` is exact.
fn exactness(
    present: &[FolderName],
    indexes: &BTreeMap<String, FolderIndex>,
    installs: &[ReleasedFolder<'_>],
    skipped: &BTreeSet<&str>,
) -> (Option<AdoptReason>, Vec<FolderName>) {
    let unloaded: Vec<FolderName> = present
        .iter()
        .filter(|f| skipped.contains(f.key()))
        .cloned()
        .collect();
    let expected: BTreeSet<&str> = present
        .iter()
        .map(FolderName::key)
        .filter(|k| !skipped.contains(k))
        .collect();
    let installed: BTreeSet<&str> = installs.iter().map(|(key, _)| *key).collect();
    if installed != expected {
        return (Some(AdoptReason::FolderSetDiffers), unloaded);
    }
    let same = installs.iter().all(|(key, released)| {
        indexes.get(*key).is_some_and(|live| {
            let on_disk: BTreeSet<(&str, &str)> = live
                .files
                .iter()
                .map(|f| (f.path.as_str(), f.sha256.as_str()))
                .collect();
            on_disk == *released
        })
    });
    let reason = (!same).then_some(AdoptReason::FilesDiffer);
    (reason, unloaded)
}

/// A group as reported.
fn render(gw: GroupWork, candidates: &[CandidateFolder]) -> AdoptGroup {
    let toc_version = candidates
        .iter()
        .find(|c| c.folder == gw.group.primary)
        .and_then(|c| c.toc.as_ref())
        .and_then(|t| t.version.clone())
        .filter(|v| !v.trim().is_empty() && !v.contains("@project-version@"))
        .unwrap_or_else(|| "unknown".to_owned());
    let claim = gw.group.claim;
    let (version, version_confidence, release_id) = match gw.release {
        Some((release_id, version)) => (version, VersionConfidence::Release, Some(release_id)),
        None => (toc_version, VersionConfidence::Toc, None),
    };
    AdoptGroup {
        name: claim
            .as_ref()
            .and_then(|c| c.name.clone())
            .unwrap_or_else(|| gw.group.primary.as_str().to_owned()),
        folders: gw.group.folders,
        primary: gw.group.primary,
        tier: gw.group.tier,
        strength: claim.as_ref().map(|c| c.strength),
        r#ref: claim.as_ref().and_then(|c| c.r#ref.clone()),
        version,
        version_confidence,
        release_id,
        evidence: claim.map(|c| c.evidence).unwrap_or_default(),
        alternatives: gw.group.alternatives,
        unloaded_folders: gw.unloaded,
        reason: gw.reason,
    }
}

/// One `fetch` progress event per ref fetched from its source now.
fn report_fetches(ctx: &Context, answers: &BTreeMap<Wanted, Fetched>) {
    let fetched: Vec<&Wanted> = answers
        .iter()
        .filter(|(_, f)| f.origin == Origin::Fetched)
        .map(|(w, _)| w)
        .collect();
    for w in &fetched {
        ctx.progress
            .event(Phase::Fetch, fetched.len() as u64, &w.r#ref.to_string());
    }
}

/// `63 MB`, `0.4 MB`.
fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::files_index::FileEntry;

    fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    /// A live folder's index of `(path, sha256)` files.
    fn live(name: &str, files: &[(&str, &str)]) -> (String, FolderIndex) {
        let entries = files
            .iter()
            .map(|(path, sha)| FileEntry {
                path: (*path).to_owned(),
                size: 1,
                mtime_ns: 0,
                sha256: (*sha).to_owned(),
            })
            .collect();
        (name.to_ascii_lowercase(), FolderIndex::new(name, entries))
    }

    fn released<'a>(key: &'a str, files: &[(&'a str, &'a str)]) -> ReleasedFolder<'a> {
        (key, files.iter().copied().collect())
    }

    #[test]
    fn exactness_needs_the_same_folders_and_files() {
        let present = [f("Foo"), f("Foo_Options")];
        let indexes: BTreeMap<String, FolderIndex> = [
            live("Foo", &[("Foo.toc", "aa"), ("core.lua", "bb")]),
            live("Foo_Options", &[("Foo_Options.toc", "cc")]),
        ]
        .into();
        let foo = released("foo", &[("Foo.toc", "aa"), ("core.lua", "bb")]);
        let options = released("foo_options", &[("Foo_Options.toc", "cc")]);
        let none = BTreeSet::new();
        let check = |installs: &[ReleasedFolder<'_>], skipped: &BTreeSet<&str>| {
            exactness(&present, &indexes, installs, skipped)
        };

        assert_eq!(
            check(&[foo.clone(), options.clone()], &none),
            (None, vec![])
        );
        let changed = released("foo", &[("Foo.toc", "aa"), ("core.lua", "XX")]);
        assert_eq!(
            check(&[changed, options.clone()], &none).0,
            Some(AdoptReason::FilesDiffer)
        );
        let extra = released(
            "foo",
            &[("Foo.toc", "aa"), ("core.lua", "bb"), ("new.lua", "dd")],
        );
        assert_eq!(
            check(&[extra, options.clone()], &none).0,
            Some(AdoptReason::FilesDiffer),
            "a file the release has and the disk lacks"
        );
        assert_eq!(
            check(std::slice::from_ref(&foo), &none).0,
            Some(AdoptReason::FolderSetDiffers),
            "a folder on disk the release does not install"
        );
        let more = released("foo_more", &[("Foo_More.toc", "ee")]);
        assert_eq!(
            check(&[foo.clone(), options.clone(), more], &none).0,
            Some(AdoptReason::FolderSetDiffers),
            "a folder the release installs that is not on disk"
        );
        // A folder this client does not load is unloaded, and not held against it.
        let skipped: BTreeSet<&str> = ["foo_options"].into();
        assert_eq!(check(&[foo], &skipped), (None, vec![f("Foo_Options")]));
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(megabytes(63_000_000), "63 MB");
        assert_eq!(megabytes(400_000), "0.4 MB");
        assert_eq!(megabytes(0), "0.0 MB");
    }
}
