//! Folder inventory: classifies each `AddOns/<Name>` directory as protected, managed,
//! linked or unmanaged by its case-folded key, with TOC metadata.
//!
//! The read-only view that ownership checks, `list` and `status` build on (vision §5.1).
//! Classes, in precedence order: `invalid` (not a folder name, e.g. `.git`), `protected`
//! (`Blizzard_*`, even as a symlink: private-server packs ship them), `linked` (a
//! symlink, e.g. a developer's working copy; never read or followed), `managed` (owned
//! by an addon record), `shared` (in an AddOns directory another client shares, owned by
//! that client's record only) and `unmanaged`. Keys are ASCII case-folded, because Wine
//! resolves names case-insensitively. The inventory reports conflicts and case
//! duplicates; it never decides what may be mutated, and it never writes (not even
//! `AddOns/`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::discover::find_path;
use crate::error::{Error, Result, Suggestion};
use crate::model::{FolderName, Target};
use crate::rules::{ClientRules, RulesTable};
use crate::state::paths::Paths;
use crate::state::records::{TargetRecord, load_record};
use crate::toc::{DiskFolder, LoadedToc, VerdictReport, judge, loaded_toc};

/// How much to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Names and classes only; no TOC is opened (`status --cached`).
    Classes,
    /// Also each folder's loaded TOC, its verdict and link targets.
    Full,
}

/// Everything in a target's `AddOns/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Inventory {
    /// `AddOns/` does not exist (it is never created here).
    pub addons_dir_missing: bool,
    /// Sorted by key, then name.
    pub folders: Vec<FolderEntry>,
    /// Record folders with no entry on disk.
    pub missing: Vec<MissingFolder>,
    pub counts: Counts,
    /// Regular files (and other non-folders) directly in `AddOns/`, by name as on disk
    /// (lossy for non-UTF-8 names), sorted.
    pub stray_files: Vec<String>,
}

/// One entry of `AddOns/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderEntry {
    /// As on disk (lossy for non-UTF-8 names).
    pub name: String,
    /// ASCII case-folded.
    pub key: String,
    pub class: FolderClass,
    /// Every addon record claiming the key, sorted.
    pub owners: Vec<String>,
    /// Another entry has the same key.
    pub case_duplicate: bool,
    /// More than one record claims the key (corrupt state).
    pub ownership_conflict: bool,
    /// The folder or its TOC could not be read.
    pub unreadable: bool,
    pub toc: Option<TocSummary>,
    pub verdict: Option<VerdictReport>,
    /// A linked entry's target, at [`Depth::Full`] only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// The other clients sharing this AddOns directory whose records claim the key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub owned_on: Vec<OwnedOn>,
}

/// An addon of another client, sharing the AddOns directory, that claims a folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct OwnedOn {
    /// That client's target id.
    pub target: String,
    pub addon: String,
}

/// What an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FolderClass {
    Protected,
    Managed,
    /// Owned by another client's record, in an AddOns directory the two share.
    Shared,
    Linked,
    Unmanaged,
    Invalid,
}

/// The loaded TOC of a folder, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TocSummary {
    pub file: String,
    /// The title this client shows: a conditional `Title` variant that admits it, else
    /// the plain `## Title`.
    pub title: Option<String>,
    /// The title without colour codes, textures and atlases (`toc::plain_title`).
    pub title_plain: Option<String>,
    /// `## Notes`, as this client reads it, raw.
    pub notes: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub interfaces: Vec<u32>,
    /// The source-identifying `X-…ID` fields (curse, wowi, wago, tukui), by key.
    pub x_ids: BTreeMap<String, String>,
    /// `X-Website`, raw.
    pub website: Option<String>,
    /// What the TOC requires on this client (conditional variants evaluated).
    pub dependencies: Vec<String>,
    /// `OptionalDeps` on this client.
    pub optional_deps: Vec<String>,
    /// `X-Tukui-ProjectFolders`.
    pub tukui_project_folders: Vec<String>,
    /// The account-wide `## SavedVariables` globals (where `errors` finds an embedded
    /// `BugGrabberDB`, M3-D44). Not on the wire.
    #[serde(skip)]
    #[schemars(skip)]
    pub saved_variables: Vec<String>,
}

/// A folder a record lists that is not on disk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct MissingFolder {
    pub addon: String,
    pub folder: String,
}

/// Entries per class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Counts {
    pub protected: u32,
    pub managed: u32,
    pub shared: u32,
    pub linked: u32,
    pub unmanaged: u32,
    pub invalid: u32,
}

/// The inventory of `target`'s `AddOns/` (found ignoring the case of `Interface` and
/// `AddOns`). `record` supplies managed ownership. An `AddOns/` that exists but cannot
/// be listed is error `io`; anything wrong with one entry stays on that entry.
pub fn inventory(
    target: &Target,
    record: Option<&TargetRecord>,
    table: &RulesTable,
    depth: Depth,
) -> Result<Inventory> {
    inventory_shared(target, record, &[], table, depth)
}

/// [`inventory`] of an AddOns directory other clients share: `shared` holds each such
/// client's target id and record (`discover::shared::siblings`). A folder only they
/// claim is `shared`; every claim of theirs is listed in the entry's `owned_on`.
pub fn inventory_shared(
    target: &Target,
    record: Option<&TargetRecord>,
    shared: &[(String, TargetRecord)],
    table: &RulesTable,
    depth: Depth,
) -> Result<Inventory> {
    let Some(addons) = find_path(&target.client_dir, &["Interface", "AddOns"]) else {
        return Ok(Inventory::missing_dir());
    };
    let listing = std::fs::read_dir(&addons).map_err(|err| {
        Error::io(
            &addons,
            &err,
            Suggestion::text(format!("check the permissions of {}", addons.display())),
        )
    })?;

    let owners = ownership(record);
    let mut stray_files = Vec::new();
    let mut folders = Vec::new();
    for entry in listing.filter_map(std::result::Result::ok) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() && !kind.is_symlink() {
            stray_files.push(entry.file_name().to_string_lossy().into_owned());
            continue;
        }
        let raw = entry.file_name();
        let folder = raw.to_str().and_then(|n| FolderName::new(n).ok());
        let name = raw.to_string_lossy().into_owned();
        let key = folder
            .as_ref()
            .map_or_else(|| name.to_ascii_lowercase(), |f| f.key().to_owned());
        let claimed: Vec<String> = owners.get(&key).cloned().unwrap_or_default();
        let owned_on = shared_owners(shared, &key);
        let class = match &folder {
            None => FolderClass::Invalid,
            Some(f) if f.is_protected() => FolderClass::Protected,
            Some(_) if kind.is_symlink() => FolderClass::Linked,
            Some(_) if !claimed.is_empty() => FolderClass::Managed,
            Some(_) if !owned_on.is_empty() => FolderClass::Shared,
            Some(_) => FolderClass::Unmanaged,
        };
        let mut folder_entry = FolderEntry {
            name,
            key,
            class,
            ownership_conflict: claimed.len() > 1,
            owners: claimed,
            case_duplicate: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
            owned_on,
        };
        if depth == Depth::Full {
            read_details(
                &mut folder_entry,
                &entry.path(),
                kind.is_symlink(),
                target,
                table,
            );
        }
        folders.push(folder_entry);
    }
    folders.sort_by(|a, b| (&a.key, &a.name).cmp(&(&b.key, &b.name)));
    let duplicate_keys: Vec<String> = folders
        .windows(2)
        .filter(|pair| pair[0].key == pair[1].key)
        .map(|pair| pair[0].key.clone())
        .collect();
    for entry in &mut folders {
        entry.case_duplicate = duplicate_keys.contains(&entry.key);
    }

    let mut missing: Vec<MissingFolder> = record
        .into_iter()
        .flat_map(|r| r.addons.values())
        .flat_map(|addon| addon.folders.iter().map(move |f| (addon, f)))
        .filter(|(_, f)| !folders.iter().any(|e| e.key == f.key()))
        .map(|(addon, f)| MissingFolder {
            addon: addon.name.clone(),
            folder: f.as_str().to_owned(),
        })
        .collect();
    missing.sort();
    missing.dedup();
    stray_files.sort();

    Ok(Inventory {
        addons_dir_missing: false,
        counts: Counts::of(&folders),
        folders,
        missing,
        stray_files,
    })
}

impl Inventory {
    fn missing_dir() -> Self {
        Self {
            addons_dir_missing: true,
            folders: Vec::new(),
            missing: Vec::new(),
            counts: Counts::default(),
            stray_files: Vec::new(),
        }
    }

    /// The non-folder directly in `AddOns/` that holds `folder`'s name, ignoring ASCII
    /// case as the commit's own check for a free name does: an install of `folder` would
    /// find its name taken.
    pub fn stray_named(&self, folder: &str) -> Option<&str> {
        self.stray_files
            .iter()
            .map(String::as_str)
            .find(|name| name.eq_ignore_ascii_case(folder))
    }
}

impl Counts {
    fn of(folders: &[FolderEntry]) -> Self {
        let mut counts = Self::default();
        for entry in folders {
            let slot = match entry.class {
                FolderClass::Protected => &mut counts.protected,
                FolderClass::Managed => &mut counts.managed,
                FolderClass::Shared => &mut counts.shared,
                FolderClass::Linked => &mut counts.linked,
                FolderClass::Unmanaged => &mut counts.unmanaged,
                FolderClass::Invalid => &mut counts.invalid,
            };
            *slot = slot.saturating_add(1);
        }
        counts
    }
}

/// The records of `target`'s registered siblings among `all` (the clients sharing its
/// AddOns directory, `discover::shared::siblings`), by target id: the `shared` input of
/// [`inventory_shared`]. A sibling discovered unregistered is looked up by its marker,
/// which registration writes under the global lock: one registered since discovery
/// counts too. An unregistered sibling has no records.
pub fn sibling_records<'a>(
    paths: &Paths,
    target: &Target,
    all: impl IntoIterator<Item = &'a Target>,
) -> Result<Vec<(String, TargetRecord)>> {
    crate::discover::shared::siblings(target, all)
        .into_iter()
        .filter_map(|t| {
            let key = t
                .key
                .or_else(|| crate::discover::registry::read_marker(&t.client_dir))?;
            Some((t.id.clone(), key))
        })
        .map(|(id, key)| Ok((id, load_record(paths, &key)?.value)))
        .collect()
}

/// Every addon of the clients in `shared` that claims the folder `key`, sorted.
fn shared_owners(shared: &[(String, TargetRecord)], key: &str) -> Vec<OwnedOn> {
    let mut owned_on: Vec<OwnedOn> = shared
        .iter()
        .flat_map(|(target, record)| {
            record
                .addons
                .values()
                .filter(|addon| addon.folders.iter().any(|f| f.key() == key))
                .map(|addon| OwnedOn {
                    target: target.clone(),
                    addon: addon.name.clone(),
                })
        })
        .collect();
    owned_on.sort();
    owned_on.dedup();
    owned_on
}

/// Folder key → the sorted names of every addon claiming it.
fn ownership(record: Option<&TargetRecord>) -> BTreeMap<String, Vec<String>> {
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for addon in record.into_iter().flat_map(|r| r.addons.values()) {
        for folder in &addon.folders {
            let names = owners.entry(folder.key().to_owned()).or_default();
            if !names.contains(&addon.name) {
                names.push(addon.name.clone());
            }
        }
    }
    for names in owners.values_mut() {
        names.sort();
    }
    owners
}

/// The TOC, verdict and link target of one entry. Links and invalid names are never
/// read.
fn read_details(
    entry: &mut FolderEntry,
    path: &Path,
    is_link: bool,
    target: &Target,
    table: &RulesTable,
) {
    if is_link {
        entry.link_target = std::fs::read_link(path)
            .ok()
            .map(|t: PathBuf| t.display().to_string());
        return;
    }
    if entry.class == FolderClass::Invalid {
        return;
    }
    let Ok(view) = DiskFolder::new(path) else {
        entry.unreadable = true;
        return;
    };
    match loaded_toc(&view, &target.rules) {
        Ok(loaded) => {
            entry.toc = loaded.as_ref().map(|l| summary(l, &target.rules));
            entry.verdict = Some(judge(
                &entry.name,
                loaded.as_ref(),
                &target.rules,
                target.family,
                target.interface,
                table,
            ));
        }
        Err(_) => entry.unreadable = true,
    }
}

/// The TOC the client of `rules` loads from a folder, as listed: its fields as that
/// client reads them, as written (the caller makes them safe to show).
fn summary(l: &LoadedToc, rules: &ClientRules) -> TocSummary {
    let title = l.toc.title_for(rules).map(str::to_owned);
    let deps = l.toc.dependencies_for(rules);
    let strings = |names: &[FolderName]| names.iter().map(|n| n.as_str().to_owned()).collect();
    TocSummary {
        file: l.file.clone(),
        title_plain: title.as_deref().map(crate::toc::plain_title),
        title,
        notes: l.toc.notes.clone(),
        version: l.toc.version.clone(),
        author: l.toc.author.clone(),
        interfaces: l.toc.interfaces.clone(),
        x_ids: l
            .toc
            .x_fields
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().ends_with("id"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        website: l.toc.website().map(str::to_owned),
        dependencies: strings(&deps.required),
        optional_deps: strings(&deps.optional),
        tukui_project_folders: l.toc.tukui_project_folders(),
        saved_variables: l.toc.saved_variables.clone(),
    }
}

/// The TOC `target`'s client loads from its AddOns folder `folder` (both found ignoring
/// case, as Wine does), as [`inventory`] lists it: `None` when the folder is missing, is
/// a symlink (never read or followed) or cannot be read, or holds no TOC the client
/// loads. `info` reads an installed addon's own folder this way, and `status` each
/// unmanaged folder's at [`Depth::Classes`], to count what adoption identifies
/// (omad-3oru).
pub fn folder_toc(target: &Target, folder: &FolderName) -> Option<TocSummary> {
    let path = find_path(
        &target.client_dir,
        &["Interface", "AddOns", folder.as_str()],
    )?;
    if !std::fs::symlink_metadata(&path).ok()?.is_dir() {
        return None;
    }
    let view = DiskFolder::new(&path).ok()?;
    let loaded = loaded_toc(&view, &target.rules).ok()??;
    Some(summary(&loaded, &target.rules))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_per_class() {
        let entry = |class| FolderEntry {
            name: "x".into(),
            key: "x".into(),
            class,
            owners: Vec::new(),
            case_duplicate: false,
            ownership_conflict: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
            owned_on: Vec::new(),
        };
        let counts = Counts::of(&[
            entry(FolderClass::Protected),
            entry(FolderClass::Protected),
            entry(FolderClass::Managed),
            entry(FolderClass::Shared),
            entry(FolderClass::Linked),
            entry(FolderClass::Unmanaged),
            entry(FolderClass::Invalid),
        ]);
        assert_eq!(
            counts,
            Counts {
                protected: 2,
                managed: 1,
                shared: 1,
                linked: 1,
                unmanaged: 1,
                invalid: 1
            }
        );
    }
}
