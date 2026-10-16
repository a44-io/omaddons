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
        for dep in &v.dependencies {
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
    // 1. A managed addon lost it: report, never repair.
    if let Some(owner) = lookups
        .managed
        .addons
        .values()
        .find(|a| a.folders.contains(missing))
    {
        return DepResolution::Missing {
            reason: MissingReason::ManagedFolderMissing,
            owner: Some(owner.name.clone()),
        };
    }
    // 2. The catalog, for this family; a name equal to the folder breaks a tie.
    let entries: Vec<&Entry> = lookups
        .catalog
        .by_folder(missing)
        .into_iter()
        .filter(|e| e.families.is_empty() || e.families.contains(&target.family))
        .collect();
    let named: Vec<&Entry> = entries
        .iter()
        .copied()
        .filter(|e| e.name.eq_ignore_ascii_case(missing.as_str()))
        .collect();
    match (entries.as_slice(), named.as_slice()) {
        ([], _) => {}
        ([only], _) | (_, [only]) => {
            return DepResolution::Resolved {
                r#ref: only.r#ref.clone(),
                via: DepVia::Catalog,
                name: Some(only.name.clone()),
            };
        }
        (all, named) => {
            let tied = if named.is_empty() { all } else { named };
            return DepResolution::Ambiguous {
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
            };
        }
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
        };
    }
    // 4. WoWInterface, ranked and guarded.
    if let Some(wowi) = lookups.wowi {
        let projects = wowi.by_dir(missing);
        if !projects.is_empty() {
            return from_wowi(missing, target, lookups.now, projects);
        }
    }
    DepResolution::Missing {
        reason: MissingReason::NoSource,
        owner: None,
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
            dependencies: deps.iter().map(|d| f(d)).collect(),
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
            },
        );
        record
    }

    fn entry(name: &str, r: &str, folders: &[&str], families: &[Family]) -> Entry {
        Entry {
            name: name.into(),
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
        };
        assert_eq!(
            resolve_with("!BugGrabber", &none, &catalog, None, None),
            resolved("gh:o/buggrabber", "BugGrabber")
        );
        assert_eq!(
            resolve_with("RetailLib", &none, &catalog, None, None),
            DepResolution::Missing {
                reason: MissingReason::NoSource,
                owner: None,
            },
            "an entry for another family falls through"
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
