//! `omaddons info <ref|addon> [-t T]`: what an addon is and what installing it would
//! get. The installed records across targets, the catalog entry, and the newest stable
//! release with how likely it is to load on each target (what plans proved, its
//! metadata, the catalog's families: nothing is downloaded or verified; `install`
//! verifies). `--changelog` adds the release notes of
//! every release newer than the oldest installed one (`sources::changelog`).
//!
//! Release metadata comes through the release memo: a check from the last 15 minutes is
//! reused (`--refresh` fetches anyway). `--offline` answers from the memo whatever its
//! age, with `stale_check`, or with `latest: null` when nothing is remembered; it never
//! makes a request. A failed fetch for an addon known locally is a partial success; for
//! one known nowhere, the command's error.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::drift::DriftSummary;
use super::history::TargetRef;
use super::resolve_args::{aimed_at, for_family, parse_ref};
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::drift::{DriftMode, DriftState};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{
    AddonReason, AddonState, Channel, FolderName, Ref, ReleaseSet, Target, TxId, VersionConfidence,
};
use crate::net::{NetConfig, NetLog, stack};
use crate::output::{Outcome, Style};
use crate::politeness::SOURCE_RECHECK_MIN_INTERVAL;
use crate::resolve::cache::{Freshness, ReleaseCache};
use crate::resolve::catalog::Catalog;
use crate::resolve::select::{newest, select};
use crate::resolve::updates::{Confidence, rank_confidence};
use crate::rules::Family;
use crate::sources::changelog::{
    Changelog, ChangelogUnavailable, Installed, changelog_for, human_lines, oldest_since,
};
use crate::sources::git_hosts::GitRunner;
use crate::sources::{SourceRegistry, Wanted};
use crate::state::memo::FactIndex;

/// `info`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InfoArgs {
    /// The addon: a catalog name, an installed addon's name, or a ref or link
    #[arg(value_name = "REF")]
    pub query: String,
    /// Fetch the newest release from its source, even if it was checked recently
    #[arg(long)]
    pub refresh: bool,
    /// Add the release notes of every release newer than the oldest installed one
    #[arg(long)]
    pub changelog: bool,
}

/// `info`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InfoData {
    /// The argument exactly as given.
    pub query: String,
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub name: Option<String>,
    pub catalog: Option<CatalogInfo>,
    pub installed: Vec<InstalledInfo>,
    pub latest: Option<Latest>,
    pub offline: bool,
    /// `--changelog` only: the notes of the releases newer than the oldest installed one
    /// (the latest's alone when none records a release), or `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<Option<Changelog>>,
    /// Why `changelog` is `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog_unavailable: Option<ChangelogUnavailable>,
}

/// The catalog entry.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogInfo {
    pub name: String,
    /// The variant shown, when the name has several (M3-D16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub aliases: Vec<String>,
    pub families: Vec<Family>,
    pub folders: Vec<FolderName>,
    pub notes: Option<String>,
    /// The name's other variants, and the families each is for.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<CatalogVariant>,
}

/// Another variant of a catalog name.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogVariant {
    pub variant: String,
    pub families: Vec<Family>,
}

/// One installed record.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstalledInfo {
    pub target: TargetRef,
    pub version: String,
    pub version_confidence: VersionConfidence,
    pub channel: Channel,
    pub pinned: bool,
    pub state: AddonState,
    /// `explicit` when a user asked for it, `dependency` when only the resolver did.
    pub reason: AddonReason,
    /// The addons that require it here, sorted.
    pub required_by: Vec<String>,
    /// A dependency nothing requires any more.
    pub orphan: bool,
    pub folders: Vec<FolderName>,
    /// Whether its files changed on disk (stat-first, local), with paths under `-v`.
    pub drift: DriftSummary,
    #[schemars(with = "String")]
    pub installed_at: jiff::Timestamp,
    pub tx: TxId,
    /// `-v` only: the record file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
}

/// The newest stable release.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Latest {
    pub version: String,
    pub release_id: String,
    pub channel: Channel,
    /// release.json flavor names, when the release has them.
    pub flavors: Vec<String>,
    /// The selected targets it is `verified`, `declared` or `likely` to load on, sorted.
    pub targets: Vec<String>,
    /// For each selected target, how likely it is to load ([`judge`]). A hint only:
    /// `install` verifies every TOC.
    pub compat: std::collections::BTreeMap<String, LoadHint>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_urls: Option<Vec<String>>,
}

/// How likely the newest release is to load on one client: `outdated`'s words, plus
/// `blocked` for what a plan proved does not load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoadHint {
    /// A plan verified a candidate loads here.
    Verified,
    /// Its release metadata names this client.
    Declared,
    /// Nothing says otherwise.
    Likely,
    /// The metadata names other clients only, or the catalog none of its families.
    Unlikely,
    /// A plan verified that no candidate loads here.
    Blocked,
}

impl LoadHint {
    fn likely(self) -> bool {
        matches!(self, Self::Verified | Self::Declared | Self::Likely)
    }
}

/// `set`'s newest stable release on `target`, by the first rule that says something:
/// what plans proved (`facts`); else what its best candidate's metadata says (it
/// describes this very release); else the catalog's `families`, when it lists any (a
/// `git:` or `url:` release carries no metadata); else likely.
fn judge(set: &ReleaseSet, target: &Target, facts: &FactIndex, families: &[Family]) -> LoadHint {
    let candidates = select(set, target, Channel::Stable, None).candidates;
    let Some(best) = candidates.first() else {
        return LoadHint::Unlikely;
    };
    let rules = target.rules.name.as_str();
    let verdicts: Vec<_> = candidates
        .iter()
        .map(|c| facts.verdict(&c.release_id, &c.asset.filename, rules, target.interface))
        .collect();
    if verdicts.iter().flatten().any(|v| v.loads) {
        return LoadHint::Verified;
    }
    if verdicts.iter().all(|v| v.is_some_and(|v| !v.loads)) {
        return LoadHint::Blocked;
    }
    match rank_confidence(best.rank) {
        Confidence::Declared => LoadHint::Declared,
        Confidence::Unlikely => LoadHint::Unlikely,
        Confidence::Verified | Confidence::Likely if families.is_empty() => LoadHint::Likely,
        Confidence::Verified | Confidence::Likely if families.contains(&target.family) => {
            LoadHint::Likely
        }
        Confidence::Verified | Confidence::Likely => LoadHint::Unlikely,
    }
}

/// `info`'s outcome.
#[derive(Debug)]
pub struct InfoOutcome {
    data: InfoData,
    warnings: Vec<Warning>,
    /// The fetch failed for an addon known locally.
    partial: bool,
    /// For `-v` log lines: the release memo ignored or not written.
    pub notes: Vec<String>,
}

impl Outcome for InfoOutcome {
    type Data = InfoData;

    fn command(&self) -> &'static str {
        "info"
    }

    fn data(&self) -> &InfoData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn partial(&self) -> bool {
        self.partial
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let name = d.name.as_deref().unwrap_or(&d.query);
        writeln!(w, "{name}  {}", d.r#ref)?;
        if let Some(c) = &d.catalog {
            let folders: Vec<&str> = c.folders.iter().map(FolderName::as_str).collect();
            let families = |list: &[Family]| -> String {
                list.iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let variant = c
                .variant
                .as_ref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            writeln!(
                w,
                "  catalog{variant}: folders {} · families {}",
                folders.join(", "),
                families(&c.families)
            )?;
            for other in &c.variants {
                writeln!(
                    w,
                    "  other build ({}): families {}",
                    other.variant,
                    families(&other.families)
                )?;
            }
        }
        for i in &d.installed {
            let confidence = serde_json::to_value(i.version_confidence)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let channel = serde_json::to_value(i.channel)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let drift = match i.drift.state {
                DriftState::Modified => {
                    let files = if i.drift.files == 1 { "file" } else { "files" };
                    format!(" · {} {files} changed on disk", i.drift.files)
                }
                DriftState::Missing => " · a folder is missing".to_owned(),
                DriftState::Unindexed => " · not indexed".to_owned(),
                DriftState::Clean => String::new(),
            };
            let dependency = if i.orphan {
                " · a dependency nothing requires".to_owned()
            } else if i.reason == AddonReason::Dependency {
                format!(" · dependency of {}", i.required_by.join(", "))
            } else {
                String::new()
            };
            writeln!(
                w,
                "  {}: {} ({confidence}, {channel}) installed {}{drift}{dependency}",
                i.target.id,
                i.version,
                super::history::local_time(i.installed_at)
            )?;
        }
        match (&d.latest, d.offline) {
            (Some(l), _) => {
                let not: Vec<&str> = l
                    .compat
                    .iter()
                    .filter(|(_, hint)| !hint.likely())
                    .map(|(id, _)| id.as_str())
                    .collect();
                let likely = l.targets.join(", ");
                let line = match (l.targets.is_empty(), not.is_empty()) {
                    (true, _) => format!(
                        "latest: {} · not likely to load on any client here",
                        l.version
                    ),
                    (false, true) => format!("latest: {} · likely to load on {likely}", l.version),
                    (false, false) => format!(
                        "latest: {} · likely to load on {likely} · not on {}",
                        l.version,
                        not.join(", ")
                    ),
                };
                writeln!(w, "  {line}")?;
                if style.verbose > 0 {
                    for (id, hint) in &l.compat {
                        let hint = serde_json::to_value(hint).unwrap_or_default();
                        writeln!(w, "    {id}: {}", hint.as_str().unwrap_or_default())?;
                    }
                }
            }
            (None, true) => writeln!(w, "  latest: not checked (offline)")?,
            (None, false) => writeln!(w, "  latest: unavailable")?,
        }
        if let Some(changelog) = &d.changelog {
            writeln!(w, "  changelog:")?;
            for line in human_lines(changelog.as_ref(), d.changelog_unavailable) {
                writeln!(w, "    {line}")?;
            }
        }
        Ok(())
    }
}

/// Runs `info`.
pub fn run(ctx: &Context, args: &InfoArgs) -> Result<InfoOutcome> {
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let (catalog, catalog_warnings) = Catalog::load(&world.paths);
    warnings.extend(catalog_warnings);
    let records = world.records()?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let parsed = parse_ref(
        &world.config.git_hosts,
        &records,
        &catalog,
        &cwd,
        &args.query,
    )
    .map_err(|e| {
        if matches!(e.code, ErrorCode::NotFound) {
            not_found(&args.query)
        } else {
            aimed_at(e, &ctx.argv, &args.query)
        }
    })?;
    let targets: Vec<&DiscoveredTarget> = world.select_many(ctx)?;
    // A catalog name with variants means the selected client's (one `-t`, else the
    // default target), else its default variant.
    let family = match targets.as_slice() {
        [one] => Some(one.target.family),
        _ => world
            .default_target(&ctx.argv)
            .and_then(Result::ok)
            .map(|d| d.target.family),
    };
    let parsed = match family {
        Some(family) => for_family(&parsed, &catalog, family, &args.query)?,
        None => parsed,
    };
    let r = parsed.r#ref.clone();

    let mut installed = Vec::new();
    // Each installed release, for the changelog's oldest.
    let mut releases: Vec<Installed<'_>> = Vec::new();
    for d in &targets {
        let Some(record) = records.get(&d.target.id) else {
            continue;
        };
        let Some(key) = d.target.key else { continue };
        let mut report = None;
        for a in record.addons.values().filter(|a| a.r#ref == r) {
            if let Some(release_id) = &a.release_id {
                releases.push(Installed {
                    release_id,
                    version: &a.version,
                    installed_at: a.installed_at,
                });
            }
            let report = report.get_or_insert_with(|| {
                super::drift::report(&world.paths, &d.target, record, DriftMode::Stat)
            });
            let drift = report
                .addons
                .iter()
                .find(|r| r.addon.eq_ignore_ascii_case(&a.name))
                .map_or(
                    DriftSummary {
                        state: DriftState::Clean,
                        files: 0,
                        folders: None,
                    },
                    |r| DriftSummary::of(r, verbose),
                );
            installed.push(InstalledInfo {
                target: TargetRef {
                    id: d.target.id.clone(),
                    key,
                },
                version: a.version.clone(),
                version_confidence: a.version_confidence,
                channel: a.channel,
                pinned: a.pinned,
                state: a.state,
                reason: a.reason,
                required_by: a.required_by.clone(),
                orphan: a.is_orphan(),
                folders: a.folders.clone(),
                drift,
                installed_at: a.installed_at,
                tx: a.tx_id,
                record_path: verbose.then(|| world.paths.target_record(&key)),
            });
        }
    }
    let entry = catalog
        .entries()
        .iter()
        .find(|e| e.r#ref == r)
        .or_else(|| catalog.resolve(&args.query, family));
    let catalog_info = entry.map(|e| CatalogInfo {
        name: e.name.clone(),
        variant: e.variant.clone(),
        aliases: e.aliases.clone(),
        families: e.families.clone(),
        folders: e.folders.clone(),
        notes: e.notes.clone(),
        variants: catalog
            .variants(&e.name)
            .into_iter()
            .filter(|other| other.identity() != e.identity())
            .filter_map(|other| {
                Some(CatalogVariant {
                    variant: other.variant.clone()?,
                    families: other.families.clone(),
                })
            })
            .collect(),
    });
    let name = installed
        .first()
        .and_then(|i| {
            records
                .get(&i.target.id)?
                .addons
                .values()
                .find(|a| a.r#ref == r)
                .map(|a| a.name.clone())
        })
        .or_else(|| catalog_info.as_ref().map(|c| c.name.clone()));

    let known_locally = !installed.is_empty() || catalog_info.is_some();
    let net = NetConfig {
        offline: ctx.offline,
        git_hosts: world.config.git_hosts.clone(),
        ..NetConfig::default()
    };
    let log = NetLog::new(&world.paths)?;
    let http = stack(&net, log.clone(), &world.paths);
    let sources = world.sources().with_offline(ctx.offline);
    let registry =
        SourceRegistry::builtin(&sources).with_git(GitRunner::new(&net, log, &world.paths));
    // Read-only: an answer from the last 15 minutes is good enough.
    let freshness = if ctx.offline {
        Freshness::OfflineOnly
    } else if args.refresh {
        Freshness::Refresh
    } else {
        Freshness::Window(SOURCE_RECHECK_MIN_INTERVAL)
    };
    let cache = ReleaseCache::new(
        &world.paths,
        http.as_ref(),
        &registry,
        freshness,
        ctx.command,
        ctx.actor,
    );
    let wanted = Wanted::stable(r.clone());
    let fetched = cache
        .get(std::slice::from_ref(&wanted))
        .remove(&wanted)
        .map_or_else(
            || Err(Error::internal("the fetch returned nothing for the ref")),
            |f| f.set,
        );
    // What plans learned, for judging each client (taken before the cache is spent).
    let facts = cache.facts().clone();
    // Before any error return: a remembered failure is worth keeping too.
    let remembered = cache.remember(&[]);
    warnings.extend(remembered.warnings);
    let (latest, partial, set) = match fetched {
        Ok(set) => {
            let families = catalog_info
                .as_ref()
                .map_or(&[][..], |c| c.families.as_slice());
            let found = latest(&set, &targets, verbose, &facts, families);
            (Some(found), false, Some(set))
        }
        // Offline with nothing remembered: the local part only, as in M1.
        Err(_) if ctx.offline => (None, false, None),
        Err(error) if known_locally => {
            warnings.push(Warning::item(&error));
            (None, true, None)
        }
        Err(error) => return Err(error),
    };
    let mut notes = remembered.notes;
    // `--changelog`: the releases newer than the oldest installed one.
    let (changelog, changelog_unavailable) = if args.changelog {
        let newest_stable = set
            .as_ref()
            .and_then(|s| newest(s, Channel::Stable).map(|r| (s, r)));
        match newest_stable {
            Some((set, release)) => {
                let since = oldest_since(&releases, set);
                let found = changelog_for(&registry, set, release, since.as_deref(), http.as_ref());
                notes.extend(found.diagnostic);
                (Some(found.changelog), found.unavailable)
            }
            None if ctx.offline => (Some(None), Some(ChangelogUnavailable::Offline)),
            None if partial => (Some(None), Some(ChangelogUnavailable::FetchFailed)),
            None => (Some(None), Some(ChangelogUnavailable::NoReleases)),
        }
    } else {
        (None, None)
    };
    Ok(InfoOutcome {
        data: InfoData {
            query: args.query.clone(),
            r#ref: r,
            name,
            catalog: catalog_info,
            installed,
            latest: latest.flatten(),
            offline: ctx.offline,
            changelog,
            changelog_unavailable,
        },
        warnings,
        partial,
        notes,
    })
}

/// The newest stable release and the targets with a candidate for it; `None` when the
/// source lists no stable release.
fn latest(
    set: &ReleaseSet,
    targets: &[&DiscoveredTarget],
    verbose: bool,
    facts: &FactIndex,
    families: &[Family],
) -> Option<Latest> {
    let release = newest(set, Channel::Stable)?;
    let mut flavors: Vec<String> = release
        .assets
        .iter()
        .flat_map(|a| a.flavors.iter().map(|f| f.flavor.clone()))
        .collect();
    flavors.sort();
    flavors.dedup();
    let compat: std::collections::BTreeMap<String, LoadHint> = targets
        .iter()
        .map(|d| (d.target.id.clone(), judge(set, &d.target, facts, families)))
        .collect();
    let likely: Vec<String> = compat
        .iter()
        .filter(|(_, hint)| hint.likely())
        .map(|(id, _)| id.clone())
        .collect();
    let urls = verbose.then(|| {
        release
            .assets
            .iter()
            .filter_map(|a| match &a.location {
                crate::model::AssetLocation::Url(url) => Some(url.clone()),
                // A token URL exists only at download time: show where it comes from.
                crate::model::AssetLocation::Deferred {
                    source: crate::sources::SourceId::Tukui,
                    key,
                } => Some(crate::sources::tukui::canonical_url(key)),
                _ => None,
            })
            .collect()
    });
    Some(Latest {
        version: release.version.clone(),
        release_id: release.release_id.clone(),
        channel: release.channel,
        flavors,
        targets: likely,
        compat,
        asset_urls: urls,
    })
}

/// A query that matches nothing anywhere.
fn not_found(query: &str) -> Error {
    Error::new(
        ErrorCode::NotFound,
        format!("{query} is not installed, not in the catalog, and not a ref"),
        Suggestion::command(["omaddons", "list"]),
    )
}
