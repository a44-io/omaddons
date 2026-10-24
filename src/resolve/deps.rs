//! Dependencies, the pure layer: which folders' requirements an install follows, when a
//! required folder is already satisfied, and where to get one that is not (M2-D7,
//! M2-D8). No I/O, no clock, no fetching: the engine's dependency rounds, `status` and
//! the doctor plug in their views and indexes through small traits.
//!
//! - **Needed folders** are the primary folder and what it transitively requires inside
//!   its own archive. Their outside requirements are installed. Requirements of the
//!   other folders (a bridge such as `Masque_Dominos`) are only reported.
//! - **Satisfied:** a `Blizzard_*` folder (the client has it), a folder of the same
//!   archive, a folder another work installs in the same transaction, or a folder
//!   already in AddOns (managed: its record gains the requester; unmanaged, linked or
//!   protected: nothing changes, an unmanaged one earns a hint).
//! - **Resolution**, first unique hit wins: a managed record that lost the folder (not
//!   repaired: reported), the catalog for the target's family, the Tukui index, then
//!   WoWInterface, ranked with guards against ambiguity and abandoned projects.

use std::collections::BTreeSet;

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::Serialize;

use crate::engine::inspect::FolderVerdict;
use crate::inventory::FolderClass;
use crate::limits::DEPENDENCY_SOURCE_MAX_AGE;
use crate::model::{FolderName, Ref, Target};
use crate::resolve::catalog::{Catalog, Entry};
use crate::rules::Family;
use crate::state::records::TargetRecord;

/// A name as it compares to another: ASCII-lowercase, without anything but letters and
/// digits (`My Addon`, `my-addon` and `MyAddon` are one name). Title and author
/// normalisation build on it.
pub fn normalized(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The primary folder and every folder of the same archive it requires, transitively.
pub fn needed_folders(primary: &FolderName, folders: &[FolderVerdict]) -> BTreeSet<FolderName> {
    let in_archive = |f: &FolderName| folders.iter().any(|v| v.folder == *f);
    let mut needed = BTreeSet::from([primary.clone()]);
    let mut todo = vec![primary.clone()];
    while let Some(folder) = todo.pop() {
        let Some(v) = folders.iter().find(|v| v.folder == folder) else {
            continue;
        };
        for dep in &v.dependencies.required {
            if in_archive(dep) && needed.insert(dep.clone()) {
                todo.push(dep.clone());
            }
        }
    }
    needed
}

/// A requirement of a folder the user did not ask for: reported, not installed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct BridgeDependency {
    /// The archive's folder that requires it (`Masque_Dominos`).
    pub folder: FolderName,
    /// What it requires, outside the archive (`Masque`).
    pub requires: FolderName,
}

/// The requirements outside the archive of folders outside `needed`, sorted. Raw: the
/// caller drops the ones satisfied after its last round.
pub fn bridge_dependencies(
    needed: &BTreeSet<FolderName>,
    folders: &[FolderVerdict],
) -> Vec<BridgeDependency> {
    let in_archive = |f: &FolderName| folders.iter().any(|v| v.folder == *f);
    let mut bridges: Vec<BridgeDependency> = folders
        .iter()
        .filter(|v| !needed.contains(&v.folder))
        .flat_map(|v| {
            v.dependencies
                .required
                .iter()
                .filter(|d| !in_archive(d))
                .map(|d| BridgeDependency {
                    folder: v.folder.clone(),
                    requires: d.clone(),
                })
        })
        .collect();
    bridges.sort();
    bridges.dedup();
    bridges
}

/// A folder of the target as it will be after the works already planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewFolder {
    /// A planned work installs it.
    Planned { addon: String },
    /// It is in AddOns; `owner` is the managed addon it belongs to.
    Present {
        class: FolderClass,
        owner: Option<String>,
    },
}

/// The target as dependency checks see it.
pub trait DepView {
    fn folder(&self, dep: &FolderName) -> Option<ViewFolder>;
}

/// Why a required folder needs nothing installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Satisfier {
    /// A `Blizzard_*` folder: the client provides it.
    Client,
    /// A folder of the same archive.
    SameArchive,
    /// A managed addon has it: its record gains the requester in `required_by`.
    Managed(String),
    /// It is there, unmanaged, linked or protected; nothing changes (an unmanaged one
    /// earns a hint).
    Present(FolderClass),
    /// Another work of the transaction installs it; its record gains the requester.
    Planned(String),
}

/// Whether `dep`, required by a folder of `archive`, is already satisfied on the target.
/// A folder without a loadable TOC (`invalid`) satisfies nothing.
pub fn is_satisfied(
    dep: &FolderName,
    archive: &[FolderName],
    view: &dyn DepView,
) -> Option<Satisfier> {
    if dep.is_protected() {
        return Some(Satisfier::Client);
    }
    if archive.contains(dep) {
        return Some(Satisfier::SameArchive);
    }
    match view.folder(dep)? {
        ViewFolder::Planned { addon } => Some(Satisfier::Planned(addon)),
        ViewFolder::Present {
            class: FolderClass::Managed,
            owner: Some(owner),
        } => Some(Satisfier::Managed(owner)),
        ViewFolder::Present {
            class: FolderClass::Invalid,
            ..
        } => None,
        ViewFolder::Present { class, .. } => Some(Satisfier::Present(class)),
    }
}

/// The catalog's entries shipping a folder.
pub trait CatalogLookup {
    fn by_folder(&self, folder: &FolderName) -> Vec<&Entry>;
}

impl CatalogLookup for Catalog {
    fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
        Self::by_folder(self, folder)
    }
}

/// A Tukui index entry shipping a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TukuiHit {
    pub slug: String,
    pub name: String,
}

/// The Tukui index: which entries' `directories` hold a folder.
pub trait TukuiDirs {
    fn by_directory(&self, folder: &FolderName) -> Vec<TukuiHit>;
}

/// A WoWInterface project whose (sanitised) `UIDir` holds a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WowiProject {
    pub uid: u64,
    /// `UIName`.
    pub name: String,
    /// `UIDir`, sanitised.
    pub dirs: Vec<FolderName>,
    /// The families its `UICompatibility` maps to.
    pub families: Vec<Family>,
    /// `UIDownloadTotal`.
    pub downloads: u64,
    /// `UIDate`.
    pub date: Option<Timestamp>,
}

/// The WoWInterface index: which projects ship a folder.
pub trait WowiDirs {
    fn by_dir(&self, folder: &FolderName) -> Vec<WowiProject>;
}

/// Where resolution looks.
pub struct DepLookups<'a> {
    /// The target's record: a record that lists the folder while it is gone is reported.
    pub managed: &'a TargetRecord,
    /// Whether its owner's work in this transaction stops shipping a folder: not lost,
    /// so it resolves like any other.
    pub released: &'a dyn Fn(&FolderName) -> bool,
    pub catalog: &'a dyn CatalogLookup,
    pub tukui: Option<&'a dyn TukuiDirs>,
    pub wowi: Option<&'a dyn WowiDirs>,
    /// For the age guard.
    pub now: Timestamp,
}

/// Which index a dependency came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DepVia {
    Catalog,
    Tukui,
    Wowi,
}

/// One possible source of a dependency, when no single one is safe to choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DepCandidate {
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    pub name: String,
    pub via: DepVia,
    /// When the project last changed (WoWInterface), so a message can say why.
    #[schemars(with = "Option<String>")]
    pub updated_at: Option<Timestamp>,
}

/// Why no single source was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityReason {
    SeveralCatalogEntries,
    WowiAmbiguous,
    /// The best WoWInterface project has not changed in `DEPENDENCY_SOURCE_MAX_AGE`.
    WowiAbandoned,
}

/// Why nothing provides the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    NoSource,
    /// A managed addon lists the folder but it is gone; reinstalling that addon brings
    /// it back.
    ManagedFolderMissing,
}

/// Where a missing required folder comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepResolution {
    Resolved {
        r#ref: Ref,
        via: DepVia,
        /// The catalog name, Tukui name or WoWInterface `UIName`, for the new record.
        name: Option<String>,
        /// A catalog entry that does not declare the client's family: a guess its TOCs
        /// settle, so a refusal is no failure (the wire still says `catalog`).
        undeclared: bool,
    },
    Ambiguous {
        candidates: Vec<DepCandidate>,
        reason: AmbiguityReason,
    },
    Missing {
        reason: MissingReason,
        /// `managed_folder_missing`: the addon whose record lists the folder.
        owner: Option<String>,
    },
}

/// Where to get `missing`, a required folder no rule satisfies, for `target`.
pub fn resolve(missing: &FolderName, target: &Target, lookups: &DepLookups<'_>) -> DepResolution {
    // 1. A managed addon lost it: report, never repair. One whose update or removal in
    // this transaction drops it lost nothing.
    if let Some(owner) = lookups
        .managed
        .addons
        .values()
        .find(|a| a.folders.contains(missing))
        && !(lookups.released)(missing)
    {
        return DepResolution::Missing {
            reason: MissingReason::ManagedFolderMissing,
            owner: Some(owner.name.clone()),
        };
    }
    // 2. The catalog's entries declaring this family.
    let entries = lookups.catalog.by_folder(missing);
    let declared: Vec<&Entry> = entries
        .iter()
        .copied()
        .filter(|e| e.declares(target.family))
        .collect();
    if let Some(found) = from_catalog(missing, &declared, false) {
        return found;
    }
    // 3. Tukui: only a unique entry (several fall through to WoWInterface).
    if let Some(tukui) = lookups.tukui
        && let [hit] = tukui.by_directory(missing).as_slice()
    {
        return DepResolution::Resolved {
            r#ref: Ref::Tukui {
                slug: hit.slug.clone(),
            },
            via: DepVia::Tukui,
            name: Some(hit.name.clone()),
            undeclared: false,
        };
    }
    // 4. WoWInterface, ranked and guarded.
    let wowi = lookups
        .wowi
        .map(|wowi| wowi.by_dir(missing))
        .filter(|projects| !projects.is_empty())
        .map(|projects| from_wowi(missing, target, lookups.now, projects));
    if let Some(resolved @ DepResolution::Resolved { .. }) = wowi {
        return resolved;
    }
    // 5. The catalog's other entries of this client's generation (modern or legacy):
    // the publisher does not declare the family, but the TOCs may still load here. Never
    // a variant: a family no variant covers gets no other family's build.
    let legacy = match target.family {
        Family::Unknown => target.rules.legacy,
        family => family.is_legacy(),
    };
    let undeclared: Vec<&Entry> = entries
        .iter()
        .copied()
        .filter(|e| e.variant.is_none() && !e.declares(target.family) && e.of_generation(legacy))
        .collect();
    from_catalog(missing, &undeclared, true)
        .or(wowi)
        .unwrap_or(DepResolution::Missing {
            reason: MissingReason::NoSource,
            owner: None,
        })
}

/// One catalog entry among `entries` (a name equal to the folder breaks a tie), several
/// tied, or none.
fn from_catalog(
    missing: &FolderName,
    entries: &[&Entry],
    undeclared: bool,
) -> Option<DepResolution> {
    let named: Vec<&Entry> = entries
        .iter()
        .copied()
        .filter(|e| e.name.eq_ignore_ascii_case(missing.as_str()))
        .collect();
    match (entries, named.as_slice()) {
        ([], _) => None,
        ([only], _) | (_, [only]) => Some(DepResolution::Resolved {
            r#ref: only.r#ref.clone(),
            via: DepVia::Catalog,
            name: Some(only.name.clone()),
            undeclared,
        }),
        (all, named) => {
            let tied = if named.is_empty() { all } else { named };
            Some(DepResolution::Ambiguous {
                candidates: tied
                    .iter()
                    .map(|e| DepCandidate {
                        r#ref: e.r#ref.clone(),
                        name: e.name.clone(),
                        via: DepVia::Catalog,
                        updated_at: None,
                    })
                    .collect(),
                reason: AmbiguityReason::SeveralCatalogEntries,
            })
        }
    }
}

/// How a project ranks for a folder: single-folder, then named like it, then built for
/// the family (a hint only), then downloads, then the newest.
fn rank(
    missing: &FolderName,
    family: Family,
    p: &WowiProject,
) -> (bool, bool, bool, u64, Option<Timestamp>) {
    (
        p.dirs.as_slice() == std::slice::from_ref(missing),
        normalized(&p.name) == normalized(missing.as_str()),
        p.families.contains(&family),
        p.downloads,
        p.date,
    )
}

/// The best project when it beats the next on being single-folder or named like the
/// folder (a sole project must be one of those itself) and is recent; else why not.
fn from_wowi(
    missing: &FolderName,
    target: &Target,
    now: Timestamp,
    mut projects: Vec<WowiProject>,
) -> DepResolution {
    // Best first.
    projects.sort_by_key(|p| std::cmp::Reverse(rank(missing, target.family, p)));
    let decisive = |p: &WowiProject| {
        let (single, named, ..) = rank(missing, target.family, p);
        (single, named)
    };
    let top = &projects[0];
    let beats = match projects.get(1) {
        Some(second) => decisive(top) > decisive(second),
        None => decisive(top) != (false, false),
    };
    let max_age =
        SignedDuration::try_from(DEPENDENCY_SOURCE_MAX_AGE).unwrap_or(SignedDuration::MAX);
    let recent = top.date.is_some_and(|d| now.duration_since(d) <= max_age);
    if beats && recent {
        return DepResolution::Resolved {
            r#ref: Ref::Wowi { id: top.uid },
            via: DepVia::Wowi,
            name: Some(top.name.clone()),
            undeclared: false,
        };
    }
    DepResolution::Ambiguous {
        candidates: projects
            .iter()
            .take(3)
            .map(|p| DepCandidate {
                r#ref: Ref::Wowi { id: p.uid },
                name: p.name.clone(),
                via: DepVia::Wowi,
                updated_at: p.date,
            })
            .collect(),
        reason: if beats {
            AmbiguityReason::WowiAbandoned
        } else {
            AmbiguityReason::WowiAmbiguous
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{
        AddonReason, AddonRecord, AddonState, Channel, TargetKey, TxId, VersionConfidence,
    };
    use crate::toc::{Verdict, VerdictReport};

    fn f(name: &str) -> FolderName {
        FolderName::new(name).unwrap()
    }

    fn verdict(folder: &str, deps: &[&str]) -> FolderVerdict {
        FolderVerdict {
            folder: f(folder),
            report: VerdictReport {
                loaded: None,
                verdict: Verdict::Compatible { out_of_date: false },
                warnings: Vec::new(),
            },
            dependencies: crate::toc::DepList {
                required: deps.iter().map(|d| f(d)).collect(),
                ..crate::toc::DepList::default()
            },
            interface: Some(120_105),
        }
    }

    fn now() -> Timestamp {
        "2026-10-16T00:00:00Z".parse().unwrap()
    }

    fn days_ago(days: i64) -> Option<Timestamp> {
        Some(now() - SignedDuration::from_hours(24 * days))
    }

    fn vanilla() -> Target {
        let mut target =
            crate::engine::testing::target(std::path::Path::new("/nonexistent/_classic_era_"));
        target.family = Family::Vanilla;
        target
    }

    fn record(owner: &str, folders: &[&str]) -> TargetRecord {
        let key = TargetKey::new();
        let mut record = TargetRecord::empty(key);
        record.addons.insert(
            TargetRecord::addon_key(owner),
            AddonRecord {
                r#ref: "gh:o/owner".parse().unwrap(),
                name: owner.to_owned(),
                version: "1".into(),
                version_confidence: VersionConfidence::Release,
                release_id: Some("gh:o/owner@1".into()),
                channel: Channel::Stable,
                pinned: false,
                reason: AddonReason::Explicit,
                required_by: Vec::new(),
                folders: folders.iter().map(|n| f(n)).collect(),
                state: AddonState::Ok,
                installed_at: now(),
                tx_id: TxId::new(),
                blob_sha256: None,
                revision: None,
                left_out: Vec::new(),
                any_flavor: false,
            },
        );
        record
    }

    fn entry(name: &str, r: &str, folders: &[&str], families: &[Family]) -> Entry {
        Entry {
            name: name.into(),
            variant: None,
            aliases: Vec::new(),
            r#ref: r.parse().unwrap(),
            families: families.to_vec(),
            folders: folders.iter().map(|n| f(n)).collect(),
            notes: None,
        }
    }

    struct Entries(Vec<Entry>);

    impl CatalogLookup for Entries {
        fn by_folder(&self, folder: &FolderName) -> Vec<&Entry> {
            self.0
                .iter()
                .filter(|e| e.folders.contains(folder))
                .collect()
        }
    }

    struct Tukui(Vec<(String, Vec<FolderName>)>);

    impl TukuiDirs for Tukui {
        fn by_directory(&self, folder: &FolderName) -> Vec<TukuiHit> {
            self.0
                .iter()
                .filter(|(_, dirs)| dirs.contains(folder))
                .map(|(slug, _)| TukuiHit {
                    slug: slug.clone(),
                    name: slug.to_uppercase(),
                })
                .collect()
        }
    }

    struct Wowi(Vec<WowiProject>);

    impl WowiDirs for Wowi {
        fn by_dir(&self, folder: &FolderName) -> Vec<WowiProject> {
            self.0
                .iter()
                .filter(|p| p.dirs.contains(folder))
                .cloned()
                .collect()
        }
    }

    fn project(
        uid: u64,
        name: &str,
        dirs: &[&str],
        downloads: u64,
        date: Option<Timestamp>,
    ) -> WowiProject {
        WowiProject {
            uid,
            name: name.into(),
            dirs: dirs.iter().map(|n| f(n)).collect(),
            families: vec![Family::Vanilla],
            downloads,
            date,
        }
    }

    struct View(BTreeMap<FolderName, ViewFolder>);

    impl DepView for View {
        fn folder(&self, dep: &FolderName) -> Option<ViewFolder> {
            self.0.get(dep).cloned()
        }
    }

    #[test]
    fn what_satisfies_a_required_folder() {
        let view = View(BTreeMap::from([
            (
                f("Planned"),
                ViewFolder::Planned {
                    addon: "Other".into(),
                },
            ),
            (
                f("Ace3"),
                ViewFolder::Present {
                    class: FolderClass::Managed,
                    owner: Some("Ace3".into()),
                },
            ),
            (
                f("Loose"),
                ViewFolder::Present {
                    class: FolderClass::Unmanaged,
                    owner: None,
                },
            ),
            (
                f("Linked"),
                ViewFolder::Present {
                    class: FolderClass::Linked,
                    owner: None,
                },
            ),
            (
                f("Broken"),
                ViewFolder::Present {
                    class: FolderClass::Invalid,
                    owner: None,
                },
            ),
        ]));
        let archive = [f("Foo"), f("Foo_Lib")];
        let check = |dep: &str| is_satisfied(&f(dep), &archive, &view);
        assert_eq!(check("Blizzard_AuctionUI"), Some(Satisfier::Client));
        assert_eq!(
            check("foo_lib"),
            Some(Satisfier::SameArchive),
            "folder identity is the key"
        );
        assert_eq!(check("Planned"), Some(Satisfier::Planned("Other".into())));
        assert_eq!(check("Ace3"), Some(Satisfier::Managed("Ace3".into())));
        assert_eq!(
            check("Loose"),
            Some(Satisfier::Present(FolderClass::Unmanaged))
        );
        assert_eq!(
            check("Linked"),
            Some(Satisfier::Present(FolderClass::Linked))
        );
        assert_eq!(
            check("Broken"),
            None,
            "a folder without a loadable TOC satisfies nothing"
        );
        assert_eq!(check("Elsewhere"), None);
    }

    #[test]
    fn needed_folders_and_bridges() {
        let folders = [
            verdict("A", &["A_Lib", "Ace3"]),
            verdict("A_Lib", &["A_Core"]),
            verdict("A_Core", &[]),
            verdict("Masque_A", &["Masque", "A"]),
            verdict("A_Unused", &[]),
        ];
        let needed = needed_folders(&f("A"), &folders);
        assert_eq!(needed, BTreeSet::from([f("A"), f("A_Lib"), f("A_Core")]));
        assert_eq!(
            bridge_dependencies(&needed, &folders),
            [BridgeDependency {
                folder: f("Masque_A"),
                requires: f("Masque"),
            }],
            "only the outside requirements of folders nobody needs"
        );
    }

    fn ambiguous(r: DepResolution) -> Option<(Vec<DepCandidate>, AmbiguityReason)> {
        match r {
            DepResolution::Ambiguous { candidates, reason } => Some((candidates, reason)),
            _ => None,
        }
    }

    fn resolve_with(
        missing: &str,
        managed: &TargetRecord,
        catalog: &Entries,
        tukui: Option<&Tukui>,
        wowi: Option<&Wowi>,
    ) -> DepResolution {
        let lookups = DepLookups {
            managed,
            released: &|_| false,
            catalog,
            tukui: tukui.map(|t| t as &dyn TukuiDirs),
            wowi: wowi.map(|w| w as &dyn WowiDirs),
            now: now(),
        };
        resolve(&f(missing), &vanilla(), &lookups)
    }

    #[test]
    fn a_managed_addon_that_lost_the_folder_is_reported_with_its_owner() {
        let managed = record("BigWigs", &["BigWigs", "BigWigs_Core"]);
        let catalog = Entries(vec![entry("BigWigs", "gh:o/bw", &["BigWigs_Core"], &[])]);
        assert_eq!(
            resolve_with("BigWigs_Core", &managed, &catalog, None, None),
            DepResolution::Missing {
                reason: MissingReason::ManagedFolderMissing,
                owner: Some("BigWigs".into()),
            }
        );
        // An update that stops shipping it lost nothing: it resolves like any folder.
        let lookups = DepLookups {
            managed: &managed,
            released: &|f| f.as_str() == "BigWigs_Core",
            catalog: &catalog,
            tukui: None,
            wowi: None,
            now: now(),
        };
        assert_eq!(
            resolve(&f("BigWigs_Core"), &vanilla(), &lookups),
            DepResolution::Resolved {
                r#ref: "gh:o/bw".parse().unwrap(),
                via: DepVia::Catalog,
                name: Some("BigWigs".into()),
                undeclared: false,
            }
        );
    }

    #[test]
    fn the_catalog_answers_for_the_family_and_breaks_ties_by_name() {
        let none = TargetRecord::empty(TargetKey::new());
        let catalog = Entries(vec![
            entry(
                "BugGrabber",
                "gh:o/buggrabber",
                &["!BugGrabber"],
                &[Family::Vanilla],
            ),
            entry(
                "RetailOnly",
                "gh:o/retail",
                &["RetailLib"],
                &[Family::Mainline],
            ),
            entry("Ace3", "gh:o/ace3", &["Ace3"], &[]),
            entry("Bundle", "gh:o/bundle", &["Ace3"], &[]),
            entry("One", "gh:o/one", &["Shared"], &[]),
            entry("Two", "gh:o/two", &["Shared"], &[]),
        ]);
        let resolved = |r: &str, name: &str| DepResolution::Resolved {
            r#ref: r.parse().unwrap(),
            via: DepVia::Catalog,
            name: Some(name.into()),
            undeclared: false,
        };
        assert_eq!(
            resolve_with("!BugGrabber", &none, &catalog, None, None),
            resolved("gh:o/buggrabber", "BugGrabber")
        );
        assert_eq!(
            resolve_with("RetailLib", &none, &catalog, None, None),
            DepResolution::Resolved {
                r#ref: "gh:o/retail".parse().unwrap(),
                via: DepVia::Catalog,
                name: Some("RetailOnly".into()),
                undeclared: true,
            },
            "an entry for another family of the generation, as a last guess"
        );
        assert_eq!(
            resolve_with("Ace3", &none, &catalog, None, None),
            resolved("gh:o/ace3", "Ace3"),
            "the entry named like the folder"
        );
        // Two entries, neither named like the folder.
        let (candidates, reason) =
            ambiguous(resolve_with("Shared", &none, &catalog, None, None)).unwrap();
        assert_eq!(reason, AmbiguityReason::SeveralCatalogEntries);
        assert_eq!(candidates.len(), 2);
    }

    /// Families rank within a generation: an entry not declaring the client's family is
    /// tried only after Tukui and WoWInterface miss, and an entry of the other
    /// generation never answers.
    #[test]
    fn undeclared_entries_come_last_and_never_cross_a_generation() {
        let none = TargetRecord::empty(TargetKey::new());
        let rio = entry("RaiderIO", "gh:o/rio", &["RaiderIO"], &[Family::Mainline]);
        let undeclared = |r: &str, name: &str| DepResolution::Resolved {
            r#ref: r.parse().unwrap(),
            via: DepVia::Catalog,
            name: Some(name.into()),
            undeclared: true,
        };
        let catalog = Entries(vec![
            rio.clone(),
            entry(
                "InFlight",
                "gh:o/inflight",
                &["InFlight"],
                &[Family::MistsLegacy],
            ),
            entry("A", "gh:o/a", &["Twice"], &[Family::Mainline]),
            entry("B", "gh:o/b", &["Twice"], &[Family::Tbc]),
        ]);
        assert_eq!(
            resolve_with("RaiderIO", &none, &catalog, None, None),
            undeclared("gh:o/rio", "RaiderIO")
        );
        // A declared entry of the same folder wins at once.
        let declared = Entries(vec![
            rio.clone(),
            entry(
                "RIO Classic",
                "gh:o/rio-classic",
                &["RaiderIO"],
                &[Family::Vanilla],
            ),
        ]);
        let found = resolve_with("RaiderIO", &none, &declared, None, None);
        assert!(
            matches!(&found, DepResolution::Resolved { undeclared: false, name: Some(n), .. } if n == "RIO Classic"),
            "{found:?}"
        );
        // Tukui and a unique WoWInterface project come first.
        let tukui = Tukui(vec![("rio".into(), vec![f("RaiderIO")])]);
        let found = resolve_with("RaiderIO", &none, &catalog, Some(&tukui), None);
        assert!(
            matches!(
                found,
                DepResolution::Resolved {
                    via: DepVia::Tukui,
                    ..
                }
            ),
            "{found:?}"
        );
        let port = Wowi(vec![project(
            9,
            "RaiderIO",
            &["RaiderIO"],
            100,
            days_ago(5),
        )]);
        let found = resolve_with("RaiderIO", &none, &catalog, None, Some(&port));
        assert!(
            matches!(
                found,
                DepResolution::Resolved {
                    via: DepVia::Wowi,
                    ..
                }
            ),
            "{found:?}"
        );
        // An abandoned WoWInterface answer yields to the undeclared entry.
        let old = Wowi(vec![project(
            9,
            "RaiderIO",
            &["RaiderIO"],
            100,
            days_ago(800),
        )]);
        assert_eq!(
            resolve_with("RaiderIO", &none, &catalog, None, Some(&old)),
            undeclared("gh:o/rio", "RaiderIO")
        );
        // Two undeclared entries tie.
        let (_, reason) = ambiguous(resolve_with("Twice", &none, &catalog, None, None)).unwrap();
        assert_eq!(reason, AmbiguityReason::SeveralCatalogEntries);
        // Never across a generation, in either direction.
        assert!(matches!(
            resolve_with("InFlight", &none, &catalog, None, None),
            DepResolution::Missing { .. }
        ));
        let mut legacy = vanilla();
        legacy.family = Family::WrathLegacy;
        legacy.install_kind = crate::rules::InstallKind::Custom;
        let lookups = DepLookups {
            managed: &none,
            released: &|_| false,
            catalog: &catalog,
            tukui: None,
            wowi: None,
            now: now(),
        };
        assert!(matches!(
            resolve(&f("RaiderIO"), &legacy, &lookups),
            DepResolution::Missing { .. }
        ));
        assert_eq!(
            resolve(&f("InFlight"), &legacy, &lookups),
            undeclared("gh:o/inflight", "InFlight")
        );
        // A variant is one family's build: a family no variant covers gets none.
        let builds = Entries(vec![
            Entry {
                variant: Some("r152".into()),
                ..entry(
                    "BugGrabber",
                    "gh:o/bg-r152",
                    &["!BugGrabber"],
                    &[Family::WrathLegacy],
                )
            },
            Entry {
                variant: Some("r130".into()),
                ..entry(
                    "BugGrabber",
                    "gh:o/bg-r130",
                    &["!BugGrabber"],
                    &[Family::TbcLegacy],
                )
            },
        ]);
        let mut cata = legacy.clone();
        cata.family = Family::CataLegacy;
        let lookups = DepLookups {
            catalog: &builds,
            ..lookups
        };
        assert!(
            matches!(
                resolve(&f("!BugGrabber"), &cata, &lookups),
                DepResolution::Missing { .. }
            ),
            "never another family's build"
        );
    }

    #[test]
    fn tukui_then_wowinterface() {
        let none = TargetRecord::empty(TargetKey::new());
        let catalog = Entries(Vec::new());
        let tukui = Tukui(vec![("elvui".into(), vec![f("ElvUI"), f("ElvUI_Options")])]);
        assert_eq!(
            resolve_with("ElvUI_Options", &none, &catalog, Some(&tukui), None),
            DepResolution::Resolved {
                r#ref: "tukui:elvui".parse().unwrap(),
                via: DepVia::Tukui,
                name: Some("ELVUI".into()),
                undeclared: false,
            }
        );

        let wowi = Wowi(vec![
            project(1, "LibStub", &["LibStub"], 900, days_ago(30)),
            project(2, "A Bundle", &["LibStub", "Other"], 5_000, days_ago(10)),
            project(3, "Old Lib", &["OldLib"], 10, days_ago(800)),
            project(4, "Mixed", &["Mix", "MixA"], 100, days_ago(1)),
            project(5, "Mixed Too", &["Mix", "MixB"], 200, days_ago(1)),
            project(6, "Third", &["Mix", "MixC"], 50, days_ago(1)),
            project(7, "Fourth", &["Mix", "MixD"], 10, days_ago(1)),
        ]);
        assert_eq!(
            resolve_with("LibStub", &none, &catalog, None, Some(&wowi)),
            DepResolution::Resolved {
                r#ref: "wowi:1".parse().unwrap(),
                via: DepVia::Wowi,
                name: Some("LibStub".into()),
                undeclared: false,
            },
            "the single-folder project beats a bigger bundle"
        );
        // Too old to choose.
        let (candidates, reason) =
            ambiguous(resolve_with("OldLib", &none, &catalog, None, Some(&wowi))).unwrap();
        assert_eq!(
            (reason, candidates.len()),
            (AmbiguityReason::WowiAbandoned, 1)
        );
        // No project is the folder's own.
        let (candidates, reason) =
            ambiguous(resolve_with("Mix", &none, &catalog, None, Some(&wowi))).unwrap();
        assert_eq!(reason, AmbiguityReason::WowiAmbiguous);
        let uids: Vec<String> = candidates.iter().map(|c| c.r#ref.to_string()).collect();
        assert_eq!(
            uids,
            ["wowi:5", "wowi:4", "wowi:6"],
            "the top three, by downloads"
        );
        assert_eq!(
            resolve_with("Nowhere", &none, &catalog, Some(&tukui), Some(&wowi)),
            DepResolution::Missing {
                reason: MissingReason::NoSource,
                owner: None,
            }
        );
    }

    #[test]
    fn names_normalise() {
        assert_eq!(normalized("My Addon-2"), "myaddon2");
        assert_eq!(normalized("!BugGrabber"), "buggrabber");
    }
}
