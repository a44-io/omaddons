//! `omaddons list [-t T] [--unmanaged] [--protected] [--deep]`: the managed addons of
//! each target and counts of every folder class; on request the unmanaged (plus shared,
//! linked and invalid) and protected folders. With `-v` (stat-first) or `--deep` (every file
//! re-hashed), each addon's drift. Read-only: no lock, no writes, no network.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use super::drift::DriftSummary;
use super::status::Remembered;
use super::world::World;
use crate::cli::Context;
use crate::discover::DiscoveredTarget;
use crate::drift::{DriftMode, DriftState};
use crate::error::{Result, Warning};
use crate::inventory::{Depth, FolderClass, FolderEntry, Inventory, OwnedOn, inventory_shared};
use crate::model::{AddonReason, AddonRecord, AddonState, Channel, TargetKey, VersionConfidence};
use crate::output::{Outcome, Style, table};
use crate::resolve::updates::{StatusKind, StatusRow, UpdateStatus};
use crate::rules::{ClientVersion, Family, RulesTable};
use crate::state::records::load_record;
use crate::toc::Verdict;

/// `list`'s own flags.
#[derive(Debug, Clone, Copy, Default, clap::Args)]
pub struct ListArgs {
    /// Also list unmanaged, shared, linked and invalid folders, with their TOC
    #[arg(long)]
    pub unmanaged: bool,
    /// Also list protected (Blizzard_*) folders
    #[arg(long)]
    pub protected: bool,
    /// Check each managed addon's files by content, not size and time (shows drift)
    #[arg(long)]
    pub deep: bool,
}

/// `list`'s data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListData {
    /// Sorted by id.
    pub targets: Vec<ListTarget>,
}

/// One target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListTarget {
    pub id: String,
    /// `null` until the target is registered.
    pub key: Option<TargetKey>,
    pub version: ClientVersion,
    pub family: Family,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `-v` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_dir: Option<PathBuf>,
    /// `Interface/AddOns` does not exist yet.
    pub addons_dir_missing: bool,
    /// Sorted by name, ignoring case.
    pub addons: Vec<ListAddon>,
    pub counts: ListCounts,
    /// With `--unmanaged`: unmanaged, shared, linked and invalid folders by key; else
    /// `null`.
    pub unmanaged: Option<Vec<FolderInfo>>,
    /// With `--protected`; else `null`.
    pub protected: Option<Vec<ProtectedFolder>>,
}

/// One managed addon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListAddon {
    pub name: String,
    pub version: String,
    pub version_confidence: VersionConfidence,
    #[serde(rename = "ref")]
    pub source_ref: String,
    pub channel: Channel,
    pub pinned: bool,
    pub state: AddonState,
    /// `explicit` when a user asked for it, `dependency` when only the resolver did.
    pub reason: AddonReason,
    /// The addons that require it, sorted.
    pub required_by: Vec<String>,
    /// A dependency nothing requires any more.
    pub orphan: bool,
    /// The newest release the release memo knows, as `outdated` judges it (no request).
    pub latest: Latest,
    /// A count; with `-v` the folder names.
    pub folders: Folders,
    /// Record folders that are not on disk.
    pub missing_folders: Vec<String>,
    /// `-v` or `--deep`: whether its files changed on disk, with paths under `-v`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<DriftSummary>,
    /// `-v`: each folder's TOC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_tocs: Option<Vec<FolderInfo>>,
}

/// What the release memo says is newest for an addon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Latest {
    /// The newest release's version, when the status names one.
    pub version: Option<String>,
    pub status: StatusKind,
}

/// A folder count, or the names with `-v`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Folders {
    Count(usize),
    Names(Vec<String>),
}

/// Folders per class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListCounts {
    pub managed: u32,
    pub unmanaged: u32,
    /// Managed by another client sharing this AddOns directory.
    pub shared: u32,
    pub protected: u32,
    pub linked: u32,
    pub invalid: u32,
}

/// A folder and what its TOC says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderInfo {
    pub folder: String,
    pub class: FolderClass,
    pub title: Option<String>,
    pub version: Option<String>,
    /// The TOC's Interface for this client: the highest in the family's range, else the
    /// first listed.
    pub interface: Option<u32>,
    pub verdict: Option<VerdictKind>,
    pub case_duplicate: bool,
    /// A `shared` folder's owners on the clients sharing this AddOns directory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub owned_on: Vec<OwnedOn>,
}

/// The TOC-load verdict, flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Compatible,
    OutOfDate,
    WrongFlavor,
    NoToc,
}

/// A protected folder: its name only (a client pack can hold dozens).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProtectedFolder {
    pub folder: String,
}

/// `list`'s outcome.
#[derive(Debug)]
pub struct ListOutcome {
    data: ListData,
    warnings: Vec<Warning>,
}

impl Outcome for ListOutcome {
    type Data = ListData;

    fn command(&self) -> &'static str {
        "list"
    }

    fn data(&self) -> &ListData {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, style: &Style) -> std::io::Result<()> {
        if self.data.targets.is_empty() {
            return writeln!(
                w,
                "No WoW client found. Add one with: omaddons targets add <path>"
            );
        }
        for t in &self.data.targets {
            render_target(w, style, t)?;
        }
        Ok(())
    }
}

/// Runs `list`.
pub fn run(ctx: &Context, args: ListArgs) -> Result<ListOutcome> {
    let world = World::load(ctx)?;
    let verbose = ctx.output.style.verbose > 0;
    let depth = if args.unmanaged || verbose {
        Depth::Full
    } else {
        Depth::Classes
    };
    let mut warnings = world.warnings.clone();
    let remembered = Remembered::load(&world);
    let mut targets = Vec::new();
    for d in world.select_many(ctx)? {
        let record = match d.target.key {
            Some(key) => {
                let loaded = load_record(&world.paths, &key)?;
                let warning = loaded.warning(&world.paths.target_record(&key));
                ctx.notes.warn_all(warning.as_slice());
                warnings.extend(warning);
                Some(loaded.value)
            }
            None => None,
        };
        let shared = world.sibling_records(&d.target)?;
        let inv = inventory_shared(&d.target, record.as_ref(), &shared, &world.table, depth)?;
        let rows = record
            .as_ref()
            .map(|r| remembered.statuses(&d.target, r, &world.table))
            .unwrap_or_default();
        let mut listed = list_target(
            d,
            record
                .as_ref()
                .map(|r| r.addons.values().collect())
                .unwrap_or_default(),
            &rows,
            &inv,
            args,
            verbose,
            &world.table,
        );
        // Drift only on request: a plain `list` stays as cheap as the inventory.
        if let (true, Some(record)) = (verbose || args.deep, &record) {
            let mode = if args.deep {
                DriftMode::Deep
            } else {
                DriftMode::Stat
            };
            let report = super::drift::report(&world.paths, &d.target, record, mode);
            for a in &mut listed.addons {
                a.drift = report
                    .addons
                    .iter()
                    .find(|r| r.addon.eq_ignore_ascii_case(&a.name))
                    .map(|r| DriftSummary::of(r, verbose));
            }
        }
        targets.push(listed);
    }
    Ok(ListOutcome {
        data: ListData { targets },
        warnings,
    })
}

/// Assembles one target from its record's addons and its inventory.
fn list_target(
    d: &DiscoveredTarget,
    addons: Vec<&AddonRecord>,
    rows: &[StatusRow],
    inv: &Inventory,
    args: ListArgs,
    verbose: bool,
    table: &RulesTable,
) -> ListTarget {
    let t = &d.target;
    let info = |e: &FolderEntry| folder_info(e, t.family, table);
    let latest = |a: &AddonRecord| {
        let status = rows
            .iter()
            .find(|r| r.addon.eq_ignore_ascii_case(&a.name))
            .map(|r| &r.status);
        Latest {
            version: status
                .and_then(UpdateStatus::latest)
                .map(|l| l.version.clone()),
            status: status.map_or(StatusKind::Unchecked, UpdateStatus::kind),
        }
    };
    let mut listed: Vec<ListAddon> = addons
        .into_iter()
        .map(|a| {
            let keys: Vec<&str> = a.folders.iter().map(|f| f.key()).collect();
            let on_disk = |key: &str| inv.folders.iter().find(|e| e.key == key);
            ListAddon {
                name: a.name.clone(),
                version: a.version.clone(),
                version_confidence: a.version_confidence,
                source_ref: a.r#ref.to_string(),
                channel: a.channel,
                pinned: a.pinned,
                state: a.state,
                reason: a.reason,
                required_by: a.required_by.clone(),
                orphan: a.is_orphan(),
                latest: latest(a),
                folders: if verbose {
                    Folders::Names(a.folders.iter().map(|f| f.as_str().to_owned()).collect())
                } else {
                    Folders::Count(a.folders.len())
                },
                missing_folders: a
                    .folders
                    .iter()
                    .filter(|f| on_disk(f.key()).is_none())
                    .map(|f| f.as_str().to_owned())
                    .collect(),
                drift: None,
                folder_tocs: verbose
                    .then(|| keys.iter().filter_map(|k| on_disk(k)).map(info).collect()),
            }
        })
        .collect();
    listed.sort_by_key(|a| a.name.to_ascii_lowercase());
    let of = |classes| of_class(inv, classes);
    ListTarget {
        id: t.id.clone(),
        key: t.key,
        version: t.version,
        family: t.family,
        label: t.label.clone(),
        client_dir: verbose.then(|| t.client_dir.clone()),
        addons_dir_missing: inv.addons_dir_missing,
        addons: listed,
        counts: ListCounts {
            managed: inv.counts.managed,
            unmanaged: inv.counts.unmanaged,
            shared: inv.counts.shared,
            protected: inv.counts.protected,
            linked: inv.counts.linked,
            invalid: inv.counts.invalid,
        },
        unmanaged: args.unmanaged.then(|| {
            of(&[
                FolderClass::Unmanaged,
                FolderClass::Shared,
                FolderClass::Linked,
                FolderClass::Invalid,
            ])
            .map(info)
            .collect()
        }),
        protected: args.protected.then(|| {
            of(&[FolderClass::Protected])
                .map(|e| ProtectedFolder {
                    folder: e.name.clone(),
                })
                .collect()
        }),
    }
}

/// The inventory's entries of the given classes, in inventory order.
fn of_class<'a>(
    inv: &'a Inventory,
    classes: &'static [FolderClass],
) -> impl Iterator<Item = &'a FolderEntry> {
    inv.folders
        .iter()
        .filter(move |e| classes.contains(&e.class))
}

fn folder_info(e: &FolderEntry, family: Family, table: &RulesTable) -> FolderInfo {
    let range = table.family_range(family);
    let interface = e.toc.as_ref().and_then(|toc| {
        range
            .and_then(|r| {
                toc.interfaces
                    .iter()
                    .copied()
                    .filter(|i| r.contains(*i))
                    .max()
            })
            .or_else(|| toc.interfaces.first().copied())
    });
    FolderInfo {
        folder: e.name.clone(),
        class: e.class,
        title: e.toc.as_ref().and_then(|t| t.title.clone()),
        version: e.toc.as_ref().and_then(|t| t.version.clone()),
        interface,
        verdict: e.verdict.as_ref().map(|v| match v.verdict {
            Verdict::Compatible { out_of_date: false } => VerdictKind::Compatible,
            Verdict::Compatible { out_of_date: true } => VerdictKind::OutOfDate,
            Verdict::WrongFlavor { .. } => VerdictKind::WrongFlavor,
            Verdict::NoToc => VerdictKind::NoToc,
        }),
        case_duplicate: e.case_duplicate,
        owned_on: e.owned_on.clone(),
    }
}

/// The wire name of a serializable enum value.
fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn render_target(w: &mut dyn Write, style: &Style, t: &ListTarget) -> std::io::Result<()> {
    let mut head = vec![t.version.to_string(), wire(&t.family)];
    head.extend(
        t.label
            .clone()
            .filter(|l| *l != format!("{} (battlenet)", t.id)),
    );
    writeln!(w, "{} ({})", style.bold(&t.id), head.join(", "))?;
    if style.paths_visible()
        && let Some(dir) = &t.client_dir
    {
        writeln!(w, "  {}", style.dim(&dir.display().to_string()))?;
    }
    let rows: Vec<Vec<String>> = t
        .addons
        .iter()
        .map(|a| {
            let mut flags = vec![wire(&a.channel)];
            if a.pinned {
                flags.push("pinned".into());
            }
            if a.orphan {
                flags.push(style.warn("orphan"));
            } else if a.reason == AddonReason::Dependency {
                flags.push(if style.verbose > 0 && !a.required_by.is_empty() {
                    format!("dependency of {}", a.required_by.join(", "))
                } else {
                    "dep".into()
                });
            }
            if a.state != AddonState::Ok {
                flags.push(style.warn(&wire(&a.state)));
            }
            if !a.missing_folders.is_empty() {
                flags.push(style.warn(&format!("missing {}", a.missing_folders.join(", "))));
            }
            match a.drift.as_ref().map(|d| (d.state, d.files)) {
                Some((DriftState::Modified, n)) => {
                    let files = if n == 1 { "file" } else { "files" };
                    flags.push(style.warn(&format!("{n} {files} changed")));
                }
                Some((DriftState::Unindexed, _)) => flags.push("not indexed".into()),
                _ => {}
            }
            let latest = latest_cell(&a.latest);
            vec![
                format!("  {}", a.name),
                a.version.clone(),
                latest,
                a.source_ref.clone(),
                flags.join("  "),
            ]
        })
        .collect();
    w.write_all(table(&rows).as_bytes())?;
    if let Some(folders) = &t.unmanaged {
        let rows: Vec<Vec<String>> = folders
            .iter()
            .map(|f| {
                let verdict = match f.owned_on.first() {
                    Some(owned) if f.class == FolderClass::Shared => {
                        format!("shared (managed on {})", owned.target)
                    }
                    _ => f
                        .verdict
                        .map(|v| wire(&v))
                        .unwrap_or_else(|| wire(&f.class)),
                };
                vec![
                    format!("  {}", f.folder),
                    f.title.clone().unwrap_or_default(),
                    f.version.clone().unwrap_or_default(),
                    verdict,
                ]
            })
            .collect();
        if !rows.is_empty() {
            writeln!(w, "  {}", style.dim("unmanaged:"))?;
            w.write_all(table(&rows).as_bytes())?;
        }
    }
    if let Some(folders) = &t.protected
        && !folders.is_empty()
    {
        let names: Vec<&str> = folders.iter().map(|f| f.folder.as_str()).collect();
        writeln!(w, "  {} {}", style.dim("protected:"), names.join(", "))?;
    }
    let c = t.counts;
    let mut counts = format!(
        "  {} managed · {} unmanaged · {} protected · {} linked",
        c.managed, c.unmanaged, c.protected, c.linked
    );
    if c.shared > 0 {
        counts.push_str(&format!(" · {} shared", c.shared));
    }
    if c.invalid > 0 {
        counts.push_str(&format!(" · {} invalid", c.invalid));
    }
    if t.addons_dir_missing {
        counts.push_str(" · no AddOns folder yet");
    }
    writeln!(w, "{}", style.dim(&counts))
}

/// The latest column: a newer release when one is known (pinned: the flags say why it
/// waits), blank otherwise, since a current addon's version would only repeat the
/// installed one.
fn latest_cell(latest: &Latest) -> String {
    let newer = || format!("→ {}", latest.version.as_deref().unwrap_or("?"));
    match latest.status {
        StatusKind::UpdateAvailable | StatusKind::Pinned => newer(),
        StatusKind::Blocked => format!("{} (blocked)", newer()),
        StatusKind::Failed => "check failed".to_owned(),
        StatusKind::Current | StatusKind::Ahead | StatusKind::Unchecked | StatusKind::Static => {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_column_shows_only_a_newer_release() {
        for (status, cell) in [
            (StatusKind::UpdateAvailable, "→ 1.2"),
            (StatusKind::Pinned, "→ 1.2"),
            (StatusKind::Blocked, "→ 1.2 (blocked)"),
            (StatusKind::Failed, "check failed"),
            (StatusKind::Current, ""),
            (StatusKind::Ahead, ""),
            (StatusKind::Unchecked, ""),
            (StatusKind::Static, ""),
        ] {
            let latest = Latest {
                version: Some("1.2".to_owned()),
                status,
            };
            assert_eq!(latest_cell(&latest), cell, "{status:?}");
        }
    }
}
