//! `omaddons info <ref|addon> [-t T]`: what an addon is and what installing it would
//! get. The installed records across targets, the catalog entry, and the newest stable
//! release with the targets that have a candidate for it (metadata only: nothing is
//! downloaded or verified; `install` verifies). No changelog until M2.
//!
//! `--offline` skips the fetch (`latest: null`, `offline: true`). A failed fetch for an
//! addon known locally is a partial success; for one known nowhere, the command's error.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::history::TargetRef;
use super::install::parse_ref;
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::discover::registry::resolve_one;
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::model::{AddonState, Channel, FolderName, Ref, ReleaseSet, TxId, VersionConfidence};
use crate::net::{NetConfig, NetLog, stack};
use crate::output::{Outcome, Style};
use crate::resolve::catalog::Catalog;
use crate::resolve::select::{newest, select};
use crate::rules::Family;
use crate::sources::git_hosts::GitRunner;
use crate::sources::{SourceRegistry, Wanted, fetch_once};

/// `info`'s own arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InfoArgs {
    /// The addon: a catalog name, an installed addon's name, or a ref or link
    #[arg(value_name = "REF")]
    pub query: String,
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
}

/// The catalog entry.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CatalogInfo {
    pub name: String,
    pub aliases: Vec<String>,
    pub families: Vec<Family>,
    pub folders: Vec<FolderName>,
    pub notes: Option<String>,
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
    pub folders: Vec<FolderName>,
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
    /// Targets with a metadata candidate (not verified).
    pub targets: Vec<String>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_urls: Option<Vec<String>>,
}

/// `info`'s outcome.
#[derive(Debug)]
pub struct InfoOutcome {
    data: InfoData,
    warnings: Vec<Warning>,
    /// The fetch failed for an addon known locally.
    partial: bool,
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

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        let d = &self.data;
        let name = d.name.as_deref().unwrap_or(&d.query);
        writeln!(w, "{name}  {}", d.r#ref)?;
        if let Some(c) = &d.catalog {
            let folders: Vec<&str> = c.folders.iter().map(FolderName::as_str).collect();
            let families: Vec<String> = c
                .families
                .iter()
                .filter_map(|f| serde_json::to_value(f).ok()?.as_str().map(str::to_owned))
                .collect();
            writeln!(
                w,
                "  catalog: folders {} · families {}",
                folders.join(", "),
                families.join(", ")
            )?;
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
            writeln!(
                w,
                "  {}: {} ({confidence}, {channel}) installed {}",
                i.target.id,
                i.version,
                super::history::local_time(i.installed_at)
            )?;
        }
        match (&d.latest, d.offline) {
            (Some(l), _) if l.targets.is_empty() => {
                writeln!(
                    w,
                    "  latest: {} · no candidate for any target here",
                    l.version
                )?;
            }
            (Some(l), _) => {
                writeln!(
                    w,
                    "  latest: {} · candidates for {}",
                    l.version,
                    l.targets.join(", ")
                )?;
            }
            (None, true) => writeln!(w, "  latest: not checked (offline)")?,
            (None, false) => writeln!(w, "  latest: unavailable")?,
        }
        Ok(())
    }
}

/// Runs `info`.
pub fn run(ctx: &Context, args: &InfoArgs) -> Result<InfoOutcome> {
    let world = World::load()?;
    let verbose = ctx.output.style.verbose > 0;
    let mut warnings = world.warnings.clone();
    let (catalog, catalog_warnings) = Catalog::load(&world.paths);
    warnings.extend(catalog_warnings);
    let records = world.records()?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let parsed = parse_ref(&world, &records, &catalog, &cwd, &args.query).map_err(|e| {
        if matches!(e.code, ErrorCode::NotFound) {
            not_found(&args.query)
        } else {
            e
        }
    })?;
    let r = parsed.r#ref.clone();
    let targets: Vec<&DiscoveredTarget> = match &ctx.target {
        Some(selector) => vec![resolve_one(selector, &world.resolved.targets, &ctx.argv)?],
        None => world.resolved.targets.iter().collect(),
    };

    let mut installed = Vec::new();
    for d in &targets {
        let Some(record) = records.get(&d.target.id) else {
            continue;
        };
        let Some(key) = d.target.key else { continue };
        for a in record.addons.values().filter(|a| a.r#ref == r) {
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
                folders: a.folders.clone(),
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
        .or_else(|| catalog.by_name(&args.query));
    let catalog_info = entry.map(|e| CatalogInfo {
        name: e.name.clone(),
        aliases: e.aliases.clone(),
        families: e.families.clone(),
        folders: e.folders.clone(),
        notes: e.notes.clone(),
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
    let (latest, partial) = if ctx.offline {
        (None, false)
    } else {
        match fetch(&world, ctx, &r) {
            Ok(set) => (Some(latest(&set, &targets, verbose)), false),
            Err(error) if known_locally => {
                warnings.push(Warning::item(&error));
                (None, true)
            }
            Err(error) => return Err(error),
        }
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
        },
        warnings,
        partial,
    })
}

/// One fetch of `r`'s releases.
fn fetch(world: &World, ctx: &Context, r: &Ref) -> Result<ReleaseSet> {
    let net = NetConfig {
        offline: ctx.offline,
        git_hosts: world.config.git_hosts.clone(),
        ..NetConfig::default()
    };
    let log = NetLog::new(&world.paths)?;
    let http = stack(&net, log.clone(), &world.paths);
    let registry = SourceRegistry::builtin().with_git(GitRunner::new(&net, log));
    let wanted = Wanted::stable(r.clone());
    fetch_once([wanted.clone()], &registry, http.as_ref())
        .remove(&wanted)
        .unwrap_or_else(|| Err(Error::internal("the fetch returned nothing for the ref")))
}

/// The newest stable release and the targets with a candidate for it; `None` when the
/// source lists no stable release.
fn latest(set: &ReleaseSet, targets: &[&DiscoveredTarget], verbose: bool) -> Option<Latest> {
    let release = newest(set, Channel::Stable)?;
    let mut flavors: Vec<String> = release
        .assets
        .iter()
        .flat_map(|a| a.flavors.iter().map(|f| f.flavor.clone()))
        .collect();
    flavors.sort();
    flavors.dedup();
    let mut with_candidate: Vec<String> = targets
        .iter()
        .filter(|d| {
            !select(set, &d.target, Channel::Stable, None)
                .candidates
                .is_empty()
        })
        .map(|d| d.target.id.clone())
        .collect();
    with_candidate.sort();
    let urls = verbose.then(|| {
        release
            .assets
            .iter()
            .filter_map(|a| match &a.location {
                crate::model::AssetLocation::Url(url) => Some(url.clone()),
                _ => None,
            })
            .collect()
    });
    Some(Latest {
        version: release.version.clone(),
        release_id: release.release_id.clone(),
        channel: release.channel,
        flavors,
        targets: with_candidate,
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
