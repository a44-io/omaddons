//! `external_artifacts`: what sits in a client's `Interface/AddOns`: managed addons
//! changed by hand or by another install, and folders the client's record or load rules
//! disagree with.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::doctor::budget::Budget;
use crate::doctor::facts::{StatusFacts, TargetFacts};
use crate::doctor::finding::{
    Confidence, Detected, Evidence, FolderEvidence, Observation, Subject,
};
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;
use crate::drift::{DriftMode, DriftState};
use crate::engine::inspect::SkippedFolder;
use crate::inventory::{FolderClass, FolderEntry, Inventory};
use crate::model::{AddonRecord, AddonState, FolderName, Target, TargetKey};
use crate::state::records::TargetRecord;
use crate::toc::{Verdict, WrongFlavorReason};

// ---------------------------------------------------------------------------------------
// What the inventory rows share.

/// A registered client in scope, with what the inventory rows read.
pub(crate) struct KeyedClient<'v> {
    /// The discovered target: its rules, family, interface and client dir.
    pub target: &'v Target,
    pub key: TargetKey,
    /// The registry id, else the key's string (`DoctorView::target_ref`).
    pub id: String,
    pub record: Option<&'v TargetRecord>,
    pub inventory: Option<&'v Inventory>,
}

impl KeyedClient<'_> {
    fn reference(&self) -> ClientRef<'_> {
        ClientRef {
            id: &self.id,
            key: self.key,
        }
    }
}

/// The client a finding is about.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientRef<'a> {
    pub id: &'a str,
    pub key: TargetKey,
}

impl ClientRef<'_> {
    fn subject(&self) -> Subject {
        Subject::target(self.id, self.key)
    }
}

/// The discovered clients with a target key, in `-t` scope, sorted by id. A client
/// omaddons never registered has no record and no managed folder, so it is never a
/// subject here. An unavailable record or inventory is `None` (the view reports the
/// layer skipped).
pub(crate) fn keyed_clients<'v>(view: &'v DoctorView, sel: &Selection) -> Vec<KeyedClient<'v>> {
    let Some(world) = view.world() else {
        return Vec::new();
    };
    // A scope that does not resolve made the run fail before any detector ran.
    let scope = sel.scope(view).ok();
    let records = view.records();
    let mut clients: Vec<KeyedClient<'v>> = world
        .resolved
        .targets
        .iter()
        .filter_map(|d| {
            let key = d.target.key?;
            if !scope
                .as_ref()
                .is_none_or(|scope| scope.includes_client(Some(key), &d.target.id))
            {
                return None;
            }
            Some(KeyedClient {
                target: &d.target,
                key,
                id: view.target_ref(key).id,
                record: records
                    .and_then(|r| r.get(&key))
                    .map(|loaded| &loaded.value),
                inventory: view.inventory(d),
            })
        })
        .collect();
    clients.sort_by(|a, b| a.id.cmp(&b.id));
    clients
}

/// The inventory entry of each recorded folder of `addon`: the entry with the folder's
/// key; when the key has several spellings, only the one spelled as the record spells it
/// (none: that key is `case-collision`'s). A folder with no entry is missing
/// (`record-folder-missing`'s).
pub(crate) fn recorded_entries<'i, 'a>(
    addon: &'a AddonRecord,
    inv: &'i Inventory,
) -> Vec<(&'i FolderEntry, &'a FolderName)> {
    addon
        .folders
        .iter()
        .filter_map(|folder| {
            let spellings: Vec<&FolderEntry> = inv
                .folders
                .iter()
                .filter(|e| e.key == folder.key())
                .collect();
            let entry = match spellings.as_slice() {
                [one] => Some(*one),
                many => many.iter().copied().find(|e| e.name == folder.as_str()),
            }?;
            Some((entry, folder))
        })
        .collect()
}

/// Runs `rule` on each keyed client in scope, checking the budget before each.
fn each_client(
    view: &DoctorView,
    sel: &Selection,
    budget: &mut Budget,
    rule: impl Fn(&KeyedClient<'_>) -> Vec<Observation>,
) -> Detected {
    let mut found = Vec::new();
    for client in keyed_clients(view, sel) {
        if budget.check().is_err() {
            return Detected::cut_short(found, budget);
        }
        found.extend(rule(&client));
    }
    Detected::complete(found)
}

/// `names`, the first three of more than three, then "and <k> more".
fn listed(names: &[String]) -> String {
    match names {
        [first @ .., last] if names.len() <= 3 && !first.is_empty() => {
            format!("{} and {last}", first.join(", "))
        }
        [only] => only.clone(),
        _ => {
            let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
            format!(
                "{} and {} more",
                shown.join(", "),
                names.len().saturating_sub(3)
            )
        }
    }
}

/// `owners` comma-separated: the first three of more than three, then "and <k> more".
fn claimants(owners: &[String]) -> String {
    let shown: Vec<&str> = owners.iter().take(3).map(String::as_str).collect();
    match owners.len().saturating_sub(3) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

// ---------------------------------------------------------------------------------------
// `ownership-conflict`, `case-collision`, `wrong-flavor`, `out-of-date-toc`.

/// `ownership-conflict`: a folder several addons of one record claim.
pub(crate) fn ownership_conflict(
    view: &DoctorView,
    sel: &Selection,
    budget: &mut Budget,
) -> Detected {
    each_client(view, sel, budget, |c| match c.inventory {
        Some(inv) => ownership_conflicts(&c.reference(), &c.target.addons_dir(), inv),
        None => Vec::new(),
    })
}

/// `case-collision`: AddOns entries whose names differ only in case.
pub(crate) fn case_collision(view: &DoctorView, sel: &Selection, budget: &mut Budget) -> Detected {
    each_client(view, sel, budget, |c| match c.inventory {
        Some(inv) => case_collisions(&c.reference(), &c.target.addons_dir(), inv),
        None => Vec::new(),
    })
}

/// `wrong-flavor`: a managed addon's folder the client no longer loads.
pub(crate) fn wrong_flavor(view: &DoctorView, sel: &Selection, budget: &mut Budget) -> Detected {
    each_client(view, sel, budget, |c| match (c.record, c.inventory) {
        (Some(record), Some(inv)) => wrong_flavor_in(&c.reference(), c.target, record, inv),
        _ => Vec::new(),
    })
}

/// `out-of-date-toc`: a managed addon the client loads only as out of date.
pub(crate) fn out_of_date_toc(view: &DoctorView, sel: &Selection, budget: &mut Budget) -> Detected {
    each_client(view, sel, budget, |c| match (c.record, c.inventory) {
        (Some(record), Some(inv)) => out_of_date_in(&c.reference(), c.target, record, inv),
        _ => Vec::new(),
    })
}

/// `ownership-conflict`'s rule: once per key an entry of which several addons of this
/// record claim, whatever its spellings.
pub(crate) fn ownership_conflicts(
    c: &ClientRef<'_>,
    addons_dir: &Path,
    inv: &Inventory,
) -> Vec<Observation> {
    let mut seen = BTreeSet::new();
    inv.folders
        .iter()
        .filter(|e| e.ownership_conflict && seen.insert(e.key.as_str()))
        .map(|e| Observation {
            subject: c.subject().folder_entry(&e.key, &e.name),
            title: format!(
                "{} is claimed by {} addons of this client ({})",
                e.name,
                e.owners.len(),
                claimants(&e.owners)
            ),
            confidence: Confidence::High,
            addon: None,
            folder: Some(e.name.clone()),
            evidence: Evidence {
                path: Some(addons_dir.join(&e.name)),
                owners: Some(e.owners.clone()),
                ..Evidence::default()
            },
        })
        .collect()
}

/// `case-collision`'s rule: once per key with two or more spellings, of any class.
pub(crate) fn case_collisions(
    c: &ClientRef<'_>,
    addons_dir: &Path,
    inv: &Inventory,
) -> Vec<Observation> {
    let mut by_key: BTreeMap<&str, Vec<&FolderEntry>> = BTreeMap::new();
    for e in inv.folders.iter().filter(|e| e.case_duplicate) {
        by_key.entry(e.key.as_str()).or_default().push(e);
    }
    by_key
        .into_values()
        .filter_map(|mut spellings| {
            spellings.sort_by(|a, b| a.name.cmp(&b.name));
            let first = *spellings.first()?;
            if spellings.len() < 2 {
                return None;
            }
            let names: Vec<String> = spellings.iter().map(|e| e.name.clone()).collect();
            Some(Observation {
                subject: c.subject().folder_entry(&first.key, &first.name),
                title: format!("{} differ only in case", listed(&names)),
                confidence: Confidence::High,
                addon: None,
                folder: Some(first.name.clone()),
                evidence: Evidence {
                    paths: Some(names.iter().map(|name| addons_dir.join(name)).collect()),
                    count: Some(names.len() as u64),
                    ..Evidence::default()
                },
            })
        })
        .collect()
}

/// Why the client does not load `e`, when that is a finding: a wrong-flavor verdict
/// other than an unknown client interface (which every folder of such a client gets),
/// or no TOC it loads (install's `no_loadable_toc`).
fn not_loaded(e: &FolderEntry) -> Option<WrongFlavorReason> {
    match &e.verdict.as_ref()?.verdict {
        Verdict::NoToc => Some(WrongFlavorReason::NoLoadableToc),
        Verdict::WrongFlavor { reason } => match reason {
            WrongFlavorReason::TargetInterfaceUnknown
            | WrongFlavorReason::DependencySkipped { .. } => None,
            other => Some(other.clone()),
        },
        Verdict::Compatible { .. } => None,
    }
}

/// A reason's wire name (`FolderEvidence::reason`).
fn reason_name(reason: &WrongFlavorReason) -> &'static str {
    match reason {
        WrongFlavorReason::GameTypeExcluded => "game_type_excluded",
        WrongFlavorReason::NoInterface => "no_interface",
        WrongFlavorReason::InterfaceNotInFamily { .. } => "interface_not_in_family",
        WrongFlavorReason::BelowClientFloor { .. } => "below_client_floor",
        WrongFlavorReason::NoLoadableToc => "no_loadable_toc",
        WrongFlavorReason::TargetInterfaceUnknown => "target_interface_unknown",
        WrongFlavorReason::DependencySkipped { .. } => "dependency_skipped",
    }
}

fn folder_evidence(e: &FolderEntry, reason: Option<&WrongFlavorReason>) -> FolderEvidence {
    FolderEvidence {
        name: e.name.clone(),
        toc: e
            .verdict
            .as_ref()
            .and_then(|v| v.loaded.as_ref())
            .map(|loaded| loaded.file.clone()),
        reason: reason.map(reason_name),
        interfaces: e
            .toc
            .as_ref()
            .map(|toc| toc.interfaces.clone())
            .unwrap_or_default(),
        floor: match reason {
            Some(WrongFlavorReason::BelowClientFloor { floor, .. }) => Some(*floor),
            _ => None,
        },
    }
}

/// The recorded entries of `addon` a rule judges: its own (never another client's
/// `Shared` folder) and readable.
fn judged<'i>(addon: &AddonRecord, inv: &'i Inventory) -> Vec<(&'i FolderEntry, FolderName)> {
    let mut entries: Vec<(&FolderEntry, FolderName)> = recorded_entries(addon, inv)
        .into_iter()
        .filter(|(e, _)| e.class != FolderClass::Shared && !e.unreadable)
        .map(|(e, recorded)| (e, recorded.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    entries
}

/// `wrong-flavor`'s rule: once per addon (never an `--any-flavor` one, whose builds the
/// user chose to run) with a recorded folder the client does not load.
pub(crate) fn wrong_flavor_in(
    c: &ClientRef<'_>,
    target: &Target,
    record: &TargetRecord,
    inv: &Inventory,
) -> Vec<Observation> {
    record
        .addons
        .values()
        .filter(|addon| !addon.any_flavor)
        .filter_map(|addon| {
            let bad: Vec<(&FolderEntry, FolderName, WrongFlavorReason)> = judged(addon, inv)
                .into_iter()
                .filter_map(|(e, recorded)| Some((e, recorded, not_loaded(e)?)))
                .collect();
            let (first, recorded, reason) = bad.first()?;
            let title = if bad.len() == 1 {
                let name = FolderName::new(&first.name).unwrap_or_else(|_| recorded.clone());
                let left_out = SkippedFolder {
                    name,
                    reason: reason.clone(),
                }
                .left_out();
                format!(
                    "{} no longer loads on this client: {}",
                    addon.name,
                    left_out.describe()
                )
            } else {
                format!(
                    "{}: {} folders no longer load on this client",
                    addon.name,
                    bad.len()
                )
            };
            Some(addon_observation(
                c,
                target,
                addon,
                title,
                bad.iter()
                    .map(|(e, _, why)| folder_evidence(e, Some(why)))
                    .collect(),
            ))
        })
        .collect()
}

/// `out-of-date-toc`'s rule: once per addon (`--any-flavor` ones included) with a
/// recorded folder the client loads only as out of date.
pub(crate) fn out_of_date_in(
    c: &ClientRef<'_>,
    target: &Target,
    record: &TargetRecord,
    inv: &Inventory,
) -> Vec<Observation> {
    record
        .addons
        .values()
        .filter_map(|addon| {
            let old: Vec<FolderEvidence> = judged(addon, inv)
                .into_iter()
                .filter(|(e, _)| {
                    e.verdict.as_ref().is_some_and(|v| {
                        matches!(v.verdict, Verdict::Compatible { out_of_date: true })
                    })
                })
                .map(|(e, _)| folder_evidence(e, None))
                .collect();
            if old.is_empty() {
                return None;
            }
            let title = if old.len() == 1 {
                format!("{} loads only as out of date on this client", addon.name)
            } else {
                format!(
                    "{} loads only as out of date on this client ({} folders)",
                    addon.name,
                    old.len()
                )
            };
            Some(addon_observation(c, target, addon, title, old))
        })
        .collect()
}

/// One addon's finding: its judged folders and the client's interface.
fn addon_observation(
    c: &ClientRef<'_>,
    target: &Target,
    addon: &AddonRecord,
    title: String,
    folders: Vec<FolderEvidence>,
) -> Observation {
    Observation {
        subject: c.subject().addon(&addon.name),
        title,
        confidence: Confidence::High,
        addon: Some(addon.name.clone()),
        folder: None,
        evidence: Evidence {
            expected: Some(target.interface.to_string()),
            folders: Some(folders),
            ..Evidence::default()
        },
    }
}

// ---------------------------------------------------------------------------------------
// `managed-files-modified`, `ownership-transferred`.

/// `managed-files-modified`, from the Discovery and Records layers: each managed file
/// checked stat-first, as plain `status` checks it.
pub(crate) fn managed_files_modified(
    view: &DoctorView,
    sel: &Selection,
    _: &mut Budget,
) -> Detected {
    let facts = StatusFacts {
        targets: super::drift_facts(view, sel, DriftMode::Stat),
        machine: Default::default(),
    };
    Detected::complete(managed_files_modified_in(&facts))
}

/// `managed-files-modified`'s rule: one finding per managed addon whose files changed on
/// disk.
pub(crate) fn managed_files_modified_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
        .targets
        .iter()
        .flat_map(|t| {
            t.drift
                .iter()
                .filter(|d| d.state == DriftState::Modified)
                .map(|d| {
                    let files = if d.files == 1 { "file" } else { "files" };
                    Observation {
                        subject: Subject::target(&t.id, t.key).addon(&d.addon),
                        title: format!("{}'s files changed on disk ({} {files})", d.addon, d.files),
                        confidence: Confidence::High,
                        addon: Some(d.addon.clone()),
                        folder: None,
                        evidence: Evidence {
                            count: Some(d.files),
                            ..Evidence::default()
                        },
                    }
                })
        })
        .collect()
}

/// `ownership-transferred`, from the Records layer alone: each record with degraded
/// addons.
pub(crate) fn ownership_transferred(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    let targets = view
        .records()
        .into_iter()
        .flat_map(|records| records.available())
        .map(|(key, loaded)| TargetFacts {
            degraded: loaded
                .value
                .addons
                .values()
                .filter(|addon| addon.state == AddonState::Degraded)
                .count(),
            ..TargetFacts::new(view.target_ref(*key).id, *key)
        })
        .collect();
    let facts = StatusFacts {
        targets,
        machine: Default::default(),
    };
    Detected::complete(ownership_transferred_in(&facts))
}

/// `ownership-transferred`'s rule: a target whose addons lost folders to other addons
/// (`--force`).
pub(crate) fn ownership_transferred_in(facts: &StatusFacts) -> Vec<Observation> {
    facts
        .targets
        .iter()
        .filter(|t| t.degraded > 0)
        .map(|t| {
            let n = t.degraded;
            let addons = if n == 1 { "addon" } else { "addons" };
            Observation {
                subject: Subject::target(&t.id, t.key),
                title: format!("{n} {addons} lost a folder to another addon"),
                confidence: Confidence::High,
                addon: None,
                folder: None,
                evidence: Evidence {
                    count: Some(n as u64),
                    ..Evidence::default()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::*;
    use crate::doctor::finding::Finding;
    use crate::doctor::registry::{ids, row};
    use crate::inventory::{Counts, OwnedOn, TocSummary};
    use crate::model::{AddonReason, Channel, TxId, VersionConfidence};
    use crate::toc::{LoadedTocSummary, TocLevel, VerdictReport};

    const KEY: &str = "01M5ZQ7Y3KJ8V9W2X4C6B0N1RT";
    const ADDONS: &str = "/wow/_retail_/Interface/AddOns";

    fn client() -> ClientRef<'static> {
        ClientRef {
            id: "retail",
            key: KEY.parse().unwrap(),
        }
    }

    fn entry(name: &str, class: FolderClass) -> FolderEntry {
        FolderEntry {
            name: name.to_owned(),
            key: name.to_ascii_lowercase(),
            class,
            owners: Vec::new(),
            case_duplicate: false,
            ownership_conflict: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
            owned_on: Vec::new(),
        }
    }

    /// A managed folder whose TOC (listing `interfaces`) gets `verdict`.
    fn judged_entry(name: &str, verdict: Verdict, interfaces: &[u32]) -> FolderEntry {
        let loads = !matches!(verdict, Verdict::NoToc);
        FolderEntry {
            toc: loads.then(|| TocSummary {
                file: format!("{name}.toc"),
                title: None,
                title_plain: None,
                version: None,
                author: None,
                interfaces: interfaces.to_vec(),
                x_ids: Default::default(),
                website: None,
                dependencies: Vec::new(),
                optional_deps: Vec::new(),
                tukui_project_folders: Vec::new(),
                saved_variables: Vec::new(),
            }),
            verdict: Some(VerdictReport {
                loaded: loads.then(|| LoadedTocSummary {
                    file: format!("{name}.toc"),
                    level: TocLevel::Plain,
                    suffix: None,
                    case_ambiguous: false,
                    interface_raw: None,
                }),
                verdict,
                warnings: Vec::new(),
            }),
            ..entry(name, FolderClass::Managed)
        }
    }

    /// `folders` as the inventory lists them: sorted by key then name, each key with
    /// several spellings marked.
    fn inventory(mut folders: Vec<FolderEntry>) -> Inventory {
        let keys: Vec<String> = folders.iter().map(|e| e.key.clone()).collect();
        for e in &mut folders {
            e.case_duplicate = keys.iter().filter(|k| **k == e.key).count() > 1;
        }
        folders.sort_by(|a, b| (&a.key, &a.name).cmp(&(&b.key, &b.name)));
        Inventory {
            addons_dir_missing: false,
            folders,
            missing: Vec::new(),
            counts: Counts::default(),
            stray_files: 0,
        }
    }

    fn addon(name: &str, folders: &[&str], any_flavor: bool) -> AddonRecord {
        AddonRecord {
            r#ref: format!("gh:o/{}", name.to_ascii_lowercase())
                .parse()
                .unwrap(),
            name: name.to_owned(),
            version: "v1".to_owned(),
            version_confidence: VersionConfidence::Release,
            release_id: None,
            channel: Channel::Stable,
            pinned: false,
            reason: AddonReason::Explicit,
            required_by: Vec::new(),
            folders: folders
                .iter()
                .map(|f| FolderName::new(f).unwrap())
                .collect(),
            state: AddonState::Ok,
            installed_at: "2026-10-14T21:00:00Z".parse().unwrap(),
            tx_id: TxId::new(),
            blob_sha256: None,
            revision: None,
            left_out: Vec::new(),
            any_flavor,
        }
    }

    fn record(addons: Vec<AddonRecord>) -> TargetRecord {
        let mut record = TargetRecord::empty(KEY.parse().unwrap());
        for a in addons {
            record.addons.insert(TargetRecord::addon_key(&a.name), a);
        }
        record
    }

    fn retail() -> Target {
        crate::engine::testing::target(Path::new("/wow/_retail_"))
    }

    /// `observations` as `id`'s row reports them.
    fn findings(id: &str, observations: Vec<Observation>) -> Vec<Value> {
        let row = row(id).unwrap();
        observations
            .into_iter()
            .map(|obs| {
                assert!(obs.evidence.is_private_safe(), "{:?}", obs.evidence);
                let finding: Finding = row.finding(obs);
                serde_json::to_value(&finding).unwrap()
            })
            .collect()
    }

    fn pretty(value: &impl serde::Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }

    fn owned(mut e: FolderEntry, owners: &[&str]) -> FolderEntry {
        e.owners = owners.iter().map(|o| (*o).to_owned()).collect();
        e.ownership_conflict = owners.len() > 1;
        e
    }

    #[test]
    fn ownership_conflict_cases() {
        let managed = |name| entry(name, FolderClass::Managed);
        // (case, entries, each finding's instance suffix, title and owners)
        type Case = (
            &'static str,
            Vec<FolderEntry>,
            Vec<(&'static str, &'static str, Value)>,
        );
        let cases: Vec<Case> = vec![
            (
                "two_addons_one_folder",
                vec![owned(managed("Foo"), &["Bar", "Foo"])],
                vec![(
                    ":folder=foo",
                    "Foo is claimed by 2 addons of this client (Bar, Foo)",
                    json!(["Bar", "Foo"]),
                )],
            ),
            (
                "three_addons_one_folder",
                vec![owned(managed("Foo"), &["A", "B", "C"])],
                vec![(
                    ":folder=foo",
                    "Foo is claimed by 3 addons of this client (A, B, C)",
                    json!(["A", "B", "C"]),
                )],
            ),
            (
                "five_owners_title_is_cut",
                vec![owned(managed("Foo"), &["A", "B", "C", "D", "E"])],
                vec![(
                    ":folder=foo",
                    "Foo is claimed by 5 addons of this client (A, B, C and 2 more)",
                    json!(["A", "B", "C", "D", "E"]),
                )],
            ),
            (
                "conflict_on_a_case_pair_reported_once",
                vec![
                    owned(managed("Foo"), &["Bar", "Foo"]),
                    owned(managed("foo"), &["Bar", "Foo"]),
                ],
                vec![(
                    ":folder=foo",
                    "Foo is claimed by 2 addons of this client (Bar, Foo)",
                    json!(["Bar", "Foo"]),
                )],
            ),
            ("no_conflict", vec![owned(managed("Foo"), &["Foo"])], vec![]),
            (
                "shared_folder_is_not_a_conflict",
                vec![FolderEntry {
                    owned_on: vec![OwnedOn {
                        target: "classic".to_owned(),
                        addon: "Foo".to_owned(),
                    }],
                    ..entry("Foo", FolderClass::Shared)
                }],
                vec![],
            ),
        ];
        for (case, entries, want) in cases {
            let inv = inventory(entries);
            let got = findings(
                ids::OWNERSHIP_CONFLICT,
                ownership_conflicts(&client(), Path::new(ADDONS), &inv),
            );
            let ctx = format!("{case}\n{}\n{}", pretty(&inv.folders), pretty(&got));
            assert_eq!(got.len(), want.len(), "{ctx}");
            for (f, (instance, title, owners)) in got.iter().zip(want) {
                assert!(f["instance"].as_str().unwrap().ends_with(instance), "{ctx}");
                assert_eq!(f["title"], title, "{ctx}");
                assert_eq!(f["evidence"]["owners"], owners, "{ctx}");
                assert_eq!(f["evidence"]["path"], format!("{ADDONS}/Foo"), "{ctx}");
                assert_eq!(f["remediation"]["argv"], Value::Null, "{ctx}: prose");
            }
        }
    }

    #[test]
    fn case_collision_cases() {
        // (case, entries, each finding's instance suffix, spellings and title)
        type Case = (
            &'static str,
            Vec<FolderEntry>,
            Vec<(&'static str, Vec<&'static str>, &'static str)>,
        );
        let cases: Vec<Case> = vec![
            (
                "two_spellings",
                vec![
                    entry("foo", FolderClass::Managed),
                    entry("Foo", FolderClass::Unmanaged),
                ],
                vec![(
                    ":folder=foo",
                    vec!["Foo", "foo"],
                    "Foo and foo differ only in case",
                )],
            ),
            (
                "three_spellings",
                vec![
                    entry("FOO", FolderClass::Unmanaged),
                    entry("foo", FolderClass::Managed),
                    entry("Foo", FolderClass::Unmanaged),
                ],
                vec![(
                    ":folder=foo",
                    vec!["FOO", "Foo", "foo"],
                    "FOO, Foo and foo differ only in case",
                )],
            ),
            (
                "managed_and_stray_file",
                vec![
                    entry("Foo", FolderClass::Managed),
                    entry("FOO", FolderClass::Unmanaged),
                ],
                vec![(
                    ":folder=foo",
                    vec!["FOO", "Foo"],
                    "FOO and Foo differ only in case",
                )],
            ),
            (
                "link_and_folder",
                vec![
                    entry("Bar", FolderClass::Linked),
                    entry("bar", FolderClass::Unmanaged),
                ],
                vec![(
                    ":folder=bar",
                    vec!["Bar", "bar"],
                    "Bar and bar differ only in case",
                )],
            ),
            (
                "invalid_names",
                vec![
                    entry(".git", FolderClass::Invalid),
                    entry(".GIT", FolderClass::Invalid),
                ],
                vec![(
                    ":folder=.git",
                    vec![".GIT", ".git"],
                    ".GIT and .git differ only in case",
                )],
            ),
            (
                "none",
                vec![
                    entry("Foo", FolderClass::Managed),
                    entry("Bar", FolderClass::Unmanaged),
                ],
                vec![],
            ),
        ];
        for (case, entries, want) in cases {
            let inv = inventory(entries);
            let got = findings(
                ids::CASE_COLLISION,
                case_collisions(&client(), Path::new(ADDONS), &inv),
            );
            let ctx = format!("{case}\n{}\n{}", pretty(&inv.folders), pretty(&got));
            assert_eq!(got.len(), want.len(), "{ctx}");
            for (f, (instance, names, title)) in got.iter().zip(want) {
                assert!(f["instance"].as_str().unwrap().ends_with(instance), "{ctx}");
                assert_eq!(f["title"], title, "{ctx}");
                assert_eq!(f["folder"], names[0], "{ctx}: the bytewise first");
                let paths: Vec<String> = names.iter().map(|n| format!("{ADDONS}/{n}")).collect();
                assert_eq!(f["evidence"]["paths"], json!(paths), "{ctx}");
                assert_eq!(f["evidence"]["count"], names.len(), "{ctx}");
                assert_eq!(f["remediation"]["argv"], Value::Null, "{ctx}: prose");
            }
        }
    }

    fn wrong(reason: WrongFlavorReason) -> Verdict {
        Verdict::WrongFlavor { reason }
    }

    const CURRENT: Verdict = Verdict::Compatible { out_of_date: false };
    const OUT_OF_DATE: Verdict = Verdict::Compatible { out_of_date: true };

    #[test]
    fn wrong_flavor_cases() {
        let below = || {
            wrong(WrongFlavorReason::BelowClientFloor {
                interfaces: vec![110_207],
                floor: 120_000,
            })
        };
        // (case, folders on disk, the record's addons, the finding's title and folders)
        type Case = (
            &'static str,
            Vec<FolderEntry>,
            Vec<AddonRecord>,
            Option<(&'static str, Value)>,
        );
        let cases: Vec<Case> = vec![
            (
                "game_type_excluded",
                vec![judged_entry(
                    "Foo",
                    wrong(WrongFlavorReason::GameTypeExcluded),
                    &[120_105],
                )],
                vec![addon("Foo", &["Foo"], false)],
                Some((
                    "Foo no longer loads on this client: Foo (gated to other game types)",
                    json!([{"name": "Foo", "toc": "Foo.toc", "reason": "game_type_excluded", "interfaces": [120_105]}]),
                )),
            ),
            (
                "no_interface",
                vec![judged_entry(
                    "Foo",
                    wrong(WrongFlavorReason::NoInterface),
                    &[],
                )],
                vec![addon("Foo", &["Foo"], false)],
                Some((
                    "Foo no longer loads on this client: Foo (no Interface this client reads)",
                    json!([{"name": "Foo", "toc": "Foo.toc", "reason": "no_interface"}]),
                )),
            ),
            (
                "interface_not_in_family",
                vec![judged_entry(
                    "Foo",
                    wrong(WrongFlavorReason::InterfaceNotInFamily {
                        interfaces: vec![11_507],
                        family: crate::rules::Family::Mainline,
                    }),
                    &[11_507],
                )],
                vec![addon("Foo", &["Foo"], false)],
                Some((
                    "Foo no longer loads on this client: Foo (lists only interface 11507)",
                    json!([{"name": "Foo", "toc": "Foo.toc", "reason": "interface_not_in_family", "interfaces": [11_507]}]),
                )),
            ),
            (
                "below_client_floor",
                vec![judged_entry("OmniCC", below(), &[110_207])],
                vec![addon("OmniCC", &["OmniCC"], false)],
                Some((
                    "OmniCC no longer loads on this client: OmniCC (interface 110207, below the client's floor 120000)",
                    json!([{"name": "OmniCC", "toc": "OmniCC.toc", "reason": "below_client_floor", "interfaces": [110_207], "floor": 120_000}]),
                )),
            ),
            (
                "no_toc_is_no_loadable_toc",
                vec![judged_entry("Foo", Verdict::NoToc, &[])],
                vec![addon("Foo", &["Foo"], false)],
                Some((
                    "Foo no longer loads on this client: Foo (no TOC this client loads)",
                    json!([{"name": "Foo", "reason": "no_loadable_toc"}]),
                )),
            ),
            (
                "target_interface_unknown_is_no_finding",
                vec![judged_entry(
                    "Foo",
                    wrong(WrongFlavorReason::TargetInterfaceUnknown),
                    &[120_105],
                )],
                vec![addon("Foo", &["Foo"], false)],
                None,
            ),
            (
                "any_flavor_addon_is_skipped",
                vec![judged_entry("Foo", below(), &[110_207])],
                vec![addon("Foo", &["Foo"], true)],
                None,
            ),
            (
                "compatible_is_no_finding",
                vec![judged_entry("Foo", CURRENT, &[120_105])],
                vec![addon("Foo", &["Foo"], false)],
                None,
            ),
            (
                "out_of_date_is_not_wrong_flavor",
                vec![judged_entry("Foo", OUT_OF_DATE, &[110_200])],
                vec![addon("Foo", &["Foo"], false)],
                None,
            ),
            (
                "unreadable_is_not_judged",
                vec![FolderEntry {
                    unreadable: true,
                    ..judged_entry("Foo", below(), &[110_207])
                }],
                vec![addon("Foo", &["Foo"], false)],
                None,
            ),
            (
                "two_folders_one_finding",
                vec![
                    judged_entry("OmniCC_Config", below(), &[110_207]),
                    judged_entry("OmniCC", below(), &[110_207]),
                ],
                vec![addon("OmniCC", &["OmniCC", "OmniCC_Config"], false)],
                Some((
                    "OmniCC: 2 folders no longer load on this client",
                    json!([
                        {"name": "OmniCC", "toc": "OmniCC.toc", "reason": "below_client_floor", "interfaces": [110_207], "floor": 120_000},
                        {"name": "OmniCC_Config", "toc": "OmniCC_Config.toc", "reason": "below_client_floor", "interfaces": [110_207], "floor": 120_000},
                    ]),
                )),
            ),
            (
                "case_pair_judged_on_the_recorded_spelling",
                vec![
                    judged_entry("Foo", below(), &[110_207]),
                    judged_entry("foo", CURRENT, &[120_105]),
                ],
                vec![addon("Foo", &["foo"], false)],
                None,
            ),
            (
                "missing_folder_is_left_to_row_13",
                vec![],
                vec![addon("Foo", &["Foo"], false)],
                None,
            ),
        ];
        for (case, entries, addons, want) in cases {
            let inv = inventory(entries);
            let got = findings(
                ids::WRONG_FLAVOR,
                wrong_flavor_in(&client(), &retail(), &record(addons), &inv),
            );
            let ctx = format!("{case}\n{}\n{}", pretty(&inv.folders), pretty(&got));
            match want {
                None => assert!(got.is_empty(), "{ctx}"),
                Some((title, folders)) => {
                    assert_eq!(got.len(), 1, "{ctx}");
                    let f = &got[0];
                    let addon = f["addon"].as_str().unwrap();
                    assert!(
                        f["instance"]
                            .as_str()
                            .unwrap()
                            .ends_with(&format!(":addon={}", addon.to_ascii_lowercase())),
                        "{ctx}"
                    );
                    assert_eq!(f["label"], format!("retail/{addon}"), "{ctx}");
                    assert_eq!(f["title"], title, "{ctx}");
                    assert_eq!(f["evidence"]["folders"], folders, "{ctx}");
                    assert_eq!(f["evidence"]["expected"], "120105", "{ctx}");
                    assert_eq!(
                        f["remediation"]["argv"],
                        json!(["omaddons", "update", addon, "-t", "retail"]),
                        "{ctx}"
                    );
                    assert_eq!(f["remediation"]["destructive"], false, "{ctx}");
                }
            }
        }
    }

    #[test]
    fn out_of_date_cases() {
        // (case, entries, the addon, its finding's title and folder count)
        type Case = (
            &'static str,
            Vec<FolderEntry>,
            AddonRecord,
            Option<(&'static str, usize)>,
        );
        let cases: Vec<Case> = vec![
            (
                "one_folder",
                vec![judged_entry("Foo", OUT_OF_DATE, &[110_200])],
                addon("Foo", &["Foo"], false),
                Some(("Foo loads only as out of date on this client", 1)),
            ),
            (
                "two_folders",
                vec![
                    judged_entry("Foo", OUT_OF_DATE, &[110_200]),
                    judged_entry("Foo_Options", OUT_OF_DATE, &[110_200]),
                ],
                addon("Foo", &["Foo", "Foo_Options"], false),
                Some((
                    "Foo loads only as out of date on this client (2 folders)",
                    2,
                )),
            ),
            (
                "any_flavor_still_reported",
                vec![judged_entry("Foo", OUT_OF_DATE, &[110_200])],
                addon("Foo", &["Foo"], true),
                Some(("Foo loads only as out of date on this client", 1)),
            ),
            (
                "current_is_no_finding",
                vec![judged_entry("Foo", CURRENT, &[120_105])],
                addon("Foo", &["Foo"], false),
                None,
            ),
            (
                "wrong_flavor_is_not_out_of_date",
                vec![judged_entry(
                    "Foo",
                    wrong(WrongFlavorReason::GameTypeExcluded),
                    &[120_105],
                )],
                addon("Foo", &["Foo"], false),
                None,
            ),
        ];
        for (case, entries, addon, want) in cases {
            let inv = inventory(entries);
            let got = findings(
                ids::OUT_OF_DATE_TOC,
                out_of_date_in(&client(), &retail(), &record(vec![addon]), &inv),
            );
            let ctx = format!("{case}\n{}\n{}", pretty(&inv.folders), pretty(&got));
            match want {
                None => assert!(got.is_empty(), "{ctx}"),
                Some((title, n)) => {
                    assert_eq!(got.len(), 1, "{ctx}");
                    assert_eq!(got[0]["title"], title, "{ctx}");
                    let folders = got[0]["evidence"]["folders"].as_array().unwrap();
                    assert_eq!(folders.len(), n, "{ctx}");
                    assert!(folders.iter().all(|f| f.get("reason").is_none()), "{ctx}");
                    assert_eq!(
                        got[0]["remediation"]["argv"],
                        json!(["omaddons", "update", "Foo", "-t", "retail"]),
                        "{ctx}"
                    );
                }
            }
        }
    }

    #[test]
    fn recorded_entries_pick_the_recorded_spelling() {
        let inv = inventory(vec![
            entry("Foo", FolderClass::Managed),
            entry("foo", FolderClass::Unmanaged),
            entry("Bar", FolderClass::Managed),
        ]);
        let names = |folders: &[&str]| -> Vec<String> {
            recorded_entries(&addon("X", folders, false), &inv)
                .into_iter()
                .map(|(e, _)| e.name.clone())
                .collect()
        };
        assert_eq!(names(&["foo"]), ["foo"], "the recorded spelling of a pair");
        assert_eq!(
            names(&["FOO"]),
            Vec::<String>::new(),
            "an unrecorded spelling"
        );
        assert_eq!(names(&["BAR"]), ["Bar"], "one spelling: its entry");
        assert_eq!(names(&["Gone"]), Vec::<String>::new(), "missing");
    }

    #[test]
    fn keyed_clients_cases() {
        use crate::doctor::view::tests::{client as discovered, world_of};
        use crate::doctor::view::{LayerState, Preloaded};
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let paths = crate::state::paths::Paths::with_roots(
            home,
            home.join("config"),
            home.join("state"),
            home.join("cache"),
        );
        let keys = ["01M5ZQ7Y3KJ8V9W2X4C6B0N1RT", "01M5ZQ7Y3KJ8V9W2X4C6B0N1RV"];
        let world = world_of(
            &paths,
            vec![
                discovered("retail", &home.join("wow/_retail_"), Some(keys[1])),
                discovered("classic", &home.join("wow/_classic_"), Some(keys[0])),
                discovered("era", &home.join("wow/_classic_era_"), None),
            ],
        );
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                world: Some(LayerState::Available(world)),
                ..Preloaded::default()
            },
        );
        let ids = |target: Option<&str>| -> Vec<String> {
            let sel = Selection::parse(target, &[], &[], &[]).unwrap();
            keyed_clients(&view, &sel)
                .into_iter()
                .map(|c| c.key.to_string())
                .collect()
        };
        // Unregistered ids read as their key, so the order is the keys'.
        assert_eq!(
            ids(None),
            keys,
            "sorted_by_id, unregistered_client_is_left_out"
        );
        assert_eq!(
            ids(Some("classic")),
            [keys[0]],
            "out_of_scope_client_is_left_out"
        );
    }

    #[test]
    fn budget_is_checked_between_clients() {
        use crate::doctor::view::{LayerState, Preloaded};
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let paths = crate::state::paths::Paths::with_roots(
            home,
            home.join("config"),
            home.join("state"),
            home.join("cache"),
        );
        let world = crate::doctor::view::tests::world_of(
            &paths,
            vec![crate::doctor::view::tests::client(
                "retail",
                &home.join("wow/_retail_"),
                Some(KEY),
            )],
        );
        let view = DoctorView::preloaded(
            paths,
            Preloaded {
                world: Some(LayerState::Available(world)),
                ..Preloaded::default()
            },
        );
        let sel = Selection::parse(None, &[], &[], &[]).unwrap();
        let mut budget = Budget::new(Duration::ZERO, 1_000);
        let detected = ownership_conflict(&view, &sel, &mut budget);
        assert!(detected.found.is_empty());
        assert!(detected.incomplete.is_some(), "{detected:?}");
    }

    #[test]
    fn prose_remediations_name_no_command() {
        let inv = inventory(vec![
            owned(entry("Foo", FolderClass::Managed), &["Bar", "Foo"]),
            entry("Baz", FolderClass::Unmanaged),
            entry("baz", FolderClass::Unmanaged),
        ]);
        let mut texts: Vec<String> = Vec::new();
        for (id, found) in [
            (
                ids::OWNERSHIP_CONFLICT,
                ownership_conflicts(&client(), Path::new(ADDONS), &inv),
            ),
            (
                ids::CASE_COLLISION,
                case_collisions(&client(), Path::new(ADDONS), &inv),
            ),
        ] {
            for f in findings(id, found) {
                texts.push(f["remediation"]["command"].as_str().unwrap().to_owned());
            }
            let manual = row(id).unwrap().manual.unwrap();
            texts.push(manual.instruction.to_owned());
        }
        assert_eq!(texts.len(), 4, "{texts:?}");
        for text in texts {
            let words: Vec<&str> = text.split_whitespace().collect();
            let runnable = words.windows(2).any(|pair| {
                pair[0] == "omaddons"
                    && crate::app::COMMANDS
                        .iter()
                        .any(|c| c.name.split_whitespace().next() == Some(pair[1]))
            });
            assert!(!runnable, "{text}");
        }
    }

    #[test]
    fn update_argv_per_target() {
        let inv = inventory(vec![judged_entry("Foo", OUT_OF_DATE, &[110_200])]);
        let other = ClientRef {
            id: "classic",
            key: KEY.parse().unwrap(),
        };
        let got = findings(
            ids::OUT_OF_DATE_TOC,
            out_of_date_in(
                &other,
                &retail(),
                &record(vec![addon("Foo", &["Foo"], false)]),
                &inv,
            ),
        );
        assert_eq!(
            got[0]["remediation"]["argv"],
            json!(["omaddons", "update", "Foo", "-t", "classic"])
        );
    }
}
