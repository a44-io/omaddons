//! What a user's argument names, as every command reads it: a canonical ref, a pasted
//! URL, a path, an installed addon's name or folder, or a catalog name or alias. One
//! resolver, so `install`, `info`, `sync` and `adopt --ref` mean the same thing by the
//! same string.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::Ref;
use crate::resolve::catalog::Catalog;
use crate::rules::Family;
use crate::sources::refs::{CatalogMatch, InstalledMatch, ParsedRef, RefContext, parse_user_ref};
use crate::state::desired::Entry;
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
    let all: Vec<(&str, &TargetRecord)> = records.iter().map(|(k, v)| (k.as_str(), v)).collect();
    parse_among(git_hosts, &all, catalog, cwd, request)
}

/// `error`, from parsing the argument `request` of the command `argv`, with each
/// `omaddons install <ref>` fix the parser suggests (the https form, each ambiguity
/// candidate) aimed at the command that ran: `argv` with `request` replaced by `<ref>`,
/// every other argument (`-t` included) kept.
pub fn aimed_at(mut error: Error, argv: &[String], request: &str) -> Error {
    let aim = |s: &mut Suggestion| {
        let aimed = s.argv().and_then(|a| match a {
            [_, command, r] if command == "install" => {
                crate::cli::fix_argv_replacing(argv, request, r)
            }
            _ => None,
        });
        if let Some(aimed) = aimed {
            *s = Suggestion::command(aimed);
        }
    };
    aim(&mut error.fix);
    for alternative in &mut error.alternatives {
        aim(alternative);
    }
    error
}

/// What a desired-state entry names on the target `target_id`: its `ref`, else its
/// `name`, read as `install` reads an argument, except that an installed name is looked
/// up in that target's record only, and a relative path resolves against `base_dir`
/// (the directory of the desired-state file, so a file means the same thing wherever it
/// is used). Only the ref: a release link's tag is not a pin (`version` and `pinned`
/// are). Errors are `ref_invalid`, `ref_ambiguous` and `not_found`, as for `install`.
pub fn resolve_entry(
    entry: &Entry,
    target: (&str, Family),
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    git_hosts: &[String],
    base_dir: &Path,
) -> Result<Ref> {
    let (target_id, family) = target;
    let own: Vec<(&str, &TargetRecord)> = records
        .iter()
        .filter(|(id, _)| id.eq_ignore_ascii_case(target_id))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let request = entry.request();
    parse_among(git_hosts, &own, catalog, base_dir, request)
        .and_then(|p| for_family(&p, catalog, family, request))
        .map(|p| p.r#ref)
}

/// What `parsed` names on a client of `family`: itself, unless it is a catalog name with
/// variants (M3-D16), which names the variant declaring `family`. On a client no variant
/// covers, the catalog has no entry: `not_found`, as for a name it does not know, never
/// another family's build.
pub fn for_family(
    parsed: &ParsedRef,
    catalog: &Catalog,
    family: Family,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let Some(name) = &parsed.catalog_variants else {
        return Ok(parsed.clone());
    };
    match catalog.resolve(name, Some(family)) {
        Some(entry) => Ok(ParsedRef {
            r#ref: entry.r#ref.clone(),
            ..parsed.clone()
        }),
        None => Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "no {name} build for this client is known ({} clients)",
                family.as_str()
            ),
            Suggestion::command(["omaddons", "search", request]),
        )
        .details(serde_json::json!({ "name": name, "family": family }))),
    }
}

fn parse_among(
    git_hosts: &[String],
    records: &[(&str, &TargetRecord)],
    catalog: &Catalog,
    cwd: &Path,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let installed = |name: &str| -> Vec<InstalledMatch> {
        let mut found: Vec<InstalledMatch> = Vec::new();
        for &(target_id, record) in records {
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
                        target_id: target_id.to_owned(),
                    });
                }
            }
        }
        found
    };
    // A name with variants answers with its default variant; each client then resolves
    // its own ([`for_family`]).
    let in_catalog = |name: &str| {
        let variants = if catalog.has_variants(name) {
            catalog
                .variants(name)
                .into_iter()
                .map(|e| e.r#ref.clone())
                .collect()
        } else {
            Vec::new()
        };
        catalog.resolve(name, None).map(|e| CatalogMatch {
            name: e.name.clone(),
            r#ref: e.r#ref.clone(),
            variants,
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
