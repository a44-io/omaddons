//! Fixes that point other commands at `adopt` (E5.3): a name no record answers to, but
//! which is a folder in AddOns that omaddons did not install, is adopted where it is,
//! never installed over or forced. Offline: an inventory of folder classes, no TOC read.

use std::collections::BTreeMap;

use crate::app::world::World;
use crate::discover::DiscoveredTarget;
use crate::error::{Error, Suggestion};
use crate::inventory::{Depth, FolderClass, inventory};
use crate::model::Ref;
use crate::resolve::catalog::Catalog;
use crate::state::records::TargetRecord;

/// `omaddons adopt <folder> [--ref <ref>] -t <target>`.
pub fn adopt_folder(folder: &str, r#ref: Option<&Ref>, target: &str) -> Suggestion {
    let mut argv = vec!["omaddons".to_owned(), "adopt".to_owned(), folder.to_owned()];
    if let Some(r) = r#ref {
        argv.extend(["--ref".to_owned(), r.to_string()]);
    }
    argv.extend(["-t".to_owned(), target.to_owned()]);
    Suggestion::command(argv)
}

/// `error` with the first of `fixes` as its fix and the others as alternatives; as it
/// is when `fixes` is empty.
pub fn adopting(mut error: Error, fixes: Vec<Suggestion>) -> Error {
    let mut fixes = fixes.into_iter();
    if let Some(first) = fixes.next() {
        error.fix = first;
        error.alternatives.extend(fixes);
    }
    error
}

/// One adopt fix per target of `targets` (in id order) whose AddOns holds an unmanaged
/// folder named exactly `name` (ignoring case; never a symlink, a protected, invalid or
/// case-duplicated folder), with `--ref` when the catalog knows the name. Empty when
/// there is none: the caller's own fix stands.
pub fn adopt_fixes(
    world: &World,
    records: &BTreeMap<String, TargetRecord>,
    targets: &[&DiscoveredTarget],
    name: &str,
) -> Vec<Suggestion> {
    let key = name.to_ascii_lowercase();
    let mut targets = targets.to_vec();
    targets.sort_by(|a, b| a.target.id.cmp(&b.target.id));
    let mut catalog: Option<Catalog> = None;
    let mut fixes = Vec::new();
    for d in targets {
        let record = records.get(&d.target.id);
        let Ok(inv) = inventory(&d.target, record, &world.table, Depth::Classes) else {
            continue;
        };
        let Some(entry) = inv.folders.iter().find(|e| {
            e.key.eq(key.as_str()) && e.class == FolderClass::Unmanaged && !e.case_duplicate
        }) else {
            continue;
        };
        let catalog = catalog.get_or_insert_with(|| Catalog::load(&world.paths).0);
        let r#ref = catalog.by_name(name).map(|e| &e.r#ref);
        fixes.push(adopt_folder(&entry.name, r#ref, &d.target.id));
    }
    fixes
}
