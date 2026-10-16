//! What a user's argument names, as every command reads it: a canonical ref, a pasted
//! URL, a path, an installed addon's name or folder, or a catalog name or alias. One
//! resolver, so `install`, `info`, `sync` and `adopt --ref` mean the same thing by the
//! same string.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Error;
use crate::model::Ref;
use crate::resolve::catalog::Catalog;
use crate::sources::refs::{CatalogMatch, InstalledMatch, ParsedRef, RefContext, parse_user_ref};
use crate::state::records::TargetRecord;

/// Parses `request` against the installed addons of `records` (by target id; name or
/// folder, ignoring ASCII case) and `catalog` (name or alias); a relative path resolves
/// against `cwd`, and `git_hosts` are the extra hosts `git:` refs may name.
pub fn parse_ref(
    git_hosts: &[String],
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    cwd: &Path,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let installed = |name: &str| -> Vec<InstalledMatch> {
        let mut found: Vec<InstalledMatch> = Vec::new();
        for (target_id, record) in records {
            for addon in record.addons.values() {
                let by_folder = addon
                    .folders
                    .iter()
                    .any(|f| f.as_str().eq_ignore_ascii_case(name));
                if (addon.name.eq_ignore_ascii_case(name) || by_folder)
                    && !found.iter().any(|m| m.r#ref == addon.r#ref)
                {
                    found.push(InstalledMatch {
                        name: addon.name.clone(),
                        r#ref: addon.r#ref.clone(),
                        target_id: target_id.clone(),
                    });
                }
            }
        }
        found
    };
    let in_catalog = |name: &str| {
        catalog.by_name(name).map(|e| CatalogMatch {
            name: e.name.clone(),
            r#ref: e.r#ref.clone(),
        })
    };
    let ref_ctx = RefContext {
        cwd: cwd.to_path_buf(),
        git_hosts,
        installed: &installed,
        catalog: &in_catalog,
    };
    parse_user_ref(request, &ref_ctx)
}

/// The catalog's name for `r`: a catalog addon is recorded under it, however it was
/// named.
pub fn catalog_name(catalog: &Catalog, r: &Ref) -> Option<String> {
    catalog
        .entries()
        .iter()
        .find(|e| e.r#ref == *r)
        .map(|e| e.name.clone())
}
